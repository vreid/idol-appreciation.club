// @vitest-environment happy-dom
import { cleanup, fireEvent, render, screen } from "@solidjs/testing-library";
import { afterEach, describe, expect, it, vi } from "vite-plus/test";
import type { ApiClient, Feed, Placement } from "../api";
import FeedPanel from "./FeedPanel";

afterEach(() => {
  cleanup();
});

const PAGE: Feed = {
  items: [
    {
      id: "p2",
      username: "noor",
      seq: 41,
      x: 8,
      y: 8,
      w: 64,
      h: 48,
      rot: 0,
      created_at: "2026-10-13T12:00:00Z",
      tags: ["encore"],
    },
    {
      id: "p1",
      username: "wren",
      seq: 40,
      x: 0,
      y: 0,
      w: 32,
      h: 32,
      rot: 15,
      created_at: "2026-10-13T11:00:00Z",
      tags: [],
    },
  ],
  next_before: 40,
};

function makeClient(page: Feed = PAGE): ApiClient {
  return {
    register: vi.fn(),
    login: vi.fn(),
    logout: vi.fn(),
    me: vi.fn(),
    place: vi.fn(),
    near: vi.fn(),
    center: vi.fn(),
    report: vi.fn(),
    removePlacement: vi.fn(),
    updateTags: vi.fn(),
    feed: vi.fn(() => Promise.resolve(page)),
    dig: vi.fn(() =>
      Promise.resolve({ stack: [], total: 0, truncated: false }),
    ),
    hot: vi.fn(() =>
      Promise.resolve({ window_hours: 24, total: 0, cells: [] }),
    ),
    pendingAccounts: vi.fn(),
    approveAccount: vi.fn(),
  };
}

function mount(
  client: ApiClient = makeClient(),
  onJump = vi.fn((_place: Placement) => {}),
) {
  render(() => (
    <FeedPanel client={client} onClose={() => {}} onJump={onJump} />
  ));
  return { client, onJump };
}

describe("FeedPanel", () => {
  it("loads and lists entries newest-first", async () => {
    mount();
    await vi.waitFor(() => {
      expect(screen.getByText("@noor")).toBeTruthy();
    });
    expect(screen.getByText("@wren")).toBeTruthy();
    expect(screen.getByText("encore")).toBeTruthy();
    expect(screen.getByText("Older")).toBeTruthy();
  });

  it("shows an empty note without entries", async () => {
    mount(
      makeClient({
        items: [],
        next_before: null,
      }),
    );
    await vi.waitFor(() => {
      expect(screen.getByText(/Nothing here yet/u)).toBeTruthy();
    });
    expect(screen.queryByText("Older")).toBeNull();
  });

  it("jumps with a plain Placement (seq cursor stripped)", async () => {
    const onJump = vi.fn((_place: Placement) => {});
    mount(makeClient(), onJump);
    await vi.waitFor(() => {
      expect(screen.getByText("@wren")).toBeTruthy();
    });
    fireEvent.click(screen.getByText("@wren"));
    expect(onJump).toHaveBeenCalledTimes(1);
    const arg: Placement | undefined = onJump.mock.calls[0]?.[0];
    if (arg === undefined) throw new Error("onJump not called");
    expect(arg).toEqual({
      id: "p1",
      username: "wren",
      x: 0,
      y: 0,
      w: 32,
      h: 32,
      rot: 15,
      created_at: "2026-10-13T11:00:00Z",
      tags: [],
    });
    expect("seq" in arg).toBe(false);
  });

  it("pages older entries by cursor and appends", async () => {
    const older: Feed = {
      items: [
        {
          id: "p0",
          username: "elder",
          seq: 12,
          x: 1,
          y: 1,
          w: 4,
          h: 4,
          rot: 0,
          created_at: "2026-10-12T00:00:00Z",
          tags: [],
        },
      ],
      next_before: null,
    };
    const client = makeClient();
    client.feed = vi
      .fn()
      .mockResolvedValueOnce(PAGE)
      .mockResolvedValueOnce(older);
    mount(client);
    await vi.waitFor(() => {
      expect(screen.getByText("Older")).toBeTruthy();
    });
    fireEvent.click(screen.getByText("Older"));
    await vi.waitFor(() => {
      expect(screen.getByText("@elder")).toBeTruthy();
    });
    expect(client.feed).toHaveBeenLastCalledWith({
      before: 40,
      limit: 20,
    });
    expect(screen.queryByText("Older")).toBeNull();
  });

  it("applies the tag filter on submit", async () => {
    const client = makeClient();
    mount(client);
    const input = screen.getByLabelText("Filter by tag");
    fireEvent.input(input, { target: { value: "  Stage  " } });
    fireEvent.submit(input.closest("form")!);
    await vi.waitFor(() => {
      expect(client.feed).toHaveBeenLastCalledWith({
        tag: "stage",
        limit: 20,
      });
    });
  });

  it("shows an error line when the feed fails", async () => {
    const client = makeClient();
    client.feed = vi.fn(() => Promise.reject(new Error("down")));
    mount(client);
    await vi.waitFor(() => {
      expect(screen.getByText("Could not load the feed")).toBeTruthy();
    });
  });
});
