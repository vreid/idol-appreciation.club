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

function makeClient(): ApiClient {
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
    updateTags: vi.fn(() => Promise.resolve({ id: "p1", tags: [] })),
    pendingAccounts: vi.fn(() => Promise.resolve([])),
    approveAccount: vi.fn(() => Promise.resolve()),
    feed: vi.fn(() => Promise.resolve({ items: [], next_before: null })),
    dig: vi.fn(() =>
      Promise.resolve({ stack: [], total: 0, truncated: false }),
    ),
    hot: vi.fn(() =>
      Promise.resolve({ window_hours: 24, total: 0, cells: [] }),
    ),
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
    onUpdateTags: vi.fn(() => Promise.resolve()),
    onDeselect: vi.fn(() => store?.deselectPlace()),
    onStepDig: vi.fn((delta: number) => {
      store?.stepDig(delta);
    }),
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
    store.selectPlace(PLACEMENT);
    expect(document.querySelector(".place-detail")).not.toBeNull();
    expect(screen.getByText("@wren")).toBeTruthy();
    expect(screen.getByText("64×48 px · 15°")).toBeTruthy();
  });

  it("deselects on the close button", () => {
    const { store } = mount(makeStore());
    store.selectPlace(PLACEMENT);
    fireEvent.click(screen.getByRole("button", { name: "Close details" }));
    expect(store.selectedPlace).toBeNull();
    expect(document.querySelector(".place-detail")).toBeNull();
  });

  it("prompts for auth on report when logged out and keeps the selection", () => {
    const { store, actions } = mount(makeStore(), { isLoggedIn: () => false });
    store.selectPlace(PLACEMENT);
    fireEvent.click(screen.getByText("Report"));
    expect(actions.onAuthPrompt).toHaveBeenCalledTimes(1);
    expect(actions.onReport).not.toHaveBeenCalled();
    expect(actions.onDeselect).not.toHaveBeenCalled();
    expect(store.selectedPlace).not.toBeNull();
  });

  it("reports and deselects when logged in", () => {
    const { store, actions } = mount(makeStore(), { isLoggedIn: () => true });
    store.selectPlace(PLACEMENT);
    fireEvent.click(screen.getByText("Report"));
    expect(actions.onReport).toHaveBeenCalledWith("p1");
    expect(actions.onDeselect).toHaveBeenCalled();
    expect(store.selectedPlace).toBeNull();
  });

  it("offers Remove only to admins and passes the placement bbox", () => {
    const { store, actions } = mount(makeStore(), { isAdmin: () => true });
    store.selectPlace(PLACEMENT);
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
    store.selectPlace(PLACEMENT);
    expect(screen.queryByText("Remove")).toBeNull();
  });

  it("Escape deselects through the unified action", () => {
    const { store, actions } = mount(makeStore());
    store.selectPlace(PLACEMENT);
    fireEvent.keyDown(window, { key: "Escape" });
    expect(actions.onDeselect).toHaveBeenCalled();
    expect(store.selectedPlace).toBeNull();
  });

  it("recomputes geometry when the viewport version bumps", () => {
    const { store, geometry, bump } = mount(makeStore());
    store.selectPlace(PLACEMENT);
    const calls = geometry.mock.calls.length;
    expect(calls).toBeGreaterThan(0);
    bump(1);
    expect(geometry.mock.calls.length).toBeGreaterThan(calls);
  });
});

function digClient(stack: Placement[]): ApiClient {
  const client = makeClient();
  client.dig = vi.fn(() =>
    Promise.resolve({
      stack: stack.map((p, i) => ({ ...p, depth: i + 1 })),
      total: stack.length,
      truncated: false,
    }),
  );
  return client;
}

function stratum(id: string, username: string): Placement {
  return { ...PLACEMENT, id, username };
}

