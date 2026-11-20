// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) The GeekCast Radio Network, LLC.
// This file is part of Altered Audio Editor.

//! Scrubbing: hearing the audio under the pointer while the playhead is
//! dragged, like tape moving past a head. Drag slowly and it plays slow
//! and low, quickly and it plays fast and high, backwards and it plays in
//! reverse. Hold still and it goes quiet.
//!
//! The interface sends where the pointer is (a target) as often as it
//! moves. A play head chases the target: its speed is the distance to
//! the target over a short time constant, eased so it never jumps, and
//! capped at [`MAX_RATE`] either way. Its loudness fades in and out with
//! its speed, so a stopped head is silent and starting or stopping never
//! clicks. A far jump fades out, moves and fades back in.
//!
//! The audio comes from tiles: short stretches of the mix (the session's
//! tracks, clip and track chains, buses and the master chain, at the
//! master fader), rendered by a worker thread around the head, in the
//! direction it is moving, and kept while the drag lasts. Forward tiles
//! continue one render; backward ones are rendered in short forward
//! runs. A missing tile plays as silence until it arrives. VST3 plug-ins
//! and instruments are left out, so scrubbing never starts plug-in hosts.
//!
//! A second thread reads the tiles at the head's position (cubic
//! interpolation) and keeps a short queue filled for the device; the
//! device callback only copies from that queue. Scrubbing has its own
//! output stream and only runs while the transport is stopped, so normal
//! playback, recording and exports never see it.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use cpal::traits::{DeviceTrait, StreamTrait};
use cpal::{FromSample, SampleFormat, SizedSample, StreamConfig};
use rtrb::{Consumer, Producer, RingBuffer};

use super::graph::{Graph, Mix};
use super::plan::{self, Plan};
use super::render::Renderer;
use crate::session::model::Session;

/// Fastest the head moves, either way, in times normal speed.
pub const MAX_RATE: f64 = 4.0;
/// How quickly the head closes on the target: the time constant, seconds.
const CHASE_SECONDS: f64 = 0.05;
/// How quickly its speed changes, seconds.
const EASE_SECONDS: f64 = 0.02;
/// How quickly its loudness follows its speed, seconds.
const FADE_SECONDS: f64 = 0.005;
/// Below this speed the head fades to silence.
const QUIET_RATE: f64 = 0.06;
/// A target further away than this, seconds, is jumped to, not chased.
const JUMP_SECONDS: f64 = 1.5;

/// Frames in one tile of rendered mix.
pub const TILE: usize = 4096;
/// Tiles rendered ahead of the head in the direction it moves.
const AHEAD: i64 = 8;
/// Tiles rendered in one backward run.
const RUN: i64 = 8;
/// Most tiles kept; the ones furthest from the head go first.
const MAX_TILES: usize = 600;

/// The play head that chases the pointer.
#[derive(Debug, Clone)]
pub struct Chase {
    /// Output frames on the timeline (fractional).
    pos: f64,
    /// Output frames moved per output frame: 1 is normal speed, negative is backwards.
    rate: f64,
    gain: f64,
    tau: f64,
    ease: f64,
    fade: f64,
    jump: f64,
    /// Fading out before a jump, to land on this.
    landing: Option<f64>,
    releasing: bool,
}

impl Chase {
    /// A head at rest (and silent) at `pos`, at `out_rate` frames a second.
    pub fn new(pos: f64, out_rate: u32) -> Self {
        let r = f64::from(out_rate.max(1));
        let k = |secs: f64| 1.0 - (-1.0 / (secs * r)).exp();
        Self {
            pos,
            rate: 0.0,
            gain: 0.0,
            tau: CHASE_SECONDS * r,
            ease: k(EASE_SECONDS),
            fade: k(FADE_SECONDS),
            jump: JUMP_SECONDS * r,
            landing: None,
            releasing: false,
        }
    }

    pub fn pos(&self) -> f64 {
        self.pos
    }

    pub fn rate(&self) -> f64 {
        self.rate
    }

    pub fn gain(&self) -> f64 {
        self.gain
    }

    /// Fades out for good (the drag ended).
    pub fn release(&mut self) {
        self.releasing = true;
    }

    /// True once released and silent.
    pub fn done(&self) -> bool {
        self.releasing && self.gain < 1e-4
    }

