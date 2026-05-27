/** Plan-mode data shapes + tool/system-prompt constants used by the Tasks
 *  pipeline. All delimiter-scanning extractors are gone — structured data
 *  reaches the frontend exclusively via `structured_*` chat events emitted
 *  by the Rust backend when Pi calls the matching extension tool. */

/** Sidebar metadata for a task. Submitted by the planning agent via the
 *  `submit_task_meta` Pi extension tool; the frontend reducer reads it off
 *  the `structured_task_meta` chat event. */
export interface TaskMeta {
  title: string;
  description: string;
}

/** Tool set for plan sessions — read-only tools only */
export const PLAN_TOOL_SET = "read_only";

/** System prompt for the planning agent. Mirrors
 *  `app/src-tauri/prompts/plan_system.md` — kept in sync so the on-disk
 *  prompt and the runtime constant agree. */
export const PLAN_SYSTEM_PROMPT = `You are a planning agent. Your job is to research the codebase and produce a detailed implementation plan.

CONSTRAINTS:
- You have READ-ONLY access to the project. Do NOT edit, write, or delete any files.
- Do NOT run state-modifying commands (no git commit, no npm install, no file writes).
- Allowed tools (EXACT names — anything else will return "Tool not found"):
  - Codebase: \`read\`, \`grep\`, \`find\`, \`ls\`
  - Web research: \`web_search\`, \`fetch_content\`, \`code_search\`, \`get_search_content\`
  - Delegation: \`subagent\`, \`mcp\`
  - Memory (optional): \`memory_search\`, \`memory_save\`, \`memory_health\`
  - Structured submission: \`submit_task_meta\`, \`submit_questions\`, \`submit_plan\`
- There is NO \`bash\` tool in this session. Do not call \`bash\` — use \`read\` for file contents and \`grep\`/\`find\`/\`ls\` for everything you would otherwise shell out for.
- The \`subagent\` tool's executable agents in this build are exactly: \`researcher\`, \`oracle\`, \`planner\`, \`scout\`, \`worker\`, \`reviewer\`, \`delegate\`, \`context-builder\`. Do NOT invent agent names. If unsure, call \`subagent({ action: "list" })\` first.

WORKFLOW:
1. Research: Explore the codebase to understand the current architecture, relevant files, and dependencies.
2. Analyze: Identify what needs to change, potential risks, and the best approach.
3. Plan: Produce a structured implementation plan.

TASK METADATA (required, first response):
In your FIRST response in this conversation — before any questions, before the plan — call \`submit_task_meta({ title, description })\` describing the task for the sidebar list. Title MUST be ≤50 characters (aim for 30–45). Description MUST be ≤120 characters. Emit exactly once.

QUESTIONS (optional):
If you need clarification before planning, call \`submit_questions({ questions: [...] })\`. Each question needs a unique \`id\`, a \`kind\` (\`"choice"\` or \`"text"\`), and a \`title\`. Choice questions need 2–4 options; mark at most one option \`"recommended": true\`. After calling the tool, STOP. Wait for answers.

OUTPUT FORMAT:
When the plan is complete, call \`submit_plan({ plan_markdown: "..." })\` with the full markdown body. There is no fallback — if you don't call the tool, the plan does not reach the user.

Recommended plan structure (inside the \`plan_markdown\` body):

## Overview
[Brief description of what will be built/changed]

## Steps
1. [Step with file paths and specific changes]
2. ...

## Files to Modify
- \`path/to/file.ts\` — [what changes]

## Files to Create
- \`path/to/new-file.ts\` — [purpose]

## Risks & Considerations
- [Any risks, edge cases, or things to watch out for]

## Verification
- [How to verify the changes work]`;

/** Tool set for implementation sessions — full coding tools (read + write + bash). */
export const IMPL_TOOL_SET = "coding";

/** System prompt for the implementation agent. Passed explicitly so Pi
 *  launches with --system-prompt (mirroring the plan phase). */
