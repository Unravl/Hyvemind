import { describe, it, expect, afterEach, vi } from "vitest";
import { isLiveSwarmStatus, swarmElapsedMs } from "../format";

describe("isLiveSwarmStatus", () => {
  it('returns true for "running"', () => {
    expect(isLiveSwarmStatus("running")).toBe(true);
  });
  it('returns true for "planning"', () => {
    expect(isLiveSwarmStatus("planning")).toBe(true);
  });
  it('returns true for "implementing" (raw backend status)', () => {
    expect(isLiveSwarmStatus("implementing")).toBe(true);
  });
  it('returns false for "paused"', () => {
    expect(isLiveSwarmStatus("paused")).toBe(false);
  });
  it('returns false for "completed"', () => {
    expect(isLiveSwarmStatus("completed")).toBe(false);
  });
  it('returns false for "failed"', () => {
    expect(isLiveSwarmStatus("failed")).toBe(false);
  });
  it('returns false for "cancelled"', () => {
    expect(isLiveSwarmStatus("cancelled")).toBe(false);
  });
  it('returns false for "interrupted"', () => {
    expect(isLiveSwarmStatus("interrupted")).toBe(false);
  });
  it("returns false for undefined", () => {
    expect(isLiveSwarmStatus(undefined)).toBe(false);
  });
  it("returns false for null", () => {
    expect(isLiveSwarmStatus(null)).toBe(false);
  });
  it("returns false for empty string", () => {
    expect(isLiveSwarmStatus("")).toBe(false);
  });
});

describe("swarmElapsedMs", () => {
  afterEach(() => {
    vi.restoreAllMocks();
  });

  it("returns roughly Date.now() - createdAt for live status", () => {
    const now = 1_700_000_000_000;
    vi.spyOn(Date, "now").mockReturnValue(now);
    const createdAt = new Date(now - 5000).toISOString();
    const result = swarmElapsedMs({
      status: "running",
      createdAt,
      updatedAt: new Date(now - 1000).toISOString(),
    });
    // Allow small tolerance for ISO round-tripping
    expect(Math.abs(result - 5000)).toBeLessThanOrEqual(50);
  });

  it("freezes at updatedAt - createdAt for paused status", () => {
    const startNow = 1_700_000_000_000;
    const createdAt = new Date(startNow - 60_000).toISOString();
    const updatedAt = new Date(startNow - 10_000).toISOString();
    const nowSpy = vi.spyOn(Date, "now").mockReturnValue(startNow);
    const first = swarmElapsedMs({
      status: "paused",
      createdAt,
      updatedAt,
    });
    nowSpy.mockReturnValue(startNow + 30_000);
    const second = swarmElapsedMs({
      status: "paused",
      createdAt,
      updatedAt,
    });
    expect(first).toBe(50_000);
    expect(second).toBe(50_000);
    expect(first).toBe(second);
  });

  it("freezes identically for completed status", () => {
    const startNow = 1_700_000_000_000;
    const createdAt = new Date(startNow - 120_000).toISOString();
    const updatedAt = new Date(startNow - 30_000).toISOString();
    const nowSpy = vi.spyOn(Date, "now").mockReturnValue(startNow);
    const first = swarmElapsedMs({
      status: "completed",
      createdAt,
      updatedAt,
    });
    nowSpy.mockReturnValue(startNow + 60_000);
    const second = swarmElapsedMs({
      status: "completed",
      createdAt,
      updatedAt,
    });
    expect(first).toBe(90_000);
    expect(second).toBe(90_000);
  });

  it("treats undefined status as non-live (uses updatedAt - createdAt)", () => {
    const startNow = 1_700_000_000_000;
    const createdAt = new Date(startNow - 60_000).toISOString();
    const updatedAt = new Date(startNow - 20_000).toISOString();
    vi.spyOn(Date, "now").mockReturnValue(startNow);
    const result = swarmElapsedMs({
      status: undefined,
      createdAt,
      updatedAt,
    });
    expect(result).toBe(40_000);
  });

  it("treats null status as non-live (uses updatedAt - createdAt)", () => {
    const startNow = 1_700_000_000_000;
    const createdAt = new Date(startNow - 60_000).toISOString();
    const updatedAt = new Date(startNow - 20_000).toISOString();
    vi.spyOn(Date, "now").mockReturnValue(startNow);
    const result = swarmElapsedMs({
      status: null,
      createdAt,
      updatedAt,
    });
    expect(result).toBe(40_000);
  });

  it("falls back to fallbackMs when createdAt is missing", () => {
    expect(
      swarmElapsedMs({
        status: "running",
        createdAt: undefined,
        updatedAt: new Date().toISOString(),
        fallbackMs: 12_345,
      }),
    ).toBe(12_345);
  });

  it("falls back to fallbackMs when createdAt is null", () => {
    expect(
      swarmElapsedMs({
        status: "paused",
        createdAt: null,
        updatedAt: new Date().toISOString(),
        fallbackMs: 999,
      }),
    ).toBe(999);
  });

  it("falls back to fallbackMs when createdAt is unparseable", () => {
    expect(
      swarmElapsedMs({
        status: "paused",
        createdAt: "not-a-date",
        updatedAt: new Date().toISOString(),
        fallbackMs: 7,
      }),
    ).toBe(7);
  });

  it("returns 0 when createdAt missing and no fallback", () => {
    expect(
      swarmElapsedMs({
        status: "paused",
        createdAt: undefined,
        updatedAt: undefined,
      }),
    ).toBe(0);
  });

  it("falls back to fallbackMs when non-live and updatedAt missing", () => {
    const createdAt = new Date(1_700_000_000_000).toISOString();
    expect(
      swarmElapsedMs({
        status: "paused",
        createdAt,
        updatedAt: undefined,
        fallbackMs: 42,
      }),
    ).toBe(42);
  });

  it("falls back to 0 when non-live, valid createdAt, missing updatedAt, no fallback", () => {
    const createdAt = new Date(1_700_000_000_000).toISOString();
    expect(
      swarmElapsedMs({
        status: "paused",
        createdAt,
        updatedAt: undefined,
      }),
    ).toBe(0);
  });

  it("falls back to fallbackMs when non-live and updatedAt unparseable", () => {
    const createdAt = new Date(1_700_000_000_000).toISOString();
    expect(
      swarmElapsedMs({
        status: "completed",
        createdAt,
        updatedAt: "garbage",
        fallbackMs: 100,
      }),
    ).toBe(100);
  });

  it("clamps to 0 for live status when createdAt is in the future (clock skew)", () => {
    const now = 1_700_000_000_000;
    vi.spyOn(Date, "now").mockReturnValue(now);
    const createdAt = new Date(now + 5_000).toISOString();
    expect(
      swarmElapsedMs({
        status: "running",
        createdAt,
        updatedAt: new Date(now + 6_000).toISOString(),
      }),
    ).toBe(0);
  });

  it("clamps to 0 for non-live status when updatedAt < createdAt (clock skew)", () => {
    const now = 1_700_000_000_000;
    vi.spyOn(Date, "now").mockReturnValue(now);
    const createdAt = new Date(now - 10_000).toISOString();
    const updatedAt = new Date(now - 20_000).toISOString();
    expect(
      swarmElapsedMs({
        status: "paused",
        createdAt,
        updatedAt,
      }),
    ).toBe(0);
  });
});
