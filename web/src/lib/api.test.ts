import { describe, expect, it, vi } from "vitest";
import { ApiError, createApiClient, type Placement } from "./api";

interface RecordedRequest {
  url: string;
  init: RequestInit;
}

function jsonResponse(
  status: number,
  body: unknown,
  headers?: Record<string, string>,
): Response {
  const response = new Response(body === null ? null : JSON.stringify(body), {
    status,
    headers,
  });
  return response;
}

function fetchMock(responder: (url: string, init: RequestInit) => Response): {
  calls: RecordedRequest[];
  fetch: typeof fetch;
} {
  const calls: RecordedRequest[] = [];
  const impl = (
    input: RequestInfo | URL,
    init?: RequestInit,
  ): Promise<Response> => {
    calls.push({ url: String(input), init: init ?? {} });
    return Promise.resolve(responder(String(input), init ?? {}));
  };
  return { calls, fetch: impl as typeof fetch };
}

const samplePlacement: Placement = {
  id: 11,
  username: "wonyoung",
  x: 134217728,
  y: 134217728,
  w: 256,
  h: 128,
  created_at: "2026-10-12T00:00:00Z",
};

describe("auth flows", () => {
  it("register posts JSON credentials", async () => {
    const mock = fetchMock(() => jsonResponse(200, { ok: true }));
    const api = createApiClient({ fetch: mock.fetch });
    await api.register("liz", "pw123456");
    expect(mock.calls[0]?.url).toBe("/api/auth/register");
    expect(mock.calls[0]?.init.method).toBe("POST");
    expect(mock.calls[0]?.init.credentials).toBe("same-origin");
    expect(mock.calls[0]?.init.headers).toMatchObject({
      "Content-Type": "application/json",
    });
    expect(JSON.parse(String(mock.calls[0]?.init.body))).toEqual({
      username: "liz",
      password: "pw123456",
    });
  });

  it("login and logout post to their endpoints", async () => {
    const mock = fetchMock(() => jsonResponse(200, {}));
    const api = createApiClient({ fetch: mock.fetch });
    await api.login("rei", "pw");
    await api.logout();
    expect(mock.calls[0]?.url).toBe("/api/auth/login");
    expect(mock.calls[1]?.url).toBe("/api/auth/logout");
    expect(mock.calls[1]?.init.method).toBe("POST");
  });

  it("me() maps 200 to Me and 401 to null", async () => {
    const me = {
      id: 3,
      username: "gaeul",
      is_admin: true,
      cooldown_until: "2026-10-12T10:00:00Z",
    };
    let status = 200;
    const mock = fetchMock(() => jsonResponse(status, me));
    const api = createApiClient({ fetch: mock.fetch });
    await expect(api.me()).resolves.toEqual(me);
    status = 401;
    await expect(api.me()).resolves.toBeNull();
    expect(mock.calls[1]?.url).toBe("/api/auth/me");
  });

  it("me() propagates server errors other than 401/403", async () => {
    const mock = fetchMock(() => jsonResponse(500, { error: "boom" }));
    const api = createApiClient({ fetch: mock.fetch });
    await expect(api.me()).rejects.toMatchObject({ status: 500 });
  });
});

