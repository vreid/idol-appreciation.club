<script lang="ts">
  import type { AppStore } from "../state.svelte";

  let { store }: { store: AppStore } = $props();
</script>

<div class="toasts" role="status" aria-live="polite">
  {#each store.toasts as toast (toast.id)}
    <button
      class="toast {toast.kind}"
      type="button"
      onclick={() => store.dismissToast(toast.id)}
    >
      {toast.message}
    </button>
  {/each}
</div>
