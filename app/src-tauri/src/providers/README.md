# `providers/` — LLM Backend Dispatch

The object-safe LLM provider layer. Every outbound model call — whether
it originates in the Hivemind engine, the Nurse intervention helper, the
chat command, or a settings connectivity probe — is dispatched through
an `Arc<dyn Provider>` registered in [`ProviderRegistry`](mod.rs). Five
concrete impls live in [`mod.rs`](mod.rs); the trait surface lives in
[`provider_trait.rs`](provider_trait.rs).

For per-file detail (key types, IPC surface, debug commands), see the
[`CLAUDE.md` Project Layout / Key Types
sections](../../../../CLAUDE.md). Don't duplicate that material here —
link to it.

## Purpose

Hyvemind has to talk to several LLM API shapes (Anthropic Messages,
OpenAI-compatible chat-completions, OpenRouter's headered passthrough,
local Ollama, and Pi-mediated subscription auth). The provider layer
hides those differences behind a uniform, object-safe `call(...)`
surface so the engine can iterate `Arc<dyn Provider>` instances without
caring whose API it's talking to.

The split between [`Provider`](provider_trait.rs:115) and
[`StreamingProvider`](provider_trait.rs:149) fixes the historical
"silently drop the channel" bug at the pre-audit `providers.rs:285-310`,
where Anthropic and Pi-subscription paths accepted an `mpsc::Sender` and
immediately `drop`'d it — leaving the receiver to wait for deltas that
would never arrive. After the audit-6.6 split, only providers that
genuinely emit progressive deltas implement `StreamingProvider`, and the
engine routes streaming callers through them via
[`Provider::as_streaming`](provider_trait.rs:132).

## The `Provider` trait

[`provider_trait.rs:115`](provider_trait.rs)

```rust
#[async_trait]
pub trait Provider: Send + Sync + std::fmt::Debug {
    async fn call(&self, req: CallRequest) -> Result<ModelResponse>;
    fn name(&self) -> &str;
    fn as_streaming(&self) -> Option<&dyn StreamingProvider> { None }
}
```

- **Object-safe by construction.** No generic methods, no `Self: Sized`
  bounds, no associated types. The registry stores instances as
  `HashMap<String, Arc<dyn Provider>>`
  ([`mod.rs:1238`](mod.rs)) — replacing the closed-set enum dispatch
  the pre-audit code used. Object-safety lets new providers ship
  without touching every dispatch site.
- **`Send + Sync + Debug`** are required so providers can live inside
  `Arc<dyn Provider>` shared across tokio tasks. The `Debug` bound is
  satisfied by **manual, redacting impls** on every concrete provider
  (`AnthropicProvider` at [`mod.rs:667`](mod.rs),
  `OpenAICompatibleProvider` at [`mod.rs:153`](mod.rs)); the derived
  `Debug` would leak the bearer-token material if a future
  `tracing::instrument` ever Debug-formatted the provider.
- **`CallRequest`** ([`provider_trait.rs:49`](provider_trait.rs)) is
  the parameter bundle: mandatory `model_id` / `system_prompt` /
  `user_prompt`, optional `temperature` / `top_p` / `max_tokens` /
  `timeout`, and an optional `structured: StructuredOutputConfig` that
  flips the call into tool-use mode. Providers that don't support a
  given knob silently ignore it (e.g. `PiSubscriptionProvider` ignores
  sampling parameters because Pi mediates them upstream).
- **`ModelResponse`** ([`mod.rs:48`](mod.rs)) is the uniform return:
  `output`, `input_tokens`, `output_tokens`, `model_id`, `duration_ms`.

## The `StreamingProvider` extension trait

[`provider_trait.rs:149`](provider_trait.rs)

```rust
#[async_trait]
pub trait StreamingProvider: Provider {
    async fn call_streaming(
        &self,
        req: CallRequest,
        tx: mpsc::UnboundedSender<StreamChunk>,
    ) -> Result<ModelResponse>;
}
```

Only providers that can emit real progressive deltas implement this
trait:

| Provider | `Provider` | `StreamingProvider` | Notes |
|----------|------------|---------------------|-------|
| `OpenAICompatibleProvider` | yes | yes | SSE parser at [`mod.rs:85`](mod.rs) |
| `OpenRouterProvider` | yes | yes | Wraps the OpenAI-compat impl |
| `PiSubscriptionProvider` | yes | **no** | Pi returns a single buffered output via `collect_response()` |
| `AnthropicProvider` | yes | **no** | Buffered Messages API only; no SSE path implemented |
| `MockProvider` (test) | yes | yes | Scripted via `StreamingMockConfig` |

