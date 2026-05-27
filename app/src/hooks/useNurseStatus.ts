import { useState, useEffect, useCallback } from "react";
import type { NurseStatusSnapshot, NurseEvent } from "../types/nurse";
import { isTauri } from "../lib/tauri";
import * as ipc from "../lib/ipc";
import { subscribe as subscribeNurse } from "../lib/nurseEventStore";

const EMPTY_SNAPSHOT: NurseStatusSnapshot = {
  stats: {
    monitored_count: 0,
    stall_count: 0,
    intervention_count: 0,
    last_check_at: null,
    is_running: false,
  },
  sessions: [],
  recent_interventions: [],
  config: {
    enabled: true,
    stall_threshold_secs: 300,
    nurse_model: "anthropic/claude-haiku-4.5",
    max_interventions: 3,
    tick_interval_secs: 60,
    nurse_provider: null,
  },
  health: {
    last_tick_at: null,
    last_successful_tick_at: null,
    consecutive_failed_ticks: 0,
    consecutive_bad_parse_ticks: 0,
    consecutive_skipped_ticks: 0,
    degraded: false,
  },
};

/**
 * Live nurse status hook. Multi-consumer-safe: the underlying Tauri
 * listener is registered exactly once via the singleton
 * `nurseEventStore`, regardless of how many `useNurseStatus()` call
 * sites mount (Settings link + Topbar dropdown + Nurse screen can all
 * mount simultaneously without doubling events).
 */
export function useNurseStatus() {
  const [status, setStatus] = useState<NurseStatusSnapshot>(EMPTY_SNAPSHOT);
  const [isLoading, setIsLoading] = useState(true);

  const refresh = useCallback(async () => {
    if (!isTauri()) return;
    try {
      const s = await ipc.getNurseStatus();
      setStatus(s);
    } catch (err) {
      console.error("Failed to fetch nurse status:", err);
    } finally {
      setIsLoading(false);
    }
  }, []);

  useEffect(() => {
    refresh();

    if (!isTauri()) return;

    // Poll every 30s as a safety net — ensures recovery even if an
    // event is missed (e.g., tick early-return paths, race conditions).
    const pollId = setInterval(() => refresh(), 30_000);

    const unsubscribe = subscribeNurse((event: NurseEvent) => {
      setStatus((prev) => {
        switch (event.event_type) {
          case "StatusUpdate": {
            const { event_type: _evt, ...snapshot } = event;
            return snapshot as unknown as NurseStatusSnapshot;
          }
          case "Intervention": {
            const { event_type: _evt, ...record } = event;
            return {
              ...prev,
              recent_interventions: [
                record as unknown as NurseStatusSnapshot["recent_interventions"][0],
                ...prev.recent_interventions,
              ],
            };
          }
          case "UserNotice":
            // User-facing notice is for toast notifications; the
            // status snapshot itself does not change. Surface via a
            // window event so the toast layer can pick it up.
            try {
              window.dispatchEvent(
                new CustomEvent("hyvemind:nurse-user-notice", {
                  detail: event,
                }),
              );
            } catch {
              /* ignore */
            }
            return prev;
          default:
            return prev;
        }
      });
    });

    return () => {
      clearInterval(pollId);
      unsubscribe();
    };
  }, [refresh]);

  return { status, isLoading, refresh };
}
