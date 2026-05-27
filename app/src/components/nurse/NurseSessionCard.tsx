import { useEffect, useRef } from "react";
import type { DerivedSession } from "../../hooks/useNurseSessions";
import type { SessionOwnerDto, Tier } from "../../lib/nurseTypes";
import { Pill } from "../atoms";
import { SignalChip } from "./SignalChip";

const TIER_BORDER: Record<Tier, string> = {
  quiet: "border-emerald-500/30",
  warning: "border-amber-400/40",
  stalled: "border-red-400/50",
  critical: "border-red-500 pulse-red",
};

const TIER_LABEL: Record<Tier, string> = {
  quiet: "Healthy",
  warning: "Warning",
  stalled: "Stalled",
  critical: "Critical",
};

function formatAge(ms: number): string {
  const secs = Math.floor(ms / 1000);
  if (secs < 60) return `${secs}s`;
  if (secs < 3600) return `${Math.floor(secs / 60)}m`;
  return `${Math.floor(secs / 3600)}h`;
}

function ownerLabel(owner: SessionOwnerDto | undefined): string {
  if (!owner) return "Unknown";
  switch (owner.kind) {
    case "task":
      return `Task ${owner.task_id.slice(0, 8)}`;
    case "review":
      return `Review ${owner.job_id.slice(0, 8)}`;
    case "merge":
      return `Merge ${owner.job_id.slice(0, 8)} · r${owner.round}`;
    case "swarm":
      return [
        `Swarm ${owner.swarm_id.slice(0, 8)}`,
        owner.role,
        owner.feature_id,
      ]
        .filter(Boolean)
        .join(" · ");
    case "unknown":
      return "Unknown";
  }
}

function ownerTone(
  owner: SessionOwnerDto | undefined,
): "honey" | "blue" | "violet" | "green" | "neutral" {
  if (!owner) return "neutral";
  switch (owner.kind) {
    case "task":
      return "honey";
    case "review":
      return "blue";
    case "merge":
      return "violet";
    case "swarm":
      return "green";
    default:
      return "neutral";
  }
}

/**
 * Single live-session card. Color border tracks the highest active
 * signal severity (tier). Announces tier transitions via
 * `aria-live="polite"`.
 */
export function NurseSessionCard({
  derived,
  onOpenDetail,
}: {
  derived: DerivedSession;
  onOpenDetail: (sessionId: string) => void;
}) {
  const { session, tier, age_ms } = derived;
  const signals = session.active_signals || [];
  const prevTierRef = useRef<Tier>(tier);

  // Surface tier transitions via aria-live so screen readers get a
  // polite cue when a session escalates. The announcer span is
  // visually hidden.
  useEffect(() => {
    prevTierRef.current = tier;
  }, [tier]);

  const announce =
    prevTierRef.current !== tier
      ? `Session ${session.session_id.slice(0, 8)} is now ${TIER_LABEL[tier]}.`
      : "";

  return (
    <button
      type="button"
      onClick={() => onOpenDetail(session.session_id)}
      data-testid="nurse-session-card"
      data-tier={tier}
      className={`text-left w-full rounded-xl border-2 bg-ink-850 p-3 card-hover transition focus:outline-none focus-visible:ring-2 focus-visible:ring-honey-500 ${TIER_BORDER[tier]}`}
    >
      <div aria-live="polite" className="sr-only">
        {announce}
      </div>

      <div className="flex items-start justify-between gap-2">
        <div className="min-w-0">
          <div className="flex items-center gap-1.5 mb-1 flex-wrap">
            <Pill tone={ownerTone(session.owner)}>
              {ownerLabel(session.owner)}
            </Pill>
            <span className="text-[10px] text-dim font-mono">
              {TIER_LABEL[tier]}
            </span>
          </div>
          {session.model && (
            <div className="text-[11px] text-muted font-mono truncate">
              {session.model}
            </div>
          )}
          {session.project_path && (
            <div className="text-[10px] text-dim truncate" title={session.project_path}>
              {session.project_path}
            </div>
          )}
        </div>
        {session.intervention_count > 0 && (
          <span
            className="text-[10px] text-amber-300 bg-amber-500/15 px-1.5 py-0.5 rounded shrink-0"
            title={`${session.intervention_count} interventions on this session`}
          >
            {session.intervention_count}
          </span>
        )}
      </div>

      {signals.length > 0 && (
        <div className="mt-2 flex flex-wrap gap-1">
          {signals.slice(0, 6).map((s) => (
            <SignalChip key={s.dedup_key} signal={s} />
          ))}
          {signals.length > 6 && (
            <span className="text-[10px] text-dim self-center">
              +{signals.length - 6} more
            </span>
          )}
        </div>
      )}

      <div className="mt-2 flex items-center gap-2 text-[10px] text-dim">
        <span>last activity {formatAge(age_ms)} ago</span>
        <span aria-hidden="true">·</span>
        <span>{session.event_count} events</span>
      </div>
    </button>
  );
}
