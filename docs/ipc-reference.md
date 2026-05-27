# Tauri IPC Reference

Every `#[tauri::command]` registered in `app/src-tauri/src/lib.rs` is documented here. The authoritative list of registered commands is the `tauri::generate_handler!` macro at `app/src-tauri/src/lib.rs:543`; if a handler isn't there it isn't callable from the renderer regardless of whether it's annotated.

All commands return `Result<T, IpcError>`. `IpcError` is a structured envelope (`app/src-tauri/src/state/ipc_error.rs:48`) — the frontend mirror lives at `app/src/lib/ipc.ts` with the same `kind` discriminator. Free-form `String` arms are lifted into `IpcError::Internal` via the blanket `From<String>` impl.

## IpcError envelope

Discriminator field is `kind` (snake_case). Common envelope fields: `message` (always), `id` (entity id), `details` (free-form JSON, often `{"chain": [...]}` from an `anyhow::Error`).

| `kind` | When thrown |
|--------|-------------|
| `provider_unauthenticated` | Provider rejected the request (401/403, `invalid_api_key`, `authentication_error`). The frontend prompts for credentials. |
| `provider_rate_limited` | Provider returned 429 / `rate_limit_error`. The frontend can suggest backing off. |
| `circuit_breaker_open` | Per-provider breaker is open; calls are short-circuited locally. |
| `not_found { resource, resource_id }` | A swarm/session/review/hivemind/extension lookup missed. The renamed payload fields avoid collision with the discriminator `kind` and the envelope-level `id`. |
| `validation` | Input failed validation (empty id, traversal, oversized payload, malformed body, illegal model name). User-attributable. |
| `not_approved` | Working-directory allowlist (audit 1.11) rejected the path, or subscription auth missing. |
| `internal` | I/O error, logic bug, panic, upstream 5xx that isn't a rate limit. `details.chain` carries the full `anyhow` chain when applicable. |

`IpcError::from_provider_error` (`app/src-tauri/src/state/ipc_error.rs:205`) maps stringy provider errors to the right typed envelope by substring-matching on `401` / `429` / `circuit breaker open` markers.

## Commands by bucket

Counts reflect the `invoke_handler!` registration at `app/src-tauri/src/lib.rs:543-647`. Total: 99 production commands + 1 debug-only (`clear_response_cache` is gated `#[cfg(debug_assertions)]`).

---

### Tasks (chat) — `commands/chat.rs` (7)

Internal name is "chat" for historical reasons; the user-facing surface is Tasks. See `PRODUCT.md §3`.

#### `send_message`
- **Signature**: `fn send_message(message: String, model: Option<String>, session_id: Option<String>, working_dir: Option<String>, thinking_level: Option<String>, system_prompt: Option<String>, tool_set: Option<String>, images: Option<Vec<ImagePayload>>, is_steer: Option<bool>) -> Result<String, IpcError>`
- **Purpose**: Send a prompt to a Pi session (spawning, reusing, or steering as appropriate) and stream the response back as `chat-event` Tauri events. Returns the session id immediately so the frontend can attach listeners before chunks arrive.
- **Delegates to**: `pi::manager::PiManager::spawn_session_with_options` / `get_session` (`app/src-tauri/src/commands/chat.rs:550-649`); steers via `PiSession::steer` (`app/src-tauri/src/commands/chat.rs:714`); streams via the engine forwarder in the same file.
- **Errors**: `validation` (message > 16 MiB cap; malformed `session_id`), `not_approved` (working dir not on allowlist), `internal` (Pi spawn failed), provider-mapped (`provider_unauthenticated` / `provider_rate_limited` / `circuit_breaker_open` lifted from Pi via `IpcError::from_provider_error`).
- **Notes**: Maps subscription-provider names (`chatgpt` → `openai-codex`, `claude-sub` → `anthropic`) before handing the model to Pi (`app/src-tauri/src/commands/chat.rs:360`). Graveyard fill-in via `merge_graveyard_into` (`app/src-tauri/src/commands/chat.rs:329`) so a respawn after eviction keeps the user's original system prompt / tool set / thinking level. Emits `chat-event` lifecycle (`start`, `chunk`, `thinking`, `phase`, `heartbeat`, `tool_*`, `done`, `error`, `queued`).

#### `stop_chat`
- **Signature**: `fn stop_chat(session_id: String) -> Result<(), IpcError>`
- **Purpose**: Gracefully abort an in-flight stream; falls back to killing the Pi process if abort fails. Kept-alive on graceful abort so the next message reuses the warm session.
- **Delegates to**: `PiSession::abort` then `PiManager::kill_session` (`app/src-tauri/src/commands/chat.rs:1613-1644`).
- **Errors**: `validation` (malformed `session_id`), `internal` (kill failed). Missing session is treated as success (idempotent).

#### `get_chat_history`
- **Signature**: `fn get_chat_history(session_id: String) -> Result<Vec<ChatMessage>, IpcError>`
- **Purpose**: Rebuild a chat transcript by replaying the in-memory `PiEvent` log into structured `ChatMessage` rows.
- **Delegates to**: `PiSession::get_transcript` (`app/src-tauri/src/commands/chat.rs:1665`).
- **Errors**: `validation`, `not_found { resource: "session" }` if the session isn't in the pool.

#### `list_chat_sessions`
- **Signature**: `fn list_chat_sessions() -> Result<Vec<String>, IpcError>`
- **Purpose**: List every persisted Pi session by reading `*.jsonl` stems out of `~/.hyvemind/chat-sessions/`.
- **Delegates to**: `std::fs::read_dir` against `AppState::chat_sessions_dir` (`app/src-tauri/src/commands/chat.rs:1858`).
- **Errors**: `internal` (directory unreadable).

#### `delete_chat_session`
- **Signature**: `fn delete_chat_session(session_id: String) -> Result<(), IpcError>`
- **Purpose**: Kill the live Pi session (if any) and remove the on-disk JSONL transcript. Idempotent.
- **Delegates to**: `PiManager::kill_session` + `std::fs::remove_file` (`app/src-tauri/src/commands/chat.rs:1893-1904`).
- **Errors**: `validation` (path-traversal guard — `session_id` is joined into `chat_sessions_dir`), `internal` (filesystem write failed).

#### `is_session_busy`
- **Signature**: `fn is_session_busy(session_id: String) -> Result<bool, IpcError>`
- **Purpose**: Cheap, non-blocking check whether the session is mid-prompt.
- **Delegates to**: `PiSession::is_busy` (`app/src-tauri/src/commands/chat.rs:1918`).
- **Errors**: `validation`. Missing session returns `Ok(false)` (no error).

#### `get_session_last_assistant_text`
- **Signature**: `fn get_session_last_assistant_text(session_id: String) -> Result<String, IpcError>`
- **Purpose**: Read the last assistant message's text from the authoritative on-disk Pi transcript. Used as a reconciliation fallback when the streamed `chat-event` IPC drops chunks under burst load.
- **Delegates to**: pure `parse_last_assistant_text` over the JSONL on disk (`app/src-tauri/src/commands/chat.rs:1935-2008`).
- **Errors**: `validation`, `internal` (file unreadable).

---

### Tasks (UI state) — `commands/tasks.rs` (6)

The persisted message-JSON blob the Tasks view writes per task plus auxiliary helpers (state snapshot, file picker, auto-commit).

