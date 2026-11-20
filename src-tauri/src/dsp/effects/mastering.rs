// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) The GeekCast Radio Network, LLC.
// This file is part of Altered Audio Editor.

//! Mastering Suite: the last polish for a finished mix, in one window.
//! Five stages run in this order, each built from our own effects:
//!
//! 1. Equalizer: an optional low cut, a low shelf, one peak band and a
//!    high shelf (the cookbook filters in `biquad.rs`).
//! 2. Reverb: a small, short room (Room Reverb) blended in by Amount.
//!    At 0 it is out of the path.
//! 3. Exciter: the highs above a corner are driven into a saturation
//!    curve (the Tube Preamp's lopsided tube curve, a soft tape curve or
//!    a harder clip) and the result is mixed back over the dry sound,
//!    adding harmonics and air.
//! 4. Width: the Stereo Expander's mid/side width, with the bass kept
//!    in the middle.
//! 5. Loudness: Push drives the sound into the true-peak limiter, which
//!    keeps every peak (between samples too) under the Ceiling. It
//!    always looks ahead the same distance, so switching it off keeps the
//!    same delay and playback never has to restart.
//!
//! Output trims the level last. It only goes down, so the ceiling holds.
//! With every stage flat and Loudness off, the sound passes unchanged
//! (only delayed by the look-ahead).

use std::collections::BTreeMap;

use super::true_peak::{LimiterSettings, TruePeakLimiter};
use super::{choice, db_to_gain, num, space, stereo, toggle, Delay, EffectInfo, Params, Processor};
use crate::dsp::biquad::{Biquad, Coeffs, Shape};

pub const ID: &str = "altaudio.mastering";

/// Exciter flavours: a lopsided tube curve (warm, even harmonics), a soft
/// tape curve with its top rolled off, and a harder clip for sparkle.
pub const MODES: &[&str] = &["Tube", "Tape", "Bright"];

/// The limiter's fixed look-ahead, ms.
const LOOKAHEAD_MS: f32 = 5.0;

pub fn info() -> EffectInfo {
    EffectInfo {
        id: ID,
        name: "Mastering Suite",
        category: "Special",
        description: "The final polish for a finished mix, in one window: tone (EQ), a touch of room, an exciter for warmth and air, stereo width, and a true-peak limiter that makes it louder without going over the ceiling. Put it on the master or a whole-mix track, last in the chain.",
        params: vec![
            toggle("lc_on", "Low cut", false).group("Equalizer"),
            num("lc_freq", "Low cut at", "Hz", 20.0, 200.0, 30.0, 1.0).log().group("Equalizer"),
            num("ls_freq", "Low shelf", "Hz", 30.0, 800.0, 100.0, 1.0).log().group("Equalizer"),
            num("ls_gain", "Low shelf gain", "dB", -12.0, 12.0, 0.0, 0.1).group("Equalizer"),
            num("pk_freq", "Peak", "Hz", 100.0, 12_000.0, 1000.0, 1.0).log().group("Equalizer"),
            num("pk_gain", "Peak gain", "dB", -12.0, 12.0, 0.0, 0.1).group("Equalizer"),
            num("pk_q", "Peak width (Q)", "", 0.3, 6.0, 1.0, 0.01).log().group("Equalizer"),
            num("hs_freq", "High shelf", "Hz", 1000.0, 20_000.0, 8000.0, 10.0).log().group("Equalizer"),
            num("hs_gain", "High shelf gain", "dB", -12.0, 12.0, 0.0, 0.1).group("Equalizer"),
            num("rv_amount", "Amount", "%", 0.0, 100.0, 0.0, 1.0).group("Reverb"),
            num("ex_amount", "Amount", "%", 0.0, 100.0, 0.0, 1.0).group("Exciter"),
            choice("ex_mode", "Character", MODES, 0).group("Exciter"),
            num("width", "Width", "%", 0.0, 200.0, 100.0, 1.0).group("Width"),
            toggle("l_on", "On", true).group("Loudness"),
            num("push", "Push", "dB", 0.0, 18.0, 0.0, 0.1).group("Loudness"),
            num("ceiling", "Ceiling", "dBTP", -6.0, 0.0, -1.0, 0.1).group("Loudness"),
            num("release", "Release", "ms", 10.0, 500.0, 80.0, 1.0).log().group("Loudness"),
            num("output", "Output gain", "dB", -18.0, 0.0, 0.0, 0.1).group("Output"),
        ],
    }
}

