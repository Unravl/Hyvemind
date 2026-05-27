// Internal name — surfaces as "Tasks" in the UI. See PRODUCT.md §3.
import React, {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useRef,
  useState,
} from "react";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { isTauri } from "./tauri";
import * as ipc from "./ipc";
import type { ImagePayload } from "./ipc";
import { onChatEvent, onNurseEvent, safeUnlisten } from "./events";
import { subscribeHivemindEventListener } from "./hivemindEventStore";
import type {
  HivemindSummary,
  ImageAttachment,
  ResumableReviewSnapshot,
  ReviewInterruptedState,
  ReviewStateSnapshot,
} from "./types";
import {
  PLAN_SYSTEM_PROMPT,
  PLAN_TOOL_SET,
  QUEEN_PLANNING_SYSTEM_PROMPT,
  IMPL_SYSTEM_PROMPT,
  IMPL_TOOL_SET,
  buildImplementPrompt,
  buildSwarmAnswerPrompt,
  SWARM_QUESTION_SKIPPED_VALUE,
  type MilestoneSpec,
  type SwarmFeatureSpec,
  type SwarmQuestion,
} from "./plan-mode";
import {
  REVIEW_CONTEXT_SYSTEM_PROMPT,
  REVIEW_MERGE_SYSTEM_PROMPT,
  extractSourceContext,
  buildContextGatherPrompt,
  buildMergePrompt,
  buildReviewerPlan,
  dedupeReviewerLabels,
  parseRoundsConfig,
  truncateMergePrompt,
} from "./review-mode";
import type { RoundConfig, ParsedVerdict } from "./review-mode";
import { buildAnswerPrompt } from "./questions";
import type { TaskQuestion } from "./questions";
import {
  applyTaskEvent,
  hasUnansweredQuestions,
  mapChatEventToTaskEvent,
  mapNurseEventToTaskEvent,
  mapHivemindEventToTaskEvent,
  makeInitialTaskState,
  resetSessionStats,
  reviewInterruptedFromSnapshot,
  PHASE_RANK,
  normalizeAutoMode,
  type AutoMode,
  type TaskMessage,
  type TaskRuntimeState,
  type TaskPhase,
} from "./taskReducer";
import { useProject } from "../components/ProjectPicker";
import { getCompletionSoundConfig, playCompletionSound } from "./sounds";
import { assignSortOrders, migrateTasks } from "./sortOrder";
import { workspaceLabel } from "./categories";

/* ── Sidebar list item (lifted verbatim from Tasks.tsx) ───── */

export interface TaskListItem {
  id: string;
  group: string;
  title: string;
  project: string;
  model: string;
  phase: string;
  when: string;
  preview: string;
  active?: boolean;
  isSwarm?: boolean;
  swarmName?: string;
  hivemind?: string | null;
  directory?: string;
  projectPath?: string;
  sortOrder?: number;      // Manual sort position (lower = higher in list)
  createdAt?: number;      // Epoch ms when the task was created
  /** True when the user has manually renamed this task. When set, the planning
   *  agent's TASK_META block will NOT overwrite the title (but WILL still
   *  update the preview/description). Persisted to localStorage via the
   *  existing `saveTaskList` path. */
  titleEdited?: boolean;
  /** When set, this task is the planning conversation for a Swarm — it uses
   *  the QUEEN_PLANNING_SYSTEM_PROMPT instead of the generic plan prompt,
   *  and the PlanCard surfaces a "Launch Swarm" affordance instead of
   *  "Implement". The string is the backend swarm id from `create_swarm`. */
  swarmId?: string;
}

/* ── localStorage keys + helpers ──────────────────────────── */

const TASK_SESSIONS_KEY = "hyvemind:task-sessions";
const TASK_DRAFTS_KEY = "hyvemind:task-drafts";
export const TASK_LIST_KEY = "hyvemind:task-list";
const TASK_MESSAGES_PREFIX = "hyvemind:task-messages:";
export const ACTIVE_TASK_KEY = "hyvemind:active-task";
const AUTO_MODE_DEFAULT_KEY = "hyvemind:auto-mode-default";
export const MERGE_TIMEOUT_MIN_KEY = "hyvemind:hivemind-merge-timeout-min";
export const MERGE_TIMEOUT_DEFAULT_MIN = 20;
export const CONTEXT_TIMEOUT_MIN_KEY = "hyvemind:hivemind-context-timeout-min";
export const CONTEXT_TIMEOUT_DEFAULT_MIN = 10;
/** Frontend mirror of the backend `chat_check_in_secs` config. The Settings
 *  screen writes this to localStorage on every save so the watchdog can
 *  read it synchronously without an IPC hop. */
export const CHAT_CHECK_IN_SECS_KEY = "hyvemind:chat-check-in-secs";
export const CHAT_CHECK_IN_SECS_DEFAULT = 300;
export const CHAT_CHECK_IN_SECS_MIN = 60;
export const CHAT_CHECK_IN_SECS_MAX = 3600;

/** Frontend mirror of the backend `extension_poll_interval_secs` config.
 *  The Settings screen writes this to localStorage on every save. */
export const EXTENSION_POLL_INTERVAL_KEY = "hyvemind:extension-poll-interval-secs";
export const EXTENSION_POLL_INTERVAL_DEFAULT = 120;
export const EXTENSION_POLL_INTERVAL_MIN = 30;
export const EXTENSION_POLL_INTERVAL_MAX = 3600;

export const loadMergeTimeoutMs = (): number => {
  try {
    const raw = localStorage.getItem(MERGE_TIMEOUT_MIN_KEY);
    const parsed = raw == null ? NaN : Number(raw);
    if (Number.isFinite(parsed) && parsed >= 1) {
      return Math.floor(parsed) * 60 * 1000;
    }
  } catch {
    /* fall through */
  }
  return MERGE_TIMEOUT_DEFAULT_MIN * 60 * 1000;
};

export const loadContextTimeoutMs = (): number => {
  try {
    const raw = localStorage.getItem(CONTEXT_TIMEOUT_MIN_KEY);
    const parsed = raw == null ? NaN : Number(raw);
    if (Number.isFinite(parsed) && parsed >= 1) {
      return Math.floor(parsed) * 60 * 1000;
    }
  } catch {
    /* fall through */
  }
  return CONTEXT_TIMEOUT_DEFAULT_MIN * 60 * 1000;
};

/** Read the configured Nurse chat check-in interval in milliseconds.
 *  Synchronous so watchdog setup doesn't have to await IPC. The Settings
 *  screen keeps `CHAT_CHECK_IN_SECS_KEY` in localStorage in sync with the
 *  backend config. */
export const loadChatCheckInMs = (): number => {
  try {
    const raw = localStorage.getItem(CHAT_CHECK_IN_SECS_KEY);
    const parsed = raw == null ? NaN : Number(raw);
    if (Number.isFinite(parsed) && parsed >= CHAT_CHECK_IN_SECS_MIN && parsed <= CHAT_CHECK_IN_SECS_MAX) {
      return Math.floor(parsed) * 1000;
    }
  } catch {
    /* fall through */
  }
  return CHAT_CHECK_IN_SECS_DEFAULT * 1000;
};

const loadAutoModeDefault = (): AutoMode => {
  try {
    const raw = localStorage.getItem(AUTO_MODE_DEFAULT_KEY);
    if (raw === "full" || raw === "review" || raw === "off") return raw;
    // Legacy boolean string: "true" → full, anything else → off.
    if (raw === "true") return "full";
    return "off";
  } catch {
    return "off";
  }
};

interface TaskSessionEntry {
  sessionId: string | null;
  model?: string;
  hivemind?: string | null;
  thinking?: string;
  directory?: string;
  lastError?: string | null;
  projectPath?: string;
  planSessionId?: string;
  implSessionId?: string;
  planText?: string;
  taskPhase?: string;
  autoMode?: AutoMode | boolean;
  reviewCompleted?: boolean;
  pendingQuestions?: TaskQuestion[] | null;
  activeReviewJobId?: string | null;
  /** Best-known context window (tokens) for the selected model. Used as a
   *  fallback for the bottom-of-Tasks-view meter when Pi reports 0. */
  contextWindowHint?: number;
  /** Parsed FEATURES JSON for swarm-planning tasks. Persisted so the
   *  Launch Swarm button keeps working across reloads without re-parsing. */
  swarmFeatures?: SwarmFeatureSpec[];
  /** Parsed milestones for swarm-planning tasks. Persisted alongside
   *  `swarmFeatures` so the auto-launch path forwards them to `start_swarm`
   *  even when the swarm is launched after an app restart. */
  swarmMilestones?: MilestoneSpec[];
}

const loadTaskSessions = (): Record<string, TaskSessionEntry> => {
  try {
    const raw = localStorage.getItem(TASK_SESSIONS_KEY);
    return raw ? JSON.parse(raw) : {};
  } catch {
    return {};
  }
};

const saveTaskSessions = (sessions: Record<string, TaskSessionEntry>) => {
  try {
    localStorage.setItem(TASK_SESSIONS_KEY, JSON.stringify(sessions));
  } catch {
    /* quota exceeded */
  }
};

const loadTaskDrafts = (): Record<string, string> => {
  try {
    const raw = localStorage.getItem(TASK_DRAFTS_KEY);
    const parsed = raw ? JSON.parse(raw) : {};
    return parsed && typeof parsed === "object" ? parsed : {};
  } catch {
    return {};
  }
};

const saveTaskDrafts = (drafts: Record<string, string>) => {
  try {
    localStorage.setItem(TASK_DRAFTS_KEY, JSON.stringify(drafts));
  } catch {
    /* quota exceeded */
  }
};

const loadTaskMessagesFromDisk = async (taskId: string): Promise<TaskMessage[]> => {
  try {
    const raw = await ipc.loadTaskMessages(taskId);
    return raw ? JSON.parse(raw) : [];
  } catch {
    return [];
  }
};

const saveTaskMessagesToDisk = (taskId: string, messages: TaskMessage[]) => {
  ipc.saveTaskMessages(taskId, JSON.stringify(messages)).catch(() => {});
};

const deleteTaskMessagesFromDisk = (taskId: string) => {
  ipc.deleteTaskMessages(taskId).catch(() => {});
};

export const loadTaskList = (): TaskListItem[] => {
  try {
    const raw = localStorage.getItem(TASK_LIST_KEY);
    return raw ? JSON.parse(raw) : [];
  } catch {
    return [];
  }
};

const saveTaskList = (tasks: TaskListItem[]) => {
  try {
    localStorage.setItem(TASK_LIST_KEY, JSON.stringify(tasks));
  } catch {
    /* quota exceeded */
  }
};

/* ── Internal review-flow state ───────────────────────────── */

export interface ReviewFlowState {
  active: boolean;
  /** Coarse lifecycle marker. Backend now drives merge/context transitions
   *  via `hivemind-progress` events; this field is consulted only for
   *  display gates (composer lock, reconciler short-circuit). */
  phase: "context" | "round" | "merge" | "done";
  enrichedPrompt: string | null;
  currentRound: number;
  roundsConfig: RoundConfig[];
  currentPlan: string;
  currentJobId: string | null;
  isStartingRound?: boolean;
  contextSid: string | null;
  reviewId: string;
  hivemindId?: string;
  orchestratorModel?: string;
  orchestratorThinking?: string;
  orchestratorContextWindow?: number | null;
  orchestratorMaxOutput?: number | null;
  orchestratorInherit?: boolean;
  contextWatchdog?: ReturnType<typeof setTimeout> | null;
  /** Captured `prompt` arg from the context Pi's `submit_review_prompt`
   *  tool call. The context-done branch consumes this directly; no
   *  text-scraping fallback exists. */
  reviewPromptFromTool: string | null;
}

/**
 * Detect whether a message send should be routed as a steer to the
 * context-gather Pi session. Returns the contextSid when true, null otherwise.
 */
export function detectContextSteer(
  flow: ReviewFlowState | null | undefined,
): { isContextSteer: boolean; contextSid: string | null } {
  if (flow && flow.phase === "context" && flow.contextSid) {
    return { isContextSteer: true, contextSid: flow.contextSid };
  }
  return { isContextSteer: false, contextSid: null };
}

const modelRefForReview = (model: { id: string; provider: string }): string =>
  model.provider ? `${model.provider}/${model.id}` : model.id;

/** Extract the provider id from a "provider/model" string, defaulting to
 *  "anthropic" when the model id is bare (no slash). The frontend always
 *  stores fully-qualified ids, so the bare-id branch is a safety net. */

function providerOf(modelId: string): string {
  if (!modelId) return "anthropic";
  const idx = modelId.indexOf("/");
  if (idx === -1) return "anthropic";
  return modelId.slice(0, idx) || "anthropic";
}

export function resolveOrchestratorModel(
  hm:
    | {
        inherit_orchestrator: boolean;
        orchestrator_model: string | null;
        orchestrator_provider: string | null;
      }
    | null
    | undefined,
  inheritedModel: string,
): string {
  if (!hm || hm.inherit_orchestrator || !hm.orchestrator_model) return inheritedModel;
  const model = hm.orchestrator_model;
  const provider = hm.orchestrator_provider || "";
  if (!provider || model.startsWith(`${provider}/`)) return model;
  return `${provider}/${model}`;
}

export function buildReviewRoundStartOptions(
  flow: Pick<
    ReviewFlowState,
    "roundsConfig" | "currentRound" | "reviewId" | "hivemindId" | "orchestratorModel"
  >,
  name?: string,
  taskId?: string,
  projectPath?: string | null,
) {
  const round = flow.roundsConfig[flow.currentRound];
  // Build the per-model context window map keyed by "provider/model_id".
  // Only emit entries for models that actually carry a stored context_window
  // — the backend's `model_context_windows` is `Option<HashMap<_, _>>` so
  // missing entries cleanly degrade to the hardcoded `get_model_context_window`
  // fallback in engine.rs.
  const modelContextWindows: Record<string, number> = {};
  const modelTemperatures: Record<string, number> = {};
  const modelTopPs: Record<string, number> = {};
  for (const m of round.models) {
    const key = m.provider ? `${m.provider}/${m.id}` : m.id;
    if (typeof m.context_window === "number" && m.context_window > 0) {
      modelContextWindows[key] = m.context_window;
    }
    if (typeof m.temperature === "number" && Number.isFinite(m.temperature)) {
      modelTemperatures[key] = m.temperature;
    }
    if (typeof m.top_p === "number" && Number.isFinite(m.top_p)) {
      modelTopPs[key] = m.top_p;
    }
  }
  return {
    numRounds: 1,
    // 1-based cumulative round. The backend uses this as an offset so
    // round 2's `merge_started`/`merge_completed`/`verdicts_*` events
    // carry `round: 2` and round 2's capture files become
    // `merge-r2.txt` / `output-*-r2.txt` instead of overwriting round 1.
    roundNumber: flow.currentRound + 1,
    models: round.models.map(modelRefForReview),
    timeoutSeconds: round.timeout,
    reviewId: flow.reviewId,
    hivemindId: flow.hivemindId,
    name,
    taskId,
    // Persist which project the review ran against so the All Reviews page
    // can filter by project. Empty/whitespace strings collapse to undefined
    // so the backend leaves the column NULL rather than storing "".
    projectPath: projectPath && projectPath.trim() ? projectPath : undefined,
    // Send each map only when non-empty; the IPC layer happily passes
    // undefined through and the backend treats it as `None`.
    modelContextWindows:
      Object.keys(modelContextWindows).length > 0 ? modelContextWindows : undefined,
    modelTemperatures:
      Object.keys(modelTemperatures).length > 0 ? modelTemperatures : undefined,
    modelTopPs:
      Object.keys(modelTopPs).length > 0 ? modelTopPs : undefined,
    // Frontend-resolved orchestrator override. Already collapses the
    // hivemind's `inherit_orchestrator` flag against the Task's default
    // model (see triggerReviewForTask + the replay/resume paths). Without
    // this, the backend cannot know the Task's active model and would
    // fall through to "last reviewer in round" — the cause of the
    // Kimi/gpt-5.5 → azure-openai-responses merge deadlock.
    orchestratorModel: flow.orchestratorModel || undefined,
  };
}

// ---------------------------------------------------------------------------
// Merge-prompt context-window resolution
// ---------------------------------------------------------------------------

/** Resolved context window plus the provenance tier that produced it.
 *  The provenance string flows into review-log telemetry so future
 *  debuggers can tell at a glance whether the merge respected the user's
 *  stored value or fell back. */
export type CtxWindowSource =
  | "orchestrator_stored"
  | "inherited_model_stored"
  | "catalog"
  | "fallback";

export interface ResolvedContextWindow {
  contextWindow: number;
  source: CtxWindowSource;
}

/** Strip a leading "provider/" prefix from a fully-qualified model id. */
function stripProviderPrefix(id: string): { provider: string | null; modelId: string } {
  if (!id) return { provider: null, modelId: "" };
  const idx = id.indexOf("/");
  if (idx === -1) return { provider: null, modelId: id };
  return { provider: id.slice(0, idx), modelId: id.slice(idx + 1) };
}

/** Resolve the orchestrator's context window from the strongest available
 *  source. Priority order:
 *    1. `orchestrator_context_window` stored against the hivemind, when not
 *       inheriting.
 *    2. If inheriting (or the stored value is missing): the matched reviewer
 *       row's `context_window`, comparing on `(provider, model_id)` tuples
 *       normalised across both sides.
 *    3. A catalog lookup (provider+modelId) via the in-runtime cache built
 *       lazily from `refreshModels()`.
 *    4. A 200k constant (logs a warning via the caller).
 */
export function resolveMergeContextWindow(
  hm: { inherit_orchestrator: boolean; orchestrator_context_window: number | null } | null | undefined,
  mergeModelId: string,
  roundsConfig: RoundConfig[],
  catalogLookup: (provider: string, modelId: string) => number | undefined,
): ResolvedContextWindow {
  // 1. Orchestrator-stored
  if (hm && !hm.inherit_orchestrator && typeof hm.orchestrator_context_window === "number" && hm.orchestrator_context_window > 0) {
    return { contextWindow: hm.orchestrator_context_window, source: "orchestrator_stored" };
  }

  // 2. Inherited reviewer row match — normalise both sides to (provider, modelId).
  const target = stripProviderPrefix(mergeModelId);
  const targetModelId = target.modelId;
  const targetProvider = target.provider;
  for (const round of roundsConfig) {
    for (const m of round.models) {
      const sameModelId = m.id === targetModelId || m.id === mergeModelId;
      if (!sameModelId) continue;
      // Require provider match when both sides expose provider info.
      if (targetProvider && m.provider && m.provider !== targetProvider) continue;
      if (typeof m.context_window === "number" && m.context_window > 0) {
        return { contextWindow: m.context_window, source: "inherited_model_stored" };
      }
    }
  }

  // 3. Catalog lookup via refreshModels cache.
  const lookupProvider = targetProvider || providerOf(mergeModelId);
  const lookupModelId = targetModelId || mergeModelId;
  const fromCatalog = catalogLookup(lookupProvider, lookupModelId);
  if (typeof fromCatalog === "number" && fromCatalog > 0) {
    return { contextWindow: fromCatalog, source: "catalog" };
  }

  // 4. Last-resort fallback.
  return { contextWindow: 200_000, source: "fallback" };
}

/** Resolve the output reservation (tokens) for the merge prompt.
 *  Pulls from `orchestrator_max_output` first (when set + not inheriting),
 *  otherwise looks for an inherited reviewer row, otherwise defaults to 16k.
 *  Final value is clamped to [4096, 64000] AND to floor(contextWindow * 0.25)
 *  so a model advertising a huge max_output doesn't starve the input budget. */
export function resolveOutputReservation(
  hm: { inherit_orchestrator: boolean; orchestrator_max_output: number | null } | null | undefined,
  mergeModelId: string,
  roundsConfig: RoundConfig[],
  contextWindow: number,
): number {
  let raw: number | null = null;
  if (hm && !hm.inherit_orchestrator && typeof hm.orchestrator_max_output === "number" && hm.orchestrator_max_output > 0) {
    raw = hm.orchestrator_max_output;
  } else {
    const target = stripProviderPrefix(mergeModelId);
    for (const round of roundsConfig) {
      for (const m of round.models) {
        const sameModelId = m.id === target.modelId || m.id === mergeModelId;
        if (!sameModelId) continue;
        if (target.provider && m.provider && m.provider !== target.provider) continue;
        if (typeof m.max_output === "number" && m.max_output > 0) {
          raw = m.max_output;
          break;
        }
      }
      if (raw !== null) break;
    }
  }
  const base = raw ?? 16_384;
  const lo = 4_096;
  const hi = 64_000;
  const ctxClamp = Math.floor(contextWindow * 0.25);
  return Math.max(lo, Math.min(hi, Math.min(base, ctxClamp)));
}

/* ── Reconciliation decider (pure, unit-testable) ─────────── */

/** Threshold after which a merge with no chunks is considered stuck. Polling
 *  reconciliation surfaces a user-visible error rather than waiting for the
 *  in-flight merge watchdog (which only catches the case where Pi never
 *  emits anything at all). */
export const RECONCILE_MERGE_STUCK_MS = 450_000;

/** Threshold after which a context-gather Pi with no events is considered
 *  stuck. Same rationale as RECONCILE_MERGE_STUCK_MS, but for the context
 *  phase — RC3/5 of the fix: the context phase previously had no
 *  reconciliation at all. */
export const RECONCILE_CONTEXT_STUCK_MS = 450_000;

/** Output safety cap for the context-gather Pi. Streamed text is accumulated
 *  in `reviewAccumulatorsRef`; once it crosses this threshold the session
 *  is aborted with a user-facing error rather than allowed to keep dumping
 *  full files. Observed runaway runs hit ~85KB in 7 minutes while still
 *  streaming — capping well above that catches true runaways while leaving
 *  generous room for legitimate large bundles. */
export const CONTEXT_OUTPUT_CAP_CHARS = 600_000;

export type ReconcileDecision =
  | { kind: "noop" }
  | { kind: "merge_stuck"; jobId: string }
  | { kind: "context_stuck"; reviewId: string }
  | { kind: "ended"; status: "completed" | "failed" | "cancelled"; error?: string };

/** Compare backend SQLite state against the in-memory orchestration flow and
 *  produce a recovery decision. Pure — no side effects. The caller is
 *  responsible for unconditionally syncing UI rows from the snapshot
 *  (`review_resync`) outside of this decider.
 *
 *  Context-phase reconciliation does not consult the backend snapshot
 *  (the context Pi runs entirely on the orchestrator side; SQLite has
 *  nothing to say about it). It is driven purely by `contextIdleMs`
 *  against the flow's `contextSid`. */
export function decideReconcile(
  snapshot: ReviewStateSnapshot,
  flow: ReviewFlowState | null,
  mergeIdleMs: number,
  mergeStuckThresholdMs: number,
  contextIdleMs: number = 0,
  contextStuckThresholdMs: number = RECONCILE_CONTEXT_STUCK_MS,
): ReconcileDecision {
  // Context-phase stuck check. Independent of snapshot — the snapshot's
  // job_id refers to a hivemind round, while the context Pi runs before
  // any round is dispatched. RC3 of the fix: previously there was no
  // recovery path here at all.
  if (
    flow?.phase === "context" &&
    flow.contextSid &&
    contextIdleMs > contextStuckThresholdMs
  ) {
    return { kind: "context_stuck", reviewId: flow.reviewId };
  }
  // Stale-jobId guard. If the in-memory flow has advanced to a different
  // jobId, the snapshot refers to a previous round and must not drive any
  // recovery action. RC5 of the fix: tightened to apply only when
  // flow.phase === "round" — a stale jobId during merge is the exact
  // symptom of a missed handoff (not a reason to ignore it), so we let
  // the merge_stuck check below run regardless.
  if (
    flow &&
    flow.phase === "round" &&
    flow.currentJobId !== null &&
    flow.currentJobId !== snapshot.job_id
  ) {
    return { kind: "noop" };
  }
  const status = snapshot.status;
  if (status === "failed" || status === "cancelled") {
    return { kind: "ended", status, error: snapshot.error ?? undefined };
  }
  if (status === "completed") {
    if (flow?.phase === "round") {
      // Backend owns the merge dispatch now; the frontend never advances to
      // merge from a reconcile signal.
      return { kind: "noop" };
    }
    if (flow?.phase === "merge") {
      if (mergeIdleMs > mergeStuckThresholdMs) {
        return { kind: "merge_stuck", jobId: snapshot.job_id };
      }
      return { kind: "noop" };
    }
    if (!flow) {
      // Backend says completed but we have no in-memory orchestration to
      // advance. UI sync via review_resync above is the best we can do.
      return { kind: "ended", status: "completed" };
    }
  }
  return { kind: "noop" };
}

/* ── Public API ───────────────────────────────────────────── */

/** Options for `triggerReviewForTask`. */
export type TriggerReviewOptions = { force?: boolean };
export type TriggerReviewForTask = (taskId: string, opts?: TriggerReviewOptions) => Promise<void>;

/** Returns true when a task is in the exact post-`review_error` state that
 *  should surface the "Retry review" affordance: phase ratcheted at
 *  "review", a visible error, and no active/interrupted review state. The
 *  UI render condition and the runtime guard share this predicate so they
 *  stay aligned. */
export function canRetryErroredReviewState(
  t: Pick<
    TaskRuntimeState,
    "phase" | "error" | "reviewInterrupted" | "reviewProgress" | "activeReviewJobId" | "streaming"
  >,
): boolean {
  return (
    t.phase === "review" &&
    !!t.error &&
    !t.reviewInterrupted &&
    !t.reviewProgress &&
    !t.activeReviewJobId &&
    !t.streaming
  );
}

