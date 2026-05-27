/**
 * TypeScript mirrors of the Nurse subsystem's Rust types.
 *
 * Wire shapes must stay bit-identical to the legacy `types/nurse.ts`
 * surface (snake_case, optional fields where the backend defaults).
 * See `app/src-tauri/src/nurse/snapshot.rs` for the source of truth
 * once the backend lands. The frontend lands these mirrors first so
 * the Nurse screen can compile/test against a stable wire contract
 * even before the backend handlers exist; missing IPC commands degrade
 * to friendly empty states (see `formatIpcError` handling in hooks).
 */

/* ── Core enums ──────────────────────────────────────────────── */

export type Severity = "info" | "warn" | "stalled" | "critical";

export type Tier = "quiet" | "warning" | "stalled" | "critical";

export type NurseActionKind = "leave_it" | "steer" | "restart" | "cancel";

export type NurseLifecycleStatus =
  | "started"
  | "reasoning"
  | "completed"
  | "failed";

/** Origin tier for an intervention. Drives the small "how did we get
 *  here?" badge on each Intervention Log row. */
export type NurseDispatchTier =
  | "deterministic"
  | "templated"
  | "llm"
  | "synthesized"
  | "manual";

export type NurseProfile = "tasks" | "swarm" | "hivemind" | "test" | "default";

export type NurseInterventionMode = "auto" | "observe";

export type NurseMasterMode = "enabled" | "observe" | "disabled";

/* ── Session ownership ──────────────────────────────────────── */

/**
 * Discriminated-union DTO for the source `pi/session.rs::SessionOwner`
 * enum. Backend serialises via `SessionOwnerDto::from(&owner)` with
 * `#[serde(tag = "kind", rename_all = "snake_case")]`.
 */
export type SessionOwnerDto =
  | { kind: "task"; task_id: string }
  | { kind: "review"; job_id: string }
  | {
      kind: "merge";
      job_id: string;
      round: number;
      swarm_id?: string | null;
    }
  | {
      kind: "swarm";
      swarm_id: string;
      role: string;
      feature_id?: string | null;
    }
  | { kind: "unknown" };

/* ── Live status snapshot (legacy-compatible) ───────────────── */

export type SessionHealthStatus =
  | "healthy"
  | "warning"
  | "stalled"
  | "intervening"
  | "resolved"
  | "failed";

export interface NurseStats {
  monitored_count: number;
  stall_count: number;
  intervention_count: number;
  last_check_at: string | null;
  is_running: boolean;
  /** Counter incremented every time a Tier 3 classifier dispatch is
   *  skipped because `nurse_model == "none"`. Surfaced as a warning
   *  banner in the Nurse screen header. Optional for back-compat. */
  tier3_skipped_no_model?: number;
}

export interface NurseHealth {
  last_tick_at: number | null;
  last_successful_tick_at: number | null;
  consecutive_failed_ticks: number;
  consecutive_bad_parse_ticks: number;
  consecutive_skipped_ticks: number;
  degraded: boolean;
  /** Number of nurse-bus events dropped because the bounded mpsc
   *  channel was full. Drives the "missed N events under load" chip
   *  in the screen header. Optional for back-compat. */
  observability_dropped?: number;
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
  /** Discriminated owner (Task / Review / Merge / Swarm / Unknown).
   *  Optional for back-compat with the pre-rewrite wire shape. */
  owner?: SessionOwnerDto;
  /** Model id currently driving this session (e.g.
   *  `anthropic/claude-opus-4.7`). Optional for back-compat. */
  model?: string;
  /** Absolute project path the session is operating in. Optional. */
  project_path?: string;
  /** Highest-severity active signal on this session. Drives the card's
   *  border tint without forcing the consumer to fold over the full
   *  signal list. */
  highest_severity?: Severity;
  /** Active signals raised against this session. */
  active_signals?: ActiveSignal[];
}

export interface ActiveSignal {
  /** Stable detector id (e.g. `stall`, `loop`, `tool_failure`). */
  detector: string;
  /** Per-detector dedup key, e.g. `loop:exact:abc123` or
   *  `tool_stuck:bash:xyz`. Used as the React list key. */
  dedup_key: string;
  severity: Severity;
  /** Human-friendly one-liner. */
  description: string;
  /** Wall-clock ISO 8601 timestamp the signal was first raised. */
  raised_at: string;
  /** Optional opaque JSON the chip-expand view renders inline. */
  evidence?: unknown;
}

export interface NurseSessionAction {
  level: string;
  session_id: string;
  message: string;
  timestamp: string;
}

