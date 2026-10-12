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

/** Bounds of the native world expressed in CRS.Simple lat/lng space. */
const WORLD_LAT_LNG_BOUNDS = L.latLngBounds([-1, 0], [0, 1]);

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

interface LeafletTileRecord {
  el?: HTMLImageElement;
}

type LeafletTileIndex = Record<string, LeafletTileRecord | undefined>;

/**
 * Leaflet keeps rendered tiles in the undocumented `_tiles` index keyed
 * "z:x:y". Inference cannot express this library internal, so the shape is
 * asserted once here; `undefined` means the layout changed.
 */
function leafletTileIndex(layer: L.TileLayer): LeafletTileIndex | undefined {
  const internal = layer as unknown as { _tiles?: LeafletTileIndex };
  return internal._tiles;
}

const PULSE_MS = 1400;

export function createWorldMap(
  container: HTMLElement,
  callbacks: MapCallbacks,
): WorldMapController {
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

  let ghostEl: HTMLImageElement | null = null;
  let ghostNative = { w: 0, h: 0 };
  let lastPointer: L.Point | null = null;
  let highlight: L.Rectangle | null = null;

  map.on("click", (event: L.LeafletMouseEvent) => {
    const world = latLngToWorld(event.latlng.lat, event.latlng.lng);
    callbacks.onMapClick(world.x, world.y);
  });
  map.on("mousemove", (event: L.LeafletMouseEvent) => {
    lastPointer = event.containerPoint;
    positionGhost();
  });
  map.on("move", () => positionGhost());
  map.on("zoomend", () => resizeGhost());
  map.on("popupclose", () => {
    if (highlight !== null) {
      map.removeLayer(highlight);
      highlight = null;
    }
  });

  function positionGhost(): void {
    if (ghostEl === null || lastPointer === null) return;
    ghostEl.style.transform = `translate(${lastPointer.x}px, ${lastPointer.y}px)`;
    ghostEl.classList.toggle("hidden", map.isMoving());
  }

  function resizeGhost(): void {
    if (ghostEl === null) return;
    const scale = worldScale(map.getZoom());
    ghostEl.style.width = `${ghostNative.w * scale}px`;
    ghostEl.style.height = `${ghostNative.h * scale}px`;
    positionGhost();
  }

  function buildPopup(p: PlacementForPopup): HTMLDivElement {
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

  return {
    invalidateBBox(bbox: BBox): void {
      const tiles = leafletTileIndex(tileLayer);
      if (tiles === undefined) return;
      for (const key of tilesIntersecting(Object.keys(tiles), bbox)) {
        const record = tiles[key];
        const el = record?.el;
        const tile = parseTileKey(key);
        if (el !== undefined && tile !== null) {
          el.src = tileUrl(tile.z, tile.x, tile.y);
        }
      }
    },

    pulse(bbox: BBox): void {
      const rect = L.rectangle(
        L.latLngBounds(
          worldToLatLng(bbox.x, bbox.y),
          worldToLatLng(bbox.x + bbox.w, bbox.y + bbox.h),
        ),
        { className: "pulse-rect", interactive: false, keyboard: false },
      ).addTo(map);
      setTimeout(() => map.removeLayer(rect), PULSE_MS);
    },

    setGhost(ghost: GhostImage | null): void {
      if (ghost === null) {
        ghostEl?.remove();
        ghostEl = null;
        return;
      }
      ghostNative = { w: ghost.w, h: ghost.h };
      if (ghostEl === null) {
        ghostEl = document.createElement("img");
        ghostEl.className = "map-ghost";
        ghostEl.alt = "";
        container.append(ghostEl);
      }
      ghostEl.src = ghost.url;
      resizeGhost();
    },

    setPlacing(placing: boolean): void {
      container.classList.toggle("placing", placing);
      if (placing) map.scrollWheelZoom.disable();
      else map.scrollWheelZoom.enable();
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
      if (highlight !== null) map.removeLayer(highlight);
      highlight = L.rectangle(
        L.latLngBounds(
          worldToLatLng(p.x, p.y),
          worldToLatLng(p.x + p.w, p.y + p.h),
        ),
        { className: "highlight-rect", interactive: false, keyboard: false },
      ).addTo(map);

      L.popup({ className: "world-popup", closeButton: true })
        .setLatLng(worldToLatLng(p.x + p.w / 2, p.y + p.h / 2))
        .setContent(buildPopup(p))
        .openOn(map);
    },

    closePopup(): void {
      map.closePopup();
    },

    destroy(): void {
      ghostEl?.remove();
      ghostEl = null;
      map.remove();
    },
  };
}