#### `save_task_messages`
- **Signature**: `fn save_task_messages(task_id: String, messages: String) -> Result<(), IpcError>`
- **Purpose**: Persist the Tasks-view message array to `~/.hyvemind/task-messages/{task_id}.json` via an atomic write.
- **Delegates to**: `state::store::atomic_write` (`app/src-tauri/src/commands/tasks.rs:74`).
- **Errors**: `validation` (`task_id` traversal), `internal` (disk I/O).

#### `load_task_messages`
- **Signature**: `fn load_task_messages(task_id: String) -> Result<Option<String>, IpcError>`
- **Purpose**: Read the persisted JSON blob; returns `None` if no file exists (new/never-saved task).
- **Delegates to**: `std::fs::read_to_string` (`app/src-tauri/src/commands/tasks.rs:102`).
- **Errors**: `validation`, `internal`.

#### `delete_task_messages`
- **Signature**: `fn delete_task_messages(task_id: String) -> Result<(), IpcError>`
- **Purpose**: Remove the persisted JSON blob. Idempotent on `NotFound`.
- **Delegates to**: `std::fs::remove_file` (`app/src-tauri/src/commands/tasks.rs:126`).
- **Errors**: `validation`, `internal`.

#### `get_task_state`
- **Signature**: `fn get_task_state(task_id: String, session_id: Option<String>) -> Result<TaskStateSnapshot, IpcError>`
- **Purpose**: Authoritative snapshot the frontend uses to resync after a focus/reload event. Combines persisted message JSON + live Pi session state (`session_alive`, `session_busy`, in-memory transcript) for the supplied `session_id`.
- **Delegates to**: `PiManager::get_session` + `PiSession::is_alive` / `is_busy` / `get_transcript` (`app/src-tauri/src/commands/tasks.rs:185-219`).
- **Errors**: `validation`, `internal`.

#### `auto_commit_task`
- **Signature**: `fn auto_commit_task(working_dir: String, task_title: String) -> Result<AutoCommitResult, IpcError>`
- **Purpose**: Run `git add -A && git commit -m <ai-title>` for the Tasks-toolbar Auto-commit button. AI title generation uses the configured default model and the staged diff (truncated, binary-aware); falls back to the sanitized task title, then to a static `"Auto-commit"` / `"chore: auto-commit"` if everything else fails.
- **Delegates to**: `tokio::process::Command` for git; `ProviderRegistry::get(...).call(...)` for the title (`app/src-tauri/src/commands/tasks.rs:717-1014`).
- **Errors**: Almost never returns `Err` — non-fatal guard conditions (not a repo, no changes, git missing) flow through `AutoCommitResult { ok: false, message, .. }`. Only programming errors escape.
- **Notes**: Per-directory mutex (`AppState::auto_commit_locks`) serialises concurrent auto-commits in the same repo. Working dir is checked against the approved-dirs allowlist; rejection collapses to `ok: false`.

#### `list_project_files`
- **Signature**: `fn list_project_files(working_dir: String, query: String, limit: Option<usize>) -> Result<Vec<ProjectFileEntry>, IpcError>`
- **Purpose**: File-tree walk for the `@`-mention picker. Respects `.gitignore`, skips `node_modules`/`target`/`dist`/`build`/`__pycache__`, scores by basename-exact / startswith / contains / path-contains (1000 / 800 / 500–201 / 200–1). Empty query returns most-recently-modified.
- **Delegates to**: `ignore::WalkBuilder` wrapped in `tokio::task::spawn_blocking` (`app/src-tauri/src/commands/tasks.rs:332-499`).
- **Errors**: `not_approved` (working dir not on allowlist), `internal` (walk task panic).

---

### Hivemind — `commands/hivemind.rs` (18 + 1 debug-only)

`clear_response_cache` is only registered in debug builds (`#[cfg(debug_assertions)]` at `app/src-tauri/src/lib.rs:571`).

#### `start_review`
- **Signature**: `fn start_review(plan: String, stance: Option<String>, num_rounds: Option<u32>, timeout_seconds: Option<u32>, models: Option<Vec<String>>, review_id: Option<String>, hivemind_id: Option<String>, name: Option<String>, task_id: Option<String>, round_number: Option<u32>, model_context_windows: Option<HashMap<String,u64>>, model_temperatures: Option<HashMap<String,f64>>, model_top_ps: Option<HashMap<String,f64>>) -> Result<String, IpcError>`
- **Purpose**: Kick off a Hivemind review. Returns a server-generated `job_id` immediately; the engine runs in a detached `tokio::spawn` and emits `hivemind-progress` events through completion.
- **Delegates to**: `hivemind::engine::ReviewEngine::run` (`app/src-tauri/src/commands/hivemind.rs:399-538`); persists via `HivemindStore::create_job` (`app/src-tauri/src/commands/hivemind.rs:267`).
- **Errors**: `validation` (plan > `MAX_PLAN_LEN`; malformed `review_id`/`hivemind_id`/`task_id`), `internal` (store / engine plumbing).
- **Notes**: `stance` argument is currently ignored — hardcoded `Against`. `round_number` is the 1-based cumulative round for the Tasks-flow multi-round driver; collapses to `round_offset = round_number - 1` so capture files (`merge-rN.txt`, `output-*-rN.txt`) don't overwrite earlier rounds.

#### `get_review_status`
- **Signature**: `fn get_review_status(job_id: String) -> Result<ReviewStatus, IpcError>`
- **Purpose**: Snapshot of one review job — high-level status, cost, per-step summaries with truncated 200-char previews.
- **Delegates to**: `HivemindStore::get_job` + `get_job_steps` (`app/src-tauri/src/commands/hivemind.rs:716-727`).
- **Errors**: `internal` (job not found returns a stringy `Err` lifted to `Internal`).

#### `list_reviews`
- **Signature**: `fn list_reviews(limit: Option<u32>, offset: Option<u32>, hivemind_id: Option<String>) -> Result<ListReviewsResponse, IpcError>`
- **Purpose**: Paginated list of logical review runs (Hivemind groups child jobs under a synthetic `hmr-*` id). Filterable by `hivemind_id`.
- **Delegates to**: `HivemindStore::count_logical_runs` + `list_logical_run_page` (`app/src-tauri/src/commands/hivemind.rs:1128-1138`).
- **Errors**: `validation` (malformed `hivemind_id`), `internal`.

#### `create_hivemind`
- **Signature**: `fn create_hivemind(name: String, description: String, rounds_config: String, inherit_orchestrator: Option<bool>, orchestrator_model: Option<String>, orchestrator_provider: Option<String>, orchestrator_thinking: Option<String>, orchestrator_context_window: Option<i64>, orchestrator_max_output: Option<i64>) -> Result<HivemindSummary, IpcError>`
- **Purpose**: Insert a new Hivemind config row (the "team of models" template).
- **Delegates to**: `HivemindStore::create_hivemind` (`app/src-tauri/src/commands/hivemind.rs:1217`).
- **Errors**: `internal` (`rounds_config` > `MAX_ROUNDS_CONFIG`; store failure).

#### `list_hiveminds`
- **Signature**: `fn list_hiveminds(limit: Option<u32>, offset: Option<u32>) -> Result<Vec<HivemindSummary>, IpcError>`
- **Purpose**: Paginated list of saved Hivemind configs with run counts (`batch_count_hivemind_runs`).
- **Delegates to**: `HivemindStore::list_hiveminds` + `batch_count_hivemind_runs` (`app/src-tauri/src/commands/hivemind.rs:1260-1271`).
- **Errors**: `internal`.

