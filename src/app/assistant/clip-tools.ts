// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) The GeekCast Radio Network, LLC.
// This file is part of Altered Audio Editor.

/**
 * Assistant tools for clip editing and automation: split, delete, move,
 * trim, gain, fades, and volume, pan and effect settings over time.
 *
 * Each edit is built from the same functions the Editor's own gestures
 * use (clip-edit.ts, automation.ts), so the result is what a person would
 * get doing it by hand. Times the assistant gives and sees are seconds
 * from the session start; clip envelopes are stored in samples from the
 * clip's left edge, track and bus lanes in samples on the timeline.
 */

import type { EffectInfo, EffectParamInfo } from "../core";
import type { CommandRunner } from "../../stores/media-pool";
import { clipLength, type ClipChange, type ClipDoc, type EnvelopeDoc, type EnvelopePointDoc, type FadeDoc, type Op, type SessionDoc, type SessionStore, type TrackDoc, type BusDoc } from "../../stores/session";
import { clipEnd, fadeClip, findClips, LockedError, mediaOf, rippleDeleteClips, splitClips, trimClip, withLayers } from "../../panels/editor/clip-edit";
import { autoMode, targetKey, type AutoTarget } from "../../panels/editor/automation";
import { FADE_SHAPES } from "../../panels/editor/clip-edit";
import { naturalLength, stretchTo, unstretch } from "../../panels/editor/stretch";
import { PITCH_MAX, PITCH_MIN, STRETCH_MAX, STRETCH_MIN, stretchOf } from "../../stores/session";

type Args = Record<string, unknown>;

export interface ToolResultLike {
  text: string;
  structured?: unknown;
  isError: boolean;
}

/** What the clip tools need from the dispatcher. */
export interface ClipToolContext {
  session: SessionStore;
  doc(): SessionDoc;
  track(doc: SessionDoc, ref: string): TrackDoc;
  bus(doc: SessionDoc, ref: string): BusDoc;
  edit(label: string, body: (run: CommandRunner, doc: SessionDoc) => Promise<void> | void): Promise<void>;
  catalog(): Promise<EffectInfo[]>;
  newId(): string;
  error(message: string): Error;
  noChange(message: string): Error;
}

const round = (v: number, digits = 3) => Number(v.toFixed(digits));
const plural = (n: number, one: string, many = `${one}s`) => `${n} ${n === 1 ? one : many}`;
const str = (v: unknown): string | undefined => (typeof v === "string" ? v : undefined);
const num = (v: unknown): number | undefined => (typeof v === "number" && Number.isFinite(v) ? v : undefined);
const bool = (v: unknown): boolean | undefined => (typeof v === "boolean" ? v : undefined);
const key = (s: string) => s.trim().toLowerCase();

/** Seconds to session samples (never below 0). */
export const toSamples = (seconds: number, rate: number) => Math.max(0, Math.round(seconds * rate));
/** Session samples to seconds, to the millisecond. */
export const toSeconds = (samples: number, rate: number) => round(samples / rate);

// --- Automation values ---

/** Lowest and highest volume an envelope or lane takes, in dB. -60 is silence. */
export const VOL_MIN = -60;
export const VOL_MAX = 12;

/** What a lane moves, as the assistant names it. */
export type ParamRef = { parameter: "volume" } | { parameter: "pan" } | { parameter: "effect"; slot_id: string; control: string };

/** The kind of thing that owns envelopes. */
type OwnerKind = "clip" | "track" | "bus";

interface Owner {
  kind: OwnerKind;
  id: string;
  /** How labels and answers name it. */
  name: string;
  /** The clip and its track, for clip envelopes. */
  clip?: ClipDoc;
  track?: TrackDoc;
  bus?: BusDoc;
  envelopes: EnvelopeDoc[];
  effects: { slots: { id: string; effect: string; params: Record<string, number> }[] };
  /** Where the clip starts and ends on the timeline (clips only). */
  span?: [number, number];
}

interface Resolved {
  target: AutoTarget;
  /** "Volume", "Pan", "Limiter Ceiling". */
  label: string;
  unit: string;
  min: number;
  max: number;
}

/**
 * Merges new points into a lane. `replace_range` drops the old points
 * from the first new point to the last and puts the new ones there;
 * `replace_all` keeps only the new ones; `add` keeps every old point not
 * at the same position as a new one. The result is in time order.
 */
export function mergePoints(existing: readonly EnvelopePointDoc[], incoming: readonly EnvelopePointDoc[], mode: "replace_range" | "replace_all" | "add"): EnvelopePointDoc[] {
  const fresh = new Map<number, EnvelopePointDoc>();
  for (const p of incoming) fresh.set(p.position, p);
  const news = [...fresh.values()];
  let kept: EnvelopePointDoc[];
  if (mode === "replace_all" || news.length === 0) kept = mode === "replace_all" ? [] : [...existing];
  else if (mode === "add") kept = existing.filter((p) => !fresh.has(p.position));
  else {
    const lo = Math.min(...news.map((p) => p.position));
    const hi = Math.max(...news.map((p) => p.position));
    kept = existing.filter((p) => p.position < lo || p.position > hi);
  }
  return [...kept, ...news].sort((a, b) => a.position - b.position);
}

/** The points outside `from` to `to` (inclusive). */
export function removeRange(points: readonly EnvelopePointDoc[], from: number, to: number): EnvelopePointDoc[] {
  return points.filter((p) => p.position < from || p.position > to);
}

const FADE_IDS = FADE_SHAPES.map((s) => s.id);

/** The clip palette (Clip Color dialog), by name, in its order: palette entry N is the token --clip-N. */
export const CLIP_COLORS = ["green", "blue", "purple", "orange", "yellow", "pink"] as const;

