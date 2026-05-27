// ── Chat ──
export interface ChatMessage {
  role: string;
  content: string;
  timestamp: string;
}

export interface ChatEvent {
  session_id: string;
  event_type: string;
  content: string;
}

// ── Image Attachments ──
export interface ImageAttachment {
  id: string;
  mediaType: string;
  data: string;       // base64 data (no prefix)
  previewUrl: string;  // URL.createObjectURL for rendering
}

// ── Queue State (steer/follow-up) ──
export interface QueueState {
  steering: string[];
  followUp: string[];
}

// ── Hivemind ──
export interface ReviewStatus {
  job_id: string;
  status: string;
  current_round: number;
  total_rounds: number;
  steps: StepSummary[];
  error: string | null;
  final_output: string | null;
  total_cost: number;
  total_input_tokens: number;
  total_output_tokens: number;
  created_at: string;
  completed_at: string | null;
}

export interface StepSummary {
  model_id: string;
  provider: string;
  status: string;
  output_preview: string;
  input_tokens: number | null;
  output_tokens: number | null;
  duration_ms: number | null;
  round_number: number;
}

export interface ReviewSummary {
  /** May be a logical review_id for grouped multi-round runs. Use child_job_ids for backend event matching. */
  job_id: string;
  child_job_ids: string[];
  status: string;
  created_at: string;
  stance: string;
  plan_preview: string;
  name?: string | null;
  total_cost: number;
  num_rounds: number;
  total_input_tokens: number;
  total_output_tokens: number;
  completed_at: string | null;
  hivemind_id: string | null;
  num_models: number;
  /** Absolute path of the project the review ran against. `null` for legacy
   *  rows persisted before migration 0019_jobs_project_path. */
  project_path: string | null;
}

export interface ListReviewsResponse {
  reviews: ReviewSummary[];
  total_runs: number;
}

export type HivemindProgressEventType =
  | "started"
  | "context_started"
  | "context_chunk"
  | "context_text"
  | "context_thinking"
  | "context_tool_start"
  | "context_tool_update"
  | "context_tool_end"
  | "context_completed"
  | "round_started"
  | "model_chunk"
  | "model_completed"
  | "model_failed"
  | "round_completed"
  | "merge_started"
  | "merge_chunk"
  | "merge_text"
  | "merge_thinking"
  | "merge_tool_start"
  | "merge_tool_update"
  | "merge_tool_end"
  | "merge_completed"
  | "completed"
  | "failed"
  | "cancelled";

export interface HivemindProgressEvent {
  job_id: string;
  review_id?: string | null;
  /** Canonical variants listed in `HivemindProgressEventType`. Backend may
   *  still emit legacy `review_interrupted` / `merge_interrupted` events at
   *  startup recovery; consumers narrow as needed. */
  event_type: HivemindProgressEventType | string;
  round: number;
  model_id: string;
  message: string;
  delta?: string;
  input_tokens?: number;
  output_tokens?: number;
  duration_ms?: number;
  cost?: number;
  output_len?: number;
  /** Coarse phase tag for the current event. The backend emits "cancelled"
   *  (distinct from "failed") on user-initiated cancellation so the UI can
   *  render a neutral pill instead of the red "failed" tone. */
  phase?:
    | "context"
    | "round"
    | "merge"
    | "completed"
    | "cancelled"
    | "failed"
    | ReviewResumePhase
    | "started";
  total_rounds?: number;
  task_id?: string | null;
  swarm_id?: string | null;
  feature_id?: string | null;
  source_label?: string;
  /** Pi session id for inline `context_*` / `merge_*` structured-chunk
   *  events. Carried on `*_started` so frontend reducers can register the
   *  session as "internal" before the first delta arrives. */
  session_id?: string;
  /** Tool call id for `*_tool_start` / `*_tool_update` / `*_tool_end`. */
  tool_call_id?: string;
  /** Tool name on `*_tool_start`. */
  tool_name?: string;
  /** Tool args (raw JSON) on `*_tool_start`. */
  tool_args?: unknown;
  /** Streamed tool output chunk on `*_tool_update`. */
  tool_output?: string;
  /** Final tool result on `*_tool_end`. */
  tool_result?: unknown;
  /** Model IDs scheduled for this round. Present on `round_started` so
   *  reducers can seed spinner rows before any model produces output.
   *  @deprecated Superseded by `model_instances` which carries the per-call
   *  `model_idx` needed to disambiguate duplicate reviewer instances. Kept
   *  for back-compat: reducers fall back to this field (using array index
   *  as the implicit `model_idx`) when `model_instances` is absent. */
  models?: string[];
  /** Richer round_started shape: one entry per scheduled reviewer call,
   *  carrying both the human-facing `model_id` and the 0-based instance
   *  index `model_idx` that distinguishes duplicate model ids within the
   *  round (e.g. four calls to the same provider/model with different
   *  temperatures). */
  model_instances?: { model_id: string; model_idx: number }[];
  /** 0-based instance index of the per-model call within its round.
   *  Carried on `model_chunk` / `model_completed` / `model_failed` so the
   *  frontend reducer can key state per `(model_id, model_idx)` instead of
   *  collapsing duplicate-instance rows. Optional for back-compat with
   *  legacy events replayed from older backends. */
  model_idx?: number;
}

