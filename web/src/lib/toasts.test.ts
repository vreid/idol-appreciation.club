import {
  afterEach,
  beforeEach,
  describe,
  expect,
  it,
  vi,
} from "vite-plus/test";
import { MAX_VISIBLE_TOASTS, createToastHub } from "./toasts";

describe("toast hub", () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  it("auto-dismisses info toasts after their lifetime", () => {
    const hub = createToastHub({ toastMs: 1000 });
    hub.push("info", "hello");
    expect(hub.list).toHaveLength(1);
    vi.advanceTimersByTime(1000);
    expect(hub.list).toHaveLength(0);
  });

  it("keeps error toasts longer than info toasts", () => {
    const hub = createToastHub({ toastMs: 1000, errorToastMs: 5000 });
    for (const [kind, message] of [
      ["info", "bye"],
      ["error", "stay"],
    ] as const) {
      hub.push(kind, message);
    }
    vi.advanceTimersByTime(1000);
    expect(hub.list.map((t) => t.kind)).toEqual(["error"]);
    vi.advanceTimersByTime(4000);
    expect(hub.list).toHaveLength(0);
  });

  it("dedupes a repeated message into the same pill", () => {
    const hub = createToastHub({ toastMs: 1000 });
    for (let i = 0; i < 3; i++) {
      hub.push("info", "Nothing placed here yet");
    }
    expect(hub.list).toHaveLength(1);
  });

  it("re-arms the timer when a duplicate arrives", () => {
    const hub = createToastHub({ toastMs: 1000 });
    hub.push("info", "again");
    vi.advanceTimersByTime(900);
    hub.push("info", "again");
    vi.advanceTimersByTime(900);
    // 1800ms elapsed > original 1000ms lifetime: the duplicate re-armed it.
    expect(hub.list).toHaveLength(1);
    vi.advanceTimersByTime(200);
    expect(hub.list).toHaveLength(0);
  });

  it("keeps distinct messages as separate pills", () => {
    const hub = createToastHub();
    for (const message of ["one", "two"]) {
      hub.push("info", message);
    }
    expect(hub.list).toHaveLength(2);
  });

  it("caps visible toasts and drops the oldest", () => {
    const hub = createToastHub();
    for (const message of ["a", "b", "c", "d", "e"]) {
      hub.push("info", message);
    }
    expect(hub.list).toHaveLength(MAX_VISIBLE_TOASTS);
    expect(hub.list.map((t) => t.message)).toEqual(["c", "d", "e"]);
  });

  it("dismiss removes a toast immediately", () => {
    const hub = createToastHub();
    hub.push("info", "gone");
    const id = hub.list[0]?.id ?? -1;
    hub.dismiss(id);
    expect(hub.list).toHaveLength(0);
  });
});
