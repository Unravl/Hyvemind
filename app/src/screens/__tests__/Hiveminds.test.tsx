import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import type { HivemindSummary } from "../../lib/types";

const mocks = vi.hoisted(() => ({
  tauriMode: false,
  getProviders: vi.fn(),
  listHiveminds: vi.fn(),
  createHivemind: vi.fn(),
  deleteHivemind: vi.fn(),
  // audit 6.7 — Hiveminds now reads the providers list from the
  // ProvidersProvider context (see vi.mock below). Tests stage data
  // here via mockResolvedValue, and `useProviders()` reflects it
  // synchronously after the first React tick.
  useProvidersResult: { providers: [] as any[], configured: [] as any[] },
}));

vi.mock("../../lib/tauri", () => ({ isTauri: () => mocks.tauriMode }));

vi.mock("../../lib/ipc", () => ({
  getProviders: mocks.getProviders,
  listHiveminds: mocks.listHiveminds,
  createHivemind: mocks.createHivemind,
  deleteHivemind: mocks.deleteHivemind,
}));

// IMPORTANT: keep the returned object stable across renders. The screen does
// `const { refreshHivemindOptions } = useTaskRuntime()` and then uses that
// function inside a `useCallback` dependency list; if the mock returns a
// fresh `vi.fn()` on every render, the callback identity changes every render,
// the `useEffect([loadHiveminds])` re-fires, and we loop until the worker
// OOMs. Cache the function so the identity is stable.
const stableRefreshHivemindOptions = vi.hoisted(() => vi.fn().mockResolvedValue(undefined));
vi.mock("../../lib/taskRuntime", () => ({
  useTaskRuntime: () => ({
    refreshHivemindOptions: stableRefreshHivemindOptions,
  }),
}));

// audit 6.7 — Hiveminds now reads the configured-providers list from
// the ProvidersProvider context. We stub the provider so tests stage
// data into `mocks.useProvidersResult` (managed below in beforeEach +
// any test that wants to assert on a custom provider set).
vi.mock("../../lib/ProvidersProvider", () => ({
  useProviders: () => ({
    providers: mocks.useProvidersResult.providers,
    configured: mocks.useProvidersResult.configured,
    isLoading: false,
    error: null,
    refresh: vi.fn().mockResolvedValue(undefined),
  }),
  ProvidersProvider: ({ children }: { children: React.ReactNode }) => children,
}));

// Mock HivemindEditModal
vi.mock("../HivemindEdit", () => ({
  HivemindEditModal: () => null,
}));

import { HivemindsScreen } from "../Hiveminds";

const makeSummary = (overrides: Partial<HivemindSummary> = {}): HivemindSummary => ({
  id: "hm-orig",
  name: "test-team",
  description: "A test hivemind.",
  runs: 5,
  rounds_config: JSON.stringify([
    { timeout: 300, models: [{ id: "gpt-4o", provider: "openai" }] },
  ]),
  inherit_orchestrator: true,
  orchestrator_model: null,
  orchestrator_provider: null,
  orchestrator_thinking: "high",
  orchestrator_context_window: null,
  orchestrator_max_output: null,
  created_at: "2026-05-09T00:00:00Z",
  updated_at: "2026-05-09T00:00:00Z",
  ...overrides,
});

