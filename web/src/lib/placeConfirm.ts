import type { PlaceOutcome, PlaceRequest } from "./placeApi";
import type { PlaceAttempt, PreparedImage } from "./stateTypes";
import { longEdgeToSize } from "./sizing";
import { clampToWorldPoint } from "./world";
import { ApiError } from "./api";
import { describeApiError, isAuthStatus } from "./apiErrors";
import type { ToastKind } from "./toasts";

/** Everything confirmAt needs from the store's reactive closure. The
 * getters are read at call time, so the panel's live size/rotation are
 * always what lands. */
export interface PlaceConfirmDeps {
  prepared: () => PreparedImage | null;
  submitting: () => boolean;
  longEdge: () => number;
  rot: () => number;
  place: (req: PlaceRequest) => Promise<PlaceOutcome>;
  /** Parks the 202 acceptance: the store keeps the user's own preview
   * on the map as a pending marker and resolves it through
   * `placed`/`place_failed` events or the upload-status poll. */
  onAccepted: (accepted: {
    jobId: string;
    x: number;
    y: number;
    w: number;
    h: number;
    rot: number;
    previewUrl: string;
  }) => void;
  pushToast: (kind: ToastKind, message: string) => void;
  requireAuth: () => void;
  /** Success: the flow resets (panel closes, preview released). */
  onPlaced: () => void;
  /** Failure: back to the panel for another attempt. */
  onFailed: () => void;
  onSubmitting: () => void;
  onSettled: () => void;
}

/** POSTs the prepared image at (x, y). The single confirm path for both
 * the click-to-choose flow and the long-press pin — auth and cooldown
 * handling live here and nowhere else. Era handling (docs
 * scaling-architecture.md "Upload flow"): a 201 (old synchronous
 * backend) returns the placement immediately; a 202 parks the pending
 * marker and returns a seq-0 placement so callers can pulse at the
 * spot — the marker clears when the worker resolves the upload. */
export async function confirmAt(
  deps: PlaceConfirmDeps,
  x: number,
  y: number,
): Promise<PlaceAttempt> {
  const image = deps.prepared();
  if (image === null || deps.submitting()) return { ok: false };
  const size = longEdgeToSize(image.naturalW, image.naturalH, deps.longEdge());
  const rot = deps.rot();
  // Out-of-world spots clamp to the border, as with tap-to-choose.
  const spot = clampToWorldPoint({ x, y });
  deps.onSubmitting();
  try {
    const outcome: PlaceOutcome = await deps.place({
      image: image.file,
      fileName: image.fileName,
      x: spot.x,
      y: spot.y,
      size: Math.max(size.w, size.h),
      rot,
    });
    if (outcome.era === "sync") {
      // Old-era backend: behave exactly as before the pipeline existed.
      const p = outcome.placement;
      deps.onPlaced();
      deps.pushToast("success", "Image placed!");
      return { ok: true, placement: p };
    }
    // Quarantine era: the acceptance is the answer. The user's own
    // preview stays on the map (pending marker) until the ingest
    // worker reports; the AABB from the 202 is the marker geometry.
    // onAccepted owns the flow reset AND the previewUrl ownership (the
    // store must not revoke it while the marker shows it).
    const a = outcome.accepted;
    deps.onAccepted({
      jobId: a.id,
      x: a.x,
      y: a.y,
      w: a.w,
      h: a.h,
      rot: a.rot,
      previewUrl: image.previewUrl,
    });
    deps.pushToast(
      "success",
      "Image accepted — it will appear as soon as processing finishes.",
    );
    return {
      ok: true,
      pending: true,
      placement: {
        id: a.id,
        x: a.x,
        y: a.y,
        w: a.w,
        h: a.h,
        rot: a.rot,
        seq: 0,
      },
    };
  } catch (err) {
    if (err instanceof ApiError && err.status === 429) {
      const wait =
        err.retryAfterSeconds === null
          ? ""
          : ` Try again in ${err.retryAfterSeconds}s.`;
      deps.pushToast("error", `Cooldown active.${wait}`);
    } else if (isAuthStatus(err)) {
      deps.requireAuth();
    } else {
      deps.pushToast("error", describeApiError(err));
    }
    deps.onFailed();
    return { ok: false };
  } finally {
    deps.onSettled();
  }
}
