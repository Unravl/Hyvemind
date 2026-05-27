import React from "react";
import type {
  MonitoredSessionSnapshot,
  SessionHealthStatus,
} from "../../types/nurse";

/* ── Status colors ─────────────────────────────────────────── */

export const STATUS_COLORS: Record<SessionHealthStatus, string> = {
  healthy: "bg-emerald-400",
  warning: "bg-amber-400",
  stalled: "bg-red-400",
  intervening: "bg-orange-400",
  resolved: "bg-sky-400",
  failed: "bg-red-600",
};

export const STATUS_LABELS: Record<SessionHealthStatus, string> = {
  healthy: "Healthy",
  warning: "Warning",
  stalled: "Stalled",
  intervening: "Intervening",
  resolved: "Resolved",
  failed: "Failed",
};

/* ── Helpers ───────────────────────────────────────────────── */

export function truncateSessionId(id: string, maxLen = 12): string {
  if (id.length <= maxLen) return id;
  return id.slice(0, maxLen) + "…";
}

export function formatAge(ms: number): string {
  const secs = Math.floor((Date.now() - ms) / 1000);
  if (secs < 60) return `${secs}s`;
  if (secs < 3600) return `${Math.floor(secs / 60)}m`;
  return `${Math.floor(secs / 3600)}h`;
}

/* ── Session Card ──────────────────────────────────────────── */

export function SessionCard({ session }: { session: MonitoredSessionSnapshot }) {
  return (
    <div className="flex items-center gap-2 px-3 py-2 border-b border-line last:border-b-0">
      <span className={`w-2 h-2 rounded-full shrink-0 ${STATUS_COLORS[session.status]}`} />
      <div className="flex-1 min-w-0">
        <div className="text-[11px] font-mono text-white/90 truncate" title={session.session_id}>
          {truncateSessionId(session.session_id)}
        </div>
        <div className="text-[10px] text-muted flex gap-2">
          <span>{STATUS_LABELS[session.status]}</span>
          <span>·</span>
          <span>{formatAge(session.last_activity_ms)} ago</span>
          <span>·</span>
          <span>{session.event_count} events</span>
        </div>
      </div>
      {session.intervention_count > 0 && (
        <span className="text-[10px] text-amber-300 bg-amber-500/15 px-1.5 py-0.5 rounded">
          {session.intervention_count} int.
        </span>
      )}
    </div>
  );
}
