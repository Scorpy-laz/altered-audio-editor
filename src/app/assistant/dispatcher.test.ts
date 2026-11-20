// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) The GeekCast Radio Network, LLC.
// This file is part of Altered Audio Editor.

import { describe, expect, it } from "vitest";
import tools from "./tools.json";
import { AssistantDispatcher, isAbsolute, TOOL_NAMES, type AssistantDeps } from "./dispatcher";
import { UndoStack, type Command } from "../../commands";
import { SessionStore, type MarkerDoc, type Op, type SessionDoc, type TrackDoc } from "../../stores/session";
import type { EffectInfo, EffectParamInfo, MixdownRequest } from "../core";
import type { CommandRunner, MediaItem } from "../../stores/media-pool";
import { BUILTIN_PRESETS } from "../../panels/voice-chain/chain";
import { claudeCodeCommand, claudeDesktopConfig, codexConfig, tomlString } from "./snippets";

const RATE = 48000;

function track(id: string, name: string, extra: Partial<TrackDoc> = {}): TrackDoc {
  return { id, name, role: "standard", channels: "mono", color: null, height: 96, volume_db: 0, pan: 0, muted: false, solo: false, record_armed: false, clips: [], ...extra };
}

function startDoc(): SessionDoc {
  return {
    format: "altaudio.session",
    schema_version: 1,
    id: "s",
    name: "Episode 12",
    sample_rate: RATE,
    bit_depth: 24,
    media: [{ id: "m1", path: "C:\\audio\\host.wav", relative_path: null, sample_rate: RATE, channels: 1, length: RATE * 60 }],
    tracks: [
      track("t1", "Host", { clips: [{ id: "c1", name: "host", media_id: "m1", start: RATE * 2, source_in: 0, source_out: RATE * 60, gain_db: 0, muted: false, locked: false, color: null, fade_in: { length: 0, shape: "linear", curve: 0 }, fade_out: { length: 0, shape: "linear", curve: 0 } }] }),
      track("t2", "Guest"),
      track("t3", "Keys", { kind: "midi" }),
    ],
    buses: [{ id: "b1", name: "Voices", channels: "stereo", color: null, height: 60, volume_db: 0, pan: 0, muted: false, solo: false, output: { type: "master" }, sends: [], effects: { enabled: true, input_gain_db: 0, output_gain_db: 0, mix: 1, slots: [] } }],
    markers: [{ id: "k1", name: "Intro", kind: "cue", position: RATE, duration: 0, description: "", chapter: null, contracted_duration: null }],
    view: { samples_per_pixel: 1024, scroll: 0, scroll_y: 0, playhead: RATE * 3, selection: null },
    libraries: [],
    master: { volume_db: 0, channels: "stereo", height: 60 },
  };
}

const clone = <T>(v: T): T => structuredClone(v);

/** What the core does with an op, for the ops the assistant sends. */
function applyOp(doc: SessionDoc, op: Op): void {
  const tr = (id: string) => {
    const t = doc.tracks.find((x) => x.id === id);
    if (!t) throw new Error(`no track ${id}`);
    return t;
  };
  switch (op.op) {
    case "batch":
      for (const o of op.ops) applyOp(doc, o);
      return;
    case "set_effects": {
      const t = op.target;
      if (t.kind === "master") doc.master = { ...doc.master!, effects: op.chain };
      else if (t.kind === "track") tr(t.id).effects = op.chain;
      else if (t.kind === "bus") doc.buses!.find((b) => b.id === t.id)!.effects = op.chain;
      else for (const x of doc.tracks) for (const c of x.clips) if (c.id === t.id) c.effects = op.chain;
      return;
    }
    case "set_track":
      Object.assign(tr(op.id), op.changes);
      return;
    case "set_bus":
      Object.assign(doc.buses!.find((b) => b.id === op.id)!, op.changes);
      return;
    case "set_master_volume":
      doc.master!.volume_db = op.volume_db;
      return;
    case "add_markers":
      doc.markers.push(...op.markers);
      return;
    case "remove_markers":
      doc.markers = doc.markers.filter((m) => !op.ids.includes(m.id));
      return;
    case "add_media":
      doc.media.push(op.media);
      return;
    case "add_track":
      doc.tracks.splice(op.index, 0, track(op.id, op.name, { channels: op.stereo ? "stereo" : "mono" }));
      return;
    case "add_clips":
      tr(op.track_id).clips.push(...op.clips.map((c) => ({ ...c, gain_db: 0, muted: false, locked: false, color: null, fade_in: { length: 0, shape: "linear", curve: 0 }, fade_out: { length: 0, shape: "linear", curve: 0 } })));
      return;
    default:
      throw new Error(`the fake core does not know ${op.op}`);
  }
}

