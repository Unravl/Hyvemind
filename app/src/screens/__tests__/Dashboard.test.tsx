import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen } from "@testing-library/react";

vi.mock("../../lib/tauri", () => ({ isTauri: () => false }));

vi.mock("../../lib/ipc", () => ({
  getDashboardStats: vi.fn().mockRejectedValue(new Error("not available")),
  getModelUsage: vi.fn().mockRejectedValue(new Error("not available")),
  getProviderUsage: vi.fn().mockRejectedValue(new Error("not available")),
  getCostSummary: vi.fn().mockRejectedValue(new Error("not available")),
  getRecentActivity: vi.fn().mockRejectedValue(new Error("not available")),
}));

vi.mock("../../lib/events", () => ({
  onSwarmEvent: vi.fn().mockResolvedValue(vi.fn()),
}));

vi.mock("../../components/ProjectPicker", () => ({
  ProjectPicker: () => null,
  useProject: () => ({
    project: null,
    setProject: vi.fn(),
    projects: [],
  }),
  LANG_DOT: {},
}));

import { DashboardScreen } from "../Dashboard";

describe("DashboardScreen", () => {
  const go = vi.fn();

  beforeEach(() => {
    vi.clearAllMocks();
  });

  it("renders without crashing", () => {
    render(<DashboardScreen go={go} />);
    expect(screen.getByText("Dashboard")).toBeInTheDocument();
  });

  it("shows all section headings", () => {
    render(<DashboardScreen go={go} />);
    expect(screen.getByText("Model Usage")).toBeInTheDocument();
    expect(screen.getByText("Token Usage by Model")).toBeInTheDocument();
    expect(screen.getByText("Cost Summary")).toBeInTheDocument();
    expect(screen.getByText("Recent Activity")).toBeInTheDocument();
  });

  it("shows stat labels", async () => {
    render(<DashboardScreen go={go} />);
    expect(await screen.findByText("Active Tasks")).toBeInTheDocument();
    expect(screen.getByText("Running Swarms")).toBeInTheDocument();
    expect(screen.getByText("Total Reviews")).toBeInTheDocument();
    expect(screen.getByText("Cost Today")).toBeInTheDocument();
  });

  it("default time range is All Time", () => {
    render(<DashboardScreen go={go} />);
    const select = screen.getByDisplayValue("All Time");
    expect(select).toBeInTheDocument();
  });
});
