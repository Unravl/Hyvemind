# Bee-Colony Agents — Reference

> The product framing for the bee colony lives in `PRODUCT.md §4`. This doc is the **engineering reference**: where each agent's prompt is loaded, what shape its inputs and outputs take, how it interacts with the Pi runtime, what Nurse does when it stalls or crashes, and what to do when you want to add a new bee role.
>
> Audience: contributors and AI coding agents who need to reason about, modify, or extend the agent layer.

---

## 1. Why a colony, and why each agent gets its own prompt

Hyvemind's runtime is a small set of cooperating LLM-driven agents, each named after a real role in a honey-bee colony. The metaphor is a design constraint, not decoration:

- **Specialisation over generality.** Every agent has one job — plan (Scout), implement (Worker), validate (Guard), orchestrate (Queen), or watch for stalls (Nurse). A single "do everything" agent is harder to reason about, harder to bound, and harder to recover when it fails.
- **Per-role thinking budget.** Reasoning is the most expensive thing an LLM does. Scout thinks `High` because planning is the place creative friction pays off; Worker thinks `Medium` because focused execution doesn't need scratchpad-heavy reasoning; Nurse thinks `Low` because it's a fast classifier on a 60s tick. The level lives in `model_settings.<role>_thinking_level` and is parsed into `crate::pi::rpc::ThinkingLevel` at session-spawn time (e.g. queen.rs:1079-1084 for Scout).
- **Per-role tool set.** Read-only tools for planners (Scout, Queen-planning), full coding tools for implementers (Worker, Guard, Queen-runtime), no tools at all for Nurse (it operates purely on metadata over a single non-streaming provider call). Tool sets are wired via `PiSessionOptions::for_scout/for_worker/for_guard` and similar constructors.

Each role's contract — what it must call, what it must return — lives in its own system prompt under `app/src-tauri/prompts/`. The prompts are deliberately short, opinionated, and aimed at making the structured-output tool call (`submit_scout_result`, `submit_handoff`, `submit_guard_result`, `nurse_decisions`) the only valid completion path. There is **no fallback** — if the model finishes without the tool call, the run errors and is surfaced to the user or to Nurse.

---

## 2. Shared infrastructure

### Prompt loading

Five prompts are compiled into the binary at build time and are the canonical source of truth:

| Prompt file | Loader | Notes |
|-------------|--------|-------|
| `prompts/scout_system.md` | `core/scout.rs:17` `include_str!` via `SCOUT_SYSTEM_PROMPT` | Exposed by `scout::default_system_prompt()` (scout.rs:47) |
| `prompts/worker_system.md` | `core/worker.rs:18` | Exposed by `worker::default_system_prompt()` (worker.rs:34) |
| `prompts/guard_system.md` | `core/guard.rs:18` | Exposed by `guard::default_system_prompt()` (guard.rs:89) |
| `prompts/nurse_system.md` | `nurse/prompt.rs:9` `include_str!` | Exposed by `nurse::prompt::default_system_prompt()` and consumed by the Tier 3 LLM classifier (`nurse/classifier.rs`) and the Settings screen |
| `prompts/stability_test_task.md` | `core/stability_test/runner.rs:27` | Drives the test bot |
| `prompts/stability_test_verifier.md` | `core/stability_test/runner.rs:30-31` | Drives the verifier bot |

Three prompts are **on disk but not compiled in** — they are loaded or composed dynamically depending on context:

- `prompts/queen_system.md` — runtime Queen prompt. Read at runtime / constructed dynamically by the Queen-planning intake conversation; not `include_str!`'d.
- `prompts/queen_planning.md` — the 7-phase intake recipe driven by the Tasks-view planning conversation. Selected by the planning surface, not the swarm runtime.
- `prompts/plan_system.md` — shared plan template used by the generic Planner agent (Tasks-view "plan mode") and mirrored verbatim in `core/stability_test/runner.rs:41` as `PLAN_SYSTEM_PROMPT` for the test bot.

If you change any compiled prompt, you must rebuild (`cargo check` is enough to invalidate). If you change a dynamically-loaded prompt, the change takes effect on the next session spawn.

### The Pi runtime is the substrate

No Hyvemind agent implements its own agentic loop. Every bee role spawns a Pi subprocess via `PiManager::spawn_session_with_options` (see `pi/manager.rs`) configured with:

- the role's compiled system prompt (delivered through the Pi SDK's `systemPromptOverride`, NOT embedded in the user prompt)
- a per-role `ThinkingLevel` (`Low`, `Medium`, `High`)
- a per-role `ToolSet` (`Default`, `ReadOnly`, `Custom([...])`)
- a working directory (always the swarm's `working_directory`, never the host's cwd)

The bee-role module's `run_*` function (e.g. `run_scout`, `run_worker`, `run_guard`) is a thin coordinator: send the user prompt, await the response, pull the captured `submit_*` tool args off the session, deserialise into the typed result. The Pi process itself owns tool execution, model dispatch, streaming, and session-file persistence — Hyvemind only owns orchestration and the structured-output contract.

### Shared session attribution

Every swarm-role session sets a `SessionOwner::Swarm { swarm_id, role }` immediately after spawn (e.g. queen.rs:1090-1093). This is the routing key Nurse uses to identify which agent is misbehaving and which swarm to attribute the intervention to. Tasks-view (chat) sessions use `SessionOwner::Chat { task_id }`; the stability-test verifier uses `SessionOwner::Unknown` (stability_test/runner.rs:975).

### Run IDs

The Queen mints a fresh `run_id` per `run_feature_full` invocation (queen.rs:993-998). All three same-attempt sessions — Scout, Worker, inline Guard — share that `run_id` so the progress log and per-ID debug files can be grouped on retries. The format is `run-{feature_id}-{fix_attempt_count}-{uuid_simple}`.

---

## 3. Queen

### Role

Orchestrates execution of an already-approved multi-feature plan. Owns the scheduler, the feature dependency graph, milestone bookkeeping, and fix-feature synthesis after Guard failures.

### Thinking level and tool set

- **Thinking**: `Medium` (default; overridable via `model_settings.queen_thinking_level`).
- **Tool set**: Full coding tools at runtime. The separate **planning** persona is read-only.

### System prompt summary

The runtime Queen prompt (`prompts/queen_system.md`) is short and operational:

