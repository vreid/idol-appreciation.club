import { createEffect, onCleanup, onMount, createSignal, Show } from "solid-js";
import { createApiClient } from "./lib/api";
import { createAppStore } from "./lib/state";
import { burstConfetti } from "./lib/confetti";
import { readNaturalSize } from "./lib/images";
import { SseClient, eventTouchesCenter } from "./lib/sse";
import { createWorldMap, type WorldMapController } from "./lib/worldMap";
import {
  queryRadiusForZoom,
  rotAABB,
  worldScale,
  type BBox,
} from "./lib/world";
import type { Placement } from "./lib/api";
import PlaceDetail, {
  type PlaceDetailActions,
  type SelectionGeometry,
} from "./lib/components/PlaceDetail";
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

/** One deselect path: clears the store selection, the map highlight,
   and hands the wheel back to zoom. */
function releaseSelection(map: MapHandle, store: AppStore): void {
  store.deselectPlace();
  map.current?.clearSelection();
  map.current?.setDigMode(false);
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
  // Empty spots are a silent no-op — no toast spam on bare canvas — but
  // they deselect (clicking away closes the detail panel and restores
  // wheel-zoom).
  if (top === undefined) {
    releaseSelection(map, store);
    return;
  }
  store.selectPlace(top, { x, y });
  controller.showSelection(top);
  // Wheel now peels the stack under the click point instead of zooming.
  controller.setDigMode(true, (deltaY) => {
    store.flipDig(deltaY > 0 ? 1 : -1);
  });
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
    releaseSelection(map, store);
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

/** Tracks the selected placement (including wheel-flipped strata) so the
   highlight always wraps the pixels the panel describes. */
function wireSelectionSync(map: MapHandle, store: AppStore): void {
  createEffect(() => {
    const controller = map.current;
    if (controller === null) return;
    const selected = store.selectedPlace;
    if (selected !== null) controller.showSelection(selected);
  });
}

/** Screen-space anchor geometry for the selected placement. Deliberately
   non-reactive: reflects the map state at the moment the viewport signal
   bumps (PlaceDetail re-runs its layout effect on that signal). */
function selectionGeometry(
  map: MapHandle,
  placement: Placement,
): SelectionGeometry {
  const c = map.current;
  if (c === null) {
    return {
      aabb: { x: -9999, y: -9999, w: 0, h: 0 },
      viewport: { w: 0, h: 0 },
    };
  }
  const scale = worldScale(c.currentZoom());
  const aabbSize = rotAABB(placement.w, placement.h, placement.rot);
  const center = c.worldToScreenPoint(
    placement.x + placement.w / 2,
    placement.y + placement.h / 2,
  );
  return {
    aabb: {
      x: center.x - (aabbSize.w * scale) / 2,
      y: center.y - (aabbSize.h * scale) / 2,
      w: aabbSize.w * scale,
      h: aabbSize.h * scale,
    },
    viewport: {
      w: map.el?.clientWidth ?? 0,
      h: map.el?.clientHeight ?? 0,
    },
  };
}

/** Panel actions live in App so Report/Remove reuse the same store paths
   as the rest of the UI. */
function placeDetailActions(
  map: MapHandle,
  store: AppStore,
): PlaceDetailActions {
  return {
    isLoggedIn: () => store.me !== null,
    onReport: (id) => void store.reportPlacement(id),
    isAdmin: () => store.me?.is_admin === true,
    onRemove: (id, bbox) => void onRemove(map, store, id, bbox),
    onAuthPrompt: () => {
      store.promptAuth();
    },
    onDeselect: () => {
      releaseSelection(map, store);
    },
  };
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
      // The selected stratum vanished under someone else: close the
      // panel and hand the wheel back to the map.
      if (event.type === "remove" && store.selectedPlace?.id === event.id) {
        releaseSelection(map, store);
      }
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
  wireSelectionSync(map, store);
  const detailActions = placeDetailActions(map, store);

  // Bumped on every pan/zoom so anchored overlays reposition.
  const [viewportVersion, bumpViewport] = createSignal(0);

  onMount(() => {
    if (map.el === undefined) return;
    map.current = createWorldMap(map.el, {
      onMapClick: (x, y) => void onMapClick(map, store, x, y),
    });
    map.current.onViewportChange(() => {
      bumpViewport(1);
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
      <div class="map-root" ref={map.el}>
        <PlaceDetail
          store={store}
          container={map.el}
          viewportVersion={viewportVersion}
          geometry={(placement) => selectionGeometry(map, placement)}
          actions={detailActions}
        />
      </div>

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
