# Nurse — push-mode engine + three-tier dispatcher

The Nurse engine is the sole supervisor for every long-running Pi session in Hyvemind — Tasks-view conversations, swarm Workers/Scouts/Guards, Hivemind context-gather + merge phases. Producers push activity onto a `NurseBus` and a single engine subscribes, runs a registry of detectors per session, hands raised signals to the dispatcher, and writes the full per-decision event chain to disk regardless of `HYVEMIND_DEBUG`. The goal is post-hoc diagnosability: when a 4-hour swarm misfires, the user can paste `~/.hyvemind/debug/nurse/` into Claude Code and reconstruct exactly what happened — every signal, every gate, every tier decision, the classifier prompt + response, the kill-verification timeline, and the downstream outcome.

## Module map

```
nurse/
  mod.rs                       Module root + re-exports
  bus.rs                       NurseBus (broadcast<Arc<NurseBusEvent>>) + lifecycle event enum
  engine.rs                    NurseEngine — subscribe loop, per-session detector dispatch, signal handoff
  dispatcher.rs                Three-tier dispatch pipeline (Tier 1 deterministic / Tier 2 playbook /
                               Tier 3 LLM classifier) + InFlightGuard + tier1_lookup + EventSeq +
                               Watchdog fast-paths
  health.rs                    SessionHealth, Signal, Severity, Tier, EscalationState
  detector.rs                  Detector trait, DetectorContext, DetectorRegistry, TickKind (Fast | Slow)
  detectors/
    stall.rs                   Time-based + post-prompt-silence detection
    reasoning_loop.rs          Siphash exact / compression / minhash paraphrase loop checks
    tool_failure.rs            Repeated tool failure clustering by signature
    process_health.rs          Pi subprocess liveness + stderr crash patterns
    provider_health.rs         CircuitBreaker / missing-provider signal sources
    context_saturation.rs      Pi context-window % threshold
    retry_exhaustion.rs        auto_retry_end clustering in a sliding window
  classifier.rs                LlmClassifier — Tier 3 wrapper around ProviderRegistry (90s timeout)
  intervention.rs              DefaultApplier (production ActionApplier) + KillableSession /
                               SessionKiller traits + kill_with_verification + cancel_hivemind_review
  intervention_writer.rs       Bounded mpsc → in-memory ring (drives the get_nurse_status IPC)
  playbook.rs                  Tier 2 templated steer table (dedup_key → canned message)
  storm_guard.rs               Per-(session_id, dedup_key) sliding-window guard (default 3 / 60s)
  budget.rs                    Per-detector + age-decay intervention budget (Clone, is_cooldown_elapsed)
  config.rs                    NurseConfig, NurseMode, NurseProfile, ProfileConfig, BudgetConfig
                               (with effective_model / effective_provider per-profile resolution)
  schema.rs                    Provider-native tools/tool_choice JSON for the Tier 3 classifier
  prompt.rs                    include_str!("../../prompts/nurse_system.md") accessor
  snapshot.rs                  Wire DTOs (NurseLifecyclePayload / NurseStatusSnapshot / NurseDecision)
  synthesized.rs               SynthesizedKind, InterventionOwner, describe_synthesized table
  supervisor.rs                Thin wrapper over util::supervise::super_watchdog
  observability/
    mod.rs                     ObservabilityHandles, prune_on_startup
    writer.rs                  JsonlWriter — bounded mpsc, non-blocking
    decision_log.rs            DecisionLogger (daily-rotated decisions.jsonl.YYYY-MM-DD)
    capture.rs                 ClassifierCapture (per-decision prompt/response files, 1 MiB cap)
    signal_stream.rs           SignalStream (per-session JSONL with rotation)
    bus_telemetry.rs           BusTelemetry (bus.jsonl — lag, capacity_pressure, lifecycle)
```

Per-file responsibilities are intentionally narrow — every file is small enough to understand in one read.

## Wiring order

`NurseEngine` requires the dispatcher to be attached before it can start. `lib.rs::setup` does this in order:

