<script lang="ts">
  // The slides strip (spec §9.1, §7): which window is watched and what it needs, then every slide in
  // time order with when it was first on screen and how it was taken. Newest at the end, followed
  // while the reader is there, as the panes are.
  import { session } from "./session.svelte";
  import type { SlideView } from "./wire";

  let { toUrl, onChoose }: { toUrl: (abs: string) => string; onChoose: () => void } = $props();

  const running = $derived(session.status.phase === "running");
  const c = $derived(session.capture);
  const single = $derived(c.state === "asking" && c.candidates.length === 1 ? c.candidates[0] : null);

  /** The worker's reasons are fragments of notice sentences; here each starts a sentence. */
  const sentence = (s: string | null) => (s ? s[0].toUpperCase() + s.slice(1) : "");

  function badge(s: SlideView): { word: string; why: string } {
    if (s.uncertain) return { word: "unsettled", why: "Taken automatically after 10 s of change: it may show a transition" };
    return s.auto ? { word: "auto", why: "Taken automatically when the slide changed" } : { word: "manual", why: "Taken by you" };
  }

  let pinned = true;
  let lastTop = 0;
  function onscroll(e: Event) {
    const el = e.currentTarget as HTMLElement;
    if (el.scrollHeight - el.scrollTop - el.clientHeight < 4) pinned = true;
    else if (el.scrollTop < lastTop - 1) pinned = false;
    lastTop = el.scrollTop;
  }
  function follow(node: HTMLElement) {
    return { destroy: session.onFrame(() => pinned && (node.scrollTop = node.scrollHeight)) };
  }
</script>