- Honour the feature decomposition, dependency graph, and milestones produced during planning — do not re-plan during execution.
- When Guard reports a failed milestone assertion, synthesize a new "fix feature" via `submit_features` that cites the exact failing assertion and the file paths involved.
- Assign every fix feature to the same milestone as the work it's repairing, so Guard re-runs the same assertion set.
- The scheduler enforces a hard cap of **3 fix attempts per feature** — do not try to bypass it.
- Pass `timeout: 300` on any bash call that may exceed 2 minutes (tests, builds, installs).
- Assertions must be **runnable, observable, falsifiable** — `cargo test passes cleanly`, not `the system works`.
- The planning-mode prompt (`queen_planning.md`) is a separate, much longer 7-phase recipe: Understand → Infrastructure → Credentials → Testing → Readiness → Milestones → Propose Plan. It is invoked by the Tasks-view planning surface, not by the swarm runtime.

### Lifecycle

- Spawns when `commands::swarms::start_swarm` calls `run_swarm_full` (queen.rs:483).
- Runs until every feature is `Completed`, `Failed`, or `Skipped`; or until the cancel token fires (pause / stop); or until the budget cap trips (`core/budget.rs`).
- Does not spawn its own Pi session for the orchestration loop — the Queen prompt is constructed dynamically during planning and is not re-issued at runtime. The Queen's runtime behaviour is implemented in Rust (the scheduler), not in a Pi subprocess.

### Inputs

- `QueenConfig` (queen.rs:440-455): `max_concurrent_features` (default 1, clamped by `HYVEMIND_SWARM_FEATURE_PARALLELISM` ≤ 6), `max_fix_attempts` (default 3), `swarm_budget_usd`, `daily_budget_usd`.
- `SwarmState` (id, working directory, status), `Vec<Feature>` (the plan), `Vec<Milestone>`, `Vec<ValidationAssertion>` (Phase 2 validators).
- Subsystem handles: `PiManager`, `SwarmStore`, `UsageStore`, `ActivityTx`, `SwarmUsageAccumulator`, `PauseHandles`, `ScoutReviewContext`.

### Outputs

- Drives features through `FeatureStatus` transitions (`Scouting` → `Implementing` → `Validating` → `Completed`/`Failed`/`Skipped`).
- Emits `ProgressEvent`s onto the broadcast channel (consumed by `swarm-event` listeners and the progress log writer).
- Emits Pi-session attribution events (`pi_session_spawned`, `pi_session_killed`) so the progress log can detect orphan subprocesses.
- Synthesizes fix features via `create_fix_features` (queen.rs:2491) when Guard validation fails.

### Failure modes

- **Scheduler returns no batch but features are not all terminal**: the loop logs a warning and continues; this state is usually a dependency-graph bug surfaced via `cycle_detection`.
- **Pi subprocess crash during a feature**: caught by the per-feature `Result` in `run_feature_full` (queen.rs:962); the feature is marked `Failed`, a fix-feature is generated if `fix_attempt_count < max_fix_attempts`.
- **Worker handoff missing**: the error chain carries a typed `HandoffParseFailed` marker (queen.rs:2095, handoff.rs:120-124). The Queen downcasts on it and calls `synthesize_nurse_for_error` (queen.rs:2081), which delegates to `NurseEngine::report_error` so a visible intervention bubble appears before the feature is flipped to `Failed`.
- **Queen orchestrator panic**: caught by the `super_watchdog` pattern that wraps every long-running spawn site (see §5). The swarm transitions to `Interrupted` and is resumable on the next launch.
- **Budget cap exceeded**: `core/budget.rs` emits `budget_exceeded` and the Queen pauses the next batch.

### Code path

- `core/queen.rs:483` — `run_swarm_full` entry point.
- `core/queen.rs:962` — `run_feature_full` (per-feature pipeline: Scout → optional Hivemind → Worker → optional inline Guard).
- `core/queen.rs:2491` — `create_fix_features` synthesizer.
- `core/scheduler.rs` — dependency-graph + bounded-parallelism dispatch.

---

## 4. Scout

### Role

Per-feature planner. Given one feature and the working directory, produces a step-by-step implementation plan, a complexity estimate, and a risk list.

### Thinking level and tool set

- **Thinking**: `High` (default; overridable via `model_settings.scout_thinking_level`, parsed at queen.rs:1079-1084).
- **Tool set**: Read-only via `PiSessionOptions::for_scout` — `read`, `grep`, `find`, `ls`, plus the structured-submission tool `submit_scout_result`. **No bash.** Scout MUST NOT modify files.

### System prompt summary

`prompts/scout_system.md` is intentionally minimal:

- Analyse a feature request and the current working directory.
- Produce a step-by-step plan, a complexity rating (`low`/`medium`/`high`), and a risks list.
- End the run by calling `submit_scout_result({plan, estimated_complexity, risks})`.
- The model's text response is treated as scratch reasoning — only the tool args are read.
- Pass `timeout: 300` on any bash call (note: tool set is read-only by default, but a Scout that somehow had bash available is told to budget).

### Lifecycle

- Spawned by `run_feature_full` at queen.rs:1074-1093 with session id `scout-{swarm_id}-{feature_id}`.
- Per-feature: dies at the end of `run_scout` regardless of outcome (queen.rs:1157 kills the session immediately after collecting the result).
- Cannot be paused mid-stream; pause takes effect at the next feature boundary.

### Inputs

