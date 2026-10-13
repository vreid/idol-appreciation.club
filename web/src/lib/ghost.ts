import type L from "leaflet";
import { rotAABB, worldScale } from "./world";
import type { GhostImage } from "./worldMap";

export interface GhostState {
  el: HTMLImageElement | null;
  outline: HTMLDivElement | null;
  native: { w: number; h: number };
  rot: number;
  pointer: L.Point | null;
  moving: boolean;
}

export function makeGhostState(): GhostState {
  return {
    el: null,
    outline: null,
    native: { w: 0, h: 0 },
    rot: 0,
    pointer: null,
    moving: false,
  };
}

export function positionGhost(map: L.Map, ghost: GhostState): void {
  if (ghost.el === null || ghost.pointer === null) return;
  const { x, y } = ghost.pointer;
  ghost.el.style.transform = `translate(${x}px, ${y}px) rotate(${ghost.rot}deg)`;
  ghost.el.classList.toggle("hidden", ghost.moving);
  if (ghost.outline !== null) {
    // The dashed outline is the rotated rect's AABB, centred on the same
    // point as the rect centre.
    const scale = worldScale(map.getZoom());
    const aabb = rotAABB(ghost.native.w, ghost.native.h, ghost.rot);
    ghost.outline.style.transform = `translate(${x + ((ghost.native.w - aabb.w) / 2) * scale}px, ${y + ((ghost.native.h - aabb.h) / 2) * scale}px)`;
    ghost.outline.classList.toggle("hidden", ghost.moving);
  }
}

export function setGhostImage(
  map: L.Map,
  container: HTMLElement,
  ghost: GhostState,
  next: GhostImage | null,
): void {
  ghost.el?.remove();
  ghost.el = null;
  ghost.outline?.remove();
  ghost.outline = null;
  if (next === null) return;
  ghost.native = { w: next.w, h: next.h };
  ghost.rot = next.rot;
  const el = document.createElement("img");
  el.className = "map-ghost";
  el.alt = "";
  el.src = next.url;
  container.append(el);
  ghost.el = el;
  const scale = worldScale(map.getZoom());
  el.style.width = `${ghost.native.w * scale}px`;
  el.style.height = `${ghost.native.h * scale}px`;
  const aabb = rotAABB(next.w, next.h, next.rot);
  const outline = document.createElement("div");
  outline.className = "map-ghost-outline";
  outline.style.width = `${aabb.w * scale}px`;
  outline.style.height = `${aabb.h * scale}px`;
  container.append(outline);
  ghost.outline = outline;
  positionGhost(map, ghost);
}