// --- Equalizer ---

/// Low cut, low shelf, peak and high shelf on both channels. A band that
/// does nothing is skipped, so a flat EQ is exactly transparent.
struct Eq {
    rate: u32,
    bands: [[Biquad; 2]; 4],
    on: [bool; 4],
}

impl Eq {
    fn new(p: &Params, rate: u32) -> Self {
        let mut eq = Self {
            rate,
            bands: [[Biquad::new(Coeffs::FLAT); 2]; 4],
            on: [false; 4],
        };
        eq.set(p);
        eq
    }

    /// New settings, keeping each filter's memory so a moving band glides.
    fn set(&mut self, p: &Params) {
        let r = self.rate;
        let want = [
            p.on("lc_on")
                .then(|| Coeffs::new(Shape::HighPass, r, p.get("lc_freq"), 0.707, 0.0)),
            (p.get("ls_gain").abs() > 1e-6)
                .then(|| Coeffs::new(Shape::LowShelf, r, p.get("ls_freq"), 0.8, p.get("ls_gain"))),
            (p.get("pk_gain").abs() > 1e-6).then(|| {
                Coeffs::new(
                    Shape::Peak,
                    r,
                    p.get("pk_freq"),
                    p.get("pk_q"),
                    p.get("pk_gain"),
                )
            }),
            (p.get("hs_gain").abs() > 1e-6)
                .then(|| Coeffs::new(Shape::HighShelf, r, p.get("hs_freq"), 0.8, p.get("hs_gain"))),
        ];
        for (i, c) in want.into_iter().enumerate() {
            match c {
                Some(c) => {
                    if !self.on[i] {
                        // Coming in fresh: start from silence, not old memory.
                        self.bands[i] = [Biquad::new(c), Biquad::new(c)];
                    } else {
                        self.bands[i][0].set(c);
                        self.bands[i][1].set(c);
                    }
                    self.on[i] = true;
                }
                None => self.on[i] = false,
            }
        }
    }

    fn active(&self) -> bool {
        self.on.iter().any(|&o| o)
    }

    fn process(&mut self, buf: &mut [f32]) {
        for (band, on) in self.bands.iter_mut().zip(self.on) {
            if !on {
                continue;
            }
            for f in buf.as_chunks_mut::<2>().0 {
                f[0] = band[0].run(f[0]);
                f[1] = band[1].run(f[1]);
            }
        }
    }

    fn reset(&mut self) {
        for b in self.bands.iter_mut().flatten() {
            b.reset();
        }
    }
}

// --- Exciter ---

/// Drives the highs into a curve and mixes the result over the dry sound.
struct Exciter {
    rate: u32,
    mode: usize,
    /// Mix of the excited highs, 0 to about 0.6.
    mix: f32,
    /// Drive into the curve (linear).
    drive: f32,
    hp: [[Biquad; 2]; 2],
    /// Tape only: rolls the top off the harmonics.
    lp: [Biquad; 2],
    /// Tube only: removes the offset the lopsided curve adds.
    dc: [(f32, f32); 2],
    dc_r: f32,
}

impl Exciter {
    fn new(p: &Params, rate: u32) -> Self {
        let mut e = Self {
            rate,
            mode: usize::MAX,
            mix: 0.0,
            drive: 1.0,
            hp: [[Biquad::new(Coeffs::FLAT); 2]; 2],
            lp: [Biquad::new(Coeffs::FLAT); 2],
            dc: [(0.0, 0.0); 2],
            dc_r: (-std::f32::consts::TAU * 10.0 / rate as f32).exp(),
        };
        e.set(p);
        e
    }