describe("PlaceDetail depth steppers", () => {
  it("cycles via the stepper buttons", async () => {
    const store = makeStore(
      digClient([
        stratum("p1", "wren"),
        stratum("p2", "noor"),
        stratum("p3", "sable"),
      ]),
    );
    store.selectPlace(stratum("p1", "wren"), { x: 10, y: 10 });
    mount(store);
    await waitFor(() => {
      expect(screen.getByText("1 of 3")).toBeTruthy();
    });
    fireEvent.click(screen.getByLabelText("Next stratum"));
    await waitFor(() => {
      expect(screen.getByText("2 of 3")).toBeTruthy();
    });
    expect(screen.getByText("@noor")).toBeTruthy();
    fireEvent.click(screen.getByLabelText("Previous stratum"));
    await waitFor(() => {
      expect(screen.getByText("1 of 3")).toBeTruthy();
    });
  });

  it("hides the steppers for single-stratum piles", async () => {
    const single = makeStore(digClient([stratum("p1", "wren")]));
    single.selectPlace(stratum("p1", "wren"), { x: 10, y: 10 });
    mount(single);
    await waitFor(() => {
      expect(screen.getByText("@wren")).toBeTruthy();
    });
    expect(screen.queryByText("1 of 1")).toBeNull();
  });

  it("steps with the keyboard and leaves inputs alone", async () => {
    const store = makeStore(
      digClient([stratum("p1", "wren"), stratum("p2", "noor")]),
    );
    store.selectPlace(stratum("p1", "wren"), { x: 10, y: 10 });
    mount(store);
    await waitFor(() => {
      expect(screen.getByText("1 of 2")).toBeTruthy();
    });
    fireEvent.keyDown(window, { key: "ArrowRight" });
    await waitFor(() => {
      expect(screen.getByText("2 of 2")).toBeTruthy();
    });
    fireEvent.keyDown(window, { key: "ArrowLeft" });
    await waitFor(() => {
      expect(screen.getByText("1 of 2")).toBeTruthy();
    });
  });
});

describe("PlaceDetail anonymous view", () => {
  it("shows chips without an editor to anonymous viewers", async () => {
    const store = makeStore();
    store.selectPlace({ ...PLACEMENT, tags: ["stage", "encore"] });
    mount(store);
    await waitFor(() => {
      expect(screen.getByText("encore")).toBeTruthy();
    });
    expect(screen.queryByLabelText("Add tag")).toBeNull();
  });
});

describe("PlaceDetail tags", () => {
  const TAGGED: Placement = { ...PLACEMENT, tags: ["stage", "encore"] };

  async function mountSignedIn(
    username: string,
    is_admin: boolean,
    overrides: Partial<PlaceDetailActions> = {},
  ) {
    const client = makeClient();
    client.me = vi.fn(() =>
      Promise.resolve({ id: "u9", username, is_admin, cooldown_until: null }),
    );
    const store = makeStore(client);
    store.selectPlace(TAGGED);
    const mounted = mount(store, overrides);
    await store.refreshMe();
    return mounted;
  }

  function mountAsOwner(overrides: Partial<PlaceDetailActions> = {}) {
    return mountSignedIn("wren", false, overrides);
  }

  it("renders chips for a tagged selection", () => {
    const store = makeStore();
    store.selectPlace(TAGGED);
    mount(store);
    expect(screen.getByText("stage")).toBeTruthy();
    expect(screen.getByText("encore")).toBeTruthy();
  });

  it("renders nothing for untagged selections viewed by others", () => {
    const store = makeStore();
    store.selectPlace(PLACEMENT);
    mount(store);
    expect(screen.queryByRole("list", { name: "Tags" })).toBeNull();
  });

  it("hides the editor from signed-in non-owners", async () => {
    await mountSignedIn("noor", false);
    await waitFor(() => {
      expect(screen.getByText("stage")).toBeTruthy();
    });
    expect(screen.queryByLabelText("Add tag")).toBeNull();
    expect(
      screen.queryByRole("button", { name: "Remove tag stage" }),
    ).toBeNull();
  });

  it("offers chips with removal and the add form to the owner", async () => {
    const { actions } = await mountAsOwner();
    fireEvent.click(screen.getByRole("button", { name: "Remove tag stage" }));
    await waitFor(() => {
      expect(actions.onUpdateTags).toHaveBeenCalledWith("p1", ["encore"]);
    });
    fireEvent.input(screen.getByLabelText("Add tag"), {
      target: { value: "  finale  " },
    });
    fireEvent.submit(screen.getByLabelText("Add tag").closest("form")!);
    await waitFor(() => {
      expect(actions.onUpdateTags).toHaveBeenCalledWith("p1", [
        "stage",
        "encore",
        "finale",
      ]);
    });
  });

  it("offers the editor to admins even when not the owner", async () => {
    await mountSignedIn("mod", true, { isAdmin: () => true });
    await waitFor(() => {
      expect(screen.getByLabelText("Add tag")).toBeTruthy();
    });
  });

  it("clears the draft and ignores empty submissions", async () => {
    const { actions } = await mountAsOwner();
    const input = screen.getByLabelText("Add tag");
    fireEvent.input(input, { target: { value: "   " } });
    fireEvent.submit(input.closest("form")!);
    expect(actions.onUpdateTags).not.toHaveBeenCalled();
    fireEvent.input(input, { target: { value: "bisou" } });
    fireEvent.submit(input.closest("form")!);
    await waitFor(() => {
      expect(actions.onUpdateTags).toHaveBeenCalledWith("p1", [
        "stage",
        "encore",
        "bisou",
      ]);
    });
    expect(screen.getByLabelText<HTMLInputElement>("Add tag").value).toBe("");
  });
});
