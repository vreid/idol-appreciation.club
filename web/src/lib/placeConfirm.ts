import type { ApiClient, PlacedPlacement } from "./api";
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
  place: ApiClient["place"];
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
 * handling live here and nowhere else. */
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
    const placement: PlacedPlacement = await deps.place({
      image: image.file,
      fileName: image.fileName,
      x: spot.x,
      y: spot.y,
      size: Math.max(size.w, size.h),
      rot,
    });
    deps.onPlaced();
    deps.pushToast("success", "Image placed!");
    return { ok: true, placement };
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
