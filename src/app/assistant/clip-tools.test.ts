// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) The GeekCast Radio Network, LLC.
// This file is part of Altered Audio Editor.

import { describe, expect, it } from "vitest";
import { clone, RATE, setup } from "./harness";
import { colorName, colorValue, mergePoints, removeRange } from "./clip-tools";
import type { ClipDoc, EnvelopePointDoc, Op } from "../../stores/session";

const host = { kind: "track", id: "Host" };
const c1 = { kind: "clip", id: "c1" };

/** A session ready for these tests; `ops` are the person's own edits, made before (not in History). */
async function ready(ops: Op[] = []) {
  const s = setup();
  await s.load();
  for (const op of ops) await s.store.apply(op);
  return s;
}

const clipById = (doc: { tracks: { clips: ClipDoc[] }[] }, id: string) => doc.tracks.flatMap((t) => t.clips).find((c) => c.id === id)!;
const pt = (seconds: number, value: number): EnvelopePointDoc => ({ position: Math.round(seconds * RATE), value, curve: "linear" });

describe("assistant clip tools: one undoable step each", () => {
  const filterOnHost: Op = {
    op: "set_effects",
    target: { kind: "track", id: "t1" },
    chain: { enabled: true, input_gain_db: 0, output_gain_db: 0, mix: 1, slots: [{ id: "fx1", effect: "altaudio.filter", enabled: true, params: {}, preset: null }] },
  };
  const filterOnClip: Op = { ...filterOnHost, target: { kind: "clip", id: "c1" } } as Op;
  const CASES: [string, Record<string, unknown>, string, Op[]?][] = [
    ["split_clip", { clip_id: "c1", times_seconds: [20, 10] }, "Assistant: Split host at 2 Points"],
    ["split_clip", { tracks: ["Host", "Guest"], times_seconds: [30] }, "Assistant: Split Host, Guest at 30 s"],
    ["split_clip", { all_tracks: true, times_seconds: [5, 6] }, "Assistant: Split All Tracks at 2 Points"],
    ["delete_clips", { clip_ids: ["c1"] }, "Assistant: Delete Clip host"],
    ["delete_clips", { clip_ids: ["c1"], ripple: true }, "Assistant: Ripple Delete Clip host"],
    ["move_clip", { clip_id: "c1", start_seconds: 5, track: "Guest" }, "Assistant: Move host to Guest"],
    ["trim_clip", { clip_id: "c1", start_seconds: 5, end_seconds: 30 }, "Assistant: Trim host"],
    ["set_clip_gain", { clip_ids: ["c1"], gain_db: -3.5 }, "Assistant: Clip Gain: host"],
    ["set_clip_gain", { clip_ids: ["c1"], muted: true }, "Assistant: Mute Clip: host"],
    ["set_clip_fades", { clip_id: "c1", fade_in_seconds: 2, fade_out_shape: "equal_power", fade_out_seconds: 1, fade_out_bend: 0.5 }, "Assistant: Set Fades on host"],
    ["set_clip_color", { clip_ids: ["c1"], color: "blue" }, "Assistant: Clip Color: host"],
    ["stretch_clip", { clip_id: "c1", ratio: 1.5, preserve_pitch: false }, "Assistant: Time Stretch host"],
    ["stretch_clip", { clip_id: "c1", pitch_semitones: 3 }, "Assistant: Time Stretch host"],
    ["set_automation", { target: c1, parameter: "volume", points: [{ time_seconds: 2, value: -60 }, { time_seconds: 4, value: 0 }] }, "Assistant: Automate Volume on clip host"],
    ["set_automation", { target: c1, parameter: "pan", points: [{ time_seconds: 10, value: -100 }, { time_seconds: 20, value: 100 }] }, "Assistant: Automate Pan on clip host"],
    ["set_automation", { target: host, parameter: "pan", points: [{ time_seconds: 0, value: -100 }, { time_seconds: 8, value: 100 }] }, "Assistant: Automate Pan on Host"],
    ["set_automation", { target: { kind: "bus", id: "Voices" }, parameter: "volume", points: [{ time_seconds: 0, value: 0 }, { time_seconds: 8, value: -12 }] }, "Assistant: Automate Volume on Voices"],
    ["set_automation", { target: host, parameter: "effect", slot_id: "fx1", control: "frequency", points: [{ time_seconds: 1, value: 200 }, { time_seconds: 9, value: 8000 }] }, "Assistant: Automate Filter Frequency on Host", [filterOnHost]],
    ["set_automation", { target: c1, parameter: "effect", slot_id: "fx1", control: "Frequency", points: [{ time_seconds: 3, value: 500 }] }, "Assistant: Automate Filter Frequency on clip host", [filterOnClip]],
    ["clear_automation", { target: c1 }, "Assistant: Clear Automation on clip host", [{ op: "set_clips", changes: [{ id: "c1", volume: [pt(0, -6), pt(5, 0)], pan: [pt(1, 50)] }] }]],
    ["clear_automation", { target: host, parameter: "volume", start_seconds: 2, end_seconds: 4 }, "Assistant: Clear Volume Automation on Host", [{ op: "set_envelopes", owner: "t1", envelopes: [{ id: "e1", target: { type: "volume" }, enabled: true, points: [pt(1, 0), pt(3, -10), pt(5, 0)] }] }]],
  ];

  for (const [tool, args, label, ops] of CASES) {
    it(`${tool}: ${label}`, async () => {
      const { a, history, doc } = await ready(ops);
      const before = clone(doc());
      const r = await a.run(tool, args);
      expect(r.isError, r.text).toBe(false);
      expect(history.current.entries.map((e) => e.label)).toEqual([label]);
      const after = clone(doc());
      expect(after).not.toEqual(before);
      await history.undo();
      expect(doc()).toEqual(before);
      await history.redo();
      expect(doc()).toEqual(after);
    });
  }

  it("groups and ungroups clips, each one step", async () => {
    const { a, history, doc } = await ready();
    await a.run("split_clip", { clip_id: "c1", times_seconds: [30] });
    const ids = doc().tracks[0]!.clips.map((c) => c.id);
    const g = await a.run("group_clips", { clip_ids: ids });
    expect(g.isError, g.text).toBe(false);
    const group = (g.structured as { group: string }).group;
    expect(doc().tracks[0]!.clips.every((c) => c.group === group)).toBe(true);
    const s = (await a.run("get_session", {})).structured as { tracks: { clips?: { group?: string }[] }[] };
    expect(s.tracks[0]!.clips!.map((c) => c.group)).toEqual([group, group]);
    const u = await a.run("ungroup_clips", { clip_ids: [ids[1]!] });
    expect(u.isError, u.text).toBe(false);
    expect(clipById(doc(), ids[1]!).group).toBeNull();
    expect(history.current.entries.map((e) => e.label)).toEqual(["Assistant: Split host at 30 s", "Assistant: Group 2 Clips", `Assistant: Remove host from Group`]);
    await history.undo();
    expect(clipById(doc(), ids[1]!).group).toBe(group);
    expect((await a.run("group_clips", { clip_ids: ["c1"] })).text).toContain("two or more");
  });
});

