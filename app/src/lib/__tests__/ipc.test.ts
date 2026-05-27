import { describe, it, expect, vi, beforeEach, type Mock } from "vitest";

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(),
}));

vi.mock("@sentry/react", () => ({
  captureException: vi.fn(),
  addBreadcrumb: vi.fn(),
}));

import { invoke } from "@tauri-apps/api/core";
import * as SentryMock from "@sentry/react";
import * as ipc from "../ipc";

beforeEach(() => {
  (invoke as Mock).mockReset();
  (SentryMock.addBreadcrumb as Mock).mockReset();
});

// ── Chat ──

describe("Chat commands", () => {
  it("sendMessage calls invoke with correct args", async () => {
    (invoke as Mock).mockResolvedValue("session-123");
    const result = await ipc.sendMessage("hello", "claude-opus-4.1", "sess-1");
    expect(invoke).toHaveBeenCalledWith("send_message", {
      message: "hello",
      model: "claude-opus-4.1",
      sessionId: "sess-1",
    });
    expect(result).toBe("session-123");
  });

  it("sendMessage with optional params omitted", async () => {
    (invoke as Mock).mockResolvedValue("session-456");
    await ipc.sendMessage("hi");
    expect(invoke).toHaveBeenCalledWith("send_message", {
      message: "hi",
      model: undefined,
      sessionId: undefined,
    });
  });

  it("stopChat calls invoke correctly", async () => {
    (invoke as Mock).mockResolvedValue(undefined);
    await ipc.stopChat("sess-1");
    expect(invoke).toHaveBeenCalledWith("stop_chat", { sessionId: "sess-1" });
  });

  it("getChatHistory calls invoke correctly", async () => {
    const mockHistory = [{ role: "user", content: "hi", timestamp: "2024-01-01" }];
    (invoke as Mock).mockResolvedValue(mockHistory);
    const result = await ipc.getChatHistory("sess-1");
    expect(invoke).toHaveBeenCalledWith("get_chat_history", { sessionId: "sess-1" });
    expect(result).toEqual(mockHistory);
  });

  it("sendMessage propagates errors", async () => {
    (invoke as Mock).mockRejectedValue(new Error("connection failed"));
    await expect(ipc.sendMessage("hi")).rejects.toThrow("connection failed");
  });

  it("stopChat propagates errors", async () => {
    (invoke as Mock).mockRejectedValue(new Error("not found"));
    await expect(ipc.stopChat("bad-id")).rejects.toThrow("not found");
  });

  it("getChatHistory propagates errors", async () => {
    (invoke as Mock).mockRejectedValue(new Error("no session"));
    await expect(ipc.getChatHistory("bad-id")).rejects.toThrow("no session");
  });
});

// ── Hivemind ──