1. `NurseEngine::new(...)` — constructs the engine with empty `OnceCell`s for the app handle and dispatcher.
2. `engine.attach_app_handle(app.handle())` — frontend emit path is now live.
3. Construct `LlmClassifier`, `DefaultApplier`, and `Dispatcher` (the dispatcher takes a `Weak<NurseEngine>` so it can read snapshots without owning the engine).
4. `engine.attach_dispatcher(Arc::new(dispatcher))` — `OnceCell::set` is single-shot.
5. `engine.start()` — returns `Err` if any required `OnceCell` is empty. Spawns the subscribe loop + slow-probe task + maintenance loop, all wrapped in `super_watchdog`.

## Push bus contract

Producers (`commands/chat.rs`, `core/queen.rs` and friends, `hivemind/engine.rs`) call `bus.publish(NurseBusEvent::…)` whenever something happens on a Pi session. The engine subscribes once and dispatches per session.

| Event | Producer | Triggers |
|---|---|---|
| `SessionSpawned { session_id, owner }` | `PiManager` on `spawn_session` | Detector context creation; `bus.jsonl` lifecycle row |
| `SessionEnded { session_id }` | `PiManager` on `kill_session` / natural exit | Detector context teardown; 24h timer to prune signal stream |
| `OwnerChanged { session_id, owner }` | Swarm role transitions (rare) | Profile re-derivation via `NurseProfile::for_owner` |
| `Event { session_id, kind, data }` | `touch_activity`-style calls from every producer | Drives `SessionHealth.event_counter` and feeds the per-detector tick |

`Event.kind` is a discriminator: `text_delta`, `thinking_delta`, `tool_start`, `tool_end`, `auto_retry_end`, `circuit_breaker_open`, etc. Detectors interpret only the kinds they care about; unknown kinds are dropped.

**Backpressure**: the bus is a `tokio::sync::broadcast` with capacity `HYVEMIND_NURSE_BUS_CAPACITY` (default 4096). `RecvError::Lagged(n)` is logged to `bus.jsonl` and the engine enters post-lag suppression for affected sessions — Tier 2 and Tier 3 are skipped (Tier 1 deterministic actions still fire) until the post-lag window expires. `capacity_pressure` events fire (sampled 1/s) when the buffer crosses 80% fill.

## Detector contract

Every detector implements the `Detector` trait:

```rust
trait Detector {
    fn name(&self) -> &'static str;
    fn description(&self) -> &'static str;
    fn config_schema(&self) -> Vec<TunableDef>;
    fn tick_kind(&self) -> TickKind;                       // Fast | Slow
    fn tick(&mut self, ctx: &DetectorContext, h: &mut SessionHealth) -> Vec<SignalDelta>;
}
```

**Lint-enforced invariant** — every `Signal` raised by a detector must populate `evidence` with everything a reader needs to second-guess the raise. The trait's doc comment carries the rule verbatim: *"if a future Claude Code session looking at this row 30 days from now would have to look at source code to understand why this signal raised, the evidence field is incomplete."* The integration test in `detector.rs` walks every built-in detector and fails if its raises produce empty evidence.

**Fast vs Slow ticks** — `tick_kind() == Fast` detectors run on every engine tick (default 10s, `HYVEMIND_NURSE_TICK_INTERVAL_SECS`). `Slow` detectors (process-health stderr scans, reasoning-loop minhash) run on a separate task at `HYVEMIND_NURSE_SLOW_PROBE_INTERVAL_SECS`. This split is what keeps the engine loop unblocked when a classifier call or a stderr ring scan takes time.

**SignalDelta** — `Raise { Signal }` or `Clear { dedup_key }`. Every delta is persisted to `signals/{session_id}.jsonl` before the engine pipeline runs, so signal history survives an engine panic.

## Three-tier dispatch pipeline

A decision is born the moment a `SignalDelta::Raise` would change a session's `Tier`, or the moment `report_error` / `report_synthesized` enters the engine. Each decision is keyed by a `decision_id` (uuid4 simple). `Dispatcher::handle_signal` is the single entry point.

| Tier | Spends tokens? | Bypasses storm guard? | Implementation |
|---|---|---|---|
| **Tier 1** — Deterministic | No | Yes (always fires) | Hardcoded `dedup_key` → action table in `dispatcher.rs::tier1_lookup` (`process_dead` / `crash_pattern` / `session_gone_unobserved` / `no_providers_configured` / `synthesized:process_crashed` / `scheduler_deadlock:*`). |
| **Tier 2** — Templated playbook | No | No | `SteerPlaybook` table in `playbook.rs` — matches dedup_key substrings to canned messages. |
| **Tier 3** — LLM classifier | Yes | No | `LlmClassifier` wraps `ProviderRegistry`; 90s timeout. Skipped (`classifier_skipped_no_model`) when neither `ProfileConfig.nurse_model` (per-profile override) nor `NurseConfig.nurse_model` (engine-wide fallback) is set. |

