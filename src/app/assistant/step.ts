// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) The GeekCast Radio Network, LLC.
// This file is part of Altered Audio Editor.

import type { Command } from "../../commands";
import type { CommandRunner } from "../../stores/media-pool";

/** Thrown by a step's body when there turned out to be nothing to change. */
export class NoChange extends Error {
  constructor(message: string) {
    super(message);
    this.name = "NoChange";
  }
}

/**
 * One assistant tool call as one History entry ("Assistant: ...").
 *
 * The first time it runs, `body` does the work with a runner that records
 * every command it runs (imports, session ops), using the same code the
 * menus use. Undo takes them back in reverse; redo runs them again.
 * Because it runs through the undo stack, it waits its turn behind the
 * person's own edits and never interleaves with them.
 *
 * The body must never touch the undo stack itself (that would wait on
 * this very step); it gets its own runner for that.
 *
 * A body that throws leaves nothing behind: whatever it already ran is
 * undone. A body that records nothing throws `NoChange`, so no empty step
 * lands in the History.
 */
export class AssistantStep implements Command {
  readonly label: string;
  private children: Command[] = [];
  private ran = false;

  constructor(
    label: string,
    private readonly body: (run: CommandRunner) => Promise<void>,
  ) {
    this.label = label.startsWith("Assistant: ") ? label : `Assistant: ${label}`;
  }

  async do(): Promise<void> {
    if (this.ran) {
      for (const c of this.children) await c.do();
      return;
    }
    const recorded: Command[] = [];
    const runner: CommandRunner = {
      execute: async (c: Command) => {
        await c.do();
        recorded.push(c);
      },
    };
    try {
      await this.body(runner);
      if (recorded.length === 0) throw new NoChange("Nothing needed to change.");
    } catch (e) {
      for (const c of recorded.reverse()) {
        try {
          await c.undo();
        } catch {
          // Keep rolling back the rest.
        }
      }
      throw e;
    }
    this.children = recorded;
    this.ran = true;
  }

  async undo(): Promise<void> {
    for (const c of [...this.children].reverse()) await c.undo();
  }
}
