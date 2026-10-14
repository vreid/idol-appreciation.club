import { createSignal } from "solid-js";
import type {
  ApiClient,
  CenterInfo,
  Me,
  PendingAccount,
  Placement,
} from "./api";
import { ApiError, MAX_UPLOAD_BYTES } from "./api";
import { describeApiError, isAuthStatus } from "./apiErrors";
import { createPileState } from "./pile";
import { confirmAt, type PlaceConfirmDeps } from "./placeConfirm";
import {
  clampLongEdge,
  DEFAULT_LONG_EDGE,
  isUploadSizeOk,
  longEdgeToSize,
} from "./sizing";
import { clampToWorldPoint } from "./world";
import { createToastHub } from "./toasts";
import { createTagsEditor } from "./tagsEdit";
import { createSessionRefresher } from "./session";
import { createAdminAccounts } from "./adminAccounts";
import type {
  AppStore,
  AppStoreDeps,
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

/** A world-px point (the long-press pin). */
type Spot = { x: number; y: number };

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
  const [pendingSpot, setPendingSpot] = createSignal<Spot | null>(null);
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
  const pile = createPileState({
    client,
    selected: selectedPlace,
    setSelected: (p) => void setSelectedPlace(p),
  });
  const toastHub = createToastHub(deps);
  const { push: pushToast, dismiss: dismissToast } = toastHub;

  // Tag edits keep the open panel consistent with the authoritative list.
  const tagsEditor = createTagsEditor({
    client,
    requireAuth,
    toast: pushToast,
    selected: selectedPlace,
    // Signal setters return the new value; this slot is void.
    setSelected: (p) => void setSelectedPlace(p),
  });

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

  const placeDeps = (): PlaceConfirmDeps => ({
    prepared: () => prepared(),
    submitting: () => submittingPlace(),
    longEdge: () => longEdge(),
    rot: () => rotation(),
    place: client.place,
    pushToast,
    requireAuth: () => void requireAuth(),
    onPlaced: doCancelPlace,
    onFailed: () => void setPlaceMode("panel"),
    onSubmitting: () => void setSubmittingPlace(true),
    onSettled: () => void setSubmittingPlace(false),
  });

  function doCancelPlace(): void {
    releasePrepared();
    setRotationSignal(0);
    setPlaceMode("idle");
    setSubmittingPlace(false);
    setPendingSpot(null);
  }

  const session = createSessionRefresher({
    client,
    setMe: (next) => void setMe(next),
    setAuthChecked: (checked) => void setAuthChecked(checked),
    setCenter: (c) => void setCenter(c),
    onError: (message) => {
      pushToast("error", message);
    },
  });

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
    await session.refreshMe();
    setAuthPrompt(false);
    const who = me()?.username ?? username;
    const verb =
      action === "login" ? "Welcome back" : "Account created, welcome";
    pushToast("success", `${verb}, ${who}!`);
    return true;
  }

  const adminAccounts = createAdminAccounts({
    client,
    toast: pushToast,
    setPending: (list) => void setAdminPending(list),
    removePending: (username) => {
      setAdminPending((list) =>
        list.filter((account) => account.username !== username),
      );
    },
  });

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
      return pile.stack();
    },
    get digTotal() {
      return pile.total();
    },
    get digIndex() {
      return pile.index();
    },
    selectPlace(placement: Placement, point?: { x: number; y: number }): void {
      setSelectedPlace(placement);
      if (point === undefined) {
        pile.reset();
        return;
      }
      pile.load(placement, point);
    },
    queryPile(x: number, y: number): Promise<boolean> {
      return pile.query(x, y);
    },
    adoptPile(top: Placement): void {
      pile.adopt(top);
    },
    stepDig(delta: number): boolean {
      return pile.step(delta);
    },
    deselectPlace(): void {
      setSelectedPlace(null);
      pile.reset();
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

    refreshMe: () => session.refreshMe(),
    refreshCenter: () => session.refreshCenter(),

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
    loadAdminPending: () => adminAccounts.loadPending(),
    approveAccount: (username) => adminAccounts.approve(username),

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

    pinSpot(x: number, y: number): void {
      setPendingSpot(clampToWorldPoint({ x, y }));
    },

    get pendingSpot() {
      return pendingSpot();
    },

    confirmPinned: async () => {
      const spot = pendingSpot();
      if (spot === null) return { ok: false };
      const attempt = await confirmAt(placeDeps(), spot.x, spot.y);
      if (attempt.ok) setPendingSpot(null);
      return attempt;
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

    confirmSpot: (x, y) => confirmAt(placeDeps(), x, y),

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

    updateTags: (placementId, tags) => tagsEditor.update(placementId, tags),
  };
}