describe("place", () => {
  it("sends multipart fields x, y, size plus the image file", async () => {
    const mock = fetchMock(() =>
      jsonResponse(201, { id: 9, x: 1, y: 2, w: 32, h: 16, seq: 77 }),
    );
    const api = createApiClient({ fetch: mock.fetch });
    const image = new Blob(["abc"], { type: "image/webp" });
    const placed = await api.place({
      image,
      fileName: "wiz.webp",
      x: 134217664,
      y: 134217600,
      size: 32,
    });
    const init = mock.calls[0]?.init;
    expect(mock.calls[0]?.url).toBe("/api/place");
    expect(init?.method).toBe("POST");
    expect(init?.headers).toBeUndefined();
    const form = init?.body as FormData;
    expect(form).toBeInstanceOf(FormData);
    expect(form.get("x")).toBe("134217664");
    expect(form.get("y")).toBe("134217600");
    expect(form.get("size")).toBe("32");
    const file = form.get("image");
    expect(file).toBeInstanceOf(File);
    expect((file as File).name).toBe("wiz.webp");
    expect(placed).toEqual({ id: 9, x: 1, y: 2, w: 32, h: 16, seq: 77 });
  });

  it("turns 429 into ApiError carrying Retry-After", async () => {
    const mock = fetchMock(() =>
      jsonResponse(429, { error: "cooldown" }, { "Retry-After": "42" }),
    );
    const api = createApiClient({ fetch: mock.fetch });
    const err = await api
      .place({ image: new Blob(), fileName: "a.png", x: 0, y: 0, size: 16 })
      .catch((e: unknown) => e);
    expect(err).toBeInstanceOf(ApiError);
    expect(err).toMatchObject({ status: 429, retryAfterSeconds: 42 });
  });

  it("maps 400 validation to ApiError with server message", async () => {
    const mock = fetchMock(() => jsonResponse(400, { error: "bad size" }));
    const api = createApiClient({ fetch: mock.fetch });
    await expect(
      api.place({ image: new Blob(), fileName: "a.png", x: 0, y: 0, size: 1 }),
    ).rejects.toMatchObject({ status: 400, message: "bad size" });
  });
});

describe("near / center", () => {
  it("builds the near query and parses placements", async () => {
    const mock = fetchMock(() => jsonResponse(200, [samplePlacement]));
    const api = createApiClient({ fetch: mock.fetch });
    await expect(api.near(10, 20, 300)).resolves.toEqual([samplePlacement]);
    expect(mock.calls[0]?.url).toBe("/api/placements/near?x=10&y=20&r=300");
  });

  it("rejects a non-array near response", async () => {
    const mock = fetchMock(() => jsonResponse(200, {}));
    const api = createApiClient({ fetch: mock.fetch });
    await expect(api.near(0, 0, 1)).rejects.toBeInstanceOf(ApiError);
  });

  it("parses center with current holder and all-time list", async () => {
    const body = {
      current: {
        username: "yujin",
        since: "2026-10-12T09:00:00Z",
        placement: { id: 5, x: 1, y: 2, w: 3, h: 4 },
      },
      all_time: [{ username: "yujin", seconds: 800 }],
    };
    const mock = fetchMock(() => jsonResponse(200, body));
    const api = createApiClient({ fetch: mock.fetch });
    await expect(api.center()).resolves.toEqual(body);
    expect(mock.calls[0]?.url).toBe("/api/center");
  });

  it("parses center with null current", async () => {
    const mock = fetchMock(() =>
      jsonResponse(200, { current: null, all_time: [] }),
    );
    const api = createApiClient({ fetch: mock.fetch });
    await expect(api.center()).resolves.toEqual({
      current: null,
      all_time: [],
    });
  });
});

describe("report / admin remove", () => {
  it("posts a report with placement_id", async () => {
    const mock = fetchMock(() => new Response(null, { status: 204 }));
    const api = createApiClient({ fetch: mock.fetch });
    await api.report(33);
    expect(mock.calls[0]?.url).toBe("/api/reports");
    expect(JSON.parse(String(mock.calls[0]?.init.body))).toEqual({
      placement_id: 33,
    });
  });

  it("sends DELETE for admin removal", async () => {
    const mock = fetchMock(() => new Response(null, { status: 204 }));
    const api = createApiClient({ fetch: mock.fetch });
    await api.removePlacement(77);
    expect(mock.calls[0]?.url).toBe("/api/admin/placements/77");
    expect(mock.calls[0]?.init.method).toBe("DELETE");
  });
});

describe("transport errors", () => {
  it("maps rejected fetch to status 0 ApiError", async () => {
    const boom = (() =>
      Promise.reject(new TypeError("offline"))) as typeof fetch;
    const api = createApiClient({ fetch: boom });
    await expect(api.center()).rejects.toMatchObject({ status: 0 });
  });

  it("network failure during place also surfaces as status 0", async () => {
    const api = createApiClient({
      fetch: vi.fn(() =>
        Promise.reject(new Error("x")),
      ) as unknown as typeof fetch,
    });
    await expect(
      api.place({ image: new Blob(), fileName: "a", x: 0, y: 0, size: 16 }),
    ).rejects.toMatchObject({ status: 0 });
  });
});
