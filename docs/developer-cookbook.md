# Developer Cookbook

Task-oriented recipes. For architecture see `docs/architecture.md`. For the IPC surface see `docs/ipc-reference.md`. For subsystem internals see the per-subsystem READMEs (`app/src-tauri/src/{core,hivemind,pi,state,extensions,providers,domain}/README.md`).

Recipes assume you already understand the architecture and just want the "I want to do X — what files do I touch?" answer. All references use `file:line` against the worktree at the time of writing — search if the line drifted.

---

## Recipe: Run the app locally

1. Install prerequisites: Rust (stable via rustup), Node 20+, Bun (for the Pi build), Tauri per-platform deps. Full list in `CONTRIBUTING.md:16-43`.
2. From the repo root: `cd app && npm install`.
3. `npm run tauri:dev` (script at `app/package.json:35`). Behind the scenes:
   - Runs `npm run prepare-pi` which calls `bash ../scripts/build-pi.sh` (2-5 min first time, free after thanks to the `app/src-tauri/binaries/.pi-version` stamp file).
   - Sets `HYVEMIND_DEBUG=1` so per-ID logs land under `~/.hyvemind/debug/`.
   - Launches Tauri with `--no-watch` — Rust hot-reload is intentionally off (`CONTRIBUTING.md:77-94`).
4. To pick up Rust changes: stop the dev shell, `cd app/src-tauri && cargo build`, then restart `npm run tauri:dev`.

---

## Recipe: Run the test suites

1. **Backend** — from `app/src-tauri/`:
   - `cargo test` — full suite (~960 `#[test]` / `#[tokio::test]` annotations).
   - `cargo fmt --check` — format check (CI mirror).
   - `cargo clippy -- -D warnings` — lint, warnings as errors (CI mirror).
   - `cargo check` — fast type check if you don't need binaries.
2. **Frontend** — from `app/`:
   - `npm test` — Vitest run-once across ~40 test files.
   - `npm run test:watch` — Vitest watch mode.
   - `npx tsc --noEmit` — type check (CI mirror).
3. **Single backend test** — `cargo test --package hyvemind module::name` from `app/src-tauri/`. Add `-- --nocapture` to see `println!` output.
4. **Single frontend test** — `npx vitest run path/to/file.test.ts` from `app/`.
5. CI runs the same commands; if they pass locally your PR's CI passes too.

---

## Recipe: Enable debug logging and find the right log

1. Set `HYVEMIND_DEBUG=1` before launch. `npm run tauri:dev` already sets it (`app/package.json:35`); for `cargo run` from `app/src-tauri/` prepend manually: `HYVEMIND_DEBUG=1 cargo run`.
2. Log location depends on the entity (routing layer at `app/src-tauri/src/state/log_routing.rs`):
   - **One Task / chat session** — `~/.hyvemind/debug/sessions/{session_id}.jsonl`
   - **One Hivemind review** — `~/.hyvemind/debug/reviews/{review_id}.jsonl`
   - **One swarm (orchestration)** — `~/.hyvemind/debug/swarms/{swarm_id}/swarm.jsonl`
   - **One agent run within a swarm** — `~/.hyvemind/debug/swarms/{swarm_id}/{agent}-{feature_id}-{run_id}.jsonl`
   - **Startup / library / uncategorised** — `~/.hyvemind/debug/general.jsonl.YYYY-MM-DD` (daily rotation)
3. Routing priority (most specific wins): `review_id` > `swarm_id`+`agent` > `swarm_id` > `session_id` > `general`. Files older than `HYVEMIND_DEBUG_LOG_RETENTION_DAYS` (default 7) are pruned at startup by `prune_old_debug_logs` (`app/src-tauri/src/lib.rs:50`).
4. Errors across all sessions today:
   ```
   grep -r '"level":"ERROR"' ~/.hyvemind/debug/sessions/ ~/.hyvemind/debug/reviews/ ~/.hyvemind/debug/swarms/ | python3 -m json.tool
   ```
5. For full investigation patterns (per-session / per-review / per-swarm) see `CLAUDE.md §Debug Mode`.

---

## Recipe: Add a new Tauri IPC command

Model: any simple setter in `commands/settings.rs` (e.g. `set_chat_check_in_secs` at `app/src-tauri/src/commands/settings.rs:331`).

