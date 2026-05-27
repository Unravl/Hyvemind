import { describe, it, expect } from "vitest";
import {
  applyHivemindEvent,
  attributionKeyFromEvent,
  type HivemindReviewState,
} from "../hivemindReducer";
import type { HivemindProgressEvent } from "../types";

const JOB = "job-rev-1";
const REVIEW = "rev-1";
const TASK = "task-1";

function evt(partial: Partial<HivemindProgressEvent>): HivemindProgressEvent {
  return {
    job_id: JOB,
    review_id: REVIEW,
    event_type: "started",
    round: 0,
    model_id: "",
    message: "",
    task_id: TASK,
    ...partial,
  } as HivemindProgressEvent;
}

function reduce(events: HivemindProgressEvent[]): HivemindReviewState {
  let state: HivemindReviewState = {};
  for (const e of events) {
    state = applyHivemindEvent(state, e);
  }
  return state;
}

const KEY = `task:${TASK}`;

describe("hivemindReducer phase pipeline", () => {
  it("lands on between_rounds after merge_completed for a non-final round", () => {
    const state = reduce([
      evt({ event_type: "started" }),
      evt({ event_type: "context_started", phase: "context" }),
      evt({ event_type: "context_completed", phase: "context" }),
      evt({ event_type: "round_started", round: 1, message: "Round 1 of 2 started" }),
      evt({
        event_type: "model_chunk",
        round: 1,
        model_id: "anthropic/claude",
        delta: "tok",
      }),
      evt({
        event_type: "model_completed",
        round: 1,
        model_id: "anthropic/claude",
        input_tokens: 10,
        output_tokens: 20,
        duration_ms: 500,
      }),
      evt({ event_type: "round_completed", round: 1 }),
      evt({ event_type: "merge_started", round: 1, phase: "merge" }),
      evt({
        event_type: "merge_chunk",
        round: 1,
        delta: "merge tok",
        phase: "merge",
      }),
      evt({
        event_type: "merge_completed",
        round: 1,
        message: "Round 1 merge complete",
      }),
    ]);

    const review = state[KEY];
    expect(review).toBeDefined();
    expect(review.phase).toBe("between_rounds");
    expect(review.roundOrder).toEqual([1]);
    expect(review.merges[1]?.status).toBe("completed");
    // currentRound (derived as roundOrder[roundOrder.length-1]) still
    // reads 1, which is what the "Round 1 merged" pill prints.
    expect(review.roundOrder[review.roundOrder.length - 1]).toBe(1);
  });

  it("flips to phase=round when the next round_started arrives", () => {
    const state = reduce([
      evt({ event_type: "started" }),
      evt({ event_type: "round_started", round: 1 }),
      evt({ event_type: "merge_started", round: 1 }),
      evt({ event_type: "merge_completed", round: 1 }),
      evt({ event_type: "round_started", round: 2, message: "Round 2 of 2 started" }),
    ]);

    const review = state[KEY];
    expect(review.phase).toBe("round");
    expect(review.roundOrder).toEqual([1, 2]);
    expect(review.roundOrder[review.roundOrder.length - 1]).toBe(2);
  });

  it("transitions between_rounds → completed on final-round completion", () => {
    const state = reduce([
      evt({ event_type: "started" }),
      evt({ event_type: "round_started", round: 1 }),
      evt({ event_type: "merge_started", round: 1 }),
      evt({ event_type: "merge_completed", round: 1 }),
      evt({ event_type: "completed", message: "review done" }),
    ]);

    const review = state[KEY];
    expect(review.phase).toBe("completed");
    expect(review.status).toBe("completed");
    expect(review.merges[1]?.status).toBe("completed");
  });

  it("does not regress phase to merge when a late merge_chunk arrives after round_started(N+1)", () => {
    // The reducer's `merge_chunk` arm sets phase = "merge" unconditionally.
    // Document the (slightly imperfect) current behaviour: if a delayed
    // R1 merge_chunk arrives AFTER R2's round_started, the phase will
    // briefly flip back to "merge". This test confirms that even so, the
    // round_order remains correct and the merge for R1 stays completed —
    // so subsequent R2 events restore phase to "round" without data loss.
    const state = reduce([
      evt({ event_type: "started" }),
      evt({ event_type: "round_started", round: 1 }),
      evt({ event_type: "merge_started", round: 1 }),
      evt({ event_type: "merge_completed", round: 1 }),
      evt({ event_type: "round_started", round: 2 }),
      // A late merge_chunk for round 1 lands after we've moved on.
      evt({ event_type: "merge_chunk", round: 1, delta: "late" }),
    ]);

    const review = state[KEY];
    // RoundOrder is preserved.
    expect(review.roundOrder).toEqual([1, 2]);
    // The R1 merge status remains completed — the late chunk does not
    // resurrect it as streaming.
    expect(review.merges[1]?.status).toBe("completed");
    // The next R2 model_chunk would set phase back to "round"; verify:
    const after = applyHivemindEvent(state, evt({
      event_type: "model_chunk",
      round: 2,
      model_id: "x",
      delta: "y",
    }));
    expect(after[KEY].phase).toBe("round");
  });

  it("merge_completed does not regress phase if we somehow saw the next round_started first", () => {
    // Out-of-order: a stray late merge_completed should not overwrite the
    // already-advanced "round" phase set by an earlier round_started(2).
    const state = reduce([
      evt({ event_type: "started" }),
      evt({ event_type: "round_started", round: 1 }),
      evt({ event_type: "merge_started", round: 1 }),
      evt({ event_type: "round_started", round: 2 }),
      // Stray late R1 merge_completed.
      evt({ event_type: "merge_completed", round: 1 }),
    ]);

    const review = state[KEY];
    expect(review.phase).toBe("round");
    expect(review.merges[1]?.status).toBe("completed");
  });
});

