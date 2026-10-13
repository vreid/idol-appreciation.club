<script lang="ts">
  import { onMount } from "svelte";
  import { createApiClient } from "./lib/api";
  import { createAppStore } from "./lib/state.svelte";
  import { burstConfetti } from "./lib/confetti";
  import { readNaturalSize } from "./lib/images";
  import { SseClient, eventTouchesCenter } from "./lib/sse";
  import { createWorldMap, type WorldMapController } from "./lib/worldMap";
  import { queryRadiusForZoom, type BBox } from "./lib/world";
  import TopBar from "./lib/components/TopBar.svelte";
  import PlacePanel from "./lib/components/PlacePanel.svelte";
  import Toasts from "./lib/components/Toasts.svelte";
  import AboutModal from "./lib/components/AboutModal.svelte";

  const store = createAppStore(createApiClient());

  let mapEl = $state<HTMLDivElement>();
  let fileInput = $state<HTMLInputElement>();
  let controller: WorldMapController | null = null;

  let showAbout = $state(false);

  const sse = new SseClient({
    onEvent: (event) => {
      const [x, y, w, h] = event.bbox;
      const bbox: BBox = { x, y, w, h };
      controller?.invalidateBBox(bbox);
      controller?.pulse(bbox);
      if (eventTouchesCenter(event.bbox)) void store.refreshCenter();
    },
  });

  onMount(() => {
    if (mapEl === undefined) return;
    controller = createWorldMap(mapEl, {
      onMapClick: (x, y) => void handleMapClick(x, y),
      isLoggedIn: () => store.me !== null,
      isAdmin: () => store.me?.is_admin === true,
      onReport: (id) => void store.reportPlacement(id),
      onRemove: (id, bbox) => void handleRemove(id, bbox),
      onAuthPrompt: () => store.promptAuth(),
    });
    void store.refreshMe();
    void store.refreshCenter();
    const poll = setInterval(() => void store.refreshCenter(), 30_000);
    sse.start();
    return () => {
      sse.stop();
      clearInterval(poll);
      controller?.destroy();
      controller = null;
    };
  });

  // Keep the ghost image in sync with the placing flow.
  $effect(() => {
    if (controller === null) return;
    const mode = store.placeMode;
    const prepared = store.prepared;
    if (mode === "choose" && prepared !== null) {
      const size = store.ghostSize();
      controller.setGhost({
        url: prepared.previewUrl,
        w: size.w,
        h: size.h,
      });
      controller.setPlacing(true);
    } else {
      controller.setGhost(null);
      controller.setPlacing(false);
    }
  });

  async function handleMapClick(x: number, y: number): Promise<void> {
    if (controller === null) return;
    if (store.placeMode === "choose") {
      const attempt = await store.confirmSpot(x, y);
      if (attempt.ok && controller !== null) {
        const p = attempt.placement;
        controller.pulse({ x: p.x, y: p.y, w: p.w, h: p.h });
        const at = controller.worldToScreenPoint(p.x + p.w / 2, p.y + p.h / 2);
        if (mapEl !== undefined) burstConfetti(mapEl, at.x, at.y);
      }
      return;
    }
    const zoom = controller.currentZoom();
    const near = await store.refreshNear(x, y, queryRadiusForZoom(zoom));
    const top = near[0];
    // Empty spots are a silent no-op — no toast spam on bare canvas.
    if (top === undefined) return;
    controller.showPlacement(top);
  }

  async function handleRemove(id: string, bbox: BBox): Promise<void> {
    if (controller === null) return;
    const removed = await store.removePlacement(id);
    if (removed) {
      // Optimistic: drop the pixels right away; SSE confirms later.
      controller.invalidateBBox(bbox);
      controller.closePopup();
    }
  }

  function pickImage(): void {
    if (store.me === null) {
      store.promptAuth();
      return;
    }
    fileInput?.click();
  }

  async function onFileChosen(event: Event): Promise<void> {
    const input = event.target;
    if (!(input instanceof HTMLInputElement)) return;
    const file = input.files?.[0];
    input.value = "";
    if (file === undefined) return;
    if (!file.type.startsWith("image/")) {
      store.notify("That file is not an image");
      return;
    }
    const size = await readNaturalSize(file);
    if (size === null) {
      store.notify("Could not read that image");
      return;
    }
    store.startPlace(file, file.name, size.w, size.h);
  }
</script>

<div class="map-root" bind:this={mapEl}></div>

<input
  class="hidden-input"
  type="file"
  accept="image/*"
  onchange={onFileChosen}
  bind:this={fileInput}
/>

<TopBar {store} onPickImage={pickImage} />

<div class="footer-chip">
  <button class="linkish" type="button" onclick={() => (showAbout = true)}>
    About
  </button>
  <span aria-hidden="true">·</span>
  <a
    href="https://github.com/vreid/idol-appreciation.club"
    target="_blank"
    rel="noreferrer">GitHub</a
  >
</div>

<PlacePanel {store} />
<Toasts {store} />

{#if showAbout}
  <AboutModal onClose={() => (showAbout = false)} />
{/if}
