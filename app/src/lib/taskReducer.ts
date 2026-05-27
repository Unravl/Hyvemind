import type { ChatEvent, HivemindProgressEvent, ImageAttachment, InterruptedMergeState, QueueState, ResumableReviewSnapshot, ReviewInterruptedState, ReviewStateSnapshot, StepFull, ToolCallState } from "./types";
import type { ReviewProgress, RoundRunState, ModelRunState } from "./review-mode";
import { verdictsFromToolArgs } from "./review-mode";
import type { TaskQuestion } from "./questions";
import {
  featuresFromToolArgs,
  type MilestoneSpec,
  type ReadinessManifest,
  type SwarmFeatureSpec,
  type SwarmQuestion,
  type TaskMeta,
} from "./plan-mode";
import type { StreamAgent, StreamEntry } from "./streamEntry";

/* ── Public types ─────────────────────────────────────────── */

/** Auto-mode controls what happens after the planning agent emits a plan.
 *  - "off":    user must click Implement / Launch Swarm.
 *  - "full":   auto-trigger Hivemind review (if configured) AND auto-implement
 *              once the review completes (or immediately if no hivemind).
 *  - "review": auto-trigger Hivemind review (if configured), then STOP at
 *              plan-ready so the user can review and click Implement themselves. */
export type AutoMode = "off" | "full" | "review";

export function normalizeAutoMode(v: unknown): AutoMode {
  if (v === "full" || v === "review" || v === "off") return v;
  if (v === true) return "full";
  return "off";
}

export interface TaskMessage {
  who: "user" | "asst" | "questions" | "review" | "plan" | "complete" | "session-divider" | "nurse" | "error";
  t?: string;
  /** Epoch-ms timestamp of when this message was first constructed. Drives the
   *  live-updating relative-time label rendered by `<RelativeTime/>`. When set,
   *  takes priority over the legacy `t?: string` preformatted label. Older
   *  persisted messages on disk that predate this field fall back to `t`. */
  createdAt?: number;
  text?: string;
  model?: string;
  queen?: boolean;
  tools?: ToolCallState[];
  reasoning?: string;
  reasoningStartedAt?: number;
  reasoningDurationMs?: number;
  steered?: boolean;
  planText?: string;
  questions?: TaskQuestion[];
  /** Set on `who: "nurse"` messages. Correlates a streaming intervention
   *  across `started` → `reasoning` chunks → `completed` / `failed` so the
   *  reducer can append deltas to the same inline card rather than
   *  spawning a new message per event. */
  nurseInterventionId?: string;
  /** Action level driving the Nurse card's icon/colour: steer, restart,
   *  cancel, leave_it, diagnose. */
  nurseLevel?: string;
  /** First-person summary of what the Nurse spotted. Renders as a bold
   *  one-liner: "I have spotted: …". */
  nurseObservation?: string;
  /** First-person summary of what the Nurse will do. Renders as the second
   *  line: "I'll …". */
  nurseAction?: string;
  /** Streaming rationale, appended one chunk at a time. Renders as a
   *  collapsible reasoning block. */
  nurseReasoning?: string;
  /** Lifecycle status. Drives the badge (Working / Resolved / Failed) and
   *  the pulse animation on the inline card. */
  nurseStatus?: "started" | "reasoning" | "completed" | "failed";
  /** Populated when `nurseStatus === "failed"`. */
  nurseError?: string;
  dividerSessionId?: string;
  dividerLabel?: string;
  /** For session dividers: the human-readable configuration name (e.g. hivemind name "enhance")
   *  or the actual LLM model name for non-hivemind dividers.
   *  Context-determined; check `dividerLabel` to disambiguate. */
  dividerModel?: string;
  /** For session dividers: the actual LLM model name driving the session.
   *  Set on the hivemind review divider to distinguish the orchestrator model
   *  from the hivemind configuration name stored in `dividerModel`. */
  dividerAgentModel?: string;
  dividerThinking?: string;
  dividerUsage?: { input: number; output: number; contextPercent: number; cost: number; tokPerSec?: number };
  images?: ImageAttachment[];
  /** When set, this `who: "asst"` message originated from a Hivemind internal
   *  Pi session (context-gather or per-round merge). Drives an inline-card
   *  wrapper in the message renderer and prevents the message from being
   *  interpreted as plan/intake content. `sessionId` is used by the live
   *  streaming router to bind incoming chunks/thinking/tool events to the
   *  right message instance. */
  reviewKind?: { phase: "context" | "merge"; round?: number; reviewId: string; sessionId: string };
  /** Inline error marker for an internal Pi (context/merge) message that
   *  failed mid-flight. The parent task `error` banner is the primary
   *  surface; this preserves the failure on the specific message even
   *  after the banner is dismissed. */
  error?: string;
  /** Set on `who: "error"` messages. Carries the failure text rendered as
   *  a persistent inline error bubble in the conversation. The top-of-
   *  conversation banner (`state.error`) is wiped on the next send;
   *  this message is not, so the user can scroll back and see what
   *  failed in past turns. */
  errorMessage?: string;
  /** Compound dedup key for Hivemind model-failure error bubbles.
   *  Format: `${jobId}::${round}::${modelId}::${modelIdx}`.
   *  Only set when `who === "error"` and the error originated from `hm_model_failed`. */
  hivemindFailureKey?: string;
  /** Parsed Swarm features from the Queen plan block. Populated on `plan`
   *  messages during streaming/done and used to survive serialization so
   *  Launch Swarm remains active across tab switches and app restarts. */
  features?: SwarmFeatureSpec[];
  /** Per-reviewer verdicts attached to a Hivemind merge bubble. Sourced from
   *  the merge orchestrator's `submit_verdicts` tool call. Only populated on
   *  the merge `who: "asst"` message whose `reviewKind.phase === "merge"`. */
  verdicts?: import("./review-mode").ParsedVerdict[] | null;
  /** Outcome flag on `who: "complete"` messages produced by the Tasks-view
   *  implementation agent's `submit_task_complete` tool call. The optional
   *  `text` field on the same message stores the human-readable summary. */
  successState?: "success" | "partial" | "failure";
}

export type TaskPhase =
  | "intake"
  | "questions"
  | "plan"
  | "plan-ready"
  | "review"
  | "implement"
  | "implement-done";

export const PHASE_RANK: Record<TaskPhase, number> = {
  intake: 0,
  questions: 1,
  plan: 2,
  "plan-ready": 3,
  review: 4,
  implement: 5,
  "implement-done": 6,
};

export interface SessionUsage {
  input: number;
  output: number;
  cacheRead: number;
  cost: number;
  contextTokens: number;
  contextWindow: number;
  contextPercent: number;
  tokPerSec: number;
}

/** Tracks the currently-active internal Pi session during a Hivemind review —
 *  either the context-gather Pi (start of review) or the merge Pi (between
 *  rounds). Drives the telemetry strip in the HivemindReviewBar. Overwritten
 *  on each new internal-session spawn (the "reset when a new one starts"
 *  behavior). Token/context/TPS metrics flow through the existing
 *  `liveTps` and `sessionUsage` fields, which are pointed at the internal
 *  session because the task's `sessionId` is already set to it during these
 *  phases. */
export interface InternalPiState {
  kind: "context" | "merge";
  sessionId: string;
  modelName: string;
  status: "running" | "done";
}

/** Single source of truth for one task's runtime state. The component renders
 *  `tasks[activeId]` directly; events update `tasks[taskId]` regardless of
 *  whether that task is currently visible. */
