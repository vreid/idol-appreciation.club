/**
 * Typed client for the idol-appreciation.club API (same-origin, cookie auth).
 * Pure data layer: every method takes a fetch implementation so tests can
 * stub it; the app injects globalThis.fetch.
 */

import { isRecord } from "./guards";

export interface Me {
  id: string;
  username: string;
  is_admin: boolean;
  cooldown_until: string | null;
}

export interface RegisterResult {
  username: string;
  pending: boolean;
}

export interface PendingAccount {
  username: string;
  created_at: string;
}

export interface Placement {
  id: string;
  username: string;
  x: number;
  y: number;
  w: number;
  h: number;
  created_at: string;
}

export interface PlacedPlacement {
  id: string;
  x: number;
  y: number;
  w: number;
  h: number;
  seq: number;
}

export interface CenterHolder {
  username: string;
  since: string;
  placement: { id: string; x: number; y: number; w: number; h: number };
}

export interface CenterInfo {
  current: CenterHolder | null;
  all_time: { username: string; seconds: number }[];
}

export const MAX_UPLOAD_BYTES = 10 * 1024 * 1024;

export class ApiError extends Error {
  readonly status: number;
  /** Seconds from a Retry-After header, when present. */
  readonly retryAfterSeconds: number | null;

  constructor(
    status: number,
    message: string,
    retryAfterSeconds: number | null = null,
  ) {
    super(message);
    this.name = "ApiError";
    this.status = status;
    this.retryAfterSeconds = retryAfterSeconds;
  }
}

export interface PlaceRequest {
  image: Blob;
  fileName: string;
  /** Top-left corner in native world px. */
  x: number;
  y: number;
  /** Long edge in px, 16..2048. */
  size: number;
}

export interface ApiClient {
  register: (username: string, password: string) => Promise<RegisterResult>;
  login: (username: string, password: string) => Promise<void>;
  logout: () => Promise<void>;
  /** null when not signed in (401/403). */
  me: () => Promise<Me | null>;
  place: (req: PlaceRequest) => Promise<PlacedPlacement>;
  near: (x: number, y: number, r: number) => Promise<Placement[]>;
  center: () => Promise<CenterInfo>;
  report: (placementId: string) => Promise<void>;
  removePlacement: (placementId: string) => Promise<void>;
  pendingAccounts: () => Promise<PendingAccount[]>;
  approveAccount: (username: string) => Promise<void>;
}

export interface ApiClientDeps {
  fetch?: typeof fetch;
}

function retryAfterOf(response: Response): number | null {
  const raw = response.headers.get("Retry-After");
  if (raw === null) return null;
  const seconds = Number(raw);
  return Number.isFinite(seconds) ? seconds : null;
}

async function parseBody(response: Response): Promise<unknown> {
  const text = await response.text();
  if (text.length === 0) return null;
  try {
    return JSON.parse(text) as unknown;
  } catch {
    return null;
  }
}

function messageOf(body: unknown, status: number): string {
  if (
    typeof body === "object" &&
    body !== null &&
    "error" in body &&
    typeof body.error === "string"
  ) {
    return body.error;
  }
  return `HTTP ${status}`;
}

function expectObject(body: unknown): Record<string, unknown> {
  if (!isRecord(body)) {
    throw new ApiError(0, "unexpected response body");
  }
  return body;
}

function num(obj: Record<string, unknown>, key: string): number {
  const value = obj[key];
  if (typeof value !== "number") {
    throw new ApiError(0, `missing number field "${key}"`);
  }
  return value;
}

function str(obj: Record<string, unknown>, key: string): string {
  const value = obj[key];
  if (typeof value !== "string") {
    throw new ApiError(0, `missing string field "${key}"`);
  }
  return value;
}

function bool(obj: Record<string, unknown>, key: string): boolean {
  const value = obj[key];
  if (typeof value !== "boolean") {
    throw new ApiError(0, `missing boolean field "${key}"`);
  }
  return value;
}

function nullableStr(obj: Record<string, unknown>, key: string): string | null {
  const value = obj[key];
  if (value !== null && typeof value !== "string") {
    throw new ApiError(0, `missing nullable string field "${key}"`);
  }
  return value;
}

interface RequestOptions {
  method?: string;
  body?: BodyInit;
  json?: boolean;
}

function placementOf(body: unknown): PlacedPlacement {
  const obj = expectObject(body);
  return {
    id: str(obj, "id"),
    x: num(obj, "x"),
    y: num(obj, "y"),
    w: num(obj, "w"),
    h: num(obj, "h"),
    seq: num(obj, "seq"),
  };
}

function placementFields(obj: Record<string, unknown>): Placement {
  return {
    id: str(obj, "id"),
    username: str(obj, "username"),
    x: num(obj, "x"),
    y: num(obj, "y"),
    w: num(obj, "w"),
    h: num(obj, "h"),
    created_at: str(obj, "created_at"),
  };
}

