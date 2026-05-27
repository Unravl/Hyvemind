import { describe, it, expect, vi } from "vitest";
import { render, screen, fireEvent, waitFor } from "@testing-library/react";
import { HivemindReviewLivePanel } from "../HivemindReviewLivePanel";
import type { ReviewState } from "../../lib/hivemindReducer";
import { loadMergedPlan } from "../../lib/mergedPlanLoader";

// The panel pulls in MergedPlanModal which itself pulls in the IPC layer.
// Stub the lazy plan loader so no IPC happens during render.
vi.mock("../../lib/mergedPlanLoader", () => ({
  loadMergedPlan: vi.fn(async () => null),
}));

function makeState(overrides: Partial<ReviewState>): ReviewState {
  return {
    jobId: "job-1",
    status: "running",
    phase: "round",
    rounds: {},
    roundOrder: [],
    merges: {},
    mergeOrder: [],
    startedAt: Date.now() - 1000,
    ...overrides,
  };
}

describe("HivemindReviewLivePanel phase pill", () => {
  it("renders 'Merging R1' while merge is in flight", () => {
    const state = makeState({
      phase: "merge",
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
      merges: { 1: { round: 1, status: "streaming", preview: "" } },
      mergeOrder: [1],
    });
    render(<HivemindReviewLivePanel state={state} />);
    expect(screen.getByText("Merging R1")).toBeInTheDocument();
    // The "Synthesising reviewer feedback…" spinner block is visible.
    expect(
      screen.getByText(/Synthesising reviewer feedback/),
    ).toBeInTheDocument();
  });

  it("renders 'Round 1 merged' in the between_rounds phase with no merge spinner", () => {
    const state = makeState({
      phase: "between_rounds",
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
    });
    render(<HivemindReviewLivePanel state={state} />);
    expect(screen.getByText("Round 1 merged")).toBeInTheDocument();
    // The "Synthesising reviewer feedback…" copy must NOT appear during
    // the between_rounds window — that copy belongs to the in-flight
    // merge phase only.
    expect(
      screen.queryByText(/Synthesising reviewer feedback/),
    ).not.toBeInTheDocument();
  });

  it("renders 'Round 2/2' once the next round starts", () => {
    const state = makeState({
      phase: "round",
      rounds: {
        1: {
          round: 1,
          models: {
            m: { instanceKey: "m", modelId: "m", status: "completed", outputPreview: "" },
          },
          modelOrder: ["m"],
        },
        2: {
          round: 2,
          models: {
            m: { instanceKey: "m", modelId: "m", status: "streaming", outputPreview: "" },
          },
          modelOrder: ["m"],
        },
      },
      roundOrder: [1, 2],
      merges: { 1: { round: 1, status: "completed", preview: "" } },
      mergeOrder: [1],
    });
    render(<HivemindReviewLivePanel state={state} />);
    expect(screen.getByText("Round 2/2")).toBeInTheDocument();
  });

  it("keeps the merged-plan modal open when a new round arrives (regression: state-race bug)", async () => {
    // Reproduce the bug: open the modal for R1, then re-render with a
    // state where R2 has started. Previously the effect on `activeRound`
    // would call `setMergedPlan(null)` and the modal would vanish.
    const mockedLoader = vi.mocked(loadMergedPlan);
    mockedLoader.mockResolvedValueOnce("# Merged plan for round 1\n\nSome content.");

    const r1State = makeState({
      phase: "between_rounds",
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
    });

    const { rerender } = render(<HivemindReviewLivePanel state={r1State} />);

    const button = screen.getByRole("button", {
      name: /view merged plan for round 1/i,
    });
    fireEvent.click(button);

    // Wait for the modal to appear with the loaded content.
    await waitFor(() => {
      expect(screen.getByText("Merged plan \u2014 Round 1")).toBeInTheDocument();
    });

    // Re-render with a state that advances `currentRound` — simulating the
    // live `round_started` event for round 2 that previously closed the modal.
    const r2State = makeState({
      phase: "round",
      rounds: {
        1: {
          round: 1,
          models: {
            m: { instanceKey: "m", modelId: "m", status: "completed", outputPreview: "" },
          },
          modelOrder: ["m"],
        },
        2: {
          round: 2,
          models: {
            m: { instanceKey: "m", modelId: "m", status: "streaming", outputPreview: "" },
          },
          modelOrder: ["m"],
        },
      },
      roundOrder: [1, 2],
      merges: { 1: { round: 1, status: "completed", preview: "" } },
      mergeOrder: [1],
    });
    rerender(<HivemindReviewLivePanel state={r2State} />);

    // The modal must still be present and must still show round 1 — the
    // round whose plan was loaded — even though the panel's `activeRound`
    // has advanced to 2.
    expect(screen.getByText("Merged plan \u2014 Round 1")).toBeInTheDocument();
    // The underlying panel's pill should reflect the new round.
    expect(screen.getByText("Round 2/2")).toBeInTheDocument();
  });

  it("delegates to onViewMergedPlan when provided and does not render its own modal", async () => {
    const mockedLoader = vi.mocked(loadMergedPlan);
    mockedLoader.mockResolvedValueOnce("# Merged plan for round 1\n\nSome content.");

    const onViewMergedPlan = vi.fn();
    const state = makeState({
      phase: "between_rounds",
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
    });

    render(
      <HivemindReviewLivePanel
        state={state}
        onViewMergedPlan={onViewMergedPlan}
      />,
    );

    const button = screen.getByRole("button", {
      name: /view merged plan for round 1/i,
    });
    fireEvent.click(button);

    await waitFor(() => {
      expect(onViewMergedPlan).toHaveBeenCalledWith({
        round: 1,
        text: "# Merged plan for round 1\n\nSome content.",
      });
    });

    // Critical assertion: the panel does NOT render its own modal when the
    // parent owns the merged-plan modal slot.
    expect(screen.queryByText(/Merged plan \u2014 Round/)).not.toBeInTheDocument();
  });

  it("emits a screen-reader announcement during between_rounds", () => {
    const state = makeState({
      phase: "between_rounds",
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
    });
    render(<HivemindReviewLivePanel state={state} />);
    expect(
      screen.getByText("Round 1 merged, preparing next round"),
    ).toBeInTheDocument();
  });

  it("renders four rows with disambiguated labels for duplicate model_id instances", () => {
    // The original bug repro: four reviewer instances all configured
    // against `anthropic/claude-sonnet-4` (e.g. with different
    // temperatures) used to collapse into a single row. With the
    // instance-keyed reducer + the inline dedupe-label logic in the
    // panel they must render as four independent rows labelled
    // `anthropic/claude-sonnet-4`, `... #2`, `... #3`, `... #4` —
    // matching the convention from `dedupeReviewerLabels` in
    // `review-mode.ts` so live and historical views agree.
    const modelId = "anthropic/claude-sonnet-4";
    const state = makeState({
      phase: "round",
      rounds: {
        1: {
          round: 1,
          models: {
            [`${modelId}#0`]: {
              instanceKey: `${modelId}#0`,
              modelId,
              status: "completed",
              outputPreview: "",
            },
            [`${modelId}#1`]: {
              instanceKey: `${modelId}#1`,
              modelId,
              status: "completed",
              outputPreview: "",
            },
            [`${modelId}#2`]: {
              instanceKey: `${modelId}#2`,
              modelId,
              status: "completed",
              outputPreview: "",
            },
            [`${modelId}#3`]: {
              instanceKey: `${modelId}#3`,
              modelId,
              status: "completed",
              outputPreview: "",
            },
          },
          modelOrder: [
            `${modelId}#0`,
            `${modelId}#1`,
            `${modelId}#2`,
            `${modelId}#3`,
          ],
        },
      },
      roundOrder: [1],
    });
    render(<HivemindReviewLivePanel state={state} />);

    // First occurrence is rendered bare; subsequent occurrences get the
    // ` #N` suffix. There must be exactly one of each label.
    expect(screen.getAllByText(modelId).length).toBe(1);
    expect(screen.getByText(`${modelId} #2`)).toBeInTheDocument();
    expect(screen.getByText(`${modelId} #3`)).toBeInTheDocument();
    expect(screen.getByText(`${modelId} #4`)).toBeInTheDocument();
  });
});
