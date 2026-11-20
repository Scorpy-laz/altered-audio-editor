// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) The GeekCast Radio Network, LLC.
// This file is part of Altered Audio Editor.

/**
 * The interface side of the assistant connection: runs each tool call the
 * core hands over, through the same session ops and commands the menus
 * use. Every call that changes the session is exactly one History entry
 * named "Assistant: ...", so one Undo takes it back. Times the assistant
 * sees and gives are seconds; the session works in samples.
 *
 * Input shapes were already checked against the schemas in tools.json by
 * the core; this file checks what only the session can tell (ids, names,
 * ranges that depend on the effect).
 */

import tools from "./tools.json";
import { AssistantStep, NoChange } from "./step";
import { clipTools, colorName, type ClipToolContext } from "./clip-tools";
import { autoMode } from "../../panels/editor/automation";
import type { UndoStack } from "../../commands";
import type { EffectInfo, EffectParamInfo, MixdownRequest, MixdownResult } from "../core";
import type { LoudnessReport } from "../loudness";
import { DEFAULT_FORMAT, formatAvailable, type FormatKind, type OutputFormat } from "../output-format";
import type { CommandRunner, MediaItem } from "../../stores/media-pool";
import { chainOf, addSlot, applyPreset, addsOne, clampParam, formatParam, paramValue, removeSlot, moveSlotTo, setSlot, newSlot, BUILTIN_PRESETS, PRESET_INFO, MAX_SLOTS, type ChainPreset } from "../../panels/voice-chain/chain";
import type { EffectPreset } from "../../panels/voice-chain/effect-presets";
import { newMarker, nextMarkerName } from "../../panels/markers/markers";
import { clipLength, sessionEnd, stretchOf, type EnvelopeDoc, type EffectChainDoc, type EffectTarget, type MarkerKind, type Op, type SessionDoc, type SessionStore, type TrackDoc, type BusDoc } from "../../stores/session";

export interface ToolResult {
  text: string;
  structured?: unknown;
  isError: boolean;
}

/** A mistake the assistant can fix: sent back as a tool error. */
export class ToolError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "ToolError";
  }
}

type Args = Record<string, unknown>;

export interface AssistantDeps {
  session: SessionStore;
  history: UndoStack;
  transport: {
    readonly current: { playing: boolean; position: number };
    play(from?: number): Promise<void>;
    pause(): Promise<void>;
    stop(): Promise<void>;
    seek(to: number): Promise<void>;
  };
  catalog(): Promise<EffectInfo[]>;
  /** The person's saved Voice Chain presets, and the effects this build has. */
  chainPresets(): Promise<{ saved: ChainPreset[]; known: ReadonlySet<string> }>;
  /** Presets of one effect, built-in and saved. */
  effectPresets(effect: string): readonly EffectPreset[];
  measure(request: MixdownRequest): Promise<LoudnessReport>;
  exportMix(path: string, request: MixdownRequest): Promise<MixdownResult>;
  pathsExist(paths: string[]): Promise<boolean[]>;
  /** Saves to `path`, or the session's own file when null. Returns the file saved. */
  save(path: string | null): Promise<string | null>;
  /** Opens a session file. False when it did not open. */
  open(path: string): Promise<boolean>;
  /** Adds files to the Files panel with `runner` (one command). */
  importMedia(paths: string[], runner: CommandRunner): Promise<{ ids: string[] }>;
  /** Resolves once files being measured are done. */
  mediaIdle(): Promise<void>;
  media(id: string): MediaItem | undefined;
  newId(): string;
  /** "windows", "linux" or "macos". */
  platform: string;
}

/** The tool names the core may send. */
export const TOOL_NAMES: readonly string[] = (tools.tools as { name: string }[]).map((t) => t.name);

const round = (v: number, digits = 3) => Number(v.toFixed(digits));
const plural = (n: number, one: string, many = `${one}s`) => `${n} ${n === 1 ? one : many}`;
const str = (v: unknown): string | undefined => (typeof v === "string" ? v : undefined);
const num = (v: unknown): number | undefined => (typeof v === "number" && Number.isFinite(v) ? v : undefined);
const bool = (v: unknown): boolean | undefined => (typeof v === "boolean" ? v : undefined);
const key = (s: string) => s.trim().toLowerCase();

/** A resolved Voice Chain or mixer strip. */
interface Strip {
  target: EffectTarget;
  /** How labels and answers name it: the track's name, "the Master". */
  name: string;
  chain: EffectChainDoc;
  track?: TrackDoc;
  bus?: BusDoc;
}

export class AssistantDispatcher {
  /** Calls run one at a time, in arrival order. */
  private queue: Promise<unknown> = Promise.resolve();

  constructor(private readonly d: AssistantDeps) {}

  /** Runs one call. Never throws: failures come back with isError. */
  run(tool: string, args: Args): Promise<ToolResult> {
    const next = this.queue.then(() => this.dispatch(tool, args ?? {}));
    this.queue = next.catch(() => undefined);
    return next;
  }

  private async dispatch(tool: string, args: Args): Promise<ToolResult> {
    try {
      const fn = this.handlers[tool];
      if (!fn) return { text: `Unknown tool: ${tool}`, isError: true };
      return await fn(args);
    } catch (e) {
      if (e instanceof NoChange) return { text: e.message, isError: false };
      const message = e instanceof Error ? e.message : String(e);
      return { text: message || "Something went wrong.", isError: true };
    }
  }