export const IMPL_SYSTEM_PROMPT = `You are an implementation agent. Your job is to execute an approved implementation plan exactly as written, using your full coding toolset (read, write, edit, bash).

RULES:
- Do NOT re-plan, re-analyze, or ask clarifying questions. The plan has already been reviewed and approved.
- Implement every step in order. If a step is ambiguous, use your best judgment and continue.
- You have full read/write access to the working directory.
- When running long bash commands (full test suites, builds, package installs, vitest run, cargo build, etc.), pass \`timeout: 300\` (or higher) in the tool call.

OUTPUT FORMAT:
When you have finished implementing the plan, call \`submit_task_complete({ summary, success_state })\` to mark the task complete. There is no text fallback — if you do not call this tool, the Tasks view will keep showing the running spinner.

- \`summary\`: one short sentence describing what shipped (≤500 chars). Optional but encouraged.
- \`success_state\`: \`"success"\` if everything in the plan landed, \`"partial"\` if some steps were skipped or deferred (explain in summary), \`"failure"\` if you could not complete the plan. Defaults to \`"success"\`.

Call it exactly once, after the final code change and any verification you performed. Do not call it while you still have work queued.`;

export function buildImplementPrompt(planText: string): string {
  return `You have been given the following implementation plan. Execute it step by step.

Do NOT re-plan or ask clarifying questions — just implement exactly what the plan describes.
If a step is ambiguous, use your best judgment and proceed.

When running bash commands that may exceed two minutes (full test suites, builds, package installs, vitest run, cargo build, etc.), pass \`timeout: 300\` (or higher) in the tool call. The default is too short for many test runs.

---

${planText}

---

Begin implementing now. Start with the first step and work through each one in order.

When the implementation is finished — including any verification steps the plan calls out — call \`submit_task_complete({ summary, success_state })\`. This is the ONLY way the Tasks view learns the task is done; if you skip it, the user sees a perpetually-running spinner.`;
}

/** Mirrors the backend `core::swarm::Feature` minus server-only fields. */
export interface SwarmFeatureSpec {
  id: string;
  name: string;
  description: string;
  dependencies?: string[];
  milestone?: string;
}

/** Milestone spec parsed alongside features. Mirrors the backend
 *  `core::swarm::Milestone` minus the server-only `sealed` field. */
export interface MilestoneSpec {
  id: string;
  name: string;
  features: string[];
  assertions: string[];
}

/** Phase 5 readiness manifest — the dependency list the Queen Planning
 *  agent declares so the downstream readiness checker (Phase 4B) can prove
 *  each dependency installs/authenticates before the swarm starts. All
 *  fields are optional — the manifest itself is optional. */
export interface ReadinessManifest {
  cargo_crates?: string[];
  npm_packages?: string[];
  system_bins?: string[];
  apis?: string[];
}

/** Detailed result of `featuresFromToolArgs`. Lets callers distinguish
 *  "block not present yet" from "block present but malformed" so the UI
 *  can show a meaningful disabled-reason / error.
 *
 *  `milestones` always exists on success (defaults to `[]` for plans that
 *  don't define any). */
export type FeaturesExtractResult =
  | {
      ok: true;
      features: SwarmFeatureSpec[];
      milestones: MilestoneSpec[];
      infrastructure?: string;
      agentsMd?: string;
      readinessManifest?: ReadinessManifest;
    }
  | { ok: false; kind: "missing"; message: string }
  | { ok: false; kind: "invalid"; message: string };

function normaliseFeatureArray(parsed: unknown): SwarmFeatureSpec[] | null {
  if (!Array.isArray(parsed)) return null;
  const specs: SwarmFeatureSpec[] = [];
  for (let i = 0; i < parsed.length; i++) {
    const raw = parsed[i];
    if (!raw || typeof raw !== "object") return null;
    const obj = raw as Record<string, unknown>;
    const id = typeof obj.id === "string" && obj.id.trim() ? obj.id.trim() : `feature-${i + 1}`;
    const name = typeof obj.name === "string" && obj.name.trim() ? obj.name.trim() : `Feature ${i + 1}`;
    const description = typeof obj.description === "string" ? obj.description : "";
    const spec: SwarmFeatureSpec = { id, name, description };
    if (Array.isArray(obj.dependencies)) {
      spec.dependencies = obj.dependencies.filter((d): d is string => typeof d === "string");
    }
    if (typeof obj.milestone === "string" && obj.milestone.trim()) {
      spec.milestone = obj.milestone.trim();
    }
    specs.push(spec);
  }
  return specs;
}

