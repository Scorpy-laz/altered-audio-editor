// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) The GeekCast Radio Network, LLC.
// This file is part of Altered Audio Editor.

/**
 * Voice Chain edits as plain functions: each returns a new chain, so the
 * panel sends one undoable `set_effects` per change. Also the built-in
 * presets and the slider maths.
 */

import type { EffectInfo, EffectParamInfo } from "../../app/core";
import type { EffectChainDoc, EffectSlotDoc } from "../../stores/session";

export const MAX_SLOTS = 16;

export const EMPTY_CHAIN: EffectChainDoc = {
  enabled: true,
  input_gain_db: 0,
  output_gain_db: 0,
  mix: 1,
  slots: [],
};

export function chainOf(c: EffectChainDoc | undefined | null): EffectChainDoc {
  return c
    ? { ...EMPTY_CHAIN, ...c, slots: c.slots ?? [] }
    : { ...EMPTY_CHAIN, slots: [] };
}

/** A new slot with every parameter at its default. */
export function newSlot(info: EffectInfo, id: string): EffectSlotDoc {
  return {
    id,
    effect: info.id,
    enabled: true,
    params: Object.fromEntries(info.params.map((p) => [p.key, p.default])),
    preset: null,
  };
}

/** Adds an effect at `index` (the end when missing). Null when the chain is full. */
export function addSlot(
  chain: EffectChainDoc,
  slot: EffectSlotDoc,
  index?: number,
): EffectChainDoc | null {
  if (chain.slots.length >= MAX_SLOTS) return null;
  const slots = [...chain.slots];
  slots.splice(
    index === undefined
      ? slots.length
      : Math.max(0, Math.min(index, slots.length)),
    0,
    slot,
  );
  return { ...chain, slots };
}

/**
 * The chain as its numbered slots: each effect in the slot it asked for
 * (its `position`), or the one after the effect before it. Effects run
 * in list order, so a slot never comes before an earlier effect's.
 */
export function layoutSlots(chain: EffectChainDoc): (EffectSlotDoc | null)[] {
  const out: (EffectSlotDoc | null)[] = Array.from({ length: MAX_SLOTS }, () => null);
  const n = chain.slots.length;
  let next = 0;
  chain.slots.forEach((s, k) => {
    // Leave room for the effects still to come.
    const latest = MAX_SLOTS - (n - k);
    const at = Math.min(latest, Math.max(next, s.position ?? next));
    out[at] = s;
    next = at + 1;
  });
  return out;
}

/** How many effects show before numbered slot `at`, leaving out `skip`. */
function countBefore(chain: EffectChainDoc, at: number, skip?: string): number {
  return layoutSlots(chain)
    .slice(0, at)
    .filter((s) => s && s.id !== skip).length;
}

/**
 * Puts an effect in numbered slot `at`. An effect already there moves
 * down one. Null when the chain is full.
 */
export function addSlotAt(chain: EffectChainDoc, slot: EffectSlotDoc, at: number): EffectChainDoc | null {
  if (chain.slots.length >= MAX_SLOTS) return null;
  const to = Math.max(0, Math.min(MAX_SLOTS - 1, at));
  const slots = pinned(chain);
  slots.splice(countBefore(chain, to), 0, { ...slot, position: to });
  return { ...chain, slots: bumped(slots) };
}

/** Moves an effect to numbered slot `at`; an effect already there moves down one. */
export function moveSlotToIndex(chain: EffectChainDoc, id: string, at: number): EffectChainDoc {
  const s = chain.slots.find((x) => x.id === id);
  if (!s) return chain;
  const to = Math.max(0, Math.min(MAX_SLOTS - 1, at));
  const before = countBefore(chain, to, id);
  const slots = pinned(chain).filter((x) => x.id !== id);
  slots.splice(before, 0, { ...s, position: to });
  return { ...chain, slots: bumped(slots) };
}

/** Every effect pinned to the slot it shows in now, so moving one leaves the rest in place. */
function pinned(chain: EffectChainDoc): EffectSlotDoc[] {
  const where = new Map(layoutSlots(chain).flatMap((s, i) => (s ? [[s.id, i] as const] : [])));
  return chain.slots.map((s) => ({ ...s, position: where.get(s.id) ?? s.position ?? null }));
}

/** Pushes effects down where one now sits on or above the one before. */
function bumped(slots: EffectSlotDoc[]): EffectSlotDoc[] {
  let next = 0;
  return slots.map((s) => {
    const at = Math.max(next, s.position ?? next);
    next = at + 1;
    return at === s.position ? s : { ...s, position: at };
  });
}

export function removeSlot(chain: EffectChainDoc, id: string): EffectChainDoc {
  return { ...chain, slots: chain.slots.filter((s) => s.id !== id) };
}

