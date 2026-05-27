import React, { createContext, useContext, useEffect, useMemo, useState, useCallback } from "react";
import * as ipc from "../lib/ipc";
import { isTauri } from "../lib/tauri";
import { formatIpcError } from "../lib/ipc";
import type { DetectorSchema } from "../lib/nurseTypes";

/**
 * App-level Nurse context. Provides the screen-wide time-range
 * filter (so all four tabs share it) and a memoized cache of the
 * registered detector schemas (`get_nurse_detector_schemas`) so the
 * Profiles tab doesn't refetch on every profile sub-tab switch.
 */
export type NurseTimeRange = "1h" | "24h" | "7d" | "all";

interface NurseCtx {
  timeRange: NurseTimeRange;
  setTimeRange: (r: NurseTimeRange) => void;
  schemas: DetectorSchema[];
  schemasLoading: boolean;
  schemasError: string | null;
  refreshSchemas: () => Promise<void>;
}

const NurseContext = createContext<NurseCtx | null>(null);

export function useNurseCtx(): NurseCtx {
  const ctx = useContext(NurseContext);
  if (!ctx)
    throw new Error("useNurseCtx must be used within a <NurseProvider>");
  return ctx;
}

export function NurseProvider({ children }: { children: React.ReactNode }) {
  const [timeRange, setTimeRange] = useState<NurseTimeRange>("24h");
  const [schemas, setSchemas] = useState<DetectorSchema[]>([]);
  const [schemasLoading, setSchemasLoading] = useState(true);
  const [schemasError, setSchemasError] = useState<string | null>(null);

  const refreshSchemas = useCallback(async () => {
    if (!isTauri()) {
      setSchemas([]);
      setSchemasLoading(false);
      return;
    }
    setSchemasLoading(true);
    setSchemasError(null);
    try {
      const s = await ipc.getNurseDetectorSchemas();
      setSchemas(s);
    } catch (err) {
      // Backend not wired — return empty so screens degrade gracefully.
      setSchemasError(formatIpcError(err));
      setSchemas([]);
    } finally {
      setSchemasLoading(false);
    }
  }, []);

  useEffect(() => {
    refreshSchemas();
  }, [refreshSchemas]);

  const value = useMemo<NurseCtx>(
    () => ({
      timeRange,
      setTimeRange,
      schemas,
      schemasLoading,
      schemasError,
      refreshSchemas,
    }),
    [timeRange, schemas, schemasLoading, schemasError, refreshSchemas],
  );

  return (
    <NurseContext.Provider value={value}>{children}</NurseContext.Provider>
  );
}
