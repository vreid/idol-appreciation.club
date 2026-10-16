import { describe, expect, it } from "vite-plus/test";
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
  const init: ResponseInit = { status };
  if (headers !== undefined) {
    init.headers = headers;
  }
  return new Response(body === null ? null : JSON.stringify(body), init);
}

function urlOf(input: RequestInfo | URL): string {
  if (typeof input === "string") return input;
  if (input instanceof URL) return input.href;
  return input.url;
}

function fetchMock(responder: (url: string, init: RequestInit) => Response): {
  calls: RecordedRequest[];
  fetch: typeof fetch;
} {
  const calls: RecordedRequest[] = [];
  const impl: typeof fetch = (input, init) => {
    const url = urlOf(input);
    calls.push({ url, init: init ?? {} });
    return Promise.resolve(responder(url, init ?? {}));
  };
  return { calls, fetch: impl };
}

function jsonBody(request: RecordedRequest | undefined): unknown {
  const body = request?.init.body;
  if (typeof body !== "string") throw new Error("expected a string body");
  return JSON.parse(body);
}

const offlineFetch: typeof fetch = () =>
  Promise.reject(new TypeError("offline"));
const failingFetch: typeof fetch = () => Promise.reject(new Error("x"));

const samplePlacement: Placement = {
  id: "0197d3a4-8f2b-7c11-b3a1-5d2e9f60a101",
  username: "wonyoung",
  x: 134217728,
  y: 134217728,
  w: 256,
  h: 128,
  rot: 0,
  created_at: "2026-10-12T00:00:00Z",
};

describe("auth flows", () => {
  it("register posts JSON credentials and parses pending result", async () => {
    const mock = fetchMock(() =>
      jsonResponse(201, { username: "liz", pending: true }),
    );
    const api = createApiClient({ fetch: mock.fetch });
    const created = await api.register("liz", "pw");
    expect(created).toEqual({ username: "liz", pending: true });
    expect(mock.calls[0]?.url).toBe("/api/auth/register");
    expect(mock.calls[0]?.init.method).toBe("POST");
    expect(mock.calls[0]?.init.credentials).toBe("same-origin");
    expect(mock.calls[0]?.init.headers).toMatchObject({
      "Content-Type": "application/json",
    });
    expect(jsonBody(mock.calls[0])).toEqual({
      username: "liz",
      password: "pw",
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
      id: "0197d3a4-8f2b-7c11-b3a1-5d2e9f60a0e1",
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
        placement: {
          id: "0197d3a4-8f2b-7c11-b3a1-5d2e9f60a105",
          x: 1,
          y: 2,
          w: 3,
          h: 4,
          rot: 0,
        },
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
    await api.report("0197d3a4-8f2b-7c11-b3a1-5d2e9f60a10b");
    expect(mock.calls[0]?.url).toBe("/api/reports");
    expect(jsonBody(mock.calls[0])).toEqual({
      placement_id: "0197d3a4-8f2b-7c11-b3a1-5d2e9f60a10b",
    });
  });

  it("sends DELETE for admin removal", async () => {
    const mock = fetchMock(() => new Response(null, { status: 204 }));
    const api = createApiClient({ fetch: mock.fetch });
    await api.removePlacement("0197d3a4-8f2b-7c11-b3a1-5d2e9f60a10d");
    expect(mock.calls[0]?.url).toBe(
      "/api/admin/placements/0197d3a4-8f2b-7c11-b3a1-5d2e9f60a10d",
    );
    expect(mock.calls[0]?.init.method).toBe("DELETE");
  });

  it("puts tags and parses the authoritative list", async () => {
    const mock = fetchMock(() =>
      jsonResponse(200, {
        id: "0197d3a4-8f2b-7c11-b3a1-5d2e9f60a10b",
        tags: ["stage", "encore"],
      }),
    );
    const api = createApiClient({ fetch: mock.fetch });
    await expect(
      api.updateTags("0197d3a4-8f2b-7c11-b3a1-5d2e9f60a10b", [
        "stage",
        "encore",
      ]),
    ).resolves.toEqual({
      id: "0197d3a4-8f2b-7c11-b3a1-5d2e9f60a10b",
      tags: ["stage", "encore"],
    });
    expect(mock.calls[0]?.url).toBe(
      "/api/placements/0197d3a4-8f2b-7c11-b3a1-5d2e9f60a10b/tags",
    );
    expect(mock.calls[0]?.init.method).toBe("PUT");
    expect(jsonBody(mock.calls[0])).toEqual({ tags: ["stage", "encore"] });
  });

  it("fetches the feed with only the set params", async () => {
    const mock = fetchMock(() =>
      jsonResponse(200, {
        items: [
          {
            id: "0197d3a4-8f2b-7c11-b3a1-5d2e9f60a10b",
            username: "wren",
            seq: 41,
            x: 1,
            y: 2,
            w: 3,
            h: 4,
            rot: 0,
            created_at: "2026-10-13T00:00:00Z",
            tags: ["stage"],
          },
        ],
        next_before: 41,
      }),
    );
    const api = createApiClient({ fetch: mock.fetch });
    await expect(api.feed({})).resolves.toMatchObject({
      next_before: 41,
      items: [{ username: "wren", tags: ["stage"] }],
    });
    expect(mock.calls[0]?.url).toBe("/api/feed");
    await api.feed({ tag: "stage", before: 41, limit: 5 });
    expect(mock.calls[1]?.url).toBe("/api/feed?tag=stage&before=41&limit=5");
  });

  it("rejects a malformed tags response as ApiError 0", async () => {
    const mock = fetchMock(() => jsonResponse(200, { tags: "stage" }));
    const api = createApiClient({ fetch: mock.fetch });
    await expect(
      api.updateTags("0197d3a4-8f2b-7c11-b3a1-5d2e9f60a10b", ["stage"]),
    ).rejects.toMatchObject({ name: "ApiError", status: 0 });
  });

  it("lists pending accounts for admins", async () => {
    const mock = fetchMock(() =>
      jsonResponse(200, [
        { username: "newbie", created_at: "2026-10-12T09:00:00Z" },
      ]),
    );
    const api = createApiClient({ fetch: mock.fetch });
    await expect(api.pendingAccounts()).resolves.toEqual([
      { username: "newbie", created_at: "2026-10-12T09:00:00Z" },
    ]);
    expect(mock.calls[0]?.url).toBe("/api/admin/accounts?pending=true");
  });

  it("posts admin approval for a username", async () => {
    const mock = fetchMock(() => new Response(null, { status: 204 }));
    const api = createApiClient({ fetch: mock.fetch });
    await api.approveAccount("newbie");
    expect(mock.calls[0]?.url).toBe("/api/admin/accounts/newbie/approve");
    expect(mock.calls[0]?.init.method).toBe("POST");
  });
});

describe("transport errors", () => {
  it("maps rejected fetch to status 0 ApiError", async () => {
    const api = createApiClient({ fetch: offlineFetch });
    await expect(api.center()).rejects.toMatchObject({ status: 0 });
  });

  it("network failure during place also surfaces as status 0", async () => {
    const api = createApiClient({ fetch: failingFetch });
    await expect(
      api.place({
        image: new Blob(),
        fileName: "a",
        x: 0,
        y: 0,
        size: 16,
        rot: 0,
      }),
    ).rejects.toMatchObject({ status: 0 });
  });
});
