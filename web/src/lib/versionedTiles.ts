/**
 * Versioned read-path orchestration (docs/scaling-architecture.md, Read
 * path): the tile-version store, the viewport snapshot lifecycle, the
 * SSE area subscription, and the map mount that binds them together.
 */

import { SseClient } from "./sse";
import {
  createTileVersionsStore,
  type TileVersionsStore,
} from "./tileVersions";
import { createWorldMap } from "./worldMap";
import { onLongPress, onMapClick, type MapHandle } from "./appActions";
import type { AppStore } from "./state";

/** Snapshot fetcher for the versions store (GET /api/versions). */
async function fetchTileVersions(query: string): Promise<unknown> {
  const res = await fetch(`/api/versions?${query}`);
  if (!res.ok) throw new Error(`versions ${res.status}`);
  return (await res.json()) as unknown;
}

/**
 * Versioned read path (Read path): snapshot the viewport on (re)connect
 * and after large pans; the snapshot also establishes the subscribed
 * area the SSE stream is z-filtered to. Area checks run on a 2s interval
 * — setArea no-ops while the query is unchanged. Returns the disposer.
 */
function wireTileVersions(
  map: MapHandle,
  sse: SseClient,
  tileVersions: TileVersionsStore,
  onOpenHook: { current: () => void },
): () => void {
  const syncVersions = (): void => {
    const controller = map.current;
    if (controller === null) return;
    const bbox = controller.viewportBBox();
    if (bbox === null) return;
    const z = Math.round(controller.currentZoom());
    void tileVersions.refreshSnapshot(bbox, z).then(() => {
      const query = tileVersions.areaQuery();
      if (query !== "") sse.setArea(query);
    });
  };
  onOpenHook.current = syncVersions;
  const areaWatcher = setInterval(() => {
    const controller = map.current;
    if (controller === null) return;
    const bbox = controller.viewportBBox();
    if (bbox === null) return;
    if (
      tileVersions.shouldResubscribe(bbox, Math.round(controller.currentZoom()))
    ) {
      syncVersions();
    }
  }, 2_000);
  return () => {
    clearInterval(areaWatcher);
    tileVersions.clear();
  };
}

interface MountWorldMapArgs {
  map: MapHandle;
  store: AppStore;
  tileVersions: TileVersionsStore;
  sse: SseClient;
  onOpenHook: { current: () => void };
  fileInput: () => HTMLInputElement | undefined;
  bumpViewport: () => void;
}

/** Everything the map needs at mount: construction, polls, SSE start.
 * Returns the full disposer for onCleanup. */
export function mountWorldMap(args: MountWorldMapArgs): () => void {
  const { map, store, tileVersions, sse, onOpenHook, bumpViewport } = args;
  if (map.el === undefined) return () => {};
  map.current = createWorldMap(
    map.el,
    {
      onMapClick: (x, y) => void onMapClick(map, store, x, y),
      onLongPress: (x, y) => {
        onLongPress(map, store, () => args.fileInput()?.click(), x, y);
      },
    },
    {
      versionOf: (z, x, y) => tileVersions.get(z, x, y),
    },
  );
  map.current.onViewportChange(bumpViewport);
  void store.refreshMe();
  void store.refreshCenter();
  const poll = setInterval(() => void store.refreshCenter(), 30_000);
  const stopVersions = wireTileVersions(map, sse, tileVersions, onOpenHook);
  sse.start();
  return () => {
    sse.stop();
    clearInterval(poll);
    stopVersions();
    map.current?.destroy();
    map.current = null;
  };
}

/** Everything the versioned read path needs before the map exists.
 * The versions event branch wraps the caller's world-event handler. */
export function createVersionedReadPath(
  map: MapHandle,
  baseOptions: ConstructorParameters<typeof SseClient>[0],
): {
  tileVersions: TileVersionsStore;
  sse: SseClient;
  onOpenHook: { current: () => void };
} {
  const tileVersions = createTileVersionsStore({
    fetchVersions: fetchTileVersions,
  });
  // Snapshot on (re)connect + resubscribe after large pans; the hook
  // body is installed once the map exists (wireTileVersions).
  const onOpenHook: { current: () => void } = { current: () => {} };
  const sse = new SseClient({
    ...baseOptions,
    onEvent: (event) => {
      if (event.type === "versions") {
        // Batched tile-version updates: learn, then refetch exactly the
        // rendered tiles whose version changed (URL swap, no buster).
        const changed = tileVersions.learn(event.tiles);
        map.current?.refreshTileKeys(changed);
        return;
      }
      baseOptions.onEvent(event);
    },
    onOpen: () => {
      onOpenHook.current();
    },
  });
  return { tileVersions, sse, onOpenHook };
}
