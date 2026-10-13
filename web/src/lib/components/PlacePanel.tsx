import { Show } from "solid-js";
import type { AppStore } from "../state";

export default function PlacePanel(props: { store: AppStore }) {
  const store = props.store;

  return (
    <Show when={store.placeMode !== "idle" && store.prepared !== null}>
      <aside class="place-panel card">
        <div class="panel-head">
          <h3>Place an image</h3>
          <button
            class="btn ghost small"
            type="button"
            onClick={() => {
              store.cancelPlace();
            }}
          >
            ✕
          </button>
        </div>

        <Show when={store.placeMode === "panel"}>
          <img
            class="place-preview"
            src={store.prepared?.previewUrl}
            alt={`preview of ${store.prepared?.fileName}`}
          />
          <label class="place-slider">
            <span>
              Long edge <b>{store.longEdge}px</b> → {store.ghostSize().w}×
              {store.ghostSize().h}px
            </span>
            <input
              type="range"
              min={16}
              max={4096}
              step={1}
              value={store.longEdge}
              onInput={(e) => {
                store.setLongEdge(e.currentTarget.valueAsNumber);
              }}
            />
          </label>
          <label class="place-slider">
            <span>
              Rotation <b>{store.rotation}°</b>
            </span>
            <input
              type="range"
              min={-180}
              max={180}
              step={1}
              value={store.rotation}
              onInput={(e) => {
                store.setRotation(e.currentTarget.valueAsNumber);
              }}
            />
          </label>
          <Show
            when={store.pendingSpot}
            fallback={
              <button
                class="btn primary wide"
                type="button"
                onClick={() => {
                  store.enterChoose();
                }}
              >
                🎯 Choose spot
              </button>
            }
          >
            <button
              class="btn primary wide"
              type="button"
              onClick={() => {
                void store.confirmPinned();
              }}
            >
              📍 Place at pinned spot
            </button>
          </Show>
        </Show>

        <Show when={store.placeMode === "choose"}>
          <img
            class="place-preview choose"
            src={store.prepared?.previewUrl}
            alt={`preview of ${store.prepared?.fileName}`}
          />
          <p class="place-hint">
            Tap or click the map — the ghost is centered where you point.
          </p>
          <div class="panel-row">
            <button
              class="btn ghost wide"
              type="button"
              onClick={() => {
                store.backToPanel();
              }}
            >
              ← Adjust size
            </button>
            <Show when={store.submittingPlace}>
              <button class="btn primary wide" type="button" disabled>
                Placing…
              </button>
            </Show>
          </div>
        </Show>
      </aside>
    </Show>
  );
}