export interface TaskRuntimeState {
  taskId: string;
  sessionId: string | null;
  messages: TaskMessage[];
  streaming: boolean;
  /** Latched `true` by `structured_task_complete`; cleared by `stream_start`
   *  (new turn), `done`, `error`, or `stop` (terminal boundaries). Suppresses
   *  trailing same-turn events (`tool_start`, `chunk`, `thinking`, `retrying`)
   *  from re-enabling the spinner after the agent has signalled completion.
   *  In-memory only — not persisted to `~/.hyvemind/task-messages/`. */
  currentTurnComplete: boolean;
  /** Latched true by `stopTask` and cleared on the next `submitMessage`.
   *  The next message after an explicit Stop is treated as a steer: the
   *  user-visible "steered" badge is applied, and the backend prepends an
   *  interruption preamble before handing the prompt to Pi. Survives Pi
   *  process churn (the flag lives on the frontend task, not on the Pi
   *  session), so it remains correct even when the session has been
   *  evicted between stop and resume. */
  pendingSteerAfterStop?: boolean;
  error: string | null;
  model: string;
  hivemind: string | null;
  thinking: string;
  phase: TaskPhase;
  planText: string | null;
  pendingQuestions: TaskQuestion[] | null;
  /** Index of the currently-displayed question in the QuestionsDock (0-based).
   *  Persisted across task switches so the user's place is preserved. Reset
   *  to 0 when a new questions batch arrives (component detects via
   *  questionsKey change). */
  pendingQuestionIdx: number;
  /** Partial answers collected so far in the QuestionsDock, keyed by
   *  question id. Persisted across task switches so a user can answer
   *  1 of 3 questions, switch tasks, and return without losing progress.
   *  Cleared when the questions are submitted. */
  pendingQuestionAnswers: Record<string, string>;
  /** Stable key derived from the current question batch's question ids
   *  (e.g. `"q1|q2|q3"`). Used to detect whether a new `structured_questions`
   *  event represents a genuinely different batch (reset progress) or an
   *  identical re-ask (preserve progress). */
  questionsKey: string | null;
  reviewProgress: ReviewProgress | null;
  /** Best-known context-window size (tokens) for the active model. Set by
   *  the UI when the user picks a model whose metadata is known (from
   *  ModelBrowser's catalog/detail merge). Used as a fallback for the
   *  bottom-of-Tasks-view meter when Pi reports `contextWindow === 0`
   *  (typical for custom OpenAI-compatible models with non-standard names).
   *  Pi's value remains authoritative whenever it is non-zero. */
  contextWindowHint?: number;
  /** Job ID of the currently-active embedded hivemind review (if any).
   *  Used to route `hivemind-progress` events to the correct task and to
   *  trigger focus-resync. Cleared on `hm_failed` and on review-flow finish. */
  activeReviewJobId: string | null;
  /** Live TPS estimate during streaming. Updated by `tps_update` events.
   *  Reset to null when streaming ends or the final `usage` event arrives. */
  liveTps: number | null;
  sessionUsage: SessionUsage | null;
  queueState: QueueState | null;
  autoMode: AutoMode;
  reviewCompleted: boolean;
  projectPath: string | null;
  /** Set when reconciliation discovers a previously-interrupted merge run for
   *  this task's review job. Cleared when the user starts a resume (or when
   *  the merge re-completes via normal flow). The UI surfaces a "Resume merge"
   *  affordance based on this state. Kept as a shim derived from
   *  `reviewInterrupted` when phase === "merge" so legacy consumers continue
   *  to work. */
  mergeInterrupted: InterruptedMergeState | null;
  /** Generalised resumable-review state covering all five phases (context,
   *  round, merge, between_rounds, final). Populated by the
   *  `review_interrupted` reducer case (driven by either a startup event or
   *  the `get_resumable_review_for_task` reconcile probe). Cleared by
   *  `review_resume_started`. The UI surfaces a phase-aware "Resume" banner
   *  based on this state. */
  reviewInterrupted: ReviewInterruptedState | null;
  /** Telemetry slot for the active context-gather or merge Pi during a
   *  Hivemind review. Cleared when the review ends or fails. */
  internalPi: InternalPiState | null;
  /** Parsed TASK_META emitted by the planning agent on its first text response.
   *  Used by the runtime to populate sidebar title/description. One-shot:
   *  set once on first valid block, then persists across all reducer branches
   *  (every other branch spreads `prev`). Not persisted to localStorage — the
   *  primary persistence is `titleEdited` on `TaskListItem`. Defense-in-depth
   *  resync extraction handles the crash-before-save case. */
  taskMeta?: TaskMeta | null;
  /** Live Pi session lifecycle indicator for an in-flight prompt. Surfaces
   *  the gap between "user pressed Send" and "first token visible" — most
   *  noticeable on large prompts or thinking-heavy models. Reset to `null`
   *  on `stream_start`, `done`, `error`, `stop`. The UI hides the label
   *  once `hasFirstStream` latches to `true` (first chunk/thinking event),
   *  yielding the screen to the normal streaming/reasoning indicators. */
  streamPhase: {
    /** Human-readable label derived from `rawPhase` via `phaseLabel()`. */
    label: string;
    /** Raw backend phase identifier (e.g. `"awaiting_model"`). */
    rawPhase: string;
    /** ms since the user pressed Send. Bumped by `heartbeat` events. */
    elapsedMs: number;
    /** ms since the last PiEvent. Bumped by `heartbeat` events. */
    silentMs: number;
    /** Number of tokens currently in Pi's context window for this turn.
     *  Populated by the `context_loaded` event after the first non-zero
     *  usage tick proves the prompt has been ingested. */
    contextTokens: number | null;
    /** Latched `true` once any visible token (chunk or thinking) arrives.
     *  Suppresses further phase label updates so the in-progress message
     *  body is the user's focus. */
    hasFirstStream: boolean;
  } | null;
  /** Set while Pi is auto-retrying a transient provider error (e.g.
   *  Anthropic `overloaded_error`). Surfaces a "Server overloaded — retrying
   *  in Xs (attempt N/M)…" banner in the Tasks chat surface. Cleared on
   *  `retry_resumed`, `done`, `error`, or `stop`. */
  retryStatus: {
    attempt: number;
    maxAttempts: number;
    delayMs: number;
    summary: string;
    startedAt: number;
  } | null;
  /** Backend swarm id this task is the planning conversation for, or null
   *  for ordinary tasks. Set on creation by NewSwarm/Swarms via
   *  `createTask({ swarmId })`. When set, `submitMessage` uses
   *  QUEEN_PLANNING_SYSTEM_PROMPT in the plan phase, and the PlanCard
   *  shows "Launch Swarm" in plan-ready. Mirrored on TaskListItem so the
   *  sidebar can resolve the linkage without consulting taskRuntime. */
  swarmId?: string | null;
  /** Parsed swarm features submitted by the Queen Planning agent via its
   *  `submit_features` tool call. Lives outside `planText` because the plan
   *  body and the features payload arrive on independent chat-events. Drives
   *  the "Launch Swarm" CTA in Tasks.tsx for swarm-linked tasks. */
  swarmFeatures?: SwarmFeatureSpec[] | null;
  /** Parsed milestones from the same FEATURES JSON block. Always present
   *  alongside `swarmFeatures` (may be `[]` for legacy bare-array payloads
   *  or plans with fewer than 4 features). Passed through to `start_swarm`
   *  so the backend can persist per-milestone assertions and Guard can
   *  validate against them when a milestone completes. */
  swarmMilestones?: MilestoneSpec[] | null;
  /** Parse error from the FEATURES block, when the model emitted both
   *  delimiters but the body was unparseable even after heuristic repair.
   *  Surfaced in the disabled-reason tooltip so the user sees *why* the
   *  Launch Swarm button is greyed out. Cleared once a parse succeeds. */
  swarmFeaturesError?: string | null;
  /** True between the moment a features-refresh turn starts and the moment
   *  it ends. Set by exactly TWO writers, both routed through the shared
   *  `armFeaturesRefreshAndDispatch` helper in `taskRuntime.tsx`:
   *    1. `finishReviewFlow` — immediately after a Hivemind review of a
   *       swarm-planning task completes, when we ask Queen to re-emit
   *       `submit_plan` and `submit_features` against the refined plan.
   *    2. `handleRequestFeatures` — the user-initiated "Re-emit FEATURES"
   *       retry button.
   *
   *  Cleared by the next terminal event of the in-flight Queen turn
   *  (`done` / `error` / `stop`) AND by the `structured_features` clause
   *  when refined features land. If the terminal event arrives while this
   *  flag is still true, the reducer interprets that as "features didn't
   *  land" and trips `featuresRefreshFailed`. A 3-min watchdog in
   *  `taskRuntime.tsx` is the last-resort safety net for Pi subprocesses
   *  that never produce a terminal event. Any new writer MUST go through
   *  `armFeaturesRefreshAndDispatch` so the watchdog and dispatch-failure
   *  semantics stay in sync. */
  pendingFeaturesRefresh?: boolean;
  /** Set true when a features-refresh turn (either the post-Hivemind
   *  `[HivemindReview]` follow-up dispatched by `finishReviewFlow`, or a
   *  user-initiated Re-emit FEATURES retry dispatched by
   *  `handleRequestFeatures`) ended without Queen calling
   *  `submit_features`, or when the 3-min watchdog / follow-up dispatch
   *  failed. Surfaces a recovery banner in the PlanCard footer so the user
   *  can launch with whatever feature set is present, or click Re-emit.
   *  **Persists across unrelated subsequent turns** — only cleared by a
   *  successful `structured_features` event or by a fresh refresh attempt
   *  that re-sets `pendingFeaturesRefresh: true` and then succeeds. */
  featuresRefreshFailed?: boolean;
  /** Phase 4C — every unanswered ``swarm-question`` block extracted from
   *  the assistant transcript so far. The Tasks view renders these in a
   *  blocking modal until the user submits answers. Re-derived on every
   *  chunk/done event so a streaming Queen-planning agent can populate the
   *  modal incrementally; suppressed for any question id that already
   *  appears in `answeredSwarmQuestionIds`. `null` (rather than `[]`) means
   *  "no questions outstanding" — the modal is only shown when the array
   *  is non-empty. */
  pendingSwarmQuestions?: SwarmQuestion[] | null;
  /** Question ids the user has already answered (or explicitly skipped).
   *  Persists for the lifetime of the task so a re-derivation of pending
   *  questions on the next chunk doesn't re-pop the modal with questions
   *  that were already submitted. New ids in a fresh swarm-question block
   *  emitted later in the conversation are naturally unanswered, so the
   *  modal will re-open with just those. */
  answeredSwarmQuestionIds?: string[];
}

export type TaskEvent =
  | { kind: "stream_start" }
  | { kind: "chunk"; content: string }
  | { kind: "thinking"; content: string }
  | { kind: "tool_start"; data: { tool_call_id: string; name: string } }
  | { kind: "tool_update"; data: { tool_call_id: string; output: string } }
  | { kind: "tool_end"; data: { tool_call_id: string; result?: unknown } }
  | { kind: "tps_update"; tps: number }
  | { kind: "usage"; usage: SessionUsage }
  | { kind: "queue_update"; queue: QueueState }
  | { kind: "done" }
  | { kind: "error"; message: string }
  | { kind: "stop" }
  | { kind: "resync"; messages?: TaskMessage[]; sessionAlive?: boolean }
  | { kind: "review_start"; jobId: string; round: number; totalRounds: number; models: string[]; reviewId: string }
  | { kind: "hm_started"; jobId: string }
  | { kind: "hm_round_started"; jobId: string; round: number; models?: string[] }
  | { kind: "hm_model_completed"; jobId: string; modelId: string; round: number; inputTokens?: number; outputTokens?: number; durationMs?: number; cost?: number }
  | { kind: "hm_model_failed"; jobId: string; modelId: string; modelIdx?: number; round: number; error: string }
  | { kind: "hm_round_completed"; jobId: string; round: number }
  | { kind: "hm_completed"; jobId: string }
  | { kind: "hm_failed"; jobId: string; message: string }
  | { kind: "review_resync"; snapshot: ReviewStateSnapshot }
  | { kind: "review_error"; error: string }
  | { kind: "merge_interrupted"; jobId: string; round: number; outputLen: number; message: string }
  | { kind: "merge_resume_started" }
  | { kind: "review_interrupted"; snapshot: ResumableReviewSnapshot }
  | { kind: "review_resume_started" }
  | { kind: "internal_pi_started"; sessionId: string; modelName: string; piKind: "context" | "merge" }
  | { kind: "internal_pi_tps"; sessionId: string; tps: number }
  | { kind: "internal_pi_done"; sessionId: string }
  | { kind: "internal_pi_failed"; sessionId: string; message: string }
  // Inline-display events for internal context/merge Pi sessions. The router
  // dispatches these in *addition* to the existing accumulator/log paths so
  // the user can see the Pi's thinking, tool calls, and streamed text in the
  // main chat area while the review is mid-flight. Each event binds to the
  // TaskMessage whose `reviewKind.sessionId` matches `sessionId`.
  | { kind: "internal_pi_message_start"; sessionId: string; reviewKind: { phase: "context" | "merge"; round?: number; reviewId: string }; modelName: string }
  | { kind: "internal_pi_chunk"; sessionId: string; content: string }
  | { kind: "internal_pi_thinking"; sessionId: string; content: string }
  | { kind: "internal_pi_tool_start"; sessionId: string; data: { tool_call_id: string; name: string } }
  | { kind: "internal_pi_tool_update"; sessionId: string; data: { tool_call_id: string; output: string } }
  | { kind: "internal_pi_tool_end"; sessionId: string; data: { tool_call_id: string; result?: unknown } }
  // Nurse intervention lifecycle. The router emits these into the
  // currently-active task's reducer when the backend `nurse-event` channel
  // fires a `Lifecycle` variant for a session belonging to this task.
  // Each event is keyed by `interventionId` so streaming reasoning chunks
  // append to the same inline `who: "nurse"` message.
  | { kind: "nurse_started"; interventionId: string; level: string; observation: string; action: string; sessionId: string; t?: string }
  | { kind: "nurse_reasoning"; interventionId: string; delta: string }
  | { kind: "nurse_completed"; interventionId: string; fullReasoning?: string }
  | { kind: "nurse_failed"; interventionId: string; error?: string }
  // Pi session lifecycle events that surface the agent's pre-streaming
  // phase to the UI. `phase` is the discrete transition; `heartbeat` is
  // the periodic timer update while a phase is active; `context_loaded`
  // is the one-shot signal that the prompt has been ingested.
  | { kind: "phase"; rawPhase: string }
  | { kind: "heartbeat"; phase: string; elapsedMs: number; silentMs: number }
  | { kind: "context_loaded"; contextTokens: number }
  | {
      kind: "retrying";
      attempt: number;
      maxAttempts: number;
      delayMs: number;
      summary: string;
    }
  | { kind: "retry_resumed"; attempt: number; success: boolean }
  // Structured-output planning events. The backend forwards these when the
  // planning agent calls the matching `submit_*` extension tool (see
  // `commands/chat.rs::structured_tool_event_type`). The reducer inserts the
  // parsed payload directly into the message stream; this is the only path —
  // there is no text-scanning fallback.
  | { kind: "structured_task_meta"; meta: { title: string; description: string } }
  | { kind: "structured_questions"; questions: TaskQuestion[] }
  | { kind: "structured_plan"; planText: string }
  | {
      kind: "structured_features";
      features: SwarmFeatureSpec[];
      milestones: MilestoneSpec[];
      infrastructure?: string;
      agentsMd?: string;
      readinessManifest?: ReadinessManifest;
    }
  | { kind: "structured_verdicts"; verdicts: import("./review-mode").ParsedVerdict[]; sessionId?: string | null; round?: number | null }
  | { kind: "structured_stability_impl_complete"; sessionId?: string | null }
  | { kind: "structured_task_complete"; summary?: string; successState?: "success" | "partial" | "failure" };

/** Build a `ReviewInterruptedState` from a backend `ResumableReviewSnapshot`.
 *  Centralised so both the reducer `review_interrupted` case and the runtime
 *  retry path (which feeds the snapshot directly into `resumeReview`) produce
 *  identical UI state. */
