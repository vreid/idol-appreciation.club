/**
 * The place flow's API surface (docs scaling-architecture.md "Upload
 * flow"): era-tagged place outcomes and the upload-status poll
 * fallback. Split from api.ts to keep both files under the repo's
 * 400-line unit cap; the transport primitives are injected.
 */

import type { PlaceAccepted, PlacedPlacement, UploadStatus } from "@contract";
import {
  placeAcceptedSchema,
  placedPlacementSchema,
  uploadStatusSchema,
} from "@contract";
import { parseOk } from "./api";
import type { ApiClient } from "./api";

export interface PlaceRequest {
  image: Blob;
  fileName: string;
  /** Top-left corner of the unrotated rect in native world px. */
  x: number;
  y: number;
  /** Long edge in px, 16..4096. */
  size: number;
  /** Clockwise rotation in degrees (free angle, default 0). */
  rot: number;
}

/** POST /api/place outcome, both server eras: the old synchronous
 * backend answers 201 with the recorded placement; the quarantine-era
 * backend answers 202 with the upload id and the placement arrives
 * later via `placed`/`place_failed` events (or the status poll). */
export type PlaceOutcome =
  | { era: "sync"; placement: PlacedPlacement }
  | { era: "quarantine"; accepted: PlaceAccepted };

/** A response the era-splitting callers inspect themselves: status
 * line, parsed body and the Retry-After hint, with NO throw on error
 * statuses. */
export interface RawResponse {
  ok: boolean;
  status: number;
  body: unknown;
  retryAfterSeconds: number | null;
}

/** The transport primitives api.ts owns: the raw request and its
 * error-normalizing guard. */
export interface PlaceApiDeps {
  requestRaw: (
    url: string,
    options?: { method?: string; body?: BodyInit },
  ) => Promise<RawResponse>;
  ensureOk: (raw: RawResponse) => void;
}

/** POST /api/place — multipart form, era split on the status line, the
 * boundary parse pins each era's shape. */
async function place(
  deps: PlaceApiDeps,
  req: PlaceRequest,
): Promise<PlaceOutcome> {
  const form = new FormData();
  form.append("image", req.image, req.fileName);
  form.append("x", String(req.x));
  form.append("y", String(req.y));
  form.append("size", String(req.size));
  form.append("rot", String(req.rot));
  const raw = await deps.requestRaw("/api/place", {
    method: "POST",
    body: form,
  });
  deps.ensureOk(raw);
  if (raw.status === 202) {
    return {
      era: "quarantine",
      accepted: parseOk(placeAcceptedSchema, raw.body, "place 202"),
    };
  }
  return {
    era: "sync",
    placement: parseOk(placedPlacementSchema, raw.body, "place 201"),
  };
}

/** GET /api/uploads/{id} — poll fallback for a 202-accepted upload;
 * null on 404 (unknown id, or a pre-flip backend without the route —
 * callers keep waiting on events in that case). */
async function uploadStatus(
  deps: PlaceApiDeps,
  id: string,
): Promise<UploadStatus | null> {
  const raw = await deps.requestRaw(`/api/uploads/${id}`);
  if (raw.status === 404) return null;
  deps.ensureOk(raw);
  return parseOk(uploadStatusSchema, raw.body, "uploads/:id");
}

export function createPlaceApi(
  deps: PlaceApiDeps,
): Pick<ApiClient, "place" | "uploadStatus"> {
  return {
    place: (req) => place(deps, req),
    uploadStatus: (id) => uploadStatus(deps, id),
  };
}
