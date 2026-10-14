import { For, Show, createEffect, createSignal, onCleanup } from "solid-js";
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
  /** Replaces a placement's tag set via the store (toasts on failure). */
  onUpdateTags(placementId: string, tags: string[]): Promise<void>;
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

  // Non-keyed on purpose: the panel must survive the selection changing
  // underneath it (wheel flips read through the reactive store getters).
  return (
    <Show when={store.selectedPlace}>
      <div
        ref={panel}
        class={`place-detail card side-${position().side}`}
        style={{
          left: `${position().left}px`,
          top: `${position().top}px`,
        }}
      >
        <div class="pd-head">
          <strong class="pd-user">@{store.selectedPlace?.username}</strong>
          <Show when={store.digTotal > 1 && store.digIndex >= 0}>
            <span class="pd-depth" title="Wheel flips through the pile">
              {store.digIndex + 1} of {store.digTotal}
            </span>
          </Show>
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
          {store.selectedPlace?.w}×{store.selectedPlace?.h} px ·{" "}
          {store.selectedPlace?.rot}°
        </div>
        <TagsRow store={store} actions={props.actions} />
        <div class="pd-actions">
          <button
            class="pd-btn"
            type="button"
            onClick={() => {
              if (props.actions.isLoggedIn()) {
                props.actions.onReport(store.selectedPlace?.id ?? "");
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
                const sel = store.selectedPlace;
                if (sel === null) return;
                props.actions.onRemove(sel.id, {
                  x: sel.x,
                  y: sel.y,
                  w: sel.w,
                  h: sel.h,
                });
              }}
            >
              Remove
            </button>
          </Show>
        </div>
      </div>
    </Show>
  );
}

/** Normalized label chips for the selection. The owner or an admin can
 * add chips (one input, Enter or the button commits) and remove them;
 * the backend is the authority on the 1..=32 chars / max 8 rules and
 * answers with the authoritative list. */
function TagsRow(props: { store: AppStore; actions: PlaceDetailActions }) {
  const [draft, setDraft] = createSignal("");
  const tags = () => props.store.selectedPlace?.tags ?? [];
  const canEdit = () => {
    const me = props.store.me;
    const placement = props.store.selectedPlace;
    if (placement === null) return false;
    return me?.username === placement.username || props.actions.isAdmin();
  };

  function commit(): void {
    const tag = draft().trim();
    setDraft("");
    if (tag.length === 0) return;
    const id = props.store.selectedPlace?.id;
    if (id === undefined) return;
    void props.actions.onUpdateTags(id, [...tags(), tag]);
  }

  function removeAt(index: number): void {
    const id = props.store.selectedPlace?.id;
    if (id === undefined) return;
    void props.actions.onUpdateTags(
      id,
      tags().filter((_, i) => i !== index),
    );
  }

  return (
    <Show when={tags().length > 0 || canEdit()}>
      <div class="pd-tags" role="list" aria-label="Tags">
        <For each={tags()}>
          {(tag, index) => (
            <span class="pd-tag" role="listitem">
              {tag}
              <Show when={canEdit()}>
                <button
                  class="pd-tag-x"
                  type="button"
                  aria-label={`Remove tag ${tag}`}
                  onClick={() => {
                    removeAt(index());
                  }}
                >
                  ✕
                </button>
              </Show>
            </span>
          )}
        </For>
        <Show when={canEdit()}>
          <form
            class="pd-tag-add"
            onSubmit={(event) => {
              event.preventDefault();
              commit();
            }}
          >
            <input
              aria-label="Add tag"
              placeholder="add tag"
              maxLength={32}
              value={draft()}
              onInput={(event) => {
                setDraft(event.currentTarget.value);
              }}
            />
            <button class="pd-btn" type="submit">
              Add
            </button>
          </form>
        </Show>
      </div>
    </Show>
  );
}