describe("assistant clip editing", () => {
  it("splits a clip at several times and returns the pieces in order", async () => {
    const { a, doc } = await ready();
    const r = await a.run("split_clip", { clip_id: "c1", times_seconds: [40, 10, 20.5] });
    const s = r.structured as { pieces: { id: string; start_seconds: number; end_seconds: number }[]; new_clip_ids: string[] };
    expect(s.pieces.map((p) => [p.start_seconds, p.end_seconds])).toEqual([[2, 10], [10, 20.5], [20.5, 40], [40, 62]]);
    expect(s.pieces[0]!.id).toBe("c1");
    expect(s.new_clip_ids).toHaveLength(3);
    // Each piece plays the audio it played before: source follows the timeline.
    const third = clipById(doc(), s.pieces[2]!.id);
    expect(third.source_in).toBe(Math.round(18.5 * RATE));
  });

  it("ripple deletes a middle piece and closes the gap; the file is never touched", async () => {
    const { a, doc } = await ready();
    const r = await a.run("split_clip", { clip_id: "c1", times_seconds: [10, 20] });
    const [, mid, last] = (r.structured as { pieces: { id: string }[] }).pieces;
    const mediaBefore = clone(doc().media);
    await a.run("delete_clips", { clip_ids: [mid!.id], ripple: true });
    expect(clipById(doc(), last!.id).start).toBe(10 * RATE);
    expect(doc().media).toEqual(mediaBefore);
  });

  it("trims edges, moving the source with the left edge, and says when an edge stopped", async () => {
    const { a, doc } = await ready();
    let r = await a.run("trim_clip", { clip_id: "c1", start_seconds: 5, end_seconds: 30 });
    expect(r.structured).toMatchObject({ start_seconds: 5, end_seconds: 30 });
    expect(clipById(doc(), "c1").source_in).toBe(3 * RATE);
    // The file starts 3 s before the clip's start now: the left edge can go back only to 2 s.
    r = await a.run("trim_clip", { clip_id: "c1", start_seconds: 0 });
    expect(r.isError).toBe(false);
    expect(r.text).toContain("stopped at 2 s");
    expect(clipById(doc(), "c1").start).toBe(2 * RATE);
  });

  it("keeps fades from overlapping", async () => {
    const { a, doc } = await ready([{ op: "set_clips", changes: [{ id: "c1", fade_in: { length: 50 * RATE, shape: "linear", curve: 0 } }] }]);
    const r = await a.run("set_clip_fades", { clip_id: "c1", fade_out_seconds: 20, fade_out_shape: "cosine" });
    expect(r.text).toContain("fade out kept at 10 s");
    expect(clipById(doc(), "c1").fade_out).toEqual({ length: 10 * RATE, shape: "cosine", curve: 0 });
  });

  it("colors clips from the palette, by hex or back to the track color", async () => {
    const { a, doc } = await ready();
    await a.run("set_clip_color", { clip_ids: ["c1"], color: "3" });
    expect(clipById(doc(), "c1").color).toBe("--clip-3");
    let s = (await a.run("get_session", {})).structured as { tracks: { clips?: { color: string | null }[] }[] };
    expect(s.tracks[0]!.clips![0]!.color).toBe("purple");
    await a.run("set_clip_color", { clip_ids: ["c1"], color: "#A1B2C3" });
    expect(clipById(doc(), "c1").color).toBe("#a1b2c3");
    await a.run("set_clip_color", { clip_ids: ["c1"], color: "track" });
    expect(clipById(doc(), "c1").color).toBeNull();
    s = (await a.run("get_session", {})).structured as { tracks: { clips?: { color: string | null }[] }[] };
    expect(s.tracks[0]!.clips![0]!.color).toBeNull();
    expect(colorValue("Pink")).toBe("--clip-6");
    expect(colorName("--clip-2")).toBe("blue");
  });

  it("stretches by length or ratio, with or without keeping the pitch, and back", async () => {
    const { a, doc } = await ready([{ op: "set_clips", changes: [{ id: "c1", volume: [pt(0, 0), pt(10, -6)] }] }]);
    let r = await a.run("stretch_clip", { clip_id: "c1", length_seconds: 30 });
    expect(r.structured).toMatchObject({ start_seconds: 2, end_seconds: 32, ratio: 0.5, preserve_pitch: true });
    // The envelope stretches with the audio.
    expect(clipById(doc(), "c1").envelopes![0]!.points[1]!.position).toBe(5 * RATE);
    r = await a.run("stretch_clip", { clip_id: "c1", ratio: 2, preserve_pitch: false });
    expect(clipById(doc(), "c1").stretch).toMatchObject({ ratio: 2, keep_pitch: false });
    const s = (await a.run("get_session", {})).structured as { tracks: { clips?: { stretch?: unknown; end_seconds: number }[] }[] };
    expect(s.tracks[0]!.clips![0]).toMatchObject({ end_seconds: 122, stretch: { ratio: 2, preserve_pitch: false } });
    r = await a.run("stretch_clip", { clip_id: "c1", length_seconds: 1000 });
    expect(r.text).toContain("limited");
    r = await a.run("stretch_clip", { clip_id: "c1", remove: true });
    expect(clipById(doc(), "c1").stretch).toBeNull();
    expect(clipById(doc(), "c1").envelopes![0]!.points[1]!.position).toBe(10 * RATE);
    r = await a.run("stretch_clip", { clip_id: "c1", pitch_semitones: -2 });
    expect(clipById(doc(), "c1").stretch).toMatchObject({ ratio: 1, keep_pitch: true, pitch: -2 });
    r = await a.run("stretch_clip", { clip_id: "c1", pitch_semitones: 0 });
    expect(clipById(doc(), "c1").stretch).toBeNull();
  });

  it("says what is wrong and changes nothing on a mistake", async () => {
    const { a, history, doc } = await ready([{ op: "set_clips", changes: [{ id: "c1", locked: true }] }]);
    const before = clone(doc());
    const cases: [string, Record<string, unknown>, string][] = [
      ["split_clip", { clip_id: "c1", times_seconds: [10] }, "locked"],
      ["split_clip", { clip_id: "c1", tracks: ["Host"], times_seconds: [10] }, "exactly one of"],
      ["split_clip", { tracks: ["Host"], times_seconds: [10] }, "No unlocked clip"],
      ["split_clip", { tracks: ["Keys"], times_seconds: [10] }, "MIDI track"],
      ["split_clip", { clip_id: "zz", times_seconds: [10] }, 'no clip with id "zz"'],
      ["move_clip", { clip_id: "c1", start_seconds: 9 }, "locked"],
      ["trim_clip", { clip_id: "c1", end_seconds: 9 }, "locked"],
      ["stretch_clip", { clip_id: "c1", ratio: 2 }, "locked"],
      ["delete_clips", { clip_ids: ["c1", "nope"] }, 'no clip with id "nope"'],
      ["set_clip_color", { clip_ids: ["c1"], color: "teal" }, "palette name"],
      ["set_clip_gain", { clip_ids: ["c1"] }, "Give gain_db"],
      ["set_clip_fades", { clip_id: "c1" }, "at least one of"],
      ["set_automation", { target: { kind: "master" }, parameter: "volume", points: [{ time_seconds: 1, value: 0 }] }, "Master has no automation lanes"],
      ["set_automation", { target: c1, parameter: "volume", points: [{ time_seconds: 1, value: 0 }] }, "outside clip host (2 s to 62 s)"],
      ["set_automation", { target: host, parameter: "effect", points: [{ time_seconds: 1, value: 0 }] }, "needs slot_id"],
      ["set_automation", { target: host, parameter: "effect", slot_id: "x", control: "y", points: [{ time_seconds: 1, value: 0 }] }, "Its Voice Chain is empty"],
      ["set_automation", { target: { kind: "track", id: "Keys" }, parameter: "volume", points: [{ time_seconds: 1, value: 0 }] }, "MIDI track"],
      ["clear_automation", { target: host }, "to clear"],
    ];
    for (const [tool, args, needle] of cases) {
      const r = await a.run(tool, args);
      expect(r.isError, `${tool}: ${r.text}`).toBe(tool === "clear_automation" ? false : true);
      expect(r.text).toContain(needle);
    }
    expect(history.current.entries).toHaveLength(0);
    expect(doc()).toEqual(before);
  });

  it("moves only movable effect controls, as the Editor does", async () => {
    const chain = { enabled: true, input_gain_db: 0, output_gain_db: 0, mix: 1, slots: [{ id: "fx1", effect: "altaudio.filter", enabled: true, params: {}, preset: null }] };
    const { a, history } = await ready([
      { op: "set_effects", target: { kind: "track", id: "t1" }, chain },
      { op: "set_effects", target: { kind: "clip", id: "c1" }, chain },
    ]);
    const on = (target: unknown, control: string) => a.run("set_automation", { target, parameter: "effect", slot_id: "fx1", control, points: [{ time_seconds: 3, value: 1 }] });
    expect((await on(host, "enabled")).text).toContain("cannot move over time on a track or bus");
    expect((await on(host, "lookahead")).text).toContain("restarts playback");
    expect((await on(host, "nope")).text).toContain("Controls that can move over time: frequency");
    // A clip's envelope can switch an on/off control.
    expect((await on(c1, "enabled")).isError).toBe(false);
    expect(history.current.entries).toHaveLength(1);
    const got = (await a.run("get_automation", { target: host })).structured as { can_automate: { parameter: string; control?: string }[] };
    expect(got.can_automate.map((c) => c.control ?? c.parameter)).toEqual(["volume", "pan", "frequency"]);
  });
});

