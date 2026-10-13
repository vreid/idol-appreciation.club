import { createSignal } from "solid-js";
import type {
  ApiClient,
  CenterInfo,
  DigEntry,
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
import { createToastHub } from "./toasts";
import type {
  AppStore,
  AppStoreDeps,
  PlaceAttempt,
  PlaceMode,
  PreparedImage,
} from "./stateTypes";

export type { Toast, ToastKind } from "./toasts";
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

  const [me, setMe] = createSignal<Me | null>(null);
  const [authChecked, setAuthChecked] = createSignal(false);
  const [center, setCenter] = createSignal<CenterInfo | null>(null);
  const [placeMode, setPlaceMode] = createSignal<PlaceMode>("idle");
  const [prepared, setPrepared] = createSignal<PreparedImage | null>(null);
  const [longEdge, setLongEdgeSignal] = createSignal(DEFAULT_LONG_EDGE);
  const [rotation, setRotationSignal] = createSignal(0);
  const [submittingPlace, setSubmittingPlace] = createSignal(false);
  const [authPrompt, setAuthPrompt] = createSignal(false);
  const [registrationPending, setRegistrationPending] = createSignal(false);
  const [authError, setAuthError] = createSignal("");
  const [adminPending, setAdminPending] = createSignal<PendingAccount[]>([]);
  const [selectedPlace, setSelectedPlace] = createSignal<Placement | null>(
    null,
  );
  const [digStack, setDigStack] = createSignal<DigEntry[]>([]);
  const [digTotal, setDigTotal] = createSignal(0);
  const [digIndex, setDigIndex] = createSignal(-1);
  const toastHub = createToastHub(deps);
  const { push: pushToast, dismiss: dismissToast } = toastHub;

  function requireAuth(): boolean {
    if (me() !== null) return true;
    setAuthPrompt(true);
    pushToast("error", "Please log in first");
    return false;
  }

  function releasePrepared(): void {
    const image = prepared();
    if (image !== null) revokeObjectUrl(image.previewUrl);
    setPrepared(null);
  }

  function doCancelPlace(): void {
    releasePrepared();
    setRotationSignal(0);
    setPlaceMode("idle");
    setSubmittingPlace(false);
  }

  async function doRefreshMe(): Promise<void> {
    try {
      setMe(await client.me());
    } catch {
      setMe(null);
      pushToast("error", "Could not reach the server");
    } finally {
      setAuthChecked(true);
    }
  }

  async function doRefreshCenter(): Promise<void> {
    try {
      setCenter(await client.center());
    } catch {
      /* keep the last known holder; the next poll may succeed */
    }
  }

  async function authenticate(
    action: "login" | "register",
    username: string,
    password: string,
  ): Promise<boolean> {
    setRegistrationPending(false);
    setAuthError("");
    try {
      if (action === "login") {
        await client.login(username, password);
      } else {
        const created = await client.register(username, password);
        if (created.pending) {
          // External approval gate: no session yet, account awaits an admin.
          setRegistrationPending(true);
          pushToast("info", "Account created — awaiting admin approval");
          return false;
        }
      }
    } catch (err) {
      // While authenticating, an error status IS the answer: surface the
      // server's words inline (e.g. 403 "account pending approval").
      const message =
        err instanceof ApiError ? err.message : describeApiError(err);
      setAuthError(message);
      pushToast("error", message);
      return false;
    }
    await doRefreshMe();
    setAuthPrompt(false);
    const who = me()?.username ?? username;
    const verb =
      action === "login" ? "Welcome back" : "Account created, welcome";
    pushToast("success", `${verb}, ${who}!`);
    return true;
  }

  async function loadAdminPending(): Promise<void> {
    try {
      setAdminPending(await client.pendingAccounts());
    } catch (err) {
      pushToast("error", describeApiError(err));
    }
  }

  async function approveAccount(username: string): Promise<void> {
    try {
      await client.approveAccount(username);
      setAdminPending((list) =>
        list.filter((account) => account.username !== username),
      );
      pushToast("success", `Approved ${username}`);
    } catch (err) {
      pushToast("error", describeApiError(err));
    }
  }

  return {
    get me() {
      return me();
    },
    get authChecked() {
      return authChecked();
    },
    get center() {
      return center();
    },
    get toasts() {
      return toastHub.list;
    },
    get placeMode() {
      return placeMode();
    },
    get selectedPlace() {
      return selectedPlace();
    },
    get digStack() {
      return digStack();
    },
    get digTotal() {
      return digTotal();
    },
    get digIndex() {
      return digIndex();
    },
    selectPlace(placement: Placement, point: { x: number; y: number }): void {
      setSelectedPlace(placement);
      setDigStack([]);
      setDigTotal(0);
      setDigIndex(-1);
      // A failed dig degrades silently — the panel works without it.
      void client.dig(point.x, point.y).then((stack) => {
        if (selectedPlace()?.id !== placement.id) return; // superseded
        setDigStack(stack.stack);
        setDigTotal(stack.total);
        setDigIndex(stack.stack.findIndex((e) => e.id === placement.id));
      });
    },
    flipDig(delta: number): void {
      const stack = digStack();
      if (stack.length === 0) return;
      const next = Math.min(Math.max(digIndex() + delta, 0), stack.length - 1);
      if (next === digIndex()) return;
      setDigIndex(next);
      setSelectedPlace(stack[next] ?? null);
    },
    deselectPlace(): void {
      setSelectedPlace(null);
      setDigStack([]);
      setDigTotal(0);
      setDigIndex(-1);
    },
    get prepared() {
      return prepared();
    },
    get longEdge() {
      return longEdge();
    },
    get rotation() {
      return rotation();
    },
    setRotation(value: number): void {
      setRotationSignal(Math.max(-180, Math.min(180, Math.round(value))));
    },
    get submittingPlace() {
      return submittingPlace();
    },
    get authPrompt() {
      return authPrompt();
    },

    dismissToast,

    notify: (message: string) => {
      pushToast("info", message);
    },

    notifyError: (message: string) => {
      pushToast("error", message);
    },

    refreshMe: doRefreshMe,
    refreshCenter: doRefreshCenter,

    login: (username, password) => authenticate("login", username, password),
    register: (username, password) =>
      authenticate("register", username, password),
    get registrationPending() {
      return registrationPending();
    },
    get authError() {
      return authError();
    },
    clearAuthError() {
      setAuthError("");
    },
    get adminPending() {
      return adminPending();
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
      setMe(null);
      pushToast("info", "Logged out");
    },

    promptAuth(): void {
      setAuthPrompt(true);
    },

    dismissAuth(): void {
      setAuthPrompt(false);
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
      setPrepared({
        file,
        fileName,
        previewUrl: createObjectUrl(file),
        naturalW,
        naturalH,
      });
      setLongEdgeSignal(DEFAULT_LONG_EDGE);
      setPlaceMode("panel");
      return true;
    },

    setLongEdge(value: number): void {
      setLongEdgeSignal(clampLongEdge(value));
    },

    ghostSize() {
      const image = prepared();
      if (image === null) return { w: longEdge(), h: longEdge() };
      return longEdgeToSize(image.naturalW, image.naturalH, longEdge());
    },

    enterChoose(): void {
      if (prepared() !== null) setPlaceMode("choose");
    },

    backToPanel(): void {
      if (prepared() !== null) setPlaceMode("panel");
    },

    cancelPlace: doCancelPlace,

    async confirmSpot(x, y): Promise<PlaceAttempt> {
      const image = prepared();
      if (image === null || submittingPlace()) return { ok: false };
      const size = longEdgeToSize(image.naturalW, image.naturalH, longEdge());
      const rot = rotation();
      const spot = clampToWorldPoint({ x, y });
      setSubmittingPlace(true);
      try {
        const placement = await client.place({
          image: image.file,
          fileName: image.fileName,
          x: spot.x,
          y: spot.y,
          size: Math.max(size.w, size.h),
          rot,
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
        setPlaceMode("panel");
        return { ok: false };
      } finally {
        setSubmittingPlace(false);
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