    /// Moves the head one frame toward `target`; returns where it reads
    /// and how loud, for that frame.
    pub fn step(&mut self, target: f64) -> (f64, f32) {
        let target = if target.is_finite() {
            target.max(0.0)
        } else {
            self.pos
        };
        if self.landing.is_none() && (target - self.pos).abs() > self.jump {
            self.landing = Some(target);
        }
        let want_gain = if let Some(land) = self.landing {
            // Fade out where it is, then land with no speed and fade back in.
            if self.gain < 1e-3 {
                self.pos = land;
                self.rate = 0.0;
                self.landing = None;
            }
            0.0
        } else {
            let dist = target - self.pos;
            let want = if dist.abs() < 0.5 {
                0.0
            } else {
                (dist / self.tau).clamp(-MAX_RATE, MAX_RATE)
            };
            self.rate += (want - self.rate) * self.ease;
            if dist.abs() < 0.5 && self.rate.abs() < 1e-4 {
                self.rate = 0.0;
            }
            self.pos = (self.pos + self.rate).max(0.0);
            // Smooth in from silence: quiet near a standstill, full from QUIET_RATE up.
            let x = (self.rate.abs() / QUIET_RATE).min(1.0);
            x * x * (3.0 - 2.0 * x)
        };
        let want_gain = if self.releasing { 0.0 } else { want_gain };
        self.gain += (want_gain - self.gain) * self.fade;
        if self.gain < 1e-6 {
            self.gain = 0.0;
        }
        (self.pos, self.gain as f32)
    }
}

/// Rendered stretches of the mix by tile number.
pub type Tiles = HashMap<i64, Arc<Vec<f32>>>;

/// The tile a frame is in.
fn tile_of(frame: i64) -> i64 {
    frame.div_euclid(TILE as i64)
}

/// Renders tiles of a session's mix, on the worker thread.
pub struct TileMaker {
    session: Session,
    plan: Plan,
    rate: u32,
    master: f32,
    /// A render in progress and the tile it makes next, so moving forward
    /// carries on without starting over.
    cursor: Option<(Mix, i64)>,
    blocks: Vec<Vec<f32>>,
}

/// The session as scrubbing plays it: VST3 plug-ins and instruments left out.
pub fn without_plugins(mut s: Session) -> Session {
    let keep = |c: &mut crate::session::model::EffectChain| {
        c.slots.retain(|x| !crate::plugins::is_plugin(&x.effect));
    };
    for t in &mut s.tracks {
        t.instrument = None;
        keep(&mut t.effects);
        for c in &mut t.clips {
            keep(&mut c.effects);
        }
    }
    for b in &mut s.buses {
        keep(&mut b.effects);
    }
    keep(&mut s.master.effects);
    s
}

impl TileMaker {
    pub fn new(session: Session, out_rate: u32) -> Self {
        let mut session = without_plugins(session);
        if session.tracks.is_empty() {
            session.tracks.push(crate::session::model::Track::new(
                "",
                crate::session::model::ChannelLayout::Stereo,
            ));
        }
        Self {
            plan: plan::build(&session, out_rate),
            master: plan::master_gain(&session),
            session,
            rate: out_rate,
            cursor: None,
            blocks: vec![Vec::with_capacity(TILE * 2)],
        }
    }

    /// Renders tile `t` (a non-negative tile number).
    fn render(&mut self, t: i64) -> Vec<f32> {
        let at = self.cursor.as_ref().map(|(_, n)| *n);
        if at != Some(t) {
            let start = (t.max(0) as u64) * TILE as u64;
            let renderer = Renderer::new(self.plan.clone(), start);
            // The whole mix, faders and all, runs here as one stereo stream.
            let graph = Graph::new(&self.session, self.rate, true);
            self.cursor = Some((Mix::new(renderer, graph), t));
        }
        let (mix, next) = self.cursor.as_mut().expect("set above");
        let mut out = Vec::with_capacity(TILE * 2);
        while out.len() < TILE * 2 {
            let want = TILE - out.len() / 2;
            let n = mix.render(want, &mut self.blocks);
            out.extend_from_slice(&self.blocks[0][..n * 2]);
            if n == 0 {
                out.resize(TILE * 2, 0.0);
            }
        }
        if self.master != 1.0 {
            for s in &mut out {
                *s *= self.master;
            }
        }
        *next = t + 1;
        out
    }

