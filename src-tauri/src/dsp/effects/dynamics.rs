// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) The GeekCast Radio Network, LLC.
// This file is part of Altered Audio Editor.

//! Dynamics effects: Dynamics (a compressor), Limiter, Breath Gate,
//! De-Esser and Speech Leveler. All original code built from textbook
//! techniques: envelope followers, a gain curve with a soft knee, and a
//! look-ahead window for the limiter.

use std::collections::VecDeque;

use super::true_peak::{LimiterSettings, TruePeakLimiter};
use super::{db_to_gain, num, toggle, EffectInfo, Params, Processor};
use crate::dsp::biquad::{Coeffs, Shape, StereoBiquad};

/// Smoothing coefficient for a time constant in milliseconds.
fn coef(ms: f64, rate: u32) -> f32 {
    let n = ms.max(0.01) * f64::from(rate) / 1000.0;
    (-1.0 / n).exp() as f32
}

fn to_db(x: f32) -> f32 {
    20.0 * x.max(1e-9).log10()
}

pub fn compressor_info() -> EffectInfo {
    EffectInfo {
        id: "altaudio.dynamics",
        name: "Dynamics",
        category: "Dynamics",
        description: "A compressor: evens out loud and quiet moments above the threshold.",
        params: vec![
            num("threshold", "Threshold", "dB", -60.0, 0.0, -20.0, 0.1),
            num("ratio", "Ratio", ":1", 1.0, 20.0, 3.0, 0.1).log(),
            num("attack", "Attack", "ms", 0.1, 200.0, 10.0, 0.1).log(),
            num("release", "Release", "ms", 5.0, 2000.0, 150.0, 1.0).log(),
            num("knee", "Knee", "dB", 0.0, 24.0, 6.0, 0.1),
            num("makeup", "Makeup gain", "dB", 0.0, 24.0, 0.0, 0.1),
            toggle("linked", "Link channels", true),
        ],
    }
}

pub fn multiband_info() -> EffectInfo {
    EffectInfo {
        id: "altaudio.multiband",
        name: "Multiband Compressor",
        category: "Dynamics",
        description:
            "Splits the sound into low, middle and high bands and compresses each on its own, to tame boom or harshness without squashing the whole voice.",
        params: vec![
            num("low_freq", "Low / mid split", "Hz", 40.0, 1000.0, 200.0, 1.0).log(),
            num("high_freq", "Mid / high split", "Hz", 1000.0, 12_000.0, 3500.0, 10.0).log(),
            num("low_threshold", "Low threshold", "dB", -60.0, 0.0, -24.0, 0.1),
            num("low_ratio", "Low ratio", ":1", 1.0, 20.0, 2.0, 0.1).log(),
            num("low_makeup", "Low gain", "dB", -12.0, 12.0, 0.0, 0.1),
            num("mid_threshold", "Mid threshold", "dB", -60.0, 0.0, -22.0, 0.1),
            num("mid_ratio", "Mid ratio", ":1", 1.0, 20.0, 2.5, 0.1).log(),
            num("mid_makeup", "Mid gain", "dB", -12.0, 12.0, 0.0, 0.1),
            num("high_threshold", "High threshold", "dB", -60.0, 0.0, -26.0, 0.1),
            num("high_ratio", "High ratio", ":1", 1.0, 20.0, 2.0, 0.1).log(),
            num("high_makeup", "High gain", "dB", -12.0, 12.0, 0.0, 0.1),
            num("attack", "Attack", "ms", 0.1, 200.0, 15.0, 0.1).log(),
            num("release", "Release", "ms", 5.0, 2000.0, 200.0, 1.0).log(),
            num("output", "Output gain", "dB", -12.0, 12.0, 0.0, 0.1),
        ],
    }
}

pub fn limiter_info() -> EffectInfo {
    EffectInfo {
        id: "altaudio.limiter",
        name: "Limiter",
        category: "Dynamics",
        description: "Keeps peaks from going over the ceiling, looking ahead so it never clips. Turn on True peak to also catch the peaks between samples that a converter or an encoder can reconstruct.",
        params: vec![
            num("ceiling", "Ceiling", "dB", -24.0, 0.0, -6.0, 0.1),
            num("input_boost", "Input boost", "dB", 0.0, 24.0, 0.0, 0.1),
            num("lookahead", "Look-ahead", "ms", 0.0, 20.0, 5.0, 0.1),
            num("release", "Release", "ms", 10.0, 1000.0, 100.0, 1.0).log(),
            toggle("linked", "Link channels", true),
            toggle("true_peak", "True peak", false),
        ],
    }
}

pub fn gate_info() -> EffectInfo {
    EffectInfo {
        id: "altaudio.breath_gate",
        name: "Breath Gate",
        category: "Dynamics",
        description: "Turns the track down between phrases, so breaths and room noise drop away.",
        params: vec![
            num("threshold", "Threshold", "dB", -80.0, 0.0, -40.0, 0.1),
            num("attack", "Attack", "ms", 0.1, 100.0, 2.0, 0.1).log(),
            num("hold", "Hold", "ms", 0.0, 1000.0, 50.0, 1.0),
            num("release", "Release", "ms", 5.0, 2000.0, 200.0, 1.0).log(),
            num("range", "Closed level", "dB", -100.0, 0.0, -60.0, 0.5),
        ],
    }
}

pub fn deesser_info() -> EffectInfo {
    EffectInfo {
        id: "altaudio.deesser",
        name: "De-Esser",
        category: "Dynamics",
        description:
            "Softens harsh S and SH sounds by turning down only the highs when they flare.",
        params: vec![
            num("freq", "Frequency", "Hz", 2000.0, 12_000.0, 6000.0, 10.0).log(),
            num("threshold", "Threshold", "dB", -60.0, 0.0, -30.0, 0.1),
            num("reduction", "Most reduction", "dB", 0.0, 24.0, 8.0, 0.1),
        ],
    }
}