    fn corner(mode: usize) -> f64 {
        match mode {
            0 => 2500.0,
            1 => 1800.0,
            _ => 4500.0,
        }
    }

    fn set(&mut self, p: &Params) {
        let amount = p.get("ex_amount").clamp(0.0, 100.0);
        self.mix = (amount / 100.0 * 0.6) as f32;
        // 6 dB of drive at the lightest, 24 dB at full.
        self.drive = db_to_gain(6.0 + amount * 0.18);
        let mode = p.index("ex_mode").min(MODES.len() - 1);
        if mode != self.mode {
            self.mode = mode;
            let hp = Coeffs::new(Shape::HighPass, self.rate, Self::corner(mode), 0.707, 0.0);
            self.hp = [[Biquad::new(hp); 2]; 2];
            let lp = Coeffs::new(Shape::LowPass, self.rate, 9000.0, 0.707, 0.0);
            self.lp = [Biquad::new(lp); 2];
            self.dc = [(0.0, 0.0); 2];
        }
    }

    fn active(&self) -> bool {
        self.mix > 0.0
    }

    #[inline]
    fn curve(&self, x: f32) -> f32 {
        match self.mode {
            0 => super::tube::tube(x, 1.0, 0.35),
            1 => stereo::shaped(0, x),
            _ => stereo::shaped(0, x * 1.6).clamp(-0.9, 0.9),
        }
    }

    fn process(&mut self, buf: &mut [f32]) {
        let g = self.drive;
        for f in buf.as_chunks_mut::<2>().0 {
            for (c, s) in f.iter_mut().enumerate() {
                let x = *s;
                let first = self.hp[c][0].run(x);
                let band = self.hp[c][1].run(first);
                let mut y = self.curve(band * g) / g;
                match self.mode {
                    0 => {
                        let (x1, y1) = self.dc[c];
                        let out = y - x1 + self.dc_r * y1;
                        self.dc[c] = (y, if out.abs() < 1e-20 { 0.0 } else { out });
                        y = out;
                    }
                    1 => y = self.lp[c].run(y),
                    _ => {}
                }
                *s = x + y * self.mix;
            }
        }
    }

    fn reset(&mut self) {
        for b in self.hp.iter_mut().flatten() {
            b.reset();
        }
        for b in &mut self.lp {
            b.reset();
        }
        self.dc = [(0.0, 0.0); 2];
    }
}

// --- The suite ---

/// The room the Reverb stage blends in: small and short, so it adds
/// glue and depth, not an obvious echo.
fn room(rate: u32) -> space::Room {
    let saved: BTreeMap<String, f64> = [
        ("size", 55.0),
        ("decay", 0.9),
        ("predelay", 8.0),
        ("damping", 55.0),
        ("width", 80.0),
        ("mix", 0.0),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v))
    .collect();
    space::Room::new(&Params::resolve(&space::room_info(), &saved), rate)
}

/// Reverb Amount as the room's wet share: full Amount is 30% wet.
fn reverb_mix(p: &Params) -> f32 {
    (p.get("rv_amount").clamp(0.0, 100.0) / 100.0 * 0.3) as f32
}

fn widener(p: &Params, rate: u32) -> Option<stereo::Expander> {
    let width = p.get("width");
    ((width - 100.0).abs() > 1e-6).then(|| {
        let saved: BTreeMap<String, f64> = [("width", width), ("center", 0.0), ("bass", 150.0)]
            .into_iter()
            .map(|(k, v)| (k.to_string(), v))
            .collect();
        stereo::Expander::new(&Params::resolve(&stereo::expander_info(), &saved), rate)
    })
}

