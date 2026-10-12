import type { APIRequestContext } from "@playwright/test";

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
  const login = await request.post("/api/auth/login", {
    data: { username, password: BOT_PASSWORD },
  });
  if (!login.ok()) {
    throw new Error(`bot login failed: HTTP ${login.status()} for ${username}`);
  }
  return username;
}

function expectRegisterAcceptable(status: number): void {
  if (status >= 200 && status < 300) return;
  if (status === 409) return;
  throw new Error(`bot register failed unexpectedly: HTTP ${status}`);
}