1. **Pick the module.** Add `#[tauri::command] pub async fn …` to the existing `commands/{chat,tasks,hivemind,swarms,settings,dashboard,extensions,nurse,sessions,tests}.rs` if it belongs to one. Otherwise create a new file under `commands/` and add `pub mod your_module;` to `app/src-tauri/src/commands/mod.rs`.
2. **Signature.** Return `Result<T, IpcError>` (envelope at `state/ipc_error.rs`). Take `tauri::State<'_, AppState>` for backend state, `tauri::AppHandle` for emit, plus your own typed `Deserialize` args. `set_daily_budget` (`commands/settings.rs:398`) is a representative shape.
3. **Validate inputs.** Use the helpers in `commands/util.rs` (`validate_id`, `canonicalize_clean`) or write small local validators (see `validate_endpoint` / `validate_working_dir` at `commands/settings.rs:19,51`). Return `Err(IpcError::validation(...))` on bad input.
4. **Register in `lib.rs`.** Add `commands::your_module::your_command,` to the `tauri::generate_handler!` block at `app/src-tauri/src/lib.rs:543-647`. Debug-only commands use `#[cfg(debug_assertions)]` — see `clear_response_cache` at `lib.rs:571`.
5. **Emit events (optional).** If state changes other surfaces care about, emit via `app_handle.emit("event-name", payload)`. Add the event row to `CLAUDE.md §Tauri Events` and to the event-listener index in `docs/frontend-architecture.md`.
6. **Frontend caller.** Add a typed wrapper to `app/src/lib/ipc.ts` near the existing `invoke<T>(name, args)` helpers (definition at `ipc.ts:97`). Use the bracket-arg form: `invoke<MyType>("your_command", { foo, bar })`. Don't call `rawInvoke` directly — going through the wrapper gives you the Sentry capture + `IpcError` discriminator.
7. **Typed payloads.** Add shared interfaces under `app/src/types/` and import from there. Mirror the Rust `Serialize` shape exactly (snake_case is on-wire format; Tauri rewrites camelCase from JS callers).
8. **Update docs:**
   - `CLAUDE.md §Tauri Commands` — bump the bucket count and append the command name.
   - `docs/ipc-reference.md` — add the per-handler entry (signature / returns / errors / delegation).
9. **Backend test.** Add to the inline `#[cfg(test)] mod tests` block in the same `commands/X.rs` file.
10. **Frontend test.** Add under `app/src/lib/__tests__/` or the relevant component `__tests__/` dir if the call participates in a meaningful UI flow.

---

## Recipe: Add a new tunable (HYVEMIND_* env var)

Model: every accessor in `app/src-tauri/src/tunables.rs`.

1. **Add the accessor** to `app/src-tauri/src/tunables.rs`. Append `pub fn your_thing() -> T { from_env("HYVEMIND_YOUR_THING", default) }` in the matching section comment block (Pi pool / Hivemind / Cache / Swarm / Debug / Circuit breaker / HTTP timeouts).
2. **Document inline** in the `///` rustdoc: env name, default, what it gates. `pi_max_processes` at `tunables.rs:44` is the canonical template; `circuit_breaker_cooldown` at `tunables.rs:157` shows the `Duration` wrapper pattern.
3. **Use it via `tunables::your_thing()` at construction time** — never in a hot loop. Each call re-reads `std::env::var` (deliberate; see module doc at `tunables.rs:8-18`). Typical sites: `PiManager::new`, `ReviewEngine::new`, scheduler clamps, startup.
4. **Add a unit test** in the `#[cfg(test)] mod tests` block at `tunables.rs:178`. Use a uniquely-named env var so it can't race with parallel tests — see the `HYVEMIND_TEST_*_5_8` naming convention. The `pi_max_processes_honours_env_override` test (`tunables.rs:234`) is the template.
5. **Update docs:**
   - `CLAUDE.md §Environment Variables → Tunables` — add the bullet with default and one-line description.
   - `PRODUCT.md §7` if the default is one the product doc cites (Pi pool size, swarm parallelism, etc.).
6. **If user-configurable via UI**, also add a setter command (see "Add a new Tauri IPC command") that writes the corresponding field on `Config` and emits a change event. Examples: `set_extension_poll_interval_secs` (`commands/settings.rs:364`), `set_daily_budget` (`commands/settings.rs:398`).

---

## Recipe: Add a new screen

