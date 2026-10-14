import type { ApiClient, PendingAccount } from "./api";
import { describeApiError } from "./apiErrors";

/** The admin account-approval queue, extracted from the store. */
export interface AdminAccountsDeps {
  client: Pick<ApiClient, "pendingAccounts" | "approveAccount">;
  toast(kind: "error" | "success", message: string): void;
  setPending(list: PendingAccount[]): void;
  removePending(username: string): void;
}

export interface AdminAccounts {
  loadPending(): Promise<void>;
  approve(username: string): Promise<void>;
}

export function createAdminAccounts(deps: AdminAccountsDeps): AdminAccounts {
  return {
    async loadPending() {
      try {
        deps.setPending(await deps.client.pendingAccounts());
      } catch (err) {
        deps.toast("error", describeApiError(err));
      }
    },
    async approve(username) {
      try {
        await deps.client.approveAccount(username);
        deps.removePending(username);
        deps.toast("success", `Approved ${username}`);
      } catch (err) {
        deps.toast("error", describeApiError(err));
      }
    },
  };
}