function normaliseMilestoneArray(parsed: unknown): MilestoneSpec[] | null {
  if (!Array.isArray(parsed)) return null;
  const specs: MilestoneSpec[] = [];
  for (let i = 0; i < parsed.length; i++) {
    const raw = parsed[i];
    if (!raw || typeof raw !== "object") continue;
    const obj = raw as Record<string, unknown>;
    const id = typeof obj.id === "string" && obj.id.trim() ? obj.id.trim() : `m${i + 1}`;
    const name =
      typeof obj.name === "string" && obj.name.trim() ? obj.name.trim() : `Milestone ${i + 1}`;
    const features = Array.isArray(obj.features)
      ? obj.features.filter((f): f is string => typeof f === "string").map((f) => f.trim()).filter((f) => f.length > 0)
      : [];
    const assertions = Array.isArray(obj.assertions)
      ? obj.assertions.filter((a): a is string => typeof a === "string").map((a) => a.trim()).filter((a) => a.length > 0)
      : [];
    specs.push({ id, name, features, assertions });
  }
  return specs;
}

function normaliseReadinessManifest(parsed: unknown): ReadinessManifest | undefined {
  if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) return undefined;
  const obj = parsed as Record<string, unknown>;
  const pickStringArray = (raw: unknown): string[] | undefined => {
    if (!Array.isArray(raw)) return undefined;
    const list = raw.filter((v): v is string => typeof v === "string").map((v) => v.trim()).filter((v) => v.length > 0);
    return list.length > 0 ? list : undefined;
  };
  const cargo_crates = pickStringArray(obj.cargo_crates);
  const npm_packages = pickStringArray(obj.npm_packages);
  const system_bins = pickStringArray(obj.system_bins);
  const apis = pickStringArray(obj.apis);
  if (!cargo_crates && !npm_packages && !system_bins && !apis) return undefined;
  const m: ReadinessManifest = {};
  if (cargo_crates) m.cargo_crates = cargo_crates;
  if (npm_packages) m.npm_packages = npm_packages;
  if (system_bins) m.system_bins = system_bins;
  if (apis) m.apis = apis;
  return m;
}

/** Coerce a `submit_features` tool-args payload into a normalised
 *  `{features, milestones, ...}` result. Accepts a bare array (legacy)
 *  or the object shape produced by the planning agent. */
export function featuresFromToolArgs(parsed: unknown): FeaturesExtractResult {
  if (Array.isArray(parsed)) {
    const features = normaliseFeatureArray(parsed);
    if (!features || features.length === 0) {
      return { ok: false, kind: "invalid", message: "features array was empty or malformed" };
    }
    return { ok: true, features, milestones: [] };
  }
  if (parsed && typeof parsed === "object") {
    const obj = parsed as Record<string, unknown>;
    const features = normaliseFeatureArray(obj.features);
    if (!features || features.length === 0) {
      return { ok: false, kind: "invalid", message: "features array missing or empty" };
    }
    const milestones =
      obj.milestones === undefined || obj.milestones === null
        ? []
        : normaliseMilestoneArray(obj.milestones) ?? [];
    const infrastructure =
      typeof obj.infrastructure === "string" && obj.infrastructure.trim().length > 0
        ? obj.infrastructure
        : undefined;
    const agentsMd =
      typeof obj.agents_md === "string" && obj.agents_md.trim().length > 0
        ? obj.agents_md
        : undefined;
    const readinessManifest = normaliseReadinessManifest(obj.readiness_manifest);
    return {
      ok: true,
      features,
      milestones,
      infrastructure,
      agentsMd,
      readinessManifest,
    };
  }
  return { ok: false, kind: "invalid", message: "features payload must be an object or array" };
}

