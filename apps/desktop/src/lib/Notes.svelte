<script lang="ts">
  // The notes (spec §9.1, §9.3): the committed document rendered once per chunk and frozen, each
  // snapshot's time in the gutter; the preview below it on the teal rule, finished blocks fading in.
  import { chunks, previewBlocks, render, type SlideCtx } from "./markdown";
  import { session } from "./session.svelte";

  let { toUrl }: { toUrl: (abs: string) => string } = $props();

  const ctx: SlideCtx = $derived({ notesDir: session.folder?.notes_dir ?? "", slides: new Set(session.slides.map((s) => s.path)), toUrl });

  // Rendered HTML by chunk: a chunk is parsed once, however often the list around it changes.
  const cache = new Map<string, string>();
  function html(key: string, md: string): string {
    let h = cache.get(key);
    if (h === undefined) {
      h = render(md, ctx);
      cache.set(key, h);
    }
    return h;
  }

  type Part = { key: string; time: string | null; html: string };

  const committed: Part[] = $derived.by(() => {
    // The document as last read, then each block committed since; keys change when the document is read again.
    const base = `${session.revision - session.committed.length}:${session.document.length}:${ctx.slides.size}`;
    const parts = chunks(session.document).map((c, i) => ({ key: `${base}:d${i}`, time: c.time, md: c.md }));
    session.committed.forEach((block, b) => chunks(block).forEach((c, i) => parts.push({ key: `${base}:b${b}:${i}`, time: c.time, md: c.md })));
    return parts.filter((p) => p.md.trim() !== "").map((p) => ({ key: p.key, time: p.time, html: html(p.key, p.md) }));
  });

  // Follows the newest writing while the reader is at the end, as the transcript does.
  let pinned = true;
  function onscroll(e: Event) {
    const el = e.currentTarget as HTMLElement;
    pinned = el.scrollHeight - el.scrollTop - el.clientHeight < 4;
  }
  function follow(node: HTMLElement) {
    return { destroy: session.onFrame(() => pinned && (node.scrollTop = node.scrollHeight)) };
  }

  const preview: Part[] = $derived.by(() => {
    const blocks = previewBlocks(session.previewShown);
    const op = session.preview?.op ?? 0;
    // Finished blocks are cached; only the last, still growing, is rendered again.
    return blocks.map((md, i) => (i < blocks.length - 1 ? { key: `p${op}:${i}`, time: null, html: html(`p${op}:${i}:${ctx.slides.size}`, md) } : { key: `p${op}:last`, time: null, html: render(md, ctx) }));
  });
</script>

<section class="pane" aria-label="Notes">
  <div class="scroll" use:follow {onscroll}>
    {#if committed.length === 0 && preview.length === 0}
      <p class="empty">Notes appear here after the first snapshot.</p>
    {/if}
    {#each committed as p (p.key)}
      <div class="chunk">
        {#if p.time}<span class="at num">{p.time}</span>{/if}
        <div class="md">{@html p.html}</div>
      </div>
    {/each}
    {#if preview.length}
      <div class="chunk preview" aria-live="off">
        <span class="at writing">writing</span>
        {#each preview as p, i (p.key)}
          <div class="md" class:done={i < preview.length - 1}>{@html p.html}</div>
        {/each}
      </div>
    {/if}
  </div>
</section>

<style>
  .pane {
    min-height: 0;
    display: flex;
  }

  .scroll {
    flex: 1;
    overflow-y: auto;
    padding: 1.25rem 1.5rem 3rem 0;
  }

  .empty {
    margin: 2rem 0 0 4.9rem;
    color: var(--graphite);
  }

  .chunk {
    position: relative;
    padding-left: 4.9rem;
    max-width: calc(4.9rem + var(--measure));
    margin-bottom: 1.4rem;
  }

  .at {
    position: absolute;
    left: 0;
    top: 0.45rem;
    width: 4.2rem;
    text-align: right;
    font-size: var(--step--1);
    color: var(--graphite);
  }

  .writing {
    color: var(--teal);
  }

  .preview {
    border-left: 3px solid var(--teal);
    margin-left: 4.9rem;
    padding-left: 0.75rem;
  }

  .preview .at {
    left: calc(-4.9rem - 3px);
  }

  .done {
    animation: arrive var(--fade) ease-out;
  }

  /* The rendered Markdown, on the app's 18 px / 1.2 scale. */
  .md :global(:first-child) {
    margin-top: 0;
  }

  .md :global(h1) {
    font-size: var(--step-2);
    line-height: 1.25;
    margin: 0 0 0.8rem;
  }

  .md :global(h2) {
    font-size: var(--step-1);
    line-height: 1.3;
    margin: 1.2em 0 0.4em;
  }

  .md :global(h3) {
    font-size: var(--step-0);
    margin: 1em 0 0.3em;
  }

  .md :global(p) {
    margin: 0 0 0.7em;
  }

  .md :global(ul),
  .md :global(ol) {
    margin: 0 0 0.7em;
    padding-left: 1.2em;
  }

  .md :global(li::marker) {
    color: var(--graphite);
  }

  .md :global(img) {
    display: block;
    max-width: 100%;
    height: auto;
    margin: 0.6rem 0;
    border: 1px solid var(--rule);
  }

  .md :global(table) {
    border-collapse: collapse;
    font-size: var(--step--1);
    margin: 0 0 0.9em;
  }

  .md :global(th),
  .md :global(td) {
    text-align: left;
    padding: 0.3rem 0.8rem 0.3rem 0;
    border-bottom: 1px solid var(--rule);
  }

  .md :global(code) {
    font-family: ui-monospace, "SF Mono", Menlo, monospace;
    font-size: 0.9em;
  }

  .md :global(pre) {
    background: var(--plate);
    padding: 0.7rem 0.9rem;
    overflow-x: auto;
    font-size: var(--step--1);
  }

  .md :global(blockquote) {
    margin: 0 0 0.7em;
    padding-left: 0.9rem;
    border-left: 2px solid var(--rule);
    color: var(--graphite);
  }
</style>
