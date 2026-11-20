// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) The GeekCast Radio Network, LLC.
// This file is part of Altered Audio Editor.

//! Pitch Shift: changes pitch without changing speed. The audio is made
//! longer (or shorter) by the pitch factor with the same pitch-keeping
//! stretch the timeline uses, then played back that much faster (or
//! slower), which brings the length back and moves the pitch. A fixed
//! delay in front keeps the output steady; it is reported as latency so
//! playback stays in time.

use std::collections::VecDeque;

use super::{num, EffectInfo, Params, Processor};
use crate::dsp::stretch::Stretcher;

pub fn pitch_info() -> EffectInfo {
    EffectInfo {
        id: "altaudio.pitch_shift",
        name: "Pitch Shift",
        category: "Time and Pitch",
        description: "Raises or lowers the pitch without changing the speed. Small amounts keep a voice natural.",
        params: vec![
            num("semitones", "Semitones", "st", -12.0, 12.0, 0.0, 1.0),
            num("cents", "Fine tune", "cents", -100.0, 100.0, 0.0, 1.0),
        ],
    }
}

/// Most speed changes waiting to reach the reader at once; more are
/// folded into the last one.
const PENDING: usize = 32;

pub struct PitchShift {
    factor: f64,
    /// The factor the reader uses now: the stretched audio it reads was
    /// made at this factor, so it changes only when the reader reaches
    /// audio made at a new one.
    reading: f64,
    /// Changes on the way: (stretched frame where it starts, factor).
    pending: [(i64, f64); PENDING],
    waiting: usize,
    /// Stretched frames already read and dropped.
    done: i64,
    rate: u32,
    st: Option<Stretcher>,
    stretched: Vec<f32>,
    pos: f64,
    out: VecDeque<f32>,
    tmp: Vec<f32>,
    latency: usize,
}

impl PitchShift {
    pub fn new(p: &Params, rate: u32) -> Self {
        let factor = Self::factor(p);
        // Enough to cover the stretch's frame and search, at any factor.
        let latency = if (factor - 1.0).abs() < 1e-6 {
            0
        } else {
            (f64::from(rate) * 0.075) as usize
        };
        let mut me = Self {
            factor,
            reading: factor,
            pending: [(0, 1.0); PENDING],
            waiting: 0,
            done: 0,
            rate,
            st: None,
            stretched: Vec::new(),
            pos: 0.0,
            out: VecDeque::new(),
            tmp: Vec::new(),
            latency,
        };
        me.reset();
        me
    }

    fn factor(p: &Params) -> f64 {
        let semis = p.get("semitones") + p.get("cents") / 100.0;
        2f64.powf(semis / 12.0).clamp(0.5, 2.0)
    }
}

/// Cubic interpolation through four samples, `t` between the middle two.
fn cubic(a: f32, b: f32, c: f32, d: f32, t: f32) -> f32 {
    let m1 = (c - a) * 0.5;
    let m2 = (d - b) * 0.5;
    let t2 = t * t;
    let t3 = t2 * t;
    (2.0 * t3 - 3.0 * t2 + 1.0) * b
        + (t3 - 2.0 * t2 + t) * m1
        + (-2.0 * t3 + 3.0 * t2) * c
        + (t3 - t2) * m2
}