fn limiter(p: &Params, rate: u32) -> TruePeakLimiter {
    TruePeakLimiter::new(
        2,
        rate,
        LimiterSettings {
            ceiling_db: p.get("ceiling") as f32,
            release_ms: p.get("release") as f32,
            lookahead_ms: LOOKAHEAD_MS,
        },
    )
}

pub struct Mastering {
    rate: u32,
    eq: Eq,
    /// Built on first use; kept once built so its tail rings on when
    /// Amount moves back to 0 under automation.
    reverb: Option<space::Room>,
    reverb_mix: f32,
    exciter: Exciter,
    widener: Option<stereo::Expander>,
    width: f64,
    limiter: TruePeakLimiter,
    /// The settings the limiter was built with: ceiling and release.
    limiter_key: (f64, f64),
    limiting: bool,
    /// Holds the sound back by the limiter's look-ahead while it is off.
    bypass: Delay,
    push: f32,
    output: f32,
}

impl Mastering {
    pub fn new(p: &Params, rate: u32) -> Self {
        let limiter = limiter(p, rate);
        let lat = limiter.latency();
        let mix = reverb_mix(p);
        Self {
            rate,
            eq: Eq::new(p, rate),
            reverb: (mix > 0.0).then(|| {
                let mut r = room(rate);
                r.set_mix(mix);
                r
            }),
            reverb_mix: mix,
            exciter: Exciter::new(p, rate),
            widener: widener(p, rate),
            width: p.get("width"),
            limiter,
            limiter_key: (p.get("ceiling"), p.get("release")),
            limiting: p.on("l_on"),
            bypass: Delay::new(lat),
            push: db_to_gain(p.get("push")),
            output: db_to_gain(p.get("output")),
        }
    }
}

impl Processor for Mastering {
    fn process(&mut self, buf: &mut [f32]) {
        if self.eq.active() {
            self.eq.process(buf);
        }
        if let Some(r) = &mut self.reverb {
            r.process(buf);
        }
        if self.exciter.active() {
            self.exciter.process(buf);
        }
        if let Some(w) = &mut self.widener {
            w.process(buf);
        }
        if self.limiting {
            let push = self.push;
            let mut out = [0f32; 2];
            for f in buf.as_chunks_mut::<2>().0 {
                self.limiter.push(&[f[0] * push, f[1] * push], &mut out);
                f[0] = out[0];
                f[1] = out[1];
            }
        } else {
            self.bypass.run(buf);
        }
        if self.output != 1.0 {
            for s in buf.iter_mut() {
                *s *= self.output;
            }
        }
    }

    fn latency(&self) -> usize {
        self.bypass.frames()
    }

    fn reset(&mut self) {
        self.eq.reset();
        if let Some(r) = &mut self.reverb {
            r.reset();
        }
        self.exciter.reset();
        if let Some(w) = &mut self.widener {
            w.reset();
        }
        self.limiter.reset();
        self.bypass.clear();
    }