<aside class="strip" aria-label="Slides">
  <div class="binding" class:live={c.state === "watching"} class:bad={c.state === "denied" || c.state === "failing"}>
    {#if c.state === "watching"}
      <p class="state">Watching</p>
      <p class="window">{c.window}</p>
      <div class="actions">
        <button class="outline" onclick={() => session.captureNow()} disabled={!session.canCapture} title={session.canCapture ? "Capture the slide now (⌘⇧2)" : "Available once the window has been captured"}>Capture</button>
        <span class="keys" aria-hidden="true">⌘⇧2</span>
        <button class="quiet" onclick={onChoose}>Choose…</button>
      </div>
    {:else if c.state === "ready"}
      <p class="window">{c.window}</p>
      <p class="note">Chosen for this course. Watching starts with the lecture.</p>
      <div class="actions"><button class="quiet" onclick={onChoose}>Choose…</button></div>
    {:else if c.state === "paused"}
      <p class="state">Paused</p>
      <p class="window">{c.window}</p>
      <p class="note">{sentence(c.detail)}.</p>
      <div class="actions"><button class="quiet" onclick={onChoose}>Choose…</button></div>
    {:else if c.state === "asking"}
      <p class="ask">{sentence(c.detail)}. {single ? "Watch it?" : "Choose the window to watch."}</p>
      <div class="actions">
        {#if single && running}<button class="primary" onclick={() => session.captureWatch(single.id)}>Watch it</button>{/if}
        <button class="quiet" onclick={onChoose}>Choose…</button>
      </div>
    {:else if c.state === "denied"}
      <p class="ask">Screen Recording is off for LectureLive.</p>
      <div class="actions">
        <button class="outline" onclick={() => session.openScreenSettings()}>Open Settings</button>
        <button class="quiet" onclick={onChoose}>Choose…</button>
      </div>
      <p class="note">Allow LectureLive there, then quit and reopen LectureLive.</p>
    {:else if c.state === "failing"}
      <p class="ask">{c.window} could not be captured: {c.detail}.</p>
      <div class="actions"><button class="quiet" onclick={onChoose}>Choose…</button></div>
    {:else}
      <p class="window">No window chosen</p>
      <p class="note">Choose Zoom's window and the part that shows the slide.</p>
      <div class="actions"><button class="outline" onclick={onChoose} disabled={!session.folder} title={session.folder ? "" : "Open a lecture folder first"}>Choose window</button></div>
    {/if}
  </div>

  <div class="body">
    <div class="list" use:follow {onscroll}>
      {#if session.slides.length === 0}
        <p class="empty">Screenshots you take during the lecture become slides.{#if c.state !== "unbound"}{" "}Zoom's slides are added as they change.{/if}</p>
      {:else}
        <ol>
          {#each session.slides as s (s.index)}
            {@const b = badge(s)}
            <li>
              <span class="at"><span class="num">{s.at}</span><span class="badge" class:unsettled={s.uncertain} title={b.why}>{b.word}</span></span>
              <img src={toUrl(s.path)} alt="Slide {s.index}, {s.auto ? 'taken automatically' : 'taken by you'} at {s.at}" title="Slide {s.index}" loading="lazy" decoding="async" />
            </li>
          {/each}
        </ol>
      {/if}
    </div>
    {#if session.dragging}
      <div class="drop" role="status"><span>{running ? "Drop images to add them as slides" : "Start the lecture to add slides"}</span></div>
    {/if}
  </div>
</aside>

<style>
  .strip {
    display: flex;
    flex-direction: column;
    min-height: 0;
    font-size: var(--step--1);
  }

  .binding {
    padding: 1rem 1rem 0.9rem 1rem;
    border-bottom: 1px solid var(--rule);
    border-left: 3px solid transparent;
  }

  /* What is live: the watched window. */
  .binding.live {
    border-left-color: var(--teal);
  }

  .binding p {
    margin: 0;
  }

  .state {
    color: var(--graphite);
  }

  .live .state {
    color: var(--teal);
  }

  .window {
    font-size: var(--step-0);
    color: var(--ink);
    overflow-wrap: anywhere;
  }

  .binding .note {
    margin-top: 0.2rem;
    color: var(--graphite);
  }

  .ask {
    font-size: var(--step-0);
    color: var(--ink);
  }

  .bad .ask {
    color: var(--signal);
  }

  .actions {
    display: flex;
    align-items: center;
    flex-wrap: wrap;
    gap: 0.5rem;
    margin-top: 0.6rem;
  }

  button {
    font: inherit;
    padding: 0.3rem 0.75rem;
    border-radius: var(--radius);
    border: 1.5px solid var(--teal);
    cursor: pointer;
  }

  button:disabled {
    opacity: 0.45;
    cursor: default;
  }

  .primary {
    background: var(--teal);
    color: var(--paper);
    font-weight: 700;
  }

  .outline {
    background: transparent;
    color: var(--teal);
  }

  .quiet {
    border: 0;
    padding: calc(0.3rem + 1.5px) 0;
    background: transparent;
    color: var(--graphite);
  }

  .quiet:hover {
    color: var(--ink);
  }

  .keys {
    color: var(--graphite);
    font-variant-numeric: tabular-nums;
  }

  .body {
    position: relative;
    flex: 1;
    min-height: 0;
    display: flex;
  }

  .list {
    flex: 1;
    min-height: 0;
    overflow-y: auto;
    padding: 1rem 1rem 2rem 0;
  }

  .empty {
    margin: 0 0 0 1rem;
    color: var(--graphite);
  }

  ol {
    margin: 0;
    padding: 0;
    list-style: none;
  }

  li {
    position: relative;
    padding-left: 4.9rem;
    margin-bottom: 1rem;
    animation: arrive var(--fade) ease-out;
  }

  /* The hanging gutter: when it was first on screen, and how it was taken. */
  .at {
    position: absolute;
    left: 0;
    width: 4.2rem;
    text-align: right;
    color: var(--graphite);
  }

  .badge {
    display: block;
    font-size: 0.75rem;
    line-height: 1.2;
    font-style: italic;
  }

  .badge.unsettled {
    color: var(--ink);
  }

  img {
    display: block;
    width: 100%;
    height: auto;
    border: 1px solid var(--rule);
  }

  /* Covers the list while files are dragged over the window. */
  .drop {
    position: absolute;
    inset: 0;
    padding: 0.5rem 0.75rem;
    background: var(--paper);
  }

  .drop span {
    display: grid;
    place-items: center;
    height: 100%;
    padding: 1rem;
    text-align: center;
    border: 2px dashed var(--teal);
    border-radius: var(--radius);
    color: var(--teal);
    font-size: var(--step-0);
  }
</style>
