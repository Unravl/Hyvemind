import { describe, it, expect } from "vitest";
import {
  applyTaskEvent,
  makeInitialTaskState,
  type TaskRuntimeState,
} from "../taskReducer";
import type { SwarmFeatureSpec, MilestoneSpec } from "../plan-mode";

const MODEL = "claude-sub/claude-opus-4-7";

const ORIG: SwarmFeatureSpec = {
  id: "feat-1",
  name: "Original",
  description: "pre-review feature",
  dependencies: [],
} as any;

const REFINED: SwarmFeatureSpec = {
  id: "feat-1",
  name: "Refined",
  description: "post-review feature",
  dependencies: [],
} as any;

const ORIG_MILESTONE: MilestoneSpec = {
  id: "m1",
  name: "M1",
  features: ["feat-1"],
  assertions: [],
} as any;

const REFINED_MILESTONE: MilestoneSpec = {
  id: "m1",
  name: "M1-refined",
  features: ["feat-1"],
  assertions: [],
} as any;

/** Seed a state that simulates "post-Hivemind, awaiting features refresh". */
function pendingRefreshState(): TaskRuntimeState {
  return {
    ...makeInitialTaskState("task-test", MODEL),
    pendingFeaturesRefresh: true,
    swarmId: "swarm-x",
    swarmFeatures: [ORIG],
    swarmMilestones: [ORIG_MILESTONE],
    featuresRefreshFailed: false,
  };
}

describe("taskReducer features-refresh terminal-event behaviour", () => {
  it("Case A — done after failure: clears pending, trips failure, keeps stale features", () => {
    const prev = pendingRefreshState();
    const next = applyTaskEvent(prev, { kind: "done" }, MODEL);
    expect(next.pendingFeaturesRefresh).toBe(false);
    expect(next.featuresRefreshFailed).toBe(true);
    // Stale features must remain so the user can still launch with them.
    expect(next.swarmFeatures).toEqual([ORIG]);
    expect(next.swarmMilestones).toEqual([ORIG_MILESTONE]);
  });

  it("Case B — done after success: structured_features clears pending, done preserves clean state", () => {
    const prev = pendingRefreshState();
    const afterFeatures = applyTaskEvent(
      prev,
      {
        kind: "structured_features",
        features: [REFINED],
        milestones: [REFINED_MILESTONE],
      },
      MODEL,
    );
    expect(afterFeatures.pendingFeaturesRefresh).toBe(false);
    expect(afterFeatures.featuresRefreshFailed).toBe(false);
    expect(afterFeatures.swarmFeatures).toEqual([REFINED]);

    const next = applyTaskEvent(afterFeatures, { kind: "done" }, MODEL);
    expect(next.pendingFeaturesRefresh).toBe(false);
    // Trailing done must NOT flip failure back to true (prev.pendingFeaturesRefresh is already false here).
    expect(next.featuresRefreshFailed).toBe(false);
    expect(next.swarmFeatures).toEqual([REFINED]);
  });

  it("Case C — error: clears pending and trips failure", () => {
    const prev = pendingRefreshState();
    const next = applyTaskEvent(prev, { kind: "error", message: "boom" }, MODEL);
    expect(next.pendingFeaturesRefresh).toBe(false);
    expect(next.featuresRefreshFailed).toBe(true);
    expect(next.error).toContain("boom");
    expect(next.streaming).toBe(false);
  });

  it("Case D — retry success path: structured_features clears featuresRefreshFailed", () => {
    const prev: TaskRuntimeState = {
      ...makeInitialTaskState("task-test", MODEL),
      pendingFeaturesRefresh: false,
      featuresRefreshFailed: true,
      swarmId: "swarm-x",
      swarmFeatures: [ORIG],
      swarmMilestones: [ORIG_MILESTONE],
    };
    const next = applyTaskEvent(
      prev,
      {
        kind: "structured_features",
        features: [REFINED],
        milestones: [REFINED_MILESTONE],
      },
      MODEL,
    );
    expect(next.featuresRefreshFailed).toBe(false);
    expect(next.pendingFeaturesRefresh).toBe(false);
    expect(next.swarmFeatures).toEqual([REFINED]);
  });

  it("Case E — stop → done composition: failure flag set once and preserved", () => {
    const prev = pendingRefreshState();
    const afterStop = applyTaskEvent(prev, { kind: "stop" }, MODEL);
    expect(afterStop.pendingFeaturesRefresh).toBe(false);
    expect(afterStop.featuresRefreshFailed).toBe(true);

    const afterDone = applyTaskEvent(afterStop, { kind: "done" }, MODEL);
    expect(afterDone.pendingFeaturesRefresh).toBe(false);
    // Trailing done sees prev.pendingFeaturesRefresh === false but
    // prev.featuresRefreshFailed === true → preserve.
    expect(afterDone.featuresRefreshFailed).toBe(true);

    // Same with error → done.
    const prev2 = pendingRefreshState();
    const afterError = applyTaskEvent(prev2, { kind: "error", message: "x" }, MODEL);
    expect(afterError.featuresRefreshFailed).toBe(true);
    const afterDone2 = applyTaskEvent(afterError, { kind: "done" }, MODEL);
    expect(afterDone2.featuresRefreshFailed).toBe(true);
  });

  it("Case F — structured_features → stop must not flip failure to true", () => {
    const prev = pendingRefreshState();
    const afterFeatures = applyTaskEvent(
      prev,
      {
        kind: "structured_features",
        features: [REFINED],
        milestones: [REFINED_MILESTONE],
      },
      MODEL,
    );
    expect(afterFeatures.featuresRefreshFailed).toBe(false);
    expect(afterFeatures.pendingFeaturesRefresh).toBe(false);

    const afterStop = applyTaskEvent(afterFeatures, { kind: "stop" }, MODEL);
    // stop sees prev.pendingFeaturesRefresh === false (already cleared by
    // structured_features) → must NOT trip failure.
    expect(afterStop.featuresRefreshFailed).toBe(false);
    expect(afterStop.pendingFeaturesRefresh).toBe(false);
  });

  it("Case G — stream_start preserves pendingFeaturesRefresh (regression guard)", () => {
    const prev = pendingRefreshState();
    const next = applyTaskEvent(prev, { kind: "stream_start" }, MODEL);
    // If a future refactor adds pendingFeaturesRefresh to the stream_start
    // reset list, the whole done-path fix collapses (prev would be false by
    // the time done arrives). This test locks the current behaviour in.
    expect(next.pendingFeaturesRefresh).toBe(true);
    expect(next.featuresRefreshFailed).toBe(false);
  });
});
