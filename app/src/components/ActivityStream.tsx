import React from "react";
import {
  InlineReasoningIndicator,
  ReasoningBlock,
  RelativeTime,
  ToolCallGroup,
} from "./atoms";
import { Markdown } from "./Markdown";
import { PlanCard } from "./PlanCard";
import { I } from "./icons";
import { fmtTok2 } from "../lib/uiPrefs";
import { type ParsedVerdict } from "../lib/review-mode";
import { hasHandoffStart, hasCompleteHandoff } from "../lib/handoff";
import {
  displayTextOf,
  type ActiveSession,
  type ChatBubbleEntry,
  type CompleteEntry,
  type ErrorEntry,
  type NurseEntry,
  type PlanEntry,
  type SessionMarkerEntry,
  type StreamAgent,
  type StreamEntry,
} from "../lib/streamEntry";
import { NurseMessage } from "./NurseMessage";
import {
  computeReasoningRenderPlan,
  hasBreakerAfter,
} from "../lib/streamReasoningMerge";
const AGENT_TONE: Record<StreamAgent, { dot: string; text: string; border: string; bg: string }> = {
  scout: {
    dot: "bg-blue-400",
    text: "text-blue-200",
    border: "border-blue-500/30",
    bg: "bg-blue-500/10",
  },
  worker: {
    dot: "bg-honey-400",
    text: "text-honey-200",
    border: "border-honey-500/30",
    bg: "bg-honey-500/10",
  },
  guard: {
    dot: "bg-emerald-400",
    text: "text-emerald-200",
    border: "border-emerald-500/30",
    bg: "bg-emerald-500/10",
  },
  "hivemind-context": {
    dot: "bg-honey-300",
    text: "text-honey-100",
    border: "border-honey-400/40",
    bg: "bg-honey-400/10",
  },
  planning: {
    dot: "bg-blue-400",
    text: "text-blue-200",
    border: "border-blue-500/30",
    bg: "bg-blue-500/10",
  },
  implementation: {
    dot: "bg-honey-400",
    text: "text-honey-200",
    border: "border-honey-500/30",
    bg: "bg-honey-500/10",
  },
  "hivemind-merge": {
    dot: "bg-purple-400",
    text: "text-purple-200",
    border: "border-purple-500/30",
    bg: "bg-purple-500/10",
  },
};

const AGENT_LABEL: Record<StreamAgent, string> = {
  scout: "scout",
  worker: "worker",
  guard: "guard",
  "hivemind-context": "hivemind ctx",
  planning: "planning",
  implementation: "implementation",
  "hivemind-merge": "hivemind merge",
};

const DEFAULT_TAIL_LIMIT = 300;

function shortClockTime(iso: string): string {
  try {
    return new Date(iso).toLocaleTimeString([], {
      hour: "2-digit",
      minute: "2-digit",
      second: "2-digit",
    });
  } catch {
    return "";
  }
}

function formatMmSs(ms: number): string {
  const totalSec = Math.max(0, Math.floor(ms / 1000));
  const m = Math.floor(totalSec / 60);
  const s = totalSec % 60;
  return `${m}:${s.toString().padStart(2, "0")}`;
}

function useRafThrottled(value: string, active: boolean): string {
  const [throttled, setThrottled] = React.useState(value);
  const pendingRef = React.useRef<string | null>(null);
  const rafRef = React.useRef<number | null>(null);

  React.useEffect(() => {
    if (!active) {
      if (rafRef.current !== null) {
        cancelAnimationFrame(rafRef.current);
        rafRef.current = null;
      }
      pendingRef.current = null;
      setThrottled(value);
      return;
    }
    pendingRef.current = value;
    if (rafRef.current !== null) return;
    rafRef.current = requestAnimationFrame(() => {
      rafRef.current = null;
      const next = pendingRef.current;
      pendingRef.current = null;
      if (next !== null) setThrottled(next);
    });
  }, [value, active]);

  React.useEffect(() => {
    return () => {
      if (rafRef.current !== null) {
        cancelAnimationFrame(rafRef.current);
        rafRef.current = null;
      }
    };
  }, []);

  return throttled;
}

interface MergedReasoning {
  text: string;
  durationMs?: number;
  tailIsLastEntry: boolean;
  tailHasText: boolean;
  /** Epoch-ms of the leader (first) reasoning entry in the merged run —
   *  drives the live-updating label in the shared ReasoningBlock header. */
  createdAt?: number;
}

