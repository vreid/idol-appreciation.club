import {
  afterEach,
  beforeEach,
  describe,
  expect,
  it,
  vi,
} from "vite-plus/test";
import { samplePlacedEvent } from "./placeWait";

import { ApiError, type ApiClient, type PlaceRequest } from "./api";
import { createAppStore } from "./state";

interface ClientProbe {
  placeCalls: { x: number; y: number; size: number }[];
}

function makeClient(
  overrides: Partial<ApiClient> = {},
): ApiClient & ClientProbe {
  const placeCalls: { x: number; y: number; size: number }[] = [];
  return {
    register: vi.fn(() => Promise.resolve({ username: "bot", pending: false })),
    login: vi.fn(() => Promise.resolve()),
    logout: vi.fn(() => Promise.resolve()),
    pendingAccounts: vi.fn(() => Promise.resolve([])),
    approveAccount: vi.fn(() => Promise.resolve()),
    feed: vi.fn(() => Promise.resolve({ items: [], next_before: null })),
    dig: vi.fn(() =>
      Promise.resolve({ stack: [], total: 0, truncated: false }),
    ),
    me: vi.fn(() => Promise.resolve(null)),
    place: vi.fn((req: PlaceRequest) => {
      placeCalls.push({ x: req.x, y: req.y, size: req.size });
      const { id } = samplePlacedEvent("job-1");
      return Promise.resolve({
        era: "quarantine" as const,
        accepted: {
          id,
          state: "pending" as const,
          x: req.x,
          y: req.y,
          w: req.size,
          h: req.size,
          rot: req.rot,
        },
      });
    }),
    uploadStatus: vi.fn(() => Promise.resolve(null)),
    near: vi.fn(() => Promise.resolve([])),
    center: vi.fn(() => Promise.resolve({ current: null, all_time: [] })),
    report: vi.fn(() => Promise.resolve()),
    removePlacement: vi.fn(() => Promise.resolve()),
    updateTags: vi.fn(() => Promise.resolve({ id: "j", tags: [] })),
    hot: vi.fn(() =>
      Promise.resolve({ window_hours: 24, total: 0, cells: [] }),
    ),
    ...overrides,
    placeCalls,
  };
}

function makeStore(client: ApiClient = makeClient()) {
  let urls = 0;
  const revoked: string[] = [];
  const store = createAppStore(client, {
    createObjectUrl: () => `blob:fake-${++urls}`,
    revokeObjectUrl: (url) => {
      revoked.push(url);
    },
  });
  return { store, revoked };
}

beforeEach(() => {
  vi.useFakeTimers();
});

afterEach(() => {
  vi.useRealTimers();
});

describe("session", () => {
  it("loads me and center on refresh", async () => {
    const client = makeClient({
      me: vi.fn(() =>
        Promise.resolve({
          id: "0197d3a4-8f2b-7c11-b3a1-5d2e9f60a0e2",
          username: "rei",
          is_admin: false,
          cooldown_until: null,
        }),
      ),
    });
    const { store } = makeStore(client);
    await store.refreshMe();
    await store.refreshCenter();
    expect(store.me?.username).toBe("rei");
    expect(store.authChecked).toBe(true);
    expect(store.center).toEqual({ current: null, all_time: [] });
  });

  it("falls back to null me on fetch failure and toasts", async () => {
    const client = makeClient({ me: () => Promise.reject(new Error("x")) });
    const { store } = makeStore(client);
    await store.refreshMe();
    expect(store.me).toBeNull();
    expect(store.toasts[0]?.kind).toBe("error");
    vi.advanceTimersByTime(7000);
    expect(store.toasts.length).toBe(0);
  });

  it("login success refreshes me and clears the auth prompt", async () => {
    const client = makeClient({
      me: vi.fn(() =>
        Promise.resolve({
          id: "0197d3a4-8f2b-7c11-b3a1-5d2e9f60a0e4",
          username: "liz",
          is_admin: true,
          cooldown_until: null,
        }),
      ),
    });
    const { store } = makeStore(client);
    store.promptAuth();
    const ok = await store.login("liz", "pw");
    expect(ok).toBe(true);
    expect(store.me?.is_admin).toBe(true);
    expect(store.authPrompt).toBe(false);
  });

  it("login failure keeps the prompt and reports the error", async () => {
    const client = makeClient({
      login: () => Promise.reject(new ApiError(401, "wrong password", null)),
    });
    const { store } = makeStore(client);
    const ok = await store.login("liz", "bad");
    expect(ok).toBe(false);
    expect(store.me).toBeNull();
    expect(store.toasts.some((t) => t.message === "wrong password")).toBe(true);
  });

  it("register then logout clears the user", async () => {
    const { store } = makeStore(makeClient());
    expect(await store.register("gaeul", "pw")).toBe(true);
    await store.logout();
    expect(store.me).toBeNull();
  });

  it("pending registration surfaces approval notice without a session", async () => {
    const client = makeClient({
      register: vi.fn(() =>
        Promise.resolve({ username: "gaeul", pending: true }),
      ),
    });
    const { store } = makeStore(client);
    expect(await store.register("gaeul", "pw")).toBe(false);
    expect(store.registrationPending).toBe(true);
    expect(store.me).toBeNull();
    expect(store.toasts.some((t) => t.message.includes("admin approval"))).toBe(
      true,
    );
    expect(client.me).not.toHaveBeenCalled();
  });

  it("pending 403 on login surfaces the server message inline", async () => {
    const client = makeClient({
      login: vi.fn(() =>
        Promise.reject(new ApiError(403, "account pending approval")),
      ),
    });
    const { store } = makeStore(client);
    expect(await store.login("gaeul", "pw")).toBe(false);
    expect(store.authError).toBe("account pending approval");
  });
});

