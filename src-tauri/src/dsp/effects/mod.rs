// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) The GeekCast Radio Network, LLC.
// This file is part of Altered Audio Editor.

//! Built-in effects and the Voice Chain that runs them.
//!
//! Every effect is described by data ([`catalog`]): its id, name, group
//! and parameters with ranges and defaults. The interface draws its
//! controls from that, so adding an effect needs no interface code.
//! Saved chains store parameter values by key; a missing key takes the
//! default and an unknown key is ignored, so old files always open.
//!
//! Processing runs on the render thread, not the audio callback, so
//! effects may keep state but must not block. Effects with look-ahead
//! report a latency, which the renderer compensates for.

pub mod amp;
pub mod center_extract;
pub mod curve_dynamics;
mod dynamics;
mod echo;
mod eq;
mod expander;
mod filters;
mod gapfill;
pub mod mastering;
mod modulation;
mod phase;
pub mod phase_curve;
mod pitch;
mod plosive;
pub mod print;
mod restore;
pub mod reverb;
pub mod shaped;
mod space;
mod speech_denoiser;
mod stereo;
mod suite;
mod true_peak;
pub mod tube;
pub mod vocal;

use std::collections::BTreeMap;

use serde::Serialize;

use crate::session::model::{EffectChain, EffectSlot, Id};

pub use dynamics::{Compressor, DeEsser, Gate, Limiter, Multiband, SpeechLeveler};
pub use eq::{HumCut, ParametricEq};
pub use gapfill::{context_wanted, fill_gap, FillTexture};
pub use restore::{fill_span, find_spots, repair_spots, Spots};

/// Effects with a level detector that can listen to a sidechain key.
pub const KEYED: [&str; 2] = ["altaudio.dynamics", "altaudio.breath_gate"];

/// Most effects a chain holds.
pub const MAX_SLOTS: usize = 16;

/// How a parameter is shown and stored.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ParamInfo {
    pub key: &'static str,
    pub name: &'static str,
    /// Shown after the value: "dB", "Hz", "ms", ":1", "" ...
    pub unit: &'static str,
    pub min: f64,
    pub max: f64,
    pub default: f64,
    pub step: f64,
    /// Logarithmic slider (frequencies, times).
    pub log: bool,
    /// For a list: the label of each whole value from `min`.
    #[serde(skip_serializing_if = "<[&str]>::is_empty")]
    pub choices: &'static [&'static str],
    /// An on/off switch (0 or 1).
    pub toggle: bool,
    /// Controls that belong together share a group name in the editor.
    #[serde(skip_serializing_if = "str::is_empty")]
    pub group: &'static str,
    /// Changing it changes the effect's look-ahead, so playback restarts
    /// to pick it up; the editor applies it on release, not while dragging.
    pub retimes: bool,
    /// Stored with the effect but not a control (a learned noise print).
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub hidden: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EffectInfo {
    pub id: &'static str,
    pub name: &'static str,
    /// Menu group: "Filter and EQ", "Dynamics", "Amplitude", "Restoration", "Time and Pitch".
    pub category: &'static str,
    pub description: &'static str,
    pub params: Vec<ParamInfo>,
}

pub(crate) const fn num(
    key: &'static str,
    name: &'static str,
    unit: &'static str,
    min: f64,
    max: f64,
    default: f64,
    step: f64,
) -> ParamInfo {
    ParamInfo {
        key,
        name,
        unit,
        min,
        max,
        default,
        step,
        log: false,
        choices: &[],
        toggle: false,
        group: "",
        retimes: false,
        hidden: false,
    }
}

impl ParamInfo {
    pub(crate) const fn log(mut self) -> Self {
        self.log = true;
        self
    }
    pub(crate) const fn group(mut self, g: &'static str) -> Self {
        self.group = g;
        self
    }
    pub(crate) const fn hidden(mut self) -> Self {
        self.hidden = true;
        self
    }
}

pub(crate) const fn toggle(key: &'static str, name: &'static str, default: bool) -> ParamInfo {
    ParamInfo {
        key,
        name,
        unit: "",
        min: 0.0,
        max: 1.0,
        default: if default { 1.0 } else { 0.0 },
        step: 1.0,
        log: false,
        choices: &[],
        toggle: true,
        group: "",
        retimes: false,
        hidden: false,
    }
}

pub(crate) const fn choice(
    key: &'static str,
    name: &'static str,
    choices: &'static [&'static str],
    default: usize,
) -> ParamInfo {
    ParamInfo {
        key,
        name,
        unit: "",
        min: 0.0,
        max: (choices.len() - 1) as f64,
        default: default as f64,
        step: 1.0,
        log: false,
        choices,
        toggle: false,
        group: "",
        retimes: false,
        hidden: false,
    }
}