    /// The tiles wanted next around the head at tile `head`, moving
    /// backwards when `back`: nearest first.
    fn wanted(head: i64, back: bool) -> impl Iterator<Item = i64> {
        let dir = if back { -1 } else { 1 };
        std::iter::once(head)
            .chain((1..=AHEAD).map(move |k| head + k * dir))
            .chain([head - dir, head - 2 * dir])
            .filter(|&t| t >= 0)
    }

    /// Renders the nearest missing tile around `head` into `tiles`.
    /// False when nothing was missing.
    pub fn fill(&mut self, tiles: &Mutex<Tiles>, head: i64, back: bool) -> bool {
        let missing = {
            let Ok(have) = tiles.lock() else {
                return false;
            };
            Self::wanted(head, back).find(|t| !have.contains_key(t))
        };
        let Some(t) = missing else {
            return false;
        };
        let continues = self.cursor.as_ref().is_some_and(|(_, n)| *n == t);
        // Backwards, render a short run that ends at `t`, so one start covers several tiles.
        let first = if back && !continues {
            (t - RUN + 1).max(0)
        } else {
            t
        };
        let made: Vec<(i64, Arc<Vec<f32>>)> =
            (first..=t).map(|k| (k, Arc::new(self.render(k)))).collect();
        if let Ok(mut have) = tiles.lock() {
            for (k, v) in made {
                have.entry(k).or_insert(v);
            }
            if have.len() > MAX_TILES {
                let mut keys: Vec<i64> = have.keys().copied().collect();
                keys.sort_by_key(|k| std::cmp::Reverse((k - head).abs()));
                for k in keys.into_iter().take(have.len() - MAX_TILES) {
                    have.remove(&k);
                }
            }
        }
        true
    }
}

/// Reads stereo frames out of tiles at fractional positions.
pub struct Reader {
    held: Vec<(i64, Arc<Vec<f32>>)>,
}

impl Default for Reader {
    fn default() -> Self {
        Self {
            held: Vec::with_capacity(8),
        }
    }
}

impl Reader {
    /// Takes the tiles covering frames `lo..=hi` (plus the neighbours the
    /// interpolation reads) out of `tiles`, holding the lock only briefly.
    pub fn grab(&mut self, tiles: &Mutex<Tiles>, lo: f64, hi: f64) {
        self.held.clear();
        let a = tile_of(lo.floor() as i64 - 1);
        let b = tile_of(hi.ceil() as i64 + 2);
        if let Ok(have) = tiles.lock() {
            for t in a..=b {
                if let Some(v) = have.get(&t) {
                    self.held.push((t, v.clone()));
                }
            }
        }
    }

    fn frame(&self, i: i64) -> [f32; 2] {
        let t = tile_of(i);
        let Some((_, v)) = self.held.iter().find(|(k, _)| *k == t) else {
            return [0.0, 0.0];
        };
        let at = (i - t * TILE as i64) as usize * 2;
        [v[at], v[at + 1]]
    }

    /// The stereo frame at fractional position `pos` (4-point cubic).
    pub fn at(&self, pos: f64) -> [f32; 2] {
        let i = pos.floor() as i64;
        let f = (pos - i as f64) as f32;
        let p = [
            self.frame(i - 1),
            self.frame(i),
            self.frame(i + 1),
            self.frame(i + 2),
        ];
        let mut out = [0f32; 2];
        for (c, o) in out.iter_mut().enumerate() {
            let (y0, y1, y2, y3) = (p[0][c], p[1][c], p[2][c], p[3][c]);
            let a = -0.5 * y0 + 1.5 * y1 - 1.5 * y2 + 0.5 * y3;
            let b = y0 - 2.5 * y1 + 2.0 * y2 - 0.5 * y3;
            let cc = -0.5 * y0 + 0.5 * y2;
            *o = ((a * f + b) * f + cc) * f + y1;
        }
        out
    }
}

