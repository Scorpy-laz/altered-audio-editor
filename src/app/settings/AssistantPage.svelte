<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
<!-- Copyright (c) The GeekCast Radio Network, LLC. -->
<!-- This file is part of Altered Audio Editor. -->

<script lang="ts">
  /**
   * Settings > Assistant: the local connection that lets an AI assistant on
   * this computer read and edit the open session (Model Context Protocol).
   * Changes here apply at once, like Backup and Restore, not on OK.
   */
  import { onMount } from "svelte";
  import { assistantActivity, assistantClearActivity, assistantConfigure, assistantNewToken, assistantStatus, onAssistantActivity, type AssistantEntry, type AssistantStatus } from "../core";
  import { claudeCodeCommand, claudeDesktopConfig, codexConfig } from "../assistant/snippets";
  import { PLATFORM } from "../../lib/platform";

  interface Props {
    /** Opens the Assistant Activity window. */
    onOpenActivity?: (() => void) | undefined;
    /** Tells the rest of the app (the status bar) what changed. */
    onStatus?: ((s: AssistantStatus) => void) | undefined;
  }
  const { onOpenActivity, onStatus }: Props = $props();

  let status = $state<AssistantStatus | null>(null);
  let port = $state(47321);
  let busy = $state(false);
  let message = $state("");
  let showToken = $state(false);
  let entries = $state<AssistantEntry[]>([]);

  function take(s: AssistantStatus) {
    status = s;
    port = s.port;
    onStatus?.(s);
  }

  onMount(() => {
    void assistantStatus().then(take, (e) => (message = String(e)));
    void assistantActivity(50).then((list) => (entries = list), () => {});
    let off: (() => void) | undefined;
    let gone = false;
    void onAssistantActivity((e) => (entries = [...entries.slice(-49), e])).then((f) => {
      if (gone) f();
      else off = f;
    });
    return () => {
      gone = true;
      off?.();
    };
  });

  async function run(task: () => Promise<void>) {
    busy = true;
    message = "";
    try {
      await task();
    } catch (e) {
      message = e instanceof Error ? e.message : String(e);
    } finally {
      busy = false;
    }
  }

  const configure = (enabled: boolean, allowEdits: boolean) =>
    run(async () => {
      const p = Math.round(Number(port));
      if (!Number.isFinite(p) || p < 1024 || p > 65535) throw new Error("Pick a port from 1024 to 65535.");
      take(await assistantConfigure(enabled, p, allowEdits));
    });

  const newToken = () =>
    run(async () => {
      take(await assistantNewToken());
      message = "New token made. Assistants set up through the bridge pick it up on their next call; ones given the token directly need the new one.";
    });

  const copy = (what: string, text: string) =>
    run(async () => {
      await navigator.clipboard.writeText(text);
      message = `${what} copied.`;
    });

  const clear = () =>
    run(async () => {
      await assistantClearActivity();
      entries = [];
    });
</script>

