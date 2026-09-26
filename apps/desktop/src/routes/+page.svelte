<script lang="ts">
  // The lecture window (spec §9.1): status strip, transcript | notes | slides, command line.
  import "$lib/theme.css";
  import { isTauri } from "@tauri-apps/api/core";
  import { demoTransport } from "$lib/fixture";
  import { session } from "$lib/session.svelte";
  import { tauriTransport, type Transport } from "$lib/transport";
  import Transcript from "$lib/Transcript.svelte";

  /** The app talks to Tauri; the browser preview (`?fixture=demo`) plays a scripted lecture. */
  function transport(): Transport {
    if (isTauri()) return tauriTransport();
    return demoTransport();
  }

  const t = transport();
  const started = session.init(t);
</script>

<div class="window">
  <header class="strip"></header>
  <main class="panes">
    <Transcript />
    <section class="notes" aria-label="Notes"></section>
    <aside class="slides" aria-label="Slides">
      {#if session.slides.length === 0}
        <p class="quiet">Screenshots you take during the lecture become slides.</p>
      {:else}
        <ol>
          {#each session.slides as s (s.index)}<li class="num">Slide {s.index}</li>{/each}
        </ol>
      {/if}
    </aside>
  </main>
  <footer class="command"></footer>
</div>
{#await started catch e}
  <p class="fatal" role="alert">The app could not reach its backend: {String(e)}</p>
{/await}

<style>
  .window {
    height: 100vh;
    display: grid;
    grid-template-rows: auto minmax(0, 1fr) auto;
  }

  .strip,
  .command {
    background: var(--plate);
    min-height: 3.2rem;
  }

  .strip {
    border-bottom: 1px solid var(--rule);
  }

  .command {
    border-top: 1px solid var(--rule);
  }

  .panes {
    display: grid;
    grid-template-columns: minmax(20rem, 34fr) minmax(28rem, 56fr) 10rem;
    min-height: 0;
  }

  .panes > :global(*) {
    min-height: 0;
    border-right: 1px solid var(--rule);
  }

  .panes > :global(*:last-child) {
    border-right: 0;
  }

  .slides {
    padding: 1.25rem 1rem;
    font-size: var(--step--1);
    overflow-y: auto;
  }

  .slides ol {
    margin: 0;
    padding: 0;
    list-style: none;
  }

  .quiet {
    margin: 0;
    color: var(--graphite);
  }

  @media (max-width: 1100px) {
    .panes {
      grid-template-columns: minmax(18rem, 38fr) minmax(24rem, 62fr);
    }
    .slides {
      display: none;
    }
  }

  .fatal {
    position: fixed;
    inset: auto 1rem 1rem;
    padding: 0.75rem 1rem;
    border-radius: var(--radius);
    background: var(--signal-soft);
    color: var(--signal);
  }
</style>