/// Makes `out.len() / 2` stereo frames by moving `chase` toward `target`
/// and reading `tiles` where it goes. Also used by the tests, without a
/// device.
pub fn produce(
    chase: &mut Chase,
    target: f64,
    tiles: &Mutex<Tiles>,
    reader: &mut Reader,
    out: &mut [f32],
    scratch: &mut Vec<(f64, f32)>,
) {
    scratch.clear();
    for _ in 0..out.len() / 2 {
        scratch.push(chase.step(target));
    }
    let lo = scratch.iter().map(|x| x.0).fold(f64::INFINITY, f64::min);
    let hi = scratch
        .iter()
        .map(|x| x.0)
        .fold(f64::NEG_INFINITY, f64::max);
    if scratch.iter().any(|x| x.1 > 0.0) {
        reader.grab(tiles, lo, hi);
    }
    for (f, &(pos, g)) in out.as_chunks_mut::<2>().0.iter_mut().zip(scratch.iter()) {
        if g == 0.0 {
            f[0] = 0.0;
            f[1] = 0.0;
        } else {
            let s = reader.at(pos);
            f[0] = s[0] * g;
            f[1] = s[1] * g;
        }
    }
}

// --- The player ---

#[derive(Default)]
struct Shared {
    stop: AtomicBool,
    /// The drag ended: fade out, then stop.
    release: AtomicBool,
    /// Where the pointer is, in output frames (f64 bits).
    target: AtomicU64,
    /// Where the head is, in output frames (f64 bits), and its speed.
    head: AtomicU64,
    rate: AtomicU64,
}

struct Running {
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
    session_rate: u32,
    out_rate: u32,
}

