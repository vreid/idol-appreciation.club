<script lang="ts">
  import { onDestroy } from "svelte";
  import type { AppStore } from "../state.svelte";
  import { cooldownRemainingMs, formatCountdown } from "../cooldown";
  import { formatDuration, timeAgo } from "../format";

  let {
    store,
    onOpenLeaderboard,
    onPickImage,
  }: {
    store: AppStore;
    onOpenLeaderboard: () => void;
    onPickImage: () => void;
  } = $props();

  let now = $state(Date.now());
  const ticker = setInterval(() => (now = Date.now()), 1000);
  onDestroy(() => clearInterval(ticker));

  let authOpen = $state(false);
  let authTab = $state<"login" | "register">("login");
  let username = $state("");
  let password = $state("");
  let busy = $state(false);
  let adminOpen = $state(false);

  // The auth popover pops itself open when an action needs a login.
  $effect(() => {
    if (store.authPrompt) authOpen = true;
  });

  function toggleAdmin(): void {
    adminOpen = !adminOpen;
    if (adminOpen) void store.loadAdminPending();
  }

  const heldSeconds = $derived.by(() => {
    const since = store.center?.current?.since;
    if (since === undefined) return 0;
    return Math.max(0, Math.floor((now - Date.parse(since)) / 1000));
  });

  const cooldownLeft = $derived(
    cooldownRemainingMs(store.me?.cooldown_until ?? null, now),
  );

  // Nudge /me once when the cooldown runs out so the fresh state shows up.
  let hadCooldown = $state(false);
  $effect(() => {
    if (cooldownLeft > 0) {
      hadCooldown = true;
    } else if (hadCooldown) {
      hadCooldown = false;
      void store.refreshMe();
    }
  });

  async function submitAuth(event: SubmitEvent): Promise<void> {
    event.preventDefault();
    if (busy) return;
    busy = true;
    const ok =
      authTab === "login"
        ? await store.login(username.trim(), password)
        : await store.register(username.trim(), password);
    busy = false;
    if (ok) {
      authOpen = false;
      password = "";
    }
  }

  function toggleAuth(): void {
    authOpen = !authOpen;
  }

  function switchAuthTab(tab: "login" | "register"): void {
    authTab = tab;
    store.clearAuthError();
  }
</script>

<header class="topbar">
  <div class="brand" title="hold the center 0/0">
    <span class="brand-heart" aria-hidden="true">♥</span>
    <span class="brand-name">idol-appreciation<b>.club</b></span>
  </div>

  <button
    class="chip center-chip"
    type="button"
    onclick={onOpenLeaderboard}
    title="Who holds 0/0 — click for the leaderboard"
  >
    {#if store.center?.current}
      <span class="chip-crown" aria-hidden="true">👑</span>
      <span class="chip-name">{store.center.current.username}</span>
      <span class="chip-time">{formatDuration(heldSeconds)}</span>
    {:else}
      <span class="chip-crown" aria-hidden="true">👑</span>
      <span class="chip-name muted">0/0 unclaimed</span>
    {/if}
  </button>

  <div class="top-actions">
    <button class="btn primary" type="button" onclick={onPickImage}>
      📸 Place image
    </button>
    <button class="btn ghost" type="button" onclick={onOpenLeaderboard}>
      🏆 Leaderboard
    </button>

    {#if store.authChecked && store.me}
      <span class="me-box">
        <span class="me-name">@{store.me.username}</span>
        {#if cooldownLeft > 0}
          <span class="cooldown" title="Place cooldown">
            ⏳ {formatCountdown(cooldownLeft)}
          </span>
        {/if}
        {#if store.me.is_admin}
          <div class="admin-wrap">
            <button
              class="btn ghost small"
              type="button"
              onclick={toggleAdmin}
              title="Accounts awaiting approval"
            >
              admin{store.adminPending.length > 0
                ? ` (${store.adminPending.length})`
                : ""}
            </button>
            {#if adminOpen}
              <div class="auth-pop admin-pop">
                <div class="admin-head">
                  <strong>Pending accounts</strong>
                  <button
                    class="btn ghost small"
                    type="button"
                    onclick={() => void store.loadAdminPending()}
                    title="Refresh"
                  >
                    ↻
                  </button>
                </div>
                {#if store.adminPending.length === 0}
                  <p class="admin-empty">Nothing pending 🎉</p>
                {:else}
                  <ul class="admin-list">
                    {#each store.adminPending as account (account.username)}
                      <li>
                        <span class="admin-name">{account.username}</span>
                        <span class="admin-ago">
                          {timeAgo(account.created_at, now)}
                        </span>
                        <button
                          class="btn primary small"
                          type="button"
                          onclick={() =>
                            void store.approveAccount(account.username)}
                        >
                          Approve
                        </button>
                      </li>
                    {/each}
                  </ul>
                {/if}
              </div>
            {/if}
          </div>
        {/if}
        <button
          class="btn ghost small"
          type="button"
          onclick={() => store.logout()}
        >
          Log out
        </button>
      </span>
    {:else if store.authChecked}
      <div class="auth-wrap">
        <button class="btn ghost" type="button" onclick={toggleAuth}>
          Log in
        </button>
        {#if authOpen}
          <div class="auth-pop">
            <div class="auth-tabs">
              <button
                type="button"
                class:active={authTab === "login"}
                onclick={() => switchAuthTab("login")}>Log in</button
              >
              <button
                type="button"
                class:active={authTab === "register"}
                onclick={() => switchAuthTab("register")}>Register</button
              >
            </div>
            <form class="auth-form" onsubmit={submitAuth}>
              {#if store.registrationPending}
                <p class="auth-note ok" role="status">
                  Account created — awaiting admin approval
                </p>
              {/if}
              {#if store.authError}
                <p class="auth-note err" role="alert">{store.authError}</p>
              {/if}
              <input
                name="username"
                placeholder="username"
                autocomplete="username"
                required
                minlength="1"
                maxlength="32"
                bind:value={username}
              />
              <input
                name="password"
                type="password"
                placeholder="password"
                autocomplete={authTab === "login"
                  ? "current-password"
                  : "new-password"}
                required
                minlength="4"
                maxlength="128"
                bind:value={password}
              />
              <button class="btn primary" type="submit" disabled={busy}>
                {#if busy}
                  …
                {:else if authTab === "login"}
                  Log in
                {:else}
                  Create account
                {/if}
              </button>
            </form>
          </div>
        {/if}
      </div>
    {/if}
  </div>
</header>
