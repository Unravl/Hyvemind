import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";

const tauriState = vi.hoisted(() => ({ enabled: false }));
const ipcMocks = vi.hoisted(() => ({
  getSettings: vi.fn(),
  setRuntimeSettings: vi.fn(),
  saveApiKey: vi.fn(),
  deleteApiKey: vi.fn(),
  getProviders: vi.fn(),
  checkSubscriptionAuth: vi.fn(),
  getPiStatus: vi.fn(),
  updatePi: vi.fn(),
  addProvider: vi.fn(),
  setDefaultProjectPath: vi.fn(),
  setDefaultModel: vi.fn(),
  setAutoCommitTasks: vi.fn(),
  testProviderModels: vi.fn(),
  testProviderChat: vi.fn(),
  testProviderPi: vi.fn(),
  // useNurseStatus calls getNurseStatus on mount in Tauri mode and writes
  // the result wholesale into a NurseStatusSnapshot state. Returning a
  // partial shape here would crash NurseSettingsSection when the user
  // navigates to the Other tab during the "saves advanced runtime settings"
  // test, so the mock returns a fully-shaped snapshot.
  getNurseStatus: vi.fn(() =>
    Promise.resolve({
      stats: {
        monitored_count: 0,
        stall_count: 0,
        intervention_count: 0,
        last_check_at: null,
        is_running: false,
      },
      sessions: [],
      recent_interventions: [],
      config: {
        enabled: false,
        stall_threshold_secs: 300,
        nurse_model: "anthropic/claude-haiku-4.5",
        max_interventions: 3,
        tick_interval_secs: 60,
        nurse_provider: null,
        swarms_only: false,
      },
      health: {
        last_tick_at: null,
        last_successful_tick_at: null,
        consecutive_failed_ticks: 0,
        consecutive_bad_parse_ticks: 0,
        consecutive_skipped_ticks: 0,
        degraded: false,
      },
    }),
  ),
  setNurseConfig: vi.fn(() => Promise.resolve()),
}));

vi.mock("../../lib/tauri", () => ({ isTauri: () => tauriState.enabled }));

vi.mock("../../lib/ipc", () => ipcMocks);

vi.mock("../../lib/events", () => ({
  onPiUpdateProgress: vi.fn(() => Promise.resolve(() => {})),
  onNurseEvent: vi.fn(() => Promise.resolve(() => {})),
  safeUnlisten: vi.fn((fn?: (() => void) | null) => fn?.()),
}));

vi.mock("../../lib/taskRuntime", () => ({
  useTaskRuntime: () => ({
    hivemindOptions: [],
  }),
  MERGE_TIMEOUT_MIN_KEY: "hyvemind:hivemind-merge-timeout-min",
  MERGE_TIMEOUT_DEFAULT_MIN: 20,
  CHAT_CHECK_IN_SECS_KEY: "hyvemind:chat-check-in-secs",
  CHAT_CHECK_IN_SECS_DEFAULT: 300,
  CHAT_CHECK_IN_SECS_MIN: 60,
  CHAT_CHECK_IN_SECS_MAX: 3600,
  EXTENSION_POLL_INTERVAL_KEY: "hyvemind:extension-poll-interval-secs",
  EXTENSION_POLL_INTERVAL_DEFAULT: 120,
  EXTENSION_POLL_INTERVAL_MIN: 30,
  EXTENSION_POLL_INTERVAL_MAX: 3600,
}));

// audit 6.7 — Settings now connects to the shared SettingsProvider /
// ProvidersProvider for cache-sync writes. Mock them as inert shims
// so tests don't need a real provider mounted.
vi.mock("../../lib/SettingsProvider", () => ({
  useSettings: () => ({
    settings: null,
    isLoading: false,
    error: null,
    refresh: vi.fn().mockResolvedValue(undefined),
    patchSettings: vi.fn(),
  }),
  useSetting: () => null,
  SettingsProvider: ({ children }: { children: React.ReactNode }) => children,
}));

vi.mock("../../lib/ProvidersProvider", () => ({
  useProviders: () => ({
    providers: [],
    configured: [],
    isLoading: false,
    error: null,
    refresh: vi.fn().mockResolvedValue(undefined),
  }),
  useProvider: () => null,
  ProvidersProvider: ({ children }: { children: React.ReactNode }) => children,
}));

vi.mock("../../components/ProjectPicker", () => ({
  ProjectPicker: () => null,
  useProject: () => ({
    project: {
      id: "auth-service",
      name: "auth-service",
      org: "hyvemind",
      cwd: "~/code",
      branch: "main",
      dirty: 0,
      lang: "rust",
      activeSwarms: 0,
      chats: 0,
      lastTouched: "",
    },
    setProject: vi.fn(),
    projects: [],
  }),
  LANG_DOT: {},
}));

vi.mock("../../App", () => ({
  renderMd: (text: string) => text,
  usePiGate: () => ({ refresh: vi.fn() }),
}));

import { SettingsScreen } from "../Settings";

const mockSettings = {
  configured_providers: ["anthropic"],
  default_model: "anthropic/claude-sonnet-4",
  default_hivemind: null,
  default_project_path: null,
  concurrency_cap: 8,
  max_pi_processes: 30,
  data_dir: "/tmp/.hyvemind",
  source_dir: "/tmp/project",
  stable_mode: false,
  debug_mode: false,
  auto_commit_tasks: false,
};

