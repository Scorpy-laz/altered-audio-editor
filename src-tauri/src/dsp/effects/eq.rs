// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) The GeekCast Radio Network, LLC.
// This file is part of Altered Audio Editor.

//! Filter and EQ effects: Parametric EQ and Hum Cut.

use super::{choice, num, toggle, EffectInfo, Params, Processor};
use crate::dsp::biquad::{Coeffs, Shape, StereoBiquad};

const SLOPES: &[&str] = &["12 dB/oct", "24 dB/oct", "48 dB/oct"];

pub fn parametric_info() -> EffectInfo {
    let band = |n: &'static str, f: f64| {
        [
            num(
                match n {
                    "1" => "b1_freq",
                    "2" => "b2_freq",
                    _ => "b3_freq",
                },
                "Frequency",
                "Hz",
                20.0,
                20_000.0,
                f,
                1.0,
            )
            .log(),
            num(
                match n {
                    "1" => "b1_gain",
                    "2" => "b2_gain",
                    _ => "b3_gain",
                },
                "Gain",
                "dB",
                -24.0,
                24.0,
                0.0,
                0.1,
            ),
            num(
                match n {
                    "1" => "b1_q",
                    "2" => "b2_q",
                    _ => "b3_q",
                },
                "Width (Q)",
                "",
                0.1,
                12.0,
                1.0,
                0.01,
            )
            .log(),
        ]
    };
    let mut params = vec![
        toggle("hp_on", "On", false).group("High-pass"),
        num("hp_freq", "Frequency", "Hz", 10.0, 2_000.0, 80.0, 1.0)
            .log()
            .group("High-pass"),
        choice("hp_slope", "Slope", SLOPES, 1).group("High-pass"),
        num("ls_freq", "Frequency", "Hz", 20.0, 2_000.0, 120.0, 1.0)
            .log()
            .group("Low shelf"),
        num("ls_gain", "Gain", "dB", -24.0, 24.0, 0.0, 0.1).group("Low shelf"),
    ];
    for (n, f, g) in [
        ("1", 250.0, "Band 1"),
        ("2", 1_000.0, "Band 2"),
        ("3", 4_000.0, "Band 3"),
    ] {
        params.extend(band(n, f).map(|p| p.group(g)));
    }
    params.extend([
        num(
            "hs_freq",
            "Frequency",
            "Hz",
            1_000.0,
            20_000.0,
            8_000.0,
            1.0,
        )
        .log()
        .group("High shelf"),
        num("hs_gain", "Gain", "dB", -24.0, 24.0, 0.0, 0.1).group("High shelf"),
        toggle("lp_on", "On", false).group("Low-pass"),
        num(
            "lp_freq",
            "Frequency",
            "Hz",
            1_000.0,
            22_000.0,
            18_000.0,
            1.0,
        )
        .log()
        .group("Low-pass"),
        choice("lp_slope", "Slope", SLOPES, 1).group("Low-pass"),
    ]);
    EffectInfo {
        id: "altaudio.parametric_eq",
        name: "Parametric EQ",
        category: "Filter and EQ",
        description:
            "High-pass and low-pass filters, two shelves and three bands to shape the tone.",
        params,
    }
}

pub fn hum_info() -> EffectInfo {
    EffectInfo {
        id: "altaudio.hum_cut",
        name: "Hum Cut",
        category: "Filter and EQ",
        description: "Removes electrical hum and its overtones with narrow notches.",
        params: vec![
            choice(
                "mains",
                "Mains",
                &["60 Hz (Americas)", "50 Hz (most other places)"],
                0,
            ),
            num("harmonics", "Overtones", "", 1.0, 10.0, 4.0, 1.0),
            num("q", "Narrowness", "", 2.0, 60.0, 25.0, 1.0).log(),
        ],
    }
}

/// A filter that can be left out of the chain. A filter that joins
/// starts from rest; one that leaves keeps nothing that matters.
pub(crate) struct Stage {
    f: StereoBiquad,
    pub(crate) on: bool,
}

impl Stage {
    pub(crate) fn off() -> Self {
        Self {
            f: StereoBiquad::new(Coeffs::FLAT),
            on: false,
        }
    }

