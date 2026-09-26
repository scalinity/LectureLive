<script lang="ts">
  // The spend view (spec §9.1): the CLI's `lecture spend` as a sheet beside the lecture.
  import { session } from "./session.svelte";
  import type { SpendSummary } from "./wire";

  let dialog: HTMLDialogElement;
  let s = $state<SpendSummary | undefined>(undefined);

  export async function show() {
    s = await session.spendSummary();
    dialog.showModal();
  }

  /** The CLI's money: two decimals, thousands grouped, "<$0.01" for a sliver. */
  function money(usd: number): string {
    if (usd > 0 && usd < 0.005) return "<$0.01";
    return "$" + usd.toLocaleString("en-US", { minimumFractionDigits: 2, maximumFractionDigits: 2 });
  }

  const share = $derived(s && s.total > 0 ? Math.round((100 * s.estimated) / s.total) : 0);
</script>

<dialog bind:this={dialog} class="sheet" aria-labelledby="spend-title">
  <div class="head">
    <h2 id="spend-title">Spend</h2>
    <button class="close" onclick={() => dialog.close()}>Close</button>
  </div>
  {#if !s || s.calls === 0}
    <p class="quiet">Nothing spent yet. Every paid request is logged from the next lecture on.</p>
  {:else}
    <p class="row all"><span>All time</span><span class="num">{money(s.total)}</span></p>

    {#each s.months as m (m.key)}
      <section class="month">
        <h3 class="row"><span>{m.label}</span><span class="num">{money(m.total)}</span></h3>
        {#each m.courses as [course, usd] (course)}
          {@const top = Math.max(...m.courses.map(([, u]) => u))}
          <p class="row course-row">
            <span class="name">{course}</span>
            <span class="bar"><span style:width="{top > 0 ? (usd / top) * 100 : 0}%"></span></span>
            <span class="num">{money(usd)}</span>
          </p>
        {/each}
      </section>
    {/each}

    <h3 class="recent-title">Recent lectures</h3>
    {#each s.recent as r (r.day + r.course + r.lecture)}
      <div class="lecture">
        <span class="at num">{r.label}</span>
        <p class="row"><span class="name"><span class="course">{r.course} ›</span> {r.lecture}</span><span class="num">{money(r.total)}</span></p>
        <p class="kinds num">{r.kinds.map(([k, v]) => `${k} ${money(v)}`).join("   ")}</p>
      </div>
    {/each}

    <p class="quiet foot">
      {s.calls} paid requests; {s.estimated === 0 ? "all billed by xAI" : `${share}% estimated from published rates, the rest billed by xAI`}.
    </p>
  {/if}
</dialog>

<style>
  .sheet {
    margin: 0 0 0 auto;
    height: 100vh;
    max-height: 100vh;
    width: min(34rem, 100vw);
    padding: 1.4rem 1.6rem 2rem;
    border: 0;
    border-left: 1px solid var(--rule);
    background: var(--paper);
    color: var(--ink);
  }

  .sheet::backdrop {
    background: var(--scrim);
  }

  .head {
    display: flex;
    justify-content: space-between;
    align-items: baseline;
  }

  h2 {
    margin: 0 0 1rem;
    font-size: var(--step-2);
  }

  .close {
    font: inherit;
    font-size: var(--step--1);
    padding: 0.35rem 0.8rem;
    border: 1.5px solid var(--teal);
    border-radius: var(--radius);
    background: transparent;
    color: var(--teal);
    cursor: pointer;
  }

  .row {
    display: flex;
    align-items: center;
    gap: 0.8rem;
    margin: 0;
  }

  .row > .num {
    margin-left: auto;
    text-align: right;
  }

  .all {
    color: var(--graphite);
    padding-bottom: 0.8rem;
    border-bottom: 1px solid var(--rule);
  }

  .month {
    margin: 1.2rem 0 0;
  }

  h3 {
    font-size: var(--step-0);
    margin: 0 0 0.35rem;
  }

  .course-row {
    font-size: var(--step--1);
    padding: 0.15rem 0;
  }

  .course-row .name {
    width: 11rem;
    flex-shrink: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .bar {
    flex: 1;
    max-width: 10rem;
  }

  .bar span {
    display: block;
    height: 0.55rem;
    min-width: 2px;
    background: var(--teal);
  }

  .recent-title {
    margin-top: 1.6rem;
    padding-top: 1rem;
    border-top: 1px solid var(--rule);
  }

  .lecture {
    position: relative;
    padding-left: 4.2rem;
    margin-bottom: 0.7rem;
  }

  .lecture .at {
    position: absolute;
    left: 0;
    top: 0.1rem;
    font-size: var(--step--1);
    color: var(--graphite);
  }

  .lecture .name {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .lecture .course {
    color: var(--graphite);
  }

  .kinds {
    margin: 0.1rem 0 0;
    font-size: var(--step--1);
    color: var(--graphite);
    white-space: pre;
  }

  .quiet {
    color: var(--graphite);
  }

  .foot {
    margin-top: 1.4rem;
    font-size: var(--step--1);
  }
</style>
