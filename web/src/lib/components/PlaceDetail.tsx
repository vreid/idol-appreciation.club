import { Show, createEffect, createSignal, onCleanup } from "solid-js";
import { panelLayout, type ScreenRect } from "../panelLayout";
import type { AppStore } from "../state";
import type { Placement } from "../api";

export interface PlaceDetailActions {
  isLoggedIn(): boolean;
  onReport(id: string): void;
  isAdmin(): boolean;
  onRemove(
    id: string,
    bbox: { x: number; y: number; w: number; h: number },
  ): void;
  onAuthPrompt(): void;
  /** Store deselect + map highlight clear, in one call. */
  onDeselect(): void;
}

/** Anchor geometry for the current selection, read from the live map. */
export interface SelectionGeometry {
  aabb: ScreenRect;
  viewport: { w: number; h: number };
}

interface Position {
  left: number;
  top: number;
  side: "right" | "left";
}

/**
 * The anchored detail panel for a selected placement: a Solid-rendered
 * card docked BESIDE the selection's rotated AABB (right side preferred,
 * flipping to the left near the right viewport edge, clamped top and
 * bottom), repositioned on every map pan/zoom. Replaces the old Leaflet
 * popup, which rendered on top of the image it described.
 */
export default function PlaceDetail(props: {
  store: AppStore;
  container: HTMLElement | undefined;
  /** Reactive counter bumped on every map pan/zoom. */
  viewportVersion: () => number;
  /** Non-reactive read of the selection's screen geometry. */
  geometry: (placement: Placement) => SelectionGeometry;
  actions: PlaceDetailActions;
}) {
  const store = props.store;
  const [position, setPosition] = createSignal<Position>({
    left: 0,
    top: 0,
    side: "right",
  });
  let panel: HTMLDivElement | undefined;

  createEffect(() => {
    const placement = store.selectedPlace;
    const container = props.container;
    if (placement === null || container === undefined) return;
    // Reactive dependency on pan/zoom even though the value is unused.
    void props.viewportVersion();
    const { aabb, viewport } = props.geometry(placement);
    const size = {
      w: panel?.offsetWidth ?? 220,
      h: panel?.offsetHeight ?? 90,
    };
    setPosition(panelLayout(aabb, size, viewport));
  });

  // Escape closes the panel (deselect) — panel-scoped UX, so the
  // listener lives here alongside the rest of the panel's behavior.
  createEffect(() => {
    if (store.selectedPlace === null) return;
    // Closes over props.actions — the scoping heuristic can't see it.
    // eslint-disable-next-line unicorn/consistent-function-scoping
    const onKey = (event: KeyboardEvent) => {
      if (event.key === "Escape") props.actions.onDeselect();
    };
    window.addEventListener("keydown", onKey);
    onCleanup(() => {
      window.removeEventListener("keydown", onKey);
    });
  });

  return (
    <Show when={store.selectedPlace} keyed>
      {(placement) => (
        <div
          ref={panel}
          class={`place-detail card side-${position().side}`}
          style={{
            left: `${position().left}px`,
            top: `${position().top}px`,
          }}
        >
          <div class="pd-head">
            <strong class="pd-user">@{placement.username}</strong>
            <button
              class="pd-close"
              type="button"
              aria-label="Close details"
              onClick={() => {
                props.actions.onDeselect();
              }}
            >
              ✕
            </button>
          </div>
          <div class="pd-meta">
            {placement.w}×{placement.h} px · {placement.rot}°
          </div>
          <div class="pd-actions">
            <button
              class="pd-btn"
              type="button"
              onClick={() => {
                if (props.actions.isLoggedIn()) {
                  props.actions.onReport(placement.id);
                  props.actions.onDeselect();
                } else {
                  props.actions.onAuthPrompt();
                }
              }}
            >
              Report
            </button>
            <Show when={props.actions.isAdmin()}>
              <button
                class="pd-btn pd-danger"
                type="button"
                onClick={() => {
                  props.actions.onRemove(placement.id, {
                    x: placement.x,
                    y: placement.y,
                    w: placement.w,
                    h: placement.h,
                  });
                }}
              >
                Remove
              </button>
            </Show>
          </div>
        </div>
      )}
    </Show>
  );
}
