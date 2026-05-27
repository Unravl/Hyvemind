//! `LlmClassifier` — Tier-3 wrapper around `ProviderRegistry::call`.
//!
//! Builds a structured prompt from a session's `SessionHealth`, dispatches
//! via the chosen provider with the `nurse_decisions` tool schema, parses
//! the response into a [`NurseDecision`], and surfaces per-call errors.
//!
//! The classifier preserves the legacy `NURSE_PROVIDER_TIMEOUT_SECS = 90`
//! timeout (now `tunables::nurse_provider_timeout_secs`) and the
//! "`nurse_model == "none"` → skip" semantics.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Result};

use crate::nurse::config::NurseConfig;
use crate::nurse::health::SessionHealth;
use crate::nurse::schema::{nurse_input_schema, structured_config_for_provider, NURSE_TOOL_NAME};
use crate::nurse::snapshot::NurseDecision;
use crate::providers::provider_trait::CallRequest;
use crate::providers::ProviderRegistry;
use crate::state::sync::AsyncRwLock;

pub struct LlmClassifier {
    registry: Arc<AsyncRwLock<ProviderRegistry>>,
    /// Optional counter bumped once per provider call (Tier 3 path). Wired
    /// from `NurseHealthCounters::llm_calls_total` at engine construction
    /// so the topbar pill can show cumulative nurse-model API spend. None
    /// in unit tests that don't care.
    call_counter: Option<Arc<std::sync::atomic::AtomicU64>>,
}

pub struct ClassifyOutput {
    pub decision: NurseDecision,
    pub raw_response: String,
    pub provider: String,
    pub model: String,
    pub duration_ms: u64,
    /// Tokens served from the provider's prompt cache on this call.
    /// Anthropic: `cache_read_input_tokens`. DeepSeek: `prompt_cache_hit_tokens`.
    /// Providers without a cache report 0.
    pub cache_hit_tokens: u64,
    /// Tokens written into the provider's prompt cache on this call.
    /// Anthropic: `cache_creation_input_tokens`. Providers without explicit
    /// cache markers report 0.
    pub cache_write_tokens: u64,
}

/// Concrete error carried via `anyhow::Error` when the provider call
/// succeeded but the response couldn't be parsed back into a `NurseDecision`.
/// The dispatcher downcasts on this to log a rich
/// `classifier_returned_unparseable` event (with the cache stats from the
/// successful call) instead of a generic `classifier_failed` that loses the
/// cache visibility.
#[derive(Debug, Clone)]
pub struct ClassifierParseFailure {
    pub provider: String,
    pub model: String,
    pub duration_ms: u64,
    pub raw: String,
    pub parse_error: String,
    pub cache_hit_tokens: u64,
    pub cache_write_tokens: u64,
}

impl std::fmt::Display for ClassifierParseFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "nurse classifier parse failed: {} (provider={}, model={}, raw_len={})",
            self.parse_error,
            self.provider,
            self.model,
            self.raw.len()
        )
    }
}

impl std::error::Error for ClassifierParseFailure {}

impl LlmClassifier {
    pub fn new(registry: Arc<AsyncRwLock<ProviderRegistry>>) -> Self {
        Self {
            registry,
            call_counter: None,
        }
    }

    /// Builder: attach the engine-wide call counter. Increment fires
    /// immediately before each provider invocation in `classify_prepared`.
    pub fn with_call_counter(mut self, counter: Arc<std::sync::atomic::AtomicU64>) -> Self {
        self.call_counter = Some(counter);
        self
    }

    /// Build the user-prompt bytes that [`Self::classify_prepared`] will
    /// send. The dispatcher captures this on disk via `spawn_blocking`
    /// BEFORE invoking the provider so a mid-call crash leaves an
    /// unambiguous "invoked, never returned" file pair. Returning a
    /// string (not `Result<String>`) keeps the capture site
    /// infallible — serialisation cannot fail for the JSON object we
    /// construct, but a defensive `unwrap_or_else` makes that explicit
    /// rather than relying on the invariant.
    ///
    /// `_config` is accepted so the signature matches the `ClassifierBackend`
    /// trait used by the dispatcher tests; the body only consults `health`.
    pub fn build_prompt(&self, _config: &NurseConfig, health: &SessionHealth) -> String {
        build_prompt_body(health).unwrap_or_else(|e| {
            tracing::warn!(
                error = %e,
                "nurse classifier: build_prompt serialisation failed — sending fallback"
            );
            r#"{"signals": [], "note": "prompt serialisation failed"}"#.to_string()
        })
    }

