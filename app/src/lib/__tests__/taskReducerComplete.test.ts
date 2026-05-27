import { describe, it, expect } from "vitest";
import {
  applyTaskEvent,
  makeInitialTaskState,
  mapChatEventToTaskEvent,
  toStreamEntries,
  type TaskMessage,
  type TaskPhase,
} from "../taskReducer";
import type { ChatEvent } from "../types";

const MODEL = "claude-sub/claude-opus-4-7";

function initial() {
  return makeInitialTaskState("task-test", MODEL);
}

/** Force the reducer state into the `implement` phase by injecting a plan
 *  message + a synthetic phase progression. Mirrors the runtime path where
 *  the user clicks "Implement Plan" and the reducer rides through
 *  `plan-ready` → `implement` via `advancePhase`. */
function stateInImplementPhase(): ReturnType<typeof initial> {
  const messages: TaskMessage[] = [
    { who: "user", text: "build it" },
    { who: "plan", planText: "## Plan\n- step 1" },
  ];
  return {
    ...initial(),
    messages,
    phase: "implement" as TaskPhase,
    streaming: true,
  };
}

describe("taskReducer structured_task_complete", () => {
  it("marks the task complete, advances to implement-done, halts streaming", () => {
    const prev = stateInImplementPhase();
    const next = applyTaskEvent(
      prev,
      {
        kind: "structured_task_complete",
        summary: "Shipped the change.",
        successState: "success",
      },
      MODEL,
    );
    const completes = next.messages.filter((m) => m.who === "complete");
    expect(completes).toHaveLength(1);
    expect(completes[0].text).toBe("Shipped the change.");
    expect(completes[0].successState).toBe("success");
    expect(next.phase).toBe("implement-done");
    expect(next.streaming).toBe(false);
    expect(next.queueState).toBeNull();
    expect(next.liveTps).toBeNull();
    expect(next.streamPhase).toBeNull();

    // Stream-entry adapter surfaces the complete entry with the new fields.
    const entries = toStreamEntries(next.messages);
    const completeEntry = entries.find((e) => e.kind === "complete");
    expect(completeEntry).toBeDefined();
    if (completeEntry && completeEntry.kind === "complete") {
      expect(completeEntry.text).toBe("Shipped the change.");
      expect(completeEntry.successState).toBe("success");
    }
  });

  it("an empty payload still marks the task complete", () => {
    const prev = stateInImplementPhase();
    const next = applyTaskEvent(
      prev,
      { kind: "structured_task_complete" },
      MODEL,
    );
    const completes = next.messages.filter((m) => m.who === "complete");
    expect(completes).toHaveLength(1);
    expect(completes[0].text).toBeUndefined();
    expect(completes[0].successState).toBeUndefined();
    expect(next.phase).toBe("implement-done");
    expect(next.streaming).toBe(false);
  });

  it("a done event while in implement phase does NOT auto-mark complete (regression guard)", () => {
    const prev = stateInImplementPhase();
    const next = applyTaskEvent(prev, { kind: "done" }, MODEL);
    const completes = next.messages.filter((m) => m.who === "complete");
    expect(completes).toHaveLength(0);
    // Phase MUST stay at `implement`; the implicit auto-complete used to
    // ratchet it to `implement-done` here. With the heuristic removed the
    // phase only advances when an explicit `who: "complete"` message lands.
    expect(next.phase).toBe("implement");
    expect(next.streaming).toBe(false);
  });

  it("a tool_start between two structured_task_complete events still produces exactly one complete (screenshot regression)", () => {
    const prev = stateInImplementPhase();
    const first = applyTaskEvent(
      prev,
      {
        kind: "structured_task_complete",
        summary: "done",
        successState: "success",
      },
      MODEL,
    );
    // Backend currently emits `tool_start` AFTER `structured_task_complete`
    // for the `submit_task_complete` tool. That push appends an empty
    // `asst` bubble to the tail, displacing the `complete` entry. The
    // dedup guard MUST scan back across that bubble to the user message.
    const afterToolStart = applyTaskEvent(
      first,
      {
        kind: "tool_start",
        data: { tool_call_id: "call-1", name: "submit_task_complete" },
      },
      MODEL,
    );
    const twice = applyTaskEvent(
      afterToolStart,
      {
        kind: "structured_task_complete",
        summary: "done again",
        successState: "success",
      },
      MODEL,
    );
    const completes = twice.messages.filter((m) => m.who === "complete");
    expect(completes).toHaveLength(1);
    expect(completes[0].text).toBe("done");
  });

  it("structured_task_complete after replay/load of a persisted complete entry is a no-op", () => {
    // Mimics `load_task_messages` rehydrating from
    // ~/.hyvemind/task-messages/task-{id}.json: a `complete` already exists
    // and a fresh `asst` bubble was streamed afterwards. A late
    // `structured_task_complete` MUST NOT push a second chip.
    const messages: TaskMessage[] = [
      { who: "user", text: "build it" },
      { who: "plan", planText: "## Plan\n- step 1" },
      { who: "complete", text: "shipped", successState: "success" },
      { who: "asst", text: "trailing chatter" },
    ];
    const prev = {
      ...initial(),
      messages,
      phase: "implement-done" as TaskPhase,
      streaming: false,
    };
    const next = applyTaskEvent(
      prev,
      {
        kind: "structured_task_complete",
        summary: "shipped again",
        successState: "success",
      },
      MODEL,
    );
    const completes = next.messages.filter((m) => m.who === "complete");
    expect(completes).toHaveLength(1);
    expect(completes[0].text).toBe("shipped");
  });

  it("duplicate structured_task_complete events do not push duplicate complete messages", () => {
    const prev = stateInImplementPhase();
    const once = applyTaskEvent(
      prev,
      {
        kind: "structured_task_complete",
        summary: "done",
        successState: "success",
      },
      MODEL,
    );
    const twice = applyTaskEvent(
      once,
      {
        kind: "structured_task_complete",
        summary: "done again",
        successState: "success",
      },
      MODEL,
    );
    const completes = twice.messages.filter((m) => m.who === "complete");
    expect(completes).toHaveLength(1);
    // The first emit wins — the dedup guard collapses the second one.
    expect(completes[0].text).toBe("done");
  });
});

