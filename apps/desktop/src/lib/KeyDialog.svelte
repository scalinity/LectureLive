<script lang="ts">
  // The API key (spec §2): stored in the macOS Keychain by the backend. The field is cleared once
  // saved, and the key is never read back into the page.
  import { session } from "./session.svelte";

  let dialog: HTMLDialogElement;
  let key = $state("");
  let envAvailable = $state(false);
  let stored = $state(false);

  export async function show() {
    const s = await session.keyStatus();
    stored = s?.stored ?? false;
    envAvailable = s?.env_available ?? false;
    dialog.showModal();
  }

  async function save(e: SubmitEvent) {
    e.preventDefault();
    const ok = await session.saveKey(key);
    key = "";
    if (ok) dialog.close();
  }

  async function useEnv() {
    if (await session.importKey()) dialog.close();
  }
</script>

<dialog bind:this={dialog} aria-labelledby="key-title">
  <form onsubmit={save}>
    <h2 id="key-title">xAI API key</h2>
    <p>{stored ? "A key is stored in the Keychain. Paste a new one to replace it." : "LectureLive needs your xAI key to transcribe and write notes. It is kept in the macOS Keychain and never shown again."}</p>
    <input type="password" bind:value={key} autocomplete="off" placeholder="xai-…" aria-label="API key" />
    {#if session.error}<p class="error" role="alert">{session.error}</p>{/if}
    <div class="actions">
      {#if envAvailable}<button type="button" class="outline" onclick={useEnv}>Use the key in .env</button>{/if}
      <button type="button" class="outline" onclick={() => dialog.close()}>{stored ? "Close" : "Later"}</button>
      <button type="submit" class="primary" disabled={!key.trim()}>Save to Keychain</button>
    </div>
  </form>
</dialog>

<style>
  dialog {
    width: min(32rem, calc(100vw - 2rem));
    padding: 1.5rem 1.6rem;
    border: 1px solid var(--rule);
    border-radius: var(--radius);
    background: var(--paper);
    color: var(--ink);
  }

  dialog::backdrop {
    background: var(--scrim);
  }

  h2 {
    margin: 0 0 0.5rem;
    font-size: var(--step-1);
  }

  p {
    margin: 0 0 1rem;
    color: var(--graphite);
  }

  input {
    width: 100%;
    font: inherit;
    padding: 0.5rem 0.7rem;
    border: 1.5px solid var(--teal);
    border-radius: var(--radius);
    background: var(--paper);
    color: var(--ink);
  }

  .error {
    margin: 0.7rem 0 0;
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
    padding: 0.45rem 0.95rem;
    border-radius: var(--radius);
    border: 1.5px solid var(--teal);
    cursor: pointer;
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
</style>
