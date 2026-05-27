import React, { useState, useRef, useEffect, useMemo } from "react";
import { useNurseStatus } from "../hooks/useNurseStatus";
import { isTauri } from "../lib/tauri";
import { SessionCard } from "./sessions/SessionCard";
import type { SessionHealthStatus } from "../types/nurse";

/* ── Status-aware dot color for the pill ───────────────────── */

function pillDotColor(
  sessionsLen: number,
  hasWarning: boolean,
): string {
  if (sessionsLen === 0) return "bg-line-strong";
  if (hasWarning) return "bg-amber-400";
  return "bg-emerald-400";
}

/* ── Main Dropdown ─────────────────────────────────────────── */

function SessionsDropdownInner({
  onOpenChange,
}: {
  onOpenChange?: (open: boolean) => void;
}) {
  const { status } = useNurseStatus();
  const { sessions, stats, config } = status;

  const [open, setOpen] = useState(false);
  const ref = useRef<HTMLDivElement>(null);

  // All open-state transitions go through this so the parent (Topbar) can
  // disable its drag zones while the dropdown is open. Mirrors
  // NurseDropdown's pattern exactly.
  const setOpenAndNotify = (next: boolean | ((current: boolean) => boolean)) => {
    setOpen((current) => {
      const resolved = typeof next === "function" ? next(current) : next;
      onOpenChange?.(resolved);
      return resolved;
    });
  };

  // Close on outside click
  useEffect(() => {
    if (!open) return;
    const handler = (e: MouseEvent) => {
      if (ref.current && !ref.current.contains(e.target as Node)) {
        setOpenAndNotify(false);
      }
    };
    document.addEventListener("mousedown", handler);
    return () => document.removeEventListener("mousedown", handler);
  }, [open]);

  // Sort: stalled/intervening first
  const sortedSessions = useMemo(() => {
    const priority: Record<SessionHealthStatus, number> = {
      stalled: 0,
      intervening: 1,
      warning: 2,
      failed: 3,
      resolved: 4,
      healthy: 5,
    };
    return [...sessions].sort(
      (a, b) => (priority[a.status] ?? 5) - (priority[b.status] ?? 5),
    );
  }, [sessions]);

  const hasWarning = sessions.some(
    (s) =>
      s.status === "stalled" ||
      s.status === "intervening" ||
      s.status === "warning" ||
      s.status === "failed",
  );

  const inTauri = isTauri();
  const sessionsLen = sessions.length;
  const label = inTauri
    ? `${sessionsLen} Session${sessionsLen !== 1 ? "s" : ""}`
    : "3 Sessions";
  const dotColor = inTauri ? pillDotColor(sessionsLen, hasWarning) : "bg-emerald-400";

  return (
    <div className="relative" ref={ref}>
      {/* Trigger pill — matches the legacy static "X Sessions" pill styling. */}
      <button
        type="button"
        onClick={() => setOpenAndNotify((v) => !v)}
        aria-expanded={open}
        aria-haspopup="dialog"
        className="flex items-center gap-1.5 px-2.5 h-7 rounded-md border border-line bg-ink-850 text-muted text-[12px] hover:border-line-strong hover:text-white/85 transition"
        title={`${label} — Nurse-monitored`}
      >
        <span className={`w-1.5 h-1.5 rounded-full ${dotColor}`} />
        <span>{label}</span>
      </button>

      {/* Dropdown panel */}
      {open && (
        <div
          role="dialog"
          aria-label="Active Sessions"
          className="absolute right-0 top-full mt-1.5 w-80 max-h-[480px] bg-ink-900 border border-line rounded-xl shadow-2xl z-50 flex flex-col overflow-hidden"
        >
          {/* Header */}
          <div className="px-3 py-2.5 border-b border-line bg-ink-850">
            <div className="text-[12px] text-white font-medium">
              Active Sessions
            </div>
            <div className="text-[10px] text-muted mt-0.5">
              Nurse-monitored
              {stats.monitored_count !== sessionsLen && config.enabled && (
                <span> · {stats.monitored_count} tracked</span>
              )}
            </div>
          </div>

          {/* Sessions list */}
          {sortedSessions.length > 0 ? (
            <div className="flex-1 overflow-y-auto min-h-0">
              {sortedSessions.map((s) => (
                <SessionCard key={s.session_id} session={s} />
              ))}
            </div>
          ) : (
            <div className="px-3 py-6 text-center text-[12px] text-muted">
              No active sessions
            </div>
          )}
        </div>
      )}
    </div>
  );
}

export const SessionsDropdown = React.memo(SessionsDropdownInner);