    /// New coefficients, keeping the filter's memory while it stays on.
    pub(crate) fn set(&mut self, c: Coeffs) {
        if !self.on {
            self.f.reset();
            self.on = true;
        }
        self.f.l.set(c);
        self.f.r.set(c);
    }

    pub(crate) fn process(&mut self, buf: &mut [f32]) {
        if self.on {
            self.f.process(buf);
        }
    }

    pub(crate) fn reset(&mut self) {
        self.f.reset();
    }
}

/// Q of section `k` of a Butterworth cascade of `sections`, worked out
/// the same way as `butterworth_qs` without making a list.
pub(crate) fn butterworth_q(sections: usize, k: usize) -> f64 {
    let n = (sections * 2) as f64;
    1.0 / (2.0 * (std::f64::consts::PI * (2.0 * k as f64 + 1.0) / (2.0 * n)).sin())
}

const BANDS: [[&str; 3]; 3] = [
    ["b1_freq", "b1_gain", "b1_q"],
    ["b2_freq", "b2_gain", "b2_q"],
    ["b3_freq", "b3_gain", "b3_q"],
];

/// High-pass (up to four sections), low shelf, three bands, high shelf
/// and low-pass (up to four), in series. Bands and shelves at 0 dB are
/// left out until they are first moved.
pub struct ParametricEq {
    rate: u32,
    hp: [Stage; 4],
    ls: Stage,
    bands: [Stage; 3],
    hs: Stage,
    lp: [Stage; 4],
}

impl ParametricEq {
    pub fn new(p: &Params, rate: u32) -> Self {
        let mut me = Self {
            rate,
            hp: std::array::from_fn(|_| Stage::off()),
            ls: Stage::off(),
            bands: std::array::from_fn(|_| Stage::off()),
            hs: Stage::off(),
            lp: std::array::from_fn(|_| Stage::off()),
        };
        me.tune(p);
        me
    }

    /// Sets every filter from `p`, keeping the memory of those that stay.
    fn tune(&mut self, p: &Params) {
        let rate = self.rate;
        let sections = |slope: usize| [1usize, 2, 4][slope.min(2)];
        let pass = |stages: &mut [Stage; 4], on: bool, shape: Shape, freq: f64, slope: usize| {
            let n = if on { sections(slope) } else { 0 };
            for (k, s) in stages.iter_mut().enumerate() {
                if k < n {
                    s.set(Coeffs::new(shape, rate, freq, butterworth_q(n, k), 0.0));
                } else {
                    s.on = false;
                }
            }
        };
        pass(
            &mut self.hp,
            p.on("hp_on"),
            Shape::HighPass,
            p.get("hp_freq"),
            p.index("hp_slope"),
        );
        // A band or shelf joins once its gain is off 0 dB and then stays,
        // so a gain moving through 0 dB never restarts it.
        let gain = |k: &str| p.get(k).abs() > 1e-3;
        let boost = |s: &mut Stage, shape: Shape, freq: f64, q: f64, g: &str| {
            if s.on || gain(g) {
                s.set(Coeffs::new(shape, rate, freq, q, p.get(g)));
            }
        };
        boost(
            &mut self.ls,
            Shape::LowShelf,
            p.get("ls_freq"),
            1.0,
            "ls_gain",
        );
        for (s, k) in self.bands.iter_mut().zip(BANDS) {
            boost(s, Shape::Peak, p.get(k[0]), p.get(k[2]), k[1]);
        }
        boost(
            &mut self.hs,
            Shape::HighShelf,
            p.get("hs_freq"),
            1.0,
            "hs_gain",
        );
        pass(
            &mut self.lp,
            p.on("lp_on"),
            Shape::LowPass,
            p.get("lp_freq"),
            p.index("lp_slope"),
        );
    }

    fn stages(&mut self) -> impl Iterator<Item = &mut Stage> {
        self.hp
            .iter_mut()
            .chain(std::iter::once(&mut self.ls))
            .chain(self.bands.iter_mut())
            .chain(std::iter::once(&mut self.hs))
            .chain(self.lp.iter_mut())
    }
}

impl Processor for ParametricEq {
    fn process(&mut self, buf: &mut [f32]) {
        for s in self.stages() {
            s.process(buf);
        }
    }
    fn reset(&mut self) {
        for s in self.stages() {
            s.reset();
        }
    }
    fn update(&mut self, p: &Params) -> bool {
        self.tune(p);
        true
    }
}

