<script lang="ts">
  // The window and region picker (spec §7.1): which window shows the slides, and the part of it that is
  // the slide. Saved per course; when the saved window no longer matches, the strip opens this again
  // with the reason, and the saved region drawn on the new window's still.
  import { inRegion, inWindow } from "./parts";
  import { session } from "./session.svelte";
  import type { PreviewShot, Region, WindowView } from "./wire";

  let { toUrl }: { toUrl: (abs: string) => string } = $props();

  const WHOLE: Region = { x: 0, y: 0, w: 1, h: 1 };

  let dialog: HTMLDialogElement;
  let windows = $state.raw<WindowView[]>([]);
  let listError = $state<string | null>(null);
  let chosen = $state<number | null>(null);
  let shot = $state.raw<PreviewShot | null>(null);
  let shotError = $state<string | null>(null);
  let loading = $state(false);
  let region = $state.raw<Region>(WHOLE);
  /** Parts of the region that are not the slide (a speaker's camera), in fractions of the window while drawn. */
  let parts = $state.raw<Region[]>([]);
  /** The next drag draws a part to leave out rather than the region. */
  let leaving = $state(false);
  let drawing = $state.raw<Region | null>(null);
  let reason = $state<string | null>(null);
  let saving = $state(false);

  const px = (f: number, n: number) => Math.round(f * n);
  const zoom = (w: WindowView) => /zoom/i.test(w.app);

  /** Opens the picker; `why` is the strip's reason when the saved window no longer matches. */
  export async function show(why: string | null = null) {
    reason = why ? why[0].toUpperCase() + why.slice(1) : null;
    shot = null;
    shotError = null;
    dialog.showModal();
    leaving = false;
    const saved = await session.captureSavedRegion();
    region = saved?.region ?? WHOLE;
    parts = saved ? saved.leave_out.map((p) => inWindow(p, saved.region)) : [];
    await list();
    const asked = session.capture.candidates[0]?.id;
    const first = windows.find((w) => w.id === asked) ?? windows.find((w) => zoom(w) && w.on_screen) ?? windows.find((w) => w.on_screen);
    if (first) await choose(first.id);
  }

  async function list() {
    const got = await session.captureWindows();
    listError = got ? null : session.error;
    // Zoom's windows first, then by app; those on screen before those that are not.
    windows = (got ?? []).slice().sort((a, b) => Number(zoom(b)) - Number(zoom(a)) || Number(b.on_screen) - Number(a.on_screen) || a.app.localeCompare(b.app));
  }

  async function choose(id: number) {
    chosen = id;
    loading = true;
    shot = null;
    const got = await session.capturePreview(id);
    if (chosen !== id) return; // another row was chosen meanwhile
    loading = false;
    shot = got ?? null;
    shotError = got ? null : session.error;
  }

  async function save() {
    if (chosen === null) return;
    saving = true;
    const kept = parts.map((p) => inRegion(p, region)).filter((p): p is Region => p !== null);
    const ok = await session.captureSelect(chosen, region, kept);
    saving = false;
    if (ok) dialog.close();
  }

  function watchAgain(i: number) {
    parts = parts.filter((_, j) => j !== i);
  }

  /** Dragging over the still draws the region, in fractions of the window; a new drag redraws it. While
   *  leaving a part out, one drag draws one part, cut to the region. */
  function draw(node: HTMLElement) {
    let start: { x: number; y: number } | null = null;
    const at = (e: PointerEvent) => {
      const b = node.getBoundingClientRect();
      const clamp = (v: number) => Math.min(1, Math.max(0, v));
      return { x: clamp((e.clientX - b.left) / b.width), y: clamp((e.clientY - b.top) / b.height) };
    };
    const down = (e: PointerEvent) => {
      if (e.button !== 0) return;
      start = at(e);
      node.setPointerCapture(e.pointerId);
      e.preventDefault();
    };
    const move = (e: PointerEvent) => {
      if (!start) return;
      const p = at(e);
      const r = { x: Math.min(start.x, p.x), y: Math.min(start.y, p.y), w: Math.abs(p.x - start.x), h: Math.abs(p.y - start.y) };
      if (r.w > 0.02 && r.h > 0.02) {
        if (leaving) drawing = r;
        else region = r; // a click keeps the region drawn before
      }
    };
    const up = () => {
      start = null;
      if (!leaving || !drawing) return;
      const cut = inRegion(drawing, region);
      if (cut) parts = [...parts, inWindow(cut, region)];
      drawing = null;
      leaving = false;
    };
    node.addEventListener("pointerdown", down);
    node.addEventListener("pointermove", move);
    node.addEventListener("pointerup", up);
    node.addEventListener("pointercancel", up);
    return {
      destroy() {
        node.removeEventListener("pointerdown", down);
        node.removeEventListener("pointermove", move);
        node.removeEventListener("pointerup", up);
        node.removeEventListener("pointercancel", up);
      },
    };
  }