impl Processor for PitchShift {
    fn process(&mut self, buf: &mut [f32]) {
        let Some(st) = self.st.as_mut() else {
            return;
        };
        st.push(buf);
        self.tmp.clear();
        st.pull(&mut self.tmp);
        self.stretched.extend_from_slice(&self.tmp);
        // Read the stretched audio `factor` times faster.
        let frames = self.stretched.len() / 2;
        while (self.pos as usize) + 3 < frames {
            while self.waiting > 0 && self.pending[0].0 as f64 <= self.done as f64 + self.pos {
                self.reading = self.pending[0].1;
                self.pending.copy_within(1..self.waiting, 0);
                self.waiting -= 1;
            }
            let i = self.pos as usize;
            let t = (self.pos - i as f64) as f32;
            for c in 0..2 {
                let s = |k: usize| self.stretched[(i + k) * 2 + c];
                // One frame of history before `i` is kept for the curve.
                let a = if i > 0 { s(0) } else { s(1) };
                self.out.push_back(cubic(a, s(1), s(2), s(3), t));
            }
            self.pos += self.reading;
        }
        let used = (self.pos as usize).min(frames);
        if used > 0 {
            self.stretched.drain(..used * 2);
            self.pos -= used as f64;
            self.done += used as i64;
        }
        for v in buf.iter_mut() {
            *v = self.out.pop_front().unwrap_or(0.0);
        }
    }
    fn latency(&self) -> usize {
        self.latency
    }
    fn reset(&mut self) {
        // Short frames keep the delay small enough for the latency above.
        self.st = (self.latency > 0).then(|| Stretcher::with_frame(self.factor, self.rate, 0.015));
        self.stretched.clear();
        self.pos = 0.0;
        self.done = 0;
        self.reading = self.factor;
        self.waiting = 0;
        self.out.clear();
        // The stretch's first sample lines up one frame in (see `cubic`).
        let lead = self.latency.saturating_sub(1);
        self.out.extend(std::iter::repeat_n(0.0, lead * 2));
    }
    /// A new pitch while it plays: the stretch takes the new factor from
    /// its next frame, and the reader follows when it gets there. Built
    /// with no shift (and so no delay), it cannot start shifting without
    /// changing its latency, so it is rebuilt then.
    fn update(&mut self, p: &Params) -> bool {
        let factor = Self::factor(p);
        let Some(st) = self.st.as_mut() else {
            return (factor - 1.0).abs() < 1e-6;
        };
        if factor == self.factor {
            return true;
        }
        let from = st.set_ratio(factor);
        self.factor = factor;
        if self.waiting == PENDING {
            self.pending[PENDING - 1] = (from, factor);
        } else {
            self.pending[self.waiting] = (from, factor);
            self.waiting += 1;
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::super::build;
    use super::super::tests::slot;
    use std::f32::consts::TAU;

    fn crossings(semitones: f64) -> f32 {
        let rate = 48_000usize;
        let mut p = build(
            &slot("altaudio.pitch_shift", &[("semitones", semitones)]),
            rate as u32,
        )
        .unwrap();
        let mut b: Vec<f32> = (0..rate * 2)
            .flat_map(|i| {
                let v = (TAU * 440.0 * i as f32 / rate as f32).sin() * 0.5;
                [v, v]
            })
            .collect();
        for c in b.chunks_mut(2 * 512) {
            p.process(c);
        }
        // Rising zero crossings over the middle second.
        let left: Vec<f32> = b.iter().step_by(2).copied().collect();
        let seg = &left[rate / 2..rate * 3 / 2];
        seg.windows(2).filter(|w| w[0] < 0.0 && w[1] >= 0.0).count() as f32
    }

    #[test]
    fn shifts_pitch_and_keeps_speed() {
        let up = crossings(12.0);
        assert!((up - 880.0).abs() < 15.0, "an octave up: {up}");
        let down = crossings(-12.0);
        assert!((down - 220.0).abs() < 8.0, "an octave down: {down}");
        let third = crossings(4.0);
        assert!((third - 554.4).abs() < 12.0, "a major third up: {third}");
    }

    #[test]
    fn no_shift_passes_audio_untouched() {
        let mut p = build(&slot("altaudio.pitch_shift", &[]), 48_000).unwrap();
        assert_eq!(p.latency(), 0);
        let mut b = vec![0.25f32, -0.5, 0.75, 0.1];
        p.process(&mut b);
        assert_eq!(b, vec![0.25, -0.5, 0.75, 0.1]);
    }

    #[test]
    fn output_keeps_flowing_with_small_blocks() {
        let rate = 48_000usize;
        let mut p = build(
            &slot("altaudio.pitch_shift", &[("semitones", 3.0)]),
            rate as u32,
        )
        .unwrap();
        let mut b: Vec<f32> = (0..rate)
            .flat_map(|i| {
                let v = (TAU * 300.0 * i as f32 / rate as f32).sin() * 0.5;
                [v, v]
            })
            .collect();
        for c in b.chunks_mut(2 * 64) {
            p.process(c);
        }
        // After the delay there are no dropouts: no 10 ms stretch of silence.
        let left: Vec<f32> = b.iter().step_by(2).copied().collect();
        let after = &left[p.latency() + 4800..];
        let silent = after.windows(480).any(|w| w.iter().all(|v| v.abs() < 1e-4));
        assert!(!silent, "no gaps once running");
    }

    mod automation {
        use super::super::super::glide::testing::*;

        const FX: &str = "altaudio.pitch_shift";

        /// Frequency of the left channel from rising zero crossings.
        fn freq(b: &[f32]) -> f32 {
            let left: Vec<f32> = b.iter().step_by(2).copied().collect();
            let ups = left
                .windows(2)
                .filter(|w| w[0] < 0.0 && w[1] >= 0.0)
                .count();
            ups as f32 * RATE as f32 / left.len() as f32
        }

        #[test]
        fn a_moving_pitch_follows_without_restarting() {
            let mut p = make(FX, &[("semitones", 2.0)]);
            let mut b = sine(440.0, RATE as usize * 3);
            let third = b.len() / 3;
            let (a, rest) = b.split_at_mut(third);
            for c in a.chunks_mut(256) {
                p.process(c);
            }
            let (sweep, hold) = rest.split_at_mut(third);
            ramp(&mut *p, FX, &[], "semitones", (2.0, 7.0), sweep, 128);
            for c in hold.chunks_mut(256) {
                assert!(p.update(&params(FX, &[("semitones", 7.0)])));
                p.process(c);
            }
            assert!(b.iter().all(|s| s.is_finite()));
            let n = b.len();
            // Steady before and after: about 2 and then 7 semitones up.
            let before = freq(&b[n / 6..n / 3]);
            let after = freq(&b[n * 5 / 6..]);
            let want = |st: f32| 440.0 * 2f32.powf(st / 12.0);
            assert!((before - want(2.0)).abs() < 10.0, "{before}");
            assert!((after - want(7.0)).abs() < 10.0, "{after}");
            // Never a gap: the sound keeps going through the sweep.
            for c in b[n / 3..].chunks(2 * 2400) {
                assert!(rms(c) > 0.1, "a gap in the sound: {}", rms(c));
            }
        }

        #[test]
        fn built_without_a_shift_it_must_be_rebuilt_to_start_one() {
            let mut p = make(FX, &[]);
            assert!(p.update(&params(FX, &[])));
            assert!(!p.update(&params(FX, &[("cents", 50.0)])));
        }
    }
}
