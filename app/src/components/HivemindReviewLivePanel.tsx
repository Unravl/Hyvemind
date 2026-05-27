import { useEffect, useRef, useState } from "react";
import type { ReviewState, ModelState } from "../lib/hivemindReducer";
import { I } from "./icons";
import { Pill } from "./atoms";
import { fmtTok2 } from "../lib/uiPrefs";
import { loadMergedPlan } from "../lib/mergedPlanLoader";
import { MergedPlanModal } from "./MergedPlanModal";

interface HivemindReviewLivePanelProps {
  state: ReviewState;
  sourceLabel?: string;
  compact?: boolean;
  onCancelReview?: () => void;
  onCollapse?: () => void;
  /** Configured total rounds for the *whole* multi-round review. Overrides
   *  the default of `state.roundOrder.length`, which only counts rounds
   *  that have streamed events so far. Tasks dispatches each round as a
   *  fresh `start_review` job, so without this override the panel resets
   *  to "Round 2/1" the moment round 2 begins. Defaults to the streamed
   *  count when omitted (Swarms / single-job reviews use that path). */
  totalRoundsOverride?: number;
  /** When provided, the panel delegates merged-plan modal rendering to the
   *  parent screen. The panel still owns the per-round loading / error
   *  affordances next to the trigger button, but on a successful
   *  `loadMergedPlan` call it invokes this callback instead of opening
   *  its own local modal. This decouples the modal's lifetime from the
   *  panel's so it survives parent re-mounts (e.g. the Tasks-view dock
   *  swapping the expanded panel for the collapsed bar 5s after
   *  completion). When omitted, the panel falls back to managing the
   *  modal locally — preserving standalone / legacy callers. */
  onViewMergedPlan?: (args: { round: number; text: string }) => void;
}

function computeTps(outputTokens?: number, durationMs?: number): number | null {
  if (!outputTokens || !durationMs || durationMs <= 0) return null;
  const tps = (outputTokens / durationMs) * 1000;
  if (!isFinite(tps) || tps <= 0) return null;
  return Math.round(tps);
}

function fmtDuration(ms?: number): string | null {
  if (ms == null || ms <= 0) return null;
  const sec = ms / 1000;
  if (sec < 60) return `${Math.round(sec)}s`;
  const m = Math.floor(sec / 60);
  const s = Math.floor(sec % 60);
  return `${m}m ${s.toString().padStart(2, "0")}s`;
}

export function fmtElapsedSec(s: number): string {
  if (s < 60) return `${s}s`;
  const m = Math.floor(s / 60);
  const rem = s % 60;
  return `${m}m ${rem.toString().padStart(2, "0")}s`;
}

function fmtElapsed(startedAt: number | undefined, endedAt?: number): string | null {
  if (startedAt == null) return null;
  const end = endedAt ?? Date.now();
  const sec = Math.max(0, Math.floor((end - startedAt) / 1000));
  return fmtElapsedSec(sec);
}

/**
 * Displays a live-updating elapsed time without triggering React re-renders.
 *
 * Uses requestAnimationFrame + direct DOM textContent writes (throttled to ~1Hz)
 * so the parent panel doesn't need to re-render every second.
 *
 * If `endedAt` is provided, the timer is frozen and rendered statically.
 */
interface ElapsedTimeProps {
  startedAt: number | undefined;
  endedAt?: number;
  className?: string;
}

function ElapsedTime({ startedAt, endedAt, className }: ElapsedTimeProps) {
  const ref = useRef<HTMLSpanElement | null>(null);

  useEffect(() => {
    if (startedAt == null) return;
    // If the timer is frozen (endedAt set), no animation loop needed; the
    // initial render below already shows the final value.
    if (endedAt != null) return;

    let raf = 0;
    let lastUpdate = 0;

    const loop = (now: number) => {
      // Throttle to ~1Hz: only touch the DOM if at least 1s has elapsed.
      if (now - lastUpdate >= 1000) {
        lastUpdate = now;
        const text = fmtElapsed(startedAt, undefined);
        if (ref.current && text != null) {
          ref.current.textContent = text;
        }
      }
      raf = requestAnimationFrame(loop);
    };

    raf = requestAnimationFrame(loop);
    return () => {
      cancelAnimationFrame(raf);
    };
  }, [startedAt, endedAt]);

  const initial = fmtElapsed(startedAt, endedAt);
  if (initial == null) return null;
  return (
    <span ref={ref} className={className}>
      {initial}
    </span>
  );
}