function registerOf(body: unknown): RegisterResult {
  const obj = expectObject(body);
  return {
    username: str(obj, "username"),
    pending: bool(obj, "pending"),
  };
}

function meOf(body: unknown): Me {
  const obj = expectObject(body);
  return {
    id: str(obj, "id"),
    username: str(obj, "username"),
    is_admin: bool(obj, "is_admin"),
    cooldown_until: nullableStr(obj, "cooldown_until"),
  };
}

function pendingAccountsOf(body: unknown): PendingAccount[] {
  if (!Array.isArray(body)) {
    throw new ApiError(0, "expected an array of pending accounts");
  }
  return body.map((entry) => {
    const obj = expectObject(entry);
    return {
      username: str(obj, "username"),
      created_at: str(obj, "created_at"),
    };
  });
}

function placementsNearOf(body: unknown): Placement[] {
  if (!Array.isArray(body)) {
    throw new ApiError(0, "expected an array of placements");
  }
  return body.map((raw) => placementFields(expectObject(raw)));
}

function centerOf(body: unknown): CenterInfo {
  const obj = expectObject(body);
  const rawCurrent = obj["current"];
  let current: CenterHolder | null = null;
  if (rawCurrent !== null && rawCurrent !== undefined) {
    const cur = expectObject(rawCurrent);
    const placement = expectObject(cur["placement"]);
    current = {
      username: str(cur, "username"),
      since: str(cur, "since"),
      placement: {
        id: str(placement, "id"),
        x: num(placement, "x"),
        y: num(placement, "y"),
        w: num(placement, "w"),
        h: num(placement, "h"),
      },
    };
  }
  const rawAllTime = obj["all_time"];
  if (!Array.isArray(rawAllTime)) {
    throw new ApiError(0, 'missing "all_time" array');
  }
  return {
    current,
    all_time: rawAllTime.map((raw) => {
      const entry = expectObject(raw);
      return {
        username: str(entry, "username"),
        seconds: num(entry, "seconds"),
      };
    }),
  };
}

export function createApiClient(deps: ApiClientDeps = {}): ApiClient {
  const doFetch = deps.fetch ?? fetch;

  async function request(
    url: string,
    options: RequestOptions = {},
  ): Promise<unknown> {
    const init: RequestInit = {
      method: options.method ?? "GET",
      credentials: "same-origin",
    };
    if (options.json === true) {
      init.headers = { "Content-Type": "application/json" };
    }
    if (options.body !== undefined) {
      init.body = options.body;
    }
    let response: Response;
    try {
      response = await doFetch(url, init);
    } catch (cause) {
      throw new ApiError(0, `network error: ${String(cause)}`);
    }
    const parsed = await parseBody(response);
    if (!response.ok) {
      throw new ApiError(
        response.status,
        messageOf(parsed, response.status),
        retryAfterOf(response),
      );
    }
    return parsed;
  }

  async function postJson(url: string, payload: unknown): Promise<void> {
    await request(url, {
      method: "POST",
      json: true,
      body: JSON.stringify(payload),
    });
  }

  return {
    register: async (username, password) =>
      registerOf(
        await request("/api/auth/register", {
          method: "POST",
          json: true,
          body: JSON.stringify({ username, password }),
        }),
      ),
    login: (username, password) =>
      postJson("/api/auth/login", { username, password }),
    logout: () => postJson("/api/auth/logout", {}),
    me: async () => {
      try {
        return meOf(await request("/api/auth/me"));
      } catch (cause) {
        if (cause instanceof ApiError && [401, 403].includes(cause.status)) {
          return null;
        }
        throw cause;
      }
    },
    place: async (req) => {
      const form = new FormData();
      form.append("image", req.image, req.fileName);
      form.append("x", String(req.x));
      form.append("y", String(req.y));
      form.append("size", String(req.size));
      return placementOf(
        await request("/api/place", { method: "POST", body: form }),
      );
    },
    near: async (x, y, r) => {
      const query = new URLSearchParams({
        x: String(x),
        y: String(y),
        r: String(r),
      });
      return placementsNearOf(await request(`/api/placements/near?${query}`));
    },
    center: async () => centerOf(await request("/api/center")),
    report: (placementId) =>
      postJson("/api/reports", { placement_id: placementId }),
    pendingAccounts: async () =>
      pendingAccountsOf(await request("/api/admin/accounts?pending=true")),
    approveAccount: (username) =>
      postJson(
        `/api/admin/accounts/${encodeURIComponent(username)}/approve`,
        {},
      ),
    removePlacement: async (placementId) => {
      await request(`/api/admin/placements/${placementId}`, {
        method: "DELETE",
      });
    },
  };
}
