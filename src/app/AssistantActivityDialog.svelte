<!--
  SPDX-License-Identifier: GPL-3.0-or-later
  Copyright (c) The GeekCast Radio Network, LLC.
  This file is part of Altered Audio Editor.

  Edit > Assistant Activity: every request an AI assistant made through
  the assistant connection since the app started, newest at the bottom,
  with what it asked for and what came of it. Edits it made are also in
  History as "Assistant: ..." steps.
-->
<script lang="ts">
  import { onMount } from "svelte";
  import Dialog from "./Dialog.svelte";
  import { assistantActivity, assistantClearActivity, onAssistantActivity, type AssistantEntry, type AssistantStatus } from "./core";

  interface Props {
    status: AssistantStatus | null;
    onSettings: () => void;
    onClose: () => void;
  }
  const { status, onSettings, onClose }: Props = $props();

  let entries = $state<AssistantEntry[]>([]);
  let list = $state<HTMLOListElement | null>(null);

  onMount(() => {
    void assistantActivity(500).then((l) => (entries = l), () => {});
    let off: (() => void) | undefined;
    let gone = false;
    void onAssistantActivity((e) => (entries = [...entries.slice(-499), e])).then((f) => {
      if (gone) f();
      else off = f;
    });
    return () => {
      gone = true;
      off?.();
    };
  });

  $effect(() => {
    void entries.length;
    list?.lastElementChild?.scrollIntoView({ block: "nearest" });
  });

  async function clear() {
    await assistantClearActivity().catch(() => {});
    entries = [];
  }
</script>

<Dialog title="Assistant Activity" {onClose} width={720}>
  <p class="state">
    {#if !status || !status.enabled}
      The assistant connection is off.
    {:else if status.problem}
      {status.problem}
    {:else}
      Listening at {status.url}. Changes are {status.allowEdits ? "allowed" : "not allowed (read only)"}.
    {/if}
  </p>
  {#if entries.length === 0}
    <p class="empty">Nothing yet. Every request an assistant makes is listed here.</p>
  {:else}
    <ol class="rows" bind:this={list}>
      {#each entries as e (e.seq)}
        <li class:fail={!e.ok}>
          <span class="time">{e.time}</span>
          <span class="tool">{e.tool}</span>
          <span class="what">
            {#if e.args}<span class="args">{e.args}</span>{/if}
            <span class="msg">{e.ok ? "" : "Failed: "}{e.message}</span>
          </span>
        </li>
      {/each}
    </ol>
  {/if}
  {#snippet footer()}
    <button type="button" class="start" onclick={onSettings}>Settings...</button>
    <button type="button" disabled={entries.length === 0} onclick={() => void clear()}>Clear</button>
    <button type="button" onclick={onClose}>Close</button>
  {/snippet}
</Dialog>

<style>
  .state {
    margin: 0 0 8px;
    color: var(--text-dim);
  }
  .empty {
    margin: 0;
    color: var(--text-faint);
  }
  .rows {
    max-height: 50vh;
    margin: 0;
    padding: 0;
    overflow: auto;
    list-style: none;
    font-variant-numeric: tabular-nums;
  }
  li {
    display: grid;
    grid-template-columns: 64px 140px minmax(0, 1fr);
    gap: 8px;
    padding: 3px 4px;
    border-bottom: 1px solid var(--border);
  }
  .time {
    color: var(--text-faint);
  }
  .tool {
    color: var(--text);
  }
  li.fail .tool,
  li.fail .msg {
    color: var(--danger);
  }
  .what {
    display: flex;
    flex-direction: column;
    min-width: 0;
  }
  .args {
    overflow: hidden;
    color: var(--text-faint);
    font-family: var(--font-mono);
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .msg {
    color: var(--text-dim);
  }
</style>