pub fn leveler_info() -> EffectInfo {
    EffectInfo {
        id: "altaudio.speech_leveler",
        name: "Speech Leveler",
        category: "Dynamics",
        description: "Rides the level of speech toward a target, like a slow hand on the fader.",
        params: vec![
            num("target", "Target level", "dB", -40.0, -6.0, -20.0, 0.1),
            num("max_gain", "Most boost", "dB", 0.0, 30.0, 10.0, 0.1),
            num("max_cut", "Most cut", "dB", 0.0, 30.0, 10.0, 0.1),
            num("speed", "Response", "ms", 100.0, 10_000.0, 1500.0, 1.0).log(),
            num("floor", "Ignore below", "dB", -80.0, -20.0, -50.0, 0.1),
            num("amount", "Leveling amount", "%", 0.0, 100.0, 100.0, 1.0),
            num("range", "Target dynamic range", "dB", 0.0, 60.0, 45.0, 1.0),
            num("boost_low", "Boost low signals", "", 0.0, 1.0, 1.0, 1.0),
            num("comp_on", "Compressor", "", 0.0, 1.0, 0.0, 1.0),
            num(
                "comp_thr",
                "Compressor threshold",
                "dB",
                -9.0,
                -1.0,
                -1.0,
                0.1,
            ),
            num("gate_on", "Noise gate", "", 0.0, 1.0, 0.0, 1.0),
            num("gate_off", "Noise offset", "dB", 0.0, 25.0, 15.0, 0.1),
        ],
    }
}

// --- Dynamics (compressor) ---

pub struct Compressor {
    threshold: f32,
    slope: f32,
    knee: f32,
    /// Makeup gain in dB as heard now, gliding toward `makeup_to`.
    makeup: f32,
    makeup_to: f32,
    attack: f32,
    release: f32,
    linked: bool,
    rate: u32,
    /// Per-frame glide for settings applied straight to the gain (about 10 ms).
    glide: f32,
    /// Gain reduction in dB per channel (only [0] when linked).
    gr: [f32; 2],
}

impl Compressor {
    pub fn new(p: &Params, rate: u32) -> Self {
        Self::with(
            [
                p.get("threshold"),
                p.get("ratio"),
                p.get("knee"),
                p.get("makeup"),
            ],
            [p.get("attack"), p.get("release")],
            p.on("linked"),
            rate,
        )
    }

    /// From `[threshold dB, ratio, knee dB, makeup dB]` and `[attack ms, release ms]`.
    fn with(v: [f64; 4], ms: [f64; 2], linked: bool, rate: u32) -> Self {
        let mut c = Self {
            threshold: 0.0,
            slope: 0.0,
            knee: 0.0,
            makeup: v[3] as f32,
            makeup_to: 0.0,
            attack: 0.0,
            release: 0.0,
            linked,
            rate,
            glide: coef(10.0, rate),
            gr: [0.0; 2],
        };
        c.set(v, ms, linked);
        c
    }

    /// New settings while running, keeping the gain reduction. The
    /// threshold, ratio and knee reach the sound through the attack and
    /// release smoothing; the makeup gain glides over about 10 ms.
    pub(crate) fn set(&mut self, v: [f64; 4], ms: [f64; 2], linked: bool) {
        self.threshold = v[0] as f32;
        self.slope = 1.0 - 1.0 / v[1].max(1.0) as f32;
        self.knee = v[2] as f32;
        self.makeup_to = v[3] as f32;
        self.attack = coef(ms[0], self.rate);
        self.release = coef(ms[1], self.rate);
        if self.linked && !linked {
            // The right channel starts from the shared reduction.
            self.gr[1] = self.gr[0];
        }
        self.linked = linked;
    }

    fn set_from(&mut self, p: &Params) {
        self.set(
            [
                p.get("threshold"),
                p.get("ratio"),
                p.get("knee"),
                p.get("makeup"),
            ],
            [p.get("attack"), p.get("release")],
            p.on("linked"),
        );
    }

    /// Wanted gain reduction in dB for a level in dB (soft knee).
    fn curve(&self, x: f32) -> f32 {
        let over = x - self.threshold;
        if self.knee > 0.0 && over.abs() <= self.knee / 2.0 {
            self.slope * (over + self.knee / 2.0).powi(2) / (2.0 * self.knee)
        } else if over > 0.0 {
            self.slope * over
        } else {
            0.0
        }
    }

    fn follow(&mut self, ch: usize, level: f32) -> f32 {
        let want = self.curve(to_db(level));
        let g = &mut self.gr[ch];
        let c = if want > *g { self.attack } else { self.release };
        *g = want + (*g - want) * c;
        db_to_gain(f64::from(self.makeup - *g))
    }
}

impl Compressor {
    /// Compresses `buf`, detecting the level of `key` (or of `buf` itself).
    fn run(&mut self, buf: &mut [f32], key: Option<&[f32]>) {
        for (i, f) in buf.as_chunks_mut::<2>().0.iter_mut().enumerate() {
            let k = key.map_or([f[0], f[1]], |k| [k[i * 2], k[i * 2 + 1]]);
            // Exact once it arrives: at a steady setting this is a no-op.
            self.makeup = self.makeup_to + (self.makeup - self.makeup_to) * self.glide;
            if self.linked {
                let g = self.follow(0, k[0].abs().max(k[1].abs()));
                f[0] *= g;
                f[1] *= g;
            } else {
                let gl = self.follow(0, k[0].abs());
                let gr = self.follow(1, k[1].abs());
                f[0] *= gl;
                f[1] *= gr;
            }
        }
    }
}

impl Processor for Compressor {
    fn process(&mut self, buf: &mut [f32]) {
        self.run(buf, None);
    }
    fn process_keyed(&mut self, buf: &mut [f32], key: &[f32]) {
        self.run(buf, Some(key));
    }
    fn reset(&mut self) {
        self.gr = [0.0; 2];
        self.makeup = self.makeup_to;
    }
    fn update(&mut self, p: &Params) -> bool {
        self.set_from(p);
        true
    }
}

// --- Multiband Compressor ---

/// A Linkwitz-Riley split (two matched Butterworth sections per side), so
/// the low and high parts add back to a flat sound.
struct Split {
    lo: [StereoBiquad; 2],
    hi: [StereoBiquad; 2],
}

impl Split {
    fn new(rate: u32, freq: f64) -> Self {
        let q = std::f64::consts::FRAC_1_SQRT_2;
        let lp = Coeffs::new(Shape::LowPass, rate, freq, q, 0.0);
        let hp = Coeffs::new(Shape::HighPass, rate, freq, q, 0.0);
        Self {
            lo: [StereoBiquad::new(lp), StereoBiquad::new(lp)],
            hi: [StereoBiquad::new(hp), StereoBiquad::new(hp)],
        }
    }

    /// Moves the split point, keeping the filters' memory.
    fn set(&mut self, rate: u32, freq: f64) {
        let q = std::f64::consts::FRAC_1_SQRT_2;
        let lp = Coeffs::new(Shape::LowPass, rate, freq, q, 0.0);
        let hp = Coeffs::new(Shape::HighPass, rate, freq, q, 0.0);
        for b in &mut self.lo {
            b.l.set(lp);
            b.r.set(lp);
        }
        for b in &mut self.hi {
            b.l.set(hp);
            b.r.set(hp);
        }
    }