#### `update_hivemind`
- **Signature**: `fn update_hivemind(hivemind_id: String, name: String, description: String, rounds_config: String, inherit_orchestrator: Option<bool>, orchestrator_model: Option<String>, orchestrator_provider: Option<String>, orchestrator_thinking: Option<String>, orchestrator_context_window: Option<i64>, orchestrator_max_output: Option<i64>) -> Result<HivemindSummary, IpcError>`
- **Purpose**: Replace an existing Hivemind config in place.
- **Delegates to**: `HivemindStore::update_hivemind` (`app/src-tauri/src/commands/hivemind.rs:1312`).
- **Errors**: `validation` (bad `hivemind_id`; oversized `rounds_config`), `internal` (id not found / store error).

#### `delete_hivemind`
- **Signature**: `fn delete_hivemind(hivemind_id: String) -> Result<(), IpcError>`
- **Purpose**: Remove a Hivemind config row.
- **Delegates to**: `HivemindStore::delete_hivemind` (`app/src-tauri/src/commands/hivemind.rs:1372`).
- **Errors**: `validation`, `internal`.

#### `get_review_step_outputs`
- **Signature**: `fn get_review_step_outputs(job_id: String) -> Result<Vec<StepOutput>, IpcError>`
- **Purpose**: Untruncated step outputs (the frontend feeds these into the merge agent).
- **Delegates to**: `HivemindStore::get_job_steps` (`app/src-tauri/src/commands/hivemind.rs:1407`).
- **Errors**: `validation`, `internal`.

#### `get_review_state`
- **Signature**: `fn get_review_state(job_id: String) -> Result<ReviewStateSnapshot, IpcError>`
- **Purpose**: Canonical resync snapshot for one job — full step outputs (untruncated), `is_running` derived server-side. Used after focus changes or dropped `hivemind-progress` events.
- **Delegates to**: `build_review_state_snapshot` (`app/src-tauri/src/commands/hivemind.rs:1049`).
- **Errors**: `validation`, `internal`.

#### `log_review_event`
- **Signature**: `fn log_review_event(review_id: String, event_type: String, data: serde_json::Value) -> Result<(), IpcError>`
- **Purpose**: Append a frontend-originated event to the per-review JSONL log (silently no-ops when debug logging is off or the `review_id` has no live logger).
- **Delegates to**: `ReviewLogger::log` (`app/src-tauri/src/commands/hivemind.rs:1607`).
- **Errors**: `validation` (bad `review_id`, oversized payload).

#### `get_review_plan`
- **Signature**: `fn get_review_plan(review_id: String) -> Result<String, IpcError>`
- **Purpose**: Return the enriched prompt (plan + source context) the first round used, so the Replay action can re-run the review with a different Hivemind without re-gathering context.
- **Delegates to**: `HivemindStore::count_jobs_with_review_id` / `list_jobs_by_review_id` / `get_job` (`app/src-tauri/src/commands/hivemind.rs:1626-1682`).
- **Errors**: `validation` (bad id), `not_found { resource: "review" }`, `validation` (review has no stored plan).

#### `cancel_review`
- **Signature**: `fn cancel_review(job_id: String) -> Result<(), IpcError>`
- **Purpose**: Signal the registered `CancellationToken`, mark the SQLite job `cancelled`, emit `hivemind-progress { event_type: "cancelled" }`. Safe on already-finished jobs.
- **Delegates to**: `running_reviews` registry + `HivemindStore::update_job_status` (`app/src-tauri/src/commands/hivemind.rs:581-622`).
- **Errors**: `validation`, `internal` (Db update failed).

#### `delete_review`
- **Signature**: `fn delete_review(run_id: String) -> Result<(), IpcError>`
- **Purpose**: Permanently delete a logical review run + all on-disk artifacts (`{reviews_dir}/{run_id}.jsonl`, `{reviews_dir}/{run_id}/`, every merge output). Refuses to delete if any child job is still `pending` / `running` / `round_*`.
- **Delegates to**: `HivemindStore::any_job_in_status_for_logical_run` + `delete_logical_run` (`app/src-tauri/src/commands/hivemind.rs:640-700`).
- **Errors**: `validation` ("Cannot delete a running review. Cancel it first."), `internal`.

#### `save_round_verdicts`
- **Signature**: `fn save_round_verdicts(job_id: String, round_number: u32, verdicts: Vec<RoundVerdict>) -> Result<(), IpcError>`
- **Purpose**: Persist the orchestrator's per-suggestion verdicts after a merge completes. Idempotent — re-saving overwrites the prior `(job_id, round_number)` rows.
- **Delegates to**: `HivemindStore::save_round_verdicts` (`app/src-tauri/src/commands/hivemind.rs:1454`).
- **Errors**: `validation`, `internal`. Emits `hivemind-progress { event_type: "verdicts_updated" }` on success.

#### `list_round_verdicts`
- **Signature**: `fn list_round_verdicts(job_id: String) -> Result<Vec<RoundVerdict>, IpcError>`
- **Purpose**: Fetch every persisted verdict for one job (or every child of a logical `hmr-*` run), sorted by `(round_number, reviewer_model)`. Compensates for round-offset rewriting on legacy rows.
- **Delegates to**: `HivemindStore::list_round_verdicts` / `fetch_round_verdicts_for_jobs` (`app/src-tauri/src/commands/hivemind.rs:1502-1546`).
- **Errors**: `validation`, `not_found { resource: "review" }`, `internal`.

#### `read_merge_output`
- **Signature**: `fn read_merge_output(job_id: String, round: i64) -> Result<String, IpcError>`
- **Purpose**: Read the on-disk merge output for `(job_id, round)`. Returns an empty string when no row exists or the file is missing (UI uses this to preview partial text after an interruption).
- **Delegates to**: `HivemindStore::get_merge_run` + `tokio::fs::read_to_string` (`app/src-tauri/src/commands/hivemind.rs:1700-1715`).
- **Errors**: `validation`, `internal`.

#### `register_context_session`
- **Signature**: `fn register_context_session(review_id: String, session_id: String, model_id: String, provider: String) -> Result<(), IpcError>`
- **Purpose**: Tell the store which Pi session ran the context-gather phase for a review, so `get_orchestrator_usage` can attribute its tokens.
- **Delegates to**: `HivemindStore::upsert_review_context_session` (`app/src-tauri/src/commands/hivemind.rs:1764`).
- **Errors**: `validation` (any of the four ids invalid), `internal`.

#### `get_orchestrator_usage`
- **Signature**: `fn get_orchestrator_usage(review_id: String) -> Result<OrchestratorUsage, IpcError>`
- **Purpose**: Aggregate token / cost / duration across the orchestrator's context + merge sessions for one review.
- **Delegates to**: `HivemindStore::get_review_context_session` + `list_merge_runs_by_review_id` + `UsageStore::get_usage_for_sessions` (`app/src-tauri/src/commands/hivemind.rs:1779-1825`).
- **Errors**: `validation`, `internal`.

#### `get_resumable_review_for_task`
- **Signature**: `fn get_resumable_review_for_task(task_id: String) -> Result<Option<ResumableReviewSnapshot>, IpcError>`
- **Purpose**: Return the most-recent resumable review attached to a task. `Ok(None)` when no review exists, the latest one is `cancelled`/`failed`, or it's `completed` without a final merge to apply.
- **Delegates to**: `HivemindStore::latest_job_for_task` + `build_resumable_snapshot` (`app/src-tauri/src/commands/hivemind.rs:2042-2082`).
- **Errors**: `validation`, `internal`.