describe("mapChatEventToTaskEvent → structured_task_complete", () => {
  function chatEvent(content: string): ChatEvent {
    return {
      session_id: "sess-test",
      event_type: "structured_task_complete",
      content,
    } as ChatEvent;
  }

  it("parses summary + success_state from valid JSON", () => {
    const ev = mapChatEventToTaskEvent(
      chatEvent(
        JSON.stringify({ summary: "  shipped  ", success_state: "partial" }),
      ),
    );
    expect(ev).toEqual({
      kind: "structured_task_complete",
      summary: "shipped",
      successState: "partial",
    });
  });

  it("drops unknown success_state values", () => {
    const ev = mapChatEventToTaskEvent(
      chatEvent(JSON.stringify({ success_state: "bogus" })),
    );
    expect(ev).toEqual({
      kind: "structured_task_complete",
      summary: undefined,
      successState: undefined,
    });
  });

  it("caps summary at 500 chars", () => {
    const long = "x".repeat(800);
    const ev = mapChatEventToTaskEvent(
      chatEvent(JSON.stringify({ summary: long })),
    );
    if (!ev || ev.kind !== "structured_task_complete") {
      throw new Error("expected structured_task_complete");
    }
    expect(ev.summary?.length).toBe(500);
  });

  it("returns a completion event even for an empty / malformed payload", () => {
    expect(mapChatEventToTaskEvent(chatEvent(""))?.kind).toBe(
      "structured_task_complete",
    );
    expect(mapChatEventToTaskEvent(chatEvent("not-json"))?.kind).toBe(
      "structured_task_complete",
    );
  });
});

describe("taskReducer currentTurnComplete latch", () => {
  it("spinner stays off after `tool_start` for `submit_task_complete` lands post-completion", () => {
    const prev = stateInImplementPhase();
    const afterComplete = applyTaskEvent(
      prev,
      {
        kind: "structured_task_complete",
        summary: "done",
        successState: "success",
      },
      MODEL,
    );
    expect(afterComplete.currentTurnComplete).toBe(true);
    expect(afterComplete.streaming).toBe(false);

    // Backend emits the matching `tool_start` for the submit_task_complete
    // tool AFTER the structured event. Pre-fix this re-set streaming=true.
    const next = applyTaskEvent(
      afterComplete,
      {
        kind: "tool_start",
        data: { tool_call_id: "call-1", name: "submit_task_complete" },
      },
      MODEL,
    );
    expect(next.streaming).toBe(false);
    expect(next.currentTurnComplete).toBe(true);
  });

  it("trailing `chunk` after completion does not reactivate spinner", () => {
    const prev = stateInImplementPhase();
    const afterComplete = applyTaskEvent(
      prev,
      {
        kind: "structured_task_complete",
        summary: "done",
        successState: "success",
      },
      MODEL,
    );
    const msgLenBefore = afterComplete.messages.length;
    const next = applyTaskEvent(
      afterComplete,
      { kind: "chunk", content: "trailing text" },
      MODEL,
    );
    expect(next.streaming).toBe(false);
    expect(next.currentTurnComplete).toBe(true);
    // The chunk text MUST still be appended to messages (latch only gates
    // the streaming flag, not the message-list mutations).
    expect(next.messages.length).toBeGreaterThan(msgLenBefore);
  });

  it("trailing `thinking` after completion does not reactivate spinner", () => {
    const prev = stateInImplementPhase();
    const afterComplete = applyTaskEvent(
      prev,
      {
        kind: "structured_task_complete",
        summary: "done",
        successState: "success",
      },
      MODEL,
    );
    const next = applyTaskEvent(
      afterComplete,
      { kind: "thinking", content: "trailing reasoning" },
      MODEL,
    );
    expect(next.streaming).toBe(false);
    expect(next.currentTurnComplete).toBe(true);
  });

  it("new turn via `stream_start` clears the latch and re-enables streaming", () => {
    const prev = stateInImplementPhase();
    const afterComplete = applyTaskEvent(
      prev,
      {
        kind: "structured_task_complete",
        summary: "done",
        successState: "success",
      },
      MODEL,
    );
    expect(afterComplete.currentTurnComplete).toBe(true);

    const afterStart = applyTaskEvent(
      afterComplete,
      { kind: "stream_start" },
      MODEL,
    );
    expect(afterStart.currentTurnComplete).toBe(false);
    expect(afterStart.streaming).toBe(true);

    const afterChunk = applyTaskEvent(
      afterStart,
      { kind: "chunk", content: "new turn content" },
      MODEL,
    );
    expect(afterChunk.streaming).toBe(true);
    expect(afterChunk.currentTurnComplete).toBe(false);
  });
});