function renderModelRow(
  m: ModelState,
  runStartedAt: number,
  compact: boolean,
  label: string,
) {
  const rowPad = compact ? "px-2.5 py-1" : "px-3 py-1.5";
  const textSize = compact ? "text-[11px]" : "text-[12px]";

  return (
    <div
      key={m.instanceKey}
      className={`flex items-center gap-3 ${rowPad} ${textSize}`}
    >
      <div className="flex items-center gap-1.5 flex-1 min-w-[60px]">
        <span className="w-4 h-4 shrink-0 flex items-center justify-center">
          {m.status === "completed" && I.check({ size: 14, className: "text-emerald-400" })}
          {m.status === "failed" && I.x({ size: 14, className: "text-red-400" })}
          {m.status === "streaming" && (
            <span className="w-3 h-3 rounded-full border-2 border-honey-400/40 border-t-honey-400 animate-spin" />
          )}
        </span>

        <span className="font-mono text-white/85 truncate">
          {label}
        </span>
      </div>

      {m.status === "failed" ? (
        <span className="text-[10.5px] font-mono text-red-400 truncate max-w-[320px]">
          {m.errorMessage || "failed"}
        </span>
      ) : m.status === "completed" ? (
        <div className="flex items-center gap-1.5 text-[10.5px] font-mono shrink-0 tabular-nums">
          <span className="text-blue-300" title="output tokens">
            &darr;{fmtTok2(m.outputTokens ?? 0)}
          </span>
          {(() => {
            const tps = computeTps(m.outputTokens, m.durationMs);
            return tps != null ? <span className="text-emerald-300">{tps} t/s</span> : null;
          })()}
          {(() => {
            const dur = fmtDuration(m.durationMs);
            return dur != null ? <span className="text-honey-300/90">{dur}</span> : null;
          })()}
        </div>
      ) : (
        <div className="flex items-center gap-1 text-[10.5px] font-mono shrink-0 tabular-nums">
          <ElapsedTime
            startedAt={runStartedAt}
            className="text-amber-300/70"
          />
        </div>
      )}
    </div>
  );
}