export function reviewInterruptedFromSnapshot(
  s: ResumableReviewSnapshot,
): ReviewInterruptedState {
  return {
    phase: s.phase,
    reviewId: s.reviewId,
    jobId: s.latestJobId,
    round: s.round,
    totalRounds: s.totalRounds,
    planText: s.planText,
    models: s.models,
    completedStepOutputs: s.completedStepOutputs,
    mergeOutput: s.mergeOutput,
    message: s.message,
  };
}

/** Clear the per-Pi-session token/context counters. Use whenever a new Pi
 *  session boundary is crossed (phase transition, fresh task, review→merge,
 *  review→done) so the bottom bar starts at zero for the new active agent
 *  instead of carrying stale numbers from the prior session. */
export function resetSessionStats<T extends Pick<TaskRuntimeState, "sessionUsage" | "liveTps">>(
  s: T,
): T {
  return { ...s, sessionUsage: null, liveTps: null };
}

/** Infer the pretty-pill agent role for a Tasks-view session divider from its
 *  human-readable label. Returns `undefined` for labels that don't correspond
 *  to a known session type (e.g. "Swarm launched" or auto-commit summaries) —
 *  those continue to render as the understated plain-line marker. */
function inferAgentFromLabel(label: string | undefined): StreamAgent | undefined {
  if (!label) return undefined;
  if (
    label === "Planning session started" ||
    label === "Swarm planning session started"
  ) {
    return "planning";
  }
  if (label === "Implementation session started") return "implementation";
  if (label === "Hivemind context complete") return "hivemind-context";
  if (label.startsWith("Hivemind review")) return "hivemind-merge";
  return undefined;
}

/** Adapter that maps the internal `TaskMessage[]` reducer state into the
 *  shared `StreamEntry[]` shape consumed by `<ActivityStream/>`. Pure
 *  render-time transformation: the on-disk format remains `TaskMessage[]`. */
export function toStreamEntries(messages: TaskMessage[]): StreamEntry[] {
  // Pre-scan: build a map from sessionId → inferred agent so end-dividers
  // (which carry no label, only `dividerSessionId` + `dividerUsage`) can
  // inherit the agent role from their matching start divider. This keeps
  // the pretty pill colour-stable across the start → end boundary of a
  // single session.
  const sessionAgent = new Map<string, StreamAgent>();
  for (const m of messages) {
    if (m.who !== "session-divider") continue;
    if (!m.dividerSessionId) continue;
    if (typeof m.dividerLabel !== "string" || m.dividerLabel.length === 0) continue;
    const agent = inferAgentFromLabel(m.dividerLabel);
    if (agent) sessionAgent.set(m.dividerSessionId, agent);
  }

  const out: StreamEntry[] = [];
  for (let i = 0; i < messages.length; i++) {
    const m = messages[i];
    const id = `msg-${i}`;
    if (m.who === "user") {
      out.push({
        kind: "chat_bubble",
        surface: "task",
        who: "user",
        id,
        text: m.text ?? "",
        images: m.images,
        t: m.t,
        createdAt: m.createdAt,
        steered: m.steered,
      });
      continue;
    }
    if (m.who === "asst") {
      out.push({
        kind: "chat_bubble",
        surface: "task",
        who: "asst",
        id,
        text: m.text ?? "",
        model: m.model,
        reasoning: m.reasoning,
        reasoningStartedAt: m.reasoningStartedAt,
        reasoningDurationMs: m.reasoningDurationMs,
        tools: m.tools,
        images: m.images,
        reviewKind: m.reviewKind,
        error: m.error,
        t: m.t,
        createdAt: m.createdAt,
        sessionId: m.reviewKind?.sessionId,
        steered: m.steered,
        verdicts: m.verdicts,
      });
      continue;
    }
    if (m.who === "plan") {
      out.push({
        kind: "plan",
        surface: "task",
        id,
        planText: m.planText ?? "",
        features: m.features,
        t: m.t,
        createdAt: m.createdAt,
      });
      continue;
    }
    if (m.who === "questions") {
      out.push({
        kind: "questions",
        surface: "task",
        id,
        questions: m.questions ?? [],
        t: m.t,
        createdAt: m.createdAt,
      });
      continue;
    }
    if (m.who === "complete") {
      out.push({
        kind: "complete",
        surface: "task",
        id,
        t: m.t,
        createdAt: m.createdAt,
        text: m.text,
        successState: m.successState,
      });
      continue;
    }
    if (m.who === "nurse") {
      out.push({
        kind: "nurse",
        surface: "task",
        id,
        interventionId: m.nurseInterventionId || id,
        level: m.nurseLevel || "steer",
        observation: m.nurseObservation || "",
        action: m.nurseAction || "",
        reasoning: m.nurseReasoning,
        status: m.nurseStatus || "started",
        error: m.nurseError,
        t: m.t,
        createdAt: m.createdAt,
      });
      continue;
    }
    if (m.who === "session-divider") {
      const hasLabel = typeof m.dividerLabel === "string" && m.dividerLabel.length > 0;
      if (hasLabel) {
        // Special-case: "Hivemind context complete" semantically marks the
        // *end* of the context Pi session (not the start of something new),
        // so emit it as phase="end" with success=true. The pretty-pill
        // renderer will then show "ended ✓" instead of a perpetually-
        // running pill.
        if (m.dividerLabel === "Hivemind context complete") {
          out.push({
            kind: "session_marker",
            surface: "task",
            phase: "end",
            id,
            label: m.dividerLabel,
            agent: "hivemind-context",
            success: true,
            sessionId: m.dividerSessionId,
            t: m.t,
            createdAt: m.createdAt,
          });
          continue;
        }
        out.push({
          kind: "session_marker",
          surface: "task",
          phase: "start",
          id,
          label: m.dividerLabel ?? "",
          sessionId: m.dividerSessionId,
          model: m.dividerModel,
          agentModel: m.dividerAgentModel,
          thinking: m.dividerThinking,
          agent: inferAgentFromLabel(m.dividerLabel),
          t: m.t,
          createdAt: m.createdAt,
        });
      } else if (m.dividerUsage) {
        const inheritedAgent = m.dividerSessionId
          ? sessionAgent.get(m.dividerSessionId)
          : undefined;
        out.push({
          kind: "session_marker",
          surface: "task",
          phase: "end",
          id,
          label: "",
          sessionId: m.dividerSessionId,
          model: m.dividerModel,
          agentModel: m.dividerAgentModel,
          thinking: m.dividerThinking,
          usage: m.dividerUsage,
          agent: inheritedAgent,
          success: true,
          t: m.t,
          createdAt: m.createdAt,
        });
      }
      continue;
    }
    if (m.who === "error") {
      out.push({
        kind: "error",
        surface: "task",
        id,
        message: m.errorMessage ?? m.text ?? "",
        t: m.t,
        createdAt: m.createdAt,
      });
      continue;
    }
    // `who: "review"` is intentionally skipped (legacy no-op).
  }
  return out;
}

export function makeInitialTaskState(taskId: string, model: string): TaskRuntimeState {
  return {
    taskId,
    sessionId: null,
    messages: [],
    streaming: false,
    currentTurnComplete: false,
    error: null,
    model,
    hivemind: null,
    thinking: "high",
    phase: "intake",
    planText: null,
    pendingQuestions: null,
    pendingQuestionIdx: 0,
    pendingQuestionAnswers: {},
    questionsKey: null,
    reviewProgress: null,
    contextWindowHint: undefined,
    activeReviewJobId: null,
    sessionUsage: null,
    liveTps: null,
    queueState: null,
    autoMode: "off",
    reviewCompleted: false,
    projectPath: null,
    mergeInterrupted: null,
    reviewInterrupted: null,
    internalPi: null,
    taskMeta: null,
    streamPhase: null,
    retryStatus: null,
    swarmId: null,
    swarmFeatures: null,
    swarmMilestones: null,
    swarmFeaturesError: null,
    pendingFeaturesRefresh: false,
    featuresRefreshFailed: false,
    pendingSwarmQuestions: null,
    answeredSwarmQuestionIds: [],
  };
}

/* ── Pure message updaters ───────────────────────────────── */

/** Append a streaming chunk to the rolling assistant bubble, or start a new
 *  one if the last message isn't a contiguous text bubble. Structured planning
 *  output (plan/questions/features/task-meta/verdicts) arrives via the
 *  dedicated `structured_*` chat-events, not by scanning this text. */
export function processChunkEvent(prev: TaskMessage[], content: string, model: string): TaskMessage[] {
  const last = prev[prev.length - 1];
  if (last && last.who === "asst" && !(last.tools && last.tools.length > 0)) {
    return [...prev.slice(0, -1), { ...last, text: (last.text || "") + content }];
  }
  return [...prev, { who: "asst", text: content, model, createdAt: Date.now() }];
}

export function processThinkingEvent(prev: TaskMessage[], content: string, model: string): TaskMessage[] {
  const last = prev[prev.length - 1];
  // Split rule: a new bubble is allocated only when the previous turn is over.
  // Two signals end a turn:
  //   1. `tools` were recorded on the bubble — the model emitted a tool call,
  //      which mirrors the rule processChunkEvent already uses for text. The
  //      next reasoning chunk belongs to a new turn.
  //   2. `reasoningDurationMs` is set — finalizeReasoningDuration fired on the
  //      `done` event, finalising the turn explicitly.
  // Text presence is NOT a turn boundary: extended-thinking models (Claude
  // with thinking enabled, DeepSeek R1, etc.) interleave thinking and text
  // inside a single response, so a `thinking → text → thinking` sequence must
  // accumulate into the *same* bubble — otherwise the visible answer gets
  // sliced in half across two bubbles. Interleave order between text and
  // reasoning is collapsed at render time (the violet ReasoningBlock renders
  // above the text bubble regardless of when each chunk streamed); preserving
  // exact order would require an ordered-blocks data model.
  if (
    last &&
    last.who === "asst" &&
    !(last.tools && last.tools.length > 0) &&
    last.reasoningDurationMs == null
  ) {
    return [
      ...prev.slice(0, -1),
      {
        ...last,
        reasoning: (last.reasoning || "") + content,
        // Preserve the original start so the live timer measures the whole
        // turn (start of first thinking burst → done), not just the most
        // recent burst.
        reasoningStartedAt: last.reasoningStartedAt ?? Date.now(),
      },
    ];
  }
  return [...prev, { who: "asst", reasoning: content, model, reasoningStartedAt: Date.now(), createdAt: Date.now() }];
}

export function processToolStartEvent(prev: TaskMessage[], data: { tool_call_id: string; name: string }, model: string): TaskMessage[] {
  const last = prev[prev.length - 1];
  const tool: ToolCallState = { tool_call_id: data.tool_call_id, name: data.name, output: "", done: false };
  if (last && last.who === "asst") {
    return [...prev.slice(0, -1), { ...last, tools: [...(last.tools || []), tool] }];
  }
  return [...prev, { who: "asst", text: "", model, tools: [tool], createdAt: Date.now() }];
}

export function processToolUpdateEvent(prev: TaskMessage[], data: { tool_call_id: string; output: string }): TaskMessage[] {
  const last = prev[prev.length - 1];
  if (last && last.who === "asst" && last.tools) {
    const tools = last.tools.map((t) =>
      t.tool_call_id === data.tool_call_id ? { ...t, output: t.output + data.output } : t,
    );
    return [...prev.slice(0, -1), { ...last, tools }];
  }
  return prev;
}

/** Extract human-readable text from a Pi tool result.
 *
 *  Pi wraps tool output in `{ content: [{ text: "..." }, ...] }`.
 *  When `tool_execution_update` events were received the accumulated
 *  `t.output` is used instead, but for tools that return results all
 *  at once (e.g. `read`) we only get the structured `result` object
 *  and need to pull the text out of it. */