function MergeScoringPill({
  verdicts: verdictsProp,
  streaming,
}: {
  verdicts?: ParsedVerdict[] | null;
  streaming: boolean;
}) {
  const [expanded, setExpanded] = React.useState(false);
  const verdicts: ParsedVerdict[] = React.useMemo(
    () => (streaming ? [] : verdictsProp ?? []),
    [verdictsProp, streaming],
  );

  const verdictGlyph = (v: ParsedVerdict["verdict"]) => {
    if (v === "accepted") return { icon: I.check({ size: 11, className: "text-emerald-400" }), label: "accepted", cls: "text-emerald-300" };
    if (v === "rejected") return { icon: I.x({ size: 11, className: "text-red-400" }), label: "rejected", cls: "text-red-300" };
    return { icon: <span className="text-amber-300 text-[12px] leading-none">↻</span>, label: "modified", cls: "text-amber-300" };
  };

  const sevCls = (s: number | null) => {
    if (s == null) return "text-dim";
    if (s >= 5) return "text-red-300";
    if (s >= 4) return "text-amber-300";
    return "text-dim";
  };

  return (
    <div className="rounded-lg border border-honey-500/25 bg-honey-500/5 overflow-hidden my-1.5">
      <button
        onClick={() => !streaming && setExpanded((v) => !v)}
        disabled={streaming}
        className={`w-full px-3 py-2 flex items-center gap-2 text-[12px] transition-colors ${
          streaming ? "cursor-default" : "hover:bg-honey-500/10"
        }`}
      >
        {streaming ? (
          <span className="w-3 h-3 rounded-full border-2 border-honey-400 border-t-transparent animate-spin shrink-0" />
        ) : (
          I.check({ size: 12, className: "text-honey-400 shrink-0" })
        )}
        <span className="font-mono text-honey-300 font-medium">
          {streaming ? "Scoring models…" : "Models scored"}
        </span>
        {!streaming && verdicts.length > 0 && (
          <span className="text-[10.5px] text-honey-400/60 font-mono">
            {verdicts.length} {verdicts.length === 1 ? "decision" : "decisions"}
          </span>
        )}
        {!streaming && (
          <>
            <span className="flex-1" />
            <span className="text-dim text-[10.5px]">{expanded ? "collapse" : "expand"}</span>
            <span className={`text-dim transition-transform ${expanded ? "rotate-180" : ""}`}>
              {I.chevD({ size: 11 })}
            </span>
          </>
        )}
      </button>
      {expanded && !streaming && (
        <div className="px-3 pb-2 border-t border-honey-500/15 max-h-64 overflow-auto">
          {verdicts.length === 0 ? (
            <div className="text-[12px] text-dim font-mono py-2">No verdicts parsed</div>
          ) : (
            <div className="flex flex-col gap-2 mt-2">
              {verdicts.map((v, i) => {
                const g = verdictGlyph(v.verdict);
                return (
                  <div key={i} className="text-[12px] leading-relaxed">
                    <div className="flex items-center gap-1.5 flex-wrap">
                      <span className="shrink-0">{g.icon}</span>
                      <span className={`font-mono text-[10.5px] ${g.cls}`}>{g.label}</span>
                      {v.severity != null && (
                        <span className={`font-mono text-[10.5px] ${sevCls(v.severity)}`}>S{v.severity}</span>
                      )}
                      {v.best_find && (
                        <span className="text-honey-400 text-[11px] leading-none" title="Best find of round">★</span>
                      )}
                      <span className="font-mono text-[10.5px] text-blue-300/80 truncate">{v.reviewer_model}</span>
                    </div>
                    <div className="text-white/85 mt-0.5">{v.suggestion}</div>
                    {v.reason && (
                      <div className="text-dim text-[11px] mt-0.5 pl-3 border-l border-honey-500/15">{v.reason}</div>
                    )}
                  </div>
                );
              })}
            </div>
          )}
        </div>
      )}
    </div>
  );
}

function SessionMarker({
  entry,
  hasMatchingEnd,
}: {
  entry: SessionMarkerEntry;
  hasMatchingEnd: boolean;
}) {
  if (entry.agent) {
    const tone = AGENT_TONE[entry.agent];
    const running = entry.phase === "start" && !hasMatchingEnd;
    const isEnded = entry.phase === "end";
    const endedTextOnly = running ? "running…" : "";
    const defaultLabel = AGENT_LABEL[entry.agent];
    const showSubLabel = !!entry.label && entry.label !== defaultLabel;
    return (
      <div className="my-4">
        <div className="flex items-center gap-3">
          <div className="flex-1 h-px bg-line" />
          <div className={`flex items-center gap-2 px-2.5 py-1 rounded-md border ${tone.border} ${tone.bg}`}>
            <span className={`w-1.5 h-1.5 rounded-full ${tone.dot} ${running ? "animate-pulse" : ""}`} />
            <span className={`text-[11px] font-mono font-medium uppercase tracking-wider ${tone.text}`}>
              {defaultLabel ?? entry.agent}
            </span>
            {showSubLabel && (
              <span className="text-dim text-[10.5px] font-mono">· {entry.label}</span>
            )}
            {entry.featureId && (
              <span className="text-dim text-[10.5px] font-mono">· {entry.featureId}</span>
            )}
            {entry.model && (
              <span className="text-dim text-[10.5px] font-mono">· pi: {entry.model}</span>
            )}
            {!isEnded && !running && (entry.createdAt != null || entry.t) && (
              <span className="text-dim text-[10.5px] font-mono">
                · <RelativeTime createdAt={entry.createdAt} fallback={entry.t} />
              </span>
            )}
            {isEnded && (
              <span
                className={`text-[10.5px] font-mono ${entry.success ? "text-emerald-300/70" : "text-red-300/70"}`}
              >
                · ended{" "}
                <RelativeTime
                  createdAt={entry.createdAt}
                  fallback={entry.t}
                />
                {entry.success ? " ✓" : " ✗"}
              </span>
            )}
            {running && (
              <span className="text-dim text-[10.5px] font-mono">· {endedTextOnly}</span>
            )}
          </div>
          <div className="flex-1 h-px bg-line" />
        </div>
        {entry.usage && (
          <div className="mt-2 flex items-center justify-center gap-4 text-[10.5px] text-muted font-mono">
            {entry.sessionId && entry.phase === "end" && (
              <span className="text-dim">{entry.sessionId.slice(0, 8)}</span>
            )}
            <span>Context {Math.round(entry.usage.contextPercent)}%</span>
            <span className="text-emerald-300">↑{fmtTok2(entry.usage.input)}</span>
            <span className="text-blue-300">↓{fmtTok2(entry.usage.output)}</span>
            {(entry.usage.tokPerSec ?? 0) > 0 && (
              <span className="text-amber-300">{entry.usage.tokPerSec} t/s</span>
            )}
            {entry.usage.cost > 0 && (
              <span className="text-honey-400">${entry.usage.cost.toFixed(4)}</span>
            )}
          </div>
        )}
      </div>
    );
  }

  return (
    <div className="my-6">
      <div className="flex items-center gap-3">
        <div className="flex-1 h-px bg-line" />
        <span className="text-[10.5px] text-dim font-medium uppercase tracking-wider">
          {entry.label || (entry.phase === "end" ? "Planning session ended" : "Session started")}
        </span>
        <div className="flex-1 h-px bg-line" />
      </div>
      {entry.phase === "start" && (entry.model || entry.agentModel || entry.thinking || entry.sessionId) && (
        <div className="mt-1.5 flex items-center justify-center gap-4 text-[10.5px] text-muted font-mono">
          {entry.model && <span className="text-blue-300">{entry.model}</span>}
          {entry.agentModel && <span className="text-purple-300">{entry.agentModel}</span>}
          {entry.thinking && <span className="text-amber-300">thinking: {entry.thinking}</span>}
          {entry.sessionId && <span className="text-dim">{entry.sessionId.slice(0, 8)}</span>}
        </div>
      )}
      {entry.usage && (
        <div className="mt-2 flex items-center justify-center gap-4 text-[10.5px] text-muted font-mono">
          {entry.sessionId && entry.phase === "end" && (
            <span className="text-dim">{entry.sessionId.slice(0, 8)}</span>
          )}
          <span>Context {Math.round(entry.usage.contextPercent)}%</span>
          <span className="text-emerald-300">↑{fmtTok2(entry.usage.input)}</span>
          <span className="text-blue-300">↓{fmtTok2(entry.usage.output)}</span>
          {(entry.usage.tokPerSec ?? 0) > 0 && (
            <span className="text-amber-300">{entry.usage.tokPerSec} t/s</span>
          )}
          {entry.usage.cost > 0 && (
            <span className="text-honey-400">${entry.usage.cost.toFixed(4)}</span>
          )}
        </div>
      )}
    </div>
  );
}

