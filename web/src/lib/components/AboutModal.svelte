<script lang="ts">
  let { onClose }: { onClose: () => void } = $props();
</script>

<div class="modal-backdrop" onclick={onClose} role="presentation">
  <div
    class="modal about"
    role="dialog"
    aria-modal="true"
    aria-label="About idol-appreciation.club"
    onclick={(e) => e.stopPropagation()}
  >
    <div class="modal-head">
      <h2>About</h2>
      <button class="btn ghost small" type="button" onclick={onClose}>
        ✕
      </button>
    </div>
    <p>
      idol-appreciation.club is a collaborative, infinite canvas of idol images.
      Anyone can place pictures, overwriting each other on a cooldown — the game
      is to hold the world center <b>0/0</b>.
    </p>
    <p class="tos">
      You may only upload images you have the right to use. Reports are reviewed
      by admins; abusive content gets removed.
    </p>
    <p>
      <a
        class="btn ghost"
        href="https://github.com/vreid/idol-appreciation.club"
        target="_blank"
        rel="noreferrer">GitHub ↗</a
      >
    </p>
  </div>
</div>
