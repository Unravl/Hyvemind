import { invoke as rawInvoke } from "@tauri-apps/api/core";
import * as Sentry from "@sentry/react";

/**
 * Typed envelope returned by every `#[tauri::command]` on the Rust side.
 *
 * Mirrors `state::ipc_error::IpcError`. `kind` is the discriminator and any
 * payload fields (e.g. `resource` / `resource_id` on `not_found`) are
 * flattened next to it on the wire.
 *
 * Note on naming: the Rust enum variant for `not_found` carries `resource`
 * and `resource_id` rather than the audit plan's `kind`/`id` — `kind` would
 * collide with the variant discriminator, and `id` with the envelope's
 * own `id` field after serde flattening. See `state/ipc_error.rs` for the
 * full rationale.
 */
export type IpcError =
  | { kind: "provider_unauthenticated"; message: string; id?: string | null; details?: unknown }
  | { kind: "provider_rate_limited"; message: string; id?: string | null; details?: unknown }
  | { kind: "circuit_breaker_open"; message: string; id?: string | null; details?: unknown }
  | {
      kind: "not_found";
      resource: string;
      resource_id: string;
      message: string;
      id?: string | null;
      details?: unknown;
    }
  | { kind: "validation"; message: string; id?: string | null; details?: unknown }
  | { kind: "not_approved"; message: string; id?: string | null; details?: unknown }
  | { kind: "internal"; message: string; id?: string | null; details?: unknown };

/**
 * Best-effort runtime narrowing for the typed envelope. Tauri rejects with
 * the envelope object directly; legacy commands and lower-level Tauri errors
 * still arrive as plain strings or `Error` instances. Callers that just want
 * a user-facing string should use {@link formatIpcError}.
 */
export function isIpcError(err: unknown): err is IpcError {
  if (!err || typeof err !== "object") return false;
  const obj = err as Record<string, unknown>;
  if (typeof obj.kind !== "string" || typeof obj.message !== "string") return false;
  switch (obj.kind) {
    case "provider_unauthenticated":
    case "provider_rate_limited":
    case "circuit_breaker_open":
    case "not_found":
    case "validation":
    case "not_approved":
    case "internal":
      return true;
    default:
      return false;
  }
}

/**
 * Format any IPC rejection — typed envelope or otherwise — into a single
 * user-facing string. Use this at error-display sites (toasts, modals).
 *
 * Branches on `IpcError.kind` so each error class can carry its own
 * lead-in copy (e.g. unauthenticated → "Provider authentication failed: …").
 * Falls back to the raw message for unknown kinds or non-envelope errors.
 */
export function formatIpcError(err: unknown): string {
  if (isIpcError(err)) {
    switch (err.kind) {
      case "provider_unauthenticated":
        return `Provider authentication failed: ${err.message}`;
      case "provider_rate_limited":
        return `Provider rate-limited: ${err.message}`;
      case "circuit_breaker_open":
        return `Provider unavailable (circuit breaker open): ${err.message}`;
      case "not_found":
        return `Not found: ${err.message}`;
      case "validation":
        return err.message;
      case "not_approved":
        return err.message;
      case "internal":
        return err.message;
    }
  }
  if (err instanceof Error) return err.message;
  if (typeof err === "string") return err;
  try {
    return JSON.stringify(err);
  } catch {
    return String(err);
  }
}

// Single chokepoint for every IPC call. Captures failures to Sentry tagged
// with the command name and the structured envelope kind (when present),
// then re-throws so existing callers' error handling (which routes through
// `console.error` → ErrorModal) is unchanged.
async function invoke<T>(name: string, args?: Record<string, unknown>): Promise<T> {
  try {
    // Forward only-name when no args, so callers like `invoke("list_swarms")`
    // produce a single-arg call that matches Tauri's overload set and the
    // existing `toHaveBeenCalledWith(name)` test contracts.
    return args === undefined ? await rawInvoke<T>(name) : await rawInvoke<T>(name, args);
  } catch (err) {
    // Sentry needs an Error instance, but typed envelopes are plain objects.
    // Surface the structured `kind` and the user-facing message as a synthetic
    // Error so the dashboard groups failures meaningfully.
    let captured: Error;
    const tags: Record<string, string> = { source: "ipc", ipc_command: name };
    if (isIpcError(err)) {
      tags.ipc_error_kind = err.kind;
      captured = new Error(formatIpcError(err));
    } else if (err instanceof Error) {
      captured = err;
    } else {
      captured = new Error(String(err));
    }
    Sentry.captureException(captured, { tags });
    throw err;
  }
}
import type {
  ChatMessage,
  ReviewStatus,
  ReviewSummary,
  ListReviewsResponse,
  HivemindSummary,
  StepOutput,
  SwarmState,
  ModelSettings,
  Feature,
  Milestone,
  ProgressEvent,
  SettingsResponse,
  ProviderInfo,
  SystemPromptInfo,
  ModelInfoResponse,
  TestModelsResult,
  TestChatResult,
  TestPiResult,
  PiStatusResponse,
  SubscriptionAuthResponse,
  DashboardStats,
  ModelUsageSummary,
  ProviderUsageSummary,
  CostSummary,
  ActivityEntry,
  TaskStateSnapshot,
  ReviewStateSnapshot,
  RoundVerdict,
  AutoCommitResult,
  MergeRunInfo,
  OrchestratorUsage,
  ResumableReviewSnapshot,
} from "./types";