/** Moves a slot up (-1) or down (+1). Unchanged at either end. */
export function moveSlot(
  chain: EffectChainDoc,
  id: string,
  by: number,
): EffectChainDoc {
  const i = chain.slots.findIndex((s) => s.id === id);
  const j = i + by;
  if (i < 0 || j < 0 || j >= chain.slots.length) return chain;
  const slots = [...chain.slots];
  const [s] = slots.splice(i, 1);
  slots.splice(j, 0, s!);
  return { ...chain, slots };
}

/** Moves a slot to `to` (an index in the current list). Unchanged when it is already there. */
export function moveSlotTo(
  chain: EffectChainDoc,
  id: string,
  to: number,
): EffectChainDoc {
  const i = chain.slots.findIndex((s) => s.id === id);
  const j = Math.max(0, Math.min(to, chain.slots.length - 1));
  if (i < 0 || i === j) return chain;
  const slots = [...chain.slots];
  const [s] = slots.splice(i, 1);
  slots.splice(j, 0, s!);
  return { ...chain, slots };
}

/** Puts `slot` where the slot `id` is, keeping its place in the list. */
export function replaceSlot(
  chain: EffectChainDoc,
  id: string,
  slot: EffectSlotDoc,
): EffectChainDoc {
  return { ...chain, slots: chain.slots.map((s) => (s.id === id ? slot : s)) };
}

export function setSlot(
  chain: EffectChainDoc,
  id: string,
  change: Partial<EffectSlotDoc>,
): EffectChainDoc {
  return {
    ...chain,
    slots: chain.slots.map((s) => (s.id === id ? { ...s, ...change } : s)),
  };
}

/** Sets one parameter, snapped to its step and range. Clears the preset name. */
export function setParam(
  chain: EffectChainDoc,
  id: string,
  p: EffectParamInfo,
  value: number,
): EffectChainDoc {
  const v = clampParam(p, value);
  return {
    ...chain,
    slots: chain.slots.map((s) =>
      s.id === id
        ? { ...s, params: { ...s.params, [p.key]: v }, preset: null }
        : s,
    ),
  };
}

export function clampParam(p: EffectParamInfo, value: number): number {
  if (!Number.isFinite(value)) return p.default;
  const snapped = p.step > 0 ? Math.round(value / p.step) * p.step : value;
  // Trim float noise from the step maths (0.30000000000000004).
  return Math.min(p.max, Math.max(p.min, Number(snapped.toFixed(6))));
}

export function paramValue(slot: EffectSlotDoc, p: EffectParamInfo): number {
  const v = slot.params[p.key];
  return typeof v === "number" && Number.isFinite(v)
    ? clampParam(p, v)
    : p.default;
}

/** Slider position 0..1 for a value, logarithmic where the control asks for it. */
export function toSlider(p: EffectParamInfo, v: number): number {
  if (p.max <= p.min) return 0;
  if (p.log) return Math.log(v / p.min) / Math.log(p.max / p.min);
  return (v - p.min) / (p.max - p.min);
}

export function fromSlider(p: EffectParamInfo, t: number): number {
  const x = Math.min(1, Math.max(0, t));
  const v = p.log
    ? p.min * Math.pow(p.max / p.min, x)
    : p.min + x * (p.max - p.min);
  return clampParam(p, v);
}

/** A value as the control shows it: "-6.0 dB", "4.0:1", "1.2 kHz", "On". */
export function formatParam(p: EffectParamInfo, v: number): string {
  if (p.toggle) return v >= 0.5 ? "On" : "Off";
  if (p.choices?.length) return p.choices[Math.round(v - p.min)] ?? String(v);
  if (p.unit === "Hz" && v >= 1000)
    return `${(v / 1000).toFixed(v >= 10_000 ? 1 : 2)} kHz`;
  const digits = p.step >= 1 ? 0 : p.step >= 0.1 ? 1 : 2;
  const text = v.toFixed(digits);
  if (!p.unit) return text;
  return p.unit.startsWith(":") ? `${text}${p.unit}` : `${text} ${p.unit}`;
}

/** Badge colors by group, so the slot list reads at a glance. */
export const CATEGORY_COLORS: Record<string, string> = {
  "Filter and EQ": "#4f9fd8",
  Dynamics: "#d8964f",
  Amplitude: "#8bc06a",
  Restoration: "#b07ad8",
  "Time and Pitch": "#d86f9b",
  Modulation: "#5fbfb0",
  "Stereo Imagery": "#5f8fd8",
  Special: "#d8c14f",
  "Delay and Reverb": "#9d7cf0",
  Vocal: "#ec6a92",
};

// --- Presets ---

export interface ChainPreset {
  name: string;
  /** "Voice chains" replace the chain; the single-effect groups add one slot. */
  group: string;
  /** A full per-voice chain for a host's track: offered in the Voice Chain only, not in Batch Convert. */
  trackOnly?: boolean;
  chain: Omit<EffectChainDoc, "slots"> & {
    slots: { effect: string; params: Record<string, number> }[];
  };
}

