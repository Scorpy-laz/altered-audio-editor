// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) The GeekCast Radio Network, LLC.
// This file is part of Altered Audio Editor.

//! Room Reverb (an algorithmic reverb) and Tape Delay (an analog-style
//! delay). Both are original code from textbook methods: the reverb is a
//! bank of parallel damped feedback combs into a few all-pass diffusers
//! (the Schroeder layout, with comb lengths in the same spread the classic
//! designs use); the delay is a feedback delay line with a low-pass,
//! soft saturation and a slow wobble in the loop.

use std::f64::consts::TAU;

use super::glide::{Glide, TimeGlide};
use super::{num, EffectInfo, Params, Processor};

pub fn room_info() -> EffectInfo {
    EffectInfo {
        id: "altaudio.room-reverb",
        name: "Room Reverb",
        category: "Delay and Reverb",
        description: "A built-in reverb that needs no impulse file. Size sets how big the space feels, Decay how long it rings, Damping how fast the highs die away.",
        params: vec![
            num("size", "Size", "%", 20.0, 200.0, 100.0, 1.0),
            num("decay", "Decay", "s", 0.2, 12.0, 1.8, 0.1).log(),
            num("predelay", "Pre-delay", "ms", 0.0, 200.0, 15.0, 1.0),
            num("damping", "Damping", "%", 0.0, 100.0, 40.0, 1.0),
            num("width", "Width", "%", 0.0, 100.0, 100.0, 1.0),
            num("mix", "Mix", "%", 0.0, 100.0, 25.0, 1.0),
        ],
    }
}

pub fn tape_info() -> EffectInfo {
    EffectInfo {
        id: "altaudio.tape-delay",
        name: "Tape Delay",
        category: "Delay and Reverb",
        description: "An echo that gets darker, dirtier and a little wobbly each time it repeats, like an old tape or bucket-brigade delay.",
        params: vec![
            num("time", "Time", "ms", 20.0, 1500.0, 350.0, 1.0).log(),
            num("feedback", "Feedback", "%", 0.0, 95.0, 40.0, 1.0),
            num("tone", "Tone", "Hz", 500.0, 16000.0, 3500.0, 1.0).log(),
            num("saturation", "Saturation", "%", 0.0, 100.0, 30.0, 1.0),
            num("wobble", "Wobble", "%", 0.0, 100.0, 20.0, 1.0),
            num("offset", "Right side time", "%", 50.0, 150.0, 100.0, 1.0),
            num("mix", "Mix", "%", 0.0, 100.0, 30.0, 1.0),
        ],
    }
}

// --- Room Reverb ---

/// Comb and all-pass lengths for 44.1 kHz, in frames.
const COMBS: [usize; 8] = [1116, 1188, 1277, 1356, 1422, 1491, 1557, 1617];
const ALLPASS: [usize; 4] = [556, 441, 341, 225];
/// How much longer the right side's lines are, in frames at 44.1 kHz.
const SPREAD: usize = 23;
/// The largest Size and Pre-delay the controls allow: lines are made this
/// long up front so automation never has to grow them.
const MAX_SIZE: f64 = 200.0;
const MAX_PREDELAY_MS: f64 = 200.0;

/// A delay line whose length can change while it runs: it is written all
/// the way round and read `len` frames back, so a new length only moves
/// the read point and keeps what the line holds.
struct Line {
    buf: Vec<f32>,
    at: usize,
    len: usize,
}

impl Line {
    fn new(len: usize, max: usize) -> Self {
        let len = len.max(2);
        Self {
            buf: vec![0.0; len.max(max)],
            at: 0,
            len,
        }
    }

    /// The sample written `len` frames ago.
    #[inline]
    fn read(&self) -> f32 {
        let n = self.buf.len();
        let r = if self.at >= self.len {
            self.at - self.len
        } else {
            self.at + n - self.len
        };
        self.buf[r]
    }

    #[inline]
    fn write(&mut self, x: f32) {
        self.buf[self.at] = x;
        self.at += 1;
        if self.at == self.buf.len() {
            self.at = 0;
        }
    }

