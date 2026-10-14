import { burstConfetti } from "./confetti";
import { readNaturalSize } from "./images";
import { queryRadiusForZoom, type BBox } from "./world";
import type { WorldMapController } from "./worldMap";
import type { PlaceDetailActions } from "./components/PlaceDetail";
import type { Placement } from "@contract";
import type { AppStore } from "./state";

/** The imperative map bits the async handlers need access to. */
export interface MapHandle {
  current: WorldMapController | null;
  el: HTMLDivElement | undefined;
}

/** One deselect path: selection + highlight released. */
export function releaseSelection(map: MapHandle, store: AppStore): void {
  store.deselectPlace();
  map.current?.clearSelection();
}

/** Long-press target: deselect, pin the spot, open the upload flow. */
export function onLongPress(
  map: MapHandle,
  store: AppStore,
  openPicker: () => void,
  x: number,
  y: number,
): void {
  releaseSelection(map, store);
  store.pinSpot(x, y);
  openPicker();
}

export async function onMapClick(
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
  // Empty spots silently deselect (and restore wheel-zoom).
  if (top === undefined) {
    releaseSelection(map, store);
    return;
  }
  store.selectPlace(top);
  controller.showSelection(top);
}

export async function onRemove(
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

export async function onFileChosen(
  event: Event,
  store: AppStore,
): Promise<void> {
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

/** Jumps the map to a placement and selects it at its centre (the dig
 * query fills the depth cue). */
export function jumpToPlacement(
  map: MapHandle,
  store: AppStore,
  place: Placement,
): void {
  const cx = place.x + place.w / 2;
  const cy = place.y + place.h / 2;
  map.current?.centerOn(cx, cy);
  store.selectPlace(place);
}

/** Panel actions share the same store paths as the map click handlers. */
export function placeDetailActions(
  map: MapHandle,
  store: AppStore,
): PlaceDetailActions {
  return {
    isLoggedIn: () => store.me !== null,
    onReport: (id) => void store.reportPlacement(id),
    isAdmin: () => store.me?.is_admin === true,
    onRemove: (id, bbox) => void onRemove(map, store, id, bbox),
    onUpdateTags: async (id, tags) => {
      await store.updateTags(id, tags);
    },
    onAuthPrompt: () => {
      store.promptAuth();
    },
    onDeselect: () => {
      releaseSelection(map, store);
    },
  };
}