<div class="page">
  <p class="note">Lets an AI assistant on this computer, such as Claude or Codex, read the open session and, if you allow it, make changes: Voice Chain effects, levels, markers, loudness checks, exports. Off until you turn it on.</p>
  {#if status}
    <label class="check"><input type="checkbox" checked={status.enabled} disabled={busy} onchange={(e) => void configure(e.currentTarget.checked, status!.allowEdits)} /> Allow assistants to connect</label>
    <p class="state" class:on={status.running} class:warn={!!status.problem}>
      {#if status.problem}
        {status.problem}
      {:else if status.running}
        Listening at <code>{status.url}</code>, for programs on this computer only.
      {:else}
        Off. Nothing listens for connections.
      {/if}
    </p>
    <label class="check"><input type="checkbox" checked={status.allowEdits} disabled={busy} onchange={(e) => void configure(status!.enabled, e.currentTarget.checked)} /> Allow changes</label>
    <p class="note">Off: the assistant can only read the session and measure loudness. On: it can also edit, play, export and save. Each change it makes is one step in History named "Assistant: ...", so Undo takes it back.</p>

    <h4>Connection</h4>
    <div class="row">
      <label class="row">Port <input type="number" min="1024" max="65535" bind:value={port} disabled={busy} /></label>
      <button type="button" disabled={busy || port === status.port} onclick={() => void configure(status!.enabled, status!.allowEdits)}>Apply</button>
    </div>
    <div class="row">
      Token <code class="token">{showToken ? status.token : "•".repeat(24)}</code>
      <button type="button" onclick={() => (showToken = !showToken)}>{showToken ? "Hide" : "Show"}</button>
      <button type="button" disabled={busy} onclick={() => void copy("Token", status!.token)}>Copy</button>
      <button type="button" disabled={busy} onclick={() => void newToken()}>New Token</button>
    </div>
    <p class="note">Anything holding this token can control the editor while the connection is on. Keep it private.</p>

    <h4>Set up an assistant</h4>
    <p class="note">These start this program as a small bridge that finds the editor by itself, so they never need the token. Paste them into the assistant's settings, then restart the assistant. The guide (docs/mcp.md) has the details.</p>
    <div class="row">
      <button type="button" disabled={busy} onclick={() => void copy("Claude Desktop settings", claudeDesktopConfig(status!.exe, status!.bridgeFlag))}>Copy Claude Desktop Config</button>
      <button type="button" disabled={busy} onclick={() => void copy("Codex settings", codexConfig(status!.exe, status!.bridgeFlag))}>Copy Codex Config</button>
      <button type="button" disabled={busy} onclick={() => void copy("Claude Code command", claudeCodeCommand(status!.exe, PLATFORM === "windows", status!.bridgeFlag))}>Copy Claude Code Command</button>
    </div>

    <h4>Activity</h4>
    {#if entries.length === 0}
      <p class="note">Nothing yet. Every request an assistant makes is listed here.</p>
    {:else}
      <ol class="log">
        {#each [...entries].reverse().slice(0, 8) as e (e.seq)}
          <li class:fail={!e.ok}><span class="t">{e.time}</span> <span class="tool">{e.tool}</span> <span class="msg">{e.message}</span></li>
        {/each}
      </ol>
    {/if}
    <div class="row">
      {#if onOpenActivity}<button type="button" onclick={onOpenActivity}>Open Activity Window</button>{/if}
      <button type="button" disabled={busy || entries.length === 0} onclick={() => void clear()}>Clear</button>
    </div>
  {:else}
    <p class="note">Reading the settings…</p>
  {/if}
  {#if message}<p class="msg">{message}</p>{/if}
</div>

<style>
  /* Same look as the other Settings pages. Those styles live in SettingsDialog and do not reach this component. */
  .page {
    display: flex;
    flex-direction: column;
    gap: 8px;
  }
  .note {
    margin: 0;
    color: var(--text-dim);
  }
  .state {
    margin: 0 0 0 22px;
    color: var(--text-faint);
  }
  .state.on {
    color: var(--text);
  }
  .state.warn {
    color: var(--danger);
  }
  .row,
  .check {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
    align-items: center;
  }
  .msg {
    margin: 0;
    color: var(--accent);
  }
  h4 {
    margin: 8px 0 0;
    padding-bottom: 4px;
    border-bottom: 1px solid var(--border);
    font-size: 13px;
    font-weight: 600;
  }
  code {
    font-family: var(--font-mono);
    word-break: break-all;
  }
  .token {
    max-width: 360px;
  }
  .log {
    margin: 0;
    padding: 0;
    list-style: none;
    font-variant-numeric: tabular-nums;
  }
  .log li {
    display: flex;
    gap: 8px;
    padding: 2px 0;
    border-bottom: 1px solid var(--border);
    color: var(--text-dim);
    white-space: nowrap;
    overflow: hidden;
  }
  .log li.fail .tool {
    color: var(--danger);
  }
  .log .t {
    color: var(--text-faint);
  }
  .log .tool {
    color: var(--text);
  }
  .log .msg {
    overflow: hidden;
    text-overflow: ellipsis;
    color: var(--text-dim);
  }
  input[type="number"],
  button {
    height: 22px;
    padding: 0 6px;
    border: 1px solid var(--border-strong);
    border-radius: 2px;
    background: var(--surface-1);
    color: var(--text);
    font: inherit;
  }
  input[type="number"] {
    width: 72px;
  }
  button {
    flex-shrink: 0;
    white-space: nowrap;
    cursor: pointer;
  }
  button:hover:not(:disabled) {
    border-color: var(--text-faint);
  }
  button:disabled {
    opacity: 0.5;
    cursor: default;
  }
</style>