    /// A new length, capped at what the line can hold.
    fn set_len(&mut self, len: usize) {
        self.len = len.clamp(2, self.buf.len());
    }
}

struct Comb {
    line: Line,
    g: f32,
    /// Input scale that keeps the loudness the same at any decay.
    k: f32,
    damp: f32,
    held: f32,
}

impl Comb {
    fn new(len: usize, max: usize, rate: u32, decay: f64, damp: f32) -> Self {
        let mut c = Self {
            line: Line::new(len, max),
            g: 0.0,
            k: 0.0,
            damp,
            held: 0.0,
        };
        c.tune(len, rate, decay, damp);
        c
    }

    /// Length, decay and damping, keeping what is ringing.
    fn tune(&mut self, len: usize, rate: u32, decay: f64, damp: f32) {
        self.line.set_len(len);
        let secs = self.line.len as f64 / f64::from(rate);
        let g = 10f64.powf(-3.0 * secs / decay).min(0.9995);
        self.g = g as f32;
        self.k = ((1.0 - g * g) / COMBS.len() as f64).sqrt() as f32;
        self.damp = damp;
    }

    #[inline]
    fn run(&mut self, x: f32) -> f32 {
        let out = self.line.read();
        self.held = out * (1.0 - self.damp) + self.held * self.damp;
        if self.held.abs() < 1e-20 {
            self.held = 0.0;
        }
        self.line.write(x * self.k + self.held * self.g);
        out
    }
}

struct Diffuser(Line);

impl Diffuser {
    #[inline]
    fn run(&mut self, x: f32) -> f32 {
        let held = self.0.read();
        self.0.write(x + held * 0.5);
        held - x * 0.5
    }
}

/// The settings that set the lines' lengths and loop gains.
#[derive(Clone, Copy, PartialEq)]
struct Shape {
    size: f64,
    decay: f64,
    damping: f64,
    predelay: f64,
}

impl Shape {
    fn of(p: &Params) -> Self {
        Self {
            size: p.get("size"),
            decay: p.get("decay"),
            damping: p.get("damping"),
            predelay: p.get("predelay"),
        }
    }
}

fn line_len(n: usize, extra: usize, scale: f64) -> usize {
    ((n + extra) as f64 * scale) as usize
}

pub struct Room {
    rate: u32,
    combs: [[Comb; 8]; 2],
    diffusers: [[Diffuser; 4]; 2],
    pre: Line,
    shape: Shape,
    width: Glide,
    mix: Glide,
}

impl Room {
    pub fn new(p: &Params, rate: u32) -> Self {
        let r = f64::from(rate) / 44_100.0;
        let scale = r * p.get("size") / 100.0;
        let most = r * MAX_SIZE / 100.0;
        let decay = p.get("decay").max(0.05);
        // More damping means a stronger low-pass in each loop.
        let damp = (p.get("damping") / 100.0 * 0.85) as f32;
        let combs = |extra: usize| {
            COMBS.map(|n| {
                Comb::new(
                    line_len(n, extra, scale),
                    line_len(n, extra, most),
                    rate,
                    decay,
                    damp,
                )
            })
        };
        let diffusers = |extra: usize| {
            ALLPASS.map(|n| {
                Diffuser(Line::new(
                    line_len(n, extra, scale),
                    line_len(n, extra, most),
                ))
            })
        };
        let pre_len = |ms: f64| (ms / 1000.0 * f64::from(rate)) as usize + 1;
        // A pre-delay of 0 still delays one frame, as it always has; the
        // line's floor of two frames is not used for it.
        let mut pre = Line::new(
            pre_len(p.get("predelay")),
            pre_len(MAX_PREDELAY_MS.max(p.get("predelay"))),
        );
        pre.len = pre_len(p.get("predelay")).max(1);
        Self {
            rate,
            combs: [combs(0), combs(SPREAD)],
            diffusers: [diffusers(0), diffusers(SPREAD)],
            pre,
            shape: Shape::of(p),
            width: Glide::new(p.get("width") / 100.0),
            mix: Glide::new(p.get("mix") / 100.0),
        }
    }

