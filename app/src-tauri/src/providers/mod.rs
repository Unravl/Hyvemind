use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use eventsource_stream::Eventsource;
use futures_util::StreamExt;
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_TYPE};
use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

use crate::hivemind::circuit_breaker::CircuitBreaker;
use crate::tunables;

pub mod provider_trait;
pub use provider_trait::{CallRequest, Provider, StreamingProvider};

/// Inactivity timeout for SSE streams — if no chunk arrives within this
/// window, the stream is considered stalled and aborted with an error.
const INACTIVITY_TIMEOUT_SECS: u64 = 30;

/// Maximum total attempts when an SSE stream closes cleanly without yielding
/// any content or a usage frame. Some upstream providers (e.g. crof) have
/// been observed returning HTTP 200 + an empty SSE body for transient
/// reasons; retrying typically recovers on the second attempt.
const MAX_EMPTY_STREAM_ATTEMPTS: u32 = 3;

/// Base delay between empty-stream retries. Multiplied by the attempt number
/// for a linear ramp (500 ms, then 1000 ms) — kept tight so the user-facing
/// latency cost of all retries stays under ~2 s.
const EMPTY_STREAM_RETRY_BASE_DELAY_MS: u64 = 500;

// ---------------------------------------------------------------------------
// Core data types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelInfo {
    pub model_id: String,
    pub context_window: usize,
    pub cost_per_1m_input: f64,
    pub cost_per_1m_output: f64,
    pub supports_streaming: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ModelResponse {
    pub output: String,
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub model_id: String,
    pub duration_ms: u64,
    /// Tokens served from the provider's prompt cache on this call.
    /// Anthropic: `cache_read_input_tokens`. DeepSeek (OpenAI-compatible):
    /// `prompt_cache_hit_tokens`. Providers without a cache report 0.
    #[serde(default)]
    pub cache_hit_tokens: u64,
    /// Tokens written into the provider's prompt cache on this call.
    /// Anthropic: `cache_creation_input_tokens`. DeepSeek and other
    /// OpenAI-compatible backends report no equivalent (their cache is
    /// implicit) — leave 0.
    #[serde(default)]
    pub cache_write_tokens: u64,
}

/// A single token/text delta emitted by a streaming model call.
///
/// Sent over the `progress_tx` channel passed to `call_with_progress` so
/// callers can render progressive output and detect liveness.
#[derive(Debug, Clone)]
pub struct StreamChunk {
    pub delta: String,
}

/// Result of parsing a single SSE event payload from an OpenAI-compatible
/// streaming endpoint. Returned by [`parse_sse_event_data`].
#[derive(Debug, Default, PartialEq)]
pub(crate) struct ParsedSseEvent {
    /// Newly produced output text from `choices[0].delta.content`, if any.
    pub delta: Option<String>,
    /// Final-frame usage block (`prompt_tokens`, `completion_tokens`).
    pub usage: Option<(u32, u32)>,
    /// DeepSeek-specific cache-hit token count from `usage.prompt_cache_hit_tokens`
    /// on the final frame. `0` when the field is absent (every non-DeepSeek
    /// OpenAI-compatible endpoint we hit today).
    pub cache_hit_tokens: u64,
    /// `true` when the event payload is the literal sentinel `[DONE]`.
    pub done: bool,
}

/// Parse a single SSE `data:` payload from an OpenAI-compatible streaming
/// endpoint. Extracted as a free function so it can be unit-tested without
/// spinning up an HTTP server.
///
/// - Returns `Ok(ParsedSseEvent { done: true, .. })` when `data == "[DONE]"`.
/// - Returns `Ok(ParsedSseEvent::default())` for events we recognise but
///   that contribute nothing (e.g. role-only deltas, reasoning-only deltas).
/// - Returns `Err` if `data` is not valid JSON.
pub(crate) fn parse_sse_event_data(data: &str) -> Result<ParsedSseEvent> {
    let trimmed = data.trim();
    if trimmed == "[DONE]" {
        return Ok(ParsedSseEvent {
            done: true,
            ..Default::default()
        });
    }

    let value: serde_json::Value =
        serde_json::from_str(trimmed).context("invalid SSE JSON payload")?;

    let mut out = ParsedSseEvent::default();

    if let Some(content) = value
        .get("choices")
        .and_then(|c| c.get(0))
        .and_then(|c| c.get("delta"))
        .and_then(|d| d.get("content"))
        .and_then(|s| s.as_str())
    {
        if !content.is_empty() {
            out.delta = Some(content.to_string());
        }
    }

    if let Some(usage) = value.get("usage") {
        let pt = usage.get("prompt_tokens").and_then(|v| v.as_u64());
        let ct = usage.get("completion_tokens").and_then(|v| v.as_u64());
        if pt.is_some() || ct.is_some() {
            out.usage = Some((pt.unwrap_or(0) as u32, ct.unwrap_or(0) as u32));
        }
        // DeepSeek surfaces its automatic prefix-cache stats here. Absent on
        // every other OpenAI-compatible endpoint, in which case we leave 0.
        out.cache_hit_tokens = usage
            .get("prompt_cache_hit_tokens")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
    }

    Ok(out)
}

// ---------------------------------------------------------------------------
// Provider dispatch was migrated to `Arc<dyn Provider>` trait objects in
// audit 6.6 (was: closed-set enum dispatch). Provider lookup now happens
// through `Arc<dyn Provider>` (and `as_streaming()` for streaming callers)
// in `ProviderRegistry`. See `provider_trait.rs` for the trait definitions
// and the per-provider impls at the bottom of this file.
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// OpenAICompatibleProvider — base for OpenAI, OpenRouter, Ollama, custom
// ---------------------------------------------------------------------------

pub struct OpenAICompatibleProvider {
    client: reqwest::Client,
    base_url: String,
    api_key: String,
    provider_name: String,
    circuit_breaker: Arc<CircuitBreaker>,
    timeout: Duration,
    /// Extra headers to attach to every chat-completion request. Used by
    /// `OpenRouterProvider` to inject `HTTP-Referer` / `X-Title` without
    /// duplicating the request path.
    extra_headers: HeaderMap,
    /// Models on this endpoint that have rejected the structured-output
    /// `tool_choice: { function: ... }` form with a 400 (e.g. DeepSeek's
    /// `deepseek-reasoner` / `deepseek-v4-pro` thinking mode, certain
    /// Kimi-K2 deployments). Once observed, we send `tool_choice: "auto"`
    /// directly on subsequent calls so the rejection isn't paid each round.
    /// Mutex guards only HashSet ops (no `.await` held).
    auto_tool_choice_models: Arc<Mutex<HashSet<String>>>,
}

// Manual `Debug` impl that redacts `api_key` and `extra_headers`. The default
// derived impl would surface bearer-token material if a future `tracing::`
// call site ever Debug-formatted the provider (e.g. via `?provider` or
// `#[tracing::instrument]`). This is a structural defense — log sites in
// this file already avoid such formatting, but redacting at the source means
// even an accidental Debug-format can never leak a key.
impl std::fmt::Debug for OpenAICompatibleProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let auto_tc_len = self
            .auto_tool_choice_models
            .lock()
            .map(|s| s.len())
            .unwrap_or(0);
        f.debug_struct("OpenAICompatibleProvider")
            .field("base_url", &self.base_url)
            .field("api_key", &"[REDACTED]")
            .field("provider_name", &self.provider_name)
            .field("circuit_breaker", &self.circuit_breaker)
            .field("timeout", &self.timeout)
            .field("extra_headers_len", &self.extra_headers.len())
            .field("auto_tool_choice_models_len", &auto_tc_len)
            .finish_non_exhaustive()
    }
}

impl OpenAICompatibleProvider {
    pub fn new(
        base_url: impl Into<String>,
        api_key: impl Into<String>,
        provider_name: impl Into<String>,
        timeout: Option<Duration>,
    ) -> Result<Self> {
        Self::new_with_headers(base_url, api_key, provider_name, timeout, HeaderMap::new())
    }

    /// Construct a provider that attaches the given extra headers to every
    /// chat-completion request (in addition to `Content-Type` and the
    /// `Authorization: Bearer` header derived from `api_key`).
    ///
    /// Returns `Err` if the underlying reqwest client cannot be built (e.g.
    /// missing TLS backend). Callers in the registry surface this by
    /// skipping the provider so the app stays up instead of panicking.
    pub fn new_with_headers(
        base_url: impl Into<String>,
        api_key: impl Into<String>,
        provider_name: impl Into<String>,
        timeout: Option<Duration>,
        extra_headers: HeaderMap,
    ) -> Result<Self> {
        let timeout = timeout.unwrap_or_else(tunables::provider_timeout);
        let provider_name_owned: String = provider_name.into();
        let client = reqwest::Client::builder()
            .timeout(timeout)
            .build()
            .with_context(|| {
                format!(
                    "failed to build reqwest client for provider '{}'",
                    provider_name_owned
                )
            })?;
        Ok(Self {
            client,
            base_url: base_url.into().trim_end_matches('/').to_string(),
            api_key: api_key.into(),
            circuit_breaker: Arc::new(CircuitBreaker::new(
                provider_name_owned.clone(),
                tunables::circuit_breaker_threshold(),
                tunables::circuit_breaker_cooldown(),
            )),
            provider_name: provider_name_owned,
            timeout,
            extra_headers,
            auto_tool_choice_models: Arc::new(Mutex::new(HashSet::new())),
        })
    }

    fn build_payload(
        &self,
        model_id: &str,
        system_prompt: &str,
        user_prompt: &str,
        temperature: Option<f64>,
        top_p: Option<f64>,
        max_tokens: Option<u32>,
    ) -> serde_json::Value {
        let mut payload = serde_json::json!({
            "model": model_id,
            "messages": [
                { "role": "system", "content": system_prompt },
                { "role": "user", "content": user_prompt }
            ]
        });
        if let Some(temp) = temperature {
            payload["temperature"] = serde_json::json!(temp);
        }
        if let Some(tp) = top_p {
            payload["top_p"] = serde_json::json!(tp);
        }
        if let Some(mt) = max_tokens {
            payload["max_tokens"] = serde_json::json!(mt);
        }
        payload
    }

    /// Parse a buffered (non-streaming) OpenAI-compatible JSON response into
    /// `(output, input_tokens, output_tokens, cache_hit_tokens)`. Used both
    /// by the fallback path in `call_with_progress` (when an upstream server
    /// ignores `stream: true` and returns `application/json`), by
    /// `call_structured`, and by tests.
    ///
    /// `cache_hit_tokens` is DeepSeek's `usage.prompt_cache_hit_tokens`. Every
    /// other OpenAI-compatible endpoint we currently hit omits that field, in
    /// which case it falls through to `0`.
    fn parse_buffered_response(body: &serde_json::Value) -> (String, u32, u32, u64) {
        let output = body["choices"]
            .get(0)
            .and_then(|c| c["message"]["content"].as_str())
            .unwrap_or("")
            .to_string();
        let input_tokens = body["usage"]["prompt_tokens"].as_u64().unwrap_or(0) as u32;
        let output_tokens = body["usage"]["completion_tokens"].as_u64().unwrap_or(0) as u32;
        let cache_hit_tokens = body["usage"]["prompt_cache_hit_tokens"]
            .as_u64()
            .unwrap_or(0);
        (output, input_tokens, output_tokens, cache_hit_tokens)
    }

    pub async fn call(
        &self,
        model_id: &str,
        system_prompt: &str,
        user_prompt: &str,
        temperature: Option<f64>,
        top_p: Option<f64>,
        max_tokens: Option<u32>,
        timeout: Option<Duration>,
    ) -> Result<ModelResponse> {
        self.call_with_progress(
            model_id,
            system_prompt,
            user_prompt,
            temperature,
            top_p,
            max_tokens,
            None,
            timeout,
        )
        .await
    }

