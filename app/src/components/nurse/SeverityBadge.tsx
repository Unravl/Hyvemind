import type { Severity } from "../../lib/nurseTypes";

/* ── Severity palette ────────────────────────────────────────
 * Per the plan: Info = dim, Warn = amber-400, Stalled = red-400,
 * Critical = red-500 + pulse-red CSS utility.
 */
export const SEVERITY_DOT: Record<Severity, string> = {
  info: "bg-dim",
  warn: "bg-amber-400",
  stalled: "bg-red-400",
  critical: "bg-red-500",
};

export const SEVERITY_TEXT: Record<Severity, string> = {
  info: "text-dim",
  warn: "text-amber-400",
  stalled: "text-red-400",
  critical: "text-red-500",
};

export const SEVERITY_BG: Record<Severity, string> = {
  info: "bg-ink-700/40",
  warn: "bg-amber-500/10",
  stalled: "bg-red-500/10",
  critical: "bg-red-500/15",
};

export const SEVERITY_LABEL: Record<Severity, string> = {
  info: "Info",
  warn: "Warn",
  stalled: "Stalled",
  critical: "Critical",
};

/**
 * Color-mapped severity indicator. Critical pulses via the global
 * `pulse-red` utility from `index.css`.
 */
export function SeverityBadge({
  severity,
  className = "",
}: {
  severity: Severity;
  className?: string;
}) {
  const pulse = severity === "critical" ? "pulse-red" : "";
  return (
    <span
      className={`inline-flex items-center gap-1 px-1.5 py-0.5 rounded text-[10px] font-medium ${SEVERITY_BG[severity]} ${SEVERITY_TEXT[severity]} ${className}`}
    >
      <span
        className={`w-1.5 h-1.5 rounded-full ${SEVERITY_DOT[severity]} ${pulse}`}
      />
      {SEVERITY_LABEL[severity]}
    </span>
  );
}
