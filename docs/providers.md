# LLM Providers

A short orientation to the LLM provider subsystem. Hyvemind talks to
five distinct LLM API shapes through a single object-safe `Provider`
trait so the rest of the codebase (Hivemind engine, Nurse, chat
command, settings probes) can dispatch without caring whose API it is.
The deep dive lives in
[`app/src-tauri/src/providers/README.md`](../app/src-tauri/src/providers/README.md);
this page is the index.

## The five providers

| Provider | What it talks to | Streaming | Notes |
|----------|------------------|-----------|-------|
| `AnthropicProvider` | `POST https://api.anthropic.com/v1/messages` | no | Native Messages API. Adjusts `input_tokens` for prompt-cache hits. |
| `OpenAICompatibleProvider` | `POST {base_url}/chat/completions` | yes (SSE) | The workhorse — covers OpenAI / DeepSeek / Groq / Mistral / Ollama / any custom endpoint. |
| `OpenRouterProvider` | `https://openrouter.ai/api/v1` | yes (delegated) | Thin wrapper around `OpenAICompatibleProvider` that injects `HTTP-Referer` and `X-Title` headers. |
| `PiSubscriptionProvider` | Pi subprocess via JSONL RPC | no | Lets `chatgpt` and `claude-sub` users authenticate through their Pi-managed `~/.pi/agent/auth.json` instead of an API key. |
| `MockProvider` | nothing (test-only) | yes (scripted) | Canned `output` + `StreamingMockConfig` for engine tests. |