impl Running {
    fn stop(&mut self) {
        self.shared.release.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// Scrubbing for the open session or the Waveform editor's file.
#[derive(Default)]
pub struct ScrubPlayer {
    current: Mutex<Option<Running>>,
}

impl ScrubPlayer {
    /// Starts scrubbing `session` with the head resting at session sample `from`.
    pub fn start(&self, session: &Session, from: u64) -> Result<(), String> {
        let mut cur = self.current.lock().map_err(|e| e.to_string())?;
        if let Some(mut r) = cur.take() {
            r.stop();
        }
        let shared = Arc::new(Shared::default());
        let (tx, rx) = mpsc::channel::<Result<u32, String>>();
        let session = session.clone();
        let session_rate = session.sample_rate.max(1);
        let s = shared.clone();
        let thread = thread::Builder::new()
            .name("scrub".into())
            .spawn(move || run(session, from, s, tx))
            .map_err(|e| e.to_string())?;
        let out_rate = match rx.recv_timeout(Duration::from_secs(5)) {
            Ok(Ok(r)) => r,
            Ok(Err(e)) => {
                let _ = thread.join();
                return Err(e);
            }
            Err(_) => {
                shared.stop.store(true, Ordering::SeqCst);
                return Err("The audio device did not respond.".into());
            }
        };
        *cur = Some(Running {
            shared,
            thread: Some(thread),
            session_rate,
            out_rate,
        });
        Ok(())
    }

    /// The pointer is at session sample `pos`.
    pub fn to(&self, pos: u64) {
        if let Ok(cur) = self.current.lock() {
            if let Some(r) = cur.as_ref() {
                let f = pos as f64 * f64::from(r.out_rate) / f64::from(r.session_rate);
                r.shared.target.store(f.to_bits(), Ordering::Relaxed);
            }
        }
    }

    /// Fades out and stops. Returns where the head was, in session samples.
    pub fn stop(&self) -> Option<u64> {
        let mut cur = self.current.lock().ok()?;
        let mut r = cur.take()?;
        let head = f64::from_bits(r.shared.head.load(Ordering::Relaxed));
        r.stop();
        Some((head * f64::from(r.session_rate) / f64::from(r.out_rate)).round() as u64)
    }

    pub fn is_active(&self) -> bool {
        self.current.lock().map(|c| c.is_some()).unwrap_or(false)
    }
}

impl Drop for ScrubPlayer {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

fn run(session: Session, from: u64, shared: Arc<Shared>, ready: mpsc::Sender<Result<u32, String>>) {
    let Some(device) = super::output::output_device_any() else {
        let _ = ready.send(Err("No audio output device found.".into()));
        return;
    };
    let stereo = super::output::map().stereo.map(usize::from);
    let wanted = stereo[0].max(stereo[1]) + 1;
    let Some(supported) = super::output::config_for(&device, wanted) else {
        let _ = ready.send(Err("The audio device is not available.".into()));
        return;
    };
    let format = supported.sample_format();
    let config: StreamConfig =
        super::host::with_buffer(supported.config(), supported.buffer_size());
    let out_rate = config.sample_rate;
    let channels = usize::from(config.channels).max(1);
    let start = plan::to_out(from, session.sample_rate.max(1), out_rate) as f64;
    shared.target.store(start.to_bits(), Ordering::Relaxed);
    shared.head.store(start.to_bits(), Ordering::Relaxed);

    // About 40 ms queued for the device, more if its buffer is bigger.
    let buffer = super::host::buffer_frames() as usize;
    let lead = ((f64::from(out_rate) * 0.04) as usize)
        .max(buffer * 2)
        .min(out_rate as usize / 5);
    let (producer, consumer) = RingBuffer::<f32>::new((lead * 2).max(4096) * channels);

    let tiles: Arc<Mutex<Tiles>> = Arc::default();
    let worker = {
        let tiles = tiles.clone();
        let shared = shared.clone();
        thread::Builder::new()
            .name("scrub-render".into())
            .spawn(move || {
                let mut maker = TileMaker::new(session, out_rate);
                while !shared.stop.load(Ordering::SeqCst) {
                    let head = f64::from_bits(shared.head.load(Ordering::Relaxed));
                    let back = f64::from_bits(shared.rate.load(Ordering::Relaxed)) < 0.0;
                    if !maker.fill(&tiles, tile_of(head as i64), back) {
                        thread::sleep(Duration::from_millis(3));
                    }
                }
            })
    };
    let worker = match worker {
        Ok(w) => w,
        Err(e) => {
            let _ = ready.send(Err(e.to_string()));
            return;
        }
    };

    let stream = match format {
        SampleFormat::F32 => build::<f32>(&device, config, consumer),
        SampleFormat::I16 => build::<i16>(&device, config, consumer),
        SampleFormat::I32 => build::<i32>(&device, config, consumer),
        SampleFormat::U16 => build::<u16>(&device, config, consumer),
        other => Err(format!("Unsupported output sample format: {other}")),
    };
    let stream = match stream.and_then(|s| s.play().map(|_| s).map_err(|e| e.to_string())) {
        Ok(s) => s,
        Err(e) => {
            shared.stop.store(true, Ordering::SeqCst);
            let _ = worker.join();
            let _ = ready.send(Err(format!(
                "Could not start scrubbing: {e}.{}",
                super::host::buffer_hint()
            )));
            return;
        }
    };
    let _ = ready.send(Ok(out_rate));

    feed(
        producer, lead, channels, stereo, start, out_rate, &tiles, &shared,
    );
    drop(stream);
    shared.stop.store(true, Ordering::SeqCst);
    let _ = worker.join();
}

/// Keeps the device's queue topped up until the drag ends and the sound
/// has faded out.
#[allow(clippy::too_many_arguments)]
fn feed(
    mut producer: Producer<f32>,
    lead: usize,
    channels: usize,
    stereo: [usize; 2],
    start: f64,
    out_rate: u32,
    tiles: &Mutex<Tiles>,
    shared: &Shared,
) {
    const BLOCK: usize = 64;
    let mut chase = Chase::new(start, out_rate);
    let mut reader = Reader::default();
    let mut block = vec![0f32; BLOCK * 2];
    let mut frames = vec![0f32; BLOCK * channels];
    let mut scratch = Vec::with_capacity(BLOCK);
    let cap = producer.buffer().capacity();
    // Give up waiting after two seconds of a stuck device.
    let mut idle = 0u32;
    while !shared.stop.load(Ordering::SeqCst) {
        if shared.release.load(Ordering::SeqCst) {
            chase.release();
        }
        if chase.done() {
            break;
        }
        let queued = (cap - producer.slots()) / channels;
        if queued >= lead || producer.slots() < frames.len() {
            thread::sleep(Duration::from_millis(1));
            idle += 1;
            if idle > 2000 && chase.releasing {
                break;
            }
            continue;
        }
        idle = 0;
        let target = f64::from_bits(shared.target.load(Ordering::Relaxed));
        produce(
            &mut chase,
            target,
            tiles,
            &mut reader,
            &mut block,
            &mut scratch,
        );
        shared.head.store(chase.pos().to_bits(), Ordering::Relaxed);
        shared.rate.store(chase.rate().to_bits(), Ordering::Relaxed);
        frames.fill(0.0);
        for (f, s) in frames
            .chunks_exact_mut(channels)
            .zip(block.as_chunks::<2>().0.iter())
        {
            let [ol, or] = stereo;
            if channels == 1 || ol == or {
                if let Some(x) = f.get_mut(if channels == 1 { 0 } else { ol }) {
                    *x = (s[0] + s[1]) * 0.5;
                }
            } else {
                if let Some(x) = f.get_mut(ol) {
                    *x = s[0];
                }
                if let Some(x) = f.get_mut(or) {
                    *x = s[1];
                }
            }
        }
        let _ = producer.push_partial_slice(&frames);
    }
}

fn build<T>(
    device: &cpal::Device,
    config: StreamConfig,
    mut consumer: Consumer<f32>,
) -> Result<cpal::Stream, String>
where
    T: SizedSample + FromSample<f32>,
{
    device
        .build_output_stream(
            config,
            move |out: &mut [T], _| {
                // No allocation, no locks: copy what is queued, silence for the rest.
                for s in out.iter_mut() {
                    *s = T::from_sample(consumer.pop().unwrap_or(0.0));
                }
            },
            |e| eprintln!("scrub stream error: {e}"),
            None,
        )
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::model::{new_id, ChannelLayout, MediaRef, Track};
    use crate::session::ops::{apply, ClipSpec, Op};
    use std::path::{Path, PathBuf};

    const RATE: u32 = 48_000;

    /// Runs the head toward `target(frame)` for `frames` frames; returns
    /// each frame's position, speed and loudness.
    fn chase_for(
        c: &mut Chase,
        frames: usize,
        target: impl Fn(usize) -> f64,
    ) -> Vec<(f64, f64, f64)> {
        (0..frames)
            .map(|i| {
                c.step(target(i));
                (c.pos(), c.rate(), c.gain())
            })
            .collect()
    }

    #[test]
    fn a_resting_head_is_silent() {
        let mut c = Chase::new(10_000.0, RATE);
        let run = chase_for(&mut c, 4_800, |_| 10_000.0);
        assert!(run
            .iter()
            .all(|&(p, r, g)| p == 10_000.0 && r == 0.0 && g == 0.0));
    }

    #[test]
    fn follows_a_steady_drag_at_its_speed() {
        for speed in [1.0, 0.5, 2.0, -1.0, -3.0] {
            let start = 200_000.0;
            let mut c = Chase::new(start, RATE);
            // A pointer moving at `speed`, updated once a screen frame (about 16 ms), like the interface sends.
            let run = chase_for(&mut c, RATE as usize, |i| {
                start + speed * (i - i % 800) as f64
            });
            let (_, rate, gain) = run[RATE as usize - 1];
            assert!(
                (rate - speed).abs() < 0.05 * speed.abs().max(1.0),
                "{speed}: {rate}"
            );
            assert!(gain > 0.99, "{speed}: {gain}");
            // Never jumpy: the speed changes by a sliver per frame.
            for w in run.windows(2) {
                assert!((w[1].1 - w[0].1).abs() < 0.01, "{speed}");
                assert!(w[1].0.is_finite());
            }
        }
    }

    #[test]
    fn speed_is_capped_both_ways() {
        for dir in [1.0, -1.0] {
            let start = 2_000_000.0;
            let mut c = Chase::new(start, RATE);
            let run = chase_for(&mut c, RATE as usize, |i| start + dir * 10.0 * i as f64);
            let top = run.iter().map(|x| x.1.abs()).fold(0.0, f64::max);
            assert!(top <= MAX_RATE + 1e-9 && top > MAX_RATE - 0.01, "{top}");
        }
    }

    #[test]
    fn stops_and_goes_quiet_when_the_pointer_stops() {
        let mut c = Chase::new(0.0, RATE);
        chase_for(&mut c, RATE as usize / 2, |i| i as f64);
        let stop_at = (RATE / 2) as f64;
        let run = chase_for(&mut c, RATE as usize / 2, |_| stop_at);
        let (pos, rate, gain) = run[run.len() - 1];
        assert!((pos - stop_at).abs() < 1.0, "lands on the pointer: {pos}");
        assert!(rate.abs() < 1e-3 && gain < 1e-3, "{rate} {gain}");
        // Quiet within a quarter second, gliding down rather than cutting off.
        let quiet = run.iter().position(|x| x.2 < 0.01).unwrap();
        assert!(quiet < RATE as usize / 4 && quiet > 100, "{quiet}");
    }

    #[test]
    fn a_far_jump_fades_out_moves_and_fades_back_in() {
        let mut c = Chase::new(0.0, RATE);
        chase_for(&mut c, RATE as usize / 2, |i| i as f64);
        let far = 10.0 * RATE as f64;
        let run = chase_for(&mut c, RATE as usize / 2, |i| far + i as f64);
        // It fades to silence before the move; never plays the stretch in between.
        let landed = run.iter().position(|x| x.0 >= far).unwrap();
        assert!(run[landed].2 < 1e-3);
        assert!(run[..landed].iter().all(|x| x.0 < RATE as f64));
        assert!(run[run.len() - 1].2 > 0.99);
    }

    #[test]
    fn release_fades_out_within_a_few_ms_and_ignores_bad_targets() {
        let mut c = Chase::new(0.0, RATE);
        chase_for(&mut c, RATE as usize / 4, |i| i as f64);
        c.release();
        let run = chase_for(&mut c, 2_400, |_| f64::NAN);
        assert!(c.done());
        assert!(run.iter().all(|x| x.0.is_finite() && x.2.is_finite()));
        // Releases smoothly: no step bigger than the fade allows.
        for w in run.windows(2) {
            assert!((w[1].2 - w[0].2).abs() < 0.01);
        }
    }

    // --- Tiles and the whole path, without a device ---

    /// Two seconds of a mono sine at `hz`.
    fn sine_wav(dir: &Path, hz: f32) -> PathBuf {
        let path = dir.join("tone.wav");
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: RATE,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut w = hound::WavWriter::create(&path, spec).unwrap();
        for i in 0..RATE as usize * 2 {
            let v = (std::f32::consts::TAU * hz * i as f32 / RATE as f32).sin() * 0.5;
            w.write_sample((v * 32_767.0) as i16).unwrap();
        }
        w.finalize().unwrap();
        path
    }

    fn session_with(path: &Path) -> Session {
        let mut s = Session::new("scrub", RATE);
        let m = MediaRef {
            id: new_id(),
            path: path.into(),
            relative_path: None,
            sample_rate: RATE,
            channels: 1,
            length: u64::from(RATE) * 2,
        };
        let mid = m.id;
        s.media.push(m);
        s.tracks.push(Track::new("t", ChannelLayout::Stereo));
        let tid = s.tracks[0].id;
        apply(
            &mut s,
            &Op::AddClips {
                track_id: tid,
                clips: vec![ClipSpec {
                    id: new_id(),
                    name: "c".into(),
                    media_id: mid,
                    start: 0,
                    source_in: 0,
                    source_out: u64::from(RATE) * 2,
                }],
            },
        )
        .unwrap();
        s
    }

    /// Plays a drag from `from` at `speed` for `secs`, rendering tiles as
    /// the worker would; returns the left channel.
    fn scrub(s: &Session, from: f64, speed: f64, secs: f64) -> Vec<f32> {
        let tiles = Mutex::new(Tiles::new());
        let mut maker = TileMaker::new(s.clone(), RATE);
        let mut chase = Chase::new(from, RATE);
        let mut reader = Reader::default();
        let mut scratch = Vec::new();
        let mut block = vec![0f32; 128];
        let mut left = Vec::new();
        let mut done = 0usize;
        while (done as f64) < secs * f64::from(RATE) {
            // The worker keeps up (it runs on its own thread in the app).
            while maker.fill(&tiles, tile_of(chase.pos() as i64), chase.rate() < 0.0) {}
            let target = from + speed * done as f64;
            produce(
                &mut chase,
                target,
                &tiles,
                &mut reader,
                &mut block,
                &mut scratch,
            );
            left.extend(block.chunks(2).map(|f| f[0]));
            done += 64;
        }
        left
    }

    /// Frequency from upward zero crossings over the last half.
    fn pitch(x: &[f32]) -> f64 {
        let half = &x[x.len() / 2..];
        let ups = half
            .windows(2)
            .filter(|w| w[0] < 0.0 && w[1] >= 0.0)
            .count();
        ups as f64 / (half.len() as f64 / f64::from(RATE))
    }

    #[test]
    fn scrubbing_plays_the_mix_at_the_drag_speed_and_pitch() {
        let dir = tempfile::tempdir().unwrap();
        let s = session_with(&sine_wav(dir.path(), 500.0));
        // Forward at normal speed: the tone at its own pitch, at full level.
        let fwd = scrub(&s, 2_000.0, 1.0, 0.6);
        assert!((pitch(&fwd) - 500.0).abs() < 15.0, "{}", pitch(&fwd));
        let peak = fwd[fwd.len() / 2..]
            .iter()
            .fold(0f32, |m, v| m.max(v.abs()));
        assert!(peak > 0.4 && peak < 0.55, "{peak}");
        // Half speed is an octave down, double speed an octave up.
        assert!((pitch(&scrub(&s, 2_000.0, 0.5, 0.6)) - 250.0).abs() < 10.0);
        assert!((pitch(&scrub(&s, 2_000.0, 2.0, 0.4)) - 1000.0).abs() < 25.0);
        // Backwards plays too, at the same pitch.
        let back = scrub(&s, 80_000.0, -1.0, 0.6);
        assert!((pitch(&back) - 500.0).abs() < 15.0, "{}", pitch(&back));
        assert!(back.iter().all(|v| v.is_finite()));
    }

    #[test]
    fn plug_ins_are_left_out() {
        let mut s = Session::new("scrub", RATE);
        s.master
            .effects
            .slots
            .push(crate::session::model::EffectSlot {
                id: new_id(),
                effect: "vst3:abc".into(),
                enabled: true,
                params: Default::default(),
                preset: None,
                sidechain: None,
                plugin_state: None,
                data: None,
                position: None,
            });
        assert!(without_plugins(s).master.effects.slots.is_empty());
    }

    /// Writes a scrubbed drag for listening: set ALTERED_FX_OUT to a folder
    /// and run `cargo test --lib scrub_listening -- --ignored`. A 500 Hz
    /// tone with a click every 100 ms, dragged forward at 1x, slowed to a
    /// stop, then backward fast, then a wobble back and forth.
    #[test]
    #[ignore = "writes files for listening"]
    fn scrub_listening_file() {
        let Ok(out) = std::env::var("ALTERED_FX_OUT") else {
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tone.wav");
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: RATE,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut w = hound::WavWriter::create(&path, spec).unwrap();
        for i in 0..RATE as usize * 2 {
            let t = i as f32 / RATE as f32;
            let tone = (std::f32::consts::TAU * 500.0 * t).sin() * 0.3;
            let click = if i % 4800 < 48 { 0.6 } else { 0.0 };
            w.write_sample(((tone + click) * 32_767.0) as i16).unwrap();
        }
        w.finalize().unwrap();
        let s = session_with(&path);
        let tiles = Mutex::new(Tiles::new());
        let mut maker = TileMaker::new(s, RATE);
        let mut chase = Chase::new(0.0, RATE);
        let mut reader = Reader::default();
        let mut scratch = Vec::new();
        let mut block = vec![0f32; 128];
        let mut all = Vec::new();
        let r = f64::from(RATE);
        // The pointer path, sampled once a screen frame (800 frames).
        let path_at = |t: f64| -> f64 {
            if t < 1.0 {
                t * r
            } else if t < 1.5 {
                r + (t - 1.0) * 0.4 * r
            } else if t < 2.0 {
                1.2 * r
            } else if t < 2.5 {
                1.2 * r - (t - 2.0) * 2.0 * r
            } else {
                0.4 * r + (t * 6.0).sin() * 0.2 * r
            }
        };
        let total = RATE as usize * 4;
        let mut done = 0;
        while done < total {
            while maker.fill(&tiles, tile_of(chase.pos() as i64), chase.rate() < 0.0) {}
            let frame = (done / 800 * 800) as f64 / r;
            produce(
                &mut chase,
                path_at(frame),
                &tiles,
                &mut reader,
                &mut block,
                &mut scratch,
            );
            all.extend_from_slice(&block);
            done += 64;
        }
        let out = std::path::PathBuf::from(out);
        std::fs::create_dir_all(&out).unwrap();
        let spec = hound::WavSpec {
            channels: 2,
            sample_rate: RATE,
            bits_per_sample: 32,
            sample_format: hound::SampleFormat::Float,
        };
        let mut w = hound::WavWriter::create(out.join("scrub-drag.wav"), spec).unwrap();
        for v in all {
            w.write_sample(v).unwrap();
        }
        w.finalize().unwrap();
    }
}