LLM efficiency rules:

- `PiSession::nurse_activity_count()` is the watermark for model-backed Nurse checks. It increments only for Nurse-relevant Pi events and intentionally ignores `SessionStats` usage polls and `Heartbeat` keepalives.
- `check_chat_session` returns early without synthesizing a stalled signal when the session is gone, not busy, has no Nurse-relevant activity yet, or has no new Nurse-relevant activity since the previous admitted watchdog check.
- The batched reviewer includes only busy sessions whose `nurse_activity_count()` has advanced beyond `SessionState.last_batch_reviewed_activity_count`; the watermark is recorded before the provider call so a bad parse/provider failure does not re-spend tokens on the identical transcript every interval.

Gating happens **before** the tier runs: `storm_guard` → `budget_evaluated` → `inflight_guard`. Each gate emits a row to `decisions.jsonl` so a gated decision is just as visible as a dispatched one. The dispatcher also runs Watchdog fast-paths that can finalise a decision early as `fast_path_awaiting_model` or `fast_path_healthy_streaming` when a session is provably making progress; the corresponding `tier1_evaluated` row records the short-circuit.

**InFlightGuard** — `engine.in_flight: Arc<Mutex<HashMap<SessionId, DecisionId>>>` holds one decision per session at a time. The dispatcher takes ownership for the duration of the chain and clears the slot in a `Drop` guard so a panic mid-dispatch doesn't leak the slot. A signal that arrives while a decision is in flight is gated as `inflight_guard` rather than spawning a parallel decision.

**Self-kill grace** — after a `Restart` / `Cancel` self-kill, `SELF_KILL_GRACE = 30s` suppresses any further dispatch on the same session id so the new session has time to boot without being kill-storm'd by stale signals.

**Synthesized path (§D.7 — Hivemind pseudo sessions)** — Hivemind context-gather and model-call sites have no Pi session backing them, but they can still fail (provider error, circuit breaker open). `engine.report_error` / `engine.report_synthesized` enter `dispatch_synthesized`, which writes a 3-row decision-log chain (`decision_started` → `intervention_dispatched` → `decision_finalised{status:"dispatched_synthesized"}`) where `decision_id == intervention_id`. Reviewers / merge runs get cancelled via `cancel_hivemind_review` (moved verbatim into `intervention.rs`); the dispatcher will not attempt `kill_with_verification` against a pseudo session.

