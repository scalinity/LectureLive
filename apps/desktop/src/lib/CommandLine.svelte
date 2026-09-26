<script lang="ts">
  // The command line (spec §9.1): the CLI's grammar in one field. Empty ⏎ is a snapshot, a hint then
  // ⏎ a hinted snapshot, `polish` ⏎ a polish. Stop goes Stop → Stop waiting; quitting is the third.
  import FolderPicker from "./FolderPicker.svelte";
  import { session } from "./session.svelte";
  import type { NoticeKind } from "./wire";

  let hint = $state("");
  let history = $state(false);
  let pageRunning = $state(false);

  const MARK: Record<NoticeKind, string> = { notes: "◆", slide: "▣", page: "✦", done: "✓", warn: "▲" };
  const live = $derived(session.status.phase !== "idle" && session.status.phase !== "ended");
  const latest = $derived(session.notices[session.notices.length - 1]);
  // The fallback offer (spec §4.1): the first other input unless the person picked one.
  let pick = $state("");
  const chosen = $derived(session.fallbacks.some((i) => i.uid === pick) ? pick : (session.fallbacks[0]?.uid ?? ""));

  async function submit(e: SubmitEvent) {
    e.preventDefault();
    const h = hint;
    hint = "";
    await session.snapshot(h);
  }

  async function page() {
    pageRunning = true;
    await session.openPage();
    pageRunning = false;
  }
</script>

