import React from "react";
import { I } from "./icons";
import { fmtTok2 } from "../lib/uiPrefs";

/* ── ContextStatusBar ─────────────────────────────────────── */
/*
 * Reusable bottom status bar shared by Tasks view and Swarm Control's
 * Live-activity panel. Renders:
 *  - Truncated 8-char session ID chip (click → copy full UUID)
 *  - Reasoning toggle (brain icon)
 *  - Tool-calls toggle (terminal icon)
 *  - Truncated mono model label
 *  - Context fill bar with percentage
 *  - ↑in / ↓out tokens vs ctx-window label
 *  - Optional t/s
 *
 * This is purely presentational — all state lives in the parent.
 */

export interface ContextStatusBarProps {
  /** Truncated 8-char chip if present; clicking copies full UUID. */
  sessionId?: string | null;
  /** Truncated mono label of the active model. */
  modelLabel?: string | null;
  /** Full model id used for the tooltip on the model label. */
  modelTitle?: string | null;
  showReasoning: boolean;
  showToolCalls: boolean;
  onToggleReasoning: () => void;
  onToggleToolCalls: () => void;
  /** Context fill, 0..100. */
  ctxPct: number;
  /** Context-window label, e.g. "200k" or "1M". */
  ctxLabel: string;
  tokIn: number;
  tokOut: number;
  /** Omitted from rendering if null/0. */
  tokPerSec?: number | null;
  /** Tailwind class controlling the inner container's max width. */
  maxWidthClass?: string;
  /** When true, shows a small "swarm" badge indicating these are swarm-wide combined stats. */
  isSwarmContext?: boolean;
}

export function ContextStatusBar({
  sessionId,
  modelLabel,
  modelTitle,
  showReasoning,
  showToolCalls,
  onToggleReasoning,
  onToggleToolCalls,
  ctxPct,
  ctxLabel,
  tokIn,
  tokOut,
  tokPerSec,
  maxWidthClass = "max-w-[860px]",
  isSwarmContext = false,
}: ContextStatusBarProps) {
  const fullTitle = (modelTitle || modelLabel || "").trim();
  return (
    <div className={`${maxWidthClass} mx-auto px-6 py-1.5 flex items-center gap-3 text-[11px]`}>
      {sessionId && (
        <button
          data-ctx-session-id={sessionId}
          className="font-mono text-dim hover:text-white/70 transition-colors cursor-pointer shrink-0"
          title="Click to copy session ID"
          onClick={() => {
            navigator.clipboard.writeText(sessionId);
          }}
        >
          {sessionId.slice(0, 8)}
        </button>
      )}
      <button
        onClick={onToggleReasoning}
        title={showReasoning ? "Hide Reasoning" : "Show Reasoning"}
        className={`h-5 px-1.5 rounded flex items-center gap-1 text-[10px] font-medium transition-all shrink-0 ${
          showReasoning
            ? "bg-violet-500/15 text-violet-400 border border-violet-500/30"
            : "text-dim hover:text-violet-300 hover:bg-violet-500/10"
        }`}
      >
        {I.brain({ size: 10 })}
      </button>
      <button
        onClick={onToggleToolCalls}
        title={showToolCalls ? "Hide Tool Calls" : "Show Tool Calls"}
        aria-label={showToolCalls ? "Hide Tool Calls" : "Show Tool Calls"}
        aria-pressed={showToolCalls}
        className={`h-5 px-1.5 rounded flex items-center gap-1 text-[10px] font-medium transition-all shrink-0 ${
          showToolCalls
            ? "bg-cyan-500/15 text-cyan-400 border border-cyan-500/30"
            : "text-dim hover:text-cyan-300 hover:bg-cyan-500/10"
        }`}
      >
        {I.terminal({ size: 10 })}
      </button>
      {modelLabel && (
        <span
          className="font-mono text-dim truncate min-w-0 max-w-[200px]"
          title={fullTitle || modelLabel}
          aria-label={fullTitle || modelLabel}
        >
          {modelLabel}
        </span>
      )}
      {isSwarmContext && (
        <span className="text-[9px] uppercase tracking-wider font-bold text-honey-400 border border-honey-500/30 rounded px-1 py-0.5 leading-none shrink-0">
          swarm
        </span>
      )}
      <div className="flex-1" />
      <span className="text-dim font-medium">Context</span>
      <div className="w-24 h-1.5 rounded-full bg-ink-700 overflow-hidden">
        <div
          className={`h-full ${
            ctxPct >= 90 ? "bg-rose-500" : ctxPct >= 70 ? "bg-amber-400" : "bg-honey-400"
          }`}
          style={{ width: `${ctxPct}%` }}
        />
      </div>
      <span className="font-mono text-muted">{ctxPct}%</span>
      <span className="font-mono text-emerald-300">&uarr;{fmtTok2(tokIn)}</span>
      <span className="text-dim">&middot;</span>
      <span className="font-mono text-blue-300">&darr;{fmtTok2(tokOut)}</span>
      <span className="text-dim">/</span>
      <span className="font-mono text-muted">{ctxLabel}</span>
      {tokPerSec != null && tokPerSec > 0 && (
        <>
          <span className="text-dim">&middot;</span>
          <span className="font-mono text-amber-300">{tokPerSec} t/s</span>
        </>
      )}
    </div>
  );
}
