import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { render, screen, waitFor, act, cleanup } from "@testing-library/react";
import userEvent from "@testing-library/user-event";

afterEach(() => cleanup());

// `isTauri` is mocked per-test where the live path matters. The default
// for the legacy mock-data tests is `false`.
vi.mock("../../lib/tauri", () => ({ isTauri: vi.fn(() => false) }));

vi.mock("../../lib/ipc", () => ({
  listSwarms: vi.fn(),
  createSwarm: vi.fn(),
  startSwarm: vi.fn(),
  deleteSwarm: vi.fn(),
  getSwarmFeatures: vi.fn(),
  getSwarmUsage: vi.fn(),
  getSwarmMilestones: vi.fn(),
  listHiveminds: vi.fn(),
  resumeSwarm: vi.fn(),
}));

vi.mock("../../lib/events", () => ({
  onSwarmEvent: vi.fn().mockResolvedValue(vi.fn()),
  onSwarmReconciled: vi.fn().mockResolvedValue(vi.fn()),
  safeUnlisten: vi.fn(),
}));

vi.mock("../../lib/confirm", () => ({
  confirmDialog: vi.fn().mockResolvedValue(true),
}));

vi.mock("../../lib/sounds", () => ({
  getCompletionSoundConfig: () => ({ enabled: false, sound: "default" }),
  playCompletionSound: vi.fn(),
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

vi.mock("../../lib/taskRuntime", () => ({
  useTaskRuntime: () => ({
    localTasks: [],
    createTask: vi.fn(),
    setActiveTask: vi.fn(),
  }),
}));

import { SwarmsScreen } from "../Swarms";
import { isTauri } from "../../lib/tauri";
import {
  listSwarms,
  deleteSwarm,
  getSwarmFeatures,
  getSwarmUsage,
  getSwarmMilestones,
  listHiveminds,
} from "../../lib/ipc";
import { confirmDialog } from "../../lib/confirm";

describe("SwarmsScreen (mock-data path)", () => {
  const go = vi.fn();

  beforeEach(() => {
    vi.clearAllMocks();
    (isTauri as unknown as ReturnType<typeof vi.fn>).mockReturnValue(false);
  });

  it("renders without crashing", () => {
    render(<SwarmsScreen go={go} />);
    expect(screen.getByText("Swarms")).toBeInTheDocument();
  });

  it("shows swarm cards from mock data", () => {
    render(<SwarmsScreen go={go} />);
    expect(screen.getByText("auth-refactor")).toBeInTheDocument();
    expect(screen.getByText("payments-v2")).toBeInTheDocument();
  });

  it("renders filter buttons", () => {
    render(<SwarmsScreen go={go} />);
    expect(screen.getByText("all")).toBeInTheDocument();
    expect(screen.getByText(/^running/)).toBeInTheDocument();
    expect(screen.getByText(/^completed/)).toBeInTheDocument();
    expect(screen.getByText(/^paused/)).toBeInTheDocument();
    expect(screen.getByText(/^failed/)).toBeInTheDocument();
  });

  it("has New Swarm button", () => {
    render(<SwarmsScreen go={go} />);
    const newSwarmBtns = screen.getAllByText("New Swarm");
    expect(newSwarmBtns.length).toBeGreaterThanOrEqual(1);
  });

  it("navigates to new-swarm when New Swarm is clicked", async () => {
    const user = userEvent.setup();
    render(<SwarmsScreen go={go} />);
    const newSwarmBtns = screen.getAllByText("New Swarm");
    await user.click(newSwarmBtns[0]);
    expect(go).toHaveBeenCalledWith("new-swarm");
  });

  it("renders Edit Swarm buttons on each card", () => {
    render(<SwarmsScreen go={go} />);
    const editBtns = screen.getAllByText("Edit Swarm");
    expect(editBtns.length).toBeGreaterThanOrEqual(1);
  });

  it("navigates to new-swarm in edit mode when Edit Swarm is clicked", async () => {
    const user = userEvent.setup();
    render(<SwarmsScreen go={go} />);
    const editBtns = screen.getAllByText("Edit Swarm");
    await user.click(editBtns[0]);
    expect(go).toHaveBeenCalledWith(
      "new-swarm",
      expect.objectContaining({ edit: true }),
    );
  });

  it("shows stats strip with Active, Paused, etc.", () => {
    render(<SwarmsScreen go={go} />);
    expect(screen.getByText("Active")).toBeInTheDocument();
    const pausedEls = screen.getAllByText(/Paused/);
    expect(pausedEls.length).toBeGreaterThanOrEqual(1);
  });

  // The mock SWARMS dataset contains one swarm of each of the five
  // statuses (running, planning, paused, completed, failed). We assert
  // that a Delete button is rendered for *every* card so users can always
  // remove a swarm. Regression coverage for the bug where running/paused
  // cards omitted the Delete button entirely.
  it("renders a Delete button on every swarm card across all statuses", () => {
    render(<SwarmsScreen go={go} />);
    const deleteBtns = screen.getAllByText("Delete");
    expect(deleteBtns.length).toBe(5);
  });

  it("renders Delete in the footer for the running-status card", () => {
    render(<SwarmsScreen go={go} />);
    const nameEl = screen.getByText("auth-refactor");
    let card: HTMLElement | null = nameEl;
    while (card && !card.className.includes("bg-ink-800")) {
      card = card.parentElement;
    }
    expect(card).not.toBeNull();
    const cardEl = card as HTMLElement;
    const footer = cardEl.querySelector(
      '[data-testid="swarm-card-footer"]',
    ) as HTMLElement | null;
    expect(footer).not.toBeNull();
    const deleteBtn = Array.from(footer!.querySelectorAll("button")).find(
      (b) => b.textContent?.includes("Delete"),
    );
    expect(deleteBtn).toBeDefined();
  });

  it("renders Delete in the footer for the paused-status card", () => {
    render(<SwarmsScreen go={go} />);
    const nameEl = screen.getByText("mobile-onboarding");
    let card: HTMLElement | null = nameEl;
    while (card && !card.className.includes("bg-ink-800")) {
      card = card.parentElement;
    }
    expect(card).not.toBeNull();
    const cardEl = card as HTMLElement;
    const footer = cardEl.querySelector(
      '[data-testid="swarm-card-footer"]',
    ) as HTMLElement | null;
    expect(footer).not.toBeNull();
    const deleteBtn = Array.from(footer!.querySelectorAll("button")).find(
      (b) => b.textContent?.includes("Delete"),
    );
    expect(deleteBtn).toBeDefined();
  });
});

/* ── Live (Tauri) path ─────────────────────────────────────── */

describe("SwarmsScreen (live Tauri path)", () => {
  const go = vi.fn();

  // Helper: a single completed-status swarm with one of each role.
  const makeSwarmState = (overrides: Partial<any> = {}) => ({
    id: "swarm-1",
    name: "test-swarm",
    status: "completed",
    working_directory: "~/code/test",
    model_settings: {
      primary_model: "anthropic/claude-sonnet-4",
      scout_model: "anthropic/claude-sonnet-4",
      guard_model: "anthropic/claude-sonnet-4",
      scout_thinking_level: "medium",
      worker_thinking_level: "medium",
      guard_thinking_level: "medium",
      queen_thinking_level: "medium",
      use_hivemind_on_scout: false,
      use_hivemind_on_queen: false,
      hivemind_id: null,
    },
    current_phase: "executing",
    current_feature_index: 0,
    created_at: new Date(Date.now() - 90_000).toISOString(),
    updated_at: new Date().toISOString(),
    error: null,
    ...overrides,
  });

  const makeFeature = (status: string, idx: number) => ({
    id: `feat-${idx}`,
    title: `Feature ${idx}`,
    description: "",
    status,
    dependencies: [],
  });

  beforeEach(() => {
    vi.clearAllMocks();
    (isTauri as unknown as ReturnType<typeof vi.fn>).mockReturnValue(true);
    (listHiveminds as unknown as ReturnType<typeof vi.fn>).mockResolvedValue([]);
    (getSwarmFeatures as unknown as ReturnType<typeof vi.fn>).mockResolvedValue([]);
    (getSwarmUsage as unknown as ReturnType<typeof vi.fn>).mockResolvedValue({
      input_tokens: 0,
      output_tokens: 0,
      cost: 0,
      duration_ms: 0,
    });
    (getSwarmMilestones as unknown as ReturnType<typeof vi.fn>).mockResolvedValue([]);
    (deleteSwarm as unknown as ReturnType<typeof vi.fn>).mockResolvedValue(undefined);
    (confirmDialog as unknown as ReturnType<typeof vi.fn>).mockResolvedValue(true);
  });

  it("renders 2/5 features when getSwarmFeatures returns 5 with 2 completed", async () => {
    (listSwarms as unknown as ReturnType<typeof vi.fn>).mockResolvedValue([
      makeSwarmState(),
    ]);
    (getSwarmFeatures as unknown as ReturnType<typeof vi.fn>).mockResolvedValue([
      makeFeature("completed", 1),
      makeFeature("completed", 2),
      makeFeature("pending", 3),
      makeFeature("pending", 4),
      makeFeature("pending", 5),
    ]);

    render(<SwarmsScreen go={go} />);

    await waitFor(() => {
      expect(screen.getByText("2/5")).toBeInTheDocument();
    });
  });

  it("renders cost from getSwarmUsage and non-zero duration", async () => {
    (listSwarms as unknown as ReturnType<typeof vi.fn>).mockResolvedValue([
      makeSwarmState(),
    ]);
    (getSwarmUsage as unknown as ReturnType<typeof vi.fn>).mockResolvedValue({
      input_tokens: 1000,
      output_tokens: 500,
      cost: 0.12,
      duration_ms: 90_000,
    });

    render(<SwarmsScreen go={go} />);

    await waitFor(() => {
      expect(screen.getByText("test-swarm")).toBeInTheDocument();
    });
    // `formatCost` renders cost < 1 with 3 decimals.
    await waitFor(() => {
      expect(screen.getByText("$0.120")).toBeInTheDocument();
    });
    // Duration should be non-zero — for ~90s created_at, expect "1m Xs"
    await waitFor(() => {
      const card = screen.getByText("test-swarm").closest(".bg-ink-800");
      expect(card).not.toBeNull();
      expect(card!.textContent).toMatch(/1m \d+s|\dm \d+s/);
    });
  });

  it("resolves hivemind name when id matches a HivemindSummary", async () => {
    (listSwarms as unknown as ReturnType<typeof vi.fn>).mockResolvedValue([
      makeSwarmState({
        model_settings: {
          primary_model: "anthropic/claude-sonnet-4",
          scout_model: "anthropic/claude-sonnet-4",
          guard_model: "anthropic/claude-sonnet-4",
          scout_thinking_level: "medium",
          worker_thinking_level: "medium",
          guard_thinking_level: "medium",
          queen_thinking_level: "medium",
          use_hivemind_on_scout: true,
          use_hivemind_on_queen: false,
          hivemind_id: "hm-abc12345-6789-0000-aaaa-bbbbccccdddd",
        },
      }),
    ]);
    (listHiveminds as unknown as ReturnType<typeof vi.fn>).mockResolvedValue([
      {
        id: "hm-abc12345-6789-0000-aaaa-bbbbccccdddd",
        name: "security-review",
        description: "",
        rounds_config: "[]",
        inherit_orchestrator: true,
        orchestrator_model: null,
        orchestrator_provider: null,
        orchestrator_thinking: "medium",
        orchestrator_context_window: null,
        orchestrator_max_output: null,
        runs: 0,
        created_at: "",
        updated_at: "",
      },
    ]);

    render(<SwarmsScreen go={go} />);

    // The resolved name appears in the stats chip AND in each role chip
    // that opts into hivemind (Queen / Scout). The bottom chip is the
    // canonical signal — assert at least one occurrence.
    await waitFor(() => {
      expect(screen.getAllByText("security-review").length).toBeGreaterThan(0);
    });
  });

  it("falls back to truncated UUID when hivemind_id doesn't match any summary", async () => {
    (listSwarms as unknown as ReturnType<typeof vi.fn>).mockResolvedValue([
      makeSwarmState({
        model_settings: {
          primary_model: "anthropic/claude-sonnet-4",
          scout_model: "anthropic/claude-sonnet-4",
          guard_model: "anthropic/claude-sonnet-4",
          scout_thinking_level: "medium",
          worker_thinking_level: "medium",
          guard_thinking_level: "medium",
          queen_thinking_level: "medium",
          use_hivemind_on_scout: true,
          use_hivemind_on_queen: false,
          hivemind_id: "hm-abcd1234-aaaa-bbbb-cccc-ddddeeeeffff",
        },
      }),
    ]);
    (listHiveminds as unknown as ReturnType<typeof vi.fn>).mockResolvedValue([]);

    render(<SwarmsScreen go={go} />);

    // First 8 chars + ellipsis, rendered in stats chip + role chips.
    await waitFor(() => {
      expect(screen.getAllByText("hm-abcd1\u2026").length).toBeGreaterThan(0);
    });
  });

  it("renders a Resume button (not Retry) on failed swarm cards", async () => {
    (listSwarms as unknown as ReturnType<typeof vi.fn>).mockResolvedValue([
      makeSwarmState({ status: "failed", error: "boom" }),
    ]);

    render(<SwarmsScreen go={go} />);

    await waitFor(() => {
      expect(screen.getByText("test-swarm")).toBeInTheDocument();
    });
    expect(screen.queryByText("Retry")).toBeNull();
    expect(screen.getByText("Resume")).toBeInTheDocument();
  });

  it("renders a Resume button on failed swarm cards and invokes resumeSwarm on click", async () => {
    const { resumeSwarm } = await import("../../lib/ipc");
    (resumeSwarm as unknown as ReturnType<typeof vi.fn>).mockResolvedValue(undefined);

    (listSwarms as unknown as ReturnType<typeof vi.fn>).mockResolvedValue([
      makeSwarmState({ status: "failed", error: "something broke" }),
    ]);

    render(<SwarmsScreen go={go} />);
    await waitFor(() => {
      expect(screen.getByText("Resume")).toBeInTheDocument();
    });

    await userEvent.click(screen.getByText("Resume"));
    await waitFor(() => {
      expect(resumeSwarm).toHaveBeenCalledWith("swarm-1");
    });
  });

  it("Delete calls confirmDialog and only invokes deleteSwarm on true; shows Deleting\u2026 while pending", async () => {
    const user = userEvent.setup();
    (listSwarms as unknown as ReturnType<typeof vi.fn>).mockResolvedValue([
      makeSwarmState({ status: "completed" }),
    ]);
    let resolveDelete: (() => void) | null = null;
    (deleteSwarm as unknown as ReturnType<typeof vi.fn>).mockImplementation(
      () => new Promise<void>((res) => { resolveDelete = () => res(); }),
    );

    render(<SwarmsScreen go={go} />);

    await waitFor(() => {
      expect(screen.getByText("test-swarm")).toBeInTheDocument();
    });

    const deleteBtn = screen.getByText("Delete");
    await user.click(deleteBtn);

    expect(confirmDialog).toHaveBeenCalled();

    await waitFor(() => {
      expect(deleteSwarm).toHaveBeenCalledWith("swarm-1");
    });

    // While the deletion is pending the button reads "Deleting…" and is disabled.
    await waitFor(() => {
      const btn = screen.getByText("Deleting\u2026").closest("button");
      expect(btn).not.toBeNull();
      expect(btn!.hasAttribute("disabled")).toBe(true);
    });

    // Resolve the delete and verify Cancel-confirm path is independent.
    await act(async () => {
      resolveDelete?.();
    });
  });

  it("does NOT call deleteSwarm when confirmDialog resolves false", async () => {
    const user = userEvent.setup();
    (listSwarms as unknown as ReturnType<typeof vi.fn>).mockResolvedValue([
      makeSwarmState({ status: "completed" }),
    ]);
    (confirmDialog as unknown as ReturnType<typeof vi.fn>).mockResolvedValueOnce(false);

    render(<SwarmsScreen go={go} />);

    await waitFor(() => {
      expect(screen.getByText("test-swarm")).toBeInTheDocument();
    });

    await user.click(screen.getByText("Delete"));
    expect(confirmDialog).toHaveBeenCalled();
    // Give any pending microtasks a chance to flush.
    await new Promise((r) => setTimeout(r, 10));
    expect(deleteSwarm).not.toHaveBeenCalled();
  });

  it("Delete, Edit Swarm, and Clone Plan all live inside the footer block", async () => {
    (listSwarms as unknown as ReturnType<typeof vi.fn>).mockResolvedValue([
      makeSwarmState({ status: "completed" }),
    ]);

    render(<SwarmsScreen go={go} />);

    await waitFor(() => {
      expect(screen.getByText("test-swarm")).toBeInTheDocument();
    });

    const card = screen.getByText("test-swarm").closest(".bg-ink-800") as HTMLElement;
    expect(card).not.toBeNull();
    const footer = card.querySelector(
      '[data-testid="swarm-card-footer"]',
    ) as HTMLElement | null;
    expect(footer).not.toBeNull();

    const buttons = Array.from(footer!.querySelectorAll("button")).map(
      (b) => b.textContent || "",
    );
    expect(buttons.some((t) => t.includes("Delete"))).toBe(true);
    expect(buttons.some((t) => t.includes("Edit Swarm"))).toBe(true);
    expect(buttons.some((t) => t.includes("Clone Plan"))).toBe(true);
  });
});