function ErrorRow({ entry }: { entry: ErrorEntry }) {
  return (
    // a11y: inline errors are transient and important. `role="alert"` implies
    // `aria-live="assertive"` + `aria-atomic="true"` so the SR interrupts and
    // reads the full row when it appears.
    <div
      role="alert"
      aria-live="assertive"
      className="flex items-start gap-2 my-1.5 px-2 py-1.5 rounded-md border border-red-500/30 bg-red-500/10 text-[11px] text-red-200 font-mono"
    >
      <span className="w-1.5 h-1.5 rounded-full bg-red-400 shrink-0 mt-[5px]" />
      {entry.agent && (
        <span className="uppercase text-red-300/80 shrink-0">{entry.agent}</span>
      )}
      {entry.featureId && <span className="text-dim shrink-0">{entry.featureId}</span>}
      {(entry.createdAt != null || entry.t) && (
        <RelativeTime
          createdAt={entry.createdAt}
          fallback={entry.t}
          className="text-dim shrink-0"
        />
      )}
      <span className="text-red-200/90 break-words whitespace-pre-wrap min-w-0 flex-1">
        {entry.message}
      </span>
    </div>
  );
}

function CompleteChip({ entry }: { entry: CompleteEntry }) {
  const state = entry.successState;
  // Default (and explicit success) keeps the long-standing emerald styling.
  // Partial → amber. Failure → red. Legacy entries without a successState
  // render as the original emerald chip.
  const palette =
    state === "failure"
      ? {
          border: "border-rose-500/30",
          bg: "bg-rose-500/10",
          icon: "text-rose-400",
          label: "text-rose-200",
          summary: "text-rose-200/80",
          text: "Task Failed",
        }
      : state === "partial"
      ? {
          border: "border-amber-500/30",
          bg: "bg-amber-500/10",
          icon: "text-amber-400",
          label: "text-amber-200",
          summary: "text-amber-200/80",
          text: "Task Partially Complete",
        }
      : {
          border: "border-emerald-500/30",
          bg: "bg-emerald-500/10",
          icon: "text-emerald-400",
          label: "text-emerald-200",
          summary: "text-emerald-200/80",
          text: "Task Complete",
        };
  const summary = entry.text?.trim();
  return (
    <div className="flex flex-col items-center py-2 gap-1">
      <div
        className={`flex items-center gap-2.5 px-5 py-2.5 rounded-full border ${palette.border} ${palette.bg}`}
      >
        {I.check({ size: 14, className: palette.icon })}
        <span className={`text-[13px] font-semibold ${palette.label}`}>
          {palette.text}
        </span>
      </div>
      {summary ? (
        <div className={`text-[12px] ${palette.summary} max-w-xl text-center px-3`}>
          {summary}
        </div>
      ) : null}
    </div>
  );
}

function PlanCardWrapper({
  entry,
  onImplementPlan,
  onHivemindReview,
  onLaunchSwarm,
  onRequestFeatures,
  planCard,
}: {
  entry: PlanEntry;
  onImplementPlan?: () => void;
  onHivemindReview?: () => void;
  onLaunchSwarm?: () => void;
  onRequestFeatures?: () => void;
  planCard?: {
    implementing?: boolean;
    autoMode?: boolean;
    launching?: boolean;
    launchDisabledReason?: string;
    showImplement?: boolean;
    showLaunchSwarm?: boolean;
    showHivemindReview?: boolean;
    showRequestFeatures?: boolean;
    requestingFeatures?: boolean;
    pendingFeaturesRefresh?: boolean;
    featuresRefreshFailed?: boolean;
  };
}) {
  return (
    <PlanCard
      planText={entry.planText}
      onImplement={planCard?.showImplement ? onImplementPlan : undefined}
      implementing={planCard?.implementing}
      autoMode={planCard?.autoMode}
      onHivemindReview={planCard?.showHivemindReview ? onHivemindReview : undefined}
      onLaunchSwarm={planCard?.showLaunchSwarm ? onLaunchSwarm : undefined}
      launching={planCard?.launching}
      launchDisabledReason={planCard?.launchDisabledReason}
      onRequestFeatures={planCard?.showRequestFeatures ? onRequestFeatures : undefined}
      requestingFeatures={planCard?.requestingFeatures}
      pendingFeaturesRefresh={planCard?.pendingFeaturesRefresh}
      featuresRefreshFailed={planCard?.featuresRefreshFailed}
    />
  );
}

