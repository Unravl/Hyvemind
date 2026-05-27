/** Heading the context-gather agent uses to delimit the source bundle inside
 *  an enriched prompt. Reused by the merge agent so it sees the same source
 *  the reviewers used — see `extractSourceContext`. */
export const SOURCE_CONTEXT_HEADING = "# Source Context";

/** System prompt for the context-gathering agent (read-only Pi session) */
export const REVIEW_CONTEXT_SYSTEM_PROMPT = `You are a source context gatherer preparing input for a multi-model code review. Your job is to read an implementation plan and gather the source code reviewers need to evaluate the plan well.

CONSTRAINTS:
- You have READ-ONLY access to the project. Do NOT edit, write, or delete any files.
- Allowed tools: read, grep, find, ls (codebase) · web_search, fetch_content, code_search, get_search_content (web research) · subagent (delegate deeper research) · mcp (configured MCP servers) · bash for read-only commands (cat, head, etc.)
- Submission tool: \`submit_review_prompt({ prompt })\` — the only way the host receives your output.

GUIDING PRINCIPLE:
Reviewers cannot critique code they cannot see. Be generous with surrounding context — they need enough source to verify claims, understand call sites, spot edge cases, and reason about side effects. But do NOT paste entire large files verbatim: unrelated code burns reviewer attention and dilutes focus from what actually matters. Aim for "everything a thoughtful reviewer would ask to see, and nothing they wouldn't."

WORKFLOW:
1. Read the plan carefully. Identify every source file the plan references or modifies.
2. For each MODIFIED file, choose one of two strategies:
   - INCLUDE THE FULL FILE only when the file is UNDER 1000 LINES. Below that bar, pasting it whole adds value without flooding the reviewer with unrelated code.
   - INCLUDE A FOCUSED REGION whenever the file is 1000 LINES OR MORE (HARD RULE — no exceptions). Also prefer a focused region for any file where the full content would mostly be code unrelated to the change. A focused region includes:
     * the functions, types, methods, or blocks the plan modifies,
     * the signatures (and short bodies, where useful) of callees and helpers those modified blocks reach into,
     * the type and trait definitions the modified code depends on,
     * sibling logic that interacts with the changed code (e.g. other branches of the same state machine, related event handlers, mirroring read/write paths),
     * any nearby invariants, guards, comments, or assertions a reviewer would need to judge correctness.
   You are NOT trying to be minimal — you are trying to be focused. For sub-1000-line files err on giving more surrounding context than strictly necessary; for ≥1000-line files err on tighter focus. The failure mode to avoid in both directions is a reviewer who can see the change but cannot see the function it calls.
3. For files only REFERENCED for ambient context (types, APIs, usage patterns the plan cites), include the specific items plus their immediate surrounding context — not the full file.

HANDLING LARGE FILES (HARD RULE):
Files of 1000 lines or more MUST be served as focused excerpts. The Read tool returns at most ~2000 lines / ~100KB per call. Do NOT walk a large file in successive offset chunks just to obey "full file" — that is precisely the failure mode this prompt is designed to prevent, and it will cause the context-gather session to be aborted with an output-size error. Instead, use grep or a directed scan to locate the regions the plan touches, then read those regions (and their surrounding context) with offset/limit.

SUBMISSION:
Call \`submit_review_prompt({ prompt })\` exactly once with the full payload. The \`prompt\` body MUST include the full plan first, then a \`# Source Context\` section, then the gathered files. The merge agent extracts the source bundle by splitting on the \`# Source Context\` heading, so it must appear inside the payload on its own line. There is no fallback path — your text response is ignored; only the tool args reach the host.

PAYLOAD SHAPE (inside the \`prompt\` arg):

# Implementation Plan

<full plan text here, unmodified>

# Source Context

=== FILE: <relative/path/to/file> ===
<file content or relevant excerpts>

=== FILE: <relative/path/to/another/file> ===
<file content or relevant excerpts>

<... more files ...>

IMPORTANT:
- Include the full plan text in the payload — reviewers need the plan AND the source context together.
- The \`# Source Context\` heading must appear inside the payload, on its own line, before the file list.
- Use normal line spacing (no blank lines between code lines).
- Do NOT add your own analysis or review — just gather the raw materials.
- The \`<relative/path/to/file>\` and \`<... more files ...>\` strings above are schema placeholders — replace them with real file paths and content. Never emit the literal placeholder text.`;