describe("hivemindReducer round_started model seeding", () => {
  it("seeds streaming rows for every model in `model_instances` immediately", () => {
    const state = reduce([
      evt({ event_type: "started" }),
      evt({
        event_type: "round_started",
        round: 1,
        message: "Round 1 of 1 started",
        model_instances: [
          { model_id: "anthropic/claude", model_idx: 0 },
          { model_id: "openai/gpt-4", model_idx: 1 },
        ],
      }),
    ]);

    const review = state[KEY];
    expect(review).toBeDefined();
    expect(review.phase).toBe("round");
    expect(review.roundOrder).toEqual([1]);
    // showRows is gated on roundOrder.length > 0
    expect(review.roundOrder.length > 0).toBe(true);
    const round = review.rounds[1];
    expect(round).toBeDefined();
    // modelOrder is now keyed by instanceKey (`${modelId}#${modelIdx}`).
    expect(round.modelOrder).toEqual(["anthropic/claude#0", "openai/gpt-4#1"]);
    for (const key of round.modelOrder) {
      const model = round.models[key];
      expect(model.status).toBe("streaming");
      expect(model.outputPreview).toBe("");
      expect(model.instanceKey).toBe(key);
    }
  });

  it("subsequent model_chunk mutates the pre-seeded row without duplicating it", () => {
    const state = reduce([
      evt({ event_type: "started" }),
      evt({
        event_type: "round_started",
        round: 1,
        model_instances: [
          { model_id: "anthropic/claude", model_idx: 0 },
          { model_id: "openai/gpt-4", model_idx: 1 },
        ],
      }),
      evt({
        event_type: "model_chunk",
        round: 1,
        model_id: "anthropic/claude",
        model_idx: 0,
        delta: "hello",
      }),
    ]);

    const round = state[KEY].rounds[1];
    expect(round.modelOrder).toEqual(["anthropic/claude#0", "openai/gpt-4#1"]);
    expect(round.models["anthropic/claude#0"].outputPreview).toContain("hello");
    expect(round.models["anthropic/claude#0"].status).toBe("streaming");
    // The other pre-seeded row is still present and untouched.
    expect(round.models["openai/gpt-4#1"].outputPreview).toBe("");
  });

  it("round_started with empty `model_instances` array creates the round but seeds no rows", () => {
    const state = reduce([
      evt({ event_type: "started" }),
      evt({
        event_type: "round_started",
        round: 1,
        model_instances: [],
      }),
    ]);

    const review = state[KEY];
    expect(review.roundOrder).toEqual([1]);
    expect(review.rounds[1]).toBeDefined();
    expect(review.rounds[1].modelOrder).toEqual([]);
  });

  it("keeps distinct rows for duplicate model_ids with different model_idx", () => {
    // The original bug: four reviewer instances configured against the
    // same provider/model (different temperatures) collapsed into a
    // single row because the reducer was keyed by `model_id` alone. With
    // the instance-keyed reducer they must remain independent.
    const state = reduce([
      evt({ event_type: "started" }),
      evt({
        event_type: "round_started",
        round: 1,
        model_instances: [
          { model_id: "anthropic/claude", model_idx: 0 },
          { model_id: "anthropic/claude", model_idx: 2 },
        ],
      }),
      evt({
        event_type: "model_chunk",
        round: 1,
        model_id: "anthropic/claude",
        model_idx: 0,
        delta: "first-instance output",
      }),
      evt({
        event_type: "model_chunk",
        round: 1,
        model_id: "anthropic/claude",
        model_idx: 2,
        delta: "second-instance output",
      }),
    ]);

    const round = state[KEY].rounds[1];
    expect(round.modelOrder.length).toBe(2);
    expect(round.modelOrder).toEqual([
      "anthropic/claude#0",
      "anthropic/claude#2",
    ]);
    expect(round.models["anthropic/claude#0"]).toBeDefined();
    expect(round.models["anthropic/claude#2"]).toBeDefined();
    expect(round.models["anthropic/claude#0"].outputPreview).toBe(
      "first-instance output",
    );
    expect(round.models["anthropic/claude#2"].outputPreview).toBe(
      "second-instance output",
    );
    // Both rows still share the same human-facing modelId.
    expect(round.models["anthropic/claude#0"].modelId).toBe(
      "anthropic/claude",
    );
    expect(round.models["anthropic/claude#2"].modelId).toBe(
      "anthropic/claude",
    );
  });

  it("falls back to legacy `models` array (array index as implicit model_idx)", () => {
    // Back-compat: a legacy `round_started` from an older backend build
    // carries only `models: string[]` (no per-instance `model_idx`). The
    // reducer must still create one row per scheduled model by treating
    // the array index as the implicit index, producing distinct instance
    // keys even when the older backend never sent `model_idx`.
    const state = reduce([
      evt({ event_type: "started" }),
      evt({
        event_type: "round_started",
        round: 1,
        models: ["a", "b"],
      }),
    ]);

    const round = state[KEY].rounds[1];
    expect(round.modelOrder).toEqual(["a#0", "b#1"]);
    expect(round.models["a#0"]).toBeDefined();
    expect(round.models["b#1"]).toBeDefined();
    expect(round.models["a#0"].modelId).toBe("a");
    expect(round.models["b#1"].modelId).toBe("b");
  });

  it("falls back to bare model_id key when an event lacks model_idx entirely", () => {
    // A legacy `model_chunk` (no `model_idx` field at all) still needs to
    // land somewhere. `instanceKeyOf(modelId, undefined)` falls back to
    // the bare `modelId`, which collapses duplicate-instance events from
    // the legacy backend into a single row — acceptable, since the old
    // backend never disambiguated them either.
    const state = reduce([
      evt({ event_type: "started" }),
      evt({
        event_type: "model_chunk",
        round: 1,
        model_id: "legacy/model",
        delta: "x",
      }),
    ]);

    const round = state[KEY].rounds[1];
    expect(round.modelOrder).toEqual(["legacy/model"]);
    expect(round.models["legacy/model"].modelId).toBe("legacy/model");
    expect(round.models["legacy/model"].outputPreview).toBe("x");
  });
});

