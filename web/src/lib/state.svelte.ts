/**
 * Application state (Svelte 5 runes). All logic here is framework-light and
 * covered by state.test.ts; Svelte components only render and delegate.
 */

import type {
  ApiClient,
  CenterInfo,
  Me,
  PendingAccount,
  Placement,
} from "./api";
import { ApiError, MAX_UPLOAD_BYTES } from "./api";
import { describeApiError, isAuthStatus } from "./apiErrors";
import {
  clampLongEdge,
  DEFAULT_LONG_EDGE,
  isUploadSizeOk,
  longEdgeToSize,
} from "./sizing";
import { clampToWorldPoint } from "./world";
import { createToastHub } from "./toasts.svelte";
import type {
  AppStore,
  AppStoreDeps,
  PlaceAttempt,
  PlaceMode,
  PreparedImage,
} from "./stateTypes";

export type { Toast, ToastKind } from "./toasts.svelte";
export type {
  AppStore,
  AppStoreDeps,
  PlaceAttempt,
  PlaceMode,
  PreparedImage,
} from "./stateTypes";

export function createAppStore(
  client: ApiClient,
  deps: AppStoreDeps = {},
): AppStore {
  const createObjectUrl =
    deps.createObjectUrl ?? ((blob: Blob) => URL.createObjectURL(blob));
  const revokeObjectUrl =
    deps.revokeObjectUrl ??
    ((url: string) => {
      URL.revokeObjectURL(url);
    });

  let me = $state<Me | null>(null);
  let authChecked = $state(false);
  let center = $state<CenterInfo | null>(null);
  let placeMode = $state<PlaceMode>("idle");
  let prepared = $state<PreparedImage | null>(null);
  let longEdge = $state(DEFAULT_LONG_EDGE);
  let submittingPlace = $state(false);
  let authPrompt = $state(false);

  const toastHub = createToastHub(deps);
  const { push: pushToast, dismiss: dismissToast } = toastHub;

  function requireAuth(): boolean {
    if (me !== null) return true;
    authPrompt = true;
    pushToast("error", "Please log in first");
    return false;
  }

  function releasePrepared(): void {
    if (prepared !== null) revokeObjectUrl(prepared.previewUrl);
    prepared = null;
  }

  function doCancelPlace(): void {
    releasePrepared();
    placeMode = "idle";
    submittingPlace = false;
  }

  async function doRefreshMe(): Promise<void> {
    try {
      me = await client.me();
    } catch {
      me = null;
      pushToast("error", "Could not reach the server");
    } finally {
      authChecked = true;
    }
  }

  async function doRefreshCenter(): Promise<void> {
    try {
      center = await client.center();
    } catch {
      /* keep the last known holder; the next poll may succeed */
    }
  }

  let registrationPending = $state(false);
  let authError = $state("");
  let adminPending = $state<PendingAccount[]>([]);

  async function authenticate(
    action: "login" | "register",
    username: string,
    password: string,
  ): Promise<boolean> {
    registrationPending = false;
    authError = "";
    try {
      if (action === "login") {
        await client.login(username, password);
      } else {
        const created = await client.register(username, password);
        if (created.pending) {
          // External approval gate: no session yet, account awaits an admin.
          registrationPending = true;
          pushToast("info", "Account created — awaiting admin approval");
          return false;
        }
      }
    } catch (err) {
      // While authenticating, an error status IS the answer: surface the
      // server's words inline (e.g. 403 "account pending approval").
      authError = err instanceof ApiError ? err.message : describeApiError(err);
      pushToast("error", authError);
      return false;
    }
    await doRefreshMe();
    authPrompt = false;
    const who = me?.username ?? username;
    const verb =
      action === "login" ? "Welcome back" : "Account created, welcome";
    pushToast("success", `${verb}, ${who}!`);
    return true;
  }

  async function loadAdminPending(): Promise<void> {
    try {
      adminPending = await client.pendingAccounts();
    } catch (err) {
      pushToast("error", describeApiError(err));
    }
  }

  async function approveAccount(username: string): Promise<void> {
    try {
      await client.approveAccount(username);
      adminPending = adminPending.filter((a) => a.username !== username);
      pushToast("success", `Approved ${username}`);
    } catch (err) {
      pushToast("error", describeApiError(err));
    }
  }

  return {
    get me() {
      return me;
    },
    get authChecked() {
      return authChecked;
    },
    get center() {
      return center;
    },
    get toasts() {
      return toastHub.list;
    },
    get placeMode() {
      return placeMode;
    },
    get prepared() {
      return prepared;
    },
    get longEdge() {
      return longEdge;
    },
    get submittingPlace() {
      return submittingPlace;
    },
    get authPrompt() {
      return authPrompt;
    },

    dismissToast,

    notify: (message: string) => {
      pushToast("info", message);
    },

    refreshMe: doRefreshMe,
    refreshCenter: doRefreshCenter,

    login: (username, password) => authenticate("login", username, password),
    register: (username, password) =>
      authenticate("register", username, password),
    get registrationPending() {
      return registrationPending;
    },
    get authError() {
      return authError;
    },
    clearAuthError() {
      authError = "";
    },
    get adminPending() {
      return adminPending;
    },
    loadAdminPending,
    approveAccount,

    async logout(): Promise<void> {
      try {
        await client.logout();
      } catch (err) {
        pushToast("error", describeApiError(err));
        return;
      }
      me = null;
      pushToast("info", "Logged out");
    },

    promptAuth(): void {
      authPrompt = true;
    },

    dismissAuth(): void {
      authPrompt = false;
    },

    startPlace(file, fileName, naturalW, naturalH): boolean {
      if (!isUploadSizeOk(file.size)) {
        pushToast(
          "error",
          `Image too large: max ${Math.floor(MAX_UPLOAD_BYTES / 1024 / 1024)}MB`,
        );
        return false;
      }
      releasePrepared();
      prepared = {
        file,
        fileName,
        previewUrl: createObjectUrl(file),
        naturalW,
        naturalH,
      };
      longEdge = DEFAULT_LONG_EDGE;
      placeMode = "panel";
      return true;
    },

    setLongEdge(value: number): void {
      longEdge = clampLongEdge(value);
    },

    ghostSize() {
      if (prepared === null) return { w: longEdge, h: longEdge };
      return longEdgeToSize(prepared.naturalW, prepared.naturalH, longEdge);
    },

    enterChoose(): void {
      if (prepared !== null) placeMode = "choose";
    },

    backToPanel(): void {
      if (prepared !== null) placeMode = "panel";
    },

    cancelPlace: doCancelPlace,

    async confirmSpot(x, y): Promise<PlaceAttempt> {
      const image = prepared;
      if (image === null || submittingPlace) return { ok: false };
      const size = longEdgeToSize(image.naturalW, image.naturalH, longEdge);
      const spot = clampToWorldPoint({ x, y });
      submittingPlace = true;
      try {
        const placement = await client.place({
          image: image.file,
          fileName: image.fileName,
          x: spot.x,
          y: spot.y,
          size: Math.max(size.w, size.h),
        });
        doCancelPlace();
        pushToast("success", "Image placed!");
        return { ok: true, placement };
      } catch (err) {
        if (err instanceof ApiError && err.status === 429) {
          const wait =
            err.retryAfterSeconds === null
              ? ""
              : ` Try again in ${err.retryAfterSeconds}s.`;
          pushToast("error", `Cooldown active.${wait}`);
        } else if (isAuthStatus(err)) {
          requireAuth();
        } else {
          pushToast("error", describeApiError(err));
        }
        placeMode = "panel";
        return { ok: false };
      } finally {
        submittingPlace = false;
      }
    },

    async refreshNear(x, y, r): Promise<Placement[]> {
      try {
        return await client.near(x, y, r);
      } catch {
        pushToast("error", "Could not load placements here");
        return [];
      }
    },

    async reportPlacement(placementId): Promise<void> {
      if (!requireAuth()) return;
      try {
        await client.report(placementId);
        pushToast("info", "Reported to the mods, thank you");
      } catch (err) {
        if (isAuthStatus(err)) requireAuth();
        else pushToast("error", describeApiError(err));
      }
    },

    async removePlacement(placementId): Promise<boolean> {
      try {
        await client.removePlacement(placementId);
        pushToast("info", "Placement removed");
        return true;
      } catch (err) {
        pushToast("error", describeApiError(err));
        return false;
      }
    },
  };
}
