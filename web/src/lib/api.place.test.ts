import { describe, expect, it } from "vite-plus/test";

import { ApiError, createApiClient } from "./api";

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

describe("place", () => {
  it("sends multipart fields x, y, size plus the image file (202 era)", async () => {
    const mock = fetchMock(() =>
      jsonResponse(202, {
        id: "0197d3a4-8f2b-7c11-b3a1-5d2e9f60a102",
        state: "pending",
        x: 1,
        y: 2,
        w: 32,
        h: 16,
        rot: 45,
      }),
    );
    const api = createApiClient({ fetch: mock.fetch });
    const image = new Blob(["abc"], { type: "image/webp" });
    const outcome = await api.place({
      image,
      fileName: "wiz.webp",
      x: 134217664,
      y: 134217600,
      size: 32,
      rot: 45,
    });
    const init = mock.calls[0]?.init;
    expect(mock.calls[0]?.url).toBe("/api/place");
    expect(init?.method).toBe("POST");
    expect(init?.headers).toBeUndefined();
    const form = init?.body;
    if (!(form instanceof FormData)) throw new Error("expected FormData");
    expect(form.get("x")).toBe("134217664");
    expect(form.get("y")).toBe("134217600");
    expect(form.get("size")).toBe("32");
    expect(form.get("rot")).toBe("45");
    const file = form.get("image");
    if (!(file instanceof File)) throw new Error("expected File");
    expect(file.name).toBe("wiz.webp");
    expect(outcome).toEqual({
      era: "quarantine",
      accepted: {
        id: "0197d3a4-8f2b-7c11-b3a1-5d2e9f60a102",
        state: "pending",
        x: 1,
        y: 2,
        w: 32,
        h: 16,
        rot: 45,
      },
    });
  });

  it("parses a 201-era synchronous recording as the sync outcome", async () => {
    const mock = fetchMock(() =>
      jsonResponse(201, {
        id: "p1",
        x: 1,
        y: 2,
        w: 3,
        h: 4,
        rot: 0,
        seq: 7,
      }),
    );
    const api = createApiClient({ fetch: mock.fetch });
    const outcome = await api.place({
      image: new Blob(),
      fileName: "a.png",
      x: 1,
      y: 2,
      size: 3,
      rot: 0,
    });
    expect(outcome).toEqual({
      era: "sync",
      placement: { id: "p1", x: 1, y: 2, w: 3, h: 4, rot: 0, seq: 7 },
    });
  });

  it("turns 429 into ApiError carrying Retry-After", async () => {
    const mock = fetchMock(() =>
      jsonResponse(429, { error: "cooldown" }, { "Retry-After": "42" }),
    );
    const api = createApiClient({ fetch: mock.fetch });
    const err = await api
      .place({
        image: new Blob(),
        fileName: "a.png",
        x: 0,
        y: 0,
        size: 16,
        rot: 0,
      })
      .catch((e: unknown) => e);
    expect(err).toBeInstanceOf(ApiError);
    expect(err).toMatchObject({ status: 429, retryAfterSeconds: 42 });
  });

  it("maps 400 validation to ApiError with server message", async () => {
    const mock = fetchMock(() => jsonResponse(400, { error: "bad size" }));
    const api = createApiClient({ fetch: mock.fetch });
    await expect(
      api.place({
        image: new Blob(),
        fileName: "a.png",
        x: 0,
        y: 0,
        size: 1,
        rot: 0,
      }),
    ).rejects.toMatchObject({ status: 400, message: "bad size" });
  });
});

describe("place errors", () => {
  it("network failure during place surfaces as status 0", async () => {
    const impl = (() =>
      Promise.reject(new TypeError("fetch failed"))) as typeof fetch;
    const api = createApiClient({ fetch: impl });
    await expect(
      api.place({
        image: new Blob(),
        fileName: "a.png",
        x: 0,
        y: 0,
        size: 16,
        rot: 0,
      }),
    ).rejects.toMatchObject({ status: 0 });
  });
});

describe("upload status", () => {
  it("uploadStatus maps 404 to null and parses the states", async () => {
    const bodies: Array<Response | undefined> = [
      jsonResponse(404, { error: "not found" }),
      jsonResponse(200, { state: "pending" }),
      jsonResponse(200, {
        state: "placed",
        id: "p1",
        seq: 5,
        bbox: [1, 2, 3, 4],
      }),
    ];
    let n = 0;
    const mock = fetchMock(() => {
      const body = bodies[n];
      n += 1;
      return body ?? jsonResponse(500, { error: "fixture exhausted" });
    });
    const api = createApiClient({ fetch: mock.fetch });
    await expect(api.uploadStatus("u1")).resolves.toBeNull();
    expect(mock.calls[0]?.url).toBe("/api/uploads/u1");
    await expect(api.uploadStatus("u1")).resolves.toEqual({
      state: "pending",
    });
    await expect(api.uploadStatus("u1")).resolves.toEqual({
      state: "placed",
      id: "p1",
      seq: 5,
      bbox: [1, 2, 3, 4],
    });
  });
});