/** A clip color as the assistant names it: a palette name, "#rrggbb", or null for the track's color. */
export function colorName(color: string | null | undefined): string | null {
  if (!color) return null;
  const m = /^--clip-([1-6])$/.exec(color);
  return m ? CLIP_COLORS[Number(m[1]) - 1]! : color;
}

/** What the clip change takes for a color name: the palette token, the hex, or "" for the track color. Null when it is not a color. */
export function colorValue(name: string): string | null {
  const n = name.trim().toLowerCase();
  if (n === "track" || n === "none" || n === "default") return "";
  const i = CLIP_COLORS.indexOf(n as (typeof CLIP_COLORS)[number]);
  if (i >= 0) return `--clip-${i + 1}`;
  if (/^[1-6]$/.test(n)) return `--clip-${n}`;
  if (/^#[0-9a-f]{6}$/.test(n)) return n;
  return null;
}
const fadeName = (id: string) => FADE_SHAPES.find((s) => s.id === id)?.label ?? id;

export function clipTools(c: ClipToolContext): Record<string, (a: Args) => Promise<ToolResultLike>> {
  const ok = (text: string, structured?: unknown): ToolResultLike => (structured === undefined ? { text, isError: false } : { text, structured, isError: false });
  const fail = (m: string) => c.error(m);

  /** A clip by id, with its track. */
  function clipOf(doc: SessionDoc, id: string): { clip: ClipDoc; track: TrackDoc } {
    for (const track of doc.tracks) {
      const clip = track.clips.find((x) => x.id === id);
      if (clip) return { clip, track };
      if ((track.midi_clips ?? []).some((x) => x.id === id)) throw fail(`${id} is a MIDI clip; these tools work on audio clips.`);
    }
    throw fail(`There is no clip with id "${id}". get_session lists clip ids.`);
  }

  const span = (doc: SessionDoc, clip: ClipDoc): [number, number] => [clip.start, clipEnd(clip, mediaOf(doc), doc.sample_rate)];
  const secs = (doc: SessionDoc, n: number) => toSeconds(n, doc.sample_rate);
  const clipView = (doc: SessionDoc, clip: ClipDoc) => {
    const [s, e] = span(doc, clip);
    return { id: clip.id, name: clip.name, start_seconds: secs(doc, s), end_seconds: secs(doc, e) };
  };

  function owner(doc: SessionDoc, raw: unknown): Owner {
    const t = (raw ?? {}) as { kind?: string; id?: string };
    if (t.kind === "master") throw fail("The Master has no automation lanes. Automate the tracks, or route them to a bus and automate the bus.");
    const id = str(t.id);
    if (!id) throw fail(`target.id is needed for a ${t.kind ?? "target"}.`);
    if (t.kind === "clip") {
      const { clip, track } = clipOf(doc, id);
      return { kind: "clip", id: clip.id, name: `clip ${clip.name}`, clip, track, envelopes: clip.envelopes ?? [], effects: { slots: clip.effects?.slots ?? [] }, span: span(doc, clip) };
    }
    if (t.kind === "track") {
      const track = c.track(doc, id);
      if (track.kind === "midi") throw fail(`${track.name} is a MIDI track. Its instrument's sound can be automated on a bus it feeds.`);
      return { kind: "track", id: track.id, name: track.name, track, envelopes: track.envelopes ?? [], effects: { slots: track.effects?.slots ?? [] } };
    }
    if (t.kind === "bus") {
      const bus = c.bus(doc, id);
      return { kind: "bus", id: bus.id, name: bus.name, bus, envelopes: (bus.envelopes as EnvelopeDoc[] | undefined) ?? [], effects: { slots: bus.effects?.slots ?? [] } };
    }
    throw fail("target.kind must be track, clip or bus.");
  }

  /** Effect controls that can move over time on this owner (as the Editor offers them). */
  function movable(o: Owner, p: EffectParamInfo): boolean {
    if (p.hidden || p.retimes) return false;
    // Track and bus lanes glide; on/off and list controls do not.
    if (o.kind !== "clip" && (p.toggle || p.choices?.length)) return false;
    return true;
  }

  function choices(o: Owner, catalog: readonly EffectInfo[]) {
    const out: (Resolved & { ref: ParamRef })[] = [
      { ref: { parameter: "volume" }, target: { type: "volume" }, label: "Volume", unit: "dB", min: VOL_MIN, max: VOL_MAX },
      { ref: { parameter: "pan" }, target: { type: "pan" }, label: "Pan", unit: "", min: -100, max: 100 },
    ];
    for (const slot of o.effects.slots) {
      const info = catalog.find((e) => e.id === slot.effect);
      for (const p of info?.params ?? []) {
        if (!movable(o, p)) continue;
        out.push({ ref: { parameter: "effect", slot_id: slot.id, control: p.key }, target: { type: "effect_param", slot_id: slot.id, param: p.key }, label: `${info!.name} ${p.name}`, unit: p.unit, min: p.min, max: p.max });
      }
    }
    return out;
  }

  function resolve(doc: SessionDoc, o: Owner, a: Args, catalog: readonly EffectInfo[]): Resolved {
    const which = str(a.parameter);
    if (which === "volume") return { target: { type: "volume" }, label: "Volume", unit: "dB", min: VOL_MIN, max: VOL_MAX };
    if (which === "pan") {
      if (o.kind !== "clip" && doc.space) throw fail("In a 3D Space session pan does nothing on tracks and buses; their place in the room is set in the 3D Space panel.");
      return { target: { type: "pan" }, label: "Pan", unit: "", min: -100, max: 100 };
    }
    if (which !== "effect") throw fail('parameter must be "volume", "pan" or "effect".');
    const slotId = str(a.slot_id);
    const control = str(a.control);
    if (!slotId || !control) throw fail('parameter "effect" needs slot_id (from get_chain) and control (the control\'s key).');
    const slot = o.effects.slots.find((s) => s.id === slotId);
    if (!slot) {
      const ids = o.effects.slots.map((s) => s.id);
      throw fail(`${o.name} has no effect with slot_id "${slotId}". ${ids.length ? `Its slot ids: ${ids.join(", ")}.` : "Its Voice Chain is empty; add_effect first."}`);
    }
    const info = catalog.find((e) => e.id === slot.effect);
    if (!info) throw fail(`The effect in slot ${slotId} is not available in this build.`);
    const p = info.params.find((x) => x.key === control) ?? info.params.find((x) => key(x.name) === key(control));
    const allowed = info.params.filter((x) => movable(o, x)).map((x) => x.key);
    if (!p || p.hidden) throw fail(`${info.name} has no control "${control}". Controls that can move over time: ${allowed.join(", ") || "none"}.`);
    if (!movable(o, p)) throw fail(`${info.name} ${p.name} cannot move over time${p.retimes ? " (changing it restarts playback)" : " on a track or bus (it is an on/off or list control)"}. Controls that can: ${allowed.join(", ") || "none"}.`);
    return { target: { type: "effect_param", slot_id: slot.id, param: p.key }, label: `${info.name} ${p.name}`, unit: p.unit, min: p.min, max: p.max };
  }

  /** The envelope of `o` for `target`, if it has one. */
  const laneOf = (o: Owner, target: AutoTarget) => o.envelopes.find((e) => targetKey(e.target) === targetKey(target));

  /** Timeline sample of a stored point. */
  const at = (o: Owner, p: EnvelopePointDoc) => (o.span ? o.span[0] : 0) + p.position;

  function laneView(doc: SessionDoc, o: Owner, e: EnvelopeDoc, catalog: readonly EffectInfo[]) {
    const t = e.target;
    const ref =
      t.type === "effect_param"
        ? { parameter: "effect", slot_id: t.slot_id, control: t.param }
        : t.type === "space"
          ? { parameter: "space", space_setting: t.param }
          : { parameter: t.type };
    let label = t.type === "volume" ? "Volume" : t.type === "pan" ? "Pan" : t.type === "space" ? `3D ${t.param}` : "Removed effect";
    let unit = t.type === "volume" ? "dB" : "";
    if (t.type === "effect_param") {
      const slot = o.effects.slots.find((s) => s.id === t.slot_id);
      const info = slot ? catalog.find((x) => x.id === slot.effect) : undefined;
      const p = info?.params.find((x) => x.key === t.param);
      if (info && p) {
        label = `${info.name} ${p.name}`;
        unit = p.unit;
      }
    }
    return {
      ...ref,
      label,
      ...(unit ? { unit } : {}),
      enabled: e.enabled,
      points: e.points.map((p) => ({ time_seconds: secs(doc, at(o, p)), value: round(p.value, 4) })),
    };
  }

  /** The ops that give `o` these points for `target` (empty removes the envelope or lane), and make them play. */
  function writeOps(doc: SessionDoc, o: Owner, target: AutoTarget, points: EnvelopePointDoc[]): Op[] {
    if (o.kind === "clip") {
      const id = o.id;
      const change = (pts: EnvelopePointDoc[]): ClipChange =>
        target.type === "volume" ? { id, volume: pts } : target.type === "pan" ? { id, pan: pts } : { id, effect_points: [{ slot_id: (target as { slot_id: string }).slot_id, param: (target as { param: string }).param, points: pts }] };
      const changes: ClipChange[] = [];
      // An envelope that is switched off would stay off; take it away first so the new one plays.
      if (points.length && laneOf(o, target)?.enabled === false) changes.push(change([]));
      changes.push(change(points));
      return [{ op: "set_clips", changes }];
    }
    const lane = laneOf(o, target);
    const envelopes = points.length
      ? lane
        ? o.envelopes.map((e) => (e === lane ? { ...e, enabled: true, points } : e))
        : [...o.envelopes, { id: c.newId(), target, enabled: true, points }]
      : o.envelopes.filter((e) => e !== lane);
    const ops: Op[] = [{ op: "set_envelopes", owner: o.id, envelopes }];
    if (points.length) ops.push(...playOps(o));
    return ops;
  }

  /** Lanes play in Read: a track recording automation (Write, Latch, Touch) goes back to Read, and its lanes show. */
  function playOps(o: Owner): Op[] {
    if (o.kind === "track") {
      const t = o.track!;
      const changes = { ...(t.record_mode ? { record_mode: "off" as const } : {}), ...(!t.show_automation ? { show_automation: true } : {}) };
      return Object.keys(changes).length ? [{ op: "set_track", id: t.id, changes }] : [];
    }
    if (o.kind === "bus" && o.bus!.record_mode) return [{ op: "set_bus", id: o.id, changes: { record_mode: "off" } }];
    return [];
  }

  const one = (ops: Op[]): Op => (ops.length === 1 ? ops[0]! : { op: "batch", ops });

  return {
    split_clip: async (a) => {
      const doc0 = c.doc();
      const rate = doc0.sample_rate;
      const times = [...new Set(((a.times_seconds as number[]) ?? []).map((t) => toSamples(t, rate)))].sort((x, y) => x - y);
      if (times.length === 0) throw fail("Give times_seconds: one or more times to split at.");
      const clipId = str(a.clip_id);
      const trackRefs = Array.isArray(a.tracks) ? (a.tracks as string[]) : undefined;
      const all = a.all_tracks === true;
      if ([clipId !== undefined, trackRefs !== undefined, all].filter(Boolean).length !== 1) throw fail("Give exactly one of clip_id, tracks or all_tracks.");

      if (clipId !== undefined) {
        const { clip } = clipOf(doc0, clipId);
        if (clip.locked) throw fail(`Clip ${clip.name} is locked. Unlock it first (Clip > Lock Position).`);
        const [s, e] = span(doc0, clip);
        const outside = times.filter((t) => t <= s || t >= e);
        if (outside.length) throw fail(`Clip ${clip.name} runs from ${secs(doc0, s)} s to ${secs(doc0, e)} s; split times must fall inside it, not at its edges (${outside.map((t) => `${secs(doc0, t)} s`).join(", ")} ${outside.length === 1 ? "does" : "do"} not).`);
        const pieces = [clip.id];
        await c.edit(times.length === 1 ? `Split ${clip.name} at ${secs(doc0, times[0]!)} s` : `Split ${clip.name} at ${times.length} Points`, async (run) => {
          for (const t of times) {
            const d = c.doc();
            const last = pieces[pieces.length - 1]!;
            const op = splitClips(d, [last], t, () => c.newId());
            if (!op) throw fail(`Cannot split at ${secs(d, t)} s: it is too close to an edge of the audio.`);
            const added = (op.op === "batch" ? op.ops.find((o) => o.op === "restore_clips") : undefined) as { clips: [string, ClipDoc][] } | undefined;
            await run.execute(c.session.command("Split", op));
            pieces.push(added!.clips[0]![1].id);
          }
        });
        const d = c.doc();
        const view = findClips(d, pieces)
          .map((x) => clipView(d, x.clip))
          .sort((x, y) => x.start_seconds - y.start_seconds);
        return ok(`Split ${clip.name} into ${view.length} clips: ${view.map((v) => `${v.id} (${v.start_seconds} to ${v.end_seconds} s)`).join(", ")}.`, { pieces: view, new_clip_ids: pieces.slice(1) });
      }

      const tracks = all ? doc0.tracks.filter((t) => t.kind !== "midi") : trackRefs!.map((r) => c.track(doc0, r));
      const midi = tracks.find((t) => t.kind === "midi");
      if (midi) throw fail(`${midi.name} is a MIDI track; split_clip works on audio clips.`);
      if (tracks.length === 0) throw fail("The session has no audio tracks.");
      const ids = new Set(tracks.map((t) => t.id));
      const locked: string[] = [];
      const created: string[] = [];
      const where = all ? "All Tracks" : tracks.map((t) => t.name).join(", ");
      await c.edit(times.length === 1 ? `Split ${where} at ${secs(doc0, times[0]!)} s` : `Split ${where} at ${times.length} Points`, async (run) => {
        for (const t of times) {
          const d = c.doc();
          const media = mediaOf(d);
          const under = d.tracks.filter((x) => ids.has(x.id)).flatMap((x) => x.clips.filter((k) => t > k.start && t < clipEnd(k, media, d.sample_rate)));
          for (const k of under) if (k.locked) locked.push(`${k.name} at ${secs(d, t)} s`);
          const op = splitClips(d, under.map((k) => k.id), t, () => c.newId());
          if (!op) continue;
          const added = (op.op === "batch" ? op.ops.find((o) => o.op === "restore_clips") : undefined) as { clips: [string, ClipDoc][] } | undefined;
          await run.execute(c.session.command("Split", op));
          created.push(...(added?.clips.map(([, k]) => k.id) ?? []));
        }
        if (created.length === 0) throw fail(`No ${locked.length ? "unlocked " : ""}clip on ${all ? "any track" : where} spans ${times.length === 1 ? "that time" : "those times"}.${locked.length ? ` Locked: ${locked.join(", ")}.` : ""}`);
      });
      const d = c.doc();
      const pieces = findClips(d, created).map((x) => clipView(d, x.clip));
      return ok(`Made ${plural(created.length, "new clip")}.${locked.length ? ` Locked clips were left whole: ${locked.join(", ")}.` : ""}`, { new_clips: pieces, new_clip_ids: created, ...(locked.length ? { skipped_locked: locked } : {}) });
    },

    delete_clips: async (a) => {
      const doc0 = c.doc();
      const ids = [...new Set((a.clip_ids as string[]) ?? [])];
      for (const id of ids) clipOf(doc0, id);
      const ripple = a.ripple === true;
      const label = `${ripple ? "Ripple Delete" : "Delete"} ${ids.length === 1 ? `Clip ${clipOf(doc0, ids[0]!).clip.name}` : `${ids.length} Clips`}`;
      await c.edit(label, async (run, d) => {
        let op: Op | null;
        try {
          op = ripple ? rippleDeleteClips(d, ids) : { op: "remove_clips", ids };
        } catch (e) {
          if (e instanceof LockedError) throw fail(`${e.message} (A ripple delete would move it.)`);
          throw e;
        }
        if (op) await run.execute(c.session.command(ripple ? "Ripple Delete" : "Delete Clips", op));
      });
      return ok(`Deleted ${plural(ids.length, "clip")}${ripple ? "; later clips on the same tracks moved left to close the gap" : ""}.`);
    },

    move_clip: async (a) => {
      const doc0 = c.doc();
      const { clip, track } = clipOf(doc0, String(a.clip_id));
      const start = num(a.start_seconds);
      const trackRef = str(a.track);
      if (start === undefined && trackRef === undefined) throw fail("Give start_seconds, track or both.");
      if (clip.locked) throw fail(`Clip ${clip.name} is locked. Unlock it first (Clip > Lock Position).`);
      const to = trackRef !== undefined ? c.track(doc0, trackRef) : track;
      if (to.kind === "midi") throw fail(`${to.name} is a MIDI track; audio clips go on audio tracks.`);
      const newStart = start === undefined ? clip.start : toSamples(start, doc0.sample_rate);
      if (newStart === clip.start && to.id === track.id) throw c.noChange(`Clip ${clip.name} is already there.`);
      await c.edit(to.id === track.id ? `Move ${clip.name}` : `Move ${clip.name} to ${to.name}`, async (run, d) => {
        const change: ClipChange = { id: clip.id, ...(newStart !== clip.start ? { start: newStart } : {}), ...(to.id !== track.id ? { track_id: to.id } : {}) };
        await run.execute(c.session.command("Move Clip", { op: "set_clips", changes: withLayers(d, [change]) }));
      });
      const d = c.doc();
      const v = clipView(d, clipOf(d, clip.id).clip);
      return ok(`Clip ${clip.name} is on ${to.name} from ${v.start_seconds} s to ${v.end_seconds} s.`, { ...v, track: to.id });
    },

    trim_clip: async (a) => {
      const doc0 = c.doc();
      const { clip } = clipOf(doc0, String(a.clip_id));
      const start = num(a.start_seconds);
      const end = num(a.end_seconds);
      if (start === undefined && end === undefined) throw fail("Give start_seconds, end_seconds or both.");
      if (clip.locked) throw fail(`Clip ${clip.name} is locked. Unlock it first (Clip > Lock Position).`);
      const rate = doc0.sample_rate;
      const [s0, e0] = span(doc0, clip);
      const wantS = start === undefined ? s0 : toSamples(start, rate);
      const wantE = end === undefined ? e0 : toSamples(end, rate);
      if (wantE <= wantS) throw fail("end_seconds must be after start_seconds.");
      if (wantS === s0 && wantE === e0) throw c.noChange(`Clip ${clip.name} already runs from ${secs(doc0, s0)} s to ${secs(doc0, e0)} s.`);
      await c.edit(`Trim ${clip.name}`, async (run) => {
        if (wantS !== s0) {
          const d = c.doc();
          const ch = trimClip(d, clipOf(d, clip.id).clip, "left", wantS - s0);
          if (ch) await run.execute(c.session.command("Trim Clip", { op: "set_clips", changes: [ch] }));
        }
        if (wantE !== e0) {
          const d = c.doc();
          const now = clipOf(d, clip.id).clip;
          const ch = trimClip(d, now, "right", wantE - span(d, now)[1]);
          if (ch) await run.execute(c.session.command("Trim Clip", { op: "set_clips", changes: [ch] }));
        }
      });
      const d = c.doc();
      const v = clipView(d, clipOf(d, clip.id).clip);
      const notes: string[] = [];
      if (start !== undefined && Math.abs(v.start_seconds - secs(d, wantS)) > 0.0005) notes.push(`the start stopped at ${v.start_seconds} s (the audio file, or the shortest clip, allows no further)`);
      if (end !== undefined && Math.abs(v.end_seconds - secs(d, wantE)) > 0.0005) notes.push(`the end stopped at ${v.end_seconds} s (the audio file, or the shortest clip, allows no further)`);
      return ok(`Clip ${clip.name} now runs from ${v.start_seconds} s to ${v.end_seconds} s.${notes.length ? ` Note: ${notes.join("; ")}.` : ""}`, v);
    },

    set_clip_gain: async (a) => {
      const doc0 = c.doc();
      const ids = [...new Set((a.clip_ids as string[]) ?? [])];
      const clips = ids.map((id) => clipOf(doc0, id).clip);
      const gain = num(a.gain_db);
      const muted = bool(a.muted);
      if (gain === undefined && muted === undefined) throw fail("Give gain_db, muted or both.");
      const changes: ClipChange[] = clips
        .map((k) => ({ id: k.id, ...(gain !== undefined && gain !== k.gain_db ? { gain_db: gain } : {}), ...(muted !== undefined && muted !== k.muted ? { muted } : {}) }))
        .filter((ch) => Object.keys(ch).length > 1);
      if (changes.length === 0) throw c.noChange(`${clips.length === 1 ? `Clip ${clips[0]!.name} already has` : "Those clips already have"} that setting.`);
      const what = gain !== undefined && muted === undefined ? "Clip Gain" : muted !== undefined && gain === undefined ? (muted ? "Mute Clip" : "Unmute Clip") : "Clip Gain and Mute";
      await c.edit(clips.length === 1 ? `${what}: ${clips[0]!.name}` : `${what}: ${clips.length} Clips`, async (run) => {
        await run.execute(c.session.command(what, { op: "set_clips", changes }));
      });
      const parts = [...(gain !== undefined ? [`gain ${round(gain, 1)} dB`] : []), ...(muted !== undefined ? [muted ? "muted" : "unmuted"] : [])];
      return ok(`${clips.length === 1 ? `Clip ${clips[0]!.name}` : plural(clips.length, "clip")}: ${parts.join(", ")}.`);
    },

    set_clip_fades: async (a) => {
      const doc0 = c.doc();
      const { clip } = clipOf(doc0, String(a.clip_id));
      const rate = doc0.sample_rate;
      const fields = ["fade_in_seconds", "fade_out_seconds", "fade_in_shape", "fade_out_shape", "fade_in_bend", "fade_out_bend"];
      if (!fields.some((f) => a[f] !== undefined)) throw fail(`Give at least one of ${fields.join(", ")}.`);
      for (const f of ["fade_in_shape", "fade_out_shape"]) {
        const s = str(a[f]);
        if (s !== undefined && !FADE_IDS.includes(s)) throw fail(`${f} must be one of ${FADE_IDS.join(", ")}.`);
      }
      const len = clipLength(clip, doc0.media.find((m) => m.id === clip.media_id), rate);
      const notes: string[] = [];
      // Lengths go in one at a time so the two fades never overlap, as dragging them does.
      let work: ClipDoc = { ...clip };
      const change: ClipChange = { id: clip.id };
      for (const which of ["in", "out"] as const) {
        const secsWanted = num(a[`fade_${which}_seconds`]);
        const field = which === "in" ? "fade_in" : "fade_out";
        let fade: FadeDoc = { ...work[field] };
        if (secsWanted !== undefined) {
          const want = toSamples(secsWanted, rate);
          const ch = fadeClip({ ...doc0, tracks: doc0.tracks }, work, which, want);
          if (ch && ch[field]) fade = { ...ch[field]! };
          if (fade.length !== want) notes.push(`fade ${which} kept at ${secs(doc0, fade.length)} s (the clip is ${secs(doc0, len)} s long and the fades may not overlap)`);
        }
        const shape = str(a[`fade_${which}_shape`]);
        if (shape !== undefined) fade.shape = shape;
        const bend = num(a[`fade_${which}_bend`]);
        if (bend !== undefined) fade.curve = Math.max(-1, Math.min(1, bend));
        work = { ...work, [field]: fade };
        const cur = clip[field];
        if (fade.length !== cur.length || fade.shape !== cur.shape || fade.curve !== cur.curve) change[field] = fade;
      }
      if (!change.fade_in && !change.fade_out) throw c.noChange(`Clip ${clip.name} already has those fades.`);
      await c.edit(`Set Fades on ${clip.name}`, async (run) => {
        await run.execute(c.session.command("Fades", { op: "set_clips", changes: [change] }));
      });
      const f = (x: FadeDoc) => ({ seconds: secs(doc0, x.length), shape: x.shape, bend: x.curve });
      return ok(
        `Clip ${clip.name}: fade in ${secs(doc0, work.fade_in.length)} s (${fadeName(work.fade_in.shape)}), fade out ${secs(doc0, work.fade_out.length)} s (${fadeName(work.fade_out.shape)}).${notes.length ? ` Note: ${notes.join("; ")}.` : ""}`,
        { fade_in: f(work.fade_in), fade_out: f(work.fade_out) },
      );
    },

    set_clip_color: async (a) => {
      const doc0 = c.doc();
      const ids = [...new Set((a.clip_ids as string[]) ?? [])];
      const clips = ids.map((id) => clipOf(doc0, id).clip);
      const raw = String(a.color ?? "");
      const value = colorValue(raw);
      if (value === null) throw fail(`color must be a palette name (${CLIP_COLORS.join(", ")}), a number 1 to 6, a hex color like #7fa7d9, or "track" for the track's color.`);
      const changes: ClipChange[] = clips.filter((k) => (k.color ?? "") !== value).map((k) => ({ id: k.id, color: value }));
      if (changes.length === 0) throw c.noChange(`${clips.length === 1 ? `Clip ${clips[0]!.name} is` : "Those clips are"} already that color.`);
      await c.edit(clips.length === 1 ? `Clip Color: ${clips[0]!.name}` : `Clip Color: ${clips.length} Clips`, async (run) => {
        await run.execute(c.session.command("Clip Color", { op: "set_clips", changes }));
      });
      const shown = colorName(value) ?? "the track's color";
      return ok(`${clips.length === 1 ? `Clip ${clips[0]!.name}` : plural(clips.length, "clip")}: ${shown}.`);
    },

    group_clips: async (a) => {
      const doc0 = c.doc();
      const ids = [...new Set((a.clip_ids as string[]) ?? [])];
      if (ids.length < 2) throw fail("A group needs two or more clips.");
      const clips = ids.map((id) => clipOf(doc0, id).clip);
      const group = c.newId();
      await c.edit(`Group ${clips.length} Clips`, async (run) => {
        await run.execute(c.session.command("Group Clips", { op: "set_clips", changes: clips.map((k) => ({ id: k.id, group })) }));
      });
      const left = [...new Set(clips.map((k) => k.group).filter((g): g is string => !!g))];
      return ok(`Grouped ${plural(clips.length, "clip")} (group ${group}). Grouped clips are selected and moved together in the Editor.${left.length ? " Clips that were in another group left it." : ""}`, { group });
    },

    ungroup_clips: async (a) => {
      const doc0 = c.doc();
      const ids = [...new Set((a.clip_ids as string[]) ?? [])];
      const clips = ids.map((id) => clipOf(doc0, id).clip);
      const grouped = clips.filter((k) => k.group);
      if (grouped.length === 0) throw c.noChange(clips.length === 1 ? `Clip ${clips[0]!.name} is not in a group.` : "None of those clips is in a group.");
      await c.edit(grouped.length === 1 ? `Remove ${grouped[0]!.name} from Group` : `Ungroup ${grouped.length} Clips`, async (run) => {
        await run.execute(c.session.command("Ungroup Clips", { op: "set_clips", changes: grouped.map((k) => ({ id: k.id, ungroup: true })) }));
      });
      return ok(`Took ${plural(grouped.length, "clip")} out of ${grouped.length === 1 ? "its group" : "their groups"}.`);
    },

    stretch_clip: async (a) => {
      const doc0 = c.doc();
      const { clip } = clipOf(doc0, String(a.clip_id));
      const rate = doc0.sample_rate;
      const m = doc0.media.find((x) => x.id === clip.media_id);
      if (clip.locked) throw fail(`Clip ${clip.name} is locked. Unlock it first (Clip > Lock Position).`);
      if (clip.looped) throw fail(`Clip ${clip.name} loops, and a looping clip cannot be stretched. Turn Loop Clip off first.`);
      const natural = naturalLength(clip, m, rate);
      const view = () => {
        const d = c.doc();
        const k = clipOf(d, clip.id).clip;
        return { ...clipView(d, k), natural_seconds: secs(d, natural), ratio: round(stretchOf(k), 4), preserve_pitch: k.stretch?.keep_pitch ?? true, pitch_semitones: k.stretch?.pitch ?? 0 };
      };
      if (a.remove === true) {
        const ch = unstretch(clip, m, rate);
        if (!ch) throw c.noChange(`Clip ${clip.name} is not stretched.`);
        await c.edit(`Remove Stretch from ${clip.name}`, async (run) => {
          await run.execute(c.session.command("Remove Stretch", { op: "set_clips", changes: [ch] }));
        });
        const v = view();
        return ok(`Clip ${clip.name} plays at its natural length and pitch again (${v.end_seconds - v.start_seconds > 0 ? round(v.end_seconds - v.start_seconds) : 0} s).`, v);
      }
      const lengthS = num(a.length_seconds);
      const ratioIn = num(a.ratio);
      if (lengthS !== undefined && ratioIn !== undefined) throw fail("Give length_seconds or ratio, not both.");
      const keep = bool(a.preserve_pitch) ?? clip.stretch?.keep_pitch ?? true;
      const pitch = num(a.pitch_semitones);
      if (pitch !== undefined && pitch !== 0 && !keep) throw fail("pitch_semitones works only with preserve_pitch true. Without it the pitch follows the speed.");
      if (lengthS === undefined && ratioIn === undefined && pitch === undefined && a.preserve_pitch === undefined) throw fail("Give length_seconds, ratio, preserve_pitch, pitch_semitones or remove.");
      const length = lengthS !== undefined ? toSamples(lengthS, rate) : ratioIn !== undefined ? Math.round(natural * ratioIn) : clipLength(clip, m, rate);
      if (length <= 0) throw fail("The new length must be above 0.");
      const r = stretchTo(clip, m, rate, length, keep);
      const ratio = r ? r.ratio : stretchOf(clip);
      const p = keep ? Math.max(PITCH_MIN, Math.min(PITCH_MAX, pitch ?? clip.stretch?.pitch ?? 0)) : 0;
      let change: ClipChange;
      if (Math.abs(ratio - 1) < 1e-6 && p === 0) {
        // Natural length and pitch: no stretch at all.
        change = r ? { ...r.change } : clip.stretch ? (unstretch(clip, m, rate) ?? { id: clip.id }) : { id: clip.id };
        delete change.stretch;
        if (clip.stretch) change.unstretch = true;
        else delete change.unstretch;
      } else {
        change = { ...(r?.change ?? { id: clip.id }) };
        delete change.unstretch;
        const next = { ...(clip.stretch ?? {}), ratio, keep_pitch: keep, pitch: p };
        const cur = clip.stretch;
        const same = !!cur && cur.ratio === next.ratio && cur.keep_pitch === next.keep_pitch && (cur.pitch ?? 0) === p;
        if (!same || Object.keys(change).length > 1) change.stretch = next;
      }
      if (Object.keys(change).length <= 1) throw c.noChange(`Clip ${clip.name} already plays that way.`);
      await c.edit(`Time Stretch ${clip.name}`, async (run) => {
        await run.execute(c.session.command("Time Stretch", { op: "set_clips", changes: [change] }));
      });
      const v = view();
      const notes: string[] = [];
      if (r?.limited) notes.push(`a clip plays from ${STRETCH_MIN * 100}% to ${STRETCH_MAX * 100}% of its natural length, so the stretch was limited`);
      if (pitch !== undefined && (pitch < PITCH_MIN || pitch > PITCH_MAX)) notes.push(`pitch kept at ${Math.max(PITCH_MIN, Math.min(PITCH_MAX, pitch))} semitones`);
      return ok(
        `Clip ${clip.name} now plays ${round(v.end_seconds - v.start_seconds)} s (${round(v.ratio * 100, 1)}% of its natural ${v.natural_seconds} s), ${v.preserve_pitch ? `pitch kept${v.pitch_semitones ? `, shifted ${v.pitch_semitones} semitones` : ""}` : "pitch following the speed"}.${notes.length ? ` Note: ${notes.join("; ")}.` : ""}`,
        v,
      );
    },

    get_automation: async (a) => {
      const doc = c.doc();
      const o = owner(doc, a.target);
      const catalog = await c.catalog().catch(() => [] as EffectInfo[]);
      const lanes = o.envelopes.map((e) => laneView(doc, o, e, catalog));
      const can = choices(o, catalog)
        .filter((x) => !(x.ref.parameter === "pan" && o.kind !== "clip" && doc.space))
        .map((x) => ({ ...x.ref, label: x.label, ...(x.unit ? { unit: x.unit } : {}), min: x.min, max: x.max }));
      const data = {
        target: { kind: o.kind, id: o.id },
        name: o.name,
        ...(o.span ? { clip_start_seconds: secs(doc, o.span[0]), clip_end_seconds: secs(doc, o.span[1]), clip_gain_db: o.clip!.gain_db } : {}),
        ...(o.kind === "track" ? { automation_mode: autoMode(o.track!), fader_volume_db: o.track!.volume_db, fader_pan: o.track!.pan } : {}),
        ...(o.kind === "bus" ? { automation_mode: autoMode(o.bus!), fader_volume_db: o.bus!.volume_db, fader_pan: o.bus!.pan } : {}),
        lanes,
        can_automate: can,
      };
      return { text: JSON.stringify(data), structured: data, isError: false };
    },

    set_automation: async (a) => {
      const doc0 = c.doc();
      const o0 = owner(doc0, a.target);
      const catalog = await c.catalog();
      const r = resolve(doc0, o0, a, catalog);
      const mode = (str(a.mode) ?? "replace_range") as "replace_range" | "replace_all" | "add";
      const raw = (a.points as { time_seconds?: unknown; value?: unknown }[]) ?? [];
      const rate = doc0.sample_rate;
      const notes: string[] = [];
      let clamped = 0;
      const points: EnvelopePointDoc[] = raw.map((p, i) => {
        const t = num(p.time_seconds);
        const v = num(p.value);
        if (t === undefined || v === undefined) throw fail(`points[${i}] needs time_seconds and value as numbers.`);
        let pos = toSamples(t, rate);
        if (o0.span) {
          const [s, e] = o0.span;
          // Half a millisecond either side is taken as the edge.
          const slack = Math.round(rate / 2000);
          if (pos < s - slack || pos > e + slack) throw fail(`points[${i}] at ${round(t)} s is outside clip ${o0.clip!.name} (${secs(doc0, s)} s to ${secs(doc0, e)} s). Clip envelope times are still seconds from the session start.`);
          pos = Math.max(s, Math.min(e, pos)) - s;
        }
        const kept = Math.max(r.min, Math.min(r.max, v));
        if (kept !== v) clamped++;
        return { position: pos, value: kept, curve: "linear" as const };
      });
      if (clamped) notes.push(`${plural(clamped, "value")} kept inside ${round(r.min, 4)} to ${round(r.max, 4)}${r.unit ? ` ${r.unit}` : ""}`);
      const old = laneOf(o0, r.target)?.points ?? [];
      const next = mergePoints(old, points, mode);
      const same = JSON.stringify(old) === JSON.stringify(next) && laneOf(o0, r.target)?.enabled !== false;
      if (same) throw c.noChange(`${r.label} on ${o0.name} already has those points.`);
      await c.edit(`Automate ${r.label} on ${o0.name}`, async (run, d) => {
        const o = owner(d, a.target);
        await run.execute(c.session.command("Automation", one(writeOps(d, o, r.target, next))));
      });
      const d = c.doc();
      const o = owner(d, a.target);
      const lane = laneOf(o, r.target);
      const view = lane ? laneView(d, o, lane, catalog) : null;
      if (o.kind === "track" && o0.track!.record_mode) notes.push(`${o.name} was recording automation (${o0.track!.record_mode}); it is back to Read so the lane plays`);
      if (o.kind === "bus" && o0.bus!.record_mode) notes.push(`${o.name} was recording automation (${o0.bus!.record_mode}); it is back to Read so the lane plays`);
      if (o.kind !== "clip" && r.target.type === "volume") notes.push("the lane takes the fader's place while it plays");
      return ok(`${r.label} on ${o.name} now has ${plural(next.length, "point")}.${notes.length ? ` Note: ${notes.join("; ")}.` : ""}`, view);
    },

    clear_automation: async (a) => {
      const doc0 = c.doc();
      const o0 = owner(doc0, a.target);
      const catalog = await c.catalog();
      const start = num(a.start_seconds);
      const end = num(a.end_seconds);
      if (start !== undefined && end !== undefined && end < start) throw fail("end_seconds must not be before start_seconds.");
      const rate = doc0.sample_rate;
      const base = o0.span ? o0.span[0] : 0;
      const from = start === undefined ? Number.NEGATIVE_INFINITY : toSamples(start, rate) - base;
      const to = end === undefined ? Number.POSITIVE_INFINITY : toSamples(end, rate) - base;
      const whole = start === undefined && end === undefined;
      const one_ = a.parameter !== undefined ? resolveForClear(doc0, o0, a, catalog) : null;
      const lanes = one_ ? o0.envelopes.filter((e) => targetKey(e.target) === targetKey(one_.target)) : o0.envelopes.filter((e) => o0.kind !== "clip" || e.target.type !== "space");
      const touched = lanes.filter((e) => removeRange(e.points, from, to).length !== e.points.length || (whole && o0.kind !== "clip"));
      if (touched.length === 0) throw c.noChange(`${one_ ? one_.label : "Nothing"} on ${o0.name} has automation${whole ? "" : " in that range"} to clear.`);
      const what = one_ ? one_.label : "Automation";
      await c.edit(`Clear ${what === "Automation" ? "Automation" : `${what} Automation`} on ${o0.name}`, async (run, d) => {
        const o = owner(d, a.target);
        const keys = new Set(touched.map((e) => targetKey(e.target)));
        if (o.kind === "clip") {
          const ch: ClipChange = { id: o.id };
          for (const e of o.envelopes) {
            if (!keys.has(targetKey(e.target))) continue;
            const pts = removeRange(e.points, from, to);
            if (e.target.type === "volume") ch.volume = pts;
            else if (e.target.type === "pan") ch.pan = pts;
            else if (e.target.type === "effect_param") (ch.effect_points ??= []).push({ slot_id: e.target.slot_id, param: e.target.param, points: pts });
          }
          await run.execute(c.session.command("Clear Automation", { op: "set_clips", changes: [ch] }));
          return;
        }
        const envelopes = o.envelopes.flatMap((e) => {
          if (!keys.has(targetKey(e.target))) return [e];
          const pts = removeRange(e.points, from, to);
          return pts.length ? [{ ...e, points: pts }] : [];
        });
        await run.execute(c.session.command("Clear Automation", { op: "set_envelopes", owner: o.id, envelopes }));
      });
      return ok(`Cleared ${one_ ? one_.label : plural(touched.length, "lane")} on ${o0.name}${whole ? "" : ` from ${start === undefined ? "the start" : `${round(start)} s`} to ${end === undefined ? "the end" : `${round(end)} s`}`}.`);
    },
  };

  /** Like resolve, but an effect lane whose effect was removed can still be cleared. */
  function resolveForClear(doc: SessionDoc, o: Owner, a: Args, catalog: readonly EffectInfo[]): { target: AutoTarget; label: string } {
    if (a.parameter === "effect") {
      const slotId = str(a.slot_id);
      const control = str(a.control);
      if (slotId && control && !o.effects.slots.some((s) => s.id === slotId)) return { target: { type: "effect_param", slot_id: slotId, param: control }, label: "Removed effect" };
    }
    if (a.parameter === "pan") return { target: { type: "pan" }, label: "Pan" };
    return resolve(doc, o, a, catalog);
  }
}