/** Where a built-in voice chain sits in the Presets menu, and what it is for. */
export const PRESET_INFO: Record<string, { category: "Podcast Voices" | "Character Types" | "Cleanup" | "Voice Over"; blurb: string }> = {
  "Podcast Voice": { category: "Podcast Voices", blurb: "All-round: noise, level, compress, EQ, de-ess, limit -6 dB" },
  "Podcast Voice - Host": { category: "Podcast Voices", blurb: "The host's chain: leveler, bright voice EQ, limit -6 dB, gate -14 dB" },
  "Podcast Voice - Co-Host": { category: "Podcast Voices", blurb: "The co-host's chain: same as Host, gate -20 dB" },
  "Podcast Voice - Guest": { category: "Podcast Voices", blurb: "Noisier rooms: heavier noise cut and squeeze, gate -45 dB" },
  "Podcast Voice - Rumble Cut": { category: "Podcast Voices", blurb: "Podcast Voice with a steep 100 Hz low cut first" },
  "Podcast Voice - Quick Level": { category: "Podcast Voices", blurb: "Leveler and limit -6 dB only, for clean audio" },
  "Podcast Voice 3": { category: "Podcast Voices", blurb: "Clean takes: light compress, EQ, limit -6 dB" },
  "Podcast Voice 3 -3 dB": { category: "Podcast Voices", blurb: "Same as Voice 3, limit -3 dB" },
  "Podcast Voice 4 -3 dB": { category: "Podcast Voices", blurb: "Uneven speakers: leveler, firmer squeeze, limit -3 dB" },
  Godlike: { category: "Character Types", blurb: "Pitched down 3 st, big low end" },
  Demonic: { category: "Character Types", blurb: "Pitched down 8 st, heavy" },
  "Evil Voice": { category: "Character Types", blurb: "Pitched down 5 st, dark" },
  Ethereal: { category: "Character Types", blurb: "Pitched up 2 st, airy" },
  Angelic: { category: "Character Types", blurb: "Pitched up 3 st, bright" },
  "Voice Leveler and Noise Gate": { category: "Cleanup", blurb: "Evens levels, gate -40 dB" },
  "Clean Up and Level Voice Over": { category: "Cleanup", blurb: "Noise cut, level, de-ess, limit -3 dB" },
  "Remote Guest Cleanup": { category: "Cleanup", blurb: "Calls and remote guests: strong noise and room cut" },
  "Radio Announcer Voice": { category: "Voice Over", blurb: "Hard compression, limit -1 dB, big and close" },
  "Voice Over": { category: "Voice Over", blurb: "Compress, de-ess, limit -3 dB" },
  "DeEss and Limit Male Voice Over": { category: "Voice Over", blurb: "De-ess at 5.5 kHz, limit -3 dB" },
  "DeEss and Limit Female Voice Over": { category: "Voice Over", blurb: "De-ess at 7.5 kHz, limit -3 dB" },
  "Breath Gate": { category: "Cleanup", blurb: "Gate -40 dB" },
  "Breath Gate - Low": { category: "Cleanup", blurb: "Gate -14 dB: close mic, strong voice" },
  "Breath Gate - Medium": { category: "Cleanup", blurb: "Gate -20 dB: normal mic distance" },
};

/** True when choosing the preset adds one effect instead of replacing the chain. */
export function addsOne(p: ChainPreset): boolean {
  return p.chain.slots.length === 1 && p.group !== "Voice chains" && p.group !== "Mastering";
}

const base = { enabled: true, input_gain_db: 0, output_gain_db: 0, mix: 1 };
const fx = (effect: string, params: Record<string, number> = {}) => ({
  effect: `altaudio.${effect}`,
  params,
});
const chainP = (
  name: string,
  group: string,
  slots: ReturnType<typeof fx>[],
  extra: Partial<EffectChainDoc> = {},
): ChainPreset => ({
  name,
  group,
  chain: { ...base, ...extra, slots },
});
const trackOnly = (p: ChainPreset): ChainPreset => ({ ...p, trackOnly: true });

/** Presets Batch Convert offers: everything but the per-voice track chains. */
export function batchPresets(list: readonly ChainPreset[]): ChainPreset[] {
  return list.filter((p) => !p.trackOnly);
}

// Our own starting points, tuned by ear on spoken word. The order follows
// the podcast chain: level, compress, shape, de-ess, limit, then gate.
const voiceEq = fx("parametric_eq", {
  hp_on: 1,
  hp_freq: 80,
  hp_slope: 1,
  b1_freq: 300,
  b1_gain: -2,
  b1_q: 1.2,
  b3_freq: 4000,
  b3_gain: 2,
  b3_q: 0.9,
});
const leveler = fx("speech_leveler", {
  target: -20,
  max_gain: 8,
  max_cut: 8,
  speed: 2000,
  floor: -50,
});
const comp = (threshold: number, ratio: number, makeup: number) =>
  fx("dynamics", {
    threshold,
    ratio,
    attack: 8,
    release: 160,
    knee: 6,
    makeup,
  });
const limit = (ceiling: number) =>
  fx("limiter", { ceiling, lookahead: 5, release: 100 });