  // --- helpers ---

  private doc(): SessionDoc {
    const d = this.d.session.current;
    if (!d) throw new ToolError("No session is open.");
    return d;
  }

  private sec(samples: number, doc = this.doc()): number {
    return round(samples / doc.sample_rate);
  }

  private samples(seconds: number, doc = this.doc()): number {
    return Math.max(0, Math.round(seconds * doc.sample_rate));
  }

  /** A track by id or exact name (case does not matter). */
  private track(doc: SessionDoc, ref: string): TrackDoc {
    const byId = doc.tracks.find((t) => t.id === ref);
    if (byId) return byId;
    const hits = doc.tracks.filter((t) => key(t.name) === key(ref));
    if (hits.length === 1) return hits[0]!;
    if (hits.length > 1) throw new ToolError(`${hits.length} tracks are called "${ref}". Use an id: ${hits.map((t) => t.id).join(", ")}.`);
    throw new ToolError(`There is no track "${ref}". Tracks: ${doc.tracks.map((t) => `${t.name} (${t.id})`).join(", ") || "none"}.`);
  }

  private bus(doc: SessionDoc, ref: string): BusDoc {
    const buses = doc.buses ?? [];
    const byId = buses.find((b) => b.id === ref);
    if (byId) return byId;
    const hits = buses.filter((b) => key(b.name) === key(ref));
    if (hits.length === 1) return hits[0]!;
    if (hits.length > 1) throw new ToolError(`${hits.length} buses are called "${ref}". Use an id: ${hits.map((b) => b.id).join(", ")}.`);
    throw new ToolError(`There is no bus "${ref}". Buses: ${buses.map((b) => `${b.name} (${b.id})`).join(", ") || "none"}.`);
  }

  private strip(doc: SessionDoc, raw: unknown): Strip {
    const t = (raw ?? {}) as { kind?: string; id?: string };
    const id = str(t.id);
    if (t.kind === "master") {
      return { target: { kind: "master" }, name: "the Master", chain: chainOf(doc.master?.effects) };
    }
    if (!id) throw new ToolError(`target.id is needed for a ${t.kind ?? "target"}.`);
    if (t.kind === "track") {
      const track = this.track(doc, id);
      return { target: { kind: "track", id: track.id }, name: track.name, chain: chainOf(track.effects), track };
    }
    if (t.kind === "bus") {
      const bus = this.bus(doc, id);
      return { target: { kind: "bus", id: bus.id }, name: bus.name, chain: chainOf(bus.effects), bus };
    }
    if (t.kind === "clip") {
      for (const tr of doc.tracks) {
        const c = tr.clips.find((x) => x.id === id);
        if (c) return { target: { kind: "clip", id: c.id }, name: `clip ${c.name}`, chain: chainOf(c.effects) };
      }
      throw new ToolError(`There is no clip with id "${id}". get_session lists clip ids.`);
    }
    throw new ToolError(`target.kind must be track, clip, bus or master.`);
  }

  /** The effect slot named by slot_id or index. */
  private slotOf(s: Strip, args: Args) {
    const id = str(args.slot_id);
    const index = num(args.index);
    if (id === undefined && index === undefined) throw new ToolError("Give slot_id or index of the effect (get_chain lists them).");
    const at = id !== undefined ? s.chain.slots.findIndex((x) => x.id === id) : index!;
    const slot = s.chain.slots[at];
    if (!slot) {
      const n = s.chain.slots.length;
      throw new ToolError(id !== undefined ? `There is no effect with slot_id "${id}" on ${s.name}.` : `${s.name} has ${plural(n, "effect")}${n ? `, index 0 to ${n - 1}` : ""}.`);
    }
    return { slot, at };
  }

  private async effectInfo(ref: string): Promise<EffectInfo> {
    const all = await this.d.catalog();
    const hit = all.find((e) => e.id === ref) ?? all.find((e) => key(e.name) === key(ref));
    if (!hit) throw new ToolError(`There is no effect "${ref}". list_effects lists them.`);
    return hit;
  }

  private async nameOf(effect: string): Promise<string> {
    return (await this.d.catalog()).find((e) => e.id === effect)?.name ?? effect;
  }

  /** Sets `values` on a slot's params, kept in range. Unknown keys are an error naming the known ones. */
  private applyParams(info: EffectInfo, params: Record<string, number>, values: Record<string, unknown>, notes: string[]) {
    const controls = info.params.filter((p) => !p.hidden);
    for (const [k, raw] of Object.entries(values)) {
      const p = controls.find((x) => x.key === k) ?? controls.find((x) => key(x.name) === key(k));
      if (!p) throw new ToolError(`${info.name} has no control "${k}". Its controls: ${controls.map((x) => x.key).join(", ") || "none"}.`);
      const v = num(raw);
      if (v === undefined) throw new ToolError(`${p.key} must be a number.`);
      const kept = clampParam(p, v);
      if (Math.abs(kept - v) > Math.max(1e-6, p.step / 2)) notes.push(`${p.key} kept at ${formatParam(p, kept)} (asked ${v})`);
      params[p.key] = kept;
    }
  }

  /** Runs `body` as one undoable "Assistant: ..." step. */
  private async edit(label: string, body: (run: CommandRunner, doc: SessionDoc) => Promise<void> | void): Promise<void> {
    await this.d.history.execute(new AssistantStep(label, async (run) => body(run, this.doc())));
  }