1. Create `app/src/screens/YourScreen.tsx`. Export a component taking `{ go: GoFn, ...params }`.
2. Import in `app/src/App.tsx` near the other screen imports.
3. Add a `case "your-screen":` to `ScreenRouter` (`App.tsx:363`). Forward `go` and any `nav.params`.
4. If top-level tab: add an entry to the `NAV` array (`App.tsx:91`). The Cmd/Ctrl+N keyboard shortcuts (`App.tsx:559`) auto-pick up new tabs.
5. If Pi-gated (most screens are): add the tab id to the `LOCKED_WHEN_NO_PI` set (`App.tsx:69`). The guard at `App.tsx:539` blocks navigation when Pi isn't installed.
6. Add tests under `app/src/screens/__tests__/YourScreen.test.tsx`.
7. **Update docs:**
   - `CLAUDE.md §Frontend Architecture → Screens` table.
   - `PRODUCT.md §8 "User Experience"` screen list.
8. For the deeper walk-through (route params, sub-context wiring, design conventions) see `docs/frontend-architecture.md` "Add a new screen" recipe.

---

## Recipe: Add a database migration

1. Create `app/src-tauri/migrations/000N_short_description.sql`. Numbering is monotonic — the highest existing number is the previous migration. Seed migration: `0001_hivemind.sql`.
2. Write idempotent DDL using `CREATE TABLE IF NOT EXISTS`, `CREATE INDEX IF NOT EXISTS`, etc. See `migrations/0001_hivemind.sql` for the convention. Use `ON DELETE CASCADE` for child tables (compare `job_steps.job_id REFERENCES jobs(id) ON DELETE CASCADE` at `0001_hivemind.sql:22`).
3. `sqlx::migrate!("./migrations")` (`app/src-tauri/src/hivemind/store.rs:237`) picks new files up automatically on the next process start — no Rust code to wire up.
4. Update the model structs in `app/src-tauri/src/hivemind/store.rs` if your migration adds/changes columns the store reads. Run `cargo check` first to find the broken `FromRow` impls.
5. Run `cargo test --package hyvemind hivemind::` to validate the schema; store tests open in-memory SQLite and replay all migrations.
6. **Delete `~/.hyvemind/hivemind/` before dev-testing** if you're iterating on the migration — sqlx records applied versions in `schema_migrations`; a backwards-incompatible edit to an already-applied migration will fail until the local DB is recreated.
7. If your migration adds a `NOT NULL` column without a `DEFAULT`, existing rows will break — either supply a `DEFAULT` or precede it with an `UPDATE`.
8. No CLAUDE.md update needed unless the migration adds a brand-new file under `migrations/` (append it to §Project Layout) or introduces a table other subsystems will read (add an §Investigating recipe).

---

## Recipe: Bump the bundled Pi binary

1. Edit `scripts/pi-version.txt` — one line, exact version (no `v` prefix, no `^`). Currently `0.74.0`.
2. Run `npm run prepare-pi` from `app/` (or just `npm run tauri:dev` / `npm run tauri:build` — both invoke it via `app/package.json:34-36`). The script at `scripts/build-pi.sh` short-circuits when `binaries/.pi-version` matches the requested pin (`build-pi.sh:52-56`); otherwise it rebuilds end-to-end.
3. Verify the rebuild fired: `cat app/src-tauri/binaries/.pi-version` should show the new pin. The log line `[build-pi] building Pi v…` confirms it ran.
4. The three npm extensions (`pi-web-access`, `pi-subagents`, `pi-mcp-adapter`, declared at `build-pi.sh:24`) get re-snapshotted under `app/src-tauri/binaries/pi-extensions/` along with their flat `node_modules/`. The two local extensions (`hyvemind-providers`, `hyvemind-handoff` at `build-pi.sh:25`) are copied fresh from `app/src-tauri/pi-extensions/` every run (also on the short-circuit path, `build-pi.sh:53`).
5. Smoke test:
   - **Tasks screen** — send a message; verify tokens stream.
   - **Settings → Providers** — run `test_provider_pi` for at least one provider.
   - **Swarms** — spin up a tiny swarm; confirm Queen → Scout → Worker chain still parses handoffs.
6. Confirm `CLAUDE.md §Pi binary` still accurately describes the pin + build flow. The file pin is the only source of truth — no other code references the version.

---

## Recipe: Add a new frontend event listener

Pick the consumer pattern, then wire it up:

1. **Single consumer** — call `listen()` inside a `useEffect` in the component that owns the state. Always clean up in the returned cleanup.
2. **Multiple consumers** — add a singleton store under `app/src/lib/` (model: `hivemindEventStore.ts`, `swarmActivityStore.ts`) that registers one Tauri listener at module load and fans out via `subscribe(id, cb)`. Mirror the hydrate-on-first-subscribe + monotonic-`seq` dedup pattern from `swarmActivityStore` if your channel has a paged history command.
3. Add the event name to `CLAUDE.md §Frontend Architecture → Event listener index` so the next agent can find "who listens for X" without grepping the whole tree.
4. If the backend emits the event somewhere new, also update `CLAUDE.md §Tauri Events`.

