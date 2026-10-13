import { For, Show, type Accessor, createSignal } from "solid-js";
import type { AppStore } from "../state";
import { timeAgo } from "../format";

/** Admin-only pending-accounts popover: toggle button + approval list. */
export default function AdminPopover(props: {
  store: AppStore;
  now: Accessor<number>;
}) {
  const store = props.store;
  const [open, setOpen] = createSignal(false);

  function toggle(): void {
    const next = !open();
    setOpen(next);
    if (next) void store.loadAdminPending();
  }

  return (
    <div class="admin-wrap">
      <button
        class="btn ghost small"
        type="button"
        onClick={toggle}
        title="Accounts awaiting approval"
      >
        admin
        {store.adminPending.length > 0 ? ` (${store.adminPending.length})` : ""}
      </button>
      <Show when={open()}>
        <div class="auth-pop admin-pop card">
          <div class="admin-head">
            <strong>Pending accounts</strong>
            <button
              class="btn ghost small"
              type="button"
              onClick={() => void store.loadAdminPending()}
              title="Refresh"
            >
              ↻
            </button>
          </div>
          <Show
            when={store.adminPending.length > 0}
            fallback={<p class="admin-empty">Nothing pending 🎉</p>}
          >
            <ul class="admin-list">
              <For each={store.adminPending}>
                {(account) => (
                  <li>
                    <span class="admin-name">{account.username}</span>
                    <span class="admin-ago">
                      {timeAgo(account.created_at, props.now())}
                    </span>
                    <button
                      class="btn primary small"
                      type="button"
                      onClick={() =>
                        void store.approveAccount(account.username)
                      }
                    >
                      Approve
                    </button>
                  </li>
                )}
              </For>
            </ul>
          </Show>
        </div>
      </Show>
    </div>
  );
}
