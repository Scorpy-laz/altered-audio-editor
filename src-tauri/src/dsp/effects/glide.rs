// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) The GeekCast Radio Network, LLC.
// This file is part of Altered Audio Editor.

//! Smooth control changes for effects that take new settings while they
//! play (automation). A new value is reached in a straight line over the
//! next block, so a moving control has no steps (zipper noise) and a
//! still one gives exactly the value it was built with.

/// A control value that moves to a new target over one block.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Glide {
    cur: f64,
    target: f64,
    step: f64,
    left: usize,
}

impl Glide {
    pub fn new(v: f64) -> Self {
        Self {
            cur: v,
            target: v,
            step: 0.0,
            left: 0,
        }
    }

    /// The value to reach by the end of the next block.
    pub fn set(&mut self, v: f64) {
        self.target = v;
    }

    /// Goes straight to `v`, with no ramp.
    pub fn jump(&mut self, v: f64) {
        *self = Self::new(v);
    }

    /// Starts a block of `frames` frames: plans the ramp to the target.
    pub fn begin(&mut self, frames: usize) {
        if self.cur == self.target || frames == 0 {
            self.cur = self.target;
            self.left = 0;
        } else {
            self.step = (self.target - self.cur) / frames as f64;
            self.left = frames;
        }
    }

    /// The value for the next frame.
    #[inline]
    pub fn next(&mut self) -> f64 {
        if self.left > 0 {
            self.left -= 1;
            self.cur = if self.left == 0 {
                self.target
            } else {
                self.cur + self.step
            };
        }
        self.cur
    }

    /// The value now, without moving.
    pub fn value(&self) -> f64 {
        self.cur
    }

    /// True while a ramp is under way.
    pub fn moving(&self) -> bool {
        self.left > 0
    }
}

/// A slow one-pole glide for delay times, like a tape machine speeding up
/// or slowing down: the echo bends in pitch instead of jumping.
#[derive(Debug, Clone, Copy)]
pub(crate) struct TimeGlide {
    cur: f64,
    target: f64,
    coef: f64,
}

impl TimeGlide {
    /// `ms` is the time constant.
    pub fn new(v: f64, rate: u32, ms: f64) -> Self {
        Self {
            cur: v,
            target: v,
            coef: 1.0 - (-1000.0 / (ms * f64::from(rate.max(1)))).exp(),
        }
    }

    pub fn set(&mut self, v: f64) {
        self.target = v;
    }

    #[inline]
    pub fn next(&mut self) -> f64 {
        if self.cur != self.target {
            self.cur += (self.target - self.cur) * self.coef;
            if (self.target - self.cur).abs() < 1e-6 {
                self.cur = self.target;
            }
        }
        self.cur
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_still_glide_gives_its_value_exactly() {
        let mut g = Glide::new(0.37);
        g.begin(64);
        assert!((0..64).all(|_| g.next() == 0.37));
        let mut t = TimeGlide::new(123.4, 48_000, 50.0);
        assert!((0..64).all(|_| t.next() == 123.4));
    }

    #[test]
    fn a_glide_reaches_its_target_in_a_straight_line_over_the_block() {
        let mut g = Glide::new(0.0);
        g.set(1.0);
        g.begin(4);
        let v: Vec<f64> = (0..4).map(|_| g.next()).collect();
        assert_eq!(v, vec![0.25, 0.5, 0.75, 1.0]);
        assert!(!g.moving());
        let mut t = TimeGlide::new(0.0, 48_000, 10.0);
        t.set(100.0);
        for _ in 0..48_000 {
            t.next();
        }
        assert_eq!(t.next(), 100.0);
    }
}

/// Helpers for the automation tests of each effect.
#[cfg(test)]
pub(crate) mod testing {
    use std::collections::BTreeMap;

    use super::super::tests::slot;
    use super::super::{build, info, Params, Processor};

    pub const RATE: u32 = 48_000;

    pub fn params(effect: &str, set: &[(&str, f64)]) -> Params {
        let saved: BTreeMap<String, f64> =
            set.iter().map(|(k, v)| ((*k).to_string(), *v)).collect();
        Params::resolve(&info(effect).unwrap(), &saved)
    }

    pub fn make(effect: &str, set: &[(&str, f64)]) -> Box<dyn Processor> {
        build(&slot(effect, set), RATE).unwrap()
    }

    pub fn noise(frames: usize, seed: u32) -> Vec<f32> {
        let mut s = seed;
        (0..frames * 2)
            .map(|_| {
                s = s.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                ((s >> 8) as f32 / 8_388_608.0 - 1.0) * 0.3
            })
            .collect()
    }