const param = (key: string, min: number, max: number, def: number, step: number, unit = ""): EffectParamInfo => ({ key, name: key[0]!.toUpperCase() + key.slice(1), unit, min, max, default: def, step, log: false, toggle: false });

const CATALOG: EffectInfo[] = [
  { id: "altaudio.limiter", name: "Limiter", category: "Dynamics", description: "Keeps peaks under a ceiling.", params: [param("ceiling", -20, 0, -6, 0.1, "dB"), param("release", 1, 1000, 100, 1, "ms")] },
  { id: "altaudio.gain", name: "Gain", category: "Amplitude", description: "Raises or lowers the level.", params: [param("gain", -48, 48, 0, 0.1, "dB")] },
  { id: "vst3:abc", name: "Fancy Comp", category: "Dynamics", description: "", params: [], plugin: true },
];

function setup() {
  let doc = startDoc();
  const store = new SessionStore({
    get: async () => ({ session: clone(doc), path: null }),
    setView: async () => {},
    apply: async (op: Op) => {
      const before = clone(doc);
      const next = clone(doc);
      applyOp(next, op);
      doc = next;
      return { session: clone(doc), inverse: { op: "__restore", doc: before } as unknown as Op };
    },
  });
  // The fake core's undo: put the whole document back.
  const realApply = store.apply.bind(store);
  store.apply = async (op: Op) => {
    if ((op as unknown as { op: string }).op === "__restore") {
      const back = (op as unknown as { doc: SessionDoc }).doc;
      const before = clone(doc);
      doc = clone(back);
      store.set(clone(doc));
      return { op: "__restore", doc: before } as unknown as Op;
    }
    return realApply(op);
  };
  const history = new UndoStack();
  const pool = new Map<string, MediaItem>();
  const files = new Set(["C:\\audio\\guest.wav", "C:\\audio\\music.mp3", "C:\\audio\\broken.wav", "C:\\out\\taken.mp3"]);
  const exported: { path: string; req: MixdownRequest }[] = [];
  const transport = {
    current: { playing: false, position: 0 },
    async play(from?: number) {
      this.current = { playing: true, position: from ?? doc.view.playhead };
    },
    async pause() {
      this.current = { ...this.current, playing: false };
    },
    async stop() {
      this.current = { ...this.current, playing: false };
    },
    async seek(to: number) {
      store.setView({ ...store.current!.view, playhead: to });
    },
  };
  let ids = 0;
  const deps: AssistantDeps = {
    session: store,
    history,
    transport,
    catalog: async () => CATALOG,
    chainPresets: async () => ({ saved: [], known: new Set(BUILTIN_PRESETS.flatMap((p) => p.chain.slots.map((s) => s.effect))) }),
    effectPresets: (id) => (id === "altaudio.limiter" ? [{ name: "-3 dB", params: { ceiling: -3 } }] : []),
    measure: async () => ({ integrated: -18.234, integratedMono: -21, range: 5.5, truePeak: -0.4, samplePeak: -0.6, seconds: 62, cancelled: false }),
    exportMix: async (path, req) => {
      exported.push({ path, req });
      return { frames: RATE * 62, sampleRate: RATE, bitDepth: 24, peak: 0.9, cancelled: false };
    },
    pathsExist: async (paths) => paths.map((p) => files.has(p)),
    save: async (path) => path ?? null,
    open: async () => true,
    importMedia: async (paths: string[], runner: CommandRunner) => {
      const fresh = paths.map((p) => ({ id: `media-${++ids}`, path: p, name: p.split("\\").pop()!, status: (p.includes("broken") ? "error" : "ready") as MediaItem["status"], info: p.includes("broken") ? null : { sampleRate: 44100, channels: 2, length: 44100 * 10 }, error: p.includes("broken") ? "not audio" : null }) as MediaItem);
      const cmd: Command = {
        label: "Import",
        do: () => fresh.forEach((m) => pool.set(m.id, m)),
        undo: () => fresh.forEach((m) => pool.delete(m.id)),
      };
      await runner.execute(cmd);
      return { ids: fresh.map((m) => m.id) };
    },
    mediaIdle: async () => {},
    media: (id) => pool.get(id),
    newId: () => `id${++ids}`,
    platform: "windows",
  };
  const a = new AssistantDispatcher(deps);
  return { a, store, history, doc: () => doc, pool, exported, transport, load: () => store.load() };
}

