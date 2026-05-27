export interface NurseStatusSnapshot {
  stats: NurseStats;
  sessions: MonitoredSessionSnapshot[];
  recent_interventions: NurseInterventionRecord[];
  config: NurseServiceConfigSnapshot;
  health: NurseHealth;
  /** Optional: status of the batched LLM reviewer. Present only when the
   *  reviewer was attached at engine start (the production default).
   *  Drives the topbar Nurse countdown progress bar. */
  batch?: BatchTickSnapshot | null;
}

/** Mirror of Rust `BatchTickSnapshotDto`. All fields are best-effort and
 *  may be 0 before the first tick has completed. */
export interface BatchTickSnapshot {
  enabled: boolean;
  interval_secs: number;
  last_tick_at_unix_ms: number;
  last_tick_duration_ms: number;
  next_tick_at_unix_ms: number;
  last_tick_session_count: number;
  /** Cumulative Nurse LLM provider calls this app-session (Tier 3 + batched).
   *  Resets to 0 on app start; not persisted. */
  llm_calls_total: number;
}

export interface NurseHealth {
  last_tick_at: number | null;
  last_successful_tick_at: number | null;
  consecutive_failed_ticks: number;
  consecutive_bad_parse_ticks: number;
  consecutive_skipped_ticks: number;
  degraded: boolean;
}

export interface NurseStats {
  monitored_count: number;
  stall_count: number;
  intervention_count: number;
  last_check_at: string | null;
  is_running: boolean;
}

export interface MonitoredSessionSnapshot {
  session_id: string;
  last_activity_ms: number;
  event_count: number;
  is_alive: boolean;
  is_busy: boolean;
  status: SessionHealthStatus;
  stall_detected_at: string | null;
  intervention_count: number;
  last_check_at: string;
}

export type SessionHealthStatus =
  | 'healthy' | 'warning' | 'stalled' | 'intervening' | 'resolved' | 'failed';

export interface NurseInterventionRecord {
  id: string;
  session_id: string;
  timestamp: string;
  level: string;
  analysis: string;
  action_taken: NurseSessionAction;
  outcome: string | null;
}

export interface NurseSessionAction {
  level: string;
  session_id: string;
  message: string;
  timestamp: string;
}

export interface NurseServiceConfigSnapshot {
  enabled: boolean;
  stall_threshold_secs: number;
  nurse_model: string;
  max_interventions: number;
  tick_interval_secs: number;
  nurse_provider: string | null;
  /** When true, Nurse keeps detecting on every session but suppresses
   *  every intervention whose owner isn't a swarm agent. Optional for
   *  back-compat with older backends — treat `undefined` as `false`. */
  swarms_only?: boolean;
}

export type NurseLifecycleStatus = 'started' | 'reasoning' | 'completed' | 'failed';

/**
 * Payload for a live, in-flow Nurse intervention. Emitted by the backend
 * as `nurse-event` with `event_type: "Lifecycle"`. Streams in three
 * stages keyed by `intervention_id`:
 *
 *  1. `started`     — Nurse announces what it spotted and what it'll do.
 *  2. `reasoning`*  — zero or more chunks of streaming rationale.
 *  3. `completed` | `failed` — final outcome (optionally with full reasoning).
 *
 * Routing: scope events to the current Tasks-view by matching
 * `task_id` (preferred) or `session_id`. SwarmControl matches `swarm_id`.
 */
export interface NurseLifecyclePayload {
  intervention_id: string;
  status: NurseLifecycleStatus;
  level: string;
  session_id: string;
  task_id?: string | null;
  swarm_id?: string | null;
  feature_id?: string | null;
  observation: string;
  action: string;
  reasoning_delta?: string | null;
  full_reasoning?: string | null;
  error?: string | null;
  timestamp: string;
}

export type NurseEvent =
  | ({ event_type: 'StatusUpdate' } & NurseStatusSnapshot)
  | ({ event_type: 'Intervention' } & NurseInterventionRecord)
  | {
      event_type: 'UserNotice';
      session_id: string;
      level: string;
      message: string;
      timestamp: string;
    }
  | ({ event_type: 'Lifecycle' } & NurseLifecyclePayload);
