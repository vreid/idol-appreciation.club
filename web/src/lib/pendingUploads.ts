/**
 * The pending-upload registry for the 202 quarantine flow (docs
 * scaling-architecture.md "Upload flow" step 5): the user's own
 * preview parked at the accepted AABB until the ingest worker
 * resolves the upload. Owns the waiter (event lane + status-poll
 * lane) so the store only sees track/resolve.
 */

import { createSignal } from "solid-js";
import type { PlaceFailedEvent, PlacedEvent } from "@contract";
import { createPlacementWaiter, resolvePlacementEvent } from "./placeWait";
import type { PendingUpload } from "./stateTypes";
import type { ToastKind } from "./toasts";

export interface PendingUploadsDeps {
  /** Poll fallback lane (docs step 5): a PK lookup the worker updates
   * once per upload; it carries the flow when the SSE lane is down
   * and races it when it is not. */
  poll: (jobId: string) => Promise<
    | { state: "pending" }
    | {
        state: "placed";
        id: string;
        seq: number;
        bbox: [number, number, number, number];
      }
    | {
        state: "failed";
        attempts: number;
        permanent: boolean;
        error: string;
      }
    | null
  >;
  revokeObjectUrl: (url: string) => void;
  pushToast: (kind: ToastKind, message: string) => void;
  /** A parked upload settled: `placed` carries the worker's event
   * (bbox/seq for the pulse); null means failed or the wait budget
   * ran out — the marker is gone either way. */
  onResolved: (jobId: string, placed: PlacedEvent | null) => void;
}

export interface PendingUploads {
  /** 202-accepted uploads not yet recorded, oldest first. */
  readonly list: () => PendingUpload[];
  /** SSE dispatch target: wake the flow that owns this job id (a
   * place_failed toasts here and settles its waiter null). */
  resolveEvent: (event: PlacedEvent | PlaceFailedEvent) => void;
  track: (entry: PendingUpload) => void;
}

export function createPendingUploads(deps: PendingUploadsDeps): PendingUploads {
  const placementWaiter = createPlacementWaiter({
    poll: deps.poll,
    // The poll lane's failure toast (the event lane toasts through its
    // own dispatcher).
    onFailed: (_jobId, error) => {
      deps.pushToast("error", `Placement failed: ${error}`);
    },
  });

  const [uploads, setUploads] = createSignal<PendingUpload[]>([]);

  function drop(jobId: string): void {
    const entry = uploads().find((p) => p.jobId === jobId);
    if (entry !== undefined) deps.revokeObjectUrl(entry.previewUrl);
    setUploads((list) => list.filter((p) => p.jobId !== jobId));
    placementWaiter.failed.delete(jobId);
  }

  return {
    list: uploads,
    resolveEvent: (event) => {
      resolvePlacementEvent(placementWaiter, event, deps.pushToast);
    },
    track: (entry) => {
      setUploads((list) => [...list, entry]);
      // Detached resolver: the marker stays until the worker's event
      // or the status poll settles the upload; the placement normally
      // lands regardless of which lane wins (the tiles arrive via the
      // versions events).
      void placementWaiter.wait(entry.jobId).then((placed) => {
        const failed = placementWaiter.failed.has(entry.jobId);
        drop(entry.jobId);
        if (placed !== null) {
          deps.pushToast("success", "Image placed!");
        } else if (!failed) {
          deps.pushToast(
            "info",
            "Still processing — it will appear when ready",
          );
        }
        deps.onResolved(entry.jobId, placed);
      });
    },
  };
}
