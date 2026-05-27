@PRODUCT.md

# Hyvemind — Technical & Operational Reference

> **Read `PRODUCT.md` first.** It contains the product overview (what Hyvemind is, the three systems, the bee colony agents, key differentiators, current alpha state, brand, business model, roadmap, glossary). This file is the **technical/operational reference** — file paths, IPC surface, debug commands, investigation guides — and deliberately does **not** repeat product context.
>
> **Terminology**: Hyvemind has no "Chat" surface. Wherever this file says `chat.rs`, `chat-event`, `chat-sessions/`, etc., these are internal/historical names for the **Tasks-view conversation**. See `PRODUCT.md §3` for details.

## How to use this document

This file is the technical reference for AI coding agents and humans working on Hyvemind. The major sections you can jump to:

- **[Documentation Maintenance](#documentation-maintenance)** — read this whenever you change something the docs describe. The trigger tables tell you exactly which section to update.
- **[Quick Reference](#quick-reference)** — common dev commands.
- **[Content Security Policy](#content-security-policy-csp)** — the renderer's CSP and why each directive is what it is.
- **[Pi binary](#pi-binary)** — how the bundled Pi runtime is built and pinned.
- **[Crash recovery](#crash-recovery)** — what's reconciled on the next launch and what's lost.
- **[Project Layout](#project-layout)** — Rust backend file tree.
- **[Frontend Architecture](#frontend-architecture)** — React 18 + Vite + Tailwind shell, screens, providers, IPC layer, event listeners.
- **[Key Types](#key-types)** — the types most code paths route through.
- **Investigation guides** — recipes for [Tasks](#investigating-a-task), [Sessions](#investigating-a-session-id), [Hivemind Reviews](#investigating-a-hivemind-review), and [Swarms](#investigating-a-swarm--progress_logjsonl).
- **[Tauri Commands](#tauri-commands-ipc)** — IPC surface.
- **[Tauri Events](#tauri-events-backend--frontend)** — streaming event channels.
- **[Environment Variables](#environment-variables)** — credentials, debug flags, tunables.
- **[Debug Mode](#debug-mode-checking-logs)** — how to read the structured logs Hyvemind writes when `HYVEMIND_DEBUG=1`.

### Sibling documentation

Don't duplicate any of these in CLAUDE.md — link to them:

| Doc | What it covers |
|-----|----------------|
| `PRODUCT.md` | Product context: vision, three systems, bee-colony agents, brand, roadmap, glossary |
| `AGENTS.md` | Thin index pointing at per-subsystem READMEs and the canonical docs |
| `CONTRIBUTING.md` | Dev setup, PR checklist, how to add a provider |
| `CHANGELOG.md` | Release notes; `[Unreleased]` accumulates work since the last tag |
| `SECURITY.md` | Threat model, vulnerability reporting process |
| `CODE_OF_CONDUCT.md` | Contributor Covenant v2.1 |
| `app/src/A11Y.md` | Frontend accessibility conventions (aria-live, screen-reader rules) |
| `docs/README.md` | Index of all topical / deep-dive docs (read this to find a doc) |
| `docs/architecture.md` | System component map + sequence-flow diagrams (Mermaid) + crash-recovery + storage layout |
| `docs/ipc-reference.md` | Per-command reference for every Tauri IPC handler (signature / returns / errors / delegation) |
| `docs/frontend-architecture.md` | React deep-dive: provider tree, `taskRuntime.tsx` sub-contexts, IPC wrapper, singleton event stores, recipes |
| `docs/bee-agents.md` | Per-agent deep-dive (Queen / Scout / Worker / Guard / Nurse + scout-review + stability-test pair) |
| `docs/providers.md` | LLM provider abstraction overview (the 5 impls, dispatch, cost, circuit-breaker integration) |
| `docs/extension-authoring.md` | How to author Rust provider extensions, Pi local TS extensions, and frontend topbar widgets |
| `docs/developer-cookbook.md` | Task-oriented recipes (add a command / tunable / screen / migration, bump Pi, enable debug, etc.) |
| `docs/hivemind-custom-prompts.md` | Ready-to-paste Hivemind reviewer prompts |
| `app/src-tauri/src/core/README.md` | Swarm execution engine deep-dive |
| `app/src-tauri/src/hivemind/README.md` | Multi-model review engine deep-dive |
| `app/src-tauri/src/pi/README.md` | Pi subprocess management deep-dive |
| `app/src-tauri/src/state/README.md` | Persistence, AppState, secret store, atomic writes |
| `app/src-tauri/src/extensions/README.md` | Provider usage/balance extension framework |
| `app/src-tauri/src/nurse/README.md` | Push-mode Nurse engine — bus, detectors, three-tier dispatcher, observability |
| `app/src-tauri/src/providers/README.md` | LLM provider abstraction deep-dive (trait surface, the 5 impls, add-a-provider walkthrough) |
| `app/src-tauri/src/domain/README.md` | Domain type glossary (`SwarmState`, `Feature`, `Milestone`, `ModelSettings`, transitions) |

## Documentation Maintenance

> **The rule**: when you change something the docs describe, update the doc in the same commit. The tables below tell the next agent exactly which section needs an edit.

### Triggers that require a CLAUDE.md edit

| You changed... | Update... |
|---|---|
| Added/removed a `#[tauri::command]` (or its registration in `lib.rs` `invoke_handler!`) | §Tauri Commands (IPC) |
| Added/removed an `emit("…", …)` Tauri event from any backend code | §Tauri Events |
| Added/changed a `HYVEMIND_*` env var or default in `tunables.rs` | §Environment Variables → Tunables |
| Added a new file under `src-tauri/src/{commands,core,hivemind,nurse,pi,state,extensions,providers,domain}/` | §Project Layout (tree) |
| Deleted a file the tree references | §Project Layout (remove the line) |
| Added a `pub` type that other subsystems reach for | §Key Types |
| Added a new Provider impl in `providers/mod.rs` | §Key Types `Provider` row AND `PRODUCT.md §7 "Provider abstraction"` |
| Added a new bee agent role | §Project Layout (new prompt under `prompts/`), §Key Types, `PRODUCT.md §4` bee-colony table, `PRODUCT.md §13` glossary |
| Changed the CSP in `tauri.conf.json` | §Content Security Policy (the directive table + rationale) |
| Added an extension under `extensions/builtins/` | §Project Layout AND `extensions/README.md` |
| Added a new on-disk path under `~/.hyvemind/` | `PRODUCT.md §7 "Storage layout"` AND the matching `Investigating a …` recipe section in CLAUDE.md |
| Changed Pi binary pin in `scripts/pi-version.txt` | Verify §Pi binary still describes the pin + build flow correctly |
| Added a new screen or top-level frontend section | §Frontend Architecture (Screens table) |
| Added a `useEffect(() => { listen("…", …) }, ...)` in the frontend | §Frontend Architecture → Event listener index |
| Added a new React Context provider near the top of `App.tsx` | §Frontend Architecture → State management table |
| Removed an entire subsystem (e.g. agentmemory) | Strip every mention from §Project Layout, §Key Types, §Tauri Commands, §Tauri Events, §Environment Variables AND `PRODUCT.md` |

### Triggers that require a PRODUCT.md edit

| You changed... | Update... |
|---|---|
| Shipped a feature that was in §6 "Partially built" or §6 "Not yet implemented" | Move it to "Built and working" |
| Added/removed a screen | §8 User Experience (screen list) |
| Changed brand colours or typography | §9 Brand & Design Identity |
| Added a bee role | §4 The Bee Colony table + §13 Glossary |
| Renamed a user-facing concept | §13 Glossary + every reference in §3, §8, §10 |
| Changed a tunable default that PRODUCT.md cites (e.g. Pi pool size, swarm parallelism) | §7 Concurrency Model + §13 Glossary |
| Added a release to `CHANGELOG.md` | §6 Current State (move done items, refresh "By the numbers") |

### Triggers that require a per-subsystem README edit

The READMEs are deep-dives for one subsystem each. Edit them when:

- You changed an **exported type** (public `pub struct` / `pub enum` / `pub trait`) of the subsystem
- You changed the **lifecycle contract** (when something starts, when it stops, what it owns)
- You changed the **event surface** (what it emits, what it listens to)
- You added a **new file** under that subsystem (also update §Project Layout in CLAUDE.md)

The six subsystem READMEs:

| Subsystem | Path |
|-----------|------|
| Swarm engine | `app/src-tauri/src/core/README.md` |
| Multi-model review engine | `app/src-tauri/src/hivemind/README.md` |
| Pi subprocess management | `app/src-tauri/src/pi/README.md` |
| Persistence + AppState | `app/src-tauri/src/state/README.md` |
| Provider extensions | `app/src-tauri/src/extensions/README.md` |
| Nurse push-mode engine | `app/src-tauri/src/nurse/README.md` |

### Recurring sync — run this whenever you suspect drift

Drift accumulates silently. If you're touching CLAUDE.md anyway, spend three minutes on the audit:

```bash
# 1. Test count (informational — CLAUDE.md cites an approximate number)
grep -rhcE '#\[test\]|#\[tokio::test\]' app/src-tauri/src --include='*.rs' \
  | awk '{s+=$1} END {print s}'                                                # update §Quick Reference if drift exceeds ±50

# 2. Tauri command count and grouping
awk '/generate_handler!\[/,/\]\)/' app/src-tauri/src/lib.rs \
  | grep -oE 'commands::[a-z_]+::[a-z_]+' | sort | uniq -c                    # cross-check §Tauri Commands buckets

# 3. Tunable list
grep -E '^pub fn ' app/src-tauri/src/tunables.rs                              # cross-check §Tunables table

# 4. Project Layout sanity — every path the tree mentions exists
python3 -c "
import re, pathlib
root = pathlib.Path('.')
md = (root/'CLAUDE.md').read_text()
for p in re.findall(r'app/src[A-Za-z0-9/_.\\-]+\\.(?:rs|tsx?|md|toml|json|sql)', md):
    if not (root/p).exists(): print('MISSING:', p)
"

# 5. Frontend event listeners — every backend emit should have a frontend subscriber
grep -roE 'emit(_all)?\\("[a-z][a-z_-]+"' app/src-tauri/src --include='*.rs' \
  | sort -u                                                                    # backend emits
grep -roE 'listen[^"]*"[a-z][a-z_-]+"' app/src --include='*.ts' --include='*.tsx' \
  | sort -u                                                                    # frontend listens
```

### Updates that should NOT touch CLAUDE.md

- Refactoring **inside** an existing module that doesn't change its public surface
- Bug fixes that don't add/remove files, commands, events, or env vars
- Frontend component splits/merges within `app/src/components/` (unless you change a category in the §Frontend Architecture table)
- Internal type renames that don't appear in CLAUDE.md
- Test-only additions (count drift inside ±50 is fine; just don't claim a number you didn't recount)
- Comment-only changes

## Quick Reference

```bash
# Rust backend
cd app/src-tauri
cargo check                    # type-check
cargo test                     # full backend test suite (~960 #[test] annotations)
cargo build                    # release build
cargo fmt --check              # CI-mirror format check
cargo clippy -- -D warnings    # CI-mirror lint

# Frontend (React + Vite)
cd app
npm test                       # Vitest run-once (~40 test files)
npm run test:watch             # Vitest in watch mode
npx tsc --noEmit               # CI-mirror type check
npm run dev                    # Vite dev server (port 1430)

# Full Tauri shell (Rust + Vite + bundled Pi)
cd app
npm run tauri:dev              # dev mode with HYVEMIND_DEBUG=1
npm run tauri:build            # production bundle
```

## Content Security Policy (CSP)

The Tauri WebView runs under a strict CSP defined in `app/src-tauri/tauri.conf.json` under `app.security.csp`. The policy is:

```
default-src 'self';
script-src 'self';
style-src 'self' 'unsafe-inline' https://fonts.googleapis.com;
img-src 'self' data: blob: asset: http://asset.localhost https://asset.localhost;
connect-src 'self' ipc: http://ipc.localhost https://ipc.localhost http://localhost:* ws://localhost:*;
font-src 'self' data: https://fonts.gstatic.com;
object-src 'none';
base-uri 'self';
frame-ancestors 'none';
form-action 'self'
```

**Why each directive looks like this**

| Directive | Allowed | Reason |
|-----------|---------|--------|
| `default-src` | `'self'` | Deny-by-default fallback for any directive not listed below. |
| `script-src` | `'self'` only | No inline JS, no `eval`. The Vite production build emits bundled scripts only, and the codebase has no `dangerouslySetInnerHTML`/`eval`/`new Function`. **Do not add `'unsafe-inline'` or `'unsafe-eval'` here** — that's the single biggest XSS lever. |
| `style-src` | `'self'`, `'unsafe-inline'`, `https://fonts.googleapis.com` | Tailwind + Vite inject runtime `<style>` blocks (this is unavoidable without nonces, which Tauri doesn't wire up). `fonts.googleapis.com` hosts the `@import`-style CSS for Inter and JetBrains Mono loaded from `app/index.html`. |
| `img-src` | `'self'`, `data:`, `blob:`, `asset:`, `http(s)://asset.localhost` | `data:` for inline SVG backgrounds in `src/index.css`. `blob:` for `URL.createObjectURL` image-attachment previews in Tasks/Chat. `asset:` and `asset.localhost` are Tauri 2's asset protocol, in case future features serve local files to the WebView. |
| `connect-src` | `'self'`, `ipc:`, `http(s)://ipc.localhost`, `http://localhost:*`, `ws://localhost:*` | Tauri 2 IPC bridge uses `ipc:`/`ipc.localhost` (platform-dependent). `http://localhost:*` covers the Vite dev server (port 1430). `ws://localhost:*` is a defensive allow for any future Vite HMR (HMR is currently disabled in `vite.config.ts`). The renderer makes no direct external HTTP calls — all LLM provider traffic goes through Rust via Tauri IPC. |
| `font-src` | `'self'`, `data:`, `https://fonts.gstatic.com` | `fonts.gstatic.com` serves the actual font binaries Google Fonts hands out. `data:` is permitted for any tooling that inlines a fallback glyph. |
| `object-src` | `'none'` | No `<object>`/`<embed>`/Flash. |
| `base-uri` | `'self'` | Prevents `<base href="...">` injection redirecting relative URLs to an attacker host. |
| `frame-ancestors` | `'none'` | The Tauri window can't be embedded in another frame. |
| `form-action` | `'self'` | Defensive; no real `<form>` submits happen, but locks down `<form action="...">` injection. |

**Things to watch when modifying the frontend**

- Adding a new external CDN (image, font, analytics, etc.) requires updating the corresponding directive. Prefer self-hosting.
- Sentry events ship via the `tauri-plugin-sentry` IPC bridge, not a direct browser call to `*.ingest.sentry.io`. If that ever changes to direct ingest, add the Sentry host to `connect-src`.
- If you switch `react-markdown` to render raw HTML (`rehype-raw` or similar) you reintroduce XSS risk; the current strict policy assumes markdown is rendered through React components only.
- All LLM provider HTTP traffic is made by Rust (`reqwest`), not the WebView. Keep it that way — moving any provider call into the frontend would require opening `connect-src` to that vendor.

**Verification status**

- `cargo check` passes with the policy in place (Tauri's config codegen accepts it).
- Full dev-mode WebView verification (`tauri:dev` + DevTools console) was not run in the worktree where this change was authored (no `node_modules`/Pi-binary install). When pulling this change locally, run `npm run tauri:dev` and watch the DevTools console for `Refused to load ... because it violates the following Content Security Policy directive:` warnings — adjust the relevant directive if any legitimate asset is blocked.

## Pi binary

Pi is bundled, not user-installed. The pinned version lives in `scripts/pi-version.txt` (one line, e.g. `0.74.0`); bumping Pi is a one-line edit followed by a rebuild. `bash scripts/build-pi.sh` bun-compiles `@earendil-works/pi-coding-agent` at that version into `app/src-tauri/binaries/pi-<target-triple>` and snapshots the three required npm extensions (`pi-web-access`, `pi-subagents`, `pi-mcp-adapter`) plus two local extensions (`hyvemind-providers`, `hyvemind-handoff` from `app/src-tauri/pi-extensions/`) and Pi's runtime sibling files (`package.json`, `dist/modes/interactive/assets/`) into the same `binaries/` directory. The build is idempotent — a stamp file at `binaries/.pi-version` short-circuits re-runs when the pin hasn't changed. `tauri.conf.json` ships the binary via `externalBin` and the support files via `bundle.resources`. `npm run prepare-pi` is invoked automatically by `tauri:dev` / `tauri:build` (see `app/package.json`); the stamp file short-circuits when the pin hasn't changed, so day-to-day rebuilds are free.

## Crash recovery

After an ungraceful exit (host crash, force-quit, OOM, power loss), Hyvemind reconciles three classes of orphaned state on the next launch. Anything not in this list is **lost**.

| What | Where it's reconciled | What gets recovered | What's lost |
|------|----------------------|--------------------|-------------|
| Orphan Pi session transcripts | `PiManager::reconcile_graveyard_from_disk(&home)` at startup in `lib.rs`, before `app.manage(app_state)` | One in-memory `GraveyardEntry` per `~/.hyvemind/chat-sessions/{session_id}.jsonl` file. Each entry carries the parsed `cwd` and `provider/model` from the transcript header so the next `send_message` against that `session_id` respawns Pi with `--continue --session <file>` against the original working directory. | The Pi subprocess itself (already dead). Any tokens that were mid-stream when the host crashed — Pi only persists complete events. The exact `tool_set`, `thinking_level`, and `system_prompt` originally chosen are reconstructed from the header where present and otherwise default; the next prompt typically re-supplies them. |
| Orphan "Implementing" swarms | `reconcile_orphaned_swarms` + `migrate_legacy_reconciled_failures` early in `AppState::new` | Each on-disk swarm still tagged `Implementing` is rewritten to `Interrupted` with an explanatory error message; the user can click Resume to continue from where the Queen left off. `Paused` swarms are intentionally untouched so user-issued pauses survive a restart. | In-flight Worker/Guard child Pi sessions (those orphans are reaped via the graveyard above). |
| Orphan Hivemind reviews / merges | `hivemind_store.sweep_interrupted_merges()` and `.sweep_interrupted_jobs()` in `AppState::new`; rows are then drained via `take_pending_interrupted_emits()` and emitted to the frontend as `merge_interrupted` / `review_interrupted` Tauri events after `app.manage()` | Any `merge_run` left `running` or any job left `pending` / `running` / `round_*` is flipped to `interrupted` so the UI can offer recovery instead of showing a phantom spinner. | Partial round outputs that hadn't been persisted to SQLite at crash time. |

The graveyard reconciliation is deliberately conservative: it never overwrites an existing in-memory `GraveyardEntry`, file parse failures are logged and skipped (the entry is still inserted with defaults so a respawn is at least possible), and a missing `chat-sessions/` directory is a no-op (fresh install).

**`run_id` semantics (audit 2.3)** — the Queen mints one `run_id` per `run_feature_full` invocation (`core/queen.rs:993`). Scout, Worker, inline Guard, and validator Guard for the same attempt all share it. Format: `run-{feature_id}-{fix_attempt_count}-{uuid_simple}`. This is what lets `progress_log.jsonl` distinguish retries on the same feature, and what routes per-agent debug files (`debug/swarms/{swarm_id}/{agent}-{feature_id}-{run_id}.jsonl`). A feature with `fix_attempt_count == 3` will have 4 distinct `run_id`s in the log (original + 3 fixes).

**`Paused` is intentionally NOT rewritten** by the reconciler — a user-issued pause survives a restart. Only `Implementing` (and `Failed`-with-in-flight-features) flips to `Interrupted`.

**Process-enumeration-style reaping** (`ps`/`sysinfo` scan for orphan Pi children whose original parent PID is gone) is intentionally **not** implemented. Pi children whose parent dies are already handled by `kill_on_drop(true)` on the `tokio::process::Child` wrappers and by the OS's parent-death signalling on macOS/Linux. Scanning the system process table to claim "ours" by argv heuristics is too invasive and racy to be reliable — and the on-disk transcript (which we do recover) is what matters for resuming a conversation, not the dead subprocess.

## Project Layout

This is the Rust backend tree. For the frontend, see [Frontend Architecture](#frontend-architecture).

```
app/
  src-tauri/
    src/
      main.rs                    # Entry point — calls lib::run()
      lib.rs                     # Tauri builder, invoke_handler!, setup hooks, RunEvent::Exit teardown
      sentry_setup.rs            # Sentry client init (opt-in via config.crash_reporting)
      tunables.rs                # Centralised HYVEMIND_* env-var knobs (see §Tunables)
      commands/                  # Tauri IPC command handlers — thin delegators only, no business logic
        mod.rs                   # Module declarations
        chat.rs                  # send_message, stop_chat, get_chat_history, prewarm_pi_session,
                                 #   list_chat_sessions, delete_chat_session, is_session_busy,
                                 #   get_session_last_assistant_text
        tasks.rs                 # save/load/delete_task_messages, get_task_state, auto_commit_task,
                                 #   list_project_files
        hivemind.rs              # start_review, get_review_status, list_reviews, create/update/delete_hivemind,
                                 #   get_review_step_outputs, cancel_review, delete_review, save/list_round_verdicts,
                                 #   read_merge_output, register_context_session, get_orchestrator_usage,
                                 #   get_resumable_review_for_task, get_review_plan, get_review_state,
                                 #   log_review_event (+ debug-only clear_response_cache)
        swarms.rs                # create_swarm, update_swarm, start/pause/resume/stop_swarm,
                                 #   get/list/delete_swarm, get_swarm_progress/features/milestones/usage,
                                 #   check_swarm_readiness
        settings.rs              # get_settings, get_system_prompts, list/save/delete_custom_prompt,
                                 #   set_runtime_settings, set_default_model/hivemind/project_path,
                                 #   request_working_dir_approval, save/delete_api_key, get_providers,
                                 #   add_provider, test_provider_models/chat/pi, refresh_models,
                                 #   get_pi_status, update_pi, install_pi, check_subscription_auth,
                                 #   set_auto_commit_tasks/conventional, set_task_completion_sound,
                                 #   set_crash_reporting, set_chat_check_in_secs,
                                 #   set_extension_poll_interval_secs, set_daily_budget
        dashboard.rs             # get_dashboard_stats, get_model_usage, get_provider_usage,
                                 #   get_cost_summary, get_recent_activity
        extensions.rs            # list_extensions, get_usage_snapshots, refresh_usage_snapshot,
                                 #   update_extension_settings
        nurse.rs                 # get_nurse_status, set_nurse_config, check_chat_session
        sessions.rs              # list_active_pi_sessions, kill_pi_session, reconcile_active_sessions,
                                 #   pi_pool_stats, get_pi_session_stats
        tests.rs                 # run_stability_test, cancel_test_run, get_active_test_run,
                                 #   list_test_runs, get_test_run, get/set_stability_test_config
        util.rs                  # path normalisation + shared helpers (no commands)
      core/                      # Swarm execution engine (see app/src-tauri/src/core/README.md)
        queen.rs                 # Orchestrator — decomposes goal, runs the integrated swarm loop
        scout.rs                 # Per-feature planner (read-only); produces implementation plan + risks
        scout_review.rs          # Optional Hivemind review of the Scout's plan before Worker handoff
        worker.rs                # Implementer; takes plan, writes code, calls `submit_handoff` Pi tool
        handoff.rs               # WorkerHandoff capture — reads `submit_handoff` tool args directly off the
                                 #   JSONL `tool_execution_start` event via the `hyvemind-handoff` Pi
                                 #   extension. No delimited-marker parser, no last-JSON-block fallback —
                                 #   missing tool call surfaces as `HandoffParseFailed` (Queen downcasts to a
                                 #   Nurse intervention bubble at queen.rs:2095) and the feature fails.
        guard.rs                 # Milestone validator; synthesises fix-features (cap 3 attempts)
        scheduler.rs             # Topological sort, cycle detection, next_ready_batch() (bounded parallelism)
        swarm_context.rs         # Per-run context: cwd, git state, registries, model settings
        services.rs              # Service composition (Pi pool, store, progress log, registries)
        budget.rs                # Per-swarm + daily cost tracking; emits budget_exceeded
        readiness.rs             # Pre-flight checks (cargo crates, npm packages, system binaries)
        validation.rs            # Milestone-assertion validation helpers
        idempotency.rs           # Idempotency key generation/tracking for swarm ops
        recovery.rs              # Crash-recovery constants & helpers (e.g. INTERRUPTED_BY_RESTART_MSG)
        stability_test.rs        # Stability-test types + active-run tracking
        stability_test/
          runner.rs              # Test runner — spawns worker+guard, emits test-progress
      hivemind/                  # Multi-model review engine (see app/src-tauri/src/hivemind/README.md)
        mod.rs                   # Module root
        engine.rs                # ReviewEngine — JoinSet concurrency, per-round timeout, cross-review
        circuit_breaker.rs       # 3-state breaker (Closed/Open/HalfOpen) with probe_in_flight gating
        cache.rs                 # moka-based ResponseCache (lock-free, TTL- and size-bounded)
        backoff.rs               # Exponential backoff: min(60s, 5s*2^attempt + rand(0,2s))
        store.rs                 # SQLite store for review jobs/steps (sqlx, WAL mode)
        review_log.rs            # ReviewLogger — high-level event JSONL under ~/.hyvemind/reviews/
        review_schema.rs         # Review domain types (ReviewConfig, RoundConfig, Stance, etc.)
        merge_capture.rs         # Per-merge-run text capture under reviews/{id}/merge-r*.txt
        output_capture.rs        # Per-model-call output capture under reviews/{id}/output-*.txt
        events.rs                # ReviewEvent enum for progress emit
        phase.rs                 # Phase derivation helpers for progress UI
        verdicts.rs              # Round-verdict / decision types
        error.rs                 # Review error types
      nurse/                     # Push-mode Nurse engine (see app/src-tauri/src/nurse/README.md)
        mod.rs                   # Module root
        bus.rs                   # NurseBus — tokio::sync::broadcast<Arc<NurseBusEvent>>
        engine.rs                # NurseEngine — subscribes to bus, runs detectors, drives Dispatcher
        dispatcher.rs            # Three-tier pipeline (Tier 1 tier1_lookup / Tier 2 SteerPlaybook / Tier 3 LlmClassifier)
                                 #   + EventSeq + InFlightGuard + SELF_KILL_GRACE
        intervention.rs          # DefaultApplier + KillableSession/SessionKiller traits + kill_with_verification
                                 #   + cancel_hivemind_review + mark_self_killed
        health.rs                # SessionHealth, Signal, Severity, Tier, EscalationState
        detector.rs              # Detector trait, DetectorContext, DetectorRegistry, TickKind (Fast/Slow)
        detectors/
          mod.rs
          stall.rs               # Time-based + post-prompt silence stall detection
          reasoning_loop.rs      # Siphash exact / compression / minhash paraphrase loop detection
          tool_failure.rs        # Repeated tool failure clustering
          process_health.rs      # Pi subprocess liveness + stderr crash patterns
          provider_health.rs     # Circuit-breaker / missing-provider signals
          context_saturation.rs  # Pi context-window % threshold
          retry_exhaustion.rs    # Auto-retry-end clustering in a sliding window
        classifier.rs            # LlmClassifier — Tier 3 wrapper around ProviderRegistry
        intervention_writer.rs   # Bounded mpsc → in-memory ring (legacy IPC compat)
        playbook.rs              # Tier 2 templated steer table
        storm_guard.rs           # Per-(session, kind) sliding-window guard
        budget.rs                # Per-detector + age-decay intervention budget
        config.rs                # NurseConfig + NurseMode + NurseProfile + ProfileConfig + BudgetConfig
                                 #   + clamp_stall_threshold
        schema.rs                # Provider-native tools/tool_choice JSON (Tier 3 classifier schema)
        prompt.rs                # include_str!("../../prompts/nurse_system.md") accessor
        snapshot.rs              # Wire DTOs (NurseDecision, NurseEvent, NurseLifecyclePayload,
                                 #   NurseStatusSnapshot, SessionOwnerDto, TunableDef, NurseDispatchTier, ...)
        synthesized.rs           # SynthesizedKind, InterventionOwner, describe_synthesized
        supervisor.rs            # Thin wrapper over util::supervise::super_watchdog
        observability/           # Always-on JSONL surfaces under ~/.hyvemind/debug/nurse/
          mod.rs                 # ObservabilityHandles, prune_on_startup
          writer.rs              # JsonlWriter — bounded mpsc, non-blocking
          decision_log.rs        # DecisionLogger — daily-rotated decisions.jsonl.YYYY-MM-DD
          capture.rs             # ClassifierCapture — per-decision prompt/response files (1 MiB cap)
          signal_stream.rs       # SignalStream — per-session JSONL with 4 MiB rotation
          bus_telemetry.rs       # BusTelemetry — bus.jsonl.YYYY-MM-DD (lag, owner_changed, capacity_pressure)
      pi/                        # Pi subprocess management (see app/src-tauri/src/pi/README.md)
        mod.rs                   # Module root
        defaults.rs              # Seeds sensible defaults for bundled Pi extensions
                                 #   (e.g. forces `workflow: "none"` in ~/.pi/web-search.json
                                 #   so the curator UI doesn't hang headless web_search calls)
        rpc.rs                   # JSONL protocol client; dedicated stdout/stderr reader tasks
        session.rs               # PiSession — owns OwnedSemaphorePermit + authoritative transcript
        manager.rs               # PiManager — process pool with global Semaphore (default 30)
        events.rs                # PiEvent enum + PiEventBatcher (100ms / 50-token IPC throttling)
        eviction.rs              # Unified maintenance loop: idle eviction (10-min TTL), context-bloat respawn,
                                 #   auto_commit_locks sweep, pi_pool_stats observability log
        transport.rs             # Transport abstraction (used for testing)
        chunk_sink.rs            # Streaming-chunk accumulation helper
        mock.rs                  # MockRpcClient (feature-gated behind `test-mocks`)
      extensions/                # Provider usage/balance extension framework (see extensions/README.md)
        mod.rs                   # Module root, builtin registration
        traits.rs                # ProviderExtension / UsageProvider / BillingProvider traits
        types.rs                 # ExtensionManifest, UsageSnapshot, UsageMetric, ExtensionError
        registry.rs              # Extension registry + per-provider matching
        context.rs               # ExtensionContext (app handle, config access, http client, api_key lookup)
        poller.rs                # Per-extension polling loop (emits usage-snapshot-updated)
        tests.rs                 # Integration tests for registry, dedup, capabilities
        builtins/                # Built-in provider extensions
          mod.rs
          anthropic_usage.rs     # Anthropic usage/credit polling
          openrouter_credits.rs  # OpenRouter credit polling
          deepseek_balance.rs    # DeepSeek balance polling
          crof_usage.rs          # CROF usage polling
          claude_sub_usage.rs    # Claude subscription usage polling
          chatgpt_sub_usage.rs   # ChatGPT subscription usage polling
          neuralwatt_usage.rs    # Neuralwatt usage polling
      providers/                 # LLM backend dispatch (see app/src-tauri/src/providers/README.md)
        mod.rs                   # All provider impls (Anthropic, OpenAICompatible, OpenRouter,
                                 #   PiSubscription, Mock) + ProviderRegistry + cost lookup
        provider_trait.rs        # Object-safe Provider trait + StreamingProvider extension trait
      domain/                    # Cross-subsystem type definitions (see app/src-tauri/src/domain/README.md)
        mod.rs                   # Re-exports
        swarm.rs                 # SwarmState, SwarmStatus, Feature, FeatureStatus, ModelSettings, Milestone
      state/                     # Persistence + app state (see app/src-tauri/src/state/README.md)
        mod.rs                   # Module root
        activity_log.rs          # Per-swarm append-only swarm-activity transcript; schema-versioned JSONL + paginated reader
        app_state.rs             # AppState — Tauri-managed root holding every subsystem Arc
        config.rs                # JSON config + env-var overrides; hot-reload-safe via clone-and-drop
        secret_store.rs          # OS keyring + AES-256-GCM-encrypted `.credentials` cache
        store.rs                 # SwarmStore — per-swarm directory layout, atomic writes (tempfile+rename)
        progress.rs              # Append-only JSONL progress log; crash-safe replay target (ProgressReader)
        swarm_registry.rs        # RunningSwarm registry with CancellationToken + pause support
        usage_store.rs           # Aggregated usage/cost for Dashboard + topbar
        log_redact.rs            # API-key scrubbing writer wrapped around debug log files
        log_routing.rs           # PerIdRoutingLayer — routes tracing events to per-ID files on disk
        ipc_error.rs             # IpcError envelope returned by every command (mirrored in frontend)
        channel_drop.rs          # Channel-drop tracking utilities
        sync.rs                  # Misc sync primitives
      util/                      # Cross-cutting utilities
        mod.rs
        supervise.rs             # super_watchdog — respawns a tokio task once if the outer watchdog panics
    prompts/                     # Agent system prompts (most compiled in via include_str!)
      scout_system.md            # loaded by core/scout.rs
      worker_system.md           # loaded by core/worker.rs
      guard_system.md            # loaded by core/guard.rs
      nurse_system.md            # loaded by nurse/prompt.rs
      queen_system.md            # on disk but NOT include_str'd — Queen prompt is constructed dynamically
      queen_planning.md          # on disk but NOT include_str'd — planning sub-prompt
      plan_system.md             # on disk but NOT include_str'd — shared plan template
      stability_test_task.md     # loaded by core/stability_test/runner.rs
      stability_test_verifier.md # loaded by core/stability_test/runner.rs
    pi-extensions/               # Local Pi extensions (TypeScript, bundled into binaries/ by build-pi.sh)
      hyvemind-providers/        # Provider management extension (index.ts + package.json)
      hyvemind-handoff/          # Worker handoff extension (index.ts + package.json)
    migrations/
      0001_hivemind.sql          # SQLite schema: jobs, job_steps, schema_migrations
    Cargo.toml
    tauri.conf.json
  binaries/                      # Bundled Pi distribution (gitignored, built by scripts/build-pi.sh)
    pi-<target-triple>           # bun-compiled Pi binary, pinned to scripts/pi-version.txt
    package.json                 # Pi's own package.json (runtime version + asset path resolution)
    theme/                       # Pi's theme JSONs (read at startup before mode selection)
    dist/                        # Pi runtime assets it expects as siblings
    pi-extensions/               # Bundled npm extensions: pi-web-access, pi-subagents, pi-mcp-adapter,
                                 #   plus the two local extensions copied from src-tauri/pi-extensions/
  src/                           # Frontend (see §Frontend Architecture)
```

## Key Types

| Type | Location | Purpose |
|------|----------|---------|
| `AppState` | `state/app_state.rs` | Tauri-managed root state; holds every subsystem `Arc` |
| `Config` | `state/config.rs` | JSON config loaded from `~/.hyvemind/config.json` with `HYVEMIND_*` env overrides |
| `SecretStore` | `state/secret_store.rs` | OS-keyring wrapper + AES-256-GCM-encrypted `.credentials` cache |
| `SwarmStore` | `state/store.rs` | Per-swarm directory layout, atomic writes via `tempfile + rename` |
| `ProgressLogger` / `ProgressReader` | `state/progress.rs` | Append-only JSONL progress log + crash-safe replay |
| `ActivityWriter` / `ActivityReader` | `state/activity_log.rs` | Per-swarm append-only `swarm-activity` transcript with monotonic `seq` per event; paginated reader powers `get_swarm_activity_log` history replay |
| `RunningSwarm` / `SwarmRegistry` | `state/swarm_registry.rs` | In-memory registry with `CancellationToken` + pause support |
| `UsageStore` | `state/usage_store.rs` | Aggregated usage/cost feeding Dashboard + topbar |
| `PerIdRoutingLayer` | `state/log_routing.rs` | Tracing layer that routes events to per-session / per-review / per-swarm files |
| `IpcError` | `state/ipc_error.rs` | Structured error envelope every `#[tauri::command]` returns; mirrored in `app/src/lib/ipc.ts` |
| `SwarmState` / `SwarmStatus` | `domain/swarm.rs` | Canonical swarm lifecycle types |
| `Feature` / `FeatureStatus` | `domain/swarm.rs` | Feature with dependencies, status, fix-attempt tracking |
| `ModelSettings` | `domain/swarm.rs` | Which models to use for scout / worker / guard / Hivemind |
| `Milestone` | `domain/swarm.rs` | Group of features with assertions Guard runs |
| `Provider` (trait) | `providers/provider_trait.rs:115` | Object-safe base trait — every backend implements `call(req: CallRequest) -> Result<ModelResponse>`. Stored in `ProviderRegistry` as `Arc<dyn Provider>`. **Audit 6.6 split** the streaming variant into a separate `StreamingProvider` extension trait (`provider_trait.rs:149`, surfaced via `Provider::as_streaming()`) to fix a silent-drop bug where Anthropic / Pi-sub accepted an `mpsc::Sender` and dropped it. Do not merge them back. Only `OpenAICompatibleProvider`, `OpenRouterProvider`, and `MockProvider` implement `StreamingProvider`; Anthropic and PiSubscription remain buffered-only. |
| `ModelResponse` | `providers/mod.rs` | LLM call result (output, tokens, cost) |
| `ProviderRegistry` | `providers/mod.rs` | Dispatch + lookup keyed by `provider_id` |
| `ReviewEngine` | `hivemind/engine.rs` | Multi-model review orchestrator |
| `ReviewLogger` | `hivemind/review_log.rs` | Writes the per-review event log under `~/.hyvemind/reviews/{id}.jsonl` |
| `OutputCapture` | `hivemind/output_capture.rs` | Writes per-model full outputs to `~/.hyvemind/reviews/{id}/output-*.txt` |
| `MergeCapture` | `hivemind/merge_capture.rs` | Writes per-merge text to `~/.hyvemind/reviews/{id}/merge-r*.txt` |
| `CircuitBreaker` | `hivemind/circuit_breaker.rs` | 3-state breaker (Closed/Open/HalfOpen) with probe-in-flight gating |
| `ResponseCache` | `hivemind/cache.rs` | moka-based, lock-free, TTL- and size-bounded |
| `PiManager` | `pi/manager.rs` | Process pool with global `Semaphore` (default 30) |
| `PiSession` | `pi/session.rs` | Single Pi subprocess wrapper; owns an `OwnedSemaphorePermit` |
| `PiEventBatcher` | `pi/events.rs` | 100ms / 50-token coalescer for streaming events |
| `IdleEvictionPolicy` | `pi/eviction.rs` | 10-min idle TTL + maintenance loop (context-bloat respawn, lock sweep, observability log) |
| `Queen` / `Scout` / `Worker` / `Guard` | `core/{queen,scout,worker,guard}.rs` | Bee-role agents — see `core/README.md` for the loop |
| `Scheduler` | `core/scheduler.rs` | Topological feature ordering with batch dispatch (bounded parallelism) |
| `WorkerHandoff` | `core/handoff.rs:89` | Structured payload the Worker emits via the `submit_handoff` Pi tool (NOT delimiter-parsed; the local `hyvemind-handoff` Pi extension captures tool args directly). Schema: `{ feature_id, run_id, salient_summary, what_was_implemented, verification, success_state: "success"|"failure"|"partial", discovered_issues?: [{severity, description, suggested_fix?}] }`. `feature_id` mismatch against `expected_feature_id` returns `None` from `handoff_from_tool_args` (`handoff.rs:138`) — Queen downcasts on `HandoffParseFailed` (`queen.rs:2095`) to synthesise a Nurse intervention bubble; the feature is marked `Failed`. `SuccessState` deserialiser is case-insensitive with `failed`/`fail` aliases. **There is no fallback parser** — adding one would break the contract. |
| `NurseEngine` | `nurse/engine.rs` | Global push-mode Nurse — subscribes once to `NurseBus`, runs the detector registry per session, drives the `Dispatcher`. `engine.start()` returns `Err` until `attach_app_handle` + `attach_dispatcher` have run; spawns `run_loop` (bus subscription + periodic sweep at `tick_interval_secs`). |
| `Dispatcher` | `nurse/dispatcher.rs` | Three-tier pipeline (Tier 1 `tier1_lookup` deterministic / Tier 2 `SteerPlaybook` templated / Tier 3 `LlmClassifier`) plus `EventSeq`, `InFlightGuard`, `SELF_KILL_GRACE = 30s`. Sole nurse dispatcher. |
| `DefaultApplier` / `SessionKiller` / `KillableSession` | `nurse/intervention.rs` | Action surface — applies dispatch outcomes, emits `nurse-event`, performs `kill_with_verification` (`abort → 3s grace poll → kill_session → 7s post-kill poll → dead_at\|double_fail_giving_up`). Also hosts `cancel_hivemind_review` and `mark_self_killed`. |
| `NurseConfig` | `nurse/config.rs` | Master switches (`enabled`, `mode: Enabled\|Observe\|Disabled`, `nurse_model`, `nurse_provider`) + per-profile `ProfileConfig` map. `clamp_stall_threshold` lives here. `nurse_model = "none"` skips Tier 3 silently. |
| `NurseDecision` | `nurse/snapshot.rs` | Wire DTO for dispatcher output: `LeaveIt { check_back_secs }` / `Steer { message }` / `Restart` / `Cancel { message? }`. Bit-identical to the legacy shape so existing frontend listeners see no change. Per-detector intervention budget (see `BudgetState`) replaces the old flat `max_interventions = 3` ceiling. Hivemind pseudo sessions (no Pi process behind them) only support `leave_it` / `cancel`. |
| `BudgetState` | `nurse/budget.rs` | Per-detector + age-decay intervention budget — replaces v1's flat `max_interventions = 3` ceiling. Uses `is_cooldown_elapsed` + `try_admit` with initial cap + per-hour decay + max cap + per-detector cap + per-`dedup_key` cooldown. Defaults per profile via `ProfileConfig::default_for(profile)`. |
| `super_watchdog` | `util/supervise.rs:150` | One-shot panic respawn — runs the inner future, respawns exactly once on panic/join-error, logs `"nurse unrecoverable"` if the respawn also crashes. Used by `nurse/supervisor.rs` to wrap `NurseEngine::run_loop` and identically by the Pi maintenance loop (`lib.rs`). Every fire-and-forget supervisor should use this pattern. |

## Bee-role tool & thinking-level matrix

Per-role Pi wiring lives in `pi/rpc.rs` (`PiSessionOptions::for_scout/for_worker/for_guard`). Per-swarm overrides via `model_settings.{role}_thinking_level`.

| Role | Thinking (default) | Tool set | Structured-output Pi tool | Failure mode if tool is never called |
|------|--------------------|----------|---------------------------|-------------------------------------|
| Queen | `Medium` | Full coding + bash (runtime persona); read-only (planning persona) | `submit_features` (planning) | Feature decomposition errors |
| Scout | `High` | Read-only (`read`/`grep`/`find`/`ls`); **no bash** | `submit_scout_result` | `"scout for feature '<id>' did not call submit_scout_result"` (`scout.rs:104`) → feature `Failed` |
| Worker | `Medium` | Full coding + bash | `submit_handoff` (via `hyvemind-handoff` Pi extension) | `HandoffParseFailed` → Queen downcast (`queen.rs:2095`) → Nurse bubble → feature `Failed` |
| Guard | `Medium` | Full coding + bash | `submit_guard_result` | `"guard for validator feature '<id>' did not call submit_guard_result"` (`guard.rs:166`) |
| Nurse classifier (Tier 3 only) | `Low` | **None** (pure classifier; non-streaming `ProviderRegistry.call` via `nurse/classifier.rs`; dispatch entry at `nurse/dispatcher.rs`) | `nurse_decisions` (schema: `nurse/schema.rs`) | Classifier returns parse error → decision finalises as gated/no-op; loop continues |

**There is no fallback parser for any `submit_*` tool** — the Rust backend captures `args` directly off the JSONL `tool_execution_start` event. The local `hyvemind-handoff` Pi extension's `execute` is a no-op echo for the same reason. Do not add regex fallbacks "just in case" — it would defeat the contract and mask prompt regressions.

## Hidden invariants & lock ordering

Rules that are not visible from reading one file at a time. Breaking any of them is a real bug — most have already been broken once.

### Cross-subsystem invariants (domain layer)

- **`domain/` imports nothing from `core::*`, `state::*`, or `pi::*`** (`domain/mod.rs:1`). It exists to break the historical `core` ↔ `state` cycle; reintroducing such an import re-creates the cycle.
- **`Feature.status == Completed` MUST be paired with a persisted `WorkerHandoff`** at `~/.hyvemind/swarms/{id}/handoffs/{feature_id}.json`. Exception: `validate-*` features (validators have no Worker step). Marking Completed without a handoff breaks Queen replay.
- **The crash reconciler is the ONLY writer of `SwarmStatus::Interrupted`** and the ONLY setter of `Feature.interrupted = true && Feature.resumable = true` (`core/recovery.rs:81,366`). All three are cleared by `resume_swarm`. Setting them anywhere else corrupts the Resume affordance.
- **`max_fix_attempts` is forward-only** (default `3`, `domain/swarm.rs:355`). Removing the check at `core/queen.rs:1804` lets Guard spawn fix-features indefinitely.
- **`Milestone.sealed` is forward-only** (`true → false` is never valid). The scheduler treats sealed milestones as closed worlds (`core/scheduler.rs:100`).
- **`SwarmState.queen_plan_review_done` is forward-only** — once a Queen master-plan Hivemind review is attempted, it's suppressed for the swarm's lifetime (clones / resumes carry it forward).
- **`Feature::is_validator()` is keyed on the `validate-` id prefix** (`domain/swarm.rs:365`). Validator features skip Scout/Worker. Renaming the prefix without updating the predicate silently routes them through the wrong pipeline.
- **Every `#[serde(default)]` on a domain type is load-bearing** — each matches a real file format on user disks. Removing one breaks back-compat for older `state.json` / `features.json` files.

### Lock ordering & async discipline

- `features: Arc<RwLock<Vec<Feature>>>` is **dominant** over `swarm_state: Arc<RwLock<SwarmState>>` (`core/queen.rs:7`). Hold neither across `.await` unless the clone-and-drop idiom from that header comment is used.
- `SwarmUsageAccumulator` wraps `std::sync::Mutex` — **sync-only, never `.await`-spanning**. Lock-poison policy is `unwrap_or_else(|e| e.into_inner())` (project-wide for additive counters; `domain/swarm.rs:11`).
- `ProviderRegistry` sits behind an `AsyncRwLock`. Callers `Arc::clone` the result from `registry.get(name)` and **drop the read guard before any I/O** — a refresh write lock blocks until in-flight reads drop.

### Provider registry refresh contract

- Every command that mutates `config.providers` or `config.provider_keys` **MUST call `AppState::refresh_provider_registry`** (`state/app_state.rs:624`). Forgetting it leaves Hivemind / Nurse dispatching against a stale snapshot until app restart. `save_api_key`, `delete_api_key`, `add_provider` all do this — new mutators must too.

### Extension contract (provider-usage pollers)

- **Never construct your own `reqwest::Client`** — use `ctx.http()` (shared pool, 30s timeout).
- **Never hold a config read guard across `.await`** in `fetch()` — read into a local, drop, then I/O.
- **`ExtensionError::Unsupported` is terminal** — the poller exits permanently. Use `Auth`/`Network`/`Parse`/`Internal` for transient errors (auto-backoff applies).
- **`MIN_REFRESH_INTERVAL_SECS = 30`** (`extensions/poller.rs:38`); lower values are silently clamped.

## Frontend Architecture

The frontend lives under `app/src/`. It's a React 18 + Vite + Tailwind shell speaking to the Rust backend exclusively through Tauri IPC (`invoke()` for commands, `listen()` for events). All LLM provider HTTP traffic happens in Rust — the renderer never reaches an external host.

### Stack

| Layer | Version / config | Notes |
|-------|-----------------|-------|
| UI runtime | React 18.3, TypeScript strict | No SSR; everything renders in the Tauri WebView |
| Bundler | Vite 4 on port `1430` (`strictPort: true`, HMR disabled) | HMR disabled to avoid Tauri reload races; the dev shell rebuilds on file save instead |
| CSS | Tailwind 3.4 (custom palette — see Design system below) | No CSS-in-JS, no shadcn — bespoke `components/atoms.tsx` library |
| Routing | Custom `ScreenRouter` in `App.tsx` (no react-router) | `NavState { tab, params }`; Cmd/Ctrl + 1-6 jumps between tabs |
| State | React Context + custom hooks (no Redux / Zustand) | `TaskRuntimeProvider` is the heavyweight; see State management below |
| IPC wrapper | `lib/ipc.ts` (typed `invoke<T>` around `@tauri-apps/api/core`) | Sentry capture on every call + `IpcError` discriminator |
| Tests | Vitest 4 + Testing Library + jsdom | ~40 test files under `**/__tests__/`; polyfills in `test/setup.ts` |
| Error tracking | `@sentry/react` 10 + `tauri-plugin-sentry-api` | Renderer ships events via the Tauri Sentry plugin (not direct HTTP) |
| DnD | `@dnd-kit/core` + `sortable` + `modifiers` | Topbar pill reorder + task list sorting |
| Markdown | `react-markdown` 10 + `remark-gfm` | Strict — no `rehype-raw`; HTML is escaped (CSP relies on this) |

### Directory layout

```
app/src/
  App.tsx                 # Root: provider tree, Topbar, Sidebar, ScreenRouter, keyboard shortcuts
  main.tsx                # React DOM entry point + Sentry init
  index.css               # Tailwind layers + custom utilities (hex-bg, shimmer, pulse-*, honey-edge)
  A11Y.md                 # Frontend accessibility conventions (aria-live, screen-reader rules)
  screens/                # Top-level routable views
    __tests__/
  components/             # Shared UI library (atoms + composed widgets)
    __tests__/
  lib/                    # IPC wrapper, context providers, event stores, runtime
    __tests__/
  hooks/                  # Custom React hooks (useNurseStatus, etc.)
  types/                  # IPC payload + domain TypeScript types
  extensions/             # Topbar extension widgets (provider usage pills)
    __tests__/
  state/                  # TestRunProvider lives here
  data/                   # Mock data for browser-preview / Storybook-style use
  test/                   # Vitest setup (polyfills)
```

### Screens

11 screens, registered in `App.tsx` `ScreenRouter` (lines ~359-386). All accept `go: GoFn` and optionally screen-specific params.

| Tab | File | Purpose |
|-----|------|---------|
| `dashboard` | `screens/Dashboard.tsx` | Stats overview + cost trends |
| `tasks` | `screens/Tasks.tsx` | Planning conversation (the only conversational surface) |
| `swarms` | `screens/Swarms.tsx` | Swarm list + management |
| `new-swarm` | `screens/NewSwarm.tsx` | Swarm creation / edit form |
| `swarm-control` | `screens/SwarmControl.tsx` | Live swarm execution monitor |
| `hiveminds` | `screens/Hiveminds.tsx` | Multi-model review templates list |
| `hivemind-edit` | `screens/HivemindEdit.tsx` | Rounds + models + orchestrator config |
| `model-browser` | `screens/ModelBrowser.tsx` | Searchable model catalog |
| `review-history` | `screens/ReviewHistory.tsx` | Historical review runs for a Hivemind |
| `tests` | `screens/Tests.tsx` | Stability-test runner + results |
| `settings` | `screens/Settings.tsx` | API keys, providers, Nurse config, zoom, sound, source dir |

**To add a new screen**: write the file under `screens/`, import it in `App.tsx`, add a case to `ScreenRouter`, add a tab entry to `NAV` if it's a top-level destination, and (if hidden behind the Pi gate) add the tab id to `LOCKED_WHEN_NO_PI`. Then update §Screens in this section.

### State management

No Redux. Context providers nest in `App.tsx` (audit 6.7); each owns one slice of state and subscribes to the Tauri events that drive it.

| Provider | File | Owns | Subscribes to |
|----------|------|------|---------------|
| `SettingsProvider` | `lib/SettingsProvider.tsx` | Backend `SettingsResponse` cache; `useSettings()` / `useSetting<K>()` hooks | `default-model-changed`, `default-project-path-changed`, `default-hivemind-changed` |
| `ProvidersProvider` | `lib/ProvidersProvider.tsx` | Provider list + configured status | `usage-snapshot-updated` (debounced 250ms) |
| `TaskRuntimeProvider` | `lib/taskRuntime.tsx` (~5,400 LOC) | Six internal context slices (`taskRuntime.tsx:835-840`): `TaskDraftContext` (in-flight prompt), `TaskListContext` (per-task message arrays), `TaskRuntimeStateContext` (active session / streaming), `TaskActionsContext` (send/stop/resume callbacks), `HivemindOptionsContext` (review-tier selection), `DefaultsContext` (model + project defaults) | `chat-event`, `hivemind-progress`, `swarm-activity`, `nurse-event` |
| `ExtensionProvider` | `extensions/ExtensionProvider.tsx` | Topbar pill order (localStorage) + usage snapshots | `usage-snapshot-updated` |
| `TestRunProvider` | `state/TestRunProvider.tsx` | Active test-run state | `test-progress` |
| `ErrorModalProvider` | `components/ErrorModal.tsx` | Global error surface + Sentry capture | — |
| `ToastProvider` | `components/Toast.tsx` | Toast queue | — |
| `ContextMenuProvider` | `components/ContextMenu.tsx` | Right-click menu state | — |
| `ProjectContext` | `App.tsx` + `components/ProjectPicker.tsx` | Project list + active project (localStorage) | — |
| `PiStatusContext` | `App.tsx` | Pi-installed gate for navigation; locks dashboard / tasks / swarms / hiveminds / model-browser / review-history when Pi is missing | — |

### IPC layer (`lib/ipc.ts`)

Single chokepoint wrapping `@tauri-apps/api/core::invoke`. Every call:

1. Forwards to `rawInvoke<T>(name, args)` (forwards name-only when `args === undefined` to keep test contracts simple).
2. On rejection, capture to Sentry with tags `source: "ipc"`, `ipc_command`, and (when present) `ipc_error_kind`.
3. Re-throw so callers' error handling (`console.error` → `ErrorModal`) is unchanged.

The `IpcError` discriminator mirrors `state/ipc_error.rs`:

| `kind` | When |
|--------|------|
| `provider_unauthenticated` | API key missing / rejected |
| `provider_rate_limited` | 429 from upstream |
| `circuit_breaker_open` | Per-provider breaker tripped |
| `not_found` | Resource (with `resource` + `resource_id` payload) missing |
| `validation` | Request didn't pass validation |
| `not_approved` | User hasn't approved the working directory (`request_working_dir_approval`) |
| `internal` | Anything else |

`formatIpcError(err)` turns any rejection (typed envelope, `Error`, or string) into a user-facing string for toasts / modals. Use it at every error display site.

### Event listener index

These are the consumer-side counterparts to the §Tauri Events backend table. Mirroring this so a frontend agent can find "who listens for X" without grepping the whole tree.

| Event | Subscribed in | Use |
|-------|--------------|-----|
| `chat-event` | `lib/taskRuntime.tsx` | Streams Tasks tokens, drives Tasks completion |
| `hivemind-progress` | `lib/hivemindEventStore.ts` (singleton fan-out) + `lib/taskRuntime.tsx` | Routes per-review events to the right consumer (task vs swarm vs standalone) |
| `swarm-event` | `lib/taskRuntime.tsx`, `screens/Swarms.tsx`, `screens/SwarmControl.tsx` | Debounced state refresh on lifecycle change |
| `swarm-activity` | `lib/swarmActivityStore.ts` (singleton fan-out) + `screens/SwarmControl.tsx` | Per-feature agent activity stream. On first subscribe for a swarm the store hydrates from `get_swarm_activity_log` (paged, deduped against live events via per-event `seq`) so SwarmControl can replay history before live events take over. |
| `swarm_reconciled` | `screens/Swarms.tsx` | Render Resume badges immediately after a startup-replay reconciliation |
| `nurse-event` | `lib/taskRuntime.tsx`, `hooks/useNurseStatus.ts` | Status snapshot + in-flow intervention bubbles |
| `test-progress` | `state/TestRunProvider.tsx`, `screens/Tests.tsx` | Test phase + result updates |
| `usage-snapshot-updated` | `extensions/ExtensionProvider.tsx`, `lib/ProvidersProvider.tsx` | Topbar pill refresh + provider-configured status |
| `default-model-changed` / `-project-path-changed` / `-hivemind-changed` | `lib/SettingsProvider.tsx` | Patches the cached `SettingsResponse` |
| `pi-install-progress` / `pi-update-progress` | `screens/Settings.tsx` | Pi install/update progress bar |

**Singleton event stores** — `lib/hivemindEventStore.ts` and `lib/swarmActivityStore.ts` each register **one** Tauri listener and fan events out to per-id callbacks via `subscribe(reviewId, cb)` / `subscribe(swarmId, cb)`. **Opening a second `listen()` against the same channel doubles every event** — the Tauri runtime does NOT deduplicate. The moment a second consumer appears for a channel, promote it to a store.

`swarmActivityStore` additionally **hydrates on first subscribe per swarm**: pages `get_swarm_activity_log(swarmId, afterSeq)` up to 200 pages, folds via `applyActivityEvent`, tracks `maxSeenSeq`. Live events received during hydration are buffered and **deduped against the log via the per-event `seq` field** when hydration completes. LRU bookkeeping caps at `MAX_SWARMS = 50` (`swarmActivityStore.ts:13`); oldest evicts when over the cap. Model new stores on this pattern when paged history + dedup is needed.

### Design system

Custom Tailwind palette in `app/tailwind.config.ts`. The whole dark theme is "ink + honey":

| Token | Value | Use |
|-------|-------|-----|
| `ink-950` | `#07080C` | Deepest background |
| `ink-900` | `#0B0D12` | Body background |
| `ink-850` / `ink-800` / `ink-700` | `#0F1219` / `#13161F` / `#1A1E2A` | Panel / surface backgrounds (lightest → darkest panel) |
| `ink-600` / `ink-500` / `ink-400` | `#222837` / `#2A3142` / `#3A4254` | Borders, dividers, raised surfaces |
| `honey-500` | `#F5B919` | Primary accent (buttons, active nav, wordmark) |
| `honey-50` … `honey-900` | full scale | Tints and shades |
| `line` / `line-soft` / `line-strong` | `#1F2433` / `#171B26` / `#2C3346` | Border palette |
| `muted` | `#8B92A5` | Disabled text |
| `dim` | `#5A6072` | Secondary text |

Custom shadows: `glow` (honey ring + drop), `panel` (subtle inset). Fonts: `Inter` (sans), `JetBrains Mono` (mono).

Custom CSS utilities in `index.css`: `.hex-bg` (animated hex tile background), `.shimmer` (token-streaming shimmer), `.pulse-{green,amber,cyan,brain}` (status dot pulses), `.honey-edge` (double-border inset), `.card-hover` (lift + border + shadow on hover).

### Accessibility

See `app/src/A11Y.md` for the complete convention guide. Quick summary:

- Use `aria-live="polite"` for status updates the user shouldn't be interrupted by (token streaming, background sync)
- Use `aria-live="assertive"` only for error toasts and modal alerts
- Streaming text is **non-atomic** (`aria-atomic={false}`) so each chunk is announced incrementally
- Manual VoiceOver pass required for any feature touching the chat stream, swarm activity, or modal flow — there is no automated SR test
- Focus traps live in modals via `focus-trap-react`

## Investigating a Task

Task messages (frontend UI state) are stored as JSON arrays in `~/.hyvemind/task-messages/task-{NUMERIC_ID}.json`. Each element is a message object with `who` (user/asst/plan/questions/session-divider), `text`, `tools`, `reasoning`, `model`, etc.

```bash
# List all task message files
ls -lt ~/.hyvemind/task-messages/

# Read a task's messages (pretty-printed)
python3 -m json.tool ~/.hyvemind/task-messages/task-{ID}.json

# Find which task contains a specific session ID
grep -l '{SESSION_ID}' ~/.hyvemind/task-messages/*.json

# Summarize task messages (who, text preview, tool count)
python3 -c "
import json, sys
with open(sys.argv[1]) as f:
    for i, m in enumerate(json.load(f)):
        who = m.get('who','?')
        text = (m.get('text','') or '')[:100].replace(chr(10),' ')
        tools = len(m.get('tools',[])) if m.get('tools') else 0
        plan = bool(m.get('planText'))
        print(f'[{i:2d}] {who:20s} tools={tools} plan={plan} {text}')
" ~/.hyvemind/task-messages/task-{ID}.json
```

## Investigating a Session ID

When given a session ID (UUID) to investigate, use these commands. Session transcripts live under `~/.hyvemind/chat-sessions/` (the directory name is historical; these are Tasks-view conversations).

```bash
# Read the main session transcript
cat ~/.hyvemind/chat-sessions/{SESSION_ID}.jsonl | python3 -m json.tool --no-ensure-ascii

# Check if it has subagent runs (subdirectory with task-hash/run-N structure)
ls -R ~/.hyvemind/chat-sessions/{SESSION_ID}/ 2>/dev/null

# Read a specific subagent run
cat ~/.hyvemind/chat-sessions/{SESSION_ID}/{TASK_HASH}/run-{N}/session.jsonl | python3 -m json.tool

# Search debug logs for the session (requires HYVEMIND_DEBUG=1 to have been enabled)
grep '{SESSION_ID}' ~/.hyvemind/debug/hyvemind-debug.jsonl.* | python3 -m json.tool

# See the last N events in a session transcript
tail -20 ~/.hyvemind/chat-sessions/{SESSION_ID}.jsonl | python3 -m json.tool

# List all sessions sorted by most recent
ls -lt ~/.hyvemind/chat-sessions/*.jsonl | head -20

# Find a session by partial ID (e.g., first 8 chars of UUID)
find ~/.hyvemind/chat-sessions/ -name "*{PARTIAL_ID}*"

# Summarize session events (type, role, model, tools)
python3 -c "
import json, sys
with open(sys.argv[1]) as f:
    for i, line in enumerate(f):
        obj = json.loads(line.strip())
        t = obj.get('type','')
        ts = obj.get('timestamp','')[:19]
        if t == 'message':
            role = obj['message']['role']
            stop = obj['message'].get('stopReason','')
            tools = [c.get('name','') for c in obj['message']['content'] if c.get('type') in ('toolCall','tool_use')]
            text = [c.get('text','')[:80] for c in obj['message']['content'] if c.get('type') == 'text']
            print(f'{i:3d} {ts} [{role}] stop={stop} tools={tools} text={text[:1]}')
        else:
            print(f'{i:3d} {ts} [{t}]')
" ~/.hyvemind/chat-sessions/{SESSION_ID}.jsonl
```

Subagent artifacts live at `~/.hyvemind/chat-sessions/subagent-artifacts/{hash}_{role}_{n}_{type}.{ext}`.

## Investigating a Hivemind Review

When given a review ID (e.g., `hmr-a1b2c3d4`):

- High-level event log (always written when `HYVEMIND_DEBUG=1`): `~/.hyvemind/reviews/{review_id}.jsonl`
- Per-model full outputs (capture files, written alongside the event log): `~/.hyvemind/reviews/{review_id}/output-{model_id_safe}-r{round}-i{model_idx}.txt` (model id with `/` and `:` replaced by `_`; `model_idx` is the 0-based instance index of the reviewer call within the round, always suffixed so duplicate-instance reviewers don't overwrite each other)
- TRACE-level firehose (provider request/response, internal state): `~/.hyvemind/debug/reviews/{review_id}.jsonl`
- SQLite job records: `~/.hyvemind/hivemind/`

The event log no longer inlines the full `output` string. `model_call_completed` events carry `output_file` (relative path under `~/.hyvemind/`) and `output_len` instead — `cat` the referenced file for the full text.

### Commands

```bash
# Read the high-level event log (pretty-printed)
cat ~/.hyvemind/reviews/hmr-a1b2c3d4.jsonl | python3 -m json.tool

# Read the TRACE-level firehose for the same review
cat ~/.hyvemind/debug/reviews/hmr-a1b2c3d4.jsonl | python3 -m json.tool

# List all review event logs sorted by most recent
ls -lt ~/.hyvemind/reviews/

# List per-model output captures for one review
ls -lt ~/.hyvemind/reviews/hmr-a1b2c3d4/

# Summarize review timeline
python3 -c "
import json, sys
with open(sys.argv[1]) as f:
    for line in f:
        obj = json.loads(line.strip())
        ts = obj.get('timestamp','')[:19]
        evt = obj.get('event','')
        d = obj.get('data',{})
        keys = ['model_id','provider','round','session_id','error']
        info = {k: d[k] for k in keys if k in d}
        print(f'{ts} [{evt}] {info}')
" ~/.hyvemind/reviews/hmr-a1b2c3d4.jsonl

# Extract model outputs from a review (cat the capture files)
python3 -c "
import json, sys, os
home = os.path.expanduser('~/.hyvemind')
with open(sys.argv[1]) as f:
    for line in f:
        obj = json.loads(line.strip())
        if obj.get('event') == 'model_call_completed':
            d = obj['data']
            m = d.get('model_id','?')
            tok = f\"{d.get('input_tokens',0)} in / {d.get('output_tokens',0)} out\"
            dur = f\"{d.get('duration_ms',0)}ms\"
            print(f'\\n--- {m} ({tok}, {dur}) ---')
            of = d.get('output_file')
            if of:
                with open(os.path.join(home, of)) as outf:
                    print(outf.read()[:500])
" ~/.hyvemind/reviews/hmr-a1b2c3d4.jsonl

# Check for errors
grep -i 'failed\|error' ~/.hyvemind/reviews/hmr-a1b2c3d4.jsonl | python3 -m json.tool

# Find a review by partial ID
find ~/.hyvemind/reviews/ -name "hmr-a1b2*"
```

### Review log events

| Event | When | Key Data |
|-------|------|----------|
| `context_started` | Context Pi session created | session_id |
| `context_completed` | Context gathered | enriched prompt length |
| `engine_started` | Backend begins model dispatch | models, stance, timeout |
| `round_started` | Round begins | round number, model count |
| `model_call_started` | Model dispatched | model_id, provider |
| `model_call_completed` | Model responds | `output_file` (relative path, suffixed `-i{model_idx}`), `output_len`, `model_idx`, tokens, cost, duration |
| `model_call_failed` | Model errors | error message |
| `round_completed` | All models done | response count |
| `merge_started` | Merge Pi session created | session_id, round |
| `merge_completed` | Merge produces updated plan | round |
| `review_completed` | All rounds done | total_rounds |

### Cross-referencing Pi sessions

Context and merge phases run as Pi sessions. The review log records their `session_id` values. To see the full Pi transcript:
```bash
cat ~/.hyvemind/chat-sessions/{SESSION_ID}.jsonl | python3 -m json.tool
```

## Investigating a Swarm — `progress_log.jsonl`

Each swarm writes an append-only JSONL log at `~/.hyvemind/swarms/{swarm_id}/progress_log.jsonl`. After audit 2.3 the first line is a `{"schema_version": 2}` header; logs written before 2.3 lack the header and are treated as `schema_version=1`. The replay reader (`ProgressReader::rebuild_state` and `rebuild_session_associations` in `state/progress.rs`) skips the header and tolerates a truncated tail.

### Progress event types (`ProgressEvent.event_type`)

| Event | When | Key Data |
|-------|------|----------|
| `swarm_started` | Queen begins execution | swarm-level only |
| `swarm_completed` / `swarm_failed` / `swarm_paused` / `swarm_resumed` | Swarm lifecycle transitions | swarm-level only |
| `feature_started` | Scout or Worker phase begins on a feature | `feature_id`, `run_id` |
| `feature_scouted` | Scout produced a plan | `feature_id`, `run_id` |
| `feature_implemented` | Worker phase completed (handoff parsed) | `feature_id`, `run_id` |
| `feature_validated` | Guard passed / feature reached `Completed` | `feature_id`, `run_id` |
| `feature_failed` | Feature terminal-failed | `feature_id`, `run_id` |
| `feature_skipped` | Cancelled or dependency unmet | `feature_id` |
| `nurse_intervention` | Nurse Steer/Restart/Diagnose | metadata.intervention_kind |
| `guard_validation` | Guard run started / produced result | `feature_id`, `run_id` (where known) |
| `hivemind_review_started/_completed/_skipped` | Optional Hivemind review on scout/queen plan | `feature_id`, metadata.hivemind_id |
| `discovered_issue` | Worker surfaced a non-blocking issue | `feature_id`, metadata.severity/description |
| `budget_exceeded` | Per-swarm or daily cap hit; queen pauses next batch | metadata.reason/scope/spend/cap |
| `pi_session_spawned` *(2.3)* | A Pi subprocess was spawned for a swarm role | metadata.session_id/role/feature_id/pid |
| `pi_session_killed` *(2.3)* | A previously-spawned Pi subprocess was killed | metadata.session_id/reason |
| `worker_handoff` *(2.3)* | Worker emitted its structured handoff JSON | `feature_id`, `run_id`, metadata.success_state |
| `guard_attempt` *(2.3)* | Guard started a new validation attempt | `feature_id`, `run_id` (where known), metadata.attempt/milestone_id |
| `heartbeat_tick` *(2.3)* | Per-feature liveness pulse (every 30s while in flight) | `feature_id`, `run_id` |
| `error` | Generic error event | metadata may carry details |

Every event now carries an optional `run_id` (audit 2.3) so retries on the same feature are distinguishable. The Queen mints one `run_id` per `run_feature_full` invocation; Scout / Worker / inline Guard / validator Guard for the same attempt all share it.

### Commands

```bash
# Pretty-print the full progress log
cat ~/.hyvemind/swarms/{SWARM_ID}/progress_log.jsonl | python3 -m json.tool --no-ensure-ascii

# Detect schema version (header line on row 1)
head -1 ~/.hyvemind/swarms/{SWARM_ID}/progress_log.jsonl | python3 -m json.tool

# Filter to just one feature's run history (latest run_id wins)
python3 -c "
import json, sys
target = sys.argv[2]
for line in open(sys.argv[1]):
    try: e = json.loads(line)
    except: continue
    if e.get('feature_id') == target:
        ts = e.get('timestamp','')[:19]
        rid = e.get('run_id','-')
        et = e.get('event_type','')
        print(f'{ts} {rid} {et} {e.get(\"message\",\"\")[:80]}')
" ~/.hyvemind/swarms/{SWARM_ID}/progress_log.jsonl {FEATURE_ID}

# Count heartbeats per feature (a feature with zero heartbeats finished before the timer fired)
grep '"event_type":"heartbeat_tick"' ~/.hyvemind/swarms/{SWARM_ID}/progress_log.jsonl | \
  python3 -c "
import json, sys, collections
c = collections.Counter()
for line in sys.stdin:
    try: c[json.loads(line).get('feature_id','-')] += 1
    except: pass
for k,v in c.most_common(): print(f'{v:4d} {k}')
"

# Find Pi sessions that were spawned but never killed (crash victims)
python3 -c "
import json, sys
spawned, killed = {}, set()
for line in open(sys.argv[1]):
    try: e = json.loads(line)
    except: continue
    md = e.get('metadata') or {}
    if e.get('event_type') == 'pi_session_spawned':
        spawned[md.get('session_id','?')] = (e.get('feature_id'), md.get('pid'), md.get('role'))
    elif e.get('event_type') == 'pi_session_killed':
        killed.add(md.get('session_id','?'))
orphan = [(s,*v) for s,v in spawned.items() if s not in killed]
for s,fid,pid,role in orphan: print(f'orphan session={s} role={role} pid={pid} feature={fid}')
" ~/.hyvemind/swarms/{SWARM_ID}/progress_log.jsonl
```

### Sibling log: `activity_log.jsonl`

Alongside `progress_log.jsonl` each swarm also writes `~/.hyvemind/swarms/{swarm_id}/activity_log.jsonl` — the firehose feed consumed by the `swarm-activity` Tauri event after the 50ms/256-byte text/thinking coalescer in `commands/swarms.rs`. Every line is one IPC payload (`SwarmActivityEvent` in `app/src/lib/events.ts`) augmented with a monotonic per-swarm `seq` field. Schema-versioned via a `{"schema_version": 1}` header on the first line; truncated tails are tolerated. The frontend pages through it via `get_swarm_activity_log(swarm_id, after_seq, limit)` on mount so SwarmControl can replay history even if the user opens the panel after an agent has already streamed events.

```bash
# Pretty-print the full activity log
cat ~/.hyvemind/swarms/{SWARM_ID}/activity_log.jsonl | python3 -m json.tool --no-ensure-ascii
```

## Tauri Commands (IPC)

All registered in `lib.rs` via `invoke_handler!`. Every command returns `Result<T, IpcError>` (see `state/ipc_error.rs` + `app/src/lib/ipc.ts`). The lib.rs registration is the **authoritative list** — when adding or removing one, update this section.

Counts are approximate (114 production + 1 debug-only); the per-handler list under each bucket is exhaustive.

| Bucket | Module | Count | Commands |
|--------|--------|------:|----------|
| Tasks (internal: chat) | `commands/chat.rs` | 8 | `send_message`, `stop_chat`, `get_chat_history`, `prewarm_pi_session`, `list_chat_sessions`, `delete_chat_session`, `is_session_busy`, `get_session_last_assistant_text` |
| Tasks (UI state) | `commands/tasks.rs` | 6 | `save_task_messages`, `load_task_messages`, `delete_task_messages`, `get_task_state`, `auto_commit_task`, `list_project_files` |
| Hivemind | `commands/hivemind.rs` | 18 + 1 dbg | `start_review`, `get_review_status`, `list_reviews`, `create_hivemind`, `list_hiveminds`, `update_hivemind`, `delete_hivemind`, `get_review_step_outputs`, `get_review_state`, `log_review_event`, `get_review_plan`, `cancel_review`, `delete_review`, `save_round_verdicts`, `list_round_verdicts`, `read_merge_output`, `register_context_session`, `get_orchestrator_usage`, `get_resumable_review_for_task` (+ `clear_response_cache` in debug builds only) |
| Swarms | `commands/swarms.rs` | 15 | `create_swarm`, `update_swarm`, `start_swarm`, `pause_swarm`, `resume_swarm`, `stop_swarm`, `get_swarm`, `list_swarms`, `delete_swarm`, `get_swarm_progress`, `get_swarm_activity_log`, `get_swarm_features`, `get_swarm_milestones`, `get_swarm_usage`, `check_swarm_readiness` |
| Settings | `commands/settings.rs` | 30 | `get_settings`, `get_system_prompts`, `list_custom_prompts`, `save_custom_prompt`, `delete_custom_prompt`, `set_runtime_settings`, `set_default_model`, `set_default_hivemind`, `set_default_project_path`, `request_working_dir_approval`, `save_api_key`, `delete_api_key`, `get_providers`, `add_provider`, `test_provider_models`, `test_provider_chat`, `test_provider_pi`, `refresh_models`, `get_pi_status`, `update_pi`, `install_pi`, `open_pi_terminal`, `check_subscription_auth`, `set_auto_commit_tasks`, `set_auto_commit_conventional`, `set_task_completion_sound`, `set_crash_reporting`, `set_chat_check_in_secs`, `set_extension_poll_interval_secs`, `set_daily_budget` |
| Dashboard | `commands/dashboard.rs` | 5 | `get_dashboard_stats`, `get_model_usage`, `get_provider_usage`, `get_cost_summary`, `get_recent_activity` |
| Extensions | `commands/extensions.rs` | 4 | `list_extensions`, `get_usage_snapshots`, `refresh_usage_snapshot`, `update_extension_settings` |
| Nurse | `commands/nurse.rs` | 17 | `get_nurse_status` (engine snapshot), `set_nurse_config` (per-profile fan-out via `clamp_stall_threshold`), `check_chat_session` (gates on live session state + fresh `PiSession::nurse_activity_count()` before synthetic Signal + `Dispatcher::handle_signal`, origin=Watchdog, 95s outer timeout), `get_nurse_engine_status`, `get_nurse_intervention_log`, `get_nurse_detector_stats`, `get_nurse_session_detail`, `record_nurse_intervention_feedback`, `nurse_manual_action`, `get_nurse_detector_schemas`, `get_nurse_decision_chain`, `get_nurse_decisions_for_session`, `get_nurse_signal_stream`, `get_nurse_capture`, `export_nurse_diagnostic_bundle`, `get_nurse_profile`, `set_nurse_profile` |
| Sessions | `commands/sessions.rs` | 5 | `list_active_pi_sessions`, `kill_pi_session`, `reconcile_active_sessions`, `pi_pool_stats`, `get_pi_session_stats` |
| Tests | `commands/tests.rs` | 7 | `run_stability_test`, `cancel_test_run`, `get_active_test_run`, `list_test_runs`, `get_test_run`, `get_stability_test_config`, `set_stability_test_config` |

For per-command signatures / return types / delegation targets, see `docs/ipc-reference.md`.

### Choosing the right `IpcError` variant

When raising an error from a command, pick the variant the frontend's `formatIpcError` and per-kind branching expect. `IpcError::from_provider_error` (`state/ipc_error.rs:205`) auto-classifies stringy provider errors by substring (`401` / `429` / `circuit breaker open`).

| Use this | When |
|---|---|
| `validation` | User-attributable: empty id, path traversal, oversized payload, malformed body, illegal model name |
| `not_found { resource, resource_id }` | A swarm/session/review/hivemind/extension lookup missed (renamed fields avoid colliding with the discriminator `kind`) |
| `not_approved` | Working-directory allowlist rejected the path, or subscription auth missing |
| `provider_unauthenticated` / `provider_rate_limited` / `circuit_breaker_open` | Lift from provider error string via `IpcError::from_provider_error` |
| `internal` | I/O error, logic bug, panic, upstream 5xx — `details.chain` carries the full `anyhow` chain |

Free-form `String` errors lift into `IpcError::Internal` via the blanket `From<String>`. Avoid relying on it — it loses the discriminator and breaks the frontend's branched handlers (e.g., `not_approved` → approval modal).

### Hivemind command gotchas

- `start_review`: the `stance` argument is **silently ignored** — hardcoded `Against`. `For` / `Neutral` exist in the schema but are not wired (PRODUCT.md §6).
- `start_review`: `round_number` is the 1-based cumulative round for the Tasks-flow driver; collapses to `round_offset = round_number - 1` so capture files (`merge-rN.txt`, `output-*-rN.txt`) don't overwrite earlier rounds. Preserve this offset when adding round-related fields.
- `delete_review`: refuses if any child job is `pending` / `running` / `round_*` — returns `validation` error `"Cannot delete a running review. Cancel it first."`. Call `cancel_review` first.
- `register_context_session` MUST be called when the orchestrator spawns a context-gather Pi session, or `get_orchestrator_usage` cannot attribute its tokens to the review.
- `clear_response_cache` is **debug-builds-only** (`#[cfg(debug_assertions)]` at `lib.rs:571`); never depend on it in production code paths.

## Tauri Events (backend → frontend)

Backend code emits via `app_handle.emit("…", payload)`; the frontend subscribes via `listen("…", cb)` (see §Frontend Architecture → Event listener index for the consumer side).

| Event | Emitted by | Payload purpose |
|-------|-----------|-----------------|
| `chat-event` | `commands/chat.rs` (via `PiEventBatcher`) | Streaming Tasks-view tokens + lifecycle (start / chunk / done / error) |
| `hivemind-progress` | `hivemind/engine.rs`, `hivemind/review_log.rs`, `lib.rs` startup sweep | Review round progress, model completions, lifecycle events (incl. `merge_interrupted` / `review_interrupted` from crash recovery) |
| `swarm-event` | `commands/swarms.rs` | Swarm lifecycle (feature status changes, completion/failure) |
| `swarm-activity` | `core/services.rs` (Pi forwarder) | Per-feature agent activity stream (Pi events, stdin/stdout) |
| `swarm_reconciled` | `lib.rs` setup hook | Fan-out for swarms the progress-log replay flagged as `Interrupted` — frontend uses it to render Resume badges immediately |
| `nurse-event` | `nurse/intervention.rs` (`DefaultApplier::apply` + `dispatch_synthesized`), `commands/nurse.rs` (`set_nurse_config` StatusUpdate) | Nurse status updates + intervention notifications |
| `test-progress` | `core/stability_test/runner.rs` | Stability-test phase, status, progress updates |
| `usage-snapshot-updated` | `extensions/poller.rs` | Provider usage/balance snapshots pushed by extension pollers |
| `default-model-changed` | `commands/settings.rs::set_default_model` | Default-model selection changed in Settings |
| `default-project-path-changed` | `commands/settings.rs::set_default_project_path` | Default project-path changed in Settings |
| `default-hivemind-changed` | `commands/settings.rs::set_default_hivemind` | Default Hivemind selection changed in Settings |
| `pi-install-progress` | `commands/settings.rs::install_pi` | Pi install progress bar feed |
| `pi-update-progress` | `commands/settings.rs::update_pi` | Pi update progress bar feed |

## Environment Variables

### Credentials and logging

- `ANTHROPIC_API_KEY` — Anthropic API key (loaded at startup, overrides config file)
- `OPENAI_API_KEY` — OpenAI API key
- `OPENROUTER_API_KEY` — OpenRouter API key
- `HYVEMIND_DEBUG=1` — Enable debug mode (TRACE-level structured JSON logs to disk)
- `RUST_LOG` — Override stderr log level (e.g. `RUST_LOG=debug`)
- `HYVEMIND_PI_MAX_PROCESSES` — Override the Pi pool ceiling at startup. Default `30` (see `tunables::pi_max_processes()`). Overrides `config.max_pi_processes` when set to a positive integer. The Tasks screen calls `prewarm_pi_session` on mount so the first message skips the 1-2s cold-start; the 10-min idle eviction reclaims an unused warm session.

### Tunables (defined in `src/tunables.rs`)

All values are read on every accessor call, so overrides take effect at any
process start without rebuilding. Invalid / unparseable values silently fall
back to the default.

- `HYVEMIND_PI_MAX_PROCESSES` — ceiling for concurrent Pi subprocesses (default `30`)
- `HYVEMIND_CONCURRENCY_CAP` — max concurrent model calls per Hivemind round (default `8`)
- `HYVEMIND_ROUND_TIMEOUT_SECS` — fallback per-round timeout when a `RoundConfig` doesn't supply one (default `300`)
- `HYVEMIND_DEFAULT_MAX_TOKENS` — default `max_tokens` for providers that need one (currently Anthropic; default `4096`)
- `HYVEMIND_RESPONSE_CACHE_SIZE` — max entries in the per-review response cache (default `1000`)
- `HYVEMIND_RESPONSE_CACHE_TTL_SECS` — per-entry TTL for the response cache (default `3600`)
- `HYVEMIND_SWARM_FEATURE_PARALLELISM` — upper clamp on a swarm's `max_concurrent_features` (default `6`)
- `HYVEMIND_DEBUG_LOG_RETENTION_DAYS` — debug log retention window in days (default `7`)
- `HYVEMIND_LOG_CHANNEL_CAPACITY` — bounded capacity for the async tracing log channel (default `4096`)
- `HYVEMIND_CIRCUIT_BREAKER_THRESHOLD` — consecutive failures before the per-provider circuit breaker opens (default `5`)
- `HYVEMIND_CIRCUIT_BREAKER_COOLDOWN_SECS` — Open → HalfOpen cooldown for the breaker (default `60`)
- `HYVEMIND_PROVIDER_TIMEOUT_SECS` — default HTTP request timeout for Anthropic / OpenAI-compatible providers (default `120`)

### Nurse internal constants (not env-tunable)

Compile-time tuning knobs in `nurse/dispatcher.rs` / `nurse/intervention.rs`. Not user-tunable via `HYVEMIND_*` env vars — change in code only.

- `SELF_KILL_GRACE = 30s` (`nurse/dispatcher.rs`) — suppression window after a self-initiated kill, so the dispatcher doesn't immediately re-escalate on the resulting `SessionEnded`/`ProcessHealth` signal storm.
- `kill_with_verification` timeline (`nurse/intervention.rs`): `abort → 3s grace poll → kill_session → 7s post-kill poll → dead_at | double_fail_giving_up`. `double_fail_giving_up` is the safety circuit when both signals fail to land.
- Per-detector intervention budget (`nurse/budget.rs`) replaces v1's flat `max_interventions = 3` ceiling. Per-profile defaults via `ProfileConfig::default_for(profile)`; see `BudgetState` in §Key Types.
- `super_watchdog` (`util/supervise.rs`) wraps `NurseEngine::run_loop` via `nurse/supervisor.rs`; respawns the inner loop exactly once on panic before logging `"nurse unrecoverable"`. See `super_watchdog` row in §Key Types.

Most other Nurse timings (Tier 3 classifier timeout, bus capacity, tick interval, observability caps) are environment-tunable via `HYVEMIND_NURSE_*` knobs — see CLAUDE.md §Tunables for the full list.

## Debug Mode (checking logs)

When the user says "check logs", this means examine the debug log files for errors, warnings, or unexpected behavior.

### Enabling debug mode

```bash
HYVEMIND_DEBUG=1 cargo run          # from app/src-tauri
```

### Log locations

Debug logs are routed per-ID by `state/log_routing.rs`. Events fire inside `#[tracing::instrument]` spans that carry an ID field, and the layer writes each event to the matching bucket:

| Path | Contains |
|------|----------|
| `~/.hyvemind/debug/sessions/{session_id}.jsonl` | All TRACE/DEBUG/INFO/WARN/ERROR for one Tasks-view session (Pi rpc, chat command, session lifecycle) |
| `~/.hyvemind/debug/reviews/{review_id}.jsonl` | All events for one Hivemind review (engine, providers, merge) |
| `~/.hyvemind/debug/swarms/{swarm_id}/swarm.jsonl` | Swarm orchestration (Queen loop, scheduler, top-level Nurse) |
| `~/.hyvemind/debug/swarms/{swarm_id}/{agent}-{feature_id}-{run_id}.jsonl` | One agent run on one feature (e.g. `worker-feat-001-run-7.jsonl`); missing pieces collapse to `worker.jsonl` / `worker-feat-001.jsonl` |
| `~/.hyvemind/debug/general.jsonl.YYYY-MM-DD` | Startup, library logs, anything that fires outside a known span (daily rotation) |

Routing priority (most specific wins): `review_id` > `swarm_id`+`agent` > `swarm_id` > `session_id` > `general`. Files older than 7 days are pruned on startup. Stderr stays at INFO. **No debug files exist unless `HYVEMIND_DEBUG=1` was set when the event fired.**

### How to check logs

```bash
# Inspect a specific session end-to-end
cat ~/.hyvemind/debug/sessions/{session_id}.jsonl | python3 -m json.tool

# Inspect a specific hivemind review end-to-end
cat ~/.hyvemind/debug/reviews/{review_id}.jsonl | python3 -m json.tool

# Inspect one swarm's full activity
ls ~/.hyvemind/debug/swarms/{swarm_id}/
cat ~/.hyvemind/debug/swarms/{swarm_id}/*.jsonl | python3 -m json.tool

# Pull just one Worker's transcript for one feature
cat ~/.hyvemind/debug/swarms/{swarm_id}/worker-{feature_id}-{run_id}.jsonl | python3 -m json.tool

# General / uncategorized events (startup, library messages)
tail -20 ~/.hyvemind/debug/general.jsonl.$(date +%Y-%m-%d) | python3 -m json.tool

# Errors across all sessions today
grep -r '"level":"ERROR"' ~/.hyvemind/debug/sessions/ ~/.hyvemind/debug/reviews/ ~/.hyvemind/debug/swarms/ | python3 -m json.tool

# All Pi subprocess stdin/stdout for a specific session (truncated previews at TRACE)
grep 'pi stdin\|pi stdout' ~/.hyvemind/debug/sessions/{session_id}.jsonl | python3 -m json.tool

# LLM provider request/response payloads for a specific review
grep 'provider request\|provider response\|anthropic request\|anthropic response' ~/.hyvemind/debug/reviews/{review_id}.jsonl | python3 -m json.tool

# Nurse interventions on a specific swarm
grep 'nurse' ~/.hyvemind/debug/swarms/{swarm_id}/*.jsonl | python3 -m json.tool

# Circuit breaker state changes (usually in a review or session bucket)
grep -r 'circuit' ~/.hyvemind/debug/ | python3 -m json.tool
```

### What's logged at each level

| Level | What | Where |
|-------|------|-------|
| `ERROR` | Failures (crashed processes, failed API calls, panics) | stderr + debug file |
| `WARN` | Stall detections, circuit breaker trips, timeouts, retries | stderr + debug file |
| `INFO` | Command invocations, session lifecycle, high-level status | stderr + debug file |
| `DEBUG` | Prompt/response summaries (lengths, previews), scheduler state, session counts, config details | debug file only |
| `TRACE` | Raw Pi JSONL lines (truncated to 500-char preview + length), full Pi RPC command bodies, atomic writes, circuit breaker internal state | debug file only |

Full LLM responses are NOT inlined in the JSONL anymore. For hivemind reviews they live in per-call capture files (`~/.hyvemind/reviews/{review_id}/output-{model}-r{round}.txt`); for Tasks-view sessions they live in `~/.hyvemind/chat-sessions/{session_id}.jsonl` (written by Pi itself).

### Log rotation

- Per-ID files (`sessions/`, `reviews/`, `swarms/`) accumulate as long as their entity exists; pruned by mtime after 7 days on startup
- `general.jsonl.YYYY-MM-DD` rotates daily and is pruned by date after 7 days
- Writes are non-blocking: events queue into a bounded `mpsc::sync_channel(4096)` consumed by a worker thread that owns up to 64 open file handles (LRU eviction)
- Channel overflow drops the event silently — the runtime never stalls on log I/O

### rtk proxy warning

If using `rtk` (Rust Token Killer), its hook rewrites shell commands and **may suppress or filter output** from `ls`, `cat`, `tail`, `grep`, etc. When investigating log files, bypass rtk:

```bash
rtk proxy ls -la ~/.hyvemind/debug/        # explicit proxy passthrough
rtk proxy tail -50 ~/.hyvemind/debug/...   # or use rtk proxy prefix
```

### Frontend devtools

Open the Tauri webview console: **Cmd + Option + I** (macOS) or right-click → "Inspect Element". The console shows:
- Frontend IPC calls and errors
- Tauri event listener registrations
- React rendering issues
- Network requests (if any from the webview)

### Debugging a stuck Tasks-view session

When a Task spinner hangs without producing output:

1. **Check backend logs** — read the per-session file directly (no grep needed; everything in there is for that one session):
   ```bash
   rtk proxy tail -200 ~/.hyvemind/debug/sessions/{SESSION_ID}.jsonl | python3 -c "
   import json, sys
   for line in sys.stdin:
       try:
           obj = json.loads(line.strip())
           ts = obj.get('timestamp','')[:19]
           lvl = obj.get('level','')
           target = obj.get('target','')
           fields = obj.get('fields',{})
           msg = fields.get('message','')
           extra = ''
           for k in ('event_type','type'):
               if k in fields: extra += f' {k}={fields[k]}'
           print(f'{ts} [{lvl}] {target}: {msg}{extra}')
       except: pass
   "
   ```

2. **Interpret the results**:
   - Events flowing with `pi stdout: parsed event` → Pi subprocess is alive, model is responding (may just be slow)
   - `auto_retry_end` event → the model hit a rate limit or transient error and Pi retried
   - `pi process crashed` / `exit_code=1` → Pi subprocess died; check stderr in the log
   - No events after `sending prompt` → model API is unresponsive or network issue
   - `skipping unrecognized pi stdout line` → Pi emitted a line the parser doesn't handle (usually harmless)

3. **Check the session transcript directly**:
   ```bash
   # Find the most recent session file
   ls -lt ~/.hyvemind/chat-sessions/*.jsonl | head -5
   # Read the last events
   tail -20 ~/.hyvemind/chat-sessions/{SESSION_ID}.jsonl | python3 -m json.tool
   ```

4. **Common causes of stuck sessions**:
   - **Slow model** — some models (especially large ones or free-tier endpoints) can take 30-120s
   - **Rate limiting** — look for `auto_retry_end` events; the session is waiting to retry
   - **API key issue** — check Settings page; the provider should show "Configured" with a green dot
   - **Pi subprocess crash** — look for ERROR level logs with `pi process` or `exit_code`
   - **Frontend event listener missed** — open devtools console, check for JS errors

5. **Distinguish backend vs frontend issues**:
   - If backend logs show events flowing but the UI is stuck → frontend issue (check devtools console)
   - If backend logs stop after `sending prompt` → model/API issue
   - If backend logs show errors → backend issue

### Debugging a hivemind review

The hivemind review engine dispatches model calls through the `ProviderRegistry`. When a review returns stub/placeholder data or fails:

1. **Check the review log** (requires `HYVEMIND_DEBUG=1`):
   ```bash
   # High-level events
   cat ~/.hyvemind/reviews/{REVIEW_ID}.jsonl | python3 -m json.tool
   # Full TRACE firehose for the same review
   cat ~/.hyvemind/debug/reviews/{REVIEW_ID}.jsonl | python3 -m json.tool
   # A specific model's full output
   cat ~/.hyvemind/reviews/{REVIEW_ID}/output-{model_id_safe}-r{round}-i{model_idx}.txt
   ```

2. **Common issues**:
   - `provider 'X' not found in registry` → API key not configured for that provider
   - `duration_ms: 0` with placeholder text → stub response (provider not wired up)
   - `circuit breaker open` → too many consecutive failures for that provider
   - All models show identical token counts → responses are cached or stubbed

3. **Verify the provider registry loaded correctly** — startup logs land in the daily general bucket:
   ```bash
   grep 'provider registry refreshed' ~/.hyvemind/debug/general.jsonl.$(date +%Y-%m-%d) | python3 -m json.tool
   ```
