import {
  Show,
  createEffect,
  createMemo,
  createSignal,
  onCleanup,
} from "solid-js";
import type { AppStore } from "../state";
import { cooldownRemainingMs, formatCountdown } from "../cooldown";
import AdminPopover from "./AdminPopover";
import AuthPopover from "./AuthPopover";

export default function TopBar(props: {
  store: AppStore;
  onPickImage: () => void;
  /** Toggles the feed panel (a plain boolean flip in App). */
  onToggleFeed: () => void;
  /** Toggles the hot-regions panel. */
  onToggleHot: () => void;
}) {
  const store = props.store;

  const [now, setNow] = createSignal(Date.now());
  const ticker = setInterval(() => {
    setNow(Date.now());
  }, 1000);

  const [authOpen, setAuthOpen] = createSignal(false);

  // The auth popover pops itself open when an action needs a login.
  createEffect(() => {
    if (store.authPrompt) setAuthOpen(true);
  });

  const cooldownLeft = createMemo(() =>
    cooldownRemainingMs(store.me?.cooldown_until ?? null, now()),
  );

  // Nudge /me once when the cooldown runs out so the fresh state shows up.
  const [hadCooldown, setHadCooldown] = createSignal(false);
  createEffect(() => {
    if (cooldownLeft() > 0) {
      setHadCooldown(true);
    } else if (hadCooldown()) {
      setHadCooldown(false);
      void store.refreshMe();
    }
  });

  onCleanup(() => {
    clearInterval(ticker);
  });

  return (
    <header class="topbar">
      <div class="brand" title="hold the center 0/0">
        <span class="brand-heart" aria-hidden="true">
          ♥
        </span>
        <span class="brand-name">
          idol-appreciation<b>.club</b>
        </span>
      </div>

      <div class="top-actions">
        <button class="btn primary" type="button" onClick={props.onPickImage}>
          📸 Place image
        </button>
        <button
          class="btn ghost small"
          type="button"
          aria-label="Toggle feed"
          onClick={props.onToggleFeed}
        >
          Feed
        </button>
        <button
          class="btn ghost small"
          type="button"
          aria-label="Toggle hot regions"
          onClick={props.onToggleHot}
        >
          Hot
        </button>
        <Show when={store.authChecked && store.me !== null}>
          <span class="me-box">
            <span class="me-name">@{store.me?.username}</span>
            <Show when={cooldownLeft() > 0}>
              <span class="cooldown" title="Place cooldown">
                ⏳ {formatCountdown(cooldownLeft())}
              </span>
            </Show>
            <Show when={store.me?.is_admin === true}>
              <AdminPopover store={store} now={now} />
            </Show>
            <button
              class="btn ghost small"
              type="button"
              onClick={() => void store.logout()}
            >
              Log out
            </button>
          </span>
        </Show>
        <Show when={store.authChecked && store.me === null}>
          <div class="auth-wrap">
            <button
              class="btn ghost"
              type="button"
              onClick={() => {
                setAuthOpen(!authOpen());
              }}
            >
              Log in
            </button>
            <Show when={authOpen()}>
              <AuthPopover
                store={store}
                onClose={() => {
                  setAuthOpen(false);
                }}
              />
            </Show>
          </div>
        </Show>
      </div>
    </header>
  );
}
