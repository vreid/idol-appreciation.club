import { describe, expect, it } from "vitest";
import { formatDuration, timeAgo } from "./format";

const NOW = Date.parse("2026-10-12T12:00:00Z");

describe("timeAgo", () => {
  it("buckets elapsed time into friendly units", () => {
    expect(timeAgo("2026-10-12T11:59:59Z", NOW)).toBe("just now");
    expect(timeAgo("2026-10-12T11:59:30Z", NOW)).toBe("30s ago");
    expect(timeAgo("2026-10-12T11:30:00Z", NOW)).toBe("30m ago");
    expect(timeAgo("2026-10-12T06:00:00Z", NOW)).toBe("6h ago");
    expect(timeAgo("2026-10-09T12:00:00Z", NOW)).toBe("3d ago");
  });

  it("clamps future timestamps to just now", () => {
    expect(timeAgo("2026-10-12T13:00:00Z", NOW)).toBe("just now");
  });

  it("renders unknown for garbage input", () => {
    expect(timeAgo("nonsense", NOW)).toBe("unknown");
  });
});

describe("formatDuration", () => {
  it("formats seconds, minutes, hours, and days", () => {
    expect(formatDuration(0)).toBe("0s");
    expect(formatDuration(42)).toBe("42s");
    expect(formatDuration(65)).toBe("1m 05s");
    expect(formatDuration(3600 * 3 + 60 * 12)).toBe("3h 12m");
    expect(formatDuration(86400 * 2 + 3600 * 4)).toBe("2d 04h");
  });

  it("never goes negative", () => {
    expect(formatDuration(-10)).toBe("0s");
  });
});
