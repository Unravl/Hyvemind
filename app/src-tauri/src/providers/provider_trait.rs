//! Object-safe `Provider` and `StreamingProvider` traits for LLM backends.
//!
//! Replaces the legacy enum-dispatch with an `Arc<dyn Provider>`
//! pattern (audit 6.6). The split between `Provider` and `StreamingProvider`
//! lets callers that need progressive deltas opt in to streaming-capable
//! providers only, fixing the historical "drop the channel" bug at
//! `providers.rs:285-310` where non-streaming providers silently discarded
//! the supplied `mpsc::Sender<StreamChunk>`.
//!
//! ## Cost lookup
//!
//! `cost_per_1m_tokens(model_id)` returns `(input_per_1m, output_per_1m)` for
//! a given model ID, or `None` if the provider doesn't know the model. The
//! engine's `compute_cost(...)` helper iterates providers in the registry
//! and uses the first hit; if nobody knows it, the engine falls back to a
//! generic `(1.0, 5.0)` rate. This replaces the duplicate `match` table that
//! used to live at `engine.rs:3448` so a single source of truth lives on
//! each provider impl.
//!
//! ## Streaming
//!
//! `StreamingProvider::call_streaming(req, tx)` takes ownership of the
//! `mpsc::UnboundedSender<StreamChunk>`. With the trait split, only
//! streaming-capable providers (`OpenAICompatibleProvider`,
//! `OpenRouterProvider`, `OllamaProvider`, test `MockProvider`) accept a
//! sender at all — `AnthropicProvider` and `PiSubscriptionProvider` are
//! `Provider`-only and the engine routes through them via `call(...)` so
//! the channel is never silently dropped.

use std::time::Duration;

use anyhow::Result;
use async_trait::async_trait;
use tokio::sync::mpsc;

use crate::hivemind::review_schema::StructuredOutputConfig;

use super::{ModelResponse, StreamChunk};

/// Bundle of the eight arguments a non-streaming model call needs. Replaces
/// the long parameter lists on the old enum-dispatch `call(...)` and friends so
/// callers can build a request once and forward it through either the
/// `Provider` or `StreamingProvider` surface without re-typing every field.
///
/// All fields are optional except `model_id`, `system_prompt`, and
/// `user_prompt`. Providers ignore parameters they don't support (e.g.
/// `PiSubscriptionProvider` ignores `temperature`, `top_p`, `max_tokens`
/// because Pi mediates those upstream).
#[derive(Debug, Clone)]
pub struct CallRequest {
    pub model_id: String,
    pub system_prompt: String,
    pub user_prompt: String,
    pub temperature: Option<f64>,
    pub top_p: Option<f64>,
    pub max_tokens: Option<u32>,
    pub timeout: Option<Duration>,
    /// Optional structured-output envelope. When `Some`, providers that
    /// support tool-use inject the tools + tool_choice into the request
    /// body (Anthropic, OpenAI-compatible flavours). Providers that don't
    /// (PiSubscription, Mock) ignore this and fall through to plain `call`.
    pub structured: Option<StructuredOutputConfig>,
    /// Optional per-call thinking level (`"off"` | `"low"` | `"medium"` |
    /// `"high"`). Currently only honoured by `PiSubscriptionProvider`, which
    /// maps it onto Pi's `--thinking` flag so claude-sub / chatgpt reviewer
    /// calls match the per-step config the user picked instead of being
    /// silently forced to `off`. Other providers ignore it (they take
    /// thinking through their own provider-native fields).
    pub thinking: Option<String>,
    /// When `true`, providers that support explicit prompt caching
    /// (Anthropic today via `cache_control: { type: "ephemeral" }`) mark the
    /// static prefix — system prompt + tools — as cacheable. Providers
    /// without explicit cache markers (DeepSeek, every OpenAI-compatible
    /// endpoint) ignore this; their caching is automatic on byte-stable
    /// prefixes and needs no hint.
    pub cache_static_prefix: bool,
}