    /// Phase 5: structured-output variant. Issues a buffered (non-streaming)
    /// chat-completion request with `tools` + `tool_choice` injected, and
    /// returns the model's `tool_calls[0].function.arguments` string (or
    /// the plain `message.content` if the model ignored `tool_choice`).
    ///
    /// Non-streaming on purpose: parsing `tool_calls` deltas across SSE
    /// frames varies per upstream (OpenAI proper, OpenRouter passthrough,
    /// Ollama) and reviewer outputs are small enough that the latency cost
    /// of buffering is acceptable. The progress channel doesn't apply when
    /// the response is a single JSON envelope.
    pub async fn call_structured(
        &self,
        model_id: &str,
        system_prompt: &str,
        user_prompt: &str,
        temperature: Option<f64>,
        top_p: Option<f64>,
        max_tokens: Option<u32>,
        timeout: Option<Duration>,
        structured: &crate::hivemind::review_schema::StructuredOutputConfig,
    ) -> Result<ModelResponse> {
        self.circuit_breaker
            .before_request()
            .await
            .map_err(|e| anyhow!("{}", e))?;

        let url = format!("{}/chat/completions", self.base_url);
        let mut payload = self.build_payload(
            model_id,
            system_prompt,
            user_prompt,
            temperature,
            top_p,
            max_tokens,
        );
        payload["tools"] = serde_json::json!(structured.tools);

        // If we've already learned this model rejects a forced tool_choice
        // (e.g. DeepSeek's deepseek-reasoner / deepseek-v4-pro thinking mode,
        // certain Kimi-K2 deployments), skip the rejection round-trip and
        // start with "auto". The model's system prompt steers it toward the
        // single injected tool; the engine's fallback parser handles the
        // rare case where it produces plain text instead.
        //
        // CACHE STABILITY: once a model has been recorded in
        // `auto_tool_choice_models`, every subsequent call to it sends the
        // identical `tool_choice: "auto"` byte string — the same payload
        // shape across calls is what lets DeepSeek's automatic prefix cache
        // hit. The only call that differs is the very first one for a
        // freshly-seen problematic model (it issues the forced form, gets
        // rejected, then retries auto). Calls 2..N share the prefix.
        let model_prefers_auto = self
            .auto_tool_choice_models
            .lock()
            .map(|s| s.contains(model_id))
            .unwrap_or(false);
        payload["tool_choice"] = if model_prefers_auto {
            serde_json::json!("auto")
        } else {
            structured.tool_choice.clone()
        };

        debug!(
            provider = %self.provider_name,
            model = %model_id,
            tool_choice_auto = model_prefers_auto,
            "Sending structured request to OpenAI-compatible endpoint"
        );

        let start = Instant::now();

        let response = self
            .send_structured(&url, &payload, timeout)
            .await
            .context("Failed to send structured request")?;

        // Inspect the first response. If the server rejected our forced
        // tool_choice with a 400 that we recognise, do NOT trip the circuit
        // breaker and do NOT surface the error — silently retry once with
        // tool_choice: "auto". Anything else (other 4xx, 5xx) is a real
        // failure: record_failure + return Err as usual so Nurse can react.
        let response = if !response.status().is_success() && !model_prefers_auto {
            let status = response.status();
            let body = response
                .text()
                .await
                .unwrap_or_else(|_| "unable to read body".into());
            if status == reqwest::StatusCode::BAD_REQUEST && is_tool_choice_unsupported(&body) {
                // Cache so we don't pay the rejection again for this model.
                if let Ok(mut s) = self.auto_tool_choice_models.lock() {
                    s.insert(model_id.to_string());
                }
                info!(
                    provider = %self.provider_name,
                    model = %model_id,
                    "endpoint rejected forced tool_choice; retrying with tool_choice: \"auto\""
                );
                payload["tool_choice"] = serde_json::json!("auto");
                self.send_structured(&url, &payload, timeout)
                    .await
                    .context("Failed to send structured request (auto fallback)")?
            } else {
                self.circuit_breaker.record_failure().await;
                return Err(anyhow!("Provider error ({}): {}", status, body));
            }
        } else {
            response
        };

        let duration_ms = start.elapsed().as_millis() as u64;

        if !response.status().is_success() {
            let status = response.status();
            let body = response
                .text()
                .await
                .unwrap_or_else(|_| "unable to read body".into());
            self.circuit_breaker.record_failure().await;
            return Err(anyhow!("Provider error ({}): {}", status, body));
        }

        let resp_body: serde_json::Value = response
            .json()
            .await
            .context("Failed to parse structured response body")?;

        let mut output = extract_openai_tool_call_or_text(&resp_body);
        let (_text, mut input_tokens, mut output_tokens, mut cache_hit_tokens) =
            OpenAICompatibleProvider::parse_buffered_response(&resp_body);

        // Second recovery path: HTTP returned 200 but the body had neither
        // tool_calls[0].function.arguments nor message.content (observed on
        // Kimi-K2.6 via neuralwatt — the upstream accepts our forced
        // tool_choice but emits an empty envelope). If we used the forced
        // form, retry once with "auto". Same Nurse-quiet pattern as the
        // 400 fix: success leaves the engine seeing a single clean call.
        if output.trim().is_empty() && !model_prefers_auto {
            info!(
                provider = %self.provider_name,
                model = %model_id,
                body_summary = %summarize_empty_response(&resp_body),
                "endpoint returned 200 with empty structured output; retrying with tool_choice: \"auto\""
            );
            if let Ok(mut s) = self.auto_tool_choice_models.lock() {
                s.insert(model_id.to_string());
            }
            payload["tool_choice"] = serde_json::json!("auto");
            let retry_response = self
                .send_structured(&url, &payload, timeout)
                .await
                .context("Failed to send structured request (empty auto fallback)")?;
            if retry_response.status().is_success() {
                let retry_body: serde_json::Value = retry_response
                    .json()
                    .await
                    .context("Failed to parse structured response body (empty auto fallback)")?;
                output = extract_openai_tool_call_or_text(&retry_body);
                let (_t, i, o, c) = OpenAICompatibleProvider::parse_buffered_response(&retry_body);
                input_tokens = i;
                output_tokens = o;
                cache_hit_tokens = c;
                if output.trim().is_empty() {
                    warn!(
                        provider = %self.provider_name,
                        model = %model_id,
                        body_summary = %summarize_empty_response(&retry_body),
                        "endpoint returned 200 with empty output on both forced and auto tool_choice"
                    );
                }
            } else {
                let status = retry_response.status();
                let body = retry_response
                    .text()
                    .await
                    .unwrap_or_else(|_| "unable to read body".into());
                self.circuit_breaker.record_failure().await;
                return Err(anyhow!("Provider error ({}): {}", status, body));
            }
        } else if output.trim().is_empty() {
            // Already on auto (cache hit) and still empty. Log the body
            // summary so the user can see what came back — the engine will
            // raise the user-facing "empty response" error from this same
            // ModelResponse.
            warn!(
                provider = %self.provider_name,
                model = %model_id,
                body_summary = %summarize_empty_response(&resp_body),
                "endpoint returned 200 with empty output (auto tool_choice already in use)"
            );
        }

        self.circuit_breaker.record_success().await;

        Ok(ModelResponse {
            output,
            input_tokens,
            output_tokens,
            model_id: model_id.to_string(),
            duration_ms,
            cache_hit_tokens,
            cache_write_tokens: 0,
        })
    }

    /// Build and send a structured chat-completion request. Extracted from
    /// `call_structured` so the same wire path can be reused for the
    /// tool_choice-rejection retry without duplicating header / timeout
    /// boilerplate.
    async fn send_structured(
        &self,
        url: &str,
        payload: &serde_json::Value,
        timeout: Option<Duration>,
    ) -> reqwest::Result<reqwest::Response> {
        let mut request = self
            .client
            .post(url)
            .header(CONTENT_TYPE, "application/json");
        if !self.api_key.is_empty() {
            request = request.header(AUTHORIZATION, format!("Bearer {}", self.api_key));
        }
        if !self.extra_headers.is_empty() {
            request = request.headers(self.extra_headers.clone());
        }
        if let Some(t) = timeout {
            request = request.timeout(t);
        }
        request.json(payload).send().await
    }

    /// Stream a chat-completion call against the OpenAI-compatible endpoint.
    ///
    /// If `progress_tx` is `Some`, a [`StreamChunk`] is sent for each
    /// `choices[0].delta.content` fragment as it arrives. The channel is
    /// _not_ closed by this function — drop the receiver to stop listening.
    ///
    /// Detects upstream stalls with a `INACTIVITY_TIMEOUT_SECS` watchdog on
    /// the SSE byte stream. Falls back to buffered JSON parsing when the
    /// server ignores `stream: true` and returns a non-`text/event-stream`
    /// response.
    pub async fn call_with_progress(
        &self,
        model_id: &str,
        system_prompt: &str,
        user_prompt: &str,
        temperature: Option<f64>,
        top_p: Option<f64>,
        max_tokens: Option<u32>,
        progress_tx: Option<tokio::sync::mpsc::UnboundedSender<StreamChunk>>,
        timeout: Option<Duration>,
    ) -> Result<ModelResponse> {
        self.circuit_breaker
            .before_request()
            .await
            .map_err(|e| anyhow!("{}", e))?;

        let url = format!("{}/chat/completions", self.base_url);
        let mut payload = self.build_payload(
            model_id,
            system_prompt,
            user_prompt,
            temperature,
            top_p,
            max_tokens,
        );
        payload["stream"] = serde_json::json!(true);
        payload["stream_options"] = serde_json::json!({ "include_usage": true });

        // SAFETY (log redaction): `url` is `{base_url}/chat/completions` —
        // base_url is configured at provider-construction time and never
        // contains the api_key (auth is attached only via the Authorization
        // header below). No header map / request body is logged here.
        debug!(
            provider = %self.provider_name,
            model = %model_id,
            url = %url,
            "Sending streaming request to OpenAI-compatible endpoint"
        );

        let start = Instant::now();
        let mut attempt: u32 = 0;

        loop {
            attempt += 1;

            let mut request = self
                .client
                .post(&url)
                .header(CONTENT_TYPE, "application/json");
            if !self.api_key.is_empty() {
                request = request.header(AUTHORIZATION, format!("Bearer {}", self.api_key));
            }
            if !self.extra_headers.is_empty() {
                request = request.headers(self.extra_headers.clone());
            }
            if let Some(t) = timeout {
                request = request.timeout(t);
            }

            let response = request
                .json(&payload)
                .send()
                .await
                .context("Failed to send request")?;

            if !response.status().is_success() {
                let status = response.status();
                let body = response
                    .text()
                    .await
                    .unwrap_or_else(|_| "unable to read body".into());
                self.circuit_breaker.record_failure().await;
                return Err(anyhow!(
                    "{} API error ({}): {}",
                    self.provider_name,
                    status,
                    body
                ));
            }

            // Some endpoints ignore `stream: true` and return a normal JSON body.
            // Detect that via Content-Type and fall back to the buffered path.
            let is_sse = response
                .headers()
                .get(CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .map(|ct| ct.contains("text/event-stream"))
                .unwrap_or(false);

            if !is_sse {
                // SAFETY (log redaction): only provider_name + model_id are logged;
                // no headers, response body, or auth material reaches the log.
                debug!(
                    provider = %self.provider_name,
                    model = %model_id,
                    "stream requested but server returned non-SSE response — falling back to buffered JSON"
                );
                let body: serde_json::Value = response
                    .json()
                    .await
                    .context("Failed to parse response JSON")?;
                let (output, input_tokens, output_tokens, cache_hit_tokens) =
                    Self::parse_buffered_response(&body);
                self.circuit_breaker.record_success().await;
                let duration_ms = start.elapsed().as_millis() as u64;
                return Ok(ModelResponse {
                    output,
                    input_tokens,
                    output_tokens,
                    model_id: model_id.to_string(),
                    duration_ms,
                    cache_hit_tokens,
                    cache_write_tokens: 0,
                });
            }

            // SSE path -------------------------------------------------------
            let mut accumulator = String::new();
            let mut input_tokens: u32 = 0;
            let mut output_tokens: u32 = 0;
            let mut cache_hit_tokens: u64 = 0;
            let mut got_usage = false;

            let mut stream = response.bytes_stream().eventsource();
            let inactivity = Duration::from_secs(INACTIVITY_TIMEOUT_SECS);

            loop {
                let next = match tokio::time::timeout(inactivity, stream.next()).await {
                    Ok(item) => item,
                    Err(_) => {
                        self.circuit_breaker.record_failure().await;
                        // SAFETY (log redaction): only provider_name, model_id,
                        // and the literal timeout constant are logged. No header
                        // map, request body, or stream contents reach the log.
                        warn!(
                            provider = %self.provider_name,
                            model = %model_id,
                            timeout_secs = INACTIVITY_TIMEOUT_SECS,
                            "SSE stream stalled — no chunks received within inactivity window"
                        );
                        return Err(anyhow!(
                            "{} stream stalled — no chunks for {}s (model={})",
                            self.provider_name,
                            INACTIVITY_TIMEOUT_SECS,
                            model_id
                        ));
                    }
                };

                let event = match next {
                    Some(Ok(ev)) => ev,
                    Some(Err(e)) => {
                        self.circuit_breaker.record_failure().await;
                        return Err(anyhow!(
                            "{} SSE parse error (model={}): {}",
                            self.provider_name,
                            model_id,
                            e
                        ));
                    }
                    None => break, // stream ended
                };

                let parsed = match parse_sse_event_data(&event.data) {
                    Ok(p) => p,
                    Err(e) => {
                        // SAFETY (log redaction): `data_preview` is the first
                        // 200 chars of the SSE event payload (model output text),
                        // never a header value. No Authorization / x-api-key
                        // material reaches the log.
                        debug!(
                            provider = %self.provider_name,
                            model = %model_id,
                            error = %e,
                            data_preview = %event.data.chars().take(200).collect::<String>(),
                            "skipping malformed SSE event"
                        );
                        continue;
                    }
                };

                if parsed.done {
                    break;
                }

                if let Some(delta) = parsed.delta {
                    if let Some(tx) = progress_tx.as_ref() {
                        // Receiver may have dropped — `send` on an unbounded
                        // channel only fails when the receiver is gone, in
                        // which case we silently swallow the error. The final
                        // ModelResponse is still constructed from
                        // `accumulator`, so the call returns OK.
                        let _ = tx.send(StreamChunk {
                            delta: delta.clone(),
                        });
                    }
                    accumulator.push_str(&delta);
                }

                if let Some((pt, ct)) = parsed.usage {
                    input_tokens = pt;
                    output_tokens = ct;
                    got_usage = true;
                }
                if parsed.cache_hit_tokens > 0 {
                    cache_hit_tokens = parsed.cache_hit_tokens;
                }
            }

            // Empty-stream detection: some upstream providers terminate SSE
            // cleanly without yielding any content or a usage frame. Treat
            // that as a transient failure and retry up to MAX_EMPTY_STREAM_ATTEMPTS.
            // (No deltas have been sent through `progress_tx` in this case
            // since `accumulator` is empty, so retrying is safe.)
            if accumulator.is_empty() && !got_usage {
                if attempt < MAX_EMPTY_STREAM_ATTEMPTS {
                    let delay =
                        Duration::from_millis(EMPTY_STREAM_RETRY_BASE_DELAY_MS * attempt as u64);
                    warn!(
                        provider = %self.provider_name,
                        model = %model_id,
                        attempt = attempt,
                        max_attempts = MAX_EMPTY_STREAM_ATTEMPTS,
                        delay_ms = delay.as_millis() as u64,
                        "SSE stream returned no content — retrying"
                    );
                    tokio::time::sleep(delay).await;
                    continue;
                }
                self.circuit_breaker.record_failure().await;
                warn!(
                    provider = %self.provider_name,
                    model = %model_id,
                    attempts = attempt,
                    "SSE stream returned no content after exhausting retries — failing"
                );
                return Err(anyhow!(
                    "{} returned an empty SSE stream after {} attempts (model={})",
                    self.provider_name,
                    attempt,
                    model_id
                ));
            }

            self.circuit_breaker.record_success().await;
            let duration_ms = start.elapsed().as_millis() as u64;

            if !got_usage {
                // SAFETY (log redaction): only provider_name + model_id reach the
                // log. No headers / response body emitted.
                debug!(
                    provider = %self.provider_name,
                    model = %model_id,
                    "SSE stream ended without a usage frame — token counts will be 0"
                );
            }

            return Ok(ModelResponse {
                output: accumulator,
                input_tokens,
                output_tokens,
                model_id: model_id.to_string(),
                duration_ms,
                cache_hit_tokens,
                cache_write_tokens: 0,
            });
        }
    }

    pub fn name(&self) -> &str {
        &self.provider_name
    }
}

// ---------------------------------------------------------------------------
// AnthropicProvider — native Anthropic Messages API (NOT OpenAI-compatible)
// ---------------------------------------------------------------------------

pub struct AnthropicProvider {
    client: reqwest::Client,
    api_key: String,
    circuit_breaker: Arc<CircuitBreaker>,
    #[allow(dead_code)]
    timeout: Duration,
}

// See note on `OpenAICompatibleProvider`'s manual Debug impl — same rationale.
impl std::fmt::Debug for AnthropicProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AnthropicProvider")
            .field("api_key", &"[REDACTED]")
            .field("circuit_breaker", &self.circuit_breaker)
            .field("timeout", &self.timeout)
            .finish_non_exhaustive()
    }
}

