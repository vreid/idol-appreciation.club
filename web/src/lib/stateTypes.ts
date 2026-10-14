import type {
  CenterInfo,
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
  /** Selects a placement and opens its anchored detail panel. */
  selectPlace(placement: Placement): void;
  deselectPlace(): void;
}

export interface AppStoreDeps {
  /** Injectable so tests can run without a DOM/object-URL implementation. */
  createObjectUrl?: (blob: Blob) => string;
  revokeObjectUrl?: (url: string) => void;
  toastMs?: number;
  errorToastMs?: number;
}
