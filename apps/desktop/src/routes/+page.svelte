<script lang="ts">
  // The lecture window (spec §9.1): status strip, transcript | notes | slides, command line.
  import "$lib/theme.css";
  import { isTauri } from "@tauri-apps/api/core";
  import { demoWithNotes } from "$lib/fixture";
  import { session } from "$lib/session.svelte";
  import { tauriTransport, type Transport } from "$lib/transport";
  import CommandLine from "$lib/CommandLine.svelte";
  import KeyDialog from "$lib/KeyDialog.svelte";
  import Notes from "$lib/Notes.svelte";
  import SpendView from "$lib/SpendView.svelte";
  import StatusStrip from "$lib/StatusStrip.svelte";
  import Transcript from "$lib/Transcript.svelte";

  /** The app talks to Tauri; the browser preview (`?fixture=demo`) plays a scripted lecture. */
  function transport(): Transport {
    if (isTauri()) return tauriTransport();
    return demoWithNotes();
  }

  try {
    if (localStorage.getItem("lecturelive.large") === "1") document.documentElement.classList.add("large");
  } catch {
    // private storage unavailable: normal type
  }

  let keyDialog: KeyDialog;
  let spendView: SpendView;
  const t = transport();
  const started = session.init(t).then(async () => {
    if (!(await session.keyStatus())?.stored) keyDialog.show();
  });
</script>

<div class="window">
  <StatusStrip onSpend={() => spendView.show()} onKey={() => keyDialog.show()} />
  <main class="panes">
    <Transcript />
    <Notes toUrl={(p) => t.assetUrl(p)} />
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
  <CommandLine />
</div>
<KeyDialog bind:this={keyDialog} />
<SpendView bind:this={spendView} />
{#await started catch e}
  <p class="fatal" role="alert">The app could not reach its backend: {String(e)}</p>
{/await}

<style>
  .window {
    height: 100vh;
    display: grid;
    grid-template-rows: auto minmax(0, 1fr) auto;
    grid-template-columns: minmax(0, 1fr);
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
