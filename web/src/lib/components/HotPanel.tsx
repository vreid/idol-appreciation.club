import { For, Show, createSignal, onMount } from "solid-js";
import type { ApiClient, HotCell } from "../api";
import { WORLD_SIZE } from "../world";

/** The /api/hot grid: z6 tile coordinates (64x64 cells). */
export const HOT_CELLS = 64;
export const HOT_CELL_PX = WORLD_SIZE / HOT_CELLS;

/** Native world coords of a cell's centre + a zoom where the cell fills
 * a comfortable slice of the viewport (z8 = 4x4 tiles per cell). */
export function cellCenter(cell: HotCell): {
  x: number;
  y: number;
  zoom: number;
} {
  return {
    x: (cell.x + 0.5) * HOT_CELL_PX,
    y: (cell.y + 0.5) * HOT_CELL_PX,
    zoom: 8,
  };
}

/**
 * The lighter v1 of hot spots: a top-regions list. Heat-overlay rendering
 * (a canvas layer tinted by score) is the heavier alternative and stays a
 * possible follow-up.
 */
export default function HotPanel(props: {
  client: ApiClient;
  onClose(): void;
  /** Map pan+zoom for a clicked cell (centered coords + zoom). */
  onJump(target: { x: number; y: number; zoom: number }): void;
}) {
  const [cells, setCells] = createSignal<HotCell[]>([]);
  const [total, setTotal] = createSignal(0);
  const [windowHours, setWindowHours] = createSignal(24);
  const [error, setError] = createSignal("");
  const [loading, setLoading] = createSignal(false);

  onMount(() => {
    setLoading(true);
    props.client
      .hot({ window: 24, limit: 8 })
      .then((page) => {
        setCells(page.cells);
        setTotal(page.total);
        setWindowHours(page.window_hours);
      })
      .catch(() => setError("Could not load hot regions"))
      .finally(() => {
        setLoading(false);
      });
  });

  return (
    <section class="feed-panel hot-panel card" aria-label="Hot regions">
      <div class="pd-head">
        <strong class="feed-title">Hot</strong>
        <button
          class="pd-close"
          type="button"
          aria-label="Close hot regions"
          onClick={() => {
            props.onClose();
          }}
        >
          ✕
        </button>
      </div>
      <Show when={cells().length > 0}>
        <p class="hot-sub">
          busiest regions, last {windowHours()}h · {total()} placements
        </p>
        <ul class="feed-list">
          <For each={cells()}>
            {(cell) => (
              <li>
                <button
                  class="feed-item"
                  type="button"
                  title="Jump to this region"
                  onClick={() => {
                    props.onJump(cellCenter(cell));
                  }}
                >
                  <span class="hot-rank-score">
                    <span class="feed-user">
                      {cell.x}/{cell.y}
                    </span>
                    <span class="hot-score">{cell.score.toFixed(2)}</span>
                  </span>
                  <span class="feed-meta">
                    {cell.count} placement{cell.count === 1 ? "" : "s"}
                  </span>
                </button>
              </li>
            )}
          </For>
        </ul>
      </Show>
      <Show when={cells().length === 0 && !loading() && error() === ""}>
        <p class="feed-empty">Nothing hot in the last {windowHours()}h.</p>
      </Show>
      <Show when={error() !== ""}>
        <p class="feed-error">{error()}</p>
      </Show>
    </section>
  );
}