function extractResultText(result: unknown): string {
  if (result == null) return "";
  if (typeof result === "string") return result;
  if (typeof result === "object" && !Array.isArray(result)) {
    const obj = result as Record<string, unknown>;
    if (Array.isArray(obj.content)) {
      const texts = (obj.content as Array<Record<string, unknown>>)
        .filter((c) => typeof c.text === "string")
        .map((c) => c.text as string);
      if (texts.length > 0) return texts.join("\n");
    }
  }
  return JSON.stringify(result);
}

export function processToolEndEvent(prev: TaskMessage[], data: { tool_call_id: string; result?: unknown }): TaskMessage[] {
  const last = prev[prev.length - 1];
  if (last && last.who === "asst" && last.tools) {
    const tools = last.tools.map((t) =>
      t.tool_call_id === data.tool_call_id
        ? { ...t, done: true, output: t.output || extractResultText(data.result) }
        : t,
    );
    return [...prev.slice(0, -1), { ...last, tools }];
  }
  return prev;
}

/* ── Internal Pi (Hivemind context/merge) message helpers ──
 *
 * These mirror processChunk/Thinking/Tool* but bind by `reviewKind.sessionId`
 * instead of "last asst message", and skip plan/questions delimiter parsing
 * (an internal Pi's text is intermediate, not user-facing plan content). */

function findInternalPiMessageIdx(messages: TaskMessage[], sessionId: string): number {
  for (let i = messages.length - 1; i >= 0; i--) {
    const m = messages[i];
    if (m.who === "asst" && m.reviewKind?.sessionId === sessionId) return i;
  }
  return -1;
}

export function processInternalPiMessageStart(
  prev: TaskMessage[],
  sessionId: string,
  reviewKind: { phase: "context" | "merge"; round?: number; reviewId: string },
  modelName: string,
): TaskMessage[] {
  // Idempotent: don't push a duplicate if the message already exists.
  if (findInternalPiMessageIdx(prev, sessionId) !== -1) return prev;
  return [
    ...prev,
    {
      who: "asst",
      model: modelName,
      reviewKind: { ...reviewKind, sessionId },
      createdAt: Date.now(),
    },
  ];
}

export function processInternalPiChunk(prev: TaskMessage[], sessionId: string, content: string): TaskMessage[] {
  const idx = findInternalPiMessageIdx(prev, sessionId);
  if (idx === -1) return prev;
  const m = prev[idx];
  const next = [...prev];
  next[idx] = { ...m, text: (m.text || "") + content };
  return next;
}

export function processInternalPiThinking(prev: TaskMessage[], sessionId: string, content: string): TaskMessage[] {
  const idx = findInternalPiMessageIdx(prev, sessionId);
  if (idx === -1) return prev;
  const m = prev[idx];
  const next = [...prev];
  next[idx] = {
    ...m,
    reasoning: (m.reasoning || "") + content,
    reasoningStartedAt: m.reasoningStartedAt ?? Date.now(),
  };
  return next;
}

export function processInternalPiToolStart(
  prev: TaskMessage[],
  sessionId: string,
  data: { tool_call_id: string; name: string },
): TaskMessage[] {
  const idx = findInternalPiMessageIdx(prev, sessionId);
  if (idx === -1) return prev;
  const m = prev[idx];
  const tool: ToolCallState = { tool_call_id: data.tool_call_id, name: data.name, output: "", done: false };
  const next = [...prev];
  next[idx] = { ...m, tools: [...(m.tools || []), tool] };
  return next;
}

export function processInternalPiToolUpdate(
  prev: TaskMessage[],
  sessionId: string,
  data: { tool_call_id: string; output: string },
): TaskMessage[] {
  const idx = findInternalPiMessageIdx(prev, sessionId);
  if (idx === -1) return prev;
  const m = prev[idx];
  if (!m.tools) return prev;
  const tools = m.tools.map((t) =>
    t.tool_call_id === data.tool_call_id ? { ...t, output: t.output + data.output } : t,
  );
  const next = [...prev];
  next[idx] = { ...m, tools };
  return next;
}

export function processInternalPiToolEnd(
  prev: TaskMessage[],
  sessionId: string,
  data: { tool_call_id: string; result?: unknown },
): TaskMessage[] {
  const idx = findInternalPiMessageIdx(prev, sessionId);
  if (idx === -1) return prev;
  const m = prev[idx];
  if (!m.tools) return prev;
  const tools = m.tools.map((t) =>
    t.tool_call_id === data.tool_call_id
      ? { ...t, done: true, output: t.output || extractResultText(data.result) }
      : t,
  );
  const next = [...prev];
  next[idx] = { ...m, tools };
  return next;
}

/** Per-turn finalisation hook. All structured planning output (plan,
 *  questions, features, task meta) arrives via dedicated `structured_*`
 *  chat-events, so there is nothing to scrape here; the message list is
 *  returned untouched. Kept as a named pass-through so the surrounding
 *  reducer reads cleanly. */
export function processDoneEvent(prev: TaskMessage[], _model: string): TaskMessage[] {
  return prev;
}

export function finalizeReasoningDuration(prev: TaskMessage[]): TaskMessage[] {
  const idx = [...prev].reverse().findIndex((m) => m.reasoning != null);
  if (idx === -1) return prev;
  const i = prev.length - 1 - idx;
  const m = prev[i];
  if (m.reasoningStartedAt == null || m.reasoningDurationMs != null) return prev;
  const next: TaskMessage[] = [...prev];
  next[i] = { ...m, reasoningDurationMs: Date.now() - m.reasoningStartedAt };
  delete (next[i] as { reasoningStartedAt?: number }).reasoningStartedAt;
  return next;
}

/* ── Phase derivation ─────────────────────────────────────── */

/** True when the message list contains a `questions` block with non-empty
 *  questions that has NOT yet been replied to by a user message. The "reply"
 *  signal is the next `who: "user"` message after the questions message —
 *  matches the existing convention in `answerQuestions` (taskRuntime.tsx),
 *  which appends a `who: "user"` message containing the rendered answers
 *  immediately before re-prompting the agent.
 *
 *  Belt-and-braces guard for auto-mode: when questions are asked, auto-mode
 *  should never skip them — even if a buggy or unusual model output causes
 *  the plan phase to ratchet prematurely. */
export function hasUnansweredQuestions(messages: TaskMessage[]): boolean {
  // Find the LAST questions message with a non-empty questions array.
  let lastQIdx = -1;
  for (let i = messages.length - 1; i >= 0; i--) {
    const m = messages[i];
    if (m.who === "questions" && Array.isArray(m.questions) && m.questions.length > 0) {
      lastQIdx = i;
      break;
    }
  }
  if (lastQIdx === -1) return false;
  for (let i = lastQIdx + 1; i < messages.length; i++) {
    if (messages[i].who === "user") return false;
  }
  return true;
}

/** Advance phase only if `target` is strictly later than `prev`. */
export function advancePhase(prev: TaskPhase, target: TaskPhase): TaskPhase {
  return PHASE_RANK[target] > PHASE_RANK[prev] ? target : prev;
}

/** Derive the next phase from the current message list, using the same
 *  ratchet rules the original code applied across multiple useEffects. */
export function derivePhaseFromMessages(prev: TaskPhase, messages: TaskMessage[]): TaskPhase {
  let next = prev;

  if (messages.some((m) => m.who === "plan")) {
    next = advancePhase(next, "plan-ready");
  }

  if (PHASE_RANK[next] < PHASE_RANK["plan"]) {
    if (messages.some((m) => m.who === "questions" && Array.isArray(m.questions) && m.questions.length > 0)) {
      next = advancePhase(next, "questions");
    }
  }

  if (next === "implement" && messages.some((m) => m.who === "complete")) {
    next = "implement-done";
  }

  return next;
}

/* ── Hivemind progress helpers ────────────────────────────── */

/** Match a stored model id (e.g. "anthropic/claude-opus-4-7") against a bare
 *  id from the backend (e.g. "claude-opus-4-7"). Backend `model_completed`
 *  events emit just the model name; we stored the prefixed form. */
function modelMatches(stored: string, fromEvent: string): boolean {
  return stored === fromEvent || stored.endsWith(`/${fromEvent}`);
}

/** Build initial running rows for a list of model ids. */
function makeRunningRows(models: string[], startedAt: number): ModelRunState[] {
  return models.map((id) => ({ modelId: id, status: "running", startedAt }));
}

/** Replace the entry in `prev.rounds` for `round` with `next`, growing the
 *  array as needed. Returns the same reference if no change. */
function setRound(rounds: RoundRunState[], next: RoundRunState): RoundRunState[] {
  const idx = rounds.findIndex((r) => r.round === next.round);
  if (idx === -1) return [...rounds, next].sort((a, b) => a.round - b.round);
  if (rounds[idx] === next) return rounds;
  const out = [...rounds];
  out[idx] = next;
  return out;
}

/** Map a backend phase identifier to a user-facing label. Returns "" for
 *  phases where the existing streaming UI already covers the affordance
 *  (currently just `"streaming"`) — the reducer / view treat an empty
 *  label as "render nothing", which keeps the label out of the way once
 *  tokens are flowing. */
export function phaseLabel(raw: string): string {
  switch (raw) {
    case "agent_starting":
      return "Starting agent";
    case "agent_ready":
      return "Agent online";
    case "prompt_loaded":
      return "Prompt loaded";
    case "awaiting_model":
      return "Waiting for model response";
    case "thinking":
      return "Thinking…";
    case "tool_running":
      return "Running tool…";
    case "turn_complete":
      return "Turn complete";
    case "streaming":
    default:
      return "";
  }
}

/* ── Reducer ──────────────────────────────────────────────── */

// Returns the most recent `who: "questions"` message that carries a
// non-empty `questions` array AND has not yet been replied to by the user.
// The "reply" signal is a `who: "user"` message after the questions message —
// mirrors `hasUnansweredQuestions` above. Replaces the earlier helper
// which returned the latest batch unconditionally and caused the
// QuestionsDock to reappear with already-answered questions whenever the
// user switched away and back to a task (a remount re-derived
// `pendingQuestions` from history).
/** True if a `who: "complete"` message already exists since the most
 *  recent user message or session-divider. Used to make the
 *  `structured_task_complete` arm idempotent across event reorderings
 *  (e.g. an intervening `tool_start` push) and across session replay
 *  via `load_task_messages`. Backend ALSO suppresses duplicate
 *  emits in `commands/chat.rs`; this is defense in depth.
 */
function hasCompleteInCurrentTurn(messages: TaskMessage[]): boolean {
  for (let i = messages.length - 1; i >= 0; i--) {
    const m = messages[i];
    if (m.who === "user" || m.who === "session-divider") return false;
    if (m.who === "complete") return true;
  }
  return false;
}

function findLastUnansweredQuestionsMessage(messages: TaskMessage[]): TaskMessage | undefined {
  let lastIdx = -1;
  for (let i = messages.length - 1; i >= 0; i--) {
    const m = messages[i];
    if (m.who === "questions" && Array.isArray(m.questions) && m.questions.length > 0) {
      lastIdx = i;
      break;
    }
  }
  if (lastIdx === -1) return undefined;
  for (let i = lastIdx + 1; i < messages.length; i++) {
    if (messages[i].who === "user") return undefined; // answered
  }
  return messages[lastIdx];
}