#### `clear_response_cache` *(debug builds only)*
- **Signature**: `fn clear_response_cache() -> Result<CacheMetricsSnapshot, IpcError>`
- **Purpose**: Wipe the process-wide `ResponseCache` and return the pre-wipe metrics. For local cache debugging only.
- **Delegates to**: `ResponseCache::clear` + `metrics` (`app/src-tauri/src/commands/hivemind.rs:1575-1588`).
- **Errors**: None in practice.

---

### Swarms — `commands/swarms.rs` (15)

#### `create_swarm`
- **Signature**: `fn create_swarm(name: String, description: String, working_directory: String, model_settings: serde_json::Value) -> Result<SwarmState, IpcError>`
- **Purpose**: Create a fresh swarm (idle, no features yet). Generates a fresh id, initialises the on-disk directory under `~/.hyvemind/swarms/{id}/`, registers in the swarm registry.
- **Delegates to**: `SwarmStore::init_swarm` + `write_state` + `SwarmRegistry::register` (`app/src-tauri/src/commands/swarms.rs:118-138`).
- **Errors**: `validation` (`description` > `MAX_GOAL_LEN`; oversized JSON payload), `not_approved` (working dir not on allowlist), `internal`.

#### `start_swarm`
- **Signature**: `fn start_swarm(swarm_id: String, features: Vec<serde_json::Value>, milestones: Option<Vec<serde_json::Value>>) -> Result<(), IpcError>`
- **Purpose**: Spawn the Queen orchestrator as a detached task with the supplied feature + milestone lists. Rejects if the swarm is already active. Milestones fall back to `milestones.json` on disk when omitted.
- **Delegates to**: `SwarmRegistry::get_start_lock` + `spawn_queen_task` (`app/src-tauri/src/commands/swarms.rs:184-198`).
- **Errors**: `validation` (bad id / oversized JSON / already-running), `not_found`, `internal`.

#### `update_swarm`
- **Signature**: `fn update_swarm(swarm_id: String, name: String, working_directory: String, model_settings: serde_json::Value) -> Result<SwarmState, IpcError>`
- **Purpose**: Edit metadata + model settings on an idle swarm. Refuses to edit while `Implementing`. Persists to disk and syncs the registry.
- **Delegates to**: pure `apply_update` helper (`app/src-tauri/src/commands/swarms.rs:1173`) + `SwarmStore::write_state` + `SwarmRegistry::replace_state` (`app/src-tauri/src/commands/swarms.rs:1272-1297`).
- **Errors**: `validation`, `not_approved`, `not_found`, `internal`.

#### `pause_swarm`
- **Signature**: `fn pause_swarm(swarm_id: String) -> Result<(), IpcError>`
- **Purpose**: Pause a running swarm via its `CancellationToken`-driven pause flag. Idempotent on swarms that aren't in the registry.
- **Delegates to**: `SwarmRegistry::pause` (`app/src-tauri/src/commands/swarms.rs:1352`).
- **Errors**: `validation`, `not_found`, `internal`.

#### `resume_swarm`
- **Signature**: `fn resume_swarm(swarm_id: String) -> Result<(), IpcError>`
- **Purpose**: Two paths. Fast path: the queen task is alive but paused — just wake it. Slow path: queen died with the process — rehydrate features/milestones from disk, reset mid-flight feature statuses to `Pending`, flip the swarm back to `Implementing`, and spawn a fresh queen.
- **Delegates to**: `SwarmRegistry::resume` (fast) or `SwarmStore::read_state/features/milestones` + `spawn_queen_task` (slow) (`app/src-tauri/src/commands/swarms.rs:1387-1497`).
- **Errors**: `validation` (bad id / not resumable status), `not_approved` (working dir gone from allowlist), `not_found`, `internal`.

#### `stop_swarm`
- **Signature**: `fn stop_swarm(swarm_id: String) -> Result<(), IpcError>`
- **Purpose**: Three-case idempotent stop. Registry-running → cancel queen + reap Pi sessions. Registry-missing + disk non-terminal → write `Cancelled` to disk. Registry-missing + disk terminal → no-op.
- **Delegates to**: `SwarmRegistry::stop` + `PiManager::kill_sessions_for_swarm` + `resolve_stop_from_disk` helper (`app/src-tauri/src/commands/swarms.rs:1518-1568`).
- **Errors**: `validation`, `not_found`, `internal`.

#### `get_swarm`
- **Signature**: `fn get_swarm(swarm_id: String) -> Result<SwarmState, IpcError>`
- **Purpose**: Return the current `SwarmState`. Registry first, then disk.
- **Delegates to**: `SwarmRegistry::get_state` then `SwarmStore::read_state` (`app/src-tauri/src/commands/swarms.rs:1586-1599`).
- **Errors**: `validation`, `not_found`, `internal`.

#### `list_swarms`
- **Signature**: `fn list_swarms() -> Result<Vec<SwarmState>, IpcError>`
- **Purpose**: All known swarms (registry + disk merged), with a best-effort reconciliation pass that rewrites disk-only `Implementing` rows to `Failed`/`Interrupted`.
- **Delegates to**: `SwarmRegistry::list_all` + `SwarmStore::list_swarms` + `core::recovery::reconcile_orphaned_swarms` (`app/src-tauri/src/commands/swarms.rs:1620-1642`).
- **Errors**: `internal`.

#### `delete_swarm`
- **Signature**: `fn delete_swarm(swarm_id: String) -> Result<(), IpcError>`
- **Purpose**: Permanent removal — stop first if running, then `remove_dir_all` the swarm directory.
- **Delegates to**: `SwarmRegistry::stop` / `remove` + `SwarmStore::delete_swarm` (`app/src-tauri/src/commands/swarms.rs:1674-1695`).
- **Errors**: `validation` (defends against traversal that would wipe the wrong directory), `internal`.

#### `get_swarm_progress`
- **Signature**: `fn get_swarm_progress(swarm_id: String) -> Result<Vec<ProgressEvent>, IpcError>`
- **Purpose**: Read the full append-only `progress_log.jsonl` for a swarm.
- **Delegates to**: `ProgressReader::read_all` (`app/src-tauri/src/commands/swarms.rs:1718`).
- **Errors**: `validation`, `internal`.

#### `get_swarm_activity_log`
- **Signature**: `fn get_swarm_activity_log(swarm_id: String, after_seq: Option<u64>, limit: Option<u32>) -> Result<SwarmActivityLogPage, IpcError>`
- **Purpose**: Paginate through the per-swarm `activity_log.jsonl` (the firehose feed `swarm-activity` events are minted from). SwarmControl calls this on mount so it can replay history before live events arrive; the `swarmActivityStore` dedupes against live events via per-event `seq`.
- **Delegates to**: `state::activity_log::ActivityReader::page` inside `spawn_blocking` (`app/src-tauri/src/commands/swarms.rs:1770`).
- **Errors**: `validation`, `not_found { resource: "swarm" }` (id unknown to `SwarmStore`), `internal`.

#### `get_swarm_features`
- **Signature**: `fn get_swarm_features(swarm_id: String) -> Result<Vec<Feature>, IpcError>`
- **Purpose**: Read persisted `features.json`. Returns empty when the swarm exists but hasn't been started.
- **Delegates to**: `SwarmStore::read_features` (`app/src-tauri/src/commands/swarms.rs:1796`).
- **Errors**: `validation`, `internal`.

