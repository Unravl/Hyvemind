import { useCallback, useSyncExternalStore } from "react";
import { isTauri } from "@tauri-apps/api/core";
import { onHivemindProgress, safeUnlisten } from "./events";
import {
  applyHivemindEvent,
  attributionKeyFromEvent,
  type HivemindReviewState,
  type ReviewState,
} from "./hivemindReducer";
import type { HivemindProgressEvent } from "./types";

/**
 * Singleton store for the `hivemind-progress` Tauri event channel.
 *
 * Mirrors the pattern in `swarmActivityStore.ts`: one global Tauri
 * listener is registered the first time any subscriber attaches, then
 * the fan-out happens in JS. Consumers that need raw events use
 * `subscribeHivemindEventListener`; consumers that just want the
 * derived `ReviewState` for an attribution key use
 * `useHivemindReviewState` (or the lower-level
 * `subscribeHivemindReview` / `getHivemindReviewState`).
 *
 * The store keeps a derived `HivemindReviewState` map keyed by
 * attribution (see `attributionKeyFromEvent`), so multiple Tasks /
 * SwarmControl panels can each subscribe to just the slice they care
 * about without registering their own `listen()` call.
 */

type StateListener = () => void;
type RawListener = (evt: HivemindProgressEvent) => void;

let reviewState: HivemindReviewState = {};
const stateListeners = new Map<string, Set<StateListener>>();
const rawListeners = new Set<RawListener>();

let globalUnlisten: (() => void) | null = null;
let globalListenerPromise: Promise<unknown> | null = null;

function ensureGlobalListener(): void {
  if (globalUnlisten || globalListenerPromise) return;
  if (!isTauri()) return;
  globalListenerPromise = onHivemindProgress((evt) => {
    // 1. Update derived review state and notify keyed subscribers.
    const key = attributionKeyFromEvent(evt);
    const prev = reviewState;
    const next = applyHivemindEvent(prev, evt);
    if (next !== prev) {
      reviewState = next;
      const subs = stateListeners.get(key);
      if (subs) for (const l of subs) l();
    }
    // 2. Fan out raw event to any unfiltered listeners.
    if (rawListeners.size > 0) {
      for (const l of rawListeners) {
        try {
          l(evt);
        } catch (e) {
          console.error("hivemindEventStore raw listener threw", e);
        }
      }
    }
  })
    .then((fn) => {
      globalUnlisten = fn;
    })
    .catch(() => {
      globalListenerPromise = null;
    });
}

/* ── Derived review state (per attribution key) ───────────────── */

const EMPTY_REVIEW_STATE: HivemindReviewState = {};

/**
 * Snapshot of the derived `ReviewState` for a given attribution key.
 * Returns `undefined` if no events have been seen for that key.
 *
 * Keys are produced by `attributionKeyFromEvent`:
 *  - `task:${task_id}`
 *  - `swarm:${swarm_id}:queen`
 *  - `swarm:${swarm_id}:feat:${feature_id}`
 *  - `job:${job_id}` (fallback)
 */
export function getHivemindReviewState(key: string): ReviewState | undefined {
  return reviewState[key];
}

/** Snapshot of the entire review state map (used by tests / debug). */
export function getAllHivemindReviewState(): HivemindReviewState {
  return reviewState;
}

/**
 * Subscribe to changes to a single attribution key's `ReviewState`.
 * Returns an unsubscribe function. Registers the global Tauri
 * listener on first call.
 */
export function subscribeHivemindReview(
  key: string,
  listener: StateListener,
): () => void {
  ensureGlobalListener();
  let set = stateListeners.get(key);
  if (!set) {
    set = new Set();
    stateListeners.set(key, set);
  }
  set.add(listener);
  return () => {
    const s = stateListeners.get(key);
    if (!s) return;
    s.delete(listener);
    if (s.size === 0) stateListeners.delete(key);
  };
}

/**
 * `useSyncExternalStore`-friendly hook returning the live
 * `ReviewState` for the given attribution key (or `undefined`).
 */
export function useHivemindReviewState(
  key: string,
): ReviewState | undefined {
  const subscribe = useCallback(
    (l: StateListener) => subscribeHivemindReview(key, l),
    [key],
  );
  const getSnapshot = useCallback(() => getHivemindReviewState(key), [key]);
  return useSyncExternalStore(subscribe, getSnapshot, getSnapshot);
}

/* ── Raw event subscription (for side-effect consumers) ────────── */

/**
 * Subscribe to the raw `hivemind-progress` event stream. The
 * listener receives every event; do any filtering by `task_id`,
 * `swarm_id`, `job_id`, etc. in the callback. Registers the global
 * Tauri listener on first call.
 *
 * Returns an unsubscribe function. Mirrors the shape of
 * `onHivemindProgress` but multiplexes through the singleton store
 * instead of opening its own Tauri channel.
 */
export function subscribeHivemindEventListener(
  listener: RawListener,
): () => void {
  ensureGlobalListener();
  rawListeners.add(listener);
  return () => {
    rawListeners.delete(listener);
  };
}

/* ── Test helpers ──────────────────────────────────────────────── */

export function _resetHivemindEventStoreForTests(): void {
  reviewState = {};
  stateListeners.clear();
  rawListeners.clear();
  if (globalUnlisten) {
    safeUnlisten(globalUnlisten);
    globalUnlisten = null;
  }
  globalListenerPromise = null;
}

// Re-export for convenience so consumers don't need a second import.
export type { ReviewState, HivemindReviewState };
export { EMPTY_REVIEW_STATE };
