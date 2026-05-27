import type { HivemindProgressEvent } from "./types";

export interface ModelState {
  /** The unique reducer key, formatted as `${modelId}#${modelIdx}` when an
   *  instance index is known, or `${modelId}` when the event is from a
   *  legacy backend that didn't carry `model_idx`. Always equals the
   *  matching key in `RoundState.models` and the entry in `modelOrder`. */
  instanceKey: string;
  /** Human-facing model identifier (e.g. `anthropic/claude-sonnet-4`).
   *  Multiple `ModelState`s in the same round may share this value when
   *  the user configured duplicate reviewer instances. */
  modelId: string;
  status: "streaming" | "completed" | "failed";
  outputPreview: string;
  inputTokens?: number;
  outputTokens?: number;
  cost?: number;
  durationMs?: number;
  errorMessage?: string;
}

export interface RoundState {
  round: number;
  /** Keyed by `ModelState.instanceKey` so duplicate-model reviewer
   *  instances (same `model_id`, different `model_idx`) don't collapse. */
  models: Record<string, ModelState>;
  /** Ordered list of instance keys matching `models`. */
  modelOrder: string[];
}

export interface ContextState {
  status: "streaming" | "completed";
  preview: string;
}

export interface MergeState {
  round: number;
  status: "streaming" | "completed";
  preview: string;
}

/**
 * Reducer-internal phase for a Hivemind review.
 *
 * `between_rounds` is the short-lived state between the moment
 * `merge_completed` is observed for round N and `round_started` arrives
 * for round N+1 (or `completed` for the final round). It exists so the
 * UI can stop saying "Merging R{N}" the instant the backend says the
 * merge is done — without prematurely claiming we're already in round
 * N+1 (which would mislabel the pill and bump tabs before any model
 * actually streams). On the final round, `between_rounds` is overridden
 * by `completed` as soon as that event arrives.
 */
export type ReviewPhase =
  | "context"
  | "round"
  | "merge"
  | "between_rounds"
  | "completed";

export interface ReviewState {
  jobId: string;
  /** Lifecycle status. `cancelled` is distinct from `failed` — the user
   *  pressed Stop / the parent task was aborted, which is intent, not error.
   *  `skipped` is retained for the legacy case where a review was bypassed
   *  without ever starting (e.g. config gate). */
  status: "running" | "completed" | "failed" | "cancelled" | "skipped";
  phase: ReviewPhase;
  context?: ContextState;
  rounds: Record<number, RoundState>;
  roundOrder: number[];
  merges: Record<number, MergeState>;
  mergeOrder: number[];
  sourceLabel?: string;
  message?: string;
  startedAt: number;
  endedAt?: number;
}

export type HivemindReviewState = Record<string, ReviewState>;

const PREVIEW_MAX_CHARS = 600;

function trimPreview(prev: string, delta: string): string {
  const next = prev + delta;
  if (next.length <= PREVIEW_MAX_CHARS) return next;
  return next.slice(next.length - PREVIEW_MAX_CHARS);
}

export function attributionKeyFromEvent(evt: HivemindProgressEvent): string {
  if (evt.task_id) return `task:${evt.task_id}`;
  if (evt.swarm_id && evt.feature_id) return `swarm:${evt.swarm_id}:feat:${evt.feature_id}`;
  if (evt.swarm_id) return `swarm:${evt.swarm_id}:queen`;
  return `job:${evt.job_id}`;
}

function ensureReview(
  state: HivemindReviewState,
  key: string,
  jobId: string,
  sourceLabel?: string,
): ReviewState {
  const existing = state[key];
  if (existing && existing.jobId === jobId) return existing;
  return {
    jobId,
    status: "running",
    phase: "context",
    rounds: {},
    roundOrder: [],
    merges: {},
    mergeOrder: [],
    sourceLabel,
    startedAt: Date.now(),
  };
}

