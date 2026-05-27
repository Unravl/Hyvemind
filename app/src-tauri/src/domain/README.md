# `domain/` — Cross-Subsystem Type Definitions

Pure data types shared by `core` (swarm execution engine) and `state` (persistence, registry, progress log). Sits **below** both in the dependency graph: no `pi::*`, `state::*`, or `core::*` imports, no I/O. Every type is `Serialize + Deserialize` with only `chrono`, `serde`, `uuid`, and `std::sync` dependencies. Exists to break the historical `core` ↔ `state` cycle ([`mod.rs:1-15`](./mod.rs)): **add new types here whenever they're referenced by both `core` and `state`**. For a one-row index of every type see [`CLAUDE.md` §Key Types](../../../../CLAUDE.md); this README is the per-field, per-invariant, per-producer/consumer reference.

## Files

| File | Contents |
|------|----------|
| `mod.rs` | One re-export: `pub mod swarm;` |
| `swarm.rs` | Every public type below |

---

## `SwarmConfig`

[`swarm.rs:28-36`](./swarm.rs) — user-supplied input that creates a swarm. Built in the command handler, passed once to `SwarmState::from_config`, then dropped. **Not** persisted to disk on its own — only the derived `SwarmState`, features, and milestones are.

### Shape

| Field | Type | Purpose |
|-------|------|---------|
| `name` | `String` | Display name in the Swarms list. |
| `description` | `String` | Free-form goal text shown on the swarm card. |
| `working_directory` | `String` | Absolute path the swarm operates inside. Must already be approved via `request_working_dir_approval`. |
| `model_settings` | `ModelSettings` | Which models play each bee role + hivemind flags + budget. |
| `features` | `Vec<Feature>` | Initial features (may be empty — Queen decomposes). |
| `milestones` | `Vec<Milestone>` | Initial milestones (may be empty). |

### Invariants

- `working_directory` validity is enforced by the command handler ([`commands/swarms.rs:63-110`](../commands/swarms.rs)); the domain type doesn't check it.
- Every `Feature.dependencies` entry should resolve to a feature `id` in `features`. The `Scheduler` rejects cycles and dangling deps at construction time ([`core/scheduler.rs:209`](../core/scheduler.rs)).

### Lifecycle & flow

Created in `commands/swarms.rs` (`create_swarm` at [`commands/swarms.rs:104`](../commands/swarms.rs), plus `update_swarm` and clone paths). Passed by reference to `SwarmState::from_config` ([`swarm.rs:191`](./swarm.rs)), which is the only meaningful consumer.

---

## `SwarmState`

[`swarm.rs:169-220`](./swarm.rs) — authoritative runtime + on-disk snapshot of a swarm. Persisted to `~/.hyvemind/swarms/{id}/state.json` via atomic-rename writes.

### Shape

| Field | Type | Purpose |
|-------|------|---------|
| `id` | `String` | UUIDv4 minted in `from_config`. Stable for the swarm's lifetime; used as the directory key under `~/.hyvemind/swarms/`. |
| `name` | `String` | Cloned from `SwarmConfig.name`. |
| `status` | `SwarmStatus` | See state machine below. |
| `working_directory` | `String` | Cloned from `SwarmConfig.working_directory`. Never mutated post-creation. |
| `model_settings` | `ModelSettings` | Cloned from `SwarmConfig.model_settings`. May be updated by `update_swarm`. |
| `current_phase` | `String` | Human-readable phase tag: `"planning"`, `"implementing"`, `"finished"`. Set by the Queen as it transitions ([`core/queen.rs:522, 923`](../core/queen.rs)). Informational only — the source of truth for "is this swarm doing anything" is `status`. |
| `current_feature_index` | `usize` | Legacy cursor from the pre-scheduler era. Stays `0` for parallel-execution swarms; preserved for serialisation back-compat. |
| `created_at` | `DateTime<Utc>` | Set once in `from_config`. |
| `updated_at` | `DateTime<Utc>` | Touched by every `set_status` / `set_error` call. |
| `error` | `Option<String>` | Populated only by `set_error` (which also flips `status` to `Failed`). |
| `queen_plan_review_done` | `bool` | Latches `true` once the Queen-plan Hivemind review has been attempted (success or explicit skip). Prevents re-reviewing on every `start_swarm`; carries forward through clone / resume because the plan is unchanged. `#[serde(default)]` for back-compat. |

