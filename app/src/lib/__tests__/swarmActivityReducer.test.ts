import { describe, it, expect } from "vitest";
import {
  applyActivityEvent,
  initialActivityState,
  toStreamEntries,
  type AgentBubbleItem,
  type AgentDividerItem,
} from "../swarmActivityReducer";
import type { SwarmActivityEvent, SwarmActivityKind } from "../events";

function ev(
  kind: SwarmActivityKind,
  overrides: Partial<SwarmActivityEvent> = {},
): SwarmActivityEvent {
  return {
    swarm_id: "sw1",
    feature_id: "feat-001",
    agent: "scout",
    session_id: "scout-feat-001",
    timestamp: "2026-05-14T12:00:00.000Z",
    kind,
    ...overrides,
  };
}

describe("swarmActivityReducer", () => {
  it("agent_start pushes a divider tagged with model", () => {
    const s = applyActivityEvent(
      initialActivityState,
      ev("agent_start", { model: "opus-4.7" }),
    );
    expect(s.items).toHaveLength(1);
    const d = s.items[0] as AgentDividerItem;
    expect(d.kind).toBe("agent_divider");
    expect(d.model).toBe("opus-4.7");
    expect(d.endedAt).toBeUndefined();
  });

  it("text deltas append to a fresh bubble for the session", () => {
    let s = applyActivityEvent(initialActivityState, ev("agent_start"));
    s = applyActivityEvent(s, ev("text", { text: "Hello " }));
    s = applyActivityEvent(s, ev("text", { text: "world." }));
    expect(s.items).toHaveLength(2);
    const b = s.items[1] as AgentBubbleItem;
    expect(b.kind).toBe("agent_bubble");
    expect(b.text).toBe("Hello world.");
  });

  it("thinking deltas accumulate reasoning and track duration", () => {
    let s = applyActivityEvent(initialActivityState, ev("agent_start"));
    s = applyActivityEvent(s, ev("thinking", { text: "step 1 " }));
    s = applyActivityEvent(s, ev("thinking", { text: "step 2" }));
    const b = s.items[1] as AgentBubbleItem;
    expect(b.reasoning).toBe("step 1 step 2");
    expect(b.reasoningStartedAt).toBeTypeOf("number");
    expect(b.reasoningDurationMs).toBeGreaterThanOrEqual(0);
  });

  it("tool lifecycle: start → update → end", () => {
    let s = applyActivityEvent(initialActivityState, ev("agent_start"));
    s = applyActivityEvent(
      s,
      ev("tool_start", { tool_call_id: "t1", tool_name: "Read" }),
    );
    s = applyActivityEvent(
      s,
      ev("tool_update", { tool_call_id: "t1", tool_output: "line A\n" }),
    );
    s = applyActivityEvent(
      s,
      ev("tool_update", { tool_call_id: "t1", tool_output: "line B\n" }),
    );
    s = applyActivityEvent(s, ev("tool_end", { tool_call_id: "t1" }));
    const b = s.items[1] as AgentBubbleItem;
    expect(b.tools).toHaveLength(1);
    expect(b.tools[0]).toMatchObject({
      tool_call_id: "t1",
      name: "Read",
      output: "line A\nline B\n",
      done: true,
    });
  });

  it("agent_end patches the matching divider with endedAt + success AND appends an end marker", () => {
    let s = applyActivityEvent(
      initialActivityState,
      ev("agent_start", { model: "opus-4.7" }),
    );
    s = applyActivityEvent(s, ev("text", { text: "done" }));
    s = applyActivityEvent(
      s,
      ev("agent_end", {
        success: true,
        timestamp: "2026-05-14T12:01:00.000Z",
      }),
    );
    // Divider patch (for invariants/testing).
    const d = s.items[0] as AgentDividerItem;
    expect(d.endedAt).toBe("2026-05-14T12:01:00.000Z");
    expect(d.success).toBe(true);
    expect(s.bubbleBySession["scout-feat-001"]).toBeUndefined();
    expect(s.dividerBySession["scout-feat-001"]).toBeUndefined();

    // New canonical render-time end marker should be appended.
    // Items: [divider, bubble, end_marker]
    expect(s.items).toHaveLength(3);
    const end = s.items[2] as {
      kind: string;
      sessionId: string;
      success: boolean;
      endedAt: string;
      model?: string;
    };
    expect(end.kind).toBe("agent_end_marker");
    expect(end.sessionId).toBe("scout-feat-001");
    expect(end.success).toBe(true);
    expect(end.endedAt).toBe("2026-05-14T12:01:00.000Z");
    expect(end.model).toBe("opus-4.7");
  });

  it("inline_end_marker_appears_before_later_worker_activity (regression)", () => {
    // Reproduces the original bug: scout ends, then worker starts + emits text.
    // The scout end marker must appear at index 1 (right after scout start),
    // BEFORE the worker start + bubble — not at the tail.
    let s = applyActivityEvent(
      initialActivityState,
      ev("agent_start", {
        agent: "scout",
        session_id: "scout-S",
        feature_id: "feat-a",
        model: "m",
        timestamp: "2026-05-14T12:00:00.000Z",
      }),
    );
    s = applyActivityEvent(
      s,
      ev("agent_end", {
        agent: "scout",
        session_id: "scout-S",
        feature_id: "feat-a",
        success: true,
        timestamp: "2026-05-14T12:01:00.000Z",
      }),
    );
    s = applyActivityEvent(
      s,
      ev("agent_start", {
        agent: "worker",
        session_id: "worker-W",
        feature_id: "feat-a",
        model: "m",
        timestamp: "2026-05-14T12:02:00.000Z",
      }),
    );
    s = applyActivityEvent(
      s,
      ev("text", {
        agent: "worker",
        session_id: "worker-W",
        feature_id: "feat-a",
        text: "worker output",
        timestamp: "2026-05-14T12:03:00.000Z",
      }),
    );

    const entries = toStreamEntries(s);
    expect(entries).toHaveLength(4);
    // [0] scout start marker
    expect(entries[0].kind).toBe("session_marker");
    expect((entries[0] as { phase: string }).phase).toBe("start");
    expect((entries[0] as { sessionId: string }).sessionId).toBe("scout-S");
    // [1] scout end marker — chronologically between scout start and worker start.
    expect(entries[1].kind).toBe("session_marker");
    expect((entries[1] as { phase: string }).phase).toBe("end");
    expect((entries[1] as { sessionId: string }).sessionId).toBe("scout-S");
    // [2] worker start marker
    expect(entries[2].kind).toBe("session_marker");
    expect((entries[2] as { phase: string }).phase).toBe("start");
    expect((entries[2] as { sessionId: string }).sessionId).toBe("worker-W");
    // [3] worker bubble
    expect(entries[3].kind).toBe("chat_bubble");
    expect((entries[3] as { sessionId: string }).sessionId).toBe("worker-W");
    expect((entries[3] as { text: string }).text).toBe("worker output");
  });

  it("emits both start and end markers with the same sessionId for one session", () => {
    let s = applyActivityEvent(
      initialActivityState,
      ev("agent_start", { session_id: "S", model: "m" }),
    );
    s = applyActivityEvent(
      s,
      ev("agent_end", { session_id: "S", success: true }),
    );
    const entries = toStreamEntries(s);
    const start = entries.find(
      (e) =>
        e.kind === "session_marker" &&
        (e as { phase: string }).phase === "start" &&
        (e as { sessionId: string }).sessionId === "S",
    );
    const end = entries.find(
      (e) =>
        e.kind === "session_marker" &&
        (e as { phase: string }).phase === "end" &&
        (e as { sessionId: string }).sessionId === "S",
    );
    expect(start).toBeTruthy();
    expect(end).toBeTruthy();
  });

  it("interleaves bubbles from concurrent agents by session id", () => {
    let s = applyActivityEvent(
      initialActivityState,
      ev("agent_start", { agent: "scout", session_id: "S" }),
    );
    s = applyActivityEvent(
      s,
      ev("agent_start", {
        agent: "worker",
        session_id: "W",
        feature_id: "feat-002",
      }),
    );
    s = applyActivityEvent(
      s,
      ev("text", { agent: "scout", session_id: "S", text: "s-out" }),
    );
    s = applyActivityEvent(
      s,
      ev("text", {
        agent: "worker",
        session_id: "W",
        feature_id: "feat-002",
        text: "w-out",
      }),
    );
    // 2 dividers + 2 bubbles, ordered by arrival.
    expect(s.items).toHaveLength(4);
    expect(s.items[0].kind).toBe("agent_divider");
    expect(s.items[1].kind).toBe("agent_divider");
    const b1 = s.items[2] as AgentBubbleItem;
    const b2 = s.items[3] as AgentBubbleItem;
    expect(b1.agent).toBe("scout");
    expect(b1.text).toBe("s-out");
    expect(b2.agent).toBe("worker");
    expect(b2.text).toBe("w-out");
  });

  it("interleaved thinking → text → thinking merges into the SAME bubble (extended-thinking turn)", () => {
    // Extended-thinking models (Claude w/ thinking enabled, DeepSeek R1, etc.)
    // emit reasoning and text interleaved inside one assistant turn. Text is
    // NOT a turn boundary — only tool calls and finalized turns split bubbles.
    let s = applyActivityEvent(initialActivityState, ev("agent_start"));
    s = applyActivityEvent(s, ev("thinking", { text: "turn-1 reasoning " }));
    s = applyActivityEvent(s, ev("text", { text: "turn-1 output" }));
    s = applyActivityEvent(s, ev("thinking", { text: "turn-2 reasoning " }));
    s = applyActivityEvent(s, ev("text", { text: " + turn-2 output" }));

    // 1 divider + 1 bubble — the whole turn collapses into one bubble.
    expect(s.items).toHaveLength(2);
    expect(s.items[0].kind).toBe("agent_divider");
    const b = s.items[1] as AgentBubbleItem;
    expect(b.kind).toBe("agent_bubble");

    // Both reasoning bursts concatenate; both text deltas concatenate.
    expect(b.reasoning).toBe("turn-1 reasoning turn-2 reasoning ");
    expect(b.text).toBe("turn-1 output + turn-2 output");

    // bubbleBySession still points to the single bubble.
    expect(s.bubbleBySession["scout-feat-001"]).toBe(b.id);
  });

  it("thinking after a tool call splits into a new bubble (tools mark a new turn)", () => {
    // Tools ARE a turn boundary, mirroring the rule the text case applies
    // to text-after-tool. A `thinking` chunk after a tool call belongs to a
    // fresh bubble.
    let s = applyActivityEvent(initialActivityState, ev("agent_start"));
    s = applyActivityEvent(s, ev("thinking", { text: "thinking A " }));
    s = applyActivityEvent(s, ev("thinking", { text: "thinking B " }));
    s = applyActivityEvent(
      s,
      ev("tool_start", { tool_call_id: "t1", tool_name: "Read" }),
    );
    s = applyActivityEvent(s, ev("tool_end", { tool_call_id: "t1" }));
    s = applyActivityEvent(s, ev("thinking", { text: "thinking C" }));

    // 1 divider + 2 bubbles — tool boundary forced a fresh bubble for the
    // post-tool thinking burst.
    expect(s.items).toHaveLength(3);
    const b1 = s.items[1] as AgentBubbleItem;
    const b2 = s.items[2] as AgentBubbleItem;
    expect(b1.reasoning).toBe("thinking A thinking B ");
    expect(b1.tools).toHaveLength(1);
    expect(b1.text).toBe("");
    expect(b2.reasoning).toBe("thinking C");
    expect(b2.tools).toHaveLength(0);
    expect(b2.text).toBe("");
    expect(s.bubbleBySession["scout-feat-001"]).toBe(b2.id);
  });

  it("after a tool-driven thinking-split, follow-up text routes to the NEW bubble", () => {
    // Repeats the original intent with a tool boundary as the actual split
    // trigger (text alone no longer splits). The follow-up text must attach
    // to the post-tool bubble, not the pre-tool one.
    let s = applyActivityEvent(initialActivityState, ev("agent_start"));
    s = applyActivityEvent(s, ev("thinking", { text: "r1" }));
    s = applyActivityEvent(s, ev("text", { text: "t1" }));
    s = applyActivityEvent(
      s,
      ev("tool_start", { tool_call_id: "t-split", tool_name: "Read" }),
    );
    s = applyActivityEvent(s, ev("tool_end", { tool_call_id: "t-split" }));
    s = applyActivityEvent(s, ev("thinking", { text: "r2" }));

    // Snapshot the new bubble id and confirm session points to it.
    const newBubbleId = s.bubbleBySession["scout-feat-001"];
    expect(newBubbleId).toBeTruthy();
    const b2Before = s.items.find(
      (i) => i.kind === "agent_bubble" && i.id === newBubbleId,
    ) as AgentBubbleItem;
    expect(b2Before.text).toBe("");
    expect(b2Before.reasoning).toBe("r2");

    // Follow-up text should accumulate on the NEW bubble, not the first.
    s = applyActivityEvent(s, ev("text", { text: "hello " }));
    s = applyActivityEvent(s, ev("text", { text: "world" }));

    const first = s.items[1] as AgentBubbleItem;
    const second = s.items[2] as AgentBubbleItem;
    expect(first.text).toBe("t1");
    expect(first.reasoning).toBe("r1");
    expect(first.tools).toHaveLength(1);
    expect(second.id).toBe(newBubbleId);
    expect(second.text).toBe("hello world");
    expect(second.reasoning).toBe("r2");
    // And a follow-up tool also routes to the new bubble.
    s = applyActivityEvent(
      s,
      ev("tool_start", { tool_call_id: "tx", tool_name: "Shell" }),
    );
    const secondAfterTool = s.items[2] as AgentBubbleItem;
    expect(secondAfterTool.tools).toHaveLength(1);
    expect(secondAfterTool.tools[0].name).toBe("Shell");
  });

  it("text after tool calls splits into a new bubble (per-turn text)", () => {
    let s = applyActivityEvent(initialActivityState, ev("agent_start"));
    s = applyActivityEvent(s, ev("text", { text: "calling a tool… " }));
    s = applyActivityEvent(
      s,
      ev("tool_start", { tool_call_id: "t1", tool_name: "Read" }),
    );
    s = applyActivityEvent(s, ev("tool_end", { tool_call_id: "t1" }));
    // Post-tool text — should land on a NEW bubble.
    s = applyActivityEvent(s, ev("text", { text: "here is the result" }));

    // 1 divider + 2 bubbles.
    expect(s.items).toHaveLength(3);
    const b1 = s.items[1] as AgentBubbleItem;
    const b2 = s.items[2] as AgentBubbleItem;
    expect(b1.text).toBe("calling a tool… ");
    expect(b1.tools).toHaveLength(1);
    expect(b2.text).toBe("here is the result");
    expect(b2.tools).toHaveLength(0);
    expect(s.bubbleBySession["scout-feat-001"]).toBe(b2.id);
  });

  it("streaming text continues to accumulate on the same bubble when no tools yet", () => {
    let s = applyActivityEvent(initialActivityState, ev("agent_start"));
    s = applyActivityEvent(s, ev("text", { text: "Hello " }));
    s = applyActivityEvent(s, ev("text", { text: "world " }));
    s = applyActivityEvent(s, ev("text", { text: "again" }));
    // Still 1 divider + 1 bubble — multi-chunk text does not split.
    expect(s.items).toHaveLength(2);
    const b = s.items[1] as AgentBubbleItem;
    expect(b.text).toBe("Hello world again");
  });

  it("after a text-split, a follow-up tool attaches to the NEW bubble", () => {
    let s = applyActivityEvent(initialActivityState, ev("agent_start"));
    s = applyActivityEvent(s, ev("text", { text: "first turn text" }));
    s = applyActivityEvent(
      s,
      ev("tool_start", { tool_call_id: "t1", tool_name: "Read" }),
    );
    s = applyActivityEvent(s, ev("tool_end", { tool_call_id: "t1" }));
    s = applyActivityEvent(s, ev("text", { text: "second turn text" }));
    s = applyActivityEvent(
      s,
      ev("tool_start", { tool_call_id: "t2", tool_name: "Shell" }),
    );

    expect(s.items).toHaveLength(3);
    const b1 = s.items[1] as AgentBubbleItem;
    const b2 = s.items[2] as AgentBubbleItem;
    expect(b1.tools.map((t) => t.tool_call_id)).toEqual(["t1"]);
    expect(b2.tools.map((t) => t.tool_call_id)).toEqual(["t2"]);
    expect(b2.text).toBe("second turn text");
  });

  it("error event appends an error row", () => {
    let s = applyActivityEvent(initialActivityState, ev("agent_start"));
    s = applyActivityEvent(s, ev("error", { error: "boom" }));
    expect(s.items[1].kind).toBe("error");
    expect((s.items[1] as { message: string }).message).toBe("boom");
  });

  // The Hivemind merge phase is fanned into the SwarmControl per-feature
  // activity stream via `spawn_agent_forwarder_public` with the
  // `"hivemind-merge"` agent label. The reducer must produce a divider +
  // bubble structurally identical to the existing `hivemind-context`
  // path so the ActivityStream component renders it with the
  // `AGENT_LABEL["hivemind-merge"]` ("hivemind merge") pill.
  it("agent: 'hivemind-merge' produces an agent_divider + agent_bubble identical to hivemind-context", () => {
    let s = applyActivityEvent(
      initialActivityState,
      ev("agent_start", {
        agent: "hivemind-merge",
        session_id: "hivemind-merge-feat-001",
        model: "claude-sonnet-4",
      }),
    );
    s = applyActivityEvent(
      s,
      ev("text", {
        agent: "hivemind-merge",
        session_id: "hivemind-merge-feat-001",
        text: "merged plan content",
      }),
    );
    s = applyActivityEvent(
      s,
      ev("agent_end", {
        agent: "hivemind-merge",
        session_id: "hivemind-merge-feat-001",
        success: true,
        timestamp: "2026-05-14T12:01:00.000Z",
      }),
    );

    const divider = s.items[0] as AgentDividerItem;
    expect(divider.kind).toBe("agent_divider");
    expect(divider.agent).toBe("hivemind-merge");
    expect(divider.model).toBe("claude-sonnet-4");
    expect(divider.endedAt).toBe("2026-05-14T12:01:00.000Z");

    const bubble = s.items[1] as AgentBubbleItem;
    expect(bubble.kind).toBe("agent_bubble");
    expect(bubble.agent).toBe("hivemind-merge");
    expect(bubble.text).toBe("merged plan content");

    // Final item is the AgentEndItem appended by agent_end.
    expect(s.items[2].kind).toBe("agent_end_marker");

    // toStreamEntries must emit start + bubble + end with the
    // pretty "hivemind merge" label (no fall-through to `hivemind-merge`).
    const stream = toStreamEntries(s);
    expect(stream).toHaveLength(3);
    expect(stream[0].kind).toBe("session_marker");
    expect((stream[0] as { label: string }).label).toContain("hivemind merge");
    expect(stream[1].kind).toBe("chat_bubble");
    expect(stream[2].kind).toBe("session_marker");
  });

  describe("toStreamEntries", () => {
    it("returns all entries when no featureId filter is supplied", () => {
      let s = applyActivityEvent(
        initialActivityState,
        ev("agent_start", { feature_id: "feat-a", session_id: "sa", model: "m" }),
      );
      s = applyActivityEvent(
        s,
        ev("text", { feature_id: "feat-a", session_id: "sa", text: "alpha" }),
      );
      s = applyActivityEvent(
        s,
        ev("agent_start", {
          feature_id: "feat-b",
          session_id: "sb",
          agent: "worker",
          model: "m",
        }),
      );
      s = applyActivityEvent(
        s,
        ev("text", {
          feature_id: "feat-b",
          session_id: "sb",
          agent: "worker",
          text: "beta",
        }),
      );

      const all = toStreamEntries(s);
      expect(
        all.some((e) => e.kind === "chat_bubble" && e.text === "alpha"),
      ).toBe(true);
      expect(
        all.some((e) => e.kind === "chat_bubble" && e.text === "beta"),
      ).toBe(true);
    });

    it("filters dividers, bubbles, and end markers to the requested featureId", () => {
      let s = applyActivityEvent(
        initialActivityState,
        ev("agent_start", { feature_id: "feat-a", session_id: "sa", model: "m" }),
      );
      s = applyActivityEvent(
        s,
        ev("text", { feature_id: "feat-a", session_id: "sa", text: "alpha" }),
      );
      s = applyActivityEvent(
        s,
        ev("agent_end", { feature_id: "feat-a", session_id: "sa", success: true }),
      );
      s = applyActivityEvent(
        s,
        ev("agent_start", {
          feature_id: "feat-b",
          session_id: "sb",
          agent: "worker",
          model: "m",
        }),
      );
      s = applyActivityEvent(
        s,
        ev("text", {
          feature_id: "feat-b",
          session_id: "sb",
          agent: "worker",
          text: "beta",
        }),
      );

      const onlyA = toStreamEntries(s, "feat-a");
      // Every entry should belong to feat-a.
      for (const e of onlyA) {
        if ("featureId" in e && e.featureId !== undefined) {
          expect(e.featureId).toBe("feat-a");
        }
      }
      expect(
        onlyA.some((e) => e.kind === "chat_bubble" && e.text === "alpha"),
      ).toBe(true);
      expect(
        onlyA.some((e) => e.kind === "chat_bubble" && e.text === "beta"),
      ).toBe(false);
      // The end marker for sa should still appear; sb's start should not.
      expect(
        onlyA.some(
          (e) =>
            e.kind === "session_marker" &&
            e.phase === "end" &&
            e.sessionId === "sa",
        ),
      ).toBe(true);
      expect(
        onlyA.some(
          (e) => e.kind === "session_marker" && e.sessionId === "sb",
        ),
      ).toBe(false);
    });

    it("returns an empty array when no items match the featureId", () => {
      let s = applyActivityEvent(
        initialActivityState,
        ev("agent_start", { feature_id: "feat-a", session_id: "sa", model: "m" }),
      );
      s = applyActivityEvent(
        s,
        ev("text", { feature_id: "feat-a", session_id: "sa", text: "alpha" }),
      );
      expect(toStreamEntries(s, "feat-zzz")).toEqual([]);
    });
  });

  describe("seq idempotency", () => {
    it("drops a re-delivered event with the same seq", () => {
      // Reproduces the doubled-marker / doubled-text bug seen in the wild
      // when a transient listener-leak makes the same swarm-activity event
      // reach the reducer twice. Pre-fix: two dividers. Post-fix: one.
      const evt = ev("agent_start", { seq: 1, model: "opus" });
      const s1 = applyActivityEvent(initialActivityState, evt);
      const s2 = applyActivityEvent(s1, evt);
      expect(s2).toBe(s1); // strictly the same reference — no-op
      expect(s1.items).toHaveLength(1);
      expect(s1.lastAppliedSeq).toBe(1);
    });

    it("drops a duplicate text delta — single bubble, single chunk", () => {
      let s = applyActivityEvent(
        initialActivityState,
        ev("agent_start", { seq: 1 }),
      );
      const textEvt = ev("text", { seq: 2, text: "Hello world." });
      s = applyActivityEvent(s, textEvt);
      s = applyActivityEvent(s, textEvt); // doubled delivery
      expect(s.items).toHaveLength(2);
      const bubble = s.items[1] as AgentBubbleItem;
      expect(bubble.text).toBe("Hello world."); // not "Hello world.Hello world."
      expect(s.lastAppliedSeq).toBe(2);
    });

    it("still applies an event with a higher seq after a duplicate", () => {
      const startEvt = ev("agent_start", { seq: 1 });
      const textEvt = ev("text", { seq: 2, text: "first " });
      const moreTextEvt = ev("text", { seq: 3, text: "second" });
      let s = applyActivityEvent(initialActivityState, startEvt);
      s = applyActivityEvent(s, textEvt);
      s = applyActivityEvent(s, textEvt); // dup
      s = applyActivityEvent(s, moreTextEvt); // higher seq — must apply
      const bubble = s.items[1] as AgentBubbleItem;
      expect(bubble.text).toBe("first second");
      expect(s.lastAppliedSeq).toBe(3);
    });

    it("events without seq fall through and apply normally", () => {
      // Synthetic / legacy events without a seq are applied as-is. Two
      // back-to-back text deltas without seq should accumulate.
      let s = applyActivityEvent(
        initialActivityState,
        ev("agent_start", { seq: undefined }),
      );
      s = applyActivityEvent(s, ev("text", { seq: undefined, text: "a" }));
      s = applyActivityEvent(s, ev("text", { seq: undefined, text: "b" }));
      const bubble = s.items[1] as AgentBubbleItem;
      expect(bubble.text).toBe("ab");
      expect(s.lastAppliedSeq).toBe(0);
    });
  });
});
