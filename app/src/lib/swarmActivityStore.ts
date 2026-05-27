import { isTauri } from "@tauri-apps/api/core";
import { onSwarmActivity, safeUnlisten, type SwarmActivityEvent } from "./events";
import { getSwarmActivityLog } from "./ipc";
import {
  applyActivityEvent,
  initialActivityState,
  type SwarmActivityState,
} from "./swarmActivityReducer";

type Listener = () => void;

/** Maximum number of swarms retained in the activity-state cache. */
const MAX_SWARMS = 50;

const states = new Map<string, SwarmActivityState>();
const listeners = new Map<string, Set<Listener>>();

/**
 * Per-swarm hydration tracking. The store hydrates from
 * `get_swarm_activity_log` the first time a subscriber attaches for
 * a given swarm, then transitions to "ready" so live `swarm-activity`
 * events apply directly.
 */
type HydrationStatus = "idle" | "loading" | "ready";
const hydrationStatus = new Map<string, HydrationStatus>();
/** Live events received while hydration is in flight; replayed (and deduped
 *  against `maxSeqByHydration`) once hydration completes. */
const liveBuffer = new Map<string, SwarmActivityEvent[]>();
/** Highest `seq` observed during hydration — buffered live events with
 *  `seq <= maxSeqByHydration` are dropped as already-included. */
const maxSeqByHydration = new Map<string, number>();

/**
 * Access-order tracking for LRU eviction.
 * Most-recently-accessed swarm is at the end.
 */
const accessOrder: string[] = [];

let globalUnlisten: (() => void) | null = null;
let globalListenerPromise: Promise<unknown> | null = null;

/** Total active subscribers across all swarms. */
function activeSubscriberCount(): number {
  let count = 0;
  for (const set of listeners.values()) count += set.size;
  return count;
}

/** Move `swarmId` to the end of the access-order list (most-recently-used). */
function touchAccessOrder(swarmId: string): void {
  const idx = accessOrder.indexOf(swarmId);
  if (idx !== -1) accessOrder.splice(idx, 1);
  accessOrder.push(swarmId);
}

/** Evict the least-recently-used swarm from both caches. */
function evictLRU(): void {
  const oldest = accessOrder[0];
  if (oldest) evictSwarmActivity(oldest);
}

/** Notify all listeners for a swarm. */
function notify(swarmId: string): void {
  const subs = listeners.get(swarmId);
  if (!subs) return;
  for (const l of subs) l();
}

/**
 * Apply a single event through the reducer, update bookkeeping, and
 * notify subscribers if state changed. Returns the new state (or the
 * previous state if the reducer was a no-op).
 */
function applyAndStore(swarmId: string, evt: SwarmActivityEvent): SwarmActivityState {
  const prev = states.get(swarmId) ?? initialActivityState;
  const next = applyActivityEvent(prev, evt);
  if (next === prev) return prev;
  states.set(swarmId, next);
  touchAccessOrder(swarmId);
  while (accessOrder.length > MAX_SWARMS) evictLRU();
  return next;
}

/**
 * Register the global Tauri `swarm-activity` listener if it is not
 * already registered or in-flight.
 *
 * Concurrency contract: at most one listener is ever installed. We keep
 * `globalListenerPromise` set across teardown so a subsequent subscribe
 * can't race in a second `listen()` before the first registration has
 * resolved. The `.then()` handler checks the live-subscriber count at
 * resolution time and immediately tears the listener down if nobody
 * needs it anymore — that's the only path that ever leaks (the old
 * code's generation counter would null out the promise eagerly, letting
 * a re-subscribe install a parallel listener and double-deliver every
 * event).
 */
function ensureGlobalListener(): void {
  if (globalUnlisten || globalListenerPromise) return;
  if (!isTauri()) return;
  globalListenerPromise = onSwarmActivity((evt) => {
    const status = hydrationStatus.get(evt.swarm_id);
    if (status === "loading") {
      // Buffer live events until hydration completes so the reducer
      // sees them in a sensible order with respect to history.
      let buf = liveBuffer.get(evt.swarm_id);
      if (!buf) {
        buf = [];
        liveBuffer.set(evt.swarm_id, buf);
      }
      buf.push(evt);
      return;
    }
    const prev = states.get(evt.swarm_id) ?? initialActivityState;
    const next = applyAndStore(evt.swarm_id, evt);
    if (next === prev) return;
    notify(evt.swarm_id);
  })
    .then((fn) => {
      globalListenerPromise = null;
      // If every subscriber unmounted while registration was in flight,
      // the listener is unwanted — tear it down immediately instead of
      // installing it (and avoid the double-listener leak).
      if (activeSubscriberCount() === 0) {
        safeUnlisten(fn);
        return;
      }
      globalUnlisten = fn;
    })
    .catch(() => {
      globalListenerPromise = null;
    });
}

/**
 * Tear down the global Tauri listener. Safe to call when none is active.
 *
 * Only acts on a resolved (installed) listener. An in-flight registration
 * is intentionally left alone; the `.then()` handler in
 * `ensureGlobalListener` will check the subscriber count when it resolves
 * and immediately tear the listener down if it isn't needed. Eagerly
 * nulling `globalListenerPromise` here is what caused the original
 * double-listener bug — `ensureGlobalListener` would see no pending
 * registration and start a second one in parallel.
 */
function teardownGlobalListener(): void {
  if (globalUnlisten) {
    safeUnlisten(globalUnlisten);
    globalUnlisten = null;
  }
}

