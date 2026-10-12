<script lang="ts">
  import type { AppStore } from "../state.svelte";
  import { formatDuration } from "../format";

  let { store, onClose }: { store: AppStore; onClose: () => void } = $props();

  $effect(() => {
    void store.refreshCenter();
  });

  function rankClass(index: number): string {
    return index < 3 ? `rank-${index + 1}` : "";
  }
</script>

<div class="modal-backdrop" onclick={onClose} role="presentation">
  <div
    class="modal"
    role="dialog"
    aria-modal="true"
    aria-label="0/0 leaderboard"
    onclick={(e) => e.stopPropagation()}
  >
    <div class="modal-head">
      <h2>0/0 leaderboard</h2>
      <button class="btn ghost small" type="button" onclick={onClose}>
        ✕
      </button>
    </div>

    <div class="lb-current">
      {#if store.center?.current}
        <span class="lb-crown" aria-hidden="true">👑</span>
        <div>
          <div class="lb-holder">@{store.center.current.username}</div>
          <div class="lb-sub">
            holding for
            {formatDuration(
              Math.max(
                0,
                (Date.now() - Date.parse(store.center.current.since)) / 1000,
              ),
            )}
            · current placement {store.center.current.placement.w}×{store
              .center.current.placement.h}
            px
          </div>
        </div>
      {:else}
        <div class="lb-sub">Nobody holds 0/0 right now — be the first!</div>
      {/if}
    </div>

    <table class="lb-table">
      <thead>
        <tr>
          <th>#</th>
          <th>Holder</th>
          <th class="num">Total time held</th>
        </tr>
      </thead>
      <tbody>
        {#each store.center?.all_time ?? [] as entry, index (entry.username)}
          <tr>
            <td class={rankClass(index)}>{index + 1}</td>
            <td>@{entry.username}</td>
            <td class="num">{entry.seconds.toLocaleString()}s</td>
          </tr>
        {:else}
          <tr>
            <td colspan="3" class="lb-empty">No history yet.</td>
          </tr>
        {/each}
      </tbody>
    </table>
  </div>
</div>