function ensureRound(review: ReviewState, round: number): RoundState {
  const existing = review.rounds[round];
  if (existing) return existing;
  const fresh: RoundState = { round, models: {}, modelOrder: [] };
  review.rounds[round] = fresh;
  if (!review.roundOrder.includes(round)) {
    review.roundOrder = [...review.roundOrder, round].sort((a, b) => a - b);
  }
  return fresh;
}

/** Format the reducer key for a reviewer instance. When the event lacks
 *  `model_idx` (older backend), we fall back to a bare `model_id` key —
 *  this collapses duplicate instances in legacy events, matching the
 *  pre-fix behaviour, but is the best we can do without an index. */
function instanceKeyOf(modelId: string, modelIdx: number | undefined): string {
  return modelIdx == null ? modelId : `${modelId}#${modelIdx}`;
}

function ensureModel(
  round: RoundState,
  modelId: string,
  modelIdx: number | undefined,
): ModelState {
  const instanceKey = instanceKeyOf(modelId, modelIdx);
  const existing = round.models[instanceKey];
  if (existing) return existing;
  const fresh: ModelState = {
    instanceKey,
    modelId,
    status: "streaming",
    outputPreview: "",
  };
  round.models[instanceKey] = fresh;
  if (!round.modelOrder.includes(instanceKey)) {
    round.modelOrder = [...round.modelOrder, instanceKey];
  }
  return fresh;
}

function ensureMerge(review: ReviewState, round: number): MergeState {
  const existing = review.merges[round];
  if (existing) return existing;
  const fresh: MergeState = { round, status: "streaming", preview: "" };
  review.merges[round] = fresh;
  if (!review.mergeOrder.includes(round)) {
    review.mergeOrder = [...review.mergeOrder, round].sort((a, b) => a - b);
  }
  return fresh;
}