/**
 * Drain the live buffer for a swarm into the reducer, deduplicating
 * against `maxSeqByHydration`. Called when hydration completes (success
 * OR failure). Events without `seq` are defensively treated as
 * "post-hydration" and applied as-is.
 */
function drainLiveBuffer(swarmId: string): boolean {
  const buf = liveBuffer.get(swarmId);
  liveBuffer.delete(swarmId);
  if (!buf || buf.length === 0) return false;
  const maxSeq = maxSeqByHydration.get(swarmId);
  let changed = false;
  for (const evt of buf) {
    if (
      typeof evt.seq === "number" &&
      typeof maxSeq === "number" &&
      evt.seq <= maxSeq
    ) {
      // Already covered by hydration — drop.
      continue;
    }
    const prev = states.get(swarmId) ?? initialActivityState;
    const next = applyAndStore(swarmId, evt);
    if (next !== prev) changed = true;
  }
  return changed;
}

/**
 * Drive the paginated hydration loop for a swarm. Folds each returned
 * event through the reducer, tracks the max seq, then drains the live
 * buffer and flips status to "ready" so subsequent live events apply
 * directly. On error: logs a warning, drains the buffer, and still
 * flips to "ready" — the live stream must keep working even if the
 * log read fails.
 */
async function hydrateSwarm(swarmId: string): Promise<void> {
  let afterSeq: number | undefined = undefined;
  let maxSeenSeq = -1;
  let changed = false;
  try {
    // Safety bound: a few hundred pages is already 100k+ events; cap
    // here protects against a runaway backend or bug returning a
    // never-advancing next_seq.
    for (let page = 0; page < 200; page++) {
      const result = await getSwarmActivityLog(swarmId, afterSeq);
      for (const evt of result.events) {
        if (typeof evt.seq === "number" && evt.seq > maxSeenSeq) {
          maxSeenSeq = evt.seq;
        }
        const prev = states.get(swarmId) ?? initialActivityState;
        const next = applyAndStore(swarmId, evt);
        if (next !== prev) changed = true;
      }
      if (result.next_seq === null || result.next_seq === undefined) break;
      // Guard against an unchanged cursor — bail rather than spin.
      if (afterSeq !== undefined && result.next_seq <= afterSeq) break;
      afterSeq = result.next_seq;
    }
    if (maxSeenSeq >= 0) maxSeqByHydration.set(swarmId, maxSeenSeq);
  } catch (err) {
    console.warn(
      `swarmActivityStore: hydration failed for ${swarmId}; live events will still apply`,
      err,
    );
  } finally {
    const drained = drainLiveBuffer(swarmId);
    hydrationStatus.set(swarmId, "ready");
    if (changed || drained) notify(swarmId);
  }
}

/**
 * Remove a swarm's activity state and listeners, and tear down the
 * global listener if no subscribers remain.
 */
export function evictSwarmActivity(swarmId: string): void {
  states.delete(swarmId);
  hydrationStatus.delete(swarmId);
  liveBuffer.delete(swarmId);
  maxSeqByHydration.delete(swarmId);
  const subs = listeners.get(swarmId);
  if (subs) {
    listeners.delete(swarmId);
    for (const l of subs) l();
  }
  const idx = accessOrder.indexOf(swarmId);
  if (idx !== -1) accessOrder.splice(idx, 1);
  if (activeSubscriberCount() === 0) teardownGlobalListener();
}

export function getSwarmActivityState(swarmId: string): SwarmActivityState {
  if (states.has(swarmId)) touchAccessOrder(swarmId);
  return states.get(swarmId) ?? initialActivityState;
}

export function subscribeSwarmActivity(swarmId: string, listener: Listener): () => void {
  ensureGlobalListener();
  let set = listeners.get(swarmId);
  if (!set) {
    set = new Set();
    listeners.set(swarmId, set);
  }
  set.add(listener);
  // Kick off background hydration the first time anyone subscribes to
  // this swarm. Subsequent subscribers piggyback on the in-flight or
  // completed hydration — `useSyncExternalStore` will deliver the
  // state snapshot once hydration's notify() fires. Outside of Tauri
  // (browser preview / SSR) there's no backend to hydrate from, so we
  // skip the IPC entirely and mark "ready" so live-event handling
  // (which is also a no-op without Tauri) stays consistent.
  if (!hydrationStatus.has(swarmId)) {
    if (!isTauri()) {
      hydrationStatus.set(swarmId, "ready");
    } else {
      hydrationStatus.set(swarmId, "loading");
      // Fire-and-forget: hydrateSwarm handles its own errors internally.
      void hydrateSwarm(swarmId);
    }
  }
  return () => {
    const s = listeners.get(swarmId);
    if (!s) return;
    s.delete(listener);
    if (s.size === 0) listeners.delete(swarmId);
    if (activeSubscriberCount() === 0) teardownGlobalListener();
  };
}

export function clearSwarmActivity(swarmId: string): void {
  states.delete(swarmId);
  const subs = listeners.get(swarmId);
  if (subs) for (const l of subs) l();
  const idx = accessOrder.indexOf(swarmId);
  if (idx !== -1) accessOrder.splice(idx, 1);
}

export function _resetSwarmActivityStoreForTests(): void {
  states.clear();
  listeners.clear();
  hydrationStatus.clear();
  liveBuffer.clear();
  maxSeqByHydration.clear();
  accessOrder.length = 0;
  if (globalUnlisten) {
    safeUnlisten(globalUnlisten);
    globalUnlisten = null;
  }
  globalListenerPromise = null;
}
