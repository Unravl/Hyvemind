/**
 * hyvemind-handoff — local Pi extension that registers Hyvemind's
 * structured-output tools. Each tool's `execute` body is a no-op echo;
 * the Rust backend captures the model's tool-call `args` off the JSONL
 * `tool_execution_start` event and bypasses delimiter parsing entirely.
 *
 * See `app/src-tauri/src/pi/rpc.rs::HYVEMIND_EXTENSION_TOOLS` for the
 * authoritative list of names — it must match the `name` field below.
 *
 * Each schema mirrors a Rust/TS type in the codebase. When you change a
 * schema, update the matching deserialiser:
 *   - submit_handoff       → `core/handoff.rs` :: `WorkerHandoff`
 *   - submit_task_complete → `app/src/lib/taskReducer.ts` :: case "structured_task_complete"
 *   - submit_task_meta     → `app/src/lib/plan-mode.ts` :: `TaskMeta`
 *   - submit_questions     → `app/src/lib/questions.ts` :: `TaskQuestion[]`
 *   - submit_plan          → `app/src/lib/plan-mode.ts` :: PLAN body
 *   - submit_features      → `app/src/lib/plan-mode.ts` :: `FeaturesExtractResult`
 *   - submit_context       → `app/src-tauri/src/hivemind/engine.rs` :: context body
 *   - submit_review_prompt → `app/src/lib/taskRuntime.tsx` :: enriched review prompt
 *   - submit_stability_*   → `app/src-tauri/src/core/stability_test/runner.rs`
 */

import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";

/** Compact no-op echo used by every Hyvemind tool. The model's `args`
 *  payload has already been captured by the Rust backend off the
 *  `tool_execution_start` event; the tool's `execute` result is only
 *  surfaced back to the model so it knows the submission landed. */
async function echoReceived(_callId: string, _params: unknown) {
  return {
    content: [{ type: "text" as const, text: "Submission received." }],
    details: {},
  };
}

/** TypeBox-compatible JSON Schema for a single Worker-discovered issue.
 *  Mirrors `DiscoveredIssue` in `core/handoff.rs`. */
const discoveredIssueSchema = {
  type: "object",
  required: ["severity", "description"],
  additionalProperties: false,
  properties: {
    severity: { type: "string", enum: ["info", "warn", "error"] },
    description: { type: "string" },
    suggested_fix: { type: "string" },
  },
} as const;

const submitTaskCompleteSchema = {
  type: "object",
  additionalProperties: false,
  properties: {
    summary: { type: "string", maxLength: 500 },
    success_state: { type: "string", enum: ["success", "partial", "failure"] },
  },
} as const;

const submitHandoffSchema = {
  type: "object",
  required: [
    "feature_id",
    "run_id",
    "salient_summary",
    "what_was_implemented",
    "verification",
    "success_state",
  ],
  additionalProperties: false,
  properties: {
    feature_id: { type: "string" },
    run_id: { type: "string" },
    salient_summary: { type: "string" },
    what_was_implemented: { type: "string" },
    verification: { type: "string" },
    success_state: { type: "string", enum: ["success", "failure", "partial"] },
    discovered_issues: { type: "array", items: discoveredIssueSchema },
  },
} as const;

const submitTaskMetaSchema = {
  type: "object",
  required: ["title", "description"],
  additionalProperties: false,
  properties: {
    title: { type: "string", maxLength: 80 },
    description: { type: "string", maxLength: 200 },
  },
} as const;

const taskQuestionOptionSchema = {
  type: "object",
  required: ["id", "label"],
  additionalProperties: false,
  properties: {
    id: { type: "string" },
    label: { type: "string" },
    hint: { type: "string" },
    recommended: { type: "boolean" },
  },
} as const;

const taskQuestionSchema = {
  type: "object",
  required: ["id", "kind", "title"],
  additionalProperties: false,
  properties: {
    id: { type: "string" },
    kind: { type: "string", enum: ["choice", "text"] },
    title: { type: "string" },
    sub: { type: "string" },
    options: { type: "array", items: taskQuestionOptionSchema },
    placeholder: { type: "string" },
  },
} as const;