impl AnthropicProvider {
    /// Returns `Err` if the underlying reqwest client cannot be built. The
    /// provider registry surfaces this by skipping the Anthropic provider
    /// so the app stays up instead of panicking at startup.
    pub fn new(api_key: impl Into<String>, timeout: Option<Duration>) -> Result<Self> {
        let timeout = timeout.unwrap_or_else(tunables::provider_timeout);

        let client = reqwest::Client::builder()
            .timeout(timeout)
            .build()
            .context("failed to build reqwest client for provider 'anthropic'")?;

        Ok(Self {
            client,
            api_key: api_key.into(),
            circuit_breaker: Arc::new(CircuitBreaker::new(
                "anthropic",
                tunables::circuit_breaker_threshold(),
                tunables::circuit_breaker_cooldown(),
            )),
            timeout,
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn call(
        &self,
        model_id: &str,
        system_prompt: &str,
        user_prompt: &str,
        temperature: Option<f64>,
        top_p: Option<f64>,
        max_tokens: Option<u32>,
        timeout: Option<Duration>,
        cache_static_prefix: bool,
    ) -> Result<ModelResponse> {
        self.circuit_breaker
            .before_request()
            .await
            .map_err(|e| anyhow!("{}", e))?;

        let max_tokens = max_tokens.unwrap_or_else(tunables::default_max_tokens);

        let system_value = anthropic_system_value(system_prompt, cache_static_prefix);
        let mut body = serde_json::json!({
            "model": model_id,
            "system": system_value,
            "messages": [
                { "role": "user", "content": user_prompt }
            ],
            "max_tokens": max_tokens
        });

        if let Some(temp) = temperature {
            body["temperature"] = serde_json::json!(temp);
        }

        if let Some(tp) = top_p {
            body["top_p"] = serde_json::json!(tp);
        }

        // SAFETY (log redaction): only `model_id` is logged. The `x-api-key`
        // header is attached on the request builder below but never reaches
        // any log macro.
        debug!(model = %model_id, cache_static_prefix, "Sending request to Anthropic API");
        let start = Instant::now();

        let mut request = self
            .client
            .post("https://api.anthropic.com/v1/messages")
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", "2023-06-01")
            .header(CONTENT_TYPE, "application/json");
        if let Some(t) = timeout {
            request = request.timeout(t);
        }
        let response = request
            .json(&body)
            .send()
            .await
            .context("Failed to send request to Anthropic")?;

        let duration_ms = start.elapsed().as_millis() as u64;

        if !response.status().is_success() {
            let status = response.status();
            let body = response
                .text()
                .await
                .unwrap_or_else(|_| "unable to read body".into());
            self.circuit_breaker.record_failure().await;
            return Err(anyhow!("Anthropic API error ({}): {}", status, body));
        }

        let resp_body: serde_json::Value = response
            .json()
            .await
            .context("Failed to parse Anthropic response")?;

        let output = resp_body["content"]
            .as_array()
            .and_then(|arr| arr.first())
            .and_then(|block| block["text"].as_str())
            .unwrap_or("")
            .to_string();

        // Token adjustment for cache-related fields in the Anthropic API.
        // If a streaming Anthropic path is added in the future (via
        // call_with_progress), this same adjustment must be applied to the
        // usage frame in the final SSE event.
        let input_tokens_raw = resp_body["usage"]["input_tokens"].as_u64().unwrap_or(0);
        let cache_creation = resp_body["usage"]["cache_creation_input_tokens"]
            .as_u64()
            .unwrap_or(0);
        let cache_read = resp_body["usage"]["cache_read_input_tokens"]
            .as_u64()
            .unwrap_or(0);
        let adjusted = input_tokens_raw
            .saturating_sub(cache_creation)
            .saturating_sub(cache_read);
        let input_tokens = adjusted as u32;

        // Log when cache-related fields are absent so API changes are detectable
        if resp_body["usage"]
            .get("cache_creation_input_tokens")
            .is_none()
        {
            // SAFETY (log redaction): only `model_id` is logged — never the
            // response body or any header.
            debug!(
                model = %model_id,
                "Anthropic response missing cache_creation_input_tokens — may indicate API schema change"
            );
        }

        let output_tokens = resp_body["usage"]["output_tokens"].as_u64().unwrap_or(0) as u32;

        self.circuit_breaker.record_success().await;

        Ok(ModelResponse {
            output,
            input_tokens,
            output_tokens,
            model_id: model_id.to_string(),
            duration_ms,
            cache_hit_tokens: cache_read,
            cache_write_tokens: cache_creation,
        })
    }

    /// Phase 5: structured-output variant of `call`. Injects the supplied
    /// `tools` array and `tool_choice` into the Anthropic request body and
    /// returns a `ModelResponse` whose `output` is the JSON-stringified
    /// `tool_use.input` (if the model called the tool) or the text content
    /// (fallback, identical to `call`).
    ///
    /// Callers deserialise the JSON into their schema type (e.g.
    /// `StructuredReview`) and render to markdown via the schema's
    /// `to_markdown()`. The merge orchestrator only ever sees markdown.
    #[allow(clippy::too_many_arguments)]
    pub async fn call_structured(
        &self,
        model_id: &str,
        system_prompt: &str,
        user_prompt: &str,
        temperature: Option<f64>,
        top_p: Option<f64>,
        max_tokens: Option<u32>,
        timeout: Option<Duration>,
        structured: &crate::hivemind::review_schema::StructuredOutputConfig,
        cache_static_prefix: bool,
    ) -> Result<ModelResponse> {
        self.circuit_breaker
            .before_request()
            .await
            .map_err(|e| anyhow!("{}", e))?;

        let max_tokens = max_tokens.unwrap_or(4096);

        let system_value = anthropic_system_value(system_prompt, cache_static_prefix);
        let tools_value = anthropic_tools_with_cache(&structured.tools, cache_static_prefix);

        let mut body = serde_json::json!({
            "model": model_id,
            "system": system_value,
            "messages": [{ "role": "user", "content": user_prompt }],
            "max_tokens": max_tokens,
            "tools": tools_value,
            "tool_choice": structured.tool_choice,
        });
        if let Some(temp) = temperature {
            body["temperature"] = serde_json::json!(temp);
        }
        if let Some(tp) = top_p {
            body["top_p"] = serde_json::json!(tp);
        }

        debug!(model = %model_id, cache_static_prefix, "Sending structured request to Anthropic API");
        let start = Instant::now();

        let mut request = self
            .client
            .post("https://api.anthropic.com/v1/messages")
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", "2023-06-01")
            .header(CONTENT_TYPE, "application/json");
        if let Some(t) = timeout {
            request = request.timeout(t);
        }
        let response = request
            .json(&body)
            .send()
            .await
            .context("Failed to send structured request to Anthropic")?;

        let duration_ms = start.elapsed().as_millis() as u64;

        if !response.status().is_success() {
            let status = response.status();
            let body = response
                .text()
                .await
                .unwrap_or_else(|_| "unable to read body".into());
            self.circuit_breaker.record_failure().await;
            return Err(anyhow!("Anthropic API error ({}): {}", status, body));
        }

        let resp_body: serde_json::Value = response
            .json()
            .await
            .context("Failed to parse structured Anthropic response")?;

        // Tool-use blocks come before text in Anthropic responses when the
        // model honoured `tool_choice`. Walk every content block, prefer
        // the first `tool_use`; fall back to text if nothing matched (model
        // ignored tool_choice — rare, but possible for non-tool-trained
        // models). Every structured request only exposes one tool so the
        // first tool_use is always the one we asked for.
        let output = extract_anthropic_tool_use_or_text(&resp_body);

        let input_tokens_raw = resp_body["usage"]["input_tokens"].as_u64().unwrap_or(0);
        let cache_creation = resp_body["usage"]["cache_creation_input_tokens"]
            .as_u64()
            .unwrap_or(0);
        let cache_read = resp_body["usage"]["cache_read_input_tokens"]
            .as_u64()
            .unwrap_or(0);
        let adjusted = input_tokens_raw
            .saturating_sub(cache_creation)
            .saturating_sub(cache_read);
        let input_tokens = adjusted as u32;
        let output_tokens = resp_body["usage"]["output_tokens"].as_u64().unwrap_or(0) as u32;

        self.circuit_breaker.record_success().await;

        Ok(ModelResponse {
            output,
            input_tokens,
            output_tokens,
            model_id: model_id.to_string(),
            duration_ms,
            cache_hit_tokens: cache_read,
            cache_write_tokens: cache_creation,
        })
    }

    pub fn name(&self) -> &str {
        "anthropic"
    }
}

// ---------------------------------------------------------------------------
// OpenRouterProvider — wraps OpenAICompatibleProvider with custom headers
//   and reasoning format ({ "reasoning": { "effort": value } })
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub struct OpenRouterProvider {
    inner: OpenAICompatibleProvider,
}

impl OpenRouterProvider {
    /// Returns `Err` if the inner reqwest client cannot be built.
    pub fn new(api_key: impl Into<String>, timeout: Option<Duration>) -> Result<Self> {
        // Option A: plumb the OpenRouter-specific headers through
        // OpenAICompatibleProvider's `extra_headers` field. This lets us
        // delegate `call_with_progress` directly and inherit SSE streaming
        // + the inactivity watchdog without duplicating the request path.
        let mut extra = HeaderMap::new();
        extra.insert(
            "HTTP-Referer",
            HeaderValue::from_static("https://hyvemind.app"),
        );
        extra.insert("X-Title", HeaderValue::from_static("Hyvemind"));

        let inner = OpenAICompatibleProvider::new_with_headers(
            "https://openrouter.ai/api/v1",
            api_key,
            "openrouter",
            timeout,
            extra,
        )?;
        Ok(Self { inner })
    }

    pub async fn call(
        &self,
        model_id: &str,
        system_prompt: &str,
        user_prompt: &str,
        temperature: Option<f64>,
        top_p: Option<f64>,
        max_tokens: Option<u32>,
        timeout: Option<Duration>,
    ) -> Result<ModelResponse> {
        self.call_with_progress(
            model_id,
            system_prompt,
            user_prompt,
            temperature,
            top_p,
            max_tokens,
            None,
            timeout,
        )
        .await
    }

    pub async fn call_with_progress(
        &self,
        model_id: &str,
        system_prompt: &str,
        user_prompt: &str,
        temperature: Option<f64>,
        top_p: Option<f64>,
        max_tokens: Option<u32>,
        progress_tx: Option<tokio::sync::mpsc::UnboundedSender<StreamChunk>>,
        timeout: Option<Duration>,
    ) -> Result<ModelResponse> {
        self.inner
            .call_with_progress(
                model_id,
                system_prompt,
                user_prompt,
                temperature,
                top_p,
                max_tokens,
                progress_tx,
                timeout,
            )
            .await
    }

    /// Phase 5: structured-output via the inner OpenAI-compatible client.
    /// OpenRouter passes the `tools` array through to the upstream provider
    /// when the chosen model supports it. Models that ignore `tool_choice`
    /// produce plain `message.content`, which the engine falls back on.
    #[allow(clippy::too_many_arguments)]
    pub async fn call_structured(
        &self,
        model_id: &str,
        system_prompt: &str,
        user_prompt: &str,
        temperature: Option<f64>,
        top_p: Option<f64>,
        max_tokens: Option<u32>,
        timeout: Option<Duration>,
        structured: &crate::hivemind::review_schema::StructuredOutputConfig,
    ) -> Result<ModelResponse> {
        self.inner
            .call_structured(
                model_id,
                system_prompt,
                user_prompt,
                temperature,
                top_p,
                max_tokens,
                timeout,
                structured,
            )
            .await
    }

    pub fn name(&self) -> &str {
        "openrouter"
    }
}

// ---------------------------------------------------------------------------
// PiSubscriptionProvider — routes through Pi sessions for subscription auth
// ---------------------------------------------------------------------------

use crate::pi::manager::PiManager;
use crate::pi::rpc::{PiSessionOptions, ThinkingLevel, ToolSet};

pub struct PiSubscriptionProvider {
    provider_name: String,
    pi_manager: Arc<PiManager>,
    models: Vec<ModelInfo>,
}

impl std::fmt::Debug for PiSubscriptionProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PiSubscriptionProvider")
            .field("provider_name", &self.provider_name)
            .field("models", &self.models.len())
            .finish_non_exhaustive()
    }
}

impl PiSubscriptionProvider {
    pub fn new(
        provider_name: impl Into<String>,
        pi_manager: Arc<PiManager>,
        models: Vec<ModelInfo>,
    ) -> Self {
        Self {
            provider_name: provider_name.into(),
            pi_manager,
            models,
        }
    }

    /// Map our internal provider name to Pi's native provider/model format.
    fn pi_model_string(&self, model_id: &str) -> String {
        match self.provider_name.as_str() {
            "chatgpt" => format!("openai-codex/{}", model_id),
            "claude-sub" => format!("anthropic/{}", model_id),
            other => format!("{}/{}", other, model_id),
        }
    }

    #[allow(dead_code, clippy::too_many_arguments)]
    pub async fn call(
        &self,
        model_id: &str,
        system_prompt: &str,
        user_prompt: &str,
        _temperature: Option<f64>,
        _top_p: Option<f64>,
        _max_tokens: Option<u32>,
        timeout: Option<Duration>,
    ) -> Result<ModelResponse> {
        self.call_inner(model_id, system_prompt, user_prompt, None, false, timeout)
            .await
    }

    /// Internal call helper.
    ///
    /// Shaped to match the Tasks-view spawn path (`commands/chat.rs`) so a
    /// Pi-subscription reviewer call looks identical on the wire to a
    /// planning call: real `--system-prompt` argv, request-supplied
    /// `--thinking`, and a real `--session` file. The earlier shape (system
    /// prompt stuffed into the user message, `--thinking off` hardcoded,
    /// `--no-session`) caused Anthropic's subscription gateway to reject
    /// the request with a misleading "out of extra usage" 400 — see the
    /// matching comment at `commands/chat.rs:587`.
    ///
    /// When `structured` is `true`, the spawned Pi session is restricted to
    /// the `submit_review` extension tool and the system prompt is augmented
    /// to require a call to it. After `collect_response`, the captured tool
    /// args are JSON-stringified and returned as `ModelResponse.output` so
    /// the reviewer pipeline sees the same shape as Anthropic/OpenAI.
    async fn call_inner(
        &self,
        model_id: &str,
        system_prompt: &str,
        user_prompt: &str,
        thinking: Option<String>,
        structured: bool,
        timeout: Option<Duration>,
    ) -> Result<ModelResponse> {
        let full_model = self.pi_model_string(model_id);
        let session_id = format!("sub-{}", uuid::Uuid::new_v4());

        let working_dir = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));