export interface NurseInterventionRecord {
  id: string;
  session_id: string;
  timestamp: string;
  level: string;
  analysis: string;
  action_taken: NurseSessionAction;
  outcome: string | null;
  /** Tier the dispatch came from. Optional for back-compat. */
  tier?: NurseDispatchTier;
  /** Profile that owned the session at dispatch time. Optional. */
  profile?: NurseProfile;
  /** Owner discriminator at dispatch time. Optional. */
  owner?: SessionOwnerDto;
  /** Outcome glyph driver: did the session recover after this? */
  success?: boolean | null;
  /** Detectors that contributed signals to this decision. */
  triggering_signals?: ActiveSignal[];
  /** Backend-side Decision id — used by `get_nurse_decision_chain`
   *  and `get_nurse_capture` to fetch the full prompt/response. */
  decision_id?: string;
}

export interface NurseServiceConfigSnapshot {
  enabled: boolean;
  stall_threshold_secs: number;
  nurse_model: string;
  max_interventions: number;
  tick_interval_secs: number;
  nurse_provider: string | null;
  /** Master-level mode (`enabled` / `observe` / `disabled`). Optional
   *  for back-compat with the pre-rewrite wire shape — older backends
   *  only carry the boolean `enabled` field. */
  mode?: NurseMasterMode;
  /** When true, Nurse keeps detecting on every session but suppresses
   *  every intervention whose owner isn't a swarm agent. Optional for
   *  back-compat with older backends — treat `undefined` as `false`. */
  swarms_only?: boolean;
}

export interface NurseStatusSnapshot {
  stats: NurseStats;
  sessions: MonitoredSessionSnapshot[];
  recent_interventions: NurseInterventionRecord[];
  config: NurseServiceConfigSnapshot;
  health: NurseHealth;
  /** Per-provider circuit-breaker snapshot driving the header pill
   *  cluster. Optional — when absent the pills render as neutral. */
  providers?: ProviderHealthSnapshot[];
}

export interface ProviderHealthSnapshot {
  provider_id: string;
  display_name: string;
  /** `closed` = healthy, `half_open` = probing, `open` = tripped. */
  breaker_state: "closed" | "half_open" | "open";
  /** ISO 8601 timestamp the breaker is eligible to half-open at, when
   *  in the `open` state. */
  retry_at?: string | null;
}

/* ── Lifecycle event payload (live in-flow Nurse intervention) ── */

/**
 * Payload for a live, in-flow Nurse intervention. Emitted by the
 * backend as `nurse-event` with `event_type: "Lifecycle"`. Streams in
 * three stages keyed by `intervention_id`:
 *
 *  1. `started`    — Nurse announces what it spotted and what it'll do.
 *  2. `reasoning`  — zero or more chunks of streaming rationale.
 *  3. `completed` | `failed` — final outcome (optionally with full reasoning).
 *
 * Routing: scope events to the current Tasks-view by matching
 * `task_id` (preferred) or `session_id`. SwarmControl matches
 * `swarm_id`. The new Nurse screen subscribes to ALL events and
 * routes by `session_id` for the live grid + by `intervention_id` for
 * the detail drawer.
 */
export interface NurseLifecyclePayload {
  intervention_id: string;
  status: NurseLifecycleStatus;
  level: NurseActionKind | string;
  session_id: string;
  task_id?: string | null;
  swarm_id?: string | null;
  feature_id?: string | null;
  review_id?: string | null;
  observation: string;
  action: string;
  reasoning_delta?: string | null;
  full_reasoning?: string | null;
  error?: string | null;
  timestamp: string;
}

/* ── Event union ────────────────────────────────────────────── */

export type NurseEvent =
  | ({ event_type: "StatusUpdate" } & NurseStatusSnapshot)
  | ({ event_type: "Intervention" } & NurseInterventionRecord)
  | {
      event_type: "UserNotice";
      session_id: string;
      level: string;
      message: string;
      timestamp: string;
    }
  | ({ event_type: "Lifecycle" } & NurseLifecyclePayload);

/* ── TunableDef (auto-generic profile UI) ──────────────────── */

export type TunableKind =
  | "numeric_range"
  | "stepper"
  | "enum"
  | "toggle"
  | "text";

export type TunableDirection =
  | "higher_more_sensitive"
  | "higher_less_sensitive"
  | "neutral";

/**
 * Schema descriptor for one tunable knob on a detector. Returned by
 * `get_nurse_detector_schemas` and rendered generically by
 * `NurseTunableField`. The renderer ALWAYS shows the unit, default
 * value, direction copy, and markdown description — a bare slider
 * with no context is treated as a regression.
 */