const host = { kind: "track", id: "Host" };

describe("assistant tools", () => {
  it("handles every tool in tools.json and nothing else", () => {
    const { a } = setup();
    const listed = (tools.tools as { name: string }[]).map((t) => t.name).sort();
    expect(a.handled.sort()).toEqual(listed);
    expect([...TOOL_NAMES].sort()).toEqual(listed);
  });

  it("reads the session in seconds", async () => {
    const { a, load } = setup();
    await load();
    const r = await a.run("get_session", {});
    expect(r.isError).toBe(false);
    const s = r.structured as { duration_seconds: number; playhead_seconds: number; tracks: { name: string; clips?: { start_seconds: number; end_seconds: number }[] }[]; markers: { start_seconds: number }[] };
    expect(s.duration_seconds).toBe(62);
    expect(s.playhead_seconds).toBe(3);
    expect(s.tracks[0]!.clips![0]).toMatchObject({ start_seconds: 2, end_seconds: 62 });
    expect(s.markers[0]!.start_seconds).toBe(1);
    expect(JSON.parse(r.text)).toEqual(s);
    const short = (await a.run("get_session", { include_clips: false })).structured as { tracks: { clips?: unknown }[] };
    expect(short.tracks[0]!.clips).toBeUndefined();
  });

  // Each of these is one History entry named "Assistant: ...", and Undo puts the session back exactly.
  const EDITS: [string, Record<string, unknown>, string][] = [
    ["add_effect", { target: host, effect: "Limiter", params: { ceiling: -3 } }, "Assistant: Add Limiter to Host"],
    ["add_effect", { target: { kind: "master" }, effect: "altaudio.limiter", preset: "-3 dB" }, "Assistant: Add Limiter to the Master"],
    ["add_effect", { target: { kind: "clip", id: "c1" }, effect: "Gain" }, "Assistant: Add Gain to clip host"],
    ["add_effect", { target: { kind: "bus", id: "Voices" }, effect: "Gain" }, "Assistant: Add Gain to Voices"],
    ["apply_chain_preset", { target: { kind: "track", id: "t2" }, preset: "podcast voice" }, "Assistant: Apply Podcast Voice to Guest"],
    ["add_marker", { position_seconds: 12.5, name: "Ad break", kind: "ad_slot", duration_seconds: 30 }, "Assistant: Add Range Marker"],
    ["remove_marker", { marker_ids: ["k1"] }, "Assistant: Remove Marker"],
    ["set_mix", { target: host, volume_db: -4.5 }, "Assistant: Set Host Volume"],
    ["set_mix", { target: { kind: "track", id: "Guest" }, pan: -30, mute: true }, "Assistant: Set Guest Mix"],
    ["set_mix", { target: { kind: "master" }, volume_db: -2 }, "Assistant: Set Master Volume"],
    ["import_audio", { paths: ["C:\\audio\\guest.wav", "C:\\audio\\music.mp3"], track: "Guest", position_seconds: 5 }, "Assistant: Import 2 Files to Guest"],
    ["import_audio", { paths: ["C:\\audio\\music.mp3"] }, "Assistant: Import music.mp3 to a new track"],
  ];

  for (const [tool, args, label] of EDITS) {
    it(`${tool} ${label} is one undoable step`, async () => {
      const { a, history, doc, load, pool } = setup();
      await load();
      const before = clone(doc());
      const poolBefore = pool.size;
      const r = await a.run(tool, args);
      expect(r.isError, r.text).toBe(false);
      expect(history.current.entries.map((e) => e.label)).toEqual([label]);
      const after = clone(doc());
      expect(after).not.toEqual(before);
      await history.undo();
      expect(doc()).toEqual(before);
      expect(pool.size).toBe(poolBefore);
      await history.redo();
      // Redo makes the same change again (new ids are kept, so it is identical).
      expect(doc()).toEqual(after);
    });
  }

  it("works on an effect by slot id or index, each call one step", async () => {
    const { a, history, doc, load } = setup();
    await load();
    await a.run("add_effect", { target: host, effect: "Gain" });
    const added = await a.run("add_effect", { target: host, effect: "Limiter", index: 0 });
    const slot = (added.structured as { slot_id: string }).slot_id;
    expect((added.structured as { index: number }).index).toBe(0);
    const chain = () => doc().tracks[0]!.effects!;
    let r = await a.run("set_effect_params", { target: host, slot_id: slot, params: { ceiling: -40, Release: 250 } });
    expect(r.isError).toBe(false);
    expect(r.text).toContain("kept at -20.0 dB");
    expect(chain().slots[0]!.params).toMatchObject({ ceiling: -20, release: 250 });
    r = await a.run("move_effect", { target: host, index: 0, to_index: 1 });
    expect(chain().slots.map((s) => s.effect)).toEqual(["altaudio.gain", "altaudio.limiter"]);
    r = await a.run("set_effect_bypass", { target: host, slot_id: slot, bypass: true });
    expect(chain().slots[1]!.enabled).toBe(false);
    r = await a.run("set_effect_bypass", { target: host, bypass: true });
    expect(chain().enabled).toBe(false);
    r = await a.run("remove_effect", { target: host, index: 0 });
    expect(r.text).toBe("Removed Gain from Host.");
    expect(history.current.entries.map((e) => e.label)).toEqual([
      "Assistant: Add Gain to Host",
      "Assistant: Add Limiter to Host",
      "Assistant: Set Limiter on Host",
      "Assistant: Move Limiter on Host",
      "Assistant: Bypass Limiter on Host",
      "Assistant: Bypass Voice Chain on Host",
      "Assistant: Remove Gain from Host",
    ]);
    // Nothing to change: an answer, but no empty History entry.
    r = await a.run("set_effect_params", { target: host, slot_id: slot, params: { ceiling: -20 } });
    expect(r.isError).toBe(false);
    expect(r.text).toContain("already");
    expect(history.current.entries).toHaveLength(7);
    // Reading back.
    const got = (await a.run("get_chain", { target: host })).structured as { enabled: boolean; effects: { slot_id: string; name: string; bypassed: boolean; controls: { key: string; value: number; text: string }[] }[] };
    expect(got.enabled).toBe(false);
    expect(got.effects[0]).toMatchObject({ slot_id: slot, name: "Limiter", bypassed: true });
    expect(got.effects[0]!.controls.find((c) => c.key === "ceiling")).toMatchObject({ value: -20, text: "-20.0 dB" });
  });

  it("says what is wrong and changes nothing on a mistake", async () => {
    const { a, history, doc, load, pool } = setup();
    await load();
    const before = clone(doc());
    const cases: [string, Record<string, unknown>, string][] = [
      ["add_effect", { target: { kind: "track", id: "Nobody" }, effect: "Limiter" }, 'There is no track "Nobody". Tracks: Host (t1)'],
      ["add_effect", { target: host, effect: "Reverb 9000" }, 'There is no effect "Reverb 9000"'],
      ["add_effect", { target: host, effect: "Limiter", params: { loudness: 3 } }, "Limiter has no control \"loudness\". Its controls: ceiling, release"],
      ["add_effect", { target: host, effect: "Limiter", preset: "Nope" }, "Its presets: -3 dB"],
      ["add_effect", { target: { kind: "track" }, effect: "Limiter" }, "target.id is needed"],
      ["remove_effect", { target: host, index: 3 }, "Host has 0 effects"],
      ["remove_effect", { target: host }, "Give slot_id or index"],
      ["apply_chain_preset", { target: host, preset: "Space Echo" }, "no Voice Chain preset"],
      ["remove_marker", { marker_ids: ["zz"] }, "No marker has the id zz"],
      ["set_mix", { target: { kind: "master" }, pan: 10 }, "volume only"],
      ["set_mix", { target: host }, "Give volume_db"],
      ["import_audio", { paths: ["C:\\audio\\broken.wav"] }, "could not be read as audio: broken.wav (not audio)"],
      ["import_audio", { paths: ["relative.wav"] }, "must be absolute"],
      ["import_audio", { paths: ["C:\\audio\\gone.wav"] }, "No file at"],
      ["import_audio", { paths: ["C:\\audio\\guest.wav"], track: "Keys" }, "MIDI track"],
      ["transport", { action: "set_selection", start_seconds: 5, end_seconds: 2 }, "after start_seconds"],
      ["undo", {}, "nothing to undo"],
      ["bogus_tool", {}, "Unknown tool"],
    ];
    for (const [tool, args, needle] of cases) {
      const r = await a.run(tool, args);
      expect(r.isError, `${tool}: ${r.text}`).toBe(true);
      expect(r.text).toContain(needle);
    }
    expect(history.current.entries).toHaveLength(0);
    expect(doc()).toEqual(before);
    // The failed import's Files panel entry was taken back too.
    expect(pool.size).toBe(0);
  });

  it("never merges with the person's own edits", async () => {
    const { a, history, store, doc, load } = setup();
    await load();
    await store.edit("Track Volume", { op: "set_track", id: "t1", changes: { volume_db: -1 } }, history);
    await a.run("set_mix", { target: host, volume_db: -6 });
    await store.edit("Track Volume", { op: "set_track", id: "t1", changes: { volume_db: -7 } }, history);
    expect(history.current.entries.map((e) => e.label)).toEqual(["Track Volume", "Assistant: Set Host Volume", "Track Volume"]);
    // The assistant only undoes its own steps.
    let r = await a.run("undo", {});
    expect(r.isError).toBe(true);
    expect(r.text).toContain('"Track Volume", was made by the person');
    await history.undo();
    expect(doc().tracks[0]!.volume_db).toBe(-6);
    r = await a.run("undo", {});
    expect(r.text).toBe('Undid "Assistant: Set Host Volume".');
    expect(doc().tracks[0]!.volume_db).toBe(-1);
    r = await a.run("redo", {});
    expect(r.isError).toBe(false);
    expect(doc().tracks[0]!.volume_db).toBe(-6);
    r = await a.run("redo", {});
    expect(r.text).toContain("was the person's");
  });

  it("runs calls one at a time in order", async () => {
    const { a, history, load } = setup();
    await load();
    const all = await Promise.all([a.run("add_marker", { position_seconds: 1, name: "A" }), a.run("add_marker", { position_seconds: 2, name: "B" }), a.run("list_markers", {})]);
    expect(all.every((r) => !r.isError)).toBe(true);
    expect((all[2]!.structured as { markers: MarkerDoc[] }).markers.map((m) => m.name)).toEqual(["Intro", "A", "B"]);
    expect(history.current.entries).toHaveLength(2);
  });

  it("drives the transport and the selection without touching History", async () => {
    const { a, history, store, transport, load } = setup();
    await load();
    await a.run("transport", { action: "play", position_seconds: 10 });
    expect(transport.current).toEqual({ playing: true, position: 10 * RATE });
    await a.run("transport", { action: "pause" });
    expect(transport.current.playing).toBe(false);
    await a.run("transport", { action: "seek", position_seconds: 4 });
    expect(store.current!.view.playhead).toBe(4 * RATE);
    await a.run("transport", { action: "set_selection", start_seconds: 1, end_seconds: 2.5 });
    expect(store.current!.view.selection).toEqual({ start: RATE, end: 2.5 * RATE });
    await a.run("transport", { action: "clear_selection" });
    expect(store.current!.view.selection).toBeNull();
    expect(history.current.entries).toHaveLength(0);
  });

  it("measures and exports with the export path's own checks", async () => {
    const { a, exported, load } = setup();
    await load();
    const m = await a.run("measure_loudness", { start_seconds: 1, end_seconds: 10, track: "Host" });
    expect(m.text.split("\n")[0]).toBe("Integrated -18.2 LUFS, true peak -0.4 dBTP, range 5.5 LU.");
    expect(m.structured).toMatchObject({ integrated_lufs: -18.2, seconds: 62 });
    for (const [args, needle] of [
      [{ path: "episode.mp3" }, "absolute"],
      [{ path: "C:\\out\\taken.mp3" }, "already exists"],
      [{ path: "C:\\out\\ep.mp3", format: "wav" }, "should end in .wav"],
      [{ path: "C:\\out\\ep.ogg" }, "format must be"],
    ] as [Record<string, unknown>, string][]) {
      const r = await a.run("export_mixdown", args);
      expect(r.isError).toBe(true);
      expect(r.text).toContain(needle);
    }
    const r = await a.run("export_mixdown", { path: "C:\\out\\taken.mp3", overwrite: true, mp3_bitrate: 192, loudness_lufs: -16, start_seconds: 2 });
    expect(r.isError, r.text).toBe(false);
    expect(r.text).toBe("Exported 62 s to C:\\out\\taken.mp3.");
    expect(exported[0]!.req).toMatchObject({ format: { kind: "mp3", mp3Bitrate: 192, channels: 2 }, normalize: { lufs: -16, peak: -1 }, range: [2 * RATE, 62 * RATE] });
  });

  it("saves only to a known or given .altaudio file", async () => {
    const { a, load } = setup();
    await load();
    expect((await a.run("save_session", {})).text).toContain("never been saved");
    expect((await a.run("save_session", { path: "C:\\s\\ep.wav" })).text).toContain(".altaudio");
    expect((await a.run("save_session", { path: "C:\\s\\ep.altaudio" })).text).toBe("Saved to C:\\s\\ep.altaudio.");
    expect((await a.run("open_session", { path: "C:\\s\\missing.altaudio" })).text).toContain("There is no file");
  });

  it("lists effects, plug-ins and presets", async () => {
    const { a } = setup();
    const all = (await a.run("list_effects", {})).structured as { effects: { id: string }[]; plugins: { id: string }[]; voice_chain_presets: { name: string; adds: string }[] };
    expect(all.effects.map((e) => e.id)).toEqual(["altaudio.limiter", "altaudio.gain"]);
    expect(all.plugins.map((e) => e.id)).toEqual(["vst3:abc"]);
    expect(all.voice_chain_presets.find((p) => p.name === "Podcast Voice")?.adds).toContain("whole chain");
    const one = (await a.run("list_effects", { effect: "limiter" })).structured as { controls: { key: string; unit: string; min: number }[]; presets: string[] };
    expect(one.controls[0]).toMatchObject({ key: "ceiling", unit: "dB", min: -20 });
    expect(one.presets).toEqual(["-3 dB"]);
  });
});