        // Mirror Tasks: write the transcript under `~/.hyvemind/chat-sessions/`
        // so Pi gets a real `--session <file>` argv instead of `--no-session`.
        // The file is removed after `kill_session` returns — reviewer sessions
        // are never resumed and we don't want chat-sessions/ to grow unbounded.
        let session_path = dirs::home_dir().map(|h| {
            h.join(".hyvemind")
                .join("chat-sessions")
                .join(format!("{}.jsonl", session_id))
        });
        if let Some(ref p) = session_path {
            if let Some(parent) = p.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
        }

        let thinking_level = thinking
            .as_deref()
            .and_then(|s| s.parse::<ThinkingLevel>().ok())
            .unwrap_or_default();

        let structured_system_prompt = if structured {
            format!(
                "{}\n\n## Output\n\nYou MUST end the run by calling the `submit_review` tool with your structured review. There is no fallback — your text response is ignored.",
                system_prompt
            )
        } else {
            system_prompt.to_string()
        };

        let mut options = PiSessionOptions::for_model(&full_model)
            .with_thinking_level(thinking_level)
            .with_system_prompt(structured_system_prompt);
        if let Some(ref p) = session_path {
            options = options.with_session_file(p.display().to_string());
        }
        if structured {
            // Restrict the session to the structured-review tool so the
            // model has no other tool to call. The default `CodingTools`
            // set passes no `--tools` flag and therefore allows everything;
            // here we want the only callable tool to be `submit_review`.
            options.tool_set = ToolSet::Custom(vec!["submit_review".to_string()]);
        }

        let session = self
            .pi_manager
            .spawn_session_with_options(&session_id, &options, &working_dir)
            .await
            .map_err(|e| anyhow!("failed to spawn Pi session: {}", e))?;
        // Subscription-provider sessions are short-lived and should not
        // be touched by the idle-eviction loop. Tag as Review so the
        // sweep skips them.
        session.set_owner(crate::pi::session::SessionOwner::Review {
            job_id: session_id.clone(),
        });

        let start = Instant::now();

        session
            .send_prompt(user_prompt, None)
            .await
            .map_err(|e| anyhow!("failed to send prompt: {}", e))?;

        let effective_timeout = timeout.unwrap_or(Duration::from_secs(180));
        let result =
            tokio::time::timeout(effective_timeout, session.collect_response()).await;

        // Drain the captured `submit_review` tool args BEFORE killing the
        // session — kill drops the captured-args map.
        let tool_args = if structured {
            session.take_tool_args("submit_review")
        } else {
            None
        };

        // Fetch real token/cost stats while the session is still alive.
        // Falls back to a length-based estimate if Pi can't report stats.
        let stats = session.get_session_stats().await.ok();

        let _ = self.pi_manager.kill_session(&session_id).await;

        if let Some(ref p) = session_path {
            let _ = std::fs::remove_file(p);
        }

        let duration_ms = start.elapsed().as_millis() as u64;

        match result {
            Ok(Ok(output)) => {
                let (input_tokens, output_tokens) = match &stats {
                    Some(s) => (s.input as u32, (s.output + s.reasoning_tokens) as u32),
                    None => (
                        ((system_prompt.len() + user_prompt.len()) / 4) as u32,
                        (output.len() / 4) as u32,
                    ),
                };
                let final_output = if structured {
                    let args = tool_args.ok_or_else(|| {
                        anyhow!(
                            "Pi-subscription reviewer ({}) did not call submit_review",
                            model_id
                        )
                    })?;
                    serde_json::to_string(&args)
                        .map_err(|e| anyhow!("failed to serialise submit_review args: {}", e))?
                } else {
                    output
                };
                Ok(ModelResponse {
                    output: final_output,
                    input_tokens,
                    output_tokens,
                    model_id: model_id.to_string(),
                    duration_ms,
                    cache_hit_tokens: 0,
                    cache_write_tokens: 0,
                })
            }
            Ok(Err(e)) => Err(anyhow!("Pi session error: {}", e)),
            Err(_) => Err(anyhow!("Pi subscription call timed out after 180s")),
        }
    }

    pub fn name(&self) -> &str {
        &self.provider_name
    }
}

// ---------------------------------------------------------------------------
// ProviderRegistry — central catalog of all configured providers
// ---------------------------------------------------------------------------

/// Central catalog of all configured providers, keyed by stable provider
/// name (e.g. `"anthropic"`, `"openrouter"`, `"ollama"`, `"chatgpt"`).
///
/// Audit 6.6: storage migrated from a closed-set enum-dispatch to
/// `HashMap<String, Arc<dyn Provider>>`. Streaming-aware callers can
/// upgrade via [`Provider::as_streaming`] without an unsafe downcast.
#[derive(Debug, Default)]
pub struct ProviderRegistry {
    providers: HashMap<String, Arc<dyn Provider>>,
}

impl ProviderRegistry {
    pub fn new() -> Self {
        Self {
            providers: HashMap::new(),
        }
    }

    pub fn register(&mut self, name: impl Into<String>, provider: Arc<dyn Provider>) {
        let name = name.into();
        info!(provider = %name, "Registered provider");
        self.providers.insert(name, provider);
    }

    pub fn get(&self, name: &str) -> Option<Arc<dyn Provider>> {
        self.providers.get(name).cloned()
    }

    /// Instantiate providers from application config (api keys +
    /// per-provider settings). Optionally registers subscription providers
    /// that route through Pi sessions when `pi_manager` is supplied.
    ///
    /// For each provider in `provider_configs` that has a matching API key
    /// (or doesn't need one), instantiate the appropriate provider:
    /// - `"Anthropic"` → `AnthropicProvider`
    /// - `"Subscription"` → `PiSubscriptionProvider` (when auth detected)
    /// - `"OpenAI Compatible"` → `OpenAICompatibleProvider` (or `OpenRouterProvider` for openrouter)
    pub fn refresh_from_config_with_pi(
        &mut self,
        api_keys: &HashMap<String, String>,
        provider_configs: &HashMap<String, crate::state::config::ProviderConfig>,
        pi_manager: Option<Arc<PiManager>>,
    ) {
        self.providers.clear();

        for (id, pc) in provider_configs {
            let api_key = api_keys.get(id).cloned().unwrap_or_default();
            let endpoint = pc.endpoint.clone().unwrap_or_default();

            match pc.provider_type.as_str() {
                "Anthropic" => {
                    if !api_key.is_empty() {
                        match AnthropicProvider::new(api_key, None) {
                            Ok(p) => {
                                self.register(id, Arc::new(p) as Arc<dyn Provider>);
                            }
                            Err(e) => warn!(
                                provider = %id,
                                error = %e,
                                "skipping Anthropic provider — failed to construct"
                            ),
                        }
                    }
                }
                "Subscription" => {
                    // Always register subscription providers when Pi is
                    // available — their auth lives in `~/.pi/agent/auth.json`,
                    // which can be added/refreshed after app start. Pi
                    // surfaces a real auth error at call time if the user
                    // truly has no subscription, instead of the misleading
                    // "provider not found in registry" pre-flight failure.
                    if let Some(ref pm) = pi_manager {
                        let models = subscription_models_for(id);
                        self.register(
                            id,
                            Arc::new(PiSubscriptionProvider::new(
                                id.as_str(),
                                Arc::clone(pm),
                                models,
                            )) as Arc<dyn Provider>,
                        );
                    }
                }
                _ => {
                    if endpoint.is_empty() {
                        continue;
                    }
                    // OpenRouter gets its own provider for custom headers
                    if id == "openrouter" && !api_key.is_empty() {
                        match OpenRouterProvider::new(api_key, None) {
                            Ok(p) => {
                                self.register(id, Arc::new(p) as Arc<dyn Provider>);
                            }
                            Err(e) => warn!(
                                provider = %id,
                                error = %e,
                                "skipping OpenRouter provider — failed to construct"
                            ),
                        }
                        continue;
                    }
                    // Skip keyless providers unless they're localhost (e.g. Ollama)
                    if api_key.is_empty() && !endpoint.contains("localhost") {
                        continue;
                    }
                    match OpenAICompatibleProvider::new(&endpoint, &api_key, id, None) {
                        Ok(p) => {
                            self.register(id, Arc::new(p) as Arc<dyn Provider>);
                        }
                        Err(e) => warn!(
                            provider = %id,
                            error = %e,
                            "skipping OpenAI-compatible provider — failed to construct"
                        ),
                    }
                }
            }
        }

        info!(
            count = self.providers.len(),
            "provider registry refreshed from config"
        );
    }
}

/// Return hardcoded model list for a subscription provider.
fn sub_model(id: &str, ctx: usize) -> ModelInfo {
    ModelInfo {
        model_id: id.into(),
        context_window: ctx,
        cost_per_1m_input: 0.0,
        cost_per_1m_output: 0.0,
        supports_streaming: true,
    }
}

fn subscription_models_for(provider_id: &str) -> Vec<ModelInfo> {
    match provider_id {
        "chatgpt" => vec![
            sub_model("gpt-5.5", 272_000),
            sub_model("gpt-5.4", 272_000),
            sub_model("gpt-5.4-mini", 272_000),
            sub_model("gpt-5.3-codex", 272_000),
            sub_model("gpt-5.3-codex-spark", 128_000),
            sub_model("gpt-5.2", 272_000),
            sub_model("gpt-5.2-codex", 272_000),
            sub_model("gpt-5.1", 272_000),
            sub_model("gpt-5.1-codex-max", 272_000),
            sub_model("gpt-5.1-codex-mini", 272_000),
        ],
        "claude-sub" => vec![
            sub_model("claude-opus-4-7", 1_000_000),
            sub_model("claude-opus-4-6", 1_000_000),
            sub_model("claude-sonnet-4-6", 1_000_000),
            sub_model("claude-opus-4-5", 200_000),
            sub_model("claude-opus-4-5-20251101", 200_000),
            sub_model("claude-sonnet-4-5", 200_000),
            sub_model("claude-sonnet-4-5-20250929", 200_000),
            sub_model("claude-opus-4-1", 200_000),
            sub_model("claude-opus-4-1-20250805", 200_000),
            sub_model("claude-opus-4-0", 200_000),
            sub_model("claude-opus-4-20250514", 200_000),
            sub_model("claude-sonnet-4-0", 200_000),
            sub_model("claude-sonnet-4-20250514", 200_000),
            sub_model("claude-haiku-4-5", 200_000),
            sub_model("claude-haiku-4-5-20251001", 200_000),
            sub_model("claude-3-7-sonnet-20250219", 200_000),
            sub_model("claude-3-5-sonnet-20241022", 200_000),
            sub_model("claude-3-5-haiku-20241022", 200_000),
            sub_model("claude-3-opus-20240229", 200_000),
        ],
        _ => vec![],
    }
}

// ---------------------------------------------------------------------------
// Phase 5: provider-response parsers for structured-output (tool-use) shapes
// ---------------------------------------------------------------------------

/// Walk an Anthropic `messages` response body and pull out the first
/// `tool_use` content block named `submit_review` — returning its `input`
/// payload JSON-stringified. Falls back to the concatenation of `text`
/// blocks when no matching tool_use is present.
///
/// Public for testing; used by `AnthropicProvider::call_structured`.
pub(crate) fn extract_anthropic_tool_use_or_text(body: &serde_json::Value) -> String {
    let content = body.get("content").and_then(|c| c.as_array());
    if let Some(arr) = content {
        for block in arr {
            // First tool_use wins. Every structured request exposes a single
            // tool (hivemind's `submit_review`, nurse's `nurse_decisions`,
            // …) so name-matching is redundant; checking against a specific
            // constant silently discards valid output from any other caller.
            if block.get("type").and_then(|t| t.as_str()) == Some("tool_use") {
                if let Some(input) = block.get("input") {
                    return input.to_string();
                }
            }
        }
        // No matching tool_use — fall back to concatenated text blocks.
        let mut out = String::new();
        for block in arr {
            if block.get("type").and_then(|t| t.as_str()) == Some("text") {
                if let Some(s) = block.get("text").and_then(|t| t.as_str()) {
                    if !out.is_empty() {
                        out.push('\n');
                    }
                    out.push_str(s);
                }
            }
        }
        if !out.is_empty() {
            return out;
        }
    }
    String::new()
}

/// Build the value Anthropic accepts for the `system` field. When
/// `cache_static_prefix` is false, returns the plain string form (the legacy
/// shape, unchanged on the wire so cache hits don't reset). When true, wraps
/// the prompt as a single text block with `cache_control: ephemeral`, which
/// is how Anthropic's prompt-caching API marks a cacheable boundary —
/// everything before the marker is cached together.
pub(crate) fn anthropic_system_value(
    system_prompt: &str,
    cache_static_prefix: bool,
) -> serde_json::Value {
    if !cache_static_prefix {
        return serde_json::Value::String(system_prompt.to_string());
    }
    serde_json::json!([
        {
            "type": "text",
            "text": system_prompt,
            "cache_control": { "type": "ephemeral" }
        }
    ])
}

/// Return the `tools` array Anthropic expects, optionally with a
/// `cache_control: ephemeral` marker appended to the LAST tool definition.
/// Anthropic caches everything up to and including the marker, so a single
/// marker on the trailing tool covers both the system prompt and the full
/// tools array. When `cache_static_prefix` is false the input is returned
/// unchanged (byte-stable with the legacy path).
pub(crate) fn anthropic_tools_with_cache(
    tools: &[serde_json::Value],
    cache_static_prefix: bool,
) -> serde_json::Value {
    if !cache_static_prefix || tools.is_empty() {
        return serde_json::Value::Array(tools.to_vec());
    }
    let mut cloned = tools.to_vec();
    let last_idx = cloned.len() - 1;
    if let Some(obj) = cloned[last_idx].as_object_mut() {
        obj.insert(
            "cache_control".to_string(),
            serde_json::json!({ "type": "ephemeral" }),
        );
    }
    serde_json::Value::Array(cloned)
}