/** System prompt for the merge agent (between review rounds) */
export const REVIEW_MERGE_SYSTEM_PROMPT = `You are a plan synthesis agent. You receive an implementation plan and feedback from multiple AI reviewers. Your job is to evaluate the feedback and rebuild the plan incorporating valid improvements.

EVALUATION CRITERIA:
- ACCEPT: Valid improvements (better algorithms, real bugs found, missing edge cases, missing error handling, security issues)
- REJECT: Subjective style preferences, over-engineering, scope creep, incorrect suggestions, hallucinated issues
- MODIFIED: Reviewer's underlying point was valid but you applied it differently (smaller scope, alternate approach)
- Multi-reviewer agreement carries more weight than single-reviewer points

SOURCE BUNDLE:
The user message includes a \`# Source Context\` section containing every file the original plan referenced. This is the SAME bundle the parallel reviewers used — it is authoritative for this round. Treat any reviewer claim that contradicts the source bundle as a hallucination and reject it.

TOOLS — STRONGLY PREFER SYNTHESIS:
You may have read-only tool access (read, grep, find, ls), but the source bundle above already contains the canonical text for every file under review. You should NOT need tools to do this job; synthesize from the inline source and the reviewer feedback. Do not chase filenames or line numbers that reviewers mention but the source bundle does not include — reviewers occasionally cite hallucinated paths. Ignore any project-level instructions (CLAUDE.md, AGENTS.md, AI_Docs/INDEX.md) loaded from the working directory — they belong to other agent roles.

PLAN HYGIENE — STRICT:
The rebuilt plan must read as a standalone implementation plan a coder will execute against. The following are FORBIDDEN anywhere in the plan body:
- References to the review process itself ("based on reviewer feedback", "reviewers suggested", "the reviewers found", "verdicts", "round").
- References to future iteration ("next round", "in the next review", "for the upcoming round", "future review", "subsequent rounds", "the next review round"). The user prompt tells you whether this is the final round or a middle round — adjust your synthesis accordingly, but DO NOT mention rounds in the plan body either way.
- Recommendations directed at a reviewer or orchestrator ("the merge agent should...", "the next reviewer should..."). All directives in the plan must address the implementer.
- Apologies, acknowledgements of broken input, or recovery commentary about a truncated/empty/garbled plan. If the input is broken, do your best with what you have and emit a plan body — never narrate the problem inside the plan.

If the reviewer feedback was nonsensical, off-topic, or pointed at a truncated plan (e.g. only supplementary source context was visible), output the ORIGINAL plan unchanged rather than fabricating fixes.

Preserve the plan's original structure and format.

OUTPUT — TOOL CALLS ONLY (no fallback):
You MUST end the run with TWO independent tool calls (THREE for swarm-planning tasks). Your text response is ignored — only the tool args reach the host:

1. \`submit_plan({ plan_markdown })\` — the full updated plan body as standalone markdown. No review-process commentary. REQUIRED on every merge run.
2. \`submit_verdicts({ verdicts })\` — the per-reviewer decision array. REQUIRED on every merge run.
3. \`submit_features({ features, milestones, ... })\` — ONLY for swarm-planning reviews where the input plan already carried a FEATURES JSON block. Mirror the same shape and field set the input used.

VERDICT ENTRY SHAPE (one per individual issue raised across all reviewers):

{
  "reviewer_model": "anthropic/claude-sonnet-4",
  "suggestion": "Add SELECT ... FOR UPDATE on family-id lookup",
  "verdict": "accepted",
  "severity": 4,
  "reason": "Real race; agreed by 2/3 reviewers.",
  "co_reviewers": ["openai/gpt-5"],
  "best_find": true
}

RULES:
- Use the EXACT label shown in the reviewer section header ("### Reviewer N: <label>") for \`reviewer_model\` — copy verbatim, including any " #2"/" #3" instance suffix used to disambiguate duplicate instances of the same model.
- Emit one verdict per individual issue raised. If two reviewers raise the same issue, emit it once attributed to the most prominent reviewer; mention the agreement in \`reason\` and list the others in \`co_reviewers\`.
- \`verdict\` MUST be one of: \`"accepted"\` | \`"rejected"\` | \`"modified"\`.
- \`severity\` is an integer 1 (low) … 5 (critical). Omit or set null if unsure.
- \`co_reviewers\` is OPTIONAL. Use the EXACT canonical form shown in the section headers.
- \`best_find\` is OPTIONAL. Set it to true on AT MOST ONE verdict per merge run — the single most impactful finding of the round. Omit entirely if nothing stands out.
- The plan body in \`submit_plan\` must be the standalone improved plan with no commentary.`;