### Invariants

- `error.is_some()` ⇒ `status == Failed` (enforced by `set_error`, [`swarm.rs:215-219`](./swarm.rs)).
- `status == Failed` does **not** imply `error.is_some()` — the crash reconciler sets `Failed` on features without touching the swarm-level `error`.
- `updated_at >= created_at` always. Both `set_status` and `set_error` overwrite `updated_at` with `Utc::now()`.
- `id` is immutable after `from_config`; never reassign it.

### Lifecycle

1. **Created** by `SwarmState::from_config` in `commands/swarms.rs::create_swarm` ([`commands/swarms.rs:104`](../commands/swarms.rs)) with `status = Planning`.
2. **Persisted** by `SwarmStore::write_state` ([`state/store.rs:73`](../state/store.rs)) on every transition.
3. **Loaded** at process startup by `SwarmStore::read_state` ([`state/store.rs:257`](../state/store.rs)); the crash reconciler in `core/recovery.rs::reconcile_orphaned_swarms` may rewrite `status` to `Interrupted` ([`core/recovery.rs:81, 272, 290`](../core/recovery.rs)).
4. **Mutated** in-memory via `Arc<RwLock<SwarmState>>` owned by `RunningSwarm` ([`state/swarm_registry.rs:73`](../state/swarm_registry.rs)) for the duration of a Queen run.
5. **Deleted** by `SwarmStore::delete_swarm` ([`state/store.rs:271`](../state/store.rs)) when the user removes the swarm from the UI.

### State transitions on `status`

```
   create_swarm
        │
        ▼
   ┌─────────┐ start_swarm   ┌──────────────┐  loop exit
   │Planning ├──────────────►│ Implementing ├────────────► Completed
   └─────────┘               └──┬───┬───┬───┘                (terminal)
                  pause_swarm   │   │   │   stop_swarm
                                │   │   └────────────────► Cancelled
                                │   │   set_error              (terminal)
                                │   └────────────────────► Failed
                                ▼                              (terminal)
                          ┌────────┐  resume_swarm
                          │ Paused ├─────────► (back to Implementing)
                          └────────┘

   ════════════ host-crash boundary ════════════
   On restart, lib.rs setup runs the reconciler:
     Implementing | Failed-with-in-flight ──► Interrupted ──resume──► Implementing
   Paused is intentionally NOT rewritten — a user pause must survive a restart.
```

- `Planning → Implementing`: `start_swarm` ([`commands/swarms.rs:418`](../commands/swarms.rs), [`core/queen.rs:521`](../core/queen.rs)).
- `Implementing → Paused`: cooperative `pause_swarm` ([`state/swarm_registry.rs:197`](../state/swarm_registry.rs), [`core/queen.rs:701`](../core/queen.rs)).
- `Paused → Implementing`: `resume_swarm` ([`state/swarm_registry.rs:218`](../state/swarm_registry.rs)).
- `Implementing → Cancelled`: `stop_swarm` ([`core/queen.rs:572, 838`](../core/queen.rs)).
- `Implementing → Completed | Failed`: Queen loop exit ([`core/queen.rs:916-920`](../core/queen.rs)).
- `* → Failed` via `set_error` ([`swarm.rs:215`](./swarm.rs), [`core/queen.rs:755`](../core/queen.rs)).
- `Implementing | Failed → Interrupted`: crash reconciler ([`core/recovery.rs:81, 127, 272, 290`](../core/recovery.rs)).
- `Interrupted → Implementing`: `resume_swarm` after recovery ([`core/recovery.rs:411`](../core/recovery.rs)).

### Producers / Consumers

- **Mutators**: `SwarmState::set_status` and `set_error` ([`swarm.rs:209, 215`](./swarm.rs)) are the only allowed mutation paths. Direct field assignment to `status` / `error` bypasses the `updated_at` touch and should be avoided outside tests; the Queen sets `current_phase` directly because phase isn't a status transition ([`core/queen.rs:522, 923`](../core/queen.rs)).
- **Writers**: `SwarmStore::write_state` ([`state/store.rs:73`](../state/store.rs)) is the only persister.
- **Readers**: `SwarmStore::read_state` ([`state/store.rs:257`](../state/store.rs)), `state/swarm_registry.rs` for live UI snapshots, `commands/swarms.rs::list_swarms` / `get_swarm`.