/** Decide whether a Hivemind `completed` event for the current job should
 *  advance to the next round (FE drives multi-round one job at a time) or
 *  finalise the review. Pure helper so the listener stays small and the
 *  decision is unit-testable. */
export type RoundCompletionDecision =
  | { kind: "advance"; nextRound: number }
  | { kind: "finish" };

export function decideRoundCompletion(
  flow: Pick<ReviewFlowState, "currentRound" | "roundsConfig">,
): RoundCompletionDecision {
  const total = flow.roundsConfig.length;
  // `currentRound` is 0-based here. After job N completes, advance to N+1
  // when N+1 is still within bounds, otherwise finish. Degenerate
  // length<=1 always finishes immediately.
  if (total <= 1) return { kind: "finish" };
  const nextRound = flow.currentRound + 1;
  if (nextRound < total) return { kind: "advance", nextRound };
  return { kind: "finish" };
}

export interface CreateTaskOpts {
  prompt?: string;
  model?: string;
  hivemind?: string | null;
  projectPath?: string | null;
  setActive?: boolean;
  autoMode?: AutoMode | boolean;
  thinking?: string;
  images?: ImageAttachment[];
  title?: string;
  /** Mark this task as the planning conversation for a Swarm. When set,
   *  the runtime uses QUEEN_PLANNING_SYSTEM_PROMPT in the plan phase and
   *  the PlanCard shows "Launch Swarm" instead of "Implement". */
  swarmId?: string;
  /** Optional sidebar description. When provided, written alongside title
   *  on the new TaskListItem instead of being derived from the prompt. */
  description?: string;
}

interface SubmitOverrides {
  model?: string;
  thinking?: string;
  projectPath?: string | null;
  images?: ImageAttachment[];
}

/** Kind of input the user must supply before the task can advance.
 *  Derived in `TaskRuntimeProvider` from runtime state and surfaced on
 *  `TaskRuntimeStateApi.awaitingInputTaskIds` so the sidebar can render
 *  a single unified awaiting-input badge. */
export type AwaitingInputKind =
  | "questions"           // pendingQuestions card in the conversation
  | "swarm-questions"     // pendingSwarmQuestions modal
  | "plan-ready"          // plan finished, awaiting Implement click (non-swarm)
  | "swarm-plan-ready";   // plan finished, awaiting Launch Swarm click

export interface TaskRuntimeApi {
  tasks: Record<string, TaskRuntimeState>;
  localTasks: TaskListItem[];
  activeId: string;
  hivemindOptions: HivemindSummary[];
  defaultModel: string;
  defaultProjectPath: string;
  defaultHivemind: string;
  streamingTaskIds: Record<string, boolean>;
  awaitingInputTaskIds: Record<string, AwaitingInputKind>;

  setActiveTask: (id: string) => void;
  updateTask: (taskId: string, updater: (t: TaskRuntimeState) => TaskRuntimeState) => void;
  setLocalTasks: (updater: (prev: TaskListItem[]) => TaskListItem[]) => void;

  /** Per-task composer draft text. Backed by a ref + debounced localStorage
   *  save so typing never triggers a React state update. */
  getDraft: (taskId: string) => string;
  setDraft: (taskId: string, value: string) => void;

  createTask: (opts: CreateTaskOpts) => string;
  submitMessage: (
    taskId: string,
    prompt: string,
    overrides?: { model?: string; thinking?: string; projectPath?: string | null; images?: ImageAttachment[] },
  ) => Promise<void>;
  stopTask: (taskId: string) => Promise<void>;
  deleteTask: (taskId: string) => void;
  triggerReviewForTask: TriggerReviewForTask;
  refreshHivemindOptions: (prefetched?: HivemindSummary[]) => Promise<void>;

  /** User-initiated retry from the review error bar. Resumes from a
   *  SQLite-backed snapshot when available; otherwise resets to plan-ready
   *  and starts a fresh review. */
  retryReview: (taskId: string) => Promise<void>;

  implementPlan: (
    taskId: string,
    fallbackUsage?: { input: number; output: number; contextPercent: number },
  ) => Promise<void>;
  answerQuestions: (
    taskId: string,
    questions: TaskQuestion[],
    answers: Record<string, any>,
  ) => Promise<void>;
  /** Phase 4C — submit answers to a batch of ``swarm-question`` blocks
   *  emitted by the Queen-planning agent. Builds the literal
   *  `[Answers] {…}` user message via `buildSwarmAnswerPrompt`, sends it
   *  through the existing chat IPC (no new Tauri command needed), clears
   *  the task's `pendingSwarmQuestions`, and records the answered ids
   *  in `answeredSwarmQuestionIds` so a re-derivation on the next chunk
   *  doesn't re-pop the modal.
   *
   *  `answers` ordering matters: the entries are written into the
   *  `[Answers] {…}` payload in array order so the Queen reads them in
   *  the same order it asked. */
  submitSwarmAnswers: (
    taskId: string,
    answers: ReadonlyArray<{ id: string; value: string }>,
  ) => Promise<void>;
  /** Phase 4C — bypass the current batch of ``swarm-question`` blocks. Sends
   *  the literal `skipped` sentinel for every pending question id, advancing
   *  the conversation while telling the Queen the user chose not to answer.
   *  Same persistence as `submitSwarmAnswers` (the ids are recorded as
   *  answered). */
  skipSwarmQuestions: (taskId: string) => Promise<void>;
  /** User-initiated recovery for an interrupted review at any phase. Reads
   *  `task.reviewInterrupted` and branches on `phase`:
   *    - context: restart context-gather Pi with persisted plan
   *    - round: re-dispatch round N via ipc.startReview
   *    - merge: backend now owns recovery; the frontend surfaces an error
   *    - between_rounds: dispatch round N+1 with merge output as plan
   *    - final: surface the final plan and mark plan-ready
   *
   *  `stateOverride` lets the retry path pass a freshly-fetched snapshot
   *  directly into the resume logic without first round-tripping through
   *  the reducer (which would race with `tasksRef.current`). */
  resumeReview: (taskId: string, stateOverride?: ReviewInterruptedState) => Promise<void>;

  /** Create a new task and immediately start a Hivemind review using an existing
   *  enriched prompt (from a prior review). Skips context gathering — goes
   *  straight to round 1 model dispatch with the selected Hivemind. */
  replayReview: (opts: {
    enrichedPrompt: string;
    hivemindId: string;
    projectPath?: string | null;
  }) => string;
  /** Arms the 3-min features-refresh watchdog and dispatches the given
   *  follow-up prompt. The only supported way to set
   *  `pendingFeaturesRefresh: true` in conjunction with a Pi dispatch.
   *  Both writers (`finishReviewFlow` and the user-initiated Re-emit
   *  FEATURES retry) go through this helper. See JSDoc on the helper
   *  in `taskRuntime.tsx`. */
  armFeaturesRefresh: (taskId: string, followUp: string) => void;
}

/* ── Split contexts (audit 6.7) ────────────────────────────
 *
 * The legacy `TaskRuntimeContext` exposed a single mega-object whose
 * value changed on every state update, forcing every consumer to
 * re-render on every keystroke / streaming chunk / hivemind refresh.
 *
 * For audit item 6.7 we split the public surface into six narrow
 * contexts so consumers subscribe only to the slice they actually
 * read. The provider populates all six inside one render so they
 * stay perfectly in sync; nothing about the runtime logic moves.
 *
 * The legacy `useTaskRuntime()` hook is preserved as a compatibility
 * shim: it composes all six slices into the same `TaskRuntimeApi`
 * object the old call sites expect. New code should prefer the
 * narrower hooks (`useTaskList`, `useTaskRuntimeState`, etc.) so the
 * re-render isolation actually kicks in.
 */

/** Refs-based draft (composer text). Stable identity for the life of
 *  the provider — keystrokes do not change the context value, so
 *  no consumers re-render when the user types. */
export interface TaskDraftApi {
  getDraft: (taskId: string) => string;
  setDraft: (taskId: string, value: string) => void;
}

/** The sidebar list of past tasks + active selection. Changes only
 *  when a task is created / deleted / reordered / activated. */
export interface TaskListApi {
  localTasks: TaskListItem[];
  activeId: string;
  setActiveTask: (id: string) => void;
  setLocalTasks: (updater: (prev: TaskListItem[]) => TaskListItem[]) => void;
}

/** Active-task streaming state. Updates frequently (every chunk).
 *  Subscribed to by the message panel only — the sidebar list lives
 *  in TaskListApi and won't re-render here. */
export interface TaskRuntimeStateApi {
  tasks: Record<string, TaskRuntimeState>;
  streamingTaskIds: Record<string, boolean>;
  awaitingInputTaskIds: Record<string, AwaitingInputKind>;
  updateTask: (taskId: string, updater: (t: TaskRuntimeState) => TaskRuntimeState) => void;
}

/** Stable-identity dispatch surface (handlers via `useMemo`). The
 *  value identity should not change once the provider has mounted,
 *  so consumers that depend only on these functions re-render
 *  effectively zero times after mount. */
export interface TaskActionsApi {
  createTask: (opts: CreateTaskOpts) => string;
  submitMessage: (
    taskId: string,
    prompt: string,
    overrides?: SubmitOverrides,
  ) => Promise<void>;
  stopTask: (taskId: string) => Promise<void>;
  deleteTask: (taskId: string) => void;
  triggerReviewForTask: TriggerReviewForTask;
  retryReview: (taskId: string) => Promise<void>;
  implementPlan: (
    taskId: string,
    fallbackUsage?: { input: number; output: number; contextPercent: number },
  ) => Promise<void>;
  answerQuestions: (
    taskId: string,
    questions: TaskQuestion[],
    answers: Record<string, any>,
  ) => Promise<void>;
  submitSwarmAnswers: (
    taskId: string,
    answers: ReadonlyArray<{ id: string; value: string }>,
  ) => Promise<void>;
  skipSwarmQuestions: (taskId: string) => Promise<void>;
  resumeReview: (taskId: string, stateOverride?: ReviewInterruptedState) => Promise<void>;
  replayReview: (opts: {
    enrichedPrompt: string;
    hivemindId: string;
    projectPath?: string | null;
  }) => string;
  /** See `TaskRuntimeApi.armFeaturesRefresh`. */
  armFeaturesRefresh: (taskId: string, followUp: string) => void;
}

/** Hivemind picker options + refresh hook. Changes when the user
 *  creates / deletes a hivemind. */
export interface HivemindOptionsApi {
  hivemindOptions: HivemindSummary[];
  refreshHivemindOptions: (prefetched?: HivemindSummary[]) => Promise<void>;
}

/** Default-model / default-project-path / default-hivemind values
 *  read off `Settings`. Changes when the user picks a new default. */
export interface DefaultsApi {
  defaultModel: string;
  defaultProjectPath: string;
  defaultHivemind: string;
}

const TaskDraftContext = createContext<TaskDraftApi | null>(null);
const TaskListContext = createContext<TaskListApi | null>(null);
const TaskRuntimeStateContext = createContext<TaskRuntimeStateApi | null>(null);
const TaskActionsContext = createContext<TaskActionsApi | null>(null);
const HivemindOptionsContext = createContext<HivemindOptionsApi | null>(null);
const DefaultsContext = createContext<DefaultsApi | null>(null);

const TaskRuntimeContext = createContext<TaskRuntimeApi | null>(null);

export function useTaskRuntime(): TaskRuntimeApi {
  const ctx = useContext(TaskRuntimeContext);
  if (!ctx) {
    throw new Error("useTaskRuntime must be used within a TaskRuntimeProvider");
  }
  return ctx;
}

/** Hook into the composer-draft refs. Returned object has stable
 *  identity; the actual draft text is held in refs and changes do
 *  not trigger re-renders here. */
export function useTaskDrafts(): TaskDraftApi {
  const ctx = useContext(TaskDraftContext);
  if (!ctx) throw new Error("useTaskDrafts must be used within a TaskRuntimeProvider");
  return ctx;
}

/** Hook into the sidebar list of past tasks + active-task id. */
export function useTaskList(): TaskListApi {
  const ctx = useContext(TaskListContext);
  if (!ctx) throw new Error("useTaskList must be used within a TaskRuntimeProvider");
  return ctx;
}

/** Hook into the active-task streaming state. Re-renders on every
 *  chunk — subscribe sparingly. */
export function useTaskRuntimeState(): TaskRuntimeStateApi {
  const ctx = useContext(TaskRuntimeStateContext);
  if (!ctx) throw new Error("useTaskRuntimeState must be used within a TaskRuntimeProvider");
  return ctx;
}

/** Hook into the dispatch surface (handlers). Stable identity. */
export function useTaskActions(): TaskActionsApi {
  const ctx = useContext(TaskActionsContext);
  if (!ctx) throw new Error("useTaskActions must be used within a TaskRuntimeProvider");
  return ctx;
}

/** Hook into the Hivemind picker options. */
export function useHivemindOptions(): HivemindOptionsApi {
  const ctx = useContext(HivemindOptionsContext);
  if (!ctx) throw new Error("useHivemindOptions must be used within a TaskRuntimeProvider");
  return ctx;
}

/** Hook into the runtime's mirror of the three default-* Settings
 *  values. For the underlying source of truth see SettingsProvider. */
export function useDefaults(): DefaultsApi {
  const ctx = useContext(DefaultsContext);
  if (!ctx) throw new Error("useDefaults must be used within a TaskRuntimeProvider");
  return ctx;
}

/* ── Provider ─────────────────────────────────────────────── */

/** Minimal navigation callback shape — matches `GoFn` in App.tsx but defined
 *  locally to avoid a circular type import. The provider uses it to route
 *  swarm-linked tasks straight to the swarm-control surface when auto-mode
 *  auto-launches them. */
type NavCallback = (tab: string, payload?: any) => void;