describe("HivemindsScreen", () => {
  const go = vi.fn();

  // audit 6.7 — helper kept around so any test that previously
  // staged providers via `mocks.getProviders.mockResolvedValue([...])`
  // can stage the same list synchronously into `useProviders()`'s
  // mocked return value.
  function stageProviders(list: any[]) {
    mocks.getProviders.mockResolvedValue(list);
    mocks.useProvidersResult.providers = list;
    mocks.useProvidersResult.configured = list.filter((p) => p.configured);
  }

  beforeEach(() => {
    mocks.tauriMode = false;
    vi.clearAllMocks();
    stageProviders([]);
    mocks.listHiveminds.mockResolvedValue([]);
    mocks.createHivemind.mockResolvedValue(makeSummary({ id: "hm-cloned", name: "test-team (Clone)" }));
    mocks.deleteHivemind.mockResolvedValue(undefined);
  });

  it("renders without crashing", () => {
    render(<HivemindsScreen go={go} />);
    expect(screen.getByText("Hiveminds")).toBeInTheDocument();
  });

  it("renders hivemind pipeline cards", () => {
    render(<HivemindsScreen go={go} />);
    // HIVEMINDS mock data contains these names
    expect(screen.getByText("enhance")).toBeInTheDocument();
    expect(screen.getByText("arch-council")).toBeInTheDocument();
  });

  it("shows model names in round labels", () => {
    render(<HivemindsScreen go={go} />);
    // Each hivemind card has Round labels
    expect(screen.getAllByText("Round 1").length).toBeGreaterThanOrEqual(1);
  });

  it("renders Edit button on each card", () => {
    render(<HivemindsScreen go={go} />);
    const editBtns = screen.getAllByText("Edit");
    expect(editBtns.length).toBeGreaterThanOrEqual(1);
  });

  it("renders History button on each card", () => {
    render(<HivemindsScreen go={go} />);
    const histBtns = screen.getAllByText("History");
    expect(histBtns.length).toBeGreaterThanOrEqual(1);
  });

  it("has New Hivemind button", () => {
    render(<HivemindsScreen go={go} />);
    expect(screen.getByText("New Hivemind")).toBeInTheDocument();
  });

  it("navigates to review-history on History click", async () => {
    const user = userEvent.setup();
    render(<HivemindsScreen go={go} />);
    const histBtns = screen.getAllByText("History");
    await user.click(histBtns[0]);
    expect(go).toHaveBeenCalledWith(
      "review-history",
      expect.objectContaining({ hivemind: expect.any(Object) }),
    );
  });

  it("shows pagination controls", () => {
    render(<HivemindsScreen go={go} />);
    expect(screen.getByText("Prev")).toBeInTheDocument();
    expect(screen.getByText("Next")).toBeInTheDocument();
  });

  it("shows provider legend", () => {
    render(<HivemindsScreen go={go} />);
    expect(screen.getByText("Anthropic")).toBeInTheDocument();
    expect(screen.getByText("OpenAI")).toBeInTheDocument();
    expect(screen.getByText("OpenRouter")).toBeInTheDocument();
  });

  it("uses provider color from Tauri rounds_config for model dots", async () => {
    mocks.tauriMode = true;
    stageProviders([
      {
        name: "openai",
        display_name: "OpenAI",
        provider_type: "openai",
        endpoint: null,
        configured: true,
        model_count: 1,
        health: true,
      },
    ]);
    mocks.listHiveminds.mockResolvedValue([
      {
        id: "hm-tauri",
        name: "tauri-team",
        description: "Provider colors from saved config.",
        runs: 3,
        rounds_config: JSON.stringify([
          {
            timeout: 300,
            models: [{ id: "gpt-custom", provider: "openai" }],
          },
        ]),
        inherit_orchestrator: true,
        orchestrator_model: null,
        orchestrator_provider: null,
        orchestrator_thinking: "high",
        created_at: "2026-05-09T00:00:00Z",
        updated_at: "2026-05-09T00:00:00Z",
      },
    ]);

    render(<HivemindsScreen go={go} />);

    const row = await screen.findByTitle("openai/gpt-custom");
    const dot = row.querySelector("span") as HTMLElement;
    expect(dot).toHaveStyle({ background: "#10a37f" });
  });

  it("uses the same computed color for a custom provider legend and model dot", async () => {
    mocks.tauriMode = true;
    stageProviders([
      {
        name: "mycloud",
        display_name: "MyCloud",
        provider_type: "custom",
        endpoint: null,
        configured: true,
        model_count: 1,
        health: true,
      },
    ]);
    mocks.listHiveminds.mockResolvedValue([
      {
        id: "hm-custom",
        name: "custom-team",
        description: "Custom provider colors.",
        runs: 0,
        rounds_config: JSON.stringify([
          { timeout: 300, models: [{ id: "special-model", provider: "mycloud" }] },
        ]),
        inherit_orchestrator: true,
        orchestrator_model: null,
        orchestrator_provider: null,
        orchestrator_thinking: "high",
        created_at: "2026-05-09T00:00:00Z",
        updated_at: "2026-05-09T00:00:00Z",
      },
    ]);

    render(<HivemindsScreen go={go} />);

    const legend = await screen.findByText("MyCloud");
    const legendDot = legend.querySelector("span") as HTMLElement;
    const row = await screen.findByTitle("mycloud/special-model");
    const modelDot = row.querySelector("span") as HTMLElement;

    expect(modelDot.style.background).toBe(legendDot.style.background);
    expect(modelDot).not.toHaveStyle({ background: "#ec4899" });
  });

  it("gives two custom providers different colors", async () => {
    mocks.tauriMode = true;
    stageProviders([
      {
        name: "alpha",
        display_name: "Alpha",
        provider_type: "custom",
        endpoint: null,
        configured: true,
        model_count: 1,
        health: true,
      },
      {
        name: "beta",
        display_name: "Beta",
        provider_type: "custom",
        endpoint: null,
        configured: true,
        model_count: 1,
        health: true,
      },
    ]);
    mocks.listHiveminds.mockResolvedValue([
      {
        id: "hm-custom-two",
        name: "two-custom",
        description: "Two custom providers.",
        runs: 0,
        rounds_config: JSON.stringify([
          {
            timeout: 300,
            models: [
              { id: "model-a", provider: "alpha" },
              { id: "model-b", provider: "beta" },
            ],
          },
        ]),
        inherit_orchestrator: true,
        orchestrator_model: null,
        orchestrator_provider: null,
        orchestrator_thinking: "high",
        created_at: "2026-05-09T00:00:00Z",
        updated_at: "2026-05-09T00:00:00Z",
      },
    ]);

    render(<HivemindsScreen go={go} />);

    await screen.findByText("two-custom");
    const alphaDot = (await screen.findByTitle("alpha/model-a")).querySelector("span") as HTMLElement;
    const betaDot = (await screen.findByTitle("beta/model-b")).querySelector("span") as HTMLElement;

    expect(alphaDot.style.background).not.toBe(betaDot.style.background);
  });

  it("preserves nested-slash model IDs without duplicating the provider prefix", async () => {
    mocks.tauriMode = true;
    mocks.listHiveminds.mockResolvedValue([
      {
        id: "hm-nested",
        name: "nested-models",
        description: "Nested OpenRouter model IDs.",
        runs: 0,
        rounds_config: JSON.stringify([
          { timeout: 300, models: ["openrouter/google/gemini-2.5-pro"] },
        ]),
        inherit_orchestrator: true,
        orchestrator_model: null,
        orchestrator_provider: null,
        orchestrator_thinking: "high",
        created_at: "2026-05-09T00:00:00Z",
        updated_at: "2026-05-09T00:00:00Z",
      },
    ]);

    render(<HivemindsScreen go={go} />);

    const row = await screen.findByTitle("openrouter/google/gemini-2.5-pro");
    expect(row).toHaveTextContent("google/gemini-2.5-pro");
    expect(row).not.toHaveTextContent("openrouter/google/gemini-2.5-pro");
  });

  it("cleans a redundant provider prefix when provider is explicit", async () => {
    mocks.tauriMode = true;
    mocks.listHiveminds.mockResolvedValue([
      {
        id: "hm-redundant",
        name: "redundant-prefix",
        description: "Legacy redundant provider prefixes.",
        runs: 0,
        rounds_config: JSON.stringify([
          {
            timeout: 300,
            models: [{ id: "openrouter/google/gemini-2.5-pro", provider: "openrouter" }],
          },
        ]),
        inherit_orchestrator: true,
        orchestrator_model: null,
        orchestrator_provider: null,
        orchestrator_thinking: "high",
        created_at: "2026-05-09T00:00:00Z",
        updated_at: "2026-05-09T00:00:00Z",
      },
    ]);

    render(<HivemindsScreen go={go} />);

    const row = await screen.findByTitle("openrouter/google/gemini-2.5-pro");
    expect(row).toHaveTextContent("google/gemini-2.5-pro");
    expect(row).not.toHaveTextContent("openrouter/google/gemini-2.5-pro");
  });

  it("uses neutral grey for empty or missing provider model dots", async () => {
    mocks.tauriMode = true;
    mocks.listHiveminds.mockResolvedValue([
      {
        id: "hm-no-provider",
        name: "no-provider",
        description: "Missing provider.",
        runs: 0,
        rounds_config: JSON.stringify([
          { timeout: 300, models: [{ id: "mystery-model", provider: "" }] },
        ]),
        inherit_orchestrator: true,
        orchestrator_model: null,
        orchestrator_provider: null,
        orchestrator_thinking: "high",
        created_at: "2026-05-09T00:00:00Z",
        updated_at: "2026-05-09T00:00:00Z",
      },
    ]);

    render(<HivemindsScreen go={go} />);

    const row = await screen.findByTitle("mystery-model");
    const dot = row.querySelector("span") as HTMLElement;
    expect(dot).toHaveStyle({ background: "#9ca3af" });
  });

  it("shows orchestrator model on card when custom orchestrator is set", async () => {
    mocks.tauriMode = true;
    mocks.getProviders.mockResolvedValue([]);
    mocks.listHiveminds.mockResolvedValue([
      {
        id: "hm-orch",
        name: "orch-team",
        description: "Has custom orchestrator.",
        runs: 1,
        rounds_config: JSON.stringify([
          { timeout: 300, models: [{ id: "gpt-custom", provider: "openai" }] },
        ]),
        inherit_orchestrator: false,
        orchestrator_model: "claude-sonnet-4",
        orchestrator_provider: "anthropic",
        orchestrator_thinking: "high",
        created_at: "2026-05-09T00:00:00Z",
        updated_at: "2026-05-09T00:00:00Z",
      },
    ]);

    render(<HivemindsScreen go={go} />);
    expect(await screen.findByText("claude-sonnet-4")).toBeInTheDocument();
  });

  it("shows 'inherits task model' when orchestrator is inherited", async () => {
    mocks.tauriMode = true;
    mocks.getProviders.mockResolvedValue([]);
    mocks.listHiveminds.mockResolvedValue([
      {
        id: "hm-inherit",
        name: "inherit-team",
        description: "Inherits orchestrator.",
        runs: 1,
        rounds_config: JSON.stringify([
          { timeout: 300, models: [{ id: "gpt-custom", provider: "openai" }] },
        ]),
        inherit_orchestrator: true,
        orchestrator_model: null,
        orchestrator_provider: null,
        orchestrator_thinking: "",
        created_at: "2026-05-09T00:00:00Z",
        updated_at: "2026-05-09T00:00:00Z",
      },
    ]);

    render(<HivemindsScreen go={go} />);
    expect(await screen.findByText("inherits task model")).toBeInTheDocument();
  });

  it("shows 'inherits task model' when inherited even when a custom model is also set", async () => {
    mocks.tauriMode = true;
    mocks.getProviders.mockResolvedValue([]);
    mocks.listHiveminds.mockResolvedValue([
      {
        id: "hm-inherit-stale",
        name: "inherit-stale",
        description: "Inherited but has stale custom model.",
        runs: 0,
        rounds_config: JSON.stringify([
          { timeout: 300, models: [{ id: "gpt-custom", provider: "openai" }] },
        ]),
        inherit_orchestrator: true,
        orchestrator_model: "claude-sonnet-4",
        orchestrator_provider: "anthropic",
        orchestrator_thinking: "high",
        created_at: "2026-05-09T00:00:00Z",
        updated_at: "2026-05-09T00:00:00Z",
      },
    ]);

    render(<HivemindsScreen go={go} />);
    expect(await screen.findByText("inherits task model")).toBeInTheDocument();
    // The custom model should NOT appear (inherited row takes precedence)
    expect(screen.queryByText("claude-sonnet-4")).not.toBeInTheDocument();
  });

  it("renders no orchestrator row when both fields are absent or falsy", async () => {
    mocks.tauriMode = true;
    mocks.getProviders.mockResolvedValue([]);
    mocks.listHiveminds.mockResolvedValue([
      {
        id: "hm-none",
        name: "no-orch",
        description: "No orchestrator configured.",
        runs: 0,
        rounds_config: JSON.stringify([
          { timeout: 300, models: [{ id: "gpt-custom", provider: "openai" }] },
        ]),
        inherit_orchestrator: false,
        orchestrator_model: null,
        orchestrator_provider: null,
        orchestrator_thinking: "",
        created_at: "2026-05-09T00:00:00Z",
        updated_at: "2026-05-09T00:00:00Z",
      },
    ]);

    render(<HivemindsScreen go={go} />);
    await screen.findByText("no-orch");
    // Neither orchestrator text should appear
    expect(screen.queryByText("inherits task model")).not.toBeInTheDocument();
    expect(screen.queryByText("claude-sonnet-4")).not.toBeInTheDocument();
  });

  it("renders no orchestrator row when model is an empty string", async () => {
    mocks.tauriMode = true;
    mocks.getProviders.mockResolvedValue([]);
    mocks.listHiveminds.mockResolvedValue([
      {
        id: "hm-empty-model",
        name: "empty-model",
        description: "Empty string orchestrator model.",
        runs: 0,
        rounds_config: JSON.stringify([]),
        inherit_orchestrator: false,
        orchestrator_model: "",
        orchestrator_provider: "anthropic",
        orchestrator_thinking: "",
        created_at: "2026-05-09T00:00:00Z",
        updated_at: "2026-05-09T00:00:00Z",
      },
    ]);

    render(<HivemindsScreen go={go} />);
    await screen.findByText("empty-model");
    expect(screen.queryByText("inherits task model")).not.toBeInTheDocument();
    // No model text rendered since empty string is guarded
  });

  it("renders orchestrator model with grey dot when provider is null", async () => {
    mocks.tauriMode = true;
    mocks.getProviders.mockResolvedValue([]);
    mocks.listHiveminds.mockResolvedValue([
      {
        id: "hm-no-prov",
        name: "no-prov",
        description: "Orchestrator model without provider.",
        runs: 0,
        rounds_config: JSON.stringify([]),
        inherit_orchestrator: false,
        orchestrator_model: "claude-sonnet-4",
        orchestrator_provider: null,
        orchestrator_thinking: "",
        created_at: "2026-05-09T00:00:00Z",
        updated_at: "2026-05-09T00:00:00Z",
      },
    ]);

    render(<HivemindsScreen go={go} />);
    expect(await screen.findByText("claude-sonnet-4")).toBeInTheDocument();
    // Verify the tooltip omits the provider prefix
    const modelEl = screen.getByText("claude-sonnet-4");
    expect(modelEl).toHaveAttribute("title", "Orchestrator: claude-sonnet-4");
  });

  it("cleans a leading slash from model strings", async () => {
    mocks.tauriMode = true;
    mocks.listHiveminds.mockResolvedValue([
      {
        id: "hm-leading-slash",
        name: "leading-slash",
        description: "Leading slash model ID.",
        runs: 0,
        rounds_config: JSON.stringify([
          { timeout: 300, models: ["/gpt-4o"] },
        ]),
        inherit_orchestrator: true,
        orchestrator_model: null,
        orchestrator_provider: null,
        orchestrator_thinking: "high",
        created_at: "2026-05-09T00:00:00Z",
        updated_at: "2026-05-09T00:00:00Z",
      },
    ]);

    render(<HivemindsScreen go={go} />);

    const row = await screen.findByTitle("gpt-4o");
    expect(row).toHaveTextContent("gpt-4o");
    expect(row).not.toHaveTextContent("/gpt-4o");
  });

  // ── Clone tests ──

  it("renders Clone button on each card in Tauri mode", async () => {
    mocks.tauriMode = true;
    mocks.listHiveminds.mockResolvedValue([makeSummary()]);

    render(<HivemindsScreen go={go} />);

    expect(await screen.findByRole("button", { name: /clone/i })).toBeInTheDocument();
  });

  it("does not render Clone button outside Tauri mode", () => {
    mocks.tauriMode = false;

    render(<HivemindsScreen go={go} />);

    expect(screen.queryByRole("button", { name: /clone/i })).not.toBeInTheDocument();
  });

  it("clones a hivemind with '(Clone)' suffix", async () => {
    mocks.tauriMode = true;

    const original = makeSummary({
      id: "hm-orig",
      name: "test-team",
      description: "A test hivemind.",
    });
    const cloned = makeSummary({
      id: "hm-cloned",
      name: "test-team (Clone)",
      description: "A test hivemind.",
    });

    mocks.listHiveminds
      .mockResolvedValueOnce([original])
      .mockResolvedValueOnce([original, cloned]);
    mocks.createHivemind.mockResolvedValue(cloned);

    const user = userEvent.setup();
    render(<HivemindsScreen go={go} />);

    await screen.findByText("test-team");
    await user.click(screen.getByRole("button", { name: /clone/i }));

    await waitFor(() => {
      expect(mocks.createHivemind).toHaveBeenCalledWith(
        "test-team (Clone)",
        "A test hivemind.",
        original.rounds_config,
        true,
        undefined,
        undefined,
        "high",
      );
    });

    expect(await screen.findByText("test-team (Clone)")).toBeInTheDocument();
  });

  it("shows an error banner when cloning fails", async () => {
    mocks.tauriMode = true;
    mocks.listHiveminds.mockResolvedValue([makeSummary()]);
    mocks.createHivemind.mockRejectedValue(new Error("backend unavailable"));

    const user = userEvent.setup();
    render(<HivemindsScreen go={go} />);

    await screen.findByText("test-team");
    await user.click(screen.getByRole("button", { name: /clone/i }));

    expect(await screen.findByText(/Failed to clone: backend unavailable/i)).toBeInTheDocument();
  });

  it("shows a refresh error when clone succeeds but list reload fails", async () => {
    mocks.tauriMode = true;
    mocks.listHiveminds
      .mockResolvedValueOnce([makeSummary()])
      .mockRejectedValueOnce(new Error("refresh failed"));
    mocks.createHivemind.mockResolvedValue(
      makeSummary({ id: "hm-cloned", name: "test-team (Clone)" }),
    );

    const user = userEvent.setup();
    render(<HivemindsScreen go={go} />);

    await screen.findByText("test-team");
    await user.click(screen.getByRole("button", { name: /clone/i }));

    expect(
      await screen.findByText(/Clone was created, but the list could not be refreshed: refresh failed/i),
    ).toBeInTheDocument();
  });

  it("shows Cloning while clone is in flight", async () => {
    mocks.tauriMode = true;
    mocks.listHiveminds.mockResolvedValue([makeSummary()]);

    let resolveClone!: (value: HivemindSummary) => void;
    mocks.createHivemind.mockReturnValue(
      new Promise<HivemindSummary>((resolve) => {
        resolveClone = resolve;
      }),
    );

    const user = userEvent.setup();
    render(<HivemindsScreen go={go} />);

    await screen.findByText("test-team");
    await user.click(screen.getByRole("button", { name: /clone/i }));

    const cloningButton = await screen.findByRole("button", { name: /cloning/i });
    expect(cloningButton).toBeDisabled();

    resolveClone(makeSummary({ id: "hm-cloned", name: "test-team (Clone)" }));
  });

  it("disables clone and delete actions on all cards while a clone is in flight", async () => {
    mocks.tauriMode = true;
    mocks.listHiveminds.mockResolvedValue([
      makeSummary({ id: "hm-a", name: "team-a" }),
      makeSummary({ id: "hm-b", name: "team-b" }),
    ]);

    let resolveClone!: (value: HivemindSummary) => void;
    mocks.createHivemind.mockReturnValue(
      new Promise<HivemindSummary>((resolve) => {
        resolveClone = resolve;
      }),
    );

    const user = userEvent.setup();
    render(<HivemindsScreen go={go} />);

    await screen.findByText("team-a");
    const actionsA = screen.getByTestId("hivemind-actions-hm-a");
    const actionsB = screen.getByTestId("hivemind-actions-hm-b");

    await user.click(within(actionsA).getByRole("button", { name: /clone/i }));

    expect(within(actionsA).getByRole("button", { name: /cloning/i })).toBeDisabled();
    expect(within(actionsB).getByRole("button", { name: /clone/i })).toBeDisabled();
    expect(within(actionsB).getByRole("button", { name: /delete/i })).toBeDisabled();
    expect(mocks.createHivemind).toHaveBeenCalledTimes(1);

    resolveClone(makeSummary({ id: "hm-a-clone", name: "team-a (Clone)" }));
  });

  it("renders Delete in the right-side action column in Tauri mode", async () => {
    mocks.tauriMode = true;
    mocks.listHiveminds.mockResolvedValue([
      makeSummary({ id: "hm-delete", name: "delete-team" }),
    ]);

    const { container } = render(<HivemindsScreen go={go} />);

    await screen.findByText("delete-team");

    const actions = screen.getByTestId("hivemind-actions-hm-delete");
    expect(within(actions).getByRole("button", { name: /delete/i })).toBeInTheDocument();
    expect(container.querySelector(".border-t")).toBeNull();
  });
});