/// Every built-in effect, in menu order.
pub fn catalog() -> Vec<EffectInfo> {
    let [b10, b20, b30] = filters::band_infos();
    vec![
        eq::parametric_info(),
        b10,
        b20,
        b30,
        eq::hum_info(),
        filters::notch_info(),
        filters::precision_info(),
        filters::curve_info(),
        phase::phase_info(),
        phase_curve::phase_curve_info(),
        dynamics::compressor_info(),
        curve_dynamics::info(),
        dynamics::multiband_info(),
        dynamics::limiter_info(),
        dynamics::gate_info(),
        dynamics::deesser_info(),
        plosive::info(),
        expander::info(),
        suite::info(),
        dynamics::leveler_info(),
        tube::opto_info(),
        tube::preamp_info(),
        vocal::vocal_info(),
        filters::center_info(),
        center_extract::info(),
        reverb::reverb_info(),
        pitch::pitch_info(),
        echo::echo_info(),
        modulation::chorus_info(),
        modulation::flanger_info(),
        modulation::phaser_info(),
        space::room_info(),
        space::tape_info(),
        stereo::mixer_info(),
        stereo::expander_info(),
        stereo::distortion_info(),
        mastering::info(),
        restore::noise_info(),
        speech_denoiser::info(),
        speech_denoiser::deep_info(),
        print::print_info(),
        print::eraser_info(),
        shaped::shaped_info(),
        amp::amp_info(),
        restore::hiss_info(),
        restore::room_info(),
        restore::click_info(),
        restore::clip_info(),
        restore::dc_info(),
        EffectInfo {
            id: "altaudio.gain",
            name: "Gain",
            category: "Amplitude",
            description: "Turns the level up or down by a fixed amount.",
            params: vec![num("gain", "Gain", "dB", -48.0, 24.0, 0.0, 0.1)],
        },
    ]
}

/// The catalog for the editor, with each control marked when moving it
/// changes the effect's look-ahead (found by building the effect at the
/// control's lowest, default and highest values).
pub fn ui_catalog() -> Vec<EffectInfo> {
    let mut all = catalog();
    for e in &mut all {
        let id = e.id;
        let latency = |key: &str, v: f64| {
            let slot = EffectSlot {
                id: crate::session::model::new_id(),
                effect: id.to_string(),
                enabled: true,
                params: [(key.to_string(), v)].into_iter().collect(),
                preset: None,
                sidechain: None,
                plugin_state: None,
                data: None,
                position: None,
            };
            build(&slot, 48_000).map_or(0, |p| p.latency())
        };
        for p in e.params.iter_mut().filter(|p| !p.hidden) {
            let base = latency(p.key, p.default);
            p.retimes = latency(p.key, p.min) != base || latency(p.key, p.max) != base;
        }
    }
    all
}

pub fn info(id: &str) -> Option<EffectInfo> {
    if crate::plugins::is_plugin(id) {
        return crate::plugins::find(id).map(|f| crate::plugins::effect_info(&f));
    }
    catalog().into_iter().find(|e| e.id == id)
}

/// The built-in effects and the installed plug-ins, for the editor.
pub fn full_catalog() -> Vec<EffectInfo> {
    let mut all = ui_catalog();
    all.extend(crate::plugins::catalog());
    all
}

/// A chain's total look-ahead, without starting any plug-in (a running
/// one reports its own; otherwise the scan's figure).
pub fn chain_latency(chain: &EffectChain, rate: u32) -> usize {
    if !chain.enabled {
        return 0;
    }
    chain
        .slots
        .iter()
        .take(MAX_SLOTS)
        .filter(|s| s.enabled)
        .map(|s| {
            if crate::plugins::is_plugin(&s.effect) {
                if crate::plugins::find(&s.effect).is_some() {
                    crate::plugins::instance::latency(s)
                } else {
                    0
                }
            } else {
                build(s, rate).map_or(0, |p| p.latency())
            }
        })
        .sum()
}

