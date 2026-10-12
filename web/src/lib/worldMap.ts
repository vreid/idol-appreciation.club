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
  worldScale,
  worldToLatLng,
  type BBox,
} from "./world";
import { timeAgo } from "./format";
import { isRecord } from "./guards";

/** Bounds of the native world expressed in CRS.Simple lat/lng space. */
const WORLD_LAT_LNG_BOUNDS = L.latLngBounds([-1, 0], [0, 1]);
const PULSE_MS = 1400;

export interface PlacementForPopup {
  id: number;
  username: string;
  x: number;
  y: number;
  w: number;
  h: number;
  created_at: string;
}

export interface MapCallbacks {
  onMapClick(x: number, y: number): void;
  isLoggedIn(): boolean;
  isAdmin(): boolean;
  onReport(id: number): void;
  onRemove(id: number, bbox: BBox): void;
  onAuthPrompt(): void;
}

export interface GhostImage {
  url: string;
  /** Native world px size of the ghost. */
  w: number;
  h: number;
}

export interface WorldMapController {
  invalidateBBox(bbox: BBox): void;
  pulse(bbox: BBox): void;
  setGhost(ghost: GhostImage | null): void;
  setPlacing(placing: boolean): void;
  jumpToCenter(): void;
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

function bboxRectangle(map: L.Map, bbox: BBox, className: string): L.Rectangle {
  return L.rectangle(
    L.latLngBounds(
      worldToLatLng(bbox.x, bbox.y),
      worldToLatLng(bbox.x + bbox.w, bbox.y + bbox.h),
    ),
    { className, interactive: false },
  ).addTo(map);
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

interface GhostState {
  el: HTMLImageElement | null;
  native: { w: number; h: number };
  pointer: L.Point | null;
  moving: boolean;
}

function positionGhost(map: L.Map, ghost: GhostState): void {
  if (ghost.el === null || ghost.pointer === null) return;
  ghost.el.style.transform = `translate(${ghost.pointer.x}px, ${ghost.pointer.y}px)`;
  ghost.el.classList.toggle("hidden", ghost.moving);
}

function setGhostImage(
  map: L.Map,
  container: HTMLElement,
  ghost: GhostState,
  next: GhostImage | null,
): void {
  ghost.el?.remove();
  ghost.el = null;
  if (next === null) return;
  const el = document.createElement("img");
  el.className = "map-ghost";
  el.alt = "";
  el.src = next.url;
  ghost.native = { w: next.w, h: next.h };
  container.append(el);
  ghost.el = el;
  const scale = worldScale(map.getZoom());
  el.style.width = `${ghost.native.w * scale}px`;
  el.style.height = `${ghost.native.h * scale}px`;
  positionGhost(map, ghost);
}

interface MapInternals {
  map: L.Map;
  tileLayer: L.TileLayer;
  ghost: GhostState;
  callbacks: MapCallbacks;
  highlight: { current: L.Rectangle | null };
}

function wireMapEvents(internal: MapInternals): void {
  const { map, ghost, callbacks, highlight } = internal;
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
  });
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
    maxBounds: WORLD_LAT_LNG_BOUNDS,
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
    bounds: WORLD_LAT_LNG_BOUNDS,
  }).addTo(map);

  L.marker(worldToLatLng(WORLD_CENTER, WORLD_CENTER), {
    interactive: false,
    keyboard: false,
    icon: L.divIcon({
      className: "center-reticle",
      html: '<div class="reticle"></div><div class="reticle-label">0/0</div>',
      iconSize: [0, 0],
    }),
  }).addTo(map);

  map.setView(worldToLatLng(WORLD_CENTER, WORLD_CENTER), 16);

  const internal: MapInternals = {
    map,
    tileLayer,
    callbacks,
    ghost: { el: null, native: { w: 0, h: 0 }, pointer: null, moving: false },
    highlight: { current: null },
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

    jumpToCenter(): void {
      map.flyTo(worldToLatLng(WORLD_CENTER, WORLD_CENTER), 16);
    },

    currentZoom(): number {
      return map.getZoom();
    },

    worldToScreenPoint(x: number, y: number): { x: number; y: number } {
      const point = map.latLngToContainerPoint(worldToLatLng(x, y));
      return { x: point.x, y: point.y };
    },

    showPlacement(p: PlacementForPopup): void {
      if (highlight.current !== null) {
        map.removeLayer(highlight.current);
      }
      highlight.current = bboxRectangle(map, p, "highlight-rect");
      L.popup({ className: "world-popup", closeButton: true })
        .setLatLng(worldToLatLng(p.x + p.w / 2, p.y + p.h / 2))
        .setContent(popupContent(p, callbacks, map))
        .openOn(map);
    },

    closePopup(): void {
      map.closePopup();
    },

    destroy(): void {
      ghost.el?.remove();
      ghost.el = null;
      map.remove();
    },
  };
}