---

## `SwarmStatus`

[`swarm.rs:39-71`](./swarm.rs)

`#[serde(rename_all = "snake_case")]` enum — serialises as
`"planning"`, `"implementing"`, etc. See the diagram above.

### Variants

| Variant | When it's set |
|---------|---------------|
| `Planning` | Initial value from `from_config`. The Queen has not yet started executing features. |
| `Implementing` | Queen is actively decomposing / scheduling / running features. |
| `Paused` | User clicked Pause; the Queen acknowledged the `CancellationToken`-mediated pause request and yielded. |
| `Interrupted` | Set **only** by the crash reconciler at startup ([`core/recovery.rs:81, 272`](../core/recovery.rs)) for swarms that were `Implementing` or `Failed`-with-in-flight-features when the host died. UI shows a Resume affordance. |
| `Completed` | All non-skipped features reached `FeatureStatus::Completed`. |
| `Failed` | Queen exited with one or more `FeatureStatus::Failed`, **or** `set_error` was called. |
| `Cancelled` | User issued `stop_swarm`. Distinct from `Failed`: cancellation is intentional. |

### Helpers

- `Display`: lowercase string matching the serde variant ([`swarm.rs:51-63`](./swarm.rs)).
- `is_resumable()`: `true` for `Paused` and `Interrupted` ([`swarm.rs:68-71`](./swarm.rs)). Used by the UI to gate Resume buttons.

### Invariants

- Once a swarm reaches `Completed`, `Failed`, or `Cancelled` it does **not** transition further at runtime. Terminal status is final unless the user clones the swarm or explicitly resumes from `Interrupted`.
- `Interrupted` is never the initial status of a fresh swarm — it can only be reached through the crash reconciliation path.

---

## `ModelSettings`

[`swarm.rs:223-298`](./swarm.rs)

Which models the swarm uses for each bee role plus the Hivemind / budget knobs. Cloned into `SwarmState` and persisted alongside it.

### Shape

| Field | Type | Purpose |
|-------|------|---------|
| `primary_model` | `String` | Worker (and Guard fallback) model id. |
| `scout_model` | `String` | Model used for Scout planning. |
| `guard_model` | `Option<String>` | Explicit Guard model; falls back to `primary_model` via `effective_guard_model` ([`swarm.rs:276`](./swarm.rs)). |
| `scout_thinking_level` | `String` | Default `"high"`. |
| `worker_thinking_level` | `String` | Default `"medium"`. |
| `guard_thinking_level` | `String` | Default `"medium"`. |
| `queen_thinking_level` | `String` | Default `"high"`. |
| `use_hivemind_on_scout` | `bool` | Run Hivemind review on each Scout's per-feature plan before handoff to a Worker. |
| `use_hivemind_on_queen` | `bool` | Run Hivemind review on the Queen's master decomposition plan. Latched per-plan by `SwarmState.queen_plan_review_done`. |
| `hivemind_id` | `Option<String>` | Which Hivemind team to use for the above two flags. If `None` or empty, the review is skipped with a logged reason ([`core/queen.rs:1248-1258`](../core/queen.rs)). |
| `max_concurrent_features` | `u32` | Default `1`, range `1..=6` (clamped by `HYVEMIND_SWARM_FEATURE_PARALLELISM`). Translates into the per-swarm `Semaphore` capacity at [`core/queen.rs:555`](../core/queen.rs). |
| `swarm_budget_usd` | `Option<f64>` | Lifetime spend cap. `None` = unlimited. When live cost meets/exceeds the cap, the Queen pauses between feature batches and emits `BudgetExceeded` ([`core/queen.rs:621, 660`](../core/queen.rs)). |

### Invariants

- `max_concurrent_features >= 1` (default ensures this; the NewSwarm UI clamps the upper bound).
- `effective_guard_model()` never returns an empty string assuming `primary_model` is non-empty.
- `hivemind_id == Some("")` is treated as "skip review", same as `None` ([`core/queen.rs:1195`](../core/queen.rs)).

### Lifecycle

Built once in `commands/swarms.rs::create_swarm`. Stored inside `SwarmConfig` then cloned into `SwarmState`. May be replaced via `update_swarm`.

### Producers / Consumers

