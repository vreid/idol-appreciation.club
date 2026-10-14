import type {
  CenterInfo,
  DigEntry,
  Me,
  PendingAccount,
  Placement,
  PlacedPlacement,
  TagList,
} from "./api";
import type { Toast } from "./toasts";

/** idle: nothing happening; panel: preview ready; choose: picking a spot. */
export type PlaceMode = "idle" | "panel" | "choose";

export interface PreparedImage {
  file: Blob;
  fileName: string;
  previewUrl: string;
  naturalW: number;
  naturalH: number;
}

export type PlaceAttempt =
  | { ok: true; placement: PlacedPlacement }
  | { ok: false };

export interface AppStore {
  readonly me: Me | null;
  readonly authChecked: boolean;
  readonly center: CenterInfo | null;
  readonly toasts: Toast[];
  readonly placeMode: PlaceMode;
  readonly prepared: PreparedImage | null;
  readonly longEdge: number;
  readonly rotation: number;
  readonly submittingPlace: boolean;
  readonly authPrompt: boolean;

  // Property syntax: the implementation is a hub closure, never a bound
  // method (components may pass the reference around directly).
  readonly dismissToast: (id: number) => void;
  /** App-level informational toast (empty placements, bad file, ...). */
  notify(message: string): void;
  /** App-level error toast (malformed live data, ...). */
  notifyError(message: string): void;
  refreshMe(): Promise<void>;
  refreshCenter(): Promise<void>;
  login(username: string, password: string): Promise<boolean>;
  register(username: string, password: string): Promise<boolean>;
  /** True right after a registration that awaits admin approval. */
  readonly registrationPending: boolean;
  /** Last auth error message, for inline display in the auth form. */
  readonly authError: string;
  clearAuthError(): void;
  /** Admin: accounts awaiting approval. */
  readonly adminPending: readonly PendingAccount[];
  loadAdminPending(): Promise<void>;
  approveAccount(username: string): Promise<void>;
  logout(): Promise<void>;
  promptAuth(): void;
  dismissAuth(): void;
  /** Returns false (with an error toast) when the file is rejected. */
  startPlace(
    file: Blob,
    fileName: string,
    naturalW: number,
    naturalH: number,
  ): boolean;
  setLongEdge(value: number): void;
  setRotation(value: number): void;
  ghostSize(): { w: number; h: number };
  /** Pins a long-pressed target so the next place lands there. */
  pinSpot(x: number, y: number): void;
  /** The long-pressed target, if one is waiting for an image. */
  get pendingSpot(): { x: number; y: number } | null;
  /** Places the prepared image at the pinned spot (same auth and
   * cooldown path as confirmSpot). */
  confirmPinned(): Promise<PlaceAttempt>;
  enterChoose(): void;
  backToPanel(): void;
  cancelPlace(): void;
  confirmSpot(x: number, y: number): Promise<PlaceAttempt>;
  refreshNear(x: number, y: number, r: number): Promise<Placement[]>;
  reportPlacement(placementId: string): Promise<void>;
  removePlacement(placementId: string): Promise<boolean>;
  /** Replaces a placement's tag set (owner or admin). On success the
   * selection and dig stack carry the authoritative list; returns null
   * on failure (a toast already describes the error). */
  updateTags(placementId: string, tags: string[]): Promise<TagList | null>;
  /** The placement whose anchored detail panel is open, if any. */
  get selectedPlace(): Placement | null;
  /** Selects a placement and opens its anchored detail panel; a point
   * (jump paths) also loads the pile under it for the depth steppers. */
  selectPlace(placement: Placement, point?: { x: number; y: number }): void;
  deselectPlace(): void;
  /** Digs the pile at a point and caches it; true when the current
   * selection is one of its strata (the repeat-click cycle case). */
  queryPile(x: number, y: number): Promise<boolean>;
  /** Selects `top` against the pile cached by queryPile (no refetch). */
  adoptPile(top: Placement): void;
  /** Moves the selection one stratum down/up through the cached pile
   * (wrapping). False when there is nothing to step through. */
  stepDig(delta: number): boolean;
  /** Strata under the selection, newest-first (depth = index + 1). */
  get digStack(): DigEntry[];
  /** Full pile size at the selection point. */
  get digTotal(): number;
  /** Which stratum the panel shows (0-based; -1 = not in the pile). */
  get digIndex(): number;
}

export interface AppStoreDeps {
  /** Injectable so tests can run without a DOM/object-URL implementation. */
  createObjectUrl?: (blob: Blob) => string;
  revokeObjectUrl?: (url: string) => void;
  toastMs?: number;
  errorToastMs?: number;
}
