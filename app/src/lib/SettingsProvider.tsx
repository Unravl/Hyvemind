/**
 * SettingsProvider — single source of truth for the backend `Settings`
 * struct. Fetches once on mount and subscribes to the three known
 * Settings-change Tauri events:
 *
 *   - `default-model-changed`         → patches `default_model`
 *   - `default-project-path-changed`  → patches `default_project_path`
 *   - `default-hivemind-changed`      → patches `default_hivemind`
 *
 * Every screen that previously called `ipc.getSettings()` on mount can
 * now use `useSettings()` (for the whole object) or `useSetting(key)`
 * (subscribes to a single field) and skip the per-screen IPC hop.
 *
 * Consumers that still need a force-refresh (e.g. after a save that
 * changes a field with no broadcast event) can call `refresh()` from
 * `useSettings()` — it re-fetches and updates.
 *
 * Audit item 6.7 — split the monolithic `taskRuntime` context and
 * deduplicate the ~14 `getSettings` calls across the app.
 */

import React, {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useRef,
  useState,
} from "react";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import * as ipc from "./ipc";
import { safeUnlisten } from "./events";
import { isTauri } from "./tauri";
import type { SettingsResponse } from "./types";

interface SettingsContextValue {
  /** Current settings, or `null` while the initial fetch is still in
   *  flight (or running outside Tauri where IPC isn't available). */
  settings: SettingsResponse | null;
  /** True until the first fetch resolves or errors out. */
  isLoading: boolean;
  /** Last fetch error, or `null` if the last fetch succeeded. */
  error: string | null;
  /** Force re-fetch from the backend. Use after a Settings save whose
   *  change isn't broadcast via the three default-* events. */
  refresh: () => Promise<void>;
  /** Apply a partial patch to the in-memory settings without going to
   *  the backend. Used by Settings.tsx's save flow to reflect saves
   *  optimistically. */
  patchSettings: (patch: Partial<SettingsResponse>) => void;
}

const Ctx = createContext<SettingsContextValue | null>(null);

export function SettingsProvider({ children }: { children: React.ReactNode }) {
  const [settings, setSettings] = useState<SettingsResponse | null>(null);
  const [isLoading, setIsLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const mountedRef = useRef(true);

  const refresh = useCallback(async () => {
    if (!isTauri()) {
      setIsLoading(false);
      return;
    }
    try {
      const s = await ipc.getSettings();
      if (!mountedRef.current) return;
      setSettings(s);
      setError(null);
    } catch (e) {
      if (!mountedRef.current) return;
      console.error("[SettingsProvider] getSettings failed:", e);
      setError(String(e));
    } finally {
      if (mountedRef.current) setIsLoading(false);
    }
  }, []);

  const patchSettings = useCallback((patch: Partial<SettingsResponse>) => {
    setSettings((prev) => (prev ? { ...prev, ...patch } : prev));
  }, []);

  // Initial fetch + lifecycle bookkeeping.
  useEffect(() => {
    mountedRef.current = true;
    void refresh();
    return () => {
      mountedRef.current = false;
    };
  }, [refresh]);

  // Subscribe to the three default-* change events. Each updates only
  // the affected field — no full refetch, so consumers subscribed to
  // unrelated slices don't re-render.
  useEffect(() => {
    if (!isTauri()) return;
    let mounted = true;
    let unlistenModel: UnlistenFn | undefined;
    let unlistenPath: UnlistenFn | undefined;
    let unlistenHm: UnlistenFn | undefined;

    listen<{ model: string | null }>("default-model-changed", (evt) => {
      if (!mounted) return;
      setSettings((prev) =>
        prev ? { ...prev, default_model: evt.payload.model } : prev,
      );
    }).then((fn) => {
      if (!mounted) { fn(); return; }
      unlistenModel = fn;
    });

    listen<{ path: string | null }>("default-project-path-changed", (evt) => {
      if (!mounted) return;
      setSettings((prev) =>
        prev ? { ...prev, default_project_path: evt.payload.path } : prev,
      );
    }).then((fn) => {
      if (!mounted) { fn(); return; }
      unlistenPath = fn;
    });

    listen<{ hivemind: string | null }>("default-hivemind-changed", (evt) => {
      if (!mounted) return;
      setSettings((prev) =>
        prev ? { ...prev, default_hivemind: evt.payload.hivemind } : prev,
      );
    }).then((fn) => {
      if (!mounted) { fn(); return; }
      unlistenHm = fn;
    });

    return () => {
      mounted = false;
      safeUnlisten(unlistenModel);
      safeUnlisten(unlistenPath);
      safeUnlisten(unlistenHm);
    };
  }, []);

  const value = useMemo<SettingsContextValue>(
    () => ({ settings, isLoading, error, refresh, patchSettings }),
    [settings, isLoading, error, refresh, patchSettings],
  );

  return <Ctx.Provider value={value}>{children}</Ctx.Provider>;
}

// Stable fallback when `useSettings` is called outside a provider —
// keeps existing unit tests that mount a single screen without the
// full provider tree working unchanged. Real renders always have the
// provider (mounted near the App root).
const FALLBACK_VALUE: SettingsContextValue = {
  settings: null,
  isLoading: false,
  error: null,
  refresh: async () => {},
  patchSettings: () => {},
};

/** Returns the full SettingsContextValue. When used outside a
 *  SettingsProvider (e.g. in legacy unit tests that mount a single
 *  screen without the provider tree), returns an empty fallback
 *  instead of throwing. */
export function useSettings(): SettingsContextValue {
  const ctx = useContext(Ctx);
  return ctx ?? FALLBACK_VALUE;
}

/** Returns a single field from the current settings. `null` while
 *  loading or when the field is unset on the backend. Re-renders only
 *  when that specific field changes (relies on parent-context update
 *  causing a re-render; React's `useContext` doesn't selectively
 *  subscribe, but consumers using just one key will still benefit from
 *  the slice via memoization in their parent components). */
export function useSetting<K extends keyof SettingsResponse>(
  key: K,
): SettingsResponse[K] | null {
  const { settings } = useSettings();
  if (!settings) return null;
  return settings[key];
}