describe("Hivemind commands", () => {
  it("startReview calls invoke with plan only", async () => {
    (invoke as Mock).mockResolvedValue("job-1");
    const result = await ipc.startReview("review this code");
    expect(invoke).toHaveBeenCalledWith("start_review", { plan: "review this code" });
    expect(result).toBe("job-1");
  });

  it("startReview calls invoke with all options", async () => {
    (invoke as Mock).mockResolvedValue("job-2");
    const result = await ipc.startReview("plan", {
      stance: "critical",
      numRounds: 3,
      timeoutSeconds: 120,
      models: ["gpt-4", "claude-opus-4.1"],
    });
    expect(invoke).toHaveBeenCalledWith("start_review", {
      plan: "plan",
      stance: "critical",
      numRounds: 3,
      timeoutSeconds: 120,
      models: ["gpt-4", "claude-opus-4.1"],
    });
    expect(result).toBe("job-2");
  });

  it("getReviewStatus calls invoke correctly", async () => {
    const mockStatus = {
      job_id: "j1",
      status: "running",
      current_round: 1,
      total_rounds: 3,
      steps: [],
      error: null,
      final_output: null,
      total_cost: 0.05,
    };
    (invoke as Mock).mockResolvedValue(mockStatus);
    const result = await ipc.getReviewStatus("j1");
    expect(invoke).toHaveBeenCalledWith("get_review_status", { jobId: "j1" });
    expect(result).toEqual(mockStatus);
  });

  it("listReviews calls invoke with no params", async () => {
    const mockReviews = [
      { job_id: "j1", status: "completed", created_at: "2024-01-01", stance: "neutral", plan_preview: "test", total_cost: 0.1 },
    ];
    (invoke as Mock).mockResolvedValue(mockReviews);
    const result = await ipc.listReviews();
    expect(invoke).toHaveBeenCalledWith("list_reviews", { limit: undefined, offset: undefined });
    expect(result).toEqual({ reviews: mockReviews, total_runs: mockReviews.length });
  });

  it("listReviews calls invoke with limit and offset", async () => {
    (invoke as Mock).mockResolvedValue([]);
    await ipc.listReviews(10, 20);
    expect(invoke).toHaveBeenCalledWith("list_reviews", { limit: 10, offset: 20 });
  });

  it("startReview propagates errors", async () => {
    (invoke as Mock).mockRejectedValue(new Error("review failed"));
    await expect(ipc.startReview("plan")).rejects.toThrow("review failed");
  });

  it("getReviewStatus propagates errors", async () => {
    (invoke as Mock).mockRejectedValue(new Error("not found"));
    await expect(ipc.getReviewStatus("bad")).rejects.toThrow("not found");
  });

  it("listReviews propagates errors", async () => {
    (invoke as Mock).mockRejectedValue(new Error("db error"));
    await expect(ipc.listReviews()).rejects.toThrow("db error");
  });

  // ── logReviewEvent guard ──

  it("logReviewEvent drops empty string reviewId and does not call invoke", async () => {
    const result = await ipc.logReviewEvent("", "test_event", {});
    expect(result).toBeUndefined();
    expect(invoke).not.toHaveBeenCalled();
    expect(SentryMock.addBreadcrumb).toHaveBeenCalledWith(
      expect.objectContaining({
        category: "hivemind",
        level: "debug",
      }),
    );
  });

  it("logReviewEvent drops whitespace-only reviewId and does not call invoke", async () => {
    const result = await ipc.logReviewEvent("   ", "test_event", {});
    expect(result).toBeUndefined();
    expect(invoke).not.toHaveBeenCalled();
    expect(SentryMock.addBreadcrumb).toHaveBeenCalledWith(
      expect.objectContaining({
        category: "hivemind",
      }),
    );
  });

  it("logReviewEvent drops null reviewId and does not call invoke", async () => {
    const result = await ipc.logReviewEvent(null, "test_event", {});
    expect(result).toBeUndefined();
    expect(invoke).not.toHaveBeenCalled();
    expect(SentryMock.addBreadcrumb).toHaveBeenCalled();
  });

  it("logReviewEvent drops undefined reviewId and does not call invoke", async () => {
    const result = await ipc.logReviewEvent(undefined, "test_event", {});
    expect(result).toBeUndefined();
    expect(invoke).not.toHaveBeenCalled();
    expect(SentryMock.addBreadcrumb).toHaveBeenCalled();
  });

  it("logReviewEvent drops non-string reviewId (number) and does not call invoke", async () => {
    const result = await ipc.logReviewEvent(123 as any, "test_event", {});
    expect(result).toBeUndefined();
    expect(invoke).not.toHaveBeenCalled();
    expect(SentryMock.addBreadcrumb).toHaveBeenCalled();
  });

  it("logReviewEvent calls invoke for valid reviewId", async () => {
    (invoke as Mock).mockResolvedValue(undefined);
    const result = await ipc.logReviewEvent("valid-id", "test_event", { key: "val" });
    expect(result).toBeUndefined();
    expect(invoke).toHaveBeenCalledWith("log_review_event", {
      reviewId: "valid-id",
      eventType: "test_event",
      data: { key: "val" },
    });
    expect(SentryMock.addBreadcrumb).not.toHaveBeenCalled();
  });
});

// ── Swarms ──

