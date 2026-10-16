import type { ApiClient, Me } from "./api";
import { ApiError } from "./api";
import { describeApiError } from "./apiErrors";

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

/** The authenticate flow behind the store's login/register actions:
 * pending registrations (approval gate), inline error surfacing, the
 * session refresh and the welcome toasts. Extracted from the store to
 * keep it under the file cap; injected setters keep it Solid-free. */
export interface AuthFlowDeps {
  client: Pick<ApiClient, "login" | "register" | "me">;
  setMe(me: Me | null): void;
  setRegistrationPending(pending: boolean): void;
  setAuthError(message: string): void;
  setAuthPrompt(open: boolean): void;
  pushToast(kind: "error" | "info" | "success", message: string): void;
  refreshMe(): Promise<void>;
  /** The signed-in username AFTER the refresh (null before). */
  username: () => string | null;
}

export interface AuthFlow {
  authenticate(
    action: "login" | "register",
    username: string,
    password: string,
  ): Promise<boolean>;
}

export function createAuthFlow(deps: AuthFlowDeps): AuthFlow {
  return {
    async authenticate(action, username, password) {
      deps.setRegistrationPending(false);
      deps.setAuthError("");
      try {
        if (action === "login") {
          await deps.client.login(username, password);
        } else {
          const created = await deps.client.register(username, password);
          if (created.pending) {
            // External approval gate: no session yet, account awaits
            // an admin.
            deps.setRegistrationPending(true);
            deps.pushToast("info", "Account created — awaiting admin approval");
            return false;
          }
        }
      } catch (err) {
        // While authenticating, an error status IS the answer: surface
        // the server's words inline (e.g. 403 "account pending
        // approval").
        const message =
          err instanceof ApiError ? err.message : describeApiError(err);
        deps.setAuthError(message);
        deps.pushToast("error", message);
        return false;
      }
      await deps.refreshMe();
      deps.setAuthPrompt(false);
      const who = deps.username() ?? username;
      const verb =
        action === "login" ? "Welcome back" : "Account created, welcome";
      deps.pushToast("success", `${verb}, ${who}!`);
      return true;
    },
  };
}
