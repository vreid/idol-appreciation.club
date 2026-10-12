<script lang="ts">
  import type { AppStore } from "../state.svelte";

  let { store }: { store: AppStore } = $props();
</script>

{#if store.placeMode !== "idle"}
  <aside class="place-panel">
    <div class="panel-head">
      <h3>Place an image</h3>
      <button
        class="btn ghost small"
        type="button"
        onclick={() => store.cancelPlace()}>✕</button
      >
    </div>

    {#if store.prepared && store.placeMode === "panel"}
      <img
        class="place-preview"
        src={store.prepared.previewUrl}
        alt="preview of {store.prepared.fileName}"
      />
      <label class="place-slider">
        <span>
          {@const size = store.ghostSize()}
          Long edge
          <b>{store.longEdge}px</b>
          →
          {size.w}×{size.h}px
        </span>
        <input
          type="range"
          min="16"
          max="2048"
          step="1"
          value={store.longEdge}
          oninput={(e) => store.setLongEdge(e.currentTarget.valueAsNumber)}
        />
      </label>
      <button
        class="btn primary wide"
        type="button"
        onclick={() => store.enterChoose()}
      >
        🎯 Choose spot
      </button>
    {:else if store.prepared}
      <img
        class="place-preview choose"
        src={store.prepared.previewUrl}
        alt="preview of {store.prepared.fileName}"
      />
      <p class="place-hint">
        Tap or click the map — the ghost is centered where you point.
      </p>
      <div class="panel-row">
        <button
          class="btn ghost wide"
          type="button"
          onclick={() => store.backToPanel()}
        >
          ← Adjust size
        </button>
        {#if store.submittingPlace}
          <button class="btn primary wide" type="button" disabled>
            Placing…
          </button>
        {/if}
      </div>
    {/if}
  </aside>
{/if}