Per-impl detail (endpoints, auth, SSE parsing, cost accounting,
structured-output behaviour, gotchas) is in
[the README](../app/src-tauri/src/providers/README.md#the-five-concrete-impls).

## The trait split (`Provider` vs `StreamingProvider`)

Every backend implements the object-safe
[`Provider`](../app/src-tauri/src/providers/provider_trait.rs) trait
(`call(req: CallRequest) -> Result<ModelResponse>`). Backends that can
emit progressive deltas additionally implement `StreamingProvider`,
and surface themselves via `Provider::as_streaming()`. This makes the
silent-drop bug unrepresentable: a streaming caller has to ask
`as_streaming()` before it can hand the provider an
`mpsc::UnboundedSender<StreamChunk>`, and a non-streaming provider
isn't reachable through that path.

`AnthropicProvider` and `PiSubscriptionProvider` are intentionally
buffered-only — the engine routes streaming callers through them via
plain `call(...)`. See
[`provider_trait.rs:149`](../app/src-tauri/src/providers/provider_trait.rs).

`CallRequest` carries the mandatory triple (`model_id`,
`system_prompt`, `user_prompt`), optional sampling knobs
(`temperature`, `top_p`, `max_tokens`), a per-call `timeout` override,
and an optional `structured: StructuredOutputConfig` that flips
Anthropic and OpenAI-compatible providers into tool-use mode.
Providers ignore knobs they don't support.

## Dispatch

`ProviderRegistry` (a `HashMap<String, Arc<dyn Provider>>`) is the
single lookup surface. It's constructed once at startup in
[`AppState::new`](../app/src-tauri/src/state/app_state.rs) and
refreshed by `AppState::refresh_provider_registry` whenever a
`#[tauri::command]` mutates API keys or provider config. Forget the
refresh and hivemind/nurse dispatch against a stale snapshot until
restart.

Lookup is `registry.get(name) -> Option<Arc<dyn Provider>>`. Callers
clone the `Arc` and drop the registry guard before any I/O — the
registry sits behind an `AsyncRwLock`, and a write lock from
`refresh_provider_registry` blocks until in-flight read guards drop
(bounded by the engine's per-round timeout). Construction rules
(which `provider_type` produces which impl, keyless localhost for
Ollama, lazy subscription auth) are in
[the README's `ProviderRegistry` section](../app/src-tauri/src/providers/README.md#providerregistry).

## Reliability primitives

Every provider integrates with three shared pieces of infrastructure:

- **Circuit breaker** — 3-state per provider with `probe_in_flight`
  gating. Trips after `tunables::circuit_breaker_threshold()`
  (default 5) consecutive failures, Open for
  `tunables::circuit_breaker_cooldown()` (default 60 s).
  [`circuit_breaker.rs`](../app/src-tauri/src/hivemind/circuit_breaker.rs).
- **Response cache** — moka-based, lock-free, TTL- and size-bounded.
  Engine-level, not provider-level.
  [`cache.rs`](../app/src-tauri/src/hivemind/cache.rs).
- **Exponential backoff with jitter** —
  `min(60s, 5s * 2^attempt + rand(0..2s))`. Providers do **not** sleep
  on failure; callers drive the backoff.
  [`backoff.rs`](../app/src-tauri/src/hivemind/backoff.rs).

## Cost lookup

Three-layer resolution walked in order: per-provider `ModelInfo`
pricing → `legacy_well_known_model_cost` match-table for known model
ids → generic `(1.0, 5.0)` per-million fallback. The engine's
`compute_cost(input_tokens, output_tokens, model_id)` helper drives
the chain so the Dashboard never shows `$0.00` for unknown models.
Details in
[the README's cost-lookup section](../app/src-tauri/src/providers/README.md#cost-lookup).

Anthropic quirk: `input_tokens` subtracts
`cache_creation_input_tokens` + `cache_read_input_tokens` so
prompt-cache hits aren't double-counted (the first cold-cache round
slightly under-bills). A future streaming Anthropic path must
replicate this adjustment on the SSE final-frame usage block.

## Provider extensions

Many registered providers also have a companion **extension** under
[`app/src-tauri/src/extensions/builtins/`](../app/src-tauri/src/extensions/builtins/)
that surfaces credit / usage / balance data for the Topbar pill and
the Settings → Provider Extensions panel. The provider id is the join
key: a provider with no extension is fine; an extension matched
against a missing provider id is silently inert.

| Provider id | Builtin extension |
|-------------|------------------|
| `anthropic` | `anthropic_usage.rs` |
| `openrouter` | `openrouter_credits.rs` |
| `deepseek` | `deepseek_balance.rs` |
| `crof` | `crof_usage.rs` |
| `claude-sub` | `claude_sub_usage.rs` |
| `chatgpt` | `chatgpt_sub_usage.rs` |
| `neuralwatt` | `neuralwatt_usage.rs` |

Authoring a new extension has its own contract (refresh interval,
error semantics, redaction). See
[`docs/extension-authoring.md`](extension-authoring.md) and
[`app/src-tauri/src/extensions/README.md`](../app/src-tauri/src/extensions/README.md).

## Adding a new provider

Two paths, both walked end-to-end in
[the README's worked example](../app/src-tauri/src/providers/README.md#worked-example-adding-a-new-provider):

- **OpenAI-compatible** (Groq, Mistral, Together, …): seed the config
  default, add models to the catalog, optionally add pricing fallback
  and a provider extension. No Rust impl needed —
  `OpenAICompatibleProvider` handles dispatch automatically.
- **New API shape**: implement the provider struct (manual redacting
  `Debug`), implement `Provider` and optionally `StreamingProvider`,
  add a registration arm in `refresh_from_config_with_pi`, extend the
  `ALLOWED_PROVIDER_TYPES` whitelist, then do the OpenAI-compatible
  steps for surrounding config and catalog.

Short-form checklist:
[`CONTRIBUTING.md` §"Adding a provider"](../CONTRIBUTING.md#adding-a-provider).
Long form: the README.

## Where things live

| Concern | Path |
|---------|------|
| Trait definitions | [`providers/provider_trait.rs`](../app/src-tauri/src/providers/provider_trait.rs) |
| Concrete impls + registry | [`providers/mod.rs`](../app/src-tauri/src/providers/mod.rs) |
| Subsystem deep dive | [`providers/README.md`](../app/src-tauri/src/providers/README.md) |
| Circuit breaker | [`hivemind/circuit_breaker.rs`](../app/src-tauri/src/hivemind/circuit_breaker.rs) |
| Response cache | [`hivemind/cache.rs`](../app/src-tauri/src/hivemind/cache.rs) |
| Backoff calculator | [`hivemind/backoff.rs`](../app/src-tauri/src/hivemind/backoff.rs) |
| Cost computation | [`hivemind/engine.rs`](../app/src-tauri/src/hivemind/engine.rs) (`compute_cost`) |
| Registry construction | [`state/app_state.rs`](../app/src-tauri/src/state/app_state.rs) (`refresh_provider_registry`) |
| `provider_type` whitelist | [`commands/settings.rs`](../app/src-tauri/src/commands/settings.rs) (`ALLOWED_PROVIDER_TYPES`) |
| Default provider seeding | [`state/config.rs`](../app/src-tauri/src/state/config.rs) (`seed_default_providers`) |
| Provider extensions | [`extensions/builtins/`](../app/src-tauri/src/extensions/builtins/) |

## Related documentation

- [`docs/architecture.md`](architecture.md) — system-level view of how
  providers slot into AppState / Hivemind / Swarm subsystems.
- [`docs/ipc-reference.md`](ipc-reference.md) — IPC commands that
  mutate provider config and must call `refresh_provider_registry`.
- [`docs/extension-authoring.md`](extension-authoring.md) — provider
  extensions; per-provider id is the join key.
- [`PRODUCT.md` §7 "Provider abstraction"](../PRODUCT.md) — product framing.
- [`CONTRIBUTING.md` §"Adding a provider"](../CONTRIBUTING.md) — short checklist.
- [`CLAUDE.md`](../CLAUDE.md) — IPC surface, log routing, debug recipes.
