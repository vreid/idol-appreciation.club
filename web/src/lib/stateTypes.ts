import type {
  CenterInfo,
  DigEntry,
  Me,
  PendingAccount,
  Placement,
  PlacedPlacement,
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
  enterChoose(): void;
  backToPanel(): void;
  cancelPlace(): void;
  confirmSpot(x: number, y: number): Promise<PlaceAttempt>;
  refreshNear(x: number, y: number, r: number): Promise<Placement[]>;
  reportPlacement(placementId: string): Promise<void>;
  removePlacement(placementId: string): Promise<boolean>;
  /** The placement whose anchored detail panel is open, if any. */
  get selectedPlace(): Placement | null;
  /** Selects and queries the strata under the click point (async; the
   * depth cue fills in when the dig resolves). */
  selectPlace(placement: Placement, point: { x: number; y: number }): void;
  deselectPlace(): void;
  /** Strata under the selection point, newest-first (depth = index + 1). */
  get digStack(): DigEntry[];
  /** Full stack size at the selection point (may exceed the 64 cap). */
  get digTotal(): number;
  /** Which stratum the panel currently shows (0-based; -1 = not in stack). */
  get digIndex(): number;
  /** Wheel-flip: delta +1 peels deeper (older), -1 back toward the top. */
  flipDig(delta: number): void;
}

export interface AppStoreDeps {
  /** Injectable so tests can run without a DOM/object-URL implementation. */
  createObjectUrl?: (blob: Blob) => string;
  revokeObjectUrl?: (url: string) => void;
  toastMs?: number;
  errorToastMs?: number;
}