- **Producers**: `commands/swarms.rs` (creation + update).
- **Consumers**: `core/queen.rs` (every agent dispatch reads it), `core/scout_review.rs` ([`core/scout_review.rs:15`](../core/scout_review.rs)), `state/store.rs` (round-trip serde).

### Back-compat fields

`#[serde(default)]` on `guard_model`, all `*_thinking_level`, `max_concurrent_features`, and `swarm_budget_usd` keeps `ModelSettings` deserialisable from any historical `state.json` (see tests at [`swarm.rs:651-690`](./swarm.rs)).

---

## `SwarmUsageSummary`

[`swarm.rs:75-90`](./swarm.rs)

Plain numeric aggregator. Used as both the DB-backed query result and the live in-memory snapshot.

### Shape

| Field | Type | Purpose |
|-------|------|---------|
| `input_tokens` | `i64` | Non-cached input. |
| `output_tokens` | `i64` | Model output. |
| `cache_read_tokens` | `i64` | Cache hits. Often dwarfs `input_tokens` on Anthropic / DeepSeek. **Added because the previous schema silently dropped this — the UI was showing ~10% of real usage.** |
| `cache_write_tokens` | `i64` | Cache writes (Anthropic `cache_creation_input_tokens`). Billed differently from regular input. |
| `cost` | `f64` | Dollars. |
| `duration_ms` | `i64` | Wall time spent in model calls. |

### Lifecycle

Constructed two ways:

1. As a snapshot of `SwarmUsageAccumulator` via `.snapshot()`.
2. As a SQL aggregate by `commands/swarms.rs::get_swarm_usage` ([`commands/swarms.rs:1838-1866`](../commands/swarms.rs)), which also adds the live accumulator snapshot so the UI sees combined DB + live totals.

Not persisted as a standalone artefact.

---

## `SwarmUsageAccumulator`

[`swarm.rs:92-166`](./swarm.rs)

`Arc<Mutex<SwarmUsageSummary>>` wrapper, `Clone`. Multiple agents update it concurrently.

### Methods

- `new()` / `default()` — zero-initialised.
- `add(input, output, cache_read, cache_write, cost, duration_ms)` — additive.
- `subtract(...)` — saturating subtract; `cost` is `max(0.0)`. Called after `record_session_usage` writes to the DB so the in-memory total doesn't double-count.
- `snapshot()` — clones the inner `SwarmUsageSummary`.

### Invariants

- All `lock()` calls follow the project-wide poison policy `unwrap_or_else(|e| e.into_inner())`. The protected data is additive counters; a panicked writer can at worst leave slightly under/over-reported totals, never an inconsistent structure. Module docstring ([`swarm.rs:10-21`](./swarm.rs)) is the canonical reference.
- `subtract` is saturating — accumulator values can never go negative.

### Lifecycle

Created lazily by `SwarmRegistry::register_usage_accumulator` ([`state/swarm_registry.rs:375`](../state/swarm_registry.rs)) when a swarm starts. Cleared by `remove_usage_accumulator` ([`state/swarm_registry.rs:389`](../state/swarm_registry.rs)) when the swarm is unregistered (`SwarmRegistry::unregister` at [`state/swarm_registry.rs:264`](../state/swarm_registry.rs)).

### Producers / Consumers

- **Writers**: every agent run path that records token usage. The accumulator is reached through `SwarmRegistry::get_usage_accumulator` ([`state/swarm_registry.rs:383`](../state/swarm_registry.rs)).
- **Readers**: `commands/swarms.rs::get_swarm_usage` ([`commands/swarms.rs:1838`](../commands/swarms.rs)).

---

## `Feature`

[`swarm.rs:301-373`](./swarm.rs)

A unit of work the swarm executes. Persisted to `~/.hyvemind/swarms/{id}/features.json`.

### Shape

