# `hivemind/` — Multi-Model Review Engine

The concurrent multi-model review subsystem. Dispatches the same prompt
to N models in parallel (stance-biased), merges the round's outputs via
a Pi-driven step, and feeds the merged plan into the next round.

For per-file detail (key types, IPC surface, debug commands), see the
[`CLAUDE.md` Project Layout / Investigating a Hivemind Review
sections](../../../../CLAUDE.md).
Don't duplicate that material here — link to it.

## Purpose

Take a single prompt + a named team of models, run it through one or
more rounds of stance-biased (For / Against / Neutral) parallel review,
and produce a refined plan. Persist the run for later inspection.

## Key files

| File | What it owns |
|------|--------------|
| `engine.rs` | `ReviewEngine` — top-level orchestrator. JoinSet concurrency (cap 8), per-round `tokio::time::timeout`, round-N → merge → round-N+1 pipeline. |
| `../providers/mod.rs` | `Arc<dyn Provider>` trait dispatch over Anthropic / OpenAI / OpenRouter / Ollama / PiSubscription. Streaming SSE parsing, token + cost accounting (per-provider). `ProviderRegistry` for lookup. |
| `circuit_breaker.rs` | 3-state breaker (Closed / Open / HalfOpen) per provider, with `probe_in_flight` to prevent split-state races. |
| `backoff.rs` | Exponential backoff with jitter: `min(60s, 5s × 2^attempt + rand(0..2s))`. |
| `cache.rs` | `ResponseCache` — moka-based, lock-free, TTL- and size-bounded. Promoted to an `AppState` singleton. |
| `store.rs` | SQLite store for review jobs / job_steps (`sqlx`, WAL mode). Migrations under `app/src-tauri/migrations/0001_hivemind.sql`. |
| `review_log.rs` | `ReviewLogger` — high-level event JSONL under `~/.hyvemind/reviews/{review_id}.jsonl`. |
| `output_capture.rs` | Per-model-call full-output capture under `~/.hyvemind/reviews/{id}/output-{model}-r{round}.txt`. |
| `merge_capture.rs` | Per-merge-run text capture under `~/.hyvemind/reviews/{id}/merge-r*.txt`. |

## Contracts

- **Concurrency cap**: `JoinSet` of model calls is bounded to 8 in
  flight; the per-round budget is enforced with `tokio::time::timeout`.
- **Round shape**: round N runs all N reviewers in parallel against the
  same enriched prompt → merge Pi session synthesizes round N output →
  output becomes round N+1 input.
- **Round-boundary emit ordering**: within `run_merge_phase`, the
  `verdicts_updated` event is emitted (and the per-round verdict
  SQLite write completes) **before** the user-visible `merge_completed`
  event. The review-logger writes for `verdicts_saved`, `verdicts_absent`,
  and `merge_completed` are fire-and-forget so nothing observability-only
  sits between `merge_completed` and the next round's `round_started`
  emit — frontend reducers must update the dock pill the instant
  `merge_completed` arrives and not assume verdicts arrive later.
- **Output storage**: `model_call_completed` events in the review log
  carry only `output_file` (relative path under `~/.hyvemind/`) and
  `output_len` — the full text lives in the capture file. Old
  inline-`output` semantics are gone.
- **Stance bias**: each model in a Hivemind has a stance (`For`,
  `Against`, `Neutral`) that's injected into its system prompt to force
  perspective diversity.
- **Circuit breaker**: trips per-provider after consecutive failures.
  HalfOpen lets a single probe call through; success closes, failure
  re-opens. Other calls short-circuit while Open.
- **Cache key**: `(provider, model, prompt_hash)`. Hits are reported
  with `duration_ms: 0` — useful for spotting accidental dedupe.

## Where things live at runtime

- High-level event log (always on when `HYVEMIND_DEBUG=1`):
  `~/.hyvemind/reviews/{review_id}.jsonl`
- Per-model output captures:
  `~/.hyvemind/reviews/{review_id}/output-{model_id_safe}-r{round}-i{model_idx}.txt`
  (model id with `/` and `:` replaced by `_`; `model_idx` is the 0-based
  instance index of the reviewer call within the round, matching
  `job_steps.sort_order` in SQLite). Always suffixed with `-i{N}` so
  duplicate-instance reviewers don't overwrite each other on disk.
- Per-merge captures: `~/.hyvemind/reviews/{review_id}/merge-r*.txt`.
- TRACE-level firehose: `~/.hyvemind/debug/reviews/{review_id}.jsonl`.
- SQLite job records: `~/.hyvemind/hivemind/`.

## See also

- [`../../../../CLAUDE.md`](../../../../CLAUDE.md) — full review-log
  event table, investigation recipes, log-routing rules.
- [`../../../../PRODUCT.md` §3 / §5](../../../../PRODUCT.md) — product
  framing for stance-biased multi-round review.
- [`../pi/README.md`](../pi/README.md) — Pi sessions are used for the
  context-gathering and merge phases.
- [`../extensions/README.md`](../extensions/README.md) — Provider
  Extensions (usage / credits / auth probes) for the provider IDs this
  engine dispatches to.
