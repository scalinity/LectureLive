<script lang="ts">
  import { session } from "./session.svelte";

  let { onChoose, showWindow = false }: { onChoose: () => void; showWindow?: boolean } = $props();
  const running = $derived(session.status.phase === "running");
  const c = $derived(session.capture);
  const auto = $derived(session.captureMode.automatic);
  const selected = $derived(session.captureMode.selected);
  /** The window cannot be captured right now; the worker keeps looking by itself while the lecture runs. */
  const recovering = $derived(c.state === "asking" || c.state === "paused" || c.state === "failing");
  const needsChoice = $derived(c.state === "unbound" || c.state === "asking" || c.state === "paused");
  const sentence = (s: string | null) => (s ? s[0].toUpperCase() + s.slice(1).replace(/[.\s]+$/, "") + "." : "");

  /** One line, three separate causes: the lecture's own Pause, the screenshot pause, and a window that is away. */
  const status = $derived.by((): { tone: "live" | "idle" | "bad"; text: string } | null => {
    if (!running) return null;
    if (session.paused) return { tone: "idle", text: "Lecture paused. Nothing is recorded or transcribed, and automatic screenshots wait until you resume." };
    if (!auto) return { tone: "idle", text: "Automatic screenshots are paused. Recording and transcription continue." };
    if (c.state === "denied") return { tone: "bad", text: "Screen Recording is off for LectureLive, so no screenshots can be taken." };
    if (recovering) {
      const why = sentence(c.detail) || (c.state === "failing" ? "The window could not be captured." : "Looking for the Zoom window.");
      return { tone: c.state === "failing" ? "bad" : "idle", text: `${why} LectureLive will keep checking while automatic screenshots are on.` };
    }
    if (c.state === "watching") return { tone: "live", text: "Automatic screenshots are on." };
    return null;
  });
  const captureTitle = $derived(
    session.canCapture ? "Take a screenshot of the selected region now (⌘⇧2)" : "Choose a window and slide region first",
  );
</script>

{#if showWindow}<p class="window">{c.window ?? "No window chosen"}</p>{/if}
<div class="capture-actions">
  {#if running}
    <button class="outline" type="button" onclick={() => session.captureNow()} disabled={!session.canCapture || !selected} title={captureTitle}>Capture</button>
    <button class="outline" type="button" onclick={() => session.setAutoCapture(!auto)} disabled={!session.canSetAutoCapture} title="Changes automatic screenshots only; recording and transcription continue">{auto ? "Pause auto-capture" : "Resume auto-capture"}</button>
  {/if}
  <button class={needsChoice || !c.window ? "outline" : "quiet"} type="button" onclick={onChoose} disabled={!session.folder} title={session.folder ? "Choose the window and the slide region" : "Open a lecture folder first"}>{!c.window ? "Choose window" : needsChoice || c.state === "failing" ? "Choose window or region" : "Choose…"}</button>
</div>
{#if running && c.state === "denied"}<button class="quiet" type="button" onclick={() => session.openScreenSettings()}>Open Screen Recording settings</button>{/if}
{#if status}<p class="status {status.tone}" role="status">{status.text}</p>{/if}
{#if running && !session.paused && !auto && session.captureGuidance}<p class="guidance">{session.captureGuidance}</p>{/if}

<style>
  .window { margin: 0; color: var(--ink); overflow-wrap: anywhere; }
  .capture-actions { display: flex; align-items: center; flex-wrap: wrap; gap: 0.5rem; margin-top: 0.6rem; }
  button { font: inherit; padding: 0.3rem 0.75rem; border-radius: var(--radius); border: 1.5px solid var(--teal); cursor: pointer; }
  button:disabled { opacity: 0.45; cursor: default; }
  .outline { background: transparent; color: var(--teal); }
  .quiet { border: 0; padding: calc(0.3rem + 1.5px) 0; background: transparent; color: var(--graphite); }
  .quiet:hover { color: var(--ink); }
  .status, .guidance { margin: 0.5rem 0 0; font-size: var(--step--1); overflow-wrap: anywhere; color: var(--graphite); }
  .status { padding-left: 0.6rem; border-left: 2px solid var(--rule); }
  .status.live { border-left-color: var(--teal); color: var(--ink); }
  .status.bad { border-left-color: var(--signal); color: var(--ink); }
</style>