| Field | Type | Purpose |
|-------|------|---------|
| `id` | `String` | Stable id used as the dependency-graph key. Validator features get a `validate-` prefix; Guard-spawned fix features get a `-fix-N` suffix. |
| `name` | `String` | Display name. |
| `description` | `String` | Prompt-ready body the Scout / Worker reads. |
| `status` | `FeatureStatus` | See state machine below. |
| `dependencies` | `Vec<String>` | Feature `id`s that must reach `Completed` before this one starts ([`core/scheduler.rs:78`](../core/scheduler.rs)). |
| `milestone` | `Option<String>` | Milestone `id` this feature belongs to. |
| `fix_attempt_count` | `u32` | Incremented each time Guard fails this feature and spawns a fix-feature. Bumped via `increment_fix_attempts` ([`swarm.rs:370`](./swarm.rs)). |
| `max_fix_attempts` | `u32` | Hard cap, default `3` ([`swarm.rs:355`](./swarm.rs)). |
| `fulfills` | `Vec<String>` | `VAL-*` assertion ids this feature is responsible for satisfying. Empty for impl features; populated on auto-injected validator features and Guard fix features. `#[serde(default)]` for back-compat. |
| `interrupted` | `bool` | Set by the crash reconciler when a feature was in an in-flight state (`Scouting`/`Implementing`/`Reviewing`/`Validating`) at process death ([`core/recovery.rs:369-372`](../core/recovery.rs)). Cleared by `resume_swarm`. `#[serde(default)]`. |
| `resumable` | `bool` | Set alongside `interrupted` when the reconciler judges the feature safe to re-queue. Drives the UI Resume badge. `#[serde(default)]`. |

### Helpers

- `new(id, name, description)` — default status `Pending`, `max_fix_attempts = 3`, all flags cleared.
- `is_validator()` — `id.starts_with("validate-")` ([`swarm.rs:365`](./swarm.rs)). Validator features skip Scout/Worker and run Guard directly.
- `increment_fix_attempts()` — bumps `fix_attempt_count`.

### Invariants

- `fix_attempt_count <= max_fix_attempts` is enforced by the Queen, which terminates with `Failed` once the cap is hit (see `run_feature_full` in `core/queen.rs`).
- `is_validator()` ⇒ `fulfills` is non-empty (validator features exist to validate assertions; an empty `fulfills` would be a synthesis bug).
- `interrupted == true` ⇒ `status == Failed` after reconciliation, because the reconciler promotes the feature to `Failed` before setting the flag ([`core/recovery.rs:366-382`](../core/recovery.rs)).
- `resumable` is only meaningful while `interrupted == true`; otherwise it's ignored by the UI.

### State transitions on `status`

```
   Pending ──► Scouting ──► [Reviewing] ──► Implementing ──► Validating
       ▲          │              │               │                │
       │          └──────────────┴───────────────┴──► Skipped     │
       │                                              (cancelled  ▼
       │                                               or dep ──► Completed | Failed
       │                                               failed)        (terminal)
       │
       └── resume_swarm clears interrupted/resumable and resets
           in-flight (Scouting/Reviewing/Implementing/Validating)
           back to Pending.
```

`Reviewing` is only entered when `model_settings.use_hivemind_on_scout` is set. On Guard failure the feature loops Validating → Implementing via a Guard-spawned fix feature (sharing the original `feature_id` + new `run_id`); `status` becomes `Failed` only once `fix_attempt_count == max_fix_attempts`. The exact path is driven by the Queen in `run_feature_full` ([`core/queen.rs:1050-1533`](../core/queen.rs)).

### Lifecycle

1. **Created** either as part of the initial `SwarmConfig.features`, or by the Queen's plan decomposition, or as a validator/fix feature synthesised mid-run.
2. **Persisted** by `SwarmStore::write_features` ([`state/store.rs:80`](../state/store.rs)) after every mutation.
3. **Loaded** by `SwarmStore::read_features` ([`state/store.rs:188`](../state/store.rs)) at startup or when the UI lists swarms.
4. **Replayed** by `ProgressReader::rebuild_state` (in `state/progress.rs`) — the JSONL progress log is the source of truth after a crash; `apply_replay_and_mark_interrupted` ([`core/recovery.rs:338`](../core/recovery.rs)) folds the log over the persisted features.

### Producers / Consumers

- **Producers**: `commands/swarms.rs` (initial), `core/queen.rs` (Queen-decomposed features), `core/guard.rs` (fix features), `core/scheduler.rs::inject_milestone_validators` (validator features).
- **Mutators**: Queen sets `status` directly ([`core/queen.rs:1058, 1278, 824, 1453, 1533`](../core/queen.rs)). The crash reconciler is the only other writer ([`core/recovery.rs:366-382`](../core/recovery.rs)).
- **Readers**: `Scheduler::next_ready_batch` ([`core/scheduler.rs:66`](../core/scheduler.rs)), `Worker::implement` ([`core/worker.rs:59, 226`](../core/worker.rs)), `Scout::scout_feature` ([`core/scout.rs:13`](../core/scout.rs)), `Guard::validate` ([`core/guard.rs:14`](../core/guard.rs)), `core/scout_review.rs` ([`core/scout_review.rs:15`](../core/scout_review.rs)).