    /// One frame in, (low, high) out.
    fn run(&mut self, f: [f32; 2]) -> ([f32; 2], [f32; 2]) {
        let mut l = f;
        let mut h = f;
        for b in &mut self.lo {
            l = [b.l.run(l[0]), b.r.run(l[1])];
        }
        for b in &mut self.hi {
            h = [b.l.run(h[0]), b.r.run(h[1])];
        }
        (l, h)
    }

    fn reset(&mut self) {
        for b in self.lo.iter_mut().chain(self.hi.iter_mut()) {
            b.reset();
        }
    }
}

pub struct Multiband {
    first: Split,
    /// The second split, also applied to the low band so all three bands share one phase.
    second: Split,
    second_low: Split,
    bands: [Compressor; 3],
    /// Output gain as heard now, gliding toward `out_to`.
    out: f32,
    out_to: f32,
    glide: f32,
    rate: u32,
    /// The split points now set, so a move recomputes only what changed.
    freqs: [f64; 2],
}

/// The two split points, the second kept at least half an octave above the first.
fn splits(p: &Params) -> [f64; 2] {
    let f1 = p.get("low_freq");
    [f1, p.get("high_freq").max(f1 * 1.5)]
}

const BANDS: [[&str; 3]; 3] = [
    ["low_threshold", "low_ratio", "low_makeup"],
    ["mid_threshold", "mid_ratio", "mid_makeup"],
    ["high_threshold", "high_ratio", "high_makeup"],
];

impl Multiband {
    pub fn new(p: &Params, rate: u32) -> Self {
        let ms = [p.get("attack"), p.get("release")];
        let band = |k: [&str; 3]| {
            Compressor::with([p.get(k[0]), p.get(k[1]), 6.0, p.get(k[2])], ms, true, rate)
        };
        let [f1, f2] = splits(p);
        let out = db_to_gain(p.get("output"));
        Self {
            first: Split::new(rate, f1),
            second: Split::new(rate, f2),
            second_low: Split::new(rate, f2),
            bands: BANDS.map(band),
            out,
            out_to: out,
            glide: coef(10.0, rate),
            rate,
            freqs: [f1, f2],
        }
    }
}

impl Processor for Multiband {
    fn process(&mut self, buf: &mut [f32]) {
        for f in buf.as_chunks_mut::<2>().0 {
            let (low, rest) = self.first.run(*f);
            let (mid, high) = self.second.run(rest);
            let (la, lb) = self.second_low.run(low);
            let mut parts = [[la[0] + lb[0], la[1] + lb[1]], mid, high];
            let mut sum = [0f32; 2];
            for (part, comp) in parts.iter_mut().zip(self.bands.iter_mut()) {
                comp.run(part, None);
                sum[0] += part[0];
                sum[1] += part[1];
            }
            self.out = self.out_to + (self.out - self.out_to) * self.glide;
            f[0] = sum[0] * self.out;
            f[1] = sum[1] * self.out;
        }
    }
    fn reset(&mut self) {
        self.first.reset();
        self.second.reset();
        self.second_low.reset();
        for b in &mut self.bands {
            b.reset();
        }
        self.out = self.out_to;
    }
    fn update(&mut self, p: &Params) -> bool {
        let [f1, f2] = splits(p);
        if f1 != self.freqs[0] {
            self.first.set(self.rate, f1);
        }
        if f2 != self.freqs[1] {
            self.second.set(self.rate, f2);
            self.second_low.set(self.rate, f2);
        }
        self.freqs = [f1, f2];
        let ms = [p.get("attack"), p.get("release")];
        for (b, k) in self.bands.iter_mut().zip(BANDS) {
            b.set([p.get(k[0]), p.get(k[1]), 6.0, p.get(k[2])], ms, true);
        }
        self.out_to = db_to_gain(p.get("output"));
        true
    }
}

// --- Limiter ---

/// One gain path of the limiter. With look-ahead L, the gain for a peak
/// is reached by the time that peak leaves the delay line:
/// 1. the gain each input sample needs,
/// 2. the smallest of those over the last L+1 samples,
/// 3. instant drop and smooth release,
/// 4. averaged over L+1 samples, so the drop is a ramp, not a step.
struct LimitPath {
    window: usize,
    /// (index, needed gain), increasing gains: front is the window minimum.
    mins: VecDeque<(u64, f32)>,
    held: f32,
    ring: Vec<f32>,
    at: usize,
    sum: f64,
    n: u64,
}

impl LimitPath {
    fn new(lookahead: usize) -> Self {
        let window = lookahead + 1;
        Self {
            window,
            mins: VecDeque::with_capacity(window + 1),
            held: 1.0,
            ring: vec![1.0; window],
            at: 0,
            sum: window as f64,
            n: 0,
        }
    }

    fn step(&mut self, need: f32, release: f32) -> f32 {
        while self.mins.back().is_some_and(|b| b.1 >= need) {
            self.mins.pop_back();
        }
        self.mins.push_back((self.n, need));
        while self
            .mins
            .front()
            .is_some_and(|f| f.0 + self.window as u64 <= self.n)
        {
            self.mins.pop_front();
        }
        self.n += 1;
        let min = self.mins.front().map_or(1.0, |f| f.1);
        self.held = if min < self.held {
            min
        } else {
            min + (self.held - min) * release
        };
        self.sum += f64::from(self.held) - f64::from(self.ring[self.at]);
        self.ring[self.at] = self.held;
        self.at = (self.at + 1) % self.window;
        (self.sum / self.window as f64) as f32
    }

    fn reset(&mut self) {
        self.mins.clear();
        self.held = 1.0;
        self.ring.fill(1.0);
        self.at = 0;
        self.sum = self.window as f64;
        self.n = 0;
    }
}

pub struct Limiter {
    /// Ceiling and input boost as heard now, gliding toward their targets.
    ceiling: f32,
    ceiling_to: f32,
    boost: f32,
    boost_to: f32,
    glide: f32,
    release: f32,
    linked: bool,
    rate: u32,
    lookahead: usize,
    paths: [LimitPath; 2],
    delay: Vec<f32>,
    delay_at: usize,
    /// With True peak on: the oversampled limiter that does the work. It
    /// always links the channels, and keeps its look-ahead between 0.5 and 5 ms.
    true_peak: Option<TruePeakLimiter>,
}

