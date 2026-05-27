import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, fireEvent, waitFor } from "@testing-library/react";
import type { NurseStatusSnapshot } from "../../types/nurse";

let currentStatus: NurseStatusSnapshot;
const mockRefresh = vi.fn().mockResolvedValue(undefined);
const mockSetNurseConfig = vi.fn().mockResolvedValue(undefined);

vi.mock("../../hooks/useNurseStatus", () => ({
  useNurseStatus: () => ({
    status: currentStatus,
    isLoading: false,
    refresh: mockRefresh,
  }),
}));

vi.mock("../../lib/ipc", () => ({
  setNurseConfig: (...args: any[]) => mockSetNurseConfig(...args),
}));

import { NurseDropdown } from "../NurseDropdown";

function baseFixture(overrides: Partial<NurseStatusSnapshot> = {}): NurseStatusSnapshot {
  const now = Date.now();
  return {
    stats: {
      monitored_count: 1,
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

function openDropdown() {
  // The trigger button's accessible name is just "Nurse" (its visible
  // text content). The full "Nurse monitor — <state>" string is the
  // title attribute, which is not the primary accessible name source.
  fireEvent.click(screen.getByRole("button", { name: /Nurse/i }));
}

describe("NurseDropdown service health", () => {
  beforeEach(() => {
    currentStatus = baseFixture();
    mockRefresh.mockClear();
    mockSetNurseConfig.mockClear();
  });

  it("renders the Service health row with relative timestamps when healthy", () => {
    render(<NurseDropdown />);
    openDropdown();

    expect(screen.getByText("Service health")).toBeInTheDocument();
    // Both "Last tick" and "Last successful" render the same "12s ago"
    // span because both timestamps are seeded 12s in the past.
    expect(screen.getAllByText("12s ago")).toHaveLength(2);
    expect(screen.queryByText(/degraded mode/i)).toBeNull();
  });

  it("no longer renders the in-dropdown Sessions list (moved to SessionsDropdown)", () => {
    currentStatus = baseFixture({
      sessions: [
        {
          session_id: "sess-abc-1",
          last_activity_ms: Date.now() - 5_000,
          event_count: 4,
          is_alive: true,
          is_busy: false,
          status: "healthy",
          stall_detected_at: null,
          intervention_count: 0,
          last_check_at: new Date().toISOString(),
        },
      ],
    });

    render(<NurseDropdown />);
    openDropdown();

    // The "Sessions" section header should not appear inside the Nurse
    // dropdown anymore — the sessions list lives in the new
    // SessionsDropdown pill.
    expect(
      screen.queryByText("Sessions", { selector: "div" }),
    ).toBeNull();
    expect(screen.queryByText("sess-abc-1")).toBeNull();
  });

  it("shows the red degraded banner and hides the per-row breakdown when degraded", () => {
    currentStatus = baseFixture({
      health: {
        last_tick_at: Date.now() - 12_000,
        last_successful_tick_at: Date.now() - 12_000,
        consecutive_failed_ticks: 0,
        consecutive_bad_parse_ticks: 0,
        consecutive_skipped_ticks: 0,
        degraded: true,
      },
    });

    render(<NurseDropdown />);
    openDropdown();

    expect(
      screen.getByText("Nurse is in degraded mode. Recovery in progress."),
    ).toBeInTheDocument();
    // Breakdown row labels must not render in degraded mode.
    expect(screen.queryByText("Service health")).toBeNull();
    expect(screen.queryByText("Last tick:")).toBeNull();
  });

  it("uses the grey dot when Nurse is disabled in settings", () => {
    currentStatus = baseFixture({
      config: {
        enabled: false,
        stall_threshold_secs: 300,
        nurse_model: "anthropic/claude-haiku-4.5",
        max_interventions: 3,
        tick_interval_secs: 60,
        nurse_provider: null,
      },
    });

    render(<NurseDropdown />);
    const btn = screen.getByRole("button", { name: /Nurse/i });
    expect(btn.getAttribute("title")).toContain("Disabled");
  });

  it("uses the amber dot when last_tick_at is stale", () => {
    currentStatus = baseFixture({
      health: {
        // 5 minutes stale, tick_interval_secs is 60 so threshold is 120s
        last_tick_at: Date.now() - 5 * 60_000,
        last_successful_tick_at: Date.now() - 5 * 60_000,
        consecutive_failed_ticks: 0,
        consecutive_bad_parse_ticks: 0,
        consecutive_skipped_ticks: 0,
        degraded: false,
      },
    });

    render(<NurseDropdown />);
    const btn = screen.getByRole("button", { name: /Nurse/i });
    expect(btn.getAttribute("title")).toContain("Recovering");
  });

  it("renders a toggle switch in the Enabled area and calls setNurseConfig on toggle", async () => {
    currentStatus = baseFixture({
      config: {
        enabled: true,
        stall_threshold_secs: 300,
        nurse_model: "anthropic/claude-haiku-4.5",
        max_interventions: 3,
        tick_interval_secs: 60,
        nurse_provider: null,
      },
    });

    render(<NurseDropdown />);
    openDropdown();

    // Verify toggle button is in document
    const toggleBtn = screen.getByRole("button", { pressed: true });
    expect(toggleBtn).toBeInTheDocument();

    // Click toggle button to turn it off
    fireEvent.click(toggleBtn);

    await waitFor(() => {
      expect(mockSetNurseConfig).toHaveBeenCalledWith({ enabled: false });
    });
    await waitFor(() => {
      expect(mockRefresh).toHaveBeenCalled();
    });
  });

  it("calls setNurseConfig with { swarms_only: true } when the Swarms Only toggle is clicked", async () => {
    currentStatus = baseFixture();
    render(<NurseDropdown />);
    openDropdown();

    const swarmsToggle = screen.getByRole("button", {
      pressed: false,
      name: /Swarms only/i,
    });
    fireEvent.click(swarmsToggle);

    await waitFor(() => {
      expect(mockSetNurseConfig).toHaveBeenCalledWith({ swarms_only: true });
    });
  });

  it("disables the Swarms Only toggle when Nurse is disabled", () => {
    currentStatus = baseFixture({
      config: {
        enabled: false,
        stall_threshold_secs: 300,
        nurse_model: "anthropic/claude-haiku-4.5",
        max_interventions: 3,
        tick_interval_secs: 60,
        nurse_provider: null,
        swarms_only: false,
      },
    });
    render(<NurseDropdown />);
    openDropdown();

    const swarmsToggle = screen.getByRole("button", {
      name: /Swarms only/i,
    });
    expect(swarmsToggle).toBeDisabled();
  });
});