For the full pattern (per-component vs singleton, unsubscribe semantics, dedup via `seq`) see `docs/frontend-architecture.md`.

---

## Recipe: Add a new bee agent role

This is a multi-subsystem change touching prompts, scheduler integration, model settings, and Nurse handling. For the deep walk-through see `docs/bee-agents.md` "Add a new role" recipe. The CLAUDE.md doc-maintenance trigger table (`CLAUDE.md §Documentation Maintenance`, row "Added a new bee agent role") lists every section that needs an edit:

- §Project Layout (new prompt under `prompts/`)
- §Key Types (new role type)
- `PRODUCT.md §4` bee-colony table
- `PRODUCT.md §13` glossary

---

## Recipe: Add a new LLM provider

For the full step-by-step (trait surface, registry dispatch, cost lookup, circuit-breaker integration) see `app/src-tauri/src/providers/README.md`. The short checklist lives in `CONTRIBUTING.md:154-176`:

- **OpenAI-compatible endpoint** (the common case): seed an entry in `seed_default_providers()` (`state/config.rs`) and add models to `get_model_catalog()` (`commands/settings.rs`). No new Rust code.
- **Custom API shape**: also add a `ProviderKind` variant + `call()` arm in `providers/mod.rs`, register in `ProviderRegistry::refresh_from_config_with_pi()`, and append the new type to `ALLOWED_PROVIDER_TYPES` (`commands/settings.rs:15`).

Update `CLAUDE.md §Key Types` `Provider` row and `PRODUCT.md §7 "Provider abstraction"` if the provider count changes.

---

## Recipe: Run a stability test

The stability test runner is a sandboxed end-to-end check that spawns a Worker + Guard against a synthetic task and verifies the handoff. Code at `app/src-tauri/src/core/stability_test/runner.rs`.

1. Open the **Tests** screen in the app.
2. Pick a configuration (or use the default). Configs live in `Config.stability_test_config` and are settable via `set_stability_test_config` (`commands/tests.rs`).
3. Click **Run**. The runner emits `test-progress` events through phases (planning, implementation, verification). `TestRunProvider` (`app/src/state/TestRunProvider.tsx`) listens and drives the UI.
4. **Results land** in the SQLite store accessible via `list_test_runs` / `get_test_run`. Active run state via `get_active_test_run`.
5. **Inspect failures** by reading the per-session logs. Find the session id in the test-run record; then:
   - Pi transcript: `~/.hyvemind/chat-sessions/{session_id}.jsonl`
   - Debug log: `~/.hyvemind/debug/sessions/{session_id}.jsonl`
   - See `CLAUDE.md §Investigating a Session ID` for the full inspection recipes.
6. Cancel mid-run via the Tests-screen button (`cancel_test_run`). Cancellation is cooperative via `CancellationToken`; the runner observes it at the next await point and persists a terminal `cancelled` record before exiting.

---

## Recipe: Investigate a stuck Task / Hivemind / Swarm

Each has a dedicated investigation guide in `CLAUDE.md` — start there:

- **Stuck Task** → `CLAUDE.md §Debugging a stuck Tasks-view session`. Covers per-session log inspection plus common causes: slow model, rate limiting, missing API key, Pi crash, missed frontend listener.
- **Stuck / weird Hivemind review** → `CLAUDE.md §Debugging a hivemind review` + `§Investigating a Hivemind Review`. Per-review event log, per-model output capture files, circuit-breaker state, registry-loaded check.
- **Stuck / failed swarm** → `CLAUDE.md §Investigating a Swarm — progress_log.jsonl`. Event-type table + Python recipes for filtering by feature and finding orphan Pi sessions.

Frontend devtools (Cmd+Opt+I on macOS) surface IPC errors and missed event listener registrations.

---

## Recipe: Trace a single LLM call

