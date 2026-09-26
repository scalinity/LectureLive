<script lang="ts">
  // The transcript (spec §9.1, §9.3): closed utterances as plain paragraphs with their times, the
  // live utterance larger on the teal rule, word by word. Pinned to the end while the reader is there.
  import { session } from "./session.svelte";

  let pinned = $state(true);
  let lastTop = 0;

  function onscroll(e: Event) {
    const el = e.currentTarget as HTMLElement;
    // Only the reader scrolls up; a layout change that moves the end away must not unpin.
    if (el.scrollHeight - el.scrollTop - el.clientHeight < 4) pinned = true;
    else if (el.scrollTop < lastTop - 1) pinned = false;
    lastTop = el.scrollTop;
  }

  /** After each frame, a pane that is pinned follows the newest words. */
  function follow(node: HTMLElement) {
    const off = session.onFrame(() => {
      if (pinned) node.scrollTop = node.scrollHeight;
    });
    return { destroy: off };
  }

  let pane: HTMLElement;
  function jump() {
    pinned = true;
    pane.scrollTop = pane.scrollHeight;
  }
</script>

<section class="pane" aria-label="Transcript">
  <div class="scroll" bind:this={pane} use:follow {onscroll}>
    {#if session.segments.length === 0 && !session.open}
      <p class="empty">The transcript appears here once the lecture starts.</p>
    {/if}
    {#each session.segments as s (s.id)}
      <p class="said">
        <span class="at num">{s.at}{#if s.recovered}<span class="mark">recovered</span>{/if}</span>
        {s.text}
      </p>
    {/each}
    {#if session.open}
      <p class="live" aria-live="off">
        {#each session.open.words as w (w.id)}<span class="w" class:tentative={!w.stable}>{w.text}</span>{" "}{/each}
      </p>
    {/if}
  </div>
  {#if !pinned}
    <button class="jump" onclick={jump}>Jump to live</button>
  {/if}
</section>

<style>
  .pane {
    position: relative;
    min-height: 0;
    display: flex;
  }

  .scroll {
    flex: 1;
    overflow-y: auto;
    padding: 1.25rem 1.25rem 2rem 0;
  }

  .empty {
    margin: 2rem 1.25rem;
    color: var(--graphite);
  }

  .said {
    position: relative;
    margin: 0 0 0.6rem;
    padding-left: 4.9rem;
    /* A two-hour transcript lays out only what is on screen. */
    content-visibility: auto;
    contain-intrinsic-size: auto 3.2em;
  }

  .at {
    position: absolute;
    left: 0;
    width: 4.2rem;
    text-align: right;
    font-size: var(--step--1);
    line-height: calc(var(--step-0) * var(--leading));
    color: var(--graphite);
  }

  .mark {
    display: block;
    font-size: 0.75rem;
    line-height: 1.2;
    font-style: italic;
  }

  .live {
    margin: 1rem 0 0 4.9rem;
    padding-left: 0.75rem;
    border-left: 3px solid var(--teal);
    font-size: var(--step-1);
    line-height: 1.45;
  }

  .w {
    color: var(--ink);
    transition: color var(--fade) ease-out;
    animation: arrive var(--fade) ease-out;
  }

  .w.tentative {
    color: var(--graphite);
  }

  .jump {
    position: absolute;
    right: 1.25rem;
    bottom: 1rem;
    font: inherit;
    font-size: var(--step--1);
    padding: 0.4rem 0.9rem;
    border-radius: var(--radius);
    border: 1px solid var(--teal);
    background: var(--paper);
    color: var(--teal);
    cursor: pointer;
  }
</style>
