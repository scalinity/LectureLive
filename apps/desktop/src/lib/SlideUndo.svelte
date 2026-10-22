<script lang="ts">
  import { session } from "./session.svelte";

  function keydown(event: KeyboardEvent) {
    if (event.defaultPrevented || event.isComposing || event.repeat || !event.metaKey || event.ctrlKey || event.altKey || event.shiftKey || event.key.toLowerCase() !== "z") return;
    const target = event.target;
    if (target instanceof HTMLElement && (target.closest("input, textarea, select") || target.isContentEditable || target.closest('[contenteditable]:not([contenteditable="false"]), [role="textbox"]'))) return;
    if (!session.canUndoRemoval || document.querySelector("dialog[open]")) return;
    event.preventDefault();
    void session.undoRemoval();
  }
</script>

<svelte:window onkeydown={keydown} />