export function applyTaskEvent(
  prev: TaskRuntimeState,
  ev: TaskEvent,
  defaultModel: string,
): TaskRuntimeState {
  const model = prev.model || defaultModel;

  switch (ev.kind) {
    case "tps_update":
      return { ...prev, liveTps: ev.tps };

    case "phase": {
      const label = phaseLabel(ev.rawPhase);
      // Once any visible token has arrived we stop overwriting the
      // label — the user's eye is on the message body, not the spinner.
      if (prev.streamPhase?.hasFirstStream) return prev;
      return {
        ...prev,
        streamPhase: {
          label,
          rawPhase: ev.rawPhase,
          elapsedMs: prev.streamPhase?.elapsedMs ?? 0,
          silentMs: prev.streamPhase?.silentMs ?? 0,
          contextTokens: prev.streamPhase?.contextTokens ?? null,
          hasFirstStream: false,
        },
      };
    }

    case "heartbeat": {
      // Ignore heartbeats outside a live phase (post-done / never-started).
      // Also ignore once the first token has arrived; the streaming UI
      // owns the screen from there.
      if (!prev.streamPhase || prev.streamPhase.hasFirstStream) return prev;
      return {
        ...prev,
        streamPhase: {
          ...prev.streamPhase,
          elapsedMs: ev.elapsedMs,
          silentMs: ev.silentMs,
        },
      };
    }

    case "context_loaded": {
      // The first non-zero context-tokens tick proves the prompt was
      // ingested. If we already have a phase block, just stamp the token
      // count; otherwise synthesize a `prompt_loaded` block so the user
      // sees a label even before any other phase event fires.
      return {
        ...prev,
        streamPhase: prev.streamPhase
          ? { ...prev.streamPhase, contextTokens: ev.contextTokens }
          : {
              label: phaseLabel("prompt_loaded"),
              rawPhase: "prompt_loaded",
              elapsedMs: 0,
              silentMs: 0,
              contextTokens: ev.contextTokens,
              hasFirstStream: false,
            },
      };
    }

    case "retrying": {
      // Pi is about to wait `delayMs` and retry the upstream call. The
      // session stays alive; surface a banner so the user knows the
      // spinner isn't stuck.
      return {
        ...prev,
        streaming: prev.currentTurnComplete ? prev.streaming : true,
        retryStatus: {
          attempt: ev.attempt,
          maxAttempts: ev.maxAttempts,
          delayMs: ev.delayMs,
          summary: ev.summary,
          startedAt: Date.now(),
        },
      };
    }

    case "retry_resumed": {
      // Pi finished its retry. On success streaming continues normally;
      // on failure the next event (Error / done with empty text) will
      // surface the terminal state. Either way the banner clears here.
      return { ...prev, retryStatus: null };
    }

    case "stream_start":
      return { ...prev, streaming: true, currentTurnComplete: false, error: null, liveTps: null, phase: advancePhase(prev.phase, "plan"), streamPhase: null, retryStatus: null };

    case "chunk": {
      const messages = processChunkEvent(prev.messages, ev.content, model);
      const phase = derivePhaseFromMessages(prev.phase, messages);
      const planMsg = messages.find((m) => m.who === "plan");
      const qMsg = findLastUnansweredQuestionsMessage(messages);
      return {
        ...prev,
        messages,
        phase,
        streaming: prev.currentTurnComplete ? prev.streaming : true,
        planText: planMsg?.planText ?? prev.planText,
        pendingQuestions: qMsg?.questions ?? null,
        streamPhase: prev.streamPhase
          ? { ...prev.streamPhase, hasFirstStream: true }
          : null,
      };
    }

    // Phase 3: structured-output planning events. These bypass delimiter
    // scanning by inserting the parsed payload directly into the message
    // stream / task state. Dual-path: if the model AND the legacy chunk
    // pipeline emit the same content, the structured path wins because it
    // arrives first (the backend emits the structured event before the
    // tool_start side-channel that drives the chunk pipeline).
    case "structured_task_meta": {
      // First-wins: don't overwrite an already-set taskMeta.
      if (prev.taskMeta) return prev;
      return { ...prev, taskMeta: ev.meta };
    }

    case "structured_questions": {
      // Derive a key from the incoming questions and only reset progress
      // when the key differs from the previous one (preserves progress on
      // identical re-asks).
      const newKey = ev.questions.map((q) => q.id).join("|");
      const isNewBatch = newKey !== prev.questionsKey;
      // Insert a `questions` message at the tail. Mirrors the post-extract
      // shape the legacy chunk pipeline produces via stripDelimitersAndInsert.
      const messages: TaskMessage[] = [
        ...prev.messages,
        { who: "questions", questions: ev.questions, model, createdAt: Date.now() },
      ];
      return {
        ...prev,
        messages,
        pendingQuestions: ev.questions,
        questionsKey: newKey,
        pendingQuestionIdx: isNewBatch ? 0 : prev.pendingQuestionIdx,
        pendingQuestionAnswers: isNewBatch ? {} : prev.pendingQuestionAnswers,
      };
    }

    case "structured_plan": {
      // Insert a `plan` message at the tail and advance the phase.
      const messages: TaskMessage[] = [
        ...prev.messages,
        { who: "plan", planText: ev.planText, model, createdAt: Date.now() },
      ];
      const phase =
        PHASE_RANK[prev.phase] < PHASE_RANK["plan-ready"] ? "plan-ready" : prev.phase;
      return {
        ...prev,
        messages,
        phase,
        planText: ev.planText,
      };
    }

    case "structured_features": {
      // Mirror onto the most-recent `plan` message (if any) so it survives
      // serialisation.
      let messages = prev.messages;
      const planIdx = messages.findIndex((m) => m.who === "plan");
      if (planIdx !== -1 && !messages[planIdx].features) {
        messages = [...messages];
        messages[planIdx] = { ...messages[planIdx], features: ev.features };
      }
      return {
        ...prev,
        messages,
        swarmFeatures: ev.features,
        swarmMilestones: ev.milestones,
        swarmFeaturesError: null,
        pendingFeaturesRefresh: false,
        featuresRefreshFailed: false,
      };
    }

    case "structured_verdicts": {
      // Attach the parsed verdicts to the most-recent merge asst message.
      // Matching priority:
      //   1. exact `sessionId` (set when verdicts arrive on a Pi chat-event)
      //   2. exact `round` (set when verdicts arrive on a backend
      //      `hivemind-progress::verdicts_updated` event — no session_id)
      //   3. most recent merge bubble (final fallback)
      // Render-side picks the verdicts up via
      // `toStreamEntries` → ChatBubbleEntry.verdicts.
      let messages = prev.messages;
      let targetIdx = -1;
      for (let i = messages.length - 1; i >= 0; i--) {
        const m = messages[i];
        if (m.who !== "asst" || m.reviewKind?.phase !== "merge") continue;
        if (ev.sessionId) {
          if (m.reviewKind.sessionId !== ev.sessionId) continue;
          targetIdx = i;
          break;
        }
        if (ev.round != null) {
          if (m.reviewKind.round !== ev.round) continue;
          targetIdx = i;
          break;
        }
        targetIdx = i;
        break;
      }
      if (targetIdx === -1) return prev;
      const target = messages[targetIdx];
      messages = [...messages];
      messages[targetIdx] = { ...target, verdicts: ev.verdicts };
      return { ...prev, messages };
    }

    case "structured_stability_impl_complete": {
      // Stability-test surface lives outside the Tasks-view reducer; this
      // case is a no-op here, kept so the event type is exhaustive.
      return prev;
    }

    case "structured_task_complete": {
      // Tasks-view implementation agent signalled completion via the
      // `submit_task_complete` tool. This is the ONLY signal that marks
      // a Tasks implementation run done — the previous implicit
      // `done`-event heuristic was removed (see `case "done"` below).
      // The system prompt + per-message implementation prompt in
      // `plan-mode.ts` instruct the agent to call this tool exactly once
      // when finished.
      if (hasCompleteInCurrentTurn(prev.messages)) {
        // Duplicate completion signal in the same turn. The first emit is
        // authoritative; collapse this one to a no-op. Phase stays where
        // it already advanced to. (Backend suppresses these too — see
        // chat.rs PhaseState.task_complete_emitted.)
        return prev;
      }
      const messages: TaskMessage[] = [
        ...prev.messages,
        {
          who: "complete" as const,
          text: ev.summary,
          successState: ev.successState,
          t: new Date().toISOString(),
          createdAt: Date.now(),
        },
      ];
      return {
        ...prev,
        messages,
        phase: advancePhase(prev.phase, "implement-done"),
        streaming: false,
        currentTurnComplete: true,
        queueState: null,
        liveTps: null,
        streamPhase: null,
      };
    }

    case "thinking": {
      const messages = processThinkingEvent(prev.messages, ev.content, model);
      return {
        ...prev,
        messages,
        streaming: prev.currentTurnComplete ? prev.streaming : true,
        streamPhase: prev.streamPhase
          ? { ...prev.streamPhase, hasFirstStream: true }
          : null,
      };
    }

    case "tool_start":
      return {
        ...prev,
        messages: processToolStartEvent(prev.messages, ev.data, model),
        streaming: prev.currentTurnComplete ? prev.streaming : true,
      };

    case "tool_update":
      return { ...prev, messages: processToolUpdateEvent(prev.messages, ev.data) };

    case "tool_end":
      return { ...prev, messages: processToolEndEvent(prev.messages, ev.data) };

    case "usage":
      return { ...prev, sessionUsage: ev.usage };

    case "queue_update":
      return { ...prev, queueState: ev.queue };

    case "done": {
      const processed = processDoneEvent(prev.messages, model);
      const finalized = finalizeReasoningDuration(processed);
      const phase = derivePhaseFromMessages(prev.phase, finalized);

      // Implementation completion is now signalled exclusively by the
      // `structured_task_complete` event (the agent calls
      // `submit_task_complete`; see `plan-mode.ts` IMPL_SYSTEM_PROMPT +
      // buildImplementPrompt). The previous implicit "if `done` lands
      // while in implement, assume complete" heuristic was removed
      // because it fired on every `done` and missed long multi-turn runs
      // where `done` arrived after the phase had already moved on.
      let messages = finalized;
      // Mirror swarmFeatures onto the plan message so it survives serialization.
      const featuresSource = prev.swarmFeatures;
      if (featuresSource && featuresSource.length > 0) {
        const planIdx = messages.findIndex((m) => m.who === "plan");
        if (planIdx !== -1 && !messages[planIdx].features) {
          const nextMessages = [...messages];
          nextMessages[planIdx] = { ...nextMessages[planIdx], features: featuresSource };
          messages = nextMessages;
        }
      }

      const planMsg = messages.find((m) => m.who === "plan");
      const qMsg = findLastUnansweredQuestionsMessage(messages);

      return {
        ...prev,
        messages,
        phase,
        streaming: false,
        currentTurnComplete: false,
        queueState: null,
        liveTps: null,
        planText: planMsg?.planText ?? prev.planText,
        pendingQuestions: qMsg?.questions ?? null,
        streamPhase: null,
        retryStatus: null,
        // If `pendingFeaturesRefresh` is still true at `done` time, the
        // `structured_features` clause never fired during this turn (it
        // always clears the flag on success), so we know features were
        // not refreshed. Pi emits `tool_execution_start` events strictly
        // before `done` for the same turn, so there is no ordering race.
        pendingFeaturesRefresh: false,
        featuresRefreshFailed: prev.pendingFeaturesRefresh
          ? true
          : (prev.featuresRefreshFailed ?? false),
      };
    }

    case "error": {
      const finalized = finalizeReasoningDuration(prev.messages);
      // Always surface an inline error bubble — earlier, an empty
      // `ev.message` would silently swallow the failure and the user
      // would see no reply at all. Fall back to a generic label when
      // the backend can't produce a friendly summary. Dedup against
      // the last message so repeated identical errors don't stack.
      const displayMessage = ev.message && ev.message.trim().length > 0
        ? ev.message
        : "Provider error (no detail available)";
      const last = finalized[finalized.length - 1];
      const lastIsSame =
        last && last.who === "error" && (last.errorMessage ?? last.text) === displayMessage;
      const messages: TaskMessage[] = lastIsSame
        ? finalized
        : [
            ...finalized,
            {
              who: "error" as const,
              errorMessage: displayMessage,
              t: new Date().toISOString(),
              createdAt: Date.now(),
            },
          ];
      return {
        ...prev,
        messages,
        error: displayMessage,
        streaming: false,
        currentTurnComplete: false,
        queueState: null,
        liveTps: null,
        streamPhase: null,
        retryStatus: null,
        // Mirror `done`: if a features-refresh turn errored, surface the
        // recovery banner so the user can launch / re-emit. The OR
        // composition preserves a prior failure if this `error` isn't
        // the refresh turn (prev.pendingFeaturesRefresh === false).
        pendingFeaturesRefresh: false,
        featuresRefreshFailed: prev.pendingFeaturesRefresh
          ? true
          : (prev.featuresRefreshFailed ?? false),
      };
    }

    case "stop":
      return {
        ...prev,
        messages: finalizeReasoningDuration(prev.messages),
        streaming: false,
        currentTurnComplete: false,
        queueState: null,
        liveTps: null,
        streamPhase: null,
        retryStatus: null,
        // Same rationale as `done` / `error`. The OR composition
        // guarantees that a `stop`→later-`done` sequence keeps
        // `featuresRefreshFailed: true` (the second event sees
        // `prev.pendingFeaturesRefresh === false` but
        // `prev.featuresRefreshFailed === true` and preserves it).
        pendingFeaturesRefresh: false,
        featuresRefreshFailed: prev.pendingFeaturesRefresh
          ? true
          : (prev.featuresRefreshFailed ?? false),
      };

    case "nurse_started": {
      // Insert a new `who: "nurse"` inline card keyed by interventionId.
      // If an entry with this interventionId somehow already exists, replace
      // it in-place so re-runs don't duplicate cards.
      const existingIdx = prev.messages.findIndex(
        (m) => m.who === "nurse" && m.nurseInterventionId === ev.interventionId,
      );
      const card: TaskMessage = {
        who: "nurse",
        t: ev.t || new Date().toISOString(),
        createdAt: Date.now(),
        nurseInterventionId: ev.interventionId,
        nurseLevel: ev.level,
        nurseObservation: ev.observation,
        nurseAction: ev.action,
        nurseStatus: "started",
      };
      const messages =
        existingIdx >= 0
          ? prev.messages.map((m, i) => (i === existingIdx ? card : m))
          : [...prev.messages, card];
      return { ...prev, messages };
    }

    case "nurse_reasoning": {
      const messages = prev.messages.map((m) => {
        if (m.who === "nurse" && m.nurseInterventionId === ev.interventionId) {
          return {
            ...m,
            nurseStatus: "reasoning" as const,
            nurseReasoning: (m.nurseReasoning || "") + ev.delta,
          };
        }
        return m;
      });
      return { ...prev, messages };
    }

    case "nurse_completed": {
      const messages = prev.messages.map((m) => {
        if (m.who === "nurse" && m.nurseInterventionId === ev.interventionId) {
          return {
            ...m,
            nurseStatus: "completed" as const,
            nurseReasoning: ev.fullReasoning ?? m.nurseReasoning,
          };
        }
        return m;
      });
      return { ...prev, messages };
    }

    case "nurse_failed": {
      const messages = prev.messages.map((m) => {
        if (m.who === "nurse" && m.nurseInterventionId === ev.interventionId) {
          return {
            ...m,
            nurseStatus: "failed" as const,
            nurseError: ev.error,
          };
        }
        return m;
      });
      return { ...prev, messages };
    }

    case "resync": {
      const messages = ev.messages && ev.messages.length > prev.messages.length
        ? ev.messages
        : prev.messages;
      // Recover features from the plan message's serialised `features` field
      // when prior state was empty — the plan bubble persists across reloads
      // and is the authoritative source after a restart.
      let swarmFeaturesResync = prev.swarmFeatures;
      if (!swarmFeaturesResync || swarmFeaturesResync.length === 0) {
        const planMsg = messages.find(
          (m) => m.who === "plan" && Array.isArray(m.features) && m.features.length > 0,
        );
        if (planMsg && planMsg.features) {
          swarmFeaturesResync = planMsg.features;
        }
      }
      const phase = derivePhaseFromMessages(prev.phase, messages);
      const planMsg = messages.find((m) => m.who === "plan");
      const qMsg = findLastUnansweredQuestionsMessage(messages);
      const next: TaskRuntimeState = {
        ...prev,
        messages,
        phase,
        planText: planMsg?.planText ?? prev.planText,
        pendingQuestions: qMsg?.questions ?? null,
        swarmFeatures: swarmFeaturesResync ?? prev.swarmFeatures,
      };
      if (ev.sessionAlive === false && prev.streaming) {
        next.streaming = false;
        next.queueState = null;
      }
      return next;
    }

    case "review_start": {
      const startedAt = Date.now();
      const roundEntry: RoundRunState = {
        round: ev.round,
        models: makeRunningRows(ev.models, startedAt),
      };
      const existingRounds = prev.reviewProgress?.rounds ?? [];
      // TODO: persist startedAt to disk for reload accuracy
      const reviewProgress: ReviewProgress = {
        reviewId: ev.reviewId,
        currentRound: ev.round,
        totalRounds: ev.totalRounds,
        phase: "reviewing",
        rounds: setRound(existingRounds, roundEntry),
        startedAt: prev.reviewProgress?.startedAt ?? Date.now(),
      };
      return {
        ...prev,
        activeReviewJobId: ev.jobId,
        reviewProgress,
      };
    }

    case "hm_started": {
      // Defensive: adopt jobId if reviewProgress already references it.
      if (prev.activeReviewJobId === ev.jobId) return prev;
      if (!prev.reviewProgress) return prev;
      return { ...prev, activeReviewJobId: ev.jobId };
    }

    case "hm_round_started": {
      if (prev.activeReviewJobId !== ev.jobId) return prev;
      if (!prev.reviewProgress) return prev;
      const startedAt = Date.now();
      const existing = prev.reviewProgress.rounds.find((r) => r.round === ev.round);
      let nextEntry: RoundRunState;
      if (existing) {
        // Stamp startedAt on any models still pending so the elapsed timer starts.
        nextEntry = {
          round: ev.round,
          models: existing.models.map((m) =>
            m.status === "pending" ? { ...m, status: "running", startedAt } : m,
          ),
        };
      } else if (ev.models && ev.models.length > 0) {
        nextEntry = { round: ev.round, models: makeRunningRows(ev.models, startedAt) };
      } else {
        // No model list available; just record currentRound advance.
        return {
          ...prev,
          reviewProgress: { ...prev.reviewProgress, currentRound: ev.round },
        };
      }
      return {
        ...prev,
        reviewProgress: {
          ...prev.reviewProgress,
          currentRound: ev.round,
          rounds: setRound(prev.reviewProgress.rounds, nextEntry),
        },
      };
    }

    case "hm_model_completed": {
      if (prev.activeReviewJobId !== ev.jobId) return prev;
      if (!prev.reviewProgress) return prev;
      const round = prev.reviewProgress.rounds.find((r) => r.round === ev.round);
      if (!round) return prev;
      const idx = round.models.findIndex((m) => modelMatches(m.modelId, ev.modelId));
      if (idx === -1) return prev;
      // Idempotency: if already done, no-op.
      if (round.models[idx].status === "done") return prev;
      const updated: ModelRunState = {
        ...round.models[idx],
        status: "done",
        inputTokens: ev.inputTokens,
        outputTokens: ev.outputTokens,
        durationMs: ev.durationMs,
        cost: ev.cost,
      };
      const nextModels = [...round.models];
      nextModels[idx] = updated;
      const nextRound: RoundRunState = { round: round.round, models: nextModels };
      return {
        ...prev,
        reviewProgress: {
          ...prev.reviewProgress,
          rounds: setRound(prev.reviewProgress.rounds, nextRound),
        },
      };
    }

    case "hm_model_failed": {
      if (prev.activeReviewJobId !== ev.jobId) return prev;
      if (!prev.reviewProgress) return prev;
      const round = prev.reviewProgress.rounds.find((r) => r.round === ev.round);
      if (!round) return prev;

      // ── Persistent conversation bubble ──
      // Append a `who: "error"` bubble so the failure survives both the live
      // dock collapse (auto-collapse 5s after terminal state) and a reload
      // of the task from disk. Dedup on the compound key — this allows
      // distinct `modelIdx` values for the same `modelId` to produce
      // separate bubbles, while a re-emit of the same
      // `(jobId, round, modelId, modelIdx)` tuple is a no-op.
      // We deliberately scan the trailing tail (not the whole list) so the
      // common case is O(1); duplicate keys deeper in history are still
      // detected via a bounded scan (last 64 entries) to keep state size
      // from growing unbounded under a buggy back-end re-emit storm.
      const dedupKey = `${ev.jobId}::${ev.round}::${ev.modelId}::${ev.modelIdx ?? 0}`;
      const tailStart = Math.max(0, prev.messages.length - 64);
      let alreadyEmitted = false;
      for (let i = prev.messages.length - 1; i >= tailStart; i--) {
        const m = prev.messages[i];
        if (m.who === "error" && m.hivemindFailureKey === dedupKey) {
          alreadyEmitted = true;
          break;
        }
      }
      const displayMessage = `Hivemind reviewer ${ev.modelId} (round ${ev.round}) failed: ${ev.error}`;
      const messages: TaskMessage[] = alreadyEmitted
        ? prev.messages
        : [
            ...prev.messages,
            {
              who: "error" as const,
              errorMessage: displayMessage,
              hivemindFailureKey: dedupKey,
              t: new Date().toISOString(),
              createdAt: Date.now(),
            },
          ];

      // ── Row-state transition ──
      // The existing `ReviewProgress.rounds[].models[]` shape is keyed by
      // position/modelId (not by `modelIdx`). When duplicate-instance
      // reviewers share a `modelId`, `findIndex` returns the first match;
      // we still update that row once, then short-circuit further row
      // updates — but the per-modelIdx bubble above is unconditional on
      // its own dedup key, so each instance still surfaces a distinct
      // failure bubble in the conversation transcript.
      const idx = round.models.findIndex((m) => modelMatches(m.modelId, ev.modelId));
      let nextReviewProgress = prev.reviewProgress;
      if (idx !== -1 && round.models[idx].status !== "failed") {
        const updated: ModelRunState = {
          ...round.models[idx],
          status: "failed",
          error: ev.error,
        };
        const nextModels = [...round.models];
        nextModels[idx] = updated;
        const nextRound: RoundRunState = { round: round.round, models: nextModels };
        nextReviewProgress = {
          ...prev.reviewProgress,
          rounds: setRound(prev.reviewProgress.rounds, nextRound),
        };
      }

      // If neither the messages list nor the row state changed, return
      // `prev` unchanged so React skips a downstream re-render.
      if (alreadyEmitted && nextReviewProgress === prev.reviewProgress) {
        return prev;
      }

      return {
        ...prev,
        messages,
        reviewProgress: nextReviewProgress,
      };
    }

    case "hm_round_completed": {
      if (prev.activeReviewJobId !== ev.jobId) return prev;
      if (!prev.reviewProgress) return prev;
      const round = prev.reviewProgress.rounds.find((r) => r.round === ev.round);
      if (!round) return prev;
      // Round end can be a timeout: any still-running rows are reclassified
      // as failed so the bar settles on a final icon.
      let changed = false;
      const nextModels = round.models.map((m) => {
        if (m.status === "running" || m.status === "pending") {
          changed = true;
          return { ...m, status: "failed" as const, error: m.error ?? "round timeout" };
        }
        return m;
      });
      if (!changed) return prev;
      const nextRound: RoundRunState = { round: round.round, models: nextModels };
      return {
        ...prev,
        reviewProgress: {
          ...prev.reviewProgress,
          rounds: setRound(prev.reviewProgress.rounds, nextRound),
        },
      };
    }

    case "hm_completed":
      return prev;

    case "hm_failed": {
      if (prev.activeReviewJobId !== ev.jobId) return prev;
      const ratchet: TaskPhase =
        PHASE_RANK[prev.phase] > PHASE_RANK["plan-ready"]
          ? prev.phase
          : "plan-ready";
      return {
        ...prev,
        error: `Review failed: ${ev.message}`,
        reviewProgress: null,
        activeReviewJobId: null,
        internalPi: null,
        streaming: false,
        phase: ratchet,
      };
    }

    case "review_error": {
      const ratchet: TaskPhase =
        PHASE_RANK[prev.phase] > PHASE_RANK["plan-ready"]
          ? prev.phase
          : "plan-ready";
      return {
        ...prev,
        error: ev.error,
        streaming: false,
        reviewProgress: null,
        activeReviewJobId: null,
        internalPi: null,
        queueState: null,
        phase: ratchet,
      };
    }

    case "merge_interrupted": {
      // Only flag the task whose active review matches this jobId. Defensive
      // guard: stale events on closed tasks are ignored.
      if (prev.activeReviewJobId !== ev.jobId) return prev;
      // Idempotency: if already flagged for the same round, no-op.
      if (
        prev.mergeInterrupted &&
        prev.mergeInterrupted.jobId === ev.jobId &&
        prev.mergeInterrupted.round === ev.round
      ) {
        return prev;
      }
      return {
        ...prev,
        streaming: false,
        mergeInterrupted: {
          jobId: ev.jobId,
          round: ev.round,
          outputLen: ev.outputLen,
          message: ev.message,
        },
      };
    }

    case "merge_resume_started":
      // Clear the interrupted flag and put the task back into a streaming
      // review state. The runtime is responsible for spawning the merge Pi
      // session; the reducer just tracks UI-visible state.
      if (!prev.mergeInterrupted && !prev.reviewInterrupted) return prev;
      return {
        ...prev,
        mergeInterrupted: null,
        reviewInterrupted: null,
        streaming: true,
        error: null,
      };

    case "review_interrupted": {
      const s = ev.snapshot;
      // Idempotency: if already flagged for the same review/phase/round, no-op.
      if (
        prev.reviewInterrupted &&
        prev.reviewInterrupted.reviewId === s.reviewId &&
        prev.reviewInterrupted.jobId === s.latestJobId &&
        prev.reviewInterrupted.phase === s.phase &&
        prev.reviewInterrupted.round === s.round
      ) {
        return prev;
      }
      const reviewInterrupted: ReviewInterruptedState = reviewInterruptedFromSnapshot(s);
      // Shim: when phase === "merge", also set the legacy `mergeInterrupted`
      // slot so existing UI consumers continue to function.
      const mergeShim: InterruptedMergeState | null =
        s.phase === "merge"
          ? {
              jobId: s.latestJobId,
              round: s.round,
              outputLen: s.mergeOutput ? s.mergeOutput.length : 0,
              message: s.message,
            }
          : prev.mergeInterrupted;
      return {
        ...prev,
        streaming: false,
        reviewInterrupted,
        mergeInterrupted: mergeShim,
      };
    }

    case "review_resume_started":
      if (!prev.reviewInterrupted && !prev.mergeInterrupted) return prev;
      return {
        ...prev,
        reviewInterrupted: null,
        mergeInterrupted: null,
        streaming: true,
        error: null,
      };

    case "internal_pi_started":
      return {
        ...prev,
        internalPi: {
          kind: ev.piKind,
          sessionId: ev.sessionId,
          modelName: ev.modelName,
          status: "running",
        },
      };

    case "internal_pi_tps":
      // SessionId-guarded so a late event from a finished context Pi can't
      // clobber liveTps after the merge Pi has taken over.
      if (!prev.internalPi || prev.internalPi.sessionId !== ev.sessionId) return prev;
      return { ...prev, liveTps: ev.tps };

    case "internal_pi_done": {
      if (!prev.internalPi || prev.internalPi.sessionId !== ev.sessionId) return prev;
      if (prev.internalPi.status === "done") return prev;
      // Finalize the bound message's reasoning timer so the UI stops showing
      // a perpetual "thinking…" spinner. Without this the message remains in
      // a perpetually-thinking state regardless of `done` arrival because
      // `processDoneEvent`/`finalizeReasoningDuration` are not part of this
      // path (they're only called for the regular `case "done"` above).
      const idx = findInternalPiMessageIdx(prev.messages, ev.sessionId);
      let messages = prev.messages;
      if (idx !== -1) {
        const m = prev.messages[idx];
        if (m.reasoningStartedAt != null && m.reasoningDurationMs == null) {
          const updated: TaskMessage = {
            ...m,
            reasoningDurationMs: Date.now() - m.reasoningStartedAt,
          };
          delete (updated as { reasoningStartedAt?: number }).reasoningStartedAt;
          messages = [...prev.messages];
          messages[idx] = updated;
        }
      }
      return {
        ...prev,
        messages,
        internalPi: { ...prev.internalPi, status: "done" },
      };
    }

    case "internal_pi_failed": {
      // Finalize the bound message + tag it with an inline error marker.
      // Used by RC2/RC3 surfaced errors (context-done with no marker,
      // context watchdog timeout, etc.). The slice's status is also flipped
      // to "done" so the telemetry strip stops spinning.
      const idx = findInternalPiMessageIdx(prev.messages, ev.sessionId);
      let messages = prev.messages;
      if (idx !== -1) {
        const m = prev.messages[idx];
        let updated: TaskMessage = m;
        if (m.reasoningStartedAt != null && m.reasoningDurationMs == null) {
          updated = {
            ...updated,
            reasoningDurationMs: Date.now() - m.reasoningStartedAt,
          };
          delete (updated as { reasoningStartedAt?: number }).reasoningStartedAt;
        }
        if (updated.error !== ev.message) {
          updated = { ...updated, error: ev.message };
        }
        if (updated !== m) {
          messages = [...prev.messages];
          messages[idx] = updated;
        }
      }
      const slice =
        prev.internalPi && prev.internalPi.sessionId === ev.sessionId
          ? { ...prev.internalPi, status: "done" as const }
          : prev.internalPi;
      if (messages === prev.messages && slice === prev.internalPi) return prev;
      return { ...prev, messages, internalPi: slice };
    }

    case "internal_pi_message_start":
      return {
        ...prev,
        messages: processInternalPiMessageStart(prev.messages, ev.sessionId, ev.reviewKind, ev.modelName),
      };

    case "internal_pi_chunk": {
      const messages = processInternalPiChunk(prev.messages, ev.sessionId, ev.content);
      return messages === prev.messages ? prev : { ...prev, messages };
    }

    case "internal_pi_thinking": {
      const messages = processInternalPiThinking(prev.messages, ev.sessionId, ev.content);
      return messages === prev.messages ? prev : { ...prev, messages };
    }

    case "internal_pi_tool_start": {
      const messages = processInternalPiToolStart(prev.messages, ev.sessionId, ev.data);
      return messages === prev.messages ? prev : { ...prev, messages };
    }

    case "internal_pi_tool_update": {
      const messages = processInternalPiToolUpdate(prev.messages, ev.sessionId, ev.data);
      return messages === prev.messages ? prev : { ...prev, messages };
    }

    case "internal_pi_tool_end": {
      const messages = processInternalPiToolEnd(prev.messages, ev.sessionId, ev.data);
      return messages === prev.messages ? prev : { ...prev, messages };
    }

    case "review_resync": {
      if (prev.activeReviewJobId !== ev.snapshot.job_id) return prev;
      // TODO: Synthesise `who:"error"` bubbles for steps with `status === "failed"`
      // so reloaded tasks show the same failure transcript as live-reviewed ones.
      
      // ── Phase preservation ──
      // The frontend's phase is driven by the runtime (taskRuntime.tsx) via
      // direct updateTask calls. The reducer only preserves it. On fresh page
      // load (no prior reviewProgress), default to "reviewing" — this is correct
      // because the runtime will advance the phase to "merging" when appropriate.
      // Attempting to derive the phase from the backend snapshot's `status` field
      // is unreliable because that field reflects job-execution state (running/
      // completed/failed), not the frontend's phase concept.
      
      // ── Persist startedAt for stable elapsed-time counters ──
      // Build two maps: one for currently-running models (to preserve their
      // startedAt across polls), and one for models seen in ANY prior poll
      // (to give new-running models a stable first-seen timestamp).
      const prevStartedAt = new Map<string, number>();
      const seenModels = new Map<string, number>();
      const prevRounds = prev.reviewProgress?.rounds ?? [];
      for (const r of prevRounds) {
        for (const m of r.models) {
          if (m.startedAt != null) {
            seenModels.set(m.modelId, m.startedAt);
            if (m.status === "running") {
              prevStartedAt.set(m.modelId, m.startedAt);
            }
          }
        }
      }
      
      // ── Build snapshot rounds ──
      const stepsByRound = new Map<number, ModelRunState[]>();
      for (const s of ev.snapshot.steps) {
        const status: ModelRunState["status"] =
          s.status === "completed" ? "done"
          : s.status === "failed" ? "failed"
          : s.status === "running" ? "running"
          : "pending";
        const display = s.provider ? `${s.provider}/${s.model_id}` : s.model_id;
        const row: ModelRunState = {
          modelId: display,
          status,
          inputTokens: s.input_tokens ?? undefined,
          outputTokens: s.output_tokens ?? undefined,
          durationMs: s.duration_ms ?? undefined,
          cost: s.cost ?? undefined,
          startedAt: status === "running"
            ? (prevStartedAt.get(display) ?? seenModels.get(display) ?? Date.now())
            : undefined,
        };
        const arr = stepsByRound.get(s.round_number) ?? [];
        arr.push(row);
        stepsByRound.set(s.round_number, arr);
      }
      const snapshotRounds: RoundRunState[] = Array.from(stepsByRound.entries())
        .map(([round, models]) => ({ round, models }))
        .sort((a, b) => a.round - b.round);
      
      // ── Merge snapshot data into existing frontend rounds ──
      // CRITICAL: Snapshot steps always have round_number=1 because each
      // frontend round launches a separate backend job with numRounds=1.
      // The snapshot's data belongs to prev.reviewProgress.currentRound.
      // Never touch other rounds — their model status is canonically correct.
      // Additionally, never regress a model that is "done" or "failed".
      // For "done" models, non-status fields (tokens, cost, duration) may
      // still be updated from snapshot if the snapshot has better data.
      let mergedRounds: RoundRunState[];
      if (prevRounds.length > 0 && prev.reviewProgress?.currentRound != null) {
        // Build map: display key → step, bare model_id → step
        const stepByKey = new Map<string, (typeof ev.snapshot.steps)[number]>();
        for (const s of ev.snapshot.steps) {
          const display = s.provider ? `${s.provider}/${s.model_id}` : s.model_id;
          stepByKey.set(display, s);
          stepByKey.set(s.model_id, s); // fallback for bare IDs from events
        }
        
        // Immutably update existing rounds — only touch the currently active round.
        // Previous rounds are canonically correct and must not be touched.
        mergedRounds = prevRounds.map((existingRound) => {
          // Only update models for the round this snapshot's job belongs to.
          // Previous rounds are canonically correct and must not be touched.
          if (!prev.reviewProgress || existingRound.round !== prev.reviewProgress.currentRound) {
            return existingRound;
          }
          return {
            ...existingRound,
            models: existingRound.models.map((model) => {
              // Try exact display match first, then bare model_id
              const matchingStep = stepByKey.get(model.modelId) ?? stepByKey.get(
                // Strip provider prefix if display match failed (model.modelId
                // is "provider/id" form, stepByKey also has bare "id")
                model.modelId.includes("/") ? model.modelId.split("/").pop()! : model.modelId
              );
              if (!matchingStep) return model; // No snapshot data → keep existing

              // Belt-and-suspenders: never regress a terminal-status model.
              // Allow metric updates (tokens, cost, duration) for done models
              // since snapshot data may be more accurate.
              if (model.status === "done" || model.status === "failed") {
                return {
                  ...model,
                  inputTokens: matchingStep.input_tokens ?? model.inputTokens,
                  outputTokens: matchingStep.output_tokens ?? model.outputTokens,
                  durationMs: matchingStep.duration_ms ?? model.durationMs,
                  cost: matchingStep.cost ?? model.cost,
                };
              }

              return {
                ...model,
                status: matchingStep.status === "completed" ? "done"
                      : matchingStep.status === "failed" ? "failed"
                      : matchingStep.status === "running" ? "running"
                      : model.status,
                inputTokens: matchingStep.input_tokens ?? model.inputTokens,
                outputTokens: matchingStep.output_tokens ?? model.outputTokens,
                durationMs: matchingStep.duration_ms ?? model.durationMs,
                cost: matchingStep.cost ?? model.cost,
                error: matchingStep.status === "completed" ? undefined
                     : matchingStep.status === "failed" ? (model.error ?? "failed")
                     : model.error,
              };
            }),
          };
        });
        
        // Append any snapshot rounds that don't exist yet in the frontend
        const existingRoundNumbers = new Set(mergedRounds.map((r) => r.round));
        for (const sr of snapshotRounds) {
          if (!existingRoundNumbers.has(sr.round)) {
            mergedRounds.push(sr);
          }
        }
      } else {
        // No frontend data yet, or currentRound is null — use snapshot rounds as-is.
        // If currentRound is null but rounds exist (state inconsistency), this
        // conservative fallback accepts the overwrite risk rather than crashing.
        mergedRounds = snapshotRounds;
      }
      
      // Sort merged rounds for stable `roundsKey` determinism.
      // The frontend rounds may be in insertion order; sorting guarantees
      // the string key used by the auto-advance effect remains consistent.
      mergedRounds.sort((a, b) => a.round - b.round);
      
      // ── Advance currentRound, bounded by existing data ──
      // Only advance to a round that actually has data in the merged array,
      // so the UI never jumps to an empty round (which would show no model rows).
      const maxExistingRound = mergedRounds.length > 0
        ? Math.max(...mergedRounds.map(r => r.round))
        : 0;
      const advancedRound = Math.min(
        Math.max(prev.reviewProgress?.currentRound ?? 0, ev.snapshot.current_round),
        maxExistingRound > 0 ? maxExistingRound : Infinity
      );
      
      // TODO: persist startedAt to disk for reload accuracy
      const reviewProgress: ReviewProgress = {
        reviewId: prev.reviewProgress?.reviewId,
        currentRound: advancedRound,
        totalRounds: prev.reviewProgress?.totalRounds ?? ev.snapshot.total_rounds,
        phase: prev.reviewProgress?.phase ?? "reviewing",
        rounds: mergedRounds,
        startedAt: prev.reviewProgress?.startedAt,
      };
      return { ...prev, reviewProgress };
    }
  }
}

