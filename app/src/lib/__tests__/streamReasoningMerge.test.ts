import { describe, it, expect } from "vitest";
import type {
  ChatBubbleEntry,
  ErrorEntry,
  SessionMarkerEntry,
  StreamAgent,
  StreamEntry,
} from "../streamEntry";
import {
  computeReasoningRenderPlan,
  hasBreakerAfter,
} from "../streamReasoningMerge";

let seq = 0;

function bubble(over: Partial<ChatBubbleEntry> = {}): ChatBubbleEntry {
  seq += 1;
  return {
    kind: "chat_bubble",
    id: `b-${seq}`,
    surface: "swarm",
    who: "asst",
    agent: "worker",
    featureId: "feat-1",
    sessionId: "sess-1",
    text: "",
    tools: [],
    t: "00:00:00",
    ...over,
  };
}

function divider(over: Partial<SessionMarkerEntry> = {}): SessionMarkerEntry {
  seq += 1;
  return {
    kind: "session_marker",
    id: `d-${seq}`,
    surface: "swarm",
    phase: "start",
    label: "",
    agent: "worker",
    featureId: "feat-1",
    sessionId: "sess-1",
    ...over,
  };
}

function errorRow(over: Partial<ErrorEntry> = {}): ErrorEntry {
  seq += 1;
  return {
    kind: "error",
    id: `e-${seq}`,
    surface: "swarm",
    agent: "worker" as StreamAgent,
    featureId: "feat-1",
    sessionId: "sess-1",
    message: "boom",
    t: "00:00:00",
    ...over,
  };
}