1. **Where the call is made.** Every provider call goes through `ProviderRegistry::call()` in `app/src-tauri/src/providers/mod.rs`. Each provider impl (`AnthropicProvider`, `OpenAICompatibleProvider`, `OpenRouterProvider`, `PiSubscriptionProvider`, `MockProvider`) implements the `Provider` trait at `providers/provider_trait.rs`. SSE-capable providers also implement `StreamingProvider`.
2. **See the request body.** Enable `HYVEMIND_DEBUG=1`. Provider request/response payloads log at TRACE inside the relevant span. Find them by entity:
   ```
   grep 'provider request\|provider response\|anthropic request\|anthropic response' \
     ~/.hyvemind/debug/reviews/{review_id}.jsonl | python3 -m json.tool
   ```
   For non-review calls (Scout, Worker, Guard) substitute the swarm-agent file path: `~/.hyvemind/debug/swarms/{swarm_id}/{agent}-{feature_id}-{run_id}.jsonl`.
3. **See the full response output (Hivemind reviews).** Responses are *not* inlined in the JSONL events (capture-file refactor moved them out). Look in:
   - Per-model full output: `~/.hyvemind/reviews/{review_id}/output-{model_id_safe}-r{round}.txt` (model id with `/` and `:` replaced by `_`).
   - Per-round merge text: `~/.hyvemind/reviews/{review_id}/merge-r{round}.txt`.
   - The `model_call_completed` event in the high-level log carries `output_file` (relative to `~/.hyvemind/`) and `output_len` — `cat` the referenced file for the full body.
4. **See the full response output (Tasks / swarm agents).** Pi writes the authoritative transcript itself at `~/.hyvemind/chat-sessions/{session_id}.jsonl`. Each `message` event carries the full assistant text + tool calls.
5. **Cost + token accounting.** Recorded per call in `ModelResponse` (`providers/mod.rs`); aggregated into `UsageStore` (`state/usage_store.rs`) and surfaced via `get_dashboard_stats` / `get_provider_usage` / `get_model_usage` / `get_cost_summary` (`commands/dashboard.rs`).
6. **Circuit-breaker state.** Per-provider 3-state machine in `hivemind/circuit_breaker.rs`. Grep `~/.hyvemind/debug/` for `circuit` to see Closed → Open → HalfOpen transitions.

---

## Recipe: Profile or measure something

Hyvemind ships no built-in profiler. The available signals are:

1. **Per-call cost + tokens** — `UsageStore` (`state/usage_store.rs`) records every model response. Read via `commands/dashboard.rs::{get_dashboard_stats, get_model_usage, get_provider_usage, get_cost_summary, get_recent_activity}`.
2. **Pi pool stats** — `commands/sessions.rs::pi_pool_stats` returns live process count, idle count, and total spawn count. Also logged periodically by the unified maintenance loop (`pi/eviction.rs`) when `HYVEMIND_DEBUG=1`.
3. **Per-session token cost** — `commands/sessions.rs::get_pi_session_stats` per session id.
4. **Swarm cost + duration** — `commands/swarms.rs::get_swarm_usage` aggregates per-feature spend. Persisted in `~/.hyvemind/swarms/{id}/state.json` via `core/budget.rs`. Daily cap configurable via `set_daily_budget` (`commands/settings.rs:398`); breaches emit `budget_exceeded` on the swarm progress log.
5. **Hivemind round timing** — `model_call_completed` events carry `duration_ms`. Sum across a review for total wall time; see `CLAUDE.md §Investigating a Hivemind Review` for a Python summarisation one-liner.
6. **Tracing spans** — every async entry point is wrapped in `#[tracing::instrument]`. With `HYVEMIND_DEBUG=1` the per-ID JSONL captures span timestamps so you can compute durations from `head`/`tail` deltas.
7. **Frontend render perf** — Cmd+Opt+I → standard Chromium DevTools Performance panel.
8. **Native CPU profiling** — `cargo flamegraph` from `app/src-tauri/` (requires `cargo install flamegraph` + perf/dtrace per-platform setup). Not part of CI; ad-hoc only.

---

## Related docs

- `docs/architecture.md` — system component map + sequence diagrams.
- `docs/frontend-architecture.md` — React provider tree, sub-contexts, event-store recipes.
- `docs/ipc-reference.md` — per-handler IPC reference.
- `docs/bee-agents.md` — per-agent deep-dive.
- `docs/providers.md` — LLM provider abstraction overview.
- `docs/extension-authoring.md` — Rust provider extensions, Pi local TS extensions, topbar widgets.
- `app/src-tauri/src/{core,hivemind,pi,state,extensions,providers,domain}/README.md` — subsystem internals.
- `CLAUDE.md` — full technical reference (env vars, debug recipes, investigation guides, doc-maintenance triggers).
- `PRODUCT.md` — product context.
- `CONTRIBUTING.md` — dev setup + PR checklist + provider-addition cheat sheet.
