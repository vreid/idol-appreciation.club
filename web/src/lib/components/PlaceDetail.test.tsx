// @vitest-environment happy-dom
import {
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@solidjs/testing-library";
import { createSignal } from "solid-js";
import { afterEach, describe, expect, it, vi } from "vite-plus/test";
import type { ApiClient, Placement } from "../api";
import { createAppStore, type AppStore } from "../state";
import PlaceDetail, {
  type PlaceDetailActions,
  type SelectionGeometry,
} from "./PlaceDetail";

afterEach(() => {
  cleanup();
});

const PLACEMENT: Placement = {
  id: "p1",
  username: "wren",
  x: 0,
  y: 0,
  w: 64,
  h: 48,
  rot: 15,
  created_at: "2026-10-13T00:00:00Z",
};

function makeClient(dig?: ApiClient["dig"]): ApiClient {
  return {
    register: vi.fn(() => Promise.resolve({ username: "b", pending: false })),
    login: vi.fn(() => Promise.resolve()),
    logout: vi.fn(() => Promise.resolve()),
    me: vi.fn(() => Promise.resolve(null)),
    place: vi.fn(() => Promise.resolve({ ...PLACEMENT, seq: 1 })),
    near: vi.fn(() => Promise.resolve([])),
    center: vi.fn(() => Promise.resolve({ current: null, all_time: [] })),
    report: vi.fn(() => Promise.resolve()),
    removePlacement: vi.fn(() => Promise.resolve()),
    pendingAccounts: vi.fn(() => Promise.resolve([])),
    approveAccount: vi.fn(() => Promise.resolve()),
    dig:
      dig ??
      vi.fn(() => Promise.resolve({ stack: [], total: 0, truncated: false })),
  };
}

function makeStore(client?: ApiClient): AppStore {
  return createAppStore(client ?? makeClient(), {
    createObjectUrl: () => "blob:fake",
    revokeObjectUrl: () => {},
  });
}

function makeActions(
  overrides: Partial<PlaceDetailActions> = {},
  store?: AppStore,
) {
  return {
    isLoggedIn: vi.fn(() => false),
    onReport: vi.fn(),
    isAdmin: vi.fn(() => false),
    onRemove: vi.fn(),
    onAuthPrompt: vi.fn(),
    onDeselect: vi.fn(() => store?.deselectPlace()),
    ...overrides,
  };
}

const GEOMETRY: SelectionGeometry = {
  aabb: { x: 100, y: 100, w: 64, h: 48 },
  viewport: { w: 1280, h: 800 },
};

function mount(store: AppStore, overrides: Partial<PlaceDetailActions> = {}) {
  const actions = makeActions(overrides, store);
  const [version, bump] = createSignal(0);
  const geometry = vi.fn(() => GEOMETRY);
  render(() => (
    <PlaceDetail
      store={store}
      container={document.createElement("div")}
      viewportVersion={version}
      geometry={geometry}
      actions={actions}
    />
  ));
  return { store, actions, geometry, bump };
}

describe("PlaceDetail", () => {
  it("renders nothing when no placement is selected", () => {
    mount(makeStore());
    expect(document.querySelector(".place-detail")).toBeNull();
  });

  it("shows the anchored panel for a selected placement", () => {
    const { store } = mount(makeStore());
    store.selectPlace(PLACEMENT, { x: 0, y: 0 });
    expect(document.querySelector(".place-detail")).not.toBeNull();
    expect(screen.getByText("@wren")).toBeTruthy();
    expect(screen.getByText("64×48 px · 15°")).toBeTruthy();
  });

  it("deselects on the close button", () => {
    const { store } = mount(makeStore());
    store.selectPlace(PLACEMENT, { x: 0, y: 0 });
    fireEvent.click(screen.getByRole("button", { name: "Close details" }));
    expect(store.selectedPlace).toBeNull();
    expect(document.querySelector(".place-detail")).toBeNull();
  });

  it("prompts for auth on report when logged out and keeps the selection", () => {
    const { store, actions } = mount(makeStore(), { isLoggedIn: () => false });
    store.selectPlace(PLACEMENT, { x: 0, y: 0 });
    fireEvent.click(screen.getByText("Report"));
    expect(actions.onAuthPrompt).toHaveBeenCalledTimes(1);
    expect(actions.onReport).not.toHaveBeenCalled();
    expect(actions.onDeselect).not.toHaveBeenCalled();
    expect(store.selectedPlace).not.toBeNull();
  });

  it("reports and deselects when logged in", () => {
    const { store, actions } = mount(makeStore(), { isLoggedIn: () => true });
    store.selectPlace(PLACEMENT, { x: 0, y: 0 });
    fireEvent.click(screen.getByText("Report"));
    expect(actions.onReport).toHaveBeenCalledWith("p1");
    expect(actions.onDeselect).toHaveBeenCalled();
    expect(store.selectedPlace).toBeNull();
  });

  it("offers Remove only to admins and passes the placement bbox", () => {
    const { store, actions } = mount(makeStore(), { isAdmin: () => true });
    store.selectPlace(PLACEMENT, { x: 0, y: 0 });
    expect(screen.getByText("Remove")).toBeTruthy();
    fireEvent.click(screen.getByText("Remove"));
    expect(actions.onRemove).toHaveBeenCalledWith("p1", {
      x: 0,
      y: 0,
      w: 64,
      h: 48,
    });
  });

  it("hides Remove from non-admins", () => {
    const { store } = mount(makeStore(), { isAdmin: () => false });
    store.selectPlace(PLACEMENT, { x: 0, y: 0 });
    expect(screen.queryByText("Remove")).toBeNull();
  });

  it("Escape deselects through the unified action", () => {
    const { store, actions } = mount(makeStore());
    store.selectPlace(PLACEMENT, { x: 0, y: 0 });
    fireEvent.keyDown(window, { key: "Escape" });
    expect(actions.onDeselect).toHaveBeenCalled();
    expect(store.selectedPlace).toBeNull();
  });

  it("recomputes geometry when the viewport version bumps", () => {
    const { store, geometry, bump } = mount(makeStore());
    store.selectPlace(PLACEMENT, { x: 0, y: 0 });
    const calls = geometry.mock.calls.length;
    expect(calls).toBeGreaterThan(0);
    bump(1);
    expect(geometry.mock.calls.length).toBeGreaterThan(calls);
  });
});

function cueClient(): ApiClient {
  const stack = [
    { ...PLACEMENT, username: "wren", depth: 1 },
    { ...PLACEMENT, id: "p2", username: "noor", depth: 2 },
    { ...PLACEMENT, id: "p3", username: "sable", depth: 3 },
  ];
  return makeClient(() =>
    Promise.resolve({ stack, total: 3, truncated: false }),
  );
}

describe("PlaceDetail dig depth cue", () => {
  it("shows the depth cue and follows flips", async () => {
    const store = makeStore(cueClient());
    store.selectPlace(PLACEMENT, { x: 10, y: 10 });
    mount(store);
    await waitFor(() => {
      expect(screen.getByText("1 of 3")).toBeTruthy();
    });
    store.flipDig(1);
    await waitFor(() => {
      expect(screen.getByText("2 of 3")).toBeTruthy();
    });
    await waitFor(() => {
      expect(screen.getByText("@noor")).toBeTruthy();
    });
  });

  it("hides the cue for single-stratum selections", async () => {
    const store = makeStore(makeClient());
    store.selectPlace(PLACEMENT, { x: 10, y: 10 });
    mount(store);
    await waitFor(() => {
      expect(screen.getByText("@wren")).toBeTruthy();
    });
    expect(screen.queryByText("1 of 1")).toBeNull();
  });
});
