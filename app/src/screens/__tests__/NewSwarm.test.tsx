import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";

vi.mock("../../lib/tauri", () => ({ isTauri: () => false }));

vi.mock("../../lib/ipc", () => ({
  createSwarm: vi.fn(),
  updateSwarm: vi.fn(),
  getSwarm: vi.fn(),
  // Audit 1.11: NewSwarm now pre-checks `approved_working_dirs` before
  // any submit IPC. Whitelist `/tmp` so the existing Tauri-mode tests
  // (which use `/tmp/payments` as the rawState working_directory) pass
  // the allowlist check and reach the createSwarm/updateSwarm spies
  // instead of popping the approval modal.
  getSettings: vi.fn().mockResolvedValue({
    default_project_path: null,
    approved_working_dirs: ["/tmp"],
  }),
  requestWorkingDirApproval: vi.fn().mockResolvedValue(true),
  listHiveminds: vi.fn().mockResolvedValue([]),
}));

// Mock ModelBrowserModal — invoke onSelect when opened so role model
// selection works in tests (the real component requires network access).
vi.mock("../ModelBrowser", async () => {
  const React = await import("react");
  return {
    ModelBrowserModal: ({ open, onSelect, onClose }: any) => {
      React.useEffect(() => {
        if (open) {
          onSelect({ id: "claude-opus-4.1", provider: "anthropic" }, { thinking: "high" });
        }
      }, [open, onSelect]);
      return null;
    },
  };
});

const createTaskMock = vi.fn();
vi.mock("../../lib/taskRuntime", () => ({
  useTaskRuntime: () => ({ createTask: createTaskMock }),
}));

// audit 6.7 — NewSwarm now reads the default project path from the
// shared SettingsProvider instead of issuing its own `getSettings()`
// call. Tests stage data into `settingsMockState.default_project_path`.
const settingsMockState: { default_project_path: string | null } = {
  default_project_path: null,
};
vi.mock("../../lib/SettingsProvider", () => ({
  useSetting: (key: string) => (settingsMockState as any)[key] ?? null,
  useSettings: () => ({
    settings: settingsMockState as any,
    isLoading: false,
    error: null,
    refresh: vi.fn().mockResolvedValue(undefined),
    patchSettings: vi.fn(),
  }),
  SettingsProvider: ({ children }: { children: React.ReactNode }) => children,
}));

import { NewSwarmScreen } from "../NewSwarm";