/**
 * Build the prompt sent to the context-gathering Pi agent.
 */
export function buildContextGatherPrompt(planText: string): string {
  return `Gather source context for the following implementation plan. Read every file the plan references, then submit the plan + source context by calling the \`submit_review_prompt\` tool. There is no fallback path — only the tool args reach the host.

---

${planText}`;
}

/**
 * Pull the `# Source Context` body out of an enriched prompt. The context-
 * gather Pi submits a `prompt` arg containing `# Implementation Plan`
 * followed by `# Source Context`; this helper returns the source bundle on
 * its own so it can be re-attached to the merge prompt. Returns null when
 * input is null/empty or the heading is absent.
 */
export function extractSourceContext(enriched: string | null | undefined): string | null {
  if (!enriched) return null;
  const idx = enriched.indexOf(SOURCE_CONTEXT_HEADING);
  if (idx === -1) return null;
  const body = enriched.slice(idx + SOURCE_CONTEXT_HEADING.length).trim();
  return body.length > 0 ? body : null;
}

/**
 * Compose the plan text sent to round-N reviewers. Round 1's `currentPlan`
 * is the enriched bundle (plan + `# Source Context`) and is returned as-is.
 * Round 2+'s `currentPlan` is the merge agent's extracted plan, which has
 * no source — append the `# Source Context` block from `enrichedPrompt` so
 * round 2+ reviewers see the same canonical source the round 1 reviewers
 * had. Idempotent: never duplicates the source if it's already present.
 */
export function buildReviewerPlan(
  currentPlan: string,
  enrichedPrompt: string | null | undefined,
): string {
  if (currentPlan.includes(SOURCE_CONTEXT_HEADING)) return currentPlan;
  const source = extractSourceContext(enrichedPrompt);
  if (!source) return currentPlan;
  return `${currentPlan.trimEnd()}\n\n${SOURCE_CONTEXT_HEADING}\n\n${source}\n`;
}

/**
 * Disambiguate duplicate reviewer labels in a single round so the merge agent
 * can attribute verdicts to a specific instance of a model.
 *
 * For each step (in order), the base label is `${provider}/${model_id}`. If
 * `(provider, model_id)` is unique within the round, return the base label
 * unchanged. If it appears multiple times, the first occurrence stays bare
 * (to preserve compatibility with single-instance reviewer_model strings
 * already in the DB) and subsequent occurrences receive a 1-based instance
 * suffix: ` #2`, ` #3`, …
 *
 * Returned array length equals input length, order preserved.
 */
export function dedupeReviewerLabels(
  steps: { provider: string; model_id: string }[],
): string[] {
  // First pass: count how many times each base label appears.
  const counts = new Map<string, number>();
  for (const s of steps) {
    const base = `${s.provider}/${s.model_id}`;
    counts.set(base, (counts.get(base) ?? 0) + 1);
  }
  // Second pass: assign labels with running index per duplicated key.
  const seen = new Map<string, number>();
  const out: string[] = [];
  for (const s of steps) {
    const base = `${s.provider}/${s.model_id}`;
    if ((counts.get(base) ?? 0) <= 1) {
      out.push(base);
      continue;
    }
    const n = (seen.get(base) ?? 0) + 1;
    seen.set(base, n);
    out.push(n === 1 ? base : `${base} #${n}`);
  }
  return out;
}

