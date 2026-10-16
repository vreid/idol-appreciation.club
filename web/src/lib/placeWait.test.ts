import { describe, expect, it, vi } from "vite-plus/test";

import {
  createPlacementWaiter,
  resolvePlacementEvent,
  samplePlacedEvent,
} from "./placeWait";
import type { StatusPoll } from "./placeWait";

const placed = samplePlacedEvent("job-1");

/** A poll stub whose answers queue per call (null = keep pending). */
function pollQueue(answers: (null | "placed" | "failed")[]): {
  poll: StatusPoll;
  calls: () => number;
} {
  let n = 0;
  const poll: StatusPoll = () => {
    const answer = answers[Math.min(n, answers.length - 1)];
    n += 1;
    if (answer === "placed") {
      return Promise.resolve({
        state: "placed",
        id: "p1",
        seq: 5,
        bbox: [0, 0, 1, 1],
      });
    }
    if (answer === "failed") {
      return Promise.resolve({
        state: "failed",
        attempts: 5,
        permanent: true,
        error: "decode failed",
      });
    }
    return Promise.resolve(null);
  };
  return { poll, calls: () => n };
}

describe("placement waiter", () => {
  it("resolves with the placed event", async () => {
    const waiter = createPlacementWaiter();
    const promise = waiter.wait("job-1");
    resolvePlacementEvent(waiter, placed, () => {});
    await expect(promise).resolves.toEqual(placed);
  });

  it("resolves from a stashed event that arrived before the waiter", async () => {
    const waiter = createPlacementWaiter();
    resolvePlacementEvent(waiter, placed, () => {});
    await expect(waiter.wait("job-1")).resolves.toEqual(placed);
  });

  it("resolves via the status poll when the event never comes", async () => {
    const { poll } = pollQueue([null, "placed"]);
    const waiter = createPlacementWaiter({ poll, pollIntervalMs: 5 });
    await expect(waiter.wait("job-1")).resolves.toEqual({
      ...placed,
      id: "p1",
    });
  });

  it("reports a poll-lane failure through onFailed and settles null", async () => {
    const { poll } = pollQueue(["failed"]);
    const onFailed = vi.fn();
    const waiter = createPlacementWaiter({
      poll,
      pollIntervalMs: 5,
      onFailed: (id, err) => {
        onFailed(id, err);
      },
    });
    await expect(waiter.wait("job-1")).resolves.toBeNull();
    expect(onFailed).toHaveBeenCalledWith("job-1", "decode failed");
    expect(waiter.failed.has("job-1")).toBe(true);
  });

  it("keeps waiting when the poll errors or 404s", async () => {
    const poll: StatusPoll = vi.fn(() => Promise.reject(new Error("down")));
    const waiter = createPlacementWaiter({
      poll,
      pollIntervalMs: 5,
      timeoutMs: 40,
    });
    await expect(waiter.wait("job-1")).resolves.toBeNull();
  });

  it("times out to null inside the budget", async () => {
    const waiter = createPlacementWaiter({ timeoutMs: 20 });
    const start = Date.now();
    await expect(waiter.wait("job-1")).resolves.toBeNull();
    expect(Date.now() - start).toBeLessThan(2_000);
  });

  it("the first terminal outcome wins and later ones are no-ops", async () => {
    const { poll } = pollQueue(["placed"]);
    const waiter = createPlacementWaiter({ poll, pollIntervalMs: 5 });
    const promise = waiter.wait("job-1");
    // The event lane fires while a poll is in flight.
    resolvePlacementEvent(waiter, placed, () => {});
    await expect(promise).resolves.toEqual(placed);
    // A late duplicate dispatch is stashed, never double-resolved.
    resolvePlacementEvent(waiter, placed, () => {});
    expect(waiter.settled.has("job-1")).toBe(true);
  });

  it("place_failed toasts the reason and settles the waiter null", async () => {
    const waiter = createPlacementWaiter();
    const promise = waiter.wait("job-1");
    const toasts: string[] = [];
    resolvePlacementEvent(
      waiter,
      {
        type: "place_failed",
        job: "job-1",
        attempts: 5,
        permanent: true,
        error: "decode failed",
      },
      (_kind, message) => {
        toasts.push(message);
      },
    );
    await expect(promise).resolves.toBeNull();
    expect(toasts).toEqual(["Placement failed: decode failed"]);
  });

  it("a placed event for an unknown job is stashed for its waiter", () => {
    const waiter = createPlacementWaiter();
    resolvePlacementEvent(waiter, placed, () => {});
    expect(waiter.settled.get("job-1")).toEqual(placed);
  });
});
