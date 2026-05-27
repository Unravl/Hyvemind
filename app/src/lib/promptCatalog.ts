/**
 * Frontend prompt catalog.
 *
 * Surfaces every prompt that lives on the React side (system prompts sent to
 * Pi sessions for the Tasks-view conversation, and the user-prompt template
 * builders for review/implement flows). The corresponding backend prompts —
 * bee agents, Hivemind reviewer base + stances, auto-commit title — are
 * served from the Rust `get_system_prompts` IPC command.
 *
 * Bodies are pulled from the same constants/builders the production code
 * paths use; user-prompt templates are rendered for display by calling the
 * builder with a literal placeholder (`[PLAN GOES HERE]` / `[REVIEWER
 * OUTPUTS GO HERE]`) so the catalog cannot drift from what the code
 * actually sends.
 */

import type { SystemPromptInfo } from "./types";
import {
  PLAN_SYSTEM_PROMPT,
  IMPL_SYSTEM_PROMPT,
  QUEEN_PLANNING_SYSTEM_PROMPT,
  buildImplementPrompt,
} from "./plan-mode";
import {
  REVIEW_CONTEXT_SYSTEM_PROMPT,
  REVIEW_MERGE_SYSTEM_PROMPT,
  buildContextGatherPrompt,
  buildMergePrompt,
} from "./review-mode";

const PLAN_PLACEHOLDER = "[PLAN GOES HERE]";
const REVIEWER_OUTPUTS_PLACEHOLDER = "[REVIEWER OUTPUTS GO HERE]";

export const FRONTEND_PROMPT_CATALOG: SystemPromptInfo[] = [
  {
    id: "tasks.plan.system",
    category: "Tasks",
    name: "Tasks Plan (system)",
    description:
      "System prompt for the Tasks-view planning conversation. Constrains the agent to read-only tools, lets it ask clarifying questions, and requires the final plan to be submitted via the `submit_plan` extension tool.",
    source: "app/src/lib/plan-mode.ts (PLAN_SYSTEM_PROMPT)",
    body: PLAN_SYSTEM_PROMPT,
  },
  {
    id: "tasks.implement.system",
    category: "Tasks",
    name: "Tasks Implement (system)",
    description:
      "System prompt for the Tasks-view implementation conversation. Gives the agent full coding tools and directs it to execute the approved plan exactly.",
    source: "app/src/lib/plan-mode.ts (IMPL_SYSTEM_PROMPT)",
    body: IMPL_SYSTEM_PROMPT,
  },
  {
    id: "tasks.implement.template",
    category: "Tasks",
    name: "Tasks Implement (user template)",
    description:
      "User-prompt template sent to the implementation agent once a plan is approved. The plan markdown is interpolated where you see [PLAN GOES HERE]; the agent runs each step in order until done.",
    source: "app/src/lib/plan-mode.ts (buildImplementPrompt)",
    body: buildImplementPrompt(PLAN_PLACEHOLDER),
  },
  {
    id: "bee.queen_planning",
    category: "Bee Agents",
    name: "Queen (planning)",
    description:
      "System prompt used by the conversational swarm planner — i.e. a Tasks-view conversation whose task has a `swarmId` set. The Queen Planning agent sits with the user to refine the goal and produce a decomposed feature/milestone plan that the autonomous Scout → Worker → Guard pipeline will then execute end-to-end. Mirrors `app/src-tauri/prompts/queen_planning.md`.",
    source: "app/src/lib/plan-mode.ts (QUEEN_PLANNING_SYSTEM_PROMPT)",
    body: QUEEN_PLANNING_SYSTEM_PROMPT,
  },
  {
    id: "hivemind.review_context.system",
    category: "Hivemind",
    name: "Review Context Gather (system)",
    description:
      "System prompt for the read-only Pi session that prepares input for a Hivemind review — reads files the plan touches and submits the plan plus relevant source via the `submit_review_prompt` extension tool.",
    source: "app/src/lib/review-mode.ts (REVIEW_CONTEXT_SYSTEM_PROMPT)",
    body: REVIEW_CONTEXT_SYSTEM_PROMPT,
  },
  {
    id: "hivemind.review_context.template",
    category: "Hivemind",
    name: "Review Context Gather (user template)",
    description:
      "User-prompt template handed to the context-gathering agent. The plan markdown is interpolated where you see [PLAN GOES HERE].",
    source: "app/src/lib/review-mode.ts (buildContextGatherPrompt)",
    body: buildContextGatherPrompt(PLAN_PLACEHOLDER),
  },
  {
    id: "hivemind.review_merge.system",
    category: "Hivemind",
    name: "Review Merge (system)",
    description:
      "System prompt for the merge agent that runs between Hivemind rounds. Synthesises feedback from every reviewer, rebuilds the plan with accepted changes, and emits a per-suggestion verdict JSON block.",
    source: "app/src/lib/review-mode.ts (REVIEW_MERGE_SYSTEM_PROMPT)",
    body: REVIEW_MERGE_SYSTEM_PROMPT,
  },
  {
    id: "hivemind.review_merge.template",
    category: "Hivemind",
    name: "Review Merge (user template)",
    description:
      "User-prompt template for the merge agent. The current plan is interpolated where you see [PLAN GOES HERE]; reviewer outputs are appended where you see [REVIEWER OUTPUTS GO HERE].",
    source: "app/src/lib/review-mode.ts (buildMergePrompt)",
    body: buildMergePrompt(PLAN_PLACEHOLDER, [
      { model: "reviewer-1", output: REVIEWER_OUTPUTS_PLACEHOLDER },
    ]),
  },
];