describe("Swarm commands", () => {
  const mockModelSettings = {
    primary_model: "claude-opus-4.1",
    scout_model: "claude-sonnet-4",
    use_hivemind_on_scout: false,
    use_hivemind_on_queen: true,
    hivemind_id: null,
  };

  const mockFeature = {
    id: "f1",
    name: "auth",
    description: "Add authentication",
    status: "pending" as const,
    dependencies: [],
    milestone: null,
    fix_attempt_count: 0,
    max_fix_attempts: 3,
  };

  const mockSwarmState = {
    id: "sw-1",
    name: "Test Swarm",
    status: "planning" as const,
    working_directory: "/tmp/project",
    model_settings: mockModelSettings,
    current_phase: "scouting",
    current_feature_index: 0,
    created_at: "2024-01-01",
    updated_at: "2024-01-01",
    error: null,
  };

  it("createSwarm calls invoke with correct args", async () => {
    (invoke as Mock).mockResolvedValue(mockSwarmState);
    const result = await ipc.createSwarm("Test Swarm", "A test swarm", "/tmp/project", mockModelSettings);
    expect(invoke).toHaveBeenCalledWith("create_swarm", {
      name: "Test Swarm",
      description: "A test swarm",
      workingDirectory: "/tmp/project",
      modelSettings: mockModelSettings,
    });
    expect(result).toEqual(mockSwarmState);
  });

  it("startSwarm calls invoke with correct args", async () => {
    (invoke as Mock).mockResolvedValue(undefined);
    await ipc.startSwarm("sw-1", [mockFeature]);
    expect(invoke).toHaveBeenCalledWith("start_swarm", {
      swarmId: "sw-1",
      features: [mockFeature],
    });
  });

  it("pauseSwarm calls invoke correctly", async () => {
    (invoke as Mock).mockResolvedValue(undefined);
    await ipc.pauseSwarm("sw-1");
    expect(invoke).toHaveBeenCalledWith("pause_swarm", { swarmId: "sw-1" });
  });

  it("resumeSwarm calls invoke correctly", async () => {
    (invoke as Mock).mockResolvedValue(undefined);
    await ipc.resumeSwarm("sw-1");
    expect(invoke).toHaveBeenCalledWith("resume_swarm", { swarmId: "sw-1" });
  });

  it("stopSwarm calls invoke correctly", async () => {
    (invoke as Mock).mockResolvedValue(undefined);
    await ipc.stopSwarm("sw-1");
    expect(invoke).toHaveBeenCalledWith("stop_swarm", { swarmId: "sw-1" });
  });

  it("getSwarm calls invoke correctly", async () => {
    (invoke as Mock).mockResolvedValue(mockSwarmState);
    const result = await ipc.getSwarm("sw-1");
    expect(invoke).toHaveBeenCalledWith("get_swarm", { swarmId: "sw-1" });
    expect(result).toEqual(mockSwarmState);
  });

  it("listSwarms calls invoke with no args", async () => {
    (invoke as Mock).mockResolvedValue([mockSwarmState]);
    const result = await ipc.listSwarms();
    expect(invoke).toHaveBeenCalledWith("list_swarms");
    expect(result).toEqual([mockSwarmState]);
  });

  it("createSwarm propagates errors", async () => {
    (invoke as Mock).mockRejectedValue(new Error("create failed"));
    await expect(ipc.createSwarm("x", "y", "/z", mockModelSettings)).rejects.toThrow("create failed");
  });

  it("startSwarm propagates errors", async () => {
    (invoke as Mock).mockRejectedValue(new Error("start failed"));
    await expect(ipc.startSwarm("sw-1", [])).rejects.toThrow("start failed");
  });

  it("pauseSwarm propagates errors", async () => {
    (invoke as Mock).mockRejectedValue(new Error("pause failed"));
    await expect(ipc.pauseSwarm("sw-1")).rejects.toThrow("pause failed");
  });

  it("resumeSwarm propagates errors", async () => {
    (invoke as Mock).mockRejectedValue(new Error("resume failed"));
    await expect(ipc.resumeSwarm("sw-1")).rejects.toThrow("resume failed");
  });

  it("stopSwarm propagates errors", async () => {
    (invoke as Mock).mockRejectedValue(new Error("stop failed"));
    await expect(ipc.stopSwarm("sw-1")).rejects.toThrow("stop failed");
  });

  it("getSwarm propagates errors", async () => {
    (invoke as Mock).mockRejectedValue(new Error("get failed"));
    await expect(ipc.getSwarm("sw-1")).rejects.toThrow("get failed");
  });

  it("listSwarms propagates errors", async () => {
    (invoke as Mock).mockRejectedValue(new Error("list failed"));
    await expect(ipc.listSwarms()).rejects.toThrow("list failed");
  });
});