// ── Merge run (durable merge stream metadata) ──
export interface MergeRunInfo {
  id: string;
  job_id: string;
  review_id: string | null;
  round_number: number;
  session_id: string;
  model_id: string;
  provider: string;
  thinking_level: string;
  status: "running" | "completed" | "failed" | "interrupted";
  started_at: string;
  completed_at: string | null;
  failed_at: string | null;
  error: string | null;
  output_path: string;
  output_len: number;
}

/** UI-side state describing an interrupted merge that the user can resume.
 *  Set on a TaskRuntimeState when reconciliation discovers a merge_run
 *  in `interrupted` status (host process died mid-merge). */
export interface InterruptedMergeState {
  jobId: string;
  round: number;
  outputLen: number;
  message: string;
}

/** Phase of a Hivemind review at the moment of interruption. Backend
 *  classifies this from SQLite state; the frontend dispatches on it to
 *  resume at the right boundary. */
export type ReviewResumePhase = "context" | "round" | "merge" | "between_rounds" | "final";

/** One model spec from the resumable review snapshot. */
export interface ResumableModelSpec {
  modelId: string;
  provider: string;
  stance: string;
  thinking?: string;
}

/** One completed model output preserved from a prior (interrupted) job. */
export interface ResumableStepOutput {
  modelId: string;
  provider: string;
  output: string;
}

/** Full resumable-review snapshot returned by `get_resumable_review_for_task`. */
export interface ResumableReviewSnapshot {
  reviewId: string;
  latestJobId: string;
  taskId: string;
  phase: ReviewResumePhase;
  round: number;
  totalRounds: number;
  planText: string;
  models: ResumableModelSpec[];
  completedStepOutputs: ResumableStepOutput[];
  mergeOutput?: string;
  message: string;
}

/** UI-side state describing an interrupted review that the user can resume.
 *  Generalises InterruptedMergeState across all five phases. */
export interface ReviewInterruptedState {
  phase: ReviewResumePhase;
  reviewId: string;
  jobId: string;
  round: number;
  totalRounds: number;
  planText: string;
  models: ResumableModelSpec[];
  completedStepOutputs: ResumableStepOutput[];
  mergeOutput?: string;
  message: string;
}

// ── Hivemind orchestrator verdicts ──
//
// Re-exported from `review-mode.ts` so consumers that only import from
// `lib/types` can still reach the types without pulling in the prompt
// constants. The single canonical definition lives in `review-mode.ts`.
export type { RoundVerdict, VerdictKind, ParsedVerdict } from "./review-mode";

// ── Orchestrator usage ──
export interface PhaseUsage {
  round: number | null;
  session_id: string;
  model_id: string;
  provider: string;
  input_tokens: number;
  output_tokens: number;
}

export interface OrchestratorUsage {
  model_id: string;
  provider: string;
  total_input_tokens: number;
  total_output_tokens: number;
  total_cost: number;
  total_duration_ms: number;
  context_session: PhaseUsage | null;
  merge_sessions: PhaseUsage[];
}

// ── Hivemind Configs ──
export interface HivemindSummary {
  id: string;
  name: string;
  description: string;
  rounds_config: string;
  inherit_orchestrator: boolean;
  orchestrator_model: string | null;
  orchestrator_provider: string | null;
  orchestrator_thinking: string;
  /** Captured at model selection time from `ModelDetail.context_length`
   *  (or static catalog fallback). `null` for legacy rows; the runtime
   *  resolver falls back to inherited-model or catalog lookup. */
  orchestrator_context_window: number | null;
  /** Captured at model selection time from `ModelDetail.max_output`. */
  orchestrator_max_output: number | null;
  runs: number;
  created_at: string;
  updated_at: string;
}

export interface StepOutput {
  model_id: string;
  provider: string;
  round_number: number;
  output: string;
  input_tokens: number | null;
  output_tokens: number | null;
  duration_ms: number | null;
  cost: number | null;
}

// ── Hivemind review state snapshot (full resync from SQLite) ──
export interface StepFull {
  model_id: string;
  provider: string;
  status: string;
  output: string;
  input_tokens: number | null;
  output_tokens: number | null;
  duration_ms: number | null;
  round_number: number;
  cost: number | null;
  prompt: string | null;
  error: string | null;
}

