<script lang="ts">
  // A synthetic lecture deck (M5): realistic input for the slide detector. In a browser tab it waits on
  // its title slide, then plays deck.json's schedule from Start; the recording is shared into Zoom from
  // it. Inside the app it shows whatever the check asks for through window.__deck.show(i).
  import { isTauri } from "@tauri-apps/api/core";
  import { onDestroy } from "svelte";
  import { fade } from "svelte/transition";
  import deck from "$lib/deck.json";

  type Step = { id: string; slide: number; build: number; ms: number; kind: string; transition: string };
  const steps = deck.states as Step[];

  let at = $state(-1); // -1: the title slide, waiting for Start
  let phase = $state(0); // animations move once a second
  let innerWidth = $state(1600);
  let innerHeight = $state(900);
  const scale = $derived(Math.min(innerWidth / 1600, innerHeight / 900));
  const step = $derived(at >= 0 ? steps[at] : null);
  const slide = $derived(step?.slide ?? 0);
  const build = $derived(step?.build ?? 0);
  const dissolve = $derived(step?.transition === "dissolve" ? 600 : 0);

  const tick = setInterval(() => phase++, 1000);
  const timers: ReturnType<typeof setTimeout>[] = [];
  onDestroy(() => {
    clearInterval(tick);
    timers.forEach(clearTimeout);
  });

  /** Plays the schedule from now, each state at its own offset so nothing drifts. */
  function start() {
    let offset = 0;
    steps.forEach((s, i) => {
      timers.push(setTimeout(() => (at = i), offset));
      offset += s.ms;
    });
  }

  (globalThis as { __deck?: unknown }).__deck = { show: (i: number) => (at = i), start, count: steps.length };
  // Inside the app, a check drives it from the title slide (show, or start to play the schedule).

  /** Bar heights for the animated charts: they cycle every 15 s and never rest. */
  const bars = (seed: number) => Array.from({ length: 8 }, (_, j) => 60 + ((phase % 15) * 37 + j * 53 + seed * 29) % 220);
</script>

<svelte:window bind:innerWidth bind:innerHeight />

<svelte:head><title>Optimisation — Lecture 6</title></svelte:head>

