import "leaflet/dist/leaflet.css";
import L from "leaflet";
import {
  latLngToWorld,
  MAX_ZOOM,
  MIN_ZOOM,
  parseTileKey,
  TILE_SIZE,
  tileUrl,
  tilesIntersecting,
  WORLD_CENTER,
  WORLD_LAT_LNG_BOUNDS,
  rotAABB,
  worldFitsViewport,
  worldScale,
  worldToLatLng,
  type BBox,
} from "./world";
import { wireUrlViewState } from "./urlViewState";
import {
  makeGhostState,
  positionGhost,
  setGhostImage,
  type GhostState,
} from "./ghost";
import { timeAgo } from "./format";
import { isRecord } from "./guards";

const PULSE_MS = 1400;

export interface PlacementForPopup {
  id: string;
  username: string;
  x: number;
  y: number;
  w: number;
  h: number;
  rot: number;
  created_at: string;
}

export interface MapCallbacks {
  onMapClick(x: number, y: number): void;
  isLoggedIn(): boolean;
  isAdmin(): boolean;
  onReport(id: string): void;
  onRemove(id: string, bbox: BBox): void;
  onAuthPrompt(): void;
}

export interface GhostImage {
  url: string;
  /** Native world px size of the unrotated ghost rect. */
  w: number;
  h: number;
  /** Clockwise rotation in degrees, applied about the rect centre. */
  rot: number;
}

export interface WorldMapController {
  invalidateBBox(bbox: BBox): void;
  pulse(bbox: BBox): void;
  setGhost(ghost: GhostImage | null): void;
  setPlacing(placing: boolean): void;
  currentZoom(): number;
  worldToScreenPoint(x: number, y: number): { x: number; y: number };
  showPlacement(p: PlacementForPopup): void;
  closePopup(): void;
  destroy(): void;
}

function tileImageOf(
  tiles: Record<string, unknown>,
  key: string,
): HTMLImageElement | null {
  const record: unknown = tiles[key];
  if (!isRecord(record)) return null;
  const el = record["el"];
  return el instanceof HTMLImageElement ? el : null;
}

/** Refetch every rendered tile intersecting the bbox (ETag/no-cache tiles). */
function invalidateTiles(tileLayer: L.TileLayer, bbox: BBox): void {
  // Leaflet keeps rendered tiles in the undocumented, protected `_tiles`
  // index keyed "z:x:y". Read it through a widened alias — the shape is
  // re-validated before use because the library does not publish it.
  const internal: object = tileLayer;
  if (!("_tiles" in internal)) return;
  const tiles: unknown = internal["_tiles"];
  if (!isRecord(tiles)) return;
  for (const key of tilesIntersecting(Object.keys(tiles), bbox)) {
    const el = tileImageOf(tiles, key);
    const tile = parseTileKey(key);
    if (el !== null && tile !== null) {
      el.src = tileUrl(tile.z, tile.x, tile.y);
    }
  }
}

function addPulseRect(map: L.Map, bbox: BBox): void {
  const rect = L.rectangle(
    L.latLngBounds(
      worldToLatLng(bbox.x, bbox.y),
      worldToLatLng(bbox.x + bbox.w, bbox.y + bbox.h),
    ),
    { className: "pulse-rect", interactive: false },
  ).addTo(map);
  setTimeout(() => {
    map.removeLayer(rect);
  }, PULSE_MS);
}

/**
 * Highlight for a (possibly rotated) placement: a screen-px-sized div centred
 * on the rect centre, rotated about it. `highlight-rect` shows the rect;
 * the anchor box carries the dashed AABB.
 */
function showHighlight(
  map: L.Map,
  highlight: { current: L.Marker | null; placement: PlacementForPopup | null },
): void {
  const p = highlight.placement;
  if (p === null) return;
  if (highlight.current !== null) {
    map.removeLayer(highlight.current);
    highlight.current = null;
  }
  const scale = worldScale(map.getZoom());
  const aabb = rotAABB(p.w, p.h, p.rot);
  const aw = Math.max(1, Math.round(aabb.w * scale));
  const ah = Math.max(1, Math.round(aabb.h * scale));
  const rw = Math.max(1, Math.round(p.w * scale));
  const rh = Math.max(1, Math.round(p.h * scale));
  highlight.current = L.marker(worldToLatLng(p.x + p.w / 2, p.y + p.h / 2), {
    interactive: false,
    keyboard: false,
    icon: L.divIcon({
      className: "highlight-anchor",
      html:
        `<div class="highlight-aabb" style="width:${aw}px;height:${ah}px"></div>` +
        `<div class="highlight-rect" style="width:${rw}px;height:${rh}px;transform:translate(-50%,-50%) rotate(${p.rot}deg)"></div>`,
      iconSize: [aw, ah],
      iconAnchor: [aw / 2, ah / 2],
    }),
  }).addTo(map);
}

function popupContent(
  p: PlacementForPopup,
  callbacks: MapCallbacks,
  map: L.Map,
): HTMLDivElement {
  const content = document.createElement("div");
  content.className = "placement-popup";

  const head = document.createElement("div");
  head.className = "pp-head";
  head.textContent = `@${p.username}`;
  content.append(head);

  const meta = document.createElement("div");
  meta.className = "pp-meta";
  meta.textContent = `${p.w}×${p.h} px · ${timeAgo(p.created_at)}`;
  content.append(meta);

  const actions = document.createElement("div");
  actions.className = "pp-actions";

  const reportBtn = document.createElement("button");
  reportBtn.type = "button";
  reportBtn.className = "pp-btn";
  reportBtn.textContent = "Report";
  reportBtn.addEventListener("click", () => {
    if (callbacks.isLoggedIn()) {
      callbacks.onReport(p.id);
      map.closePopup();
    } else {
      callbacks.onAuthPrompt();
    }
  });
  actions.append(reportBtn);

  if (callbacks.isAdmin()) {
    const removeBtn = document.createElement("button");
    removeBtn.type = "button";
    removeBtn.className = "pp-btn pp-danger";
    removeBtn.textContent = "Remove";
    removeBtn.addEventListener("click", () => {
      callbacks.onRemove(p.id, { x: p.x, y: p.y, w: p.w, h: p.h });
    });
    actions.append(removeBtn);
  }

  content.append(actions);
  return content;
}