/* ── ChatEvent → TaskEvent adapter ───────────────────────── */

export function mapChatEventToTaskEvent(e: ChatEvent): TaskEvent | null {
  switch (e.event_type) {
    case "start":
      return { kind: "stream_start" };
    case "chunk":
      return { kind: "chunk", content: e.content };
    case "thinking":
      return { kind: "thinking", content: e.content };
    case "tool_start":
      try { return { kind: "tool_start", data: JSON.parse(e.content) }; } catch { return null; }
    case "tool_update":
      try { return { kind: "tool_update", data: JSON.parse(e.content) }; } catch { return null; }
    case "tool_end":
      try { return { kind: "tool_end", data: JSON.parse(e.content) }; } catch { return null; }
    case "usage":
      try {
        const data = JSON.parse(e.content);
        return {
          kind: "usage",
          usage: {
            input: data.input || 0,
            output: data.output || 0,
            cacheRead: data.cache_read || 0,
            cost: data.cost || 0,
            contextTokens: data.context_tokens || 0,
            contextWindow: data.context_window || 0,
            contextPercent: data.context_percent || 0,
            tokPerSec: data.tokens_per_sec || 0,
          },
        };
      } catch { return null; }
    case "queue_update":
      try {
        const data = JSON.parse(e.content);
        return { kind: "queue_update", queue: { steering: data.steering || [], followUp: data.follow_up || [] } };
      } catch { return null; }
    case "done":
      return { kind: "done" };
    case "error":
      return { kind: "error", message: e.content };
    case "tps":
      try {
        const data = JSON.parse(e.content);
        return { kind: "tps_update", tps: data.tps ?? 0 };
      } catch { return null; }
    case "phase":
      return { kind: "phase", rawPhase: e.content };
    case "heartbeat":
      try {
        const d = JSON.parse(e.content);
        return {
          kind: "heartbeat",
          phase: typeof d.phase === "string" ? d.phase : "",
          elapsedMs: Number(d.elapsed_ms) || 0,
          silentMs: Number(d.silent_ms) || 0,
        };
      } catch { return null; }
    case "context_loaded": {
      const n = Number(e.content);
      return Number.isFinite(n) ? { kind: "context_loaded", contextTokens: n } : null;
    }
    case "retrying":
      try {
        const d = JSON.parse(e.content);
        return {
          kind: "retrying",
          attempt: Number(d.attempt) || 0,
          maxAttempts: Number(d.max_attempts) || 0,
          delayMs: Number(d.delay_ms) || 0,
          summary:
            typeof d.error_summary === "string" && d.error_summary
              ? d.error_summary
              : "Provider error",
        };
      } catch {
        return null;
      }
    case "retry_resumed":
      try {
        const d = JSON.parse(e.content);
        return {
          kind: "retry_resumed",
          attempt: Number(d.attempt) || 0,
          success: Boolean(d.success),
        };
      } catch {
        return null;
      }
    // Phase 3: structured-output planning events. Each `content` is a
    // JSON-stringified payload mirroring the matching delimiter-extracted
    // shape so the reducer can short-circuit delimiter parsing.
    case "structured_task_meta":
      try {
        const d = JSON.parse(e.content);
        if (
          typeof d?.title === "string" &&
          d.title.trim() &&
          typeof d?.description === "string" &&
          d.description.trim()
        ) {
          return {
            kind: "structured_task_meta",
            meta: { title: d.title.trim(), description: d.description.trim() },
          };
        }
        return null;
      } catch {
        return null;
      }
    case "structured_questions":
      try {
        const d = JSON.parse(e.content);
        // Accept either `{ questions: [...] }` (the tool's parameters shape)
        // or a bare array (defensive — matches the legacy delimiter shape).
        const arr = Array.isArray(d?.questions)
          ? d.questions
          : Array.isArray(d)
          ? d
          : null;
        if (!arr) return null;
        const valid: TaskQuestion[] = [];
        for (const q of arr) {
          if (!q || typeof q !== "object") continue;
          if (typeof q.id !== "string" || typeof q.kind !== "string" || typeof q.title !== "string") {
            continue;
          }
          if (q.kind !== "choice" && q.kind !== "text") continue;
          valid.push(q as TaskQuestion);
        }
        return valid.length > 0 ? { kind: "structured_questions", questions: valid } : null;
      } catch {
        return null;
      }
    case "structured_plan":
      try {
        const d = JSON.parse(e.content);
        const text = typeof d?.plan_markdown === "string" ? d.plan_markdown.trim() : "";
        return text ? { kind: "structured_plan", planText: text } : null;
      } catch {
        return null;
      }
    case "structured_features":
      try {
        const d = JSON.parse(e.content);
        const r = featuresFromToolArgs(d);
        if (!r.ok) return null;
        return {
          kind: "structured_features",
          features: r.features,
          milestones: r.milestones,
          infrastructure: r.infrastructure,
          agentsMd: r.agentsMd,
          readinessManifest: r.readinessManifest,
        };
      } catch {
        return null;
      }
    case "structured_verdicts":
      try {
        const parsed = JSON.parse(e.content);
        const verdicts = verdictsFromToolArgs(parsed);
        if (verdicts.length === 0) return null;
        return {
          kind: "structured_verdicts",
          verdicts,
          sessionId: e.session_id ?? null,
        };
      } catch {
        return null;
      }
    case "structured_stability_impl_complete":
      // Surface in the Tasks-view reducer only so the union stays exhaustive;
      // the stability-test runner consumes the same event server-side.
      return { kind: "structured_stability_impl_complete", sessionId: e.session_id ?? null };
    case "structured_task_complete":
      // Tasks-view implementation completion signal. Always return a valid
      // event even when both fields are absent — the empty payload is
      // still a valid "I'm done" signal. `summary` is trimmed and capped
      // at 500 chars; `success_state` is whitelisted to the three known
      // values and otherwise dropped (reducer defaults to "success"
      // semantics via the absence of the field).
      try {
        const d = e.content ? JSON.parse(e.content) : {};
        let summary: string | undefined;
        if (typeof d?.summary === "string") {
          const trimmed = d.summary.trim();
          if (trimmed) summary = trimmed.slice(0, 500);
        }
        let successState: "success" | "partial" | "failure" | undefined;
        if (
          d?.success_state === "success" ||
          d?.success_state === "partial" ||
          d?.success_state === "failure"
        ) {
          successState = d.success_state;
        }
        return { kind: "structured_task_complete", summary, successState };
      } catch {
        // Even an unparseable payload counts as a completion signal so the
        // spinner doesn't get stuck if the model emits malformed JSON.
        return { kind: "structured_task_complete" };
      }
    case "queued":
    default:
      return null;
  }
}

