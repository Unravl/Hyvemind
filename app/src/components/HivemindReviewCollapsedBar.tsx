import { useEffect, useRef, useState } from "react";
import { I } from "./icons";
import { Pill } from "./atoms";
import type { ReviewState } from "../lib/hivemindReducer";
import { fmtElapsedSec } from "./HivemindReviewLivePanel";
import { loadMergedPlan } from "../lib/mergedPlanLoader";
import { MergedPlanModal } from "./MergedPlanModal";

interface HivemindReviewCollapsedBarProps {
  state: ReviewState;
  sourceLabel?: string;
  onExpand: () => void;
  /** When provided, the bar delegates merged-plan modal rendering to the
   *  parent screen (see the matching prop on HivemindReviewLivePanel for
   *  the rationale). The bar still owns its in-place loading / error
   *  affordances next to the Plan button. */
  onViewMergedPlan?: (args: { round: number; text: string }) => void;
}

export function HivemindReviewCollapsedBar({
  state,
  sourceLabel,
  onExpand,
  onViewMergedPlan,
}: HivemindReviewCollapsedBarProps) {
  // Defensive: should never mount while running (parent only renders this
  // at "collapsed" mode), but guard to prevent a malformed UI.
  // Hooks must be called before any early return; declare them up front and
  // let the early return happen after.
  const [mergedPlan, setMergedPlan] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const isMounted = useRef(true);
  useEffect(() => {
    isMounted.current = true;
    return () => {
      isMounted.current = false;
    };
  }, []);

  if (state.status === "running") return null;

  const label = sourceLabel ?? state.sourceLabel;
  const elapsed =
    state.startedAt != null
      ? fmtElapsedSec(
          Math.max(
            0,
            Math.floor(((state.endedAt ?? Date.now()) - state.startedAt) / 1000),
          ),
        )
      : null;

  // Mirror the phase-pill logic used in HivemindReviewLivePanel for terminal
  // states. Use matching text: "Cancelled by user" matches the live panel.
  const pill =
    state.status === "failed" ? (
      <Pill tone="red">Failed</Pill>
    ) : state.status === "cancelled" ? (
      <Pill tone="neutral">Cancelled by user</Pill>
    ) : state.status === "skipped" ? (
      <Pill tone="neutral">Skipped</Pill>
    ) : state.status === "completed" ? (
      <Pill tone="green">Complete</Pill>
    ) : (
      /* defensive fallback */
      <Pill tone="honey">Review</Pill>
    );

  // Count reviewers across the last round for a low-effort
  // "what happened" signal.
  const lastRoundNum =
    state.roundOrder.length > 0
      ? state.roundOrder[state.roundOrder.length - 1]
      : null;
  const lastRound =
    lastRoundNum != null ? state.rounds[lastRoundNum] : undefined;
  const modelCount = lastRound?.modelOrder.length ?? 0;

  // Only show the merged-plan button when the review actually completed AND
  // the last round's merge produced output. (A failed/cancelled review may
  // have never written the merge file.)
  const lastMergeCompleted =
    lastRoundNum != null && state.merges[lastRoundNum]?.status === "completed";
  const showMergedPlanBtn =
    state.status === "completed" && lastRoundNum != null && lastMergeCompleted;

  const handleViewMergedPlan = async () => {
    if (!state.jobId || lastRoundNum == null || loading) return;
    setLoading(true);
    setError(null);
    const result = await loadMergedPlan(state.jobId, lastRoundNum);
    if (!isMounted.current) return;
    setLoading(false);
    if (result) {
      if (onViewMergedPlan) {
        // Parent owns the modal — hand off the plan text instead of
        // managing the modal locally.
        onViewMergedPlan({ round: lastRoundNum, text: result });
      } else {
        setMergedPlan(result);
      }
    } else {
      setError("Not available");
      setTimeout(() => {
        if (isMounted.current) setError(null);
      }, 3000);
    }
  };

  return (
    <>
      {/* The outer row is a flex container, NOT a button \u2014 the expand
          target and the merged-plan trigger are sibling <button>s so we
          don't end up with a nested-interactive a11y violation. */}
      <div className="flex items-center gap-2 w-full">
        <button
          type="button"
          onClick={onExpand}
          aria-label="Expand Hivemind review panel"
          className="flex-1 flex items-center gap-2 text-left rounded-md px-1 py-0.5 hover:bg-ink-800/50 transition-colors focus:outline-none focus-visible:ring-2 focus-visible:ring-honey-500 focus-visible:ring-offset-2 focus-visible:ring-offset-ink-950"
        >
          {pill}
          {label && (
            <span className="text-[10.5px] text-honey-300/80 font-mono truncate max-w-[220px]">
              {label}
            </span>
          )}
          {modelCount > 0 && (
            <span className="text-[10.5px] font-mono text-dim shrink-0">
              {modelCount} model{modelCount === 1 ? "" : "s"}
            </span>
          )}
          {elapsed && (
            <span className="text-[10px] font-mono text-honey-300/60 shrink-0 tabular-nums">
              {elapsed}
            </span>
          )}
          <span className="ml-auto flex items-center gap-1 text-[10px] text-honey-300/70">
            Show details
            <span className="inline-flex rotate-180">{I.chevD({ size: 12 })}</span>
          </span>
        </button>

        {showMergedPlanBtn && (
          <div className="flex items-center gap-1.5 shrink-0">
            <button
              type="button"
              onClick={handleViewMergedPlan}
              disabled={loading}
              aria-label="View merged plan for final round"
              title="View merged plan for final round"
              className="inline-flex items-center gap-1 rounded px-1.5 py-0.5 text-[10.5px] font-medium text-honey-300/80 hover:text-honey-200 disabled:opacity-50 disabled:pointer-events-none transition-colors focus:outline-none focus-visible:ring-2 focus-visible:ring-honey-500 focus-visible:ring-offset-2 focus-visible:ring-offset-ink-950"
            >
              {loading ? (
                <span className="w-3 h-3 rounded-full border-2 border-honey-400/40 border-t-honey-400 animate-spin" />
              ) : (
                I.doc({ size: 11 })
              )}
              <span>{loading ? "Loading\u2026" : "Plan"}</span>
            </button>
            {error && (
              <span className="text-red-400/70 text-[10px]">{error}</span>
            )}
          </div>
        )}
      </div>

      {/* Legacy / standalone path: when no `onViewMergedPlan` callback is
          supplied, the bar manages the modal itself. */}
      {onViewMergedPlan == null && (
        <MergedPlanModal
          open={mergedPlan != null && lastRoundNum != null}
          title={
            lastRoundNum != null
              ? `Merged plan \u2014 Round ${lastRoundNum}`
              : "Merged plan"
          }
          subtitle={label}
          planText={mergedPlan ?? ""}
          onClose={() => {
            setMergedPlan(null);
            setLoading(false);
          }}
        />
      )}
    </>
  );
}
