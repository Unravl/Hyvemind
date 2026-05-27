import { useEffect, useMemo, useState } from "react";
import * as ipc from "../lib/ipc";
import { isTauri } from "../lib/tauri";
import { subscribe as subscribeNurse } from "../lib/nurseEventStore";
import type {
  MonitoredSessionSnapshot,
  NurseEvent,
  Severity,
  Tier,
} from "../lib/nurseTypes";

/**
 * Live monitored-sessions list. Polls `get_nurse_status` every 30s as
 * a safety net and patches in place on `nurse-event`. Derives a
 * `tier` per session from the highest active signal severity so
 * consumers don't repeat the same fold.
 */
export interface DerivedSession {
  session: MonitoredSessionSnapshot;
  tier: Tier;
  /** Rounded age (ms since `last_activity_ms`) — re-derived on every
   *  render so it ticks. */
  age_ms: number;
}

const TIER_BY_SEVERITY: Record<Severity, Tier> = {
  info: "quiet",
  warn: "warning",
  stalled: "stalled",
  critical: "critical",
};

function deriveTier(s: MonitoredSessionSnapshot): Tier {
  // Highest-severity active signal wins. Fall back to the
  // legacy `status` field if `highest_severity` isn't populated by
  // the backend yet (pre-rewrite wire shape).
  if (s.highest_severity) return TIER_BY_SEVERITY[s.highest_severity];
  switch (s.status) {
    case "stalled":
      return "stalled";
    case "warning":
    case "intervening":
      return "warning";
    case "failed":
      return "critical";
    default:
      return "quiet";
  }
}

export function useNurseSessions() {
  const [sessions, setSessions] = useState<MonitoredSessionSnapshot[]>([]);
  const [isLoading, setIsLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  // Used purely to force a re-derive of `age_ms` every second so the
  // UI clock ticks without re-fetching from the backend.
  const [, setTick] = useState(0);

  useEffect(() => {
    if (!isTauri()) {
      setIsLoading(false);
      return;
    }
    let cancelled = false;
    const load = async () => {
      try {
        const s = await ipc.getNurseStatus();
        if (!cancelled) {
          setSessions(s.sessions || []);
          setError(null);
        }
      } catch (err) {
        if (!cancelled) {
          setError(err instanceof Error ? err.message : String(err));
        }
      } finally {
        if (!cancelled) setIsLoading(false);
      }
    };

    load();
    const pollId = setInterval(load, 30_000);
    const tickId = setInterval(() => setTick((t) => t + 1), 1000);

    const unsubscribe = subscribeNurse((event: NurseEvent) => {
      if (event.event_type === "StatusUpdate") {
        setSessions(event.sessions || []);
      }
      // Intervention/UserNotice/Lifecycle don't carry a fresh
      // sessions[] array; the 30s poll picks them up.
    });

    return () => {
      cancelled = true;
      clearInterval(pollId);
      clearInterval(tickId);
      unsubscribe();
    };
  }, []);

  const derived: DerivedSession[] = useMemo(() => {
    const now = Date.now();
    return sessions.map((s) => ({
      session: s,
      tier: deriveTier(s),
      age_ms: Math.max(0, now - s.last_activity_ms),
    }));
  }, [sessions]);

  return { sessions: derived, isLoading, error };
}