// ── Chat ──
export interface ImagePayload {
  media_type: string;
  data: string;
}

export const sendMessage = (message: string, model?: string, sessionId?: string, workingDir?: string, thinkingLevel?: string, systemPrompt?: string, toolSet?: string, images?: ImagePayload[], isSteer?: boolean) =>
  invoke<string>("send_message", { message, model, sessionId, workingDir, thinkingLevel, systemPrompt, toolSet, images, isSteer });

export const stopChat = (sessionId: string) =>
  invoke<void>("stop_chat", { sessionId });

export const getChatHistory = (sessionId: string) =>
  invoke<ChatMessage[]>("get_chat_history", { sessionId });

export const isSessionBusy = (sessionId: string) =>
  invoke<boolean>("is_session_busy", { sessionId });

/** Read the concatenated text of the LAST assistant message from the Pi
 *  session's authoritative JSONL on disk. Used by the streaming `done`
 *  handler to reconcile in-memory messages against actual Pi output when
 *  the IPC chat-event stream may have dropped chunks under load. Returns
 *  an empty string if the session file doesn't exist or has no assistant
 *  text. */
export const getSessionLastAssistantText = (sessionId: string) =>
  invoke<string>("get_session_last_assistant_text", { sessionId });

export const listChatSessions = () =>
  invoke<string[]>("list_chat_sessions");

export const deleteChatSession = (sessionId: string) =>
  invoke<void>("delete_chat_session", { sessionId });

// ── Hivemind ──
export const startReview = (plan: string, opts?: {
  stance?: string; numRounds?: number; timeoutSeconds?: number; models?: string[]; reviewId?: string; hivemindId?: string; name?: string; taskId?: string;
  /** Absolute path of the project the review ran against. Persisted on the
   *  `jobs` row so the All Reviews page can filter reviews by project. */
  projectPath?: string;
  /** 1-based cumulative round number for multi-round Tasks reviews. Each
   *  round is its own `start_review` call with `numRounds: 1`; this field
   *  tells the backend to emit/cache files under the correct round (`r2`,
   *  `r3`, …) instead of clobbering round 1's. Defaults to 1. */
  roundNumber?: number;
  /** Optional per-model context-window override map, keyed by
   *  `"provider/model_id"`. Plumbed into `ReviewModelConfig::context_window`
   *  on the Rust side. Missing entries degrade to the hardcoded fallback. */
  modelContextWindows?: Record<string, number>;
  /** Optional per-model sampling overrides, keyed by `"provider/model_id"`.
   *  Missing entries leave the provider's default — the field is omitted
   *  from the outbound request body. */
  modelTemperatures?: Record<string, number>;
  modelTopPs?: Record<string, number>;
  /** Provider-qualified orchestrator override (`"provider/model_id"` or
   *  bare). Send this when the calling Task wants its current model to act
   *  as the merge orchestrator — the frontend is the only thing that knows
   *  the Task's active model, so it must resolve `inherit_orchestrator`
   *  itself and pass the result here. When omitted, the backend falls
   *  through to (stored hivemind orchestrator → last reviewer in round). */
  orchestratorModel?: string;
}) => invoke<string>("start_review", { plan, ...opts });

export const getResumableReviewForTask = (taskId: string) =>
  invoke<ResumableReviewSnapshot | null>("get_resumable_review_for_task", { taskId });

/** Guard against empty, whitespace-only, or non-string reviewId passed to
 *  log_review_event IPC. Several frontend call sites fall through to "" or
 *  undefined when review flow state has been torn down and a stale/late event
 *  arrives. The backend's validate_id correctly rejects these (security measure
 *  against path traversal), but the guard prevents the IPC call entirely —
 *  avoiding both the crash and the resulting Sentry error.
 *
 *  Lifecycle verification (audited in taskRuntime.tsx): no call site can fire
 *  before flow.reviewId is assigned during active review flow. The only window
 *  where reviewId may be absent is post-teardown (stale events after
 *  finishReviewFlow clears the flow map). */