<div class="screen">
  <div class="stage" style:transform="translate(-50%, -50%) scale({scale})">
    {#key slide}
      <section class="slide" in:fade={{ duration: dissolve }} out:fade={{ duration: dissolve }}>
        {#if slide === 0}
          <div class="cover">
            <h1>Optimisation for Machine Learning</h1>
            <p class="sub">Lecture 6: Gradient methods</p>
            <p class="who">Week 6</p>
            {#if !isTauri()}<button class="start" onclick={start}>Start</button>{/if}
          </div>
        {:else}
          <h2>
            {["", "Where we are", "The gradient", "Gradient descent", "A step downhill", "Choosing the step size", "Loss during training", "Stochastic gradient descent", "Mini-batches", "Momentum", "Zig-zags in a narrow valley", "Writing the training loop", "Nesterov momentum", "Adaptive learning rates", "Adam", "Comparing the methods", "Validation loss", "Learning-rate schedules", "Warm-up", "A practical checklist", "Exam question", "Summary", "Next week", "End of lecture"][slide]}
          </h2>
          <div class="body">
            {#if slide === 1}
              <ul>
                <li>The loss is a function of the weights</li>
                {#if build >= 1}<li>Training minimises it by following the slope</li>{/if}
                {#if build >= 2}<li>Today: step sizes, momentum and adaptive methods</li>{/if}
              </ul>
            {:else if slide === 2}
              <p>The gradient collects the partial derivatives of the loss:</p>
              {#if build >= 1}<div class="eq">∇L(w) = ( ∂L/∂w₁ , ∂L/∂w₂ , … , ∂L/∂wₙ )</div>{/if}
            {:else if slide === 3}
              <ul>
                <li>Update rule: w ← w − η ∇L(w)</li>
                <li>η is the <span class="mark" class:on={build >= 1}>learning rate</span></li>
                <li>The only number we choose; the data gives the gradient</li>
              </ul>
            {:else if slide === 4 || slide === 10 || slide === 17}
              <svg class="plot" viewBox="0 0 1300 600" aria-hidden="true">
                <line x1="80" y1="540" x2="1240" y2="540" class="axis" />
                <line x1="80" y1="540" x2="80" y2="40" class="axis" />
                {#if slide === 17}
                  <polyline class="curve" points="100,120 400,120 400,300 750,300 750,440 1200,440" />
                  {#if build >= 1}<path class="arrow" d="M 420 150 L 720 280" marker-end="url(#head)" />{/if}
                {:else}
                  <path class="curve" d="M 120 80 Q 660 {slide === 10 ? 980 : 900} 1200 80" />
                  <circle cx="300" cy={slide === 10 ? 330 : 300} r="18" class="ball" />
                  {#if build >= 1}<path class="arrow" d={slide === 10 ? "M 300 330 L 520 250" : "M 300 300 L 500 430"} marker-end="url(#head)" />{/if}
                  {#if build >= 2}<path class="arrow" d="M 500 430 L 640 485" marker-end="url(#head)" />{/if}
                {/if}
                <defs><marker id="head" viewBox="0 0 10 10" refX="8" refY="5" markerWidth="6" markerHeight="6" orient="auto"><path d="M 0 0 L 10 5 L 0 10 z" class="head" /></marker></defs>
              </svg>
            {:else if slide === 5 || slide === 15}
              <table>
                <thead><tr><th>{slide === 5 ? "Learning rate" : "Method"}</th><th>{slide === 5 ? "What happens" : "Memory"}</th><th>{slide === 5 ? "Epochs to converge" : "Tuning"}</th></tr></thead>
                <tbody>
                  {#if build >= 1}<tr><td>{slide === 5 ? "0.001" : "SGD"}</td><td>{slide === 5 ? "Slow but steady" : "None"}</td><td>{slide === 5 ? "120" : "Step size"}</td></tr>{/if}
                  {#if build >= 2}<tr><td>{slide === 5 ? "0.01" : "Momentum"}</td><td>{slide === 5 ? "Fast and stable" : "One vector"}</td><td>{slide === 5 ? "18" : "Step size, β"}</td></tr>{/if}
                  {#if build >= 3}<tr><td>{slide === 5 ? "0.5" : "Adam"}</td><td>{slide === 5 ? "Overshoots and diverges" : "Two vectors"}</td><td>{slide === 5 ? "—" : "Mostly defaults"}</td></tr>{/if}
                </tbody>
              </table>
            {:else if slide === 6 || slide === 16}
              <div class="chart" aria-hidden="true">
                {#each bars(slide) as h, j (j)}<span style:height="{h}px"></span>{/each}
              </div>
              {#if build >= 1}<ul><li>Noisy, but falling on average</li></ul>{/if}
            {:else if slide === 7}
              <ul>
                <li>Use one mini-batch per step instead of the whole data set</li>
                {#if build >= 1}<li>Each step is cheaper</li>{/if}
                {#if build >= 2}<li>The noise can help escape shallow minima</li>{/if}
              </ul>
            {:else if slide === 8}
              <table>
                <thead><tr><th>Batch size</th><th>Steps per epoch</th><th>Noise</th></tr></thead>
                <tbody>
                  <tr><td>32</td><td>1 875</td><td>High</td></tr>
                  {#if build >= 1}<tr><td>512</td><td>118</td><td>Low</td></tr>{/if}
                </tbody>
              </table>
            {:else if slide === 9}
              <p>Keep a running average of past gradients:</p>
              {#if build >= 1}<div class="eq">v ← β v + ∇L(w) ,  w ← w − η v</div>{/if}
            {:else if slide === 11}
              <pre class="code">for epoch in range(epochs):
    for x, y in batches:
        loss = model.loss(x, y)
        w = w - lr * grad(loss, w){#if build >= 1}
    lr = schedule(epoch){/if}<span class="caret" class:off={phase % 2 === 1}></span></pre>
            {:else if slide === 12}
              <ul>
                <li>Look ahead before taking the gradient</li>
                {#if build >= 1}<li>Evaluate ∇L at w − η β v</li>{/if}
                {#if build >= 2}<li>Often converges faster than plain momentum</li>{/if}
              </ul>
            {:else if slide === 13}
              <ul>
                <li>Give each weight its own step size</li>
                <li>Scale by the <span class="mark" class:on={build >= 1}>size of recent gradients</span></li>
              </ul>
            {:else if slide === 14}
              <p>Two running averages, then a corrected step:</p>
              {#if build >= 1}<div class="eq">m ← β₁ m + (1 − β₁) g ,  s ← β₂ s + (1 − β₂) g²</div>{/if}
              {#if build >= 2}<div class="eq">w ← w − η m̂ / (√ŝ + ε)</div>{/if}
            {:else if slide === 18}
              <ul>
                <li>Start with a small learning rate</li>
                {#if build >= 1}<li>Raise it over the first epochs</li>{/if}
                {#if build >= 2}<li>Then decay it as training settles</li>{/if}
              </ul>
            {:else if slide === 19}
              <ul>
                <li>Plot the loss every epoch</li>
                {#if build >= 1}<li>Try three learning rates, a factor of ten apart</li>{/if}
                {#if build >= 2}<li>Add momentum before anything fancier</li>{/if}
                {#if build >= 3}<li>Keep a validation set you never train on</li>{/if}
              </ul>
            {:else if slide === 20}
              <div class="eq wide">Write the momentum update. What does β control, and what happens when β = 0?</div>
            {:else if slide === 21}
              <ul>
                <li>Gradient descent follows the slope, scaled by η</li>
                {#if build >= 1}<li>Momentum smooths the path through narrow valleys</li>{/if}
                {#if build >= 2}<li>Adaptive methods scale each weight's step</li>{/if}
              </ul>
            {:else if slide === 22}
              <p>Second-order methods and why they are rarely used at scale.</p>
            {:else}
              <p>Questions?</p>
            {/if}
          </div>
          <p class="foot">Optimisation for Machine Learning, Lecture 6 <span>{slide}</span></p>
        {/if}
      </section>
    {/key}
  </div>
</div>

<style>
  :global(body) {
    margin: 0;
    background: #222;
    overflow: hidden;
  }

  .screen {
    position: fixed;
    inset: 0;
    background: #222;
  }

  .stage {
    position: absolute;
    left: 50%;
    top: 50%;
    width: 1600px;
    height: 900px;
    transform-origin: center;
    font-family: "Helvetica Neue", Helvetica, Arial, sans-serif;
    color: #1a1a1a;
  }

  .slide {
    position: absolute;
    inset: 0;
    background: #fff;
    padding: 70px 110px;
    box-sizing: border-box;
  }

  h1 {
    margin: 250px 0 0;
    font-size: 72px;
    color: #1f4e79;
  }

  .sub {
    font-size: 40px;
    margin: 24px 0 0;
  }

  .who {
    font-size: 30px;
    color: #666;
  }

  .start {
    margin-top: 60px;
    font: inherit;
    font-size: 30px;
    padding: 14px 40px;
    border: 2px solid #1f4e79;
    border-radius: 8px;
    background: #fff;
    color: #1f4e79;
    cursor: pointer;
  }

  h2 {
    margin: 0;
    padding-bottom: 18px;
    font-size: 58px;
    color: #1f4e79;
    border-bottom: 4px solid #1f4e79;
  }

  .body {
    margin-top: 50px;
    font-size: 40px;
    line-height: 1.5;
  }

  ul {
    margin: 0;
    padding-left: 50px;
  }

  li {
    margin-bottom: 22px;
  }

  p {
    margin: 0 0 30px;
  }

  .mark.on {
    border-bottom: 6px solid #c0392b;
  }

  .eq {
    display: inline-block;
    margin: 20px 0 30px;
    padding: 24px 40px;
    border: 3px solid #1a1a1a;
    font-family: "Times New Roman", Times, serif;
    font-size: 46px;
  }

  .eq.wide {
    font-family: inherit;
    font-size: 42px;
    max-width: 1200px;
  }

  table {
    border-collapse: collapse;
    font-size: 38px;
  }

  th,
  td {
    padding: 14px 40px 14px 0;
    text-align: left;
    border-bottom: 2px solid #999;
  }

  th {
    color: #1f4e79;
  }

  .plot {
    width: 1300px;
    height: 600px;
  }

  .axis {
    stroke: #666;
    stroke-width: 4;
  }

  .curve {
    fill: none;
    stroke: #1f4e79;
    stroke-width: 8;
  }

  .ball {
    fill: #c0392b;
  }

  .arrow {
    fill: none;
    stroke: #c0392b;
    stroke-width: 8;
  }

  .head {
    fill: #c0392b;
  }

  .chart {
    display: flex;
    align-items: flex-end;
    gap: 40px;
    height: 320px;
    padding-left: 20px;
    border-left: 4px solid #666;
    border-bottom: 4px solid #666;
  }

  .chart + ul {
    margin-top: 36px;
  }

  .chart span {
    width: 80px;
    background: #1f4e79;
    transition: height 0.8s linear;
  }

  .code {
    margin: 0;
    font-family: Menlo, Monaco, monospace;
    font-size: 36px;
    line-height: 1.5;
  }

  .caret {
    display: inline-block;
    width: 4px;
    height: 42px;
    margin-left: 4px;
    vertical-align: -6px;
    background: #1a1a1a;
  }

  .caret.off {
    visibility: hidden;
  }

  .foot {
    position: absolute;
    left: 110px;
    right: 110px;
    bottom: 36px;
    margin: 0;
    font-size: 22px;
    color: #666;
  }

  .foot span {
    float: right;
  }
</style>