/// A slot's parameter values, each clamped to its range, with defaults
/// for missing ones.
#[derive(Debug, Clone)]
pub struct Params(BTreeMap<&'static str, f64>);

impl Params {
    pub fn resolve(info: &EffectInfo, saved: &BTreeMap<String, f64>) -> Self {
        Params(
            info.params
                .iter()
                .map(|p| {
                    let v = saved
                        .get(p.key)
                        .copied()
                        .filter(|v| v.is_finite())
                        .unwrap_or(p.default)
                        .clamp(p.min, p.max);
                    (p.key, v)
                })
                .collect(),
        )
    }

    /// Every value by key.
    pub fn values(&self) -> impl Iterator<Item = (&'static str, f64)> + '_ {
        self.0.iter().map(|(k, v)| (*k, *v))
    }

    pub fn get(&self, key: &str) -> f64 {
        self.0.get(key).copied().unwrap_or(0.0)
    }

    pub fn on(&self, key: &str) -> bool {
        self.get(key) >= 0.5
    }

    pub fn index(&self, key: &str) -> usize {
        self.get(key).round().max(0.0) as usize
    }
}

/// One running effect. Audio is stereo interleaved, processed in place.
pub trait Processor: Send {
    fn process(&mut self, buf: &mut [f32]);
    /// Frames of delay this effect adds (look-ahead).
    fn latency(&self) -> usize {
        0
    }
    /// Clears filter memory and envelopes, as after a jump.
    fn reset(&mut self);
    /// Takes new settings while playing, keeping its state, for
    /// automation. False when the effect cannot; the chain then rebuilds it.
    fn update(&mut self, _p: &Params) -> bool {
        false
    }
    /// True for a VST3 plug-in, which keeps running between chain builds
    /// and must not hear the same audio twice.
    fn is_plugin(&self) -> bool {
        false
    }
    /// Runs with a sidechain key, as long as `buf`: an effect with a
    /// level detector listens to `key` instead of its own input. Others
    /// ignore the key.
    fn process_keyed(&mut self, buf: &mut [f32], _key: &[f32]) {
        self.process(buf);
    }
}

/// A fixed delay for a stereo interleaved stream.
#[derive(Debug, Clone, Default)]
pub struct Delay {
    ring: Vec<f32>,
    at: usize,
}

impl Delay {
    pub fn new(frames: usize) -> Self {
        Self {
            ring: vec![0.0; frames * 2],
            at: 0,
        }
    }

    pub fn frames(&self) -> usize {
        self.ring.len() / 2
    }

    /// Delays `buf` in place.
    pub fn run(&mut self, buf: &mut [f32]) {
        let n = self.ring.len();
        if n == 0 {
            return;
        }
        for s in buf.iter_mut() {
            std::mem::swap(s, &mut self.ring[self.at]);
            self.at = (self.at + 1) % n;
        }
    }

    pub fn clear(&mut self) {
        self.ring.fill(0.0);
        self.at = 0;
    }
}

struct Gain(f32);

impl Processor for Gain {
    fn process(&mut self, buf: &mut [f32]) {
        for s in buf {
            *s *= self.0;
        }
    }
    fn reset(&mut self) {}
    fn update(&mut self, p: &Params) -> bool {
        self.0 = db_to_gain(p.get("gain"));
        true
    }
}

pub(crate) fn db_to_gain(db: f64) -> f32 {
    10f64.powf(db / 20.0) as f32
}

/// Builds the processor for one slot, or None for an unknown effect (or a
/// plug-in that is not installed or would not start).
pub fn build(slot: &EffectSlot, rate: u32) -> Option<Box<dyn Processor>> {
    if crate::plugins::is_plugin(&slot.effect) {
        return crate::plugins::instance::processor(slot, rate)
            .map(|p| Box::new(p) as Box<dyn Processor>);
    }
    let info = info(&slot.effect)?;
    let p = Params::resolve(&info, &slot.params);
    Some(match info.id {
        "altaudio.parametric_eq" => Box::new(ParametricEq::new(&p, rate)),
        "altaudio.hum_cut" => Box::new(HumCut::new(&p, rate)),
        "altaudio.dynamics" => Box::new(Compressor::new(&p, rate)),
        "altaudio.multiband" => Box::new(Multiband::new(&p, rate)),
        "altaudio.limiter" => Box::new(Limiter::new(&p, rate)),
        "altaudio.breath_gate" => Box::new(Gate::new(&p, rate)),
        "altaudio.expander" => Box::new(expander::Expander::new(&p, rate)),
        suite::SUITE => Box::new(suite::Suite::new(&p, rate)),
        "altaudio.deesser" => Box::new(DeEsser::new(&p, rate)),
        "altaudio.plosive" => Box::new(plosive::Plosive::new(&p, rate)),
        "altaudio.speech_leveler" => Box::new(SpeechLeveler::new(&p, rate)),
        "altaudio.gain" => Box::new(Gain(db_to_gain(p.get("gain")))),
        "altaudio.noise_reducer" => Box::new(restore::noise_reducer(&p, rate)),
        speech_denoiser::ID => Box::new(speech_denoiser::SpeechDenoiser::new(&p, rate)?),
        speech_denoiser::DEEP_ID => Box::new(speech_denoiser::SpeechDenoiser::deep(&p, rate)?),
        "altaudio.noise_print" => Box::new(print::print_reducer(&p, rate)),
        "altaudio.sound_eraser" => Box::new(print::sound_eraser(&p, rate)),
        shaped::SHAPED_NOISE => Box::new(shaped::shaped_reducer(&p, rate)),
        amp::AMP => Box::new(amp::AmpModel::new(&p, rate, slot.data.as_deref())),
        tube::OPTO => Box::new(tube::Opto::new(
            &p,
            rate,
            crate::dsp::meters::for_slot(slot.id),
        )),
        tube::PREAMP => Box::new(tube::Preamp::new(
            &p,
            rate,
            crate::dsp::meters::for_slot(slot.id),
        )),
        vocal::VOCAL => Box::new(vocal::VoxStrip::new(
            &p,
            rate,
            crate::dsp::meters::for_slot(slot.id),
        )),
        "altaudio.hiss_reducer" => Box::new(restore::hiss_reducer(&p, rate)),
        "altaudio.room_reducer" => Box::new(restore::room_reducer(&p, rate)),
        "altaudio.click_repair" => Box::new(restore::GapRepair::clicks(&p, rate)),
        "altaudio.clip_repair" => Box::new(restore::GapRepair::clipping(&p, rate)),
        "altaudio.dc_offset" => Box::new(restore::DcBlock::new(rate)),
        "altaudio.band_eq_10" => Box::new(filters::BandEq::new(10, &p, rate)),
        "altaudio.band_eq_20" => Box::new(filters::BandEq::new(20, &p, rate)),
        "altaudio.band_eq_30" => Box::new(filters::BandEq::new(30, &p, rate)),
        "altaudio.notch" => Box::new(filters::Notches::new(&p, rate)),
        "altaudio.precision_filter" => Box::new(filters::Precision::new(&p, rate)),
        "altaudio.curve_eq" => Box::new(filters::CurveEq::new(&p, rate)),
        "altaudio.center_remover" => Box::new(filters::CenterRemover::new(&p, rate)),
        "altaudio.center_extract" => Box::new(center_extract::CenterExtract::new(&p, rate)),
        "altaudio.pitch_shift" => Box::new(pitch::PitchShift::new(&p, rate)),
        "altaudio.echo" => Box::new(echo::Echo::new(&p, rate)),
        "altaudio.chorus" => Box::new(modulation::Chorus::new(&p, rate)),
        "altaudio.flanger" => Box::new(modulation::Flanger::new(&p, rate)),
        "altaudio.phaser" => Box::new(modulation::Phaser::new(&p, rate)),
        "altaudio.room-reverb" => Box::new(space::Room::new(&p, rate)),
        "altaudio.tape-delay" => Box::new(space::Tape::new(&p, rate)),
        "altaudio.channel-mixer" => Box::new(stereo::Mixer::new(&p)),
        "altaudio.stereo-expander" => Box::new(stereo::Expander::new(&p, rate)),
        "altaudio.distortion" => Box::new(stereo::Distortion::new(&p, rate)),
        mastering::ID => Box::new(mastering::Mastering::new(&p, rate)),
        "altaudio.phase_rotate" => Box::new(phase::PhaseRotate::new(&p, rate)),
        "altaudio.phase_curve" => Box::new(phase_curve::PhaseCurve::new(&p, rate)),
        "altaudio.dynamics_curve" => Box::new(curve_dynamics::DynamicsCurve::new(&p, rate)),
        reverb::ID => Box::new(reverb::ConvolutionReverb::new(
            &p,
            rate,
            slot.data.as_deref(),
        )),
        _ => return None,
    })
}

/// True when `chain` would change the sound: the same test
/// [`ChainProcessor::new`] makes, without building anything.
pub fn is_active(chain: &EffectChain) -> bool {
    chain.enabled
        && (chain.input_gain_db.clamp(-48.0, 24.0) != 0.0
            || chain.output_gain_db.clamp(-48.0, 24.0) != 0.0
            || chain
                .slots
                .iter()
                .take(MAX_SLOTS)
                .any(|s| s.enabled && info(&s.effect).is_some()))
}

/// A Voice Chain ready to run: input gain, each enabled effect in order,
/// output gain, then the dry/wet mix.
/// A running stage's slot: id, effect, settings and its own data, kept
/// so a stage can be rebuilt when automation needs it.
type SlotState = (Id, EffectInfo, BTreeMap<String, f64>, Option<String>);

/// An automated setting that a stage cannot take in place is applied by
/// rebuilding the stage, which loses its state (envelopes, tails, filter
/// memory). Rebuilding on every automation step would wipe that state
/// every few milliseconds, so while the setting keeps moving a stage is
/// rebuilt at most this often...
const REBUILD_GAP_MS: u32 = 250;
/// ...and once the setting stops moving for this long, the last value is
/// applied. A single jump after a quiet spell is applied at once.
const REBUILD_SETTLE_MS: u32 = 40;

/// A held-back rebuild for one stage.
#[derive(Debug, Clone, Copy)]
struct Rebuild {
    waiting: bool,
    /// Frames since the setting last moved.
    since_change: usize,
    /// Frames since the stage was last rebuilt.
    since_build: usize,
}

impl Rebuild {
    const IDLE: Rebuild = Rebuild {
        waiting: false,
        since_change: usize::MAX / 2,
        since_build: usize::MAX / 2,
    };
}

pub struct ChainProcessor {
    stages: Vec<Box<dyn Processor>>,
    /// Per stage: the track or bus keying it, and a delay that holds the
    /// key back by the look-ahead of the stages before it, so key and
    /// signal still line up when they reach it.
    keys: Vec<Option<(Id, Delay)>>,
    key_buf: Vec<f32>,
    /// Per stage: its slot id, what it is and its current settings, so
    /// automation can change one setting while it plays.
    slots: Vec<SlotState>,
    /// Per stage: a rebuild waiting for its setting to settle.
    rebuilds: Vec<Rebuild>,
    rate: u32,
    input: f32,
    output: f32,
    mix: f32,
    /// The dry signal, delayed to line up with the effects when they add
    /// latency. Grows once to the largest block seen.
    dry: Vec<f32>,
    dry_delay: Vec<f32>,
    dry_at: usize,
    latency: usize,
}

impl ChainProcessor {
    /// None when the chain would do nothing (off, empty, or every effect
    /// bypassed with unity gains), so the renderer can skip it entirely.
    pub fn new(chain: &EffectChain, rate: u32) -> Option<Self> {
        if !chain.enabled {
            return None;
        }
        let mut stages: Vec<Box<dyn Processor>> = Vec::new();
        let mut slots = Vec::new();
        let mut keys = Vec::new();
        let mut before = 0;
        for s in chain.slots.iter().take(MAX_SLOTS).filter(|s| s.enabled) {
            if let (Some(p), Some(i)) = (build(s, rate), info(&s.effect)) {
                keys.push(s.sidechain.map(|id| (id, Delay::new(before))));
                before += p.latency();
                stages.push(p);
                slots.push((s.id, i, s.params.clone(), s.data.clone()));
            }
        }
        let input = db_to_gain(chain.input_gain_db.clamp(-48.0, 24.0));
        let output = db_to_gain(chain.output_gain_db.clamp(-48.0, 24.0));
        let mix = chain.mix.clamp(0.0, 1.0) as f32;
        if stages.is_empty() && input == 1.0 && output == 1.0 {
            return None;
        }
        let latency = stages.iter().map(|s| s.latency()).sum();
        Some(Self {
            rebuilds: vec![Rebuild::IDLE; stages.len()],
            stages,
            keys,
            key_buf: Vec::new(),
            slots,
            rate,
            input,
            output,
            mix,
            dry: Vec::new(),
            dry_delay: vec![0.0; latency * 2],
            dry_at: 0,
            latency,
        })
    }

    pub fn latency(&self) -> usize {
        self.latency
    }

    /// True when a stage is a running plug-in, which must not be warmed up
    /// on audio it has already heard.
    pub fn has_plugins(&self) -> bool {
        self.stages.iter().any(|s| s.is_plugin())
    }

    /// Changes one setting of one effect while it plays (automation). An
    /// effect that cannot take it in place is rebuilt, unless that would
    /// change the chain's look-ahead. While such a setting keeps moving,
    /// the rebuild is held back (see [`REBUILD_GAP_MS`]).
    pub fn set_param(&mut self, slot: Id, key: &str, value: f64) {
        let Some(i) = self.slots.iter().position(|s| s.0 == slot) else {
            return;
        };
        let (_, info, params, _) = &mut self.slots[i];
        if params.get(key).is_some_and(|v| (v - value).abs() < 1e-6) {
            return;
        }
        params.insert(key.to_string(), value);
        let p = Params::resolve(info, params);
        if self.stages[i].update(&p) {
            // Every setting, including any held back, is now in place.
            self.rebuilds[i].waiting = false;
            return;
        }
        if self.rebuilds[i].since_build >= self.frames(REBUILD_GAP_MS) {
            self.rebuild(i);
        } else {
            self.rebuilds[i].waiting = true;
            self.rebuilds[i].since_change = 0;
        }
    }

    fn frames(&self, ms: u32) -> usize {
        (u64::from(self.rate) * u64::from(ms) / 1000) as usize
    }

    /// Builds stage `i` again from its current settings, if its look-ahead
    /// stays the same.
    fn rebuild(&mut self, i: usize) {
        let (id, info, params, data) = &self.slots[i];
        let slot = EffectSlot {
            id: *id,
            effect: info.id.to_string(),
            enabled: true,
            params: params.clone(),
            preset: None,
            sidechain: None,
            plugin_state: None,
            data: data.clone(),
            position: None,
        };
        if let Some(fresh) = build(&slot, self.rate) {
            if fresh.latency() == self.stages[i].latency() {
                self.stages[i] = fresh;
            }
        }
        self.rebuilds[i] = Rebuild {
            waiting: false,
            since_change: self.rebuilds[i].since_change,
            since_build: 0,
        };
    }

    /// Moves the held-back rebuilds' clocks on by `frames`, and runs the
    /// ones whose setting has settled or that have waited long enough.
    fn tick_rebuilds(&mut self, frames: usize) {
        let settle = self.frames(REBUILD_SETTLE_MS);
        let gap = self.frames(REBUILD_GAP_MS);
        for i in 0..self.rebuilds.len() {
            let r = &mut self.rebuilds[i];
            r.since_change = r.since_change.saturating_add(frames);
            r.since_build = r.since_build.saturating_add(frames);
            if r.waiting && (r.since_change >= settle || r.since_build >= gap) {
                self.rebuild(i);
            }
        }
    }

    pub fn reset(&mut self) {
        for s in &mut self.stages {
            s.reset();
        }
        for (_, d) in self.keys.iter_mut().flatten() {
            d.clear();
        }
        self.dry_delay.fill(0.0);
        self.dry_at = 0;
    }

    /// The tracks and buses this chain's effects listen to.
    pub fn key_sources(&self) -> impl Iterator<Item = Id> + '_ {
        self.keys.iter().flatten().map(|(id, _)| *id)
    }

    pub fn process(&mut self, buf: &mut [f32]) {
        self.process_keyed(buf, &[]);
    }

    /// Runs the chain with sidechain keys by source id, each as long as
    /// `buf`. A stage whose source has no key here listens to its input.
    pub fn process_keyed(&mut self, buf: &mut [f32], keys: &[(Id, &[f32])]) {
        self.tick_rebuilds(buf.len() / 2);
        let blend = self.mix < 1.0;
        if blend {
            self.dry.clear();
            self.dry.extend_from_slice(buf);
            // Delay the dry copy by the chain's latency so both line up.
            if !self.dry_delay.is_empty() {
                let n = self.dry_delay.len();
                for s in self.dry.iter_mut() {
                    std::mem::swap(s, &mut self.dry_delay[self.dry_at]);
                    self.dry_at = (self.dry_at + 1) % n;
                }
            }
        }
        if self.input != 1.0 {
            for s in buf.iter_mut() {
                *s *= self.input;
            }
        }
        for (st, key) in self.stages.iter_mut().zip(&mut self.keys) {
            let found = key.as_mut().and_then(|(id, delay)| {
                keys.iter()
                    .find(|(k, b)| k == id && b.len() == buf.len())
                    .map(|(_, b)| (delay, *b))
            });
            match found {
                Some((delay, k)) => {
                    self.key_buf.clear();
                    self.key_buf.extend_from_slice(k);
                    delay.run(&mut self.key_buf);
                    st.process_keyed(buf, &self.key_buf);
                }
                None => st.process(buf),
            }
        }
        let out = self.output;
        if blend {
            let wet = self.mix;
            let dry = 1.0 - wet;
            for (s, d) in buf.iter_mut().zip(&self.dry) {
                *s = *s * out * wet + *d * dry;
            }
        } else if out != 1.0 {
            for s in buf.iter_mut() {
                *s *= out;
            }
        }
    }
}

/// A short, stable fingerprint of a chain, so playback knows when it
/// changed.
pub fn chain_hash(chain: &EffectChain) -> u64 {
    let text = serde_json::to_string(chain).unwrap_or_default();
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in text.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h
}

#[cfg(test)]
mod tests {

    /// Noise from a fixed seed, so every run feeds the same sound.
    fn seeded(frames: usize) -> Vec<f32> {
        let mut x: u32 = 0x1234_5678;
        (0..frames * 2)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 17;
                x ^= x << 5;
                (x as f32 / u32::MAX as f32 - 0.5) * 0.5
            })
            .collect()
    }

    /// Every built-in effect, run twice on the same sound with default
    /// settings, gives the very same bits: no hidden randomness or state.
    #[test]
    fn every_built_in_effect_is_deterministic() {
        let input = seeded(24_000);
        for info in catalog() {
            // Effects that need a loaded file or capture have no default sound.
            let Some(mut a) = build(&slot(info.id, &[]), 48_000) else {
                continue;
            };
            let mut b = build(&slot(info.id, &[]), 48_000).unwrap();
            let (mut x, mut y) = (input.clone(), input.clone());
            for (ca, cb) in x.chunks_mut(2 * 777).zip(y.chunks_mut(2 * 777)) {
                a.process(ca);
                b.process(cb);
            }
            let same = x.iter().zip(&y).all(|(p, q)| p.to_bits() == q.to_bits());
            assert!(same, "{} gave different bits on a second run", info.id);
        }
    }
    use super::*;

    #[test]
    fn controls_that_move_the_look_ahead_are_marked() {
        let all = ui_catalog();
        let param = |e: &str, k: &str| {
            all.iter()
                .find(|x| x.id == e)
                .unwrap()
                .params
                .iter()
                .find(|p| p.key == k)
                .unwrap()
                .retimes
        };
        assert!(param("altaudio.limiter", "lookahead"));
        assert!(!param("altaudio.limiter", "ceiling"));
        assert!(!param("altaudio.gain", "gain"));
    }
    use crate::session::model::new_id;

    pub(crate) fn slot(effect: &str, params: &[(&str, f64)]) -> EffectSlot {
        EffectSlot {
            id: new_id(),
            effect: effect.into(),
            enabled: true,
            params: params.iter().map(|(k, v)| ((*k).to_string(), *v)).collect(),
            preset: None,
            sidechain: None,
            plugin_state: None,
            data: None,
            position: None,
        }
    }

    #[test]
    fn keyed_effects_exist() {
        for id in KEYED {
            assert!(info(id).is_some(), "{id}");
        }
    }

    #[test]
    fn catalog_is_well_formed() {
        let all = catalog();
        let mut ids: Vec<&str> = all.iter().map(|e| e.id).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), all.len(), "ids are unique");
        for e in &all {
            assert!(e.id.starts_with("altaudio."));
            for p in &e.params {
                assert!(
                    p.min <= p.default && p.default <= p.max,
                    "{} {}",
                    e.id,
                    p.key
                );
                assert!(
                    !p.log || p.min > 0.0,
                    "log needs a positive range: {}",
                    p.key
                );
            }
            // Every catalog entry builds.
            assert!(build(&slot(e.id, &[]), 48_000).is_some(), "{}", e.id);
        }
    }

    #[test]
    fn params_take_defaults_and_clamp() {
        let i = info("altaudio.gain").unwrap();
        let mut saved = BTreeMap::new();
        assert_eq!(Params::resolve(&i, &saved).get("gain"), 0.0);
        saved.insert("gain".into(), 99.0);
        saved.insert("unknown".into(), 1.0);
        assert_eq!(Params::resolve(&i, &saved).get("gain"), 24.0);
        saved.insert("gain".into(), f64::NAN);
        assert_eq!(Params::resolve(&i, &saved).get("gain"), 0.0);
    }

    #[test]
    fn chain_applies_gains_mix_and_skips_when_idle() {
        let mut c = EffectChain::default();
        assert!(
            ChainProcessor::new(&c, 48_000).is_none(),
            "empty chain is skipped"
        );
        c.slots.push(slot("altaudio.gain", &[("gain", -6.0206)]));
        let mut p = ChainProcessor::new(&c, 48_000).unwrap();
        let mut b = vec![1.0f32; 8];
        p.process(&mut b);
        assert!((b[0] - 0.5).abs() < 1e-3);
        c.mix = 0.5;
        let mut p = ChainProcessor::new(&c, 48_000).unwrap();
        let mut b = vec![1.0f32; 8];
        p.process(&mut b);
        assert!((b[0] - 0.75).abs() < 1e-3, "{}", b[0]);
        c.slots[0].enabled = false;
        c.mix = 1.0;
        assert!(ChainProcessor::new(&c, 48_000).is_none(), "all bypassed");
        c.enabled = false;
        c.slots[0].enabled = true;
        assert!(ChainProcessor::new(&c, 48_000).is_none(), "chain power off");
    }

    #[test]
    fn sidechain_key_drives_the_detector_after_earlier_look_ahead() {
        let src = new_id();
        let mut c = EffectChain::default();
        // A limiter with 1 ms look-ahead, then a gate keyed by `src`.
        c.slots.push(slot(
            "altaudio.limiter",
            &[("lookahead", 1.0), ("ceiling", 0.0)],
        ));
        let mut gate = slot(
            "altaudio.breath_gate",
            &[("threshold", -20.0), ("attack", 0.1), ("range", -100.0)],
        );
        gate.sidechain = Some(src);
        c.slots.push(gate);
        let mut p = ChainProcessor::new(&c, 48_000).unwrap();
        assert_eq!(p.key_sources().collect::<Vec<_>>(), vec![src]);
        let frames = 4_000;
        // Steady signal; the key is silent, then loud from frame 2,000.
        let mut buf = vec![0.3f32; frames * 2];
        let mut key = vec![0f32; frames * 2];
        key[4_000..].fill(0.5);
        p.process_keyed(&mut buf, &[(src, &key)]);
        let lat = p.latency();
        assert!(
            buf[(1_500 + lat) * 2].abs() < 1e-6,
            "closed while the key is quiet"
        );
        assert!(buf[(2_100 + lat) * 2] > 0.29, "open once the key is loud");
        assert!(
            buf[(2_000 + lat - 5) * 2].abs() < 1e-6,
            "and not before: the key is held back by the limiter's look-ahead"
        );
        // Without its key the gate hears its own input and stays open.
        let mut p = ChainProcessor::new(&c, 48_000).unwrap();
        let mut buf = vec![0.3f32; frames * 2];
        p.process(&mut buf);
        assert!(buf[(1_500 + lat) * 2] > 0.29);
    }

    /// Controls a track or bus automation lane offers (the Editor's lane
    /// picker and the assistant use the same rule): not hidden, not an
    /// on/off or list control, and not one that moves the look-ahead.
    fn glides(p: &ParamInfo) -> bool {
        !p.hidden && !p.toggle && p.choices.is_empty() && !p.retimes
    }

    /// Effects and controls that cannot take a new value in place, with
    /// the reason. A moving value rebuilds them, held back while it moves
    /// (see `REBUILD_GAP_MS`). `"*"` stands for every control.
    const REBUILDS_ON_MOVE: &[(&str, &str)] = &[
        // The impulse length cuts the loaded response and re-partitions the
        // convolver, which allocates; it stays a held-back rebuild.
        ("altaudio.convolution_reverb", "length"),
    ];

    /// Every built-in effect takes every gliding control in place, keeping
    /// its state, apart from the explicit list above.
    #[test]
    fn every_gliding_control_updates_in_place() {
        let noise = seeded(4_800);
        let mut failed = Vec::new();
        let mut listed_but_fine = Vec::new();
        for info in ui_catalog() {
            for p in info.params.iter().filter(|p| glides(p)) {
                let Some(mut fx) = build(&slot(info.id, &[]), 48_000) else {
                    continue;
                };
                let latency = fx.latency();
                let mut warm = noise.clone();
                for c in warm.chunks_mut(2 * 256) {
                    fx.process(c);
                }
                // A small move, as one automation step makes.
                let nudge = ((p.max - p.min) * 0.01).max(1e-3);
                let v = if p.default + nudge <= p.max {
                    p.default + nudge
                } else {
                    p.default - nudge
                };
                let saved = [(p.key.to_string(), v)].into_iter().collect();
                let ok = fx.update(&Params::resolve(&info, &saved));
                let listed = REBUILDS_ON_MOVE
                    .iter()
                    .any(|(e, k)| *e == info.id && (*k == "*" || *k == p.key));
                match (ok, listed) {
                    (false, false) => failed.push(format!("{} {}", info.id, p.key)),
                    (true, true) => listed_but_fine.push(format!("{} {}", info.id, p.key)),
                    _ => {}
                }
                if ok {
                    assert_eq!(fx.latency(), latency, "{} {}", info.id, p.key);
                    let mut after = noise.clone();
                    for c in after.chunks_mut(2 * 256) {
                        fx.process(c);
                    }
                    assert!(
                        after.iter().all(|s| s.is_finite()),
                        "{} {} gave a non-finite sample",
                        info.id,
                        p.key
                    );
                }
            }
        }
        assert!(
            failed.is_empty(),
            "rebuilt on every automation step: {failed:#?}"
        );
        // Keep the list honest: drop an entry once its effect learns to update.
        assert!(
            listed_but_fine.is_empty(),
            "listed but update in place now: {listed_but_fine:#?}"
        );
    }

    /// A stage that cannot take a moving setting in place is rebuilt at
    /// most every `REBUILD_GAP_MS` while the setting moves, then once more
    /// when it settles, not on every step.
    #[test]
    fn rebuilds_for_a_moving_setting_are_held_back() {
        /// A stage that never takes a setting in place. A rebuild puts a
        /// fresh Gain in its place, which is how the test counts them.
        struct Stuck;
        impl Processor for Stuck {
            fn process(&mut self, _buf: &mut [f32]) {}
            fn reset(&mut self) {}
        }
        let mut c = EffectChain::default();
        let s = slot("altaudio.gain", &[]);
        let id = s.id;
        c.slots.push(s);
        let mut chain = ChainProcessor::new(&c, 48_000).unwrap();
        chain.stages[0] = Box::new(Stuck);
        let gain = info("altaudio.gain").unwrap();
        let mut rebuilds = 0;
        // 1 when a fresh Gain took the stuck stage's place, which is put back.
        let count = |chain: &mut ChainProcessor| {
            let fresh = chain.stages[0].update(&Params::resolve(&gain, &chain.slots[0].2));
            if fresh {
                chain.stages[0] = Box::new(Stuck);
            }
            usize::from(fresh)
        };
        let mut buf = vec![0.25f32; 2 * 256];
        // Most of a second of a ramp, one step per 256 frames as the engine sends.
        for step in 0..170 {
            chain.set_param(id, "gain", -12.0 + f64::from(step) * 0.05);
            rebuilds += count(&mut chain);
            chain.process(&mut buf);
            rebuilds += count(&mut chain);
        }
        let during = rebuilds;
        // The ramp stops; its last value comes in once it settles.
        for _ in 0..20 {
            chain.process(&mut buf);
            rebuilds += count(&mut chain);
        }
        // One at once, then one every 250 ms: never one per step.
        assert!((3..=5).contains(&during), "{during} rebuilds while moving");
        assert_eq!(rebuilds, during + 1, "one more once it settled");
        // And a lone jump after a quiet spell goes in at once.
        for _ in 0..100 {
            chain.process(&mut buf);
        }
        chain.set_param(id, "gain", 3.0);
        rebuilds += count(&mut chain);
        assert_eq!(rebuilds, during + 2);
    }

    #[test]
    fn dry_path_is_delayed_with_the_effects() {
        let mut c = EffectChain::default();
        c.slots.push(slot(
            "altaudio.limiter",
            &[("lookahead", 1.0), ("ceiling", 0.0)],
        ));
        c.mix = 0.0;
        let mut p = ChainProcessor::new(&c, 48_000).unwrap();
        let lat = p.latency();
        assert_eq!(lat, 48);
        let mut b = vec![0f32; 200 * 2];
        b[0] = 0.5;
        b[1] = 0.5;
        p.process(&mut b);
        // Fully dry, but the impulse arrives after the chain's latency.
        assert_eq!(b[lat * 2], 0.5);
        assert_eq!(b[0], 0.0);
    }
}
