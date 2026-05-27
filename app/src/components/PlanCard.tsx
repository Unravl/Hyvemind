import React, { useCallback } from "react";
import { renderMd } from "../App";
import { Btn } from "./atoms";
import { I } from "./icons";

interface PlanCardProps {
  planText: string;
  onImplement?: () => void;
  implementing?: boolean;
  autoMode?: boolean;
  onHivemindReview?: () => void;
  /** Set on a swarm-planning task: replaces the "Implement Plan" CTA with
   *  "Launch Swarm". Parses the FEATURES JSON from the plan and hands it to
   *  the backend's `start_swarm`. */
  onLaunchSwarm?: () => void;
  /** True while Launch Swarm is in flight (creating the run, starting the
   *  backend) — used to disable the button and show progress text. */
  launching?: boolean;
  /** Tooltip / disabled-reason for Launch Swarm. Set when the FEATURES
   *  block is missing or malformed so the user understands why the CTA is
   *  greyed out. */
  launchDisabledReason?: string;
  /** Recovery CTA: surfaced next to a disabled Launch Swarm when FEATURES
   *  are missing/malformed. Sends a follow-up to the planning agent asking
   *  it to re-emit the FEATURES JSON block for the current plan. */
  onRequestFeatures?: () => void;
  /** True while the re-emit follow-up is in flight. */
  requestingFeatures?: boolean;
  /** True while we're waiting on Queen to (re-)emit features after a
   *  Hivemind review or a user-initiated Re-emit retry. Drives the
   *  "Refining features…" amber pulse in the footer. */
  pendingFeaturesRefresh?: boolean;
  /** True when a features-refresh turn ended without Queen emitting
   *  `submit_features` (terminal event, watchdog, or dispatch failure).
   *  Surfaces the amber warning banner; the Launch Swarm button stays
   *  enabled so the user can ship with the current feature set. */
  featuresRefreshFailed?: boolean;
}