const submitQuestionsSchema = {
  type: "object",
  required: ["questions"],
  additionalProperties: false,
  properties: {
    questions: { type: "array", items: taskQuestionSchema, minItems: 1, maxItems: 5 },
  },
} as const;

const submitPlanSchema = {
  type: "object",
  required: ["plan_markdown"],
  additionalProperties: false,
  properties: {
    plan_markdown: { type: "string" },
  },
} as const;

/** Mirrors `SwarmFeatureSpec` in `app/src/lib/plan-mode.ts`. `fulfills`
 *  is the optional Phase-2 validation-contract link. */
const swarmFeatureSchema = {
  type: "object",
  required: ["id", "name", "description"],
  additionalProperties: true,
  properties: {
    id: { type: "string" },
    name: { type: "string" },
    description: { type: "string" },
    dependencies: { type: "array", items: { type: "string" } },
    milestone: { type: "string" },
    fulfills: { type: "array", items: { type: "string" } },
  },
} as const;

const milestoneSchema = {
  type: "object",
  required: ["id", "name", "features", "assertions"],
  additionalProperties: false,
  properties: {
    id: { type: "string" },
    name: { type: "string" },
    features: { type: "array", items: { type: "string" } },
    assertions: { type: "array", items: { type: "string" } },
  },
} as const;

const readinessManifestSchema = {
  type: "object",
  additionalProperties: false,
  properties: {
    cargo_crates: { type: "array", items: { type: "string" } },
    npm_packages: { type: "array", items: { type: "string" } },
    system_bins: { type: "array", items: { type: "string" } },
    apis: { type: "array", items: { type: "string" } },
  },
} as const;

const submitFeaturesSchema = {
  type: "object",
  required: ["features"],
  additionalProperties: false,
  properties: {
    features: { type: "array", items: swarmFeatureSchema, minItems: 1 },
    milestones: { type: "array", items: milestoneSchema },
    infrastructure: { type: "string" },
    agents_md: { type: "string" },
    readiness_manifest: readinessManifestSchema,
  },
} as const;

const submitContextSchema = {
  type: "object",
  required: ["summary"],
  additionalProperties: false,
  properties: {
    summary: { type: "string" },
  },
} as const;

const submitReviewPromptSchema = {
  type: "object",
  required: ["prompt"],
  additionalProperties: false,
  properties: {
    prompt: { type: "string" },
  },
} as const;

/** Stability-test tools — Phase 4. Schemas intentionally mirror the
 *  text-format documented in `prompts/stability_test_*.md`. */
const submitStabilityQuestionsSchema = submitQuestionsSchema;
const submitStabilityPlanSchema = submitPlanSchema;
const submitStabilityVerdictSchema = {
  type: "object",
  required: ["passed", "confidence", "summary"],
  additionalProperties: false,
  properties: {
    passed: { type: "boolean" },
    confidence: { type: "number", minimum: 0, maximum: 1 },
    issues: { type: "array", items: { type: "string" } },
    summary: { type: "string" },
  },
} as const;

const submitStabilityImplCompleteSchema = {
  type: "object",
  additionalProperties: false,
  properties: {},
} as const;

const submitScoutResultSchema = {
  type: "object",
  required: ["plan", "estimated_complexity", "risks"],
  additionalProperties: false,
  properties: {
    plan: { type: "string" },
    estimated_complexity: { type: "string", enum: ["low", "medium", "high"] },
    risks: { type: "array", items: { type: "string" } },
  },
} as const;

const guardAssertionSchema = {
  type: "object",
  required: ["status", "evidence"],
  additionalProperties: false,
  properties: {
    id: { type: "string" },
    status: { type: "string", enum: ["pass", "fail"] },
    evidence: { type: "string" },
    error: { type: "string" },
  },
} as const;