export interface ReviewRun {
  id: number | string;
  child_job_ids: string[];
  status: "ok" | "issues" | "fail" | "running";
  duration: string;
  models: number;
  rounds: number;
  date: string;
  prompt: string;
  name?: string | null;
}

export interface ReviewStateSnapshot {
  job_id: string;
  status: string;
  is_running: boolean;
  current_round: number;
  total_rounds: number;
  steps: StepFull[];
  error: string | null;
  final_output: string | null;
  total_cost: number;
  total_input_tokens: number;
  total_output_tokens: number;
  created_at: string;
  completed_at: string | null;
}

// ── Swarms ──
export type SwarmStatus =
  | "planning"
  | "implementing"
  | "paused"
  | "interrupted"
  | "completed"
  | "failed"
  | "cancelled";

export type FeatureStatus = "pending" | "scouting" | "implementing" | "reviewing" | "validating" | "completed" | "failed" | "skipped";

export interface SwarmState {
  id: string;
  name: string;
  status: SwarmStatus;
  working_directory: string;
  model_settings: ModelSettings;
  current_phase: string;
  current_feature_index: number;
  created_at: string;
  updated_at: string;
  error: string | null;
}

export interface ModelSettings {
  primary_model: string;
  scout_model: string;
  guard_model?: string | null;
  scout_thinking_level?: string;
  worker_thinking_level?: string;
  guard_thinking_level?: string;
  queen_thinking_level?: string;
  use_hivemind_on_scout: boolean;
  use_hivemind_on_queen: boolean;
  hivemind_id: string | null;
  /** Maximum number of features the swarm scheduler will run concurrently.
   *  Range 1..=6, default 1 (sequential). Backend deserializes a missing
   *  field as the default for backwards compatibility with swarms persisted
   *  before this field existed. */
  max_concurrent_features?: number;
  /** Phase 5A: hard cap on this swarm's lifetime spend in USD. `null`
   *  (or missing) means unlimited. The Queen checks this between feature
   *  batches against the live swarm-spend accumulator and pauses the
   *  swarm + emits a `BudgetExceeded` event if it's exceeded. */
  swarm_budget_usd?: number | null;
}

export interface Feature {
  id: string;
  name: string;
  description: string;
  status: FeatureStatus;
  dependencies: string[];
  milestone: string | null;
  fix_attempt_count: number;
  max_fix_attempts: number;
  /** Phase 2: VAL-* assertion IDs this feature is responsible for satisfying.
   *  Auto-injected validator features (`id` starts with `validate-`) carry
   *  all milestone assertions; impl features usually have an empty list;
   *  Guard-spawned fix features carry the failed assertion IDs.
   *  Defaults to [] on features persisted before Phase 2 landed. */
  fulfills?: string[];
  /** Audit 2.2: set when the on-disk crash reconciler promoted this feature
   *  from an in-flight state (Scouting / Implementing / Reviewing /
   *  Validating) to `Failed` because the host process died mid-execution.
   *  Distinct from a genuine validation failure. Cleared by resume_swarm.
   *  Defaults to false on features persisted before audit 2.2. */
  interrupted?: boolean;
  /** Audit 2.2: set alongside `interrupted` on features the reconciler
   *  determined are safe to re-queue via `resume_swarm`. The Swarms list
   *  uses this to surface a Resume affordance. */
  resumable?: boolean;
}

export interface Milestone {
  id: string;
  name: string;
  features: string[];
  assertions: string[];
  /** Server-side state: once a milestone validator passes the scheduler
   *  refuses to inject additional features into it. Phase 2 enforcement;
   *  defaults to false on swarms persisted before this field existed. */
  sealed: boolean;
}

// ── Settings ──
export interface SettingsResponse {
  configured_providers: string[];
  default_model: string | null;
  default_hivemind: string | null;
  default_project_path: string | null;
  concurrency_cap: number;
  max_pi_processes: number;
  data_dir: string;
  source_dir: string;
  stable_mode: boolean;
  debug_mode: boolean;
  auto_commit_tasks: boolean;
  auto_commit_conventional: boolean;
  task_completion_sound_enabled: boolean;
  task_completion_sound: string;
  crash_reporting_enabled: boolean;
  chat_check_in_secs: number;
  /** Global extension poll interval in seconds. Clamped to
   * [30, 3600]. Default: 120. */
  extension_poll_interval_secs: number;
  /** Phase 5A: global daily spending cap in USD across all swarms /
   *  hivemind / chat usage. `null` (or missing) means unlimited. */
  daily_budget_usd?: number | null;
  /** Audit 1.11: working-directory allowlist. Every IPC that takes a
   *  `working_dir` rejects paths outside this list. ProjectPicker shows
   *  the approval modal for any new pick that isn't already in here. */
  approved_working_dirs?: string[];
}

