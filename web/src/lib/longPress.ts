import L from "leaflet";
import { latLngToWorld } from "./world";
import type { MapCallbacks } from "./worldMap";

const LONG_PRESS_MS = 500;
const SLOP_PX = 10;

/**
 * Long-press-to-place input: desktop right-clicks arrive as Leaflet
 * `contextmenu`; touch long-presses need the manual timer (iOS Safari
 * never fires contextmenu for touches). Fires `callbacks.onLongPress`
 * in world px and returns a sink that eats the synthetic click the
 * touchend after a fired long-press would otherwise produce.
 */
export function wireLongPress(
  container: HTMLElement,
  map: L.Map,
  callbacks: MapCallbacks,
): { eatClick: () => boolean } {
  let suppress = false;
  const fire = (lat: number, lng: number): void => {
    suppress = true;
    const world = latLngToWorld(lat, lng);
    callbacks.onLongPress(world.x, world.y);
  };

  map.on("contextmenu", (event: L.LeafletMouseEvent) => {
    fire(event.latlng.lat, event.latlng.lng);
  });

  let timer: ReturnType<typeof setTimeout> | null = null;
  let start: { x: number; y: number } | null = null;
  const cancel = (): void => {
    if (timer !== null) {
      clearTimeout(timer);
      timer = null;
      start = null;
    }
  };

  container.addEventListener(
    "touchstart",
    (event) => {
      const touch = event.touches[0];
      if (event.touches.length !== 1 || touch === undefined) {
        cancel();
        return;
      }
      start = { x: touch.clientX, y: touch.clientY };
      timer = setTimeout(() => {
        timer = null;
        if (start === null) return;
        const latlng = map.containerPointToLatLng(L.point(start.x, start.y));
        fire(latlng.lat, latlng.lng);
      }, LONG_PRESS_MS);
    },
    { passive: true },
  );
  container.addEventListener(
    "touchmove",
    (event) => {
      if (timer === null || start === null) return;
      const touch = event.touches[0];
      if (touch === undefined) {
        cancel();
        return;
      }
      // A drag is a pan, not a long-press; small jitter is fine, so
      // allow the usual slop before cancelling.
      const moved =
        Math.abs(touch.clientX - start.x) > SLOP_PX ||
        Math.abs(touch.clientY - start.y) > SLOP_PX;
      if (moved) cancel();
    },
    { passive: true },
  );
  container.addEventListener("touchend", cancel, { passive: true });
  container.addEventListener("touchcancel", cancel, { passive: true });

  return {
    eatClick: () => {
      const eat = suppress;
      suppress = false;
      return eat;
    },
  };
}
