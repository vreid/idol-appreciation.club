import { For, Show, createSignal, onMount } from "solid-js";
import type { ApiClient, FeedEntry, Placement } from "../api";
import { timeAgo } from "../format";

const PAGE_LIMIT = 20;

/** Strips the feed-only `seq` cursor field: the rest of the app speaks
 * Placement (the contract schemas strip unknown keys, so the shapes
 * match exactly). */
export function toPlacement(entry: FeedEntry): Placement {
  return {
    id: entry.id,
    username: entry.username,
    x: entry.x,
    y: entry.y,
    w: entry.w,
    h: entry.h,
    rot: entry.rot,
    created_at: entry.created_at,
    tags: entry.tags,
  };
}

/**
 * The feed: newest placements, optionally filtered to one tag, paged
 * oldest-ward with the `next_before` keyset cursor. Clicking an entry
 * jumps the map there and selects it (the anchored panel + dig depth
 * cue take over from there).
 */
export default function FeedPanel(props: {
  client: ApiClient;
  onClose(): void;
  /** Map pan + store selection for a clicked entry. */
  onJump(place: Placement): void;
}) {
  const [entries, setEntries] = createSignal<FeedEntry[]>([]);
  const [cursor, setCursor] = createSignal<number | null>(null);
  const [tag, setTag] = createSignal("");
  const [error, setError] = createSignal("");
  const [loading, setLoading] = createSignal(false);

  async function load(opts: {
    append: boolean;
    before?: number;
  }): Promise<void> {
    setLoading(true);
    setError("");
    try {
      const filterTag = tag().trim().toLowerCase();
      const page = await props.client.feed({
        ...(filterTag === "" ? {} : { tag: filterTag }),
        ...(opts.before === undefined ? {} : { before: opts.before }),
        limit: PAGE_LIMIT,
      });
      setEntries(opts.append ? [...entries(), ...page.items] : page.items);
      setCursor(page.next_before);
    } catch {
      setError("Could not load the feed");
    } finally {
      setLoading(false);
    }
  }

  onMount(() => {
    void load({ append: false });
  });

  return (
    <section class="feed-panel card" aria-label="Feed">
      <div class="pd-head">
        <strong class="feed-title">Feed</strong>
        <button
          class="pd-close"
          type="button"
          aria-label="Close feed"
          onClick={() => {
            props.onClose();
          }}
        >
          ✕
        </button>
      </div>
      <form
        class="feed-filter"
        onSubmit={(event) => {
          event.preventDefault();
          void load({ append: false });
        }}
      >
        <input
          aria-label="Filter by tag"
          placeholder="filter by tag"
          maxLength={32}
          value={tag()}
          onInput={(event) => {
            setTag(event.currentTarget.value);
          }}
        />
      </form>
      <Show when={entries().length > 0}>
        <ul class="feed-list">
          <For each={entries()}>
            {(entry) => (
              <li>
                <button
                  class="feed-item"
                  type="button"
                  title="Jump to this placement"
                  onClick={() => {
                    props.onJump(toPlacement(entry));
                  }}
                >
                  <span class="feed-user">@{entry.username}</span>
                  <span class="feed-meta">
                    {entry.w}×{entry.h} px · {timeAgo(entry.created_at)}
                  </span>
                  <Show when={entry.tags.length > 0}>
                    <span class="feed-tags">{entry.tags.join(" · ")}</span>
                  </Show>
                </button>
              </li>
            )}
          </For>
        </ul>
      </Show>
      <Show when={entries().length === 0 && !loading() && error() === ""}>
        <p class="feed-empty">
          Nothing here yet{tag() === "" ? "" : ` for #${tag().trim()}`}.
        </p>
      </Show>
      <Show when={error() !== ""}>
        <p class="feed-error">{error()}</p>
      </Show>
      <Show when={cursor() !== null}>
        <button
          class="btn ghost small feed-older"
          type="button"
          disabled={loading()}
          onClick={() => {
            const before = cursor();
            if (before !== null) void load({ append: true, before });
          }}
        >
          {loading() ? "Loading…" : "Older"}
        </button>
      </Show>
    </section>
  );
}
