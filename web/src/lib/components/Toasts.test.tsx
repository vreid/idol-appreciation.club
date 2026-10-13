// @vitest-environment happy-dom
import { cleanup, fireEvent, render, screen } from "@solidjs/testing-library";
import {
  afterEach,
  beforeEach,
  describe,
  expect,
  it,
  vi,
} from "vite-plus/test";
import { createToastHub } from "../toasts";
import Toasts from "./Toasts";

beforeEach(() => {
  vi.useFakeTimers();
});

afterEach(() => {
  cleanup();
  vi.useRealTimers();
});

function renderHub() {
  const hub = createToastHub();
  render(() => (
    <Toasts
      toasts={hub.list}
      onDismiss={(id) => {
        hub.dismiss(id);
      }}
    />
  ));
  return hub;
}

describe("Toasts", () => {
  it("renders pushed toasts with their kind class", () => {
    const hub = renderHub();
    const seeded = [
      ["info", "first"],
      ["error", "second"],
    ] as const;
    for (const [kind, message] of seeded) {
      hub.push(kind, message);
    }
    expect(screen.getByText("first").className).toBe("toast info");
    expect(screen.getByText("second").className).toBe("toast error");
  });

  it("clicking a toast dismisses just that toast", () => {
    const hub = renderHub();
    for (const message of ["one", "two"]) {
      hub.push("info", message);
    }
    fireEvent.click(screen.getByText("one"));
    expect(screen.queryByText("one")).toBeNull();
    expect(screen.getByText("two")).toBeDefined();
  });

  it("dedupes a repeated message by re-arming the same pill", () => {
    const hub = renderHub();
    for (let i = 0; i < 2; i += 1) {
      hub.push("info", "again");
    }
    expect(screen.getAllByText("again").length).toBe(1);
  });

  it("caps the visible pills at three, dropping the oldest", () => {
    const hub = renderHub();
    for (const message of ["a", "b", "c", "d"]) {
      hub.push("info", message);
    }
    expect(screen.queryByText("a")).toBeNull();
    expect(screen.getByText("d")).toBeDefined();
    expect(screen.getAllByRole("button").length).toBe(3);
  });

  it("auto-dismisses after the toast lifetime", () => {
    const hub = renderHub();
    hub.push("info", "fleeting");
    vi.advanceTimersByTime(3999);
    expect(screen.getByText("fleeting")).toBeDefined();
    vi.advanceTimersByTime(1);
    expect(screen.queryByText("fleeting")).toBeNull();
  });

  it("keeps error toasts longer than info toasts", () => {
    const hub = renderHub();
    hub.push("error", "sticky");
    vi.advanceTimersByTime(4000);
    expect(screen.getByText("sticky")).toBeDefined();
    vi.advanceTimersByTime(2000);
    expect(screen.queryByText("sticky")).toBeNull();
  });
});
