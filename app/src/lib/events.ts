// Internal name — surfaces as "Tasks" in the UI. See PRODUCT.md §3.
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type { ChatEvent, HivemindProgressEvent, SwarmEvent, PiUpdateEvent } from "./types";
import type { UsageSnapshotEvent } from "../extensions/types";

/**
 * Safely call an UnlistenFn, catching any async rejection.
 *
 * Tauri's internal _unlisten is async, so calling it returns a Promise
 * even though the TypeScript type is () => void. If the JS listener
 * map hasn't been populated yet (timing race), the async call rejects.
 * This wrapper swallows that rejection — the Rust-side unregistration
 * succeeds regardless.
 */
export const safeUnlisten = (fn: UnlistenFn | undefined | null): void => {
  if (!fn) return;
  // fn() is typed as void but Tauri's internal _unlisten is async,
  // so the runtime value is actually a Promise that can reject
  const result = fn() as unknown;
  if (result && typeof (result as Record<string, unknown>).catch === 'function') {
    (result as Promise<unknown>).catch(() => {});
  }
};

export const onChatEvent = (cb: (e: ChatEvent) => void): Promise<UnlistenFn> =>
  listen<ChatEvent>("chat-event", (event) => cb(event.payload));

/**
 * `hivemind-progress` channel — the unified event stream emitted by the
 * backend `ReviewEngine` for both Tasks- and Swarm-origin reviews.
 *
 * Event types: `started`, `context_started`, `context_chunk`,
 * `context_completed`, `round_started`, `model_chunk`, `model_completed`,
 * `model_failed`, `round_completed`, `merge_started`, `merge_chunk`,
 * `merge_completed`, `completed`, `failed`, `cancelled`. See
 * `HivemindProgressEvent` for the payload shape. Routing keys
 * (`task_id` / `swarm_id` / `feature_id`) are filled in by the engine
 * based on the originating call site.
 */
export const onHivemindProgress = (cb: (e: HivemindProgressEvent) => void): Promise<UnlistenFn> =>
  listen<HivemindProgressEvent>("hivemind-progress", (event) => cb(event.payload));

export const onSwarmEvent = (cb: (e: SwarmEvent) => void): Promise<UnlistenFn> =>
  listen<SwarmEvent>("swarm-event", (event) => cb(event.payload));

/**
 * Audit 2.2 — emitted once at app startup for each swarm the
 * `reconcile_orphaned_swarms_with_replay` sweep found in an in-flight
 * state and marked `Interrupted`. Carries the list of feature ids the
 * sweep promoted to `Failed { interrupted: true, resumable: true }` so
 * the Swarms list can surface a Resume badge immediately without waiting
 * for the next 5s poll.
 */
export interface SwarmReconciledEvent {
  swarm_id: string;
  interrupted_features: string[];
}

export const onSwarmReconciled = (
  cb: (e: SwarmReconciledEvent) => void,
): Promise<UnlistenFn> =>
  listen<SwarmReconciledEvent>("swarm_reconciled", (event) => cb(event.payload));

export type SwarmActivityKind =
  | "agent_start"
  | "agent_end"
  | "text"
  | "thinking"
  | "tool_start"
  | "tool_update"
  | "tool_end"
  | "error";

export interface SwarmActivityEvent {
  swarm_id: string;
  feature_id: string;
  agent: "scout" | "worker" | "guard" | "hivemind-context" | "hivemind-merge";
  session_id: string;
  timestamp: string;
  kind: SwarmActivityKind;
  text?: string;
  tool_call_id?: string;
  tool_name?: string;
  tool_output?: string;
  tool_result?: unknown;
  model?: string;
  success?: boolean;
  error?: string;
  /**
   * Monotonic per-swarm sequence number injected by the backend on persisted
   * + emitted events. Used by `swarmActivityStore` to deduplicate live events
   * against the hydration log on first subscribe. Optional because (a) the
   * field is recent and not strictly required by the reducer, (b) defensive
   * code should not crash if a synthetic / legacy event omits it.
   */
  seq?: number;
}

export const onSwarmActivity = (
  cb: (e: SwarmActivityEvent) => void,
): Promise<UnlistenFn> =>
  listen<SwarmActivityEvent>("swarm-activity", (event) => cb(event.payload));

export const onPiUpdateProgress = (cb: (e: PiUpdateEvent) => void): Promise<UnlistenFn> =>
  listen<PiUpdateEvent>("pi-update-progress", (event) => cb(event.payload));

export const onPiInstallProgress = (cb: (e: PiUpdateEvent) => void): Promise<UnlistenFn> =>
  listen<PiUpdateEvent>("pi-install-progress", (event) => cb(event.payload));

import type { NurseEvent } from "../types/nurse";

export const onNurseEvent = (cb: (e: NurseEvent) => void): Promise<UnlistenFn> =>
  listen<NurseEvent>("nurse-event", (event) => cb(event.payload));

export const onUsageSnapshotUpdated = (cb: (e: UsageSnapshotEvent) => void): Promise<UnlistenFn> =>
  listen<UsageSnapshotEvent>("usage-snapshot-updated", (event) => cb(event.payload));

export interface TestProgressEvent {
  run_id: string;
  phase: string;
  status: string;
  message: string;
  /** Full TestRunRecord on terminal events (complete / failed). */
  record?: unknown;
}

export const onTestProgress = (
  cb: (e: TestProgressEvent) => void,
): Promise<UnlistenFn> =>
  listen<TestProgressEvent>("test-progress", (event) => cb(event.payload));