describe("assistant automation", () => {
  it("writes clip envelopes in clip time and reads them back in session seconds", async () => {
    const { a, doc } = await ready();
    const r = await a.run("set_automation", {
      target: c1,
      parameter: "volume",
      points: [
        { time_seconds: 2, value: -60 },
        { time_seconds: 3.25, value: 0 },
        { time_seconds: 61.999, value: 20 },
      ],
    });
    expect(r.isError, r.text).toBe(false);
    expect(r.text).toContain("1 value kept inside -60 to 12 dB");
    const env = clipById(doc(), "c1").envelopes!.find((e) => e.target.type === "volume")!;
    expect(env.enabled).toBe(true);
    expect(env.points.map((p) => [p.position, p.value])).toEqual([
      [0, -60],
      [1.25 * RATE, 0],
      [Math.round(59.999 * RATE), 12],
    ]);
    const got = (await a.run("get_automation", { target: c1 })).structured as { lanes: { parameter: string; points: { time_seconds: number; value: number }[] }[]; clip_start_seconds: number };
    expect(got.clip_start_seconds).toBe(2);
    expect(got.lanes[0]!.points).toEqual([
      { time_seconds: 2, value: -60 },
      { time_seconds: 3.25, value: 0 },
      { time_seconds: 61.999, value: 12 },
    ]);
    const s = (await a.run("get_session", {})).structured as { tracks: { clips?: { automation?: unknown }[] }[] };
    expect(s.tracks[0]!.clips![0]!.automation).toEqual([{ parameter: "volume", points: 3 }]);
  });

  it("replaces a range, adds, or replaces the whole lane", async () => {
    const start = [pt(1, 0), pt(2, -5), pt(3, -10), pt(4, 0)];
    const { a, doc } = await ready([{ op: "set_envelopes", owner: "t1", envelopes: [{ id: "e1", target: { type: "volume" }, enabled: true, points: start }] }]);
    const lane = () => doc().tracks[0]!.envelopes![0]!.points.map((p) => [p.position / RATE, p.value]);
    await a.run("set_automation", { target: host, parameter: "volume", points: [{ time_seconds: 1.5, value: -20 }, { time_seconds: 3, value: -30 }] });
    expect(lane()).toEqual([[1, 0], [1.5, -20], [3, -30], [4, 0]]);
    await a.run("set_automation", { target: host, parameter: "volume", mode: "add", points: [{ time_seconds: 2, value: -1 }, { time_seconds: 3, value: -2 }] });
    expect(lane()).toEqual([[1, 0], [1.5, -20], [2, -1], [3, -2], [4, 0]]);
    await a.run("set_automation", { target: host, parameter: "volume", mode: "replace_all", points: [{ time_seconds: 6, value: -3 }] });
    expect(lane()).toEqual([[6, -3]]);
    // The lane keeps its id.
    expect(doc().tracks[0]!.envelopes![0]!.id).toBe("e1");
  });

  it("makes a track lane play: switched on, shown, and back to Read from recording", async () => {
    const { a, doc } = await ready([
      { op: "set_envelopes", owner: "t1", envelopes: [{ id: "e1", target: { type: "pan" }, enabled: false, points: [pt(1, 0)] }] },
      { op: "set_track", id: "t1", changes: { record_mode: "latch" } },
    ]);
    const r = await a.run("set_automation", { target: host, parameter: "pan", points: [{ time_seconds: 0, value: -100 }, { time_seconds: 4, value: 100 }] });
    expect(r.text).toContain("back to Read");
    const t = doc().tracks[0]!;
    expect(t.envelopes![0]!.enabled).toBe(true);
    expect(t.show_automation).toBe(true);
    expect(t.record_mode).toBe("off");
    const got = (await a.run("get_automation", { target: host })).structured as { automation_mode: string };
    expect(got.automation_mode).toBe("off");
  });

  it("switches a clip envelope that was off back on", async () => {
    const { a, store, doc } = await ready([{ op: "set_clips", changes: [{ id: "c1", volume: [pt(0, -6)] }] }]);
    // Files from elsewhere can carry an envelope that is switched off.
    clipById(doc(), "c1").envelopes![0]!.enabled = false;
    store.set(clone(doc()));
    await a.run("set_automation", { target: c1, parameter: "volume", points: [{ time_seconds: 5, value: -3 }], mode: "add" });
    const env = clipById(doc(), "c1").envelopes!.find((e) => e.target.type === "volume")!;
    expect(env.enabled).toBe(true);
    expect(env.points.map((p) => p.value)).toEqual([-6, -3]);
  });

  it("refuses pan lanes on tracks in a 3D Space session", async () => {
    const { a, store } = await ready();
    store.set({ ...store.current!, space: { layout: "5.1" } });
    const r = await a.run("set_automation", { target: host, parameter: "pan", points: [{ time_seconds: 0, value: 0 }] });
    expect(r.isError).toBe(true);
    expect(r.text).toContain("3D Space");
  });

  it("clears points in a range, a lane, or everything", async () => {
    const { a, doc } = await ready([
      { op: "set_envelopes", owner: "t1", envelopes: [
        { id: "e1", target: { type: "volume" }, enabled: true, points: [pt(1, 0), pt(3, -10), pt(5, 0)] },
        { id: "e2", target: { type: "pan" }, enabled: true, points: [pt(1, 0)] },
      ] },
    ]);
    await a.run("clear_automation", { target: host, parameter: "volume", start_seconds: 2, end_seconds: 4 });
    expect(doc().tracks[0]!.envelopes![0]!.points.map((p) => p.position / RATE)).toEqual([1, 5]);
    await a.run("clear_automation", { target: host, parameter: "pan" });
    expect(doc().tracks[0]!.envelopes!.map((e) => e.id)).toEqual(["e1"]);
    await a.run("clear_automation", { target: host });
    expect(doc().tracks[0]!.envelopes).toEqual([]);
  });

  it("merges and removes points by position", () => {
    const p = (s: number, v = 0) => ({ position: s, value: v, curve: "linear" as const });
    expect(mergePoints([p(1), p(5), p(9)], [p(4, 1), p(6, 1)], "replace_range").map((x) => x.position)).toEqual([1, 4, 6, 9]);
    expect(mergePoints([p(1), p(5)], [p(5, 2)], "add")).toEqual([p(1), p(5, 2)]);
    expect(mergePoints([p(1)], [p(3, 1), p(3, 2)], "replace_all")).toEqual([p(3, 2)]);
    expect(removeRange([p(1), p(2), p(3)], 2, 3)).toEqual([p(1)]);
  });
});