    /// Automation: everything moves in place except the limiter's ceiling
    /// and release, which rebuild it (its look-ahead, and so the delay,
    /// never changes).
    fn update(&mut self, p: &Params) -> bool {
        self.eq.set(p);
        let mix = reverb_mix(p);
        if mix > 0.0 && self.reverb.is_none() {
            self.reverb = Some(room(self.rate));
        }
        if let Some(r) = &mut self.reverb {
            r.set_mix(mix);
        }
        self.reverb_mix = mix;
        self.exciter.set(p);
        if (p.get("width") - self.width).abs() > 1e-6 {
            self.width = p.get("width");
            self.widener = widener(p, self.rate);
        }
        let key = (p.get("ceiling"), p.get("release"));
        if key != self.limiter_key {
            self.limiter = limiter(p, self.rate);
            self.limiter_key = key;
        }
        self.limiting = p.on("l_on");
        self.push = db_to_gain(p.get("push"));
        self.output = db_to_gain(p.get("output"));
        true
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::slot;
    use super::super::{build, info as find};
    use super::*;

    const RATE: u32 = 48_000;

    /// A mix-like test signal: two tones a fifth apart (one leaning left)
    /// plus seeded noise, at `amp` peak or so.
    fn music(secs: f32, amp: f32) -> Vec<f32> {
        let mut x: u32 = 0x9e37_79b9;
        (0..(RATE as f32 * secs) as usize)
            .flat_map(|i| {
                let t = i as f32 / RATE as f32;
                x ^= x << 13;
                x ^= x >> 17;
                x ^= x << 5;
                let n = (x as f32 / u32::MAX as f32 - 0.5) * 0.2;
                let a = (std::f32::consts::TAU * 220.0 * t).sin();
                let b = (std::f32::consts::TAU * 330.0 * t).sin();
                [(0.5 * a + 0.3 * b + n) * amp, (0.3 * a + 0.5 * b + n) * amp]
            })
            .collect()
    }

    fn run(params: &[(&str, f64)], input: &[f32]) -> (Vec<f32>, usize) {
        let mut p = build(&slot(ID, params), RATE).unwrap();
        let lat = p.latency();
        let mut b = input.to_vec();
        b.extend(std::iter::repeat_n(0.0, lat * 2));
        for c in b.chunks_mut(2 * 500) {
            p.process(c);
        }
        (b, lat)
    }

    fn peak(b: &[f32]) -> f32 {
        b.iter().fold(0f32, |m, s| m.max(s.abs()))
    }

    const NEUTRAL: &[(&str, f64)] = &[("l_on", 0.0)];

    #[test]
    fn neutral_settings_pass_the_sound_unchanged_but_delayed() {
        let input = music(0.5, 0.9);
        let (out, lat) = run(NEUTRAL, &input);
        assert!(lat > 0);
        assert_eq!(
            &out[lat * 2..],
            &input[..],
            "bit for bit, after the look-ahead"
        );
        assert!(out[..lat * 2].iter().all(|&s| s == 0.0));
    }

    #[test]
    fn default_settings_leave_a_quiet_mix_alone() {
        // Defaults: flat, limiter on at -1 dBTP with no push. Quiet audio is untouched.
        let input = music(0.5, 0.3);
        let (out, lat) = run(&[], &input);
        let worst = out[lat * 2..]
            .iter()
            .zip(&input)
            .fold(0f32, |m, (a, b)| m.max((a - b).abs()));
        assert!(worst < 1e-6, "{worst}");
    }

    #[test]
    fn switching_loudness_off_keeps_the_same_delay() {
        let on = build(&slot(ID, &[]), RATE).unwrap().latency();
        let off = build(&slot(ID, NEUTRAL), RATE).unwrap().latency();
        assert_eq!(on, off);
    }

    #[test]
    fn the_ceiling_holds_with_heavy_push_and_everything_on() {
        let input = music(1.5, 0.9);
        for ceiling in [-1.0, -3.0, -0.1] {
            let (out, _) = run(
                &[
                    ("ls_gain", 6.0),
                    ("hs_gain", 8.0),
                    ("pk_gain", 4.0),
                    ("rv_amount", 60.0),
                    ("ex_amount", 100.0),
                    ("ex_mode", 2.0),
                    ("width", 180.0),
                    ("push", 18.0),
                    ("ceiling", ceiling),
                ],
                &input,
            );
            let limit = 10f32.powf(ceiling as f32 / 20.0);
            assert!(peak(&out) <= limit * 1.0001, "{ceiling}: {}", peak(&out));
            assert!(out.iter().all(|s| s.is_finite()));
        }
    }

    #[test]
    fn push_makes_it_louder() {
        let input = music(1.0, 0.25);
        let rms = |b: &[f32]| (b.iter().map(|s| s * s).sum::<f32>() / b.len() as f32).sqrt();
        let (plain, lat) = run(&[], &input);
        let (pushed, _) = run(&[("push", 9.0)], &input);
        let a = rms(&plain[lat * 2..]);
        let b = rms(&pushed[lat * 2..]);
        assert!(b > a * 1.8, "{a} -> {b}");
    }

    #[test]
    fn every_stage_is_stable_with_extreme_settings() {
        let info = find(ID).unwrap();
        let input = music(0.6, 1.0);
        for mode in 0..MODES.len() {
            let mut all: Vec<(&str, f64)> = info.params.iter().map(|p| (p.key, p.max)).collect();
            all.retain(|(k, _)| *k != "ex_mode");
            all.push(("ex_mode", mode as f64));
            let (out, _) = run(&all, &input);
            assert!(out.iter().all(|s| s.is_finite()), "max, mode {mode}");
            let low: Vec<(&str, f64)> = info.params.iter().map(|p| (p.key, p.min)).collect();
            let (out, _) = run(&low, &input);
            assert!(out.iter().all(|s| s.is_finite()), "min");
        }
        // Silence stays silent (no denormal hiss or offset).
        let quiet = vec![0f32; 48_000];
        let (out, _) = run(
            &[("ex_amount", 100.0), ("rv_amount", 100.0), ("push", 18.0)],
            &quiet,
        );
        assert!(peak(&out) < 1e-6);
    }

    #[test]
    fn exciter_adds_energy_above_its_corner() {
        // A 1 kHz tone has nothing above 2 kHz; the exciter's harmonics add some.
        let tone: Vec<f32> = (0..RATE as usize)
            .flat_map(|i| {
                let v = (std::f32::consts::TAU * 3000.0 * i as f32 / RATE as f32).sin() * 0.5;
                [v, v]
            })
            .collect();
        let (dry, lat) = run(NEUTRAL, &tone);
        let (wet, _) = run(&[("l_on", 0.0), ("ex_amount", 80.0)], &tone);
        let diff: f32 = dry[lat * 2..]
            .iter()
            .zip(&wet[lat * 2..])
            .map(|(a, b)| (a - b).abs())
            .sum();
        assert!(diff > 100.0, "{diff}");
    }

    #[test]
    fn width_spreads_the_sides() {
        let input = music(0.5, 0.5);
        let side = |b: &[f32]| -> f32 { b.chunks(2).map(|f| (f[0] - f[1]).abs()).sum() };
        let (narrow, _) = run(&[("l_on", 0.0), ("width", 0.0)], &input);
        let (wide, _) = run(&[("l_on", 0.0), ("width", 200.0)], &input);
        // The bass stays as it was (kept centered), so narrow is not quite mono.
        assert!(
            side(&narrow) < side(&input) * 0.6,
            "{} {}",
            side(&narrow),
            side(&input)
        );
        assert!(
            side(&wide) > side(&input) * 1.3,
            "{} {}",
            side(&wide),
            side(&input)
        );
    }

    #[test]
    fn automation_moves_settings_in_place() {
        let info = find(ID).unwrap();
        let mut m = Mastering::new(&Params::resolve(&info, &BTreeMap::new()), RATE);
        let lat = m.latency();
        let mut saved = BTreeMap::new();
        for (k, v) in [
            ("hs_gain", 4.0),
            ("rv_amount", 30.0),
            ("ceiling", -2.0),
            ("width", 140.0),
        ] {
            saved.insert(k.to_string(), v);
        }
        assert!(m.update(&Params::resolve(&info, &saved)));
        assert_eq!(m.latency(), lat);
        let mut b = music(0.3, 0.9);
        m.process(&mut b);
        assert!(b.iter().all(|s| s.is_finite()));
        assert!(peak(&b) <= 10f32.powf(-0.1) * 1.0001);
    }

    /// Writes before and after files for listening: set ALTERED_FX_OUT to
    /// a folder and run `cargo test --lib mastering_listening -- --ignored`.
    #[test]
    #[ignore = "writes files for listening"]
    fn mastering_listening_files() {
        let Ok(dir) = std::env::var("ALTERED_FX_OUT") else {
            return;
        };
        let dir = std::path::PathBuf::from(dir);
        std::fs::create_dir_all(&dir).unwrap();
        // Six seconds: a soft chord, a ticking hi-hat-like noise burst every 250 ms, and a thump every 500 ms.
        let n = RATE as usize * 6;
        let mut x: u32 = 0x2545_f491;
        let input: Vec<f32> = (0..n)
            .flat_map(|i| {
                let t = i as f32 / RATE as f32;
                x ^= x << 13;
                x ^= x >> 17;
                x ^= x << 5;
                let noise = x as f32 / u32::MAX as f32 - 0.5;
                let tau = std::f32::consts::TAU;
                let chord = [196.0f32, 247.0, 294.0, 392.0]
                    .iter()
                    .map(|f| (tau * f * t).sin())
                    .sum::<f32>()
                    * 0.06;
                let beat = t % 0.25;
                let hat = noise * (-beat * 60.0).exp() * 0.15;
                let kick_t = t % 0.5;
                let kick = (tau * (50.0 + 80.0 * (-kick_t * 30.0).exp()) * kick_t).sin()
                    * (-kick_t * 12.0).exp()
                    * 0.35;
                let l = chord + hat * 0.8 + kick;
                let r = chord * 0.9 + hat + kick;
                [l, r]
            })
            .collect();
        let write = |name: &str, b: &[f32]| {
            let spec = hound::WavSpec {
                channels: 2,
                sample_rate: RATE,
                bits_per_sample: 32,
                sample_format: hound::SampleFormat::Float,
            };
            let mut w = hound::WavWriter::create(dir.join(name), spec).unwrap();
            for s in b {
                w.write_sample(*s).unwrap();
            }
            w.finalize().unwrap();
        };
        write("mastering-0-before.wav", &input);
        let presets: &[(&str, &[(&str, f64)])] = &[
            (
                "mastering-1-podcast.wav",
                &[
                    ("lc_on", 1.0),
                    ("lc_freq", 70.0),
                    ("ls_gain", 1.0),
                    ("pk_freq", 300.0),
                    ("pk_gain", -1.5),
                    ("hs_freq", 9000.0),
                    ("hs_gain", 2.0),
                    ("ex_amount", 15.0),
                    ("push", 6.0),
                ],
            ),
            (
                "mastering-2-music-loud.wav",
                &[
                    ("lc_on", 1.0),
                    ("ls_freq", 80.0),
                    ("ls_gain", 2.0),
                    ("pk_freq", 2500.0),
                    ("pk_gain", 1.0),
                    ("pk_q", 0.8),
                    ("hs_freq", 10000.0),
                    ("hs_gain", 2.5),
                    ("ex_amount", 25.0),
                    ("ex_mode", 2.0),
                    ("width", 120.0),
                    ("push", 10.0),
                    ("release", 50.0),
                ],
            ),
            (
                "mastering-3-warm-tape.wav",
                &[
                    ("ls_freq", 120.0),
                    ("ls_gain", 2.0),
                    ("hs_freq", 12000.0),
                    ("hs_gain", -1.5),
                    ("ex_amount", 40.0),
                    ("ex_mode", 1.0),
                    ("push", 4.0),
                ],
            ),
            (
                "mastering-4-gentle-polish.wav",
                &[
                    ("ls_gain", 1.0),
                    ("hs_freq", 10000.0),
                    ("hs_gain", 1.5),
                    ("rv_amount", 5.0),
                    ("ex_amount", 10.0),
                    ("width", 110.0),
                    ("push", 2.0),
                    ("release", 120.0),
                ],
            ),
            (
                "mastering-5-exciter-tube-full.wav",
                &[("ex_amount", 100.0), ("ex_mode", 0.0), ("l_on", 0.0)],
            ),
            (
                "mastering-6-reverb-amount-60.wav",
                &[("rv_amount", 60.0), ("l_on", 0.0)],
            ),
        ];
        for (name, p) in presets {
            let (out, lat) = run(p, &input);
            write(name, &out[lat * 2..]);
        }
    }
}
