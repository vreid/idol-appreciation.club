import {
  afterEach,
  beforeEach,
  describe,
  expect,
  it,
  vi,
} from "vite-plus/test";
import { ApiError, type ApiClient } from "./api";
import { DEFAULT_LONG_EDGE } from "./sizing";
import { createAppStore } from "./state.svelte";

interface ClientProbe {
  placeCalls: { x: number; y: number; size: number }[];
}

function makeClient(
  overrides: Partial<ApiClient> = {},
): ApiClient & ClientProbe {
  const placeCalls: { x: number; y: number; size: number }[] = [];
  return {
    register: vi.fn(() => Promise.resolve()),
    login: vi.fn(() => Promise.resolve()),
    logout: vi.fn(() => Promise.resolve()),
    me: vi.fn(() => Promise.resolve(null)),
    place: vi.fn((req) => {
      placeCalls.push({ x: req.x, y: req.y, size: req.size });
      return Promise.resolve({
        id: 1,
        x: req.x,
        y: req.y,
        w: req.size,
        h: req.size,
        seq: 5,
      });
    }),
    near: vi.fn(() => Promise.resolve([])),
    center: vi.fn(() => Promise.resolve({ current: null, all_time: [] })),
    report: vi.fn(() => Promise.resolve()),
    removePlacement: vi.fn(() => Promise.resolve()),
    ...overrides,
    placeCalls,
  };
}

function makeStore(client: ApiClient = makeClient()) {
  let urls = 0;
  const revoked: string[] = [];
  const store = createAppStore(client, {
    createObjectUrl: () => `blob:fake-${++urls}`,
    revokeObjectUrl: (url) => revoked.push(url),
  });
  return { store, revoked };
}

const PNG = new Blob(["png"], { type: "image/png" });

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
          id: 1,
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
          id: 2,
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

describe("place flow", () => {
  it("rejects oversized files without entering the flow", () => {
    const { store } = makeStore();
    const big = new Blob([new Uint8Array(11).fill(0)], {
      type: "image/png",
    });
    Object.defineProperty(big, "size", { value: 10 * 1024 * 1024 + 1 });
    expect(store.startPlace(big, "big.png", 100, 100)).toBe(false);
    expect(store.placeMode).toBe("idle");
  });

  it("prepares an image, adjusts size, and computes the ghost", () => {
    const { store, revoked } = makeStore();
    expect(store.startPlace(PNG, "a.png", 1000, 500)).toBe(true);
    expect(store.placeMode).toBe("panel");
    expect(store.longEdge).toBe(DEFAULT_LONG_EDGE);
    store.setLongEdge(300.9);
    expect(store.longEdge).toBe(301);
    expect(store.ghostSize()).toEqual({ w: 301, h: 151 });
    store.enterChoose();
    expect(store.placeMode).toBe("choose");
    store.backToPanel();
    expect(store.placeMode).toBe("panel");
    store.cancelPlace();
    expect(store.placeMode).toBe("idle");
    expect(revoked).toEqual(["blob:fake-1"]);
  });

  it("confirms a spot with the ghost top-left coordinates", async () => {
    const client = makeClient();
    const { store } = makeStore(client);
    store.startPlace(PNG, "a.png", 1000, 500);
    store.setLongEdge(512);
    store.enterChoose();
    const attempt = await store.confirmSpot(134217000, 134217700);
    expect(attempt).toEqual({
      ok: true,
      placement: { id: 1, x: 134217000, y: 134217700, w: 512, h: 512, seq: 5 },
    });
    expect(client.placeCalls[0]).toEqual({
      x: 134217000,
      y: 134217700,
      size: 512,
    });
    expect(store.placeMode).toBe("idle");
  });

  it("clamps out-of-world spots into the world", async () => {
    const client = makeClient();
    const { store } = makeStore(client);
    store.startPlace(PNG, "a.png", 100, 100);
    await store.confirmSpot(-50, 2 ** 29);
    expect(client.placeCalls[0]).toEqual({ x: 0, y: 2 ** 28, size: 256 });
  });

  it("shows the cooldown remaining on 429 and keeps the panel", async () => {
    const client = makeClient({
      place: () => Promise.reject(new ApiError(429, "cooldown", 30)),
    });
    const { store } = makeStore(client);
    store.startPlace(PNG, "a.png", 100, 100);
    store.enterChoose();
    const attempt = await store.confirmSpot(5, 5);
    expect(attempt.ok).toBe(false);
    expect(store.placeMode).toBe("panel");
    expect(
      store.toasts.some((t) => t.message.includes("Try again in 30s")),
    ).toBe(true);
  });

  it("prompts auth on 401 while placing", async () => {
    const client = makeClient({
      place: () => Promise.reject(new ApiError(401, "nope", null)),
    });
    const { store } = makeStore(client);
    store.startPlace(PNG, "a.png", 100, 100);
    await store.confirmSpot(1, 2);
    expect(store.authPrompt).toBe(true);
  });

  it("ignores confirm without a prepared image", async () => {
    const client = makeClient();
    const { store } = makeStore(client);
    expect((await store.confirmSpot(1, 2)).ok).toBe(false);
    expect(client.placeCalls.length).toBe(0);
  });
});

describe("reports and admin removal", () => {
  it("requires login before reporting", async () => {
    const client = makeClient();
    const { store } = makeStore(client);
    await store.reportPlacement(9);
    expect(client.report).not.toHaveBeenCalled();
    expect(store.authPrompt).toBe(true);
  });

  it("reports when logged in", async () => {
    const client = makeClient({
      me: () =>
        Promise.resolve({
          id: 1,
          username: "u",
          is_admin: false,
          cooldown_until: null,
        }),
      report: vi.fn(() => Promise.resolve()),
    });
    const { store } = makeStore(client);
    await store.refreshMe();
    await store.reportPlacement(9);
    expect(client.report).toHaveBeenCalledWith(9);
  });

  it("surfaces admin removal success and failure", async () => {
    const failing = makeClient({
      removePlacement: () => Promise.reject(new ApiError(403, "no", null)),
    });
    const storeA = makeStore(failing).store;
    expect(await storeA.removePlacement(1)).toBe(false);
    const ok = makeClient();
    const storeB = makeStore(ok).store;
    expect(await storeB.removePlacement(1)).toBe(true);
    expect(ok.removePlacement).toHaveBeenCalledWith(1);
  });
});
