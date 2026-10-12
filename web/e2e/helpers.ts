import {
  test,
  type APIRequestContext,
  type APIResponse,
} from "@playwright/test";

/** Stable per-run identity so retries/reruns reuse one bot account. */
export function uniqueBotName(): string {
  const runId = process.env.GITHUB_RUN_ID ?? Date.now().toString(36);
  return `e2e_bot_${runId}`;
}

export const BOT_PASSWORD = "e2e-bot-password-0nly-testing";

/**
 * True only when the live backend answers /api/health with {ok: true}.
 * Any failure (missing route, non-JSON, error status) means "not ready".
 */
export async function backendHealthy(
  request: APIRequestContext,
): Promise<boolean> {
  try {
    const res = await request.get("/api/health");
    if (!res.ok()) return false;
    const body: unknown = await res.json();
    if (typeof body !== "object" || body === null) return false;
    if (!("ok" in body)) return false;
    return body.ok === true;
  } catch {
    return false;
  }
}

export const SKIP_UNHEALTHY =
  "backend /api/health is not ok:true — skipping live E2E";

/** Registers (or reuses) the bot account and logs in. Returns the username. */
export async function registerAndLogin(
  request: APIRequestContext,
): Promise<string> {
  const username = uniqueBotName();
  const register = await request.post("/api/auth/register", {
    data: { username, password: BOT_PASSWORD },
  });
  // 409 on a rerun with the same run id: the account already exists.
  expectRegisterAcceptable(register.status());
  let login = await request.post("/api/auth/login", {
    data: { username, password: BOT_PASSWORD },
  });
  if (login.status() === 403) {
    // Approval gate active: try to self-serve via admin creds, else skip.
    login = await approveBotAndRetryLogin(request, username);
  }
  if (!login.ok()) {
    throw new Error(`bot login failed: HTTP ${login.status()} for ${username}`);
  }
  return username;
}

/**
 * When the backend gates registrations behind admin approval, a bot login
 * returns 403 "pending". With ADMIN_USERNAME/ADMIN_PASSWORD set (CI), approve
 * the bot and retry. Without creds the running test is SKIPPED with an explicit
 * reason, so local dev runs stay green regardless of gate state.
 */
async function approveBotAndRetryLogin(
  request: APIRequestContext,
  username: string,
): Promise<APIResponse> {
  const adminUser = process.env.ADMIN_USERNAME;
  const adminPass = process.env.ADMIN_PASSWORD;
  if (adminUser === undefined || adminPass === undefined) {
    test.skip(true, "approval gate active; no admin creds for bot approval");
    // test.skip aborts the running test; this keeps TypeScript happy.
    throw new Error("unreachable: test.skip aborted the test");
  }
  const adminLogin = await request.post("/api/auth/login", {
    data: { username: adminUser, password: adminPass },
  });
  if (!adminLogin.ok()) {
    throw new Error(
      `admin login for bot approval failed: HTTP ${adminLogin.status()}`,
    );
  }
  const pending = await request.get("/api/admin/accounts?pending=true");
  if (!pending.ok()) {
    throw new Error(
      `pending-accounts listing failed: HTTP ${pending.status()}`,
    );
  }
  const list: unknown = await pending.json();
  if (!Array.isArray(list)) {
    throw new Error("pending-accounts response was not an array");
  }
  const row = list.find(
    (entry) =>
      typeof entry === "object" &&
      entry !== null &&
      (entry as Record<string, unknown>)["username"] === username,
  );
  if (row === undefined) {
    throw new Error(`bot ${username} not found in pending-accounts list`);
  }
  const approve = await request.post(
    `/api/admin/accounts/${encodeURIComponent(username)}/approve`,
    { data: {} },
  );
  if (!approve.ok()) {
    throw new Error(`bot approval failed: HTTP ${approve.status()}`);
  }
  return request.post("/api/auth/login", {
    data: { username, password: BOT_PASSWORD },
  });
}

function expectRegisterAcceptable(status: number): void {
  if (status >= 200 && status < 300) return;
  if (status === 409) return;
  throw new Error(`bot register failed unexpectedly: HTTP ${status}`);
}
