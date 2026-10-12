<script lang="ts">
  import Modal from "./Modal.svelte";

  let { onClose }: { onClose: () => void } = $props();
</script>

<Modal title="About" {onClose}>
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
</Modal>
