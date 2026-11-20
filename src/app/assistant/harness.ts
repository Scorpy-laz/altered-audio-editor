// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) The GeekCast Radio Network, LLC.
// This file is part of Altered Audio Editor.

/**
 * A stand-in for the core for the assistant's tests: a session in memory
 * that applies the ops the assistant sends (as src-tauri/src/session/ops.rs
 * does, for the fields these tools use), and undoes by putting the whole
 * document back.
 */

import { AssistantDispatcher, type AssistantDeps } from "./dispatcher";
import { UndoStack, type Command } from "../../commands";
import { SessionStore, type ClipChange, type ClipDoc, type EnvelopePointDoc, type Op, type SessionDoc, type TrackDoc } from "../../stores/session";
import type { EffectInfo, EffectParamInfo, MixdownRequest } from "../core";
import type { CommandRunner, MediaItem } from "../../stores/media-pool";
import { BUILTIN_PRESETS } from "../../panels/voice-chain/chain";
import { envelopeId } from "../../panels/editor/clip-edit";

export const RATE = 48000;

export function track(id: string, name: string, extra: Partial<TrackDoc> = {}): TrackDoc {
  return { id, name, role: "standard", channels: "mono", color: null, height: 96, volume_db: 0, pan: 0, muted: false, solo: false, record_armed: false, clips: [], ...extra };
}

export function startDoc(): SessionDoc {
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

export const clone = <T>(v: T): T => structuredClone(v);

/** What the core does with an op, for the ops the assistant sends. */
export function applyOp(doc: SessionDoc, op: Op): void {
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
    case "remove_clips":
      for (const id of op.ids) if (!doc.tracks.some((t) => t.clips.some((c) => c.id === id))) throw new Error(`no clip ${id}`);
      for (const t of doc.tracks) t.clips = t.clips.filter((c) => !op.ids.includes(c.id));
      return;
    case "restore_clips":
      for (const [tid, c] of op.clips) {
        if (doc.tracks.some((t) => t.clips.some((x) => x.id === c.id))) throw new Error(`duplicate id ${c.id}`);
        tr(tid).clips.push(clone(c));
      }
      return;
    case "set_clips":
      for (const ch of op.changes) setClip(doc, ch);
      return;
    case "set_envelopes": {
      const owner = doc.tracks.find((t) => t.id === op.owner) ?? doc.buses?.find((b) => b.id === op.owner);
      if (!owner) throw new Error(`no track ${op.owner}`);
      owner.envelopes = op.envelopes.map((e) => ({ ...e, points: [...e.points].sort((a, b) => a.position - b.position) }));
      return;
    }
    default:
      throw new Error(`the fake core does not know ${op.op}`);
  }
}

/** One clip change, as the core's set_clip does it for the fields the assistant sends. */
function setClip(doc: SessionDoc, ch: ClipChange): void {
  const from = doc.tracks.find((t) => t.clips.some((c) => c.id === ch.id));
  if (!from) throw new Error(`no clip ${ch.id}`);
  const i = from.clips.findIndex((c) => c.id === ch.id);
  const c = from.clips[i]!;
  const moves = ch.start !== undefined || ch.source_in !== undefined || ch.source_out !== undefined || ch.stretch !== undefined || ch.unstretch || (ch.track_id !== undefined && ch.track_id !== from.id);
  if (moves && c.locked && ch.locked !== false) throw new Error("clip locked");
  const { id: _id, track_id, volume, pan, effect_points, unstretch, ungroup, layer, color, ...fields } = ch;
  Object.assign(c, fields);
  if (unstretch) c.stretch = null;
  if (ungroup) c.group = null;
  if (color !== undefined) c.color = color === "" ? null : color;
  if (ch.gain_db !== undefined) c.gain_db = Math.max(-96, Math.min(24, ch.gain_db));
  const put = (target: NonNullable<ClipDoc["envelopes"]>[number]["target"], pts: EnvelopePointDoc[], lo: number, hi: number, id: string) => {
    const envs = c.envelopes ?? [];
    const k = envs.findIndex((e) => JSON.stringify(e.target) === JSON.stringify(target));
    const next = pts.map((p) => ({ ...p, value: Math.max(lo, Math.min(hi, p.value)) })).sort((a, b) => a.position - b.position);
    if (k >= 0 && next.length === 0) envs.splice(k, 1);
    else if (k >= 0) envs[k] = { ...envs[k]!, points: next };
    else if (next.length) envs.push({ id, target, enabled: true, points: next });
    c.envelopes = envs;
  };
  if (volume) put({ type: "volume" }, volume, -60, 12, envelopeId(c.id, "volume"));
  if (pan) put({ type: "pan" }, pan, -100, 100, envelopeId(c.id, "pan"));
  for (const ep of effect_points ?? []) put({ type: "effect_param", slot_id: ep.slot_id, param: ep.param }, ep.points, -1e9, 1e9, `${c.id}:${ep.slot_id}:${ep.param}`);
  if (c.source_out <= c.source_in) throw new Error("empty clip");
  let at: { t: TrackDoc; i: number } = { t: from, i };
  if (track_id !== undefined && track_id !== from.id) {
    const to = doc.tracks.find((t) => t.id === track_id);
    if (!to) throw new Error(`no track ${track_id}`);
    if (to.kind === "midi") throw new Error("wrong track kind");
    from.clips.splice(i, 1);
    to.clips.push(c);
    at = { t: to, i: to.clips.length - 1 };
  }
  if (layer !== undefined) {
    at.t.clips.splice(at.i, 1);
    at.t.clips.splice(Math.min(layer, at.t.clips.length), 0, c);
  }
}

export const param = (key: string, min: number, max: number, def: number, step: number, unit = ""): EffectParamInfo => ({ key, name: key[0]!.toUpperCase() + key.slice(1), unit, min, max, default: def, step, log: false, toggle: false });

export const CATALOG: EffectInfo[] = [
  { id: "altaudio.limiter", name: "Limiter", category: "Dynamics", description: "Keeps peaks under a ceiling.", params: [param("ceiling", -20, 0, -6, 0.1, "dB"), param("release", 1, 1000, 100, 1, "ms")] },
  { id: "altaudio.gain", name: "Gain", category: "Amplitude", description: "Raises or lowers the level.", params: [param("gain", -48, 48, 0, 0.1, "dB")] },
  {
    id: "altaudio.filter",
    name: "Filter",
    category: "Filters",
    description: "Cuts lows or highs.",
    params: [
      { ...param("frequency", 20, 20000, 1000, 1, "Hz"), log: true },
      { ...param("enabled", 0, 1, 1, 1), toggle: true },
      { ...param("mode", 0, 2, 0, 1), choices: ["Low cut", "High cut", "Band"] },
      { ...param("lookahead", 0, 20, 5, 1, "ms"), retimes: true },
    ],
  },
  { id: "vst3:abc", name: "Fancy Comp", category: "Dynamics", description: "", params: [], plugin: true },
];

export function setup() {
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