    pub fn sine(freq: f64, frames: usize) -> Vec<f32> {
        (0..frames)
            .flat_map(|i| {
                let v =
                    (std::f64::consts::TAU * freq * i as f64 / f64::from(RATE)).sin() as f32 * 0.5;
                [v, v]
            })
            .collect()
    }

    pub fn rms(b: &[f32]) -> f32 {
        (b.iter().map(|x| x * x).sum::<f32>() / b.len().max(1) as f32).sqrt()
    }

    /// The largest step between neighbouring frames of either channel.
    pub fn largest_step(b: &[f32]) -> f32 {
        b.windows(4)
            .step_by(2)
            .map(|w| (w[2] - w[0]).abs().max((w[3] - w[1]).abs()))
            .fold(0.0, f32::max)
    }

    /// Every control (but `skip`) is taken in place, at its ends and its
    /// default, while audio runs, and the sound stays finite.
    pub fn every_control_updates(effect: &str, skip: &[&str]) {
        let fx_info = info(effect).unwrap();
        for p in fx_info.params.iter().filter(|p| !p.hidden) {
            if skip.contains(&p.key) {
                continue;
            }
            let mut fx = make(effect, &[]);
            let mut warm = noise(4800, 7);
            fx.process(&mut warm);
            for v in [p.min, p.max, p.default] {
                assert!(
                    fx.update(&params(effect, &[(p.key, v)])),
                    "{effect}: {} = {v} should change in place",
                    p.key
                );
                let mut b = noise(4800, 9);
                for c in b.chunks_mut(2 * 128) {
                    fx.process(c);
                }
                assert!(b.iter().all(|s| s.is_finite()), "{effect}: {} = {v}", p.key);
            }
        }
    }

    /// Runs `input` in blocks of `block` frames, moving `key` in a straight
    /// line from `from` to `to` across it, with `update` before each block.
    pub fn ramp(
        fx: &mut dyn Processor,
        effect: &str,
        set: &[(&str, f64)],
        key: &str,
        (from, to): (f64, f64),
        input: &mut [f32],
        block: usize,
    ) {
        let blocks = input.len().div_ceil(block * 2).max(1);
        for (i, c) in input.chunks_mut(block * 2).enumerate() {
            let v = from + (to - from) * (i + 1) as f64 / blocks as f64;
            let mut all: Vec<(&str, f64)> = set.to_vec();
            all.push((key, v));
            assert!(fx.update(&params(effect, &all)), "{effect}: {key}");
            fx.process(c);
        }
    }

    /// The level (dB) of a 0.5 sine in the last quarter of `b`.
    pub fn level(b: &[f32]) -> f64 {
        let peak = b[b.len() * 3 / 4..]
            .iter()
            .fold(0f32, |m, s| m.max(s.abs()));
        20.0 * (f64::from(peak) / 0.5).log10()
    }

    /// Sweeps `key` over the first half second, then holds; the end
    /// must sound like the filter built at the end value.
    pub fn sweep_matches_static(
        fx: &str,
        set: &[(&str, f64)],
        key: &str,
        range: (f64, f64),
        freq: f64,
    ) {
        let run = |v: f64| {
            let mut still = make(fx, &[set, &[(key, v)]].concat());
            let mut s = sine(freq, RATE as usize);
            for c in s.chunks_mut(128) {
                still.process(c);
            }
            s
        };
        let (first, s) = (run(range.0), run(range.1));
        let mut moving = make(fx, &[set, &[(key, range.0)]].concat());
        let mut b = sine(freq, RATE as usize);
        let half = b.len() / 2;
        let (a, rest) = b.split_at_mut(half);
        ramp(&mut *moving, fx, set, key, range, a, 64);
        for c in rest.chunks_mut(128) {
            moving.process(c);
        }
        assert!(b.iter().all(|s| s.is_finite()), "{fx} {key}");
        // No click: the sweep never steps further between frames than the
        // filter does when it holds still at either end. (A filter that
        // restarted every block would.) The first frames are left out, as
        // a filter starting from rest rings in.
        let skip = 2 * 2400;
        let most = largest_step(&first[skip..]).max(largest_step(&s[skip..]));
        let step = largest_step(&b[skip..]);
        assert!(
            step < most * 1.25 + 0.01,
            "{fx} {key}: step {step}, still {most}"
        );
        let (l, w) = (level(&b), level(&s));
        assert!(
            (l - w).abs() < 0.5,
            "{fx} {key}: {l} dB after a sweep, {w} built"
        );
    }
}