const submitGuardResultSchema = {
  type: "object",
  required: ["assertions"],
  additionalProperties: false,
  properties: {
    assertions: { type: "array", items: guardAssertionSchema },
  },
} as const;

const verdictItemSchema = {
  type: "object",
  required: ["reviewer_model", "suggestion", "verdict"],
  additionalProperties: false,
  properties: {
    reviewer_model: { type: "string" },
    suggestion: { type: "string" },
    verdict: { type: "string", enum: ["accepted", "rejected", "modified"] },
    severity: { type: "integer", minimum: 1, maximum: 5 },
    reason: { type: "string" },
    best_find: { type: "boolean" },
    co_reviewers: { type: "array", items: { type: "string" } },
  },
} as const;

const submitVerdictsSchema = {
  type: "object",
  required: ["verdicts"],
  additionalProperties: false,
  properties: {
    verdicts: { type: "array", items: verdictItemSchema },
  },
} as const;

/** Mirrors `StructuredReview` in `app/src-tauri/src/hivemind/review_schema.rs`. */
const submitReviewSchema = {
  type: "object",
  required: ["verdict"],
  additionalProperties: false,
  properties: {
    verdict: { type: "string" },
    issues: {
      type: "array",
      items: {
        type: "object",
        required: ["layer", "title", "file_path", "description"],
        additionalProperties: false,
        properties: {
          layer: { type: "integer", minimum: 1, maximum: 4 },
          title: { type: "string" },
          file_path: { type: "string" },
          description: { type: "string" },
          suggested_fix: { type: "string" },
        },
      },
    },
    strengths: { type: "array", items: { type: "string" } },
    key_takeaways: { type: "array", items: { type: "string" } },
  },
} as const;

interface RegisteredTool {
  name: string;
  label: string;
  description: string;
  parameters: unknown;
}

