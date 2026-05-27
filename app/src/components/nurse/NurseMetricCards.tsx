import { useMemo } from "react";
import type {
  MonitoredSessionSnapshot,
  NurseInterventionRecord,
  Severity,
  DetectorStatsRow,
} from "../../lib/nurseTypes";
import { SEVERITY_DOT, SEVERITY_LABEL } from "./SeverityBadge";

/**
 * The 4 at-a-glance cards at the top of the Nurse screen. Each card
 * uses `aria-live="polite"` per A11Y.md — status updates shouldn't
 * interrupt the user.
 */
export function NurseMetricCards({
  sessions,
  interventionsInRange,
  detectorStats,
}: {
  sessions: MonitoredSessionSnapshot[];
  interventionsInRange: NurseInterventionRecord[];
  detectorStats: DetectorStatsRow[];
}) {
  const monitoredCount = sessions.length;

  // Active concerns: count + severity breakdown.
  const concernsBySeverity = useMemo(() => {
    const map: Record<Severity, number> = {
      info: 0,
      warn: 0,
      stalled: 0,
      critical: 0,
    };
    for (const s of sessions) {
      for (const sig of s.active_signals ?? []) {
        map[sig.severity] += 1;
      }
    }
    return map;
  }, [sessions]);
  const totalConcerns =
    concernsBySeverity.info +
    concernsBySeverity.warn +
    concernsBySeverity.stalled +
    concernsBySeverity.critical;

  // Action-type breakdown of interventions.
  const interventionsByAction = useMemo(() => {
    const map: Record<string, number> = {};
    for (const i of interventionsInRange) {
      const level = (i.action_taken?.level || i.level || "unknown").toLowerCase();
      map[level] = (map[level] ?? 0) + 1;
    }
    return map;
  }, [interventionsInRange]);

  // Detector effectiveness — top 4 by total signals raised.
  const effectiveness = useMemo(() => {
    return [...detectorStats]
      .sort((a, b) => b.total - a.total)
      .slice(0, 4);
  }, [detectorStats]);

  return (
    <div className="grid grid-cols-1 md:grid-cols-2 xl:grid-cols-4 gap-3">
      <Card
        title="Sessions Monitored"
        primary={String(monitoredCount)}
        secondary={`${sessions.filter((s) => s.is_busy).length} busy`}
      />

      <Card
        title="Active Concerns"
        primary={String(totalConcerns)}
        secondary={
          totalConcerns === 0 ? "All quiet" : "Distribution below"
        }
        footer={
          totalConcerns > 0 ? (
            <SeverityBar counts={concernsBySeverity} total={totalConcerns} />
          ) : null
        }
      />

      <Card
        title="Interventions in Range"
        primary={String(interventionsInRange.length)}
        secondary={Object.entries(interventionsByAction)
          .map(([k, v]) => `${k} ${v}`)
          .join(" · ") || "—"}
      />

      <Card
        title="Per-Detector Effectiveness"
        primary={String(effectiveness.length)}
        secondary={
          effectiveness.length === 0
            ? "No data yet"
            : `${effectiveness.length} active detectors`
        }
        footer={
          effectiveness.length > 0 ? (
            <ul className="text-[10.5px] text-muted space-y-0.5 mt-1.5">
              {effectiveness.map((d) => (
                <li key={d.detector} className="flex items-center gap-1">
                  <span className="font-mono text-white/80 truncate flex-1">
                    {d.detector}
                  </span>
                  <span className="text-dim">{d.total}</span>
                  {d.fp_count != null && d.fp_count > 0 && (
                    <span className="text-amber-300/80" title="false positives">
                      ⚠{d.fp_count}
                    </span>
                  )}
                </li>
              ))}
            </ul>
          ) : null
        }
      />
    </div>
  );
}

function Card({
  title,
  primary,
  secondary,
  footer,
}: {
  title: string;
  primary: string;
  secondary?: string;
  footer?: React.ReactNode;
}) {
  return (
    <div
      aria-live="polite"
      className="rounded-xl border border-line bg-ink-850 p-3 shadow-panel"
    >
      <div className="text-[10px] text-dim uppercase tracking-wider">
        {title}
      </div>
      <div className="text-2xl font-semibold text-white mt-1 font-mono">
        {primary}
      </div>
      {secondary && (
        <div className="text-[11px] text-muted mt-0.5">{secondary}</div>
      )}
      {footer}
    </div>
  );
}

function SeverityBar({
  counts,
  total,
}: {
  counts: Record<Severity, number>;
  total: number;
}) {
  const order: Severity[] = ["critical", "stalled", "warn", "info"];
  return (
    <div className="mt-2">
      <div className="flex h-1.5 rounded overflow-hidden border border-line">
        {order.map((sev) => {
          const pct = total === 0 ? 0 : (counts[sev] / total) * 100;
          if (pct === 0) return null;
          return (
            <span
              key={sev}
              className={SEVERITY_DOT[sev]}
              style={{ width: `${pct}%` }}
              title={`${SEVERITY_LABEL[sev]}: ${counts[sev]}`}
            />
          );
        })}
      </div>
    </div>
  );
}