  private setChain(run: CommandRunner, s: Strip, chain: EffectChainDoc): Promise<void> {
    return run.execute(this.d.session.command("Voice Chain", { op: "set_effects", target: s.target, chain } as Op));
  }

  private json(data: unknown, lead?: string): ToolResult {
    const text = JSON.stringify(data);
    return { text: lead ? `${lead}\n${text}` : text, structured: data, isError: false };
  }

  private ok(text: string, structured?: unknown): ToolResult {
    return structured === undefined ? { text, isError: false } : { text, structured, isError: false };
  }

  private paramsView(info: EffectInfo | undefined, params: Record<string, number>) {
    if (!info) return Object.entries(params).map(([k, value]) => ({ key: k, value }));
    return info.params
      .filter((p) => !p.hidden)
      .map((p) => {
        const value = paramValue({ id: "", effect: info.id, enabled: true, params, preset: null }, p);
        return { key: p.key, name: p.name, value, unit: p.unit || undefined, text: formatParam(p, value) };
      });
  }

  private controlView(p: EffectParamInfo) {
    return {
      key: p.key,
      name: p.name,
      unit: p.unit || undefined,
      min: p.min,
      max: p.max,
      default: p.default,
      step: p.step,
      ...(p.log ? { log_scale: true } : {}),
      ...(p.toggle ? { on_off: true } : {}),
      ...(p.choices?.length ? { choices: p.choices.map((c, i) => `${p.min + i} = ${c}`) } : {}),
      ...(p.group ? { group: p.group } : {}),
    };
  }

  // --- tools ---

  /** What the clip and automation tools use from here. */
  private clipContext(): ClipToolContext {
    const deps = () => this.d;
    return {
      get session() {
        return deps().session;
      },
      doc: () => this.doc(),
      track: (doc, ref) => this.track(doc, ref),
      bus: (doc, ref) => this.bus(doc, ref),
      edit: (label, body) => this.edit(label, body),
      catalog: () => this.d.catalog(),
      newId: () => this.d.newId(),
      error: (m) => new ToolError(m),
      noChange: (m) => new NoChange(m),
    };
  }

