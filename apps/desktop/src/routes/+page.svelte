<script lang="ts">
  // The lecture window (spec §9.1): status strip, transcript | notes | slides, command line.
  import "$lib/theme.css";
  import { isTauri } from "@tauri-apps/api/core";
  import { measure } from "$lib/bench";
  import { captureCheck, cspCheck, deckRecordCheck, faultsCheck, fullscreenCheck, liveCheck, pageCheck, recordCheck, zoomCheck } from "$lib/checks";
  import { burstFixture, demoLook, demoWithNotes, FixtureTransport, twoHourFixture } from "$lib/fixture";
  import { session } from "$lib/session.svelte";
  import { tauriTransport, type Transport } from "$lib/transport";
  import CommandLine from "$lib/CommandLine.svelte";
  import KeyDialog from "$lib/KeyDialog.svelte";
  import Notes from "$lib/Notes.svelte";
  import SlidesStrip from "$lib/SlidesStrip.svelte";
  import WindowPicker from "$lib/WindowPicker.svelte";
  import SpendView from "$lib/SpendView.svelte";
  import StatusStrip from "$lib/StatusStrip.svelte";
  import Transcript from "$lib/Transcript.svelte";

  type CheckConfig = { mode: string; dir: string | null; minutes: number | null };

  // The app talks to Tauri; outside it, the browser preview plays a scripted lecture.
  const real = isTauri() ? tauriTransport() : null;
  let current: Transport;

  try {
    if (localStorage.getItem("lecturelive.large") === "1") document.documentElement.classList.add("large");
  } catch {
    // private storage unavailable: normal type
  }

  let keyDialog: KeyDialog;
  let spendView: SpendView;
  /** The window and region picker; from an ask it opens with the reason. */
  let picker: WindowPicker;

  /** A check asked for by the environment (the app) or the URL (`?bench=burst`, the browser preview). */
  async function checkConfig(): Promise<CheckConfig | null> {
    if (real) return real.call<CheckConfig | null>("check_config");
    const bench = new URLSearchParams(location.search).get("bench");
    return bench ? { mode: `bench-${bench}`, dir: null, minutes: null } : null;
  }

  /** Frame work on a gate fixture, through the real store (spec §11); reported, then the app quits. */
  async function bench(mode: string) {
    const fx = mode === "bench-burst" ? burstFixture() : twoHourFixture();
    const ft = new FixtureTransport(fx.state);
    current = ft;
    const t0 = performance.now();
    await session.init(ft);
    await new Promise((r) => requestAnimationFrame(r));
    const m = measure(session, fx.name, performance.now() - t0);
    fx.run(ft, async (notes) => {
      const report = m.stop(notes);
      console.log(JSON.stringify(report));
      (globalThis as { __bench?: unknown }).__bench = report;
      if (real) {
        await real.call("check_report", { name: fx.name, json: JSON.stringify(report) });
        await real.call("exit_app");
      }
    });
  }

  async function boot() {
    const cfg = await checkConfig();
    if (cfg?.mode.startsWith("bench-")) return bench(cfg.mode);
    current = real ?? demoLook(demoWithNotes(), new URLSearchParams(location.search).get("look"));
    await session.init(current);
    if (real && cfg?.mode === "csp" && cfg.dir) return cspCheck(session, real, cfg.dir);
    if (real && cfg?.mode === "live" && cfg.dir) return liveCheck(session, real, cfg.dir);
    if (real && cfg?.mode === "page" && cfg.dir) return pageCheck(session, real, cfg.dir);
    if (real && cfg?.mode === "capture" && cfg.dir) return captureCheck(session, real, cfg.dir);
    if (real && cfg?.mode === "zoom" && cfg.dir) return zoomCheck(session, real, cfg.dir);
    if (real && cfg?.mode === "record" && cfg.dir) return recordCheck(session, real, cfg.dir);
    if (real && cfg?.mode === "deck-record" && cfg.dir) return deckRecordCheck(session, real, cfg.dir);
    if (real && cfg?.mode === "fullscreen" && cfg.dir) return fullscreenCheck(session, real, cfg.dir);
    if (real && cfg?.mode === "faults" && cfg.dir) return faultsCheck(session, real, cfg.dir, cfg.minutes ?? 10);
    if (!(await session.keyStatus())?.stored) keyDialog.show();
  }
  const started = boot();
</script>

<div class="window">
  <StatusStrip onSpend={() => spendView.show()} onKey={() => keyDialog.show()} />
  <main class="panes">
    <Transcript />
    <Notes toUrl={(p) => current.assetUrl(p)} />
    <SlidesStrip toUrl={(p) => current.assetUrl(p)} onChoose={() => picker.show(session.capture.state === "asking" ? session.capture.detail : null)} />
  </main>
  <CommandLine />
</div>
<KeyDialog bind:this={keyDialog} />
<SpendView bind:this={spendView} />
<WindowPicker bind:this={picker} toUrl={(p) => current.assetUrl(p)} />
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
    grid-template-columns: minmax(20rem, 34fr) minmax(28rem, 56fr) 18rem;
    min-height: 0;
  }

  .panes > :global(*) {
    min-height: 0;
    border-right: 1px solid var(--rule);
  }

  .panes > :global(*:last-child) {
    border-right: 0;
  }

  @media (max-width: 1280px) {
    .panes {
      grid-template-columns: minmax(18rem, 36fr) minmax(24rem, 64fr) 16rem;
    }
    /* Large type (×1.25) makes those least widths 1,305 px, past the person's 1,168 px display: narrower ones keep
       all three columns on screen (48 rem, 1,080 px). */
    :global(:root.large) .panes {
      grid-template-columns: minmax(14rem, 36fr) minmax(20rem, 64fr) 14rem;
    }
  }

  /* Below 1100 px the strip folds away. */
  @media (max-width: 1100px) {
    .panes {
      grid-template-columns: minmax(18rem, 38fr) minmax(24rem, 62fr);
    }
    .panes > :global(.strip) {
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
