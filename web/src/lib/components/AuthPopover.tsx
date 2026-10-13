import { Show, createSignal } from "solid-js";
import type { AppStore } from "../state";

/** Login/register popover: tabs + credentials form with inline errors. */
export default function AuthPopover(props: {
  store: AppStore;
  onClose: () => void;
}) {
  const store = props.store;
  const [tab, setTab] = createSignal<"login" | "register">("login");
  const [username, setUsername] = createSignal("");
  const [password, setPassword] = createSignal("");
  const [busy, setBusy] = createSignal(false);

  async function submitAuth(event: SubmitEvent): Promise<void> {
    event.preventDefault();
    if (busy()) return;
    setBusy(true);
    const ok =
      tab() === "login"
        ? await store.login(username().trim(), password())
        : await store.register(username().trim(), password());
    setBusy(false);
    if (ok) {
      props.onClose();
      setPassword("");
    }
  }

  function switchTab(next: "login" | "register"): void {
    setTab(next);
    store.clearAuthError();
  }

  return (
    <div class="auth-pop card">
      <div class="auth-tabs">
        <button
          type="button"
          classList={{ active: tab() === "login" }}
          onClick={() => {
            switchTab("login");
          }}
        >
          Log in
        </button>
        <button
          type="button"
          classList={{ active: tab() === "register" }}
          onClick={() => {
            switchTab("register");
          }}
        >
          Register
        </button>
      </div>
      <form
        class="auth-form"
        onSubmit={(e) => {
          void submitAuth(e);
        }}
      >
        <Show when={store.registrationPending}>
          <p class="auth-note ok" role="status">
            Account created — awaiting admin approval
          </p>
        </Show>
        <Show when={store.authError}>
          <p class="auth-note err" role="alert">
            {store.authError}
          </p>
        </Show>
        <input
          name="username"
          placeholder="username"
          autocomplete="username"
          required
          minlength={1}
          maxlength={32}
          value={username()}
          onInput={(e) => {
            setUsername(e.currentTarget.value);
          }}
        />
        <input
          name="password"
          type="password"
          placeholder="password"
          autocomplete={tab() === "login" ? "current-password" : "new-password"}
          required
          minlength={4}
          maxlength={128}
          value={password()}
          onInput={(e) => {
            setPassword(e.currentTarget.value);
          }}
        />
        <button class="btn primary" type="submit" disabled={busy()}>
          <Show
            when={busy()}
            fallback={tab() === "login" ? "Log in" : "Create account"}
          >
            …
          </Show>
        </button>
      </form>
    </div>
  );
}
