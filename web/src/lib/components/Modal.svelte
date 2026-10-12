<script lang="ts">
  import type { Snippet } from "svelte";

  let {
    title,
    onClose,
    children,
  }: { title: string; onClose: () => void; children: Snippet } = $props();

  let dialogEl = $state<HTMLDivElement>();

  $effect(() => {
    dialogEl?.focus();
  });

  function onKeydown(event: KeyboardEvent): void {
    if (event.key === "Escape") onClose();
  }

  // Swallow dialog key events except Escape, which must reach the window.
  function onDialogKeydown(event: KeyboardEvent): void {
    if (event.key !== "Escape") event.stopPropagation();
  }
</script>

<svelte:window onkeydown={onKeydown} />

<div class="modal-backdrop" onclick={onClose} role="presentation">
  <div
    class="modal"
    role="dialog"
    aria-modal="true"
    aria-label={title}
    tabindex="-1"
    bind:this={dialogEl}
    onclick={(e) => e.stopPropagation()}
    onkeydown={onDialogKeydown}
  >
    <div class="modal-head">
      <h2>{title}</h2>
      <button class="btn ghost small" type="button" onclick={onClose}>
        ✕
      </button>
    </div>
    {@render children()}
  </div>
</div>
