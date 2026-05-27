import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen } from "@testing-library/react";

vi.mock("../../lib/tauri", () => ({ isTauri: () => false }));

vi.mock("../../lib/ipc", () => ({
  getSwarm: vi.fn(),
  getSwarmFeatures: vi.fn().mockResolvedValue([]),
  getSwarmProgress: vi.fn().mockResolvedValue([]),
  getSwarmUsage: vi.fn().mockResolvedValue({ input_tokens: 0, output_tokens: 0, cost: 0, duration_ms: 0 }),
  pauseSwarm: vi.fn(),
  resumeSwarm: vi.fn(),
  stopSwarm: vi.fn(),
}));

vi.mock("../../lib/events", () => ({
  onSwarmEvent: vi.fn().mockResolvedValue(vi.fn()),
  onHivemindProgress: vi.fn().mockResolvedValue(vi.fn()),
  safeUnlisten: vi.fn(),
}));

vi.mock("../../lib/taskRuntime", () => ({
  useTaskRuntime: () => ({ hivemindOptions: [] }),
}));

import { SwarmControlScreen } from "../SwarmControl";

describe("SwarmControlScreen", () => {
  const go = vi.fn();

  const mockSwarm = {
    id: "sw-1",
    name: "auth-refactor",
    status: "running",
    duration: "1h 47m",
    cost: "$12.84",
    features: [19, 27] as [number, number],
    milestone: "M3 \u2014 Auth Hardening",
    queen: "claude-opus-4.1",
    worker: "deepseek-v3.2",
    scout: "claude-sonnet-4.5",
    hivemind: "enhance",
    cwd: "~/code/atlas/services/auth",
  };

  beforeEach(() => {
    vi.clearAllMocks();
  });

  it("renders the swarm control dashboard", () => {
    render(<SwarmControlScreen go={go} swarm={mockSwarm} />);
    expect(screen.getByText("Swarm Control")).toBeInTheDocument();
  });

  it("shows the swarm name in the header", () => {
    render(<SwarmControlScreen go={go} swarm={mockSwarm} />);
    expect(screen.getByText("auth-refactor")).toBeInTheDocument();
  });

  it("shows the feature list panel", () => {
    render(<SwarmControlScreen go={go} swarm={mockSwarm} />);
    expect(screen.getByText("Tasks")).toBeInTheDocument();
  });

  it("shows the progress log panel", () => {
    render(<SwarmControlScreen go={go} swarm={mockSwarm} />);
    expect(screen.getByText("Progress log")).toBeInTheDocument();
  });

  it("shows the Hivemind panel", () => {
    render(<SwarmControlScreen go={go} swarm={mockSwarm} />);
    expect(screen.getByText("Hivemind")).toBeInTheDocument();
  });

  it("shows Active feature panel", () => {
    render(<SwarmControlScreen go={go} swarm={mockSwarm} />);
    expect(screen.getByText("Active feature")).toBeInTheDocument();
  });

  it("shows Pause button for running swarms", () => {
    render(<SwarmControlScreen go={go} swarm={mockSwarm} />);
    expect(screen.getByText("Pause")).toBeInTheDocument();
  });

  it("shows Stop button for running swarms", () => {
    render(<SwarmControlScreen go={go} swarm={mockSwarm} />);
    expect(screen.getByText("Stop")).toBeInTheDocument();
  });

  it("shows progress counter", () => {
    render(<SwarmControlScreen go={go} swarm={mockSwarm} />);
    // In non-Tauri preview mode there is no backend Feature list, so the
    // counter renders "0/0 · 0%" rather than the old mock-derived 19/27.
    const progressEls = screen.getAllByText(/0\/0/);
    expect(progressEls.length).toBeGreaterThanOrEqual(1);
  });

  it("renders swarm usage values from getSwarmUsage", async () => {
    // The component only calls getSwarmUsage in Tauri mode — flip the
    // helper so the IPC path is exercised under test.
    const tauriMod = await import("../../lib/tauri");
    const isTauriSpy = vi.spyOn(tauriMod, "isTauri").mockReturnValue(true);
    const mockIpc = await import("../../lib/ipc");
    (mockIpc.getSwarmUsage as any).mockResolvedValue({
      input_tokens: 4500,
      output_tokens: 2200,
      cost: 4.25,
      duration_ms: 3500,
    });
    // getSwarm is also called in Tauri mode; return a minimal SwarmState.
    (mockIpc.getSwarm as any).mockResolvedValue({
      id: "sw-1",
      name: "auth-refactor",
      status: "running",
      working_directory: "/tmp",
      created_at: new Date().toISOString(),
      model_settings: {},
      milestone_id: null,
      milestone_title: null,
      hivemind_id: null,
    });
    try {
      render(<SwarmControlScreen go={go} swarm={mockSwarm} />);
      // The Stat row renders the labels `swarm·in` and `swarm·out`. Once
      // the getSwarmUsage promise resolves and the component re-renders,
      // the mocked totals are reflected in the displayed token strings
      // (`formatTokens(4500) -> "4.5k"`, `formatTokens(2200) -> "2.2k"`).
      expect(await screen.findByText("Tokens In/Out")).toBeInTheDocument();
      expect(await screen.findByText("4.5k")).toBeInTheDocument();
      expect(await screen.findByText("2.2k")).toBeInTheDocument();
      expect(screen.getAllByText("/").length).toBeGreaterThanOrEqual(1);
      expect(screen.getByText("Cached")).toBeInTheDocument();
      expect(mockIpc.getSwarmUsage).toHaveBeenCalledWith("sw-1");
    } finally {
      isTauriSpy.mockRestore();
    }
  });
});