/* ── HivemindProgressEvent → TaskEvent adapter ───────────── */

export function mapHivemindEventToTaskEvent(e: HivemindProgressEvent): TaskEvent | null {
  switch (e.event_type) {
    case "started":
      return { kind: "hm_started", jobId: e.job_id };
    case "round_started":
      return {
        kind: "hm_round_started",
        jobId: e.job_id,
        round: e.round,
        ...(e.models ? { models: e.models } : {}),
      };
    case "model_completed":
      return {
        kind: "hm_model_completed",
        jobId: e.job_id,
        modelId: e.model_id,
        round: e.round,
        inputTokens: e.input_tokens,
        outputTokens: e.output_tokens,
        durationMs: e.duration_ms,
        cost: e.cost,
      };
    case "model_failed":
      return {
        kind: "hm_model_failed",
        jobId: e.job_id,
        modelId: e.model_id,
        modelIdx: e.model_idx,
        round: e.round,
        error: e.message,
      };
    case "round_completed":
      return { kind: "hm_round_completed", jobId: e.job_id, round: e.round };
    case "completed":
      return { kind: "hm_completed", jobId: e.job_id };
    case "failed":
    case "error":
      return { kind: "hm_failed", jobId: e.job_id, message: e.message };
    case "cancelled":
      // Reuse hm_failed cleanup path: clears reviewProgress/activeReviewJobId,
      // sets a user-visible error, ratchets phase back to plan-ready.
      return { kind: "hm_failed", jobId: e.job_id, message: "Review cancelled" };
    default:
      return null;
  }
}