function ChatBubble({
  entry,
  isLast,
  streaming,
  showReasoning,
  showToolCalls,
  keepExpanded,
  mergedReasoning,
  skipReasoning,
  inFlightOverlay,
}: {
  entry: ChatBubbleEntry;
  isLast: boolean;
  streaming: boolean;
  showReasoning: boolean;
  showToolCalls: boolean;
  keepExpanded: boolean;
  mergedReasoning?: MergedReasoning;
  skipReasoning: boolean;
  inFlightOverlay?: {
    retryStatus?: { summary: string; attempt: number; maxAttempts: number; delayMs: number };
    streamPhase?: { label: string; rawPhase: string; contextTokens: number | null; elapsedMs: number; hasFirstStream: boolean };
  };
}) {
  const me = entry.who === "user";
  const isStreamingNow = isLast && streaming;
  const display = displayTextOf(entry);
  const text = useRafThrottled(me ? entry.text || "" : display, isStreamingNow);
  const ownReasoning = useRafThrottled(entry.reasoning ?? "", isStreamingNow);
  const isMergedStreaming =
    !!mergedReasoning &&
    mergedReasoning.tailIsLastEntry &&
    streaming &&
    !mergedReasoning.tailHasText;
  const mergedReasoningText = useRafThrottled(
    mergedReasoning?.text ?? "",
    isMergedStreaming,
  );

  const hasOwnReasoning = ownReasoning.length > 0;
  const showOwnReasoningBlock =
    hasOwnReasoning && showReasoning && !skipReasoning && !mergedReasoning;
  const showBubble = me ? (text.length > 0) : (text.trim().length > 1);
  const tools = entry.tools ?? [];
  const showTools = showToolCalls && tools.length > 0 && !me;

  const isMerge = entry.reviewKind?.phase === "merge";

  const rawText = entry.text || "";
  const showHandoffSpinner =
    !me &&
    entry.agent === "worker" &&
    hasHandoffStart(rawText) &&
    !hasCompleteHandoff(rawText);

  const toolGroups = React.useMemo(() => {
    const groups: { name: string; calls: { tool_call_id: string; output: string; done: boolean }[] }[] = [];
    for (const tool of tools) {
      const last = groups[groups.length - 1];
      if (last && last.name === tool.name) {
        last.calls.push({ tool_call_id: tool.tool_call_id, output: tool.output, done: tool.done });
      } else {
        groups.push({
          name: tool.name,
          calls: [{ tool_call_id: tool.tool_call_id, output: tool.output, done: tool.done }],
        });
      }
    }
    return groups;
  }, [tools]);

  const isEmptyMergedSkip = skipReasoning && !showBubble && !showTools && !isMerge && !showHandoffSpinner;
  if (isEmptyMergedSkip) return null;

  const swarmHeader = entry.agent && !me ? (
    <div className="flex items-center gap-2 text-[10.5px] mb-1">
      <span className={`inline-flex items-center gap-1.5 px-1.5 py-0.5 rounded ${AGENT_TONE[entry.agent].bg} ${AGENT_TONE[entry.agent].text} font-mono uppercase tracking-wider`}>
        <span className={`w-1 h-1 rounded-full ${AGENT_TONE[entry.agent].dot}`} />
        {entry.agent}
      </span>
      {entry.featureId && <span className="text-dim font-mono">{entry.featureId}</span>}
      {(entry.createdAt != null || entry.t) && (
        <RelativeTime
          createdAt={entry.createdAt}
          fallback={entry.t}
          className="text-dim"
        />
      )}
    </div>
  ) : null;

  const reasoningNode = (() => {
    if (mergedReasoning && showReasoning) {
      return (
        <ReasoningBlock
          reasoning={mergedReasoningText}
          streaming={isMergedStreaming}
          keepExpanded={keepExpanded}
          durationMs={mergedReasoning.durationMs}
          createdAt={mergedReasoning.createdAt}
        />
      );
    }
    if (showOwnReasoningBlock) {
      return (
        <ReasoningBlock
          reasoning={ownReasoning}
          streaming={isStreamingNow && !showBubble}
          keepExpanded={keepExpanded}
          durationMs={entry.reasoningDurationMs}
          createdAt={entry.createdAt}
        />
      );
    }
    if (!showReasoning && !skipReasoning && (hasOwnReasoning || mergedReasoning)) {
      const isActivelyReasoning = mergedReasoning
        ? isMergedStreaming
        : isStreamingNow && !showBubble;
      return <InlineReasoningIndicator streaming={isActivelyReasoning} />;
    }
    if (!skipReasoning && showReasoning && isStreamingNow && !showBubble && !showTools) {
      return <InlineReasoningIndicator streaming />;
    }
    return null;
  })();

  const overlay = (() => {
    if (!inFlightOverlay) return null;
    if (!isLast) return null;
    if (me) return null;
    const retry = inFlightOverlay.retryStatus;
    if (retry) {
      return (
        <div className="flex items-center gap-2 my-1.5 px-2 py-1.5 rounded-md border border-amber-500/30 bg-amber-500/10 text-[11px] text-amber-300 font-mono">
          <svg className="animate-spin h-3 w-3 text-amber-300" xmlns="http://www.w3.org/2000/svg" fill="none" viewBox="0 0 24 24" aria-hidden="true">
            <circle className="opacity-25" cx="12" cy="12" r="10" stroke="currentColor" strokeWidth="4" />
            <path className="opacity-75" fill="currentColor" d="M4 12a8 8 0 018-8v4a4 4 0 00-4 4H4z" />
          </svg>
          <span>{retry.summary} — retrying</span>
          {retry.maxAttempts > 0 && retry.attempt > 0 && (
            <span className="text-amber-300/70">attempt {retry.attempt}/{retry.maxAttempts}</span>
          )}
          {retry.delayMs >= 500 && (
            <span className="text-amber-300/70">· in {Math.max(1, Math.round(retry.delayMs / 1000))}s</span>
          )}
        </div>
      );
    }
    const phase = inFlightOverlay.streamPhase;
    if (!phase) return null;
    if (entry.text) return null;
    if (phase.hasFirstStream) return null;
    if (!phase.label) return null;
    return (
      <div className="flex items-center gap-1.5 my-1.5 px-1 text-[11px] text-muted font-mono">
        <span>{phase.label}</span>
        {phase.contextTokens != null && phase.rawPhase === "prompt_loaded" && (
          <span className="text-dim">({phase.contextTokens.toLocaleString()} tokens)</span>
        )}
        {phase.elapsedMs >= 1000 && (
          <span className="text-dim">· {formatMmSs(phase.elapsedMs)}</span>
        )}
      </div>
    );
  })();

  const handoffNode = (() => {
    if (!showHandoffSpinner) return null;
    return (
      <div className="flex items-center gap-2 my-1.5 px-2 py-1.5 rounded-md border border-honey-500/30 bg-honey-500/10 text-[11px] text-honey-300 font-mono w-fit">
        <svg className="animate-spin h-3 w-3 text-honey-300" xmlns="http://www.w3.org/2000/svg" fill="none" viewBox="0 0 24 24" aria-hidden="true">
          <circle className="opacity-25" cx="12" cy="12" r="10" stroke="currentColor" strokeWidth="4" />
          <path className="opacity-75" fill="currentColor" d="M4 12a8 8 0 018-8v4a4 4 0 00-4 4H4z" />
        </svg>
        <span>emitting handoff…</span>
      </div>
    );
  })();

  const body = (
    <>
      {swarmHeader}
      {reasoningNode}
      {overlay}
      {handoffNode}
      {entry.images && entry.images.length > 0 && (
        <div className="flex gap-2 flex-wrap mb-2 justify-end">
          {entry.images.map((img) => (
            <img key={img.id} src={img.previewUrl} alt="Attached image" className="w-16 h-16 object-cover rounded-lg border border-black/20" />
          ))}
        </div>
      )}
      {isMerge ? (
        <div className="flex justify-start">
          <div className="max-w-[680px] min-w-[180px]">
            {(entry.createdAt != null || entry.t) && (
              <div className="flex items-center gap-2 text-[10.5px] mb-1">
                <RelativeTime
                  createdAt={entry.createdAt}
                  fallback={entry.t}
                  className="text-dim"
                />
              </div>
            )}
            <MergeScoringPill verdicts={entry.verdicts ?? null} streaming={isLast && streaming} />
            {showBubble && (
              <div
                data-ctx-bubble
                className="mt-1.5 rounded-2xl px-4 py-3 text-[13.5px] leading-relaxed bg-ink-800 border border-line text-white/90 rounded-tl-md"
                {...(isStreamingNow
                  ? { "aria-live": "polite" as const, "aria-atomic": "false" as const }
                  : {})}
              >
                <Markdown text={text} variant="assistant" />
              </div>
            )}
            {showTools && (
              <div className="mt-1">
                {toolGroups.map((g) => (
                  <ToolCallGroup key={g.calls[0].tool_call_id} name={g.name} calls={g.calls} />
                ))}
              </div>
            )}
          </div>
        </div>
      ) : (showBubble || showTools) ? (
        <div className={`flex ${me ? "justify-end" : "justify-start"}`}>
          <div className={`${entry.agent ? "max-w-[680px] min-w-[180px]" : "max-w-[680px]"}`} data-ctx-role={me ? "user" : "asst"}>
            {!entry.agent && (
              <div className={`flex items-center gap-2 text-[10.5px] mb-1 ${me ? "justify-end" : ""}`}>
                {!me && entry.text && entry.text.trim().length > 0 && entry.model && (
                  <span className="font-mono text-blue-300">{entry.model}</span>
                )}
                {me && <span className="text-muted font-medium">You</span>}
                {entry.steered && <span className="text-honey-400/70 text-[10px] font-mono">steered</span>}
                {(entry.createdAt != null || entry.t) && (
                  <RelativeTime
                    createdAt={entry.createdAt}
                    fallback={entry.t}
                    className="text-dim"
                  />
                )}
              </div>
            )}
            {showBubble && (
              <div
                data-ctx-bubble
                className={`rounded-2xl px-4 py-3 text-[13.5px] leading-relaxed ${
                  me
                    ? "bg-honey-500 text-ink-900 font-medium rounded-tr-md"
                    : "bg-ink-800 border border-line text-white/90 rounded-tl-md"
                }`}
                /*
                 * a11y: only the actively streaming assistant bubble is a live
                 * region. We use `polite` so chunks queue behind user activity
                 * and `aria-atomic="false"` so SR announces newly appended
                 * text rather than re-reading the whole bubble per chunk.
                 * User messages and finished bubbles are static — no live
                 * region needed.
                 */
                {...(isStreamingNow && !me
                  ? { "aria-live": "polite" as const, "aria-atomic": "false" as const }
                  : {})}
              >
                <Markdown text={text} variant={me ? "user" : "assistant"} />
              </div>
            )}
            {showTools && (
              <div className="mt-1">
                {toolGroups.map((g) => (
                  <ToolCallGroup key={g.calls[0].tool_call_id} name={g.name} calls={g.calls} />
                ))}
              </div>
            )}
          </div>
        </div>
      ) : null}
      {entry.error && (
        <div className="mt-2 text-[11.5px] text-red-300 font-mono">{entry.error}</div>
      )}
    </>
  );

  const reviewKind = entry.reviewKind;
  if (!reviewKind) return <div>{body}</div>;

  const reviewBadgeLabel =
    reviewKind.phase === "context"
      ? "Hivemind context"
      : `Hivemind merge${reviewKind.round ? ` · round ${reviewKind.round}` : ""}`;

  return (
    <div className="my-2 ml-2 pl-3 border-l-2 border-honey-500/40 bg-honey-500/[0.03] rounded-r-md py-1">
      <div className="text-[10.5px] font-mono text-honey-300/80 mb-1.5 flex items-center gap-1.5">
        <span className="w-1.5 h-1.5 rounded-full bg-honey-400/70" />
        <span>{reviewBadgeLabel}</span>
        {entry.model && (
          <>
            <span className="text-dim">·</span>
            <span className="text-honey-300/60">pi: {entry.model}</span>
          </>
        )}
      </div>
      {body}
    </div>
  );
}