const cleanNoise = (reduction: number) =>
  fx("noise_reducer", { reduction, strength: 2, learn: 2, smoothing: 50 });
const gate = (threshold: number) =>
  fx("breath_gate", {
    threshold,
    attack: 2,
    hold: 50,
    release: 200,
    range: -30,
  });

/** The building blocks the built-in chains are made of, for guided panels that write plain effects. */
export const BLOCKS = { fx, voiceEq, leveler, comp, limit, cleanNoise, gate };

/**
 * The host chains: the leveler and EQ the project's own hosts use, a -6 dB limiter with 3 dB of
 * boost, and a gate whose threshold is the one thing that differs between mics.
 */
const hostChain = (gateThreshold: number) => [
  cleanNoise(10),
  fx("speech_leveler", {
    target: -19.2,
    amount: 50,
    range: 45,
    boost_low: 1,
    comp_on: 0,
    comp_thr: -1,
    gate_on: 0,
    gate_off: 15,
    max_gain: 8,
    max_cut: 8,
    speed: 2000,
    floor: -50,
  }),
  fx("parametric_eq", {
    hp_on: 1,
    hp_freq: 80,
    hp_slope: 1,
    ls_freq: 110,
    ls_gain: 6,
    b1_freq: 291,
    b1_gain: -3,
    b1_q: 1.5,
    hs_freq: 17458,
    hs_gain: 13.9,
  }),
  fx("limiter", { ceiling: -6, input_boost: 3, lookahead: 7, release: 100 }),
  fx("breath_gate", { threshold: gateThreshold, attack: 2, hold: 50, release: 200, range: -30 }),
];

// --- Reverb, modulation and mastering presets ---

/** A Room Reverb preset: size, decay, pre-delay, damping, width and mix, all listed. */
const room = (size: number, decay: number, predelay: number, damping: number, width: number, mix: number) =>
  fx("room-reverb", { size, decay, predelay, damping, width, mix });

/**
 * Spaces for Room Reverb, from small rooms to a cathedral, plus places for scenes in
 * spoken-word drama. Each adds one effect. Our own settings, tuned by ear.
 */
export const REVERB_PRESETS: ChainPreset[] = [
  chainP("Small Room", "Reverb", [room(45, 0.5, 4, 55, 70, 18)]),
  chainP("Medium Room", "Reverb", [room(75, 0.9, 10, 45, 90, 22)]),
  chainP("Large Room", "Reverb", [room(110, 1.6, 18, 40, 100, 25)]),
  chainP("Concert Hall", "Reverb", [room(160, 2.8, 30, 35, 100, 28)]),
  chainP("Bright Plate", "Reverb", [room(95, 2.0, 0, 10, 100, 24)]),
  chainP("Cathedral", "Reverb", [room(200, 7.5, 45, 30, 100, 35)]),
  chainP("Drama: Kitchen", "Reverb", [room(50, 0.6, 3, 20, 80, 20)]),
  chainP("Drama: Bathroom", "Reverb", [room(35, 1.1, 2, 5, 60, 30)]),
  chainP("Drama: Stairwell", "Reverb", [room(120, 3.2, 12, 15, 90, 30)]),
  chainP("Drama: Inside a Car", "Reverb", [room(22, 0.25, 1, 70, 40, 15)]),
  chainP("Drama: Cave", "Reverb", [room(180, 5.0, 60, 50, 100, 40)]),
  chainP("Drama: Open Field", "Reverb", [room(200, 0.4, 70, 85, 100, 8)]),
  chainP("Drama: Tape Echo Dream", "Reverb", [
    fx("tape-delay", { time: 420, feedback: 55, tone: 2600, saturation: 40, wobble: 45, offset: 112, mix: 32 }),
  ]),
];

/** Chorus, flanger, phaser and widening, each adding one effect. */
const MODULATION_PRESETS: ChainPreset[] = [
  chainP("Thicken Voice", "Modulation", [fx("chorus", { voices: 3, delay: 18, depth: 25, rate: 0.6, spread: 70, mix: 30 })]),
  chainP("Ensemble", "Modulation", [fx("chorus", { voices: 6, delay: 25, depth: 45, rate: 0.9, spread: 100, mix: 45 })]),
  chainP("Jet Sweep", "Modulation", [fx("flanger", { delay: 2, depth: 80, rate: 0.15, feedback: 70, phase: 90, mix: 50 })]),
  chainP("Robot Metal", "Modulation", [fx("flanger", { delay: 0.5, depth: 5, rate: 0.05, feedback: 90, phase: 0, mix: 50 })]),
  chainP("Slow Swirl", "Modulation", [fx("phaser", { stages: 6, center: 700, depth: 70, rate: 0.2 })]),
  chainP("Wide Stereo", "Modulation", [fx("stereo-expander", { width: 160, center: 0, bass: 150 })]),
];