export function applyHivemindEvent(
  state: HivemindReviewState,
  evt: HivemindProgressEvent,
): HivemindReviewState {
  const key = attributionKeyFromEvent(evt);

  const next: HivemindReviewState = { ...state };
  const base = ensureReview(next, key, evt.job_id, evt.source_label);
  const review: ReviewState = {
    ...base,
    rounds: { ...base.rounds },
    roundOrder: [...base.roundOrder],
    merges: { ...base.merges },
    mergeOrder: [...base.mergeOrder],
    context: base.context ? { ...base.context } : undefined,
  };
  if (evt.source_label && !review.sourceLabel) review.sourceLabel = evt.source_label;

  switch (evt.event_type) {
    case "started":
      review.status = "running";
      review.phase = "context";
      review.startedAt = base.startedAt ?? Date.now();
      break;
    case "context_started":
      review.phase = "context";
      review.context = { status: "streaming", preview: "" };
      break;
    case "context_chunk": {
      const ctx = review.context ?? { status: "streaming" as const, preview: "" };
      review.context = {
        status: "streaming",
        preview: trimPreview(ctx.preview, evt.delta ?? ""),
      };
      review.phase = "context";
      break;
    }
    case "context_completed":
      review.context = {
        status: "completed",
        preview: review.context?.preview ?? "",
      };
      review.phase = "round";
      break;
    case "round_started": {
      const round = ensureRound(review, evt.round || 1);
      // Seed a spinner row per scheduled reviewer instance so users see
      // immediate progress for buffered/non-streaming providers.
      // `ensureModel` is idempotent: any instance that already exists
      // (e.g. a fast `model_chunk` raced ahead of this event) is left in
      // place. Prefer the richer `model_instances` shape; fall back to
      // the legacy `models` array (treating its index as the implicit
      // `model_idx`) for back-compat with older backend builds.
      if (evt.model_instances != null && evt.model_instances.length > 0) {
        for (const inst of evt.model_instances) {
          ensureModel(round, inst.model_id, inst.model_idx);
        }
      } else if (evt.models != null && evt.models.length > 0) {
        for (let i = 0; i < evt.models.length; i++) {
          ensureModel(round, evt.models[i], i);
        }
      }
      review.phase = "round";
      review.status = "running";
      break;
    }
    case "model_chunk": {
      const round = ensureRound(review, evt.round || 1);
      const model = ensureModel(round, evt.model_id, evt.model_idx);
      model.outputPreview = trimPreview(model.outputPreview, evt.delta ?? "");
      if (model.status !== "streaming") model.status = "streaming";
      review.phase = "round";
      break;
    }
    case "model_completed": {
      const round = ensureRound(review, evt.round || 1);
      const model = ensureModel(round, evt.model_id, evt.model_idx);
      model.status = "completed";
      model.inputTokens = evt.input_tokens;
      model.outputTokens = evt.output_tokens;
      model.cost = evt.cost;
      model.durationMs = evt.duration_ms;
      break;
    }
    case "model_failed": {
      const round = ensureRound(review, evt.round || 1);
      const model = ensureModel(round, evt.model_id, evt.model_idx);
      model.status = "failed";
      model.errorMessage = evt.message;
      break;
    }
    case "round_completed":
      break;
    case "merge_started": {
      ensureMerge(review, evt.round || review.roundOrder[review.roundOrder.length - 1] || 1);
      review.phase = "merge";
      break;
    }
    case "merge_chunk": {
      const merge = ensureMerge(
        review,
        evt.round || review.roundOrder[review.roundOrder.length - 1] || 1,
      );
      merge.preview = trimPreview(merge.preview, evt.delta ?? "");
      review.phase = "merge";
      break;
    }
    case "merge_completed": {
      const merge = ensureMerge(
        review,
        evt.round || review.roundOrder[review.roundOrder.length - 1] || 1,
      );
      merge.status = "completed";
      // Transition out of "merge" the moment the backend says the merge
      // is done. Do NOT touch roundOrder — the UI label still reads the
      // just-merged round number off the last entry. The next
      // `round_started` will flip phase to "round" and bump
      // currentRound; on the final round, `completed` does it instead.
      // Don't regress phase if a higher-priority event has already moved
      // us off (e.g. an out-of-order `round_started` for N+1 already
      // arrived); only step forward from the in-flight `merge` state.
      if (review.phase === "merge") {
        review.phase = "between_rounds";
      }
      break;
    }
    // Structured per-Pi-event merge / context chunk events. The dock-side
    // `HivemindReviewLivePanel` is driven by the coalesced `*_chunk`
    // variants; these new types feed the Tasks-view inline bubble +
    // SwarmControl `hivemind-merge` agent stream and do not contribute to
    // the singleton store's derived state. Explicit no-op cases keep the
    // switch exhaustive and document that the new event types are NOT
    // "unknown".
    case "merge_text":
    case "merge_thinking":
    case "merge_tool_start":
    case "merge_tool_update":
    case "merge_tool_end":
    case "context_text":
    case "context_thinking":
    case "context_tool_start":
    case "context_tool_update":
    case "context_tool_end":
      break;
    case "completed":
      review.status = "completed";
      review.phase = "completed";
      review.message = evt.message;
      review.endedAt = Date.now();
      break;
    case "failed":
      review.status = "failed";
      review.message = evt.message;
      review.endedAt = Date.now();
      break;
    case "cancelled":
      // User-initiated stop. Distinct from `failed` so the UI can use a
      // neutral pill colour instead of red, and from `skipped` (which is
      // used for reviews that never started at all).
      review.status = "cancelled";
      review.message = evt.message;
      review.endedAt = Date.now();
      break;
    default:
      break;
  }

  next[key] = review;
  return next;
}

export function modelVerdictTone(m: ModelState): "running" | "ok" | "fail" {
  if (m.status === "streaming") return "running";
  if (m.status === "failed") return "fail";
  return "ok";
}
