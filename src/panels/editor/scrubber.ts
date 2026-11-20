// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) The GeekCast Radio Network, LLC.
// This file is part of Altered Audio Editor.

/**
 * Audible scrubbing: dragging the playhead along the ruler plays the audio under it, like tape
 * moving past a head. The core does the sound (see `engine/scrub.rs`): it starts with the head
 * resting, silent, where the button went down, and its speed then follows the pointer, forward
 * or backward, slowing to silence when the pointer stops. This side only tells it where the
 * pointer is, at most once a screen frame, and stops it when the button is let go, leaving the
 * playhead where the pointer ended.
 *
 * Only while stopped, and only when Play audio while scrubbing is on (the default). A drag that
 * began during playback keeps its old behavior (playback jumps when the button is let go). If the
 * sound cannot start (no output device), the drag still moves the playhead.
 */

export interface ScrubTransport {
  readonly current: { readonly playing: boolean };
  seek(to: number): Promise<void>;
}

/** The core's scrub player for one document. */
export interface ScrubEngine {
  start(from: number): Promise<void>;
  to(pos: number): Promise<void>;
  stop(): Promise<unknown>;
}

/** Runs `fn` before the next screen frame. */
export type Schedule = (fn: () => void) => void;

const nextFrame: Schedule = (fn) => {
  requestAnimationFrame(() => fn());
};

export class Scrubber {
  /** Resolves true once the core's player runs; false if it could not start. */
  private started: Promise<boolean> | null = null;
  private pending: number | null = null;
  private queued = false;
  private ended = false;
  /** True when this drag plays audio: the setting is on and nothing was playing when it began. */
  readonly active: boolean;

  constructor(
    private readonly t: ScrubTransport,
    private readonly engine: ScrubEngine | null,
    enabled: boolean,
    private readonly schedule: Schedule = nextFrame,
  ) {
    this.active = enabled && engine !== null && !t.current.playing;
  }

  /** The pointer is at `pos` (samples). The first call starts the sound, resting there. */
  move(pos: number): void {
    if (!this.active || this.ended || !Number.isFinite(pos)) return;
    const engine = this.engine!;
    if (!this.started) {
      this.started = engine.start(pos).then(
        () => true,
        () => false,
      );
      return;
    }
    this.pending = pos;
    if (this.queued) return;
    this.queued = true;
    this.schedule(() => {
      this.queued = false;
      const p = this.pending;
      this.pending = null;
      if (p === null || this.ended) return;
      void this.started!.then((ok) => {
        if (ok && !this.ended) return engine.to(p).catch(() => {});
      });
    });
  }

  /** The button was let go at `pos`: fade the sound out and leave the playhead there. */
  async end(pos: number): Promise<void> {
    this.ended = true;
    if (this.started && (await this.started)) await this.engine!.stop().catch(() => {});
    await this.t.seek(pos);
  }
}