// ── Settings ──

describe("Settings commands", () => {
  it("getSettings calls invoke with no args", async () => {
    const mockSettings = {
      configured_providers: ["anthropic"],
      default_model: "claude-opus-4.1",
      default_hivemind: null,
      concurrency_cap: 8,
      max_pi_processes: 30,
      data_dir: "~/.hyvemind",
    };
    (invoke as Mock).mockResolvedValue(mockSettings);
    const result = await ipc.getSettings();
    expect(invoke).toHaveBeenCalledWith("get_settings");
    expect(result).toEqual(mockSettings);
  });

  it("setRuntimeSettings calls invoke correctly", async () => {
    const mockSettings = {
      configured_providers: ["anthropic"],
      default_model: "claude-opus-4.1",
      default_hivemind: null,
      default_project_path: null,
      concurrency_cap: 12,
      max_pi_processes: 4,
      data_dir: "~/.hyvemind",
      source_dir: "/tmp/project",
      stable_mode: false,
      debug_mode: false,
      auto_commit_tasks: false,
    };
    (invoke as Mock).mockResolvedValue(mockSettings);
    const result = await ipc.setRuntimeSettings(12, 4);
    expect(invoke).toHaveBeenCalledWith("set_runtime_settings", {
      concurrencyCap: 12,
      maxPiProcesses: 4,
    });
    expect(result).toEqual(mockSettings);
  });

  it("saveApiKey calls invoke correctly", async () => {
    (invoke as Mock).mockResolvedValue(undefined);
    await ipc.saveApiKey("anthropic", "sk-test-123");
    expect(invoke).toHaveBeenCalledWith("save_api_key", {
      provider: "anthropic",
      apiKey: "sk-test-123",
    });
  });

  it("deleteApiKey calls invoke correctly", async () => {
    (invoke as Mock).mockResolvedValue(undefined);
    await ipc.deleteApiKey("openai");
    expect(invoke).toHaveBeenCalledWith("delete_api_key", { provider: "openai" });
  });

  it("getProviders calls invoke with no args", async () => {
    const mockProviders = [
      { name: "anthropic", configured: true, model_count: 5, health: true },
    ];
    (invoke as Mock).mockResolvedValue(mockProviders);
    const result = await ipc.getProviders();
    expect(invoke).toHaveBeenCalledWith("get_providers");
    expect(result).toEqual(mockProviders);
  });

  it("refreshModels calls invoke with no provider", async () => {
    const mockModels = [
      { provider: "anthropic", model_id: "claude-opus-4.1", context_window: 200000, cost_per_1m_input: 15, cost_per_1m_output: 75 },
    ];
    (invoke as Mock).mockResolvedValue(mockModels);
    const result = await ipc.refreshModels();
    expect(invoke).toHaveBeenCalledWith("refresh_models", { provider: undefined });
    expect(result).toEqual(mockModels);
  });

  it("refreshModels calls invoke with specific provider", async () => {
    (invoke as Mock).mockResolvedValue([]);
    await ipc.refreshModels("openai");
    expect(invoke).toHaveBeenCalledWith("refresh_models", { provider: "openai" });
  });

  it("getSettings propagates errors", async () => {
    (invoke as Mock).mockRejectedValue(new Error("settings error"));
    await expect(ipc.getSettings()).rejects.toThrow("settings error");
  });

  it("saveApiKey propagates errors", async () => {
    (invoke as Mock).mockRejectedValue(new Error("save failed"));
    await expect(ipc.saveApiKey("x", "y")).rejects.toThrow("save failed");
  });

  it("deleteApiKey propagates errors", async () => {
    (invoke as Mock).mockRejectedValue(new Error("delete failed"));
    await expect(ipc.deleteApiKey("x")).rejects.toThrow("delete failed");
  });

  it("getProviders propagates errors", async () => {
    (invoke as Mock).mockRejectedValue(new Error("providers error"));
    await expect(ipc.getProviders()).rejects.toThrow("providers error");
  });

  it("refreshModels propagates errors", async () => {
    (invoke as Mock).mockRejectedValue(new Error("refresh failed"));
    await expect(ipc.refreshModels()).rejects.toThrow("refresh failed");
  });
});
