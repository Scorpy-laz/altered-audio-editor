// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) The GeekCast Radio Network, LLC.
// This file is part of Altered Audio Editor.

import { describe, expect, it } from "vitest";
import { Scrubber, type ScrubEngine, type ScrubTransport } from "./scrubber";

function fake(playing = false, startFails = false) {
  const calls: string[] = [];
  const t: ScrubTransport = {
    current: { playing },
    seek: async (to) => void calls.push(`seek:${to}`),
  };
  const engine: ScrubEngine = {
    start: async (from) => {
      calls.push(`start:${from}`);
      if (startFails) throw new Error("no device");
    },
    to: async (pos) => void calls.push(`to:${pos}`),
    stop: async () => void calls.push("stop"),
  };
  // Screen frames run when the test says so.
  const frames: (() => void)[] = [];
  const schedule = (fn: () => void) => void frames.push(fn);
  const frame = async () => {
    for (const f of frames.splice(0)) f();
    await new Promise((r) => setTimeout(r, 0));
  };
  return { t, engine, calls, schedule, frame };
}

describe("Scrubber", () => {
  it("starts resting at the first spot, then sends the pointer at most once a frame", async () => {
    const { t, engine, calls, schedule, frame } = fake();
    const s = new Scrubber(t, engine, true, schedule);
    s.move(100);
    s.move(150);
    s.move(200);
    await frame();
    s.move(260);
    await frame();
    await frame();
    expect(calls).toEqual(["start:100", "to:200", "to:260"]);
  });

  it("fades out on release and puts the playhead where the pointer ended", async () => {
    const { t, engine, calls, schedule, frame } = fake();
    const s = new Scrubber(t, engine, true, schedule);
    s.move(500);
    s.move(700);
    await s.end(900);
    // A frame still queued after release sends nothing.
    await frame();
    expect(calls).toEqual(["start:500", "stop", "seek:900"]);
  });

  it("only seeks when the pointer never moved", async () => {
    const { t, engine, calls, schedule } = fake();
    const s = new Scrubber(t, engine, true, schedule);
    await s.end(42);
    expect(calls).toEqual(["seek:42"]);
  });

  it("still moves the playhead when the sound cannot start", async () => {
    const { t, engine, calls, schedule, frame } = fake(false, true);
    const s = new Scrubber(t, engine, true, schedule);
    s.move(10);
    s.move(20);
    await frame();
    await s.end(30);
    expect(calls).toEqual(["start:10", "seek:30"]);
  });

  it("does nothing while scrubbing is off, playback was already running, or there is no engine", async () => {
    for (const [enabled, playing, none] of [
      [false, false, false],
      [true, true, false],
      [true, false, true],
    ] as const) {
      const { t, engine, calls, schedule, frame } = fake(playing);
      const s = new Scrubber(t, none ? null : engine, enabled, schedule);
      expect(s.active).toBe(false);
      s.move(100);
      s.move(150);
      await frame();
      await s.end(200);
      expect(calls).toEqual(["seek:200"]);
    }
  });

  it("ignores positions that are not numbers", async () => {
    const { t, engine, calls, schedule, frame } = fake();
    const s = new Scrubber(t, engine, true, schedule);
    s.move(Number.NaN);
    s.move(5);
    await frame();
    expect(calls).toEqual(["start:5"]);
  });
});
