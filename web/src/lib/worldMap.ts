import "leaflet/dist/leaflet.css";
import { wireLongPress } from "./longPress";
import L from "leaflet";
import {
  latLngToWorld,
  MAX_ZOOM,
  MIN_ZOOM,
  TILE_SIZE,
  tileUrl,
  WORLD_CENTER,
  WORLD_LAT_LNG_BOUNDS,
  rotAABB,
  worldFitsViewport,
  worldScale,
  worldToLatLng,
  type BBox,
} from "./world";
import {
  affectedRenderedTiles,
  createTileRefresher,
  type TileRefreshSource,
} from "./tileRefresh";
import { refetchTiles, renderedTileMap, viewWorldBBox } from "./tileDom";
import { wireUrlViewState } from "./urlViewState";
import {
  makeGhostState,
  positionGhost,
  setGhostImage,
  type GhostState,
  renderPendingMarkers,
  type PendingMarker,
} from "./ghost";
import type { Placement } from "./api";

const PULSE_MS = 1400;

export interface MapCallbacks {
  onMapClick(x: number, y: number): void;
  /** Long-press (or right-click) at world coords: opens the place flow
   * pinned to that spot on touch devices. */
  onLongPress(x: number, y: number): void;
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
  /** Immediate single-bbox refresh (optimistic local changes). */
  invalidateBBox(bbox: BBox): void;
  /** Coalesced refresh for SSE event storms (~500ms window, deduped). */
  queueTileRefresh(bbox: BBox): void;
  /** Coalesced refresh of explicit tiles (version events name them). */
  refreshTileKeys(keys: string[]): void;
  pulse(bbox: BBox): void;
  setGhost(ghost: GhostImage | null): void;
  /** The 202-accepted uploads' preview markers (re-rendered per zoom). */
  setPendingMarkers(markers: PendingMarker[]): void;
  setPlacing(placing: boolean): void;
  currentZoom(): number;
  worldToScreenPoint(x: number, y: number): { x: number; y: number };
  /** Pans the view so world (x, y) sits at the viewport centre; an
   * explicit zoom also replaces the current one. */
  centerOn(x: number, y: number, zoom?: number): void;
  /** Highlight the selection's AABB; the panel itself is Solid-rendered. */
  showSelection(p: Placement): void;
  clearSelection(): void;
  /** Fires on every pan/zoom so anchored overlays can reposition. */
  onViewportChange(cb: () => void): void;
  /** Viewport as a native-px world bbox; null before the first view. */
  viewportBBox(): BBox | null;
  /** Selection-held wheel mode: zoom disabled, wheel flips the stack. */
  destroy(): void;
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
  highlight: { current: L.Marker | null; placement: Placement | null },
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

interface MapInternals {
  map: L.Map;
  container: HTMLElement;
  tileLayer: L.TileLayer;
  ghost: GhostState;
  callbacks: MapCallbacks;
  highlight: { current: L.Marker | null; placement: Placement | null };
  pending: { markers: PendingMarker[]; layer: L.LayerGroup | null };
}

function wireMapEvents(internal: MapInternals): void {
  const { map, ghost, callbacks, highlight, pending } = internal;
  map.on("zoomend", () => {
    // The rotated highlight and the pending markers are sized in
    // screen px — re-render them at the new scale.
    if (highlight.placement !== null) {
      showHighlight(map, highlight);
    }
    renderPendingMarkers(map, pending);
  });
  const longPress = wireLongPress(internal.container, map, callbacks);
  map.on("click", (event: L.LeafletMouseEvent) => {
    // The touchend after a fired long-press synthesizes a click — eat
    // it so the pinned flow isn't immediately deselected.
    if (longPress.eatClick()) return;
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

function addTileLayer(
  map: L.Map,
  versionOf: (z: number, x: number, y: number) => number | undefined,
): L.TileLayer {
  const tileLayer = L.tileLayer("/api/tiles/{z}/{x}/{y}.webp", {
    tileSize: TILE_SIZE,
    minZoom: MIN_ZOOM,
    maxZoom: MAX_ZOOM,
    bounds: L.latLngBounds(...WORLD_LAT_LNG_BOUNDS),
  }).addTo(map);
  // Per-tile version hint (?v=, Read path): Leaflet resolves each tile's
  // URL through getTileUrl, so replacing it on the instance lets the
  // learned version decide the query while the template stays as the
  // construction fallback. ?v is a hint backed by the same counter as
  // the tile etag; versionless tiles fall through to the server's
  // max-age=1 legacy row.
  const versioned = tileLayer as L.TileLayer & {
    getTileUrl(coords: { x: number; y: number; z: number }): string;
  };
  versioned.getTileUrl = (coords) =>
    tileUrl(
      coords.z,
      coords.x,
      coords.y,
      versionOf(coords.z, coords.x, coords.y),
    );
  return tileLayer;
}

function createRefreshLoop(
  map: L.Map,
  tileLayer: L.TileLayer,
  versionOf: (z: number, x: number, y: number) => number | undefined,
): {
  queue: (bbox: BBox) => void;
  queueKeys: (keys: string[]) => void;
  invalidate: (bbox: BBox) => void;
  dispose: () => void;
} {
  const source: TileRefreshSource = {
    renderedTileKeys: () => Object.keys(renderedTileMap(tileLayer)),
    viewport: () => viewWorldBBox(map),
  };
  const refresher = createTileRefresher(source, (keys) => {
    refetchTiles(tileLayer, keys, versionOf);
  });
  return {
    queue: (bbox) => {
      refresher.queue(bbox);
    },
    queueKeys: (keys) => {
      refresher.queueKeys(keys);
    },
    invalidate: (bbox) => {
      refetchTiles(tileLayer, affectedRenderedTiles(source, [bbox]));
    },
    dispose: () => {
      refresher.dispose();
    },
  };
}

/** The Leaflet map factory: world CRS, hard world bounds, no chrome. */
function createLeafletMap(container: HTMLElement): L.Map {
  // L.map is Leaflet's map factory, not Array.prototype.map — the
  // no-array-callback-reference heuristic false-positives here.
  // eslint-disable-next-line unicorn/no-array-callback-reference
  return L.map(container, {
    crs: L.CRS.Simple,
    minZoom: MIN_ZOOM,
    maxZoom: MAX_ZOOM,
    maxBounds: L.latLngBounds(...WORLD_LAT_LNG_BOUNDS),
    maxBoundsViscosity: 1.0,
    zoomControl: false,
    attributionControl: false,
    worldCopyJump: false,
  });
}

export function createWorldMap(
  container: HTMLElement,
  callbacks: MapCallbacks,
  options: {
    /** Learned tile-version lookup for ?v= URL hints (Read path). */
    versionOf?: (z: number, x: number, y: number) => number | undefined;
  } = {},
): WorldMapController {
  const map = createLeafletMap(container);
  L.control.zoom({ position: "bottomright" }).addTo(map);

  // Single-slot notify for anchored overlays (the place-detail panel):
  // "move" covers pans and zoom animations; fires at frame rate, so the
  // consumer must be cheap.
  let viewportCb: (() => void) | null = null;
  map.on("move", () => {
    if (viewportCb !== null) viewportCb();
  });

  const versionOf = options.versionOf ?? (() => {});
  const tileLayer = addTileLayer(map, versionOf);

  const stopCentering = wireWorldCentering(map);
  const stopUrlView = wireUrlViewState(map);

  const refresh = createRefreshLoop(map, tileLayer, versionOf);

  const internal: MapInternals = {
    map,
    container,
    tileLayer,
    callbacks,
    ghost: makeGhostState(),
    highlight: { current: null, placement: null },
    pending: { markers: [], layer: null },
  };
  wireMapEvents(internal);
  const { ghost, highlight } = internal;

  return {
    invalidateBBox(bbox: BBox): void {
      refresh.invalidate(bbox);
    },

    queueTileRefresh(bbox: BBox): void {
      refresh.queue(bbox);
    },

    /** Coalesced refresh of explicit tiles (version events name them). */
    refreshTileKeys(keys: string[]): void {
      refresh.queueKeys(keys);
    },

    pulse(bbox: BBox): void {
      addPulseRect(map, bbox);
    },

    setGhost(next: GhostImage | null): void {
      setGhostImage(map, container, ghost, next);
    },

    setPendingMarkers(markers: PendingMarker[]): void {
      internal.pending.markers = markers;
      renderPendingMarkers(map, internal.pending);
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

    worldToScreenPoint: (x, y) =>
      map.latLngToContainerPoint(worldToLatLng(x, y)),
    centerOn: (x, y, zoom) =>
      void map.setView(worldToLatLng(x, y), zoom ?? map.getZoom()),

    showSelection(p: Placement): void {
      highlight.placement = p;
      showHighlight(map, highlight);
    },

    clearSelection(): void {
      highlight.placement = null;
      if (highlight.current !== null) {
        map.removeLayer(highlight.current);
        highlight.current = null;
      }
    },

    onViewportChange(cb: () => void): void {
      viewportCb = cb;
    },

    viewportBBox(): BBox | null {
      return viewWorldBBox(map);
    },

    destroy(): void {
      refresh.dispose();
      stopCentering();
      stopUrlView();
      ghost.el?.remove();
      ghost.el = null;
      map.remove();
    },
  };
}