/// Detect the family of 400 responses that mean "this model accepts tools
/// but does not accept a forced `tool_choice`". Hit on DeepSeek's
/// `deepseek-reasoner` / `deepseek-v4-pro` thinking mode (official API and
/// some compatible gateways like opencode-style proxies), certain Kimi-K2
/// deployments, and Alibaba's Qwen3 thinking-mode models routed through
/// OpenRouter ("The tool_choice parameter does not support being set to
/// required or object in thinking mode").
///
/// Matched on substrings rather than full JSON parse because different
/// gateways wrap the upstream error differently (some return the raw
/// upstream body, some re-envelope it). All known variants share the
/// `tool_choice` literal alongside a "does not support" / "not supported"
/// phrase, so a conservative substring match is both robust and won't
/// fire on malformed-tool-choice 400s (which use "invalid" wording).
pub(crate) fn is_tool_choice_unsupported(body: &str) -> bool {
    let lower = body.to_ascii_lowercase();
    if !lower.contains("tool_choice") {
        return false;
    }
    lower.contains("does not support this tool_choice")
        || lower.contains("does not support tool_choice")
        || lower.contains("tool_choice is not supported")
        || lower.contains("tool_choice not supported")
        || lower.contains("tool_choice parameter does not support")
}

/// One-line diagnostic summary of an OpenAI-compatible chat-completion
/// response body whose extracted output came back empty. Surfaces:
/// `choices` count, `finish_reason`, whether `message.content` was a
/// non-empty string, `tool_calls` count, and `usage.completion_tokens`.
/// Lets users distinguish "model produced thinking tokens only", "model
/// hit a length cap with no output", "upstream returned an empty
/// envelope", etc., from one log line.
pub(crate) fn summarize_empty_response(body: &serde_json::Value) -> String {
    let choices = body.get("choices").and_then(|c| c.as_array());
    let choice_count = choices.map(|c| c.len()).unwrap_or(0);
    let first = choices.and_then(|c| c.first());
    let finish_reason = first
        .and_then(|c| c.get("finish_reason"))
        .and_then(|f| f.as_str())
        .unwrap_or("none");
    let msg = first.and_then(|c| c.get("message"));
    let content_state = match msg.and_then(|m| m.get("content")) {
        None => "missing",
        Some(v) if v.is_null() => "null",
        Some(serde_json::Value::String(s)) if s.is_empty() => "empty_str",
        Some(serde_json::Value::String(_)) => "non_empty_str",
        Some(_) => "non_string",
    };
    let tool_calls_len = msg
        .and_then(|m| m.get("tool_calls"))
        .and_then(|t| t.as_array())
        .map(|a| a.len())
        .unwrap_or(0);
    let completion_tokens = body
        .get("usage")
        .and_then(|u| u.get("completion_tokens"))
        .and_then(|t| t.as_u64())
        .unwrap_or(0);
    format!(
        "choices={} finish_reason={} content={} tool_calls={} completion_tokens={}",
        choice_count, finish_reason, content_state, tool_calls_len, completion_tokens
    )
}

/// Walk an OpenAI-compatible chat-completion response body and pull out
/// the first `tool_calls[0].function.arguments` payload — returning the
/// arguments string verbatim (callers JSON-parse it). Falls back to
/// `choices[0].message.content` when no `tool_calls` entry is present.
///
/// Every structured request only ever exposes a single tool (hivemind's
/// `submit_review`, nurse's `nurse_decisions`, …), so the first tool call
/// is always the one we asked for — matching by name would silently
/// discard valid output when a new caller picks a different tool name.
pub(crate) fn extract_openai_tool_call_or_text(body: &serde_json::Value) -> String {
    let choice0 = body.get("choices").and_then(|c| c.get(0));
    if let Some(c) = choice0 {
        if let Some(tcs) = c
            .get("message")
            .and_then(|m| m.get("tool_calls"))
            .and_then(|t| t.as_array())
        {
            for tc in tcs {
                if let Some(args) = tc
                    .get("function")
                    .and_then(|f| f.get("arguments"))
                    .and_then(|a| a.as_str())
                {
                    return args.to_string();
                }
            }
        }
        if let Some(text) = c
            .get("message")
            .and_then(|m| m.get("content"))
            .and_then(|t| t.as_str())
        {
            return text.to_string();
        }
    }
    String::new()
}

// ---------------------------------------------------------------------------
// MockProvider — test-only stub that returns a canned ModelResponse
// ---------------------------------------------------------------------------

/// A scripted streaming step for [`MockProvider`]. Each step emits a text
/// chunk over the `progress_tx` channel.
#[cfg(test)]
#[derive(Debug, Clone)]
pub enum StreamStep {
    /// Emit a `StreamChunk { delta }` on the `progress_tx` channel.
    Chunk(String),
}

/// Streaming configuration for [`MockProvider`]. When attached via
/// [`MockProvider::with_streaming`], `call_with_progress` walks the configured
/// `steps`, sleeping `delay` between each, and produces a final
/// `ModelResponse` whose `output` is the concatenation of all delivered
/// `Chunk` deltas.
#[cfg(test)]
#[derive(Debug, Clone)]
pub struct StreamingMockConfig {
    pub steps: Vec<StreamStep>,
    pub delay: Duration,
}

#[cfg(test)]
impl StreamingMockConfig {
    /// Build a config from a slice of chunk strings, all delivered with the
    /// same inter-chunk `delay`.
    pub fn from_chunks<I, S>(chunks: I, delay: Duration) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self {
            steps: chunks
                .into_iter()
                .map(|s| StreamStep::Chunk(s.into()))
                .collect(),
            delay,
        }
    }
}

#[cfg(test)]
#[derive(Debug, Clone)]
pub struct MockProvider {
    canned_output: Arc<std::sync::Mutex<String>>,
    pub call_count: Arc<std::sync::atomic::AtomicU32>,
    /// Optional artificial delay applied before returning the canned response.
    /// Used by the round-timeout-reaping test to force `dispatch_round` to
    /// time out with in-flight spawns still pending — exercising the
    /// stranded-task cleanup path in `engine.rs`.
    delay: Option<Duration>,
    /// When `Some`, `call_with_progress` walks the configured `StreamStep`
    /// sequence (sleeping `delay` between each) and emits `StreamChunk`s on
    /// the supplied channel. When `None`, `call_with_progress` falls back to
    /// the buffered `call(...)` path (matching the old behaviour).
    streaming: Arc<std::sync::Mutex<Option<StreamingMockConfig>>>,
}

#[cfg(test)]
impl MockProvider {
    pub fn new(canned_output: impl Into<String>) -> Self {
        Self {
            canned_output: Arc::new(std::sync::Mutex::new(canned_output.into())),
            call_count: Arc::new(std::sync::atomic::AtomicU32::new(0)),
            delay: None,
            streaming: Arc::new(std::sync::Mutex::new(None)),
        }
    }

    /// Sleep for `delay` before returning a response. Used by tests that need
    /// to exercise round-timeout behaviour.
    pub fn with_delay(canned_output: impl Into<String>, delay: Duration) -> Self {
        Self {
            canned_output: Arc::new(std::sync::Mutex::new(canned_output.into())),
            call_count: Arc::new(std::sync::atomic::AtomicU32::new(0)),
            delay: Some(delay),
            streaming: Arc::new(std::sync::Mutex::new(None)),
        }
    }

    /// Attach a streaming script. Subsequent `call_with_progress` invocations
    /// will emit the configured chunks (or fail with the configured error).
    pub fn with_streaming(self, config: StreamingMockConfig) -> Self {
        *self.streaming.lock().unwrap() = Some(config);
        self
    }

    pub async fn call(
        &self,
        model_id: &str,
        _system_prompt: &str,
        _user_prompt: &str,
        _temperature: Option<f64>,
        _top_p: Option<f64>,
        _max_tokens: Option<u32>,
        _timeout: Option<Duration>,
    ) -> Result<ModelResponse> {
        self.call_count
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if let Some(d) = self.delay {
            tokio::time::sleep(d).await;
        }
        let output = self.canned_output.lock().unwrap().clone();
        Ok(ModelResponse {
            output,
            input_tokens: 0,
            output_tokens: 0,
            model_id: model_id.to_string(),
            duration_ms: 0,
            cache_hit_tokens: 0,
            cache_write_tokens: 0,
        })
    }

    /// Streaming-capable variant. If a [`StreamingMockConfig`] has been
    /// attached via [`Self::with_streaming`], walks the script, emitting
    /// `StreamChunk`s on `progress_tx` with `delay` between steps. Returns
    /// the concatenation of all delivered chunks as the final
    /// `ModelResponse::output`.
    ///
    /// With no streaming config attached, delegates to [`Self::call`].
    pub async fn call_with_progress(
        &self,
        model_id: &str,
        system_prompt: &str,
        user_prompt: &str,
        temperature: Option<f64>,
        top_p: Option<f64>,
        max_tokens: Option<u32>,
        progress_tx: Option<tokio::sync::mpsc::UnboundedSender<StreamChunk>>,
        timeout: Option<Duration>,
    ) -> Result<ModelResponse> {
        let streaming = self.streaming.lock().unwrap().clone();
        let Some(config) = streaming else {
            drop(progress_tx);
            return self
                .call(
                    model_id,
                    system_prompt,
                    user_prompt,
                    temperature,
                    top_p,
                    max_tokens,
                    timeout,
                )
                .await;
        };

        self.call_count
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let start = Instant::now();
        let mut accumulated = String::new();

        for step in &config.steps {
            if !config.delay.is_zero() {
                tokio::time::sleep(config.delay).await;
            }
            let StreamStep::Chunk(delta) = step;
            accumulated.push_str(delta);
            if let Some(tx) = progress_tx.as_ref() {
                let _ = tx.send(StreamChunk {
                    delta: delta.clone(),
                });
            }
        }

        Ok(ModelResponse {
            output: accumulated,
            input_tokens: 0,
            output_tokens: 0,
            model_id: model_id.to_string(),
            duration_ms: start.elapsed().as_millis() as u64,
            cache_hit_tokens: 0,
            cache_write_tokens: 0,
        })
    }

    pub fn name(&self) -> &str {
        "mock"
    }
}

// ---------------------------------------------------------------------------
// Provider / StreamingProvider impls (audit 6.6)
//
// Each concrete provider gets an `impl Provider`. Streaming-capable
// providers (OpenAICompatible, OpenRouter, Ollama, MockProvider) also get an
// `impl StreamingProvider` and override `Provider::as_streaming` to surface
// themselves. Anthropic and PiSubscription are intentionally NOT
// streaming-capable — that's how the audit fixes the historical
// "drop the channel" bug at providers.rs:285-310.
//
// Pricing lives on each impl's `cost_per_1m_tokens(...)`. The default
// impl on the trait derives from `model_info(...)`, so providers that
// already populate `ModelInfo::cost_per_1m_*` get cost lookup for free;
// the explicit overrides below preserve the legacy match-table values
// that used to live at `engine.rs:3448` for well-known model IDs.
// ---------------------------------------------------------------------------

/// Returns the legacy per-model cost lookup that used to live at
/// `engine.rs:3448`. Lifted to a free function so every OpenAI-compatible
/// provider impl (OpenAICompatible, OpenRouter, Ollama) can fall back to
/// these well-known model IDs when the user hasn't populated per-model
/// pricing in their `ModelInfo`.
///
/// `pub(crate)` so `hivemind::engine::compute_cost` can use the same table
/// as a last-resort fallback when no registered provider claims a model.
pub(crate) fn legacy_well_known_model_cost(model_id: &str) -> Option<(f64, f64)> {
    Some(match model_id {
        "claude-sonnet-4-20250514" | "claude-3-5-sonnet-20241022" => (3.0, 15.0),
        "claude-opus-4-20250514" => (15.0, 75.0),
        "claude-haiku-4-20250514" | "claude-3-5-haiku-20241022" => (0.80, 4.0),
        "gpt-4o" => (2.5, 10.0),
        "gpt-4o-mini" => (0.15, 0.60),
        "o3-mini" => (1.1, 4.4),
        "o1" => (15.0, 60.0),
        _ => return None,
    })
}

#[async_trait]
impl Provider for OpenAICompatibleProvider {
    async fn call(&self, req: CallRequest) -> Result<ModelResponse> {
        if let Some(structured) = req.structured {
            self.call_structured(
                &req.model_id,
                &req.system_prompt,
                &req.user_prompt,
                req.temperature,
                req.top_p,
                req.max_tokens,
                req.timeout,
                &structured,
            )
            .await
        } else {
            self.call(
                &req.model_id,
                &req.system_prompt,
                &req.user_prompt,
                req.temperature,
                req.top_p,
                req.max_tokens,
                req.timeout,
            )
            .await
        }
    }

    fn name(&self) -> &str {
        self.name()
    }

    fn as_streaming(&self) -> Option<&dyn StreamingProvider> {
        Some(self)
    }
}

#[async_trait]
impl StreamingProvider for OpenAICompatibleProvider {
    async fn call_streaming(
        &self,
        req: CallRequest,
        tx: tokio::sync::mpsc::UnboundedSender<StreamChunk>,
    ) -> Result<ModelResponse> {
        // Structured-output is non-streaming on purpose (buffered JSON
        // envelope). Honour `structured` by falling back to `call_structured`
        // and dropping the sender — same shape the legacy enum-dispatch had.
        if let Some(structured) = req.structured {
            drop(tx);
            return self
                .call_structured(
                    &req.model_id,
                    &req.system_prompt,
                    &req.user_prompt,
                    req.temperature,
                    req.top_p,
                    req.max_tokens,
                    req.timeout,
                    &structured,
                )
                .await;
        }
        self.call_with_progress(
            &req.model_id,
            &req.system_prompt,
            &req.user_prompt,
            req.temperature,
            req.top_p,
            req.max_tokens,
            Some(tx),
            req.timeout,
        )
        .await
    }
}

#[async_trait]
impl Provider for AnthropicProvider {
    async fn call(&self, req: CallRequest) -> Result<ModelResponse> {
        if let Some(structured) = req.structured {
            self.call_structured(
                &req.model_id,
                &req.system_prompt,
                &req.user_prompt,
                req.temperature,
                req.top_p,
                req.max_tokens,
                req.timeout,
                &structured,
                req.cache_static_prefix,
            )
            .await
        } else {
            self.call(
                &req.model_id,
                &req.system_prompt,
                &req.user_prompt,
                req.temperature,
                req.top_p,
                req.max_tokens,
                req.timeout,
                req.cache_static_prefix,
            )
            .await
        }
    }

    fn name(&self) -> &str {
        self.name()
    }
}

#[async_trait]
impl Provider for OpenRouterProvider {
    async fn call(&self, req: CallRequest) -> Result<ModelResponse> {
        if let Some(structured) = req.structured {
            self.call_structured(
                &req.model_id,
                &req.system_prompt,
                &req.user_prompt,
                req.temperature,
                req.top_p,
                req.max_tokens,
                req.timeout,
                &structured,
            )
            .await
        } else {
            self.call(
                &req.model_id,
                &req.system_prompt,
                &req.user_prompt,
                req.temperature,
                req.top_p,
                req.max_tokens,
                req.timeout,
            )
            .await
        }
    }