  private readonly handlers: Record<string, (a: Args) => Promise<ToolResult>> = {
    ...clipTools(this.clipContext()),
    get_session: async (a) => {
      const doc = this.doc();
      const withClips = bool(a.include_clips) ?? true;
      const catalog = await this.d.catalog().catch(() => [] as EffectInfo[]);
      const fxName = (id: string) => catalog.find((e) => e.id === id)?.name ?? id;
      const fx = (c: EffectChainDoc | undefined) => chainOf(c).slots.map((s) => ({ slot_id: s.id, effect: s.effect, name: fxName(s.effect), bypassed: !s.enabled }));
      const media = new Map(doc.media.map((m) => [m.id, m]));
      // Which settings move over time; get_automation gives the points.
      const lanes = (envs: EnvelopeDoc[]) =>
        envs.map((e) => {
          const t = e.target;
          const what = t.type === "effect_param" ? { parameter: "effect", slot_id: t.slot_id, control: t.param } : t.type === "space" ? { parameter: "space", space_setting: t.param } : { parameter: t.type };
          return { ...what, points: e.points.length, ...(e.enabled ? {} : { enabled: false }) };
        });
      const output = (o: TrackDoc["output"]) => (!o || o.type === "master" ? "master" : o.type === "bus" ? `bus ${o.bus_id}` : "none");
      const view = doc.view;
      const playing = this.d.transport.current.playing;
      const data = {
        name: doc.name,
        file: this.d.session.path,
        unsaved_changes: this.d.history.current.dirty,
        sample_rate: doc.sample_rate,
        bit_depth: doc.bit_depth,
        duration_seconds: this.sec(sessionEnd(doc), doc),
        playing,
        playhead_seconds: this.sec(playing ? this.d.transport.current.position : view.playhead, doc),
        selection: view.selection ? { start_seconds: this.sec(view.selection.start, doc), end_seconds: this.sec(view.selection.end, doc) } : null,
        tracks: doc.tracks.map((t) => ({
          id: t.id,
          name: t.name,
          kind: t.kind === "midi" ? "midi" : "audio",
          channels: t.channels,
          volume_db: t.volume_db,
          pan: t.pan,
          muted: t.muted,
          solo: t.solo,
          output: output(t.output),
          effects: fx(t.effects),
          ...(t.envelopes?.length ? { automation: lanes(t.envelopes), automation_mode: autoMode(t) } : {}),
          clip_count: t.clips.length,
          ...(withClips
            ? {
                clips: t.clips.map((c) => {
                  const m = media.get(c.media_id);
                  return {
                    id: c.id,
                    name: c.name,
                    start_seconds: this.sec(c.start, doc),
                    end_seconds: this.sec(c.start + clipLength(c, m, doc.sample_rate), doc),
                    file: m?.path ?? null,
                    gain_db: c.gain_db,
                    muted: c.muted,
                    ...(c.locked ? { locked: true } : {}),
                    color: colorName(c.color),
                    ...(c.group ? { group: c.group } : {}),
                    source_start_seconds: round(c.source_in / (m?.sample_rate || doc.sample_rate)),
                    source_end_seconds: round(c.source_out / (m?.sample_rate || doc.sample_rate)),
                    ...(m && m.length > 0 ? { file_duration_seconds: round(m.length / (m.sample_rate || doc.sample_rate)) } : {}),
                    ...(c.looped ? { looped: true } : {}),
                    ...(c.stretch && !c.looped ? { stretch: { ratio: round(stretchOf(c), 4), preserve_pitch: c.stretch.keep_pitch, ...(c.stretch.pitch ? { pitch_semitones: c.stretch.pitch } : {}) } } : {}),
                    ...(c.fade_in.length > 0 ? { fade_in: { seconds: this.sec(c.fade_in.length, doc), shape: c.fade_in.shape, bend: c.fade_in.curve } } : {}),
                    ...(c.fade_out.length > 0 ? { fade_out: { seconds: this.sec(c.fade_out.length, doc), shape: c.fade_out.shape, bend: c.fade_out.curve } } : {}),
                    ...(c.envelopes?.length ? { automation: lanes(c.envelopes) } : {}),
                    ...(chainOf(c.effects).slots.length ? { effects: fx(c.effects) } : {}),
                  };
                }),
              }
            : {}),
        })),
        buses: (doc.buses ?? []).map((b) => ({ id: b.id, name: b.name, volume_db: b.volume_db, pan: b.pan, muted: b.muted, solo: b.solo, output: output(b.output), effects: fx(b.effects), ...((b.envelopes as EnvelopeDoc[] | undefined)?.length ? { automation: lanes(b.envelopes as EnvelopeDoc[]), automation_mode: autoMode(b) } : {}) })),
        master: { volume_db: doc.master?.volume_db ?? 0, effects: fx(doc.master?.effects) },
        markers: [...doc.markers]
          .sort((x, y) => x.position - y.position)
          .map((m) => ({ id: m.id, name: m.name, kind: m.kind, start_seconds: this.sec(m.position, doc), ...(m.duration > 0 ? { duration_seconds: this.sec(m.duration, doc) } : {}) })),
      };
      return this.json(data);
    },

    list_effects: async (a) => {
      const all = await this.d.catalog();
      const one = str(a.effect);
      if (one) {
        const info = await this.effectInfo(one);
        return this.json({
          id: info.id,
          name: info.name,
          group: info.category,
          description: info.description,
          ...(info.plugin ? { plugin: true } : {}),
          ...(info.sidechain ? { sidechain: true } : {}),
          controls: info.params.filter((p) => !p.hidden).map((p) => this.controlView(p)),
          presets: this.d.effectPresets(info.id).map((p) => p.name),
        });
      }
      const { saved } = await this.d.chainPresets().catch(() => ({ saved: [] as ChainPreset[] }));
      const preset = (p: ChainPreset, mine: boolean) => ({
        name: p.name,
        adds: addsOne(p) ? "one effect, added to the end" : "a whole chain, replacing the current one",
        ...(PRESET_INFO[p.name] ? { about: PRESET_INFO[p.name]!.blurb } : {}),
        ...(mine ? { saved_by_person: true } : {}),
      });
      return this.json({
        effects: all.filter((e) => !e.plugin).map((e) => ({ id: e.id, name: e.name, group: e.category, description: e.description })),
        plugins: all.filter((e) => e.plugin).map((e) => ({ id: e.id, name: e.name, group: e.category })),
        voice_chain_presets: [...saved.map((p) => preset(p, true)), ...BUILTIN_PRESETS.filter((p) => !p.trackOnly || true).map((p) => preset(p, false))],
      });
    },

    get_chain: async (a) => {
      const doc = this.doc();
      const s = this.strip(doc, a.target);
      const catalog = await this.d.catalog();
      return this.json({
        target: s.target,
        name: s.name,
        enabled: s.chain.enabled,
        input_gain_db: s.chain.input_gain_db,
        output_gain_db: s.chain.output_gain_db,
        mix: s.chain.mix,
        effects: s.chain.slots.map((slot, index) => {
          const info = catalog.find((e) => e.id === slot.effect);
          return {
            slot_id: slot.id,
            index,
            effect: slot.effect,
            name: info?.name ?? slot.effect,
            bypassed: !slot.enabled,
            preset: slot.preset,
            controls: this.paramsView(info, slot.params),
          };
        }),
      });
    },

    add_effect: async (a) => {
      const info = await this.effectInfo(String(a.effect));
      const notes: string[] = [];
      let made = { id: "", at: 0, strip: "" };
      const s0 = this.strip(this.doc(), a.target);
      await this.edit(`Add ${info.name} to ${s0.name}`, async (run, doc) => {
        const s = this.strip(doc, a.target);
        if (s.chain.slots.length >= MAX_SLOTS) throw new ToolError(`The Voice Chain on ${s.name} is full (${MAX_SLOTS} effects).`);
        const slot = newSlot(info, this.d.newId());
        const presetName = str(a.preset);
        if (presetName) {
          const p = this.d.effectPresets(info.id).find((x) => key(x.name) === key(presetName));
          if (!p) throw new ToolError(`${info.name} has no preset "${presetName}". Its presets: ${this.d.effectPresets(info.id).map((x) => x.name).join(", ") || "none"}.`);
          this.applyParams(info, slot.params, p.params, []);
          slot.preset = p.name;
          if (p.data) slot.data = p.data;
        }
        if (a.params) {
          this.applyParams(info, slot.params, a.params as Args, notes);
          if (Object.keys(a.params as Args).length) slot.preset = null;
        }
        const index = num(a.index);
        const next = addSlot(s.chain, slot, index === undefined ? undefined : Math.min(index, s.chain.slots.length));
        if (!next) throw new ToolError(`The Voice Chain on ${s.name} is full.`);
        await this.setChain(run, s, next);
        made = { id: slot.id, at: next.slots.findIndex((x) => x.id === slot.id), strip: s.name };
      });
      return this.ok(`Added ${info.name} to ${made.strip} at index ${made.at} (slot_id ${made.id}).${notes.length ? ` Note: ${notes.join("; ")}.` : ""}`, { slot_id: made.id, index: made.at });
    },

    remove_effect: async (a) => {
      const doc = this.doc();
      const s = this.strip(doc, a.target);
      const { slot } = this.slotOf(s, a);
      const name = await this.nameOf(slot.effect);
      await this.edit(`Remove ${name} from ${s.name}`, async (run, d) => {
        const now = this.strip(d, a.target);
        await this.setChain(run, now, removeSlot(now.chain, slot.id));
      });
      return this.ok(`Removed ${name} from ${s.name}.`);
    },

    set_effect_params: async (a) => {
      const doc = this.doc();
      const s = this.strip(doc, a.target);
      const { slot } = this.slotOf(s, a);
      const info = await this.effectInfo(slot.effect);
      const notes: string[] = [];
      const changed: string[] = [];
      await this.edit(`Set ${info.name} on ${s.name}`, async (run, d) => {
        const now = this.strip(d, a.target);
        const cur = now.chain.slots.find((x) => x.id === slot.id);
        if (!cur) throw new ToolError("That effect is no longer in the chain.");
        const params = { ...cur.params };
        this.applyParams(info, params, a.params as Args, notes);
        for (const p of info.params) if (params[p.key] !== undefined && params[p.key] !== cur.params[p.key]) changed.push(`${p.key} ${formatParam(p, params[p.key]!)}`);
        if (changed.length === 0) throw new NoChange(`${info.name} on ${s.name} already has those settings.`);
        await this.setChain(run, now, setSlot(now.chain, slot.id, { params, preset: null }));
      });
      return this.ok(`${info.name} on ${s.name}: ${changed.join(", ")}.${notes.length ? ` Note: ${notes.join("; ")}.` : ""}`);
    },

    move_effect: async (a) => {
      const doc = this.doc();
      const s = this.strip(doc, a.target);
      const { slot, at } = this.slotOf(s, a);
      const to = Math.min(num(a.to_index) ?? 0, s.chain.slots.length - 1);
      const name = await this.nameOf(slot.effect);
      if (to === at) return this.ok(`${name} is already at index ${at}.`);
      await this.edit(`Move ${name} on ${s.name}`, async (run, d) => {
        const now = this.strip(d, a.target);
        const moved = moveSlotTo(now.chain, slot.id, to);
        // The slot shows where its new place in the list puts it.
        await this.setChain(run, now, { ...moved, slots: moved.slots.map((x) => (x.id === slot.id ? { ...x, position: null } : x)) });
      });
      return this.ok(`Moved ${name} on ${s.name} from index ${at} to ${to}.`);
    },

    set_effect_bypass: async (a) => {
      const doc = this.doc();
      const s = this.strip(doc, a.target);
      const bypass = a.bypass === true;
      if (a.slot_id === undefined && a.index === undefined) {
        if (s.chain.enabled === !bypass) return this.ok(`The Voice Chain on ${s.name} is already ${bypass ? "bypassed" : "on"}.`);
        await this.edit(`${bypass ? "Bypass" : "Turn On"} Voice Chain on ${s.name}`, async (run, d) => {
          const now = this.strip(d, a.target);
          await this.setChain(run, now, { ...now.chain, enabled: !bypass });
        });
        return this.ok(`The Voice Chain on ${s.name} is ${bypass ? "bypassed" : "on"}.`);
      }
      const { slot } = this.slotOf(s, a);
      const name = await this.nameOf(slot.effect);
      if (slot.enabled === !bypass) return this.ok(`${name} on ${s.name} is already ${bypass ? "bypassed" : "on"}.`);
      await this.edit(`${bypass ? "Bypass" : "Turn On"} ${name} on ${s.name}`, async (run, d) => {
        const now = this.strip(d, a.target);
        await this.setChain(run, now, setSlot(now.chain, slot.id, { enabled: !bypass }));
      });
      return this.ok(`${name} on ${s.name} is ${bypass ? "bypassed" : "on"}.`);
    },

    apply_chain_preset: async (a) => {
      const doc = this.doc();
      const s = this.strip(doc, a.target);
      const { saved, known } = await this.d.chainPresets();
      const want = String(a.preset);
      const p = [...saved, ...BUILTIN_PRESETS].find((x) => x.name === want) ?? [...saved, ...BUILTIN_PRESETS].find((x) => key(x.name) === key(want));
      if (!p) throw new ToolError(`There is no Voice Chain preset "${want}". list_effects lists them.`);
      let count = 0;
      await this.edit(`Apply ${p.name} to ${s.name}`, async (run, d) => {
        const now = this.strip(d, a.target);
        const next = applyPreset(now.chain, p, known, () => this.d.newId());
        if (!next) throw new ToolError(`The Voice Chain on ${s.name} is full.`);
        count = next.slots.length;
        await this.setChain(run, now, next);
      });
      return this.ok(`Applied ${p.name} to ${s.name}; its Voice Chain now has ${plural(count, "effect")}.`);
    },

    transport: async (a) => {
      const doc = this.doc();
      const t = this.d.transport;
      const pos = num(a.position_seconds);
      switch (a.action) {
        case "play":
          await t.play(pos === undefined ? undefined : this.samples(pos, doc));
          return this.ok(`Playing from ${this.sec(t.current.position, doc)} s.`);
        case "pause":
          await t.pause();
          return this.ok(`Paused at ${this.sec(this.d.session.current?.view.playhead ?? 0, doc)} s.`);
        case "stop":
          await t.stop();
          return this.ok(`Stopped. The playhead is at ${this.sec(this.d.session.current?.view.playhead ?? 0, doc)} s.`);
        case "seek": {
          if (pos === undefined) throw new ToolError("seek needs position_seconds.");
          await t.seek(this.samples(pos, doc));
          return this.ok(`The playhead is at ${round(pos)} s.`);
        }
        case "set_selection": {
          const start = num(a.start_seconds);
          const end = num(a.end_seconds);
          if (start === undefined || end === undefined) throw new ToolError("set_selection needs start_seconds and end_seconds.");
          if (end <= start) throw new ToolError("end_seconds must be after start_seconds.");
          this.d.session.setView({ ...doc.view, selection: { start: this.samples(start, doc), end: this.samples(end, doc) } });
          await this.d.session.flushView();
          return this.ok(`Selected ${round(start)} s to ${round(end)} s.`);
        }
        case "clear_selection":
          this.d.session.setView({ ...doc.view, selection: null });
          await this.d.session.flushView();
          return this.ok("The time selection is cleared.");
        default:
          throw new ToolError("action must be play, pause, stop, seek, set_selection or clear_selection.");
      }
    },

    list_markers: async () => {
      const doc = this.doc();
      return this.json({
        markers: [...doc.markers]
          .sort((x, y) => x.position - y.position)
          .map((m) => ({ id: m.id, name: m.name, kind: m.kind, start_seconds: this.sec(m.position, doc), duration_seconds: this.sec(m.duration, doc), description: m.description })),
      });
    },

    add_marker: async (a) => {
      const id = this.d.newId();
      let name = "";
      const kind = (str(a.kind) ?? "cue") as MarkerKind;
      const isRange = (num(a.duration_seconds) ?? 0) > 0;
      await this.edit(isRange ? "Add Range Marker" : kind === "chapter" ? "Add Chapter Marker" : "Add Marker", async (run, doc) => {
        name = str(a.name)?.trim() || nextMarkerName(doc.markers);
        const m = newMarker(id, name, this.samples(num(a.position_seconds) ?? 0, doc), this.samples(num(a.duration_seconds) ?? 0, doc), kind);
        const desc = str(a.description);
        await run.execute(this.d.session.command("Add Marker", { op: "add_markers", markers: [desc ? { ...m, description: desc } : m] }));
      });
      return this.ok(`Added marker "${name}" at ${round(num(a.position_seconds) ?? 0)} s (id ${id}).`, { id });
    },

    remove_marker: async (a) => {
      const doc = this.doc();
      const ids = [...new Set((a.marker_ids as string[]) ?? [])];
      const missing = ids.filter((id) => !doc.markers.some((m) => m.id === id));
      if (missing.length) throw new ToolError(`No marker has the id ${missing.join(", ")}. list_markers lists them.`);
      await this.edit(ids.length === 1 ? "Remove Marker" : `Remove ${ids.length} Markers`, async (run) => {
        await run.execute(this.d.session.command("Remove Markers", { op: "remove_markers", ids }));
      });
      return this.ok(`Removed ${plural(ids.length, "marker")}.`);
    },

    set_mix: async (a) => {
      const doc = this.doc();
      const s = this.strip(doc, a.target);
      if (s.target.kind === "clip") throw new ToolError("set_mix works on a track, a bus or the master. Clip level lives in its gain.");
      const vol = num(a.volume_db);
      const pan = num(a.pan);
      const mute = bool(a.mute);
      const solo = bool(a.solo);
      if (vol === undefined && pan === undefined && mute === undefined && solo === undefined) throw new ToolError("Give volume_db, pan, mute or solo.");
      const parts: string[] = [];
      if (vol !== undefined) parts.push(`volume ${round(vol, 1)} dB`);
      if (pan !== undefined) parts.push(`pan ${round(pan, 0)}`);
      if (mute !== undefined) parts.push(mute ? "muted" : "unmuted");
      if (solo !== undefined) parts.push(solo ? "solo on" : "solo off");
      const label = `Set ${s.target.kind === "master" ? "Master" : s.name} ${vol !== undefined && pan === undefined && mute === undefined && solo === undefined ? "Volume" : "Mix"}`;
      await this.edit(label, async (run, d) => {
        const now = this.strip(d, a.target);
        let op: Op;
        if (now.target.kind === "master") {
          if (pan !== undefined || mute !== undefined || solo !== undefined) throw new ToolError("The Master has volume only.");
          op = { op: "set_master_volume", volume_db: vol! };
        } else {
          const changes = { ...(vol !== undefined ? { volume_db: vol } : {}), ...(pan !== undefined ? { pan } : {}), ...(mute !== undefined ? { muted: mute } : {}), ...(solo !== undefined ? { solo } : {}) };
          op = now.target.kind === "track" ? { op: "set_track", id: now.target.id, changes } : { op: "set_bus", id: now.target.id, changes };
        }
        await run.execute(this.d.session.command("Mix", op));
      });
      return this.ok(`${s.target.kind === "master" ? "Master" : s.name}: ${parts.join(", ")}.`);
    },

    measure_loudness: async (a) => {
      const doc = this.doc();
      const req: MixdownRequest = {};
      const start = num(a.start_seconds);
      const end = num(a.end_seconds);
      if (start !== undefined || end !== undefined) {
        const from = this.samples(start ?? 0, doc);
        const to = end === undefined ? sessionEnd(doc) : this.samples(end, doc);
        if (to <= from) throw new ToolError("end_seconds must be after start_seconds.");
        req.range = [from, to];
      }
      const tr = str(a.track);
      if (tr) req.track = this.track(doc, tr).id;
      const r = await this.d.measure(req);
      if (r.cancelled) throw new ToolError("The measurement was cancelled in the editor.");
      const fix = (v: number | null | undefined) => (typeof v === "number" && Number.isFinite(v) ? round(v, 1) : null);
      const data = {
        integrated_lufs: fix(r.integrated),
        integrated_mono_lufs: fix(r.integratedMono),
        loudness_range_lu: fix(r.range),
        true_peak_dbtp: fix(r.truePeak),
        sample_peak_dbfs: fix(r.samplePeak),
        rms_dbfs: fix(r.rms),
        noise_floor_dbfs: fix(r.noiseFloor),
        seconds: round(r.seconds, 2),
      };
      const lead = data.integrated_lufs === null ? "Too short or silent to measure." : `Integrated ${data.integrated_lufs} LUFS, true peak ${data.true_peak_dbtp} dBTP, range ${data.loudness_range_lu} LU.`;
      return this.json(data, lead);
    },

    export_mixdown: async (a) => {
      const doc = this.doc();
      const path = String(a.path);
      if (!isAbsolute(path)) throw new ToolError("path must be absolute, such as C:\\Users\\me\\Music\\episode.mp3.");
      const ext = (path.match(/\.([A-Za-z0-9]+)$/)?.[1] ?? "").toLowerCase();
      const kind = (str(a.format) ?? ext) as FormatKind;
      if (!["wav", "flac", "mp3", "m4a"].includes(kind)) throw new ToolError("format must be wav, flac, mp3 or m4a (or give a path ending in one of those).");
      if (ext !== kind) throw new ToolError(`The path should end in .${kind}.`);
      if (!formatAvailable(kind, this.d.platform)) throw new ToolError(`${kind.toUpperCase()} export needs Windows. Use wav, flac or mp3.`);
      if (a.overwrite !== true && (await this.d.pathsExist([path]))[0]) throw new ToolError("That file already exists. Pass overwrite: true to replace it, or pick another path.");
      const bits = num(a.bit_depth);
      const format: OutputFormat = {
        ...DEFAULT_FORMAT,
        kind,
        bitDepth: kind === "wav" || kind === "flac" ? (kind === "flac" && bits === 32 ? 24 : (bits ?? 24)) : null,
        sampleRate: num(a.sample_rate) ?? null,
        channels: (num(a.channels) as 1 | 2 | undefined) ?? 2,
        mp3Bitrate: num(a.mp3_bitrate) ?? 128,
      };
      const req: MixdownRequest = { format };
      const start = num(a.start_seconds);
      const end = num(a.end_seconds);
      if (start !== undefined || end !== undefined) {
        const from = this.samples(start ?? 0, doc);
        const to = end === undefined ? sessionEnd(doc) : this.samples(end, doc);
        if (to <= from) throw new ToolError("end_seconds must be after start_seconds.");
        req.range = [from, to];
      } else if (sessionEnd(doc) === 0) {
        throw new ToolError("The session has no audio to export.");
      }
      const lufs = num(a.loudness_lufs);
      if (lufs !== undefined) req.normalize = { lufs, peak: num(a.true_peak_db) ?? -1 };
      const r = await this.d.exportMix(path, req);
      if (r.cancelled) throw new ToolError("The export was cancelled in the editor.");
      const seconds = r.frames / Math.max(1, r.sampleRate);
      const clipped = r.peak > 1 ? " The mix went over full scale and clipped; lower the Master or use loudness_lufs." : "";
      return this.ok(`Exported ${round(seconds, 1)} s to ${path}.${clipped}`, { path, seconds: round(seconds, 2), sample_rate: r.sampleRate, peak: round(r.peak, 4) });
    },

    save_session: async (a) => {
      this.doc();
      const path = str(a.path);
      if (path !== undefined) {
        if (!isAbsolute(path)) throw new ToolError("path must be absolute.");
        if (!/\.altaudio$/i.test(path)) throw new ToolError("A session file ends in .altaudio.");
      } else if (!this.d.session.path) {
        throw new ToolError("The session has never been saved. Give path (absolute, ending in .altaudio).");
      }
      const saved = await this.d.save(path ?? null);
      if (!saved) throw new ToolError("The session was not saved.");
      return this.ok(`Saved to ${saved}.`, { path: saved });
    },

    open_session: async (a) => {
      const path = String(a.path);
      if (!isAbsolute(path)) throw new ToolError("path must be absolute.");
      if (!/\.altaudio$/i.test(path)) throw new ToolError("open_session opens .altaudio session files. Use import_audio for audio files.");
      if (!(await this.d.pathsExist([path]))[0]) throw new ToolError(`There is no file at ${path}.`);
      if (!(await this.d.open(path))) throw new ToolError("The session did not open (the person may have cancelled).");
      const doc = this.doc();
      return this.ok(`Opened ${doc.name}: ${plural(doc.tracks.length, "track")}. Call get_session for its ids.`);
    },

    import_audio: async (a) => {
      const doc0 = this.doc();
      const paths = (a.paths as string[]).map((p) => p.trim());
      const bad = paths.filter((p) => !isAbsolute(p));
      if (bad.length) throw new ToolError(`Paths must be absolute: ${bad.join(", ")}.`);
      const exists = await this.d.pathsExist(paths);
      const gone = paths.filter((_, i) => !exists[i]);
      if (gone.length) throw new ToolError(`No file at ${gone.join(", ")}.`);
      const trackRef = str(a.track);
      const toNew = a.new_track === true || !trackRef;
      let target = toNew ? null : this.track(doc0, trackRef!);
      if (target && target.kind === "midi") throw new ToolError(`${target.name} is a MIDI track; audio needs an audio track.`);
      const at = num(a.position_seconds) ?? 0;
      const base = (p: string) => p.split(/[\\/]/).pop() ?? p;
      const first = base(paths[0]!);
      const where = target ? target.name : "a new track";
      let placed: { track: string; clips: { id: string; name: string; start_seconds: number; end_seconds: number }[] } = { track: "", clips: [] };
      await this.edit(paths.length === 1 ? `Import ${first} to ${where}` : `Import ${paths.length} Files to ${where}`, async (run, doc) => {
        const imported = await this.d.importMedia(paths, run);
        await this.d.mediaIdle();
        const items = imported.ids.map((id) => this.d.media(id));
        const failed = items.map((it, i) => (it?.status === "ready" && it.info ? null : `${base(paths[i] ?? "")}${it?.error ? ` (${it.error})` : ""}`)).filter((x): x is string => x !== null);
        if (failed.length) throw new ToolError(`These could not be read as audio: ${failed.join(", ")}.`);
        const ops: Op[] = [];
        const now = this.d.session.current ?? doc;
        const known = new Set(now.media.map((m) => m.id));
        let trackId = target?.id ?? "";
        if (!target) {
          trackId = this.d.newId();
          const stereo = items.some((it) => (it!.info!.channels ?? 2) > 1);
          ops.push({ op: "add_track", id: trackId, name: first.replace(/\.[^.]+$/, ""), stereo, index: now.tracks.length });
        }
        let pos = this.samples(at, now);
        const clips = [];
        for (const it of items) {
          const info = it!.info!;
          if (!known.has(it!.id)) {
            known.add(it!.id);
            ops.push({ op: "add_media", media: { id: it!.id, path: it!.path, relative_path: null, sample_rate: info.sampleRate, channels: info.channels, length: info.length } });
          }
          const len = Math.round((info.length * now.sample_rate) / info.sampleRate);
          const clip = { id: this.d.newId(), name: it!.name.replace(/\.[^.]+$/, ""), media_id: it!.id, start: pos, source_in: 0, source_out: info.length };
          clips.push(clip);
          placed.clips.push({ id: clip.id, name: clip.name, start_seconds: this.sec(pos, now), end_seconds: this.sec(pos + len, now) });
          pos += len;
        }
        ops.push({ op: "add_clips", track_id: trackId, clips });
        await run.execute(this.d.session.command("Add Clips", { op: "batch", ops }));
        placed = { ...placed, track: trackId };
        target = this.d.session.current?.tracks.find((t) => t.id === trackId) ?? target;
      });
      return this.ok(`Placed ${plural(placed.clips.length, "file")} on ${target?.name ?? "a new track"} (track id ${placed.track}).`, placed);
    },

    undo: async () => {
      const h = this.d.history.current;
      if (!h.canUndo) throw new ToolError("There is nothing to undo.");
      if (!h.undoLabel?.startsWith("Assistant: ")) throw new ToolError(`The last step, "${h.undoLabel}", was made by the person. The assistant only undoes its own steps.`);
      const label = h.undoLabel;
      await this.d.history.undo();
      return this.ok(`Undid "${label}".`);
    },

    redo: async () => {
      const h = this.d.history.current;
      if (!h.canRedo) throw new ToolError("There is nothing to redo.");
      if (!h.redoLabel?.startsWith("Assistant: ")) throw new ToolError(`The next step to redo, "${h.redoLabel}", was the person's. The assistant only redoes its own steps.`);
      const label = h.redoLabel;
      await this.d.history.redo();
      return this.ok(`Redid "${label}".`);
    },

    // Served by the core; listed so every tool has a handler here.
    get_activity_log: async () => this.ok("The activity log is kept by the core."),
  };

  /** Tool names this dispatcher handles (tests compare with tools.json). */
  get handled(): string[] {
    return Object.keys(this.handlers);
  }
}

/** An absolute path on Windows (C:\, \\server\share) or elsewhere (/). */
export function isAbsolute(p: string): boolean {
  return /^[A-Za-z]:[\\/]/.test(p) || /^\\\\[^\\]+\\/.test(p) || p.startsWith("/");
}