/**
 * Build the prompt sent to the merge Pi agent after a review round.
 *
 * `sourceContext`, when provided, is inlined as a `# Source Context` section
 * between the current plan and the reviewer feedback so the merge agent has
 * the same canonical source bundle the reviewers were given.
 */
export function buildMergePrompt(
  planText: string,
  outputs: { model: string; output: string }[],
  sourceContext?: string | null,
): string {
  const feedbackSections = outputs
    .map(
      (o, i) =>
        `### Reviewer ${i + 1}: ${o.model}\n\n${o.output}`,
    )
    .join("\n\n---\n\n");

  const sourceBlock = sourceContext
    ? `\n\n# Source Context (same bundle the reviewers used — authoritative for this round)\n\n${sourceContext}\n`
    : "";

  return `Here is the current implementation plan:

# Plan Under Review
${planText}
# End Plan Under Review
${sourceBlock}
Here is the feedback from ${outputs.length} reviewer(s):

${feedbackSections}

Synthesize the merged plan from the plan, the source bundle above, and the reviewer feedback. The source bundle is your ground truth — treat any reviewer claim that contradicts it as suspect. Submit your output exclusively via the \`submit_plan\` + \`submit_verdicts\` tool calls (and \`submit_features\` for swarm-planning reviews). Text responses are ignored.`;
}

/** Configuration for a single review round */
export interface RoundConfig {
  models: {
    id: string;
    provider: string;
    thinking: string;
    max_tokens: number;
    /** Stored context window in tokens, captured at model selection time
     *  from `ModelDetail.context_length` (or the static catalog fallback). */
    context_window?: number;
    /** Stored output cap in tokens, captured at model selection time. */
    max_output?: number;
    /** Per-model sampling overrides. Both are optional — if absent the
     *  provider's default applies. */
    temperature?: number;
    top_p?: number;
  }[];
  timeout: number;
}

/**
 * Parse the rounds_config JSON string from a HivemindSummary into RoundConfig[].
 * Wraps JSON.parse in a try/catch; on failure returns an empty array. Use
 * `parseRoundsConfigWithStatus` when the caller needs to surface a warning.
 */
export function parseRoundsConfig(json: string): RoundConfig[] {
  return parseRoundsConfigWithStatus(json).rounds;
}

/**
 * Same as `parseRoundsConfig` but also returns an `{ ok, error? }` flag so
 * the UI can display a warning badge for corrupted `rounds_config` blobs.
 * Returns `rounds: []` and surfaces the raw JSON via `console.error` on
 * parse failure — we explicitly do not crash here, since older or
 * corrupted configs should remain visible enough to fix from the UI.
 */
export function parseRoundsConfigWithStatus(json: string): {
  rounds: RoundConfig[];
  ok: boolean;
  error?: string;
} {
  let parsed: any;
  try {
    parsed = JSON.parse(json);
  } catch (e) {
    const msg = e instanceof Error ? e.message : String(e);
    // eslint-disable-next-line no-console
    console.error("parseRoundsConfig: failed to parse rounds_config JSON", { error: msg, raw: json });
    return { rounds: [], ok: false, error: msg };
  }
  if (!Array.isArray(parsed)) {
    return { rounds: [], ok: false, error: "rounds_config is not an array" };
  }
  const rounds: RoundConfig[] = parsed.map((round: any) => ({
    models: Array.isArray(round?.models)
      ? round.models.map((m: any) => {
          const out: RoundConfig["models"][number] = {
            id: m?.id || "",
            provider: m?.provider || "",
            thinking: m?.thinking || "none",
            max_tokens: typeof m?.max_tokens === "number" ? m.max_tokens : 16384,
          };
          if (typeof m?.context_window === "number" && m.context_window > 0) {
            out.context_window = m.context_window;
          }
          if (typeof m?.max_output === "number" && m.max_output > 0) {
            out.max_output = m.max_output;
          }
          return out;
        })
      : [],
    timeout: typeof round?.timeout === "number" ? round.timeout : 300,
  }));
  return { rounds, ok: true };
}