export const logReviewEvent = (
  reviewId: string | null | undefined,
  eventType: string,
  data: Record<string, unknown>,
) => {
  if (typeof reviewId !== "string" || !reviewId.trim()) {
    console.warn("logReviewEvent: invalid reviewId — event dropped", {
      eventType,
      reviewIdType: typeof reviewId,
    });
    Sentry.addBreadcrumb({
      category: "hivemind",
      message: `logReviewEvent dropped: reviewId=${JSON.stringify(reviewId)} eventType=${eventType}`,
      level: "debug",
    });
    return Promise.resolve();
  }
  return invoke<void>("log_review_event", { reviewId, eventType, data });
};

export const cancelReview = (jobId: string) =>
  invoke<void>("cancel_review", { jobId });

export const getReviewStatus = (jobId: string) =>
  invoke<ReviewStatus>("get_review_status", { jobId });

export const getReviewState = (jobId: string) =>
  invoke<ReviewStateSnapshot>("get_review_state", { jobId });

export const listReviews = async (limit?: number, offset?: number, hivemindId?: string) => {
  const result = await invoke<ReviewSummary[] | ListReviewsResponse>("list_reviews", {
    limit,
    offset,
    hivemindId,
  });
  return Array.isArray(result) ? { reviews: result, total_runs: result.length } : result;
};

// ── Hivemind Configs ──
export const createHivemind = (
  name: string, description: string, roundsConfig: string,
  inheritOrchestrator?: boolean, orchestratorModel?: string,
  orchestratorProvider?: string, orchestratorThinking?: string,
  orchestratorContextWindow?: number | null,
  orchestratorMaxOutput?: number | null,
) =>
  invoke<HivemindSummary>("create_hivemind", {
    name, description, roundsConfig,
    inheritOrchestrator, orchestratorModel, orchestratorProvider, orchestratorThinking,
    orchestratorContextWindow, orchestratorMaxOutput,
  });

export const listHiveminds = (limit?: number, offset?: number) =>
  invoke<HivemindSummary[]>("list_hiveminds", { limit, offset });

export const updateHivemind = (
  hivemindId: string, name: string, description: string, roundsConfig: string,
  inheritOrchestrator?: boolean, orchestratorModel?: string,
  orchestratorProvider?: string, orchestratorThinking?: string,
  orchestratorContextWindow?: number | null,
  orchestratorMaxOutput?: number | null,
) =>
  invoke<HivemindSummary>("update_hivemind", {
    hivemindId, name, description, roundsConfig,
    inheritOrchestrator, orchestratorModel, orchestratorProvider, orchestratorThinking,
    orchestratorContextWindow, orchestratorMaxOutput,
  });

export const deleteHivemind = (hivemindId: string) =>
  invoke<void>("delete_hivemind", { hivemindId });

export const getReviewPlan = (reviewId: string) =>
  invoke<string>("get_review_plan", { reviewId });

export const getReviewStepOutputs = (jobId: string) =>
  invoke<StepOutput[]>("get_review_step_outputs", { jobId });

// ── Hivemind orchestrator verdicts ──
export const saveRoundVerdicts = (
  jobId: string,
  roundNumber: number,
  verdicts: RoundVerdict[],
) =>
  invoke<void>("save_round_verdicts", { jobId, roundNumber, verdicts });

export const listRoundVerdicts = (jobId: string) =>
  invoke<RoundVerdict[]>("list_round_verdicts", { jobId });

// ── Hivemind merge runs ──
// `start_merge_run` / `complete_merge_run` write paths removed: the backend
// engine owns merge lifecycle end-to-end now. Read paths kept for the
// ReviewHistory recovery banner until a `get_review_artifacts` IPC lands.
export const readMergeOutput = (args: { jobId: string; round: number }) =>
  invoke<string>("read_merge_output", {
    jobId: args.jobId,
    round: args.round,
  });

export const getMergeRun = (args: { jobId: string; round: number }) =>
  invoke<MergeRunInfo | null>("get_merge_run", {
    jobId: args.jobId,
    round: args.round,
  });

export const listMergeRunsForJob = (jobId: string) =>
  invoke<MergeRunInfo[]>("list_merge_runs_for_job", { jobId });

// ── Orchestrator usage ──
export const registerContextSession = (args: {
  reviewId: string;
  sessionId: string;
  modelId: string;
  provider: string;
}) => invoke<void>("register_context_session", args);

export const getOrchestratorUsage = (reviewId: string) =>
  invoke<OrchestratorUsage>("get_orchestrator_usage", { reviewId });

export const deleteReview = (runId: string) =>
  invoke<void>("delete_review", { runId });

// ── Swarms ──
export const createSwarm = (name: string, description: string, workingDirectory: string, modelSettings: ModelSettings) =>
  invoke<SwarmState>("create_swarm", { name, description, workingDirectory, modelSettings });

/** Update an existing swarm's name, working directory, and model settings.
 *  Rejected by the backend if the swarm is currently `Implementing` — the
 *  caller must stop the swarm first. Returns the updated `SwarmState`. */