    /// The wet share (0 to 1) at once, keeping the tail.
    pub(crate) fn set_mix(&mut self, mix: f32) {
        self.mix.jump(f64::from(mix.clamp(0.0, 1.0)));
    }

    /// Takes a new size, decay, damping and pre-delay without clearing
    /// the lines: only the read points and loop gains move.
    fn reshape(&mut self, s: Shape) {
        let r = f64::from(self.rate) / 44_100.0;
        let scale = r * s.size / 100.0;
        let decay = s.decay.max(0.05);
        let damp = (s.damping / 100.0 * 0.85) as f32;
        for (side, extra) in [(0, 0), (1, SPREAD)] {
            for (c, n) in self.combs[side].iter_mut().zip(COMBS) {
                c.tune(line_len(n, extra, scale), self.rate, decay, damp);
            }
            for (d, n) in self.diffusers[side].iter_mut().zip(ALLPASS) {
                d.0.set_len(line_len(n, extra, scale));
            }
        }
        let pre = (s.predelay / 1000.0 * f64::from(self.rate)) as usize + 1;
        self.pre.len = pre.clamp(1, self.pre.buf.len());
        self.shape = s;
    }
}

impl Processor for Room {
    fn process(&mut self, buf: &mut [f32]) {
        let frames = buf.len() / 2;
        self.width.begin(frames);
        self.mix.begin(frames);
        for f in buf.as_chunks_mut::<2>().0 {
            let input = (f[0] + f[1]) * 0.5;
            let x = self.pre.read();
            self.pre.write(input);
            let mut wet = [0f32; 2];
            for (c, w) in wet.iter_mut().enumerate() {
                let mut s: f32 = self.combs[c].iter_mut().map(|k| k.run(x)).sum();
                for d in &mut self.diffusers[c] {
                    s = d.run(s);
                }
                *w = s;
            }
            let width = self.width.next() as f32;
            let mix = self.mix.next() as f32;
            let wet1 = width / 2.0 + 0.5;
            let wet2 = (1.0 - width) / 2.0;
            let l = wet[0] * wet1 + wet[1] * wet2;
            let r = wet[1] * wet1 + wet[0] * wet2;
            f[0] = f[0] * (1.0 - mix) + l * mix;
            f[1] = f[1] * (1.0 - mix) + r * mix;
        }
    }

    fn reset(&mut self) {
        for side in &mut self.combs {
            for c in side {
                c.line.buf.fill(0.0);
                c.held = 0.0;
            }
        }
        for side in &mut self.diffusers {
            for d in side {
                d.0.buf.fill(0.0);
            }
        }
        self.pre.buf.fill(0.0);
    }

    fn update(&mut self, p: &Params) -> bool {
        let s = Shape::of(p);
        if s != self.shape {
            self.reshape(s);
        }
        self.width.set(p.get("width") / 100.0);
        self.mix.set(p.get("mix") / 100.0);
        true
    }
}

// --- Tape Delay ---

/// How quickly a new delay time is reached: the echo bends in pitch on
/// the way, as a tape machine changing speed does.
const TIME_GLIDE_MS: f64 = 60.0;

pub struct Tape {
    rate: u32,
    rings: [Vec<f32>; 2],
    at: usize,
    times: [TimeGlide; 2],
    feedback: Glide,
    coef: f32,
    tone: [f32; 2],
    drive: f32,
    wobble: f64,
    phase: f64,
    step: f64,
    mix: Glide,
}

impl Tape {
    pub fn new(p: &Params, rate: u32) -> Self {
        let [left, right] = Self::times(p, rate);
        // Room for the longest time plus the wobble's reach.
        let len = (1.5 * 1.5 * f64::from(rate)) as usize + 64;
        let mut me = Self {
            rate,
            rings: [vec![0.0; len], vec![0.0; len]],
            at: 0,
            times: [
                TimeGlide::new(left, rate, TIME_GLIDE_MS),
                TimeGlide::new(right, rate, TIME_GLIDE_MS),
            ],
            feedback: Glide::new(p.get("feedback") / 100.0),
            coef: 0.0,
            tone: [0.0; 2],
            drive: 1.0,
            wobble: 0.0,
            phase: 0.0,
            step: 0.7 / f64::from(rate),
            mix: Glide::new(p.get("mix") / 100.0),
        };
        me.color(p);
        me
    }