/** A Mastering Suite preset: every setting listed, so one fully replaces another. */
export const masterSuite = (p: Record<string, number>): Record<string, number> => ({
  lc_on: 0, lc_freq: 30,
  ls_freq: 100, ls_gain: 0,
  pk_freq: 1000, pk_gain: 0, pk_q: 1,
  hs_freq: 8000, hs_gain: 0,
  rv_amount: 0,
  ex_amount: 0, ex_mode: 0,
  width: 100,
  l_on: 1, push: 0, ceiling: -1, release: 80,
  output: 0,
  ...p,
});

/** A limiter that keeps true peaks under `ceiling`, after `boost` dB of push. */
const tpLimit = (ceiling: number, boost: number, release = 80) =>
  fx("limiter", { ceiling, input_boost: boost, lookahead: 5, release, linked: 1, true_peak: 1 });
/** A multiband compressor: one threshold and ratio for all three bands, and the splits. */
const bands = (threshold: number, ratio: number, attack = 20, release = 200) =>
  fx("multiband", {
    low_freq: 180, high_freq: 4000,
    low_threshold: threshold - 2, low_ratio: ratio, low_makeup: 0,
    mid_threshold: threshold, mid_ratio: ratio, mid_makeup: 0,
    high_threshold: threshold - 2, high_ratio: ratio, high_makeup: 0,
    attack, release, output: 0,
  });
/** A tone EQ for mastering: low cut, a low shelf, a mud cut and a high shelf. */
const masterEq = (hp: number, low: number, mud: number, air: number) =>
  fx("parametric_eq", {
    hp_on: hp > 0 ? 1 : 0, hp_freq: Math.max(hp, 20), hp_slope: 1,
    ls_freq: 100, ls_gain: low,
    b1_freq: 300, b1_gain: mud, b1_q: 1.2,
    b2_freq: 1000, b2_gain: 0, b2_q: 1,
    b3_freq: 4000, b3_gain: 0, b3_q: 1,
    hs_freq: 10_000, hs_gain: air,
    lp_on: 0, lp_freq: 18_000, lp_slope: 1,
  });
const warmth = (drive: number) => fx("tube_preamp", { input: 0, drive, low: 0, high: 0, transformer: 1, output: 0, mix: 100 });

/**
 * Mastering chains for the master (or a whole mix): tone EQ, multiband compression, warmth,
 * then a true-peak limiter. They replace the chain. The loudness in a name is what the chain
 * aims for with a typical mix going in; check the result with File > Check Loudness.
 */
export const MASTERING_PRESETS: ChainPreset[] = [
  chainP("Master: Podcast, -16 LUFS Target", "Mastering", [masterEq(60, 1, -1.5, 1.5), bands(-24, 2), warmth(10), tpLimit(-1, 6)]),
  chainP("Master: Audiobook", "Mastering", [masterEq(80, 0, -1.5, 1), bands(-26, 1.8, 25, 250), tpLimit(-3, 4, 120)]),
  chainP("Master: Music Loud", "Mastering", [
    masterEq(30, 2, -1, 2.5),
    bands(-22, 3, 15, 150),
    warmth(20),
    fx("stereo-expander", { width: 120, center: 0, bass: 150 }),
    tpLimit(-1, 10, 50),
  ]),
  chainP("Master: Gentle Polish", "Mastering", [masterEq(25, 1, 0, 1.5), bands(-18, 1.5, 30, 250), tpLimit(-1, 2, 120)]),
  chainP("Master: Warm Tape", "Mastering", [masterEq(30, 2, 0, -1.5), warmth(45), bands(-20, 2), tpLimit(-1, 4, 100)]),
  chainP("Master: Mastering Suite, Podcast", "Mastering", [
    fx("mastering", masterSuite({ lc_on: 1, lc_freq: 70, ls_gain: 1, pk_freq: 300, pk_gain: -1.5, hs_freq: 9000, hs_gain: 2, ex_amount: 15, push: 6 })),
  ]),
];

/** What each mastering chain is for, shown beside it in menus. */
export const MASTERING_BLURBS: Record<string, string> = {
  "Master: Podcast, -16 LUFS Target": "EQ, multiband, light warmth, limit -1 dBTP with 6 dB push",
  "Master: Audiobook": "Clean EQ, gentle multiband, limit -3 dBTP",
  "Master: Music Loud": "Fuller EQ, firm multiband, warmth, wider, limit -1 dBTP with 10 dB push",
  "Master: Gentle Polish": "A little tone and glue, limit -1 dBTP",
  "Master: Warm Tape": "Rounder top, tube warmth, glue, limit -1 dBTP",
  "Master: Mastering Suite, Podcast": "The Mastering Suite in one slot, set for speech",
};

