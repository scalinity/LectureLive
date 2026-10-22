<script lang="ts">
  import { session } from "./session.svelte";

  let { onChoose, showWindow = false }: { onChoose: () => void; showWindow?: boolean } = $props();
  const running = $derived(session.status.phase === "running");
</script>

{#if showWindow}<p class="window">{session.capture.window ?? "No window chosen"}</p>{/if}
<div class="capture-actions">
  {#if running}
    <button class="outline" type="button" onclick={() => session.captureNow()} disabled={!session.canCapture} title={session.canCapture ? "Capture the selected region now (⌘⇧2)" : "Choose a window and slide region first"}>Capture</button>
    <button class="outline" type="button" onclick={() => session.setAutoCapture(!session.captureMode.automatic)} disabled={!session.canSetAutoCapture} title="Change slide capture only; recording and transcription continue">{session.captureMode.automatic ? "Pause auto-capture" : "Resume auto-capture"}</button>
  {/if}
  <button class={session.capture.window ? "quiet" : "outline"} type="button" onclick={onChoose} disabled={!session.folder} title={session.folder ? "Choose the window and slide region" : "Open a lecture folder first"}>{session.capture.window ? "Choose…" : "Choose window"}</button>
</div>
{#if running && session.captureGuidance}<p class="guidance" role="status">{session.captureGuidance}</p>{/if}

<style>
  .window { margin: 0; color: var(--ink); overflow-wrap: anywhere; }
  .capture-actions { display: flex; align-items: center; flex-wrap: wrap; gap: 0.5rem; margin-top: 0.6rem; }
  button { font: inherit; padding: 0.3rem 0.75rem; border-radius: var(--radius); border: 1.5px solid var(--teal); cursor: pointer; }
  button:disabled { opacity: 0.45; cursor: default; }
  .outline { background: transparent; color: var(--teal); }
  .quiet { border: 0; padding: calc(0.3rem + 1.5px) 0; background: transparent; color: var(--graphite); }
  .quiet:hover { color: var(--ink); }
  .guidance { margin: 0.5rem 0 0; color: var(--graphite); font-size: var(--step--1); overflow-wrap: anywhere; }
</style>
