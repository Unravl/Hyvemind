# `state/` — Persistence & App State

The central state graph (`AppState`), all on-disk persistence, the OS
keychain integration, the swarm progress log, the running-swarm
registry, and the structured-logging plumbing (redaction + per-ID
routing).

For per-file detail (key types, IPC surface, debug commands), see the
[`CLAUDE.md` Project Layout / Debug Mode
sections](../../../../CLAUDE.md).
Don't duplicate that material here — link to it.

## Purpose

Provide every subsystem with a single composed root (`AppState`),
crash-safe atomic file I/O, an append-only progress log for swarm
replay, secrets-in-keychain, and per-ID structured logs the
investigation guides depend on.

## Key files

| File | What it owns |
|------|--------------|
| `app_state.rs` | `AppState` — the Tauri-managed root. Holds Arcs to the Pi manager, hivemind registry, swarm registry, config, secret store, stores, and shared response cache. |
| `config.rs` | Config loading (JSON file + env-var overrides). Hot-reload-safe via clone-and-drop guards so `.await` never spans a lock. |
| `secret_store.rs` | OS-keychain integration via the `keyring` crate. API keys never touch plaintext disk. |
| `store.rs` | `SwarmStore` — per-swarm directory layout, atomic file writes (`tempfile::NamedTempFile + persist()`), no torn writes. |
| `progress.rs` | Append-only JSONL progress log with `BufWriter`. Crash-safe replay target for swarm recovery. |
| `swarm_registry.rs` | `RunningSwarm` registry with `CancellationToken` + pause support. Owns Queen `JoinHandle`s for clean shutdown. |
| `usage_store.rs` | Aggregated usage / cost data shown on the Dashboard and topbar. |
| `log_redact.rs` | API-key scrubbing writer wrapped around debug log files. |
| `log_routing.rs` | `PerIdRoutingLayer` — routes `tracing` events to per-ID files by inspecting span fields (`review_id` > `swarm_id`+`agent` > `swarm_id` > `session_id` > `general`). |

## Contracts

- **No `.await` while holding a config guard.** Helpers in `config.rs`
  clone-and-drop internally; bind the clone to a local before any I/O.
- **Atomic writes only.** All state files are written to a sibling
  `tempfile` and `persist()`-ed (atomic rename). Direct `File::create`
  on the target path is forbidden.
- **Secrets stay in keychain.** API keys are read via `SecretStore` and
  passed to providers by value. Never serialize keys into config files
  or logs (the redacting writer is the second line of defense).
- **Bounded log channel.** The per-ID routing layer pushes events into
  a bounded `mpsc::sync_channel(4096)` consumed by a worker that holds
  up to 64 open file handles (LRU eviction). Channel overflow drops
  events silently — the async runtime must never stall on log I/O.
- **No debug files exist unless `HYVEMIND_DEBUG=1`** was set when the
  event fired. The routing layer is a no-op otherwise.

## Where things live at runtime

- `~/.hyvemind/config.json` — provider config, default model, paths.
- `~/.hyvemind/swarms/{id}/` — `state.json`, `features.json`,
  `milestones.json`, `progress_log.jsonl`.
- `~/.hyvemind/task-messages/task-{NUMERIC_ID}.json` — frontend Tasks
  UI state (JSON array of message objects).
- `~/.hyvemind/debug/sessions/{session_id}.jsonl` — per-session firehose.
- `~/.hyvemind/debug/reviews/{review_id}.jsonl` — per-review firehose.
- `~/.hyvemind/debug/swarms/{swarm_id}/...` — per-swarm + per-agent buckets.
- `~/.hyvemind/debug/general.jsonl.YYYY-MM-DD` — anything outside a
  known span (daily rotation, 7-day retention).
- API keys: OS keychain only (via `keyring`).

## See also

- [`../../../../CLAUDE.md`](../../../../CLAUDE.md) — full debug-mode
  guide, per-ID routing priority, investigation recipes.
- [`../../../../PRODUCT.md` §7](../../../../PRODUCT.md) — storage
  layout overview and reliability primitives.
- [`../core/README.md`](../core/README.md) — primary consumer of
  `SwarmStore`, `progress`, and `swarm_registry`.
- [`../hivemind/README.md`](../hivemind/README.md) — primary consumer
  of `ResponseCache` (now an `AppState` singleton) and `SecretStore`.