- `Feature` (id, name, description, dependencies, milestone, `fix_attempt_count`).
- Working directory `Path` (the swarm's `working_directory`).
- The compiled `SCOUT_SYSTEM_PROMPT` is delivered via `PiSessionOptions::for_scout()` — NOT embedded in the user prompt. The user prompt is built by `build_scout_prompt` (scout.rs:140-176).

### Outputs

- `ScoutResult { plan, estimated_complexity, risks, feature_id }` (scout.rs:21-30).
- Complexity is normalised to exactly `low`/`medium`/`high` by `normalize_complexity` (scout.rs:179-188) — anything ambiguous becomes `medium`.

### Failure modes

- **Model never calls `submit_scout_result`**: `run_scout` errors with `"scout for feature '<id>' did not call submit_scout_result"` (scout.rs:104-111). The Queen marks the feature `Failed`.
- **Pi process crashes mid-stream**: `collect_response` wraps the underlying `PiRpcError::ProcessCrashed` with `"scout collect_response failed for feature '<id>'"` (scout.rs:96-102).
- **Tool args malformed**: `serde_json::from_value` returns a context error (scout.rs:113-118).
- Nurse can `steer` a stalled Scout mid-stream but, because Scout has no shell, cannot intervene against shell-level loops. Practically Scout sessions sit in the `Healthy` tier in Nurse — their detectors rarely raise.

### Code path

- `core/scout.rs:61` — `run_scout` entry point.
- `core/scout.rs:140` — `build_scout_prompt` user-prompt builder.
- `core/scout.rs:179` — `normalize_complexity`.

---

## 5. Worker

### Role

Implementer. Given a feature, the Scout's plan, and the working directory, write the actual code.

### Thinking level and tool set

- **Thinking**: `Medium` (default; overridable via `model_settings.worker_thinking_level`).
- **Tool set**: Full coding tools — `read`, `write`, `edit`, `bash`, `grep`, `find`, `ls`, plus the structured-submission tool `submit_handoff` (delivered by the local `hyvemind-handoff` Pi extension, see `app/src-tauri/pi-extensions/hyvemind-handoff/`).

### System prompt summary

`prompts/worker_system.md`:

- Implement the feature according to the Scout plan.
- End the run by calling `submit_handoff` with each field as a top-level argument — do NOT wrap the payload in a string and do NOT emit handoff JSON in the text response. The Rust backend reads the tool args directly; there is no fallback parser anymore.
- `success_state` MUST be exactly one of `success` / `failure` / `partial` (lowercase).
- `discovered_issues` is OPTIONAL — array of `{ severity: "info"|"warn"|"error", description, suggested_fix? }` objects for non-blocking observations (pre-existing bugs, deprecated deps, flaky tests). Severity is informational; issues never gate the swarm.
- Pass `timeout: 300` on long bash calls (tests, builds, installs, `cargo build`, `vitest run`).
- Do NOT invent issues to fill `discovered_issues` — omit or pass `[]`.

### Lifecycle

- Spawned by `run_feature_full` after Scout (and after the optional Scout-plan Hivemind review) succeeds.
- Dies at the end of `run_worker` whether the handoff parses or not.
- The Worker session is sandboxed to the swarm's `working_directory` — the Pi `bash` tool runs commands from that cwd.

### Inputs

The user prompt is built by `build_worker_prompt` (worker.rs:225-278) and includes:

- The feature (id, name, description, dependencies, working directory).
- The Scout's plan (markdown).
- A `## Swarm Context` block (rendered by `render_swarm_context_block`, worker.rs:170-222) containing — when present — `AGENTS.md` project conventions, `notes.md` architecture notes, `services.yaml :: commands`, `services.yaml :: services`.
- An `attempt_info` block when `fix_attempt_count > 0` ("Fix Attempt: N of M (this is a retry after a previous failure)").
- Instructions to call `submit_handoff` with the matching `feature_id`.

### Outputs — the WorkerHandoff JSON

The Worker's `submit_handoff` tool call args deserialise into `WorkerHandoff` (handoff.rs:89-102). Schema:

| Field | Type | Purpose |
|-------|------|---------|
| `feature_id` | `String` | The feature this run is reporting against. Validated against `expected_feature_id` — a mismatch causes `handoff_from_tool_args` to return `None` (handoff.rs:138-140), which the Queen treats as a parse failure. Must be non-empty (handoff.rs:107-109). |
| `run_id` | `String` | The per-attempt run id minted by the Queen (queen.rs:993). Lets the progress log distinguish retries on the same feature. Must be non-empty (handoff.rs:110-112). |
| `salient_summary` | `String` | A brief one-line summary, surfaced in the SwarmControl feature card and the activity log. |
| `what_was_implemented` | `String` | Long-form description of the work done — Worker's account of the changes. Goes into the activity log and is consumed by humans inspecting the swarm. |
| `verification` | `String` | How to verify the implementation (e.g. "run `cargo test`"). Hints for Guard and for humans. |
| `success_state` | `SuccessState` enum (`Success` / `Failure` / `Partial`) | Custom `Deserialize` (handoff.rs:60-76) accepts case-insensitive `success`/`failure`/`failed`/`fail`/`partial` with whitespace trimming. `Failure` marks the feature `Failed` (and triggers a fix-feature if under the cap); `Partial` is treated as a partial success that still hands off to Guard. |
| `discovered_issues` | `Vec<DiscoveredIssue>` (default `[]`) | Non-blocking notifications: `{ severity: Info/Warn/Error, description, suggested_fix? }`. Emitted to the frontend as `discovered_issue` progress events (see `state/progress.rs::ProgressEventType::DiscoveredIssue`). Never gates the swarm. |

### Failure modes

- **Worker never calls `submit_handoff`**: `handoff_from_session` (handoff.rs:147-161) returns an `anyhow::Error` wrapping `HandoffParseFailed { reason: "worker did not call submit_handoff tool" }`. The Queen downcasts on `HandoffParseFailed` in the error chain (queen.rs:2095, worker.rs:127-136) and calls `synthesize_nurse_for_error` (queen.rs:2081), which routes through `NurseEngine::report_error` to surface a visible intervention bubble explaining the missing tool call before flipping the feature to `Failed`.
- **`feature_id` mismatch in args**: same path — treated as a malformed handoff. The Queen never trusts a Worker that reports against the wrong feature.
- **Missing required field**: `serde` deserialisation fails — same `HandoffParseFailed` path.
- **Process crash mid-implementation**: `collect_response` errors with `"worker collect_response failed for feature '<id>'"` (worker.rs:101-105). The feature is marked `Failed` and the activity stream gets an `agent_end` with `success=false`.
- **Worker stalls (model hangs, infinite loop in a tool)**: the Nurse engine's `StallDetector` raises a `Signal` as `idle_secs` crosses `stall_threshold_secs`; the dispatcher escalates the session's tier to `Warning` then `Stalled` and routes the signal through the three-tier pipeline — usually surfacing a `steer` (Tier 2 templated) or a `restart` (Tier 1 / Tier 3) per §7.

### Code path

- `core/worker.rs:57` — `run_worker` entry point.
- `core/worker.rs:170` — `render_swarm_context_block` (swarm context injection).
- `core/worker.rs:225` — `build_worker_prompt` user-prompt builder.
- `core/handoff.rs:89` — `WorkerHandoff` struct.
- `core/handoff.rs:147` — `handoff_from_session` parser.

---

## 6. Guard

### Role

Validator. Runs milestone assertions against the working directory and reports per-assertion `pass`/`fail` evidence so the Queen knows whether to synthesize fix features.

### Thinking level and tool set

- **Thinking**: `Medium` (default; overridable via `model_settings.guard_thinking_level`).
- **Tool set**: Full coding tools + run commands (`read`, `bash`, `grep`, `find`, `ls`, etc.), plus the structured-submission tool `submit_guard_result`. Guard typically reads files and runs check commands; it does not write code itself.

### System prompt summary

`prompts/guard_system.md`:

- Given a set of assertions, check each and report pass/fail with evidence.
- Two assertion shapes are supported:
  1. **Legacy**: plain numbered list, results returned in array order without `id`.
  2. **Validator (Phase 2)**: each assertion is tagged with a stable `VAL-*` id (e.g. `VAL-FND-001`). When the prompt presents VAL-* ids, set the `id` field on each result so the orchestrator can key per-assertion outcomes by id.
- End the run by calling `submit_guard_result({assertions: [...]})` with one entry per assertion in the order they were presented. Each entry has `status` (`"pass"`/`"fail"`), `evidence`, optional `error` (when fail), optional `id` (when validator-form).
- Pass `timeout: 300` on long-running validation commands.

### Lifecycle

Two invocation paths:

1. **Phase 2 validator features** — when the Queen scheduler reaches an auto-injected validator feature (one whose `fulfills` references a set of `VAL-*` assertions), it calls `run_guard_with_assertions` (guard.rs:106) with the looked-up `ValidationAssertion`s. Validators replace the legacy "magic Guard phase" inside `run_feature_full` — they are scheduled like any other feature.
2. **Inline / legacy** — older swarms or feature shapes can still run a Guard inline at the end of `run_feature_full`; the structure is similar but without `VAL-*` ids on the returned results.

The session is killed at the end of the validation pass.

### Inputs

The user prompt is built by `build_guard_prompt_for_assertions` (guard.rs:202-244) and includes:

- Validator feature id and name.
- Milestone id and name.
- Working directory.
- The numbered assertions list, with `VAL-*` ids when in Phase 2 form.
- Instructions to call `submit_guard_result` with results in the presented order.

### Outputs

`ValidationResult` (guard.rs:39-47):

- `passed: bool` — true only if every assertion passed.
- `assertion_results: Vec<AssertionResult>` — per-assertion `{ assertion, passed, output, error?, assertion_id? }`.
- `feature_id: String`.

Helpers: `failure_count()` and `failures()` (guard.rs:50-62).

In Phase 2, `run_guard_with_assertions` back-fills `assertion_id` on each result row when the model omitted it (guard.rs:177-181) — the orchestrator always ends up with VAL-* ids it can key against.

### Failure modes

- **Model never calls `submit_guard_result`**: errors with `"guard for validator feature '<id>' did not call submit_guard_result"` (guard.rs:166-173).
- **Length mismatch**: `guard_result_from_args` errors when the returned `assertions` array length doesn't match the input list (guard.rs:255-261). The Queen marks the validator feature `Failed`; Nurse sees the spawning swarm's sessions sit in the `Stalled` tier if this happens repeatedly.
- **Tool args malformed**: standard `serde_json` context error (guard.rs:252-253).
- **Empty assertions list**: auto-passes with an empty `assertion_results` (guard.rs:114-125). This mirrors the legacy `run_guard` behaviour — a milestone with zero assertions is vacuously satisfied.
- **Noisy `error` field** ("none", "N/A", whitespace): filtered out by `guard_result_from_args` (guard.rs:273-276) so the UI doesn't show meaningless error strings on a passing assertion.

### Code path

- `core/guard.rs:106` — `run_guard_with_assertions` entry point.
- `core/guard.rs:202` — `build_guard_prompt_for_assertions` user-prompt builder.
- `core/guard.rs:247` — `guard_result_from_args` parser.

---

## 7. Nurse

### Role

Heartbeat / stall-detection / intervention dispatcher. Subscribes to a push-mode `NurseBus` that carries every parsed Pi event, runs a registry of per-session detectors against the resulting `SessionHealth`, and routes every raised `Signal` through a three-tier decision pipeline that decides whether to `leave_it`, `steer`, `restart`, or `cancel` the session.

### Thinking level and tool set

- **Thinking**: `Low` for the Tier 3 LLM classifier (only consulted when Tiers 1 and 2 don't match; the classifier should be cheap and fast).
- **Tool set**: None. Detectors are pure heuristics over `SessionHealth` with no Pi subprocess behind them. The Tier 3 classifier is a single non-streaming `ProviderRegistry` call returning a `nurse_decisions` tool call against the canonical JSON schema in `nurse/schema.rs` — no shell, no `read`, no `edit`.

### System prompt summary

`prompts/nurse_system.md`:

- Invoked **per signal**, not per tick — the dispatcher reaches Tier 3 only when a signal raises and Tiers 1 and 2 don't match. The classifier prompt is built by `LlmClassifier::build_prompt` (`nurse/classifier.rs:52`) from the session's `SessionHealth` snapshot (active signals, `idle_secs`, `event_count`, owner, tier).
- Tiers: `healthy` (no transcript), `warning` (short transcript), `stalled` (full recent transcript).
- Return a `decisions` array containing **only** sessions that need action — omitting a session means "leave it alone". The array may be empty.
- Each decision: `session_id` + `decision` (`leave_it`/`steer`/`restart`/`cancel`) + `reasoning` + optional `observation`, `action`, `message`, `check_back_secs`.
- `observation` and `action` are rendered inline next to the conversation in the UI — write them in first person, present tense, one sentence each (`"I noticed..."`, `"I'll steer..."`), under ~140 characters.
- Also invoked on a **single error event** (chat / swarm / hivemind) — callers enter the dispatcher via `NurseEngine::report_error` with `DispatchOrigin::ReportError`. For Hivemind synthesized failures (no Pi process behind the session id), callers use `NurseEngine::report_synthesized` directly so the dispatcher writes a 3-row decision-log chain without entering tier evaluation.
- For Hivemind-owned sessions (`SessionOwner::Review` / `SessionOwner::Merge`): Tier 1's `tier1_lookup` automatically downgrades `Restart` actions to `Cancel` via `DowngradeReason::RestartNotMeaningfulForHivemind` — there is no Pi subprocess to respawn. The classifier prompt instructs the model to default to `leave_it` (with `check_back_secs` 300-900) and only `cancel` on persistent or proven-global failures.

The classifier output is a single `nurse_decisions` tool call against the canonical JSON schema in `nurse/schema.rs` (the dispatcher passes the schema verbatim through the provider-native `tools`+`tool_choice` envelope).

### Lifecycle

- Constructed in the Tauri setup hook at `lib.rs:373` via `NurseEngine::new(...)`. The setup order is **strict** (see `lib.rs:342`):
  1. construct the `NurseBus` and hand it to `PiManager::set_nurse_bus` so every parsed Pi event fans onto the bus
  2. `attach_app_handle` so the engine can emit `nurse-event` and look up `AppState`
  3. `attach_dispatcher` with the `LlmClassifier`, `DefaultApplier`, and `SessionKiller` (PiManager) so the three-tier pipeline can run
  4. `NurseEngine::start(...)` returns `Err` if either OnceCell is still empty — refusing to run with a half-wired engine is intentional, because the legacy dark-mode fallback is gone.
- The returned `JoinHandle` is wrapped by `util::supervise::super_watchdog` (lib.rs around `:406`, see §11) so a panic in `run_loop` respawns the engine exactly once before going fatal.
- The run loop is event-driven (`engine.rs::run_loop`): it consumes `NurseBusEvent`s as they arrive (`on_bus_event` / `on_pi_event`) and runs a periodic sweep every `nurse_tick_interval_secs` (default 10s, clamp `[5, 600]`, env `HYVEMIND_NURSE_TICK_INTERVAL_SECS`). Slow detectors run on a separate task at `HYVEMIND_NURSE_SLOW_PROBE_INTERVAL_SECS` cadence to keep the engine loop unblocked.
- `engine.shutdown()` cancels the engine's shutdown token; called from `RunEvent::Exit` for graceful teardown.

### Inputs

- `NurseConfig` (`nurse/config.rs`): `enabled`, `mode: NurseMode (Enabled | Observe | Disabled)`, engine-wide `nurse_model` / `nurse_provider` fallback, and per-profile `ProfileConfig` map (`profiles: HashMap<NurseProfile, ProfileConfig>`). Effective model/provider per dispatch is resolved by `NurseConfig::effective_model(profile)` / `effective_provider(profile)` — per-profile override wins, engine-wide fallback otherwise.
- `NurseBus` for Pi event push delivery (capacity `HYVEMIND_NURSE_BUS_CAPACITY`, default 4096).
- The shared `PiManager` (consumed via the `SessionKiller` trait the dispatcher's `DefaultApplier` uses to drive Restart / Cancel).
- The shared `ProviderRegistry` (used by the Tier 3 `LlmClassifier`).
- A `tauri::AppHandle` (set via `attach_app_handle`) for emitting `nurse-event` and `swarm-event` updates and for calling `cancel_hivemind_review` on a Hivemind-owned Cancel.

### Outputs — the three-tier dispatch pipeline

Every signal that survives storm-guard, post-lag suppression, in-flight guard, and severity gating enters `Dispatcher::handle_signal` (`nurse/dispatcher.rs:453`). The pipeline is:

- **Tier 1 — deterministic.** `tier1_lookup(detector, dedup_key, owner)` (`dispatcher.rs:275`) is a hardcoded table keyed on `(detector, dedup_key)`. On hit it returns a fixed `NurseActionKind` plus a stable `Tier1EntryId` for post-hoc diagnostics. Owner-aware downgrade: `Restart` on a `SessionOwner::Review` / `SessionOwner::Merge` is coerced to `Cancel` because there is no Pi subprocess to respawn. **Tier 1 bypasses storm guard** — deterministic actions always fire.
- **Tier 2 — templated.** `SteerPlaybook::lookup(detector, dedup_key)` (`nurse/playbook.rs`) returns a canned `steer` message keyed by detector + dedup substring. No LLM call required. Tier 2 honours the storm guard.
- **Tier 3 — LLM classifier.** `LlmClassifier::classify_prepared` (`nurse/classifier.rs`) dispatches a single non-streaming `ProviderRegistry` call. The prompt is written to `~/.hyvemind/debug/nurse/captures/{decision_id}-prompt.txt` **before** the provider call so a mid-call crash leaves an unambiguous "invoked, never returned" trace. The response is captured to the matching `-response.txt`. Returns `Skip` when no model is configured (logged as `classifier_skipped_no_model`).

Final action set, surfaced as `NurseDecision` variants (mirrored in `nurse/snapshot.rs`):

- **`LeaveIt { reasoning, check_back_secs (1-1800), observation?, action? }`** — session is legitimately waiting. Storm-guard cooldowns and per-`dedup_key` budget cooldowns prevent re-decision until the cooldown elapses unless severity escalates.
- **`Steer { reasoning, message, observation?, action? }`** — `DefaultApplier` injects a steer message into the running Pi session via the dispatcher's session handle. The safest non-trivial intervention; the playbook table is heavily biased toward this.
- **`Restart { reasoning, observation?, action? }`** — `DefaultApplier` calls `kill_with_verification` (`nurse/intervention.rs:398`) to terminate the session. **In a swarm context this typically marks the in-flight feature `Failed`**, so use sparingly. Coerced to `Cancel` for Hivemind owners.
- **`Cancel { reasoning, message?, observation?, action? }`** — surfaces a message the user must see (auth failure, billing failure, impossible task) and runs `kill_with_verification`. For Hivemind owners, this additionally drives `cancel_hivemind_review` (`nurse/intervention.rs:556`) so the review's cancellation token trips.

### Kill verification

`kill_with_verification` (`nurse/intervention.rs:398`) is the 3-state machine both Cancel and Restart use:

1. send the cooperative `abort` to the session
2. poll liveness for `POST_ABORT_LIVENESS_GRACE` (~3s)
3. if the session is still alive, send `force_kill` (SIGTERM → wait → SIGKILL)
4. poll again; on confirmed death return the `dead_at` timestamp
5. if the session survives both attempts, return `double_fail_giving_up` — the safety circuit so a stuck session can't eat the budget on repeated retries

Side effects on every dispatched non-`LeaveIt`:

- Emits `nurse-event` Tauri events (lifecycle pair: `started` + `completed`) and updates the `NurseStats` aggregate consumed by the topbar badge.
- Writes the entire decision chain (`decision_started` → `storm_guard_evaluated` → `tier1_evaluated` → `playbook_evaluated` → `classifier_invoked` → `classifier_returned` → `intervention_dispatched` → `kill_verification` → `intervention_outcome` → `decision_finalised`) to `~/.hyvemind/debug/nurse/decisions.jsonl.*` regardless of `HYVEMIND_DEBUG` — see CLAUDE.md §Investigating a Nurse decision.
- Charges the per-detector `BudgetState` (`nurse/budget.rs`), which decays with age. Per-profile defaults — Tasks: flat 3; Swarm: 5 initial, +1/hr, max 10. Exhausting a detector's budget produces `BudgetGateReason` outcomes that gate further dispatches without firing them.
- Records the intervention into the bounded in-memory log surfaced via `get_nurse_intervention_log` IPC.

### Failure modes

- **No nurse model configured** (`effective_model(profile)` returns `None`): dispatch returns `DispatchResultKind::ClassifierSkippedNoModel` and the decision chain logs `classifier_skipped_no_model`. The default for fresh installs.
- **Provider call timeout** (`HYVEMIND_NURSE_PROVIDER_TIMEOUT_SECS = 90`, clamp `[10, 600]`): `DispatchResultKind::ClassifierFailed(msg)`; `decision_finalised{status:"classifier_failed"}`.
- **Bad classifier parse**: dispatcher writes `classifier_failed` to the chain and increments `consecutive_bad_parse_ticks`; engine loop continues.
- **Bus lag** (`RecvError::Lagged(n)`): logged to `bus.jsonl` and triggers post-lag Tier 2/3 suppression for affected sessions until the bus catches up.
- **In-flight collision** on the same session: `InFlightGuard` returns `DispatchResultKind::GatedInFlight { existing }` to prevent overlapping dispatches.
- **Self-kill grace gate**: a Cancel/Restart that fires within `SELF_KILL_GRACE = 30s` of the previous one for the same session is gated as `GatedSelfKillGrace` so a flapping detector can't kill-loop a session.
- **Engine run loop panic**: caught by `util::supervise::super_watchdog` at the `start()` call site — see §11.
- **Engine started without dispatcher / app handle attached**: `start()` returns `Err`; the bootstrap logs `"nurse engine v2 failed to start; Nurse interventions will not fire until the next restart"` and the app keeps running without Nurse rather than crash-looping.

### Code path

- `nurse/engine.rs:239` — `NurseEngine::start`.
- `nurse/engine.rs:293` — `report_error` (Pi-backed error → synthetic Signal → dispatcher).
- `nurse/engine.rs:262` — `report_synthesized` (Hivemind pseudo-session → 3-row decision chain).
- `nurse/dispatcher.rs:453` — `Dispatcher::handle_signal` entry point.
- `nurse/dispatcher.rs:275` — `tier1_lookup` deterministic table.
- `nurse/playbook.rs` — `SteerPlaybook::lookup` Tier 2 templated table.
- `nurse/classifier.rs:52` — `LlmClassifier::build_prompt` + `classify_prepared`.
- `nurse/intervention.rs:398` — `kill_with_verification` 3-state machine.
- `nurse/intervention.rs:556` — `cancel_hivemind_review`.
- `nurse/prompt.rs:9` — `default_system_prompt` (Settings-screen accessor).
- `nurse/schema.rs` — canonical JSON schema for the `nurse_decisions` tool.

---

## 8. Scout-review — optional Hivemind review of the Scout's plan

When a Swarm's `model_settings` references a Hivemind id for scout-plan review, the Queen routes the Scout's output through that Hivemind before handing it to the Worker. The same wiring is reused for Queen-plan review (the Queen's master plan can be reviewed before any Scouts run).

The review is implemented as a thin shim over the unified Hivemind engine — `core/scout_review.rs:65` `run_scout_hivemind_review`:

- Loads the `HivemindConfig` by id from `HivemindStore` (scout_review.rs:79-84).
- Parses the rounds config via `parse_rounds_config` and asserts it has at least one round with at least one model (scout_review.rs:86-93).
- Builds an `OrchestratorCfg` (`resolve_orchestrator`, scout_review.rs:196-225) — uses the Hivemind's explicit orchestrator if set, else the last model of the last round, else the swarm's `primary_model` falling back to `anthropic` provider.
- Builds a `ContextSpec::PiGather` (scout_review.rs:96-102) that spawns a dedicated read-only Pi session ("hivemind-context-{feature_id}") to summarise the codebase context relevant to the plan. The context-system prompt is the inline `CONTEXT_SYSTEM_PROMPT` (scout_review.rs:227-238) — it constrains the gatherer to `read`/`grep`/`find`/`ls` and forces it to call `submit_context({"summary": "..."})`.
- Stamps the run with `ReviewAttribution { swarm_id, feature_id, source_label: "Scout: {feature_id}" }` (scout_review.rs:128-135) so the review log can be traced back to the swarm.
- Dispatches the review via `ReviewEngine::run` (scout_review.rs:140-156), passing an `EngineDeps` that wires in the shared `ResponseCache` singleton, the `MergeCaptureRegistry`, the `nurse_engine` handle (so circuit-breaker trips and other Hivemind-side failures during scout-review can surface a visible Nurse bubble via `report_error` / `report_synthesized`), the read-locked `custom_prompts` snapshot, and the optional `ActivityTx` for the activity stream.
- Stance is **hardcoded to `Stance::Against`** (scout_review.rs:124). `For` and `Neutral` stances exist in the schema but are not yet wired to the UI (see `PRODUCT.md §6`).
- On success: emits `HivemindReviewCompleted` to the swarm progress channel AND a `hivemind-progress` Tauri event with the refined plan length. The Queen then hands the **refined plan** (not the original Scout plan) to the Worker.

If the Hivemind id is not configured or the Hivemind has no rounds, the review is skipped and the Scout plan is handed to the Worker unchanged.

---

## 9. Nurse intervention escalation — Steer → Restart → Cancel

Nurse's escalation ladder is preference-encoded in the system prompt for the Tier 3 classifier (prefer `steer` over `restart`, `restart` over `cancel`, and never `cancel` over a single sibling model's auth/billing error). The runtime backstops are the **per-detector budget** and the **storm guard**, both of which are enforced by `Dispatcher::handle_signal` before any action fires.

### Per-detector budget (`nurse/budget.rs`)

Replaces the old "flat `max_interventions = 3`" ceiling. `BudgetState` tracks per-detector charge with age-decay; defaults are per-profile in `ProfileConfig::default_for(profile)`:

- **Tasks profile**: flat 3 interventions per detector.
- **Swarm profile**: 5 initial, +1/hr decay, hard max 10.
- **Hivemind profile**: tight cap, since most Hivemind sessions are synthesized pseudo-sessions that only `Cancel`.

Per-`dedup_key` cooldowns prevent a flapping detector from chewing budget on the same signal. Exhausting a detector's budget produces `DispatchResultKind::GatedBudget(BudgetGateReason)` and the chain logs `budget_evaluated{result:"gated"}`. Once gated, the user-facing session card surfaces a "Nurse budget exhausted" badge instead of the spinner.

### Storm guard (`nurse/storm_guard.rs`)

Per-`(session_id, kind)` sliding window — default 3 events / 60s. The fourth dispatch on the same `(session, dedup_key)` inside the window is gated as `GatedStormGuard`. **Tier 1 bypasses storm guard** — deterministic actions (`tier1_lookup` hits) always fire regardless of recent dispatch density.

### Self-kill grace (`nurse/dispatcher.rs::SELF_KILL_GRACE`)

After a Cancel or Restart fires on a session, the next Cancel/Restart on the same session within `SELF_KILL_GRACE = 30s` is gated as `GatedSelfKillGrace`. Prevents a "kill loop" where a detector keeps raising while the session is already mid-termination.

### Watchdog respawn (`util::supervise::super_watchdog`)

The two-layer panic respawn pattern (`util/supervise.rs:150`) is the same one used by the Pi maintenance loop (`lib.rs` around `:440`) and every fire-and-forget supervisor in the codebase. For Nurse:

1. **Layer 1 (inner)**: the engine's `run_loop` is event-driven; a per-iteration error increments observability counters but does not kill the loop. Detector-tick errors are caught per-detector and counted in detector stats.
2. **Layer 2 (outer)**: `super_watchdog` wraps `NurseEngine::start(...)`. If `run_loop` panics, `super_watchdog` calls `start(...)` exactly once more, logging the explicit `"nurse unrecoverable"` marker (supervise.rs:192-198) if the respawn also crashes. After two crashes the user must restart the app.

The outcome enum is `SuperWatchdogOutcome::{CleanFirstExit, RespawnSucceeded, FatalSecondCrash}` (supervise.rs:121-132).

### When the engine refuses to start

`NurseEngine::start(...)` returns `Err` if either `attach_app_handle` or `attach_dispatcher` hasn't completed. The bootstrap in `lib.rs` logs `"nurse engine v2 failed to start; Nurse interventions will not fire until the next restart"` and lets the app keep running without Nurse — this is intentional. Crash-looping the whole app because Nurse can't initialise would make recovery harder, not easier.

---

## 10. The stability-test bot pair

Hyvemind ships an **automated end-to-end stability test** that drives the full planning pipeline in a sandbox and grades the result with a separate AI verifier. Two prompts:

### `prompts/stability_test_task.md` — the test bot

Loaded into the planning session at `core/stability_test/runner.rs:27` as `STABILITY_TEST_PROMPT`. Asks the model to:

1. Submit **exactly two** clarifying questions (one `choice` with 2-3 options and a `recommended`, one `text`) via `submit_stability_questions({ questions: [...] })`. Then STOP and wait.
2. After answers arrive, submit a minimal but well-formed plan that mentions `sample.txt` under "Files to Modify" via `submit_stability_plan({ plan_markdown: "..." })`.
3. After the implementation prompt arrives (auto-sent post-Hivemind), edit `sample.txt` to contain the proposed greeting line and call `submit_stability_impl_complete({})`.

Constraints in the prompt: keep tokens low, the greeting line MUST end up in `sample.txt`, do not touch any other files, this is automated so pick sensible defaults rather than asking extra questions.

### `prompts/stability_test_verifier.md` — the verifier bot

Loaded at `core/stability_test/runner.rs:30-31` as `STABILITY_TEST_VERIFIER_PROMPT`. Spawned by `run_verifier` (runner.rs:942-1000) with a custom read-only tool set (`read`, `bash`, `grep`, `find`, `ls`) — bash is included so the verifier can inspect the sandbox by running shell commands but the prompt forbids any modification. The verifier is told to:

1. Read the Pi session transcript at the path supplied in the user prompt (JSONL, one event per line).
2. Inspect the sandbox to confirm `sample.txt` was actually edited (vs. the original "Stability test sandbox file." content).
3. Sanity-check the assistant output for refusal phrases, gibberish, partial truncation, `stopReason="error"`, etc.
4. Read the programmatic gates included in the user prompt and weigh them in the verdict.
5. Emit the verdict via `submit_stability_verdict({ passed, confidence, issues, summary })`. There is no fallback.

`passed` is `true` only when the run went end-to-end (questions appeared, plan was produced, Hivemind ran, implementation edited `sample.txt`, no broken behaviour in the transcript). Failure-mode hints in the prompt: refusal/apology in assistant output, `sample.txt` unchanged, unrecoverable Pi `error` event, empty/truncated/nonsensical plan or implementation, tool calls without actual file changes.

### How the runner orchestrates both

`run_stability_test_inner` (runner.rs:74-78) is the entry point. Phase state machine:

| Phase | Timeout | What runs |
|-------|---------|-----------|
| `setup` | — | Prepares a sandbox under `state.test_sandbox_dir` (with `SANDBOX_RETENTION = 5`, runner.rs:33). |
| `task` (planning) | `TASK_PHASE_TIMEOUT = 300s` | Drives the test bot via `commands::chat::send_message` with the inline `PLAN_SYSTEM_PROMPT` (runner.rs:41-56, mirrored from `app/src/lib/plan-mode.ts::PLAN_SYSTEM_PROMPT`). Listens for `submit_stability_questions`, auto-answers each (picking the `recommended` option for `choice`, sending a fixed string for `text`), then waits for `submit_stability_plan`. |
| `hivemind` | `HIVEMIND_TIMEOUT = 600s` | Calls `commands::hivemind::start_review` on the captured plan against the configured Hivemind id. Polls `get_review_state` until the review terminates. |
| `impl` | `IMPL_TIMEOUT = 300s` | Sends a new prompt under the inline `IMPL_SYSTEM_PROMPT` (runner.rs:60-68) telling the bot to execute the approved plan. Waits for `submit_stability_impl_complete`. |
| `verifier` | `VERIFIER_TIMEOUT = 180s` | Spawns a separate Pi session via `run_verifier` (runner.rs:942) with the verifier system prompt. Awaits `submit_stability_verdict`. |
| `finalize` | — | Persists a `TestRunRecord` to `state.test_runs_dir` and emits a final `test-progress` event. |

The runner emits `test-progress` Tauri events at every transition (consumed by `TestRunProvider` and the Tests screen — see `CLAUDE.md §Frontend Architecture → Event listener index`).

Models are resolved by `resolve_model_config` (runner.rs:126-127); empty `task_model`, `hivemind_models`, or `verifier_model` short-circuits the run with a `finalize_with_error` call.

---

## 11. Adding a new bee role

Use the following checklist. Roughly in order:

1. **Pick the colony name** that maps to the role's behaviour. (Bee colony has plenty — Drone, Forager, Builder, Comb-Builder, Undertaker, Fanner, Receiver, Soldier, …. Consistency with the colony metaphor matters; the user-visible name and the module name should align.)

2. **Write the system prompt** at `app/src-tauri/prompts/<role>_system.md`. Constraints:
   - Short, opinionated, one role per file.
   - Define the structured-output contract — name the `submit_*` tool the role MUST call, list each argument's type and constraints, and state explicitly that there is **no fallback**.
   - Tell the model the tool-set boundary (`read`-only? full coding? no tools?). This is informational — the actual gating happens at session-spawn time.
   - Include the standard `timeout: 300` guidance when the role has bash and may run long commands.

3. **Add the agent module** at `app/src-tauri/src/core/<role>.rs`:
   - `const <ROLE>_SYSTEM_PROMPT: &str = include_str!("../../prompts/<role>_system.md");`
   - `pub fn default_system_prompt() -> &'static str` accessor (used by the Settings screen to display the prompt).
   - Result struct(s) with `#[derive(Debug, Clone, Serialize, Deserialize)]`.
   - Tool-args struct(s) with `#[derive(Debug, Deserialize)]` for parsing the `submit_*` payload.
   - `pub async fn run_<role>(session: &Arc<PiSession>, ..., working_dir: &Path) -> Result<...>` entry point. Use `#[tracing::instrument(skip_all, fields(agent = "<role>", ...))]` so the per-ID log routing layer (`state/log_routing.rs`) routes events to the right file.
   - End the function by calling `session.take_tool_args("submit_<role>_result")` — error cleanly with a `<role>_did_not_call_submit_<role>_result` style message when missing.

4. **Add the agent prompt loader to `core/mod.rs`** (`pub mod <role>;`) and re-export anything other subsystems need.

5. **Add `PiSessionOptions::for_<role>(model, system_prompt)`** in `pi/rpc.rs` — set the canonical thinking level and tool set for the role. Reuse `for_scout` / `for_worker` / `for_guard` as templates.

6. **Integrate into the swarm loop**. Where in `core/queen.rs::run_feature_full` (queen.rs:962) does this role run? Before Scout? Between Scout and Worker? After Worker? Add the dispatch block:
   - Update `FeatureStatus` in `domain/swarm.rs` if a new status applies.
   - Spawn the session with a namespaced session id (`<role>-{swarm_id}-{feature_id}`, see queen.rs:1074).
   - Set `SessionOwner::Swarm { swarm_id, role: "<role>".to_string() }` so Nurse can attribute interventions.
   - Wire up the activity-stream forwarder (`spawn_agent_forwarder`) and the `agent_start_payload` / `agent_end_payload` calls so the frontend's swarm-activity log shows the role.
   - Record usage via `record_session_usage` so the Dashboard sees the cost.
   - Kill the session at the end with `pi_manager.kill_session(...)`; emit `pi_session_killed`.
   - Add a new `ProgressEventType::<Role>Started` / `<Role>Completed` (and update `state/progress.rs`).

7. **Add Nurse handling if the role has its own failure mode.** Most roles need nothing extra — Nurse classifies on session metadata and doesn't care about the role label. But if the role has bespoke retry / fix-up semantics (the way Worker's `HandoffParseFailed` triggers a visible Nurse bubble in queen.rs:2095), add the matching downcast path next to the existing `HandoffParseFailed` case.

8. **Update `PRODUCT.md §4`** ("The Bee Colony — Agent Roles") — add a row to the table with role, thinking level, tool set, and one-sentence description. Also update `PRODUCT.md §13` glossary.

9. **Update `CLAUDE.md`**:
   - §Project Layout — add the new file under `core/`.
   - §Project Layout `prompts/` block — add the prompt filename and note whether it's `include_str!`'d.
   - §Key Types — if you added a `pub` type other subsystems reach for.
   - §Tauri Commands — only if you added IPC commands for the role.
   - §Tauri Events — only if you emit new events.

10. **Update this doc** — add a new top-level section for the role, mirroring the existing five (Role, Thinking & tool set, System prompt summary, Lifecycle, Inputs, Outputs, Failure modes, Code path). Update the §2 prompt-loading table.

11. **Add tests.** Mirror the existing per-role test layout in `core/<role>.rs`:
    - Unit tests for the user-prompt builder (`build_<role>_prompt`).
    - Unit tests for the tool-args parser.
    - E2E tests against `MockRpcClient` covering the success path, the missing-tool-call path, and the transport-crash path (see `scout_e2e_*` tests at scout.rs:226-307 for a template).

12. **Document in the per-subsystem README**. The swarm engine README (`app/src-tauri/src/core/README.md`) is the canonical deep-dive — add a paragraph there describing where the new role fits in the integrated swarm loop.

A new role that goes through this checklist will show up in the Settings screen prompt browser, the SwarmControl activity log, the per-session debug routing files (`~/.hyvemind/debug/swarms/{swarm_id}/<role>-{feature_id}-{run_id}.jsonl`), the Dashboard cost breakdown, and the Nurse intervention dispatcher — all without further wiring.