interface MapInternals {
  map: L.Map;
  tileLayer: L.TileLayer;
  ghost: GhostState;
  callbacks: MapCallbacks;
  highlight: { current: L.Marker | null; placement: PlacementForPopup | null };
}

function wireMapEvents(internal: MapInternals): void {
  const { map, ghost, callbacks, highlight } = internal;
  map.on("zoomend", () => {
    // The rotated highlight is sized in screen px — re-render it at the new
    // scale while its popup is open.
    if (highlight.placement !== null) {
      showHighlight(map, highlight);
    }
  });
  map.on("click", (event: L.LeafletMouseEvent) => {
    const world = latLngToWorld(event.latlng.lat, event.latlng.lng);
    callbacks.onMapClick(world.x, world.y);
  });
  map.on("mousemove", (event: L.LeafletMouseEvent) => {
    ghost.pointer = event.containerPoint;
    positionGhost(map, ghost);
  });
  map.on("move", () => {
    positionGhost(map, ghost);
  });
  map.on("movestart", () => {
    ghost.moving = true;
  });
  map.on("moveend", () => {
    ghost.moving = false;
    positionGhost(map, ghost);
  });
  map.on("popupclose", () => {
    if (highlight.current !== null) {
      map.removeLayer(highlight.current);
      highlight.current = null;
    }
    highlight.placement = null;
  });
}

const WORLD_CENTER_LAT_LNG = worldToLatLng(WORLD_CENTER, WORLD_CENTER);

/**
 * Leaflet clamps panning via maxBounds but never re-centres the view after a
 * resize or an interrupted pinch/scroll gesture. When the whole world fits the
 * viewport (far-out zooms), snap the view centre onto the world centre so the
 * 0/0 reticle renders dead-centre instead of clinging to a corner.
 */
function wireWorldCentering(map: L.Map): () => void {
  const centerWorldIfFitted = (): void => {
    const size = map.getSize();
    if (worldFitsViewport(size.x, size.y, map.getZoom())) {
      map.setView(WORLD_CENTER_LAT_LNG, map.getZoom(), { animate: false });
    }
  };
  const onWindowResize = (): void => {
    map.invalidateSize();
    centerWorldIfFitted();
  };
  map.on("zoomend", centerWorldIfFitted);
  window.addEventListener("resize", onWindowResize);
  return () => {
    window.removeEventListener("resize", onWindowResize);
    map.off("zoomend", centerWorldIfFitted);
  };
}

export function createWorldMap(
  container: HTMLElement,
  callbacks: MapCallbacks,
): WorldMapController {
  // L.map is Leaflet's map factory, not Array.prototype.map — the
  // no-array-callback-reference heuristic false-positives here.
  // eslint-disable-next-line unicorn/no-array-callback-reference
  const map = L.map(container, {
    crs: L.CRS.Simple,
    minZoom: MIN_ZOOM,
    maxZoom: MAX_ZOOM,
    maxBounds: L.latLngBounds(...WORLD_LAT_LNG_BOUNDS),
    maxBoundsViscosity: 1.0,
    zoomControl: false,
    attributionControl: false,
    worldCopyJump: false,
  });
  L.control.zoom({ position: "bottomright" }).addTo(map);

  const tileLayer = L.tileLayer("/api/tiles/{z}/{x}/{y}.webp", {
    tileSize: TILE_SIZE,
    minZoom: MIN_ZOOM,
    maxZoom: MAX_ZOOM,
    bounds: L.latLngBounds(...WORLD_LAT_LNG_BOUNDS),
  }).addTo(map);

  const stopCentering = wireWorldCentering(map);
  const stopUrlView = wireUrlViewState(map);

  const internal: MapInternals = {
    map,
    tileLayer,
    callbacks,
    ghost: makeGhostState(),
    highlight: { current: null, placement: null },
  };
  wireMapEvents(internal);
  const { ghost, highlight } = internal;

  return {
    invalidateBBox(bbox: BBox): void {
      invalidateTiles(tileLayer, bbox);
    },

    pulse(bbox: BBox): void {
      addPulseRect(map, bbox);
    },

    setGhost(next: GhostImage | null): void {
      setGhostImage(map, container, ghost, next);
    },

    setPlacing(placing: boolean): void {
      container.classList.toggle("placing", placing);
      if (placing) {
        map.scrollWheelZoom.disable();
      } else {
        map.scrollWheelZoom.enable();
      }
    },

    currentZoom(): number {
      return map.getZoom();
    },

    worldToScreenPoint(x: number, y: number): { x: number; y: number } {
      const point = map.latLngToContainerPoint(worldToLatLng(x, y));
      return { x: point.x, y: point.y };
    },

    showPlacement(p: PlacementForPopup): void {
      highlight.placement = p;
      showHighlight(map, highlight);
      L.popup({ className: "world-popup", closeButton: true })
        .setLatLng(worldToLatLng(p.x + p.w / 2, p.y + p.h / 2))
        .setContent(popupContent(p, callbacks, map))
        .openOn(map);
    },

    closePopup(): void {
      map.closePopup();
    },

    destroy(): void {
      stopCentering();
      stopUrlView();
      ghost.el?.remove();
      ghost.el = null;
      map.remove();
    },
  };
}