<footer class="command">
  {#if live && session.status.input_gone}
    <div class="offer" role="alert">
      <p><span class="mark warn">▲</span> <strong>{session.status.source ?? session.status.input_gone}</strong> is unplugged. LectureLive waits for it and records nothing meanwhile.</p>
      {#if session.fallbacks.length}
        <label class="from">
          Record from
          <select value={chosen} onchange={(e) => (pick = e.currentTarget.value)}>
            {#each session.fallbacks as i (i.uid)}<option value={i.uid}>{i.name}</option>{/each}
          </select>
        </label>
        <button class="outline" type="button" onclick={() => session.useInput(chosen)}>Record from it</button>
      {:else}
        <p>No other input is connected.</p>
      {/if}
    </div>
  {:else if session.error}
    <p class="line error" role="alert">▲ {session.error}</p>
  {:else if latest}
    <button class="line" onclick={() => (history = !history)} aria-expanded={history}>
      <span class="mark {latest.kind}">{MARK[latest.kind]}</span> <strong>{latest.label}</strong> {latest.detail}
    </button>
  {/if}
  {#if history}
    <ol class="history">
      {#each session.notices.slice(-8).reverse() as n, i (i)}
        <li><span class="num at">{n.at}</span> <span class="mark {n.kind}">{MARK[n.kind]}</span> <strong>{n.label}</strong> {n.detail}</li>
      {/each}
    </ol>
  {/if}

  {#if live}
    <form class="prompt" onsubmit={submit}>
      <label class="field">
        <span class="diamond" aria-hidden="true">◆</span>
        <input bind:value={hint} placeholder="a hint, or ⏎ for a snapshot" aria-label="Hint for the next snapshot" disabled={!session.canSnapshot} />
      </label>
      <button class="primary" type="submit" disabled={!session.canSnapshot}>Snapshot</button>
      {#if session.busyOp}<button class="cancel" type="button" onclick={() => session.cancel()}>Cancel</button>{/if}
      <button class="outline" type="button" onclick={() => session.polish()} disabled={!session.canPolish}>Polish</button>
      <button class="outline" type="button" onclick={page} disabled={pageRunning}>{pageRunning ? "Typesetting" : "Study page"}</button>
      {#if session.stopLabel}
        <button class={session.stopLabel === "Stop" ? "stop" : "stop waiting"} type="button" onclick={() => session.stop()}>{session.stopLabel}</button>
      {/if}
    </form>
  {:else}
    <div class="idle">
      <FolderPicker />
      {#if session.folder && session.document}
        <!-- After class (decision D1): one operation holds the folder at a time, so each waits for the other. -->
        <div class="after">
          <button class="outline" onclick={() => session.polish()} disabled={!session.canPolish || pageRunning}>Polish</button>
          <button class="outline" onclick={page} disabled={pageRunning || !!session.status.busy}>{pageRunning ? "Typesetting" : "Study page"}</button>
        </div>
      {/if}
    </div>
  {/if}
</footer>

<style>
  .command {
    background: var(--plate);
    border-top: 1px solid var(--rule);
    padding: 0.5rem 1.25rem 0.8rem;
  }

  .line {
    display: block;
    width: 100%;
    margin: 0 0 0.45rem;
    padding: 0;
    text-align: left;
    font: inherit;
    font-size: var(--step--1);
    color: var(--graphite);
    background: none;
    border: 0;
    cursor: pointer;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .line strong,
  .history strong {
    color: var(--ink);
    font-weight: 700;
    margin-right: 0.3em;
  }

  .error {
    color: var(--signal);
    cursor: default;
    white-space: normal;
  }

  /* The fallback offer: a question in ink, not an alarm; it stays until the input returns or one is chosen. */
  .offer {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 0.4rem 0.8rem;
    margin: 0 0 0.55rem;
    font-size: var(--step--1);
    color: var(--ink);
  }

  .offer p {
    margin: 0;
  }

  .offer strong {
    font-weight: 700;
  }

  .offer .from {
    display: inline-flex;
    align-items: center;
    gap: 0.5rem;
    color: var(--graphite);
  }

  .offer select {
    font: inherit;
    padding: 0.25rem 0.45rem;
    border-radius: var(--radius);
    border: 1px solid var(--rule);
    background: var(--paper);
    color: var(--ink);
  }

  .mark {
    color: var(--teal);
  }

  .mark.warn {
    color: var(--signal);
  }

  .history {
    list-style: none;
    margin: 0 0 0.6rem;
    padding: 0.4rem 0;
    border-top: 1px solid var(--rule);
    font-size: var(--step--1);
    color: var(--graphite);
    max-height: 12rem;
    overflow-y: auto;
  }

  .history .at {
    display: inline-block;
    width: 4.5rem;
  }

  .prompt {
    display: flex;
    align-items: center;
    gap: 0.6rem;
  }

  .field {
    flex: 1;
    display: flex;
    align-items: center;
    gap: 0.6rem;
    min-width: 12rem;
    padding: 0.2rem 0.8rem;
    border: 1.5px solid var(--teal);
    border-radius: var(--radius);
    background: var(--paper);
  }

  .diamond {
    color: var(--teal);
    font-size: var(--step-1);
  }

  input {
    flex: 1;
    min-width: 0;
    font: inherit;
    font-size: var(--step-1);
    padding: 0.35rem 0;
    border: 0;
    background: transparent;
    color: var(--ink);
    outline: none;
  }

  input::placeholder {
    color: var(--graphite);
  }

  .idle {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 1rem;
  }
  .after {
    display: flex;
    gap: 0.6rem;
  }

  /* Buttons: one filled action per state; the rest outlined. */
  :global(.command button.primary),
  :global(.command button.outline),
  .cancel,
  .stop {
    font: inherit;
    padding: 0.5rem 1rem;
    border-radius: var(--radius);
    border: 1.5px solid var(--teal);
    cursor: pointer;
    white-space: nowrap;
  }

  :global(.command button.primary) {
    background: var(--teal);
    color: var(--paper);
    font-weight: 700;
  }

  :global(.command button.outline) {
    background: transparent;
    color: var(--teal);
  }

  :global(.command button:disabled) {
    opacity: 0.45;
    cursor: default;
  }

  .cancel {
    background: transparent;
    border-color: var(--signal);
    color: var(--signal);
  }

  .stop {
    margin-left: 0.6rem;
    background: var(--signal);
    border-color: var(--signal);
    color: var(--paper);
    font-weight: 700;
  }

  .stop.waiting {
    background: transparent;
    color: var(--signal);
  }

  :global(.command .quiet) {
    font: inherit;
    font-size: var(--step--1);
    background: none;
    border: 0;
    color: var(--teal);
    cursor: pointer;
  }
</style>