// ---------------------------------------------------------------------------
// Merge-prompt token budget
// ---------------------------------------------------------------------------

/**
 * Approximate token count using the same 4:1 char-to-token ratio as the
 * Rust engine (`engine.rs` reviewer-loop budget). Not accurate for all
 * encodings (CJK, emoji, dense code) but consistent with the backend
 * heuristic — which is what matters so the two budgets agree.
 */
export function estimateTokens(text: string): number {
  return Math.ceil(text.length / 4);
}

export interface TruncateResult {
  prompt: string;
  truncated: boolean;
  droppedReviewers: string[];
  sourceTruncatedTo: number | null;
  perReviewerCap: number | null;
  planTruncated: boolean;
}

/** Sentinel appended when the plan itself had to be truncated. */
const PLAN_TRUNCATION_SENTINEL = "\n\n[…plan truncated to fit context window…]\n";

/** If the truncated plan ends inside an open ``` fence, append a closing fence. */
function closeOpenCodeFence(s: string): string {
  const matches = s.match(/```/g);
  if (matches && matches.length % 2 === 1) {
    return s + "\n```\n";
  }
  return s;
}

/**
 * Build a merge prompt that fits within the orchestrator's token budget.
 *
 * Priority order: **plan > source context > reviewer outputs**. We always
 * keep the plan in full when possible (it is the document being revised),
 * fall back to trimming the source bundle from the tail (high-priority
 * files appear first by convention), then per-reviewer caps, then drop
 * oldest reviewers, and finally truncate the plan itself with a sentinel.
 *
 * `maxTokens` is the orchestrator's context window; `outputReservationTokens`
 * is the budget reserved for the model's response (clamped to <=25% of
 * context so a large `max_output` doesn't starve the input budget).
 */
