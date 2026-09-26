<script lang="ts">
  // Before a lecture: the folder, the source, Start (spec §9.1).
  import { isTauri } from "@tauri-apps/api/core";
  import { open } from "@tauri-apps/plugin-dialog";
  import { session } from "./session.svelte";
  import type { InputView, LoopbackView } from "./wire";

  let inputs = $state<InputView[]>([]);
  let loop = $state<LoopbackView | undefined>(undefined);
  let source = $state("loopback");
  let starting = $state(false);

  // Read once when the bar appears; "Refresh" reads them again after a device is plugged in.
  async function refresh() {
    [inputs, loop] = await Promise.all([session.inputs(), session.loopback()]);
    if (!loop?.blackhole_present && source === "loopback") source = inputs[0]?.uid ?? "";
  }
  void refresh();

  async function choose() {
    const dir = isTauri() ? await open({ directory: true, title: "Choose the lecture folder" }) : "/Lectures/Machine Learning/Weeks/Week 06 — Optimisation";
    if (typeof dir === "string") await session.selectFolder(dir);
  }

  async function start() {
    starting = true;
    await session.start(source);
    starting = false;
  }
</script>

<div class="picker">
  <button class={session.folder ? "outline" : "primary"} onclick={choose}>{session.folder ? "Change folder" : "Choose lecture folder"}</button>
  {#if session.folder}
    <label class="source">
      <span>Listen to</span>
      <select bind:value={source}>
        {#if loop?.blackhole_present}<option value="loopback">Zoom through LectureLive Loopback</option>{/if}
        {#each inputs.filter((i) => i.uid !== "BlackHole2ch_UID") as i (i.uid)}<option value={i.uid}>{i.name}</option>{/each}
      </select>
    </label>
    <button class="quiet" onclick={refresh}>Refresh</button>
    {#if loop && !loop.blackhole_present}<span class="hint">For Zoom audio, install BlackHole 2ch: brew install blackhole-2ch</span>{/if}
    <button class="primary" onclick={start} disabled={starting || !source}>{starting ? "Starting" : "Start"}</button>
  {/if}
</div>

<style>
  .picker {
    display: flex;
    align-items: center;
    gap: 1rem;
    flex-wrap: wrap;
  }

  .source {
    display: inline-flex;
    align-items: center;
    gap: 0.6rem;
  }

  .source span,
  .hint {
    color: var(--graphite);
    font-size: var(--step--1);
  }

  select {
    font: inherit;
    padding: 0.35rem 0.5rem;
    border-radius: var(--radius);
    border: 1px solid var(--rule);
    background: var(--paper);
    color: var(--ink);
  }
</style>
