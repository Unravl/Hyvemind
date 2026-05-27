import { useCallback, useEffect, useState } from "react";
import * as ipc from "../lib/ipc";
import { isTauri } from "../lib/tauri";
import { formatIpcError } from "../lib/ipc";
import type { NurseProfile, ProfileConfig, Severity } from "../lib/nurseTypes";

/**
 * Per-profile config CRUD with optimistic updates + revert-on-error.
 *
 * `patch(updater)` applies the updater immediately to local state,
 * then fires `set_nurse_profile`. On rejection, the previous
 * snapshot is restored and the error is surfaced via `lastError`
 * (toast handling lives at the call site).
 */
interface Result {
  config: ProfileConfig | null;
  isLoading: boolean;
  isSaving: boolean;
  error: string | null;
  lastError: string | null;
  patch: (
    updater: (prev: ProfileConfig) => ProfileConfig,
  ) => Promise<boolean>;
  reload: () => Promise<void>;
  resetToDefaults: () => Promise<boolean>;
}

const DEFAULT_BUDGET = {
  initial_cap: 6,
  decay_per_hour: 3,
  max_cap: 12,
  per_detector_cap: 3,
  per_key_cooldown_secs: 120,
};

function defaultsFor(profile: NurseProfile): ProfileConfig {
  // Mirrors `ProfileConfig::default_for(profile)` on the Rust side
  // closely enough for the optimistic / pre-backend cases. Real
  // defaults are authoritative on the backend.
  const escalation: Severity =
    profile === "test" ? "warn" : profile === "hivemind" ? "stalled" : "warn";
  return {
    enabled: true,
    intervention_mode: "auto",
    escalation_min_severity: escalation,
    budget: { ...DEFAULT_BUDGET },
    detectors: {},
  };
}

export function useNurseProfile(profile: NurseProfile): Result {
  const [config, setConfig] = useState<ProfileConfig | null>(null);
  const [isLoading, setIsLoading] = useState(true);
  const [isSaving, setIsSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [lastError, setLastError] = useState<string | null>(null);

  const reload = useCallback(async () => {
    if (!isTauri()) {
      setConfig(defaultsFor(profile));
      setIsLoading(false);
      return;
    }
    setIsLoading(true);
    setError(null);
    try {
      const c = await ipc.getNurseProfile(profile);
      setConfig(c);
    } catch (err) {
      // Backend not wired yet — show defaults so the UI still renders.
      setError(formatIpcError(err));
      setConfig(defaultsFor(profile));
    } finally {
      setIsLoading(false);
    }
  }, [profile]);

  useEffect(() => {
    reload();
  }, [reload]);

  const patch = useCallback(
    async (updater: (prev: ProfileConfig) => ProfileConfig): Promise<boolean> => {
      const prev = config;
      if (!prev) return false;
      const next = updater(prev);
      // Optimistic apply.
      setConfig(next);
      setIsSaving(true);
      setLastError(null);
      try {
        await ipc.setNurseProfile(profile, next);
        return true;
      } catch (err) {
        // Revert + surface.
        setConfig(prev);
        setLastError(formatIpcError(err));
        return false;
      } finally {
        setIsSaving(false);
      }
    },
    [config, profile],
  );

  const resetToDefaults = useCallback(async (): Promise<boolean> => {
    const defaults = defaultsFor(profile);
    return patch(() => defaults);
  }, [patch, profile]);

  return {
    config,
    isLoading,
    isSaving,
    error,
    lastError,
    patch,
    reload,
    resetToDefaults,
  };
}