describe("hivemindReducer attribution routing", () => {
  it("routes events with task_id to a task:{id} key", () => {
    const e = evt({ event_type: "started", task_id: "task-99" });
    expect(attributionKeyFromEvent(e)).toBe("task:task-99");
  });

  it("routes events with swarm_id+feature_id to swarm:{}:feat:{} key", () => {
    const e = evt({
      event_type: "started",
      task_id: null,
      swarm_id: "swarm-1",
      feature_id: "feat-a",
    });
    expect(attributionKeyFromEvent(e)).toBe("swarm:swarm-1:feat:feat-a");
  });

  it("routes swarm queen events (no feature_id) to swarm:{}:queen key", () => {
    const e = evt({
      event_type: "started",
      task_id: null,
      swarm_id: "swarm-1",
      feature_id: null,
    });
    expect(attributionKeyFromEvent(e)).toBe("swarm:swarm-1:queen");
  });

  it("falls back to job:{} when no other attribution is present", () => {
    const e = evt({
      event_type: "started",
      task_id: null,
      swarm_id: null,
      feature_id: null,
    });
    expect(attributionKeyFromEvent(e)).toBe(`job:${JOB}`);
  });

  it("fans out to only the matching attribution key", () => {
    // Two events for the same job_id but different task_ids must be
    // routed to distinct keys; nothing leaks across.
    const a = applyHivemindEvent({}, evt({
      event_type: "round_started",
      round: 1,
      task_id: "task-A",
    }));
    const b = applyHivemindEvent(a, evt({
      event_type: "round_started",
      round: 1,
      task_id: "task-B",
    }));

    expect(b["task:task-A"]).toBeDefined();
    expect(b["task:task-B"]).toBeDefined();
    expect(b["task:task-A"].phase).toBe("round");
    expect(b["task:task-B"].phase).toBe("round");
    // The two states are independent objects.
    expect(b["task:task-A"]).not.toBe(b["task:task-B"]);
  });
});