    /// Run a Tier-3 classification against an already-built prompt. The
    /// dispatcher splits this from [`Self::build_prompt`] so the prompt
    /// can be captured to disk before the provider is invoked.
    ///
    /// Returns `Ok(Some(out))` on success, `Ok(None)` if the classifier
    /// is disabled (`nurse_model` resolves to `None`), `Err` on provider
    /// or parse failure.
    pub async fn classify_prepared(
        &self,
        config: &NurseConfig,
        _health: &SessionHealth,
        prompt: &str,
    ) -> Result<Option<ClassifyOutput>> {
        let model = match config.nurse_model.as_deref() {
            Some(m) if !m.is_empty() && m != "none" => m.to_string(),
            _ => return Ok(None),
        };
        let provider_name = resolve_provider(config, &model);

        // Strip the `provider/` prefix from the model id for any provider
        // whose API expects bare model names (Anthropic, DeepSeek, OpenAI,
        // PiSubscription, and any other OpenAI-compatible backend).
        // OpenRouter is the ONE exception — its API routes by the full
        // prefixed id (e.g. `anthropic/claude-3.5-sonnet`), so we pass the
        // model through unchanged.
        let api_model = if provider_name == "openrouter" {
            model.clone()
        } else {
            model
                .split_once('/')
                .map(|(_, m)| m.to_string())
                .unwrap_or_else(|| model.clone())
        };

        let system = crate::nurse::prompt::default_system_prompt().to_string();

        let structured = structured_config_for_provider(&provider_name);
        // Opt into provider-side prompt caching for the static prefix
        // (system prompt + tool schema). Anthropic honours this via
        // `cache_control: ephemeral` markers; DeepSeek and other
        // OpenAI-compatible backends ignore the flag and rely on automatic
        // prefix caching, which our byte-stable request body already supports.
        let req = CallRequest::new(api_model, system, prompt.to_string())
            .with_timeout(Some(Duration::from_secs(
                crate::tunables::nurse_provider_timeout_secs(),
            )))
            .with_structured(Some(structured))
            .with_cache_static_prefix(true);

        // Resolve and clone the Arc<dyn Provider> under the registry's read
        // lock, then drop the guard before making the call so refreshes
        // can proceed even on a long classifier call.
        let provider_arc = {
            let reg = self.registry.read().await;
            reg.get(&provider_name)
                .ok_or_else(|| {
                    anyhow!(
                        "nurse classifier provider '{}' not registered",
                        provider_name
                    )
                })?
                .clone()
        };

        let started = std::time::Instant::now();
        // Bump counter BEFORE the call so the topbar number reflects
        // intent — an in-flight crash still counts as a request issued.
        if let Some(c) = &self.call_counter {
            c.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        let response = provider_arc.call(req).await?;
        let duration_ms = started.elapsed().as_millis() as u64;

        let cache_hit_tokens = response.cache_hit_tokens;
        let cache_write_tokens = response.cache_write_tokens;
        let raw = response.output.clone();
        let decision = match parse_first_decision(&raw) {
            Ok(d) => d,
            Err(parse_err) => {
                // Provider call succeeded — the parse failed. Surface a rich
                // error so the dispatcher can log a `classifier_returned_unparseable`
                // event carrying the cache stats from this call instead of
                // silently discarding them into a generic `classifier_failed`.
                return Err(anyhow::Error::new(ClassifierParseFailure {
                    provider: provider_name,
                    model,
                    duration_ms,
                    raw,
                    parse_error: format!("{}", parse_err),
                    cache_hit_tokens,
                    cache_write_tokens,
                }));
            }
        };
        Ok(Some(ClassifyOutput {
            decision,
            raw_response: raw,
            provider: provider_name,
            model,
            duration_ms,
            cache_hit_tokens,
            cache_write_tokens,
        }))
    }

    /// Thin convenience wrapper: build prompt, then classify. Kept for
    /// any caller that doesn't need to capture the prompt independently.
    pub async fn classify(
        &self,
        config: &NurseConfig,
        health: &SessionHealth,
    ) -> Result<Option<ClassifyOutput>> {
        let prompt = self.build_prompt(config, health);
        self.classify_prepared(config, health, &prompt).await
    }
}

/// Build the structured prompt body fed to the model. Bounded byte budget
/// — the prompt MUST stay small enough that 50 active sessions don't push
/// a 150 KB body at every classifier call.
fn build_prompt_body(health: &SessionHealth) -> Result<String> {
    let snapshot = serde_json::json!({
        "session": {
            "session_id": health.session_id,
            "owner": health.owner,
            "tier": health.tier,
            "intervention_count": health.intervention_count,
        },
        "signals": health
            .signals
            .iter()
            .map(|s| {
                serde_json::json!({
                    "detector": s.detector,
                    "severity": s.severity,
                    "dedup_key": s.dedup_key,
                    "summary": s.summary,
                    "evidence": s.evidence,
                })
            })
            .collect::<Vec<_>>(),
        "last_observation": health.last_observation,
    });
    let raw = serde_json::to_string_pretty(&snapshot)?;
    Ok(raw)
}

/// Try to extract a `NurseDecision` from the raw provider output. Accepts
/// two shapes: a `tool_use` envelope (`{ "decisions": [...] }`) or a bare
/// decision object.
fn parse_first_decision(raw: &str) -> Result<NurseDecision> {
    // Try array envelope first.
    if let Ok(envelope) = serde_json::from_str::<serde_json::Value>(raw) {
        if let Some(arr) = envelope.get("decisions").and_then(|v| v.as_array()) {
            if let Some(first) = arr.first() {
                if let Ok(d) = serde_json::from_value::<NurseDecision>(first.clone()) {
                    return Ok(d);
                }
            }
        }
        if let Ok(d) = serde_json::from_value::<NurseDecision>(envelope) {
            return Ok(d);
        }
    }
    Err(anyhow!(
        "could not parse NurseDecision from classifier output (tool={}, len={})",
        NURSE_TOOL_NAME,
        raw.len()
    ))
}

/// Resolve provider name from config + model id. Mirrors the legacy
/// `resolve_nurse_provider_and_model` helper.
fn resolve_provider(config: &NurseConfig, model: &str) -> String {
    if let Some(p) = config.nurse_provider.as_deref() {
        if !p.is_empty() {
            return p.to_string();
        }
    }
    if let Some((prefix, _)) = model.split_once('/') {
        return prefix.to_string();
    }
    // Heuristic fall-back: model names beginning with `claude-` route to
    // Anthropic; everything else defaults to `openrouter`.
    if model.starts_with("claude-") {
        "anthropic".to_string()
    } else {
        "openrouter".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nurse::health::Tier;

    #[test]
    fn parse_decision_array_envelope() {
        let raw = r#"{
            "decisions": [
                { "decision": "steer", "reasoning": "loops", "message": "try harder" }
            ]
        }"#;
        let d = parse_first_decision(raw).unwrap();
        match d {
            NurseDecision::Steer { message, .. } => assert_eq!(message, "try harder"),
            other => panic!("expected Steer, got {:?}", other),
        }
    }

    #[test]
    fn parse_decision_bare_object() {
        let raw = r#"{ "decision": "leave_it", "reasoning": "fine", "check_back_secs": 30 }"#;
        let d = parse_first_decision(raw).unwrap();
        match d {
            NurseDecision::LeaveIt {
                check_back_secs, ..
            } => assert_eq!(check_back_secs, 30),
            other => panic!("expected LeaveIt, got {:?}", other),
        }
    }

    #[test]
    fn resolve_provider_prefers_explicit_override() {
        let mut cfg = NurseConfig::default();
        cfg.nurse_provider = Some("anthropic".into());
        assert_eq!(resolve_provider(&cfg, "anything"), "anthropic");
    }

    #[test]
    fn resolve_provider_splits_model_prefix() {
        let cfg = NurseConfig::default();
        assert_eq!(resolve_provider(&cfg, "anthropic/claude"), "anthropic");
        assert_eq!(resolve_provider(&cfg, "openrouter/gpt-4o"), "openrouter");
    }

    #[test]
    fn resolve_provider_heuristic_for_claude() {
        let cfg = NurseConfig::default();
        assert_eq!(resolve_provider(&cfg, "claude-sonnet-4-6"), "anthropic");
    }

    #[test]
    fn build_prompt_serializes_signals_and_owner() {
        use crate::nurse::health::{Severity, Signal};
        use crate::pi::session::SessionOwner;
        let mut h = SessionHealth::new("s1".into(), &SessionOwner::Unknown);
        h.push_signal(Signal {
            detector: "stall",
            severity: Severity::Stalled,
            dedup_key: "stall".into(),
            summary: "stuck".into(),
            raised_at: chrono::Utc::now(),
            evidence: serde_json::json!({"idle_ms": 200_000}),
        });
        h.tier = Tier::Stalled;
        let body = build_prompt_body(&h).unwrap();
        assert!(body.contains("\"stall\""));
        assert!(body.contains("\"stalled\""));
        assert!(body.contains("\"idle_ms\": 200000"));
    }

    #[test]
    fn classifier_build_prompt_method_matches_internal_helper() {
        use crate::nurse::health::{Severity, Signal};
        use crate::pi::session::SessionOwner;
        use crate::providers::ProviderRegistry;

        let registry = Arc::new(AsyncRwLock::new(ProviderRegistry::default()));
        let classifier = LlmClassifier::new(registry);
        let cfg = NurseConfig::default();

        let mut h = SessionHealth::new("s1".into(), &SessionOwner::Unknown);
        h.push_signal(Signal {
            detector: "stall",
            severity: Severity::Stalled,
            dedup_key: "stall".into(),
            summary: "stuck".into(),
            raised_at: chrono::Utc::now(),
            evidence: serde_json::json!({"idle_ms": 200_000}),
        });

        // Method form (used by dispatcher capture) must match the
        // internal helper (used by classify_prepared). Diverging bytes
        // would break the "capture matches what provider sees"
        // contract that the dispatcher relies on.
        let via_method = classifier.build_prompt(&cfg, &h);
        let via_helper = build_prompt_body(&h).unwrap();
        assert_eq!(via_method, via_helper);
    }
}
