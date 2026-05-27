# `core/` — Swarm Execution Engine

The autonomous multi-feature execution pipeline. Owns the bee-named agents
(Queen, Scout, Worker, Guard, Nurse), the dependency scheduler, the
shared swarm state types, and the supporting services that wrap them.

For per-file detail (key types, IPC surface, debug commands), see the
[`CLAUDE.md` Project Layout / Key Types sections](../../../../CLAUDE.md).
Don't duplicate that material here — link to it.

## Purpose

Given a goal + working directory + model settings, run the swarm loop:
Queen decomposes → Scouts plan → Workers implement → Guards validate →
Nurse watches every long-running agent for stalls. State is checkpointed
to disk via `state/`; long-running tasks are cancellable.

## Key files

| File | What it owns |
|------|--------------|
| `queen.rs` | Orchestrator. Decomposes the goal, runs the main loop, dispatches scheduler-ready features to Scouts/Workers. |
| `scout.rs` | Per-feature planner (read-only + analysis). Produces an implementation plan, complexity estimate, risk list. |
| `scout_review.rs` | Optional Hivemind review of a Scout's plan before Worker handoff. |
| `worker.rs` | Implementer. Takes plan + feature, writes code, emits a `WorkerHandoff` JSON block on completion. |
| `handoff.rs` | Worker-handoff parser. Delimited-JSON extraction with last-JSON-block fallback. |
| `guard.rs` | Validator. Runs milestone assertions; on failure, synthesizes a fix-feature and re-queues (hard cap 3 attempts). |
| `scheduler.rs` | Topological sort + cycle detection. `next_ready_batch()` returns features whose dependencies are satisfied. |
| `scheduler.rs` (cont.) | Bounded parallelism (default 3 features in flight). |
| `swarm.rs` | Canonical type definitions (`SwarmState`, `Feature`, `FeatureStatus`, `ModelSettings`, `Milestone`). |
| `swarm_context.rs` | Per-run context handed to agents (working directory, settings, registries). |
| `services.rs` | Service composition for the swarm runtime (Pi pool, store, progress log, registries). |
| `budget.rs` | Token / call budget tracking per swarm. |
| `readiness.rs` | Pre-flight readiness manifest (cargo crates, npm packages, system binaries, APIs). |
| `validation.rs` | Shared validation primitives for milestone assertions. |
| `stability_test.rs` | Stability-test scaffolding for long-running swarm loops. |

## Contracts

- **Cancellation**: every long-running agent loop honors a
  `tokio_util::sync::CancellationToken`. Pause / resume / stop in the UI
  trips this token cooperatively — agents never hang on a pending API call.
- **Stall detection**: agents bump an `AtomicU64` event counter each time
  they receive a Pi event. Nurse compares-and-swaps on that counter to
  detect "no progress for N seconds" without holding a lock.
- **Fix-feature cap**: a feature's `fix_attempt_count` may not exceed
  `max_fix_attempts` (default 3). Guard enforces this hard cap.
- **Worker handoff**: Workers MUST call the `submit_handoff` Pi extension
  tool with the `WorkerHandoff` payload. The Rust backend reads the args
  directly off `tool_execution_start`; there is no transcript fallback.

## Where things live at runtime

- State files: `~/.hyvemind/swarms/{swarm_id}/` (atomic writes via
  `state::store::SwarmStore`).
- Progress log: `~/.hyvemind/swarms/{swarm_id}/progress_log.jsonl`
  (append-only, crash-safe replay target).
- Per-swarm debug logs: `~/.hyvemind/debug/swarms/{swarm_id}/...` (only
  when `HYVEMIND_DEBUG=1`).

## See also

- [`../../../../CLAUDE.md`](../../../../CLAUDE.md) — full per-file
  reference, IPC surface, debug-log routing, investigation commands.
- [`../../../../PRODUCT.md` §4](../../../../PRODUCT.md) — bee-colony agent
  roles and design intent.
- [`../../prompts/`](../../prompts/) — system prompts for each agent
  (`queen_system.md`, `scout_system.md`, `worker_system.md`,
  `guard_system.md`, `nurse_system.md`).
- [`../state/README.md`](../state/README.md) — persistence layer used by
  the swarm engine.
- [`../pi/README.md`](../pi/README.md) — Pi subprocess pool the agents
  run on top of.
