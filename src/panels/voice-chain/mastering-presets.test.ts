// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) The GeekCast Radio Network, LLC.
// This file is part of Altered Audio Editor.

import { describe, expect, it } from "vitest";
import { addsOne, applyPreset, BUILTIN_PRESETS, EMPTY_CHAIN, MASTERING_BLURBS, MASTERING_PRESETS, REVERB_PRESETS } from "./chain";
import { presetMenu } from "./preset-menu";

const known = new Set(BUILTIN_PRESETS.flatMap((p) => p.chain.slots.map((s) => s.effect)));

describe("Mastering chain presets", () => {
  it("are in the built-in list, each described", () => {
    expect(MASTERING_PRESETS.length).toBeGreaterThanOrEqual(5);
    for (const p of MASTERING_PRESETS) {
      expect(BUILTIN_PRESETS).toContain(p);
      expect(p.group).toBe("Mastering");
      expect(MASTERING_BLURBS[p.name], p.name).toBeTruthy();
    }
  });

  it("replace the chain, even the one-slot Mastering Suite preset", () => {
    let n = 0;
    const id = () => `s${n++}`;
    const current = { ...EMPTY_CHAIN, slots: [{ id: "old", effect: "altaudio.gain", enabled: true, params: { gain: -3 }, preset: null }] };
    for (const p of MASTERING_PRESETS) {
      expect(addsOne(p), p.name).toBe(false);
      const next = applyPreset(current, p, known, id)!;
      expect(next.slots.some((s) => s.id === "old"), p.name).toBe(false);
      expect(next.slots.length).toBe(p.chain.slots.length);
    }
  });

  it("end with a true-peak limiter or the Mastering Suite, ceiling at or under -1 dB", () => {
    for (const p of MASTERING_PRESETS) {
      const last = p.chain.slots[p.chain.slots.length - 1]!;
      if (last.effect === "altaudio.limiter") {
        expect(last.params.true_peak, p.name).toBe(1);
        expect(last.params.ceiling, p.name).toBeLessThanOrEqual(-1);
      } else {
        expect(last.effect, p.name).toBe("altaudio.mastering");
        expect(last.params.ceiling, p.name).toBeLessThanOrEqual(-1);
        expect(last.params.l_on, p.name).toBe(1);
      }
    }
  });

  it("show in the Presets menu as their own group", () => {
    const menu = presetMenu([], BUILTIN_PRESETS, () => {});
    const sub = menu.find((m) => m.kind === "sub" && m.label.startsWith("Mastering"));
    expect(sub?.kind === "sub" && sub.items.length).toBe(MASTERING_PRESETS.length);
    const labels = menu.map((m) => (m.kind === "sub" ? m.label : ""));
    expect(labels).toContain("Reverb (add one effect)");
    expect(labels).toContain("Modulation (add one effect)");
  });
});

describe("Reverb presets", () => {
  it("each add one effect, with values inside Room Reverb's ranges", () => {
    const names = REVERB_PRESETS.map((p) => p.name);
    for (const want of ["Small Room", "Concert Hall", "Bright Plate", "Cathedral", "Drama: Stairwell"]) expect(names).toContain(want);
    for (const p of REVERB_PRESETS) {
      expect(addsOne(p), p.name).toBe(true);
      const s = p.chain.slots[0]!;
      if (s.effect !== "altaudio.room-reverb") continue;
      expect(s.params.size).toBeGreaterThanOrEqual(20);
      expect(s.params.size).toBeLessThanOrEqual(200);
      expect(s.params.decay).toBeGreaterThanOrEqual(0.2);
      expect(s.params.decay).toBeLessThanOrEqual(12);
      expect(s.params.mix).toBeLessThanOrEqual(100);
    }
  });
});