function oversizedBlob(): Blob {
  const blob = new Blob(["x"], { type: "image/png" });
  Object.defineProperty(blob, "size", { value: 10 * 1024 * 1024 + 1 });
  return blob;
}

describe("toasts", () => {
  it("auto-dismisses error toasts after their lifetime", () => {
    const { store } = makeStore();
    expect(store.startPlace(oversizedBlob(), "big.png", 10, 10)).toBe(false);
    expect(store.toasts.length).toBe(1);
    vi.advanceTimersByTime(5999);
    expect(store.toasts.length).toBe(1);
    vi.advanceTimersByTime(1);
    expect(store.toasts.length).toBe(0);
  });

  it("dismissToast removes a specific toast immediately", () => {
    const { store } = makeStore();
    store.startPlace(oversizedBlob(), "big.png", 10, 10);
    const toast = store.toasts[0];
    if (toast === undefined) throw new Error("expected an error toast");
    store.dismissToast(toast.id);
    expect(store.toasts.length).toBe(0);
  });
});

describe("reports and admin removal", () => {
  it("requires login before reporting", async () => {
    const client = makeClient();
    const { store } = makeStore(client);
    await store.reportPlacement("0197d3a4-8f2b-7c11-b3a1-5d2e9f60a009");
    expect(client.report).not.toHaveBeenCalled();
    expect(store.authPrompt).toBe(true);
  });

  it("reports when logged in", async () => {
    const client = makeClient({
      me: () =>
        Promise.resolve({
          id: "0197d3a4-8f2b-7c11-b3a1-5d2e9f60a0e3",
          username: "u",
          is_admin: false,
          cooldown_until: null,
        }),
      report: vi.fn(() => Promise.resolve()),
    });
    const { store } = makeStore(client);
    await store.refreshMe();
    await store.reportPlacement("0197d3a4-8f2b-7c11-b3a1-5d2e9f60a009");
    expect(client.report).toHaveBeenCalledWith(
      "0197d3a4-8f2b-7c11-b3a1-5d2e9f60a009",
    );
  });

  it("surfaces admin removal success and failure", async () => {
    const failing = makeClient({
      removePlacement: () => Promise.reject(new ApiError(403, "no", null)),
    });
    const storeA = makeStore(failing).store;
    expect(
      await storeA.removePlacement("0197d3a4-8f2b-7c11-b3a1-5d2e9f60a001"),
    ).toBe(false);
    const ok = makeClient();
    const storeB = makeStore(ok).store;
    expect(
      await storeB.removePlacement("0197d3a4-8f2b-7c11-b3a1-5d2e9f60a001"),
    ).toBe(true);
    expect(ok.removePlacement).toHaveBeenCalledWith(
      "0197d3a4-8f2b-7c11-b3a1-5d2e9f60a001",
    );
  });
});

describe("tags (store wiring)", () => {
  const SELECTED = {
    id: "0197d3a4-8f2b-7c11-b3a1-5d2e9f60a001",
    username: "wren",
    x: 10,
    y: 10,
    w: 64,
    h: 48,
    rot: 0,
    created_at: "2026-10-13T00:00:00Z",
    tags: ["stage"] as string[] | undefined,
  };

  it("requires login before editing tags", async () => {
    const client = makeClient();
    const { store } = makeStore(client);
    const result = await store.updateTags(SELECTED.id, ["stage", "new"]);
    expect(client.updateTags).not.toHaveBeenCalled();
    expect(result).toBeNull();
    expect(store.authPrompt).toBe(true);
  });

  it("updates the selection with the authoritative list", async () => {
    const client = makeClient({
      me: () =>
        Promise.resolve({
          id: "u1",
          username: "wren",
          is_admin: false,
          cooldown_until: null,
        }),
      updateTags: vi.fn(() =>
        Promise.resolve({
          id: "0197d3a4-8f2b-7c11-b3a1-5d2e9f60a001",
          tags: ["encore"],
        }),
      ),
    });
    const { store } = makeStore(client);
    await store.refreshMe();
    store.selectPlace(SELECTED);
    await Promise.resolve();
    const result = await store.updateTags(SELECTED.id, ["stage", "encore"]);
    expect(client.updateTags).toHaveBeenCalledWith(SELECTED.id, [
      "stage",
      "encore",
    ]);
    expect(result?.tags).toEqual(["encore"]);
    expect(store.selectedPlace?.tags).toEqual(["encore"]);
  });
});
