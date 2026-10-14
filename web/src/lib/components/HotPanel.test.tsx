// @vitest-environment happy-dom
import { cleanup, fireEvent, render, screen } from "@solidjs/testing-library";
import { afterEach, describe, expect, it, vi } from "vite-plus/test";
import type { ApiClient, Hot } from "../api";
import { WORLD_SIZE } from "../world";
import HotPanel, { cellCenter, HOT_CELL_PX } from "./HotPanel";

afterEach(() => {
  cleanup();
});

const PAGE: Hot = {
  window_hours: 24,
  total: 12,
  cells: [
    { x: 33, y: 32, count: 9, score: 7.31 },
    { x: 20, y: 40, count: 3, score: 1.5 },
  ],
};

function makeClient(page: Hot = PAGE): ApiClient {
  return {
    register: vi.fn(),
    login: vi.fn(),
    logout: vi.fn(),
    me: vi.fn(),
    place: vi.fn(),
    near: vi.fn(),
    center: vi.fn(),
    report: vi.fn(),
    removePlacement: vi.fn(),
    updateTags: vi.fn(),
    feed: vi.fn(),
    hot: vi.fn(() => Promise.resolve(page)),
    pendingAccounts: vi.fn(),
    approveAccount: vi.fn(),
    dig: vi.fn(),
  };
}

describe("HotPanel", () => {
  it("lists regions hottest-first with counts and scores", async () => {
    render(() => (
      <HotPanel client={makeClient()} onClose={() => {}} onJump={() => {}} />
    ));
    await vi.waitFor(() => {
      expect(screen.getByText("33/32")).toBeTruthy();
    });
    expect(screen.getByText("7.31")).toBeTruthy();
    expect(screen.getByText("9 placements")).toBeTruthy();
    expect(screen.getByText("3 placements")).toBeTruthy();
    expect(screen.getByText(/last 24h · 12 placements/u)).toBeTruthy();
  });

  it("asks the backend for a 24h window, top 8", async () => {
    const client = makeClient();
    render(() => (
      <HotPanel client={client} onClose={() => {}} onJump={() => {}} />
    ));
    await vi.waitFor(() => {
      expect(screen.getByText("33/32")).toBeTruthy();
    });
    expect(client.hot).toHaveBeenCalledWith({ window: 24, limit: 8 });
  });

  it("jumps to the cell centre in centered world coords at z8", () => {
    const target = cellCenter({ x: 33, y: 32, count: 9, score: 7.31 });
    expect(target.zoom).toBe(8);
    expect(target.x).toBeCloseTo(33.5 * HOT_CELL_PX, 5);
    expect(target.y).toBeCloseTo(32.5 * HOT_CELL_PX, 5);
    // The cell centre is a valid native world point: 0..WORLD_SIZE.
    expect(target.x).toBeGreaterThan(0);
    expect(target.x).toBeLessThan(WORLD_SIZE);
    expect(target.y).toBeLessThan(WORLD_SIZE);
  });

  it("fires onJump with the first cell's target on click", async () => {
    const onJump = vi.fn((_t: { x: number; y: number; zoom: number }) => {});
    render(() => (
      <HotPanel client={makeClient()} onClose={() => {}} onJump={onJump} />
    ));
    await vi.waitFor(() => {
      expect(screen.getByText("33/32")).toBeTruthy();
    });
    fireEvent.click(screen.getByText("33/32"));
    expect(onJump).toHaveBeenCalledTimes(1);
    expect(onJump.mock.calls[0]?.[0].zoom).toBe(8);
  });

  it("shows an empty note and an error line", async () => {
    const empty = makeClient({
      window_hours: 24,
      total: 0,
      cells: [],
    });
    render(() => (
      <HotPanel client={empty} onClose={() => {}} onJump={() => {}} />
    ));
    await vi.waitFor(() => {
      expect(screen.getByText(/Nothing hot/u)).toBeTruthy();
    });
    const failing = makeClient();
    failing.hot = vi.fn(() => Promise.reject(new Error("down")));
    render(() => (
      <HotPanel client={failing} onClose={() => {}} onJump={() => {}} />
    ));
    await vi.waitFor(() => {
      expect(screen.getByText("Could not load hot regions")).toBeTruthy();
    });
  });
});
