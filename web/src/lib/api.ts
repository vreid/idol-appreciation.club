/**
 * Typed client for the idol-appreciation.club API (same-origin, cookie auth).
 * Pure data layer: every method takes a fetch implementation so tests can
 * stub it; the app injects globalThis.fetch.
 */

export interface Me {
  id: number;
  username: string;
  is_admin: boolean;
  cooldown_until: string | null;
}

export interface Placement {
  id: number;
  username: string;
  x: number;
  y: number;
  w: number;
  h: number;
  created_at: string;
}

export interface PlacedPlacement {
  id: number;
  x: number;
  y: number;
  w: number;
  h: number;
  seq: number;
}

export interface CenterHolder {
  username: string;
  since: string;
  placement: { id: number; x: number; y: number; w: number; h: number };
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
  register(username: string, password: string): Promise<void>;
  login(username: string, password: string): Promise<void>;
  logout(): Promise<void>;
  /** null when not signed in (401/403). */
  me(): Promise<Me | null>;
  place(req: PlaceRequest): Promise<PlacedPlacement>;
  near(x: number, y: number, r: number): Promise<Placement[]>;
  center(): Promise<CenterInfo>;
  report(placementId: number): Promise<void>;
  removePlacement(placementId: number): Promise<void>;
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

function createApiClient(deps: ApiClientDeps = {}): ApiClient {
  const doFetch = deps.fetch ?? fetch;

  async function request(
    method: string,
    url: string,
    body?: BodyInit,
    headers?: Record<string, string>,
  ): Promise<{ status: number; body: unknown; response: Response }> {
    let response: Response;
    try {
      response = await doFetch(url, {
        method,
        credentials: "same-origin",
        headers,
        body,
      });
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
    return { status: response.status, body: parsed, response };
  }

  function postJson(url: string, payload: unknown): Promise<void> {
    return request("POST", url, JSON.stringify(payload), {
      "Content-Type": "application/json",
    }).then(() => undefined);
  }

  function expectObject(body: unknown): Record<string, unknown> {
    if (typeof body !== "object" || body === null) {
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

  return {
    register: (username, password) =>
      postJson("/api/auth/register", { username, password }),
    login: (username, password) =>
      postJson("/api/auth/login", { username, password }),
    logout: () => postJson("/api/auth/logout", {}),
    me: async () => {
      try {
        const { body } = await request("GET", "/api/auth/me");
        return body as Me;
      } catch (cause) {
        if (
          cause instanceof ApiError &&
          (cause.status === 401 || cause.status === 403)
        ) {
          return null;
        }
        throw cause;
      }
    },
    place: async ({ image, fileName, x, y, size }: PlaceRequest) => {
      const form = new FormData();
      form.append("image", image, fileName);
      form.append("x", String(x));
      form.append("y", String(y));
      form.append("size", String(size));
      const { body } = await request("POST", "/api/place", form);
      const obj = expectObject(body);
      return {
        id: num(obj, "id"),
        x: num(obj, "x"),
        y: num(obj, "y"),
        w: num(obj, "w"),
        h: num(obj, "h"),
        seq: num(obj, "seq"),
      };
    },
    near: async (x, y, r) => {
      const query = new URLSearchParams({
        x: String(x),
        y: String(y),
        r: String(r),
      });
      const { body } = await request(
        "GET",
        `/api/placements/near?${query.toString()}`,
      );
      if (!Array.isArray(body)) {
        throw new ApiError(0, "expected an array of placements");
      }
      return body.map((raw) => {
        const obj = expectObject(raw);
        return {
          id: num(obj, "id"),
          username: str(obj, "username"),
          x: num(obj, "x"),
          y: num(obj, "y"),
          w: num(obj, "w"),
          h: num(obj, "h"),
          created_at: str(obj, "created_at"),
        };
      });
    },
    center: async () => {
      const { body } = await request("GET", "/api/center");
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
            id: num(placement, "id"),
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
      const all_time = rawAllTime.map((raw) => {
        const entry = expectObject(raw);
        return {
          username: str(entry, "username"),
          seconds: num(entry, "seconds"),
        };
      });
      return { current, all_time };
    },
    report: (placementId) =>
      request(
        "POST",
        "/api/reports",
        JSON.stringify({ placement_id: placementId }),
        {
          "Content-Type": "application/json",
        },
      ).then(() => undefined),
    removePlacement: (placementId) =>
      request("DELETE", `/api/admin/placements/${placementId}`).then(
        () => undefined,
      ),
  };
}

export { createApiClient };