export const PlanCard = React.memo(({
  planText,
  onImplement,
  implementing,
  autoMode,
  onHivemindReview,
  onLaunchSwarm,
  launching,
  launchDisabledReason,
  onRequestFeatures,
  requestingFeatures,
  pendingFeaturesRefresh,
  featuresRefreshFailed,
}: PlanCardProps) => {
  const [copied, setCopied] = React.useState(false);
  const handleCopy = useCallback(() => {
    navigator.clipboard.writeText(planText).then(() => {
      setCopied(true);
      setTimeout(() => setCopied(false), 2000);
    }).catch(() => {});
  }, [planText]);
  return (
  <div className="rounded-xl border border-honey-500/30 bg-gradient-to-b from-ink-850 to-ink-900 shadow-[0_12px_40px_-12px_rgba(245,185,25,.25)] overflow-hidden">
    {/* Header */}
    <div className="px-4 h-11 border-b border-line flex items-center gap-2.5 bg-ink-800/60">
      <div className="w-6 h-6 rounded-md bg-honey-500/15 border border-honey-500/30 flex items-center justify-center">
        {I.doc({ size: 11, className: "text-honey-400" })}
      </div>
      <div className="text-[12.5px] font-semibold text-white">
        Implementation Plan
      </div>
      <div className="flex-1" />
      <span className="text-[10px] text-dim font-mono">
        {planText.length} chars
      </span>
      <button
        onClick={handleCopy}
        className="h-6 px-2 rounded-md flex items-center gap-1.5 text-[10.5px] font-medium text-dim hover:text-white hover:bg-ink-700/60 transition-colors"
        title="Copy plan to clipboard"
      >
        {copied
          ? I.check({ size: 11, className: "text-emerald-400" })
          : I.copy({ size: 11 })
        }
        <span>{copied ? "Copied" : "Copy"}</span>
      </button>
    </div>

    {/* Body — scrollable plan content */}
    <div className="p-4 max-h-[400px] overflow-auto">
      <div className="text-[13px] leading-relaxed text-white/90">
        {renderMd(planText, "assistant")}
      </div>
    </div>

    {/* Footer — extend the guard so the recovery banner / Re-emit button
        survive states where the other CTAs are all undefined (e.g., post-
        failure with onLaunchSwarm hidden). */}
    {(onImplement || onHivemindReview || onLaunchSwarm || onRequestFeatures || pendingFeaturesRefresh || featuresRefreshFailed) && !implementing && (
      <div className="px-4 h-12 border-t border-line bg-ink-800/40 flex items-center gap-2">
        {/* Status line — explicit priority branches so launchDisabledReason
            can't shadow the new wait/failure states. */}
        <div className="flex-1 text-[11px] text-dim flex items-center gap-1.5">
          {pendingFeaturesRefresh && !featuresRefreshFailed ? (
            <>
              <span className="w-2 h-2 rounded-full bg-honey-400 pulse-amber" />
              <span>Refining features…</span>
            </>
          ) : featuresRefreshFailed ? (
            <>
              <span className="w-2 h-2 rounded-full bg-honey-400" />
              <span className="text-honey-300">
                Queen didn't emit refined features — launch with the current feature set, or click Re-emit FEATURES.
              </span>
            </>
          ) : onLaunchSwarm ? (
            <>
              {I.check({ size: 11, className: "text-emerald-400" })}
              <span>{launchDisabledReason || "Plan ready — launch the swarm"}</span>
            </>
          ) : (
            <>
              {I.check({ size: 11, className: "text-emerald-400" })}
              <span>
                {autoMode ? "Auto-implementing in a moment…" : "Plan ready for implementation"}
              </span>
            </>
          )}
        </div>
        {onHivemindReview && !implementing && (
          <Btn
            kind="outline"
            size="md"
            icon={I.hex({ size: 13 })}
            onClick={onHivemindReview}
          >
            Hivemind Review
          </Btn>
        )}
        {/* Re-emit FEATURES — decoupled from onLaunchSwarm so it survives
            states where Launch is hidden. Visibility is controlled by the
            wrapper's showRequestFeatures gate. */}
        {onRequestFeatures && !launching && (
          <Btn
            kind="outline"
            size="md"
            onClick={onRequestFeatures}
            disabled={requestingFeatures}
            title="Ask the planning agent to re-emit the FEATURES JSON block"
          >
            {requestingFeatures ? "Re-emitting…" : "Re-emit FEATURES"}
          </Btn>
        )}
        {onLaunchSwarm && (
          <Btn
            kind="primary"
            size="md"
            icon={I.rocket({ size: 13 })}
            onClick={onLaunchSwarm}
            disabled={launching || !!launchDisabledReason}
            title={launchDisabledReason}
          >
            {launching ? "Launching..." : "Launch Swarm"}
          </Btn>
        )}
        {onImplement && !onLaunchSwarm && (
          <Btn
            kind="primary"
            size="md"
            icon={I.rocket({ size: 13 })}
            onClick={onImplement}
            disabled={implementing || autoMode}
          >
            {implementing ? "Starting..." : autoMode ? "Auto..." : "Implement Plan"}
          </Btn>
        )}
      </div>
    )}

    {implementing && (
      <div className="px-4 py-2 border-t border-line bg-honey-500/8 flex items-center gap-2">
        <span className="w-2 h-2 rounded-full bg-honey-400 pulse-amber" />
        <span className="text-[11px] text-honey-300 font-medium">Implementation in progress...</span>
      </div>
    )}

  </div>
  );
}, (prevProps, nextProps) => {
  return prevProps.planText === nextProps.planText &&
    prevProps.implementing === nextProps.implementing &&
    prevProps.launching === nextProps.launching &&
    prevProps.requestingFeatures === nextProps.requestingFeatures &&
    prevProps.launchDisabledReason === nextProps.launchDisabledReason &&
    prevProps.autoMode === nextProps.autoMode &&
    // New: drive the footer status-line branching; must be compared so
    // amber/warning states don't go stale on flag transitions.
    prevProps.pendingFeaturesRefresh === nextProps.pendingFeaturesRefresh &&
    prevProps.featuresRefreshFailed === nextProps.featuresRefreshFailed &&
    // New: callback identity drives button visibility (Re-emit hinges on
    // onRequestFeatures, etc.). The wrapper passes these conditionally,
    // so identity flips really do change rendering.
    prevProps.onImplement === nextProps.onImplement &&
    prevProps.onHivemindReview === nextProps.onHivemindReview &&
    prevProps.onLaunchSwarm === nextProps.onLaunchSwarm &&
    prevProps.onRequestFeatures === nextProps.onRequestFeatures;
});
