import type L from "leaflet";
import { encodeViewHash, parseViewHash } from "./urlView";
import { WORLD_CENTER, latLngToWorld, worldToLatLng } from "./world";

/** Default view when the URL carries no valid #z/x/y hash. */
const INITIAL_ZOOM = 16;

/** Debounce for URL hash writes on move/zoom settle. */
const DEBOUNCE_MS = 250;

/**
 * Linkable view state in the URL hash: `#z/x/y` with SIGNED world-px offsets
 * of the view centre from the world centre (`#16/0/0` = zoom 16 centred on
 * 0/0). A valid hash on load beats the default centred view; the view is
 * written back with a debounced history.replaceState (no history spam), and
 * hashchange/popstate (manual edits, back/forward) re-apply the hash.
 *
 * Returns a stop function removing every listener and pending timer.
 */
export function wireUrlViewState(map: L.Map): () => void {
  let hashWriteTimer: number | undefined;
  let suppressNextWrite = false;

  const applyHash = (): boolean => {
    const view = parseViewHash(window.location.hash);
    if (view === null) return false;
    map.setView(
      worldToLatLng(WORLD_CENTER + view.x, WORLD_CENTER + view.y),
      view.z,
    );
    return true;
  };

  if (!applyHash()) {
    map.setView(worldToLatLng(WORLD_CENTER, WORLD_CENTER), INITIAL_ZOOM);
  }

  const writeViewHash = (): void => {
    const center = latLngToWorld(map.getCenter().lat, map.getCenter().lng);
    const hash = encodeViewHash({
      z: map.getZoom(),
      x: center.x - WORLD_CENTER,
      y: center.y - WORLD_CENTER,
    });
    if (hash !== window.location.hash) {
      history.replaceState(history.state, "", hash);
    }
  };

  const scheduleWrite = (): void => {
    // A view applied FROM the hash would only rewrite the same hash; skip it.
    if (suppressNextWrite) {
      suppressNextWrite = false;
      return;
    }
    window.clearTimeout(hashWriteTimer);
    hashWriteTimer = window.setTimeout(writeViewHash, DEBOUNCE_MS);
  };

  const onHashChange = (): void => {
    if (applyHash()) suppressNextWrite = true;
  };

  map.on("moveend", scheduleWrite);
  map.on("zoomend", scheduleWrite);
  window.addEventListener("hashchange", onHashChange);
  window.addEventListener("popstate", onHashChange);

  // The initial setView above completed before these listeners existed, so
  // its moveend was missed — write the (possibly defaulted) view once now.
  writeViewHash();

  return () => {
    window.clearTimeout(hashWriteTimer);
    map.off("moveend", scheduleWrite);
    map.off("zoomend", scheduleWrite);
    window.removeEventListener("hashchange", onHashChange);
    window.removeEventListener("popstate", onHashChange);
  };
}