    fn name(&self) -> &str {
        self.name()
    }

    fn as_streaming(&self) -> Option<&dyn StreamingProvider> {
        Some(self)
    }
}

#[async_trait]
impl StreamingProvider for OpenRouterProvider {
    async fn call_streaming(
        &self,
        req: CallRequest,
        tx: tokio::sync::mpsc::UnboundedSender<StreamChunk>,
    ) -> Result<ModelResponse> {
        if let Some(structured) = req.structured {
            drop(tx);
            return self
                .call_structured(
                    &req.model_id,
                    &req.system_prompt,
                    &req.user_prompt,
                    req.temperature,
                    req.top_p,
                    req.max_tokens,
                    req.timeout,
                    &structured,
                )
                .await;
        }
        self.call_with_progress(
            &req.model_id,
            &req.system_prompt,
            &req.user_prompt,
            req.temperature,
            req.top_p,
            req.max_tokens,
            Some(tx),
            req.timeout,
        )
        .await
    }
}

#[async_trait]
impl Provider for PiSubscriptionProvider {
    async fn call(&self, req: CallRequest) -> Result<ModelResponse> {
        // When the caller wants a structured response, route through the
        // `submit_review` extension tool registered on the spawned Pi
        // session and return the JSON-stringified tool args.
        let structured = req.structured.is_some();
        self.call_inner(
            &req.model_id,
            &req.system_prompt,
            &req.user_prompt,
            req.thinking,
            structured,
            req.timeout,
        )
        .await
    }

    fn name(&self) -> &str {
        self.name()
    }
}

#[cfg(test)]
#[async_trait]
impl Provider for MockProvider {
    async fn call(&self, req: CallRequest) -> Result<ModelResponse> {
        self.call(
            &req.model_id,
            &req.system_prompt,
            &req.user_prompt,
            req.temperature,
            req.top_p,
            req.max_tokens,
            req.timeout,
        )
        .await
    }

    fn name(&self) -> &str {
        self.name()
    }

    fn as_streaming(&self) -> Option<&dyn StreamingProvider> {
        Some(self)
    }
}

#[cfg(test)]
#[async_trait]
impl StreamingProvider for MockProvider {
    async fn call_streaming(
        &self,
        req: CallRequest,
        tx: tokio::sync::mpsc::UnboundedSender<StreamChunk>,
    ) -> Result<ModelResponse> {
        self.call_with_progress(
            &req.model_id,
            &req.system_prompt,
            &req.user_prompt,
            req.temperature,
            req.top_p,
            req.max_tokens,
            Some(tx),
            req.timeout,
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ------------------------------------------------------------------
    // ModelInfo tests
    // ------------------------------------------------------------------

    #[test]
    fn test_model_info_fields() {
        let info = ModelInfo {
            model_id: "test-model".into(),
            context_window: 128_000,
            cost_per_1m_input: 2.5,
            cost_per_1m_output: 10.0,
            supports_streaming: true,
        };
        assert_eq!(info.model_id, "test-model");
        assert_eq!(info.context_window, 128_000);
        assert_eq!(info.cost_per_1m_input, 2.5);
        assert_eq!(info.cost_per_1m_output, 10.0);
        assert!(info.supports_streaming);
    }

    // ------------------------------------------------------------------
    // AnthropicProvider tests
    // ------------------------------------------------------------------

    #[test]
    fn test_anthropic_provider_name() {
        let provider = AnthropicProvider::new("test-key", None).unwrap();
        assert_eq!(provider.name(), "anthropic");
    }

    // ------------------------------------------------------------------
    // Log-redaction sanity check (C3 — defense in depth).
    //
    // We can't capture `tracing` output without adding a new dev-dep
    // (`tracing-test`), and the task constraints forbid new crate deps.
    // Instead, this test is a structural guard: it constructs each
    // provider with a fake API key and verifies (via Debug, which is the
    // only public observable surface for the structs) that the key does
    // not appear there.
    //
    // The `tracing::` macro audit lives in code comments at each log
    // site. As long as no log macro takes a `HeaderMap` / `&self.api_key`
    // / response.headers() argument, headers and key material cannot
    // structurally reach a log writer regardless of subscriber config.
    // ------------------------------------------------------------------

    const FAKE_KEY_SENTINEL: &str = "sk-ant-test-FAKE-SENTINEL-12345";

    #[test]
    fn anthropic_provider_debug_does_not_leak_api_key() {
        let provider = AnthropicProvider::new(FAKE_KEY_SENTINEL, None).unwrap();
        let dbg = format!("{:?}", provider);
        assert!(
            !dbg.contains(FAKE_KEY_SENTINEL),
            "AnthropicProvider Debug impl must not surface api_key — found sentinel in: {}",
            dbg
        );
    }

    #[test]
    fn openai_compatible_provider_debug_does_not_leak_api_key() {
        let provider = OpenAICompatibleProvider::new(
            "https://api.example.com/v1",
            FAKE_KEY_SENTINEL,
            "test-provider",
            None,
        )
        .unwrap();
        let dbg = format!("{:?}", provider);
        assert!(
            !dbg.contains(FAKE_KEY_SENTINEL),
            "OpenAICompatibleProvider Debug impl must not surface api_key — found sentinel in: {}",
            dbg
        );
    }

    #[test]
    fn openrouter_provider_debug_does_not_leak_api_key() {
        let provider = OpenRouterProvider::new(FAKE_KEY_SENTINEL, None).unwrap();
        let dbg = format!("{:?}", provider);
        assert!(
            !dbg.contains(FAKE_KEY_SENTINEL),
            "OpenRouterProvider Debug impl must not surface api_key — found sentinel in: {}",
            dbg
        );
    }

    // ------------------------------------------------------------------
    // OpenAICompatibleProvider tests
    // ------------------------------------------------------------------

    #[test]
    fn test_openai_compatible_provider_name() {
        let provider = OpenAICompatibleProvider::new(
            "https://api.example.com/v1",
            "key",
            "custom-provider",
            None,
        )
        .unwrap();
        assert_eq!(provider.name(), "custom-provider");
    }

    // ------------------------------------------------------------------
    // build_payload sampling-parameter tests — verify temperature/top_p
    // are emitted only when Some, and that both can coexist.
    // ------------------------------------------------------------------

    fn make_test_provider() -> OpenAICompatibleProvider {
        OpenAICompatibleProvider::new("https://api.example.com/v1", "key", "test", None).unwrap()
    }

    #[test]
    fn test_build_payload_omits_top_p_when_none() {
        let p = make_test_provider();
        let payload = p.build_payload("m", "sys", "user", None, None, None);
        assert!(payload.get("top_p").is_none());
        assert!(payload.get("temperature").is_none());
        assert!(payload.get("max_tokens").is_none());
        assert_eq!(payload["model"], "m");
    }

    #[test]
    fn test_build_payload_emits_top_p_when_some() {
        let p = make_test_provider();
        let payload = p.build_payload("m", "sys", "user", None, Some(0.85), None);
        assert_eq!(payload["top_p"], 0.85);
        assert!(payload.get("temperature").is_none());
    }

    #[test]
    fn test_build_payload_emits_temperature_and_top_p_together() {
        let p = make_test_provider();
        let payload = p.build_payload("m", "sys", "user", Some(0.2), Some(0.95), Some(1024));
        assert_eq!(payload["temperature"], 0.2);
        assert_eq!(payload["top_p"], 0.95);
        assert_eq!(payload["max_tokens"], 1024);
    }

    // ------------------------------------------------------------------
    // SSE parser tests (pure function — no HTTP)
    // ------------------------------------------------------------------

    #[test]
    fn test_parse_sse_event_data_done_sentinel() {
        let p = parse_sse_event_data("[DONE]").unwrap();
        assert!(p.done);
        assert!(p.delta.is_none());
        assert!(p.usage.is_none());
    }

    #[test]
    fn test_parse_sse_event_data_done_sentinel_with_whitespace() {
        let p = parse_sse_event_data("  [DONE]  ").unwrap();
        assert!(p.done);
    }

    #[test]
    fn test_parse_sse_event_data_content_delta() {
        let payload = r#"{"choices":[{"delta":{"content":"hello"},"index":0}]}"#;
        let p = parse_sse_event_data(payload).unwrap();
        assert!(!p.done);
        assert_eq!(p.delta.as_deref(), Some("hello"));
        assert!(p.usage.is_none());
    }

    #[test]
    fn test_parse_sse_event_data_role_only_delta_yields_no_text() {
        // First chunk from OpenAI is typically a role-only delta
        let payload = r#"{"choices":[{"delta":{"role":"assistant"},"index":0}]}"#;
        let p = parse_sse_event_data(payload).unwrap();
        assert!(p.delta.is_none());
        assert!(p.usage.is_none());
        assert!(!p.done);
    }

    #[test]
    fn test_parse_sse_event_data_reasoning_delta_ignored() {
        // delta.reasoning should be ignored (not appended to output)
        let payload = r#"{"choices":[{"delta":{"reasoning":"thinking..."},"index":0}]}"#;
        let p = parse_sse_event_data(payload).unwrap();
        assert!(p.delta.is_none());
    }

    #[test]
    fn test_parse_sse_event_data_usage_frame() {
        let payload = r#"{"choices":[],"usage":{"prompt_tokens":12,"completion_tokens":34}}"#;
        let p = parse_sse_event_data(payload).unwrap();
        assert!(!p.done);
        assert!(p.delta.is_none());
        assert_eq!(p.usage, Some((12, 34)));
    }

    #[test]
    fn test_parse_sse_event_data_invalid_json_errors() {
        let p = parse_sse_event_data("not json");
        assert!(p.is_err());
    }

    #[test]
    fn test_parse_sse_event_data_empty_content_skipped() {
        let payload = r#"{"choices":[{"delta":{"content":""},"index":0}]}"#;
        let p = parse_sse_event_data(payload).unwrap();
        // Empty string is treated as no delta.
        assert!(p.delta.is_none());
    }

    #[test]
    fn test_parse_sse_event_data_deepseek_cache_hit_tokens() {
        // DeepSeek emits prompt_cache_hit_tokens alongside prompt_tokens on the
        // final SSE usage frame. The OpenAI-compat parser must surface it so
        // the nurse decision log can show the cache savings.
        let payload = r#"{"choices":[],"usage":{"prompt_tokens":100,"completion_tokens":20,"prompt_cache_hit_tokens":80,"prompt_cache_miss_tokens":20}}"#;
        let p = parse_sse_event_data(payload).unwrap();
        assert_eq!(p.usage, Some((100, 20)));
        assert_eq!(p.cache_hit_tokens, 80);
    }

    #[test]
    fn test_parse_sse_event_data_no_cache_hit_tokens_defaults_zero() {
        // Every non-DeepSeek OpenAI-compatible endpoint omits the field.
        let payload = r#"{"choices":[],"usage":{"prompt_tokens":12,"completion_tokens":34}}"#;
        let p = parse_sse_event_data(payload).unwrap();
        assert_eq!(p.cache_hit_tokens, 0);
    }

    #[test]
    fn test_parse_buffered_response_extracts_deepseek_cache_hit() {
        let body: serde_json::Value = serde_json::from_str(
            r#"{
                "choices": [{"message": {"content": "ok"}}],
                "usage": {
                    "prompt_tokens": 6200,
                    "completion_tokens": 50,
                    "prompt_cache_hit_tokens": 6100,
                    "prompt_cache_miss_tokens": 100
                }
            }"#,
        )
        .unwrap();
        let (output, pt, ct, cache_hit) = OpenAICompatibleProvider::parse_buffered_response(&body);
        assert_eq!(output, "ok");
        assert_eq!(pt, 6200);
        assert_eq!(ct, 50);
        assert_eq!(cache_hit, 6100);
    }

    #[test]
    fn test_parse_buffered_response_defaults_cache_hit_zero_when_absent() {
        let body: serde_json::Value = serde_json::from_str(
            r#"{
                "choices": [{"message": {"content": "ok"}}],
                "usage": {"prompt_tokens": 100, "completion_tokens": 20}
            }"#,
        )
        .unwrap();
        let (_o, _p, _c, cache_hit) = OpenAICompatibleProvider::parse_buffered_response(&body);
        assert_eq!(cache_hit, 0);
    }

    // ------------------------------------------------------------------
    // Anthropic cache_control helpers
    // ------------------------------------------------------------------

    #[test]
    fn anthropic_system_value_plain_string_when_disabled() {
        let v = anthropic_system_value("my system prompt", false);
        assert_eq!(v, serde_json::Value::String("my system prompt".into()));
    }

    #[test]
    fn anthropic_system_value_wraps_with_cache_control_when_enabled() {
        let v = anthropic_system_value("my system prompt", true);
        let expected = serde_json::json!([
            {
                "type": "text",
                "text": "my system prompt",
                "cache_control": { "type": "ephemeral" }
            }
        ]);
        assert_eq!(v, expected);
    }

    #[test]
    fn anthropic_tools_with_cache_returns_unchanged_when_disabled() {
        let tools: Vec<serde_json::Value> = vec![
            serde_json::json!({ "name": "t1", "description": "x", "input_schema": {} }),
            serde_json::json!({ "name": "t2", "description": "y", "input_schema": {} }),
        ];
        let out = anthropic_tools_with_cache(&tools, false);
        assert_eq!(out, serde_json::Value::Array(tools));
    }

    #[test]
    fn anthropic_tools_with_cache_marks_last_tool_only_when_enabled() {
        let tools: Vec<serde_json::Value> = vec![
            serde_json::json!({ "name": "t1", "description": "x", "input_schema": {} }),
            serde_json::json!({ "name": "t2", "description": "y", "input_schema": {} }),
        ];
        let out = anthropic_tools_with_cache(&tools, true);
        let arr = out.as_array().expect("expected tools array");
        assert!(
            arr[0].get("cache_control").is_none(),
            "first tool must not carry cache_control"
        );
        assert_eq!(
            arr[1]["cache_control"],
            serde_json::json!({ "type": "ephemeral" })
        );
    }

    #[test]
    fn anthropic_tools_with_cache_handles_empty_array() {
        let tools: Vec<serde_json::Value> = vec![];
        let out = anthropic_tools_with_cache(&tools, true);
        assert_eq!(out, serde_json::Value::Array(vec![]));
    }