describe("SettingsScreen", () => {
  const go = vi.fn();

  beforeEach(() => {
    tauriState.enabled = false;
    vi.clearAllMocks();
    ipcMocks.getSettings.mockResolvedValue(mockSettings);
    ipcMocks.getProviders.mockResolvedValue([]);
    ipcMocks.checkSubscriptionAuth.mockResolvedValue({ chatgpt: false, claude: false });
    ipcMocks.getPiStatus.mockResolvedValue({
      installed: false,
      binary_path: null,
      resolved_path: null,
      binary_name: null,
      version: null,
      latest_version: null,
      is_outdated: false,
      install_method: "unknown",
      error: "not found",
    });
  });

  it("renders the settings page", () => {
    render(<SettingsScreen go={go} />);
    expect(screen.getByText("Settings")).toBeInTheDocument();
  });

  it("renders the Task behavior section", async () => {
    render(<SettingsScreen go={go} />);
    await userEvent.click(screen.getByRole("button", { name: "Defaults" }));
    expect(screen.getByText("Task behavior")).toBeInTheDocument();
    expect(screen.getByText("Auto Mode")).toBeInTheDocument();
  });

  it("renders the API keys section", () => {
    render(<SettingsScreen go={go} />);
    expect(screen.getByText("API keys")).toBeInTheDocument();
  });

  it("renders provider key table headers", () => {
    render(<SettingsScreen go={go} />);
    // Table column headers are always rendered on the General tab
    expect(screen.getByText("Provider")).toBeInTheDocument();
    expect(screen.getByText("Type")).toBeInTheDocument();
    expect(screen.getByText("Key")).toBeInTheDocument();
    expect(screen.getByText("Status")).toBeInTheDocument();
  });

  it("renders empty provider table in non-Tauri mode", () => {
    render(<SettingsScreen go={go} />);
    // In non-Tauri mode, displayKeys starts empty (customProviders = [])
    // The table header row is rendered but no provider rows
    expect(screen.getByText("API keys")).toBeInTheDocument();
    expect(screen.getByText("Provider")).toBeInTheDocument();
  });

  it("renders the Nurse defaults section", async () => {
    render(<SettingsScreen go={go} />);
    await userEvent.click(screen.getByRole("button", { name: "Other" }));
    // After the Nurse-v2 rewrite, Settings shows only the master toggle + a
    // link out to the new dedicated Nurse screen. Detailed config (stall
    // threshold, tick interval, model picker, health detail) lives at
    // `screens/Nurse.tsx`.
    expect(screen.getByText("Nurse")).toBeInTheDocument();
    expect(screen.getByText(/Long-running session supervisor/i)).toBeInTheDocument();
    expect(screen.getByText(/Enable Nurse/i)).toBeInTheDocument();
  });

  it("toggles Swarms only via setNurseConfig", async () => {
    tauriState.enabled = true;
    const user = userEvent.setup();
    // Override the default to start with Nurse enabled so the swarms-only
    // toggle isn't disabled. Use mockResolvedValue (not Once) so the
    // refresh after the click also sees enabled:true.
    ipcMocks.getNurseStatus.mockResolvedValue({
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
        nurse_model: "anthropic/claude-haiku-4.5",
        max_interventions: 3,
        tick_interval_secs: 60,
        nurse_provider: null,
        swarms_only: false,
      },
      health: {
        last_tick_at: null,
        last_successful_tick_at: null,
        consecutive_failed_ticks: 0,
        consecutive_bad_parse_ticks: 0,
        consecutive_skipped_ticks: 0,
        degraded: false,
      },
    });

    render(<SettingsScreen go={go} />);
    await user.click(screen.getByRole("button", { name: "Other" }));

    const toggle = await screen.findByRole("button", { name: "Swarms only" });
    await user.click(toggle);

    await waitFor(() => {
      expect(ipcMocks.setNurseConfig).toHaveBeenCalledWith({
        swarms_only: true,
      });
    });
  });

  it("shows the table column headers", () => {
    render(<SettingsScreen go={go} />);
    expect(screen.getByText("Provider")).toBeInTheDocument();
    expect(screen.getByText("Type")).toBeInTheDocument();
    expect(screen.getByText("Key")).toBeInTheDocument();
    expect(screen.getByText("Status")).toBeInTheDocument();
  });

  it("shows version info at the bottom", async () => {
    render(<SettingsScreen go={go} />);
    await userEvent.click(screen.getByRole("button", { name: "Other" }));
    expect(screen.getByText(/Hyvemind Studio/)).toBeInTheDocument();
  });

  it("saves advanced runtime settings in Tauri mode", async () => {
    tauriState.enabled = true;
    const user = userEvent.setup();
    const updatedSettings = {
      ...mockSettings,
      concurrency_cap: 12,
      max_pi_processes: 4,
    };
    ipcMocks.setRuntimeSettings.mockResolvedValue(updatedSettings);

    render(<SettingsScreen go={go} />);

    await user.click(screen.getByRole("button", { name: "Other" }));

    const concurrencyInput = await screen.findByLabelText("Concurrency cap");
    const maxPiProcessesInput = screen.getByLabelText("Max Pi processes");

    await user.clear(concurrencyInput);
    await user.type(concurrencyInput, "12");
    await user.clear(maxPiProcessesInput);
    await user.type(maxPiProcessesInput, "4");

    await user.click(screen.getByRole("button", { name: "Save advanced settings" }));

    await waitFor(() => {
      expect(ipcMocks.setRuntimeSettings).toHaveBeenCalledWith(12, 4);
    });
    expect(await screen.findByText("Advanced settings saved")).toBeInTheDocument();
  });
});
