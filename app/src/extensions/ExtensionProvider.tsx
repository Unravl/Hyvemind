import React, {
  createContext,
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
} from "react";
import * as ipc from "../lib/ipc";
import { onUsageSnapshotUpdated, safeUnlisten } from "../lib/events";
import { isTauri } from "../lib/tauri";
import type {
  ExtensionUserSettings,
  SnapshotEntry,
  UsageSnapshotEvent,
} from "./types";

interface ExtensionContextValue {
  snapshots: SnapshotEntry[];
  isLoading: boolean;
  /** Force an immediate fetch for one extension. */
  refresh: (extensionId: string) => Promise<void>;
  /** Update per-extension user settings (enabled / show_in_topbar / preferences). */
  updateSettings: (
    extensionId: string,
    settings: Partial<ExtensionUserSettings>,
  ) => Promise<void>;
}

const Ctx = createContext<ExtensionContextValue | null>(null);

/** Internal export — `useExtensions` consumes this. */
export const ExtensionReactContext = Ctx;

const EMPTY: SnapshotEntry[] = [];

/** Safety-net polling interval — mirrors `useNurseStatus`. Catches the
 *  edge case where a `usage-snapshot-updated` event is missed (e.g.
 *  webview backgrounded for a long time). */
const SAFETY_POLL_MS = 60_000;

export function ExtensionProvider({ children }: { children: React.ReactNode }) {
  const [snapshots, setSnapshots] = useState<SnapshotEntry[]>(EMPTY);
  const [isLoading, setIsLoading] = useState(true);
  // Track the last-known entries by id so the event handler can do
  // partial updates without a full re-fetch.
  const byIdRef = useRef<Map<string, SnapshotEntry>>(new Map());

  const reload = useCallback(async () => {
    if (!isTauri()) {
      setIsLoading(false);
      return;
    }
    try {
      const list = await ipc.getUsageSnapshots();
      byIdRef.current = new Map(list.map((e) => [e.manifest.id, e]));
      setSnapshots(list);
    } catch (err) {
      console.error("Failed to load usage snapshots:", err);
    } finally {
      setIsLoading(false);
    }
  }, []);

  useEffect(() => {
    if (!isTauri()) {
      setIsLoading(false);
      return;
    }
    reload();

    const pollId = window.setInterval(() => {
      void reload();
    }, SAFETY_POLL_MS);

    let unlisten: (() => void) | undefined;
    onUsageSnapshotUpdated((evt: UsageSnapshotEvent) => {
      // The event payload omits `raw`. Merge with the cached entry so
      // existing snapshot.raw (if any) is preserved; consumers needing
      // raw call `get_usage_snapshots()` explicitly.
      setSnapshots((prev) => {
        const map = new Map(byIdRef.current);
        const existing = map.get(evt.extension_id);
        if (!existing) {
          // Unknown id — schedule a full reload to pick up the new entry.
          void reload();
          return prev;
        }
        const merged: SnapshotEntry = {
          ...existing,
          status: evt.status,
          last_error: evt.error ?? null,
        };
        if (evt.snapshot) {
          merged.snapshot = {
            ...evt.snapshot,
            // raw is intentionally omitted from the event payload.
            raw: existing.snapshot?.raw ?? null,
          };
          merged.last_fetched_at = evt.snapshot.fetched_at;
        } else if (evt.status === "disabled") {
          merged.snapshot = null;
        }
        map.set(evt.extension_id, merged);
        byIdRef.current = map;
        return Array.from(map.values()).sort((a, b) =>
          a.manifest.id.localeCompare(b.manifest.id),
        );
      });
    }).then((u) => {
      unlisten = u;
    });

    return () => {
      clearInterval(pollId);
      safeUnlisten(unlisten);
    };
  }, [reload]);

  const refresh = useCallback(async (extensionId: string) => {
    if (!isTauri()) return;
    try {
      const entry = await ipc.refreshUsageSnapshot(extensionId);
      setSnapshots((prev) => {
        const map = new Map(byIdRef.current);
        map.set(entry.manifest.id, entry);
        byIdRef.current = map;
        return Array.from(map.values()).sort((a, b) =>
          a.manifest.id.localeCompare(b.manifest.id),
        );
      });
    } catch (err) {
      // Rejected cooldowns and in-flight collisions surface here; surface
      // to console but don't blow up the UI.
      console.warn(`refreshUsageSnapshot(${extensionId}) failed:`, err);
      throw err;
    }
  }, []);

  const updateSettings = useCallback(
    async (extensionId: string, settings: Partial<ExtensionUserSettings>) => {
      if (!isTauri()) return;
      await ipc.updateExtensionSettings(
        extensionId,
        settings.enabled,
        settings.show_in_topbar,
        settings.preferences,
      );
      // Eagerly patch the local state so the toggle is instantly
      // reflected; the `usage-snapshot-updated` event will land
      // shortly with the authoritative status.
      setSnapshots((prev) => {
        const map = new Map(byIdRef.current);
        const existing = map.get(extensionId);
        if (!existing) return prev;
        map.set(extensionId, {
          ...existing,
          user_settings: { ...existing.user_settings, ...settings },
        });
        byIdRef.current = map;
        return Array.from(map.values()).sort((a, b) =>
          a.manifest.id.localeCompare(b.manifest.id),
        );
      });
    },
    [],
  );

  const value = useMemo<ExtensionContextValue>(
    () => ({ snapshots, isLoading, refresh, updateSettings }),
    [snapshots, isLoading, refresh, updateSettings],
  );

  return <Ctx.Provider value={value}>{children}</Ctx.Provider>;
}