/// Limiter settings in their own units, so the Dynamics suite can drive one too.
pub(crate) struct LimiterValues {
    pub ceiling_db: f64,
    pub boost_db: f64,
    pub lookahead_ms: f64,
    pub release_ms: f64,
    pub linked: bool,
    pub true_peak: bool,
}

impl LimiterValues {
    fn from(p: &Params) -> Self {
        Self {
            ceiling_db: p.get("ceiling"),
            boost_db: p.get("input_boost"),
            lookahead_ms: p.get("lookahead"),
            release_ms: p.get("release"),
            linked: p.on("linked"),
            true_peak: p.on("true_peak"),
        }
    }

    fn true_peak_settings(&self) -> LimiterSettings {
        LimiterSettings {
            ceiling_db: self.ceiling_db as f32,
            release_ms: self.release_ms as f32,
            lookahead_ms: self.lookahead_ms as f32,
        }
    }
}

impl Limiter {
    pub fn new(p: &Params, rate: u32) -> Self {
        Self::with(&LimiterValues::from(p), rate)
    }

    pub(crate) fn with(v: &LimiterValues, rate: u32) -> Self {
        let lookahead = (v.lookahead_ms * f64::from(rate) / 1000.0).round() as usize;
        let ceiling = db_to_gain(v.ceiling_db);
        let boost = db_to_gain(v.boost_db);
        Self {
            ceiling,
            ceiling_to: ceiling,
            boost,
            boost_to: boost,
            glide: coef(5.0, rate),
            release: coef(v.release_ms, rate),
            linked: v.linked,
            rate,
            lookahead,
            paths: [LimitPath::new(lookahead), LimitPath::new(lookahead)],
            delay: vec![0.0; lookahead * 2],
            delay_at: 0,
            true_peak: v
                .true_peak
                .then(|| TruePeakLimiter::new(2, rate, v.true_peak_settings())),
        }
    }

    /// New settings while running, keeping the gain paths and the delay.
    /// False when that cannot be done in place: a look-ahead that changes
    /// the latency, True peak switched, or channel linking switched (the
    /// second path would start without the first one's history).
    pub(crate) fn set(&mut self, v: &LimiterValues) -> bool {
        if v.true_peak != self.true_peak.is_some() || v.linked != self.linked {
            return false;
        }
        match self.true_peak.as_mut() {
            Some(tp) => {
                if !tp.set(self.rate, v.true_peak_settings()) {
                    return false;
                }
            }
            None => {
                let lookahead = (v.lookahead_ms * f64::from(self.rate) / 1000.0).round() as usize;
                if lookahead != self.lookahead {
                    return false;
                }
            }
        }
        self.ceiling_to = db_to_gain(v.ceiling_db);
        self.boost_to = db_to_gain(v.boost_db);
        self.release = coef(v.release_ms, self.rate);
        true
    }

    fn need(&self, x: f32) -> f32 {
        if x > self.ceiling {
            self.ceiling / x
        } else {
            1.0
        }
    }

    /// One step of the ceiling and boost glides (exact at a steady setting).
    fn glide(&mut self) {
        self.ceiling = self.ceiling_to + (self.ceiling - self.ceiling_to) * self.glide;
        self.boost = self.boost_to + (self.boost - self.boost_to) * self.glide;
    }
}

impl Processor for Limiter {
    fn process(&mut self, buf: &mut [f32]) {
        if self.true_peak.is_some() {
            for f in buf.as_chunks_mut::<2>().0 {
                self.glide();
                let Some(tp) = self.true_peak.as_mut() else {
                    return;
                };
                let mut out = [0.0f32; 2];
                tp.push(&[f[0] * self.boost, f[1] * self.boost], &mut out);
                f[0] = out[0];
                f[1] = out[1];
            }
            return;
        }
        let n = self.delay.len();
        for f in buf.as_chunks_mut::<2>().0 {
            self.glide();
            let l = f[0] * self.boost;
            let r = f[1] * self.boost;
            let (gl, gr) = if self.linked {
                let g = self.paths[0].step(self.need(l.abs().max(r.abs())), self.release);
                (g, g)
            } else {
                let nl = self.need(l.abs());
                let nr = self.need(r.abs());
                (
                    self.paths[0].step(nl, self.release),
                    self.paths[1].step(nr, self.release),
                )
            };
            let (dl, dr) = if n == 0 {
                (l, r)
            } else {
                let a = self.delay_at;
                let out = (self.delay[a], self.delay[a + 1]);
                self.delay[a] = l;
                self.delay[a + 1] = r;
                self.delay_at = (a + 2) % n;
                out
            };
            // The clamp only catches float rounding; the gain already fits.
            let c = self.ceiling;
            f[0] = (dl * gl).clamp(-c, c);
            f[1] = (dr * gr).clamp(-c, c);
        }
    }
    fn latency(&self) -> usize {
        self.true_peak
            .as_ref()
            .map_or(self.lookahead, TruePeakLimiter::latency)
    }
    fn reset(&mut self) {
        for p in &mut self.paths {
            p.reset();
        }
        self.delay.fill(0.0);
        self.delay_at = 0;
        self.ceiling = self.ceiling_to;
        self.boost = self.boost_to;
        if let Some(tp) = self.true_peak.as_mut() {
            tp.reset();
        }
    }
    fn update(&mut self, p: &Params) -> bool {
        self.set(&LimiterValues::from(p))
    }
}

// --- Breath Gate ---

pub struct Gate {
    open_at: f32,
    floor: f32,
    hold: u32,
    up: f32,
    down: f32,
    per_ms: f64,
    /// How far the gain may climb per frame toward a raised closed level
    /// (over about 10 ms), so a moving Closed level never steps.
    lift: f32,
    env_decay: f32,
    env: f32,
    held: u32,
    gain: f32,
}

impl Gate {
    pub fn new(p: &Params, rate: u32) -> Self {
        let mut g = Self {
            open_at: 0.0,
            floor: 0.0,
            hold: 0,
            up: 0.0,
            down: 0.0,
            per_ms: f64::from(rate) / 1000.0,
            lift: (1.0 / (10.0 * f64::from(rate) / 1000.0)) as f32,
            // The detector falls over 5 ms so it does not chatter on each cycle.
            env_decay: coef(5.0, rate),
            env: 0.0,
            held: 0,
            gain: 0.0,
        };
        g.set_from(p);
        g.gain = g.floor;
        g
    }