export const BUILTIN_PRESETS: ChainPreset[] = [
  trackOnly(
    chainP("Podcast Voice", "Voice chains", [
      cleanNoise(10),
      leveler,
      comp(-22, 3, 4),
      voiceEq,
      fx("deesser", { freq: 6500, threshold: -32, reduction: 6 }),
      limit(-6),
    ]),
  ),
  trackOnly(
    chainP("Podcast Voice - Guest", "Voice chains", [
      cleanNoise(12),
      leveler,
      comp(-24, 4, 6),
      voiceEq,
      fx("deesser", { freq: 6500, threshold: -30, reduction: 8 }),
      limit(-6),
      gate(-45),
    ]),
  ),
  trackOnly(chainP("Podcast Voice - Host", "Voice chains", hostChain(-14))),
  trackOnly(chainP("Podcast Voice - Co-Host", "Voice chains", hostChain(-20))),
  trackOnly(
    chainP("Podcast Voice - Rumble Cut", "Voice chains", [
      fx("precision_filter", { design: 0, mode: 1, order: 3, freq: 100 }),
      cleanNoise(10),
      leveler,
      comp(-22, 3, 4),
      voiceEq,
      fx("deesser", { freq: 6500, threshold: -32, reduction: 6 }),
      limit(-6),
    ]),
  ),
  trackOnly(chainP("Podcast Voice - Quick Level", "Voice chains", [leveler, limit(-6)])),
  trackOnly(
    chainP("Podcast Voice 3", "Voice chains", [
      comp(-20, 2.5, 3),
      voiceEq,
      limit(-6),
    ]),
  ),
  trackOnly(
    chainP("Podcast Voice 3 -3 dB", "Voice chains", [
      comp(-20, 2.5, 3),
      voiceEq,
      limit(-3),
    ]),
  ),
  trackOnly(
    chainP("Podcast Voice 4 -3 dB", "Voice chains", [
      cleanNoise(10),
      leveler,
      comp(-26, 3.5, 6),
      voiceEq,
      fx("deesser", { freq: 6000, threshold: -32, reduction: 6 }),
      limit(-3),
    ]),
  ),
  // Character voices: the same chain order as a clean voice, plus a pitch move
  // and a tone. Starting points tuned by ear. A reverb stage joins them once
  // we have a reverb effect.
  trackOnly(
    chainP("Godlike", "Voice chains", [
      cleanNoise(10),
      leveler,
      fx("parametric_eq", {
        hp_on: 1,
        hp_freq: 60,
        hp_slope: 1,
        ls_freq: 140,
        ls_gain: 5,
        b2_freq: 2500,
        b2_gain: 2,
        b2_q: 0.9,
      }),
      comp(-24, 3, 5),
      fx("pitch_shift", { semitones: -3, cents: 0 }),
      limit(-3),
    ]),
  ),
  trackOnly(
    chainP("Demonic", "Voice chains", [
      cleanNoise(10),
      leveler,
      fx("parametric_eq", {
        hp_on: 1,
        hp_freq: 70,
        hp_slope: 1,
        ls_freq: 120,
        ls_gain: 6,
        b2_freq: 1200,
        b2_gain: -3,
        b2_q: 1,
        hs_freq: 6000,
        hs_gain: -3,
      }),
      comp(-26, 4, 6),
      fx("pitch_shift", { semitones: -8, cents: 0 }),
      limit(-3),
    ]),
  ),
  trackOnly(
    chainP("Evil Voice", "Voice chains", [
      cleanNoise(10),
      leveler,
      fx("parametric_eq", {
        hp_on: 1,
        hp_freq: 70,
        hp_slope: 1,
        ls_freq: 150,
        ls_gain: 4,
        b3_freq: 3200,
        b3_gain: 3,
        b3_q: 1.2,
      }),
      comp(-26, 4, 6),
      fx("pitch_shift", { semitones: -5, cents: 0 }),
      limit(-3),
    ]),
  ),
  trackOnly(
    chainP("Ethereal", "Voice chains", [
      cleanNoise(10),
      leveler,
      fx("parametric_eq", {
        hp_on: 1,
        hp_freq: 120,
        hp_slope: 1,
        b2_freq: 400,
        b2_gain: -2,
        b2_q: 1,
        hs_freq: 9000,
        hs_gain: 4,
      }),
      fx("pitch_shift", { semitones: 2, cents: 0 }),
      limit(-3),
    ]),
  ),
  trackOnly(
    chainP("Angelic", "Voice chains", [
      cleanNoise(10),
      leveler,
      fx("parametric_eq", {
        hp_on: 1,
        hp_freq: 100,
        hp_slope: 1,
        b3_freq: 4500,
        b3_gain: 3,
        b3_q: 0.9,
        hs_freq: 10_000,
        hs_gain: 3,
      }),
      fx("pitch_shift", { semitones: 3, cents: 0 }),
      limit(-3),
    ]),
  ),
  chainP("Voice Leveler and Noise Gate", "Voice chains", [leveler, gate(-40)]),
  chainP("Clean Up and Level Voice Over", "Voice chains", [
    cleanNoise(8),
    fx("click_repair", { sensitivity: 40, width: 1 }),
    fx("parametric_eq", { hp_on: 1, hp_freq: 70, hp_slope: 1 }),
    leveler,
    fx("deesser", { freq: 6500, threshold: -30, reduction: 6 }),
    limit(-3),
  ]),
  trackOnly(
    chainP("Radio Announcer Voice", "Voice chains", [
      comp(-28, 6, 9),
      fx("parametric_eq", {
        hp_on: 1,
        hp_freq: 60,
        ls_freq: 120,
        ls_gain: 4,
        b2_freq: 500,
        b2_gain: -2,
        b3_freq: 3500,
        b3_gain: 3,
        hs_freq: 10_000,
        hs_gain: 2,
      }),
      limit(-1),
    ]),
  ),
  trackOnly(
    chainP("Voice Over", "Voice chains", [
      fx("parametric_eq", { hp_on: 1, hp_freq: 70, hp_slope: 1 }),
      comp(-20, 2.5, 3),
      fx("deesser", { freq: 6500, threshold: -30, reduction: 6 }),
      limit(-3),
    ]),
  ),
  chainP("DeEss and Limit Male Voice Over", "Voice chains", [
    fx("deesser", { freq: 5500, threshold: -30, reduction: 8 }),
    limit(-3),
  ]),
  chainP("DeEss and Limit Female Voice Over", "Voice chains", [
    fx("deesser", { freq: 7500, threshold: -30, reduction: 8 }),
    limit(-3),
  ]),
  trackOnly(
    chainP("Remote Guest Cleanup", "Voice chains", [
      cleanNoise(18),
      fx("room_reducer", { amount: 50, decay: 0.6, reduction: 12 }),
      leveler,
      comp(-24, 4, 6),
      fx("parametric_eq", {
        hp_on: 1,
        hp_freq: 120,
        hp_slope: 2,
        b3_freq: 3000,
        b3_gain: 2,
      }),
      limit(-6),
      gate(-30),
    ]),
  ),
  chainP("Breath Gate", "Breath gates", [gate(-40)]),
  chainP("Breath Gate - Low", "Breath gates", [gate(-14)]),
  chainP("Breath Gate - Medium", "Breath gates", [gate(-20)]),
  chainP("Hard Limit to -0.1 dB", "Processing", [
    fx("limiter", {
      ceiling: -0.1,
      input_boost: 0,
      lookahead: 5,
      release: 100,
    }),
  ]),
  chainP("Voice-Over Compressor", "Processing", [
    fx("dynamics", {
      threshold: -24,
      ratio: 4,
      attack: 5,
      release: 120,
      knee: 6,
      makeup: 6,
    }),
  ]),
  chainP("Raise Pitch", "Processing", [
    fx("pitch_shift", { semitones: 2, cents: 0 }),
  ]),
  chainP("Lower Pitch", "Processing", [
    fx("pitch_shift", { semitones: -2, cents: 0 }),
  ]),
  chainP("Slapback Echo", "Delay", [
    fx("echo", { time: 110, feedback: 8, tone: 5000, wet: 28, pingpong: 0 }),
  ]),
  chainP("Podcast Room Echo", "Delay", [
    fx("echo", { time: 240, feedback: 25, tone: 4500, wet: 18, pingpong: 0 }),
  ]),
  chainP("Ping-Pong Echo", "Delay", [
    fx("echo", { time: 375, feedback: 40, tone: 6000, wet: 30, pingpong: 1 }),
  ]),
  chainP("Long Dark Echo", "Delay", [
    fx("echo", { time: 700, feedback: 55, tone: 2800, wet: 35, pingpong: 0 }),
  ]),
  chainP("Remove Vocals", "Processing", [
    fx("center_remover", { amount: 100, keep_below: 150 }),
  ]),
  chainP("Remove Clicks", "Repair", [
    fx("click_repair", { sensitivity: 50, width: 1 }),
  ]),
  chainP("Repair Clipping", "Repair", [
    fx("clip_repair", { level: -0.5, longest: 3, output: -3 }),
  ]),
  chainP("Reduce Hiss", "Repair", [fx("hiss_reducer", {})]),
  chainP("Reduce Room Echo", "Repair", [fx("room_reducer", {})]),
  chainP("Fix DC Offset", "Repair", [fx("dc_offset", {})]),
  chainP("Hum Cut 60 Hz (US)", "EQ", [
    fx("hum_cut", { mains: 0, harmonics: 4, q: 25 }),
  ]),
  chainP("Hum Cut 50 Hz (Europe)", "EQ", [
    fx("hum_cut", { mains: 1, harmonics: 4, q: 25 }),
  ]),
  chainP("Rumble Cut", "EQ", [
    fx("parametric_eq", { hp_on: 1, hp_freq: 60, hp_slope: 1 }),
  ]),
  chainP("Subsonic Cut", "EQ", [
    fx("parametric_eq", { hp_on: 1, hp_freq: 25, hp_slope: 2 }),
  ]),
  chainP("Telephone - Receiver", "EQ", [
    fx("parametric_eq", {
      hp_on: 1,
      hp_freq: 300,
      hp_slope: 2,
      lp_on: 1,
      lp_freq: 3400,
      lp_slope: 2,
      b2_freq: 1500,
      b2_gain: 3,
      b2_q: 0.8,
    }),
  ]),
  chainP(
    "Telephone - Voice Mail",
    "EQ",
    [
      fx("parametric_eq", {
        hp_on: 1,
        hp_freq: 400,
        hp_slope: 2,
        lp_on: 1,
        lp_freq: 3000,
        lp_slope: 2,
        b2_freq: 1800,
        b2_gain: 5,
        b2_q: 0.7,
      }),
      comp(-24, 4, 6),
    ],
    {},
  ),
  ...REVERB_PRESETS,
  ...MODULATION_PRESETS,
  ...MASTERING_PRESETS,
];