export interface TunableDef {
  /** Stable identifier (snake_case). */
  name: string;
  kind: TunableKind;
  /** Human-friendly unit string (e.g. `seconds`, `count`, `%`). Empty
   *  string is acceptable for dimensionless toggles / enums. */
  unit: string;
  direction: TunableDirection;
  default: unknown;
  /** For `numeric_range` / `stepper`: `{ min, max, step? }`. For
   *  `enum`: `{ choices: [{ value, label }, ...] }`. For `toggle` /
   *  `text`: `null`. */
  safe_range: unknown;
  /** Markdown one-paragraph explanation. */
  description: string;
}

export interface NumericRange {
  min: number;
  max: number;
  step?: number;
}

export interface EnumChoices {
  choices: Array<{ value: string; label: string }>;
}

/* ── Detector schemas ──────────────────────────────────────── */

export interface DetectorSchema {
  /** Stable detector id matching `ActiveSignal.detector`. */
  name: string;
  /** Display label rendered in the Profiles tab + Detector Activity. */
  display_name: string;
  description: string;
  tunables: TunableDef[];
}

/* ── Detector activity stats ───────────────────────────────── */

export interface DetectorStatsRow {
  detector: string;
  total: number;
  by_severity: Record<Severity, number>;
  /** Median ms between this detector's signal raising and the next
   *  forward-progress event on the session. */
  avg_clear_ms?: number | null;
  /** Count of interventions on sessions that completed successfully
   *  anyway within the false-positive window. */
  fp_count?: number;
}

/* ── Profile config ────────────────────────────────────────── */

export interface BudgetConfig {
  /** Cap each new session starts with. */
  initial_cap: number;
  /** Replenishment per hour. */
  decay_per_hour: number;
  /** Hard ceiling — `current_budget` never exceeds this even with
   *  decay. */
  max_cap: number;
  /** Per-detector independent cap. */
  per_detector_cap: number;
  /** Cooldown (seconds) between back-to-back interventions sharing a
   *  dedup key. */
  per_key_cooldown_secs: number;
}

export interface ProfileDetectorConfig {
  enabled: boolean;
  /** Free-form `name -> value` map. Values match the corresponding
   *  `TunableDef.kind` (numbers for numeric/stepper, strings for enum/
   *  text, booleans for toggles). */
  config: Record<string, unknown>;
}

export interface ProfileConfig {
  enabled: boolean;
  intervention_mode: NurseInterventionMode;
  escalation_min_severity: Severity;
  budget: BudgetConfig;
  detectors: Record<string, ProfileDetectorConfig>;
}

/* ── Intervention log query ────────────────────────────────── */

export interface InterventionLogQuery {
  before_ts?: string | null;
  limit?: number;
  profile?: NurseProfile | null;
  action?: NurseActionKind | null;
  tier?: NurseDispatchTier | null;
  severity?: Severity | null;
  owner_kind?: SessionOwnerDto["kind"] | null;
  success?: boolean | null;
}

export interface InterventionLogPage {
  rows: NurseInterventionRecord[];
  /** Cursor for the next call. `null` when no more pages. */
  next_before_ts: string | null;
  /** Total matching rows across all pages, if the backend can supply
   *  it cheaply. */
  total?: number;
}

/* ── Session detail (drawer) ──────────────────────────────── */

export interface SessionDetailSnapshot {
  session: MonitoredSessionSnapshot;
  /** Last N raw Pi events for the session. Newest first. */
  transcript_tail: Array<{
    timestamp: string;
    kind: string;
    text?: string;
    tool_name?: string;
  }>;
  /** Decisions made about this session, newest first. */
  decisions: Array<{
    decision_id: string;
    started_at: string;
    finalised_at?: string | null;
    status: string;
    tier_used: NurseDispatchTier;
    action?: NurseActionKind | null;
  }>;
  /** Per-detector last-tick timestamps. */
  detector_last_tick: Record<string, string>;
}

/* ── Manual action ────────────────────────────────────────── */

export type NurseManualAction =
  | { kind: "steer"; message: string }
  | { kind: "cancel"; message?: string }
  | { kind: "force_restart" };

/* ── Decision chain (intervention detail) ─────────────────── */

export interface DecisionChainEvent {
  timestamp: string;
  /** `signal_raised`, `playbook_match`, `classifier_prompt`,
   *  `classifier_response`, `dispatch_sent`, `outcome`, etc. */
  kind: string;
  /** Opaque per-kind JSON payload. */
  data: Record<string, unknown>;
}

export interface DecisionChain {
  decision_id: string;
  session_id: string;
  events: DecisionChainEvent[];
}

/* ── Feedback ─────────────────────────────────────────────── */

export interface FeedbackInput {
  intervention_id: string;
  rating: "up" | "down";
  note?: string;
}
