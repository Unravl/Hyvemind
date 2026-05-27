import { describe, it, expect } from "vitest";
import { relativeTime, relativeTimeDetailed, timeGroup } from "../time";

describe("relativeTime", () => {
  it("returns fallback when createdAt is undefined", () => {
    expect(relativeTime(undefined, "14m")).toBe("14m");
  });

  it("returns empty string when no fallback", () => {
    expect(relativeTime(undefined)).toBe("");
  });

  it('returns "now" for timestamps less than 1 minute ago', () => {
    expect(relativeTime(Date.now() - 30_000)).toBe("now");
  });

  it("returns minutes for timestamps less than 1 hour ago", () => {
    expect(relativeTime(Date.now() - 15 * 60_000)).toBe("15m");
  });

  it("returns hours for timestamps less than 1 day ago", () => {
    expect(relativeTime(Date.now() - 3 * 3_600_000)).toBe("3h");
  });

  it("returns days for timestamps more than 1 day ago", () => {
    expect(relativeTime(Date.now() - 5 * 86_400_000)).toBe("5d");
  });

  it('returns "now" for future timestamps (clock skew)', () => {
    expect(relativeTime(Date.now() + 60_000)).toBe("now");
  });

  it("handles epoch 0 as a valid timestamp", () => {
    const result = relativeTime(0);
    expect(result).toMatch(/^\d+d$/);
  });
});

describe("relativeTimeDetailed", () => {
  const NOW = 1_700_000_000_000;

  it("returns empty string when createdAt is undefined", () => {
    expect(relativeTimeDetailed(undefined, NOW)).toBe("");
  });

  it("returns '0s ago' for a timestamp less than a second old", () => {
    expect(relativeTimeDetailed(NOW - 500, NOW)).toBe("0s ago");
  });

  it("returns '22s ago' at 22 seconds", () => {
    expect(relativeTimeDetailed(NOW - 22_000, NOW)).toBe("22s ago");
  });

  it("returns '59s ago' at 59 seconds", () => {
    expect(relativeTimeDetailed(NOW - 59_000, NOW)).toBe("59s ago");
  });

  it("crosses into '1m ago' at exactly 60s", () => {
    expect(relativeTimeDetailed(NOW - 60_000, NOW)).toBe("1m ago");
  });

  it("returns '12m ago' at 12 minutes", () => {
    expect(relativeTimeDetailed(NOW - 12 * 60_000, NOW)).toBe("12m ago");
  });

  it("crosses into '1h ago' at 60 minutes (no trailing 0m)", () => {
    expect(relativeTimeDetailed(NOW - 60 * 60_000, NOW)).toBe("1h ago");
  });

  it("returns '1h 12m ago' for mixed hours/minutes", () => {
    const diff = 1 * 3_600_000 + 12 * 60_000;
    expect(relativeTimeDetailed(NOW - diff, NOW)).toBe("1h 12m ago");
  });

  it("returns '23h 59m ago' just under a day", () => {
    const diff = 23 * 3_600_000 + 59 * 60_000;
    expect(relativeTimeDetailed(NOW - diff, NOW)).toBe("23h 59m ago");
  });

  it("crosses into '1d ago' at exactly 24h (no trailing 0h)", () => {
    expect(relativeTimeDetailed(NOW - 24 * 3_600_000, NOW)).toBe("1d ago");
  });

  it("returns '1d 3h ago' for mixed days/hours", () => {
    const diff = 1 * 86_400_000 + 3 * 3_600_000;
    expect(relativeTimeDetailed(NOW - diff, NOW)).toBe("1d 3h ago");
  });

  it("returns '2d 3h ago' for two-day mixed format", () => {
    const diff = 2 * 86_400_000 + 3 * 3_600_000;
    expect(relativeTimeDetailed(NOW - diff, NOW)).toBe("2d 3h ago");
  });

  it("clamps future / clock-skew timestamps to '0s ago'", () => {
    expect(relativeTimeDetailed(NOW + 60_000, NOW)).toBe("0s ago");
  });
});

describe("timeGroup", () => {
  it('returns "Older" when createdAt is undefined', () => {
    expect(timeGroup(undefined)).toBe("Older");
  });

  it('returns "Today" for timestamps from today', () => {
    expect(timeGroup(Date.now() - 60_000)).toBe("Today");
  });

  it('returns "Yesterday" for timestamps from yesterday', () => {
    const yesterday = new Date();
    yesterday.setHours(0, 0, 0, 0);
    expect(timeGroup(yesterday.getTime() - 1)).toBe("Yesterday");
  });

  it('returns "This week" for timestamps within 7 days', () => {
    const today = new Date();
    today.setHours(0, 0, 0, 0);
    expect(timeGroup(today.getTime() - 3 * 86_400_000)).toBe("This week");
  });

  it('returns "Older" for timestamps older than 7 days', () => {
    const today = new Date();
    today.setHours(0, 0, 0, 0);
    expect(timeGroup(today.getTime() - 8 * 86_400_000)).toBe("Older");
  });

  it('returns "Today" for future timestamps (clock skew)', () => {
    expect(timeGroup(Date.now() + 86_400_000)).toBe("Today");
  });

  it("handles epoch 0 as a valid timestamp", () => {
    expect(timeGroup(0)).toBe("Older");
  });
});
