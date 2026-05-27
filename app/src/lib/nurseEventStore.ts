import { isTauri } from "@tauri-apps/api/core";
import { onNurseEvent, safeUnlisten } from "./events";
import type { NurseEvent } from "../types/nurse";

/**
 * Singleton store for the `nurse-event` Tauri event channel.
 *
 * Mirrors the pattern in `swarmActivityStore.ts` and
 * `hivemindEventStore.ts`: ONE global Tauri listener is registered
 * the first time any subscriber attaches, then fan-out happens in JS.
 *
 * **Why this is non-optional**: opening a second `listen("nurse-event", …)`
 * doubles every event (Tauri does not dedupe). Every Nurse consumer
 * — `useNurseStatus`, the new screen, the topbar dropdown, the
 * detail drawer — MUST subscribe via this store and never call
 * `onNurseEvent` directly. See CLAUDE.md §Frontend Architecture
 * "Singleton event stores".
 *
 * Two subscription surfaces are exposed:
 *
 *  - `subscribe(cb)` — fires for every event. Used by `useNurseStatus`
 *    and the global topbar dropdown.
 *  - `subscribeForSession(session_id, cb)` — fires only when the
 *    event's `session_id` matches. Cheap O(1) dispatch.
 *
 * A ring buffer of the most recent N=100 events is kept so a late
 * subscriber can immediately replay recent state (the Nurse screen
 * uses this when the user opens the tab after an event has already
 * fired).
 */

type AnyListener = (ev: NurseEvent) => void;
type SessionListener = (ev: NurseEvent) => void;

const RING_CAPACITY = 100;

const anyListeners = new Set<AnyListener>();
const sessionListeners = new Map<string, Set<SessionListener>>();

const ring: NurseEvent[] = [];

let globalUnlisten: (() => void) | null = null;
let globalListenerPromise: Promise<unknown> | null = null;

/** Total live subscribers — used by teardown to know when to release
 *  the Tauri listener. */
function totalSubscribers(): number {
  let count = anyListeners.size;
  for (const set of sessionListeners.values()) count += set.size;
  return count;
}

/** Best-effort session id extraction. `StatusUpdate` is a global
 *  snapshot with no single session id; we treat it as undefined so
 *  session-scoped subscribers don't receive it. */
function eventSessionId(ev: NurseEvent): string | undefined {
  switch (ev.event_type) {
    case "StatusUpdate":
      return undefined;
    case "Intervention":
    case "UserNotice":
    case "Lifecycle":
      return ev.session_id;
    default:
      return undefined;
  }
}

function dispatch(ev: NurseEvent): void {
  // Append to ring (bounded). Push first so any handler that immediately
  // reads `recentEvents()` sees its own event.
  ring.push(ev);
  if (ring.length > RING_CAPACITY) ring.shift();

  // Snapshot listener sets before invoking so a handler that
  // unsubscribes during dispatch doesn't mutate the set we're iterating.
  if (anyListeners.size > 0) {
    const snap = Array.from(anyListeners);
    for (const l of snap) {
      try {
        l(ev);
      } catch (e) {
        console.error("nurseEventStore any-listener threw", e);
      }
    }
  }

  const sid = eventSessionId(ev);
  if (sid) {
    const set = sessionListeners.get(sid);
    if (set && set.size > 0) {
      const snap = Array.from(set);
      for (const l of snap) {
        try {
          l(ev);
        } catch (e) {
          console.error("nurseEventStore session-listener threw", e);
        }
      }
    }
  }
}

/**
 * Register the global Tauri `nurse-event` listener if not already in
 * flight. Mirrors the swarmActivityStore concurrency contract: the
 * in-flight promise stays set across teardown so a re-subscribe can't
 * race a second `listen()` before the first registration resolves.
 */
function ensureGlobalListener(): void {
  if (globalUnlisten || globalListenerPromise) return;
  if (!isTauri()) return;
  globalListenerPromise = onNurseEvent(dispatch)
    .then((fn) => {
      globalListenerPromise = null;
      // Tear down immediately if every subscriber went away while we
      // were registering.
      if (totalSubscribers() === 0) {
        safeUnlisten(fn);
        return;
      }
      globalUnlisten = fn;
    })
    .catch(() => {
      globalListenerPromise = null;
    });
}

function teardownIfIdle(): void {
  if (totalSubscribers() > 0) return;
  if (globalUnlisten) {
    safeUnlisten(globalUnlisten);
    globalUnlisten = null;
  }
}

/**
 * Subscribe to every `nurse-event`. Returns an unsubscribe function.
 * The listener fires for ALL events (StatusUpdate, Intervention,
 * UserNotice, Lifecycle). For per-session fan-out use
 * `subscribeForSession`.
 */
export function subscribe(listener: AnyListener): () => void {
  ensureGlobalListener();
  anyListeners.add(listener);
  return () => {
    anyListeners.delete(listener);
    teardownIfIdle();
  };
}

/**
 * Subscribe to `nurse-event`s whose `session_id` matches `sessionId`.
 * `StatusUpdate` events have no session id and are NOT delivered to
 * session-scoped subscribers. Returns an unsubscribe function.
 */
export function subscribeForSession(
  sessionId: string,
  listener: SessionListener,
): () => void {
  ensureGlobalListener();
  let set = sessionListeners.get(sessionId);
  if (!set) {
    set = new Set();
    sessionListeners.set(sessionId, set);
  }
  set.add(listener);
  return () => {
    const s = sessionListeners.get(sessionId);
    if (!s) return;
    s.delete(listener);
    if (s.size === 0) sessionListeners.delete(sessionId);
    teardownIfIdle();
  };
}

/**
 * Snapshot of the ring buffer (newest last). Useful for late
 * subscribers that need to replay recent events on mount.
 */
export function recentEvents(): NurseEvent[] {
  return ring.slice();
}

/* ── Test helpers ──────────────────────────────────────────── */

export function _resetNurseEventStoreForTests(): void {
  anyListeners.clear();
  sessionListeners.clear();
  ring.length = 0;
  if (globalUnlisten) {
    safeUnlisten(globalUnlisten);
    globalUnlisten = null;
  }
  globalListenerPromise = null;
}

/** Direct dispatch helper for tests that don't want to mock Tauri. */
export function _dispatchForTests(ev: NurseEvent): void {
  dispatch(ev);
}
