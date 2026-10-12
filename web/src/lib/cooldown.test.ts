import {
  afterEach,
  beforeEach,
  describe,
  expect,
  it,
  vi,
} from "vite-plus/test";
import { Countdown, cooldownRemainingMs, formatCountdown } from "./cooldown";

beforeEach(() => {
  vi.useFakeTimers();
  vi.setSystemTime(new Date("2026-10-12T12:00:00Z"));
});

afterEach(() => {
  vi.useRealTimers();
});

describe("cooldownRemainingMs", () => {
  const NOW = Date.parse("2026-10-12T12:00:00Z");

  it("returns the positive remaining time", () => {
    expect(cooldownRemainingMs("2026-10-12T12:00:30Z", NOW)).toBe(30000);
  });

  it("returns 0 once the deadline has passed", () => {
    expect(cooldownRemainingMs("2026-10-12T11:59:59Z", NOW)).toBe(0);
  });

  it("returns 0 for no cooldown and for garbage timestamps", () => {
    expect(cooldownRemainingMs(null, NOW)).toBe(0);
    expect(cooldownRemainingMs("not-a-date", NOW)).toBe(0);
  });
});

describe("formatCountdown", () => {
  it("formats under and over an hour", () => {
    expect(formatCountdown(0)).toBe("0:00");
    expect(formatCountdown(59_999)).toBe("1:00");
    expect(formatCountdown(65_000)).toBe("1:05");
    expect(formatCountdown(3_723_000)).toBe("1:02:03");
  });
});

describe("Countdown", () => {
  it("ticks live and reports remaining time", () => {
    const ticks: number[] = [];
    const countdown = new Countdown(
      "2026-10-12T12:00:02Z",
      {
        onTick: (ms) => {
          ticks.push(ms);
        },
      },
      500,
    );
    countdown.start();
    vi.advanceTimersByTime(1500);
    expect(ticks).toEqual([2000, 1500, 1000, 500]);
    countdown.stop();
  });

  it("stops at zero and fires onExpire once", () => {
    const ticks: number[] = [];
    let expired = 0;
    const countdown = new Countdown(
      "2026-10-12T12:00:01Z",
      {
        onTick: (ms) => {
          ticks.push(ms);
        },
        onExpire: () => {
          expired++;
        },
      },
      500,
    );
    countdown.start();
    vi.advanceTimersByTime(5000);
    expect(ticks.at(-1)).toBe(0);
    expect(expired).toBe(1);
    expect(ticks.filter((t) => t === 0).length).toBe(1);
  });

  it("does nothing for a null deadline", () => {
    const ticks: number[] = [];
    const countdown = new Countdown(null, {
      onTick: (ms) => {
        ticks.push(ms);
      },
    });
    countdown.start();
    vi.advanceTimersByTime(5000);
    expect(ticks).toEqual([]);
    countdown.stop();
  });

  it("restart() resets expiry notification", () => {
    let expired = 0;
    const countdown = new Countdown(
      "2026-10-12T11:59:59Z",
      {
        onTick: () => {},
        onExpire: () => {
          expired++;
        },
      },
      500,
    );
    countdown.start();
    vi.advanceTimersByTime(1000);
    countdown.start();
    vi.advanceTimersByTime(1000);
    expect(expired).toBe(2);
  });

  it("stop() halts ticking", () => {
    const ticks: number[] = [];
    const countdown = new Countdown(
      "2026-10-12T12:10:00Z",
      {
        onTick: (ms) => {
          ticks.push(ms);
        },
      },
      500,
    );
    countdown.start();
    countdown.stop();
    vi.advanceTimersByTime(5000);
    expect(ticks.length).toBe(1);
  });
});
