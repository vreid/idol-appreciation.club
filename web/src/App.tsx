import { createEffect, onCleanup, onMount, createSignal, Show } from "solid-js";
import { createApiClient, type ApiClient } from "./lib/api";
import {
  jumpToPlacement,
  onFileChosen,
  onLongPress,
  onMapClick,
  placeDetailActions,
  releaseSelection,
  type MapHandle,
} from "./lib/appActions";
import { createAppStore } from "./lib/state";
import { SseClient, eventTouchesCenter } from "./lib/sse";
import { createWorldMap } from "./lib/worldMap";
import { rotAABB, worldScale, type BBox } from "./lib/world";
import type { Placement } from "./lib/api";
import PlaceDetail, {
  type SelectionGeometry,
} from "./lib/components/PlaceDetail";
import TopBar from "./lib/components/TopBar";
import FeedPanel from "./lib/components/FeedPanel";
import HotPanel from "./lib/components/HotPanel";
import PlacePanel from "./lib/components/PlacePanel";
import Toasts from "./lib/components/Toasts";
import AboutModal from "./lib/components/AboutModal";
import type { AppStore } from "./lib/state";

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
/** SSE handler for world events: deselect a vanished selection,
 * coalesced tile refresh + pulse, early center poll when hit. */
function sseConfig(
  map: MapHandle,
  store: AppStore,
): ConstructorParameters<typeof SseClient>[0] {
  return {
    onEvent: (event) => {
      const [x, y, w, h] = event.bbox;
      const bbox: BBox = { x, y, w, h };
      // The selected stratum vanished under someone else.
      if (event.type === "remove" && store.selectedPlace?.id === event.id) {
        releaseSelection(map, store);
      }
      // Coalesced: place AND remove events refresh affected tiles.
      map.current?.queueTileRefresh(bbox);
      map.current?.pulse(bbox);
      if (eventTouchesCenter(event.bbox)) void store.refreshCenter();
    },
    onMalformed: () => {
      // The raw payload is already console.warn'ed by the client.
      store.notifyError("Live update ignored — bad payload");
    },
  };
}

/** The About/GitHub strip under the map. */
function FooterChip(props: { onAbout(): void }) {
  return (
    <div class="footer-chip">
      <button
        class="linkish"
        type="button"
        onClick={() => {
          props.onAbout();
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
  );
}

/** The toggleable side panels. The visibility flips live in App; jump
 * behavior is shared with the map click paths. */
function WorldPanels(props: {
  client: ApiClient;
  map: MapHandle;
  store: AppStore;
  showFeed: boolean;
  showHot: boolean;
  closeFeed(): void;
  closeHot(): void;
}) {
  return (
    <>
      <Show when={props.showFeed}>
        <FeedPanel
          client={props.client}
          onClose={() => {
            props.closeFeed();
          }}
          onJump={(place) => {
            jumpToPlacement(props.map, props.store, place);
          }}
        />
      </Show>
      <Show when={props.showHot}>
        <HotPanel
          client={props.client}
          onClose={() => {
            props.closeHot();
          }}
          onJump={(target) => {
            props.map.current?.centerOn(target.x, target.y, target.zoom);
          }}
        />
      </Show>
    </>
  );
}

export default function App() {
  const client = createApiClient();
  const store = createAppStore(client);

  const map: MapHandle = { current: null, el: undefined };
  let fileInput: HTMLInputElement | undefined;
  const [showAbout, setShowAbout] = createSignal(false);
  const [showFeed, setShowFeed] = createSignal(false);
  const [showHot, setShowHot] = createSignal(false);

  const sse = new SseClient(sseConfig(map, store));

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
      onLongPress: (x, y) => {
        onLongPress(map, store, () => fileInput?.click(), x, y);
      },
    });
    map.current.onViewportChange(() => void bumpViewport(1));
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
        onToggleFeed={() => {
          setShowFeed(!showFeed());
        }}
        onToggleHot={() => {
          setShowHot(!showHot());
        }}
      />

      <WorldPanels
        client={client}
        map={map}
        store={store}
        showFeed={showFeed()}
        showHot={showHot()}
        closeFeed={() => {
          setShowFeed(false);
        }}
        closeHot={() => {
          setShowHot(false);
        }}
      />

      <FooterChip
        onAbout={() => {
          setShowAbout(true);
        }}
      />

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
