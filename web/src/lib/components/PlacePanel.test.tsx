// @vitest-environment happy-dom
import { cleanup, fireEvent, render, screen } from "@solidjs/testing-library";
import { afterEach, describe, expect, it, vi } from "vite-plus/test";
import type { ApiClient } from "../api";
import { createAppStore, type AppStore } from "../state";
import PlacePanel from "./PlacePanel";

afterEach(() => {
  cleanup();
});

const PNG = new Blob(["png"], { type: "image/png" });

/** Offline client: the panel tests never hit the network. */
function makeClient(): ApiClient {
  return {
    register: vi.fn(() => Promise.resolve({ username: "b", pending: false })),
    login: vi.fn(() => Promise.resolve()),
    logout: vi.fn(() => Promise.resolve()),
    me: vi.fn(() => Promise.resolve(null)),
    place: vi.fn(() =>
      Promise.resolve({
        id: "p",
        x: 0,
        y: 0,
        w: 16,
        h: 16,
        rot: 0,
        seq: 1,
      }),
    ),
    near: vi.fn(() => Promise.resolve([])),
    center: vi.fn(() => Promise.resolve({ current: null, all_time: [] })),
    report: vi.fn(() => Promise.resolve()),
    removePlacement: vi.fn(() => Promise.resolve()),
    updateTags: vi.fn(() => Promise.resolve({ id: "p1", tags: [] })),
    pendingAccounts: vi.fn(() => Promise.resolve([])),
    approveAccount: vi.fn(() => Promise.resolve()),
    dig: vi.fn(() =>
      Promise.resolve({ stack: [], total: 0, truncated: false }),
    ),
    feed: vi.fn(() => Promise.resolve({ items: [], next_before: null })),
  };
}

function makeStore(): AppStore {
  return createAppStore(makeClient(), {
    createObjectUrl: () => "blob:fake",
    revokeObjectUrl: () => {
      // test double: revocations are not observed here
    },
  });
}

describe("PlacePanel", () => {
  it("is hidden while idle", () => {
    render(() => <PlacePanel store={makeStore()} />);
    expect(screen.queryByText("Place an image")).toBeNull();
  });

  it("shows size and rotation sliders in panel mode", () => {
    const store = makeStore();
    store.startPlace(PNG, "a.png", 1000, 500);
    render(() => <PlacePanel store={store} />);
    expect(screen.getByText("Place an image")).toBeDefined();
    expect(screen.getByText(/256px/u)).toBeDefined();
    expect(screen.getByText(/0°/u)).toBeDefined();
    const sliders = screen.getAllByRole("slider");
    expect(sliders.length).toBe(2);
  });

  it("clamps rotation to ±180° and mirrors the store", () => {
    const store = makeStore();
    store.startPlace(PNG, "a.png", 100, 100);
    render(() => <PlacePanel store={store} />);
    const sliders = screen.getAllByRole("slider");
    const rotation = sliders[1];
    if (rotation === undefined) throw new Error("expected two sliders");
    fireEvent.input(rotation, { target: { value: "400" } });
    expect(store.rotation).toBe(180);
    fireEvent.input(rotation, { target: { value: "-90" } });
    expect(store.rotation).toBe(-90);
    expect(screen.getByText(/-90°/u)).toBeDefined();
  });

  it("switches to choose mode and back, then cancels away", async () => {
    const store = makeStore();
    store.startPlace(PNG, "a.png", 100, 100);
    render(() => <PlacePanel store={store} />);

    fireEvent.click(screen.getByText(/Choose spot/u));
    expect(store.placeMode).toBe("choose");
    expect(screen.getByText(/ghost is centered/u)).toBeDefined();

    fireEvent.click(screen.getByText(/Adjust size/u));
    expect(store.placeMode).toBe("panel");

    fireEvent.click(screen.getByText("✕"));
    expect(store.placeMode).toBe("idle");
    await Promise.resolve();
    expect(screen.queryByText("Place an image")).toBeNull();
  });

  it("offers the pinned spot after a long-press and places there", async () => {
    const store = makeStore();
    store.pinSpot(500, 700);
    store.startPlace(PNG, "a.png", 100, 100);
    render(() => <PlacePanel store={store} />);

    // The pinned primary replaces "Choose spot"; the stub client accepts
    // the place, so confirmPinned rides the same path as confirmSpot.
    expect(screen.getByText(/Place at pinned spot/u)).toBeDefined();
    expect(screen.queryByText(/🎯 Choose spot/u)).toBeNull();

    fireEvent.click(screen.getByText(/Place at pinned spot/u));
    await vi.waitFor(() => {
      expect(store.pendingSpot).toBeNull();
    });
    expect(store.placeMode).toBe("idle");
  });

  it("shows the plain choose flow when nothing is pinned", () => {
    const store = makeStore();
    store.startPlace(PNG, "a.png", 100, 100);
    render(() => <PlacePanel store={store} />);

    expect(screen.getByText(/🎯 Choose spot/u)).toBeDefined();
    expect(screen.queryByText(/Place at pinned spot/u)).toBeNull();
  });
});
