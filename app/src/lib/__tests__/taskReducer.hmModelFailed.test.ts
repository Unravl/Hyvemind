import { describe, it, expect } from "vitest";
import { applyTaskEvent, makeInitialTaskState } from "../taskReducer";

const MODEL = "claude-sub/claude-opus-4-7";

function driveToRoundStarted() {
  let state = makeInitialTaskState("task-hm", MODEL);
  state = applyTaskEvent(
    state,
    {
      kind: "review_start",
      jobId: "job-1",
      round: 1,
      totalRounds: 1,
      models: ["anthropic/claude-sonnet-4", "openai/gpt-4o"],
      reviewId: "hmr-1",
    },
    MODEL,
  );
  state = applyTaskEvent(state, { kind: "hm_started", jobId: "job-1" }, MODEL);
  state = applyTaskEvent(
    state,
    { kind: "hm_round_started", jobId: "job-1", round: 1, models: ["anthropic/claude-sonnet-4", "openai/gpt-4o"] },
    MODEL,
  );
  return state;
}

describe("taskReducer — hm_model_failed conversation bubble", () => {
  it("appends exactly one who:error bubble with the full error and hivemindFailureKey", () => {
    const state = driveToRoundStarted();
    const errorText = "Anthropic API 401: invalid x-api-key — full body...".repeat(3);
    const next = applyTaskEvent(
      state,
      {
        kind: "hm_model_failed",
        jobId: "job-1",
        modelId: "anthropic/claude-sonnet-4",
        modelIdx: 0,
        round: 1,
        error: errorText,
      },
      MODEL,
    );

    const bubbles = next.messages.filter((m) => m.who === "error");
    expect(bubbles).toHaveLength(1);
    const bubble = bubbles[0];
    expect(bubble.errorMessage).toBe(
      `Hivemind reviewer anthropic/claude-sonnet-4 (round 1) failed: ${errorText}`,
    );
    expect(bubble.hivemindFailureKey).toBe(
      "job-1::1::anthropic/claude-sonnet-4::0",
    );
    expect(bubble.errorMessage).toContain(errorText);

    // The reducer also flips the model row state.
    const round = next.reviewProgress?.rounds.find((r) => r.round === 1);
    expect(
      round?.models.find(
        (m) => m.modelId === "anthropic/claude-sonnet-4",
      )?.status,
    ).toBe("failed");
  });

  it("dedups a second hm_model_failed with the same (jobId, round, modelId, modelIdx)", () => {
    const state = driveToRoundStarted();
    const evt = {
      kind: "hm_model_failed" as const,
      jobId: "job-1",
      modelId: "anthropic/claude-sonnet-4",
      modelIdx: 0,
      round: 1,
      error: "boom",
    };
    const once = applyTaskEvent(state, evt, MODEL);
    const twice = applyTaskEvent(once, evt, MODEL);
    const bubbles = twice.messages.filter((m) => m.who === "error");
    expect(bubbles).toHaveLength(1);
  });

  it("produces two distinct bubbles when modelIdx differs (duplicate-instance reviewers)", () => {
    let state = makeInitialTaskState("task-hm-dup", MODEL);
    state = applyTaskEvent(
      state,
      {
        kind: "review_start",
        jobId: "job-2",
        round: 1,
        totalRounds: 1,
        models: ["anthropic/claude-sonnet-4", "anthropic/claude-sonnet-4"],
        reviewId: "hmr-2",
      },
      MODEL,
    );
    state = applyTaskEvent(state, { kind: "hm_started", jobId: "job-2" }, MODEL);
    state = applyTaskEvent(
      state,
      {
        kind: "hm_round_started",
        jobId: "job-2",
        round: 1,
        models: ["anthropic/claude-sonnet-4", "anthropic/claude-sonnet-4"],
      },
      MODEL,
    );

    state = applyTaskEvent(
      state,
      {
        kind: "hm_model_failed",
        jobId: "job-2",
        modelId: "anthropic/claude-sonnet-4",
        modelIdx: 0,
        round: 1,
        error: "401 unauthorized",
      },
      MODEL,
    );
    state = applyTaskEvent(
      state,
      {
        kind: "hm_model_failed",
        jobId: "job-2",
        modelId: "anthropic/claude-sonnet-4",
        modelIdx: 1,
        round: 1,
        error: "429 rate limited",
      },
      MODEL,
    );

    const bubbles = state.messages.filter((m) => m.who === "error");
    expect(bubbles).toHaveLength(2);
    expect(bubbles[0].hivemindFailureKey).toBe(
      "job-2::1::anthropic/claude-sonnet-4::0",
    );
    expect(bubbles[1].hivemindFailureKey).toBe(
      "job-2::1::anthropic/claude-sonnet-4::1",
    );
    expect(bubbles[0].errorMessage).toContain("401 unauthorized");
    expect(bubbles[1].errorMessage).toContain("429 rate limited");
  });

  it("preserves the bubble across a subsequent hm_failed (review-terminal) event", () => {
    let state = driveToRoundStarted();
    state = applyTaskEvent(
      state,
      {
        kind: "hm_model_failed",
        jobId: "job-1",
        modelId: "anthropic/claude-sonnet-4",
        modelIdx: 0,
        round: 1,
        error: "boom",
      },
      MODEL,
    );
    // hm_failed clears reviewProgress but should NOT prune the persisted bubble.
    state = applyTaskEvent(
      state,
      { kind: "hm_failed", jobId: "job-1", message: "Review failed" },
      MODEL,
    );

    const bubbles = state.messages.filter((m) => m.who === "error");
    // The bubble we appended in step 1 must still be present after hm_failed.
    const hmBubble = bubbles.find(
      (b) =>
        b.hivemindFailureKey === "job-1::1::anthropic/claude-sonnet-4::0",
    );
    expect(hmBubble).toBeDefined();
    expect(hmBubble?.errorMessage).toContain("boom");
    expect(state.reviewProgress).toBeNull();
  });
});