`report_synthesized` consults the same master `enabled` / `swarms_only` gates as `Dispatcher::handle_signal` before emitting anything. The gates are mirrored on the engine as `Arc<AtomicBool>` (`master_enabled` / `master_swarms_only`) so this sync, spawn-context-friendly path can read them without awaiting on the config RwLock; `commands::nurse::set_nurse_config` is the sole mutator and updates both the config and the mirrors. When gated, `report_synthesized` writes a symmetric `decision_started` + `decision_finalised{status: "gated_disabled" | "gated_swarms_only"}` pair (matching the dispatcher's gating pairs at `dispatcher.rs:520` and `:610`) instead of dispatching, and returns `None`. The `swarms_only` proxy treats an owner as non-swarm when it has neither `swarm_id` nor `feature_id` populated, matching how `synthesize_nurse_for_error` builds the owner for swarm-originating cases (both set together).

**Adding a `SynthesizedKind` variant** (non-Pi error cases — CircuitBreakerOpen, ProtocolViolation, SchedulerDeadlock, SteerFailed) requires extending the `describe_synthesized` table in `synthesized.rs` — there is no implicit default and the compiler enforces exhaustiveness.

## Intervention application

`DefaultApplier` in `intervention.rs` is the production `ActionApplier`. It routes by owner kind:

| Owner | Cancel | Steer / Restart |
|---|---|---|
| `Review` / `Merge` | `cancel_hivemind_review` first, then best-effort live-Pi kill | (No live Pi behind Hivemind context-gather — Steer is a no-op for these owners.) |
| `Chat` / `Swarm` (Scout/Worker/Guard) | `mark_self_killed` + `kill_with_verification` | `KillableSession::steer(message)` on the live `PiSession` |

`kill_with_verification`: abort → 3s grace poll → `kill_session` → 7s post-kill poll → `dead_at` row OR `double_fail_giving_up` (no retry — the safety circuit prevents an interrupted budget from being eaten by repeat Cancel attempts). `KillableSession` + `SessionKiller` are traits so test code can swap in fakes; production wires them to `PiSession` + `PiManager`.

## Lock ordering & async discipline

- **`config` → `sessions`** — `engine.config: tokio::RwLock` is acquired and released BEFORE `engine.sessions: std::sync::RwLock`. The dispatcher enforces this by snapshotting `nurse_cfg` before each sessions-guarded block.
- **Never hold `engine.sessions` across `.await`** — detector ticks are sync; Tier 3 classifier calls happen *after* the guard drops, with a `SessionHealth` snapshot cloned out.
- **Never call `PiManager` or dispatch interventions under the `sessions` guard.** The dispatcher reads everything it needs in one snapshot, drops the guard, then does I/O.
- **Storm guard is keyed per-(session_id, dedup_key)**, not globally per error kind. The same kind on two sessions doesn't suppress each other; the same kind on the same session does. Tier 1 bypasses storm guard.
- **`engine.in_flight` is taken under its own `Mutex`** — held only long enough to insert/check/remove. Never held across `.await`.
- **The dispatcher holds a `Weak<NurseEngine>`** — it can read snapshots but never re-acquires `engine.sessions`.

These rules are restated in CLAUDE.md §Hidden invariants & lock ordering because detectors and dispatcher code are easy places to break them.

## Observability surfaces (always-on)

Written under `~/.hyvemind/debug/nurse/` regardless of `HYVEMIND_DEBUG`. Pruned by mtime on startup.

| File | Contents | Retention env var |
|---|---|---|
| `decisions.jsonl.YYYY-MM-DD` | Full per-decision event chain (10 event types) | `HYVEMIND_NURSE_DECISION_LOG_RETENTION_DAYS` (default 30) |
| `captures/{decision_id}-prompt.txt` / `-response.txt` | Tier 3 classifier verbatim prompt + response | Pruned alongside `decisions.jsonl` |
| `signals/{session_id}.jsonl` | Every raise / clear with full evidence | 24h after `SessionEnded` |
| `bus.jsonl.YYYY-MM-DD` | Bus lifecycle + lag + capacity_pressure | `HYVEMIND_NURSE_BUS_LOG_RETENTION_DAYS` (default 30) |

See CLAUDE.md §Investigating a Nurse decision for `jq` recipes.

**Writer discipline** — every observability writer is a bounded mpsc + worker task. Channel overflow drops the event silently and increments a counter exposed via `get_nurse_engine_status` (`HYVEMIND_NURSE_OBSERVABILITY_QUEUE_DEPTH` default 2048). The engine never stalls on disk I/O.

## Per-context tuning

`NurseConfig` carries master-level switches (enabled, mode, classifier model/provider, profile map). `ProfileConfig` carries everything that varies per context (per-detector tunables, `intervention_mode`, `budget`, `escalation_min_severity`, and optional per-profile `nurse_model` / `nurse_provider: Option<String>` that override the engine-wide classifier). `NurseConfig::effective_model(profile)` / `effective_provider(profile)` resolve the override-with-fallback. Five profiles: `Tasks` / `Swarm` / `Hivemind` / `Test` / `Default`. `NurseProfile::for_owner(&SessionOwner)` selects the profile from the owner kind.

Budget defaults (initial / per-hour / max / per-detector):

| Profile | Stall warn/stalled | escalation_min | Budget |
|---|---|---|---|
| Tasks | 120 / 180 | Stalled | 3 / 0 / 5 / 2 |
| Swarm | 180 / 300 | Warn | 5 / +1 / 10 / 4 |
| Hivemind | 240 / 600 | Stalled | 3 / 0 / 5 / 2 |
| Test | 180 / 300 | Warn | 5 / 0 / 5 / 3 |

Only Swarm decays — a 72-hour swarm legitimately produces many intervention-worthy moments; a Task is short-lived and a Hivemind context-gather runs for minutes. Decay is the differentiator that keeps the long-running case viable.