export function truncateMergePrompt(
  planText: string,
  outputs: { model: string; output: string }[],
  sourceContext: string | null,
  maxTokens: number,
  opts?: { outputReservationTokens?: number; safetyFactor?: number },
): TruncateResult {
  const safetyFactor = opts?.safetyFactor ?? 0.8;
  const requestedReservation = opts?.outputReservationTokens ?? 16_384;
  // Clamp reservation so models advertising large max_output (e.g. 60k
  // on a 128k model) don't starve the input budget.
  const reservation = Math.max(
    1024,
    Math.min(requestedReservation, Math.floor(maxTokens * 0.25)),
  );
  // Always leave at least an 8k minimum viable input budget. If the model's
  // context is too small even for the plan alone, we still produce a prompt
  // (truncated) rather than refusing.
  const rawBudget = Math.floor(maxTokens * safetyFactor) - reservation;
  const inputBudget = Math.max(8_192, rawBudget);

  // Local mutable working copies of each input component.
  let workingPlan = planText;
  let workingSource = sourceContext ?? null;
  let workingOutputs = outputs.map((o) => ({ ...o }));
  let droppedReviewers: string[] = [];
  let sourceTruncatedTo: number | null = null;
  let perReviewerCap: number | null = null;
  let planTruncated = false;

  const buildAndMeasure = () => {
    const prompt = buildMergePrompt(workingPlan, workingOutputs, workingSource);
    return { prompt, tokens: estimateTokens(prompt) };
  };

  let { prompt, tokens } = buildAndMeasure();
  if (tokens <= inputBudget) {
    return {
      prompt,
      truncated: false,
      droppedReviewers,
      sourceTruncatedTo,
      perReviewerCap,
      planTruncated,
    };
  }

  // Step 1: trim source context from the end (preserve leading files).
  if (workingSource) {
    // Binary-search for the largest source-context prefix that fits when
    // combined with the plan and reviewer outputs.
    const trySource = (chars: number): { prompt: string; tokens: number } => {
      const trimmed = (sourceContext ?? "").slice(0, chars);
      const localPrompt = buildMergePrompt(workingPlan, workingOutputs, trimmed || null);
      return { prompt: localPrompt, tokens: estimateTokens(localPrompt) };
    };
    let lo = 0;
    let hi = (sourceContext ?? "").length;
    let bestFit = -1;
    while (lo <= hi) {
      const mid = Math.floor((lo + hi) / 2);
      const probe = trySource(mid);
      if (probe.tokens <= inputBudget) {
        bestFit = mid;
        lo = mid + 1;
      } else {
        hi = mid - 1;
      }
    }
    if (bestFit >= 0) {
      workingSource = bestFit > 0 ? (sourceContext ?? "").slice(0, bestFit) : null;
      sourceTruncatedTo = bestFit;
    } else {
      workingSource = null;
      sourceTruncatedTo = 0;
    }
    ({ prompt, tokens } = buildAndMeasure());
    if (tokens <= inputBudget) {
      return {
        prompt,
        truncated: true,
        droppedReviewers,
        sourceTruncatedTo,
        perReviewerCap,
        planTruncated,
      };
    }
  }

  // Step 2: per-reviewer trailing cap. Start at 32k tokens, halve until fit or floor at 4k.
  for (let cap = 32_768; cap >= 4_096; cap = Math.floor(cap / 2)) {
    const capChars = cap * 4;
    workingOutputs = outputs
      .filter((o) => !droppedReviewers.includes(o.model))
      .map((o) => ({
        model: o.model,
        output: o.output.length > capChars ? o.output.slice(0, capChars) : o.output,
      }));
    perReviewerCap = cap;
    ({ prompt, tokens } = buildAndMeasure());
    if (tokens <= inputBudget) break;
  }
  if (tokens <= inputBudget) {
    return {
      prompt,
      truncated: true,
      droppedReviewers,
      sourceTruncatedTo,
      perReviewerCap,
      planTruncated,
    };
  }

  // Step 3: drop reviewers starting from the oldest (first in array).
  // Keep at least one reviewer if possible — dropping all defeats the merge.
  while (workingOutputs.length > 1 && tokens > inputBudget) {
    const dropped = workingOutputs.shift();
    if (dropped) droppedReviewers.push(dropped.model);
    ({ prompt, tokens } = buildAndMeasure());
  }
  if (tokens <= inputBudget) {
    return {
      prompt,
      truncated: true,
      droppedReviewers,
      sourceTruncatedTo,
      perReviewerCap,
      planTruncated,
    };
  }

  // Step 4: emergency — even with all reviewers + source stripped the
  // plan alone exceeds budget. Truncate the plan from the end, preserving
  // the heading. We compute a target plan length so the rebuilt prompt's
  // estimated tokens land at or just below the budget.
  if (workingOutputs.length > 0) {
    const dropped = workingOutputs.shift();
    if (dropped) droppedReviewers.push(dropped.model);
  }
  // Compute scaffolding overhead with the plan empty so we know how much
  // budget the plan itself can have.
  const scaffoldOnly = buildMergePrompt("", workingOutputs, workingSource);
  const scaffoldTokens = estimateTokens(scaffoldOnly);
  const sentinelTokens = estimateTokens(PLAN_TRUNCATION_SENTINEL) + 8; // padding for closing fence
  const planTokenBudget = Math.max(1024, inputBudget - scaffoldTokens - sentinelTokens);
  const planCharBudget = planTokenBudget * 4;
  if (workingPlan.length > planCharBudget) {
    let truncatedPlan = workingPlan.slice(0, planCharBudget);
    truncatedPlan = closeOpenCodeFence(truncatedPlan);
    truncatedPlan = truncatedPlan + PLAN_TRUNCATION_SENTINEL;
    workingPlan = truncatedPlan;
    planTruncated = true;
  }
  ({ prompt, tokens } = buildAndMeasure());

  // Step 5: post-reconstruction validation. If scaffolding markup pushed
  // us back over budget, trim the last reviewer output by the overage.
  if (tokens > inputBudget && workingOutputs.length > 0) {
    const overageTokens = tokens - inputBudget;
    const overageChars = overageTokens * 4;
    const last = workingOutputs[workingOutputs.length - 1];
    const newLen = Math.max(0, last.output.length - overageChars - 256);
    workingOutputs[workingOutputs.length - 1] = {
      ...last,
      output: last.output.slice(0, newLen),
    };
    ({ prompt, tokens } = buildAndMeasure());
  }

  return {
    prompt,
    truncated: true,
    droppedReviewers,
    sourceTruncatedTo,
    perReviewerCap,
    planTruncated,
  };
}