export function HivemindReviewLivePanel({
  state,
  sourceLabel,
  compact = false,
  onCancelReview,
  onCollapse,
  totalRoundsOverride,
  onViewMergedPlan,
}: HivemindReviewLivePanelProps) {
  const rounds = state.roundOrder;
  const currentRound = rounds.length > 0 ? rounds[rounds.length - 1] : 1;
  // Prefer the configured total (the Tasks view passes
  // `reviewProgress.totalRounds`); fall back to streamed-round count for
  // single-job callers like SwarmControl that don't carry that out-of-band.
  // Clamp `Round X/Y` to never show `currentRound > Y` (otherwise the user
  // sees "Round 2/1" between round 1's merge and round 2's first event).
  const totalRounds = Math.max(
    totalRoundsOverride ?? rounds.length,
    currentRound,
  );

  // Round-tab selection. `null` means "track the live round" — derived
  // `activeRound` falls back to the last entry in `roundOrder`, so the
  // tab auto-advances when a new round starts. Clicking a tab pins
  // selection to that round; pinning sticks until the user clicks
  // another tab (we don't reset on new rounds, which would yank them
  // away from a round they're inspecting).
  const [selectedRound, setSelectedRound] = useState<number | null>(null);
  const activeRound = selectedRound ?? currentRound;
  const activeRoundData = state.rounds[activeRound];

  // ── Merged plan modal state ──
  // `mergedPlan` carries the round AND the extracted plan text shown in the
  // modal (null = closed). The round is stored alongside the text so the
  // modal title stays correct even if the underlying panel auto-advances
  // (`activeRound` change from a live event or a parent re-render with a
  // different feature's state). The modal owns its own lifecycle — the user
  // closes it explicitly via the X button, Escape, or backdrop click; we
  // deliberately do NOT auto-close it when `activeRound` changes, since
  // that was the source of the "modal flashes and disappears" bug.
  // `loadingRound` / `errorRound` track per-round IPC state so the user can
  // see which row is fetching / errored when multiple rounds exist.
  // `isMounted` guards against stale state updates after unmount when a
  // slow `loadMergedPlan` promise resolves after the panel is gone.
  const [mergedPlan, setMergedPlan] = useState<{ round: number; text: string } | null>(null);
  const [loadingRound, setLoadingRound] = useState<number | null>(null);
  const [errorRound, setErrorRound] = useState<number | null>(null);
  const isMounted = useRef(true);
  useEffect(() => {
    isMounted.current = true;
    return () => {
      isMounted.current = false;
    };
  }, []);

  const handleViewMergedPlan = async () => {
    if (!state.jobId || loadingRound === activeRound) return;
    const targetRound = activeRound;
    setLoadingRound(targetRound);
    setErrorRound(null);
    const result = await loadMergedPlan(state.jobId, targetRound);
    if (!isMounted.current) return;
    setLoadingRound(null);
    if (result) {
      if (onViewMergedPlan) {
        // Parent owns the modal — hand off the plan text so the modal's
        // lifetime is decoupled from this panel's mount/unmount cycle.
        onViewMergedPlan({ round: targetRound, text: result });
      } else {
        setMergedPlan({ round: targetRound, text: result });
      }
    } else {
      setErrorRound(targetRound);
      setTimeout(() => {
        if (isMounted.current) setErrorRound(null);
      }, 3000);
    }
  };

  const showRows =
    rounds.length > 0 &&
    (state.phase === "round" ||
      state.phase === "merge" ||
      state.phase === "between_rounds" ||
      state.phase === "completed");
  const label = sourceLabel ?? state.sourceLabel;

  // a11y: build a single, debounced phase/round completion announcement.
  // Each round's completion-model count is included so SR users hear
  // "Round 2 of 3 — 4 of 4 models complete" rather than per-token spam.
  const announcement = (() => {
    if (state.status === "failed") return `Hivemind review failed${state.message ? `: ${state.message}` : ""}`;
    if (state.status === "completed") return "Hivemind review complete";
    if (state.status === "skipped") return "Hivemind review skipped";
    if (state.phase === "context") return "Gathering review context";
    if (state.phase === "merge" && rounds.length > 0) return `Merging round ${currentRound} feedback`;
    if (state.phase === "between_rounds" && rounds.length > 0)
      return `Round ${currentRound} merged, preparing next round`;
    if (state.phase === "round" && rounds.length > 0) {
      const rd = state.rounds[currentRound];
      if (rd) {
        const done = rd.modelOrder
          .map((id) => rd.models[id])
          .filter((m) => m && (m.status === "completed" || m.status === "failed")).length;
        const total = rd.modelOrder.length;
        return `Round ${currentRound} of ${totalRounds}: ${done} of ${total} models complete`;
      }
      return `Round ${currentRound} of ${totalRounds} started`;
    }
    return "";
  })();

  const phasePill = (() => {
    if (state.status === "failed") return <Pill tone="red">Failed</Pill>;
    // Cancellation is user intent — render a neutral/amber pill, never red.
    // `Cancelled by user` matches the language used in IPC error logs.
    if (state.status === "cancelled")
      return <Pill tone="neutral">Cancelled by user</Pill>;
    if (state.status === "skipped") return <Pill tone="neutral">Skipped</Pill>;
    if (state.status === "completed") return <Pill tone="green">Complete</Pill>;
    if (state.phase === "context") return <Pill tone="honey">Gathering Context</Pill>;
    if (state.phase === "round" && rounds.length > 0)
      return <Pill tone="honey">Round {currentRound}/{totalRounds}</Pill>;
    if (state.phase === "merge" && rounds.length > 0)
      return <Pill tone="honey">Merging R{currentRound}</Pill>;
    // `between_rounds` fires the instant the backend emits
    // `merge_completed` for the current round. The user-visible label
    // reads the just-merged round number; the next `round_started`
    // event flips this to `Round {N+1}/{total}`. On the final round,
    // `completed` takes precedence and renders the green Complete pill.
    if (state.phase === "between_rounds" && rounds.length > 0)
      return <Pill tone="honey">Round {currentRound} merged</Pill>;
    return null;
  })();

  const isRunning = state.status === "running";

  const rowsContainerClass = compact
    ? "-mx-3 border-y border-honey-500/15 bg-ink-950/40 divide-y divide-line/40"
    : "rounded-md border border-honey-500/15 bg-ink-950/40 divide-y divide-line/40";

  return (
    <div className={`flex flex-col ${compact ? "gap-2" : "gap-2.5"}`}>
      {/*
       * a11y: visually-hidden polite live region announces phase + round
       * progression. Polite (queues, never interrupts) and atomic (each
       * update is read as a whole sentence). The text changes only on
       * round/phase transitions and on per-model completion, so SR is not
       * flooded with chunk-level updates. Tailwind's `sr-only` keeps it
       * invisible but readable by AT.
       */}
      <span className="sr-only" aria-live="polite" aria-atomic="true">
        {announcement}
      </span>
      <div className="flex items-center gap-2 flex-wrap">
        {phasePill}
        {label && (
          <span className="text-[10.5px] text-honey-300/80 font-mono truncate max-w-[220px]">
            {label}
          </span>
        )}
        {!compact && state.jobId && (
          <button
            className="text-[10px] font-mono text-honey-300/70 hover:text-honey-200 transition-colors shrink-0"
            onClick={() => navigator.clipboard.writeText(state.jobId)}
            title="Copy review ID"
          >
            {state.jobId}
          </button>
        )}
        {state.startedAt != null && (
          <ElapsedTime
            startedAt={state.startedAt}
            endedAt={state.endedAt}
            className="text-[10px] font-mono text-honey-300/60 shrink-0 tabular-nums ml-auto"
          />
        )}
        {!compact && isRunning && (
          <span className="w-2 h-2 rounded-full bg-honey-400 pulse-amber shrink-0" />
        )}
        {onCancelReview && isRunning && (
          <button
            className="text-[10px] text-red-400/70 hover:text-red-300 transition-colors shrink-0"
            onClick={onCancelReview}
            title="Cancel review"
          >
            × Cancel
          </button>
        )}
        {onCollapse && state.status !== "running" && (
          <button
            className="text-[10px] text-honey-300/70 hover:text-honey-200 transition-colors shrink-0"
            onClick={onCollapse}
            aria-label="Collapse Hivemind review panel"
            title="Collapse"
          >
            {I.chevD({ size: 12 })}
          </button>
        )}
      </div>

      {rounds.length > 1 && (
        <div role="tablist" aria-label="Review rounds" className="flex items-center gap-1 flex-wrap">
          {rounds.map((r) => {
            const isActive = r === activeRound;
            return (
              <button
                key={r}
                id={`hivemind-round-tab-${r}`}
                role="tab"
                aria-selected={isActive}
                aria-controls={`hivemind-round-panel-${r}`}
                onClick={() => setSelectedRound(r)}
                className={`px-2 py-0.5 rounded text-[10.5px] font-mono leading-tight transition-colors ${
                  isActive
                    ? "bg-honey-500/15 text-honey-200 border border-honey-500/35"
                    : "bg-ink-700 text-slate-300 border border-line hover:bg-ink-600"
                }`}
              >
                R{r}
              </button>
            );
          })}
        </div>
      )}

      {state.message && !isRunning && (
        <div className="text-[11.5px] text-dim leading-snug">{state.message}</div>
      )}

      {showRows && activeRoundData && activeRoundData.modelOrder.length > 0 && (
        /*
         * a11y: per-model rows update as models complete (status flips,
         * token counts arrive). Polite + non-atomic so SR announces only
         * the changed row content, not the whole list. The sr-only
         * announcement above carries the round-level summary.
         */
        <div
          id={`hivemind-round-panel-${activeRound}`}
          role={rounds.length > 1 ? "tabpanel" : undefined}
          aria-labelledby={rounds.length > 1 ? `hivemind-round-tab-${activeRound}` : undefined}
          className={rowsContainerClass}
          aria-live="polite"
          aria-atomic="false"
        >
          {(() => {
            const models = activeRoundData.modelOrder.map((k) => activeRoundData.models[k]);
            const totalIn = models.reduce((s, m) => s + (m.inputTokens ?? 0), 0);
            const avgIn = models.length > 0 ? totalIn / models.length : 0;
            const merge = state.merges[activeRound];
            const showMergeButton = merge?.status === "completed";
            const isLoading = loadingRound === activeRound;
            const hasError = errorRound === activeRound;
            if (avgIn <= 0 && !showMergeButton) return null;
            return (
              <div className="text-[10px] uppercase tracking-wider text-honey-300/70 font-mono px-2.5 pt-2 pb-0.5 flex items-center gap-1.5">
                {avgIn > 0 && (
                  <>
                    <span>R{activeRound}</span>
                    <span className="text-dim">&middot;</span>
                    <span className="text-emerald-300">&uarr;{fmtTok2(avgIn)}</span>
                  </>
                )}
                {showMergeButton && (
                  <div className="ml-auto flex items-center gap-2">
                    {hasError && (
                      <span className="text-red-400/80 text-[10.5px] normal-case tracking-normal">
                        Merge output not available
                      </span>
                    )}
                    <button
                      type="button"
                      onClick={handleViewMergedPlan}
                      disabled={isLoading}
                      aria-label={`View merged plan for round ${activeRound}`}
                      title={`View merged plan for round ${activeRound}`}
                      className="inline-flex items-center gap-1 rounded text-[10.5px] font-medium text-honey-300/80 hover:text-honey-200 disabled:opacity-50 disabled:pointer-events-none transition-colors focus:outline-none focus-visible:ring-2 focus-visible:ring-honey-500 focus-visible:ring-offset-2 focus-visible:ring-offset-ink-900 px-1.5 py-0.5"
                    >
                      {isLoading ? (
                        <span className="w-3 h-3 rounded-full border-2 border-honey-400/40 border-t-honey-400 animate-spin" />
                      ) : (
                        I.doc({ size: 11 })
                      )}
                      {!compact && (
                        <span className="normal-case tracking-normal">{isLoading ? "Loading…" : "View merged plan"}</span>
                      )}
                    </button>
                  </div>
                )}
              </div>
            );
          })()}
          {(() => {
            // Build per-row display labels using the same
            // "first-occurrence bare, then #2, #3" convention as
            // `dedupeReviewerLabels` (review-mode.ts). This keeps the live
            // SwarmControl panel visually aligned with ReviewHistory when
            // the user configured duplicate reviewer instances of the
            // same `model_id` (e.g. four calls with different
            // temperatures), without changing the underlying instance
            // keys used for state.
            const models = activeRoundData.modelOrder.map(
              (k) => activeRoundData.models[k],
            );
            const counts = new Map<string, number>();
            for (const m of models) {
              counts.set(m.modelId, (counts.get(m.modelId) ?? 0) + 1);
            }
            const seenSoFar = new Map<string, number>();
            const labels = models.map((m) => {
              if ((counts.get(m.modelId) ?? 0) <= 1) return m.modelId;
              const n = (seenSoFar.get(m.modelId) ?? 0) + 1;
              seenSoFar.set(m.modelId, n);
              return n === 1 ? m.modelId : `${m.modelId} #${n}`;
            });
            return models.map((m, i) =>
              renderModelRow(m, state.startedAt, compact, labels[i]),
            );
          })()}
        </div>
      )}

      {state.phase === "merge" && rounds.length > 0 && (
        <div className="text-[11px] font-mono text-honey-300/70 px-1 flex items-center gap-2">
          <span className="w-3 h-3 rounded-full border-2 border-honey-400/40 border-t-honey-400 animate-spin shrink-0" />
          Synthesising reviewer feedback into the next plan…
        </div>
      )}

      {/* Legacy / standalone path: when no `onViewMergedPlan` callback is
          supplied, the panel owns the modal itself. The Tasks screen and
          SwarmControl now opt into the parent-managed path so the modal
          survives dock-mode swaps and other parent re-mounts. */}
      {onViewMergedPlan == null && (
        <MergedPlanModal
          open={mergedPlan != null}
          title={`Merged plan — Round ${mergedPlan?.round ?? activeRound}`}
          subtitle={label}
          planText={mergedPlan?.text ?? ""}
          onClose={() => {
            setMergedPlan(null);
            setLoadingRound(null);
          }}
        />
      )}

      {state.phase === "context" && (
        <div className="text-[11px] font-mono text-honey-300/70 px-1 flex items-center gap-2">
          <span className="w-3 h-3 rounded-full border-2 border-honey-400/40 border-t-honey-400 animate-spin shrink-0" />
          Reading the plan and gathering source context…
        </div>
      )}

      {!showRows && state.phase !== "context" && state.phase !== "merge" && isRunning && (
        <div className="text-[11px] font-mono text-honey-300/70 px-1">
          Waiting for model data…
        </div>
      )}
    </div>
  );
}