    /// Left and right delay times in frames.
    fn times(p: &Params, rate: u32) -> [f64; 2] {
        let time = p.get("time") / 1000.0 * f64::from(rate);
        [time, time * p.get("offset") / 100.0]
    }

    /// Tone, saturation and wobble: no state of their own to keep.
    fn color(&mut self, p: &Params) {
        let rate = f64::from(self.rate);
        let hz = p.get("tone").min(rate * 0.45);
        self.coef = (1.0 - (-TAU * hz / rate).exp()) as f32;
        self.drive = 1.0 + (p.get("saturation") / 100.0 * 3.0) as f32;
        // Up to about a quarter of a percent of speed, 6 ms of drift at most.
        self.wobble = p.get("wobble") / 100.0 * 0.004 * rate;
    }

    fn read(ring: &[f32], at: usize, delay: f64) -> f32 {
        let n = ring.len();
        let delay = delay.clamp(1.0, (n - 2) as f64);
        let pos = at as f64 + n as f64 - delay;
        let whole = pos.floor();
        let frac = (pos - whole) as f32;
        let i0 = whole as usize % n;
        ring[i0] * (1.0 - frac) + ring[(i0 + 1) % n] * frac
    }
}

impl Processor for Tape {
    fn process(&mut self, buf: &mut [f32]) {
        let n = self.rings[0].len();
        let frames = buf.len() / 2;
        self.feedback.begin(frames);
        self.mix.begin(frames);
        for f in buf.as_chunks_mut::<2>().0 {
            // A slow sine plus a faster, smaller one, like a worn transport.
            let drift =
                ((self.phase * TAU).sin() + 0.3 * (self.phase * 7.3 * TAU).sin()) * self.wobble;
            self.phase = (self.phase + self.step).fract();
            let feedback = self.feedback.next() as f32;
            let mix = self.mix.next() as f32;
            #[allow(clippy::needless_range_loop)]
            for c in 0..2 {
                let time = self.times[c].next();
                let echo = Self::read(&self.rings[c], self.at, time + drift);
                self.tone[c] += self.coef * (echo - self.tone[c]);
                if self.tone[c].abs() < 1e-20 {
                    self.tone[c] = 0.0;
                }
                // Unity gain for small sounds, bounded for big ones.
                let soft = (self.tone[c] * self.drive).tanh() / self.drive;
                let x = f[c];
                self.rings[c][self.at] = x + soft * feedback;
                f[c] = x * (1.0 - mix) + soft * mix;
            }
            self.at = (self.at + 1) % n;
        }
    }

    fn reset(&mut self) {
        for r in &mut self.rings {
            r.fill(0.0);
        }
        self.tone = [0.0; 2];
        self.phase = 0.0;
    }