describe("computeReasoningRenderPlan", () => {
  it("lastReasoningIdx points to the latest bubble that carries reasoning", () => {
    const items: StreamEntry[] = [
      bubble({ reasoning: "thoughts A" }),
      bubble({ text: "visible" }),
      bubble({ reasoning: "thoughts B" }),
      bubble({ text: "more visible" }),
    ];
    const plan = computeReasoningRenderPlan(items, true);
    expect(plan.lastReasoningIdx).toBe(2);
  });

  it("returns -1 when no bubble has reasoning", () => {
    const items: StreamEntry[] = [bubble({ text: "hi" })];
    const plan = computeReasoningRenderPlan(items, true);
    expect(plan.lastReasoningIdx).toBe(-1);
  });

  it("with showToolCalls=true, never merges (regression guard)", () => {
    const items: StreamEntry[] = [
      bubble({ reasoning: "A" }),
      bubble({ reasoning: "B" }), // tool-only bubble in real life
      bubble({ reasoning: "C" }),
    ];
    const plan = computeReasoningRenderPlan(items, true);
    expect(plan.mergeLeader.size).toBe(0);
    expect(plan.mergeSkip.size).toBe(0);
  });

  it("merges two reasoning bubbles separated by a tool-only bubble when showToolCalls=false", () => {
    const items: StreamEntry[] = [
      bubble({ reasoning: "first thought" }),
      // tool-only bubble (no text, but has tools); reasoning empty
      bubble({
        tools: [
          { tool_call_id: "tc1", name: "shell", output: "", done: true },
        ],
      }),
      bubble({ reasoning: "second thought" }),
    ];
    const plan = computeReasoningRenderPlan(items, false);
    expect(plan.mergeLeader.size).toBe(1);
    const leader = plan.mergeLeader.get(0);
    expect(leader).toBeDefined();
    expect(leader!.reasoning).toBe("first thought\n\nsecond thought");
    expect(leader!.lastIdx).toBe(2);
    expect(plan.mergeSkip.has(2)).toBe(true);
    expect(plan.lastReasoningIdx).toBe(2);
  });

  it("sums durations across merged reasoning slices", () => {
    const items: StreamEntry[] = [
      bubble({ reasoning: "a", reasoningDurationMs: 1200 }),
      bubble({ reasoning: "b", reasoningDurationMs: 800 }),
    ];
    const plan = computeReasoningRenderPlan(items, false);
    const leader = plan.mergeLeader.get(0);
    expect(leader?.durationMs).toBe(2000);
  });

  it("visible text bubble breaks the merge", () => {
    const items: StreamEntry[] = [
      bubble({ reasoning: "A" }),
      bubble({ text: "I have a thought" }),
      bubble({ reasoning: "B" }),
    ];
    const plan = computeReasoningRenderPlan(items, false);
    // No merging happened: A is alone, B is alone.
    expect(plan.mergeLeader.size).toBe(0);
    expect(plan.mergeSkip.size).toBe(0);
    expect(plan.lastReasoningIdx).toBe(2);
  });

  it("session_marker breaks the merge", () => {
    const items: StreamEntry[] = [
      bubble({ reasoning: "A" }),
      divider(),
      bubble({ reasoning: "B" }),
    ];
    const plan = computeReasoningRenderPlan(items, false);
    expect(plan.mergeLeader.size).toBe(0);
    expect(plan.mergeSkip.size).toBe(0);
  });

  it("error row breaks the merge", () => {
    const items: StreamEntry[] = [
      bubble({ reasoning: "A" }),
      errorRow(),
      bubble({ reasoning: "B" }),
    ];
    const plan = computeReasoningRenderPlan(items, false);
    expect(plan.mergeLeader.size).toBe(0);
    expect(plan.mergeSkip.size).toBe(0);
  });

  it("does NOT merge reasoning across different agents", () => {
    const items: StreamEntry[] = [
      bubble({ agent: "scout", reasoning: "scout thinks" }),
      bubble({ agent: "worker", reasoning: "worker thinks" }),
    ];
    const plan = computeReasoningRenderPlan(items, false);
    expect(plan.mergeLeader.size).toBe(0);
    expect(plan.mergeSkip.size).toBe(0);
  });

  it("merges 3+ consecutive reasoning bubbles into a single leader", () => {
    const items: StreamEntry[] = [
      bubble({ reasoning: "A", reasoningDurationMs: 100 }),
      bubble({ reasoning: "B", reasoningDurationMs: 200 }),
      bubble({ reasoning: "C", reasoningDurationMs: 300 }),
    ];
    const plan = computeReasoningRenderPlan(items, false);
    expect(plan.mergeLeader.size).toBe(1);
    const leader = plan.mergeLeader.get(0)!;
    expect(leader.reasoning).toBe("A\n\nB\n\nC");
    expect(leader.durationMs).toBe(600);
    expect(leader.lastIdx).toBe(2);
    expect(plan.mergeSkip.has(1)).toBe(true);
    expect(plan.mergeSkip.has(2)).toBe(true);
  });

  it("a text-bubble visibility threshold of >1 char is required to break", () => {
    // single-char text like an artifact of a partial token shouldn't break.
    const items: StreamEntry[] = [
      bubble({ reasoning: "A" }),
      bubble({ text: "x" }),
      bubble({ reasoning: "B" }),
    ];
    const plan = computeReasoningRenderPlan(items, false);
    // 1-char text is NOT a breaker per the contract.
    expect(plan.mergeLeader.size).toBe(1);
    expect(plan.mergeSkip.has(2)).toBe(true);
  });

  it("does not consult merge maps when showToolCalls=true", () => {
    const items: StreamEntry[] = [
      bubble({ reasoning: "A" }),
      bubble({ reasoning: "B" }),
    ];
    const plan = computeReasoningRenderPlan(items, true);
    expect(plan.mergeLeader.size).toBe(0);
    expect(plan.mergeSkip.size).toBe(0);
    // lastReasoningIdx is still computed.
    expect(plan.lastReasoningIdx).toBe(1);
  });
});

describe("hasBreakerAfter", () => {
  it("returns false when no items follow", () => {
    const items: StreamEntry[] = [bubble({ reasoning: "A" })];
    expect(hasBreakerAfter(items, 0)).toBe(false);
  });

  it("returns true when a session_marker follows", () => {
    const items: StreamEntry[] = [bubble({ reasoning: "A" }), divider()];
    expect(hasBreakerAfter(items, 0)).toBe(true);
  });

  it("returns true when an error follows", () => {
    const items: StreamEntry[] = [bubble({ reasoning: "A" }), errorRow()];
    expect(hasBreakerAfter(items, 0)).toBe(true);
  });

  it("returns true when a visible-text bubble follows", () => {
    const items: StreamEntry[] = [
      bubble({ reasoning: "A" }),
      bubble({ text: "next turn" }),
    ];
    expect(hasBreakerAfter(items, 0)).toBe(true);
  });

  it("returns false when only reasoning-only bubbles follow", () => {
    const items: StreamEntry[] = [
      bubble({ reasoning: "A" }),
      bubble({ reasoning: "B" }),
    ];
    expect(hasBreakerAfter(items, 0)).toBe(false);
  });
});