---

## `FeatureStatus`

[`swarm.rs:376-412`](./swarm.rs)

`#[serde(rename_all = "snake_case")]` enum. See diagram above.

### Variants

| Variant | When it's set | Terminal? |
|---------|---------------|:---------:|
| `Pending` | Initial state for every new feature; also the state `resume_swarm` resets in-flight features back to. | no |
| `Scouting` | Queen has dispatched the Scout for this feature ([`core/queen.rs:1058`](../core/queen.rs)). | no |
| `Reviewing` | Scout-plan Hivemind review is in flight (only if `use_hivemind_on_scout` is set). | no |
| `Implementing` | Worker is writing code ([`core/queen.rs:1278`](../core/queen.rs)). | no |
| `Validating` | Guard is running milestone assertions. | no |
| `Completed` | Guard passed (or the feature has no milestone and the Worker handoff was successful). | **yes** |
| `Failed` | Guard failed after `max_fix_attempts`, or the Worker errored hard, or the crash reconciler promoted an in-flight feature. | **yes** |
| `Skipped` | Cancelled mid-run, or a dependency failed, or the user paused before the feature started ([`core/queen.rs:1050, 1270, 1576, 1588`](../core/queen.rs)). | **yes** |

### Helpers

- `Display`: lowercase string matching the serde variant.
- `is_terminal()`: `true` for `Completed | Failed | Skipped`. Used by the Queen loop to decide whether the whole swarm is done ([`core/queen.rs:912-913`](../core/queen.rs)) and by the reconciler to identify crash victims ([`core/recovery.rs:369`](../core/recovery.rs)).

### Invariants

- `is_terminal()` features are never re-dispatched by the scheduler ([`core/scheduler.rs:71`](../core/scheduler.rs)).
- The Queen never moves a feature backwards from a terminal state on the same run; resume after `Interrupted` is a fresh transition and goes through `Feature.interrupted`/`resumable` clearing first.

---

## `Milestone`

[`swarm.rs:415-426`](./swarm.rs)

Group of features with validation assertions Guard runs. Persisted to `~/.hyvemind/swarms/{id}/milestones.json`.

### Shape