    fn update(&mut self, p: &Params) -> bool {
        let times = Self::times(p, self.rate);
        for (g, t) in self.times.iter_mut().zip(times) {
            g.set(t);
        }
        self.feedback.set(p.get("feedback") / 100.0);
        self.mix.set(p.get("mix") / 100.0);
        self.color(p);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::super::build;
    use super::super::tests::slot;

    fn run(effect: &str, params: &[(&str, f64)], mut b: Vec<f32>) -> Vec<f32> {
        let mut p = build(&slot(effect, params), 48_000).unwrap();
        for chunk in b.chunks_mut(2 * 777) {
            p.process(chunk);
        }
        b
    }

    fn noise(frames: usize) -> Vec<f32> {
        let mut s = 12345u32;
        (0..frames * 2)
            .map(|_| {
                s = s.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                ((s >> 8) as f32 / 8_388_608.0 - 1.0) * 0.3
            })
            .collect()
    }

    fn impulse(frames: usize) -> Vec<f32> {
        let mut v = vec![0.0; frames * 2];
        v[0] = 1.0;
        v[1] = 1.0;
        v
    }

    fn rms(b: &[f32]) -> f32 {
        (b.iter().map(|x| x * x).sum::<f32>() / b.len() as f32).sqrt()
    }

    #[test]
    fn with_no_mix_both_pass_the_sound_untouched() {
        for fx in ["altaudio.room-reverb", "altaudio.tape-delay"] {
            let input = noise(4000);
            assert_eq!(run(fx, &[("mix", 0.0)], input.clone()), input, "{fx}");
        }
    }

    #[test]
    fn the_reverb_rings_for_about_its_decay_and_keeps_its_level() {
        let set = [("mix", 100.0), ("predelay", 0.0), ("decay", 1.0)];
        let wet = run("altaudio.room-reverb", &set, noise(48_000 * 3));
        // Steady noise in gives roughly the same level out.
        let level = rms(&wet[2 * 48_000..2 * 96_000]);
        assert!(level > 0.1 && level < 0.25, "level {level}");
        // A click rings, then dies: much quieter a decay later than just after.
        let tail = run("altaudio.room-reverb", &set, impulse(48_000 * 3));
        let early = rms(&tail[2 * 2000..2 * 6000]);
        let late = rms(&tail[2 * 96_000..2 * 100_000]);
        assert!(early > 0.0 && late < early * 0.03, "{early} {late}");
        assert!(tail.iter().all(|s| s.is_finite()));
    }

    #[test]
    fn a_longer_decay_rings_longer_and_zero_width_is_mono() {
        let rest = |d: f64| {
            let t = run(
                "altaudio.room-reverb",
                &[("mix", 100.0), ("predelay", 0.0), ("decay", d)],
                impulse(48_000 * 2),
            );
            rms(&t[2 * 60_000..2 * 70_000])
        };
        assert!(rest(4.0) > rest(0.5) * 5.0);
        let mono = run(
            "altaudio.room-reverb",
            &[("mix", 100.0), ("width", 0.0)],
            impulse(20_000),
        );
        assert!(mono.chunks(2).all(|f| (f[0] - f[1]).abs() < 1e-6));
    }

    #[test]
    fn the_delay_repeats_after_its_time_and_fades() {
        let set = [
            ("time", 100.0),
            ("feedback", 50.0),
            ("tone", 16000.0),
            ("saturation", 0.0),
            ("wobble", 0.0),
            ("mix", 100.0),
        ];
        let out = run("altaudio.tape-delay", &set, impulse(48_000));
        let at = |ms: usize| {
            let c = ms * 48;
            out[2 * (c - 30)..2 * (c + 30)]
                .iter()
                .fold(0f32, |m, s| m.max(s.abs()))
        };
        let (a, b, c) = (at(100), at(200), at(300));
        assert!(a > 0.2 && b < a && c < b && b > 0.05, "{a} {b} {c}");
    }

    #[test]
    fn the_delay_stays_bounded_at_high_feedback_with_saturation() {
        let out = run(
            "altaudio.tape-delay",
            &[("feedback", 95.0), ("saturation", 100.0), ("mix", 100.0)],
            noise(48_000 * 4),
        );
        assert!(out.iter().all(|s| s.is_finite() && s.abs() < 4.0));
    }

    mod automation {
        use super::super::super::glide::testing::*;
        use super::super::{Room, Tape};
        use crate::dsp::effects::Processor;

        const ROOM: &str = "altaudio.room-reverb";
        const TAPE: &str = "altaudio.tape-delay";

        #[test]
        fn every_room_and_tape_control_changes_in_place() {
            every_control_updates(ROOM, &[]);
            every_control_updates(TAPE, &[]);
        }

        /// The reported fault: Mix moving under automation wiped the tail
        /// every block, so the reverb vanished whenever the line sloped.
        #[test]
        fn a_moving_mix_keeps_the_reverb_tail() {
            let set = [("decay", 2.0), ("predelay", 10.0)];
            let tail = |moving: bool| {
                let mut fx = make(ROOM, &[("mix", if moving { 65.0 } else { 74.0 })]);
                let mut sound = noise(RATE as usize, 3);
                if moving {
                    let half = sound.len() / 2;
                    let (a, b) = sound.split_at_mut(half);
                    ramp(&mut *fx, ROOM, &set, "mix", (65.0, 0.0), a, 64);
                    ramp(&mut *fx, ROOM, &set, "mix", (0.0, 74.0), b, 64);
                } else {
                    for c in sound.chunks_mut(128) {
                        fx.process(c);
                    }
                }
                // Then silence, at Mix 74 in both: only the tail sounds.
                let mut after = vec![0.0; RATE as usize];
                for c in after.chunks_mut(128) {
                    fx.process(c);
                }
                rms(&after[..RATE as usize / 2])
            };
            let (still, moved) = (tail(false), tail(true));
            assert!(still > 0.01, "the room rings: {still}");
            assert!(moved > still * 0.7, "tail kept: {moved} vs {still}");
        }

        #[test]
        fn size_and_pre_delay_move_without_new_memory_or_silence() {
            let p = params(ROOM, &[("mix", 100.0), ("decay", 3.0)]);
            let mut room = Room::new(&p, RATE);
            let sizes = |r: &Room| -> Vec<(usize, *const f32)> {
                r.combs
                    .iter()
                    .flatten()
                    .map(|c| (c.line.buf.capacity(), c.line.buf.as_ptr()))
                    .chain(
                        r.diffusers
                            .iter()
                            .flatten()
                            .map(|d| (d.0.buf.capacity(), d.0.buf.as_ptr())),
                    )
                    .chain([(r.pre.buf.capacity(), r.pre.buf.as_ptr())])
                    .collect()
            };
            let before = sizes(&room);
            let mut b = noise(RATE as usize / 2, 5);
            room.process(&mut b);
            for (size, pre) in [(200.0, 200.0), (20.0, 0.0), (137.0, 55.0)] {
                let q = params(
                    ROOM,
                    &[
                        ("mix", 100.0),
                        ("decay", 3.0),
                        ("size", size),
                        ("predelay", pre),
                    ],
                );
                assert!(room.update(&q));
                let mut quiet = vec![0.0; 2 * 4800];
                for c in quiet.chunks_mut(256) {
                    room.process(c);
                }
                assert!(quiet.iter().all(|s| s.is_finite()));
                assert!(rms(&quiet) > 1e-3, "still ringing at size {size}");
            }
            assert_eq!(sizes(&room), before, "no line was grown or replaced");
        }

        #[test]
        fn a_room_left_alone_after_update_sounds_as_built() {
            let set = [("mix", 60.0), ("size", 80.0), ("predelay", 30.0)];
            let mut built = make(ROOM, &set);
            let mut updated = make(ROOM, &[]);
            assert!(updated.update(&params(ROOM, &set)));
            let mut a = noise(9600, 11);
            let mut b = a.clone();
            for c in a.chunks_mut(256) {
                built.process(c);
            }
            for c in b.chunks_mut(256) {
                updated.process(c);
            }
            // Lines empty, same lengths: once the mix ramp is done, alike.
            assert!(a[512..]
                .iter()
                .zip(&b[512..])
                .all(|(x, y)| (x - y).abs() < 1e-5));
        }

        #[test]
        fn a_new_tape_time_glides_and_keeps_the_echoes() {
            let set = [
                ("feedback", 90.0),
                ("tone", 16000.0),
                ("wobble", 0.0),
                ("mix", 100.0),
            ];
            let p = params(TAPE, &set);
            let mut tape = Tape::new(&p, RATE);
            let rings = (tape.rings[0].capacity(), tape.rings[0].as_ptr());
            let mut b = noise(RATE as usize / 4, 2);
            tape.process(&mut b);
            let mut quiet = vec![0.0; RATE as usize * 2];
            ramp(
                &mut tape,
                TAPE,
                &set,
                "time",
                (350.0, 600.0),
                &mut quiet,
                256,
            );
            assert!(quiet.iter().all(|s| s.is_finite()));
            assert!(rms(&quiet[quiet.len() / 2..]) > 1e-3, "echoes kept");
            // A glide, not a jump: no step bigger than the noise makes.
            assert!(largest_step(&quiet) < 0.8);
            assert_eq!((tape.rings[0].capacity(), tape.rings[0].as_ptr()), rings);
        }
    }
}