export const updateSwarm = (
  swarmId: string,
  name: string,
  workingDirectory: string,
  modelSettings: ModelSettings,
) =>
  invoke<SwarmState>("update_swarm", {
    swarmId,
    name,
    workingDirectory,
    modelSettings,
  });

/** Feature spec accepted by `start_swarm` on the backend. Only id/name/
 *  description/dependencies/milestone are read (see commands/swarms.rs);
 *  status and counters are initialized server-side. Loosening the typing
 *  here so callers don't have to forge fields the backend ignores. */
export type StartSwarmFeatureInput = Pick<Feature, "id" | "name" | "description"> & {
  dependencies?: string[];
  milestone?: string | null;
};

/** Milestone spec accepted by `start_swarm` on the backend. Mirrors
 *  `core::swarm::Milestone` minus the `sealed` field (Phase 2 server-side
 *  state — clients don't author it). The backend persists these alongside
 *  features so Guard can look up assertions when a milestone completes. */
export interface MilestoneInput {
  id: string;
  name: string;
  features: string[];
  assertions: string[];
}

export const startSwarm = (
  swarmId: string,
  features: StartSwarmFeatureInput[] | Feature[],
  milestones?: MilestoneInput[],
) =>
  invoke<void>("start_swarm", {
    swarmId,
    features,
    // Forward only when present so the Tauri arg-deserializer treats the
    // field as `Option::None` (matches the Rust signature
    // `milestones: Option<Vec<JsonValue>>`). Sending `[]` is also valid but
    // omission keeps old-shape callers indistinguishable from new-shape ones
    // sending an empty array.
    milestones,
  });

export const pauseSwarm = (swarmId: string) =>
  invoke<void>("pause_swarm", { swarmId });

export const resumeSwarm = (swarmId: string) =>
  invoke<void>("resume_swarm", { swarmId });

export const stopSwarm = (swarmId: string) =>
  invoke<void>("stop_swarm", { swarmId });

export const getSwarm = (swarmId: string) =>
  invoke<SwarmState>("get_swarm", { swarmId });

export const listSwarms = () =>
  invoke<SwarmState[]>("list_swarms");

export const deleteSwarm = (swarmId: string) =>
  invoke<void>("delete_swarm", { swarmId });

export const getSwarmProgress = (swarmId: string) =>
  invoke<ProgressEvent[]>("get_swarm_progress", { swarmId });

/**
 * One page of the per-swarm activity log returned by
 * `get_swarm_activity_log`. `events` are sorted ascending by `seq`;
 * `next_seq` is the highest seq seen when more pages likely exist,
 * `null` when the caller has caught up to the tail.
 */
export interface SwarmActivityLogPage {
  events: import("./events").SwarmActivityEvent[];
  next_seq: number | null;
}

/**
 * Page through the persisted per-swarm activity log. Used by
 * `swarmActivityStore` to hydrate state on first subscribe so the
 * SwarmControl Live Activity panel can replay history before live
 * events take over. Backend pages at `limit` (default 500, capped 2000)
 * and returns events with `seq > after_seq`.
 */
export const getSwarmActivityLog = (
  swarmId: string,
  afterSeq?: number,
  limit?: number,
) =>
  invoke<SwarmActivityLogPage>("get_swarm_activity_log", {
    swarmId,
    afterSeq,
    limit,
  });

export const getSwarmFeatures = (swarmId: string) =>
  invoke<Feature[]>("get_swarm_features", { swarmId });

/** Read the persisted milestone list for a swarm. Returns an empty array if
 *  the swarm was created before milestones were wired in, or the plan didn't
 *  define any. The `sealed` field is server-side state (Phase 2) — clients
 *  treat it as read-only. */
export const getSwarmMilestones = (swarmId: string) =>
  invoke<Milestone[]>("get_swarm_milestones", { swarmId });

export interface SwarmUsageSummary {
  input_tokens: number;
  output_tokens: number;
  cache_read_tokens: number;
  cache_write_tokens: number;
  cost: number;
  duration_ms: number;
}

export const getSwarmUsage = (swarmId: string) =>
  invoke<SwarmUsageSummary>("get_swarm_usage", { swarmId });

/** Live per-session token usage for one running Pi session. Backend returns
 *  `null` when the session is no longer in the pool (already evicted or
 *  killed) so the caller can clear the UI on agent transitions. */
export interface PiSessionStats {
  input: number;
  output: number;
  reasoning_tokens: number;
  cache_read: number;
  cache_write: number;
  total_tokens: number;
  cost: number;
  context_tokens: number;
  context_window: number;
  context_percent: number;
}

export const getPiSessionStats = (sessionId: string) =>
  invoke<PiSessionStats | null>("get_pi_session_stats", { sessionId });

// ── Settings ──
export const getSettings = () =>
  invoke<SettingsResponse>("get_settings");

export const getSystemPrompts = () =>
  invoke<SystemPromptInfo[]>("get_system_prompts");