export interface ProviderInfo {
  name: string;
  display_name: string;
  provider_type: string;
  endpoint: string | null;
  configured: boolean;
  model_count: number;
  health: boolean | null;
}

/** Read-only descriptor for one prompt shown in Settings → Prompts. */
export interface SystemPromptInfo {
  id: string;
  /** Canonical values: "Tasks" | "Hivemind" | "Bee Agents" | "Other" (see PROMPT_CATEGORY_ORDER in Settings.tsx) */
  category: string;
  name: string;
  description: string;
  source: string;
  body: string;
}

export interface ModelInfoResponse {
  provider: string;
  model_id: string;
  context_window: number;
  cost_per_1m_input: number;
  cost_per_1m_output: number;
}

export interface ModelDetail {
  id: string;
  name: string | null;
  context_length: number | null;
  max_output: number | null;
  input_price: number | null;
  output_price: number | null;
}

export interface TestModelsResult {
  ok: boolean;
  models: string[];
  details: ModelDetail[];
  error: string | null;
}

export interface TestChatResult {
  ok: boolean;
  model: string;
  reply_preview: string | null;
  error: string | null;
}

export interface TestPiResult {
  ok: boolean;
  model: string;
  reply_preview: string | null;
  error: string | null;
}

// ── Pi Status ──
export type PiInstallMethod = "npm" | "homebrew" | "unknown";

export interface PiStatusResponse {
  installed: boolean;
  binary_path: string | null;
  resolved_path: string | null;
  binary_name: string | null;
  version: string | null;
  latest_version: string | null;
  is_outdated: boolean;
  install_method: PiInstallMethod;
  error: string | null;
}

export interface PiUpdateEvent {
  event_type: string;
  message: string;
}

// ── Subscription Auth ──
export interface SubscriptionAuthResponse {
  chatgpt: boolean;
  claude: boolean;
  auth_file_exists: boolean;
  error: string | null;
}

// ── Dashboard ──
export interface DashboardStats {
  active_tasks: number;
  running_swarms: number;
  paused_swarms: number;
  total_reviews: number;
  cost_today: number;
}

export interface ModelUsageSummary {
  model_id: string;
  provider: string;
  input_tokens: number;
  output_tokens: number;
  total_tokens: number;
  calls: number;
  cost: number;
}

export interface ProviderUsageSummary {
  provider: string;
  input_tokens: number;
  output_tokens: number;
  total_tokens: number;
  calls: number;
  cost: number;
}

export interface CostSummary {
  today: number;
  week: number;
  month: number;
  all_time: number;
}

export interface ActivityEntry {
  id: string;
  timestamp: string;
  source: string;
  source_id: string | null;
  model_id: string;
  provider: string;
  input_tokens: number;
  output_tokens: number;
  cost: number;
}

// ── Tool Call State ──
export interface ToolCallState {
  tool_call_id: string;
  name: string;
  output: string;
  done: boolean;
}

// ── Progress Events (from backend progress_log.jsonl) ──
export interface ProgressEvent {
  timestamp: string;
  event_type: string;
  swarm_id: string;
  feature_id?: string;
  message: string;
  metadata?: Record<string, unknown>;
}

// ── Worker-reported Discovered Issues (Phase 5C) ─────────────
/** Severity tag a Worker may attach to a `DiscoveredIssue`.
 *  Drives the chip colour (info=blue, warn=amber, error=red) and
 *  has NO effect on swarm execution — issues are informational. */
export type IssueSeverity = "info" | "warn" | "error";

/** A non-blocking issue surfaced by a Worker via its handoff. The user can
 *  acknowledge or dismiss these from the SwarmControl UI; they never gate
 *  swarm execution. Mirrors `core::handoff::DiscoveredIssue` (Rust). */
export interface DiscoveredIssue {
  severity: IssueSeverity;
  description: string;
  suggested_fix?: string | null;
}

// ── Swarm Events ──
export interface SwarmEvent {
  swarm_id: string;
  event_type: string;
  feature_id?: string;
  message: string;
  [key: string]: unknown;
}

// ── Auto Commit ──
export interface AutoCommitResult {
  ok: boolean;
  message: string;
  commit_hash: string | null;
}

// ── Task state resync ──
export interface TaskStateSnapshot {
  task_id: string;
  messages_json: string | null;
  session_busy: boolean;
  session_alive: boolean;
  /** PiEvent variants from the backend transcript. Untyped here because the
   *  frontend currently only inspects message length for resync precedence. */
  transcript: unknown[] | null;
}

// ── Provider Extensions (re-exports) ──
export type {
  Capability,
  MetricKind,
  Tone,
  SnapshotStatus,
  UsageMetric,
  UsageSnapshot,
  ExtensionManifest,
  ExtensionUserSettings,
  SnapshotEntry,
  UsageSnapshotEvent,
} from "../extensions/types";