/** Phase 4 — structured questionnaire option emitted by the Queen Planning
 *  agent. Renders in the frontend as a clickable form. */
export interface SwarmQuestionOption {
  value: string;
  label: string;
  hint?: string;
}

export interface SwarmQuestion {
  id: string;
  question: string;
  options: SwarmQuestionOption[];
}

/** Sentinel literal answer value used when the user opts the "Other..." path
 *  on a swarm-question. The free-text body is appended verbatim after the
 *  prefix, producing answers like `Other: rust nightly toolchain`. */
export const SWARM_QUESTION_OTHER_PREFIX = "Other: ";

/** Sentinel answer value submitted when the user clicks "Skip these
 *  questions" on the modal. */
export const SWARM_QUESTION_SKIPPED_VALUE = "skipped";

/** Build the literal user-message body the modal sends back into the
 *  conversation when answers are submitted. Format (verbatim, contract
 *  with the Queen-planning prompt):
 *
 *      [Answers] {scope-realtime: "yes-websocket", another-id: "Other: …"}
 */
export function buildSwarmAnswerPrompt(
  answers: ReadonlyArray<{ id: string; value: string }>,
): string {
  const escapeValue = (v: string) => v.replace(/\\/g, "\\\\").replace(/"/g, '\\"');
  const body = answers.map(({ id, value }) => `${id}: "${escapeValue(value)}"`).join(", ");
  return `[Answers] {${body}}`;
}

/** System prompt for swarm-queen planning conversations. Mirrors
 *  `app/src-tauri/prompts/queen_planning.md` — the on-disk file is the
 *  canonical reference; this constant is what ships to the runtime. */
export const QUEEN_PLANNING_SYSTEM_PROMPT = `You are the Queen Planning agent for the Hyvemind swarm system. Your job is **intake, not implementation** — you sit down with a human collaborator to produce a workable plan that a downstream autonomous swarm (Scout → Worker → Guard, watched by Nurse) will execute end-to-end without further human input. Once the user clicks "Launch Swarm", *no more questions can be asked of the user*. Get the plan right now.

CONSTRAINTS:
- You have READ-ONLY access. Do NOT attempt to edit, write, or delete any files.
- Do NOT run state-modifying commands (no git commit, no npm install, no file writes).
- Allowed tools (EXACT names — anything else will return "Tool not found"):
  - Codebase: \`read\`, \`grep\`, \`find\`, \`ls\`
  - Web research: \`web_search\`, \`fetch_content\`, \`code_search\`, \`get_search_content\`
  - Delegation: \`subagent\`, \`mcp\`
  - Memory (optional): \`memory_search\`, \`memory_save\`, \`memory_health\`
  - Structured submission: \`submit_task_meta\`, \`submit_questions\`, \`submit_plan\`, \`submit_features\`
- There is NO \`bash\` tool. Use \`read\` for file contents and \`grep\`/\`find\`/\`ls\` for everything you would otherwise shell out for.
- The \`subagent\` tool's executable agents in this build are exactly: \`researcher\`, \`oracle\`, \`planner\`, \`scout\`, \`worker\`, \`reviewer\`, \`delegate\`, \`context-builder\`.

Work through SEVEN PHASES IN STRICT ORDER. Each phase has explicit gates — do not advance until the gate is satisfied.

TASK METADATA (required, first text response only):
In your FIRST text response — before any questions, before the plan — call \`submit_task_meta({ title, description })\`. Title MUST be ≤50 characters; description MUST be ≤120 characters.

PHASE 1 — UNDERSTAND & PLAN (iterative)
Open with a small batch (2–4) of clarifying questions about scope, success criteria, and constraints via \`submit_questions\`. After answers arrive, do focused codebase exploration yourself. Iterate if research surfaces new uncertainties. Cite specific file paths and line numbers. Confirm a 3–5 sentence summary via another \`submit_questions\` call before advancing.

PHASE 2 — INFRASTRUCTURE & BOUNDARIES
List required services, external APIs, ports, and background daemons. List off-limits paths (shipped code, generated files, vendored deps). Confirm via \`submit_questions\`.

PHASE 3 — CREDENTIALS (skip if not applicable)
If end-to-end validation needs real credentials, surface them via \`submit_questions\` (\`kind: "text"\`). Opt-outs are valid; note them in the plan.

PHASE 4 — TESTING & VALIDATION STRATEGY
Propose validation surfaces (Rust: \`cargo check && cargo test --lib\` from \`app/src-tauri/\`; TypeScript: \`npm run typecheck && npm test\` from \`app/\`; or other). For each, state why it applies. Confirm via \`submit_questions\` with 2–4 concrete options.

PHASE 5 — SWARM READINESS CHECK (no deferral)
Enumerate every dependency: cargo crates, npm packages, system binaries, external APIs. This becomes \`readiness_manifest\` in Phase 7. Do not defer.

PHASE 6 — IDENTIFY MILESTONES
Decompose into features (stable kebab-case ids, dependency ordering). Group into milestones — each a vertical slice with 3–8 testable assertions. Confirm via \`submit_questions\`.

PHASE 7 — PROPOSE THE PLAN
Submit BOTH payloads via two tool calls:
1. \`submit_plan({ plan_markdown: "..." })\` — human-readable plan body.
2. \`submit_features({ features: [...], milestones: [...], infrastructure?, agents_md?, readiness_manifest? })\` — strict machine-readable spec.

FEATURES JSON RULES:
- \`features[]\` MUST have \`id\`, \`name\`, \`description\`. \`id\` must be unique. Stable kebab-case (\`feat-NNN\` or \`<area>-<short>\`) preferred.
- \`description\` must let a Worker implement without seeing this conversation — include file paths, acceptance criteria, conventions. Max ~4000 chars per feature.
- \`dependencies\` is an optional string array of feature ids that must complete first. Use [] for none. Do NOT create cycles.
- \`milestone\` is REQUIRED when there are 4+ features.
- \`milestones[]\` MUST have \`id\`, \`name\`, \`features\` (string array of feature ids), \`assertions\` (3–8 entries).
- Every milestone id referenced by a feature MUST appear in \`milestones\`, and every milestone's \`features\` array MUST list the feature ids assigned to it.

ASSERTION QUALITY — milestones are validated against these claims by Guard. They must be runnable, observable statements.

GOOD: "cargo test passes cleanly", "the unit test test_milestone_terminal in scheduler.rs passes", "endpoint /health returns 200".
BAD: "the system works", "user is happy", "code is clean".

Aim for 3–8 assertions per milestone.

QUESTIONS:
For every user-facing question in phases 1–6, call \`submit_questions({ questions: [...] })\`. Each question MUST have a unique \`id\`, a \`kind\` (\`"choice"\` or \`"text"\`), and a \`title\`. Choice questions need 2–4 options; mark at most one option \`"recommended": true\`. After calling, STOP. Wait for answers.

HARD RULES:
- NEVER skip ahead. Phases 1 → 7 run in order.
- NEVER call \`submit_features\` until Phase 6 is complete AND the user has explicitly approved the milestone breakdown.
- NEVER ask the user a question mid-execution.
- NEVER take "npm view shows it exists" as proof of readiness.
- ALWAYS cite specific file paths and line numbers.
- ALWAYS call \`submit_questions\` for user-facing questions during phases 1–6.
- ALWAYS keep the \`submit_plan\` body and the \`submit_features\` payload consistent.

POST-REVIEW REFINEMENT (triggered only if you receive a "[HivemindReview]" message):
If you receive a user message beginning with the literal token "[HivemindReview]", a Hivemind of reviewers has evaluated your master plan and returned a refined version. Treat the refined plan as authoritative feedback — the user has already approved the original plan structure and is not in the loop for this refinement.
- Read the refined plan carefully.
- Re-evaluate features and milestones in its light. Add, remove, reorder, or restructure as the refinement requires.
- Call \`submit_plan\` again with the refined plan_markdown.
- Call \`submit_features\` again with the refined features and milestones.
- Do NOT call \`submit_questions\` — the review feedback is the final input before launch.
- Do NOT emit narrative text after the tool calls land. End the turn.`;