// ── User-defined custom prompts ──
export interface CustomPrompt {
  id: string;
  name: string;
  body: string;
  created_at: string;
  updated_at: string;
}

export const listCustomPrompts = () =>
  invoke<CustomPrompt[]>("list_custom_prompts");

export const saveCustomPrompt = (id: string | null, name: string, body: string) =>
  invoke<CustomPrompt>("save_custom_prompt", { id, name, body });

export const deleteCustomPrompt = (id: string) =>
  invoke<void>("delete_custom_prompt", { id });

export const setRuntimeSettings = (concurrencyCap: number, maxPiProcesses: number) =>
  invoke<SettingsResponse>("set_runtime_settings", { concurrencyCap, maxPiProcesses });

/** Phase 5A: persist the global daily spending cap. `null` means unlimited.
 *  Backend rejects negative values; non-finite NaN/Infinity also rejected. */
export const setDailyBudget = (usd: number | null) =>
  invoke<SettingsResponse>("set_daily_budget", { usd });

export const setDefaultModel = (model: string) =>
  invoke<void>("set_default_model", { model });

export const setDefaultHivemind = (hivemindId: string) =>
  invoke<void>("set_default_hivemind", { hivemindId });

export const setDefaultProjectPath = (path: string) =>
  invoke<void>("set_default_project_path", { path });

/** Audit 1.11: ask the backend to add a working directory to the approved
 *  allowlist after the user clicks Allow on the approval modal. Resolves
 *  to `true` when the path is now approved (newly added or already
 *  present); rejects with the validation error if the path is empty,
 *  contains a NUL byte, doesn't exist, or isn't a directory. */
export const requestWorkingDirApproval = (path: string) =>
  invoke<boolean>("request_working_dir_approval", { path });

export const saveApiKey = (provider: string, apiKey: string) =>
  invoke<void>("save_api_key", { provider, apiKey });

export const deleteApiKey = (provider: string) =>
  invoke<void>("delete_api_key", { provider });

export const getProviders = () =>
  invoke<ProviderInfo[]>("get_providers");

export const addProvider = (id: string, displayName: string, providerType?: string, endpoint?: string) =>
  invoke<void>("add_provider", { id, displayName, providerType, endpoint });

export const refreshModels = (provider?: string) =>
  invoke<ModelInfoResponse[]>("refresh_models", { provider });

export const testProviderModels = (provider: string) =>
  invoke<TestModelsResult>("test_provider_models", { provider });

export const testProviderChat = (provider: string, model: string) =>
  invoke<TestChatResult>("test_provider_chat", { provider, model });

export const testProviderPi = (provider: string, model: string) =>
  invoke<TestPiResult>("test_provider_pi", { provider, model });

// ── Pi Status ──
export const getPiStatus = () =>
  invoke<PiStatusResponse>("get_pi_status");

export const updatePi = () =>
  invoke<void>("update_pi");

export const installPi = () =>
  invoke<void>("install_pi");

export const openPiTerminal = () =>
  invoke<void>("open_pi_terminal");

// ── Subscription Auth ──
export const checkSubscriptionAuth = () =>
  invoke<SubscriptionAuthResponse>("check_subscription_auth");

// ── Dashboard ──
export const getDashboardStats = () =>
  invoke<DashboardStats>("get_dashboard_stats");

export const getModelUsage = (timeRange: string) =>
  invoke<ModelUsageSummary[]>("get_model_usage", { timeRange });

export const getProviderUsage = (timeRange: string) =>
  invoke<ProviderUsageSummary[]>("get_provider_usage", { timeRange });

export const getCostSummary = () =>
  invoke<CostSummary>("get_cost_summary");

export const getRecentActivity = (limit?: number) =>
  invoke<ActivityEntry[]>("get_recent_activity", { limit });

// ── Tasks ──
export const saveTaskMessages = (taskId: string, messages: string) =>
  invoke<void>("save_task_messages", { taskId, messages });

export const loadTaskMessages = (taskId: string) =>
  invoke<string | null>("load_task_messages", { taskId });

export const deleteTaskMessages = (taskId: string) =>
  invoke<void>("delete_task_messages", { taskId });

export const getTaskState = (taskId: string, sessionId?: string | null) =>
  invoke<TaskStateSnapshot>("get_task_state", { taskId, sessionId: sessionId ?? null });

// ── Auto Commit ──
export const setAutoCommitTasks = (enabled: boolean) =>
  invoke<void>("set_auto_commit_tasks", { enabled });

export const setAutoCommitConventional = (enabled: boolean) =>
  invoke<void>("set_auto_commit_conventional", { enabled });

export const setTaskCompletionSound = (enabled: boolean, sound: string) =>
  invoke<SettingsResponse>("set_task_completion_sound", { enabled, sound });

export const setCrashReporting = (enabled: boolean) =>
  invoke<void>("set_crash_reporting", { enabled });