| Field | Type | Purpose |
|-------|------|---------|
| `id` | `String` | Stable id referenced by `Feature.milestone` and validator-feature ids (`validate-{milestone_id}`). |
| `name` | `String` | Display name. |
| `features` | `Vec<String>` | Feature `id`s grouped under this milestone. |
| `assertions` | `Vec<String>` | `VAL-*` assertion strings Guard validates (passed via the validator feature's `fulfills`). |
| `sealed` | `bool` | Once a milestone validator passes, the scheduler refuses to inject additional features into it ([`core/scheduler.rs`](../core/scheduler.rs)). Defaults `false`; `#[serde(default)]` for back-compat. |

### Invariants

- `sealed == true` ⇒ no more fix features may be added that target this milestone.
- `assertions` is consumed by the auto-injected validator feature's `fulfills` field — keep them in sync if you mutate them.

### Lifecycle

Created from `SwarmConfig.milestones` (or by the Queen during decomposition). Persisted via `SwarmStore::write_milestones` ([`state/store.rs:88`](../state/store.rs)) and loaded via `read_milestones` ([`state/store.rs:201`](../state/store.rs)). Mutated only to flip `sealed` to `true`.

### Producers / Consumers

- **Producers**: `commands/swarms.rs` (initial), `core/queen.rs` (decomposition).
- **Consumers**: `core/scheduler.rs::inject_milestone_validators` ([`core/scheduler.rs:10`](../core/scheduler.rs)), `core/guard.rs` ([`core/guard.rs:14`](../core/guard.rs)), `core/validation.rs` ([`core/validation.rs:17`](../core/validation.rs)).

---

## Cross-subsystem invariants

Things any agent touching this module MUST preserve. The per-type sections above carry the in-type contracts; what follows is the cross-cutting ones that span types or layers.

1. **`domain/` imports nothing from `core::*`, `state::*`, or `pi::*`.** The module exists to break the historical `core` ↔ `state` cycle (`mod.rs:1-13`). Re-introducing such an import would re-create the cycle and force the workspace back into the rename-shuffle this directory was extracted to avoid.
2. **`SwarmState.updated_at` always advances on every mutation.** `set_status` and `set_error` enforce it; the Queen also touches it on the bare `state.updated_at = Utc::now()` write at [`core/queen.rs:860`](../core/queen.rs). The frontend `list_swarms` ordering relies on this — any new mutator must follow the same rule.
3. **`SwarmState.error.is_some()` ⇒ `status == Failed`** when set via `set_error`. The reverse does **not** hold (the reconciler can set `Failed` on features without populating swarm-level `error`).
4. **`Feature.status == Completed` is paired with a persisted `WorkerHandoff`** at `~/.hyvemind/swarms/{id}/handoffs/{feature_id}.json` — except for `validate-*` features, which have no Worker step. Marking a feature Completed without a handoff violates the swarm contract the Queen replay depends on.
5. **The crash reconciler is the ONLY writer of `SwarmStatus::Interrupted` and the ONLY setter of `Feature.interrupted = true && Feature.resumable = true`.** All three flags are cleared by `resume_swarm` via `reset_in_flight_features` (for `Paused`/`Interrupted` resumes) or `reset_features_for_full_resume` (for `Failed`/`Cancelled` resumes). Setting them anywhere else would corrupt the UI's Resume affordance and double-resume features that aren't actually crash victims. **Additional writer for `fix_attempt_count`**: `reset_features_for_full_resume` also clears `fix_attempt_count` to 0 on terminal-failed features when the user clicks Resume on a `Failed`/`Cancelled` swarm — deliberate UX so each retried feature gets a fresh Guard budget. The `Paused`/`Interrupted` path (`reset_in_flight_features`) preserves `fix_attempt_count`.
6. **`Feature::is_validator()` is keyed on the `validate-` id prefix.** Validator features skip Scout/Worker; renaming the prefix without updating the predicate ([`swarm.rs:365`](./swarm.rs)) would silently route validators through the wrong pipeline.
7. **Milestone sealing is forward-only.** `Milestone.sealed: true → false` is never valid. The scheduler treats sealed milestones as closed worlds ([`core/scheduler.rs:100-120`](../core/scheduler.rs)); un-sealing would corrupt the feature graph mid-run.
8. **Dependencies form a DAG.** Cycles are rejected at `Scheduler::new` *and* on every `add_features` / `add_features_respecting_seals` / `update_feature_deps` call ([`core/scheduler.rs`](../core/scheduler.rs)). Bypassing detection would hang the Queen loop forever.
9. **`max_fix_attempts` is the only retry cap on Guard loops.** Removing the check at [`core/queen.rs:1804`](../core/queen.rs) would let Guard spawn fix-features indefinitely on permanently-broken assertions.
10. **`queen_plan_review_done` is forward-only.** Once a Queen master-plan Hivemind review has been attempted, it is suppressed for the lifetime of the swarm (clones / resumes carry it forward). Resetting to `false` would re-bill the user for a review they already ran.
11. **Every `#[serde(default)]` is load-bearing.** Each one matches a real file format that exists on user disks. Removing one breaks back-compat for swarms persisted before that field existed; the tests at [`swarm.rs:461-690`](./swarm.rs) lock the canonical legacy shapes in.
12. **`SwarmUsageAccumulator` is sync, never `.await`-spanning.** It wraps a `std::sync::Mutex`. Holding the guard across an await is a bug; the API is synchronous by design — call it, drop the guard, then await elsewhere. The lock-poison policy (`unwrap_or_else(|e| e.into_inner())`, [`swarm.rs:11-21`](./swarm.rs)) is project-wide standard for additive counters and must be preserved.

## See also

- [`../core/README.md`](../core/README.md) — swarm execution engine that drives every transition above.
- [`../state/README.md`](../state/README.md) — `SwarmStore` + `SwarmRegistry` persistence and runtime registry.
- [`../../../../CLAUDE.md`](../../../../CLAUDE.md) §Key Types + §Investigating a Swarm — one-row index and progress-log event reference.
