import type { ApiClient, Me } from "./api";

/** Session refresh against /api/auth/me and /api/center, extracted from
 * the store. Injected setters keep this free of Solid imports. */
export interface SessionRefresherDeps {
  client: Pick<ApiClient, "me" | "center">;
  setMe(me: Me | null): void;
  setAuthChecked(checked: boolean): void;
  setCenter(center: Awaited<ReturnType<ApiClient["center"]>> | null): void;
  onError(message: string): void;
}

export interface SessionRefresher {
  /** Re-reads the session; null (401/403) means signed out. */
  refreshMe(): Promise<void>;
  /** Re-reads the center holder; keeps the last known holder on
   * failure — the next poll may succeed. */
  refreshCenter(): Promise<void>;
}

export function createSessionRefresher(
  deps: SessionRefresherDeps,
): SessionRefresher {
  return {
    async refreshMe() {
      try {
        deps.setMe(await deps.client.me());
      } catch {
        deps.setMe(null);
        deps.onError("Could not reach the server");
      } finally {
        deps.setAuthChecked(true);
      }
    },
    async refreshCenter() {
      try {
        deps.setCenter(await deps.client.center());
      } catch {
        /* keep the last known holder; the next poll may succeed */
      }
    },
  };
}