export function TaskRuntimeProvider({
  children,
  go,
}: {
  children: React.ReactNode;
  go?: NavCallback;
}) {
  const { project, projects } = useProject();
  const projectsRef = useRef(projects);
  projectsRef.current = projects;
  /** Mutable navigation handle — captured each render so the auto-implement
   *  effect can route swarm-linked tasks to swarm-control without re-running
   *  on every render. */
  const goRef = useRef<NavCallback | undefined>(go);
  goRef.current = go;

  /* ── Refs that survive across re-renders ────────────────── */
  const defaultModelRef = useRef("");
  const defaultProjectPathRef = useRef("");
  const defaultHivemindRef = useRef("");
  const nextIdRef = useRef(1);

  const sessionIdToTaskIdRef = useRef<Record<string, string>>({});
  const internalSessionIdsRef = useRef<Set<string>>(new Set());
  const sessionIdToReviewIdRef = useRef<Record<string, string>>({});
  const reviewFlowsRef = useRef<Record<string, ReviewFlowState | null>>({});
  const reviewAccumulatorsRef = useRef<Record<string, string>>({});
  const mountedRef = useRef(true);

  /** In-runtime catalog cache of `refreshModels()` results, keyed by
   *  `${provider}/${model_id}` -> context_window (tokens). Lazily populated
   *  by `resolveMergeContextWindow`'s catalog tier when the stored values
   *  are unavailable. Populated per-provider (scoped) so we don't fan out
   *  to every configured provider on a cache miss. */
  const modelCatalogCacheRef = useRef<Record<string, number>>({});
  const modelCatalogProviderLoadedRef = useRef<Set<string>>(new Set());

  const lastSavedMessagesRef = useRef<Record<string, TaskMessage[]>>({});
  const lastStreamingForSaveRef = useRef<Record<string, boolean>>({});
  const saveTimersRef = useRef<Record<string, ReturnType<typeof setTimeout>>>({});

  /** Per-session Nurse check-in timers for regular (non-internal)
   *  Tasks-view chat sessions. Armed on the `start` chat-event, cleared on
   *  `done` / `error`. Hivemind context/merge sessions have their own
   *  watchdogs on the review flow.
   *
   *  `epoch` is bumped on every (re)arm and absent after clear. The `fire`
   *  closure captures the epoch before awaiting the Nurse IPC and bails
   *  after the await if the epoch no longer matches — preventing a
   *  late-arriving Nurse response from rearming a watchdog whose session
   *  has already ended. */
  const chatWatchdogsRef = useRef<
    Record<string, { id: ReturnType<typeof setTimeout>; epoch: number }>
  >({});
  const chatWatchdogEpochRef = useRef(0);

  const triggerReviewRef = useRef<TriggerReviewForTask | null>(null);
  const startNextRoundRef = useRef<((taskId: string) => Promise<void>) | null>(null);
  const finishReviewRef = useRef<((taskId: string, finalPlan: string) => void) | null>(null);
  const reconcileReviewRef = useRef<((taskId: string) => Promise<void>) | null>(null);

  /** Last `chunk` event timestamp per task's active merge session. Backend
   *  is authoritative for merge lifecycle; this is retained only to drive
   *  reconciler idle-time bookkeeping when a backend `merge_chunk` arrives. */
  const mergeLastChunkAtRef = useRef<Record<string, number>>({});
  /** Last chunk/thinking event timestamp per task's active context-gather
   *  session. Mirror of mergeLastChunkAtRef for the context phase — RC3/5
   *  of the fix: enables context_stuck reconciliation. */
  const contextLastEventAtRef = useRef<Record<string, number>>({});

  /** Per-task watchdog timer for a features-refresh turn. Armed by
   *  `armFeaturesRefreshAndDispatch` from BOTH writers of
   *  `pendingFeaturesRefresh` (`finishReviewFlow` and the user-initiated
   *  Re-emit FEATURES retry in Tasks.tsx). With the reducer changes that
   *  clear `pendingFeaturesRefresh` on `done` / `error` / `stop`, the
   *  cancel effect below now beats this timer in the common case — the
   *  timer is a last-resort safety net for Pi subprocesses that never
   *  produce a terminal event (e.g., subprocess crash, host OOM). If the
   *  timer does fire, it sets `featuresRefreshFailed: true` so the
   *  recovery UI (PlanCard amber banner + Re-emit button + enabled Launch)
   *  is consistent with the reducer-path recovery. */
  const pendingFeaturesRefreshTimersRef = useRef<Record<string, ReturnType<typeof setTimeout>>>({});

  /* ── State ──────────────────────────────────────────────── */
  const [tasks, setTasks] = useState<Record<string, TaskRuntimeState>>({});
  const tasksRef = useRef(tasks);
  tasksRef.current = tasks;

  /* ── Drafts: ref-backed, debounced-persisted. Kept OUT of `tasks` so
   *  typing in the composer never triggers a React render of the task
   *  tree. The Composer owns its own local state and only writes here
   *  on task switch / unmount. */
  const draftsRef = useRef<Record<string, string>>(
    isTauri() ? loadTaskDrafts() : {},
  );
  const draftsSaveTimerRef = useRef<number | null>(null);
  const scheduleDraftsSave = useCallback(() => {
    if (!isTauri()) return;
    if (draftsSaveTimerRef.current != null) return;
    draftsSaveTimerRef.current = window.setTimeout(() => {
      draftsSaveTimerRef.current = null;
      saveTaskDrafts(draftsRef.current);
    }, 500);
  }, []);
  const getDraft = useCallback((taskId: string): string => {
    return draftsRef.current[taskId] ?? "";
  }, []);
  const setDraft = useCallback(
    (taskId: string, value: string) => {
      const cur = draftsRef.current[taskId] ?? "";
      if (cur === value) return;
      if (value === "") {
        delete draftsRef.current[taskId];
      } else {
        draftsRef.current[taskId] = value;
      }
      scheduleDraftsSave();
    },
    [scheduleDraftsSave],
  );

  const [localTasks, setLocalTasks] = useState<TaskListItem[]>(() => {
    if (!isTauri()) return [];
    return migrateTasks(assignSortOrders(loadTaskList()));
  });

  const [activeId, setActiveId] = useState<string>(() => {
    if (!isTauri()) return "t-now";
    const saved = loadTaskList();
    if (saved.length > 0) {
      for (const t of saved) {
        const m = t.id.match(/^task-(\d+)$/);
        if (m) nextIdRef.current = Math.max(nextIdRef.current, parseInt(m[1]) + 1);
      }
      const lastActive = localStorage.getItem(ACTIVE_TASK_KEY);
      if (lastActive && saved.some((t) => t.id === lastActive)) return lastActive;
      return saved[0].id;
    }
    const id = `task-${nextIdRef.current++}`;
    return id;
  });
  const activeIdRef = useRef(activeId);
  activeIdRef.current = activeId;

  const [hivemindOptions, setHivemindOptions] = useState<HivemindSummary[]>([]);

  /* ── Revision counters ───────────────────────────────────── */
  const [defaultModelRevision, setDefaultModelRevision] = useState(0);
  const [defaultProjectPathRevision, setDefaultProjectPathRevision] = useState(0);
  const [defaultHivemindRevision, setDefaultHivemindRevision] = useState(0);

  /* ── Flush pending draft writes before page unload ──────── */
  useEffect(() => {
    if (!isTauri()) return;
    const flush = () => {
      if (draftsSaveTimerRef.current != null) {
        window.clearTimeout(draftsSaveTimerRef.current);
        draftsSaveTimerRef.current = null;
      }
      saveTaskDrafts(draftsRef.current);
    };
    window.addEventListener("beforeunload", flush);
    return () => {
      window.removeEventListener("beforeunload", flush);
      flush();
    };
  }, []);

  /* ── updateTask ─────────────────────────────────────────── */
  const updateTask = useCallback(
    (taskId: string, updater: (prev: TaskRuntimeState) => TaskRuntimeState) => {
      setTasks((prev) => {
        const cur = prev[taskId];
        if (!cur) return prev;
        const next = updater(cur);
        if (next === cur) return prev;
        return { ...prev, [taskId]: next };
      });
    },
    [],
  );

  /** Synchronously mirror a task update into `tasksRef.current` *and* schedule
   *  the corresponding React state update. Used by retry/resume paths where
   *  the very next instruction reads `tasksRef.current[taskId]` and React
   *  will not yet have committed the update. Returns the new state (or null
   *  if the task no longer exists). */
  const syncTaskRefAndState = useCallback(
    (
      taskId: string,
      updater: (t: TaskRuntimeState) => TaskRuntimeState,
    ): TaskRuntimeState | null => {
      const cur = tasksRef.current[taskId];
      if (!cur) return null;
      const next = updater(cur);
      tasksRef.current = { ...tasksRef.current, [taskId]: next };
      updateTask(taskId, () => next);
      return next;
    },
    [updateTask],
  );

  const bindTaskSession = useCallback((taskId: string, sessionId?: string | null) => {
    if (!sessionId) return;
    sessionIdToTaskIdRef.current[sessionId] = taskId;
  }, []);

  const resolveTaskIdForSession = useCallback((sessionId: string): string | undefined => {
    const mapped = sessionIdToTaskIdRef.current[sessionId];
    if (mapped && tasksRef.current[mapped]) return mapped;

    for (const [taskId, task] of Object.entries(tasksRef.current)) {
      if (task.sessionId === sessionId || task.internalPi?.sessionId === sessionId) {
        sessionIdToTaskIdRef.current[sessionId] = taskId;
        return taskId;
      }
    }

    return undefined;
  }, []);

  /** Clear stale review-flow state for a task before overwriting it. Cancels
   *  pending context/merge watchdog timers, drops accumulators, and removes
   *  the prior context/merge session ids from the routing maps so late
   *  events on those sessions are ignored. */
  const clearStaleReviewFlow = useCallback((taskId: string) => {
    const oldFlow = reviewFlowsRef.current[taskId];
    const staleSessionIds = [oldFlow?.contextSid].filter(
      (sid): sid is string => !!sid,
    );

    if (oldFlow?.contextWatchdog) clearTimeout(oldFlow.contextWatchdog);

    reviewFlowsRef.current[taskId] = null;
    delete contextLastEventAtRef.current[taskId];
    delete reviewAccumulatorsRef.current[taskId];

    for (const sid of staleSessionIds) {
      delete sessionIdToTaskIdRef.current[sid];
      internalSessionIdsRef.current.delete(sid);
      delete sessionIdToReviewIdRef.current[sid];
    }
  }, []);

  /** Collect every Pi session id currently associated with a task — primary
   *  chat session, internal Pi (context/merge), review-flow contextSid, and
   *  any session in the reverse map that resolves back to this task. */
  const collectTaskSessionIds = useCallback((taskId: string): string[] => {
    const ids = new Set<string>();
    const t = tasksRef.current[taskId];
    if (t?.sessionId) ids.add(t.sessionId);
    if (t?.internalPi?.sessionId) ids.add(t.internalPi.sessionId);
    const flow = reviewFlowsRef.current[taskId];
    if (flow?.contextSid) ids.add(flow.contextSid);
    for (const [sid, mapped] of Object.entries(sessionIdToTaskIdRef.current)) {
      if (mapped === taskId) ids.add(sid);
    }
    return [...ids];
  }, []);

  /** Best-effort stop of any internal-review Pi sessions associated with a
   *  task. Used by the retry path before resuming from a snapshot/in-memory
   *  state to avoid leaving zombie context/merge Pi sessions running. */
  const stopStaleInternalReviewSessions = useCallback(
    async (taskId: string, cur?: TaskRuntimeState) => {
      const oldFlow = reviewFlowsRef.current[taskId];
      const ids = new Set<string>();
      if (oldFlow?.contextSid) ids.add(oldFlow.contextSid);
      if (cur?.internalPi?.sessionId) ids.add(cur.internalPi.sessionId);
      if (cur?.sessionId && internalSessionIdsRef.current.has(cur.sessionId)) {
        ids.add(cur.sessionId);
      }

      await Promise.all(
        [...ids].map((sid) =>
          ipc.stopChat(sid).catch((e) => {
            console.warn(
              "[review] failed to stop stale review session during retry",
              sid,
              e,
            );
          }),
        ),
      );
    },
    [],
  );

  /** Per-task in-flight guard for `retryReview` so rapid double-clicks or
   *  programmatic re-invocations cannot start duplicate retry flows. */
  const retryReviewInFlightRef = useRef<Set<string>>(new Set());

  /* ── Track mount state (guard against state updates after unmount) ── */
  useEffect(() => {
    mountedRef.current = true;
    return () => {
      mountedRef.current = false;
    };
  }, []);

  /* ── Settings load (default model + project path) ───────── */
  useEffect(() => {
    if (!isTauri()) return;
    ipc
      .getSettings()
      .then(async (s) => {
        if (s.default_project_path) {
          defaultProjectPathRef.current = s.default_project_path;
        }
        if (s.default_hivemind) {
          defaultHivemindRef.current = s.default_hivemind;
        }
        let model = s.default_model || "";
        if (model && !model.includes("/")) {
          try {
            const catalog = await ipc.refreshModels();
            const match = catalog.find((m) => m.model_id === model);
            if (match) {
              model = `${match.provider}/${model}`;
              ipc.setDefaultModel(model).catch(() => {});
            }
          } catch {}
        }
        if (model) {
          console.info(`[task-runtime] defaultModelRef updated from settings: "${model}"`);
          defaultModelRef.current = model;
          const cur = tasksRef.current[activeIdRef.current];
          if (cur && !cur.model) {
            updateTask(activeIdRef.current, (t) => ({ ...t, model }));
          }
        }
      })
      .catch((e) => console.error("Failed to load settings", e));
  }, [updateTask]);

  /* ── Listen: default model changes from Settings ────────── */
  useEffect(() => {
    if (!isTauri()) return;
    let unlisten: UnlistenFn | undefined;
    let mounted = true;

    listen<{ model: string | null }>("default-model-changed", (event) => {
      if (!mounted) return;
      // Backend always saves qualified "provider/model" strings.
      // Set ref directly — no async resolution needed.
      const newModel = event.payload.model ?? "";
      console.info(`[task-runtime] defaultModelRef updated from event: "${newModel}" (was "${defaultModelRef.current}")`);
      defaultModelRef.current = newModel;
      setDefaultModelRevision((r) => r + 1);
    }).then((fn) => {
      if (!mounted) { fn(); return; }
      unlisten = fn;
    });

    return () => {
      mounted = false;
      safeUnlisten(unlisten);
    };
  }, []);

  /* ── Listen: default project path changes from Settings ──── */
  useEffect(() => {
    if (!isTauri()) return;
    let unlisten: UnlistenFn | undefined;
    let mounted = true;

    listen<{ path: string | null }>("default-project-path-changed", (event) => {
      if (!mounted) return;
      defaultProjectPathRef.current = event.payload.path ?? "";
      setDefaultProjectPathRevision((r) => r + 1);
    }).then((fn) => {
      if (!mounted) { fn(); return; }
      unlisten = fn;
    });

    return () => {
      mounted = false;
      safeUnlisten(unlisten);
    };
  }, []);

  /* ── Listen: default hivemind changes from Settings ──── */
  useEffect(() => {
    if (!isTauri()) return;
    let unlisten: UnlistenFn | undefined;
    let mounted = true;

    listen<{ hivemind: string | null }>("default-hivemind-changed", (event) => {
      if (!mounted) return;
      defaultHivemindRef.current = event.payload.hivemind ?? "";
      setDefaultHivemindRevision((r) => r + 1);
    }).then((fn) => {
      if (!mounted) { fn(); return; }
      unlisten = fn;
    });

    return () => {
      mounted = false;
      safeUnlisten(unlisten);
    };
  }, []);

  /* ── refreshHivemindOptions for Settings dropdown sync ── */
  const refreshHivemindOptions = useCallback(
    async (prefetched?: HivemindSummary[]) => {
      if (!isTauri()) return;
      try {
        const options = prefetched ?? await ipc.listHiveminds();
        setHivemindOptions(options);

        // If the current default hivemind was deleted, reset to "no default"
        if (defaultHivemindRef.current) {
          const stillExists = options.some((o) => o.id === defaultHivemindRef.current);
          if (!stillExists) {
            defaultHivemindRef.current = "";
            setDefaultHivemindRevision((r) => r + 1);
          }
        }
      } catch (e) {
        console.error("Failed to refresh hivemind options:", e);
      }
    },
    [],
  );

  /* ── Hivemind catalog ───────────────────────────────────── */
  useEffect(() => {
    if (!isTauri()) return;
    ipc.listHiveminds().then(setHivemindOptions).catch(console.error);
  }, []);

  /* ── Listener: pi-session-evicted ─────────────────────── */
  /* The Rust-side maintenance loop emits this when an idle / bloated Pi
   * session is killed. Sweep every session-keyed ref so we don't try to
   * route stale events for a session id that no longer exists. */
  useEffect(() => {
    if (!isTauri()) return;
    let unlisten: UnlistenFn | undefined;
    let mounted = true;

    listen<{ session_id: string }>("pi-session-evicted", (event) => {
      if (!mounted) return;
      const sid = event.payload.session_id;
      if (!sid) return;
      const taskId = resolveTaskIdForSession(sid);
      const isPrimaryTaskSession =
        !!taskId && tasksRef.current[taskId]?.sessionId === sid;
      // Remove stale internal/review mappings. Keep primary task-session
      // routing intact: the on-disk transcript can be respawned under the
      // same session id, and follow-up answers must still receive events.
      if (isPrimaryTaskSession) {
        bindTaskSession(taskId, sid);
      } else {
        delete sessionIdToTaskIdRef.current[sid];
      }
      delete sessionIdToReviewIdRef.current[sid];
      internalSessionIdsRef.current.delete(sid);
      console.debug("pi-session-evicted: cleaned refs for", sid);
    }).then((fn) => {
      if (!mounted) { fn(); return; }
      unlisten = fn;
    });

    return () => {
      mounted = false;
      safeUnlisten(unlisten);
    };
  }, [bindTaskSession, resolveTaskIdForSession]);

  /* ── Persist localTasks + active id ─────────────────────── */
  useEffect(() => {
    if (isTauri() && localTasks.length > 0) saveTaskList(localTasks);
  }, [localTasks]);

  useEffect(() => {
    if (isTauri()) localStorage.setItem(ACTIVE_TASK_KEY, activeId);
  }, [activeId]);

  // Backend now owns the entire review lifecycle, including crash recovery.
  // The frontend no longer reconciles merge_runs on rehydrate; if the user
  // reloads mid-review the backend's progress events will repopulate state.
  const recoverInterruptedReviewForTask = useCallback(
    async (_taskId: string, _jobId: string) => {
      return;
    },
    [],
  );

  /* ── Initial hydration on mount ─────────────────────────── */
  useEffect(() => {
    if (!isTauri()) return;
    (async () => {
      // Migrate stale localStorage messages → disk
      const keysToMigrate: string[] = [];
      for (let i = 0; i < localStorage.length; i++) {
        const key = localStorage.key(i);
        if (key?.startsWith(TASK_MESSAGES_PREFIX)) keysToMigrate.push(key);
      }
      for (const key of keysToMigrate) {
        const taskId = key.replace(TASK_MESSAGES_PREFIX, "");
        const raw = localStorage.getItem(key);
        if (raw) await ipc.saveTaskMessages(taskId, raw).catch(() => {});
        localStorage.removeItem(key);
      }

      const savedSessions = loadTaskSessions();
      const savedList = migrateTasks(assignSortOrders(loadTaskList()));
      const ids = new Set<string>();
      for (const item of savedList) ids.add(item.id);
      for (const k of Object.keys(savedSessions)) ids.add(k);
      ids.add(activeIdRef.current);

      const built: Record<string, TaskRuntimeState> = {};
      for (const taskId of ids) {
        const entry = savedSessions[taskId];
        const item = savedList.find((t) => t.id === taskId);
        const model = entry?.model || item?.model || defaultModelRef.current;
        const messages = await loadTaskMessagesFromDisk(taskId);

        const seed: TaskRuntimeState = {
          ...makeInitialTaskState(taskId, model),
          sessionId: entry?.sessionId ?? null,
          model,
          hivemind: entry?.hivemind ?? item?.hivemind ?? null,
          thinking: entry?.thinking || "high",
          autoMode: normalizeAutoMode(entry?.autoMode),
          reviewCompleted: entry?.reviewCompleted ?? false,
          projectPath: entry?.projectPath ?? item?.projectPath ?? null,
          swarmId: item?.swarmId ?? null,
          swarmFeatures: entry?.swarmFeatures ?? null,
          swarmMilestones: entry?.swarmMilestones ?? null,
          phase: ((entry?.taskPhase as TaskPhase) ?? "intake") as TaskPhase,
          planText: entry?.planText ?? null,
          error: entry?.lastError ?? null,
          activeReviewJobId: entry?.activeReviewJobId ?? null,
          contextWindowHint: entry?.contextWindowHint,
        };

        const folded = applyTaskEvent(
          seed,
          { kind: "resync", messages },
          defaultModelRef.current,
        );
        built[taskId] = folded;
        lastSavedMessagesRef.current[taskId] = folded.messages;
        lastStreamingForSaveRef.current[taskId] = folded.streaming;

        if (entry?.sessionId) sessionIdToTaskIdRef.current[entry.sessionId] = taskId;
      }

      setTasks((prev) => {
        const out: Record<string, TaskRuntimeState> = { ...built };
        for (const [tid, existing] of Object.entries(prev)) {
          if (!out[tid]) {
            // Task was created in-memory before hydration finished (e.g. via
            // QuickTaskDialog). It isn't on disk yet, so preserve it as-is.
            out[tid] = existing;
          } else if (existing.messages.length > out[tid].messages.length) {
            // In-memory copy has progressed beyond what was on disk; prefer it.
            out[tid] = existing;
          }
        }
        return out;
      });

      for (const [tid, t] of Object.entries(built)) {
        if (t.reviewCompleted) continue;
        if (t.activeReviewJobId) {
          const jobId = t.activeReviewJobId;
          ipc
            .getReviewState(jobId)
            .then((snapshot) => {
              updateTask(tid, (cur) =>
                applyTaskEvent(cur, { kind: "review_resync", snapshot }, defaultModelRef.current),
              );
              // Mount-scan reconciliation: if an active review's status is
              // "completed" but the in-memory orchestration is gone (we just
              // hydrated from disk), the reducer's review_resync only fixes UI.
              // reconcileReview surfaces the terminal state correctly.
              reconcileReviewRef.current?.(tid);
            })
            .catch((e) => console.warn("getReviewState failed", e));
          recoverInterruptedReviewForTask(tid, jobId).catch((e) =>
            console.warn("recoverInterruptedReviewForTask failed", e),
          );
        } else {
          // Crash-recovery probe: even without an in-memory activeReviewJobId,
          // SQLite may have an interrupted review tied to this task. The
          // reconciler's getResumableReviewForTask call will surface it.
          reconcileReviewRef.current?.(tid);
        }
      }

      // Renderer-reload reconciler: after the task list is fully hydrated
      // from disk, ask the backend to kill any Task-owned Pi sessions whose
      // ids we no longer reference. This cleans up orphans after a webview
      // reload without touching Review/Merge/Swarm sessions. The
      // known_ids set is a union of all session-keyed maps.
      try {
        const known = new Set<string>();
        for (const sid of Object.keys(sessionIdToTaskIdRef.current)) {
          if (sid) known.add(sid);
        }
        for (const sid of Object.keys(sessionIdToReviewIdRef.current)) {
          if (sid) known.add(sid);
        }
        for (const sid of internalSessionIdsRef.current) {
          if (sid) known.add(sid);
        }
        const killed = await ipc.reconcileActiveSessions(Array.from(known));
        if (killed.length > 0) {
          console.info(
            "reconcileActiveSessions: killed orphan sessions",
            killed,
          );
        }
      } catch (e) {
        console.warn("reconcileActiveSessions failed", e);
      }
    })();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  /* ── Seed initial empty task ────────────────────────────── */
  useEffect(() => {
    if (!isTauri()) return;
    if (localTasks.length === 0) {
      const seedProjectPath = defaultProjectPathRef.current || project?.cwd || "";
      setLocalTasks([
        {
          id: activeId,
          group: "Active",
          title: "New Task",
          project: seedProjectPath ? workspaceLabel(seedProjectPath) : "",
          model: "",
          phase: "intake",
          when: "now",
          preview: "",
          active: true,
          projectPath: seedProjectPath,
          createdAt: Date.now(),
        },
      ]);
      setTasks((prev) => {
        if (prev[activeId]) return prev;
        const seed = makeInitialTaskState(activeId, defaultModelRef.current);
        return {
          ...prev,
          [activeId]: {
            ...seed,
            autoMode: loadAutoModeDefault(),
            messages: [{ who: "asst", text: "What would you like to build?", model: seed.model, createdAt: Date.now() }],
          },
        };
      });
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  /* ── Sidebar phase mirroring ───────────────────────────── */
  useEffect(() => {
    if (!isTauri()) return;
    setLocalTasks((prev) => {
      let changed = false;
      const next = prev.map((t) => {
        const rt = tasks[t.id];
        if (!rt) return t;
        if (rt.phase !== t.phase) {
          changed = true;
          return { ...t, phase: rt.phase };
        }
        return t;
      });
      return changed ? next : prev;
    });
  }, [tasks]);

  /* ── Persistence: messages to disk ───────────────────────── */
  useEffect(() => {
    if (!isTauri()) return;
    for (const [tid, t] of Object.entries(tasks)) {
      if (t.messages.length === 0) continue;
      if (lastSavedMessagesRef.current[tid] === t.messages) continue;
      const wasStreaming =
        lastStreamingForSaveRef.current[tid] === true && t.streaming === false;
      const delay = wasStreaming ? 0 : 500;
      if (saveTimersRef.current[tid]) clearTimeout(saveTimersRef.current[tid]);
      const messagesAtScheduleTime = t.messages;
      saveTimersRef.current[tid] = setTimeout(() => {
        saveTaskMessagesToDisk(tid, messagesAtScheduleTime);
        lastSavedMessagesRef.current[tid] = messagesAtScheduleTime;
        delete saveTimersRef.current[tid];
      }, delay);
    }
    for (const [tid, t] of Object.entries(tasks)) {
      lastStreamingForSaveRef.current[tid] = t.streaming;
    }
  }, [tasks]);

  useEffect(() => {
    return () => {
      for (const tid of Object.keys(saveTimersRef.current)) {
        clearTimeout(saveTimersRef.current[tid]);
      }
    };
  }, []);

  /* ── Persistence: meta to localStorage ─────────────────── */
  useEffect(() => {
    if (!isTauri()) return;
    const sessions = loadTaskSessions();
    let changed = false;
    for (const [tid, t] of Object.entries(tasks)) {
      const cur = sessions[tid] || { sessionId: null };
      const next: TaskSessionEntry = {
        sessionId: t.sessionId,
        model: t.model,
        hivemind: t.hivemind,
        thinking: t.thinking,
        lastError: t.error,
        projectPath: t.projectPath ?? undefined,
        planText: t.planText ?? undefined,
        taskPhase: t.phase,
        autoMode: t.autoMode,
        reviewCompleted: t.reviewCompleted,
        pendingQuestions: t.pendingQuestions,
        activeReviewJobId: t.activeReviewJobId,
        contextWindowHint: t.contextWindowHint,
        swarmFeatures: t.swarmFeatures ?? undefined,
        swarmMilestones: t.swarmMilestones ?? undefined,
      };
      const featuresChanged =
        (cur.swarmFeatures?.length ?? 0) !== (next.swarmFeatures?.length ?? 0) ||
        (cur.swarmMilestones?.length ?? 0) !== (next.swarmMilestones?.length ?? 0);
      if (
        cur.sessionId !== next.sessionId ||
        cur.model !== next.model ||
        cur.hivemind !== next.hivemind ||
        cur.thinking !== next.thinking ||
        cur.lastError !== next.lastError ||
        cur.projectPath !== next.projectPath ||
        cur.planText !== next.planText ||
        cur.taskPhase !== next.taskPhase ||
        cur.autoMode !== next.autoMode ||
        cur.reviewCompleted !== next.reviewCompleted ||
        (cur.activeReviewJobId ?? null) !== (next.activeReviewJobId ?? null) ||
        (cur.contextWindowHint ?? undefined) !== (next.contextWindowHint ?? undefined) ||
        featuresChanged
      ) {
        sessions[tid] = { ...cur, ...next };
        changed = true;
      }
    }
    if (changed) saveTaskSessions(sessions);
  }, [tasks]);

  /* ── Apply TASK_META → sidebar title/preview ──────────────
   *
   * The planning agent emits a TASK_META block on its first text response;
   * the reducer extracts it into `tasks[id].taskMeta`. This effect pushes
   * those values into `localTasks` (the sidebar list). Design notes:
   *  • `taskMetaKey` is a derived `useMemo` whose value only changes when a
   *    NEW task gets `taskMeta`. This avoids re-running on every streaming
   *    chunk — the effect body is silent until the meta block is parsed.
   *  • `appliedMetaRef` tracks which task IDs have already had their meta
   *    applied, so a follow-up plan turn (which clears `taskMeta` then sets
   *    a new one) can re-apply. The set is mutated inside the setLocalTasks
   *    updater after a matching TaskListItem is confirmed — this handles the
   *    race where `taskMeta` arrives before the TaskListItem is created.
   *  • `titleEdited === true` uses strict equality — future code paths that
   *    set `titleEdited: false` explicitly won't accidentally lock the title.
   *  • When `titleEdited === true`, the title is preserved (user owns the
   *    title) but the preview/description is still updated (agent owns the
   *    description). This asymmetry is intentional. */
  const taskMetaKey = useMemo(
    () =>
      Object.entries(tasks)
        .filter(([, t]) => t.taskMeta != null)
        .map(([id, t]) => `${id}:${t.taskMeta!.title}:${t.taskMeta!.description}`)
        .sort()
        .join("|"),
    [tasks],
  );
  const appliedMetaRef = useRef<Set<string>>(new Set());
  useEffect(() => {
    for (const [tid, t] of Object.entries(tasks)) {
      if (!t.taskMeta) continue;
      const key = `${tid}:${t.taskMeta.title}:${t.taskMeta.description}`;
      if (appliedMetaRef.current.has(key)) continue;
      setLocalTasks((prev) => {
        let applied = false;
        const next = prev.map((item) => {
          if (item.id !== tid) return item;
          applied = true;
          if (item.titleEdited === true) {
            return { ...item, preview: t.taskMeta!.description };
          }
          return {
            ...item,
            title: t.taskMeta!.title,
            preview: t.taskMeta!.description,
          };
        });
        if (applied) appliedMetaRef.current.add(key);
        return applied ? next : prev;
      });
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [taskMetaKey]);

  /* ── Mark active task in sidebar ─────────────────────── */
  useEffect(() => {
    if (!isTauri()) return;
    setLocalTasks((prev) => {
      let changed = false;
      const next = prev.map((t) => {
        const isActive = t.id === activeId;
        if (!!t.active === isActive) return t;
        changed = true;
        return { ...t, active: isActive };
      });
      return changed ? next : prev;
    });
  }, [activeId]);

  /* ── Listener: chat-event ──────────────────────────────── */
  useEffect(() => {
    if (!isTauri()) return;
    let unlisten: UnlistenFn | undefined;
    let mounted = true;

    onChatEvent((e) => {
      if (!mounted) return;
      const taskId = resolveTaskIdForSession(e.session_id);
      if (!taskId) return;

      const flow = reviewFlowsRef.current[taskId] ?? null;
      const isReviewInternal = internalSessionIdsRef.current.has(e.session_id);

      // Capture the context Pi's `submit_review_prompt` tool args. Fires
      // before the regular `tool_start` event (the backend emits the
      // structured event first). The captured payload is the only path —
      // there is no text-scraping fallback.
      if (e.event_type === "structured_review_prompt") {
        if (flow && flow.contextSid === e.session_id) {
          try {
            const parsed = JSON.parse(e.content);
            const prompt =
              typeof parsed?.prompt === "string" ? parsed.prompt : null;
            if (prompt && prompt.length > 0) {
              flow.reviewPromptFromTool = prompt;
              ipc
                .logReviewEvent(flow.reviewId, "context_tool_args_captured", {
                  session_id: e.session_id,
                  prompt_length: prompt.length,
                })
                .catch(() => {});
            }
          } catch (err) {
            console.warn(
              "[review] failed to parse submit_review_prompt args",
              err,
            );
          }
        }
        return;
      }

      // Usage events drive the Tasks-view bottom bar. They must route to the
      // reducer based on whether this session is the *currently active* Pi
      // session for the task — including context/merge sessions during a
      // Hivemind review (which are otherwise short-circuited as "internal").
      // The bar should always reflect the agent producing tokens right now.
      if (e.event_type === "usage") {
        const cur = tasksRef.current[taskId];
        if (cur && cur.sessionId && e.session_id === cur.sessionId) {
          const ev = mapChatEventToTaskEvent(e);
          if (ev) {
            setTasks((prev) => {
              const c = prev[taskId];
              if (!c) return prev;
              const next = applyTaskEvent(c, ev, defaultModelRef.current);
              return next === c ? prev : { ...prev, [taskId]: next };
            });
          }
        }
        // Internal review sessions don't need any further processing for
        // usage events — the existing internal handler ignores them anyway.
        if (isReviewInternal) return;
        return;
      }

      if (isReviewInternal) {
        // Mirror chunk/thinking/tool_* events into the message list so the
        // user can watch the context/merge Pi work inline in the main chat
        // area. Runs *in addition* to (not instead of) the existing
        // accumulator/log/done paths. The `internal_pi_message_start`
        // dispatch is idempotent (no-op if a message with this sessionId
        // already exists), so dispatching on every event is safe.
        if (
          flow &&
          (e.event_type === "chunk" ||
            e.event_type === "thinking" ||
            e.event_type === "tool_start" ||
            e.event_type === "tool_update" ||
            e.event_type === "tool_end")
        ) {
          const isContext = flow.contextSid === e.session_id;
          if (isContext) {
            const reviewKind: { phase: "context" | "merge"; round?: number; reviewId: string } =
              { phase: "context", reviewId: flow.reviewId };
            const modelName = flow.orchestratorModel || "";
            setTasks((prev) => {
              const cur = prev[taskId];
              if (!cur) return prev;
              let next = applyTaskEvent(
                cur,
                {
                  kind: "internal_pi_message_start",
                  sessionId: e.session_id,
                  reviewKind,
                  modelName,
                },
                defaultModelRef.current,
              );
              if (e.event_type === "chunk") {
                next = applyTaskEvent(
                  next,
                  { kind: "internal_pi_chunk", sessionId: e.session_id, content: e.content },
                  defaultModelRef.current,
                );
              } else if (e.event_type === "thinking") {
                next = applyTaskEvent(
                  next,
                  { kind: "internal_pi_thinking", sessionId: e.session_id, content: e.content },
                  defaultModelRef.current,
                );
              } else if (e.event_type === "tool_start") {
                try {
                  const data = JSON.parse(e.content);
                  next = applyTaskEvent(
                    next,
                    { kind: "internal_pi_tool_start", sessionId: e.session_id, data },
                    defaultModelRef.current,
                  );
                } catch {}
              } else if (e.event_type === "tool_update") {
                try {
                  const data = JSON.parse(e.content);
                  next = applyTaskEvent(
                    next,
                    { kind: "internal_pi_tool_update", sessionId: e.session_id, data },
                    defaultModelRef.current,
                  );
                } catch {}
              } else if (e.event_type === "tool_end") {
                try {
                  const data = JSON.parse(e.content);
                  next = applyTaskEvent(
                    next,
                    { kind: "internal_pi_tool_end", sessionId: e.session_id, data },
                    defaultModelRef.current,
                  );
                } catch {}
              }
              return next === cur ? prev : { ...prev, [taskId]: next };
            });
          }
        }

        if (e.event_type === "chunk") {
          reviewAccumulatorsRef.current[taskId] =
            (reviewAccumulatorsRef.current[taskId] || "") + e.content;
          // Liveness signal for the reconciler: any merge chunk resets the
          // idle clock. Without this a slow-but-progressing merge would be
          // misclassified as stuck.
          if (flow?.phase === "context" && e.session_id === flow.contextSid) {
            contextLastEventAtRef.current[taskId] = Date.now();
            const accLen = reviewAccumulatorsRef.current[taskId].length;
            if (accLen > CONTEXT_OUTPUT_CAP_CHARS) {
              const overflowSid = flow.contextSid;
              const overflowReviewId = flow.reviewId;
              const errMsg = `Hivemind context-gather exceeded ${CONTEXT_OUTPUT_CAP_CHARS} chars (${accLen} streamed) — the orchestrator model is likely dumping full files instead of focused excerpts. Try a different orchestrator model or rerun.`;
              if (flow.contextWatchdog) {
                clearTimeout(flow.contextWatchdog);
                flow.contextWatchdog = null;
              }
              internalSessionIdsRef.current.delete(overflowSid);
              delete sessionIdToReviewIdRef.current[overflowSid];
              delete contextLastEventAtRef.current[taskId];
              reviewAccumulatorsRef.current[taskId] = "";
              flow.contextSid = null;
              reviewFlowsRef.current[taskId] = null;
              ipc.stopChat(overflowSid).catch((err) => {
                console.warn("[review] stopChat after context overflow failed", err);
              });
              ipc
                .logReviewEvent(overflowReviewId, "context_output_overflow", {
                  session_id: overflowSid,
                  accumulated_len: accLen,
                  cap: CONTEXT_OUTPUT_CAP_CHARS,
                })
                .catch(() => {});
              setTasks((prev) => {
                const cur = prev[taskId];
                if (!cur) return prev;
                const next = applyTaskEvent(
                  cur,
                  { kind: "internal_pi_failed", sessionId: overflowSid, message: errMsg },
                  defaultModelRef.current,
                );
                return next === cur ? prev : { ...prev, [taskId]: next };
              });
              updateTask(taskId, (cur) =>
                applyTaskEvent(
                  cur,
                  { kind: "review_error", error: errMsg },
                  defaultModelRef.current,
                ),
              );
              return;
            }
          }
          return;
        }
        if (e.event_type === "thinking") {
          // Thinking events also count as liveness for the context Pi —
          // a context-gather can spend many minutes purely in thinking mode
          // before producing any text chunks.
          if (flow?.phase === "context" && e.session_id === flow.contextSid) {
            contextLastEventAtRef.current[taskId] = Date.now();
          }
        }
        if (e.event_type === "tps") {
          // Live TPS for the active context/merge Pi. The reducer's
          // internal_pi_tps action is sessionId-guarded so a late event
          // from a finished context Pi can't clobber the merge Pi's TPS.
          try {
            const data = JSON.parse(e.content);
            if (typeof data.tps === "number") {
              setTasks((prev) => {
                const cur = prev[taskId];
                if (!cur) return prev;
                const next = applyTaskEvent(
                  cur,
                  { kind: "internal_pi_tps", sessionId: e.session_id, tps: data.tps },
                  defaultModelRef.current,
                );
                return next === cur ? prev : { ...prev, [taskId]: next };
              });
            }
          } catch {}
          return;
        }
        if (e.event_type !== "chunk") {
          const rid = flow?.reviewId || sessionIdToReviewIdRef.current[e.session_id] || "";
          ipc
            .logReviewEvent(rid, "review_chat_event", {
              session_id: e.session_id,
              event_type: e.event_type,
              flow_phase: flow?.phase ?? null,
              flow_context_sid: flow?.contextSid ?? null,
              flow_current_round: flow?.currentRound ?? null,
              flow_current_job_id: flow?.currentJobId ?? null,
              accumulated_len: (reviewAccumulatorsRef.current[taskId] || "").length,
            })
            .catch(() => {});
        }
        if (e.event_type === "error") {
          const errorMsg = e.content || "Unknown review session error";
          internalSessionIdsRef.current.delete(e.session_id);

          const sidToKill = e.session_id;
          queueMicrotask(() => {
            ipc.stopChat(sidToKill).catch((err) => {
              console.warn("[review] stopChat after error failed", err);
            });
          });

          reviewAccumulatorsRef.current[taskId] = "";

          if (!flow) {
            const fallbackRid = sessionIdToReviewIdRef.current[e.session_id] || "";
            ipc
              .logReviewEvent(fallbackRid, "review_internal_error_no_flow", {
                session_id: e.session_id,
                task_id: taskId,
                error: errorMsg,
              })
              .catch(() => {});
            delete sessionIdToReviewIdRef.current[e.session_id];
            return;
          }

          ipc
            .logReviewEvent(flow.reviewId, "review_internal_error", {
              session_id: e.session_id,
              flow_phase: flow.phase,
              error: errorMsg,
            })
            .catch(() => {});
          if (flow.contextWatchdog) {
            clearTimeout(flow.contextWatchdog);
            flow.contextWatchdog = null;
          }

          delete sessionIdToReviewIdRef.current[e.session_id];
          delete mergeLastChunkAtRef.current[taskId];
          delete contextLastEventAtRef.current[taskId];
          reviewFlowsRef.current[taskId] = null;

          const phaseLabel = flow.phase === "context"
            ? "Context gathering"
            : flow.phase === "merge"
              ? `Merge (round ${flow.currentRound + 1}/${flow.roundsConfig.length})`
              : `Review (${flow.phase})`;

          updateTask(taskId, (t) => ({
            ...t,
            error: `${phaseLabel} failed: ${errorMsg}`,
            streaming: false,
            reviewProgress: null,
            activeReviewJobId: null,
            internalPi: null,
            phase: PHASE_RANK[t.phase] > PHASE_RANK["plan-ready"] ? t.phase : "plan-ready",
          }));
          return;
        }
        if (e.event_type === "done") {
          // Mark the telemetry strip's internalPi slot as "done" so its
          // final stats persist visibly between phases (per design: the
          // strip stays until the next context/merge Pi overwrites it,
          // or the review ends).
          setTasks((prev) => {
            const cur = prev[taskId];
            if (!cur) return prev;
            const next = applyTaskEvent(
              cur,
              { kind: "internal_pi_done", sessionId: e.session_id },
              defaultModelRef.current,
            );
            return next === cur ? prev : { ...prev, [taskId]: next };
          });
          internalSessionIdsRef.current.delete(e.session_id);
          // Phase 3: explicitly free the Pi semaphore permit. Pi spawns its
          // own subprocess per session and holds the permit until kill_session
          // drops the Arc. Without this, context/merge sessions linger and
          // can starve the next merge spawn (Pi pool deadlock). Microtask so
          // it doesn't reorder ahead of the rest of the listener.
          const sidToKill = e.session_id;
          queueMicrotask(() => {
            ipc.stopChat(sidToKill).catch((err) => {
              console.warn("[review] stopChat after done failed", err);
            });
          });
          const accumulatedText = reviewAccumulatorsRef.current[taskId] || "";
          reviewAccumulatorsRef.current[taskId] = "";
          if (!flow) {
            const fallbackRid = sessionIdToReviewIdRef.current[e.session_id] || "";
            ipc
              .logReviewEvent(fallbackRid, "merge_done_no_flow", {
                session_id: e.session_id,
                task_id: taskId,
                accumulated_len: accumulatedText.length,
              })
              .catch(() => {});
            delete sessionIdToReviewIdRef.current[e.session_id];
            return;
          }
          if (flow.phase === "context" && e.session_id === flow.contextSid) {
            // Clear the context watchdog now that the context Pi finished.
            if (flow.contextWatchdog) {
              clearTimeout(flow.contextWatchdog);
              flow.contextWatchdog = null;
            }
            delete contextLastEventAtRef.current[taskId];
            ipc
              .logReviewEvent(flow.reviewId, "context_done_branch", {
                session_id: e.session_id,
                accumulated_len: accumulatedText.length,
              })
              .catch(() => {});
            // Extract from the streamed accumulator first. If the IPC chat-
            // event stream dropped chunks (Pi broadcast capacity lag under
            // burst, slow webview consumer, etc.) the accumulator may be
            // missing the END marker even though Pi durably persisted the
            // full text to its session JSONL on disk. Fall back to reading
            // that authoritative transcript before declaring the extract
            // failed. The fallback only fires when extract returned null
            // so the happy path stays synchronous and zero-cost.
            const failedSid = e.session_id;
            const sessionIdForFetch = e.session_id;
            const reviewIdSnapshot = flow.reviewId;
            const flowRef = flow;
            const proceedContext = (enriched: string) => {
              flowRef.enrichedPrompt = enriched;
              flowRef.currentPlan = enriched;
              flowRef.phase = "round";
              // Append a closing session-divider for the context Pi so the
              // user sees a clean phase boundary in the conversation.
              updateTask(taskId, (t) => ({
                ...t,
                messages: [
                  ...t.messages,
                  {
                    who: "session-divider",
                    dividerLabel: "Hivemind context complete",
                    dividerSessionId: flowRef.contextSid ?? undefined,
                    createdAt: Date.now(),
                  },
                ],
              }));
              ipc
                .logReviewEvent(flowRef.reviewId, "context_completed", {
                  session_id: flowRef.contextSid,
                  prompt_length: enriched.length,
                })
                .catch(() => {});
              // Null contextSid so any in-flight context-watchdog Nurse
              // response (still pending from before this success path
              // fired) is recognised as stale by the
              // `flowAfter.contextSid !== contextSid` guard in
              // handleNurseDecision. Without this the late response would
              // rearm the watchdog against a dead session and surface a
              // misleading user-facing error 60s later.
              flowRef.contextSid = null;
              console.info("[review] RC2: context-done success path (startNextRound queued)");
              delete sessionIdToReviewIdRef.current[sessionIdForFetch];
              queueMicrotask(() => startNextRoundRef.current?.(taskId));
            };
            const surfaceContextExtractFailure = (
              accumLen: number,
              fallbackLen: number,
              fallbackHadStart: boolean,
              fallbackHadEnd: boolean,
            ) => {
              const errMsg =
                "Context gathering finished but the agent did not call the `submit_review_prompt` tool — review aborted.";
              ipc
                .logReviewEvent(reviewIdSnapshot, "context_extract_failed", {
                  session_id: failedSid,
                  accumulated_len: accumLen,
                  fallback_len: fallbackLen,
                  prompt_start_present: fallbackHadStart,
                  prompt_end_present: fallbackHadEnd,
                })
                .catch(() => {});
              setTasks((prev) => {
                const cur = prev[taskId];
                if (!cur) return prev;
                const next = applyTaskEvent(
                  cur,
                  { kind: "internal_pi_failed", sessionId: failedSid, message: errMsg },
                  defaultModelRef.current,
                );
                return next === cur ? prev : { ...prev, [taskId]: next };
              });
              updateTask(taskId, (t) => ({
                ...t,
                error: errMsg,
                streaming: false,
                reviewProgress: null,
                activeReviewJobId: null,
                internalPi: null,
                phase: PHASE_RANK[t.phase] > PHASE_RANK["plan-ready"] ? t.phase : "plan-ready",
              }));
              reviewFlowsRef.current[taskId] = null;
              console.warn("[review] RC2: context-done error path taken (extract failed)");
              delete sessionIdToReviewIdRef.current[failedSid];
            };
            // The context Pi delivers its enriched prompt by calling the
            // `submit_review_prompt` tool — the payload is captured into
            // `flowRef.reviewPromptFromTool` by the `structured_review_prompt`
            // chat-event handler before the model's final `done` arrives.
            // No text-scraping fallback exists; if the tool wasn't called
            // we surface a clean failure and let Nurse retry.
            const fromTool = flowRef.reviewPromptFromTool;
            if (fromTool && fromTool.length > 0) {
              ipc
                .logReviewEvent(reviewIdSnapshot, "context_used_tool_payload", {
                  session_id: failedSid,
                  prompt_length: fromTool.length,
                  accumulated_len: accumulatedText.length,
                })
                .catch(() => {});
              proceedContext(fromTool);
              return;
            }
            surfaceContextExtractFailure(accumulatedText.length, 0, false, false);
            return;
          } else {
            ipc
              .logReviewEvent(flow.reviewId, "merge_done_branch_mismatch", {
                session_id: e.session_id,
                flow_phase: flow.phase,
                flow_context_sid: flow.contextSid,
                flow_merge_sid: null,
                accumulated_len: accumulatedText.length,
              })
              .catch(() => {});
          }
          delete sessionIdToReviewIdRef.current[e.session_id];
          return;
        }
        return;
      }

      // Nurse check-in watchdog for regular (non-internal) chat sessions.
      // Armed on `start`, cleared on `done`/`error`. Hivemind context/merge
      // sessions are short-circuited by the `isReviewInternal` branch
      // above, so they don't double-arm — they have their own watchdogs.
      if (e.event_type === "start") {
        const sid = e.session_id;
        // Clear any stale timer first.
        const existing = chatWatchdogsRef.current[sid];
        if (existing) clearTimeout(existing.id);

        const fire = async (): Promise<void> => {
          const entry = chatWatchdogsRef.current[sid];
          if (!entry) return;
          const myEpoch = entry.epoch;
          let decision: ipc.NurseDecisionDto;
          try {
            decision = await ipc.checkChatSession(sid, "chat");
          } catch (err) {
            console.warn("[chat] nurse check failed", err);
            const errStr = ipc.formatIpcError(err);
            decision = {
              kind: "cancel",
              reasoning: `nurse evaluation failed: ${errStr}`,
              message: `Nurse check failed: ${errStr}`,
            };
          }
          // Bail if the watchdog was cleared (done/error) or rearmed by a
          // different fire() while our IPC was in flight.
          if (chatWatchdogsRef.current[sid]?.epoch !== myEpoch) {
            console.debug("[chat] nurse decision stale (epoch mismatch), dropping", { sid });
            return;
          }

          // Session was torn down between watchdog fire and Nurse evaluation.
          // Clear the timer silently — no error UI.
          if (decision.kind === "noop") {
            delete chatWatchdogsRef.current[sid];
            return;
          }

          if (decision.kind === "leave_it" || decision.kind === "steer") {
            const nextMs =
              decision.kind === "leave_it"
                ? Math.max(1, Math.min(decision.check_back_secs, 1800)) * 1000
                : loadChatCheckInMs();
            const nextEpoch = ++chatWatchdogEpochRef.current;
            chatWatchdogsRef.current[sid] = {
              id: setTimeout(() => {
                fire().catch(() => {});
              }, nextMs),
              epoch: nextEpoch,
            };
            return;
          }

          // restart / cancel → stop the chat session and surface as an error.
          delete chatWatchdogsRef.current[sid];
          ipc.stopChat(sid).catch(() => {});
          const userMsg =
            decision.kind === "cancel"
              ? decision.message ||
                "Chat session was cancelled by Nurse."
              : `Chat session was restarted by Nurse (${decision.reasoning}).`;
          setTasks((prev) => {
            const cur = prev[taskId];
            if (!cur) return prev;
            const next = applyTaskEvent(
              cur,
              { kind: "error", message: userMsg },
              defaultModelRef.current,
            );
            return next === cur ? prev : { ...prev, [taskId]: next };
          });
        };

        const initialEpoch = ++chatWatchdogEpochRef.current;
        chatWatchdogsRef.current[sid] = {
          id: setTimeout(() => {
            fire().catch(() => {});
          }, loadChatCheckInMs()),
          epoch: initialEpoch,
        };
      } else if (e.event_type === "done" || e.event_type === "error") {
        const existing = chatWatchdogsRef.current[e.session_id];
        if (existing) {
          clearTimeout(existing.id);
          delete chatWatchdogsRef.current[e.session_id];
        }
      }

      // Usage events were already routed at the top of the listener.
      const ev = mapChatEventToTaskEvent(e);
      if (!ev) return;
      setTasks((prev) => {
        const cur = prev[taskId];
        if (!cur) return prev;
        const next = applyTaskEvent(cur, ev, defaultModelRef.current);
        return next === cur ? prev : { ...prev, [taskId]: next };
      });
      // Post-done reconciliation for non-review-internal sessions: the
      // streamed chat-event chunks may have been dropped under broadcast-
      // channel lag (Pi side) or webview IPC contention. Pi durably writes
      // every assistant message to its session JSONL on disk, so we can
      // reconcile the last asst message's text against that authoritative
      // source. Only fires on `done` and only updates if the JSONL text
      // is materially longer than what landed in the message.
      if (e.event_type === "done") {
        const doneSid = e.session_id;
        ipc
          .getSessionLastAssistantText(doneSid)
          .then((authoritative) => {
            if (!authoritative) return;
            setTasks((prev) => {
              const cur = prev[taskId];
              if (!cur) return prev;
              // If the streaming pass already lifted a structured block
              // (plan or questions) into its own message, skip recovery.
              // The authoritative text from disk still contains the
              // delimited block; restoring it into the asst would cause
              // processDoneEvent to re-insert a duplicate plan/questions
              // card.
              const turnHasStructuredInsert = (() => {
                for (let i = cur.messages.length - 1; i >= 0; i--) {
                  const m = cur.messages[i];
                  if (m.who === "user") return false;
                  if (m.who === "plan") return true;
                  if (m.who === "questions" && m.questions) return true;
                }
                return false;
              })();
              if (turnHasStructuredInsert) return prev;
              // Walk backward and decide where the authoritative text for
              // *this* turn should live. We stop at the first user message
              // (turn boundary) — any asst messages after it belong to
              // this turn. Three outcomes:
              //   1. Found a "live" asst (last asst with no tools, no
              //      reviewKind): replace its text if the JSONL has
              //      materially more content.
              //   2. Found an asst but it has tools: this turn produced
              //      tool calls AND a text block; the chunks for the
              //      text block were dropped before a new asst was
              //      created. Append a fresh asst with the JSONL text.
              //   3. No asst messages after the boundary: same as case 2.
              let liveIdx = -1;
              let toolBoundaryFound = false;
              for (let i = cur.messages.length - 1; i >= 0; i--) {
                const m = cur.messages[i];
                if (m.who === "user") break;
                if (m.who !== "asst") continue;
                if (m.reviewKind) continue;
                // Skip plan/questions inserts emitted by the reducer.
                if ((m as { who: string }).who !== "asst") continue;
                if (m.tools && m.tools.length > 0) {
                  toolBoundaryFound = true;
                  break;
                }
                liveIdx = i;
                break;
              }
              const model = cur.model || defaultModelRef.current;
              if (liveIdx !== -1) {
                const m = cur.messages[liveIdx];
                const haveLen = (m.text || "").length;
                // Only patch if the JSONL has materially more text than
                // the streamed message. A small slack handles benign
                // trailing-whitespace differences.
                if (authoritative.length <= haveLen + 16) return prev;
                const messages = [...cur.messages];
                messages[liveIdx] = { ...m, text: authoritative };
                const recovered = applyTaskEvent(
                  { ...cur, messages },
                  { kind: "done" },
                  defaultModelRef.current,
                );
                return { ...prev, [taskId]: recovered };
              }
              if (toolBoundaryFound || cur.messages.length === 0) {
                // Append a fresh asst message — its chunks were dropped.
                const messages = [
                  ...cur.messages,
                  { who: "asst" as const, text: authoritative, model, createdAt: Date.now() },
                ];
                const recovered = applyTaskEvent(
                  { ...cur, messages },
                  { kind: "done" },
                  defaultModelRef.current,
                );
                return { ...prev, [taskId]: recovered };
              }
              return prev;
            });
          })
          .catch(() => {
            // Best-effort — if the JSONL doesn't exist or read fails,
            // leave the in-memory state alone. Already-handled by the
            // reducer's done path.
          });
      }
    }).then((fn) => {
      if (mounted) unlisten = fn;
      else safeUnlisten(fn);
    });

    return () => {
      mounted = false;
      safeUnlisten(unlisten);
    };
  }, [resolveTaskIdForSession]);

  /* ── Listener: nurse-event ─────────────────────────────── */
  /* Routes Nurse `Lifecycle` events into the matching task's reducer so an
   * inline `who: "nurse"` card streams in alongside the assistant message
   * flow. Routing keys (in order of preference): `task_id` → direct task
   * lookup; `session_id` → resolved via `sessionIdToTaskIdRef`. Events that
   * match no task are dropped silently (e.g. nurse interventions on
   * unrelated swarm sessions). */
  useEffect(() => {
    if (!isTauri()) return;
    let unlisten: UnlistenFn | undefined;
    let mounted = true;

    onNurseEvent((e) => {
      if (!mounted) return;
      if (e.event_type !== "Lifecycle") return;
      // Discriminator-narrowing for the Lifecycle variant exposes task_id /
      // session_id directly on `e` per the type definition.
      const taskIdHint = e.task_id ?? undefined;
      const taskId =
        (taskIdHint && tasksRef.current[taskIdHint] ? taskIdHint : undefined) ??
        sessionIdToTaskIdRef.current[e.session_id];
      if (!taskId) return;

      const ev = mapNurseEventToTaskEvent(e);
      if (!ev) return;
      setTasks((prev) => {
        const cur = prev[taskId];
        if (!cur) return prev;
        const next = applyTaskEvent(cur, ev, defaultModelRef.current);
        return next === cur ? prev : { ...prev, [taskId]: next };
      });
    }).then((fn) => {
      if (mounted) unlisten = fn;
      else safeUnlisten(fn);
    });

    return () => {
      mounted = false;
      safeUnlisten(unlisten);
    };
  }, []);

  /* ── Listener: hivemind-progress ───────────────────────── */
  // Subscribes through the singleton `hivemindEventStore` so this
  // provider shares a single underlying Tauri `listen("hivemind-progress")`
  // call with all other consumers (Tasks panel, SwarmControl,
  // ReviewHistory, etc.).
  useEffect(() => {
    if (!isTauri()) return;
    let mounted = true;

    const unsubscribe = subscribeHivemindEventListener((e) => {
      if (!mounted) return;
      let ownerTaskId: string | null = null;
      for (const [tid, t] of Object.entries(tasksRef.current)) {
        if (t.activeReviewJobId === e.job_id) {
          ownerTaskId = tid;
          break;
        }
      }
      if (!ownerTaskId) {
        for (const [tid, f] of Object.entries(reviewFlowsRef.current)) {
          if (f?.currentJobId === e.job_id) {
            ownerTaskId = tid;
            break;
          }
        }
      }

      // ── Crash-recovery startup event ──
      // The backend emits `merge_interrupted` on `hivemind-progress` once
      // per orphan merge_run discovered at boot. Forward it to the owning
      // task (if any) — the flag drives the Tasks-view "Resume merge" UI.
      if (e.event_type === "merge_interrupted") {
        if (ownerTaskId) {
          setTasks((prev) => {
            const cur = prev[ownerTaskId!];
            if (!cur) return prev;
            const next = applyTaskEvent(
              cur,
              {
                kind: "merge_interrupted",
                jobId: e.job_id,
                round: e.round,
                outputLen: e.output_len ?? 0,
                message: e.message || "Merge interrupted by host restart",
              },
              defaultModelRef.current,
            );
            return next === cur ? prev : { ...prev, [ownerTaskId!]: next };
          });
        }
        return;
      }

      // ── Generalised crash-recovery startup event ──
      // Backend emits `review_interrupted` for any non-completed review tied
      // to a task at boot (phase = context | round | merge | between_rounds |
      // final). Resolve the owning task via the event's `task_id`, fetch the
      // full snapshot, then dispatch `review_interrupted` to the reducer.
      if (e.event_type === "review_interrupted") {
        const targetTaskId = e.task_id ?? ownerTaskId;
        if (!targetTaskId) return;
        ipc
          .getResumableReviewForTask(targetTaskId)
          .then((snapshot) => {
            if (!snapshot) return;
            setTasks((prev) => {
              const cur = prev[targetTaskId];
              if (!cur) return prev;
              const next = applyTaskEvent(
                cur,
                { kind: "review_interrupted", snapshot },
                defaultModelRef.current,
              );
              return next === cur ? prev : { ...prev, [targetTaskId]: next };
            });
          })
          .catch((err) => {
            console.warn("[review] getResumableReviewForTask failed", err);
          });
        return;
      }
      if (!ownerTaskId) {
        // Race-fix: between `await ipc.startReview()` resolving and the
        // `flow.currentJobId` / `activeReviewJobId` writes committing, a fast
        // backend (cache hit, very fast model) can emit "completed" before
        // either lookup matches. For terminal events, kick reconciliation on
        // every active task — the right one will match by jobId via
        // getReviewState inside reconcileReview.
        if (
          e.event_type === "completed" ||
          e.event_type === "failed" ||
          e.event_type === "cancelled" ||
          e.event_type === "error"
        ) {
          for (const t of Object.values(tasksRef.current)) {
            if (t.activeReviewJobId && !t.reviewCompleted) {
              reconcileReviewRef.current?.(t.taskId);
            }
          }
        }
        return;
      }

      const ev = mapHivemindEventToTaskEvent(e);
      if (ev) {
        setTasks((prev) => {
          const cur = prev[ownerTaskId!];
          if (!cur) return prev;
          const next = applyTaskEvent(cur, ev, defaultModelRef.current);
          return next === cur ? prev : { ...prev, [ownerTaskId!]: next };
        });
      }

      // ── Structured inline merge / context chunk routing ──
      // The backend engine emits per-Pi-event `merge_text` / `merge_thinking`
      // / `merge_tool_*` / `merge_started` / `merge_completed` events on
      // `hivemind-progress` (alongside the coalesced `merge_chunk` retained
      // for the dock preview + capture file accumulator). Routing them
      // through the same `internal_pi_*` reducer events the context phase
      // uses surfaces reasoning, streamed text, and tool calls in the inline
      // Tasks chat bubble (decorated by `MergeScoringPill` in
      // ActivityStream). `context_*` is wired symmetrically so a future
      // backend-driven Tasks context path (or audit consumer) gets the same
      // treatment for free.
      const structuredSid = e.session_id;
      if (structuredSid) {
        const reviewIdHint =
          (typeof e.review_id === "string" && e.review_id.length > 0
            ? e.review_id
            : null) ||
          reviewFlowsRef.current[ownerTaskId]?.reviewId ||
          "";

        const dispatchInternal = (ev2: Parameters<typeof applyTaskEvent>[1]) => {
          setTasks((prev) => {
            const cur = prev[ownerTaskId!];
            if (!cur) return prev;
            const next = applyTaskEvent(cur, ev2, defaultModelRef.current);
            return next === cur ? prev : { ...prev, [ownerTaskId!]: next };
          });
        };

        const registerSession = (phase: "context" | "merge", round?: number) => {
          internalSessionIdsRef.current.add(structuredSid);
          sessionIdToReviewIdRef.current[structuredSid] = reviewIdHint;
          sessionIdToTaskIdRef.current[structuredSid] = ownerTaskId!;
          dispatchInternal({
            kind: "internal_pi_message_start",
            sessionId: structuredSid,
            reviewKind: { phase, round, reviewId: reviewIdHint },
            modelName: e.model_id || "",
          });
          dispatchInternal({
            kind: "internal_pi_started",
            sessionId: structuredSid,
            modelName: e.model_id || "",
            piKind: phase,
          });
        };

        const unregisterSession = () => {
          internalSessionIdsRef.current.delete(structuredSid);
          delete sessionIdToReviewIdRef.current[structuredSid];
          delete sessionIdToTaskIdRef.current[structuredSid];
        };

        switch (e.event_type) {
          case "merge_started":
            registerSession("merge", e.round);
            break;
          case "context_started":
            registerSession("context");
            break;
          case "merge_text":
          case "context_text":
            if (e.delta) {
              dispatchInternal({
                kind: "internal_pi_chunk",
                sessionId: structuredSid,
                content: e.delta,
              });
            }
            break;
          case "merge_thinking":
          case "context_thinking":
            if (e.delta) {
              dispatchInternal({
                kind: "internal_pi_thinking",
                sessionId: structuredSid,
                content: e.delta,
              });
            }
            break;
          case "merge_tool_start":
          case "context_tool_start":
            if (e.tool_call_id && e.tool_name) {
              dispatchInternal({
                kind: "internal_pi_tool_start",
                sessionId: structuredSid,
                data: { tool_call_id: e.tool_call_id, name: e.tool_name },
              });
            }
            break;
          case "merge_tool_update":
          case "context_tool_update":
            if (e.tool_call_id) {
              dispatchInternal({
                kind: "internal_pi_tool_update",
                sessionId: structuredSid,
                data: {
                  tool_call_id: e.tool_call_id,
                  output: e.tool_output ?? "",
                },
              });
            }
            break;
          case "merge_tool_end":
          case "context_tool_end":
            if (e.tool_call_id) {
              dispatchInternal({
                kind: "internal_pi_tool_end",
                sessionId: structuredSid,
                data: {
                  tool_call_id: e.tool_call_id,
                  result: e.tool_result,
                },
              });
            }
            break;
          case "merge_completed":
          case "context_completed":
            dispatchInternal({
              kind: "internal_pi_done",
              sessionId: structuredSid,
            });
            unregisterSession();
            break;
          case "merge_failed":
          case "cancelled":
          case "failed":
            if (internalSessionIdsRef.current.has(structuredSid)) {
              dispatchInternal({
                kind: "internal_pi_failed",
                sessionId: structuredSid,
                message: e.message || `Review ${e.event_type}`,
              });
              unregisterSession();
            }
            break;
          default:
            break;
        }
      }

      // ── Verdicts surfacing on the merge bubble ──
      // The backend emits `verdicts_updated` on `hivemind-progress` after
      // `merge_completed` with the round number (no session_id). Fetch the
      // saved verdicts for the job and dispatch a `structured_verdicts`
      // reducer event keyed by round so MergeScoringPill on the matching
      // merge bubble shows real counts instead of "0 decisions".
      if (e.event_type === "verdicts_updated") {
        const verdictsTaskId = ownerTaskId;
        const verdictsRound = e.round;
        ipc
          .listRoundVerdicts(e.job_id)
          .then((rows) => {
            const forRound = rows.filter((v) => v.round_number === verdictsRound);
            if (forRound.length === 0) return;
            const parsed: ParsedVerdict[] = forRound.map((v) => ({
              reviewer_model: v.reviewer_model,
              suggestion: v.suggestion,
              verdict: v.verdict,
              severity: v.severity,
              reason: v.reason,
              best_find: v.best_find,
              co_reviewers: v.co_reviewers,
            }));
            setTasks((prev) => {
              const cur = prev[verdictsTaskId];
              if (!cur) return prev;
              const next = applyTaskEvent(
                cur,
                {
                  kind: "structured_verdicts",
                  verdicts: parsed,
                  sessionId: null,
                  round: verdictsRound,
                },
                defaultModelRef.current,
              );
              return next === cur ? prev : { ...prev, [verdictsTaskId]: next };
            });
          })
          .catch((err) => {
            console.warn("[review] listRoundVerdicts after verdicts_updated failed", err);
          });
      }

      if (e.event_type === "completed") {
        // Backend has finished all rounds for this job, run the merge, and
        // persisted `final_output` via complete_job BEFORE emitting this
        // event (see engine.rs run loop). Multi-round in the Tasks view is
        // FE-driven: each round is its own `start_review` job with
        // numRounds=1, so we either advance to the next round here or
        // finalize. The dead chat-event "done" branch (~L2269) was the
        // legacy handoff path — its `flow.phase === "merge"` precondition
        // is never set, so wiring the handoff here is the working path.
        const finalizingTaskId = ownerTaskId;
        const flow = reviewFlowsRef.current[finalizingTaskId];
        const fallbackPlan =
          (flow?.currentPlan && flow.currentPlan.trim() ? flow.currentPlan : null) ??
          tasksRef.current[finalizingTaskId]?.planText ??
          "";
        ipc
          .getReviewState(e.job_id)
          .then((snapshot) => {
            const fromBackend =
              snapshot.final_output && snapshot.final_output.trim()
                ? snapshot.final_output
                : null;
            const finalPlan = fromBackend ?? fallbackPlan;
            if (!finalPlan.trim()) {
              console.warn(
                "[review] completed with no plan content — final_output, flow.currentPlan and task.planText are all empty",
              );
            }

            // Multi-round advancement: if the live flow has more rounds
            // queued, swap in the merged plan and dispatch the next round
            // via startNextReviewRound. Verdict persistence is owned by
            // the backend (engine.rs::save_round_verdicts inside the merge
            // phase) — do NOT also write from the FE here.
            const liveFlow = reviewFlowsRef.current[finalizingTaskId];
            const decision = liveFlow
              ? decideRoundCompletion(liveFlow)
              : ({ kind: "finish" } as const);

            if (liveFlow && decision.kind === "advance") {
              const fromRound = liveFlow.currentRound + 1; // 1-based for logs
              // Features are no longer inlined into the plan body — the
              // merge orchestrator pulls them off the prior round's
              // `submit_features` tool args server-side. The next round
              // just receives the cleaned plan markdown as-is.
              const nextPlan = finalPlan;
              liveFlow.currentPlan = nextPlan;
              liveFlow.currentRound = decision.nextRound;
              liveFlow.phase = "round";
              // Clear job-tracking BEFORE scheduling the next round so
              // startNextReviewRound's in-flight guard (isStartingRound /
              // currentJobId) doesn't abort, and so the next
              // `review_start` reducer event applies cleanly against a
              // task that no longer thinks the prior job is active.
              liveFlow.currentJobId = null;
              liveFlow.isStartingRound = false;
              updateTask(finalizingTaskId, (t) => ({
                ...t,
                activeReviewJobId: null,
              }));
              ipc
                .logReviewEvent(liveFlow.reviewId, "round_advanced", {
                  from_round: fromRound,
                  to_round: decision.nextRound + 1, // 1-based for logs
                  plan_len: nextPlan.length,
                })
                .catch(() => {});
              queueMicrotask(() =>
                startNextRoundRef.current?.(finalizingTaskId),
              );
            } else {
              queueMicrotask(() =>
                finishReviewRef.current?.(finalizingTaskId, finalPlan),
              );
            }
          })
          .catch((err) => {
            console.warn("[review] getReviewState after completed failed", err);
            queueMicrotask(() => finishReviewRef.current?.(finalizingTaskId, fallbackPlan));
          });
      } else if (e.event_type === "failed") {
        if (reviewFlowsRef.current[ownerTaskId]?.reviewId) {
          ipc
            .logReviewEvent(reviewFlowsRef.current[ownerTaskId]!.reviewId, "review_failed", {
              error: e.message,
            })
            .catch(() => {});
        }
        const failedFlow = reviewFlowsRef.current[ownerTaskId];
        if (failedFlow?.contextWatchdog) {
          clearTimeout(failedFlow.contextWatchdog);
          failedFlow.contextWatchdog = null;
        }
        reviewFlowsRef.current[ownerTaskId] = null;
      }
    });

    return () => {
      mounted = false;
      unsubscribe();
    };
  }, []);

  /* ── createTask ────────────────────────────────────────── */
  const createTask = useCallback(
    (opts: CreateTaskOpts): string => {
      const id = `task-${nextIdRef.current++}`;
      // Model priority: explicit override > Settings default > active task's model > empty
      const activeTaskModel = tasksRef.current[activeIdRef.current]?.model;
      const model = opts.model || defaultModelRef.current || activeTaskModel || "";
      console.info(
        `[task-runtime] createTask: model="${model}" (opts="${opts.model}", default="${defaultModelRef.current}", activeTask="${activeTaskModel}")`
      );
      const projectPath =
        opts.projectPath ?? defaultProjectPathRef.current ?? project?.cwd ?? "";
      const hivemind = opts.hivemind !== undefined ? opts.hivemind : (defaultHivemindRef.current || null);
      const thinking = opts.thinking || "high";
      const promptTrimmed = opts.prompt?.trim() || "";
      const setActive = opts.setActive ?? false;
      const autoMode: AutoMode =
        opts.autoMode === undefined ? loadAutoModeDefault() : normalizeAutoMode(opts.autoMode);
      const hasImages = !!(opts.images && opts.images.length);

      const titleSlice =
        opts.title
        || (promptTrimmed
          ? promptTrimmed.slice(0, 80)
          : hasImages
            ? "(Image)"
            : "New Task");
      const previewSlice = opts.description
        ? opts.description.slice(0, 120)
        : promptTrimmed
          ? promptTrimmed.slice(0, 120)
          : hasImages
            ? "(Image attachment)"
            : "";

      setLocalTasks((prev) => [
        {
          id,
          group: "Active",
          title: titleSlice,
          project: projectPath ? workspaceLabel(projectPath) : "",
          model,
          phase: "intake",
          when: "now",
          preview: previewSlice,
          active: setActive,
          hivemind,
          projectPath,
          createdAt: Date.now(),
          // When provided, mark the user-supplied title as already-edited so
          // the planning agent's TASK_META doesn't overwrite the swarm name.
          titleEdited: opts.title ? true : undefined,
          swarmId: opts.swarmId,
        },
        ...prev.map((t) => (setActive ? { ...t, active: false } : t)),
      ]);

      const seed = makeInitialTaskState(id, model);
      setTasks((prev) => ({
        ...prev,
        [id]: {
          ...seed,
          autoMode,
          hivemind,
          thinking,
          projectPath: projectPath || null,
          swarmId: opts.swarmId ?? null,
          messages: (promptTrimmed || hasImages)
            ? []
            : [{ who: "asst", text: "What would you like to build?", model, createdAt: Date.now() }],
        },
      }));

      if (setActive) setActiveId(id);

      if (promptTrimmed || hasImages) {
        // Defer to a microtask so the new task is in tasksRef before submit.
        // We also pass overrides as a fallback in case the React render hasn't
        // committed yet by the time the microtask runs.
        Promise.resolve().then(() => {
          submitMessageRef.current?.(id, promptTrimmed, {
            model,
            thinking,
            projectPath: projectPath || null,
            images: opts.images,
          });
        });
      }

      return id;
    },
    [project],
  );

  /* ── submitMessage ─────────────────────────────────────── */
  const submitMessageImpl = useCallback(
    async (taskId: string, prompt: string, overrides?: SubmitOverrides) => {
      const text = prompt.trim();
      if (!text && (!overrides?.images || overrides.images.length === 0)) return;

      const cur0 = tasksRef.current[taskId];
      // A message is "steered" when the agent is currently mid-stream (the
      // classic interruption-during-active-turn case) OR when the user just
      // clicked Stop and this is the first message since (the post-stop
      // redirect case — `pendingSteerAfterStop` is set by `stopTask` and
      // cleared in the `setTasks` reducer below).
      let isSteering =
        (cur0?.streaming ?? false) || (cur0?.pendingSteerAfterStop ?? false);

      // ── Context-steer detection (must happen before isSteering override) ──
      const flowSnapshot = reviewFlowsRef.current[taskId];
      const { isContextSteer, contextSid: steeredSid } = detectContextSteer(flowSnapshot);

      // ── Override isSteering for context-steer ──
      // Context-steer messages are always steers (injected into the live Pi),
      // regardless of whether the context Pi is actively streaming at this
      // instant.
      if (isContextSteer) {
        isSteering = true;
      }

      const resolvedModel =
        cur0?.model || overrides?.model || defaultModelRef.current;
      const resolvedThinking = cur0?.thinking || overrides?.thinking || "high";
      const resolvedProject =
        cur0?.projectPath ?? overrides?.projectPath ?? project?.cwd ?? null;
      const userMsg: TaskMessage = {
        who: "user",
        text,
        images: overrides?.images,
        t: "now",
        createdAt: Date.now(),
        steered: isSteering || undefined,
      };

      // Slice-based placeholder title/preview — used until the planning
      // agent emits TASK_META (which then overwrites both via the
      // taskMetaKey effect above). The `t.title === "New Task"` guard means
      // this branch is skipped if the user renamed the task before sending
      // the first message; in that case `titleEdited` is already true on the
      // TaskListItem and the agent's TASK_META will leave the title alone.
      setLocalTasks((prev) =>
        prev.map((t) =>
          t.id === taskId && t.title === "New Task"
            ? {
                ...t,
                title: text.slice(0, 80),
                preview: text.slice(0, 120),
                phase: "intake",
                model: resolvedModel,
              }
            : t.id === taskId
            ? { ...t, model: t.model || resolvedModel }
            : t,
        ),
      );

      if (!isTauri()) return;

      // ── Stale-flow guard (must run before any state mutation) ──
      // Only applies to context-steer sends. Between the user pressing
      // Enter and this function executing, the context phase may have
      // completed (contextSid nullified around line ~2060). Refuse to
      // send when the context gatherer is no longer available. Also
      // cross-check the SID identity to defend against a re-entrant
      // context phase that could have started with a different SID.
      if (isContextSteer) {
        const flowAtSend = reviewFlowsRef.current[taskId];
        const phaseAtSend = flowAtSend?.phase ?? null;
        const sidNullified = flowAtSend != null && flowAtSend.contextSid == null;
        const sidChanged = flowAtSend?.contextSid !== steeredSid;
        if (phaseAtSend !== "context" || sidNullified || sidChanged) {
          console.warn(
            "[review] Context gathering no longer active — steer message dropped.",
            { phaseAtSend, steeredSid, currentSid: flowAtSend?.contextSid },
          );
          return;
        }
      }

      let sendError: unknown = undefined;
      try {
        const sid = cur0?.sessionId || crypto.randomUUID();
        // The task's sessionId is the PLANNING session. For context steers,
        // the recipient is a different Pi session (the context gatherer).
        // Derive a separate routing SID so we never overwrite
        // cur0.sessionId with the context SID (which would corrupt session
        // tracking for subsequent sends).
        const routingSid = isContextSteer && steeredSid ? steeredSid : sid;
        const isFirstMessage = !cur0?.sessionId && !isContextSteer;
        // Map the task's primary session ID. For context steers, the
        // context SID was already mapped during review initialization
        // (~line 4124). Do NOT remap here — we must not overwrite the
        // task's sessionId-to-taskId entry.
        if (!isContextSteer) {
          sessionIdToTaskIdRef.current[sid] = taskId;
        }
        // Phase decides which system prompt + tool set we ship with the
        // message. Critically, the backend only ignores these for an *alive*
        // Pi session — on any respawn (eviction graveyard, dead-process
        // path, fresh session) the options are reapplied. Sending the wrong
        // pair (or nothing) on a follow-up after the implementation finished
        // makes Pi launch without an explicit --system-prompt, which the
        // Anthropic Claude-subscription gate rejects as "out of extra usage".
        const phaseForPrompt = cur0?.phase ?? "intake";
        const isImplPhase =
          phaseForPrompt === "implement" || phaseForPrompt === "implement-done";
        let systemPromptForSend = isImplPhase
          ? IMPL_SYSTEM_PROMPT
          : cur0?.swarmId
            ? QUEEN_PLANNING_SYSTEM_PROMPT
            : PLAN_SYSTEM_PROMPT;
        let toolSetForSend = isImplPhase ? IMPL_TOOL_SET : PLAN_TOOL_SET;

        // ── Context-steer overrides ──
        // Force the context-gather system prompt so that if the context Pi
        // crashes and respawns, it continues gathering instead of switching
        // to planning mode. The plan tool set is correct for read-only
        // context: context does not need write tools.
        if (isContextSteer) {
          systemPromptForSend = REVIEW_CONTEXT_SYSTEM_PROMPT;
          toolSetForSend = PLAN_TOOL_SET;
        }

        let modelForSend = resolvedModel;
        let projectForSend: string | null | undefined = resolvedProject;
        let thinkingForSend = resolvedThinking;

        setTasks((prev) => {
          const base =
            prev[taskId] ??
            ({
              ...makeInitialTaskState(taskId, resolvedModel),
              thinking: resolvedThinking,
              projectPath: resolvedProject,
            } as TaskRuntimeState);
          // Use the freshest values from prev[taskId] if available, otherwise
          // fall back to the overrides we resolved synchronously.
          modelForSend = base.model || resolvedModel;
          projectForSend = base.projectPath ?? resolvedProject;
          thinkingForSend = base.thinking || resolvedThinking;
          // For context steers, sid is the task's planning session ID (not
          // the context SID), so this comparison would spuriously flag a
          // new session and clear stats. Guard against it.
          const isNewSession = !isContextSteer && sid !== base.sessionId;
          const baseForNext = isNewSession ? resetSessionStats(base) : base;
          const next: TaskRuntimeState = {
            ...baseForNext,
            // Preserve the existing sessionId for context-steer so we do
            // not overwrite the task's planning session ID.
            sessionId: isContextSteer ? baseForNext.sessionId : sid,
            streaming: !isSteering ? true : baseForNext.streaming,
            pendingSteerAfterStop: false,
            error: null,
            messages: isFirstMessage
              ? [
                  ...baseForNext.messages,
                  userMsg,
                  {
                    who: "session-divider",
                    dividerLabel: base.swarmId
                      ? "Swarm planning session started"
                      : "Planning session started",
                    dividerModel: modelForSend,
                    dividerThinking: thinkingForSend,
                    dividerSessionId: sid,
                    createdAt: Date.now(),
                  },
                ]
              : [...baseForNext.messages, userMsg],
          };
          return { ...prev, [taskId]: next };
        });

        console.info(
          `[plan-mode] sendMessage: routingSid=${routingSid.slice(
            0,
            8,
          )}, taskSid=${sid.slice(0, 8)}, isFirst=${isFirstMessage}, phase=${phaseForPrompt}, toolSet=${toolSetForSend}, model=${modelForSend}, contextSteer=${isContextSteer}`,
        );

        const imagePayloads = overrides?.images?.map((img) => ({
          media_type: img.mediaType,
          data: img.data,
        }));

        await ipc.sendMessage(
          text,
          modelForSend,
          routingSid,
          projectForSend ?? undefined,
          thinkingForSend,
          systemPromptForSend,
          toolSetForSend,
          imagePayloads,
          isSteering,
        );
      } catch (e) {
        sendError = e;
        console.error("Failed to send task message:", e);
        updateTask(taskId, (t) => ({
          ...t,
          error: String(e),
          streaming: isSteering ? t.streaming : false,
        }));
      } finally {
        if (isContextSteer && flowSnapshot?.reviewId) {
          // Hoisted sendError avoids the scoping bug where catch 'e' is
          // inaccessible in the finally block.
          ipc
            .logReviewEvent(flowSnapshot.reviewId, "context_steered", {
              message_length: text.length,
              session_id: steeredSid,
              success: sendError === undefined,
            })
            .catch(() => {});
        }
      }
    },
    [project, updateTask],
  );
  const submitMessageRef = useRef(submitMessageImpl);
  submitMessageRef.current = submitMessageImpl;

  /* ── stopTask ──────────────────────────────────────────── */
  const stopTask = useCallback(
    async (taskId: string) => {
      const sid = tasksRef.current[taskId]?.sessionId;
      if (!sid) return;
      try {
        await ipc.stopChat(sid);
        // Prevent the streaming-transition useEffect from re-arming the tail.
        // The effect compares lastStreamingRef (prev) vs baseStreamingTaskIds
        // (next). Without this line, the effect would see prev[taskId]=true
        // and next[taskId]=false (a "stopped streaming" transition) and
        // immediately re-add the 15s tail, defeating the explicit stop.
        // Set BEFORE updateTask to ensure the ref is updated before any
        // re-render triggered by the state change could run the effect.
        lastStreamingRef.current[taskId] = false;
        updateTask(taskId, (t) => ({
          ...t,
          streaming: false,
          // The next message the user sends after Stop is a redirect — the
          // submit handler reads this flag to apply the "steered" badge and
          // to tell the backend to prepend an interruption preamble. Cleared
          // in submitMessageImpl's setTasks reducer when consumed.
          pendingSteerAfterStop: true,
          queueState: null,
          liveTps: null,
        }));
        // Immediately clear the 15s streaming tail for this task so the
        // sidebar updates instantly on user-initiated stop (the tail is
        // designed for smooth natural-completion transitions, not for
        // explicit stops where the user expects immediate visual feedback).
        if (tailTimersRef.current[taskId]) {
          clearTimeout(tailTimersRef.current[taskId]);
          delete tailTimersRef.current[taskId];
        }
        setTailStreamingIds((s) => {
          if (!s[taskId]) return s;
          const out = { ...s };
          delete out[taskId];
          return out;
        });
      } catch (e) {
        console.error("Failed to stop task:", e);
        updateTask(taskId, (t) => ({ ...t, error: String(e) }));
      }
    },
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [updateTask],
  );

  /* ── deleteTask ────────────────────────────────────────── */
  const deleteTask = useCallback(
    (id: string) => {
      // Collect BEFORE we tear down the in-memory state (otherwise the refs
      // are already empty when we read them).
      const sidsToKill = collectTaskSessionIds(id);

      setLocalTasks((prev) => {
        const remaining = prev.filter((t) => t.id !== id);
        if (id === activeIdRef.current) {
          if (remaining.length > 0) {
            setActiveId(remaining[0].id);
          } else {
            const newId = `task-${nextIdRef.current++}`;
            const newModel = defaultModelRef.current;
            const newProjectPath =
              defaultProjectPathRef.current || project?.cwd || "";
            const seed = makeInitialTaskState(newId, newModel);
            setTasks((p) => ({
              ...p,
              [newId]: {
                ...seed,
                autoMode: loadAutoModeDefault(),
                projectPath: newProjectPath || null,
                messages: [
                  { who: "asst", text: "What would you like to build?", model: newModel, createdAt: Date.now() },
                ],
              },
            }));
            setActiveId(newId);
            return [
              {
                id: newId,
                group: "Active",
                title: "New Task",
                project: newProjectPath ? workspaceLabel(newProjectPath) : "",
                model: newModel,
                phase: "intake",
                when: "now",
                preview: "",
                active: true,
                projectPath: newProjectPath,
                createdAt: Date.now(),
              },
            ];
          }
        }
        return remaining;
      });
      // Revoke object URLs for images before removing task
      const taskToDelete = tasksRef.current[id];
      if (taskToDelete) {
        for (const msg of taskToDelete.messages) {
          if (msg.images) {
            for (const img of msg.images) {
              URL.revokeObjectURL(img.previewUrl);
            }
          }
        }
      }

      setTasks((prev) => {
        if (!prev[id]) return prev;
        const next = { ...prev };
        delete next[id];
        return next;
      });
      const sessions = loadTaskSessions();
      delete sessions[id];
      saveTaskSessions(sessions);
      if (draftsRef.current[id] !== undefined) {
        delete draftsRef.current[id];
        scheduleDraftsSave();
      }
      deleteTaskMessagesFromDisk(id);
      // Kill every Pi session bound to this task and delete its transcript
      // on disk. `deleteChatSession` handles both (kill in-mem + rm .jsonl).
      // Falls back to `killPiSession` for ids that aren't valid chat-session
      // UUIDs (e.g. context/merge ids — though those are also UUIDs in
      // practice).
      if (isTauri()) {
        for (const sid of sidsToKill) {
          ipc.deleteChatSession(sid).catch((e) => {
            console.warn(
              "[delete-task] deleteChatSession failed, falling back to killPiSession",
              sid,
              e,
            );
            ipc.killPiSession(sid).catch(() => {});
          });
        }
      }
      // Mark this task as already-terminated so the implement-done effect
      // (if the task was already in implement-done at delete time and somehow
      // retriggers via a transient render) is a no-op.
      terminatedTasksRef.current.add(id);
      // Pi sessions bound to this task are killed (and their transcripts
      // deleted) by the deleteChatSession loop above. The block below only
      // scrubs the in-renderer reverse maps so the reconciler doesn't think
      // orphan sessions are still in use.
      for (const [sid, mappedTaskId] of Object.entries(
        sessionIdToTaskIdRef.current,
      )) {
        if (mappedTaskId === id) {
          delete sessionIdToTaskIdRef.current[sid];
        }
      }
      const drainFlow = reviewFlowsRef.current[id];
      if (drainFlow) {
        if (drainFlow.contextWatchdog) {
          clearTimeout(drainFlow.contextWatchdog);
          drainFlow.contextWatchdog = null;
        }
        if (drainFlow.contextSid) {
          internalSessionIdsRef.current.delete(drainFlow.contextSid);
          delete sessionIdToReviewIdRef.current[drainFlow.contextSid];
        }
      }
      delete reviewFlowsRef.current[id];
      delete reviewAccumulatorsRef.current[id];
      delete mergeLastChunkAtRef.current[id];
      delete contextLastEventAtRef.current[id];
      delete lastSavedMessagesRef.current[id];
      delete lastStreamingForSaveRef.current[id];
      if (saveTimersRef.current[id]) {
        clearTimeout(saveTimersRef.current[id]);
        delete saveTimersRef.current[id];
      }
    },
    [project, scheduleDraftsSave, collectTaskSessionIds],
  );

  /* ── Hivemind review flow ──────────────────────────────── */
  const startNextReviewRound = useCallback(
    async (taskId: string) => {
      const flow = reviewFlowsRef.current[taskId];
      if (!flow) return;

      // Guard against concurrent invocations for the same task.
      // If the flow already has a jobId for this round, another
      // startNextReviewRound is in flight — abort.
      if (flow.currentJobId != null) {
        console.warn("[review] startNextReviewRound: flow already has a jobId, aborting");
        return;
      }

      // Set an in-flight flag to prevent double invocation from
      // two event handlers firing in quick succession (e.g., last model
      // completes and merge finishes nearly simultaneously).
      if (flow.isStartingRound) return;
      flow.isStartingRound = true;

      updateTask(taskId, (t) => ({ ...t, streaming: false }));
      const curTask = tasksRef.current[taskId];
      const taskName = curTask?.taskMeta?.title || flow.currentPlan.slice(0, 200);
      const reviewProjectPath =
        curTask?.projectPath ?? defaultProjectPathRef.current ?? null;
      const reviewOptions = buildReviewRoundStartOptions(
        flow,
        taskName,
        taskId,
        reviewProjectPath,
      );

      try {
        const reviewerPlan = buildReviewerPlan(flow.currentPlan, flow.enrichedPrompt);
        const jobId = await ipc.startReview(reviewerPlan, reviewOptions);

        // Guard: task may have been deleted or re-initialized while we were awaiting.
        // Use the current ref entry rather than the captured `flow` to handle
        // legitimate flow replacement scenarios.
        const currentFlow = reviewFlowsRef.current[taskId];
        if (!currentFlow || !currentFlow.isStartingRound) {
          console.warn("[review] startNextReviewRound: flow changed during await, aborting");
          return;
        }

        currentFlow.currentJobId = jobId;
        currentFlow.isStartingRound = false;

        // Guard: component might have unmounted during await.
        // The mounted check prevents React state updates after unmount.
        if (mountedRef.current) {
          updateTask(taskId, (t) =>
            applyTaskEvent(
              t,
              {
                kind: "review_start",
                jobId,
                round: currentFlow.currentRound + 1,
                totalRounds: currentFlow.roundsConfig.length,
                models: reviewOptions.models,
                reviewId: currentFlow.reviewId,
              },
              defaultModelRef.current,
            ),
          );
        }
      } catch (e) {
        console.error("Review round failed:", e);
        // Clean up the in-flight flag
        const failedFlow = reviewFlowsRef.current[taskId];
        if (failedFlow) {
          failedFlow.isStartingRound = false;
        }
        ipc
          .logReviewEvent(flow.reviewId, "review_failed", {
            error: String(e),
            round: flow.currentRound + 1,
          })
          .catch(() => {});
        updateTask(taskId, (t) => ({
          ...t,
          error: `Review round failed: ${e}`,
          reviewProgress: null,
          activeReviewJobId: null,
          internalPi: null,
          phase: PHASE_RANK[t.phase] > PHASE_RANK["review"] ? t.phase : "plan-ready",
        }));
        reviewFlowsRef.current[taskId] = null;
      } finally {
        // Safety net: clear flag even if something unexpected happens
        const finalFlow = reviewFlowsRef.current[taskId];
        if (finalFlow) {
          finalFlow.isStartingRound = false;
        }
      }
    },
    [updateTask],
  );
  startNextRoundRef.current = startNextReviewRound;

  /** Arms the 3-min features-refresh watchdog and dispatches a follow-up
   *  prompt asking Queen to re-emit `submit_features`. This is the ONLY
   *  supported way to set `pendingFeaturesRefresh: true` in conjunction
   *  with a Pi dispatch — called from BOTH `finishReviewFlow` (post-
   *  Hivemind) and `handleRequestFeatures` (user retry). Any new writer
   *  MUST go through this helper so the watchdog and dispatch-failure
   *  semantics stay in sync (without it the old `finishReviewFlow`
   *  catch-less pattern reappears). The watchdog and dispatch catch both
   *  set `featuresRefreshFailed: true` so the recovery UI is consistent
   *  across reducer-path and helper-path failures. */
  const armFeaturesRefreshAndDispatch = useCallback(
    (taskId: string, followUp: string) => {
      const REFRESH_TIMEOUT_MS = 3 * 60 * 1000;
      const prevTimer = pendingFeaturesRefreshTimersRef.current[taskId];
      if (prevTimer) clearTimeout(prevTimer);
      pendingFeaturesRefreshTimersRef.current[taskId] = setTimeout(() => {
        delete pendingFeaturesRefreshTimersRef.current[taskId];
        updateTask(taskId, (t) =>
          t.pendingFeaturesRefresh
            ? {
                ...t,
                pendingFeaturesRefresh: false,
                featuresRefreshFailed: true,
                error:
                  "Queen didn't re-emit features within 3 minutes — launching will use the currently-stored features.",
              }
            : t,
        );
      }, REFRESH_TIMEOUT_MS);

      queueMicrotask(() => {
        submitMessageRef
          .current?.(taskId, followUp)
          .catch((e) => {
            const existing = pendingFeaturesRefreshTimersRef.current[taskId];
            if (existing) {
              clearTimeout(existing);
              delete pendingFeaturesRefreshTimersRef.current[taskId];
            }
            updateTask(taskId, (t) => ({
              ...t,
              pendingFeaturesRefresh: false,
              featuresRefreshFailed: true,
              error: `Failed to ask Queen to refine features: ${String(e)}`,
            }));
          });
      });
    },
    [updateTask],
  );

  const finishReviewFlow = useCallback(
    (taskId: string, finalPlan: string) => {
      const flow = reviewFlowsRef.current[taskId];
      if (flow) {
        ipc
          .logReviewEvent(flow.reviewId, "finish_review_flow", {
            current_round: flow.currentRound,
            rounds_total: flow.roundsConfig.length,
            phase_at_finish: flow.phase,
          })
          .catch(() => {});
        ipc
          .logReviewEvent(flow.reviewId, "review_completed", {
            total_rounds: flow.roundsConfig.length,
          })
          .catch(() => {});
        if (flow.contextWatchdog) {
          clearTimeout(flow.contextWatchdog);
          flow.contextWatchdog = null;
        }
        // Belt-and-suspenders: drain any lingering Pi sessions in case `done`
        // arrived but we never reached the in-listener queueMicrotask cleanup
        // (e.g., flow was nulled out by an error path mid-flight).
        if (flow.contextSid) {
          internalSessionIdsRef.current.delete(flow.contextSid);
          delete sessionIdToReviewIdRef.current[flow.contextSid];
          ipc.stopChat(flow.contextSid).catch(() => {});
        }
      }
      delete mergeLastChunkAtRef.current[taskId];
      delete contextLastEventAtRef.current[taskId];
      reviewFlowsRef.current[taskId] = null;

      // `finalPlan` is the merged plan markdown only. For ordinary Tasks-view
      // hivemind reviews, features are not in scope — the merged plan IS the
      // deliverable. For swarm-planning tasks (`t.swarmId` set) we want one
      // more step: hand the refined plan back to the Queen Planning agent and
      // ask it to re-emit `submit_features` so the Launch Swarm CTA ships
      // refined features (matching the refined plan) rather than the
      // pre-review ones.
      const taskSnapshot = tasksRef.current[taskId];
      const isSwarmPlanningTask = !!taskSnapshot?.swarmId;

      updateTask(taskId, (t) => {
        const totalModels =
          t.reviewProgress?.rounds.reduce((sum, r) => sum + r.models.length, 0) ?? 0;
        const numRounds = flow?.roundsConfig.length ?? t.reviewProgress?.totalRounds ?? 0;
        const summaryLabel =
          numRounds && totalModels
            ? `Hivemind review complete — ${numRounds} round${
                numRounds === 1 ? "" : "s"
              }, ${totalModels} model${totalModels === 1 ? "" : "s"}`
            : "Hivemind review complete";

        if (isSwarmPlanningTask) {
          // Swarm branch: keep `sessionId` populated so the follow-up
          // `submitMessage` resumes the existing Queen Planning Pi session
          // (or respawns it via the graveyard reconciler if the OS reclaimed
          // it). Mark `pendingFeaturesRefresh` so Tasks.tsx hides the
          // Launch Swarm CTA until the refreshed features land. Stats are
          // reset to zero so the topbar reflects the new turn cleanly.
          return resetSessionStats({
            ...t,
            reviewProgress: null,
            activeReviewJobId: null,
            internalPi: null,
            streaming: false,
            reviewCompleted: true,
            pendingFeaturesRefresh: true,
            messages: [
              ...t.messages,
              { who: "session-divider", dividerLabel: summaryLabel, createdAt: Date.now() },
              { who: "plan", planText: finalPlan, features: t.swarmFeatures ?? undefined, createdAt: Date.now() },
            ],
            phase: PHASE_RANK[t.phase] > PHASE_RANK["review"] ? t.phase : "plan-ready",
            planText: finalPlan,
          });
        }

        // Non-swarm branch: clear sessionId + stats. Both context and merge
        // Pi sessions are stopped at this point (see flow cleanup above);
        // leaving sessionId pointing at a dead session would let
        // submitMessageImpl reuse it and the backend would respawn Pi with
        // `with_resume()`, pulling in the merge transcript and inflating
        // the bar. The next user action (implement or follow-up) will mint
        // a fresh session.
        return resetSessionStats({
          ...t,
          sessionId: null,
          reviewProgress: null,
          activeReviewJobId: null,
          internalPi: null,
          streaming: false,
          reviewCompleted: true,
          messages: [
            ...t.messages,
            { who: "session-divider", dividerLabel: summaryLabel, createdAt: Date.now() },
            { who: "plan", planText: finalPlan, features: t.swarmFeatures ?? undefined, createdAt: Date.now() },
          ],
          phase: PHASE_RANK[t.phase] > PHASE_RANK["review"] ? t.phase : "plan-ready",
          planText: finalPlan,
        });
      });
      autoImplFiredRef.current.delete(taskId);

      // After the state update commits, dispatch the [HivemindReview]
      // follow-up to Queen and arm a 3-min watchdog as a fallback (in case
      // Queen never re-emits — likely cause: provider failure or prompt
      // regression). The watchdog clears `pendingFeaturesRefresh` so the
      // user falls back to launching with original features rather than
      // staring at a perpetually-disabled CTA.
      if (isSwarmPlanningTask) {
        const followUp =
          "[HivemindReview]\n" +
          "The Hivemind reviewers have produced a refined master plan based on your original. " +
          "Read the refined plan below carefully, then call BOTH `submit_plan` (with the refined " +
          "plan_markdown) and `submit_features` (with refined features and milestones) to commit " +
          "the refinement. Do not ask the user questions — the review feedback is the final input.\n\n" +
          "---\n\n" +
          finalPlan +
          "\n\n---\n";
        armFeaturesRefreshAndDispatch(taskId, followUp);
      }
    },
    [updateTask, armFeaturesRefreshAndDispatch],
  );
  finishReviewRef.current = finishReviewFlow;

  /* ── Merge cleanup helpers ──────────────────────────────── */
  const cleanupMergeSessionRefs = (taskId: string, mergeSid: string | null | undefined) => {
    if (mergeSid) {
      internalSessionIdsRef.current.delete(mergeSid);
      delete sessionIdToReviewIdRef.current[mergeSid];
      delete sessionIdToTaskIdRef.current[mergeSid];
    }
    delete reviewAccumulatorsRef.current[taskId];
    delete mergeLastChunkAtRef.current[taskId];
  };

  /** When the `structured_features` reducer clause clears
   *  `pendingFeaturesRefresh` (because Queen re-emitted features after a
   *  Hivemind review), cancel the watchdog timer so we don't end up
   *  surfacing a spurious "didn't re-emit in 3 min" error after the
   *  refresh has actually succeeded. */
  useEffect(() => {
    for (const [taskId, t] of Object.entries(tasks)) {
      if (!t.pendingFeaturesRefresh && pendingFeaturesRefreshTimersRef.current[taskId]) {
        clearTimeout(pendingFeaturesRefreshTimersRef.current[taskId]);
        delete pendingFeaturesRefreshTimersRef.current[taskId];
      }
    }
  }, [tasks]);

  /** Pure lookup against the in-memory catalog cache. Falls back to
   *  bare-model-id match ONLY when there is exactly one match across all
   *  cached providers — ambiguous bare-id resolution is rejected so we
   *  don't accidentally pick the wrong provider's context window. */
  const catalogLookup = useCallback((provider: string, modelId: string): number | undefined => {
    const cache = modelCatalogCacheRef.current;
    const fq = `${provider}/${modelId}`;
    if (typeof cache[fq] === "number" && cache[fq] > 0) return cache[fq];
    // Bare-id fallback: collect all entries whose model-id suffix matches.
    const suffix = `/${modelId}`;
    const matches: number[] = [];
    for (const [k, v] of Object.entries(cache)) {
      if (k.endsWith(suffix) && v > 0) matches.push(v);
    }
    if (matches.length === 1) return matches[0];
    return undefined;
  }, []);

  /** Populate the catalog cache for a single provider (idempotent). Returns
   *  immediately if already loaded for that provider. Failures are logged
   *  but non-fatal — the resolver falls through to the 200k constant. */
  const ensureCatalogForProvider = useCallback(async (provider: string): Promise<void> => {
    if (!provider) return;
    if (modelCatalogProviderLoadedRef.current.has(provider)) return;
    try {
      const entries = await ipc.refreshModels(provider);
      for (const e of entries) {
        if (e.context_window > 0) {
          modelCatalogCacheRef.current[`${e.provider}/${e.model_id}`] = e.context_window;
        }
      }
      modelCatalogProviderLoadedRef.current.add(provider);
    } catch (e) {
      // eslint-disable-next-line no-console
      console.warn("[review] catalog refresh failed", { provider, error: String(e) });
    }
  }, []);



  /* ── Resume an interrupted review at any phase ────────────── */
  const resumeReview = useCallback(
    async (taskId: string, stateOverride?: ReviewInterruptedState) => {
      const cur = tasksRef.current[taskId];
      if (!cur) return;
      const state = stateOverride ?? cur.reviewInterrupted;
      if (!state) return;

      ipc
        .logReviewEvent(state.reviewId, "review_resumed", {
          phase: state.phase,
          job_id: state.jobId,
          round: state.round,
          total_rounds: state.totalRounds,
        })
        .catch(() => {});

      switch (state.phase) {
        case "merge":
          // Backend now owns merge lifecycle and recovery; surface an error
          // so the user knows the legacy resume path is gone.
          updateTask(taskId, (t) => ({
            ...t,
            error:
              "Resume of an interrupted merge is now handled by the backend automatically — no frontend recovery needed.",
          }));
          return;

        case "final": {
          // The merge orchestrator persists the merged plan via the
          // `submit_plan` tool, and the backend writes the result to
          // `state.planText`. There is no text-delimited fallback.
          const finalPlan = state.planText;
          updateTask(taskId, (t) => ({
            ...resetSessionStats(
              applyTaskEvent(t, { kind: "review_resume_started" }, defaultModelRef.current),
            ),
            phase:
              PHASE_RANK[t.phase] > PHASE_RANK["review"]
                ? t.phase
                : "plan-ready",
            planText: finalPlan,
            reviewCompleted: true,
            streaming: false,
            messages: [
              ...t.messages,
              { who: "session-divider", dividerLabel: "Hivemind review resumed — final plan", createdAt: Date.now() },
              { who: "plan", planText: finalPlan, createdAt: Date.now() },
            ],
            reviewProgress: null,
            activeReviewJobId: null,
            internalPi: null,
          }));
          return;
        }

        case "context": {
          // Reset the task back to plan-ready with the persisted plan, then
          // re-trigger the review. triggerReviewForTask requires
          // hivemind to be set; preserve it from cur.hivemind.
          if (!cur.hivemind) {
            updateTask(taskId, (t) => ({
              ...t,
              error: "Cannot resume review: hivemind selection missing.",
            }));
            return;
          }
          updateTask(taskId, (t) =>
            applyTaskEvent(
              {
                ...t,
                phase: "plan-ready",
                planText: state.planText,
                reviewCompleted: false,
                reviewProgress: null,
                activeReviewJobId: null,
              },
              { kind: "review_resume_started" },
              defaultModelRef.current,
            ),
          );
          await triggerReviewRef.current?.(taskId);
          return;
        }

        case "round":
        case "between_rounds": {
          // Construct a fresh review flow and dispatch a single round via
          // ipc.startReview. The backend creates a new jobs row (linked to
          // this taskId) and replays the same models. Response cache absorbs
          // duplicates within-session; after a cold reboot the models
          // re-execute (acceptable v1 trade-off).
          const planText = state.planText;
          const targetRound =
            state.phase === "between_rounds" ? state.round + 1 : state.round;

          if (state.models.length === 0) {
            updateTask(taskId, (t) => ({
              ...t,
              error: "Cannot resume review: no models recorded in snapshot.",
            }));
            return;
          }

          const reviewId = state.reviewId;
          const orchModel = cur.model || defaultModelRef.current;
          const orchThinking = cur.thinking || "high";

          // Synthesise a roundsConfig from the snapshot's model list. Used
          // by startNextReviewRound downstream.
          const roundModels = state.models.map((m) => ({
            id: m.modelId,
            provider: m.provider,
            thinking: m.thinking || "none",
            max_tokens: 16384,
          }));
          const roundsConfig: RoundConfig[] = [{ models: roundModels, timeout: 600 }];

          reviewFlowsRef.current[taskId] = {
            active: true,
            phase: "round",
            enrichedPrompt: planText,
            currentRound: 0,
            roundsConfig,
            currentPlan: planText,
            currentJobId: null,
            isStartingRound: true,
            contextSid: null,
            reviewId,
            orchestratorModel: orchModel,
            orchestratorThinking: orchThinking,
            reviewPromptFromTool: null,
          };

          updateTask(taskId, (t) =>
            applyTaskEvent(
              {
                ...t,
                phase: "review",
                planText,
                reviewCompleted: false,
                reviewProgress: {
                  reviewId,
                  currentRound: targetRound - 1,
                  totalRounds: state.totalRounds,
                  phase: "reviewing",
                  rounds: [],
                },
                activeReviewJobId: null,
              },
              { kind: "review_resume_started" },
              defaultModelRef.current,
            ),
          );

          try {
            const taskName = cur.taskMeta?.title || planText.slice(0, 200);
            const resumeProjectPath =
              cur.projectPath ?? defaultProjectPathRef.current ?? null;
            const reviewOptions = {
              numRounds: 1,
              // Resume at the same 1-based round the snapshot is at so the
              // backend's capture filenames and round-keyed events align.
              roundNumber: targetRound,
              models: roundModels.map(modelRefForReview),
              timeoutSeconds: roundsConfig[0].timeout,
              reviewId,
              hivemindId: undefined,
              name: taskName,
              taskId,
              projectPath:
                resumeProjectPath && resumeProjectPath.trim()
                  ? resumeProjectPath
                  : undefined,
              orchestratorModel: orchModel || undefined,
            };
            const reviewerPlan = buildReviewerPlan(
              planText,
              reviewFlowsRef.current[taskId]?.enrichedPrompt ?? null,
            );
            const jobId = await ipc.startReview(reviewerPlan, reviewOptions);
            const currentFlow = reviewFlowsRef.current[taskId];
            if (!currentFlow || currentFlow.reviewId !== reviewId) return;
            currentFlow.currentJobId = jobId;
            currentFlow.isStartingRound = false;
            if (mountedRef.current) {
              updateTask(taskId, (t) =>
                applyTaskEvent(
                  t,
                  {
                    kind: "review_start",
                    jobId,
                    round: targetRound,
                    totalRounds: state.totalRounds,
                    models: reviewOptions.models,
                    reviewId,
                  },
                  defaultModelRef.current,
                ),
              );
            }
          } catch (e) {
            console.error("[review] resumeReview round dispatch failed", e);
            const failedFlow = reviewFlowsRef.current[taskId];
            if (failedFlow) {
              failedFlow.isStartingRound = false;
            }
            ipc
              .logReviewEvent(reviewId, "review_failed", {
                error: String(e),
                round: targetRound,
              })
              .catch(() => {});
            updateTask(taskId, (t) => ({
              ...t,
              error: `Resume review failed: ${e}`,
              reviewProgress: null,
              activeReviewJobId: null,
              internalPi: null,
              streaming: false,
              phase: PHASE_RANK[t.phase] > PHASE_RANK["review"] ? t.phase : "plan-ready",
            }));
            reviewFlowsRef.current[taskId] = null;
          }
          return;
        }
      }
    },
    [clearStaleReviewFlow, syncTaskRefAndState, updateTask],
  );

  /* ── replayReview ───────────────────────────────────────── */
  const replayReview = useCallback(
    (opts: { enrichedPrompt: string; hivemindId: string; projectPath?: string | null }): string => {
      const { enrichedPrompt, hivemindId, projectPath } = opts;

      // 1. Look up the selected hivemind and validate
      const hm = hivemindOptions.find((h) => h.id === hivemindId);
      if (!hm) throw new Error("Selected hivemind not found");
      const roundsConfig = parseRoundsConfig(hm.rounds_config);
      if (roundsConfig.length === 0) throw new Error("Hivemind has no rounds configured");

      // 2. Resolve task model — guard against empty default
      const model = defaultModelRef.current || "";
      if (!model) {
        throw new Error("No default model configured. Set a default model in Settings before replaying.");
      }

      // 3. Resolve orchestrator model (same logic as triggerReviewForTask)
      const orchModel = resolveOrchestratorModel(hm, model);
      const orchThinking =
        !hm.inherit_orchestrator && hm.orchestrator_thinking
          ? hm.orchestrator_thinking
          : "high";

      // 4. Create a properly-persisted task via createTask
      const taskId = createTask({
        prompt: undefined,
        model,
        hivemind: hivemindId,
        projectPath: projectPath ?? defaultProjectPathRef.current ?? null,
        setActive: true,
        thinking: orchThinking,
      });

      // 5. Wrap the post-creation setup in try/catch
      try {
        const reviewId = `hmr-${crypto.randomUUID().replace(/-/g, "").slice(0, 8)}`;

        // 6. Immediately patch the task into review phase
        updateTask(taskId, (t) =>
          resetSessionStats({
            ...t,
            phase: "review",
            planText: enrichedPrompt,
            hivemind: hivemindId,
            model,
            reviewCompleted: false,
            streaming: true,
            reviewProgress: {
              reviewId,
              currentRound: 0,
              totalRounds: roundsConfig.length,
              phase: "reviewing",
              rounds: [],
            },
            messages: [
              {
                who: "session-divider",
                dividerLabel: "Hivemind review replay started",
                dividerModel: hm.name,
                dividerAgentModel: orchModel,
                dividerThinking: orchThinking,
                createdAt: Date.now(),
              },
            ],
          }),
        );

        // 7. Update the sidebar title
        setLocalTasks((prev) =>
          prev.map((t) =>
            t.id === taskId
              ? { ...t, title: `Replay \u2014 ${hm.name}`, phase: "review" }
              : t,
          ),
        );

        // 8. Set up ReviewFlowState — skip "context" phase, go straight to "round"
        reviewFlowsRef.current[taskId] = {
          active: true,
          phase: "round",
          enrichedPrompt,
          currentRound: 0,
          roundsConfig,
          currentPlan: enrichedPrompt,
          currentJobId: null,
          isStartingRound: false,
          contextSid: null,
          reviewId,
          hivemindId: hm.id,
          orchestratorModel: orchModel,
          orchestratorThinking: orchThinking,
          orchestratorContextWindow: hm.orchestrator_context_window ?? null,
          orchestratorMaxOutput: hm.orchestrator_max_output ?? null,
          orchestratorInherit: hm.inherit_orchestrator,
          reviewPromptFromTool: null,
        };
        reviewAccumulatorsRef.current[taskId] = "";

        console.info(
          `[replay] flow set up for task=${taskId}, reviewId=${reviewId}, rounds=${roundsConfig.length}`,
        );

        // 9. Log the replay start for the review timeline
        ipc
          .logReviewEvent(reviewId, "review_started", {
            replay: true,
            hivemind_id: hm.id,
            hivemind_name: hm.name,
            enriched_prompt_length: enrichedPrompt.length,
          })
          .catch(() => {});

        // 10. Dispatch round 1 via setTimeout(0)
        setTimeout(() => {
          const roundStarter = startNextRoundRef.current;
          if (!roundStarter) {
            console.error("[replay] startNextRoundRef is null \u2014 round 1 will not start");
            updateTask(taskId, (t) => ({
              ...t,
              error: "Internal error: review dispatch unavailable. Try again.",
              streaming: false,
              phase: "plan-ready",
            }));
            reviewFlowsRef.current[taskId] = null;
            return;
          }
          roundStarter(taskId);
        }, 0);
      } catch (e) {
        // Partial failure after createTask
        console.error("[replay] setup failed after createTask:", e);
        updateTask(taskId, (t) => ({
          ...t,
          error: `Replay setup failed: ${e instanceof Error ? e.message : String(e)}`,
          streaming: false,
          phase: "plan-ready",
        }));
        reviewFlowsRef.current[taskId] = null;
      }

      return taskId;
    },
    [hivemindOptions, createTask, updateTask],
  );

  /* ── Reconciliation: catch missed events / pool deadlocks ── */
  const reconcileReview = useCallback(
    async (taskId: string) => {
      const t = tasksRef.current[taskId];
      if (!t || t.reviewCompleted) return;

      // ── Crash-recovery probe (phase-agnostic) ──
      // If there's NO active in-memory orchestration flow, the host process
      // may have just restarted. Probe the SQLite-backed resumable-review
      // lookup keyed by taskId. This works even when `activeReviewJobId` is
      // null (hard reboot wipes in-memory state but SQLite survives).
      const hasActiveFlow = !!reviewFlowsRef.current[taskId];
      if (!hasActiveFlow && !t.reviewInterrupted) {
        try {
          const snap = await ipc.getResumableReviewForTask(taskId);
          if (snap) {
            updateTask(taskId, (cur) =>
              applyTaskEvent(
                cur,
                { kind: "review_interrupted", snapshot: snap },
                defaultModelRef.current,
              ),
            );
          }
        } catch (e) {
          console.warn("[review] getResumableReviewForTask probe failed", e);
        }
      }

      // The remaining reconcile logic (getReviewState + decideReconcile) only
      // makes sense when we have an active in-memory job id. Skip otherwise —
      // the crash-recovery probe above is the sole path for hard-reboot
      // recovery.
      if (!t.activeReviewJobId) return;
      const jobId = t.activeReviewJobId;

      let snapshot: ReviewStateSnapshot;
      try {
        snapshot = await ipc.getReviewState(jobId);
      } catch (e) {
        console.warn("[review] reconcile getReviewState failed", e);
        return;
      }

      // Always sync UI rows from canonical SQLite state, regardless of decision.
      updateTask(taskId, (cur) =>
        applyTaskEvent(cur, { kind: "review_resync", snapshot }, defaultModelRef.current),
      );

      const flow = reviewFlowsRef.current[taskId] ?? null;
      const lastChunkAt = mergeLastChunkAtRef.current[taskId] ?? 0;
      const mergeIdleMs = lastChunkAt
        ? Date.now() - lastChunkAt
        : Number.POSITIVE_INFINITY;
      const lastContextEventAt = contextLastEventAtRef.current[taskId] ?? 0;
      const contextIdleMs = lastContextEventAt
        ? Date.now() - lastContextEventAt
        : Number.POSITIVE_INFINITY;
      const decision = decideReconcile(
        snapshot,
        flow,
        mergeIdleMs,
        RECONCILE_MERGE_STUCK_MS,
        contextIdleMs,
        RECONCILE_CONTEXT_STUCK_MS,
      );

      // RC5 fix: include flow_current_job_id and merge_sid so a future
      // stale-jobId / mismatch can be diagnosed from the review log alone.
      ipc
        .logReviewEvent(flow?.reviewId || "", "reconcile_decision", {
          job_id: jobId,
          status: snapshot.status,
          flow_phase: flow?.phase ?? null,
          decision: decision.kind,
          merge_idle_ms: Number.isFinite(mergeIdleMs) ? mergeIdleMs : null,
          context_idle_ms: Number.isFinite(contextIdleMs) ? contextIdleMs : null,
          flow_current_job_id: flow?.currentJobId ?? null,
          context_sid: flow?.contextSid ?? null,
        })
        .catch(() => {});

      switch (decision.kind) {
        case "noop":
          return;

        case "merge_stuck": {
          cleanupMergeSessionRefs(taskId, null);
          // Mark the backend job cancelled so it doesn't linger in SQLite as "running".
          ipc.cancelReview(decision.jobId).catch(() => {});
          updateTask(taskId, (cur) =>
            applyTaskEvent(
              cur,
              {
                kind: "review_error",
                error: `Hivemind merge stalled — no output for over ${RECONCILE_MERGE_STUCK_MS / 1000}s`,
              },
              defaultModelRef.current,
            ),
          );
          reviewFlowsRef.current[taskId] = null;
          return;
        }

        case "context_stuck": {
          const stuckFlow = reviewFlowsRef.current[taskId];
          if (stuckFlow?.contextWatchdog) {
            clearTimeout(stuckFlow.contextWatchdog);
            stuckFlow.contextWatchdog = null;
          }
          const stuckCtxSid = stuckFlow?.contextSid ?? null;
          if (stuckCtxSid) {
            internalSessionIdsRef.current.delete(stuckCtxSid);
            delete sessionIdToReviewIdRef.current[stuckCtxSid];
            ipc.stopChat(stuckCtxSid).catch(() => {});
          }
          delete contextLastEventAtRef.current[taskId];
          const errMsg = `Hivemind context gathering stalled — no output for over ${
            RECONCILE_CONTEXT_STUCK_MS / 1000
          }s`;
          if (stuckCtxSid) {
            setTasks((prev) => {
              const cur = prev[taskId];
              if (!cur) return prev;
              const next = applyTaskEvent(
                cur,
                { kind: "internal_pi_failed", sessionId: stuckCtxSid, message: errMsg },
                defaultModelRef.current,
              );
              return next === cur ? prev : { ...prev, [taskId]: next };
            });
          }
          updateTask(taskId, (cur) =>
            applyTaskEvent(
              cur,
              { kind: "review_error", error: errMsg },
              defaultModelRef.current,
            ),
          );
          reviewFlowsRef.current[taskId] = null;
          return;
        }

        case "ended":
          if (decision.status === "completed") return;
          // failed / cancelled — surface as review_error and clear flow.
          {
            const endedFlow = reviewFlowsRef.current[taskId];
            delete mergeLastChunkAtRef.current[taskId];
            updateTask(taskId, (cur) =>
              applyTaskEvent(
                cur,
                {
                  kind: "review_error",
                  error: decision.error ?? `Review ${decision.status}`,
                },
                defaultModelRef.current,
              ),
            );
            reviewFlowsRef.current[taskId] = null;
          }
          return;
      }
    },
    [updateTask],
  );
  reconcileReviewRef.current = reconcileReview;

  /* ── Lazy reconciliation interval (only runs while reviews active) ── */
  useEffect(() => {
    if (!isTauri()) return;
    const anyActive = Object.values(tasks).some(
      (t) => t.activeReviewJobId && !t.reviewCompleted,
    );
    if (!anyActive) return;
    const id = setInterval(() => {
      for (const t of Object.values(tasksRef.current)) {
        if (t.activeReviewJobId && !t.reviewCompleted) {
          reconcileReviewRef.current?.(t.taskId);
        }
      }
    }, 5_000);
    return () => clearInterval(id);
  }, [tasks]);

  /* ── Window focus → reconcile every active review ──────────── */
  useEffect(() => {
    if (!isTauri()) return;
    const onFocus = () => {
      for (const t of Object.values(tasksRef.current)) {
        if (t.activeReviewJobId && !t.reviewCompleted) {
          reconcileReviewRef.current?.(t.taskId);
        }
      }
    };
    window.addEventListener("focus", onFocus);
    return () => window.removeEventListener("focus", onFocus);
  }, []);

  const triggerReviewForTask = useCallback<TriggerReviewForTask>(
    async (taskId, opts) => {
      const cur = tasksRef.current[taskId];
      if (!cur) return;
      if (!opts?.force && cur.phase === "review") return;
      if (!cur.planText || !cur.hivemind) return;

      const hm = hivemindOptions.find((h) => h.id === cur.hivemind);
      if (!hm) {
        updateTask(taskId, (t) => ({ ...t, error: "Selected hivemind not found" }));
        return;
      }
      const roundsConfig = parseRoundsConfig(hm.rounds_config);
      if (roundsConfig.length === 0) {
        updateTask(taskId, (t) => ({ ...t, error: "Hivemind has no rounds configured" }));
        return;
      }

      const orchModel = resolveOrchestratorModel(hm, cur.model);
      const orchThinking =
        !hm.inherit_orchestrator && hm.orchestrator_thinking
          ? hm.orchestrator_thinking
          : cur.thinking;

      if (cur.sessionId) {
        try {
          await ipc.stopChat(cur.sessionId);
        } catch (e) {
          // Don't silently swallow: leaking a planning Pi permit contributes
          // to pool exhaustion and stalls future merge spawns. Continue
          // anyway — the new review can still start; the leaked permit will
          // free when the prior Pi process exits on its own.
          console.warn("[review] failed to stop prior plan session", e);
        }
      }

      const reviewId = `hmr-${crypto.randomUUID().replace(/-/g, "").slice(0, 8)}`;
      const contextSid = crypto.randomUUID();
      const dividerUsage = cur.sessionUsage
        ? {
            input: cur.sessionUsage.input,
            output: cur.sessionUsage.output,
            contextPercent: cur.sessionUsage.contextPercent,
            cost: cur.sessionUsage.cost,
            tokPerSec: cur.sessionUsage.tokPerSec,
          }
        : undefined;

      updateTask(taskId, (t) =>
        resetSessionStats({
          ...t,
          reviewCompleted: false,
          phase: "review",
          sessionId: contextSid,
          streaming: true,
          reviewProgress: {
            reviewId,
            currentRound: 0,
            totalRounds: roundsConfig.length,
            phase: "context",
            rounds: [],
          },
          messages: [
            ...t.messages,
            {
              who: "session-divider",
              dividerSessionId: cur.sessionId ?? undefined,
              dividerUsage,
              createdAt: Date.now(),
            },
            {
              who: "session-divider",
              dividerLabel: "Hivemind review started",
              dividerModel: hm.name,
              // orchModel is the actual LLM model driving the review session.
              // cur.model is set during task initialization (lines ~306-308),
              // so it is expected to be populated by the time we reach here (post-planning).
              // If somehow falsy, the rendering guard ({m.dividerAgentModel && …}) gracefully omits it.
              dividerAgentModel: orchModel,
              dividerThinking: t.thinking,
              dividerSessionId: contextSid,
              createdAt: Date.now(),
            },
          ],
        }),
      );

      reviewFlowsRef.current[taskId] = {
        active: true,
        phase: "context",
        enrichedPrompt: null,
        currentRound: 0,
        roundsConfig,
        // Features for swarm-planning tasks flow through the merge
        // orchestrator's `submit_features` tool call, not inline with
        // the plan body. The reviewer-facing plan is the raw plan text.
        currentPlan: cur.planText,
        currentJobId: null,
        isStartingRound: false,
        contextSid,
        reviewId,
        hivemindId: hm.id,
        orchestratorModel: orchModel,
        orchestratorThinking: orchThinking,
        orchestratorContextWindow: hm.orchestrator_context_window ?? null,
        orchestratorMaxOutput: hm.orchestrator_max_output ?? null,
        orchestratorInherit: hm.inherit_orchestrator,
        reviewPromptFromTool: null,
      };
      sessionIdToTaskIdRef.current[contextSid] = taskId;
      internalSessionIdsRef.current.add(contextSid);
      sessionIdToReviewIdRef.current[contextSid] = reviewId;
      reviewAccumulatorsRef.current[taskId] = "";
      // Seed liveness baseline so the context_stuck reconciler classifies
      // a brand-new context Pi as alive until its idle window elapses.
      contextLastEventAtRef.current[taskId] = Date.now();

      // Mark the context Pi as the active internal session so the
      // HivemindReviewBar telemetry strip can show its model + live tokens.
      updateTask(taskId, (t) =>
        applyTaskEvent(
          t,
          {
            kind: "internal_pi_started",
            sessionId: contextSid,
            modelName: orchModel,
            piKind: "context",
          },
          defaultModelRef.current,
        ),
      );

      ipc
        .logReviewEvent(reviewId, "review_started", {
          hivemind: hm.name,
          rounds: roundsConfig.length,
          models: roundsConfig.flatMap((r) => r.models.map(modelRefForReview)),
        })
        .catch(() => {});

      const contextPrompt = buildContextGatherPrompt(cur.planText);
      ipc
        .logReviewEvent(reviewId, "context_started", { session_id: contextSid })
        .catch(() => {});

      // Persist the context session mapping for orchestrator usage.
      // Best-effort: failures are logged but don't block the review.
      if (contextSid && typeof contextSid === "string" && contextSid.length > 0) {
        ipc.registerContextSession({
          reviewId,
          sessionId: contextSid,
          modelId: orchModel,
          provider: providerOf(orchModel),
        }).catch((err) => {
          console.error("[orchestrator] registerContextSession failed:", err);
        });
      }

      try {
        await ipc.sendMessage(
          contextPrompt,
          orchModel,
          contextSid,
          cur.projectPath ?? project?.cwd,
          orchThinking,
          REVIEW_CONTEXT_SYSTEM_PROMPT,
          PLAN_TOOL_SET,
        );

        // Nurse-driven check-in watchdog (replaces the old hard wall-clock
        // kill). At `chat_check_in_secs` we ask Nurse whether to leave the
        // session alone, steer it, restart it, or cancel. On `leave_it` we
        // rearm at the Nurse-chosen `check_back_secs`. On `steer` the
        // backend applies the steer in-band and we rearm at the configured
        // interval. On `restart`/`cancel` we tear the session down with a
        // user-facing error.
        const checkInMs = loadChatCheckInMs();
        const handleNurseDecision = async (): Promise<void> => {
          const stuckFlow = reviewFlowsRef.current[taskId];
          if (!stuckFlow || stuckFlow.contextSid !== contextSid) return;
          let decision: ipc.NurseDecisionDto;
          try {
            decision = await ipc.checkChatSession(contextSid, "context");
          } catch (err) {
            console.warn("[review] context nurse check failed", err);
            const errStr = ipc.formatIpcError(err);
            decision = {
              kind: "cancel",
              reasoning: `nurse evaluation failed: ${errStr}`,
              message: `Hivemind context-gather nurse check failed: ${errStr}`,
            };
          }
          const flowAfter = reviewFlowsRef.current[taskId];
          // Bail if the flow advanced past the context phase (success path
          // nulls contextSid) or the session changed. Without this guard a
          // late Nurse response on a completed context phase would rearm
          // the watchdog and later surface a misleading user-facing error.
          if (!flowAfter || flowAfter.contextSid !== contextSid) {
            console.debug(
              "[review] context nurse decision stale, dropping",
              { contextSid },
            );
            return;
          }

          // Session torn down between watchdog fire and Nurse evaluation —
          // clear silently. (Common race: context-gather completes during
          // an in-flight Nurse LLM call.)
          if (decision.kind === "noop") {
            flowAfter.contextWatchdog = null;
            return;
          }

          ipc
            .logReviewEvent(reviewId, `nurse_${decision.kind}`, {
              session_id: contextSid,
              phase: "context",
              reasoning: decision.reasoning,
              ...(decision.kind === "leave_it"
                ? { check_back_secs: decision.check_back_secs }
                : {}),
            })
            .catch(() => {});

          if (decision.kind === "leave_it" || decision.kind === "steer") {
            // Rearm. `leave_it` uses the nurse-chosen check_back_secs; for
            // `steer` we use the configured interval since the session is
            // now redirected and the next checkpoint is the user's call.
            const nextMs =
              decision.kind === "leave_it"
                ? Math.max(
                    1,
                    Math.min(decision.check_back_secs, 1800),
                  ) * 1000
                : checkInMs;
            const next = setTimeout(() => {
              handleNurseDecision().catch(() => {});
            }, nextMs);
            flowAfter.contextWatchdog = next;
            return;
          }

          // restart / cancel → kill and error.
          ipc.stopChat(contextSid).catch(() => {});
          internalSessionIdsRef.current.delete(contextSid);
          delete sessionIdToReviewIdRef.current[contextSid];
          delete sessionIdToTaskIdRef.current[contextSid];
          delete contextLastEventAtRef.current[taskId];
          flowAfter.contextWatchdog = null;
          reviewFlowsRef.current[taskId] = null;
          const userMsg =
            decision.kind === "cancel"
              ? decision.message ||
                "Hivemind context gathering was cancelled by Nurse."
              : `Hivemind context gathering was restarted by Nurse (${decision.reasoning}).`;
          setTasks((prev) => {
            const cur2 = prev[taskId];
            if (!cur2) return prev;
            const nextState = applyTaskEvent(
              cur2,
              { kind: "internal_pi_failed", sessionId: contextSid, message: userMsg },
              defaultModelRef.current,
            );
            return nextState === cur2 ? prev : { ...prev, [taskId]: nextState };
          });
          updateTask(taskId, (t) => ({
            ...t,
            error: userMsg,
            streaming: false,
            reviewProgress: null,
            activeReviewJobId: null,
            internalPi: null,
            phase: PHASE_RANK[t.phase] > PHASE_RANK["plan-ready"] ? t.phase : "plan-ready",
          }));
        };

        const watchdog = setTimeout(() => {
          handleNurseDecision().catch(() => {});
        }, checkInMs);
        const flowAfterSend = reviewFlowsRef.current[taskId];
        if (flowAfterSend && flowAfterSend.contextSid === contextSid) {
          flowAfterSend.contextWatchdog = watchdog;
        } else {
          // Flow was replaced/cleared between sendMessage and now — don't
          // leak the timer.
          clearTimeout(watchdog);
        }
      } catch (e) {
        console.error("Review context failed:", e);
        ipc
          .logReviewEvent(reviewId, "context_failed", { error: String(e) })
          .catch(() => {});
        const failedFlow = reviewFlowsRef.current[taskId];
        if (failedFlow?.contextWatchdog) {
          clearTimeout(failedFlow.contextWatchdog);
          failedFlow.contextWatchdog = null;
        }
        delete contextLastEventAtRef.current[taskId];
        updateTask(taskId, (t) => ({
          ...t,
          error: `Review context gathering failed: ${e}`,
          reviewProgress: null,
          activeReviewJobId: null,
          internalPi: null,
          phase: "plan-ready",
        }));
        reviewFlowsRef.current[taskId] = null;
      }
    },
    [hivemindOptions, project, updateTask],
  );
  triggerReviewRef.current = triggerReviewForTask;

  /* ── retryReview ───────────────────────────────────── */
  const retryReview = useCallback(
    async (taskId: string) => {
      if (retryReviewInFlightRef.current.has(taskId)) return;
      retryReviewInFlightRef.current.add(taskId);

      try {
        const initial = tasksRef.current[taskId];
        if (!initial) return;

        let snapshot: ResumableReviewSnapshot | null = null;
        try {
          snapshot = await ipc.getResumableReviewForTask(taskId);
        } catch (e) {
          console.warn(
            "[review] retryReview snapshot probe failed; falling back to fresh restart",
            e,
          );
        }

        if (!mountedRef.current) return;

        const latest = tasksRef.current[taskId];
        if (!latest) return;

        const activeReviewInProgress =
          !!latest.reviewProgress || !!latest.activeReviewJobId || latest.streaming;

        // Avoid `{ force: true }` creating a duplicate context session if
        // another review started while the snapshot probe was in flight.
        if (activeReviewInProgress) return;

        const canRetryError = canRetryErroredReviewState(latest);
        if (!snapshot && !latest.reviewInterrupted && !canRetryError) return;

        const reviewId =
          snapshot?.reviewId ?? latest.reviewInterrupted?.reviewId ?? "";

        if (reviewId) {
          ipc
            .logReviewEvent(reviewId, "review_retry_clicked", {
              had_snapshot: !!snapshot,
              phase: snapshot?.phase ?? latest.reviewInterrupted?.phase ?? null,
            })
            .catch(() => {});
        } else {
          console.info(
            "[review] retry clicked without existing review id; starting fresh if possible",
          );
        }

        if (snapshot) {
          // SQLite is the source of truth for crash recovery; prefer it
          // over any older in-memory interruption state.
          await stopStaleInternalReviewSessions(taskId, latest);
          clearStaleReviewFlow(taskId);

          const interrupted = reviewInterruptedFromSnapshot(snapshot);

          // Keep UI state and the synchronous ref mirror aligned before
          // chaining into resumeReview. Without this, resumeReview can read
          // stale tasksRef.current and no-op.
          const mirrored = syncTaskRefAndState(taskId, (t) =>
            applyTaskEvent(
              t,
              { kind: "review_interrupted", snapshot },
              defaultModelRef.current,
            ),
          );
          if (!mirrored || !mountedRef.current) return;

          await resumeReview(taskId, interrupted);
          return;
        }

        if (latest.reviewInterrupted) {
          await stopStaleInternalReviewSessions(taskId, latest);
          clearStaleReviewFlow(taskId);
          await resumeReview(taskId, latest.reviewInterrupted);
          return;
        }

        if (!latest.planText || !latest.hivemind) {
          updateTask(taskId, (t) => ({
            ...t,
            error: "Cannot retry review: plan text or hivemind missing.",
          }));
          return;
        }

        clearStaleReviewFlow(taskId);

        // Preserve sessionId until triggerReviewForTask runs so its existing
        // stopChat cleanup can terminate the prior planning/context session.
        const reset = syncTaskRefAndState(taskId, (t) => ({
          ...t,
          phase: "plan-ready",
          error: null,
          reviewProgress: null,
          activeReviewJobId: null,
          internalPi: null,
          streaming: false,
          reviewCompleted: false,
          reviewInterrupted: null,
          mergeInterrupted: null,
          queueState: null,
        }));
        if (!reset || !mountedRef.current) return;

        await triggerReviewForTask(taskId, { force: true });
      } catch (e) {
        console.error("[review] retryReview failed", e);
        if (mountedRef.current) {
          updateTask(taskId, (t) => ({
            ...t,
            error: `Retry review failed: ${e}`,
            reviewProgress: null,
            activeReviewJobId: null,
            internalPi: null,
            streaming: false,
            phase:
              PHASE_RANK[t.phase] > PHASE_RANK["review"]
                ? t.phase
                : "review",
          }));
        }
      } finally {
        retryReviewInFlightRef.current.delete(taskId);
      }
    },
    [
      clearStaleReviewFlow,
      resumeReview,
      stopStaleInternalReviewSessions,
      syncTaskRefAndState,
      triggerReviewForTask,
      updateTask,
    ],
  );

  /* ── implementPlan ────────────────────────────────────── */
  const implementPlan = useCallback(
    async (
      taskId: string,
      fallbackUsage?: { input: number; output: number; contextPercent: number },
    ) => {
      const cur = tasksRef.current[taskId];
      if (!cur || cur.phase === "implement") return;
      if (!cur.planText) return;
      const oldSid = cur.sessionId;
      if (oldSid) {
        console.info(`[plan-mode] killing plan session ${oldSid}`);
        try {
          await ipc.stopChat(oldSid);
        } catch (e) {
          console.warn("[plan-mode] failed to stop plan session:", e);
        }
      }

      const implSid = crypto.randomUUID();
      sessionIdToTaskIdRef.current[implSid] = taskId;
      console.info(`[plan-mode] spawning implementation session ${implSid}`);

      const dividerUsage = cur.sessionUsage
        ? {
            input: cur.sessionUsage.input,
            output: cur.sessionUsage.output,
            contextPercent: cur.sessionUsage.contextPercent,
            cost: cur.sessionUsage.cost,
            tokPerSec: cur.sessionUsage.tokPerSec,
          }
        : fallbackUsage
        ? { ...fallbackUsage, cost: 0, tokPerSec: 0 }
        : { input: 0, output: 0, contextPercent: 0, cost: 0, tokPerSec: 0 };

      updateTask(taskId, (t) =>
        resetSessionStats({
          ...t,
          sessionId: implSid,
          streaming: true,
          phase: "implement",
          error: null,
          messages: [
            ...t.messages,
            { who: "session-divider", dividerSessionId: oldSid ?? undefined, dividerUsage, createdAt: Date.now() },
            {
              who: "session-divider",
              dividerLabel: "Implementation session started",
              dividerModel: t.model,
              dividerThinking: t.thinking,
              dividerSessionId: implSid,
              createdAt: Date.now(),
            },
          ],
        }),
      );

      setLocalTasks((prev) =>
        prev.map((t) => (t.id === taskId ? { ...t, phase: "implement" } : t)),
      );

      const implPrompt = buildImplementPrompt(cur.planText);
      try {
        await ipc.sendMessage(
          implPrompt,
          cur.model || defaultModelRef.current,
          implSid,
          cur.projectPath ?? project?.cwd,
          cur.thinking,
          IMPL_SYSTEM_PROMPT,
          IMPL_TOOL_SET,
        );
      } catch (e) {
        console.error("Failed to start implementation:", e);
        updateTask(taskId, (t) => ({ ...t, error: String(e), streaming: false }));
      }
    },
    [project, updateTask],
  );

  /* ── answerQuestions ──────────────────────────────────── */
  const answerQuestions = useCallback(
    async (taskId: string, questions: TaskQuestion[], answers: Record<string, any>) => {
      const answerText = buildAnswerPrompt(questions, answers);
      const userMsg: TaskMessage = { who: "user", text: answerText, t: "now", createdAt: Date.now() };
      const cur = tasksRef.current[taskId];

      // Clear pendingQuestions and progress state on ALL paths before the
      // IPC send (or return) so the reducer's next chunk/done/resync call
      // correctly gates on hasUnansweredQuestions. The synchronous
      // updateTask commits the user message before the async IPC, preventing
      // a race where a chunk arrives before the answer is in the message list.

      if (!cur?.sessionId || !isTauri()) {
        updateTask(taskId, (t) => ({
          ...t,
          messages: [...t.messages, userMsg],
          pendingQuestions: null,
          pendingQuestionIdx: 0,
          pendingQuestionAnswers: {},
        }));
        return;
      }

      // Normal path: update state and send IPC. The reducer will clear
      // streaming on the subsequent done/error/stop events.
      updateTask(taskId, (t) => ({
        ...t,
        messages: [...t.messages, userMsg],
        streaming: true,
        pendingQuestions: null,
        pendingQuestionIdx: 0,
        pendingQuestionAnswers: {},
        phase: PHASE_RANK[t.phase] > PHASE_RANK["plan"] ? t.phase : "plan",
      }));

      try {
        bindTaskSession(taskId, cur.sessionId);
        await ipc.sendMessage(
          answerText,
          cur.model || defaultModelRef.current,
          cur.sessionId,
          cur.projectPath ?? project?.cwd,
          cur.thinking,
          cur.swarmId ? QUEEN_PLANNING_SYSTEM_PROMPT : PLAN_SYSTEM_PROMPT,
          PLAN_TOOL_SET,
        );
      } catch (e) {
        console.error("Failed to send answers:", e);
        updateTask(taskId, (t) => ({ ...t, error: String(e), streaming: false }));
      }
    },
    [bindTaskSession, project, updateTask],
  );

  /* ── submitSwarmAnswers ────────────────────────────────── */
  const submitSwarmAnswers = useCallback(
    async (taskId: string, answers: ReadonlyArray<{ id: string; value: string }>) => {
      if (answers.length === 0) return;
      const answerText = buildSwarmAnswerPrompt(answers);
      const userMsg: TaskMessage = { who: "user", text: answerText, t: "now", createdAt: Date.now() };
      const answeredIds = answers.map((a) => a.id);
      const cur = tasksRef.current[taskId];
      // Mark the questions answered and drop them from the pending list
      // immediately so the modal closes even if the IPC send fails. The
      // user can always retry by re-sending a follow-up message.
      const stamp = (t: TaskRuntimeState): TaskRuntimeState => {
        const prevAnswered = new Set(t.answeredSwarmQuestionIds ?? []);
        for (const id of answeredIds) prevAnswered.add(id);
        const nextPending = (t.pendingSwarmQuestions ?? []).filter((q) => !prevAnswered.has(q.id));
        return {
          ...t,
          messages: [...t.messages, userMsg],
          pendingSwarmQuestions: nextPending.length > 0 ? nextPending : null,
          answeredSwarmQuestionIds: Array.from(prevAnswered),
        };
      };
      if (!cur?.sessionId || !isTauri()) {
        updateTask(taskId, stamp);
        return;
      }
      updateTask(taskId, (t) => {
        const stamped = stamp(t);
        return {
          ...stamped,
          streaming: true,
          phase: PHASE_RANK[stamped.phase] > PHASE_RANK["plan"] ? stamped.phase : "plan",
        };
      });
      try {
        bindTaskSession(taskId, cur.sessionId);
        await ipc.sendMessage(
          answerText,
          cur.model || defaultModelRef.current,
          cur.sessionId,
          cur.projectPath ?? project?.cwd,
          cur.thinking,
          cur.swarmId ? QUEEN_PLANNING_SYSTEM_PROMPT : PLAN_SYSTEM_PROMPT,
          PLAN_TOOL_SET,
        );
      } catch (e) {
        console.error("Failed to send swarm answers:", e);
        updateTask(taskId, (t) => ({ ...t, error: String(e), streaming: false }));
      }
    },
    [bindTaskSession, project, updateTask],
  );

  /* ── skipSwarmQuestions ────────────────────────────────── */
  const skipSwarmQuestions = useCallback(
    async (taskId: string) => {
      const cur = tasksRef.current[taskId];
      const pending = cur?.pendingSwarmQuestions ?? [];
      if (pending.length === 0) return;
      const answers = pending.map((q: SwarmQuestion) => ({
        id: q.id,
        value: SWARM_QUESTION_SKIPPED_VALUE,
      }));
      await submitSwarmAnswers(taskId, answers);
    },
    [submitSwarmAnswers],
  );

  /* ── Auto-implement effect ─────────────────────────────── */
  const autoImplFiredRef = useRef<Set<string>>(new Set());
  useEffect(() => {
    if (!isTauri()) return;
    for (const t of Object.values(tasks)) {
      if (t.phase !== "plan-ready") {
        autoImplFiredRef.current.delete(t.taskId);
        continue;
      }
      if (autoImplFiredRef.current.has(t.taskId)) continue;
      // Belt-and-braces: when questions are asked, auto-mode should never
      // skip them — even if a parser bug or unusual model output ratchets
      // the phase to plan-ready prematurely.
      if (hasUnansweredQuestions(t.messages)) continue;

      // ── Early hivemind fast-path for swarm-planning tasks ──
      // Queen Planning often keeps streaming after submit_plan + submit_features
      // (narrative, follow-up tools, occasional stalls). The streaming guard
      // below would correctly let us through once features land, but only after
      // the *next* tasks-state update; in practice it can take several seconds
      // (or never) for the right rerender to land. Fire the review immediately
      // when everything we need is on hand — planText AND features. Stop the
      // lingering planning session first so Queen doesn't keep burning tokens
      // while the review runs.
      if (
        !!t.swarmId &&
        !!t.planText &&
        (t.swarmFeatures?.length ?? 0) > 0 &&
        t.hivemind &&
        !t.reviewCompleted
      ) {
        autoImplFiredRef.current.add(t.taskId);
        if (t.sessionId) {
          ipc.stopChat(t.sessionId).catch(() => {});
        }
        triggerReviewRef.current?.(t.taskId);
        continue;
      }

      // Wait for streaming to finish only when the planning agent still
      // owes us data. Swarm-planning tasks (`t.swarmId` set) require both
      // `submit_plan` AND `submit_features`; regular Tasks-view planning
      // tasks only need `submit_plan` and the model frequently keeps
      // calling tools (or stalls indefinitely) after committing the plan.
      // Once the data we need is in hand, fire immediately — the firing
      // branches each call `ipc.stopChat()` to cancel the leftover
      // planning session.
      if (t.streaming) {
        const needFeaturesStill =
          !!t.swarmId &&
          (!t.swarmFeatures || t.swarmFeatures.length === 0) &&
          !t.swarmFeaturesError;
        if (needFeaturesStill) continue;
      }
      if (!t.planText) continue;

      // Non-swarm hivemind auto-trigger (safety net for the pre-existing
      // Tasks-view review flow — autoMode-gated since regular tasks don't
      // carry the explicit-opt-in semantics that swarm tasks do).
      if (t.hivemind && !t.reviewCompleted && (t.autoMode !== "off" || !!t.swarmId)) {
        autoImplFiredRef.current.add(t.taskId);
        triggerReviewRef.current?.(t.taskId);
        continue;
      }

      // Auto-launch / auto-implement branches below require full auto. In
      // "review"-only mode the task stops at plan-ready so the user can
      // inspect the reviewed plan and click Implement themselves.
      if (t.autoMode !== "full") continue;

      // Swarm-linked branch: auto-launch the backend swarm pipeline instead
      // of running a one-shot Pi implementer in this Task. Mirrors the manual
      // `handleLaunchPlanningSwarm` flow in Tasks.tsx.
      if (t.swarmId) {
        const swarmFeatures = t.swarmFeatures;
        const swarmMilestones = t.swarmMilestones ?? undefined;
        if (!swarmFeatures || swarmFeatures.length === 0) {
          // FEATURES block didn't parse yet — but if the model finished and
          // we recorded a parse error, surface it so the user knows why
          // auto-launch can't proceed. Otherwise leave the fire flag unset
          // so a later resync (which can rescue the block) re-evaluates.
          if (!t.streaming && t.swarmFeaturesError) {
            autoImplFiredRef.current.add(t.taskId);
            updateTask(t.taskId, (prev) => ({
              ...prev,
              error: `Auto-launch blocked: ${t.swarmFeaturesError}. Send a follow-up asking the agent to re-emit a valid FEATURES JSON block.`,
            }));
          }
          continue;
        }
        autoImplFiredRef.current.add(t.taskId);
        const swarmId = t.swarmId;
        const tid = t.taskId;
        const planSid = t.sessionId;
        (async () => {
          // Don't relaunch a swarm that already moved past Planning in a
          // previous session. On app restart, persisted tasks rehydrate at
          // `plan-ready` with `autoMode` set, and `autoImplFiredRef` (in-memory)
          // is empty — without this guard the effect would respawn the Queen
          // against a swarm the backend already marked `Interrupted` on exit.
          try {
            const sw = await ipc.getSwarm(swarmId);
            if (sw.status !== "planning") {
              if (sw.status === "implementing") {
                goRef.current?.("swarm-control", { swarm: { id: swarmId } });
              }
              updateTask(tid, (prev) => ({ ...prev, streaming: false }));
              return;
            }
          } catch (e) {
            console.warn("auto-launch swarm status check failed", e);
            autoImplFiredRef.current.delete(tid);
            return;
          }
          if (planSid) {
            try {
              await ipc.stopChat(planSid);
            } catch (_) {
              /* best-effort — planning session may already be done */
            }
          }
          try {
            await ipc.startSwarm(swarmId, swarmFeatures, swarmMilestones);
            updateTask(tid, (prev) => ({
              ...prev,
              streaming: false,
              messages: [
                ...prev.messages,
                {
                  who: "session-divider" as const,
                  dividerLabel: "Swarm launched — see Swarms › Open Control",
                  createdAt: Date.now(),
                },
              ],
            }));
            goRef.current?.("swarm-control", { swarm: { id: swarmId } });
          } catch (e) {
            const msg = String(e);
            // "already running" is not a failure here — auto-launch fired
            // against a swarm someone else already started (manual click,
            // earlier auto-launch, separate session). Treat as success: keep
            // the fire flag set so we don't loop, and navigate to control.
            if (/already running/i.test(msg)) {
              updateTask(tid, (prev) => ({ ...prev, streaming: false }));
              goRef.current?.("swarm-control", { swarm: { id: swarmId } });
            } else {
              console.error("auto-launch swarm failed", e);
              updateTask(tid, (prev) => ({
                ...prev,
                error: msg,
                streaming: false,
              }));
              autoImplFiredRef.current.delete(tid);
            }
          }
        })();
        continue;
      }

      autoImplFiredRef.current.add(t.taskId);

      const oldSid = t.sessionId;
      const newSid = crypto.randomUUID();
      sessionIdToTaskIdRef.current[newSid] = t.taskId;
      const dividerUsage = t.sessionUsage
        ? {
            input: t.sessionUsage.input,
            output: t.sessionUsage.output,
            contextPercent: t.sessionUsage.contextPercent,
            cost: t.sessionUsage.cost,
            tokPerSec: t.sessionUsage.tokPerSec,
          }
        : undefined;
      const planTextSnapshot = t.planText;
      const modelSnapshot = t.model;
      const thinkingSnapshot = t.thinking;
      const projectSnapshot = t.projectPath ?? undefined;

      updateTask(t.taskId, (prev) =>
        resetSessionStats({
          ...prev,
          sessionId: newSid,
          streaming: true,
          phase: "implement" as TaskPhase,
          messages: [
            ...prev.messages,
            { who: "session-divider", dividerSessionId: oldSid ?? undefined, dividerUsage, createdAt: Date.now() },
            {
              who: "session-divider",
              dividerLabel: "Implementation session started",
              dividerModel: prev.model,
              dividerThinking: prev.thinking,
              dividerSessionId: newSid,
              createdAt: Date.now(),
            },
          ],
        }),
      );
      if (oldSid) ipc.stopChat(oldSid).catch(() => {});
      ipc
        .sendMessage(
          buildImplementPrompt(planTextSnapshot),
          modelSnapshot,
          newSid,
          projectSnapshot,
          thinkingSnapshot,
          IMPL_SYSTEM_PROMPT,
          IMPL_TOOL_SET,
        )
        .catch((err) => {
          console.error("auto-implement failed", err);
          updateTask(t.taskId, (prev) => ({ ...prev, streaming: false, error: String(err) }));
        });
    }
  }, [tasks, updateTask]);

  /* ── Auto-commit on implement → implement-done transition ─── */
  const prevPhasesRef = useRef<Map<string, TaskPhase>>(new Map());
  useEffect(() => {
    if (!isTauri()) return;

    for (const t of Object.values(tasks)) {
      const prev = prevPhasesRef.current.get(t.taskId);
      prevPhasesRef.current.set(t.taskId, t.phase);

      // Only fire on actual transition: implement → implement-done
      if (prev === t.phase) continue;
      if (t.phase !== "implement-done") continue;
      if (prev !== "implement") continue;
      if (!t.projectPath) continue;

      const taskId = t.taskId;
      const projectPath = t.projectPath;
      // Title lives on TaskListItem (localTasks), not TaskRuntimeState
      const listItem = localTasks.find((lt) => lt.id === taskId);
      const taskTitle = listItem?.title || taskId;

      // Play completion sound if enabled
      const cs = getCompletionSoundConfig();
      if (cs.enabled) {
        playCompletionSound(cs.sound);
      }

      // Snapshot the per-project override synchronously at transition time.
      // Changing the override later will not affect this in-flight decision.
      const proj = projectsRef.current.find((p) => p.id === projectPath);
      const overrideAtTransition = proj?.autoCommitOverride ?? "inherit";

      (async () => {
        try {
          let shouldCommit: boolean;
          if (overrideAtTransition === "on") {
            shouldCommit = true;
          } else if (overrideAtTransition === "off") {
            shouldCommit = false;
          } else {
            // "inherit" — fall back to the global setting
            const settings = await ipc.getSettings();
            shouldCommit = settings.auto_commit_tasks;
          }
          if (!shouldCommit) return;

          const result = await ipc.autoCommitTask(projectPath, taskTitle);

          // Guard: task may have been deleted or phase changed while commit was in flight
          const currentTask = tasksRef.current[taskId];
          if (!currentTask) return;
          if (currentTask.phase !== "implement-done") return;

          const overrideTag =
            overrideAtTransition !== "inherit" ? " (project override)" : "";
          updateTask(taskId, (prev) => ({
            ...prev,
            messages: [
              ...prev.messages,
              {
                who: "session-divider" as const,
                dividerLabel: result.ok
                  ? `Auto-committed${overrideTag}: ${result.commit_hash || "done"}`
                  : `Auto-commit skipped: ${result.message}`,
                createdAt: Date.now(),
              },
            ],
          }));
        } catch (e) {
          console.error("Auto-commit failed:", e);
        }
      })();
    }

    // Clean up refs for deleted tasks
    for (const [id] of prevPhasesRef.current) {
      if (!tasks[id]) {
        prevPhasesRef.current.delete(id);
      }
    }
  }, [tasks, localTasks, updateTask]);

  /* ── Terminal cleanup: kill Pi sessions when a task hits implement-done ─ */
  /** Tracks tasks whose Pi sessions have already been force-killed by the
   *  implement-done transition effect. Consulted BEFORE mutating state so
   *  the in-effect `updateTask` call's re-render is a no-op on re-entry. */
  const terminatedTasksRef = useRef<Set<string>>(new Set());
  useEffect(() => {
    if (!isTauri()) return;
    for (const t of Object.values(tasks)) {
      if (t.phase !== "implement-done") continue;
      if (terminatedTasksRef.current.has(t.taskId)) continue;
      // Mark BEFORE any state-touching work so the re-render the
      // updateTask call below triggers is a no-op on re-entry.
      terminatedTasksRef.current.add(t.taskId);
      const sids = collectTaskSessionIds(t.taskId);
      for (const sid of sids) {
        ipc.killPiSession(sid).catch((e) => {
          console.warn("[task-done] killPiSession failed", sid, e);
        });
      }
      // Best-effort: clear any review-flow scaffolding so late chat-events
      // for this task can no longer rebind.
      const flow = reviewFlowsRef.current[t.taskId];
      if (flow?.contextWatchdog) {
        clearTimeout(flow.contextWatchdog);
        flow.contextWatchdog = null;
      }
      reviewFlowsRef.current[t.taskId] = null;
      delete reviewAccumulatorsRef.current[t.taskId];
      for (const sid of sids) {
        delete sessionIdToTaskIdRef.current[sid];
        internalSessionIdsRef.current.delete(sid);
        delete sessionIdToReviewIdRef.current[sid];
      }
      // Detach the dead session from the runtime state so the next message
      // (if the task is revived) doesn't try to resume against a dead pid.
      // This setState will re-run this effect — the guard above absorbs it.
      updateTask(t.taskId, (prev) =>
        prev.sessionId || prev.internalPi
          ? { ...prev, sessionId: null, internalPi: null }
          : prev,
      );
    }
    // Garbage-collect entries for tasks that no longer exist so the set
    // doesn't grow unbounded across the app lifetime.
    for (const tid of terminatedTasksRef.current) {
      if (!tasks[tid]) terminatedTasksRef.current.delete(tid);
    }
  }, [tasks, collectTaskSessionIds, updateTask]);

  /* ── streamingTaskIds (with 15s tail for sidebar dots) ─── */
  const baseStreamingTaskIds = useMemo(() => {
    const out: Record<string, boolean> = {};
    for (const t of Object.values(tasks)) if (t.streaming) out[t.taskId] = true;
    return out;
  }, [tasks]);

  const [tailStreamingIds, setTailStreamingIds] = useState<Record<string, boolean>>({});
  const tailTimersRef = useRef<Record<string, ReturnType<typeof setTimeout>>>({});
  const lastStreamingRef = useRef<Record<string, boolean>>({});
  const ACTIVE_TAIL_MS = 15_000;
  useEffect(() => {
    const prev = lastStreamingRef.current;
    const next = baseStreamingTaskIds;
    for (const tid of Object.keys(prev)) {
      if (prev[tid] && !next[tid]) {
        if (tailTimersRef.current[tid]) clearTimeout(tailTimersRef.current[tid]);
        setTailStreamingIds((s) => (s[tid] ? s : { ...s, [tid]: true }));
        tailTimersRef.current[tid] = setTimeout(() => {
          delete tailTimersRef.current[tid];
          setTailStreamingIds((s) => {
            if (!s[tid]) return s;
            const out = { ...s };
            delete out[tid];
            return out;
          });
        }, ACTIVE_TAIL_MS);
      }
    }
    lastStreamingRef.current = { ...next };
  }, [baseStreamingTaskIds]);

  const streamingTaskIds = useMemo(() => {
    return { ...tailStreamingIds, ...baseStreamingTaskIds };
  }, [baseStreamingTaskIds, tailStreamingIds]);

  /* ── awaitingInputTaskIds (per-task awaiting-input badge) ─
   *
   * Single-source-of-truth derivation for the sidebar's "this task
   * needs you" affordance. Covers three input-blocked states:
   *   - pendingQuestions card → "questions"
   *   - pendingSwarmQuestions modal → "swarm-questions"
   *   - phase === "plan-ready" && autoMode !== "full" (split by swarmId) →
   *     "swarm-plan-ready" / "plan-ready". autoMode === "review" still ends
   *     at plan-ready so the user clicks Implement themselves.
   *
   * Streaming wins: an actively-streaming task never appears here
   * even if it transiently has questions/plan-ready set. This is
   * intentionally NOT the tailed `streamingTaskIds` map — we want
   * the badge to flip on the instant the task settles. */
  const awaitingInputTaskIds = useMemo(() => {
    const out: Record<string, AwaitingInputKind> = {};
    for (const t of Object.values(tasks)) {
      if (t.streaming) continue;                              // active work wins
      if ((t.pendingQuestions?.length ?? 0) > 0) {
        out[t.taskId] = "questions";
      } else if ((t.pendingSwarmQuestions?.length ?? 0) > 0) {
        out[t.taskId] = "swarm-questions";
      } else if (
        t.phase === "plan-ready" &&
        t.autoMode !== "full" &&
        // reviewProgress non-null means a hivemind review is in flight —
        // don't flash "ready to implement" while the dock is still streaming
        // and the phase may rewind to "review".
        !t.reviewProgress &&
        // Suppress the badge while we're genuinely waiting on Queen to
        // re-emit features (the user has nothing to act on yet). On
        // failure (`featuresRefreshFailed: true`) the badge SHOULD show —
        // the user does need to act — so the negation falls through.
        !(t.pendingFeaturesRefresh && !t.featuresRefreshFailed)
      ) {
        // `swarmId` lives directly on TaskRuntimeState (mirrored from
        // TaskListItem at creation), so we can read it without consulting
        // the sidebar list — keeps the memo dep array tight.
        out[t.taskId] = t.swarmId ? "swarm-plan-ready" : "plan-ready";
      }
    }
    return out;
  }, [tasks]);

  /* ── Sliced API values (audit 6.7) ──────────────────────
   *
   * Each slice is memoized over only the inputs it actually consumes.
   * A keystroke routed through `setDraft` doesn't touch any of these
   * values (drafts live in refs); a streaming chunk lands in `tasks`
   * and only `runtimeStateApi` rebuilds; a new hivemind only rebuilds
   * `hivemindOptionsApi`; etc.
   *
   * The combined `TaskRuntimeApi` is preserved at the bottom as a
   * compatibility shim for the existing screens that still consume
   * the legacy `useTaskRuntime()` hook.
   */
  const draftApi = useMemo<TaskDraftApi>(
    () => ({ getDraft, setDraft }),
    [getDraft, setDraft],
  );

  const listApi = useMemo<TaskListApi>(
    () => ({
      localTasks,
      activeId,
      setActiveTask: setActiveId,
      setLocalTasks,
    }),
    [localTasks, activeId],
  );

  const runtimeStateApi = useMemo<TaskRuntimeStateApi>(
    () => ({
      tasks,
      streamingTaskIds,
      awaitingInputTaskIds,
      updateTask,
    }),
    [tasks, streamingTaskIds, awaitingInputTaskIds, updateTask],
  );

  const actionsApi = useMemo<TaskActionsApi>(
    () => ({
      createTask,
      submitMessage: submitMessageImpl,
      stopTask,
      deleteTask,
      triggerReviewForTask,
      retryReview,
      implementPlan,
      answerQuestions,
      submitSwarmAnswers,
      skipSwarmQuestions,
      resumeReview,
      replayReview,
      armFeaturesRefresh: armFeaturesRefreshAndDispatch,
    }),
    [
      createTask,
      submitMessageImpl,
      stopTask,
      deleteTask,
      triggerReviewForTask,
      retryReview,
      implementPlan,
      answerQuestions,
      submitSwarmAnswers,
      skipSwarmQuestions,
      resumeReview,
      replayReview,
      armFeaturesRefreshAndDispatch,
    ],
  );

  const hivemindOptionsApi = useMemo<HivemindOptionsApi>(
    () => ({ hivemindOptions, refreshHivemindOptions }),
    [hivemindOptions, refreshHivemindOptions],
  );

  const defaultsApi = useMemo<DefaultsApi>(
    () => ({
      defaultModel: defaultModelRef.current,
      defaultProjectPath: defaultProjectPathRef.current,
      defaultHivemind: defaultHivemindRef.current,
    }),
    // The refs are written directly by event handlers; the revision
    // counters force this memo to rebuild on every event landing.
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [defaultModelRevision, defaultProjectPathRevision, defaultHivemindRevision],
  );

  /* ── Legacy combined API (compatibility shim) ───────────
   *
   * Every existing call to `useTaskRuntime()` reads from this object.
   * It composes the six sliced values above into the same `TaskRuntimeApi`
   * shape so screens / tests / fixtures don't need to migrate in lockstep.
   * Identity matches the slices below, so it changes whenever any slice
   * changes — which is the legacy behavior. New code should prefer the
   * narrower hooks. */
  const api = useMemo<TaskRuntimeApi>(
    () => ({
      tasks: runtimeStateApi.tasks,
      localTasks: listApi.localTasks,
      activeId: listApi.activeId,
      hivemindOptions: hivemindOptionsApi.hivemindOptions,
      defaultModel: defaultsApi.defaultModel,
      defaultProjectPath: defaultsApi.defaultProjectPath,
      defaultHivemind: defaultsApi.defaultHivemind,
      streamingTaskIds: runtimeStateApi.streamingTaskIds,
      awaitingInputTaskIds: runtimeStateApi.awaitingInputTaskIds,
      setActiveTask: listApi.setActiveTask,
      refreshHivemindOptions: hivemindOptionsApi.refreshHivemindOptions,
      updateTask: runtimeStateApi.updateTask,
      setLocalTasks: listApi.setLocalTasks,
      getDraft: draftApi.getDraft,
      setDraft: draftApi.setDraft,
      createTask: actionsApi.createTask,
      submitMessage: actionsApi.submitMessage,
      stopTask: actionsApi.stopTask,
      deleteTask: actionsApi.deleteTask,
      triggerReviewForTask: actionsApi.triggerReviewForTask,
      retryReview: actionsApi.retryReview,
      implementPlan: actionsApi.implementPlan,
      answerQuestions: actionsApi.answerQuestions,
      submitSwarmAnswers: actionsApi.submitSwarmAnswers,
      skipSwarmQuestions: actionsApi.skipSwarmQuestions,
      resumeReview: actionsApi.resumeReview,
      replayReview: actionsApi.replayReview,
      armFeaturesRefresh: actionsApi.armFeaturesRefresh,
    }),
    [draftApi, listApi, runtimeStateApi, actionsApi, hivemindOptionsApi, defaultsApi],
  );

  return (
    <DefaultsContext.Provider value={defaultsApi}>
      <HivemindOptionsContext.Provider value={hivemindOptionsApi}>
        <TaskActionsContext.Provider value={actionsApi}>
          <TaskListContext.Provider value={listApi}>
            <TaskDraftContext.Provider value={draftApi}>
              <TaskRuntimeStateContext.Provider value={runtimeStateApi}>
                <TaskRuntimeContext.Provider value={api}>
                  {children}
                </TaskRuntimeContext.Provider>
              </TaskRuntimeStateContext.Provider>
            </TaskDraftContext.Provider>
          </TaskListContext.Provider>
        </TaskActionsContext.Provider>
      </HivemindOptionsContext.Provider>
    </DefaultsContext.Provider>
  );
}
