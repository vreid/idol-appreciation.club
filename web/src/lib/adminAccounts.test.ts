import { describe, expect, it, vi } from "vite-plus/test";
import type { ApiClient, PendingAccount } from "./api";
import { createAdminAccounts } from "./adminAccounts";

function makeDeps(
  overrides: Partial<Parameters<typeof createAdminAccounts>[0]> = {},
) {
  const toasts: string[] = [];
  let pending: PendingAccount[] = [
    { username: "newbie", created_at: "2026-10-13T00:00:00Z" },
  ];
  const deps = {
    client: {
      pendingAccounts: vi.fn(() =>
        Promise.resolve([
          { username: "queued", created_at: "2026-10-13T01:00:00Z" },
        ]),
      ),
      approveAccount: vi.fn(() => Promise.resolve()),
    } satisfies Pick<ApiClient, "pendingAccounts" | "approveAccount">,
    toast: (kind: "error" | "success", message: string) => {
      toasts.push(`${kind}:${message}`);
    },
    setPending: (list: PendingAccount[]) => {
      pending = list;
    },
    removePending: (username: string) => {
      pending = pending.filter((account) => account.username !== username);
    },
    ...overrides,
  };
  return { deps, toasts, pendingList: () => pending };
}

describe("adminAccounts", () => {
  it("loads the pending queue", async () => {
    const { deps, pendingList } = makeDeps();
    await createAdminAccounts(deps).loadPending();
    expect(pendingList()[0]?.username).toBe("queued");
  });

  it("toasts a failed queue load and keeps the old list", async () => {
    const { deps, toasts, pendingList } = makeDeps({
      client: {
        pendingAccounts: () => Promise.reject(new Error("down")),
        approveAccount: () => Promise.resolve(),
      },
    });
    await createAdminAccounts(deps).loadPending();
    expect(pendingList()[0]?.username).toBe("newbie");
    expect(toasts).toHaveLength(1);
    expect(toasts[0]).toMatch(/^error:/u);
  });

  it("approves and drops the account from the queue", async () => {
    const { deps, pendingList } = makeDeps();
    await createAdminAccounts(deps).approve("newbie");
    expect(deps.client.approveAccount).toHaveBeenCalledWith("newbie");
    expect(pendingList()).toEqual([]);
  });

  it("toasts a failed approval and keeps the account queued", async () => {
    const { deps, toasts, pendingList } = makeDeps({
      client: {
        pendingAccounts: () => Promise.resolve([]),
        approveAccount: () => Promise.reject(new Error("nope")),
      },
    });
    await createAdminAccounts(deps).approve("newbie");
    expect(pendingList()[0]?.username).toBe("newbie");
    expect(toasts).toHaveLength(1);
    expect(toasts[0]).toMatch(/^error:/u);
  });
});
