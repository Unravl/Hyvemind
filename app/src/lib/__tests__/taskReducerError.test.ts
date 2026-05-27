import { describe, it, expect } from "vitest";
import { applyTaskEvent, makeInitialTaskState } from "../taskReducer";

const MODEL = "claude-sub/claude-opus-4-7";

function initial() {
  return makeInitialTaskState("task-test", MODEL);
}

describe("taskReducer error branch", () => {
  it("appends an inline error bubble carrying the message", () => {
    const next = applyTaskEvent(
      initial(),
      { kind: "error", message: "You're out of extra usage." },
      MODEL,
    );
    const bubbles = next.messages.filter((m) => m.who === "error");
    expect(bubbles).toHaveLength(1);
    expect(bubbles[0].errorMessage).toBe("You're out of extra usage.");
    expect(next.error).toBe("You're out of extra usage.");
    expect(next.streaming).toBe(false);
  });

  /**
   * Regression: an empty / whitespace-only message used to short-circuit the
   * bubble append branch entirely, leaving the user staring at a finished
   * spinner with no reply OR error visible. The bubble must still appear so
   * the failure is observable.
   */
  it("falls back to a generic label when message is empty", () => {
    const next = applyTaskEvent(initial(), { kind: "error", message: "" }, MODEL);
    const bubbles = next.messages.filter((m) => m.who === "error");
    expect(bubbles).toHaveLength(1);
    expect(bubbles[0].errorMessage).toContain("Provider error");
    expect(next.error).toContain("Provider error");
  });

  it("falls back when message is only whitespace", () => {
    const next = applyTaskEvent(initial(), { kind: "error", message: "   \n  " }, MODEL);
    const bubbles = next.messages.filter((m) => m.who === "error");
    expect(bubbles).toHaveLength(1);
    expect(bubbles[0].errorMessage).toContain("Provider error");
  });

  it("dedups against an identical trailing error bubble", () => {
    const once = applyTaskEvent(initial(), { kind: "error", message: "boom" }, MODEL);
    const twice = applyTaskEvent(once, { kind: "error", message: "boom" }, MODEL);
    const bubbles = twice.messages.filter((m) => m.who === "error");
    expect(bubbles).toHaveLength(1);
  });
});