/** The chain a preset gives, with fresh slot ids. Unknown effects are dropped. */
export function presetChain(
  p: ChainPreset,
  known: ReadonlySet<string>,
  newId: () => string,
): EffectChainDoc {
  return {
    ...p.chain,
    slots: p.chain.slots
      .filter((s) => known.has(s.effect))
      .map((s) => ({
        id: newId(),
        effect: s.effect,
        enabled: true,
        params: { ...s.params },
        preset: p.name,
      })),
  };
}

/** What choosing a preset does to the current chain. Null when it would overflow. */
export function applyPreset(
  current: EffectChainDoc,
  p: ChainPreset,
  known: ReadonlySet<string>,
  newId: () => string,
): EffectChainDoc | null {
  const next = presetChain(p, known, newId);
  if (!addsOne(p)) return next;
  const slot = next.slots[0];
  return slot ? addSlot(current, slot) : current;
}

/** A user preset saved from the current chain (slot ids are not kept). */
export function presetFrom(name: string, chain: EffectChainDoc): ChainPreset {
  return {
    name,
    group: "Voice chains",
    chain: {
      ...chain,
      slots: chain.slots.map((s) => ({
        effect: s.effect,
        params: { ...s.params },
      })),
    },
  };
}

/** Saved user presets, kept only when well formed. */
export function sanitizePresets(raw: unknown): ChainPreset[] {
  if (!Array.isArray(raw)) return [];
  const out: ChainPreset[] = [];
  for (const r of raw) {
    if (!r || typeof r !== "object") continue;
    const v = r as Record<string, unknown>;
    const c = v.chain as Record<string, unknown> | undefined;
    if (
      typeof v.name !== "string" ||
      !v.name.trim() ||
      !c ||
      !Array.isArray(c.slots)
    )
      continue;
    const num = (x: unknown, d: number) =>
      typeof x === "number" && Number.isFinite(x) ? x : d;
    out.push({
      name: v.name,
      group: "Voice chains",
      chain: {
        enabled: c.enabled !== false,
        input_gain_db: num(c.input_gain_db, 0),
        output_gain_db: num(c.output_gain_db, 0),
        mix: num(c.mix, 1),
        slots: (c.slots as unknown[])
          .filter(
            (s): s is { effect: string; params: Record<string, number> } =>
              !!s && typeof (s as { effect?: unknown }).effect === "string",
          )
          .slice(0, MAX_SLOTS)
          .map((s) => ({
            effect: s.effect,
            params:
              typeof s.params === "object" && s.params ? { ...s.params } : {},
          })),
      },
    });
  }
  return out;
}