/// Most overtones Hum Cut notches.
const HARMONICS: usize = 10;

/// Notches at the mains frequency and its overtones.
pub struct HumCut {
    rate: u32,
    filters: [Stage; HARMONICS],
}

impl HumCut {
    pub fn new(p: &Params, rate: u32) -> Self {
        let mut me = Self {
            rate,
            filters: std::array::from_fn(|_| Stage::off()),
        };
        me.tune(p);
        me
    }

    fn tune(&mut self, p: &Params) {
        let base = if p.index("mains") == 1 { 50.0 } else { 60.0 };
        let q = p.get("q");
        let n = p.index("harmonics").clamp(1, HARMONICS);
        let nyquist = f64::from(self.rate) * 0.45;
        for (k, s) in self.filters.iter_mut().enumerate() {
            let f = base * (k + 1) as f64;
            if k < n && f < nyquist {
                s.set(Coeffs::new(Shape::Notch, self.rate, f, q, 0.0));
            } else {
                s.on = false;
            }
        }
    }
}

impl Processor for HumCut {
    fn process(&mut self, buf: &mut [f32]) {
        for f in &mut self.filters {
            f.process(buf);
        }
    }
    fn reset(&mut self) {
        for f in &mut self.filters {
            f.reset();
        }
    }
    fn update(&mut self, p: &Params) -> bool {
        self.tune(p);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::super::build;
    use super::super::tests::slot;
    use std::f64::consts::PI;

    fn level(effect: &str, params: &[(&str, f64)], freq: f64) -> f64 {
        let rate = 48_000;
        let mut p = build(&slot(effect, params), rate).unwrap();
        let n = rate as usize;
        let mut b: Vec<f32> = (0..n)
            .flat_map(|i| {
                let v = (2.0 * PI * freq * i as f64 / f64::from(rate)).sin() as f32 * 0.5;
                [v, v]
            })
            .collect();
        p.process(&mut b);
        let peak = b[n..].iter().fold(0f32, |m, s| m.max(s.abs()));
        20.0 * (f64::from(peak) / 0.5).log10()
    }

    #[test]
    fn eq_bands_and_filters() {
        let eq = "altaudio.parametric_eq";
        assert!(level(eq, &[], 1000.0).abs() < 0.05, "flat by default");
        assert!((level(eq, &[("b2_gain", 6.0)], 1000.0) - 6.0).abs() < 0.3);
        assert!(
            level(
                eq,
                &[("hp_on", 1.0), ("hp_freq", 100.0), ("hp_slope", 2.0)],
                40.0
            ) < -40.0
        );
        assert!(level(eq, &[("lp_on", 1.0), ("lp_freq", 3000.0)], 12_000.0) < -30.0);
    }

    #[test]
    fn hum_cut_removes_hum_and_keeps_voice() {
        let h = "altaudio.hum_cut";
        assert!(level(h, &[], 60.0) < -25.0);
        assert!(level(h, &[], 180.0) < -25.0);
        assert!(level(h, &[("mains", 1.0)], 100.0) < -25.0);
        assert!(level(h, &[], 1000.0).abs() < 0.3);
    }

    mod automation {
        use super::super::super::glide::testing::*;

        #[test]
        fn every_eq_control_changes_in_place() {
            every_control_updates("altaudio.parametric_eq", &[]);
            every_control_updates("altaudio.hum_cut", &[]);
        }

        #[test]
        fn filter_sweeps_keep_their_memory_and_end_where_built() {
            let eq = "altaudio.parametric_eq";
            sweep_matches_static(
                eq,
                &[("lp_on", 1.0)],
                "lp_freq",
                (20_000.0, 1_500.0),
                3000.0,
            );
            sweep_matches_static(eq, &[("hp_on", 1.0)], "hp_freq", (10.0, 1_000.0), 400.0);
            sweep_matches_static(eq, &[], "b2_gain", (-12.0, 12.0), 1000.0);
            sweep_matches_static(eq, &[("b2_gain", 9.0)], "b2_freq", (200.0, 2000.0), 4000.0);
            sweep_matches_static(eq, &[], "ls_gain", (0.0, -12.0), 60.0);
            sweep_matches_static("altaudio.hum_cut", &[], "q", (60.0, 2.0), 125.0);
        }
    }
}