describe("NewSwarmScreen", () => {
  const go = vi.fn();

  beforeEach(() => {
    vi.clearAllMocks();
  });

  it("renders without crashing", () => {
    render(<NewSwarmScreen go={go} />);
    expect(screen.getByText("Configure a swarm")).toBeInTheDocument();
  });

  it("renders all 4 role rows", () => {
    render(<NewSwarmScreen go={go} />);
    expect(screen.getByText("Queen")).toBeInTheDocument();
    expect(screen.getByText("Scout")).toBeInTheDocument();
    expect(screen.getByText("Worker")).toBeInTheDocument();
    expect(screen.getByText("Guard")).toBeInTheDocument();
  });

  it("shows role descriptions", () => {
    render(<NewSwarmScreen go={go} />);
    expect(
      screen.getByText("Plans the work and decomposes into features"),
    ).toBeInTheDocument();
    expect(
      screen.getByText("Implements features in parallel"),
    ).toBeInTheDocument();
  });

  it("renders the Identity section with name and directory fields", () => {
    render(<NewSwarmScreen go={go} />);
    expect(screen.getByText("01 \u00B7 Identity")).toBeInTheDocument();
    expect(screen.getByText("Swarm name")).toBeInTheDocument();
    expect(screen.getByText("Working directory")).toBeInTheDocument();
  });

  it("renders the Agent models section", () => {
    render(<NewSwarmScreen go={go} />);
    expect(screen.getByText("02 \u00B7 Agent models")).toBeInTheDocument();
  });

  it("has the Start Planning button in create mode", () => {
    render(<NewSwarmScreen go={go} />);
    expect(
      screen.getByText(/Start Planning/),
    ).toBeInTheDocument();
  });

  it("shows Save changes button in edit mode", () => {
    const mockSwarm = {
      name: "test-swarm",
      cwd: "~/code/test",
      hivemind: "enhance",
      queen: "claude-opus-4.1",
      scout: "claude-sonnet-4.5",
      worker: "deepseek-v3.2",
      guard: "gpt-5-codex",
    };
    render(<NewSwarmScreen go={go} swarm={mockSwarm} edit={true} />);
    expect(screen.getByText("Save changes")).toBeInTheDocument();
  });

  describe("edit mode — Save changes", () => {
    const rawState = {
      id: "swarm-abc-123",
      name: "payments-v2",
      working_directory: "/tmp/payments",
      status: "planning",
      current_phase: "planning",
      current_feature_index: 0,
      created_at: "2025-01-01T00:00:00Z",
      updated_at: "2025-01-01T00:00:00Z",
      error: null,
      model_settings: {
        primary_model: "anthropic/claude-opus-4.1",
        scout_model: "anthropic/claude-sonnet-4.5",
        guard_model: "openai/gpt-5-codex",
        scout_thinking_level: "high",
        worker_thinking_level: "medium",
        guard_thinking_level: "medium",
        queen_thinking_level: "high",
        use_hivemind_on_queen: false,
        use_hivemind_on_scout: false,
        hivemind_id: null,
      },
    };

    it("calls ipc.updateSwarm with id/name/cwd/modelSettings and navigates back on success (Tauri mode)", async () => {
      const tauriMock = await import("../../lib/tauri");
      vi.spyOn(tauriMock, "isTauri").mockReturnValue(true);
      const ipcMock = await import("../../lib/ipc");
      const getSwarmSpy = ipcMock.getSwarm as ReturnType<typeof vi.fn>;
      getSwarmSpy.mockResolvedValue(rawState);
      const updateSpy = ipcMock.updateSwarm as ReturnType<typeof vi.fn>;
      updateSpy.mockResolvedValue({ ...rawState, name: "payments-v2" });

      const user = userEvent.setup();
      render(<NewSwarmScreen go={go} swarm={rawState} edit={true} />);
      // Wait for the getSwarm load to settle (button label flips off "Loading…").
      const save = await screen.findByText("Save changes");
      await user.click(save);

      expect(updateSpy).toHaveBeenCalledTimes(1);
      const args = updateSpy.mock.calls[0];
      expect(args[0]).toBe("swarm-abc-123");
      expect(args[1]).toBe("payments-v2");
      expect(args[2]).toBe("/tmp/payments");
      expect(args[3]).toMatchObject({
        primary_model: "anthropic/claude-opus-4.1",
        scout_model: "anthropic/claude-sonnet-4.5",
        guard_model: "openai/gpt-5-codex",
        scout_thinking_level: "high",
        queen_thinking_level: "high",
        worker_thinking_level: "medium",
        guard_thinking_level: "medium",
        use_hivemind_on_queen: false,
        use_hivemind_on_scout: false,
        hivemind_id: null,
      });
      expect(go).toHaveBeenCalledWith("swarms");

      vi.restoreAllMocks();
    });

    it("surfaces the backend error and does not navigate when updateSwarm rejects", async () => {
      const tauriMock = await import("../../lib/tauri");
      vi.spyOn(tauriMock, "isTauri").mockReturnValue(true);
      const ipcMock = await import("../../lib/ipc");
      const getSwarmSpy = ipcMock.getSwarm as ReturnType<typeof vi.fn>;
      getSwarmSpy.mockResolvedValue(rawState);
      const updateSpy = ipcMock.updateSwarm as ReturnType<typeof vi.fn>;
      updateSpy.mockRejectedValue(
        new Error("cannot edit a running swarm; pause or stop first"),
      );
      const errSpy = vi.spyOn(console, "error").mockImplementation(() => {});

      const user = userEvent.setup();
      render(<NewSwarmScreen go={go} swarm={rawState} edit={true} />);
      const save = await screen.findByText("Save changes");
      await user.click(save);

      // Error message rendered in the submit-error slot.
      expect(
        await screen.findByText(/cannot edit a running swarm/),
      ).toBeInTheDocument();
      // No navigation on failure.
      expect(go).not.toHaveBeenCalledWith("swarms");

      errSpy.mockRestore();
      vi.restoreAllMocks();
    });

    it("navigates to swarms in non-Tauri mode without calling updateSwarm", async () => {
      const ipcMock = await import("../../lib/ipc");
      const updateSpy = ipcMock.updateSwarm as ReturnType<typeof vi.fn>;
      updateSpy.mockClear();

      const user = userEvent.setup();
      render(<NewSwarmScreen go={go} swarm={rawState} edit={true} />);
      await user.click(screen.getByText("Save changes"));

      expect(updateSpy).not.toHaveBeenCalled();
      expect(go).toHaveBeenCalledWith("swarms");
    });

    it("seeds form fields from the raw SwarmState (name, cwd, thinking levels)", () => {
      // In non-Tauri mode the prop-based fallback path is used (no
      // getSwarm fetch), so the form is seeded directly from `rawState`.
      render(<NewSwarmScreen go={go} swarm={rawState} edit={true} />);
      expect(screen.getByDisplayValue("payments-v2")).toBeInTheDocument();
      expect(screen.getByDisplayValue("/tmp/payments")).toBeInTheDocument();
    });

    it("reflects use_hivemind_on_queen=false from getSwarm — hivemind checkbox starts unchecked (regression)", async () => {
      const tauriMock = await import("../../lib/tauri");
      vi.spyOn(tauriMock, "isTauri").mockReturnValue(true);
      const ipcMock = await import("../../lib/ipc");
      const getSwarmSpy = ipcMock.getSwarm as ReturnType<typeof vi.fn>;
      getSwarmSpy.mockResolvedValue(rawState); // both flags false
      const updateSpy = ipcMock.updateSwarm as ReturnType<typeof vi.fn>;
      updateSpy.mockResolvedValue({});

      const user = userEvent.setup();
      render(<NewSwarmScreen go={go} swarm={rawState} edit={true} />);
      await screen.findByText("Save changes");
      await user.click(screen.getByText("Save changes"));

      // Without toggling anything, the form must round-trip both flags as
      // false — today's bug is that they default to true on edit.
      expect(updateSpy).toHaveBeenCalledTimes(1);
      const ms = updateSpy.mock.calls[0][3];
      expect(ms.use_hivemind_on_queen).toBe(false);
      expect(ms.use_hivemind_on_scout).toBe(false);

      vi.restoreAllMocks();
    });

    it("toggling Queen hivemind off in edit mode persists use_hivemind_on_queen=false (regression)", async () => {
      const tauriMock = await import("../../lib/tauri");
      vi.spyOn(tauriMock, "isTauri").mockReturnValue(true);
      const ipcMock = await import("../../lib/ipc");
      const queenOnState = {
        ...rawState,
        model_settings: {
          ...rawState.model_settings,
          use_hivemind_on_queen: true,
          hivemind_id: "enhance",
        },
      };
      const getSwarmSpy = ipcMock.getSwarm as ReturnType<typeof vi.fn>;
      getSwarmSpy.mockResolvedValue(queenOnState);
      const updateSpy = ipcMock.updateSwarm as ReturnType<typeof vi.fn>;
      updateSpy.mockResolvedValue({});

      const user = userEvent.setup();
      render(<NewSwarmScreen go={go} swarm={queenOnState} edit={true} />);
      await screen.findByText("Save changes");

      // Find the Queen review-row checkbox and toggle it OFF. The checkbox
      // is the first <button> inside the first "Review with hivemind" row.
      const reviewLabels = screen.getAllByText("Review with hivemind");
      const queenRow = reviewLabels[0].closest("div")
        ?.parentElement as HTMLElement;
      const queenCheckbox = queenRow.querySelector(
        "button",
      ) as HTMLButtonElement;
      await user.click(queenCheckbox);

      await user.click(screen.getByText("Save changes"));

      expect(updateSpy).toHaveBeenCalledTimes(1);
      const ms = updateSpy.mock.calls[0][3];
      expect(ms.use_hivemind_on_queen).toBe(false);

      vi.restoreAllMocks();
    });
  });

  it("creates a swarm-linked task and navigates to tasks on Start Planning click (mock mode)", async () => {
    const user = userEvent.setup();
    render(<NewSwarmScreen go={go} />);
    // First select Queen and Scout models by clicking their model buttons.
    // The mocked ModelBrowserModal fires onSelect when opened, which sets the model.
    const modelBtns = screen.getAllByText("Select a model");
    // modelBtns are span elements; their parent button is the clickable target.
    await user.click(modelBtns[0].closest("button")!);  // Queen
    await user.click(modelBtns[1].closest("button")!);  // Scout
    const btn = screen.getByText(/Start Planning/);
    await user.click(btn);
    expect(createTaskMock).toHaveBeenCalledWith(
      expect.objectContaining({
        swarmId: expect.stringMatching(/^mock-/),
        setActive: true,
      }),
    );
    expect(go).toHaveBeenCalledWith("tasks");
  });

  it("has Cancel button that navigates back to swarms", async () => {
    const user = userEvent.setup();
    render(<NewSwarmScreen go={go} />);
    const cancel = screen.getByText("Cancel");
    await user.click(cancel);
    expect(go).toHaveBeenCalledWith("swarms");
  });

  it("has a Browse button for directory", () => {
    render(<NewSwarmScreen go={go} />);
    expect(screen.getByText("Browse")).toBeInTheDocument();
  });

  it("defaults cwd to the fallback path in non-Tauri mode", () => {
    render(<NewSwarmScreen go={go} />);
    const input = screen.getByDisplayValue("~/code/atlas/services/payments");
    expect(input).toBeInTheDocument();
  });

  it("pre-populates cwd from settings default_project_path in Tauri mode", async () => {
    const tauriMock = await import("../../lib/tauri");
    vi.spyOn(tauriMock, "isTauri").mockReturnValue(true);

    // audit 6.7 — settings now flow via the shared SettingsProvider mock.
    settingsMockState.default_project_path = "/home/user/projects/my-app";

    render(<NewSwarmScreen go={go} />);

    const input = await screen.findByDisplayValue("/home/user/projects/my-app");
    expect(input).toBeInTheDocument();

    settingsMockState.default_project_path = null;
    vi.restoreAllMocks();
  });
});
