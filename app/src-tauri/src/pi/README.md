# `pi/` — Pi Subprocess Management

Hyvemind doesn't implement its own agent loop. It speaks LF-delimited
JSONL over stdin/stdout to bundled **Pi** subprocesses
([`@earendil-works/pi-coding-agent`](https://github.com/badlogic/pi-mono))
running in `--mode rpc`. This module is the client side: spawn the
process, send prompts/steers/aborts, consume streaming events.

For per-file detail (key types, IPC surface, debug commands), see the
[`CLAUDE.md` Pi binary / Project Layout
sections](../../../../CLAUDE.md).
Don't duplicate that material here — link to it.

## Purpose

A pooled, sandboxed, semaphore-bounded set of Pi subprocesses with a
JSONL RPC client, streaming event types, and IPC-friendly batching.

## Key files

| File | What it owns |
|------|--------------|
| `rpc.rs` | JSONL protocol client. Dedicated stdout / stderr reader tasks, RPC command serialization, line-length caps. |
| `session.rs` | `PiSession` — single Pi subprocess wrapper. Owns an `OwnedSemaphorePermit` (drops with the session), authoritative event transcript, activity / stall counters. |
| `manager.rs` | `PiManager` — process pool, global `Semaphore` (default 6), pre-warm, graveyard reconciliation for orphan children on startup. |
| `events.rs` | `PiEvent` enum (tokens, tool calls, reasoning, lifecycle) + `PiSessionStats` snapshot. The 100 ms / 50-token IPC coalescer that throttles `chat-event` now lives in `commands/chat.rs` alongside the chat command, not in this module. |
| `eviction.rs` | Idle-session eviction policy for the pool. |
| `defaults.rs` | Seeds `~/.pi/web-search.json` with `workflow: "none"` on startup so `pi-web-access`'s `web_search` tool doesn't hang on the curator browser popup that headless RPC sessions can't answer. Non-destructive — only writes when the key is missing. |

## Contracts

- **Permit-bound lifetime**: a session owns an `OwnedSemaphorePermit`.
  Dropping the session releases the slot — there is no separate
  "release" call to forget.
- **Strict JSONL**: every line on stdin/stdout is a single JSON object
  terminated by `\n`. Lines are capped to a max byte length; over-cap
  lines are logged and discarded.
- **Stall counter**: each parsed Pi event bumps an `AtomicU64` on the
  session. `core::nurse` uses CAS on that counter to detect stalls
  without holding a lock.
- **Force kill is bounded**: `force_kill` issues SIGTERM, then waits
  with a deadline, then SIGKILL. It always rejoins the child.
- **Bundled Pi only**: the binary is built from `scripts/build-pi.sh` at
  the version pinned in `scripts/pi-version.txt`. Users never `npm i`
  Pi; the pool always launches the bundled `pi-<target-triple>` next
  to the app.

## Where things live at runtime

- Pi binary + support files: `app/src-tauri/binaries/` (bundled, gitignored).
- Session transcripts (written by Pi itself):
  `~/.hyvemind/chat-sessions/{session_id}.jsonl`.
- Subagent run artifacts:
  `~/.hyvemind/chat-sessions/{session_id}/{task_hash}/run-{n}/...` and
  `~/.hyvemind/chat-sessions/subagent-artifacts/`.
- Per-session debug logs (when `HYVEMIND_DEBUG=1`):
  `~/.hyvemind/debug/sessions/{session_id}.jsonl`.

## See also

- [`../../../../CLAUDE.md`](../../../../CLAUDE.md) — Pi-binary build /
  pin, RPC investigation recipes, session-debug log routing.
- [`../../../../PRODUCT.md` §7](../../../../PRODUCT.md) — why Pi was
  chosen as the underlying agent runtime.
- [`../core/README.md`](../core/README.md) — agents that consume Pi
  sessions (Queen, Scout, Worker, Guard, Nurse).
- [`../hivemind/README.md`](../hivemind/README.md) — Pi sessions used
  for context-gathering and merge phases of a review.