export interface ActivityStreamProps {
  entries: StreamEntry[];
  showReasoning: boolean;
  showToolCalls: boolean;
  streaming: boolean;
  tailLimit?: number;
  emptyState?: { icon?: React.ReactNode; primary: string; secondary?: string };
  onActiveSessionChange?: (active: ActiveSession | null) => void;
  onImplementPlan?: () => void;
  onHivemindReview?: () => void;
  onLaunchSwarm?: () => void;
  onRequestFeatures?: () => void;
  planCard?: {
    implementing?: boolean;
    autoMode?: boolean;
    launching?: boolean;
    launchDisabledReason?: string;
    showImplement?: boolean;
    showLaunchSwarm?: boolean;
    showHivemindReview?: boolean;
    showRequestFeatures?: boolean;
    requestingFeatures?: boolean;
    pendingFeaturesRefresh?: boolean;
    featuresRefreshFailed?: boolean;
  };
  inFlightOverlay?: {
    retryStatus?: { summary: string; attempt: number; maxAttempts: number; delayMs: number };
    streamPhase?: { label: string; rawPhase: string; contextTokens: number | null; elapsedMs: number; hasFirstStream: boolean };
  };
  delimiterLoading?: "plan" | "features" | "review-prompt" | "questions" | null;
  /**
   * Opt-in conversation context identifier. When this value changes (e.g.
   * Tasks-view sidebar switches between tasks) the scroll container is
   * pinned back to the bottom and the "show all earlier events" toggle is
   * collapsed, so a fresh conversation always opens at the latest message.
   *
   * Leave undefined to preserve the legacy non-resetting behaviour (used by
   * SwarmControl, which keeps the same component instance across selection
   * changes but doesn't want a forced scroll).
   */
  conversationKey?: string;
}

