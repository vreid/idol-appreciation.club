import { describe, expect, it, vi } from "vite-plus/test";
import type { ApiClient, CenterInfo } from "./api";
import { createSessionRefresher } from "./session";

const CENTER: CenterInfo = {
  current: {
    username: "wren",
    since: "2026-10-13T00:00:00Z",
    placement: { id: "p1", x: 1, y: 2, w: 3, h: 4, rot: 0 },
  },
  all_time: [],
};

function makeDeps(
  overrides: Partial<Parameters<typeof createSessionRefresher>[0]> = {},
) {
  const errors: string[] = [];
  const deps = {
    client: {
      me: vi.fn(() =>
        Promise.resolve({
          id: "u1",
          username: "wren",
          is_admin: false,
          cooldown_until: null,
        }),
      ),
      center: vi.fn(() => Promise.resolve(CENTER)),
    } satisfies Pick<ApiClient, "me" | "center">,
    setMe: vi.fn(),
    setAuthChecked: vi.fn(),
    setCenter: vi.fn(),
    onError: (message: string) => {
      errors.push(message);
    },
    ...overrides,
  };
  return { deps, errors };
}

describe("session", () => {
  it("loads me and marks auth checked", async () => {
    const { deps } = makeDeps();
    await createSessionRefresher(deps).refreshMe();
    expect(deps.client.me).toHaveBeenCalledOnce();
    expect(deps.setMe).toHaveBeenCalledWith({
      id: "u1",
      username: "wren",
      is_admin: false,
      cooldown_until: null,
    });
    expect(deps.setAuthChecked).toHaveBeenCalledWith(true);
  });

  it("reads a failed me as signed out and toasts", async () => {
    const { deps, errors } = makeDeps({
      client: {
        me: () => Promise.reject(new Error("down")),
        center: () => Promise.resolve({ current: null, all_time: [] }),
      },
    });
    await createSessionRefresher(deps).refreshMe();
    expect(deps.setMe).toHaveBeenCalledWith(null);
    expect(deps.setAuthChecked).toHaveBeenCalledWith(true);
    expect(errors).toEqual(["Could not reach the server"]);
  });

  it("stores the center holder", async () => {
    const { deps } = makeDeps();
    await createSessionRefresher(deps).refreshCenter();
    expect(deps.setCenter).toHaveBeenCalledWith(CENTER);
  });

  it("keeps the last holder when the center read fails", async () => {
    const { deps } = makeDeps({
      client: {
        me: () => Promise.resolve(null),
        center: () => Promise.reject(new Error("down")),
      },
    });
    await createSessionRefresher(deps).refreshCenter();
    expect(deps.setCenter).not.toHaveBeenCalled();
  });
});
