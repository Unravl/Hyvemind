import { describe, it, expect } from "vitest";
import {
  processChunkEvent,
  processThinkingEvent,
  type TaskMessage,
} from "../taskReducer";

const MODEL = "claude-sub/claude-sonnet-4";

describe("processThinkingEvent (interleaved-thinking merge)", () => {
  it("merges thinking → text → thinking → text into a SINGLE bubble (extended-thinking interleave)", () => {
    // Simulates the canonical extended-thinking interleave from Claude Sonnet
    // with thinking enabled (or DeepSeek R1, GPT-5 Codex): the model emits
    // reasoning, partial text, more reasoning, more text — all inside one
    // assistant turn. The visible answer must stay contiguous in one bubble.
    let msgs: TaskMessage[] = [];
    msgs = processThinkingEvent(msgs, "first reasoning burst ", MODEL);
    msgs = processChunkEvent(msgs, "Sure, here's ", MODEL);
    msgs = processThinkingEvent(msgs, "second reasoning burst", MODEL);
    msgs = processChunkEvent(msgs, "the answer.", MODEL);

    expect(msgs).toHaveLength(1);
    expect(msgs[0].who).toBe("asst");
    // The visible answer was NOT sliced in half across two bubbles.
    expect(msgs[0].text).toBe("Sure, here's the answer.");
    // Both reasoning deltas accumulated into the single bubble's `reasoning`.
    expect(msgs[0].reasoning).toBe(
      "first reasoning burst second reasoning burst",
    );
    // `reasoningStartedAt` was set on the first thinking and preserved across
    // subsequent thinking appends, so the live duration timer measures the
    // whole turn.
    expect(msgs[0].reasoningStartedAt).toBeTypeOf("number");
    // The bubble is not finalized yet (no `done` event fired).
    expect(msgs[0].reasoningDurationMs).toBeUndefined();
  });

  it("preserves reasoningStartedAt across appended thinking bursts (timer measures whole turn)", () => {
    let msgs: TaskMessage[] = [];
    msgs = processThinkingEvent(msgs, "early thoughts ", MODEL);
    const firstStart = msgs[0].reasoningStartedAt;
    expect(firstStart).toBeTypeOf("number");

    // Force a measurable delta — Date.now() has ms resolution, so sleep a tick.
    const before = Date.now();
    while (Date.now() === before) {
      /* spin briefly */
    }

    msgs = processChunkEvent(msgs, "talking ", MODEL);
    msgs = processThinkingEvent(msgs, "more thoughts", MODEL);

    expect(msgs).toHaveLength(1);
    // The original start is retained — not reset to "now" when the second
    // thinking burst appended.
    expect(msgs[0].reasoningStartedAt).toBe(firstStart);
  });

  it("starts a fresh bubble when the previous turn has tool calls (tools mark a new turn)", () => {
    // Simulate: text → tool call → next thinking. The tool call ends the
    // previous turn; the next reasoning chunk belongs to a fresh bubble.
    let msgs: TaskMessage[] = [
      {
        who: "asst",
        text: "calling a tool",
        model: MODEL,
        tools: [
          { tool_call_id: "tc-1", name: "shell", output: "done", done: true },
        ],
      },
    ];
    msgs = processThinkingEvent(msgs, "post-tool reasoning", MODEL);

    expect(msgs).toHaveLength(2);
    expect(msgs[0].text).toBe("calling a tool");
    expect(msgs[0].tools?.length).toBe(1);
    expect(msgs[1].reasoning).toBe("post-tool reasoning");
    expect(msgs[1].text).toBeUndefined();
  });

  it("starts a fresh bubble when the previous turn was finalized (reasoningDurationMs set)", () => {
    // Simulates: a `done` event fired and finalizeReasoningDuration stamped
    // `reasoningDurationMs` onto the prior bubble. A subsequent thinking
    // chunk arrives on the next turn — must NOT append to the finalized one.
    let msgs: TaskMessage[] = [
      {
        who: "asst",
        text: "final answer",
        model: MODEL,
        reasoning: "earlier reasoning",
        reasoningStartedAt: 1000,
        reasoningDurationMs: 4200,
      },
    ];
    msgs = processThinkingEvent(msgs, "new turn reasoning", MODEL);

    expect(msgs).toHaveLength(2);
    expect(msgs[0].reasoning).toBe("earlier reasoning");
    expect(msgs[0].reasoningDurationMs).toBe(4200);
    expect(msgs[1].reasoning).toBe("new turn reasoning");
    expect(msgs[1].reasoningStartedAt).toBeTypeOf("number");
  });
});
