// Persistence + ordering helpers for the topbar pill cluster.
//
// The pill order is user-controlled via drag-and-drop in
// `ExtensionTopbarSlot`. We persist the chosen order to
// `localStorage` under a stable key. Everything here is pure and
// failure-tolerant — the topbar must never crash because of a
// malformed or unwritable storage entry.

import type { SnapshotEntry } from "./types";

const STORAGE_KEY = "hyvemind:extension-topbar-order";

/** Read the saved pill ID order from localStorage. Returns `[]` on
 *  any failure (missing entry, JSON parse error, wrong shape, no
 *  storage available). */
export function loadOrder(): string[] {
  try {
    if (typeof localStorage === "undefined") return [];
    const raw = localStorage.getItem(STORAGE_KEY);
    if (!raw) return [];
    const parsed = JSON.parse(raw);
    if (!Array.isArray(parsed)) return [];
    if (!parsed.every((x) => typeof x === "string")) return [];
    return parsed as string[];
  } catch {
    return [];
  }
}

/** Persist the pill ID order. Swallows quota / unavailable-storage
 *  errors so the UX continues to work even when persistence fails. */
export function saveOrder(ids: string[]): void {
  try {
    if (typeof localStorage === "undefined") return;
    localStorage.setItem(STORAGE_KEY, JSON.stringify(ids));
  } catch (err) {
    // eslint-disable-next-line no-console
    console.warn("[extensions] failed to persist topbar order:", err);
  }
}

/** Return `visible` re-ordered by `saved`:
 *   1. entries whose `manifest.id` is in `saved`, in `saved` order,
 *   2. then unknown entries (not in `saved`) sorted alphabetically
 *      by `manifest.id`.
 *  Stale IDs in `saved` (no longer present in `visible`) are dropped. */
export function applyOrder(
  visible: SnapshotEntry[],
  saved: string[],
): SnapshotEntry[] {
  const byId = new Map<string, SnapshotEntry>();
  for (const entry of visible) byId.set(entry.manifest.id, entry);

  const ordered: SnapshotEntry[] = [];
  const placed = new Set<string>();
  for (const id of saved) {
    const entry = byId.get(id);
    if (entry && !placed.has(id)) {
      ordered.push(entry);
      placed.add(id);
    }
  }

  const remaining = visible
    .filter((e) => !placed.has(e.manifest.id))
    .sort((a, b) => a.manifest.id.localeCompare(b.manifest.id));

  return [...ordered, ...remaining];
}