const TOOLS: RegisteredTool[] = [
  {
    name: "submit_handoff",
    label: "Submit handoff",
    description:
      "Submit your implementation handoff at the end of a feature. The Rust backend reads the args directly. Required fields: feature_id, run_id, salient_summary, what_was_implemented, verification, success_state.",
    parameters: submitHandoffSchema,
  },
  {
    name: "submit_task_complete",
    label: "Signal task complete",
    description:
      "Call this when you have finished implementing the approved plan. The Tasks view marks the task complete only when this tool fires — there is no text-scanning fallback. Optional fields: `summary` (one-line plain-English description of what shipped) and `success_state` (`success` | `partial` | `failure`, default `success`).",
    parameters: submitTaskCompleteSchema,
  },
  {
    name: "submit_task_meta",
    label: "Submit task metadata",
    description:
      "Submit the sidebar title and one-line description for this Task. Call this exactly once, in your first text response.",
    parameters: submitTaskMetaSchema,
  },
  {
    name: "submit_questions",
    label: "Submit clarifying questions",
    description:
      "Submit a batch of 1–5 clarifying questions for the user. After calling, STOP and wait for the user's answers.",
    parameters: submitQuestionsSchema,
  },
  {
    name: "submit_plan",
    label: "Submit implementation plan",
    description:
      "Submit the final implementation plan markdown. Plan body should be valid markdown.",
    parameters: submitPlanSchema,
  },
  {
    name: "submit_features",
    label: "Submit swarm features",
    description:
      "Submit the structured FEATURES JSON for a swarm-planning task. The shape mirrors `{features, milestones, infrastructure?, agents_md?, readiness_manifest?}`.",
    parameters: submitFeaturesSchema,
  },
  {
    name: "submit_context",
    label: "Submit Hivemind context summary",
    description:
      "Submit the gathered codebase context for a Hivemind review. The `summary` field is the full context body the reviewers will see.",
    parameters: submitContextSchema,
  },
  {
    name: "submit_review_prompt",
    label: "Submit Hivemind review prompt",
    description:
      "Submit the full enriched review prompt (plan + gathered source context) for a Tasks-view Hivemind review. The `prompt` field is the entire payload the reviewers will see — include the full plan first, then the `# Source Context` section.",
    parameters: submitReviewPromptSchema,
  },
  {
    name: "submit_stability_questions",
    label: "Submit stability-test questions",
    description:
      "Stability-test variant of submit_questions. Identical schema; isolated so the runner can route the events without ambiguity.",
    parameters: submitStabilityQuestionsSchema,
  },
  {
    name: "submit_stability_plan",
    label: "Submit stability-test plan",
    description:
      "Stability-test variant of submit_plan. Identical schema; isolated so the runner can route the events without ambiguity.",
    parameters: submitStabilityPlanSchema,
  },
  {
    name: "submit_stability_verdict",
    label: "Submit stability-test verdict",
    description:
      "Stability-test verifier's final verdict.",
    parameters: submitStabilityVerdictSchema,
  },
  {
    name: "submit_stability_impl_complete",
    label: "Signal stability-test implementation complete",
    description:
      "Call (with empty args) to signal the implementation phase has finished. The stability-test runner watches for this tool call to transition to the verifier phase.",
    parameters: submitStabilityImplCompleteSchema,
  },
  {
    name: "submit_scout_result",
    label: "Submit scout planning result",
    description:
      "Submit the Scout's planning output: implementation plan markdown, estimated complexity (low/medium/high), and a list of risks. The Rust backend reads the args directly — this is the only way to deliver a scout result.",
    parameters: submitScoutResultSchema,
  },
  {
    name: "submit_guard_result",
    label: "Submit guard validation result",
    description:
      "Submit the Guard's validation outcome: an array of assertion results, each with status (pass/fail), evidence, and an optional error message. The Rust backend reads the args directly.",
    parameters: submitGuardResultSchema,
  },
  {
    name: "submit_verdicts",
    label: "Submit Hivemind merge verdicts",
    description:
      "Hivemind merge orchestrator: submit the per-reviewer verdict array. Each entry tags a reviewer's suggestion as accepted/rejected/modified, with optional severity (1-5), reason, best_find flag, and co_reviewers list.",
    parameters: submitVerdictsSchema,
  },
  {
    name: "submit_review",
    label: "Submit Hivemind reviewer response",
    description:
      "Hivemind reviewer: submit your structured review of the plan-under-review. Required: verdict. Optional: issues (each with layer 1-4, title, file_path, description, suggested_fix), strengths, key_takeaways. Calling this tool is the only way to deliver a review.",
    parameters: submitReviewSchema,
  },
];

export default async function (pi: ExtensionAPI) {
  // `registerTool` is the documented Pi extension surface. We feed plain
  // JSON-Schema literals as `parameters`; Pi validates them with Ajv at
  // call time, so any model that returns args matching the schema will
  // produce a successful `tool_execution_start` event with `args` populated.
  for (const tool of TOOLS) {
    try {
      // Cast through `any` because Pi's TypeScript type for `parameters`
      // is a TypeBox `TSchema` instance, but the runtime accepts any
      // JSON-Schema-shaped object. Keeping the schemas as plain literals
      // avoids pulling TypeBox in as a runtime dependency.
      (pi as unknown as {
        registerTool: (def: {
          name: string;
          label: string;
          description: string;
          parameters: unknown;
          execute: typeof echoReceived;
        }) => void;
      }).registerTool({
        name: tool.name,
        label: tool.label,
        description: tool.description,
        parameters: tool.parameters,
        execute: echoReceived,
      });
    } catch (error) {
      // Don't fail the whole extension load if Pi rejects one schema —
      // surface to stderr so the build/runtime log shows it, and keep
      // registering the rest so the unaffected surfaces still work.
      console.error(
        `[hyvemind-handoff] failed to register ${tool.name}: ${error instanceof Error ? error.message : String(error)}`,
      );
    }
  }
  console.error(`[hyvemind-handoff] registered ${TOOLS.length} tools`);
}