/** A plug-in as the Add effect menu needs it. */
export interface MenuPlugin {
  effect: string;
  name: string;
  vendor: string;
  /** VST3 categories, for example "Fx|Reverb". */
  kind: string;
}

/**
 * Plug-ins grouped for the Add effect menu: by type (the first category
 * after "Fx", "Other" when there is none) and by maker, each sorted by name.
 */
export function pluginMenuGroups(plugins: readonly MenuPlugin[]): { types: [string, MenuPlugin[]][]; makers: [string, MenuPlugin[]][] } {
  const group = (key: (p: MenuPlugin) => string) => {
    const m = new Map<string, MenuPlugin[]>();
    for (const p of plugins) {
      const k = key(p);
      m.set(k, [...(m.get(k) ?? []), p]);
    }
    return [...m.entries()]
      .sort(([a], [b]) => (a === "Other" ? 1 : b === "Other" ? -1 : a.localeCompare(b, undefined, { sensitivity: "base" })))
      .map(([k, list]): [string, MenuPlugin[]] => [k, [...list].sort((a, b) => a.name.localeCompare(b.name, undefined, { numeric: true, sensitivity: "base" }))]);
  };
  const type = (p: MenuPlugin) => {
    const parts = p.kind
      .split("|")
      .map((s) => s.trim())
      // "Fx" and "Instrument" say what every plug-in in that list is anyway.
      .filter((s) => s && s.toLowerCase() !== "fx" && s.toLowerCase() !== "instrument");
    return parts[0] ?? "Other";
  };
  return { types: group(type), makers: group((p) => p.vendor.trim() || "Other") };
}
