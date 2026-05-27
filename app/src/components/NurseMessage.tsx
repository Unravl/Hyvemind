import React, { useState } from "react";
import type { NurseEntry } from "../lib/streamEntry";

/** Inline "Nurse in action" card rendered in the conversation stream.
 *
 * Layout:
 *   ┌──────────────────────────────────────────────┐
 *   │ 🐝  Nurse                            Working │   <- header + pulsing status pill
 *   │ I have spotted: <observation>                 │
 *   │ I'll <action>                                 │
 *   │ ─────────────────────────────────────         │
 *   │ ▸ Show reasoning   (collapsible)              │
 *   └──────────────────────────────────────────────┘
 *
 * While `status` is `started` or `reasoning` the left border + dot pulse so
 * the user can see the agent is actively working. On `completed` the pulse
 * stops and the badge flips to "Resolved"; on `failed` it shows "Failed".
 */
export function NurseMessage({ entry }: { entry: NurseEntry }) {
  const [showReasoning, setShowReasoning] = useState(false);
  const working = entry.status === "started" || entry.status === "reasoning";
  const failed = entry.status === "failed";

  const statusLabel = failed
    ? "Failed"
    : entry.status === "completed"
      ? "Resolved"
      : "Working";

  const statusClass = failed
    ? "bg-red-500/10 text-red-400 border-red-500/30"
    : entry.status === "completed"
      ? "bg-emerald-500/10 text-emerald-400 border-emerald-500/30"
      : "bg-pink-500/10 text-pink-300 border-pink-400/40";

  // Nurse identity: light pink border on every card (both sides + thicker
  // left rail) so the agent is instantly distinguishable from regular bubbles
  // in any activity stream. Failed runs flip the rail to red.
  const borderClass = failed
    ? "border-pink-300/30 border-l-red-500/60"
    : "border-pink-300/40 border-l-pink-300/80";

  const dotClass = failed
    ? "bg-red-400"
    : entry.status === "completed"
      ? "bg-emerald-400"
      : "bg-pink-300 animate-pulse";

  const levelLabel = entry.level
    ? entry.level.charAt(0).toUpperCase() + entry.level.slice(1).replace(/_/g, " ")
    : "Nurse";

  return (
    <div
      className={`my-3 rounded-md border bg-ink-950/60 border-l-4 ${borderClass} px-4 py-3 ${
        working ? "ring-1 ring-pink-300/20" : ""
      }`}
      data-intervention-id={entry.interventionId}
    >
      <div className="flex items-center gap-2 mb-2">
        <span className={`inline-block w-2 h-2 rounded-full ${dotClass}`} />
        <span className="text-sm font-semibold text-pink-300">Nurse</span>
        <span className="text-xs text-ink-400">·</span>
        <span className="text-xs text-ink-400 lowercase">{levelLabel}</span>
        <div className="flex-1" />
        <span
          className={`inline-flex items-center px-2 py-0.5 text-[10px] uppercase tracking-wider rounded-full border ${statusClass}`}
        >
          {statusLabel}
        </span>
      </div>

      {entry.observation && (
        <div className="text-sm font-medium text-ink-50">
          <span className="text-ink-400">I have spotted: </span>
          {entry.observation}
        </div>
      )}
      {entry.action && (
        <div className="text-sm text-ink-200 mt-1">{entry.action}</div>
      )}

      {entry.error && (
        <div className="mt-2 text-xs text-red-400">{entry.error}</div>
      )}

      {entry.reasoning && entry.reasoning.trim().length > 0 && (
        <div className="mt-3 border-t border-line pt-2">
          <button
            type="button"
            onClick={() => setShowReasoning((v) => !v)}
            className="text-xs text-ink-400 hover:text-ink-200 inline-flex items-center gap-1"
          >
            <span
              className={`inline-block transition-transform ${
                showReasoning ? "rotate-90" : ""
              }`}
            >
              ▸
            </span>
            {showReasoning ? "Hide reasoning" : "Show reasoning"}
          </button>
          {showReasoning && (
            <pre className="mt-2 text-xs whitespace-pre-wrap font-mono text-ink-300 leading-relaxed max-h-64 overflow-y-auto">
              {entry.reasoning}
            </pre>
          )}
        </div>
      )}
    </div>
  );
}