/* ── NurseEvent (Lifecycle variant) → TaskEvent adapter ───── */

import type { NurseEvent, NurseLifecyclePayload } from "../types/nurse";

/** Maps a `nurse-event` Lifecycle payload into the matching `TaskEvent`.
 *  Returns `null` for non-Lifecycle variants (StatusUpdate / Intervention /
 *  UserNotice) — those flow to dashboard / topbar consumers, not the
 *  conversation reducer. */
export function mapNurseEventToTaskEvent(e: NurseEvent): TaskEvent | null {
  if (e.event_type !== "Lifecycle") return null;
  const p = e as { event_type: "Lifecycle" } & NurseLifecyclePayload;
  switch (p.status) {
    case "started":
      return {
        kind: "nurse_started",
        interventionId: p.intervention_id,
        level: p.level,
        observation: p.observation,
        action: p.action,
        sessionId: p.session_id,
        t: p.timestamp,
      };
    case "reasoning":
      return {
        kind: "nurse_reasoning",
        interventionId: p.intervention_id,
        delta: p.reasoning_delta || "",
      };
    case "completed":
      return {
        kind: "nurse_completed",
        interventionId: p.intervention_id,
        fullReasoning: p.full_reasoning || undefined,
      };
    case "failed":
      return {
        kind: "nurse_failed",
        interventionId: p.intervention_id,
        error: p.error || undefined,
      };
    default:
      return null;
  }
}