    /// New settings while running, keeping the detector, hold count and gain.
    pub(crate) fn set(&mut self, threshold: f64, attack: f64, hold: f64, release: f64, range: f64) {
        let per_ms = self.per_ms;
        self.floor = if range <= -99.0 {
            0.0
        } else {
            db_to_gain(range)
        };
        self.open_at = db_to_gain(threshold);
        self.hold = (hold * per_ms) as u32;
        self.held = self.held.min(self.hold);
        // Linear ramps across the whole attack or release time.
        self.up = (1.0 / (attack * per_ms).max(1.0)) as f32;
        self.down = (1.0 / (release * per_ms).max(1.0)) as f32;
    }

    fn set_from(&mut self, p: &Params) {
        self.set(
            p.get("threshold"),
            p.get("attack"),
            p.get("hold"),
            p.get("release"),
            p.get("range"),
        );
    }
}

impl Processor for Gate {
    fn process(&mut self, buf: &mut [f32]) {
        self.run(buf, None);
    }
    fn process_keyed(&mut self, buf: &mut [f32], key: &[f32]) {
        self.run(buf, Some(key));
    }
    fn reset(&mut self) {
        self.env = 0.0;
        self.held = 0;
        self.gain = self.floor;
    }
    fn update(&mut self, p: &Params) -> bool {
        self.set_from(p);
        true
    }
}

impl Gate {
    /// Gates `buf`, opening on the level of `key` (or of `buf` itself).
    fn run(&mut self, buf: &mut [f32], key: Option<&[f32]>) {
        let span = 1.0 - self.floor;
        for (i, f) in buf.as_chunks_mut::<2>().0.iter_mut().enumerate() {
            let x = match key {
                Some(k) => k[i * 2].abs().max(k[i * 2 + 1].abs()),
                None => f[0].abs().max(f[1].abs()),
            };
            self.env = x.max(self.env * self.env_decay);
            let open = if self.env >= self.open_at {
                self.held = self.hold;
                true
            } else if self.held > 0 {
                self.held -= 1;
                true
            } else {
                false
            };
            if open {
                self.gain = (self.gain + self.up * span).min(1.0);
            } else if self.gain < self.floor {
                // The closed level was raised while closed: rise to it.
                self.gain = (self.gain + self.lift).min(self.floor);
            } else {
                self.gain = (self.gain - self.down * span).max(self.floor);
            }
            f[0] *= self.gain;
            f[1] *= self.gain;
        }
    }
}

// --- De-Esser ---

/// A detector on the S band drives a high shelf that dips the highs by
/// as much as they flare. The shelf moves every 16 frames.
pub struct DeEsser {
    side: StereoBiquad,
    shelf: StereoBiquad,
    rate: u32,
    freq: f64,
    threshold: f32,
    most: f32,
    attack: f32,
    release: f32,
    env: f32,
    count: u32,
    cut: f32,
}

impl DeEsser {
    pub fn new(p: &Params, rate: u32) -> Self {
        Self {
            side: StereoBiquad::new(Coeffs::new(
                Shape::BandPass,
                rate,
                p.get("freq") * 1.2,
                1.2,
                0.0,
            )),
            shelf: StereoBiquad::new(Coeffs::FLAT),
            rate,
            freq: p.get("freq"),
            threshold: p.get("threshold") as f32,
            most: p.get("reduction") as f32,
            attack: coef(1.0, rate),
            release: coef(60.0, rate),
            env: 0.0,
            count: 0,
            cut: 0.0,
        }
    }
}

impl Processor for DeEsser {
    fn process(&mut self, buf: &mut [f32]) {
        for f in buf.as_chunks_mut::<2>().0 {
            let sl = self.side.l.run(f[0]);
            let sr = self.side.r.run(f[1]);
            let x = sl.abs().max(sr.abs());
            let c = if x > self.env {
                self.attack
            } else {
                self.release
            };
            self.env = x + (self.env - x) * c;
            if self.count == 0 {
                let cut = (to_db(self.env) - self.threshold).clamp(0.0, self.most);
                if (cut - self.cut).abs() > 0.05 {
                    self.cut = cut;
                    let co = if cut < 0.05 {
                        Coeffs::FLAT
                    } else {
                        Coeffs::new(Shape::HighShelf, self.rate, self.freq, 1.0, -f64::from(cut))
                    };
                    self.shelf.l.set(co);
                    self.shelf.r.set(co);
                }
            }
            self.count = (self.count + 1) % 16;
            f[0] = self.shelf.l.run(f[0]);
            f[1] = self.shelf.r.run(f[1]);
        }
    }
    fn reset(&mut self) {
        self.side.reset();
        self.shelf = StereoBiquad::new(Coeffs::FLAT);
        self.env = 0.0;
        self.count = 0;
        self.cut = 0.0;
    }
    fn update(&mut self, p: &Params) -> bool {
        let freq = p.get("freq");
        if freq != self.freq {
            let side = Coeffs::new(Shape::BandPass, self.rate, freq * 1.2, 1.2, 0.0);
            self.side.l.set(side);
            self.side.r.set(side);
            self.freq = freq;
            // Re-aim the shelf at the new frequency on its next step.
            self.cut = -1.0;
        }
        self.threshold = p.get("threshold") as f32;
        self.most = p.get("reduction") as f32;
        true
    }
}

// --- Speech Leveler ---

pub struct SpeechLeveler {
    target: f32,
    max_gain: f32,
    max_cut: f32,
    floor: f32,
    /// How much of the correction is applied, 0 to 1.
    amount: f32,
    /// Half-width in dB around the target where the level is left alone.
    dead: f32,
    comp: Option<f32>,
    /// Noise gate threshold in dB: the floor plus the noise offset.
    gate: Option<f32>,
    gate_db: f32,
    gate_coef: f32,
    /// Mean square follower (fast, about a syllable).
    ms_coef: f32,
    ms: f32,
    /// Gain follower (slow, the Response setting).
    gain_coef: f32,
    gain_db: f32,
    rate: u32,
}

impl SpeechLeveler {
    pub fn new(p: &Params, rate: u32) -> Self {
        let mut l = Self {
            target: 0.0,
            max_gain: 0.0,
            max_cut: 0.0,
            floor: 0.0,
            amount: 0.0,
            dead: 0.0,
            comp: None,
            gate: None,
            gate_db: 0.0,
            gate_coef: coef(60.0, rate),
            ms_coef: coef(300.0, rate),
            ms: 0.0,
            gain_coef: 0.0,
            gain_db: 0.0,
            rate,
        };
        l.set(p);
        l
    }

