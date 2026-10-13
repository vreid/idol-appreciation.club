import { createEffect, onCleanup, onMount, createSignal, Show } from "solid-js";
import { createApiClient } from "./lib/api";
import { createAppStore } from "./lib/state";
import { burstConfetti } from "./lib/confetti";
import { readNaturalSize } from "./lib/images";
import { SseClient, eventTouchesCenter } from "./lib/sse";
import { createWorldMap, type WorldMapController } from "./lib/worldMap";
import { queryRadiusForZoom, type BBox } from "./lib/world";
import TopBar from "./lib/components/TopBar";
import PlacePanel from "./lib/components/PlacePanel";
import Toasts from "./lib/components/Toasts";
import AboutModal from "./lib/components/AboutModal";
import type { AppStore } from "./lib/state";

/** The imperative map bits the async handlers need access to. */
interface MapHandle {
  current: WorldMapController | null;
  el: HTMLDivElement | undefined;
}

async function onMapClick(
  map: MapHandle,
  store: AppStore,
  x: number,
  y: number,
): Promise<void> {
  const controller = map.current;
  if (controller === null) return;
  if (store.placeMode === "choose") {
    const attempt = await store.confirmSpot(x, y);
    if (attempt.ok && map.current !== null) {
      const p = attempt.placement;
      map.current.pulse({ x: p.x, y: p.y, w: p.w, h: p.h });
      const at = map.current.worldToScreenPoint(p.x + p.w / 2, p.y + p.h / 2);
      if (map.el !== undefined) burstConfetti(map.el, at.x, at.y);
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

async function onRemove(
  map: MapHandle,
  store: AppStore,
  id: string,
  bbox: BBox,
): Promise<void> {
  const controller = map.current;
  if (controller === null) return;
  const removed = await store.removePlacement(id);
  if (removed) {
    // Optimistic: drop the pixels right away; SSE confirms later.
    controller.invalidateBBox(bbox);
    controller.closePopup();
  }
}

async function onFileChosen(event: Event, store: AppStore): Promise<void> {
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

/** Keeps the map ghost (preview image + AABB outline) in sync with the
   placing flow; runs inside the component so the effect tracks the store. */
function wireGhostSync(map: MapHandle, store: AppStore): void {
  createEffect(() => {
    const controller = map.current;
    if (controller === null) return;
    const mode = store.placeMode;
    const prepared = store.prepared;
    if (mode === "choose" && prepared !== null) {
      const size = store.ghostSize();
      controller.setGhost({
        url: prepared.previewUrl,
        w: size.w,
        h: size.h,
        rot: store.rotation,
      });
      controller.setPlacing(true);
    } else {
      controller.setGhost(null);
      controller.setPlacing(false);
    }
  });
}

export default function App() {
  const store = createAppStore(createApiClient());

  const map: MapHandle = { current: null, el: undefined };
  let fileInput: HTMLInputElement | undefined;
  const [showAbout, setShowAbout] = createSignal(false);

  const sse = new SseClient({
    onEvent: (event) => {
      const [x, y, w, h] = event.bbox;
      const bbox: BBox = { x, y, w, h };
      // Coalesced: place AND remove events refresh the affected visible
      // tiles at most once per ~500ms window, deduped by tile coord.
      map.current?.queueTileRefresh(bbox);
      map.current?.pulse(bbox);
      if (eventTouchesCenter(event.bbox)) void store.refreshCenter();
    },
    onMalformed: () => {
      // The raw payload is already console.warn'ed by the client.
      store.notifyError("Live update ignored — bad payload");
    },
  });

  // Keep the ghost image in sync with the placing flow.
  wireGhostSync(map, store);

  onMount(() => {
    if (map.el === undefined) return;
    map.current = createWorldMap(map.el, {
      onMapClick: (x, y) => void onMapClick(map, store, x, y),
      isLoggedIn: () => store.me !== null,
      isAdmin: () => store.me?.is_admin === true,
      onReport: (id) => void store.reportPlacement(id),
      onRemove: (id, bbox) => void onRemove(map, store, id, bbox),
      onAuthPrompt: () => {
        store.promptAuth();
      },
    });
    void store.refreshMe();
    void store.refreshCenter();
    const poll = setInterval(() => void store.refreshCenter(), 30_000);
    sse.start();
    onCleanup(() => {
      sse.stop();
      clearInterval(poll);
      map.current?.destroy();
      map.current = null;
    });
  });

  return (
    <>
      <div class="map-root" ref={map.el} />

      <input
        class="hidden-input"
        type="file"
        accept="image/*"
        onChange={(e) => void onFileChosen(e, store)}
        ref={fileInput}
      />

      <TopBar
        store={store}
        onPickImage={() => {
          if (store.me === null) {
            store.promptAuth();
            return;
          }
          fileInput?.click();
        }}
      />

      <div class="footer-chip">
        <button
          class="linkish"
          type="button"
          onClick={() => {
            setShowAbout(true);
          }}
        >
          About
        </button>
        <span aria-hidden="true">·</span>
        <a
          href="https://github.com/vreid/idol-appreciation.club"
          target="_blank"
          rel="noreferrer"
        >
          GitHub
        </a>
      </div>

      <PlacePanel store={store} />
      <Toasts toasts={store.toasts} onDismiss={store.dismissToast} />

      <Show when={showAbout()}>
        <AboutModal
          onClose={() => {
            setShowAbout(false);
          }}
        />
      </Show>
    </>
  );
}