impl CallRequest {
    /// Build a request with the three mandatory fields and all optional
    /// sampling/structured fields left at their defaults.
    pub fn new(
        model_id: impl Into<String>,
        system_prompt: impl Into<String>,
        user_prompt: impl Into<String>,
    ) -> Self {
        Self {
            model_id: model_id.into(),
            system_prompt: system_prompt.into(),
            user_prompt: user_prompt.into(),
            temperature: None,
            top_p: None,
            max_tokens: None,
            timeout: None,
            structured: None,
            thinking: None,
            cache_static_prefix: false,
        }
    }

    pub fn with_temperature(mut self, t: Option<f64>) -> Self {
        self.temperature = t;
        self
    }
    pub fn with_top_p(mut self, t: Option<f64>) -> Self {
        self.top_p = t;
        self
    }
    pub fn with_max_tokens(mut self, m: Option<u32>) -> Self {
        self.max_tokens = m;
        self
    }
    pub fn with_timeout(mut self, t: Option<Duration>) -> Self {
        self.timeout = t;
        self
    }
    pub fn with_structured(mut self, s: Option<StructuredOutputConfig>) -> Self {
        self.structured = s;
        self
    }
    pub fn with_thinking(mut self, t: Option<String>) -> Self {
        self.thinking = t;
        self
    }
    pub fn with_cache_static_prefix(mut self, c: bool) -> Self {
        self.cache_static_prefix = c;
        self
    }
}

/// Object-safe LLM provider surface. Every backend (Anthropic, OpenAI-compat,
/// OpenRouter, Ollama, PiSubscription, Mock) implements this trait. The
/// engine stores them as `Arc<dyn Provider>` in the registry.
///
/// Streaming providers also implement [`StreamingProvider`] and surface
/// themselves through [`Provider::as_streaming`] so callers can opt in to
/// `call_streaming(...)` without an unsafe downcast.
#[async_trait]
pub trait Provider: Send + Sync + std::fmt::Debug {
    /// Non-streaming call. Returns the buffered final `ModelResponse`. When
    /// `req.structured` is `Some`, providers that support tool-use inject
    /// the envelope; providers that don't fall through to plain text.
    async fn call(&self, req: CallRequest) -> Result<ModelResponse>;

    /// Provider identifier as used in the registry (e.g. `"anthropic"`,
    /// `"openrouter"`, `"ollama"`, or any user-defined custom OpenAI-compat
    /// provider name).
    fn name(&self) -> &str;

    /// Upcast to [`StreamingProvider`] if this provider implements it.
    /// Returns `None` for providers that only support buffered calls
    /// (Anthropic, PiSubscription). The engine uses this to fan
    /// streaming-aware callers through `call_streaming(...)` without
    /// touching providers that would silently drop the channel.
    #[allow(dead_code)]
    fn as_streaming(&self) -> Option<&dyn StreamingProvider> {
        None
    }
}

/// Streaming-capable subset of [`Provider`]. Only providers that genuinely
/// emit progressive deltas implement this — the engine uses
/// [`Provider::as_streaming`] to decide whether a given call site can hand
/// off the `mpsc::UnboundedSender<StreamChunk>` or must fall back to a
/// buffered `call(...)` (which doesn't accept a sender at all).
///
/// This fixes the silent-drop bug at the old `providers.rs:285-310` where
/// `Anthropic` and `PiSubscription` accepted a `progress_tx` and immediately
/// `drop`'d it, leaving the receiver to wait for deltas that would never
/// arrive.
#[allow(dead_code)]
#[async_trait]
pub trait StreamingProvider: Provider {
    /// Stream a model completion, emitting `StreamChunk`s on `tx` as deltas
    /// arrive. Returns the buffered final `ModelResponse` on completion.
    /// The receiver may be dropped or full; implementations must not block
    /// on a stalled receiver — the existing OpenAI-compatible path uses
    /// `try_send` and counts drops via `STREAM_CHUNK_DROP_WARN`.
    async fn call_streaming(
        &self,
        req: CallRequest,
        tx: mpsc::UnboundedSender<StreamChunk>,
    ) -> Result<ModelResponse>;
}
