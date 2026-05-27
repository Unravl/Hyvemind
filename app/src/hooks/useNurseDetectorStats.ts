import { useCallback, useEffect, useState } from "react";
import * as ipc from "../lib/ipc";
import { isTauri } from "../lib/tauri";
import { formatIpcError } from "../lib/ipc";
import type { DetectorStatsRow } from "../lib/nurseTypes";

export type NurseTimeRange = "1h" | "24h" | "7d" | "all";

interface Result {
  rows: DetectorStatsRow[];
  isLoading: boolean;
  error: string | null;
  refresh: () => Promise<void>;
}

/**
 * Per-detector aggregates over a time range. Backed by
 * `get_nurse_detector_stats`. Falls back to an empty list with a soft
 * error when the IPC isn't wired (`not_found`).
 */
export function useNurseDetectorStats(timeRange: NurseTimeRange): Result {
  const [rows, setRows] = useState<DetectorStatsRow[]>([]);
  const [isLoading, setIsLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  const refresh = useCallback(async () => {
    if (!isTauri()) {
      setIsLoading(false);
      return;
    }
    setIsLoading(true);
    setError(null);
    try {
      const r = await ipc.getNurseDetectorStats(timeRange);
      setRows(r);
    } catch (err) {
      setError(formatIpcError(err));
      setRows([]);
    } finally {
      setIsLoading(false);
    }
  }, [timeRange]);

  useEffect(() => {
    refresh();
  }, [refresh]);

  return { rows, isLoading, error, refresh };
}
