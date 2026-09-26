<script lang="ts">
  // The status strip (spec §9.1): what the app is doing, what it hears, and what it has cost today.
  import { session } from "./session.svelte";
  import type { Phase } from "./wire";

  let { onSpend, onKey }: { onSpend: () => void; onKey: () => void } = $props();

  /** The phase in one word, and what it means while it lasts. */
  const PHASE: Record<Phase, [string, string]> = {
    idle: ["Ready", ""],
    starting: ["Starting", ""],
    running: ["Listening", ""],
    stopping: ["Stopping", "finishing the transcript and recovery, then a last snapshot"],
    stopping_now: ["Stopping", "no longer waiting for recovery"],
    ended: ["Stopped", ""],
  };

  const recording = $derived(["starting", "running", "stopping", "stopping_now"].includes(session.status.phase));
  /** −60 dBFS is silence, 0 is full scale. */
  const meter = $derived(session.status.level_dbfs === null ? 0 : Math.min(1, Math.max(0, (session.status.level_dbfs + 60) / 60)));

  function clock(s: number): string {
    const h = Math.floor(s / 3600);
    const m = Math.floor((s % 3600) / 60);
    return `${h}:${String(m).padStart(2, "0")}:${String(s % 60).padStart(2, "0")}`;
  }

  function money(usd: number): string {
    return usd > 0 && usd < 0.005 ? "<$0.01" : `$${usd.toFixed(2)}`;
  }

  let large = $state(document.documentElement.classList.contains("large"));
  function toggleLarge() {
    large = !large;
    document.documentElement.classList.toggle("large", large);
    try {
      localStorage.setItem("lecturelive.large", large ? "1" : "0");
    } catch {
      // private storage unavailable: the choice lasts this window only
    }
  }
</script>

<header class="strip" data-tauri-drag-region="deep">
  <div class="left">
    <span class="phase" class:on={recording}>
      {#if recording}<span class="dot" aria-hidden="true"></span>{/if}{PHASE[session.status.phase][0]}
    </span>
    {#if PHASE[session.status.phase][1]}<span class="why" title={PHASE[session.status.phase][1]}>{PHASE[session.status.phase][1]}</span>{/if}
    {#if session.folder}
      <span class="folder" title="{session.folder.course} › {session.folder.name}"><span class="course">{session.folder.course} ›</span> {session.folder.name}</span>
    {/if}
    {#if session.status.source}
      <span class="source" title={session.status.source}>
        <span class="source-name">{session.status.source}</span>
        <span class="meter" class:silent={session.status.silence} role="meter" aria-label="Input level" aria-valuemin={-60} aria-valuemax={0} aria-valuenow={session.status.level_dbfs ?? -60}>
          <span class="fill" style:width="{meter * 100}%"></span>
        </span>
      </span>
    {/if}
    {#if session.elapsed !== null && recording}<span class="num">{clock(session.elapsed)}</span>{/if}
  </div>
  <div class="right">
    {#if session.status.phase !== "idle"}
      <span class:bad={!session.status.stt_ok}>{session.status.stt}</span>
      <span class="num" class:bad={session.status.gaps > 0}>{session.status.gaps} {session.status.gaps === 1 ? "gap" : "gaps"}</span>
    {/if}
    <button class="quiet num" onclick={onSpend} title="What LectureLive has cost">{money(session.status.spend_usd)} today</button>
    <button class="quiet" onclick={toggleLarge} aria-pressed={large} title="Larger type">Aa</button>
    <button class="quiet" onclick={onKey}>Key</button>
  </div>
</header>

<style>
  /* The window has no title bar: the strip is its top edge. The traffic lights sit in its left end (fixed in
     pixels by tauri.conf.json, whatever the type size), and anything but a button drags the window. */
  .strip {
    display: flex;
    justify-content: space-between;
    align-items: center;
    gap: 1.5rem;
    padding: 0.55rem 1.25rem 0.55rem calc(80px + 1.25rem);
    background: var(--plate);
    border-bottom: 1px solid var(--rule);
    font-size: var(--step--1);
    min-height: 3.2rem;
  }

  .left,
  .right {
    display: flex;
    align-items: center;
    gap: 1.5rem;
    min-width: 0;
  }

  .left {
    flex: 1;
    overflow: hidden;
  }

  .left > *,
  .right > * {
    white-space: nowrap;
  }

  /* The explanation and the folder give way first; the phase, meter and clock stay whole. */
  .why,
  .folder {
    flex-shrink: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .phase,
  .source,
  .num {
    flex-shrink: 0;
  }

  .right {
    flex-shrink: 0;
  }

  .why {
    color: var(--graphite);
    overflow: hidden;
    text-overflow: ellipsis;
    min-width: 0;
  }

  .phase {
    font-weight: 700;
    font-size: var(--step-0);
    color: var(--graphite);
  }

  .phase.on {
    color: var(--ink);
  }

  .dot {
    display: inline-block;
    width: 0.6em;
    height: 0.6em;
    margin-right: 0.45em;
    border-radius: 50%;
    background: var(--signal);
  }

  .course {
    color: var(--graphite);
  }

  .source {
    display: inline-flex;
    align-items: center;
    gap: 0.6rem;
  }

  .meter {
    display: inline-block;
    width: 6rem;
    height: 0.5rem;
    background: var(--rule);
    border-radius: 3px;
    overflow: hidden;
  }

  .fill {
    display: block;
    height: 100%;
    background: var(--teal);
  }

  .meter.silent .fill {
    background: var(--signal);
  }

  @media (max-width: 1100px) {
    .source-name {
      display: none;
    }
  }

  /* Large type is 125%: the name goes at 1320 px, so the clock stays whole beside the traffic lights. */
  @media (max-width: 1320px) {
    :global(.large) .source-name {
      display: none;
    }
  }

  .bad {
    color: var(--signal);
  }

  .quiet {
    font: inherit;
    color: var(--ink);
    background: none;
    border: 1px solid transparent;
    border-radius: var(--radius);
    padding: 0.2rem 0.5rem;
    cursor: pointer;
  }

  .quiet:hover {
    border-color: var(--rule);
  }

  .quiet[aria-pressed="true"] {
    border-color: var(--teal);
    color: var(--teal);
  }
</style>