describe("assistant setup snippets", () => {
  const exe = "C:\\Program Files\\Altered Audio Editor\\Altered Audio Editor.exe";
  it("builds Claude Desktop, Codex and Claude Code settings from the program path", () => {
    expect(JSON.parse(claudeDesktopConfig(exe))).toEqual({ mcpServers: { "altered-audio": { command: exe, args: ["--mcp-stdio"] } } });
    const toml = codexConfig(exe);
    expect(toml).toContain("[mcp_servers.altered-audio]");
    expect(toml).toContain("command = 'C:\\Program Files\\Altered Audio Editor\\Altered Audio Editor.exe'");
    expect(toml).toContain("args = ['--mcp-stdio']");
    expect(tomlString("it's")).toBe('"it\'s"');
    expect(tomlString("C:\\a'b")).toBe('"C:\\\\a\'b"');
    expect(claudeCodeCommand(exe, true)).toBe(`claude mcp add --transport stdio altered-audio -- "${exe}" --mcp-stdio`);
    expect(claudeCodeCommand("/opt/altered audio/app", false)).toBe("claude mcp add --transport stdio altered-audio -- '/opt/altered audio/app' --mcp-stdio");
  });

  it("knows absolute paths on every system", () => {
    for (const p of ["C:\\x.wav", "d:/x.wav", "\\\\server\\share\\x.wav", "/home/me/x.wav"]) expect(isAbsolute(p)).toBe(true);
    for (const p of ["x.wav", "..\\x.wav", "C:x.wav", "~/x.wav"]) expect(isAbsolute(p)).toBe(false);
  });
});