    // ------------------------------------------------------------------
    // tool_choice byte-stability across calls (cache-prefix property)
    //
    // Once a model is recorded in auto_tool_choice_models, every subsequent
    // call to the SAME model must serialise an identical `tool_choice` value
    // so DeepSeek's automatic prefix cache hits.
    // ------------------------------------------------------------------

    #[test]
    fn test_tool_choice_byte_stable_after_model_flipped_to_auto() {
        let provider = make_test_provider();
        // Simulate a previous call that learned the model rejects forced
        // tool_choice — same as the runtime fallback at line ~414.
        provider
            .auto_tool_choice_models
            .lock()
            .unwrap()
            .insert("deepseek-reasoner".to_string());

        // Build two payloads back-to-back using the exact same decision the
        // production call_structured path uses (lines ~373-377).
        let structured_tool_choice = serde_json::json!({
            "type": "function",
            "function": { "name": "nurse_decisions" }
        });
        let payload_for = |model_id: &str| -> serde_json::Value {
            let prefers_auto = provider
                .auto_tool_choice_models
                .lock()
                .map(|s| s.contains(model_id))
                .unwrap_or(false);
            if prefers_auto {
                serde_json::json!("auto")
            } else {
                structured_tool_choice.clone()
            }
        };

        let body_a = serde_json::to_string(&payload_for("deepseek-reasoner")).unwrap();
        let body_b = serde_json::to_string(&payload_for("deepseek-reasoner")).unwrap();
        assert_eq!(
            body_a, body_b,
            "two consecutive tool_choice serialisations for a flipped model must be byte-identical for DeepSeek prefix-cache to hit"
        );
        assert_eq!(body_a, "\"auto\"");
    }

    // ------------------------------------------------------------------
    // call_with_progress tests via a hand-rolled TcpListener mock server.
    // We intentionally avoid pulling wiremock into dev-deps.
    // ------------------------------------------------------------------

    use std::sync::atomic::{AtomicU16, Ordering};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    static MOCK_PORT_OFFSET: AtomicU16 = AtomicU16::new(0);

    /// Read just enough of an HTTP request off the socket so we can respond.
    /// Returns when we've consumed `\r\n\r\n` or hit EOF.
    async fn drain_http_request(stream: &mut tokio::net::TcpStream) {
        let mut buf = [0u8; 4096];
        let mut total = Vec::new();
        loop {
            match stream.read(&mut buf).await {
                Ok(0) => return,
                Ok(n) => {
                    total.extend_from_slice(&buf[..n]);
                    if total.windows(4).any(|w| w == b"\r\n\r\n") {
                        // Drain the body if Content-Length is present, but
                        // for our purposes the headers terminator is enough
                        // — the client doesn't read further until we reply.
                        return;
                    }
                }
                Err(_) => return,
            }
        }
    }

    /// Spawn a one-shot mock server on an ephemeral port and return its URL.
    async fn spawn_mock_server<F, Fut>(handler: F) -> String
    where
        F: FnOnce(tokio::net::TcpStream) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = ()> + Send + 'static,
    {
        // Use 127.0.0.1:0 to let the OS pick a free port.
        let _ = MOCK_PORT_OFFSET.fetch_add(1, Ordering::SeqCst);
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        tokio::spawn(async move {
            if let Ok((stream, _)) = listener.accept().await {
                handler(stream).await;
            }
        });

        format!("http://{}", addr)
    }

    #[tokio::test]
    async fn test_call_with_progress_sse_happy_path() {
        let url = spawn_mock_server(|mut stream| async move {
            drain_http_request(&mut stream).await;

            let body = concat!(
                "data: {\"choices\":[{\"delta\":{\"role\":\"assistant\"},\"index\":0}]}\n\n",
                "data: {\"choices\":[{\"delta\":{\"content\":\"Hello\"},\"index\":0}]}\n\n",
                "data: {\"choices\":[{\"delta\":{\"content\":\", world!\"},\"index\":0}]}\n\n",
                "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":7,\"completion_tokens\":3}}\n\n",
                "data: [DONE]\n\n",
            );
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\n\r\n"
            );
            let _ = stream.write_all(resp.as_bytes()).await;
            // chunked-encode each SSE block separately so the client sees
            // them as discrete frames rather than one buffered blob.
            for chunk in body.split_inclusive("\n\n") {
                let frame = format!("{:x}\r\n{}\r\n", chunk.len(), chunk);
                let _ = stream.write_all(frame.as_bytes()).await;
                let _ = stream.flush().await;
            }
            let _ = stream.write_all(b"0\r\n\r\n").await;
            let _ = stream.flush().await;
        })
        .await;

        let provider =
            OpenAICompatibleProvider::new(url, "k", "mock", Some(Duration::from_secs(5))).unwrap();

        // Unbounded to match the production callers (`hivemind::engine`)
        // after audit 6.6. The test only sends two short deltas.
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<StreamChunk>();
        let resp = provider
            .call_with_progress(
                "test-model",
                "sys",
                "user",
                None,
                None,
                None,
                Some(tx),
                None,
            )
            .await
            .expect("streaming call should succeed");

        assert_eq!(resp.output, "Hello, world!");
        assert_eq!(resp.input_tokens, 7);
        assert_eq!(resp.output_tokens, 3);
        assert_eq!(resp.model_id, "test-model");