    /// Settings only; the level and gain followers carry on.
    fn set(&mut self, p: &Params) {
        self.target = p.get("target") as f32;
        self.max_gain = if p.get("boost_low") >= 0.5 {
            p.get("max_gain") as f32
        } else {
            0.0
        };
        self.max_cut = p.get("max_cut") as f32;
        self.floor = p.get("floor") as f32;
        self.amount = (p.get("amount") / 100.0).clamp(0.0, 1.0) as f32;
        self.dead = (p.get("range") / 20.0).max(0.0) as f32;
        self.comp = (p.get("comp_on") >= 0.5).then(|| db_to_gain(p.get("comp_thr")));
        self.gate = (p.get("gate_on") >= 0.5).then(|| (p.get("floor") + p.get("gate_off")) as f32);
        self.gain_coef = coef(p.get("speed"), self.rate);
    }
}

impl Processor for SpeechLeveler {
    fn process(&mut self, buf: &mut [f32]) {
        for f in buf.as_chunks_mut::<2>().0 {
            let x = (f[0] * f[0] + f[1] * f[1]) * 0.5;
            self.ms = x + (self.ms - x) * self.ms_coef;
            let level = 10.0 * self.ms.max(1e-18).log10();
            // Pauses and room noise leave the gain where it was, so the
            // noise floor is never pumped up between sentences.
            if level > self.floor {
                let dev = self.target - level;
                let dev = dev.signum() * (dev.abs() - self.dead).max(0.0);
                let want = (dev * self.amount).clamp(-self.max_cut, self.max_gain);
                self.gain_db = want + (self.gain_db - want) * self.gain_coef;
            }
            // The gate eases the sound down 20 dB while it sits below the threshold.
            // Switched off while closed, it eases back open the same way.
            let to = match self.gate {
                Some(thr) if level < thr => -20.0,
                _ => 0.0,
            };
            self.gate_db = to + (self.gate_db - to) * self.gate_coef;
            let g = db_to_gain(f64::from(self.gain_db + self.gate_db));
            f[0] *= g;
            f[1] *= g;
            // The compressor only tames peaks left over after leveling: 4:1 above the threshold.
            if let Some(t) = self.comp {
                for v in f.iter_mut() {
                    let a = v.abs();
                    if a > t {
                        *v = v.signum() * (t + (a - t) / 4.0);
                    }
                }
            }
        }
    }
    fn reset(&mut self) {
        self.ms = 0.0;
        self.gain_db = 0.0;
        self.gate_db = 0.0;
    }
    fn update(&mut self, p: &Params) -> bool {
        self.set(p);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::super::build;
    use super::super::tests::slot;
    use std::f32::consts::PI;

    fn sine(freq: f32, amp: f32, secs: f32) -> Vec<f32> {
        let n = (48_000.0 * secs) as usize;
        (0..n)
            .flat_map(|i| {
                let v = (2.0 * PI * freq * i as f32 / 48_000.0).sin() * amp;
                [v, v]
            })
            .collect()
    }

    fn peak(b: &[f32]) -> f32 {
        b.iter().fold(0f32, |m, s| m.max(s.abs()))
    }

    fn run(effect: &str, params: &[(&str, f64)], mut b: Vec<f32>) -> Vec<f32> {
        let mut p = build(&slot(effect, params), 48_000).unwrap();
        // Odd block sizes, as the renderer may send.
        for chunk in b.chunks_mut(2 * 777) {
            p.process(chunk);
        }
        b
    }

    use super::super::{info, Params};

    /// Runs `effect` on a steady level of `amp` in 256-frame blocks (as
    /// automation does), moving `key` in a straight line from `from` to
    /// `to` over the middle half, and returns the left channel's gain per
    /// frame. A rebuild on each step would reset the gain reduction, which
    /// shows as a jump.
    fn ramped(
        effect: &str,
        fixed: &[(&str, f64)],
        key: &str,
        from: f64,
        to: f64,
        amp: f32,
    ) -> Vec<f32> {
        let mut s = slot(effect, fixed);
        s.params.insert(key.into(), from);
        let i = info(effect).unwrap();
        let mut fx = build(&s, 48_000).unwrap();
        let blocks = 4 * 48_000 / 256;
        let mut gains = Vec::new();
        for b in 0..blocks {
            let t = ((b as f64 - blocks as f64 / 4.0) / (blocks as f64 / 2.0)).clamp(0.0, 1.0);
            s.params.insert(key.into(), from + (to - from) * t);
            assert!(fx.update(&Params::resolve(&i, &s.params)), "{effect} {key}");
            let mut buf = vec![amp; 2 * 256];
            fx.process(&mut buf);
            gains.extend(buf.iter().step_by(2).map(|v| v / amp));
        }
        assert!(
            gains.iter().all(|g| g.is_finite()),
            "{effect} {key}: not finite"
        );
        gains
    }

    /// Frames left out of the checks while the effect first settles on
    /// the level (half a second; the ramp starts at one second).
    const WARM: usize = 24_000;

    /// The largest change in gain from one frame to the next.
    fn biggest_step(g: &[f32]) -> f32 {
        g.windows(2).fold(0f32, |m, w| m.max((w[1] - w[0]).abs()))
    }

    #[test]
    fn a_ramped_compressor_threshold_and_ratio_keep_the_gain_reduction() {
        let fixed = [("knee", 0.0), ("attack", 5.0), ("release", 100.0)];
        let g = ramped("altaudio.dynamics", &fixed, "threshold", -30.0, -10.0, 0.5);
        assert!(
            biggest_step(&g[WARM..]) < 0.002,
            "{}",
            biggest_step(&g[WARM..])
        );
        // It really compresses all along: -6 dB in, never back to unity.
        assert!(g[WARM..].iter().all(|v| *v < 0.9));
        let g = ramped("altaudio.dynamics", &fixed, "ratio", 2.0, 8.0, 0.5);
        assert!(
            biggest_step(&g[WARM..]) < 0.002,
            "{}",
            biggest_step(&g[WARM..])
        );
        let g = ramped("altaudio.dynamics", &fixed, "makeup", 0.0, 12.0, 0.1);
        assert!(
            biggest_step(&g[WARM..]) < 0.01,
            "makeup glides: {}",
            biggest_step(&g[WARM..])
        );
    }

    #[test]
    fn a_ramped_multiband_keeps_its_bands() {
        let g = ramped(
            "altaudio.multiband",
            &[],
            "mid_threshold",
            -40.0,
            -10.0,
            0.3,
        );
        assert!(
            biggest_step(&g[WARM..]) < 0.01,
            "{}",
            biggest_step(&g[WARM..])
        );
        let g = ramped("altaudio.multiband", &[], "low_freq", 100.0, 400.0, 0.3);
        assert!(
            biggest_step(&g[WARM..]) < 0.02,
            "{}",
            biggest_step(&g[WARM..])
        );
    }

    #[test]
    fn a_ramped_limiter_ceiling_keeps_limiting() {
        let g = ramped(
            "altaudio.limiter",
            &[("lookahead", 2.0)],
            "ceiling",
            -12.0,
            -3.0,
            0.9,
        );
        assert!(
            biggest_step(&g[WARM..]) < 0.005,
            "{}",
            biggest_step(&g[WARM..])
        );
        let g = ramped(
            "altaudio.limiter",
            &[("true_peak", 1.0)],
            "ceiling",
            -12.0,
            -3.0,
            0.9,
        );
        assert!(g[48_000..].iter().all(|v| *v < 0.79));
        // Look-ahead sets the latency, so it is refused rather than rebuilt in place.
        let i = info("altaudio.limiter").unwrap();
        let mut s = slot("altaudio.limiter", &[]);
        let mut fx = build(&s, 48_000).unwrap();
        s.params.insert("lookahead".into(), 10.0);
        assert!(!fx.update(&Params::resolve(&i, &s.params)));
    }

    #[test]
    fn a_ramped_gate_threshold_and_closed_level_do_not_reopen_or_step() {
        // A quiet level under the threshold: the gate stays closed while
        // the threshold moves, instead of snapping open on each rebuild.
        let fixed = [("range", -40.0)];
        let g = ramped(
            "altaudio.breath_gate",
            &fixed,
            "threshold",
            -30.0,
            -20.0,
            0.003,
        );
        assert!(g[24_000..].iter().all(|v| *v < 0.011), "stays closed");
        let g = ramped(
            "altaudio.breath_gate",
            &[("threshold", -20.0)],
            "range",
            -60.0,
            -20.0,
            0.003,
        );
        assert!(biggest_step(&g) < 0.002, "{}", biggest_step(&g));
        assert!(
            (g[g.len() - 1] - 0.1).abs() < 1e-3,
            "reaches the new closed level"
        );
    }

    #[test]
    fn a_ramped_deesser_and_leveler_stay_continuous() {
        let g = ramped("altaudio.deesser", &[], "threshold", -40.0, -20.0, 0.3);
        assert!(
            biggest_step(&g[WARM..]) < 0.05,
            "{}",
            biggest_step(&g[WARM..])
        );
        // -40 dB in, toward -30 then -15 dB: always a boost.
        let g = ramped(
            "altaudio.speech_leveler",
            &[("speed", 300.0)],
            "target",
            -30.0,
            -15.0,
            0.01,
        );
        assert!(
            biggest_step(&g[WARM..]) < 0.002,
            "{}",
            biggest_step(&g[WARM..])
        );
        // The leveler's boost is kept, not started from 0 dB again.
        assert!(g[WARM..].iter().all(|v| *v > 1.5));
    }

    #[test]
    fn switching_the_leveler_gate_off_while_closed_eases_it_open() {
        let i = info("altaudio.speech_leveler").unwrap();
        let mut s = slot(
            "altaudio.speech_leveler",
            &[
                ("gate_on", 1.0),
                ("floor", -60.0),
                ("gate_off", 20.0),
                ("amount", 0.0),
            ],
        );
        let mut fx = build(&s, 48_000).unwrap();
        let mut buf = vec![0.005f32; 2 * 48_000];
        fx.process(&mut buf);
        assert!(buf[buf.len() - 1] < 0.0015, "closed");
        s.params.insert("gate_on".into(), 0.0);
        assert!(fx.update(&Params::resolve(&i, &s.params)));
        let mut buf = vec![0.005f32; 2 * 48_000];
        fx.process(&mut buf);
        assert!((buf[buf.len() - 1] - 0.005).abs() < 1e-4, "open again");
    }

    /// At steady settings the effects sound exactly as before: taking the
    /// settings they were built with changes no bits.
    #[test]
    fn update_with_the_same_settings_changes_nothing() {
        for id in [
            "altaudio.dynamics",
            "altaudio.multiband",
            "altaudio.limiter",
            "altaudio.breath_gate",
            "altaudio.deesser",
            "altaudio.speech_leveler",
        ] {
            let s = slot(id, &[]);
            let p = Params::resolve(&info(id).unwrap(), &s.params);
            let mut a = build(&s, 48_000).unwrap();
            let mut b = build(&s, 48_000).unwrap();
            let input = sine(300.0, 0.6, 0.3);
            let (mut x, mut y) = (input.clone(), input);
            for (ca, cb) in x.chunks_mut(512).zip(y.chunks_mut(512)) {
                assert!(b.update(&p));
                a.process(ca);
                b.process(cb);
            }
            assert!(
                x.iter().zip(&y).all(|(p, q)| p.to_bits() == q.to_bits()),
                "{id}"
            );
        }
    }

    #[test]
    fn limiter_never_passes_the_ceiling() {
        // A sudden full-scale burst after silence: the worst case.
        let mut b = vec![0f32; 2 * 4800];
        b.extend(sine(1000.0, 1.0, 0.2));
        b.extend(vec![0f32; 2 * 4800]);
        for la in [0.0, 1.0, 5.0, 20.0] {
            let out = run(
                "altaudio.limiter",
                &[("ceiling", -6.0), ("lookahead", la)],
                b.clone(),
            );
            assert!(
                peak(&out) <= 0.5012 + 1e-4,
                "look-ahead {la}: {}",
                peak(&out)
            );
        }
        // Quiet audio passes untouched apart from the delay.
        let quiet = sine(500.0, 0.1, 0.1);
        let out = run("altaudio.limiter", &[("lookahead", 0.0)], quiet.clone());
        assert_eq!(out, quiet);
    }

    #[test]
    fn true_peak_limiter_catches_peaks_between_samples() {
        // A tone at a quarter of the sample rate, 45 degrees off: the stored
        // samples reach about 1.06 but the waveform they describe reaches 1.5.
        let b: Vec<f32> = (0..96_000)
            .flat_map(|i| {
                let v = 1.5 * (PI / 2.0 * i as f32 + PI / 4.0).sin();
                [v, v]
            })
            .collect();
        let slot = slot("altaudio.limiter", &[("ceiling", -1.0), ("true_peak", 1.0)]);
        let mut p = build(&slot, 48_000).unwrap();
        assert!(p.latency() > 0);
        let mut out = b;
        for chunk in out.chunks_mut(2 * 777) {
            p.process(chunk);
        }
        // The stored samples reach only about 1.06, so a limiter that looks at
        // samples alone would leave them near the ceiling (0.89). Allowing for
        // the 1.5 peak between them takes them down to about 0.6.
        let left: Vec<f32> = out.iter().step_by(2).copied().collect();
        let tail = peak(&left[4000..]);
        assert!(
            tail < 0.75,
            "samples left at {tail}: the peak between them was not counted"
        );
        assert!(tail > 0.5, "limited far more than needed: {tail}");
    }

    #[test]
    fn compressor_follows_its_ratio() {
        let out = run(
            "altaudio.dynamics",
            &[("threshold", -20.0), ("ratio", 4.0), ("knee", 0.0)],
            sine(1000.0, 1.0, 1.0),
        );
        // 0 dB in, 20 dB over: 4:1 leaves 5 dB over, so about -15 dB.
        let tail = &out[out.len() / 2..];
        let db = 20.0 * peak(tail).log10();
        assert!((db + 15.0).abs() < 1.0, "{db}");
        let quiet = run(
            "altaudio.dynamics",
            &[("threshold", -20.0)],
            sine(1000.0, 0.01, 0.5),
        );
        assert!(
            (peak(&quiet[quiet.len() / 2..]) - 0.01).abs() < 1e-3,
            "below threshold is untouched"
        );
    }

    #[test]
    fn gate_closes_on_quiet_and_opens_on_speech() {
        let mut b = sine(300.0, 0.002, 1.0); // -54 dB room tone
        b.extend(sine(300.0, 0.5, 0.5));
        let out = run(
            "altaudio.breath_gate",
            &[("threshold", -30.0), ("range", -40.0)],
            b,
        );
        let half = 48_000;
        assert!(
            peak(&out[half..2 * half]) < 0.002 * 0.02,
            "closed part is down 40 dB"
        );
        assert!(
            peak(&out[out.len() - half / 2..]) > 0.49,
            "open part passes"
        );
    }

    #[test]
    fn deesser_cuts_highs_not_lows() {
        let hi = run(
            "altaudio.deesser",
            &[("threshold", -40.0), ("reduction", 12.0)],
            sine(7000.0, 0.5, 0.5),
        );
        assert!(peak(&hi[hi.len() / 2..]) < 0.25, "harsh highs come down");
        let lo = run(
            "altaudio.deesser",
            &[("threshold", -40.0)],
            sine(200.0, 0.5, 0.5),
        );
        assert!(peak(&lo[lo.len() / 2..]) > 0.47, "voice body stays");
    }

    #[test]
    fn leveler_brings_quiet_speech_up_and_ignores_silence() {
        // -36 dB RMS speech toward -20 dB, limited to +10 dB of boost.
        let out = run(
            "altaudio.speech_leveler",
            &[("speed", 200.0)],
            sine(200.0, 0.0224, 3.0),
        );
        let gain = peak(&out[out.len() - 9600..]) / 0.0224;
        assert!((20.0 * gain.log10() - 10.0).abs() < 0.5);
        let silent = run("altaudio.speech_leveler", &[], sine(200.0, 0.0001, 1.0));
        assert!(
            (peak(&silent) - 0.0001).abs() < 1e-6,
            "below the floor nothing moves"
        );
    }

    #[test]
    fn leveler_amount_boost_switch_compressor_and_gate() {
        let quiet = sine(200.0, 0.0224, 3.0);
        let tail = |o: &[f32]| peak(&o[o.len() - 9600..]) / 0.0224;
        // Amount 0: nothing is corrected.
        let none = run(
            "altaudio.speech_leveler",
            &[("speed", 200.0), ("amount", 0.0)],
            quiet.clone(),
        );
        assert!((tail(&none) - 1.0).abs() < 0.02);
        // Half the correction: about half the boost in dB.
        let half = run(
            "altaudio.speech_leveler",
            &[("speed", 200.0), ("amount", 50.0), ("range", 0.0)],
            quiet.clone(),
        );
        assert!(
            (20.0 * tail(&half).log10() - 8.0).abs() < 0.8,
            "{}",
            20.0 * tail(&half).log10()
        );
        // Boost low signals off: quiet speech is not raised.
        let off = run(
            "altaudio.speech_leveler",
            &[("speed", 200.0), ("boost_low", 0.0)],
            quiet,
        );
        assert!((tail(&off) - 1.0).abs() < 0.02);
        // Compressor: a hot peak is pulled down above the threshold.
        let hot = run(
            "altaudio.speech_leveler",
            &[("comp_on", 1.0), ("comp_thr", -6.0), ("target", -6.0)],
            sine(200.0, 0.9, 1.0),
        );
        assert!(peak(&hot) < 0.9);
        // Gate: speech below the floor plus offset is eased down.
        let gated = run(
            "altaudio.speech_leveler",
            &[
                ("gate_on", 1.0),
                ("floor", -60.0),
                ("gate_off", 20.0),
                ("amount", 0.0),
            ],
            sine(200.0, 0.005, 1.0),
        );
        assert!(peak(&gated[gated.len() / 2..]) < 0.0015);
    }

    #[test]
    fn multiband_at_ratio_one_passes_sound_through_at_the_same_level() {
        let flat = [("low_ratio", 1.0), ("mid_ratio", 1.0), ("high_ratio", 1.0)];
        for hz in [80.0, 200.0, 1000.0, 3500.0, 9000.0] {
            let out = run("altaudio.multiband", &flat, sine(hz, 0.3, 0.6));
            let p = peak(&out[out.len() / 2..]);
            assert!((p - 0.3).abs() < 0.015, "{hz} Hz came out at {p}");
        }
    }

    #[test]
    fn multiband_squeezes_only_the_band_that_is_loud() {
        let set = [
            ("low_threshold", -30.0),
            ("low_ratio", 8.0),
            ("mid_ratio", 1.0),
            ("high_ratio", 1.0),
        ];
        let boom = run("altaudio.multiband", &set, sine(100.0, 0.5, 0.6));
        assert!(peak(&boom[boom.len() / 2..]) < 0.3, "loud lows come down");
        let voice = run("altaudio.multiband", &set, sine(1000.0, 0.5, 0.6));
        assert!(
            peak(&voice[voice.len() / 2..]) > 0.47,
            "the middle is left alone"
        );
    }
}
