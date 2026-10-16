import type { PlaceFailedEvent, PlacedEvent, UploadStatus } from "@contract";
import type { ToastKind } from "./toasts";

/** The 202 → resolution handoff (docs scaling-architecture.md
 * "Upload flow" step 5): a parked waiter resolves with the ingest
 * worker's `placed` event, the upload-status poll, or times out.
 * Order-independent: an event that lands before its waiter is stashed
 * (the 202 response and the SSE tick race on the client). The poll
 * runs alongside the events — it is the fallback when the event
 * stream is down and the fast path when it is not; both lanes are
 * race-safe because the first terminal outcome wins and the losers
 * are drained no-ops. A timeout resolves null — the placement usually
 * still lands, only the immediate confirmation is lost. */
export interface PlacementWaiter {
  waiters: Map<string, (event: PlacedEvent | null) => void>;
  settled: Map<string, PlacedEvent>;
  /** Jobs whose failure was already surfaced (either lane). */
  failed: Set<string>;
  wait: (jobId: string) => Promise<PlacedEvent | null>;
}

/** Poll fallback cadence and overall budget. The poll hits a PK lookup
 * that costs the server almost nothing; 2 s keeps the flow responsive
 * when the SSE lane is dead. */
const POLL_INTERVAL_MS = 2_000;
const WAIT_TIMEOUT_MS = 30_000;

/** Status poller as injected by the api client; null disables the
 * fallback lane (unit tests drive events directly). */
export type StatusPoll = (jobId: string) => Promise<UploadStatus | null>;

export function createPlacementWaiter(options?: {
  poll?: StatusPoll;
  pollIntervalMs?: number;
  timeoutMs?: number;
  /** Poll lane reported a permanent failure for the job (the event
   * lane toasts through its dispatcher; this covers the no-event
   * path). */
  onFailed?: (jobId: string, error: string) => void;
}): PlacementWaiter {
  const poll = options?.poll ?? null;
  const pollIntervalMs = options?.pollIntervalMs ?? POLL_INTERVAL_MS;
  const timeoutMs = options?.timeoutMs ?? WAIT_TIMEOUT_MS;
  const onFailed = options?.onFailed;
  const waiters = new Map<string, (event: PlacedEvent | null) => void>();
  const settled = new Map<string, PlacedEvent>();
  /** Jobs whose failure is already surfaced (poll lane or event lane):
   * the watcher checks this to suppress the "still processing" claim. */
  const failed = new Set<string>();
  const wait = (jobId: string): Promise<PlacedEvent | null> => {
    const stashed = settled.get(jobId);
    if (stashed !== undefined) {
      settled.delete(jobId);
      return Promise.resolve(stashed);
    }
    const { promise, resolve } = Promise.withResolvers<PlacedEvent | null>();
    let done = false;
    const finish = (event: PlacedEvent | null): void => {
      if (done) return;
      done = true;
      clearInterval(timer);
      clearTimeout(deadline);
      waiters.delete(jobId);
      resolve(event);
    };
    const deadline = setTimeout(() => {
      finish(null);
    }, timeoutMs);
    const timer = setInterval(() => {
      if (poll === null) return;
      poll(jobId)
        .then((status) => {
          if (status === null || status.state === "pending") return;
          if (status.state === "failed") {
            // Mirror the event lane's failure handling: the placement
            // did not land.
            failed.add(jobId);
            onFailed?.(jobId, status.error);
            finish(null);
            return;
          }
          finish({
            type: "placed",
            id: status.id,
            job: jobId,
            seq: status.seq,
            bbox: status.bbox,
            duplicate_count: 0,
          });
        })
        .catch(() => {
          // Network hiccup / old backend without the route: the
          // events lane and the next tick stay the fallback.
        });
    }, pollIntervalMs);
    waiters.set(jobId, (event) => {
      finish(event);
    });
    return promise;
  };
  return { waiters, settled, failed, wait };
}

/** SSE dispatch target: wake the flow that owns this job id. A
 * place_failed always toasts (the placement did not land); a placed
 * event with no waiter is stashed for it (the races run both ways). */
export function resolvePlacementEvent(
  waiter: PlacementWaiter,
  event: PlacedEvent | PlaceFailedEvent,
  pushToast: (kind: ToastKind, message: string) => void,
): void {
  if (event.type === "place_failed") {
    const resolve = waiter.waiters.get(event.job);
    waiter.failed.add(event.job);
    if (resolve !== undefined) {
      waiter.waiters.delete(event.job);
      resolve(null);
    }
    pushToast("error", `Placement failed: ${event.error}`);
    return;
  }
  const resolve = waiter.waiters.get(event.job);
  if (resolve !== undefined) {
    waiter.waiters.delete(event.job);
    resolve(event);
    return;
  }
  waiter.settled.set(event.job, event);
}

/** Test fixture: a minimal `placed` event for a job id (bbox is
 * required by the wire schema, so a tiny in-world rect). */
export const samplePlacedEvent = (jobId: string): PlacedEvent => ({
  type: "placed",
  id: jobId,
  job: jobId,
  seq: 5,
  bbox: [0, 0, 1, 1],
  duplicate_count: 0,
});