        // Verify two non-empty deltas were dispatched on the channel.
        let mut deltas = Vec::new();
        while let Ok(chunk) = rx.try_recv() {
            deltas.push(chunk.delta);
        }
        assert_eq!(deltas, vec!["Hello".to_string(), ", world!".to_string()]);
    }

    #[tokio::test]
    async fn test_call_with_progress_inactivity_timeout() {
        // Server sends one chunk, then sleeps far longer than the
        // INACTIVITY_TIMEOUT_SECS watchdog. We need the watchdog to fire.
        // To keep the test fast, we _override_ the constant by relying on
        // the provider's behaviour: it bails after INACTIVITY_TIMEOUT_SECS.
        // 30s is a long wait for a unit test, so we run with a tighter
        // deadline-style assertion: the call must error within 35 s.
        let url = spawn_mock_server(|mut stream| async move {
            drain_http_request(&mut stream).await;

            let resp = "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\n\r\n";
            let _ = stream.write_all(resp.as_bytes()).await;
            let chunk = "data: {\"choices\":[{\"delta\":{\"content\":\"hi\"},\"index\":0}]}\n\n";
            let frame = format!("{:x}\r\n{}\r\n", chunk.len(), chunk);
            let _ = stream.write_all(frame.as_bytes()).await;
            let _ = stream.flush().await;
            // Now hold the connection open without sending more data.
            tokio::time::sleep(Duration::from_secs(60)).await;
            // We never reach here in practice, but close the writer cleanly.
            let _ = stream.write_all(b"0\r\n\r\n").await;
        })
        .await;

        let provider =
            OpenAICompatibleProvider::new(url, "k", "mock", Some(Duration::from_secs(120)))
                .unwrap();

        let start = Instant::now();
        let result = tokio::time::timeout(
            Duration::from_secs(INACTIVITY_TIMEOUT_SECS + 10),
            provider.call_with_progress("test-model", "sys", "user", None, None, None, None, None),
        )
        .await;
        let elapsed = start.elapsed();

        let inner = result.expect("test outer timeout — watchdog did not fire");
        let err = inner.expect_err("call should have errored due to stalled stream");
        let msg = err.to_string();
        assert!(
            msg.contains("stalled"),
            "expected 'stalled' in error message, got: {}",
            msg
        );
        assert!(
            elapsed.as_secs() >= INACTIVITY_TIMEOUT_SECS - 1
                && elapsed.as_secs() <= INACTIVITY_TIMEOUT_SECS + 10,
            "watchdog fired at unexpected time: {:?}",
            elapsed
        );
    }

    #[tokio::test]
    async fn test_call_with_progress_content_type_fallback() {
        // Server ignores `stream: true` and returns plain JSON. The provider
        // should fall back to the buffered path and parse it correctly.
        let url = spawn_mock_server(|mut stream| async move {
            drain_http_request(&mut stream).await;

            let body = serde_json::json!({
                "choices": [
                    { "message": { "role": "assistant", "content": "buffered hi" } }
                ],
                "usage": { "prompt_tokens": 5, "completion_tokens": 11 }
            })
            .to_string();
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = stream.write_all(resp.as_bytes()).await;
            let _ = stream.flush().await;
        })
        .await;

        let provider =
            OpenAICompatibleProvider::new(url, "k", "mock", Some(Duration::from_secs(5))).unwrap();

        let resp = provider
            .call_with_progress("test-model", "sys", "user", None, None, None, None, None)
            .await
            .expect("fallback path should succeed");
        assert_eq!(resp.output, "buffered hi");
        assert_eq!(resp.input_tokens, 5);
        assert_eq!(resp.output_tokens, 11);
        assert_eq!(resp.model_id, "test-model");
    }

    /// Multi-request mock server: accepts in a loop, dispatching each
    /// connection through the handler with a zero-based attempt counter.
    /// Used to test retry behaviour where each attempt needs different
    /// server-side responses.
    async fn spawn_mock_server_multi<F, Fut>(handler: F) -> String
    where
        F: Fn(tokio::net::TcpStream, u32) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = ()> + Send + 'static,
    {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let handler = std::sync::Arc::new(handler);

        tokio::spawn(async move {
            let mut idx: u32 = 0;
            loop {
                match listener.accept().await {
                    Ok((stream, _)) => {
                        let handler = std::sync::Arc::clone(&handler);
                        let attempt = idx;
                        idx += 1;
                        tokio::spawn(async move {
                            handler(stream, attempt).await;
                        });
                    }
                    Err(_) => return,
                }
            }
        });

        format!("http://{}", addr)
    }

    #[tokio::test]
    async fn test_call_with_progress_retries_on_empty_stream() {
        use std::sync::atomic::{AtomicU32, Ordering};
        use std::sync::Arc;

        let attempts = Arc::new(AtomicU32::new(0));
        let attempts_clone = Arc::clone(&attempts);

        let url = spawn_mock_server_multi(move |mut stream, attempt| {
            let attempts = Arc::clone(&attempts_clone);
            async move {
                attempts.fetch_add(1, Ordering::SeqCst);
                drain_http_request(&mut stream).await;
                let resp = "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\n\r\n";
                let _ = stream.write_all(resp.as_bytes()).await;

                if attempt < 2 {
                    // First two attempts: empty SSE body, close immediately.
                    let _ = stream.write_all(b"0\r\n\r\n").await;
                    let _ = stream.flush().await;
                } else {
                    // Third attempt: real content.
                    let body = concat!(
                        "data: {\"choices\":[{\"delta\":{\"content\":\"recovered\"},\"index\":0}]}\n\n",
                        "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":3,\"completion_tokens\":1}}\n\n",
                        "data: [DONE]\n\n",
                    );
                    for chunk in body.split_inclusive("\n\n") {
                        let frame = format!("{:x}\r\n{}\r\n", chunk.len(), chunk);
                        let _ = stream.write_all(frame.as_bytes()).await;
                        let _ = stream.flush().await;
                    }
                    let _ = stream.write_all(b"0\r\n\r\n").await;
                    let _ = stream.flush().await;
                }
            }
        })
        .await;

        let provider =
            OpenAICompatibleProvider::new(url, "k", "mock", Some(Duration::from_secs(10))).unwrap();
        let resp = provider
            .call_with_progress("test-model", "sys", "user", None, None, None, None, None)
            .await
            .expect("retry should recover on the third attempt");

        assert_eq!(resp.output, "recovered");
        assert_eq!(resp.input_tokens, 3);
        assert_eq!(resp.output_tokens, 1);
        assert_eq!(
            attempts.load(Ordering::SeqCst),
            3,
            "should have made 3 HTTP requests"
        );
    }

    #[tokio::test]
    async fn test_call_with_progress_fails_after_max_empty_attempts() {
        use std::sync::atomic::{AtomicU32, Ordering};
        use std::sync::Arc;

        let attempts = Arc::new(AtomicU32::new(0));
        let attempts_clone = Arc::clone(&attempts);

        let url = spawn_mock_server_multi(move |mut stream, _attempt| {
            let attempts = Arc::clone(&attempts_clone);
            async move {
                attempts.fetch_add(1, Ordering::SeqCst);
                drain_http_request(&mut stream).await;
                let resp = "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\n\r\n";
                let _ = stream.write_all(resp.as_bytes()).await;
                let _ = stream.write_all(b"0\r\n\r\n").await;
                let _ = stream.flush().await;
            }
        })
        .await;

        let provider =
            OpenAICompatibleProvider::new(url, "k", "mock", Some(Duration::from_secs(10))).unwrap();
        let err = provider
            .call_with_progress("test-model", "sys", "user", None, None, None, None, None)
            .await
            .expect_err("should fail after exhausting empty-stream retries");

        assert!(
            err.to_string().contains("empty SSE stream"),
            "expected 'empty SSE stream' in error message, got: {}",
            err
        );
        assert_eq!(
            attempts.load(Ordering::SeqCst),
            MAX_EMPTY_STREAM_ATTEMPTS,
            "should have made MAX_EMPTY_STREAM_ATTEMPTS HTTP requests"
        );
    }

    // ------------------------------------------------------------------
    // Anthropic input_tokens cache adjustment tests
    // ------------------------------------------------------------------

    #[test]
    fn test_anthropic_input_tokens_adjustment() {
        // Cold cache: input_tokens includes cache_creation
        let cold_json = serde_json::json!({
            "content": [{"text": "ok", "type": "text"}],
            "usage": {
                "input_tokens": 175000,
                "output_tokens": 500,
                "cache_creation_input_tokens": 160000,
                "cache_read_input_tokens": 0
            }
        });
        let input_tokens_raw = cold_json["usage"]["input_tokens"].as_u64().unwrap_or(0);
        let cache_creation = cold_json["usage"]["cache_creation_input_tokens"]
            .as_u64()
            .unwrap_or(0);
        let cache_read = cold_json["usage"]["cache_read_input_tokens"]
            .as_u64()
            .unwrap_or(0);
        let adjusted = input_tokens_raw
            .saturating_sub(cache_creation)
            .saturating_sub(cache_read);
        assert_eq!(adjusted, 15000, "cold cache: 175000 - 160000 - 0 = 15000");

        // Warm cache: input_tokens includes cache_read
        let warm_json = serde_json::json!({
            "content": [{"text": "ok", "type": "text"}],
            "usage": {
                "input_tokens": 25000,
                "output_tokens": 500,
                "cache_creation_input_tokens": 0,
                "cache_read_input_tokens": 10000
            }
        });
        let input_tokens_raw2 = warm_json["usage"]["input_tokens"].as_u64().unwrap_or(0);
        let cache_creation2 = warm_json["usage"]["cache_creation_input_tokens"]
            .as_u64()
            .unwrap_or(0);
        let cache_read2 = warm_json["usage"]["cache_read_input_tokens"]
            .as_u64()
            .unwrap_or(0);
        let adjusted2 = input_tokens_raw2
            .saturating_sub(cache_creation2)
            .saturating_sub(cache_read2);
        assert_eq!(adjusted2, 15000, "warm cache: 25000 - 0 - 10000 = 15000");

        // No cache fields: falls back to input_tokens unchanged
        let no_cache_json = serde_json::json!({
            "content": [{"text": "ok", "type": "text"}],
            "usage": {
                "input_tokens": 5000,
                "output_tokens": 100
            }
        });
        let input_tokens_raw3 = no_cache_json["usage"]["input_tokens"].as_u64().unwrap_or(0);
        let cache_creation3 = no_cache_json["usage"]["cache_creation_input_tokens"]
            .as_u64()
            .unwrap_or(0);
        let cache_read3 = no_cache_json["usage"]["cache_read_input_tokens"]
            .as_u64()
            .unwrap_or(0);
        let adjusted3 = input_tokens_raw3
            .saturating_sub(cache_creation3)
            .saturating_sub(cache_read3);
        assert_eq!(adjusted3, 5000, "no cache fields: 5000 unchanged");

        // Mixed: both cache creation and cache read present (partial cache hit)
        let mixed_json = serde_json::json!({
            "content": [{"text": "ok", "type": "text"}],
            "usage": {
                "input_tokens": 95000,
                "output_tokens": 500,
                "cache_creation_input_tokens": 60000,
                "cache_read_input_tokens": 20000
            }
        });
        let input_tokens_raw4 = mixed_json["usage"]["input_tokens"].as_u64().unwrap_or(0);
        let cache_creation4 = mixed_json["usage"]["cache_creation_input_tokens"]
            .as_u64()
            .unwrap_or(0);
        let cache_read4 = mixed_json["usage"]["cache_read_input_tokens"]
            .as_u64()
            .unwrap_or(0);
        let adjusted4 = input_tokens_raw4
            .saturating_sub(cache_creation4)
            .saturating_sub(cache_read4);
        assert_eq!(
            adjusted4, 15000,
            "mixed cache: 95000 - 60000 - 20000 = 15000"
        );
    }

    // ──────────────────────────────────────────────────────────────────────
    // Phase 5 — structured-output extractors
    // ──────────────────────────────────────────────────────────────────────

    #[test]
    fn extract_anthropic_tool_use_returns_stringified_input_when_present() {
        let body = serde_json::json!({
            "content": [
                { "type": "text", "text": "preamble" },
                {
                    "type": "tool_use",
                    "id": "tu_1",
                    "name": "submit_review",
                    "input": { "verdict": "ok", "issues": [] }
                }
            ]
        });
        let out = extract_anthropic_tool_use_or_text(&body);
        // Should be the JSON-stringified input — not the text preamble.
        let parsed: serde_json::Value = serde_json::from_str(&out).expect("output is JSON");
        assert_eq!(parsed["verdict"], "ok");
    }

    #[test]
    fn extract_anthropic_tool_use_prefers_tool_use_over_text_blocks() {
        // First tool_use wins regardless of name — every structured request
        // only ever exposes one tool, so a tool_use is always the one we
        // asked for, even when text blocks are also present.
        let body = serde_json::json!({
            "content": [
                { "type": "text", "text": "## Verdict\n\nLooks good" },
                {
                    "type": "tool_use",
                    "id": "tu_1",
                    "name": "any_caller_chosen_name",
                    "input": { "ok": true }
                }
            ]
        });
        let out = extract_anthropic_tool_use_or_text(&body);
        let parsed: serde_json::Value = serde_json::from_str(&out).expect("input parses as JSON");
        assert_eq!(parsed["ok"], true);
    }

    #[test]
    fn extract_anthropic_tool_use_falls_back_to_text_when_no_tool_use_block() {
        let body = serde_json::json!({
            "content": [
                { "type": "text", "text": "## Verdict\n\nLooks good" }
            ]
        });
        let out = extract_anthropic_tool_use_or_text(&body);
        assert_eq!(out, "## Verdict\n\nLooks good");
    }

    #[test]
    fn extract_anthropic_tool_use_returns_empty_when_no_content_array() {
        let body = serde_json::json!({});
        let out = extract_anthropic_tool_use_or_text(&body);
        assert!(out.is_empty());
    }

    #[test]
    fn extract_openai_tool_call_returns_arguments_string_when_present() {
        let body = serde_json::json!({
            "choices": [{
                "message": {
                    "content": null,
                    "tool_calls": [{
                        "id": "tc_1",
                        "type": "function",
                        "function": {
                            "name": "submit_review",
                            "arguments": "{\"verdict\":\"ok\",\"issues\":[]}"
                        }
                    }]
                }
            }]
        });
        let out = extract_openai_tool_call_or_text(&body);
        let parsed: serde_json::Value = serde_json::from_str(&out).expect("arguments parses");
        assert_eq!(parsed["verdict"], "ok");
    }

    #[test]
    fn extract_openai_tool_call_returns_arguments_for_any_tool_name() {
        // Regression: the extractor used to hardcode the hivemind reviewer
        // tool name and silently discard tool_calls with any other name —
        // breaking nurse classifier (`nurse_decisions`) and any future
        // caller. The contract is "first tool_call wins", period.
        let body = serde_json::json!({
            "choices": [{
                "message": {
                    "content": "",
                    "tool_calls": [{
                        "id": "tc_1",
                        "type": "function",
                        "function": {
                            "name": "nurse_decisions",
                            "arguments": "{\"decisions\":[]}"
                        }
                    }]
                }
            }]
        });
        let out = extract_openai_tool_call_or_text(&body);
        let parsed: serde_json::Value =
            serde_json::from_str(&out).expect("arguments parses as JSON");
        assert!(parsed["decisions"].is_array());
    }

    #[test]
    fn extract_anthropic_tool_use_returns_input_for_any_tool_name() {
        // Regression: same hardcoded-name bug existed on the Anthropic side.
        let body = serde_json::json!({
            "content": [
                { "type": "tool_use", "name": "nurse_decisions", "input": { "decisions": [] } }
            ]
        });
        let out = extract_anthropic_tool_use_or_text(&body);
        let parsed: serde_json::Value = serde_json::from_str(&out).expect("input parses as JSON");
        assert!(parsed["decisions"].is_array());
    }

    #[test]
    fn extract_openai_tool_call_falls_back_to_content_when_no_tool_calls() {
        let body = serde_json::json!({
            "choices": [{
                "message": {
                    "content": "## Verdict\n\nFine"
                }
            }]
        });
        let out = extract_openai_tool_call_or_text(&body);
        assert_eq!(out, "## Verdict\n\nFine");
    }

    // ──────────────────────────────────────────────────────────────────────
    // Audit 6.6 — Provider / StreamingProvider trait coverage
    // ──────────────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn mock_provider_call_via_trait_returns_canned_output() {
        let mock = MockProvider::new("canned-out");
        let provider: Arc<dyn Provider> = Arc::new(mock);

        let req = CallRequest::new("test-model", "sys", "user");
        let resp = provider.call(req).await.expect("call ok");

        assert_eq!(resp.output, "canned-out");
        assert_eq!(resp.model_id, "test-model");
    }

    #[tokio::test]
    async fn mock_provider_call_streaming_via_trait_emits_chunks() {
        let mock = MockProvider::new("ignored").with_streaming(StreamingMockConfig::from_chunks(
            ["hello", " ", "world"],
            Duration::from_millis(0),
        ));
        let provider: Arc<dyn Provider> = Arc::new(mock);

        // Upgrade to StreamingProvider via the explicit `as_streaming()` hook.
        let streaming = provider
            .as_streaming()
            .expect("mock provider should expose StreamingProvider");

        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<StreamChunk>();
        let req = CallRequest::new("test-model", "sys", "user");
        let resp = streaming.call_streaming(req, tx).await.expect("stream ok");

        assert_eq!(resp.output, "hello world");

        let mut deltas = Vec::new();
        while let Ok(c) = rx.try_recv() {
            deltas.push(c.delta);
        }
        assert_eq!(
            deltas,
            vec!["hello".to_string(), " ".to_string(), "world".to_string()]
        );
    }

    #[test]
    fn anthropic_provider_not_streaming_via_trait() {
        let provider: Arc<dyn Provider> = Arc::new(AnthropicProvider::new("k", None).unwrap());
        assert!(provider.as_streaming().is_none());
    }

    #[test]
    fn pi_subscription_provider_not_streaming_via_trait() {
        use crate::pi::manager::PiManager;
        let pi = Arc::new(PiManager::new_for_tests());
        let provider: Arc<dyn Provider> = Arc::new(PiSubscriptionProvider::new(
            "chatgpt",
            pi,
            subscription_models_for("chatgpt"),
        ));
        assert!(provider.as_streaming().is_none());
    }

    #[test]
    fn openai_compatible_provider_is_streaming_via_trait() {
        let provider: Arc<dyn Provider> = Arc::new(
            OpenAICompatibleProvider::new("https://example.com/v1", "k", "test", None).unwrap(),
        );
        assert!(provider.as_streaming().is_some());
    }

    // ------------------------------------------------------------------
    // tool_choice fallback — DeepSeek-reasoner / Kimi-K2 endpoints that
    // reject forced tool_choice with a 400. Pure-function tests for the
    // detector; the end-to-end retry path is exercised in the integration
    // test below against a local hyper server.
    // ------------------------------------------------------------------

    #[test]
    fn is_tool_choice_unsupported_matches_deepseek_official_body() {
        let body = r#"{"error":{"message":"deepseek-reasoner does not support this tool_choice","type":"invalid_request_error","param":null,"code":"invalid_request_error"}}"#;
        assert!(is_tool_choice_unsupported(body));
    }

    #[test]
    fn is_tool_choice_unsupported_matches_uppercase_variant() {
        let body = "deepseek-v4-pro Does Not Support Tool_Choice required";
        assert!(is_tool_choice_unsupported(body));
    }

    #[test]
    fn is_tool_choice_unsupported_matches_not_supported_phrasing() {
        let body = r#"{"error":{"message":"tool_choice is not supported on this model"}}"#;
        assert!(is_tool_choice_unsupported(body));
    }

    #[test]
    fn is_tool_choice_unsupported_matches_alibaba_qwen_thinking_mode() {
        // OpenRouter relays Alibaba's raw error verbatim for Qwen3 thinking-mode
        // models. The phrasing differs from DeepSeek/Kimi but the intent is the
        // same — the model refuses our forced tool_choice and we should retry
        // with "auto".
        let body = r#"{"error":{"message":"Provider returned error","code":400,"metadata":{"raw":"{\"error\":{\"message\":\"<400> InternalError.Algo.InvalidParameter: The tool_choice parameter does not support being set to required or object in thinking mode\"}}"}}}"#;
        assert!(is_tool_choice_unsupported(body));
    }

    #[test]
    fn is_tool_choice_unsupported_does_not_fire_on_malformed_tool_choice() {
        // A different class of 400 — the value is malformed, not unsupported.
        // We must NOT silently retry "auto" here; the request is broken.
        let body = r#"{"error":{"message":"invalid tool_choice: missing field 'function'"}}"#;
        assert!(!is_tool_choice_unsupported(body));
    }

    #[test]
    fn is_tool_choice_unsupported_does_not_fire_on_unrelated_400() {
        let body = r#"{"error":{"message":"messages: too many tokens"}}"#;
        assert!(!is_tool_choice_unsupported(body));
    }

    #[test]
    fn is_tool_choice_unsupported_does_not_fire_when_word_absent() {
        // Defensive: even a "does not support" phrasing without the
        // tool_choice literal shouldn't trigger the retry.
        let body = "model does not support function calling";
        assert!(!is_tool_choice_unsupported(body));
    }

    // ------------------------------------------------------------------
    // summarize_empty_response — diagnostic helper for 200-with-empty-body
    // failures (Kimi-K2.6 on neuralwatt observed). Pure JSON inspection.
    // ------------------------------------------------------------------

    #[test]
    fn summarize_empty_response_no_choices() {
        let body = serde_json::json!({});
        let s = summarize_empty_response(&body);
        assert!(s.contains("choices=0"), "{s}");
        assert!(s.contains("content=missing"), "{s}");
        assert!(s.contains("tool_calls=0"), "{s}");
        assert!(s.contains("completion_tokens=0"), "{s}");
    }

    #[test]
    fn summarize_empty_response_null_content_with_finish_reason() {
        let body = serde_json::json!({
            "choices": [{
                "finish_reason": "tool_calls",
                "message": { "role": "assistant", "content": null }
            }],
            "usage": { "completion_tokens": 0 }
        });
        let s = summarize_empty_response(&body);
        assert!(s.contains("choices=1"), "{s}");
        assert!(s.contains("finish_reason=tool_calls"), "{s}");
        assert!(s.contains("content=null"), "{s}");
    }

    #[test]
    fn summarize_empty_response_empty_string_content() {
        let body = serde_json::json!({
            "choices": [{ "finish_reason": "stop", "message": { "content": "" } }],
            "usage": { "completion_tokens": 42 }
        });
        let s = summarize_empty_response(&body);
        assert!(s.contains("content=empty_str"), "{s}");
        assert!(s.contains("completion_tokens=42"), "{s}");
    }

    #[test]
    fn summarize_empty_response_with_tool_calls() {
        let body = serde_json::json!({
            "choices": [{
                "finish_reason": "tool_calls",
                "message": {
                    "content": null,
                    "tool_calls": [
                        { "function": { "name": "submit_review", "arguments": "" } }
                    ]
                }
            }],
            "usage": { "completion_tokens": 17 }
        });
        let s = summarize_empty_response(&body);
        assert!(s.contains("tool_calls=1"), "{s}");
        assert!(s.contains("finish_reason=tool_calls"), "{s}");
    }

    #[test]
    fn summarize_empty_response_non_empty_content_still_summarizes() {
        // Helper doesn't gate on whether the body is "actually" empty —
        // it's called by the caller who already decided.
        let body = serde_json::json!({
            "choices": [{ "finish_reason": "length", "message": { "content": "hi" } }],
            "usage": { "completion_tokens": 1 }
        });
        let s = summarize_empty_response(&body);
        assert!(s.contains("content=non_empty_str"), "{s}");
        assert!(s.contains("finish_reason=length"), "{s}");
    }
}
