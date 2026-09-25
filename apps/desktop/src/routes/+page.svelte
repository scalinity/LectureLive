<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";

  let log = $state<string[]>([]);
  let devices = $state<string[]>([]);
  let device = $state("BlackHole 2ch");
  let secs = $state(60);
  let wins = $state<[number, string][]>([]);
  let busy = $state(false);

  async function run(label: string, f: () => Promise<unknown>) {
    busy = true;
    try {
      const r = await f();
      log = [`${label}: ${typeof r === "string" ? r : JSON.stringify(r)}`, ...log];
    } catch (e) {
      log = [`${label} FAILED: ${e}`, ...log];
    } finally {
      busy = false;
    }
  }
</script>

<main>
  <h1>LectureLive canary</h1>
  <section>
    <button disabled={busy} onclick={() => run("route status", () => invoke("route", { action: "status" }))}>Route status</button>
    <button disabled={busy} onclick={() => run("route on", () => invoke("route", { action: "on" }))}>Route on</button>
    <button disabled={busy} onclick={() => run("route off", () => invoke("route", { action: "off" }))}>Route off</button>
  </section>
  <section>
    <button disabled={busy} onclick={() => run("inputs", async () => (devices = await invoke<string[]>("inputs")))}>List inputs</button>
    <select bind:value={device}>
      {#each devices as d}<option>{d}</option>{/each}
    </select>
    <input type="number" bind:value={secs} min="5" />
    <button disabled={busy} onclick={() => run("record", () => invoke("record", { device, secs }))}>Record</button>
  </section>
  <section>
    <button disabled={busy} onclick={() => run("windows", async () => (wins = await invoke<[number, string][]>("windows")))}>List windows</button>
    {#each wins as [id, label]}
      <div><button disabled={busy} onclick={() => run(`capture ${id}`, () => invoke("capture", { id }))}>Capture</button> {label}</div>
    {/each}
  </section>
  <pre>{log.join("\n")}</pre>
</main>
