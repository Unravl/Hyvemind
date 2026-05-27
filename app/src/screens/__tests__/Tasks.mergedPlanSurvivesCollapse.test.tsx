import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import React, { useEffect, useState } from "react";
import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { HivemindReviewLivePanel } from "../../components/HivemindReviewLivePanel";
import { HivemindReviewCollapsedBar } from "../../components/HivemindReviewCollapsedBar";
import { MergedPlanModal } from "../../components/MergedPlanModal";
import type { ReviewState } from "../../lib/hivemindReducer";
import { loadMergedPlan } from "../../lib/mergedPlanLoader";

// Stub the lazy plan loader so no IPC happens during render. We resolve
// it with deterministic markdown so the integration test can assert the
// modal's title/content survives the auto-collapse.
vi.mock("../../lib/mergedPlanLoader", () => ({
  loadMergedPlan: vi.fn(async () => "# Merged plan for round 1\n\nSome content."),
}));

/**
 * Minimal recreation of the Tasks-screen dock block:
 *  - Mirrors `useReviewDockMode`'s 5s auto-collapse timer (the real hook
 *    lives inside Tasks.tsx; the bug is independent of the hook's other
 *    behaviour, so we reproduce only the relevant timer here).
 *  - Renders `<HivemindReviewLivePanel>` while `mode === "expanded"`,
 *    swaps to `<HivemindReviewCollapsedBar>` after 5s.
 *  - Holds a screen-level `activeMergedPlan` state and renders a single
 *    `<MergedPlanModal>` as a sibling of the dock \u2014 i.e. exactly the
 *    structure the post-fix Tasks screen uses.
 *
 * The regression test: click "View merged plan" while expanded, advance
 * timers past 5s so the panel is unmounted and replaced by the collapsed
 * bar, and assert the modal is still in the DOM.
 */
function TasksDockHarness({ state }: { state: ReviewState }) {
  const [mode, setMode] = useState<"expanded" | "collapsed">("expanded");
  const [activeMergedPlan, setActiveMergedPlan] = useState<
    { round: number; text: string } | null
  >(null);

  // Mirror the 5s auto-collapse from useReviewDockMode for completed states.
  useEffect(() => {
    if (state.status !== "running") {
      const t = setTimeout(() => setMode("collapsed"), 5000);
      return () => clearTimeout(t);
    }
  }, [state.status]);

  return (
    <div>
      {mode === "expanded" ? (
        <HivemindReviewLivePanel
          state={state}
          sourceLabel="test-hivemind"
          onViewMergedPlan={({ round, text }) =>
            setActiveMergedPlan({ round, text })
          }
        />
      ) : (
        <HivemindReviewCollapsedBar
          state={state}
          sourceLabel="test-hivemind"
          onExpand={() => setMode("expanded")}
          onViewMergedPlan={({ round, text }) =>
            setActiveMergedPlan({ round, text })
          }
        />
      )}

      <MergedPlanModal
        open={activeMergedPlan != null}
        title={
          activeMergedPlan
            ? `Merged plan \u2014 Round ${activeMergedPlan.round}`
            : "Merged plan"
        }
        planText={activeMergedPlan?.text ?? ""}
        onClose={() => setActiveMergedPlan(null)}
      />
    </div>
  );
}

function makeCompletedState(): ReviewState {
  return {
    jobId: "job-1",
    status: "completed",
    phase: "completed",
    sourceLabel: "test-hivemind",
    rounds: {
      1: {
        round: 1,
        models: {
          m: { instanceKey: "m", modelId: "m", status: "completed", outputPreview: "" },
        },
        modelOrder: ["m"],
      },
    },
    roundOrder: [1],
    merges: { 1: { round: 1, status: "completed", preview: "" } },
    mergeOrder: [1],
    startedAt: Date.now() - 1000,
    endedAt: Date.now(),
  };
}

describe("Tasks merged-plan modal survives the dock auto-collapse", () => {
  beforeEach(() => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    vi.mocked(loadMergedPlan).mockResolvedValue(
      "# Merged plan for round 1\n\nSome content.",
    );
  });

  afterEach(() => {
    // Run any pending timers cleanly so they don't leak into sibling
    // tests, then restore real timers.
    vi.runOnlyPendingTimers();
    vi.useRealTimers();
    vi.clearAllMocks();
  });

  it("keeps the merged-plan modal open after the panel auto-collapses", async () => {
    const state = makeCompletedState();
    // StrictMode wrapper catches the focus-trap onDeactivate-on-unmount regression (focus-trap-react#738).
    render(
      <React.StrictMode>
        <TasksDockHarness state={state} />
      </React.StrictMode>,
    );

    // Expanded panel should be visible with a "View merged plan" trigger.
    const trigger = screen.getByRole("button", {
      name: /view merged plan for round 1/i,
    });
    fireEvent.click(trigger);

    // Wait for the modal title to appear (the loader resolves asynchronously
    // and the callback then opens the screen-level modal).
    await waitFor(() => {
      expect(screen.getByText("Merged plan \u2014 Round 1")).toBeInTheDocument();
    });

    // Now advance past the 5s auto-collapse. The expanded panel unmounts
    // and the collapsed bar mounts in its place. Pre-fix, the modal would
    // unmount with the panel because it was a child of it.
    await act(async () => {
      vi.advanceTimersByTime(5000);
    });

    // The collapsed bar should now be visible (its expand affordance is
    // labelled "Expand Hivemind review panel"). This proves the parent
    // swap actually happened.
    expect(
      screen.getByRole("button", { name: /expand hivemind review panel/i }),
    ).toBeInTheDocument();

    // Critical assertion: the modal is still in the DOM with the round-1
    // title, despite the parent panel having been unmounted.
    expect(screen.getByText("Merged plan \u2014 Round 1")).toBeInTheDocument();
  });

  it("opens the modal from the collapsed bar's Plan button after auto-collapse", async () => {
    const state = makeCompletedState();
    // StrictMode wrapper catches the focus-trap onDeactivate-on-unmount regression (focus-trap-react#738).
    render(
      <React.StrictMode>
        <TasksDockHarness state={state} />
      </React.StrictMode>,
    );

    // Auto-collapse first, then click the bar's Plan button.
    await act(async () => {
      vi.advanceTimersByTime(5000);
    });

    const planBtn = screen.getByRole("button", {
      name: /view merged plan for final round/i,
    });
    fireEvent.click(planBtn);

    await waitFor(() => {
      expect(screen.getByText("Merged plan \u2014 Round 1")).toBeInTheDocument();
    });
  });
});
