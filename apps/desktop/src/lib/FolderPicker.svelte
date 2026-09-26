<script lang="ts">
  // Before a lecture: the folder, the source, Start (spec §9.1); the microphone's fix-it when macOS denies it (§10).
  import { isTauri } from "@tauri-apps/api/core";
  import { open } from "@tauri-apps/plugin-dialog";
  import { session } from "./session.svelte";
  import type { InputView, LoopbackView } from "./wire";

  let inputs = $state<InputView[]>([]);
  let loop = $state<LoopbackView | undefined>(undefined);
  let mic = $state("granted");
  let source = $state("loopback");
  let starting = $state(false);

  const others = $derived(inputs.filter((i) => i.uid !== "BlackHole2ch_UID"));
  const micOff = $derived(mic === "denied" || mic === "restricted");
  const together = $derived(source.startsWith("mixed:"));

  // Read once when the bar appears; "Refresh" reads them again after a device is plugged in or access is turned on.
  async function refresh() {
    [inputs, loop, mic] = await Promise.all([session.inputs(), session.loopback(), session.microphone()]);
    if (!loop?.blackhole_present && (source === "loopback" || source.startsWith("mixed:"))) source = others[0]?.uid ?? "";
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

<div class="before">
  {#if micOff}
    <p class="fixit" role="alert">
      <span class="mark">▲</span> Microphone access is off, so nothing can be recorded. Turn it on in System Settings, then press Refresh.
      <button class="outline" onclick={() => session.openMicrophoneSettings()}>Open Settings</button>
    </p>
  {/if}
  <div class="picker">
    <button class={session.folder ? "outline" : "primary"} onclick={choose}>{session.folder ? "Change folder" : "Choose lecture folder"}</button>
    {#if session.folder}
      <label class="source">
        <span>Listen to</span>
        <select bind:value={source}>
          {#if loop?.blackhole_present}<option value="loopback">Zoom through LectureLive Loopback</option>{/if}
          {#each others as i (i.uid)}<option value={i.uid}>{i.name}</option>{/each}
          {#if loop?.mixed}{#each others as i (i.uid)}<option value="mixed:{i.uid}">Zoom and {i.name} together</option>{/each}{/if}
        </select>
      </label>
      <button class="quiet" onclick={refresh}>Refresh</button>
      {#if loop && !loop.blackhole_present}<span class="hint">For Zoom audio, install BlackHole 2ch: brew install blackhole-2ch</span>{/if}
      <button class="primary" onclick={start} disabled={starting || !source || micOff} title={micOff ? "Microphone access is off" : undefined}>{starting ? "Starting" : "Start"}</button>
    {/if}
  </div>
  {#if session.folder && together}<p class="hint">Wear headphones, so the microphone does not hear Zoom as well.</p>{/if}
</div>

<style>
  .before {
    display: flex;
    flex-direction: column;
    gap: 0.45rem;
    min-width: 0;
  }

  .picker {
    display: flex;
    align-items: center;
    gap: 1rem;
    flex-wrap: wrap;
  }

  /* The command bar's error line, in its words and marks: what stopped, and the one click that fixes it. */
  .fixit {
    margin: 0;
    font-size: var(--step--1);
    color: var(--signal);
    line-height: 1.5;
  }

  .fixit button {
    margin-left: 0.5rem;
  }

  .source {
    display: inline-flex;
    align-items: center;
    gap: 0.6rem;
  }

  p.hint {
    margin: 0;
  }

  .source span,
  .hint {
    color: var(--graphite);
    font-size: var(--step--1);
  }

  /* Sized to fit the row with Start at 1,168 px in large type; a long label is cut in the closed select, whole in its list. */
  :global(:root.large) .picker select {
    max-width: 17rem;
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