export function ActivityStream({
  entries,
  showReasoning,
  showToolCalls,
  streaming,
  tailLimit = DEFAULT_TAIL_LIMIT,
  emptyState,
  onActiveSessionChange,
  onImplementPlan,
  onHivemindReview,
  onLaunchSwarm,
  onRequestFeatures,
  planCard,
  inFlightOverlay,
  delimiterLoading,
  conversationKey,
}: ActivityStreamProps) {
  const scrollRef = React.useRef<HTMLDivElement>(null);
  const contentRef = React.useRef<HTMLDivElement>(null);
  const isAtBottomRef = React.useRef(true);
  const scrollRafRef = React.useRef<number | null>(null);
  const lastScrollTopRef = React.useRef(0);
  const activeSessionRef = React.useRef<ActiveSession>({
    sessionId: null,
    model: null,
    agent: null,
  });

  // `isAtBottomRef` represents user intent. We only relinquish follow when
  // the user actively scrolls upward; layout growth alone (reasoning panel
  // expansion + chat-bubble append in the same frame) is invisible to this
  // ref, leaving `pinToBottom`'s rAF free to catch up.
  //
  // Re-arming uses the position-based threshold so the user landing back at
  // the bottom (manually or via a programmatic pin) re-engages follow.
  const handleScroll = React.useCallback(() => {
    const el = scrollRef.current;
    if (!el) return;
    const scrollTop = el.scrollTop;
    const scrollHeight = el.scrollHeight;
    const clientHeight = el.clientHeight;
    const atBottom = scrollHeight - clientHeight - scrollTop < 80;
    const userScrolledUp = scrollTop < lastScrollTopRef.current;
    lastScrollTopRef.current = scrollTop;
    if (atBottom) {
      isAtBottomRef.current = true;
    } else if (userScrolledUp) {
      isAtBottomRef.current = false;
    }
    // Else: leave isAtBottomRef unchanged. A scroll event triggered by
    // layout growth (same scrollTop, larger scrollHeight) must not flip
    // follow off, or the user gets stranded mid-stream.
  }, []);

  // Shared "pin to bottom if user is at bottom" callback. Coalesces through
  // `scrollRafRef` so the entries-driven effect, the conversationKey effect,
  // and the ResizeObserver effect cannot schedule overlapping rAFs in the
  // same frame. Gated on `isAtBottomRef.current` so a user who scrolled up
  // is never yanked back to the bottom by sibling layout changes.
  const pinToBottom = React.useCallback(() => {
    if (!isAtBottomRef.current) return;
    if (scrollRafRef.current !== null) return;
    scrollRafRef.current = requestAnimationFrame(() => {
      scrollRafRef.current = null;
      if (!isAtBottomRef.current) return;
      const el = scrollRef.current;
      if (!el) return;
      el.scrollTop = el.scrollHeight;
    });
  }, []);

  React.useEffect(() => {
    pinToBottom();
  }, [entries, pinToBottom]);

  // Watch the scroll container AND its inner content wrapper so layout
  // changes that don't append a new entry (QuestionsDock mount, Hivemind
  // review dock mount/swap/collapse, banners, image-preview row, streaming
  // spinners, markdown / reasoning / image reflow) still re-pin to the
  // bottom when the user was already at the bottom. `scrollEl` catches
  // `clientHeight` changes from sibling docks; `contentEl` catches
  // `scrollHeight` growth from async content. Setting `scrollTop` does not
  // change observed sizes, so this cannot loop.
  React.useEffect(() => {
    const scrollEl = scrollRef.current;
    const contentEl = contentRef.current;
    if (!scrollEl || !contentEl) return;
    if (typeof ResizeObserver === "undefined") return; // jsdom / very old browsers
    const ro = new ResizeObserver(() => pinToBottom());
    ro.observe(scrollEl);
    ro.observe(contentEl);
    return () => ro.disconnect();
  }, [pinToBottom]);

  React.useEffect(() => {
    return () => {
      if (scrollRafRef.current !== null) {
        cancelAnimationFrame(scrollRafRef.current);
        scrollRafRef.current = null;
      }
    };
  }, []);

  React.useEffect(() => {
    if (!onActiveSessionChange) return;
    let chosenMarker: SessionMarkerEntry | null = null;
    let chosenBubble: ChatBubbleEntry | null = null;

    const endedSessionIds = new Set<string>();
    for (let i = entries.length - 1; i >= 0; i--) {
      const e = entries[i];
      if (e.kind === "session_marker" && e.phase === "end" && e.sessionId) {
        endedSessionIds.add(e.sessionId);
      }
    }
    for (let i = entries.length - 1; i >= 0; i--) {
      const e = entries[i];
      if (e.kind === "session_marker" && e.phase === "start") {
        if (!e.sessionId || !endedSessionIds.has(e.sessionId)) {
          chosenMarker = e;
          break;
        }
      }
    }
    if (!chosenMarker) {
      for (let i = entries.length - 1; i >= 0; i--) {
        const e = entries[i];
        if (e.kind === "session_marker") {
          chosenMarker = e;
          break;
        }
        if (e.kind === "chat_bubble" && e.who === "asst") {
          chosenBubble = e;
          break;
        }
      }
    }

    let next: ActiveSession;
    if (chosenMarker) {
      next = {
        sessionId: chosenMarker.sessionId ?? null,
        model: chosenMarker.model ?? chosenMarker.agentModel ?? null,
        agent: chosenMarker.agent ?? null,
      };
    } else if (chosenBubble) {
      next = {
        sessionId: chosenBubble.sessionId ?? null,
        model: chosenBubble.model ?? null,
        agent: chosenBubble.agent ?? null,
      };
    } else {
      next = { sessionId: null, model: null, agent: null };
    }

    const prev = activeSessionRef.current;
    if (
      prev.sessionId !== next.sessionId ||
      prev.model !== next.model ||
      prev.agent !== next.agent
    ) {
      activeSessionRef.current = next;
      onActiveSessionChange(next);
    }
  }, [entries, onActiveSessionChange]);

  const totalEntries = entries.length;
  const [showAll, setShowAll] = React.useState(false);

  // Opt-in: when the conversation context changes (e.g. Tasks-view sidebar
  // switches tasks), force the scroll container back to the bottom, re-pin
  // `isAtBottomRef`, and collapse the "show all earlier events" toggle so the
  // user always lands at the tail of the new conversation. Skipped entirely
  // when `conversationKey` is undefined to preserve legacy behaviour for
  // SwarmControl. The `scrollRafRef` guard coalesces with the entries-driven
  // autoscroll effect to avoid double rAF scheduling.
  React.useEffect(() => {
    if (conversationKey === undefined) return;
    isAtBottomRef.current = true;
    setShowAll(false);
    pinToBottom();
  }, [conversationKey, pinToBottom]);

  const hiddenCount = !showAll && totalEntries > tailLimit ? totalEntries - tailLimit : 0;
  const sliceStart = hiddenCount;
  const visible = React.useMemo(
    () => (hiddenCount > 0 ? entries.slice(sliceStart) : entries),
    [entries, hiddenCount, sliceStart],
  );

  const renderPlan = React.useMemo(
    () => computeReasoningRenderPlan(entries, showToolCalls),
    [entries, showToolCalls],
  );

  const endedSessionStartIds = React.useMemo(() => {
    const ended = new Set<string>();
    const startedAfterEnd = new Set<string>();
    for (let i = 0; i < entries.length; i++) {
      const e = entries[i];
      if (e.kind === "session_marker" && e.phase === "end" && e.sessionId) {
        ended.add(e.sessionId);
      }
    }
    for (let i = 0; i < entries.length; i++) {
      const e = entries[i];
      if (e.kind === "session_marker" && e.phase === "start" && e.sessionId && ended.has(e.sessionId)) {
        startedAfterEnd.add(`${i}:${e.sessionId}`);
      }
    }
    return startedAfterEnd;
  }, [entries]);

  return (
    <div
      ref={scrollRef}
      onScroll={handleScroll}
      data-testid="activity-stream-scroll"
      className="flex-1 min-h-0 h-full overflow-auto bg-ink-950/60"
    >
      <div ref={contentRef} className="max-w-[860px] mx-auto px-6 py-4 space-y-2">
        {totalEntries === 0 ? (
          emptyState ? (
            <div className="text-dim text-[12px] font-mono py-8 text-center flex items-center justify-center gap-2">
              {emptyState.icon}
              <div>
                <div>{emptyState.primary}</div>
                {emptyState.secondary && <div className="text-[11px] mt-1">{emptyState.secondary}</div>}
              </div>
            </div>
          ) : null
        ) : (
          <>
            {hiddenCount > 0 && (
              <div className="flex items-center gap-3 my-2">
                <div className="flex-1 h-px bg-line" />
                <button
                  type="button"
                  onClick={() => setShowAll(true)}
                  className="text-[11px] text-muted hover:text-white font-mono px-2.5 py-1 rounded-md border border-line bg-ink-850 hover:border-line-strong"
                >
                  {hiddenCount} earlier event{hiddenCount === 1 ? "" : "s"} — show all
                </button>
                <div className="flex-1 h-px bg-line" />
              </div>
            )}
            {visible.map((entry, idx) => {
              const absoluteIdx = sliceStart + idx;
              const isLast = absoluteIdx === totalEntries - 1;
              if (entry.kind === "session_marker") {
                const hasMatchingEnd =
                  entry.phase === "start" &&
                  !!entry.sessionId &&
                  endedSessionStartIds.has(`${absoluteIdx}:${entry.sessionId}`);
                return (
                  <SessionMarker key={entry.id} entry={entry} hasMatchingEnd={hasMatchingEnd} />
                );
              }
              if (entry.kind === "error") {
                return <ErrorRow key={entry.id} entry={entry} />;
              }
              if (entry.kind === "plan") {
                return (
                  <div key={entry.id}>
                    <PlanCardWrapper
                      entry={entry}
                      onImplementPlan={onImplementPlan}
                      onHivemindReview={onHivemindReview}
                      onLaunchSwarm={onLaunchSwarm}
                      onRequestFeatures={onRequestFeatures}
                      planCard={planCard}
                    />
                  </div>
                );
              }
              if (entry.kind === "questions") {
                // Rendered by QuestionsDock dock above the chat composer.
                return null;
              }
              if (entry.kind === "complete") {
                return <CompleteChip key={entry.id} entry={entry} />;
              }
              if (entry.kind === "nurse") {
                return <NurseMessage key={entry.id} entry={entry} />;
              }
              const skipReasoning = renderPlan.mergeSkip.has(absoluteIdx);
              const leader = renderPlan.mergeLeader.get(absoluteIdx);
              const tailIdx = leader ? leader.lastIdx : absoluteIdx;
              const hasOwnReasoning = (entry.reasoning?.length ?? 0) > 0;
              const isReasoningOwner = hasOwnReasoning && !skipReasoning;
              const keepExpanded =
                isReasoningOwner &&
                tailIdx === renderPlan.lastReasoningIdx &&
                !hasBreakerAfter(entries, tailIdx);
              let mergedReasoning: MergedReasoning | undefined;
              if (leader) {
                const tailEntry = entries[leader.lastIdx];
                const tailIsLastEntry = leader.lastIdx === totalEntries - 1;
                const tailHasText =
                  tailEntry.kind === "chat_bubble" &&
                  displayTextOf(tailEntry).trim().length > 0;
                // `entry` at this iteration IS the leader (mergeLeader is
                // keyed by leader absoluteIdx), so its createdAt is the
                // leader's createdAt by construction.
                mergedReasoning = {
                  text: leader.reasoning,
                  durationMs: leader.durationMs,
                  tailIsLastEntry,
                  tailHasText,
                  createdAt: entry.createdAt,
                };
              }
              return (
                <ChatBubble
                  key={entry.id}
                  entry={entry}
                  isLast={isLast}
                  streaming={streaming}
                  showReasoning={showReasoning}
                  showToolCalls={showToolCalls}
                  keepExpanded={keepExpanded}
                  mergedReasoning={mergedReasoning}
                  skipReasoning={skipReasoning}
                  inFlightOverlay={inFlightOverlay}
                />
              );
            })}
            {streaming && delimiterLoading && (
              <div className="flex justify-start">
                <div className="max-w-[680px]">
                  {/*
                   * a11y: phased loading messages ("Creating Plan…",
                   * "Decomposing Features…", ...) communicate distinct stages,
                   * so a polite live region announces each transition.
                   * Visible spinner already conveys progress visually; the
                   * label is the SR-relevant payload.
                   */}
                  <div
                    aria-live="polite"
                    aria-atomic="true"
                    className="flex items-center gap-3 px-4 py-3 rounded-xl border border-honey-500/25 bg-gradient-to-r from-honey-500/8 to-transparent"
                  >
                    <div
                      role="status"
                      aria-label="Loading"
                      className="w-4 h-4 border-2 border-honey-400/40 border-t-honey-400 rounded-full animate-spin"
                    />
                    <span className="text-[13px] text-honey-200 font-medium">
                      {delimiterLoading === "plan"
                        ? "Creating Plan…"
                        : delimiterLoading === "features"
                          ? "Decomposing Features…"
                          : delimiterLoading === "review-prompt"
                            ? "Building Hivemind Prompt…"
                            : "Preparing Questions…"}
                    </span>
                  </div>
                </div>
              </div>
            )}
            {streaming && !delimiterLoading && (
              <div className="flex justify-start">
                <div className="max-w-[680px]">
                  <div className="flex items-center gap-2 mb-1">
                    {/*
                     * a11y: bare spinner — aria-label gives SR a single,
                     * non-repeating "Streaming response" announcement.
                     * No live region (the streaming bubble above is the
                     * authoritative output region).
                     */}
                    <div
                      role="status"
                      aria-label="Streaming response"
                      className="w-4 h-4 border-2 border-honey-400/40 border-t-honey-400 rounded-full animate-spin"
                    />
                  </div>
                </div>
              </div>
            )}
          </>
        )}
      </div>
    </div>
  );
}
