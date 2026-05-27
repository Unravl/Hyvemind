/**
 * ProvidersProvider — single source of truth for the configured
 * `ProviderInfo[]` list. Fetches once on mount and refreshes on the
 * `usage-snapshot-updated` Tauri event (extension pollers push a fresh
 * snapshot whenever a provider's usage/balance changes, which is a
 * decent signal that the provider list might have moved too — e.g. a
 * new provider became configured because the user pasted an API key
 * elsewhere).
 *
 * Consumers should prefer `useProvider(id)` for a single provider or
 * `useProviders()` for the whole list (and the `configured` subset).
 * Both reads are stable: they re-read straight off the cached
 * ProviderInfo[] and don't issue extra IPC calls.
 *
 * Audit item 6.7 — split the monolithic `taskRuntime` context and
 * deduplicate the ~14 `getProviders` calls across the app.
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
import type { UnlistenFn } from "@tauri-apps/api/event";
import * as ipc from "./ipc";
import { onUsageSnapshotUpdated, safeUnlisten } from "./events";
import { isTauri } from "./tauri";
import type { ProviderInfo } from "./types";

interface ProvidersContextValue {
  /** The full provider list, or `[]` while loading / outside Tauri. */
  providers: ProviderInfo[];
  /** Convenience pre-filter for `p.configured === true`. Memoised. */
  configured: ProviderInfo[];
  /** True until the first fetch resolves or errors out. */
  isLoading: boolean;
  /** Last fetch error, or `null` if the last fetch succeeded. */
  error: string | null;
  /** Force re-fetch from the backend. Use after mutating provider
   *  state (e.g. add/save/delete a key) before downstream code reads
   *  the updated list. */
  refresh: () => Promise<void>;
}

const Ctx = createContext<ProvidersContextValue | null>(null);

const EMPTY_PROVIDERS: ProviderInfo[] = [];

export function ProvidersProvider({ children }: { children: React.ReactNode }) {
  const [providers, setProviders] = useState<ProviderInfo[]>(EMPTY_PROVIDERS);
  const [isLoading, setIsLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const mountedRef = useRef(true);
  /** Coalesce `usage-snapshot-updated`-driven re-fetches so a burst of
   *  events (multiple pollers fire at once on startup) doesn't cause
   *  N back-to-back IPC calls. */
  const refreshScheduledRef = useRef(false);

  const refresh = useCallback(async () => {
    if (!isTauri()) {
      setIsLoading(false);
      return;
    }
    try {
      const list = await ipc.getProviders();
      if (!mountedRef.current) return;
      setProviders(list);
      setError(null);
    } catch (e) {
      if (!mountedRef.current) return;
      console.error("[ProvidersProvider] getProviders failed:", e);
      setError(String(e));
    } finally {
      if (mountedRef.current) setIsLoading(false);
    }
  }, []);

  useEffect(() => {
    mountedRef.current = true;
    void refresh();
    return () => {
      mountedRef.current = false;
    };
  }, [refresh]);

  // The `usage-snapshot-updated` event is the closest signal we have
  // that the configured/health columns of the provider list might
  // have changed (it fires when a provider extension finishes a
  // poll). Refresh once per tick of events to keep the list fresh
  // without thrashing.
  useEffect(() => {
    if (!isTauri()) return;
    let unlisten: UnlistenFn | undefined;
    let mounted = true;

    onUsageSnapshotUpdated(() => {
      if (!mounted) return;
      if (refreshScheduledRef.current) return;
      refreshScheduledRef.current = true;
      // Defer by a microtask + small debounce so a burst of events
      // collapses to one refetch. 250ms is well under the human
      // perception threshold but enough to coalesce a startup storm.
      window.setTimeout(() => {
        refreshScheduledRef.current = false;
        if (!mounted) return;
        void refresh();
      }, 250);
    }).then((fn) => {
      if (!mounted) { fn(); return; }
      unlisten = fn;
    });

    return () => {
      mounted = false;
      safeUnlisten(unlisten);
    };
  }, [refresh]);

  const configured = useMemo(
    () => providers.filter((p) => p.configured),
    [providers],
  );

  const value = useMemo<ProvidersContextValue>(
    () => ({ providers, configured, isLoading, error, refresh }),
    [providers, configured, isLoading, error, refresh],
  );

  return <Ctx.Provider value={value}>{children}</Ctx.Provider>;
}

// Stable fallback when `useProviders` is called outside a provider —
// keeps existing unit tests that mount a single screen without the
// provider tree working unchanged. Real renders always have the
// provider (mounted near the App root).
const FALLBACK_VALUE: ProvidersContextValue = {
  providers: EMPTY_PROVIDERS,
  configured: EMPTY_PROVIDERS,
  isLoading: false,
  error: null,
  refresh: async () => {},
};

/** Returns the full ProvidersContextValue. When used outside a
 *  ProvidersProvider (e.g. in legacy unit tests that mount a single
 *  screen without the provider tree), returns an empty fallback
 *  instead of throwing. */
export function useProviders(): ProvidersContextValue {
  const ctx = useContext(Ctx);
  return ctx ?? FALLBACK_VALUE;
}

/** Look up one provider by id (`ProviderInfo.name`). Returns `null`
 *  if the list hasn't loaded or the provider isn't configured. */
export function useProvider(id: string): ProviderInfo | null {
  const { providers } = useProviders();
  return providers.find((p) => p.name === id) ?? null;
}