Callers that want deltas test
[`Provider::as_streaming()`](provider_trait.rs:132); if it returns
`None`, they fall back to the buffered `call(...)`. The sender is never
passed to a non-streaming provider, which makes the silent-drop bug
unrepresentable.

## `ProviderRegistry`

[`mod.rs:1237`](mod.rs)

```rust
#[derive(Debug, Default)]
pub struct ProviderRegistry {
    providers: HashMap<String, Arc<dyn Provider>>,
}
```

- **Lookup** is `get(name) -> Option<Arc<dyn Provider>>` keyed by the
  stable provider id from `config.providers` (`"anthropic"`,
  `"openrouter"`, `"chatgpt"`, `"claude-sub"`, any user-defined
  OpenAI-compat id). Callers `Arc::clone` the result and drop the
  registry guard before performing I/O.
- **Construction** happens once at startup in
  [`AppState::new`](../state/app_state.rs) at
  [`app_state.rs:360`](../state/app_state.rs), calling
  [`refresh_from_config_with_pi`](mod.rs:1267) with the live API-key
  map and provider configs plus an `Arc<PiManager>` for subscription
  providers.
- **Refresh** is driven by
  [`AppState::refresh_provider_registry`](../state/app_state.rs:624).
  Every command that mutates `config.providers` or `config.provider_keys`
  (e.g. `save_api_key`, `add_provider`, `delete_api_key` in
  [`commands/settings.rs`](../commands/settings.rs)) must call it,
  otherwise hivemind and nurse keep dispatching against a stale
  snapshot until the next app restart. Lock ordering is documented at
  [`app_state.rs:612`](../state/app_state.rs).
- **Dispatch rules** inside `refresh_from_config_with_pi`
  ([`mod.rs:1273-1353`](mod.rs)):
  - `provider_type == "Anthropic"` and key present → `AnthropicProvider`
  - `provider_type == "Subscription"` and `pi_manager.is_some()` → always register `PiSubscriptionProvider` (auth is verified lazily at call time)
  - `id == "openrouter"` with a key → `OpenRouterProvider`
  - everything else with a non-empty endpoint and either a key or a `localhost` endpoint (Ollama) → `OpenAICompatibleProvider`
  - construction failures are logged with `warn!` and the provider is skipped — the app stays up rather than panicking at startup

## The five concrete impls

### `AnthropicProvider` — [`mod.rs:658`](mod.rs)

- **Endpoint**: `POST https://api.anthropic.com/v1/messages`
  ([`mod.rs:743`](mod.rs)).
- **Auth**: `x-api-key: <key>` + `anthropic-version: 2023-06-01`
  headers. Never sent through `Authorization: Bearer`.
- **Streaming**: not implemented. `Provider::as_streaming` returns
  `None`; callers route through buffered `call(...)`. A future SSE
  path must replicate the cache-token adjustment described below.
- **Cost accounting**: `input_tokens` is the raw `usage.input_tokens`
  **minus** `cache_creation_input_tokens` **minus**
  `cache_read_input_tokens`
  ([`mod.rs:791`](mod.rs)). This avoids double-counting prompt-cache
  hits; the documented trade-off at
  [`engine.rs:3015`](../hivemind/engine.rs) is that the first
  (cold-cache) round of a review slightly under-bills. If the
  `cache_creation_input_tokens` field disappears from the response, a
  `debug!` line at [`mod.rs:803`](mod.rs) makes the schema drift
  observable.
- **Structured output**:
  [`call_structured`](mod.rs:831) injects
  `tools` + `tool_choice`; the parser at
  [`extract_anthropic_tool_use_or_text`](mod.rs:1416) walks the
  `content` array for the first `tool_use` named
  `REVIEWER_TOOL_NAME` (`"submit_review"`) and JSON-stringifies its
  `input`. Falls back to concatenated `text` blocks when the model
  ignored `tool_choice`.

### `OpenAICompatibleProvider` — [`mod.rs:134`](mod.rs)

The workhorse. Used directly for OpenAI / DeepSeek / Groq / Mistral /
Ollama / any custom OpenAI-compatible endpoint, and wrapped by
`OpenRouterProvider`.

- **Endpoint**: `POST {base_url}/chat/completions`.
- **Auth**: `Authorization: Bearer <key>` when `api_key` is non-empty;
  omitted entirely for keyless localhost endpoints (Ollama).