export const autoCommitTask = (workingDir: string, taskTitle: string) =>
  invoke<AutoCommitResult>("auto_commit_task", { workingDir, taskTitle });

// ── Project file listing (for @-mention picker) ──
export interface ProjectFileEntry {
  path: string;
  basename: string;
  score: number;
}

export const listProjectFiles = (workingDir: string, query: string, limit?: number) =>
  invoke<ProjectFileEntry[]>("list_project_files", { workingDir, query, limit });

// ── Pi Pool / Session admin ──
export interface ActiveSession {
  id: string;
  owner_kind: "task" | "review" | "merge" | "swarm" | "unknown";
  owner_key?: string | null;
  is_alive: boolean;
  is_busy: boolean;
  is_pinned: boolean;
  event_count: number;
  turn_count: number;
  last_activity_ms: number;
}

export interface PiPoolStats {
  active_count: number;
  available_permits: number;
  max_processes: number;
  graveyard_size: number;
  sessions: Array<{
    id: string;
    owner: string;
    is_alive: boolean;
    is_busy: boolean;
    is_pinned: boolean;
    event_count: number;
    turn_count: number;
    last_activity_ms: number;
    last_prompt_sent_ms: number;
  }>;
}

export const listActivePiSessions = () =>
  invoke<ActiveSession[]>("list_active_pi_sessions");

export const killPiSession = (sessionId: string) =>
  invoke<void>("kill_pi_session", { sessionId });

/** Raw-SIGKILL a Pi subprocess by session id WITHOUT going through the
 *  orderly shutdown path or removing the session from the manager. Used
 *  by the `Test Nurse → Process crash` scenario so `ProcessHealthDetector`
 *  can observe `!is_alive()` on its next slow tick. Unix-only. */
export const sigkillPiSession = (sessionId: string) =>
  invoke<void>("sigkill_pi_session", { sessionId });

/** Reconcile backend session pool against renderer's known session ids.
 *  Only Task-owned, non-busy, non-pinned sessions whose id is not in
 *  `knownIds` are killed. Returns the list of killed session ids. */
export const reconcileActiveSessions = (knownIds: string[]) =>
  invoke<string[]>("reconcile_active_sessions", { knownIds });

export const getPiPoolStats = () => invoke<PiPoolStats>("pi_pool_stats");

// ── Nurse ──
import type { NurseStatusSnapshot } from "../types/nurse";

export const getNurseStatus = () =>
  invoke<NurseStatusSnapshot>("get_nurse_status");

export const setNurseConfig = (opts: {
  enabled?: boolean;
  stall_threshold_secs?: number;
  nurse_model?: string;
  /** @deprecated Nurse is always autonomous in the batched architecture. Ignored. */
  allow_destructive?: boolean;
  tick_interval_secs?: number;
  nurse_provider?: string | null;
  /** Override for the batch-review tick interval (seconds). Pass `0` to
   *  clear the override and fall back to the `HYVEMIND_NURSE_BATCH_INTERVAL_SECS`
   *  env-var default. Clamped to [30, 3600] backend-side. */
  nurse_batch_interval_secs?: number;
  /** When true, Nurse keeps observing every session but suppresses
   *  every intervention whose owner isn't a swarm agent. */
  swarms_only?: boolean;
}) =>
  invoke<void>("set_nurse_config", {
    enabled: opts.enabled ?? null,
    stallThresholdSecs: opts.stall_threshold_secs ?? null,
    nurseModel: opts.nurse_model ?? null,
    allowDestructive: opts.allow_destructive ?? null,
    tickIntervalSecs: opts.tick_interval_secs ?? null,
    nurseProvider: opts.nurse_provider ?? null,
    nurseBatchIntervalSecs: opts.nurse_batch_interval_secs ?? null,
    swarmsOnly: opts.swarms_only ?? null,
  });

/** Tag identifying which frontend watchdog issued a Nurse evaluation.
 *  Surfaced in backend tracing so we can disambiguate the three call sites
 *  (regular chat / Hivemind context gather / Hivemind merge) when
 *  investigating logs. */
export type NurseCheckCaller = "chat" | "context" | "merge";

/** Decision returned by `check_chat_session`. The `kind` discriminant
 *  mirrors the four backend `NurseDecision` variants plus a synthetic
 *  `noop` returned by the IPC handler when the watchdog fires after the
 *  session has already been torn down — the frontend should clear its
 *  timer silently rather than show a misleading error. */
export type NurseDecisionDto =
  | { kind: "leave_it"; reasoning: string; check_back_secs: number }
  | { kind: "steer"; reasoning: string; message: string }
  | { kind: "restart"; reasoning: string }
  | { kind: "cancel"; reasoning: string; message: string }
  | { kind: "noop"; reasoning: string };