/** Legacy snapshot-based progress types. Retained so the in-flight
 *  `taskReducer`/`taskRuntime` review-flow state machine still compiles while
 *  the snapshot-driven merge orchestration is decommissioned. The live UI now
 *  drives off `hivemindReducer.ts` instead. */
export type ModelRunStatus = "pending" | "running" | "done" | "failed";

export interface ModelRunState {
  modelId: string;
  status: ModelRunStatus;
  startedAt?: number;
  inputTokens?: number;
  outputTokens?: number;
  durationMs?: number;
  cost?: number;
  error?: string;
}

export interface RoundRunState {
  round: number;
  models: ModelRunState[];
}

export interface ReviewProgress {
  reviewId?: string;
  totalRounds: number;
  currentRound: number;
  startedAt?: number;
  phase: "context" | "reviewing" | "merging";
  rounds: RoundRunState[];
}

/** Possible verdicts emitted by the merge orchestrator. */
export type VerdictKind = "accepted" | "rejected" | "modified";

/**
 * One orchestrator decision about a single reviewer suggestion.
 * Mirrors the Rust `RoundVerdict` struct in `app/src-tauri/src/hivemind/store.rs`.
 */
export interface RoundVerdict {
  /** UUID generated client-side before save. */
  id: string;
  job_id: string;
  round_number: number;
  /** "provider/model_id" form, matching the orchestrator's reviewer header. */
  reviewer_model: string;
  /** Short one-liner of the issue raised. */
  suggestion: string;
  verdict: VerdictKind;
  /** 1-5, or null when the orchestrator omitted it. */
  severity: number | null;
  reason: string | null;
  /** ISO 8601 timestamp. */
  created_at: string;
  /** Designated by the merge agent as the round's standout finding. */
  best_find: boolean;
  /** Other reviewers (besides `reviewer_model`) who independently raised the same finding. */
  co_reviewers: string[] | null;
}

/** Subset of `RoundVerdict` returned by structured tool-args parsing. */
export type ParsedVerdict = Pick<
  RoundVerdict,
  "reviewer_model" | "suggestion" | "verdict" | "severity" | "reason" | "best_find" | "co_reviewers"
>;

/** Normalize a merge-agent reviewer label to one of the exact feedback headers. */
export function canonicalizeReviewerModel(
  reviewer: string,
  reviewerLabels: string[],
): string {
  const raw = (reviewer || "").trim();
  if (!raw) return raw;

  const reviewerIndex = raw.match(/^reviewer\s*(\d+)\b/i);
  if (reviewerIndex) {
    const idx = Number(reviewerIndex[1]) - 1;
    if (idx >= 0 && idx < reviewerLabels.length) return reviewerLabels[idx];
  }

  const stripHeader = (s: string) =>
    s.replace(/^reviewer\s*\d+\s*[:—–-]\s*/i, "").trim();
  const normalized = (s: string) => stripHeader(s).toLowerCase().replace(/\s+/g, "");
  const candidate = normalized(raw);

  const exact = reviewerLabels.find((label) => normalized(label) === candidate);
  if (exact) return exact;

  const byModelId = reviewerLabels.find((label) => {
    const labelNorm = normalized(label);
    const modelNorm = normalized(label.split("/").pop() || label);
    return candidate === modelNorm || labelNorm.endsWith(`/${candidate}`);
  });
  if (byModelId) return byModelId;

  // Tightened suffix match: only match when the normalized label
  // ends with the candidate string. Replaces the old `contained`
  // fallback that used String.includes(), which caused false
  // positives when the candidate was a substring of a longer
  // model name (e.g., "gpt-4" matching "openai/gpt-4o").
  const bySuffix = reviewerLabels.find((label) => {
    const labelNorm = normalized(label);
    return labelNorm.endsWith(candidate);
  });
  return bySuffix || raw;
}