</script>

<dialog bind:this={dialog} aria-labelledby="picker-title">
  <h2 id="picker-title">Choose the window with the slides</h2>
  {#if reason}<p class="reason">{reason}.</p>{/if}
  <div class="body">
    <div class="windows">
      {#if listError}
        <p class="error" role="alert">{listError}</p>
        <button class="outline" onclick={() => session.openScreenSettings()}>Open Settings</button>
      {:else if windows.length === 0}
        <p class="hint">No windows to choose from. Open Zoom's meeting, then list the windows again.</p>
      {/if}
      {#if windows.length}
        <ul>
          {#each windows as w (w.id)}
            <li>
              <button class="row" class:chosen={chosen === w.id} aria-pressed={chosen === w.id} onclick={() => choose(w.id)}>
                <span class="app">{w.app}</span>
                <span class="title">{w.title || "Untitled window"}</span>
                <span class="size num">{w.width} × {w.height}{#if !w.on_screen}<em>, not on screen</em>{/if}</span>
              </button>
            </li>
          {/each}
        </ul>
      {/if}
      <button class="quiet" onclick={list}>List the windows again</button>
    </div>
    <div class="preview">
      {#if shot}
        <!-- The frame is exactly the still's shape, so fractions of it are fractions of the window. -->
        <div class="frame" use:draw style:aspect-ratio="{shot.width} / {shot.height}" style:max-width="calc((100vh - 20rem) * {shot.width / shot.height})">
          <img src={toUrl(shot.path)} alt="The chosen window" draggable="false" />
          <div class="region" style:left="{region.x * 100}%" style:top="{region.y * 100}%" style:width="{region.w * 100}%" style:height="{region.h * 100}%"></div>
          {#each parts as p, i (i)}
            <!-- Dimmed like outside the region: not watched. -->
            <div class="part" style:left="{p.x * 100}%" style:top="{p.y * 100}%" style:width="{p.w * 100}%" style:height="{p.h * 100}%">
              <button class="again" onpointerdown={(e) => e.stopPropagation()} onclick={() => watchAgain(i)} aria-label="Watch this part again" title="Watch this part again">×</button>
            </div>
          {/each}
          {#if drawing}<div class="part" style:left="{drawing.x * 100}%" style:top="{drawing.y * 100}%" style:width="{drawing.w * 100}%" style:height="{drawing.h * 100}%"></div>{/if}
        </div>
        <p class="hint">{leaving ? "Drag over what covers the slide, such as the speaker's camera." : "Drag over the slide, without Zoom's controls and the video tiles."}</p>
        <p class="hint num readout">
          <span>Region {px(region.w, shot.width)} × {px(region.h, shot.height)} of {shot.width} × {shot.height}{#if parts.length}, {parts.length} {parts.length === 1 ? "part" : "parts"} left out{/if}</span>
          <button class="quiet" aria-pressed={leaving} onclick={() => (leaving = !leaving)}>Leave out a part</button>
          <button class="quiet" onclick={() => (region = WHOLE)}>Use the whole window</button>
        </p>
      {:else if loading}
        <p class="hint">Capturing the window…</p>
      {:else if shotError}
        <p class="error" role="alert">{shotError}</p>
        <p class="hint">A minimised window cannot be captured: bring it back, then choose it again.</p>
      {:else if !listError}
        <p class="hint">Choose a window to see it here.</p>
      {/if}
    </div>
  </div>
  <div class="actions">
    <button class="outline" onclick={() => dialog.close()}>Cancel</button>
    <button class="primary" onclick={save} disabled={!shot || saving}>Watch this region</button>
  </div>
</dialog>

<style>
  dialog {
    width: min(64rem, calc(100vw - 2rem));
    max-height: calc(100vh - 2rem);
    padding: 1.4rem 1.6rem;
    border: 1px solid var(--rule);
    border-radius: var(--radius);
    background: var(--paper);
    color: var(--ink);
  }

  dialog::backdrop {
    background: var(--scrim);
  }

  h2 {
    margin: 0 0 0.4rem;
    font-size: var(--step-1);
  }

  .reason {
    margin: 0 0 0.8rem;
  }

  .body {
    display: grid;
    grid-template-columns: 16rem minmax(0, 1fr);
    gap: 1.5rem;
    margin-top: 0.8rem;
    font-size: var(--step--1);
  }

  .windows {
    min-height: 0;
    max-height: calc(100vh - 16rem);
    overflow-y: auto;
  }

  ul {
    margin: 0 0 0.6rem;
    padding: 0;
    list-style: none;
    border-top: 1px solid var(--rule);
  }

  li {
    border-bottom: 1px solid var(--rule);
  }

  .row {
    display: block;
    width: 100%;
    padding: 0.5rem 0.6rem;
    border: 0;
    border-left: 3px solid transparent;
    background: transparent;
    color: var(--ink);
    font: inherit;
    text-align: left;
    cursor: pointer;
  }

  .row.chosen {
    border-left-color: var(--teal);
  }

  .row:hover {
    background: var(--plate);
  }

  .row span {
    display: block;
  }

  .app {
    font-size: var(--step-0);
  }

  .title,
  .size {
    color: var(--graphite);
    overflow-wrap: anywhere;
  }

  .preview {
    min-width: 0;
  }

  .frame {
    position: relative;
    width: 100%;
    overflow: hidden;
    border: 1px solid var(--rule);
    cursor: crosshair;
    user-select: none;
    touch-action: none;
  }

  .frame img {
    display: block;
    width: 100%;
    height: 100%;
  }

  /* The one teal outline: what will be watched. Outside it is dimmed. */
  .region {
    position: absolute;
    outline: 2px solid var(--teal);
    box-shadow: 0 0 0 100vmax var(--scrim);
    pointer-events: none;
  }

  /* A part left out is a hole in what is watched, so it is dimmed as outside the region is. */
  .part {
    position: absolute;
    background: var(--scrim);
    pointer-events: none;
  }

  .again {
    position: absolute;
    top: 0.25rem;
    right: 0.25rem;
    padding: 0 0.4rem;
    border: 1px solid var(--rule);
    background: var(--paper);
    color: var(--ink);
    line-height: 1.4;
    pointer-events: auto;
  }

  .hint {
    margin: 0.5rem 0 0;
    color: var(--graphite);
  }

  .error {
    margin: 0 0 0.6rem;
    color: var(--signal);
  }

  .actions {
    display: flex;
    justify-content: flex-end;
    gap: 0.6rem;
    margin-top: 1.2rem;
  }

  button {
    font: inherit;
    border-radius: var(--radius);
    cursor: pointer;
  }

  .primary,
  .outline {
    padding: 0.45rem 0.95rem;
    border: 1.5px solid var(--teal);
  }

  .primary {
    background: var(--teal);
    color: var(--paper);
    font-weight: 700;
  }

  .primary:disabled {
    opacity: 0.45;
    cursor: default;
  }

  .outline {
    background: transparent;
    color: var(--teal);
  }

  .quiet {
    padding: 0.2rem 0;
    border: 0;
    background: transparent;
    color: var(--graphite);
    text-decoration: underline;
    text-underline-offset: 0.2em;
  }

  .quiet:hover {
    color: var(--ink);
  }

  .quiet[aria-pressed="true"] {
    color: var(--teal);
  }

  .readout {
    display: flex;
    flex-wrap: wrap;
    align-items: baseline;
    gap: 0.2rem 0.8rem;
  }
</style>