/** Force a one-shot Nurse evaluation of one Pi session. Called by the
 *  watchdog at the configured `chat_check_in_secs` interval. `caller`
 *  identifies which of the three frontend watchdogs fired the check so
 *  backend tracing can tell them apart. */
export const checkChatSession = (sessionId: string, caller: NurseCheckCaller) =>
  invoke<NurseDecisionDto>("check_chat_session", { sessionId, caller });

/** Persist the Nurse chat check-in interval (seconds). Range 60-3600. */
export const setChatCheckInSecs = (secs: number) =>
  invoke<SettingsResponse>("set_chat_check_in_secs", { secs });

// ── Nurse (new screen IPC) ──
//
// These commands ship from the parallel backend rewrite (see the
// streamed-forging-sketch plan §"Frontend — Nurse Hive UI" → IPC
// additions). Until the backend handlers land they will reject with
// `IpcError::not_found`; the calling hooks degrade to a friendly empty
// state rather than crashing.
import type {
  InterventionLogPage,
  InterventionLogQuery,
  DetectorStatsRow,
  DetectorSchema,
  SessionDetailSnapshot,
  NurseManualAction,
  DecisionChain,
  FeedbackInput,
  ProfileConfig,
  NurseProfile,
  SessionHealthStatus,
  ActiveSignal,
} from "./nurseTypes";

export const clearNurseInterventionLog = () =>
  invoke<void>("clear_nurse_intervention_log", {});

export const getNurseInterventionLog = (q: InterventionLogQuery) =>
  invoke<InterventionLogPage>("get_nurse_intervention_log", { query: q });

export const getNurseDetectorStats = (timeRange: string) =>
  invoke<DetectorStatsRow[]>("get_nurse_detector_stats", { timeRange });

export const getNurseSessionDetail = async (
  sessionId: string,
): Promise<SessionDetailSnapshot> => {
  const raw = await invoke<any>("get_nurse_session_detail", { sessionId });

  let decisions: any[] = [];
  try {
    decisions = await invoke<any[]>("get_nurse_decisions_for_session", {
      sessionId,
    });
  } catch (e) {
    console.warn("failed to fetch nurse decisions for session", e);
  }

  let transcript_tail: any[] = [];
  try {
    const history = await invoke<any[]>("get_chat_history", { sessionId });
    transcript_tail = history.map((msg) => {
      let text = msg.content;
      let tool_name: string | undefined = undefined;
      if (msg.role === "tool") {
        try {
          const parsed = JSON.parse(msg.content);
          if (parsed.name) {
            tool_name = parsed.name;
            text = `execution started (id: ${parsed.tool_call_id})`;
          } else if (parsed.event === "end") {
            text = `execution ended (id: ${parsed.tool_call_id})`;
          }
        } catch {
          // ignore parsing error, fallback to raw content
        }
      }
      return {
        timestamp: msg.timestamp,
        kind: msg.role,
        text,
        tool_name,
      };
    });
  } catch (e) {
    console.warn("failed to fetch chat history for session", e);
  }

  const active_signals: ActiveSignal[] = (raw.signals ?? []).map(
    (sig: any) => ({
      detector: sig.detector,
      dedup_key: sig.dedup_key,
      severity: sig.severity,
      description: sig.summary,
      raised_at: sig.raised_at,
      evidence: sig.evidence,
    }),
  );

  const status: SessionHealthStatus =
    raw.tier === "quiet"
      ? "healthy"
      : raw.tier === "warning"
      ? "warning"
      : raw.tier === "stalled"
      ? "stalled"
      : raw.tier === "critical"
      ? "failed"
      : "healthy";

  return {
    session: {
      session_id: raw.session_id,
      last_activity_ms: 0,
      event_count: 0,
      is_alive: true,
      is_busy: false,
      status,
      stall_detected_at: null,
      intervention_count: raw.intervention_count,
      last_check_at: new Date().toISOString(),
      owner: raw.owner,
      model: raw.model_id,
      project_path: undefined,
      highest_severity: active_signals.length > 0 ? active_signals[0].severity : undefined,
      active_signals,
    },
    transcript_tail,
    decisions: decisions.map((d) => ({
      decision_id: d.decision_id,
      started_at: d.started_at,
      finalised_at: d.finalised_at,
      status: d.status ?? "unknown",
      tier_used: d.tier_used ?? "unknown",
      action: d.action,
    })),
    detector_last_tick: {},
  };
};

export const recordNurseInterventionFeedback = (input: FeedbackInput) =>
  invoke<void>("record_nurse_intervention_feedback", { input });

export const nurseManualAction = (
  sessionId: string,
  action: NurseManualAction,
) => invoke<void>("nurse_manual_action", { sessionId, action });

export const getNurseDetectorSchemas = () =>
  invoke<DetectorSchema[]>("get_nurse_detector_schemas");

export const getNurseDecisionChain = (decisionId: string) =>
  invoke<DecisionChain>("get_nurse_decision_chain", { decisionId });

