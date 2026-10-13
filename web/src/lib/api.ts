/**
 * Typed client for the idol-appreciation.club API (same-origin, cookie auth).
 * Pure data layer: every method takes a fetch implementation so tests can
 * stub it; the app injects globalThis.fetch.
 *
 * Response shapes are parsed with the wire-contract schemas (contract/zod,
 * imported as `@contract`) the moment data enters the app. A malformed
 * payload throws ApiError(0, …) after a console warning — callers convert
 * that into toasts, never a crash.
 */

import type {
  CenterInfo,
  DigStack,
  Me,
  PendingAccount,
  Placement,
  PlacedPlacement,
  RegisterResult,
} from "@contract";
import {
  centerInfoSchema,
  digStackSchema,
  meSchema,
  pendingAccountsSchema,
  placementsNearSchema,
  placedPlacementSchema,
  registerResultSchema,
} from "@contract";

export type {
  CenterHolder,
  CenterInfo,
  DigEntry,
  DigStack,
  Me,
  PendingAccount,
  Placement,
  PlacedPlacement,
  RegisterResult,
} from "@contract";

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
  /** Top-left corner of the unrotated rect in native world px. */
  x: number;
  y: number;
  /** Long edge in px, 16..4096. */
  size: number;
  /** Clockwise rotation in degrees (free angle, default 0). */
  rot: number;
}

export interface ApiClient {
  register: (username: string, password: string) => Promise<RegisterResult>;
  login: (username: string, password: string) => Promise<void>;
  logout: () => Promise<void>;
  /** null when not signed in (401/403). */
  me: () => Promise<Me | null>;
  place: (req: PlaceRequest) => Promise<PlacedPlacement>;
  near: (x: number, y: number, r: number) => Promise<Placement[]>;
  /** All placements whose rotated pixels contain the world point,
   * newest-first with 1-based depths. */
  dig: (x: number, y: number) => Promise<DigStack>;
  center: () => Promise<CenterInfo>;
  report: (placementId: string) => Promise<void>;
  removePlacement: (placementId: string) => Promise<void>;
  pendingAccounts: () => Promise<PendingAccount[]>;
  approveAccount: (username: string) => Promise<void>;
}

export interface ApiClientDeps {
  fetch?: typeof fetch;
  /**
   * Prefix for every request path. The browser keeps "" (same-origin
   * relative URLs); the contract integration suite passes the booted
   * backend's origin.
   */
  base?: string;
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

/** Structural shape of a schema at the parse boundary (zod-agnostic). */
interface Parses<T> {
  safeParse(
    value: unknown,
  ): { success: true; data: T } | { success: false; error: unknown };
}

/** Contract-boundary parse: schema or ApiError(0), with a loud warning. */
function parseOk<T>(schema: Parses<T>, body: unknown, label: string): T {
  const parsed = schema.safeParse(body);
  if (parsed.success) return parsed.data;
  console.warn(`[api] malformed ${label} payload`, parsed.error);
  throw new ApiError(0, `unexpected ${label} response`);
}

interface RequestOptions {
  method?: string;
  body?: BodyInit;
  json?: boolean;
}

export function createApiClient(deps: ApiClientDeps = {}): ApiClient {
  const doFetch = deps.fetch ?? fetch;
  const base = deps.base ?? "";

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
      response = await doFetch(base + url, init);
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
      parseOk(
        registerResultSchema,
        await request("/api/auth/register", {
          method: "POST",
          json: true,
          body: JSON.stringify({ username, password }),
        }),
        "register",
      ),
    login: (username, password) =>
      postJson("/api/auth/login", { username, password }),
    logout: () => postJson("/api/auth/logout", {}),
    me: async () => {
      try {
        return parseOk(meSchema, await request("/api/auth/me"), "me");
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
      form.append("rot", String(req.rot));
      return parseOk(
        placedPlacementSchema,
        await request("/api/place", { method: "POST", body: form }),
        "place",
      );
    },
    near: async (x, y, r) => {
      const query = new URLSearchParams({
        x: String(x),
        y: String(y),
        r: String(r),
      });
      return parseOk(
        placementsNearSchema,
        await request(`/api/placements/near?${query}`),
        "placements/near",
      );
    },
    dig: async (x, y) => {
      const query = new URLSearchParams({ x: String(x), y: String(y) });
      return parseOk(digStackSchema, await request(`/api/dig?${query}`), "dig");
    },
    center: async () =>
      parseOk(centerInfoSchema, await request("/api/center"), "center"),
    report: (placementId) =>
      postJson("/api/reports", { placement_id: placementId }),
    pendingAccounts: async () =>
      parseOk(
        pendingAccountsSchema,
        await request("/api/admin/accounts?pending=true"),
        "pending accounts",
      ),
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
