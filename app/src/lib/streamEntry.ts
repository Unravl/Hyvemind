import type { SwarmFeatureSpec } from "./plan-mode";
import type { TaskQuestion } from "./questions";
import type { ParsedVerdict } from "./review-mode";
import type { ImageAttachment, ToolCallState } from "./types";

export type StreamSurface = "task" | "swarm";
export type StreamAgent =
  | "scout"
  | "worker"
  | "guard"
  | "hivemind-context"
  // Tasks-view sessions:
  | "planning"         // Pi planning session (intake/questions/plan)
  | "implementation"   // Implementation hand-off session (post-plan, no swarm)
  | "hivemind-merge";  // Hivemind orchestrator merge session

export interface ReviewKind {
  phase: "context" | "merge";
  round?: number;
  reviewId: string;
  sessionId: string;
}

export interface MarkerUsage {
  input: number;
  output: number;
  contextPercent: number;
  cost: number;
  tokPerSec?: number;
}

interface BaseEntry {
  id: string;
  surface: StreamSurface;
  /** Pre-formatted display string (legacy / fallback). Kept so historical
   *  on-disk task messages and mock data that predate `createdAt` still
   *  render a static timestamp. New code paths always set `createdAt`,
   *  which takes priority via `<RelativeTime/>`. */
  t?: string;
  /** Epoch-ms timestamp of when the underlying message / activity item
   *  was first created. Drives the live-updating relative-time label in
   *  ActivityStream. */
  createdAt?: number;
}

export interface ChatBubbleEntry extends BaseEntry {
  kind: "chat_bubble";
  who: "user" | "asst";
  text: string;
  model?: string;
  reasoning?: string;
  reasoningStartedAt?: number;
  reasoningDurationMs?: number;
  tools?: ToolCallState[];
  images?: ImageAttachment[];
  steered?: boolean;
  agent?: StreamAgent;
  featureId?: string;
  sessionId?: string;
  reviewKind?: ReviewKind;
  error?: string;
  /** Per-reviewer verdicts attached to a Hivemind merge bubble. Sourced
   *  from the merge orchestrator's `submit_verdicts` tool call (delivered
   *  via the `structured_verdicts` chat-event). Only populated on merge
   *  entries; unused for user/asst bubbles. */
  verdicts?: ParsedVerdict[] | null;
}

export interface SessionMarkerEntry extends BaseEntry {
  kind: "session_marker";
  phase: "start" | "end";
  label: string;
  sessionId?: string;
  model?: string;
  agentModel?: string;
  thinking?: string;
  agent?: StreamAgent;
  featureId?: string;
  usage?: MarkerUsage;
  success?: boolean;
}

export interface PlanEntry extends BaseEntry {
  kind: "plan";
  planText: string;
  features?: SwarmFeatureSpec[];
}

export interface QuestionsEntry extends BaseEntry {
  kind: "questions";
  questions: TaskQuestion[];
}

export interface CompleteEntry extends BaseEntry {
  kind: "complete";
  /** Optional human-readable summary supplied by the implementation agent's
   *  `submit_task_complete` tool call. Rendered as a small dim line under
   *  the "Task Complete" pill. */
  text?: string;
  /** Outcome reported by the agent. Drives the chip's colour: `success` keeps
   *  the emerald default, `partial` switches to amber, `failure` switches to
   *  red. Absent for legacy complete entries inserted before this field
   *  existed. */
  successState?: "success" | "partial" | "failure";
}

export interface ErrorEntry extends BaseEntry {
  kind: "error";
  agent?: StreamAgent;
  featureId?: string;
  sessionId?: string;
  message: string;
}

/** Inline Nurse intervention card rendered in the conversation stream.
 *  Streams in stages: `started` → optional `reasoning` chunks →
 *  `completed` / `failed`. The renderer shows a honey-accented card with
 *  the observation as the bold first line, the action as the second
 *  line, and the streaming reasoning collapsed underneath. While
 *  `status` is `started` or `reasoning` the card pulses subtly. */
export interface NurseEntry extends BaseEntry {
  kind: "nurse";
  interventionId: string;
  level: string;
  observation: string;
  action: string;
  reasoning?: string;
  status: "started" | "reasoning" | "completed" | "failed";
  error?: string;
  sessionId?: string;
  featureId?: string;
}

export type StreamEntry =
  | ChatBubbleEntry
  | SessionMarkerEntry
  | PlanEntry
  | QuestionsEntry
  | CompleteEntry
  | ErrorEntry
  | NurseEntry;

export interface ActiveSession {
  sessionId: string | null;
  model: string | null;
  agent?: StreamAgent | null;
  role?: "scout" | "worker" | "guard" | null;
}

/** Return the visible text for a chat bubble. Tool args never appear in the
 *  text stream (they're captured server-side off `tool_execution_start`), so
 *  this is now a thin pass-through that just trims. */
export function displayTextOf(entry: StreamEntry): string {
  if (entry.kind !== "chat_bubble") return "";
  if (entry.who === "user") return entry.text || "";
  return (entry.text || "").trim();
}
