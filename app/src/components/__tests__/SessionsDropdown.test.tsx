import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, fireEvent, act } from "@testing-library/react";
import type {
  MonitoredSessionSnapshot,
  NurseStatusSnapshot,
} from "../../types/nurse";

let currentStatus: NurseStatusSnapshot;
const mockRefresh = vi.fn().mockResolvedValue(undefined);

vi.mock("../../hooks/useNurseStatus", () => ({
  useNurseStatus: () => ({
    status: currentStatus,
    isLoading: false,
    refresh: mockRefresh,
  }),
}));

// Force the isTauri() branch on so the pill renders live counts rather
// than the static "3 Sessions" mock.
vi.mock("../../lib/tauri", () => ({
  isTauri: () => true,
}));

import { SessionsDropdown } from "../SessionsDropdown";

function mkSession(
  overrides: Partial<MonitoredSessionSnapshot> = {},
): MonitoredSessionSnapshot {
  return {
    session_id: "sess-aaaaaaaaaaaa-1",
    last_activity_ms: Date.now() - 4_000,
    event_count: 3,
    is_alive: true,
    is_busy: false,
    status: "healthy",
    stall_detected_at: null,
    intervention_count: 0,
    last_check_at: new Date().toISOString(),
    ...overrides,
  };
}

function baseFixture(
  overrides: Partial<NurseStatusSnapshot> = {},
): NurseStatusSnapshot {
  const now = Date.now();
  return {
    stats: {
      monitored_count: 0,
      stall_count: 0,
      intervention_count: 0,
      last_check_at: new Date(now).toISOString(),
      is_running: true,
    },
    sessions: [],
    recent_interventions: [],
    config: {
      enabled: true,
      stall_threshold_secs: 300,
      nurse_model: "anthropic/claude-haiku-4.5",
      max_interventions: 3,
      tick_interval_secs: 60,
      nurse_provider: null,
      swarms_only: false,
    },
    health: {
      last_tick_at: now - 12_000,
      last_successful_tick_at: now - 12_000,
      consecutive_failed_ticks: 0,
      consecutive_bad_parse_ticks: 0,
      consecutive_skipped_ticks: 0,
      degraded: false,
    },
    ...overrides,
  };
}

describe("SessionsDropdown", () => {
  beforeEach(() => {
    currentStatus = baseFixture();
    mockRefresh.mockClear();
  });

  it("renders the pill with the correct count from nurse status", () => {
    currentStatus = baseFixture({
      sessions: [
        mkSession({ session_id: "sess-1" }),
        mkSession({ session_id: "sess-2" }),
      ],
      stats: {
        monitored_count: 2,
        stall_count: 0,
        intervention_count: 0,
        last_check_at: new Date().toISOString(),
        is_running: true,
      },
    });

    render(<SessionsDropdown />);
    expect(screen.getByRole("button", { name: /2 Sessions/i })).toBeInTheDocument();
  });

  it("uses the singular label when there is exactly one session", () => {
    currentStatus = baseFixture({
      sessions: [mkSession({ session_id: "sess-only" })],
    });
    render(<SessionsDropdown />);
    expect(screen.getByRole("button", { name: /^1 Session$/ })).toBeInTheDocument();
  });

  it("opens the dropdown on click and renders one card per session", () => {
    currentStatus = baseFixture({
      sessions: [
        mkSession({ session_id: "sess-alpha-aaa" }),
        mkSession({ session_id: "sess-beta-bbb" }),
        mkSession({ session_id: "sess-gamma-ccc" }),
      ],
    });

    render(<SessionsDropdown />);
    fireEvent.click(screen.getByRole("button", { name: /3 Sessions/i }));

    expect(screen.getByRole("dialog", { name: /Active Sessions/i })).toBeInTheDocument();
    // Truncation in SessionCard caps ids at 12 chars + ellipsis.
    expect(screen.getByText("sess-alpha-a…")).toBeInTheDocument();
    expect(screen.getByText("sess-beta-bb…")).toBeInTheDocument();
    expect(screen.getByText("sess-gamma-c…")).toBeInTheDocument();
  });

  it("renders the empty-state copy when sessions is []", () => {
    currentStatus = baseFixture({ sessions: [] });
    render(<SessionsDropdown />);
    fireEvent.click(screen.getByRole("button", { name: /0 Sessions/i }));

    expect(screen.getByText("No active sessions")).toBeInTheDocument();
  });

  it("turns the status dot amber when any session is stalled", () => {
    currentStatus = baseFixture({
      sessions: [
        mkSession({ session_id: "sess-healthy", status: "healthy" }),
        mkSession({ session_id: "sess-stalled", status: "stalled" }),
      ],
    });

    const { container } = render(<SessionsDropdown />);
    const dot = container.querySelector("button > span:first-child");
    expect(dot).not.toBeNull();
    expect(dot!.className).toContain("bg-amber-400");
  });

  it("uses the grey dot when there are zero sessions", () => {
    currentStatus = baseFixture({ sessions: [] });
    const { container } = render(<SessionsDropdown />);
    const dot = container.querySelector("button > span:first-child");
    expect(dot).not.toBeNull();
    expect(dot!.className).toContain("bg-line-strong");
  });

  it("uses the emerald dot when sessions are present and all healthy", () => {
    currentStatus = baseFixture({
      sessions: [mkSession({ session_id: "sess-1", status: "healthy" })],
    });
    const { container } = render(<SessionsDropdown />);
    const dot = container.querySelector("button > span:first-child");
    expect(dot).not.toBeNull();
    expect(dot!.className).toContain("bg-emerald-400");
  });

  it("fires onOpenChange(true) on open and onOpenChange(false) on outside click", () => {
    currentStatus = baseFixture({
      sessions: [mkSession({ session_id: "sess-1" })],
    });

    const onOpenChange = vi.fn();
    render(<SessionsDropdown onOpenChange={onOpenChange} />);

    fireEvent.click(screen.getByRole("button", { name: /1 Session/i }));
    expect(onOpenChange).toHaveBeenLastCalledWith(true);

    // Outside-click: dispatch a mousedown on document.body (outside the
    // dropdown ref).
    act(() => {
      document.body.dispatchEvent(
        new MouseEvent("mousedown", { bubbles: true }),
      );
    });
    expect(onOpenChange).toHaveBeenLastCalledWith(false);
  });
});
