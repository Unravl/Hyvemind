import { describe, it, expect, vi, beforeEach } from "vitest";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("../ipc", () => ({
  sendMessage: vi.fn(),
  logReviewEvent: vi.fn().mockResolvedValue(undefined),
}));

import { detectContextSteer, resolveOrchestratorModel } from "../taskRuntime";

describe("submitMessage context-steer routing", () => {
  beforeEach(() => {
    vi.clearAllMocks();
  });

  it("detectContextSteer returns contextSid when flow is alive", () => {
    const flow = { phase: "context" as const, contextSid: "sid-ctx" };
    const result = detectContextSteer(flow as any);
    expect(result.isContextSteer).toBe(true);
    expect(result.contextSid).toBe("sid-ctx");
  });

  it("stale-flow guard fires when contextSid is null despite context phase", () => {
    const flow = { phase: "context" as const, contextSid: null };
    const result = detectContextSteer(flow as any);
    expect(result.isContextSteer).toBe(false);
    expect(result.contextSid).toBeNull();
  });
});

describe("resolveOrchestratorModel", () => {
  it("prefixes the stored provider when the model id contains an upstream slash", () => {
    expect(
      resolveOrchestratorModel(
        {
          inherit_orchestrator: false,
          orchestrator_provider: "neuralwatt",
          orchestrator_model: "moonshotai/Kimi-K2.6",
        },
        "anthropic/claude-sonnet-4",
      ),
    ).toBe("neuralwatt/moonshotai/Kimi-K2.6");
  });

  it("does not double-prefix an already provider-qualified orchestrator model", () => {
    expect(
      resolveOrchestratorModel(
        {
          inherit_orchestrator: false,
          orchestrator_provider: "neuralwatt",
          orchestrator_model: "neuralwatt/moonshotai/Kimi-K2.6",
        },
        "anthropic/claude-sonnet-4",
      ),
    ).toBe("neuralwatt/moonshotai/Kimi-K2.6");
  });

  it("uses the inherited task model when the hivemind inherits orchestrator settings", () => {
    expect(
      resolveOrchestratorModel(
        {
          inherit_orchestrator: true,
          orchestrator_provider: "neuralwatt",
          orchestrator_model: "moonshotai/Kimi-K2.6",
        },
        "crof/mimo-v2.5-pro",
      ),
    ).toBe("crof/mimo-v2.5-pro");
  });
});