/**
 * Coerce a `submit_verdicts` tool-args payload into a normalised
 * `ParsedVerdict[]`. Accepts either `{ verdicts: [...] }` (the tool's
 * envelope) or a bare array. Returns `[]` on malformed input.
 */
export function verdictsFromToolArgs(parsed: unknown): ParsedVerdict[] {
  const allowed: VerdictKind[] = ["accepted", "rejected", "modified"];
  const arr = Array.isArray(parsed)
    ? parsed
    : parsed && typeof parsed === "object" && Array.isArray((parsed as { verdicts?: unknown }).verdicts)
      ? (parsed as { verdicts: unknown[] }).verdicts
      : null;
  if (!arr) return [];
  const out: ParsedVerdict[] = [];
  for (const raw of arr) {
    if (!raw || typeof raw !== "object") continue;
    const d = raw as Record<string, unknown>;
    const reviewerValue = d.reviewer_model ?? (d as { reviewer?: unknown }).reviewer;
    const reviewer_model = typeof reviewerValue === "string" ? reviewerValue.trim() : "";
    const suggestionValue = d.suggestion;
    const suggestion = typeof suggestionValue === "string" ? suggestionValue.trim() : "";
    if (!reviewer_model || !suggestion) continue;
    const rawVerdict = typeof d.verdict === "string" ? d.verdict.toLowerCase().trim() : "";
    const verdict: VerdictKind = allowed.includes(rawVerdict as VerdictKind)
      ? (rawVerdict as VerdictKind)
      : "rejected";
    let severity: number | null = null;
    const rawSeverity = typeof d.severity === "string" ? Number(d.severity) : d.severity;
    if (typeof rawSeverity === "number" && Number.isFinite(rawSeverity)) {
      severity = Math.max(1, Math.min(5, Math.round(rawSeverity)));
    }
    const reason =
      typeof d.reason === "string" && d.reason.trim().length > 0 ? d.reason.trim() : null;
    const rawBest = d.best_find ?? (d as { bestFind?: unknown }).bestFind;
    const best_find =
      rawBest === true ||
      rawBest === 1 ||
      (typeof rawBest === "string" && rawBest.toLowerCase().trim() === "true");
    let co_reviewers: string[] | null = null;
    const rawCoReviewers = d.co_reviewers ?? (d as { coReviewers?: unknown }).coReviewers;
    if (Array.isArray(rawCoReviewers)) {
      const cleaned = rawCoReviewers
        .filter((s: unknown): s is string => typeof s === "string")
        .map((s) => s.trim())
        .filter((s) => s.length > 0);
      if (cleaned.length > 0) co_reviewers = cleaned;
    }
    out.push({ reviewer_model, suggestion, verdict, severity, reason, best_find, co_reviewers });
  }
  // Defensive: enforce at most one best_find per batch (one merge call =
  // one round). If the model marked multiple, keep the highest severity;
  // tiebreak by first occurrence.
  const bestIdxs: number[] = out
    .map((v, i) => (v.best_find ? i : -1))
    .filter((i) => i >= 0);
  if (bestIdxs.length > 1) {
    let keepIdx = bestIdxs[0];
    let keepSev = out[keepIdx].severity ?? -1;
    for (const i of bestIdxs.slice(1)) {
      const sev = out[i].severity ?? -1;
      if (sev > keepSev) {
        keepIdx = i;
        keepSev = sev;
      }
    }
    for (const i of bestIdxs) {
      if (i !== keepIdx) out[i] = { ...out[i], best_find: false };
    }
  }
  return out;
}