export const getNurseDecisionsForSession = (
  sessionId: string,
  sinceTs?: string,
  limit?: number,
) =>
  invoke<
    Array<{
      decision_id: string;
      started_at: string;
      finalised_at?: string | null;
      status: string;
      tier_used: string;
      action?: string | null;
    }>
  >("get_nurse_decisions_for_session", { sessionId, sinceTs, limit });

export const getNurseSignalStream = (
  sessionId: string,
  sinceTs?: string,
  limit?: number,
) =>
  invoke<
    Array<{
      timestamp: string;
      kind: "raised" | "cleared";
      detector: string;
      dedup_key: string;
      severity: string;
    }>
  >("get_nurse_signal_stream", { sessionId, sinceTs, limit });

export const getNurseCapture = (
  decisionId: string,
  kind: "prompt" | "response",
) => invoke<string>("get_nurse_capture", { decisionId, kind });

export const exportNurseDiagnosticBundle = (args: {
  decisionId?: string;
  sessionId?: string;
  windowSecs?: number;
}) => invoke<{ bundle_path: string }>("export_nurse_diagnostic_bundle", args);

/** Patch a per-profile config. The backend writes into
 *  `config.json::nurse_profiles[profile]`. Optimistic updates revert
 *  via the wrapping hook on rejection. */
export const setNurseProfile = (profile: NurseProfile, config: ProfileConfig) =>
  invoke<void>("set_nurse_profile", { profile, config });

export const getNurseProfile = (profile: NurseProfile) =>
  invoke<ProfileConfig>("get_nurse_profile", { profile });

/** Persist the global extension poll interval (seconds). Range 30-3600. */
export const setExtensionPollIntervalSecs = (secs: number) =>
  invoke<SettingsResponse>("set_extension_poll_interval_secs", { secs });

// ── Provider Extensions ──
import type {
  ExtensionManifest,
  SnapshotEntry,
} from "../extensions/types";

export const listExtensions = () =>
  invoke<ExtensionManifest[]>("list_extensions");

export const getUsageSnapshots = () =>
  invoke<SnapshotEntry[]>("get_usage_snapshots");

export const refreshUsageSnapshot = (extensionId: string) =>
  invoke<SnapshotEntry>("refresh_usage_snapshot", { extensionId });

export const updateExtensionSettings = (
  extensionId: string,
  enabled?: boolean,
  showInTopbar?: boolean,
  preferences?: Record<string, string> | null,
) =>
  invoke<void>("update_extension_settings", {
    extensionId,
    enabled: enabled ?? null,
    showInTopbar: showInTopbar ?? null,
    preferences: preferences ?? null,
  });

// ── Stability Tests (Tests screen) ──
export interface StabilityTestConfigDto {
  taskModel: string;
  verifierModel: string;
  hivemindId: string | null;
}

export interface GateResult {
  name: string;
  passed: boolean;
  detail: string;
}

export interface VerifierVerdict {
  passed: boolean;
  confidence: number;
  issues: string[];
  summary: string;
}

export interface TestRunRecord {
  run_id: string;
  status: string;
  started_at: string;
  completed_at: string | null;
  duration_ms: number;
  task_id: string;
  session_id: string | null;
  hivemind_job_id: string | null;
  sandbox_dir: string;
  total_cost: number;
  gates: GateResult[];
  verdict: VerifierVerdict | null;
  error: string | null;
}

export interface TestRunSummary {
  run_id: string;
  status: string;
  started_at: string;
  completed_at: string | null;
  duration_ms: number;
  total_cost: number;
  pass_count: number;
  fail_count: number;
  verdict_passed: boolean | null;
  verdict_summary: string | null;
  error: string | null;
}

export interface ActiveTestRunDto {
  run_id: string;
  started_at_ms: number;
  last_phase?: string | null;
  last_status?: string | null;
  last_message?: string | null;
}

export const runStabilityTest = () =>
  invoke<{ run_id: string }>("run_stability_test");

export const cancelTestRun = () =>
  invoke<boolean>("cancel_test_run");

/**
 * Returns the currently-running stability test, if any. Used by
 * `TestRunProvider` to rehydrate the Active-run panel on app start /
 * provider mount so users don't lose the panel across restarts.
 */
export const getActiveTestRun = () =>
  invoke<ActiveTestRunDto | null>("get_active_test_run");

export const listTestRuns = (limit?: number) =>
  invoke<TestRunSummary[]>("list_test_runs", { limit });

export const getTestRun = (runId: string) =>
  invoke<TestRunRecord | null>("get_test_run", { runId });

export const getStabilityTestConfig = () =>
  invoke<StabilityTestConfigDto>("get_stability_test_config");

export const setStabilityTestConfig = (config: StabilityTestConfigDto) =>
  invoke<StabilityTestConfigDto>("set_stability_test_config", { config });