#### `get_swarm_milestones`
- **Signature**: `fn get_swarm_milestones(swarm_id: String) -> Result<Vec<Milestone>, IpcError>`
- **Purpose**: Read persisted `milestones.json`. Empty vec when no milestones were defined.
- **Delegates to**: `SwarmStore::read_milestones` (`app/src-tauri/src/commands/swarms.rs:1816`).
- **Errors**: `validation`, `internal`.

#### `get_swarm_usage`
- **Signature**: `fn get_swarm_usage(swarm_id: String) -> Result<SwarmUsageSummary, IpcError>`
- **Purpose**: Aggregate tokens + cost + duration for a swarm. Sums `usage_log` rows whose `source_id` is `swarm_id` or `swarm_id:%` (catches scout/worker/guard and Hivemind context/round/merge phases run on the swarm's behalf), then adds live in-memory totals from the registry accumulator AND live token counts from busy Pi sessions still owned by the swarm.
- **Delegates to**: direct `sqlx::query` against `UsageStore::pool()` + `SwarmRegistry::get_usage_accumulator` + `PiManager::list_sessions` + `PiSession::get_session_stats` (`app/src-tauri/src/commands/swarms.rs:1846-1933`).
- **Errors**: `validation`, `internal`.

#### `check_swarm_readiness`
- **Signature**: `fn check_swarm_readiness(swarm_id: String, manifest: serde_json::Value) -> Result<ReadinessReport, IpcError>`
- **Purpose**: Phase 4B swarm-readiness check. The frontend MUST call this with the plan's `readiness_manifest` before `start_swarm`; a failing report blocks the launch UI.
- **Delegates to**: `core::readiness::check_readiness` (`app/src-tauri/src/commands/swarms.rs:1988`).
- **Errors**: `validation` (id / oversized manifest / manifest deserialise), `not_approved`, `not_found`, `internal`.

---

### Settings — `commands/settings.rs` (29)

#### `get_settings`
- **Signature**: `fn get_settings() -> Result<SettingsResponse, IpcError>`
- **Purpose**: Return the current config with API keys redacted to last-4 + the configured-provider list.
- **Delegates to**: `build_settings_response` reading `AppState::config` (`app/src-tauri/src/commands/settings.rs:220`).
- **Errors**: None in practice.

#### `set_runtime_settings`
- **Signature**: `fn set_runtime_settings(concurrency_cap: usize, max_pi_processes: usize) -> Result<SettingsResponse, IpcError>`
- **Purpose**: Persist Hivemind concurrency cap + Pi process ceiling.
- **Delegates to**: `Config::write_bytes` (`app/src-tauri/src/commands/settings.rs:243-256`).
- **Errors**: `internal` (`concurrency_cap` or `max_pi_processes` < 1).

#### `set_auto_commit_tasks`
- **Signature**: `fn set_auto_commit_tasks(enabled: bool) -> Result<(), IpcError>`
- **Purpose**: Toggle the Tasks-view auto-commit feature.
- **Delegates to**: `Config::write_bytes` (`app/src-tauri/src/commands/settings.rs:268-276`).
- **Errors**: `internal`.

#### `set_auto_commit_conventional`
- **Signature**: `fn set_auto_commit_conventional(enabled: bool) -> Result<(), IpcError>`
- **Purpose**: Switch auto-commit titles to Conventional Commits format.
- **Delegates to**: `Config::write_bytes` (`app/src-tauri/src/commands/settings.rs:290-298`).
- **Errors**: `internal`.

#### `set_crash_reporting`
- **Signature**: `fn set_crash_reporting(enabled: bool) -> Result<(), IpcError>`
- **Purpose**: Opt in/out of Sentry. Persisted; applied on next launch.
- **Delegates to**: `Config::write_bytes` (`app/src-tauri/src/commands/settings.rs:313-321`).
- **Errors**: `internal`.

#### `set_chat_check_in_secs`
- **Signature**: `fn set_chat_check_in_secs(secs: u64) -> Result<SettingsResponse, IpcError>`
- **Purpose**: Nurse chat check-in interval. Clamped to `[CHAT_CHECK_IN_MIN_SECS, CHAT_CHECK_IN_MAX_SECS]`.
- **Delegates to**: `Config::write_bytes` (`app/src-tauri/src/commands/settings.rs:343-352`).
- **Errors**: `internal` (out of range).

#### `set_extension_poll_interval_secs`
- **Signature**: `fn set_extension_poll_interval_secs(secs: u64) -> Result<SettingsResponse, IpcError>`
- **Purpose**: Global extension-poller interval. Clamped; effective on the next poller tick.
- **Delegates to**: `Config::write_bytes` (`app/src-tauri/src/commands/settings.rs:378-387`).
- **Errors**: `internal` (out of range).

#### `set_daily_budget`
- **Signature**: `fn set_daily_budget(usd: Option<f64>) -> Result<SettingsResponse, IpcError>`
- **Purpose**: Set or clear the daily spending cap consulted by `core::queen::run_swarm_full` between batches. `None` means unlimited.
- **Delegates to**: `Config::write_bytes` (`app/src-tauri/src/commands/settings.rs:411-419`).
- **Errors**: `internal` (NaN/Infinity/negative).

#### `set_task_completion_sound`
- **Signature**: `fn set_task_completion_sound(enabled: bool, sound: String) -> Result<SettingsResponse, IpcError>`
- **Purpose**: Persist the completion-sound toggle + which built-in sound. Validates against `["chime", "pop", "bell", "success", "tweet"]`.
- **Delegates to**: `Config::write_bytes` (`app/src-tauri/src/commands/settings.rs:445-454`).
- **Errors**: `internal`.

#### `save_api_key`
- **Signature**: `fn save_api_key(provider: String, api_key: String) -> Result<(), IpcError>`
- **Purpose**: Persist a provider API key. Writes to the OS keychain (single combined entry — one prompt regardless of how many providers are configured), the encrypted `.credentials` file cache, and metadata into `config.json`. Refreshes the Pi env-vars, provider registry, and extension registry.
- **Delegates to**: `SecretStore::save_all` + `SecretStore::save_to_file` + `Config::save_blocking` + `PiManager::update_env_vars` + `AppState::refresh_provider_registry` + `refresh_extension_registry` (`app/src-tauri/src/commands/settings.rs:492-546`).
- **Errors**: `validation` (empty provider / key), `internal`.
- **Notes**: All blocking I/O runs inside `spawn_blocking` so the macOS Keychain prompt doesn't block other config readers.

#### `delete_api_key`
- **Signature**: `fn delete_api_key(provider: String) -> Result<(), IpcError>`
- **Purpose**: Remove a stored key. Same multi-store update + registry refresh pattern as `save_api_key`. Best-effort legacy per-provider keychain entry cleanup.
- **Delegates to**: `SecretStore::save_all` + `delete` + `Config::save_blocking` (`app/src-tauri/src/commands/settings.rs:576-628`).
- **Errors**: `validation`, `internal`.

#### `set_default_model`
- **Signature**: `fn set_default_model(model: String) -> Result<(), IpcError>`
- **Purpose**: Persist the default `provider/model_id` selection. Emits `default-model-changed` so the frontend updates its cached value.
- **Delegates to**: `Config::write_bytes` + `app.emit` (`app/src-tauri/src/commands/settings.rs:646-664`).
- **Errors**: `internal`.

#### `set_default_hivemind`
- **Signature**: `fn set_default_hivemind(hivemind_id: String) -> Result<(), IpcError>`
- **Purpose**: Persist the default Hivemind for new tasks. Empty string clears. Emits `default-hivemind-changed`.
- **Delegates to**: `Config::write_bytes` + `app.emit` (`app/src-tauri/src/commands/settings.rs:822-844`).
- **Errors**: `validation`, `internal`.

#### `set_default_project_path`
- **Signature**: `fn set_default_project_path(path: String) -> Result<(), IpcError>`
- **Purpose**: Persist the canonical default project path. Implicitly approves the path via `Config::add_approved_working_dir` (audit 1.11). Emits `default-project-path-changed`.
- **Delegates to**: `validate_working_dir` + `Config::write_bytes` (`app/src-tauri/src/commands/settings.rs:680-712`).
- **Errors**: `validation` (path doesn't exist), `internal`.

#### `request_working_dir_approval`
- **Signature**: `fn request_working_dir_approval(path: String) -> Result<bool, IpcError>`
- **Purpose**: Audit 1.11 — when a user picks a directory not in `approved_working_dirs`, the frontend calls this after the user clicks Allow on the approval modal. Adds the canonical path to the allowlist.
- **Delegates to**: `commands::util::canonicalize_working_dir` + `Config::add_approved_working_dir` (`app/src-tauri/src/commands/settings.rs:732-754`).
- **Errors**: `internal` (validation failure surfaces as `Internal` via the stringy `?` path).

#### `get_providers`
- **Signature**: `fn get_providers() -> Result<Vec<ProviderInfo>, IpcError>`
- **Purpose**: List configured providers with their auth-status, sorted configured-first then alphabetical. Subscription providers (`chatgpt`, `claude-sub`) consult `check_pi_subscription_auth` for the `configured` flag; everything else checks for a stored API key.
- **Delegates to**: `Config::providers` + `check_pi_subscription_auth` (`app/src-tauri/src/commands/settings.rs:765-799`).
- **Errors**: None in practice.

#### `add_provider`
- **Signature**: `fn add_provider(id: String, display_name: String, provider_type: Option<String>, endpoint: Option<String>) -> Result<(), IpcError>`
- **Purpose**: Register a new provider entry in config (typically a user-added OpenAI-compatible endpoint). Validates `provider_type` against `ALLOWED_PROVIDER_TYPES` and the endpoint URL when present.
- **Delegates to**: `Config::providers.insert` + write (`app/src-tauri/src/commands/settings.rs:859-947`).
- **Errors**: `validation` (empty id / display name / bad provider_type / bad endpoint URL), `internal`.

#### `test_provider_models`
- **Signature**: `fn test_provider_models(provider: String) -> Result<TestModelsResult, IpcError>`
- **Purpose**: Connectivity test for a provider's `/models` endpoint. Anthropic uses `x-api-key` + `anthropic-version`; others use Bearer auth. Returns `{ ok, models, details, error }`. Enriches OpenRouter / generic OpenAI rich-format responses with catalog data; falls back to a bare `{ id }` parser so listings never silently fail.
- **Delegates to**: `reqwest::Client` (`app/src-tauri/src/commands/settings.rs:1608-1956`).
- **Errors**: `internal` (rare — unknown provider). Real failures flow through `TestModelsResult { ok: false, error: Some(...) }`.

#### `test_provider_chat`
- **Signature**: `fn test_provider_chat(provider: String, model: String) -> Result<TestChatResult, IpcError>`
- **Purpose**: Send a one-shot hello prompt directly via the provider's HTTP API. Confirms credentials + endpoint + model name are wired up.
- **Delegates to**: `reqwest::Client` (`app/src-tauri/src/commands/settings.rs:2195-2356`).
- **Errors**: `internal`. Normal failures collapse to `TestChatResult { ok: false, error }`.

#### `test_provider_pi`
- **Signature**: `fn test_provider_pi(provider: String, model: String) -> Result<TestPiResult, IpcError>`
- **Purpose**: End-to-end Pi-routing smoke test. Spawns a throwaway Pi session, sends a hello prompt with 60s timeout, kills the session. Confirms Pi's extension system actually delivers requests for this provider.
- **Delegates to**: `PiManager::spawn_session_with_options` + `PiSession::send_prompt` + `collect_response` (`app/src-tauri/src/commands/settings.rs:2381-2455`).
- **Errors**: `internal`. Normal failures collapse to `TestPiResult { ok: false, error }`.

#### `refresh_models`
- **Signature**: `fn refresh_models(provider: Option<String>) -> Result<Vec<ModelInfoResponse>, IpcError>`
- **Purpose**: Returns the hardcoded built-in model catalog filtered by provider. (Live provider model lists come from `test_provider_models`; this is the static cost/context-window catalog the ModelBrowser falls back to.)
- **Delegates to**: pure `get_model_catalog` (`app/src-tauri/src/commands/settings.rs:953-956`).
- **Errors**: None.

#### `get_pi_status`
- **Signature**: `fn get_pi_status() -> Result<PiStatusResponse, IpcError>`
- **Purpose**: Pi binary install status — resolved path, install method (npm/homebrew/unknown), installed version (via `pi --version`), latest npm version, `is_outdated`.
- **Delegates to**: `Config::pi_binary_path` + `get_installed_version` + `get_latest_npm_version` (`app/src-tauri/src/commands/settings.rs:1966-2025`).
- **Errors**: None (missing binary surfaces in `PiStatusResponse.error`).

#### `update_pi`
- **Signature**: `fn update_pi() -> Result<(), IpcError>`
- **Purpose**: Run the detected package manager's update command (npm `install -g pkg@latest` or `brew upgrade <name>`). Streams stdout/stderr through the `pi-update-progress` Tauri event.
- **Delegates to**: `tokio::process::Command` (`app/src-tauri/src/commands/settings.rs:2053-2109`).
- **Errors**: `validation` (unknown install method), `internal` (spawn / non-zero exit).

#### `install_pi`
- **Signature**: `fn install_pi() -> Result<(), IpcError>`
- **Purpose**: First-time `npm install -g @earendil-works/pi-coding-agent`. Verifies npm is on PATH and gives a friendly error pointing at nodejs.org otherwise. Streams progress through `pi-install-progress`.
- **Delegates to**: `tokio::process::Command` (`app/src-tauri/src/commands/settings.rs:2156-2189`).
- **Errors**: `validation` (npm missing), `internal` (spawn / non-zero exit).

#### `open_pi_terminal`
- **Signature**: `fn open_pi_terminal() -> Result<(), IpcError>`
- **Purpose**: Launch the bundled Pi binary in a new terminal window (macOS Terminal.app, Linux first-found terminal emulator, Windows cmd). cwd defaults to `config.default_project_path` if set, else the user's home directory.
- **Delegates to**: `spawn_terminal_with_pi` (per-OS branch) → `tokio::process::Command::spawn`.
- **Errors**: `validation` (binary missing / no terminal found / unsupported OS), `internal` (spawn failure).

#### `check_subscription_auth`
- **Signature**: `fn check_subscription_auth() -> Result<SubscriptionAuthStatus, IpcError>`
- **Purpose**: Returns `{ chatgpt, claude }` booleans indicating whether subscription auth is wired up.
- **Delegates to**: `state::config::check_pi_subscription_auth` (`app/src-tauri/src/commands/settings.rs:850-854`).
- **Errors**: None.

#### `get_system_prompts`
- **Signature**: `fn get_system_prompts() -> Result<Vec<SystemPromptInfo>, IpcError>`
- **Purpose**: Static catalog of backend-owned system prompts (Scout / Worker / Guard / Nurse / reviewer base + stance / auto-commit titles), each with `id`, `category`, `name`, `description`, `source` reference, and the exact `body` shipped to the agent.
- **Delegates to**: `build_prompt_catalog` (`app/src-tauri/src/commands/settings.rs:2593`).
- **Errors**: None.

#### `list_custom_prompts`
- **Signature**: `fn list_custom_prompts() -> Result<Vec<CustomPrompt>, IpcError>`
- **Purpose**: User-defined custom prompts in creation order.
- **Delegates to**: `Config::custom_prompts` (`app/src-tauri/src/commands/settings.rs:2628-2631`).
- **Errors**: None.

#### `save_custom_prompt`
- **Signature**: `fn save_custom_prompt(id: Option<String>, name: String, body: String) -> Result<CustomPrompt, IpcError>`
- **Purpose**: Create (`id == None`) or update (`id == Some`) one custom prompt. Validates name (≤ 100 chars) and body (≤ 32 KiB).
- **Delegates to**: in-memory mutation of `Config::custom_prompts` + `Config::write_bytes` (`app/src-tauri/src/commands/settings.rs:2654-2685`).
- **Errors**: `internal` (validation / unknown id / disk write).

#### `delete_custom_prompt`
- **Signature**: `fn delete_custom_prompt(id: String) -> Result<(), IpcError>`
- **Purpose**: Remove a custom prompt by id. Missing ids are a no-op.
- **Delegates to**: `Config::custom_prompts.retain` + write (`app/src-tauri/src/commands/settings.rs:2703-2710`).
- **Errors**: `internal`.

---

### Dashboard — `commands/dashboard.rs` (5)

All read-only, all query `AppState::usage_store` (SQLite). Time-range strings: `"day"`/`"today"` → midnight UTC, `"week"` → 7 days ago, `"month"` → 30 days ago, anything else → all-time.

#### `get_dashboard_stats`
- **Signature**: `fn get_dashboard_stats() -> Result<DashboardStats, IpcError>`
- **Purpose**: Hero-strip counters: active Pi sessions, running + paused swarms, total review count, today's cost.
- **Delegates to**: `PiManager::active_count` + `SwarmRegistry::counts` + `HivemindStore::count_jobs` + `UsageStore::get_cost_summary` (`app/src-tauri/src/commands/dashboard.rs:43-50`).
- **Errors**: None — sub-call errors gracefully degrade to 0.

#### `get_model_usage`
- **Signature**: `fn get_model_usage(time_range: String) -> Result<Vec<ModelUsageSummary>, IpcError>`
- **Purpose**: Per-model token + cost breakdown for the time window.
- **Delegates to**: `UsageStore::get_model_usage` (`app/src-tauri/src/commands/dashboard.rs:80-84`).
- **Errors**: `internal` (Db error).

#### `get_provider_usage`
- **Signature**: `fn get_provider_usage(time_range: String) -> Result<Vec<ProviderUsageSummary>, IpcError>`
- **Purpose**: Per-provider rollup of the same data.
- **Delegates to**: `UsageStore::get_provider_usage` (`app/src-tauri/src/commands/dashboard.rs:106-110`).
- **Errors**: `internal`.

#### `get_cost_summary`
- **Signature**: `fn get_cost_summary() -> Result<CostSummary, IpcError>`
- **Purpose**: Combined today / week / month dollar totals.
- **Delegates to**: `UsageStore::get_cost_summary` (`app/src-tauri/src/commands/dashboard.rs:124`).
- **Errors**: `internal`.

#### `get_recent_activity`
- **Signature**: `fn get_recent_activity(limit: Option<u32>) -> Result<Vec<ActivityEntry>, IpcError>`
- **Purpose**: Newest-first usage events (chat / hivemind / swarm) with model, tokens, cost, timestamp. Default limit 10.
- **Delegates to**: `UsageStore::get_recent_activity` (`app/src-tauri/src/commands/dashboard.rs:148-151`).
- **Errors**: `internal`.

---

### Extensions — `commands/extensions.rs` (4)

#### `list_extensions`
- **Signature**: `fn list_extensions() -> Result<Vec<ExtensionManifest>, IpcError>`
- **Purpose**: Static manifests of every registered provider extension.
- **Delegates to**: `ExtensionRegistry::manifests` (`app/src-tauri/src/commands/extensions.rs:29-30`).
- **Errors**: None.

#### `get_usage_snapshots`
- **Signature**: `fn get_usage_snapshots() -> Result<Vec<SnapshotEntry>, IpcError>`
- **Purpose**: Current snapshot map sorted by manifest id. One entry per extension; each carries the latest fetched snapshot or last error.
- **Delegates to**: `AppState::usage_snapshots` (`app/src-tauri/src/commands/extensions.rs:39-43`).
- **Errors**: None.

#### `refresh_usage_snapshot`
- **Signature**: `fn refresh_usage_snapshot(extension_id: String) -> Result<SnapshotEntry, IpcError>`
- **Purpose**: Force one extension's poller to fetch immediately. Enforces a 5-second per-extension cooldown plus a per-extension in-flight mutex (2s wait then reject) to prevent double-fetches when the startup-refresh races a user click. Caps raw payload at `RAW_PAYLOAD_CAP_BYTES`; emits `usage-snapshot-updated` on completion.
- **Delegates to**: `UsageProvider::fetch` inside a `tokio::time::timeout(poller::FETCH_TIMEOUT_SECS, ...)` (`app/src-tauri/src/commands/extensions.rs:128-132`).
- **Errors**: `validation` (bad id / cooldown active / no usage capability / fetch already in flight), `not_found { resource: "extension" }`.

#### `update_extension_settings`
- **Signature**: `fn update_extension_settings(extension_id: String, enabled: Option<bool>, show_in_topbar: Option<bool>, preferences: Option<HashMap<String,String>>) -> Result<(), IpcError>`
- **Purpose**: Per-extension user settings. Validates id against registry. Persists to config, mirrors into the snapshot map (transitioning to `Disabled` clears the snapshot; re-enabling flips to `Loading` for the next poll), and emits `usage-snapshot-updated` so the UI updates immediately.
- **Delegates to**: `Config::extension_settings` (`app/src-tauri/src/commands/extensions.rs:248-273`).
- **Errors**: `validation`, `not_found { resource: "extension" }`, `internal` (config serialize / write).

---

### Nurse — `commands/nurse.rs` (3)

#### `get_nurse_status`
- **Signature**: `fn get_nurse_status() -> Result<NurseStatusSnapshot, IpcError>`
- **Purpose**: Live snapshot — enabled flag, running flag, stall threshold, configured model, tick interval, active session counts, recent event history. Polled on mount and after every `nurse-event`.
- **Delegates to**: `NurseService::get_status` (`app/src-tauri/src/commands/nurse.rs:34-36`).
- **Errors**: Never fails — atomics-only.

#### `set_nurse_config`
- **Signature**: `fn set_nurse_config(enabled: Option<bool>, stall_threshold_secs: Option<u64>, nurse_model: Option<String>, allow_destructive: Option<bool>, tick_interval_secs: Option<u64>, nurse_provider: Option<String>) -> Result<(), IpcError>`
- **Purpose**: Update any subset of Nurse config + persist. Empty/"none" `nurse_model` clears the LLM override. `allow_destructive` is deprecated (accepted for backward compat, logged + ignored). After mutation, emits `nurse-event` with a full `StatusUpdate` snapshot.
- **Delegates to**: `NurseService::update_config` + `sync_running_state` + `Config::write_bytes` + `app.emit` (`app/src-tauri/src/commands/nurse.rs:88-144`).
- **Errors**: `internal` (disk write).

#### `check_chat_session`
- **Signature**: `fn check_chat_session(session_id: String, caller: Option<String>) -> Result<NurseDecisionDto, IpcError>`
- **Purpose**: Force a one-shot Nurse evaluation. Called by the frontend watchdog at `chat_check_in_secs`. Returns a tagged DTO (`leave_it` / `steer` / `restart` / `cancel` / `noop`). Before synthesizing a watchdog signal, the command gates on live session state: missing/gone/not-busy sessions return `noop`; sessions with no Nurse-relevant activity yet or no new Nurse-relevant activity since the previous admitted watchdog check return `leave_it` without entering the dispatcher or spending an LLM call. `steer` is applied server-side before returning (Pi RPC steer); `restart` / `cancel` are surfaced for the frontend to act on.
- **Delegates to**: `Dispatcher::handle_signal` with `DispatchOrigin::Watchdog`; steer actions are applied by the dispatcher/applier before the DTO returns.
- **Errors**: `validation`, `internal` (evaluation failure).
- **Notes**: `caller` is a free-form tag (`"chat"` / `"context"` / `"merge"`) recorded in the tracing span only.

---

### Sessions — `commands/sessions.rs` (5)

#### `list_active_pi_sessions`
- **Signature**: `fn list_active_pi_sessions() -> Result<Vec<ActiveSession>, IpcError>`
- **Purpose**: Every Pi session in the pool with owner metadata, `is_alive`/`is_busy`/`is_pinned`, event/turn counts, last-activity ms.
- **Delegates to**: `PiManager::list_sessions` + `PiSession` accessors (`app/src-tauri/src/commands/sessions.rs:48-66`).
- **Errors**: None.

#### `kill_pi_session`
- **Signature**: `fn kill_pi_session(session_id: String) -> Result<(), IpcError>`
- **Purpose**: Terminate one session. Idempotent on `SessionNotFound`.
- **Delegates to**: `PiManager::kill_session` (`app/src-tauri/src/commands/sessions.rs:77`).
- **Errors**: `validation`, `internal`.

#### `reconcile_active_sessions`
- **Signature**: `fn reconcile_active_sessions(known_ids: Vec<String>) -> Result<Vec<String>, IpcError>`
- **Purpose**: After a webview reload the renderer's session-keyed refs are reset. This kills every Task-owned, non-busy, non-pinned session whose id isn't in `known_ids`. Review/Merge/Swarm sessions are left alone (they have their own lifecycle). Returns the ids that were killed.
- **Delegates to**: `PiManager::list_sessions` + `SessionOwner::is_reconcile_evictable` + `PiManager::kill_session` (`app/src-tauri/src/commands/sessions.rs:100-120`).
- **Errors**: `validation`.

#### `pi_pool_stats`
- **Signature**: `fn pi_pool_stats() -> Result<PiPoolStats, IpcError>`
- **Purpose**: Pool-wide summary: active count, available permits, max processes, graveyard size, per-session stats.
- **Delegates to**: `PiManager::list_session_stats` + `available_permits` + `max_processes` + `graveyard_size` (`app/src-tauri/src/commands/sessions.rs:139-145`).
- **Errors**: None.

#### `get_pi_session_stats`
- **Signature**: `fn get_pi_session_stats(session_id: String) -> Result<Option<PiSessionStats>, IpcError>`
- **Purpose**: Live per-session token usage for one Pi session. Returns `Ok(None)` when the session is gone (SwarmControl uses this to clear stale stats on agent transitions). Polled every ~2s by `ContextStatusBar`.
- **Delegates to**: `PiSession::get_session_stats` (`app/src-tauri/src/commands/sessions.rs:162-167`).
- **Errors**: `validation`, provider-mapped via `IpcError::from_provider_error`.

---

### Tests — `commands/tests.rs` (7)

#### `run_stability_test`
- **Signature**: `fn run_stability_test() -> Result<RunStabilityTestResponse, IpcError>`
- **Purpose**: Kick off a sandboxed end-to-end stability test. Rejects if another run is in flight. Generates a `run_id` and spawns the inner test runner detached; the response returns immediately so the frontend can subscribe to `test-progress` before the first event.
- **Delegates to**: `core::stability_test::run_stability_test_inner` via `tauri::async_runtime::spawn` (`app/src-tauri/src/commands/tests.rs:72-83`).
- **Errors**: `validation` (already running).

#### `cancel_test_run`
- **Signature**: `fn cancel_test_run() -> Result<bool, IpcError>`
- **Purpose**: Signal the active run's `CancellationToken`. Returns `true` if a run was active.
- **Delegates to**: `active_test_run.cancel_token.cancel()` (`app/src-tauri/src/commands/tests.rs:118-124`).
- **Errors**: None.

#### `get_active_test_run`
- **Signature**: `fn get_active_test_run() -> Result<Option<ActiveTestRunDto>, IpcError>`
- **Purpose**: Snapshot of the currently running test for rehydration after restart or between `test-progress` events.
- **Delegates to**: `AppState::active_test_run` (`app/src-tauri/src/commands/tests.rs:106-113`).
- **Errors**: None.

#### `list_test_runs`
- **Signature**: `fn list_test_runs(limit: Option<u32>) -> Result<Vec<TestRunSummary>, IpcError>`
- **Purpose**: Recent persisted test records from `~/.hyvemind/test-runs/` sorted by mtime desc. Default 50, cap 500.
- **Delegates to**: `std::fs::read_dir` inside `spawn_blocking` (`app/src-tauri/src/commands/tests.rs:169-196`).
- **Errors**: `internal` (join error).

#### `get_test_run`
- **Signature**: `fn get_test_run(run_id: String) -> Result<Option<TestRunRecord>, IpcError>`
- **Purpose**: Read one persisted record (`{run_id}.json`).
- **Delegates to**: `std::fs::read_to_string` + `serde_json::from_str` (`app/src-tauri/src/commands/tests.rs:213-221`).
- **Errors**: `validation` (`run_id` shape is `YYYYMMDD-HHMMSS-<8 hex>`; validated against `validate_id`'s superset allowlist), `internal`.

#### `get_stability_test_config`
- **Signature**: `fn get_stability_test_config() -> Result<StabilityTestConfigDto, IpcError>`
- **Purpose**: Read the Tests-screen-specific model selection (kept independent of the app's `default_model`).
- **Delegates to**: `Config::stability_test` (`app/src-tauri/src/commands/tests.rs:246-250`).
- **Errors**: None.

#### `set_stability_test_config`
- **Signature**: `fn set_stability_test_config(config: StabilityTestConfigDto) -> Result<StabilityTestConfigDto, IpcError>`
- **Purpose**: Persist the Tests-screen task model / verifier model / optional `hivemind_id` selection. Validates `hivemind_id` (it's later used as a path component by the hivemind subsystem).
- **Delegates to**: `Config::write_bytes` (`app/src-tauri/src/commands/tests.rs:269-284`).
- **Errors**: `validation`, `internal`.
