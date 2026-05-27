import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, fireEvent } from "@testing-library/react";

// Force a stable empty status from the underlying hook so the screen
// doesn't try to issue real IPC calls during render.
vi.mock("../../../hooks/useNurseStatus", () => ({
  useNurseStatus: () => ({
    status: {
      stats: {
        monitored_count: 0,
        stall_count: 0,
        intervention_count: 0,
        last_check_at: null,
        is_running: true,
      },
      sessions: [],
      recent_interventions: [],
      config: {
        enabled: true,
        stall_threshold_secs: 300,
        nurse_model: "anthropic/x",
        max_interventions: 3,
        tick_interval_secs: 60,
        nurse_provider: null,
      },
      health: {
        last_tick_at: Date.now() - 1000,
        last_successful_tick_at: Date.now() - 1000,
        consecutive_failed_ticks: 0,
        consecutive_bad_parse_ticks: 0,
        consecutive_skipped_ticks: 0,
        degraded: false,
      },
    },
    isLoading: false,
    refresh: vi.fn(),
  }),
}));

vi.mock("../../../hooks/useNurseSessions", () => ({
  useNurseSessions: () => ({ sessions: [], isLoading: false, error: null }),
}));

vi.mock("../../../hooks/useNurseInterventionLog", () => ({
  useNurseInterventionLog: () => ({
    rows: [],
    hasMore: false,
    isLoading: false,
    error: null,
    loadMore: vi.fn(),
    setQuery: vi.fn(),
  }),
}));

vi.mock("../../../hooks/useNurseDetectorStats", () => ({
  useNurseDetectorStats: () => ({
    rows: [],
    isLoading: false,
    error: null,
    refresh: vi.fn(),
  }),
}));

vi.mock("../../../hooks/useNurseProfile", () => ({
  useNurseProfile: () => ({
    config: {
      enabled: true,
      intervention_mode: "auto",
      escalation_min_severity: "warn",
      budget: {
        initial_cap: 6,
        decay_per_hour: 3,
        max_cap: 12,
        per_detector_cap: 3,
        per_key_cooldown_secs: 120,
      },
      detectors: {},
    },
    isLoading: false,
    isSaving: false,
    error: null,
    lastError: null,
    patch: vi.fn().mockResolvedValue(true),
    reload: vi.fn(),
    resetToDefaults: vi.fn(),
  }),
}));

// Avoid pulling in the full ModelBrowser tree.
vi.mock("../../../screens/ModelBrowser", () => ({
  ModelBrowserModal: () => null,
}));

import { NurseScreen } from "../../../screens/Nurse";
import { NurseProvider } from "../../../lib/NurseProvider";

beforeEach(() => {
  // Run the tests without Tauri so IPC paths short-circuit.
  delete (globalThis as { isTauri?: unknown }).isTauri;
});

describe("Nurse screen tab routing", () => {
  it("renders all four tabs by default and the Live tab is initially active", () => {
    render(
      <NurseProvider>
        <NurseScreen go={vi.fn()} />
      </NurseProvider>,
    );
    expect(screen.getByTestId("nurse-tab-live")).toBeInTheDocument();
    expect(screen.getByTestId("nurse-tab-log")).toBeInTheDocument();
    expect(screen.getByTestId("nurse-tab-detectors")).toBeInTheDocument();
    expect(screen.getByTestId("nurse-tab-profiles")).toBeInTheDocument();

    // The live tab is active by default — at-a-glance cards render.
    expect(screen.getByText("Sessions Monitored")).toBeInTheDocument();
  });

  it("switches to the Intervention Log tab when its trigger is clicked", () => {
    render(
      <NurseProvider>
        <NurseScreen go={vi.fn()} />
      </NurseProvider>,
    );
    fireEvent.click(screen.getByTestId("nurse-tab-log"));
    // The log tab renders filter labels (Profile, Action, Tier, ...)
    expect(screen.getByText("Profile")).toBeInTheDocument();
    expect(screen.getByText("Tier")).toBeInTheDocument();
  });

  it("switches to the Detectors tab and renders the empty-state message", () => {
    render(
      <NurseProvider>
        <NurseScreen go={vi.fn()} />
      </NurseProvider>,
    );
    fireEvent.click(screen.getByTestId("nurse-tab-detectors"));
    expect(
      screen.getByText(/No detectors registered/i),
    ).toBeInTheDocument();
  });

  it("switches to the Profiles tab and shows the per-profile sub-tabs", () => {
    render(
      <NurseProvider>
        <NurseScreen go={vi.fn()} />
      </NurseProvider>,
    );
    fireEvent.click(screen.getByTestId("nurse-tab-profiles"));
    // Sub-tabs default to "default" being active; the others are present.
    expect(screen.getByRole("button", { name: /^tasks$/i })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /^swarm$/i })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /^hivemind$/i })).toBeInTheDocument();
  });
});