- **Streaming** ([`call_with_progress`](mod.rs:389)):
  - Sets `stream: true` + `stream_options: { include_usage: true }`.
  - Parses the SSE byte stream with
    [`eventsource_stream`](https://docs.rs/eventsource-stream); each
    event payload goes through the pure
    [`parse_sse_event_data`](mod.rs:85) helper.
  - 30-second `INACTIVITY_TIMEOUT_SECS` watchdog on the SSE stream;
    a stall trips the circuit breaker and returns an error
    ([`mod.rs:511-531`](mod.rs)).
  - Empty-stream retry: up to `MAX_EMPTY_STREAM_ATTEMPTS = 3`
    ([`mod.rs:27`](mod.rs)) with a linear 500 ms / 1000 ms backoff
    when the server returns HTTP 200 + a clean SSE close with zero
    content and no usage frame (observed on `crof`).
  - Content-Type fallback: if the server returns
    `application/json` despite `stream: true`, parses the buffered
    body via
    [`parse_buffered_response`](mod.rs:248).
- **Cost accounting**: `usage.prompt_tokens` /
  `usage.completion_tokens` from the final SSE frame (or buffered
  body). No cache-token adjustment — OpenAI-compat providers don't
  surface separate cache counts.
- **Structured output** ([`call_structured`](mod.rs:292)):
  non-streaming on purpose; the engine buffers the JSON envelope.
  Parser at
  [`extract_openai_tool_call_or_text`](mod.rs:1453) returns
  `tool_calls[0].function.arguments` verbatim or falls back to
  `message.content`.
- **Extra headers**:
  [`new_with_headers`](mod.rs:183) takes a `HeaderMap` that gets
  attached on every request. `OpenRouterProvider` uses this to inject
  `HTTP-Referer` + `X-Title` without duplicating the request path.

### `OpenRouterProvider` — [`mod.rs:941`](mod.rs)

A thin wrapper around `OpenAICompatibleProvider`:

- **Endpoint**: `https://openrouter.ai/api/v1`.
- **Extra headers**: `HTTP-Referer: https://hyvemind.app` and
  `X-Title: Hyvemind` ([`mod.rs:952`](mod.rs)) — OpenRouter uses
  these for analytics / model-leaderboard attribution.
- **Streaming**: yes, delegated to the inner impl.
- **Structured output**: yes, delegated. OpenRouter forwards `tools` to
  the upstream provider when the model supports it.
- **Gotcha**: gets its own provider type purely because of the headers;
  everything else is identical to the OpenAI-compatible path.

### `PiSubscriptionProvider` — [`mod.rs:1059`](mod.rs)

Routes calls through a Pi subprocess so users on `chatgpt` or
`claude-sub` subscriptions can authenticate via their normal
Pi-managed `~/.pi/agent/auth.json` instead of an API key.

- **Endpoint**: none directly — spawns a Pi session via
  [`PiManager::spawn_session_with_options`](../pi/manager.rs) and
  drives it through the JSONL RPC protocol.
- **Auth**: handled by Pi. The provider always registers when
  `pi_manager.is_some()` ([`mod.rs:1294`](mod.rs)); auth errors
  surface lazily at call time with a real Pi error message instead of
  the misleading "provider not found in registry".
- **Model id mapping**: rewrites internal names to Pi's
  `provider/model` form via [`pi_model_string`](mod.rs:1088) —
  `chatgpt/gpt-5.5` → `openai-codex/gpt-5.5`,
  `claude-sub/claude-opus-4-7` → `anthropic/claude-opus-4-7`.
- **Streaming**: not implemented. Sessions are short-lived (180 s
  timeout at [`mod.rs:1169`](mod.rs)) and the buffered output comes
  back as a single string from `session.collect_response()`. The
  spawned session is tagged
  `SessionOwner::Review` ([`mod.rs:1148`](mod.rs)) so the
  idle-eviction loop skips it, then explicitly killed in the
  happy and error paths.
- **Cost accounting**: prefers Pi's authoritative `get_session_stats`
  ([`mod.rs:1181`](mod.rs)) — `input` plus `output + reasoning_tokens`.
  Falls back to a `len / 4` token estimate when Pi can't report.
  Cost-per-million is zero in the seeded `ModelInfo` ([`mod.rs:1357`](mod.rs))
  because the subscription is flat-rate.
- **Structured output**: when `req.structured.is_some()`, the session
  is locked to `ToolSet::Custom(vec!["submit_review"])`
  ([`mod.rs:1137`](mod.rs)) so the only callable tool is the review
  envelope, then the captured tool args are JSON-stringified into
  `ModelResponse.output`. Tool args MUST be drained
  before `kill_session` ([`mod.rs:1183`](mod.rs)) — kill drops the
  capture map.
- **Hardcoded model list**:
  [`subscription_models_for`](mod.rs:1367) defines the known
  ChatGPT and Claude subscription models. Adding a new subscription
  model means appending a `sub_model(id, ctx)` line here.

### `MockProvider` — [`mod.rs:1534`](mod.rs)

Test-only (gated behind `#[cfg(test)]`). Returns a canned `output`
string with configurable inter-chunk delay. Implements both `Provider`
and `StreamingProvider`. Used to exercise the round-timeout-reaping
path in the engine ([`with_delay`](mod.rs:1562)) and the streaming
fan-out in `call_with_progress` ([`with_streaming`](mod.rs:1573)).

## Cost lookup

Three-layer resolution, walked in this order:

1. **Per-provider `ModelInfo::cost_per_1m_input` / `cost_per_1m_output`**.
   Populated by provider impls or the model-discovery refresh; the
   default-trait `cost_per_1m_tokens(model_id)` derives from this.
2. **`legacy_well_known_model_cost`** in
   [`mod.rs:1694`](mod.rs). A hard-coded match-table that the
   pre-audit `engine.rs:3448` used; preserved for the well-known
   Anthropic + OpenAI model ids so cost still resolves when no
   registered provider claims the model.
3. **Engine fallback**:
   [`engine.rs:3037`](../hivemind/engine.rs) returns `(1.0, 5.0)` per
   million tokens as the last-resort generic rate so the Dashboard
   never shows `$0.00` for unknown models.

Cost itself is computed by
[`engine.rs::compute_cost`](../hivemind/engine.rs):
`(input_tokens / 1M) * cost_in + (output_tokens / 1M) * cost_out`.

## Reliability integration

Every concrete provider owns an
`Arc<CircuitBreaker>` constructed from
[`tunables::circuit_breaker_threshold`](../tunables.rs) (default 5) and
[`tunables::circuit_breaker_cooldown`](../tunables.rs) (default 60s).
The integration contract:

- **Before every outbound HTTP/RPC call** the provider awaits
  [`CircuitBreaker::before_request`](../hivemind/circuit_breaker.rs:96).
  An `Open` or `HalfOpenBusy` error short-circuits the call before any
  network I/O.
- **On HTTP non-2xx, parse error, SSE stall, or empty-stream
  exhaustion** the provider calls
  [`record_failure`](../hivemind/circuit_breaker.rs:189). Three
  consecutive failures in `Closed` flips the breaker to `Open`; a
  single failure in `HalfOpen` re-trips it.
- **On full success** the provider calls
  [`record_success`](../hivemind/circuit_breaker.rs:162). This clears
  the failure counter and forces the state back to `Closed`.
- **Backoff between retries** is the responsibility of the caller
  (e.g. the engine's per-round retry loop) using
  [`BackoffCalculator`](../hivemind/backoff.rs) — defaults to
  `min(60s, 5s * 2^attempt + rand(0..2s))`. Providers themselves do
  **not** sleep on failure; they return errors and let the upper
  layer decide whether to retry.
- **Response caching** is similarly an engine concern. Providers
  produce `ModelResponse`s; the engine wraps them in a `CachedResponse`
  keyed by
  [`ResponseCache::make_key`](../hivemind/cache.rs:105)
  (`provider_id`, `model_id`, system+user prompt hashes, temperature,
  top_p, max_tokens). Bypassing the cache means the engine will dispatch
  the call directly via the registry.

## Worked example: adding a new provider

[`CONTRIBUTING.md`](../../../../CONTRIBUTING.md) lists the high-level
steps. This expanded checklist covers everything an audit would catch.

**Case 1 — OpenAI-compatible (the 90% case: Groq, Mistral, Together, …)**

1. **Seed the default config** in
   [`state/config.rs::seed_default_providers`](../state/config.rs:343):
   add a tuple `(id, display_name, "OpenAI Compatible", endpoint)`. The
   `id` is the stable registry key.
2. **Add models** to
   [`commands/settings.rs::get_model_catalog`](../commands/settings.rs:962)
   for the new `provider_filter` arm. Each entry gives the frontend a
   context-window + per-million pricing for the New Swarm + Hivemind
   pickers.
3. **Pricing fallback** — if the user might dispatch against a model
   id that isn't in `get_model_catalog` (unlikely for most providers),
   add it to
   [`legacy_well_known_model_cost`](mod.rs:1694) so the Dashboard
   shows a real number instead of the `(1.0, 5.0)` fallback.
4. **Provider extension** — if the API offers a balance/usage
   endpoint, add a builtin under
   [`extensions/builtins/`](../extensions/builtins/) and wire it into
   `register_builtin_extensions` in
   [`extensions/mod.rs`](../extensions/mod.rs). See
   [`extensions/README.md`](../extensions/README.md) for the contract.
5. **Done.** `refresh_from_config_with_pi` will instantiate
   `OpenAICompatibleProvider` for the new id automatically the next
   time the registry is refreshed. No frontend code changes needed.

**Case 2 — New API shape (non-OpenAI-compatible)**

1. **Implement the provider struct** in
   [`providers/mod.rs`](mod.rs):
   - Manual `Debug` impl that redacts `api_key` + any `HeaderMap`
     (cargo-cult the impls at
     [`mod.rs:153`](mod.rs) and
     [`mod.rs:667`](mod.rs)).
   - Construct an `Arc<CircuitBreaker>` via the same
     `tunables::circuit_breaker_*` accessors so behaviour stays
     uniform.
   - Use `tunables::provider_timeout()` for the default reqwest
     timeout; honour `CallRequest::timeout` as a per-call override.
   - Audit every `tracing::` macro call site for header / body /
     api-key arguments — log `provider_name` and `model_id` only.
2. **Implement `Provider`** (and `StreamingProvider` if the API
   genuinely streams). Don't claim to stream if the API doesn't —
   return `None` from `as_streaming()` and let the engine fall back to
   buffered calls.
3. **Register in `ProviderRegistry::refresh_from_config_with_pi`**
   ([`mod.rs:1267`](mod.rs)). Add a new arm to the
   `match pc.provider_type.as_str()` that constructs and registers
   your provider. Log construction failures with `warn!` and skip
   rather than panic.
4. **Extend the whitelist** in
   [`commands/settings.rs::ALLOWED_PROVIDER_TYPES`](../commands/settings.rs:15)
   so `add_provider` accepts the new `provider_type` from the
   Settings UI.
5. **Seed defaults + catalog + pricing + extension** — same as steps
   1-4 of Case 1.
6. **Cost lookup** — if your provider populates `ModelInfo` correctly
   with `cost_per_1m_input` / `cost_per_1m_output`, the trait default
   handles cost. Otherwise add a `cost_per_1m_tokens` override on the
   `impl Provider` block to keep the per-provider source of truth.
7. **Tests** — at minimum: a Debug-redaction test (cargo-cult
   [`mod.rs:2012`](mod.rs)) and any parser unit tests for response
   shapes. The existing TcpListener-based mock server harness at
   [`mod.rs:2176`](mod.rs) shows how to exercise the streaming path
   without pulling in wiremock.

## Where things live at runtime

- **Registry instance**: held in
  `AppState::provider_registry: Arc<AsyncRwLock<ProviderRegistry>>`
  ([`app_state.rs:70`](../state/app_state.rs)).
- **API keys**: OS keychain via the `keyring` crate; cached in the
  encrypted `~/.hyvemind/.credentials` envelope so a single Keychain
  prompt covers all providers at launch
  (see [`state/README.md`](../state/README.md)).
- **TRACE-level request/response logging**: only when `HYVEMIND_DEBUG=1`,
  routed to
  `~/.hyvemind/debug/reviews/{review_id}.jsonl` (hivemind calls) or
  `~/.hyvemind/debug/sessions/{session_id}.jsonl` (chat) via
  [`PerIdRoutingLayer`](../state/log_routing.rs). Providers
  themselves never log header or body material — `OpenAICompatibleProvider`
  and `AnthropicProvider` carry `SAFETY (log redaction)` comments at
  every `tracing::` call site explaining why.

## See also

- [`../../../../CLAUDE.md`](../../../../CLAUDE.md) — IPC surface,
  log routing, debug investigation recipes.
- [`../../../../PRODUCT.md` §7 "Provider abstraction"](../../../../PRODUCT.md)
  — product framing for the five backends.
- [`../../../../CONTRIBUTING.md` §"Adding a provider"](../../../../CONTRIBUTING.md)
  — the short-form checklist; this README is the long form.
- [`../hivemind/README.md`](../hivemind/README.md) — the multi-model
  engine that is the heaviest consumer of this layer.
- [`../extensions/README.md`](../extensions/README.md) — provider
  extensions that surface credits / usage / auth probes for the same
  provider ids the registry dispatches to.
- [`../pi/README.md`](../pi/README.md) — `PiSubscriptionProvider`
  spawns Pi sessions through this pool.
- [`../state/README.md`](../state/README.md) — the `SecretStore` /
  config / OS-keychain plumbing behind `api_key` lookup.
