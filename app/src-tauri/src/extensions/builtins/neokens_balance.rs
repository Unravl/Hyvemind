//! Neokens balance extension.
//!
//! Probes `GET /api/workspace` against the Neokens **dashboard** API
//! (`https://neokens.com`) and reports the remaining credit balance as
//! the headline metric. Tones:
//!   - `crit`   if balance <= 0 or remaining < 20% of plan
//!   - `warn`   if remaining < 50% of plan
//!   - `ok`     otherwise
//!
//! The "plan amount" is configurable per-extension via
//! `extension_settings.preferences["plan_amount"]` (decimal string,
//! USD); defaults to $10 when unset.
//!
//! NOTE on host derivation: the configured provider endpoint points
//! at the **inference proxy** (e.g. `https://api.neokens.com/v1` or
//! `https://api.quatarly.cloud/v1/chat/completions`) which only
//! exposes `POST /v1/chat/completions` and friends — `GET
//! /api/workspace` returns 404 there. The dashboard / workspace API
//! lives on the apex `https://neokens.com`. See
//! `dashboard_base_url` for the override matrix.
//!
//! **Status (2026-05): the dashboard endpoints (`/api/workspace`,
//! `/api/usage`, `/api/billing/history`, `/api/keys`) all require a
//! session cookie set by `POST /auth/login` with email + password.
//! Bearer / x-api-key / query-param auth using the `qua-sub-…` API
//! key is rejected with `401 "Not authenticated."` in every form.
//! The `qua-sub-…` key is scoped only to inference (`/v1/chat/
//! completions`, `/v1/messages`, `/v1/models`).** Until Neokens
//! ships an API-key-introspectable endpoint (e.g. `GET /v1/credits`),
//! this extension treats both 401/403 and 404 as terminal
//! `ExtensionError::Unsupported` so the poller exits permanently
//! instead of looping on a request that can never succeed.

use async_trait::async_trait;
use reqwest::StatusCode;
use tracing::{info, trace, warn};

use crate::extensions::context::ExtensionContext;
use crate::extensions::traits::{ProviderExtension, UsageProvider};
use crate::extensions::types::{
    Capability, ExtensionError, ExtensionManifest, MetricKind, Tone, UsageMetric, UsageSnapshot,
};

/// Default Neokens dashboard base URL.
const DEFAULT_BASE: &str = "https://neokens.com";

/// Maximum number of bytes of an unparseable response body to log on
/// parse failure. Keeps the debug log line bounded.
const BODY_PREVIEW_LIMIT: usize = 500;

/// Derive the dashboard base URL from a (possibly-absent) provider
/// endpoint.
///
/// Rules:
/// - `None` → `https://neokens.com`.
/// - Endpoint host contains `api.neokens.com` or `quatarly.cloud`
///   (known inference proxies that do **not** serve the workspace
///   API) → `https://neokens.com`.
/// - Any other host → strip `/v1*` suffix and trailing slashes,
///   keep the result. This supports a future self-hosted dashboard
///   (e.g. `https://dashboard.example.com/v1` →
///   `https://dashboard.example.com`).
fn dashboard_base_url(endpoint: Option<&str>) -> String {
    let Some(raw) = endpoint else {
        return DEFAULT_BASE.to_string();
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return DEFAULT_BASE.to_string();
    }

    let lower = trimmed.to_lowercase();
    if lower.contains("api.neokens.com") || lower.contains("quatarly.cloud") {
        return DEFAULT_BASE.to_string();
    }

    let cleaned = trimmed
        .trim_end_matches('/')
        .trim_end_matches("/v1/chat/completions")
        .trim_end_matches("/v1")
        .trim_end_matches('/')
        .to_string();

    if cleaned.is_empty() {
        DEFAULT_BASE.to_string()
    } else {
        cleaned
    }
}

pub struct NeokensBalance {
    provider_id: String,
    /// Pre-formatted extension id (`type_id:provider_id`).
    extension_id: String,
    /// Base URL derived from the provider configuration.
    base_url: String,
}

impl NeokensBalance {
    pub fn new(provider_id: impl Into<String>, endpoint: Option<&str>) -> Self {
        let provider_id = provider_id.into();
        let extension_id = format!("neokens_balance:{}", provider_id);
        let base_url = dashboard_base_url(endpoint);
        Self {
            provider_id,
            extension_id,
            base_url,
        }
    }
}

impl ProviderExtension for NeokensBalance {
    fn manifest(&self) -> ExtensionManifest {
        ExtensionManifest {
            id: self.extension_id.clone(),
            type_id: "neokens_balance".to_string(),
            provider_id: self.provider_id.clone(),
            display_name: format!("Neokens Balance ({})", self.provider_id),
            description: "Displays remaining Neokens account credit.".to_string(),
            capabilities: vec![Capability::Usage, Capability::Billing],
            requires_api_key: true,
            docs_url: Some("https://neokens.com/docs".to_string()),
        }
    }

    fn usage_provider(&self) -> Option<&dyn UsageProvider> {
        Some(self)
    }
}

/// Map an HTTP status from the workspace endpoint into an
/// `ExtensionError`. Returns `Ok(())` if the status indicates the
/// caller should proceed to body parsing, otherwise the appropriate
/// terminal/transient error.
///
/// Exposed at module level so it's unit-testable in isolation.
fn classify_status(status: StatusCode, url: &str) -> Result<(), ExtensionError> {
    if status.is_success() {
        return Ok(());
    }
    if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
        // Empirically, `/api/workspace` returns 401 "Not authenticated."
        // for every form of API-key auth (Bearer, x-api-key, query
        // param, cookie). The endpoint requires a session cookie set
        // by `POST /auth/login` with email + password. There is no
        // value of `qua-sub-…` API key that will succeed here, so
        // 401/403 is terminal — Unsupported makes the poller exit
        // permanently instead of looping forever on Auth/backoff.
        return Err(ExtensionError::Unsupported(format!(
            "Neokens {} returned {} — endpoint requires session-cookie auth and does not accept API keys",
            url,
            status.as_u16()
        )));
    }
    if status == StatusCode::NOT_FOUND {
        // Hard misconfiguration — the dashboard host doesn't serve
        // this route. Per CLAUDE.md, `Unsupported` is terminal and
        // the poller exits permanently. The user can re-enable by
        // editing the provider endpoint in Settings, which triggers
        // a registry refresh.
        return Err(ExtensionError::Unsupported(format!(
            "Neokens workspace endpoint returned 404 at {} — host does not serve /api/workspace",
            url
        )));
    }
    Err(ExtensionError::Network(format!(
        "Neokens API returned status {}",
        status.as_u16()
    )))
}

/// Try every known shape for the credit-balance field. Returns the
/// numeric balance on success.
fn extract_balance(body: &serde_json::Value) -> Option<f64> {
    let candidates = [
        body.get("balance"),
        body.get("credits"),
        body.get("credit"),
        body.get("total_balance"),
        body.get("remaining_credits"),
        body.get("credits_remaining"),
        body.get("creditBalance"),
        body.get("credit_balance"),
        body.pointer("/workspace/balance"),
        body.pointer("/workspace/credits"),
        body.pointer("/wallet/balance"),
        body.pointer("/wallet/credits"),
        body.pointer("/wallet/credit_balance"),
        body.pointer("/account/balance"),
        body.pointer("/account/credits"),
        body.pointer("/data/balance"),
        body.pointer("/data/credits"),
    ];

    for c in candidates.into_iter().flatten() {
        match c {
            serde_json::Value::Number(n) => {
                if let Some(f) = n.as_f64() {
                    return Some(f);
                }
            }
            serde_json::Value::String(s) => {
                if let Ok(f) = s.parse::<f64>() {
                    return Some(f);
                }
            }
            _ => continue,
        }
    }
    None
}

/// Try every known shape for the currency field. Defaults to `"USD"`.
fn extract_currency(body: &serde_json::Value) -> String {
    let candidates = [
        body.get("currency"),
        body.pointer("/wallet/currency"),
        body.pointer("/account/currency"),
        body.pointer("/data/currency"),
    ];
    for c in candidates.into_iter().flatten() {
        if let Some(s) = c.as_str() {
            if !s.is_empty() {
                return s.to_string();
            }
        }
    }
    "USD".to_string()
}

fn body_preview(body: &str) -> String {
    if body.len() <= BODY_PREVIEW_LIMIT {
        body.to_string()
    } else {
        // Slice on char boundary to avoid panic on multibyte input.
        let mut end = BODY_PREVIEW_LIMIT;
        while end > 0 && !body.is_char_boundary(end) {
            end -= 1;
        }
        body[..end].to_string()
    }
}

#[async_trait]
impl UsageProvider for NeokensBalance {
    #[tracing::instrument(
        skip(self, ctx),
        fields(
            provider_id = %self.provider_id,
            extension_id = %self.extension_id,
        )
    )]
    async fn fetch(&self, ctx: &ExtensionContext) -> Result<UsageSnapshot, ExtensionError> {
        // ── Guard: require API key ────────────────────────────────────
        let api_key = ctx.api_key(&self.provider_id).await.ok_or_else(|| {
            ExtensionError::Auth(format!(
                "No Neokens API key configured for provider '{}' — set it in Settings",
                self.provider_id
            ))
        })?;
        if api_key.is_empty() {
            return Err(ExtensionError::Auth("API key is empty".to_string()));
        }

        // ── Build balance URL ─────────────────────────────────────────
        let url = format!("{}/api/workspace", self.base_url);
        info!(%url, "fetching neokens workspace");

        // ── Fetch: try Bearer first, then x-api-key on 401/403 ───────
        let bearer_resp = ctx
            .http()
            .get(&url)
            .bearer_auth(&api_key)
            .send()
            .await
            .map_err(|e| ExtensionError::Network(format!("HTTP request failed: {}", e)))?;
        let bearer_status = bearer_resp.status();
        info!(
            status = bearer_status.as_u16(),
            scheme = "bearer",
            "neokens workspace response"
        );

        let (resp, status, auth_scheme) = if bearer_status == StatusCode::UNAUTHORIZED
            || bearer_status == StatusCode::FORBIDDEN
        {
            // Drop the failed bearer response and retry with the
            // legacy `x-api-key` header.
            drop(bearer_resp);
            let retry = ctx
                .http()
                .get(&url)
                .header("x-api-key", &api_key)
                .send()
                .await
                .map_err(|e| {
                    ExtensionError::Network(format!("HTTP request failed (x-api-key): {}", e))
                })?;
            let retry_status = retry.status();
            info!(
                status = retry_status.as_u16(),
                scheme = "x-api-key",
                "neokens workspace response"
            );
            if retry_status == StatusCode::UNAUTHORIZED || retry_status == StatusCode::FORBIDDEN {
                // Both auth schemes rejected — this is the
                // permanent server-side state for API keys
                // (dashboard endpoint requires session cookie).
                // See module-level docs for the full story.
                return Err(ExtensionError::Unsupported(format!(
                        "Neokens {} returned 401/403 with both Bearer and x-api-key — endpoint does not accept API keys",
                        url
                    )));
            }
            (retry, retry_status, "x-api-key")
        } else {
            (bearer_resp, bearer_status, "bearer")
        };

        classify_status(status, &url)?;
        info!(scheme = auth_scheme, "neokens auth scheme succeeded");

        let body_text = resp
            .text()
            .await
            .map_err(|e| ExtensionError::Network(e.to_string()))?;

        let body: serde_json::Value = match serde_json::from_str(&body_text) {
            Ok(v) => v,
            Err(e) => {
                warn!(
                    target: "extensions::neokens_balance",
                    provider_id = %self.provider_id,
                    %url,
                    body_len = body_text.len(),
                    body_preview = %body_preview(&body_text),
                    error = %e,
                    "neokens workspace body failed JSON parse",
                );
                return Err(ExtensionError::Parse(format!(
                    "Failed to parse workspace JSON response: {}",
                    e
                )));
            }
        };

        // ── Flexible balance extraction ────────────────────────────────
        let balance = match extract_balance(&body) {
            Some(b) => b,
            None => {
                warn!(
                    target: "extensions::neokens_balance",
                    provider_id = %self.provider_id,
                    %url,
                    body_len = body_text.len(),
                    body_preview = %body_preview(&body_text),
                    "neokens workspace response did not contain a recognized balance key",
                );
                return Err(ExtensionError::Parse(format!(
                    "Could not extract balance/credits from response: {}",
                    body_text
                )));
            }
        };

        let currency = extract_currency(&body);

        // ── Determine tone ─────────────────────────────────────────────
        let plan_amount = ctx
            .extension_settings(&self.extension_id)
            .await
            .preferences
            .get("plan_amount")
            .and_then(|v| v.parse::<f64>().ok())
            .unwrap_or(10.0); // Default threshold in dollars

        let pct = if plan_amount > 0.0 {
            balance / plan_amount
        } else {
            1.0
        };
        let tone: Tone = if balance <= 0.0 {
            Tone::Crit
        } else if pct < 0.2 {
            Tone::Crit
        } else if pct < 0.5 {
            Tone::Warn
        } else {
            Tone::Ok
        };

        trace!(balance, %currency, "parsed neokens balance");

        // ── Build snapshot ────────────────────────────────────────────
        let display = if currency.eq_ignore_ascii_case("CNY") {
            format!("¥{:.2}", balance)
        } else {
            format!("${:.2}", balance)
        };

        let headline = UsageMetric {
            key: "balance".to_string(),
            label: "Balance".to_string(),
            display: display.clone(),
            value: balance,
            kind: MetricKind::Currency,
            tone,
        };

        let metrics = vec![
            headline.clone(),
            UsageMetric {
                key: "plan_amount".to_string(),
                label: "Plan Amount".to_string(),
                display: format!("${:.2}", plan_amount),
                value: plan_amount,
                kind: MetricKind::Currency,
                tone: Tone::Neutral,
            },
        ];

        let raw = serde_json::to_value(&body).ok();

        Ok(UsageSnapshot {
            extension_id: self.extension_id.clone(),
            provider_id: self.provider_id.clone(),
            fetched_at: chrono::Utc::now().timestamp(),
            headline: Some(headline),
            metrics,
            raw,
        })
    }

    fn refresh_interval_secs(&self) -> u64 {
        300
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Endpoint / host derivation ──────────────────────────────────

    #[test]
    fn endpoint_derivation_default_when_endpoint_none() {
        let ext = NeokensBalance::new("neokens", None);
        assert_eq!(ext.base_url, "https://neokens.com");
    }

    #[test]
    fn endpoint_derivation_overrides_api_neokens() {
        // User configured the inference proxy URL — must be
        // rewritten to the dashboard apex.
        let ext = NeokensBalance::new("neokens", Some("https://api.neokens.com/v1"));
        assert_eq!(ext.base_url, "https://neokens.com");
    }

    #[test]
    fn endpoint_derivation_overrides_api_neokens_chat_completions() {
        let ext = NeokensBalance::new(
            "neokens",
            Some("https://api.neokens.com/v1/chat/completions"),
        );
        assert_eq!(ext.base_url, "https://neokens.com");
    }

    #[test]
    fn endpoint_derivation_overrides_quatarly() {
        let ext = NeokensBalance::new(
            "neokens",
            Some("https://api.quatarly.cloud/v1/chat/completions"),
        );
        assert_eq!(ext.base_url, "https://neokens.com");
    }

    #[test]
    fn endpoint_derivation_keeps_custom_dashboard_host() {
        let ext = NeokensBalance::new("neokens", Some("https://dashboard.example.com/v1"));
        assert_eq!(ext.base_url, "https://dashboard.example.com");
    }

    #[test]
    fn endpoint_derivation_strips_trailing_slash() {
        let ext = NeokensBalance::new("neokens", Some("https://dashboard.example.com/v1/"));
        assert_eq!(ext.base_url, "https://dashboard.example.com");
    }

    #[test]
    fn endpoint_derivation_empty_string_falls_back_to_default() {
        let ext = NeokensBalance::new("neokens", Some(""));
        assert_eq!(ext.base_url, "https://neokens.com");
    }

    // ── Manifest ────────────────────────────────────────────────────

    #[test]
    fn manifest_id_is_composite() {
        let ext = NeokensBalance::new("neokens", None);
        assert_eq!(ext.manifest().id, "neokens_balance:neokens");
        assert_eq!(ext.manifest().type_id, "neokens_balance");
        assert_eq!(ext.manifest().provider_id, "neokens");
    }

    // ── Balance extraction across shapes ────────────────────────────

    fn parse(b: &str) -> serde_json::Value {
        serde_json::from_str(b).unwrap()
    }

    #[test]
    fn extract_balance_number_root_key() {
        assert_eq!(
            extract_balance(&parse(r#"{"balance": 12.34}"#)).unwrap(),
            12.34
        );
    }

    #[test]
    fn extract_balance_string_root_key() {
        assert_eq!(
            extract_balance(&parse(r#"{"balance": "12.34"}"#)).unwrap(),
            12.34
        );
    }

    #[test]
    fn extract_balance_credits_key() {
        assert_eq!(
            extract_balance(&parse(r#"{"credits": 50.0}"#)).unwrap(),
            50.0
        );
    }

    #[test]
    fn extract_balance_workspace_nested() {
        assert_eq!(
            extract_balance(&parse(r#"{"workspace": {"balance": 25.5}}"#)).unwrap(),
            25.5
        );
    }

    #[test]
    fn extract_balance_remaining_credits() {
        assert_eq!(
            extract_balance(&parse(r#"{"remaining_credits": 7.25}"#)).unwrap(),
            7.25
        );
    }

    #[test]
    fn extract_balance_wallet_balance() {
        assert_eq!(
            extract_balance(&parse(r#"{"wallet": {"balance": 99.0}}"#)).unwrap(),
            99.0
        );
    }

    #[test]
    fn extract_balance_credit_balance_camel_case() {
        assert_eq!(
            extract_balance(&parse(r#"{"creditBalance": 3.50}"#)).unwrap(),
            3.50
        );
    }

    #[test]
    fn extract_balance_account_credits() {
        assert_eq!(
            extract_balance(&parse(r#"{"account": {"credits": 41.0}}"#)).unwrap(),
            41.0
        );
    }

    #[test]
    fn extract_balance_data_balance() {
        assert_eq!(
            extract_balance(&parse(r#"{"data": {"balance": 17.5}}"#)).unwrap(),
            17.5
        );
    }

    #[test]
    fn extract_balance_returns_none_on_unknown_shape() {
        assert!(extract_balance(&parse(r#"{"foo": "bar"}"#)).is_none());
    }

    // ── Currency extraction ─────────────────────────────────────────

    #[test]
    fn currency_defaults_to_usd() {
        assert_eq!(extract_currency(&parse(r#"{"balance": 1}"#)), "USD");
    }

    #[test]
    fn currency_root_key() {
        assert_eq!(extract_currency(&parse(r#"{"currency": "CNY"}"#)), "CNY");
    }

    #[test]
    fn currency_wallet_nested() {
        assert_eq!(
            extract_currency(&parse(r#"{"wallet": {"currency": "EUR"}}"#)),
            "EUR"
        );
    }

    #[test]
    fn currency_account_nested() {
        assert_eq!(
            extract_currency(&parse(r#"{"account": {"currency": "GBP"}}"#)),
            "GBP"
        );
    }

    // ── Status classification ───────────────────────────────────────

    #[test]
    fn status_200_ok_passes() {
        assert!(classify_status(StatusCode::OK, "https://x").is_ok());
    }

    #[test]
    fn status_401_maps_to_unsupported() {
        // The dashboard endpoint requires session-cookie auth; the
        // qua-sub-… API key cannot succeed regardless of header
        // shape. Unsupported is terminal, so the poller exits
        // permanently instead of looping on Auth/backoff.
        let err = classify_status(
            StatusCode::UNAUTHORIZED,
            "https://neokens.com/api/workspace",
        )
        .unwrap_err();
        match err {
            ExtensionError::Unsupported(msg) => {
                assert!(msg.contains("401"), "msg={msg}");
                assert!(
                    msg.contains("https://neokens.com/api/workspace"),
                    "msg={msg}"
                );
            }
            other => panic!("expected Unsupported, got {:?}", other),
        }
    }

    #[test]
    fn status_403_maps_to_unsupported() {
        let err = classify_status(StatusCode::FORBIDDEN, "https://neokens.com/api/workspace")
            .unwrap_err();
        match err {
            ExtensionError::Unsupported(msg) => {
                assert!(msg.contains("403"), "msg={msg}");
            }
            other => panic!("expected Unsupported, got {:?}", other),
        }
    }

    #[test]
    fn status_404_maps_to_unsupported_with_url() {
        let err = classify_status(StatusCode::NOT_FOUND, "https://neokens.com/api/workspace")
            .unwrap_err();
        match err {
            ExtensionError::Unsupported(msg) => {
                assert!(
                    msg.contains("https://neokens.com/api/workspace"),
                    "msg={msg}"
                );
                assert!(msg.contains("404"));
            }
            other => panic!("expected Unsupported, got {:?}", other),
        }
    }

    #[test]
    fn status_500_maps_to_network() {
        let err = classify_status(StatusCode::INTERNAL_SERVER_ERROR, "https://x").unwrap_err();
        assert!(matches!(err, ExtensionError::Network(_)));
    }

    // ── body_preview ────────────────────────────────────────────────

    #[test]
    fn body_preview_short_string_returned_whole() {
        assert_eq!(body_preview("hi"), "hi");
    }

    #[test]
    fn body_preview_long_string_truncated() {
        let s = "x".repeat(BODY_PREVIEW_LIMIT + 100);
        let preview = body_preview(&s);
        assert_eq!(preview.len(), BODY_PREVIEW_LIMIT);
    }

    #[test]
    fn body_preview_truncates_on_char_boundary() {
        // A 1-byte 'a' filler then a 4-byte emoji at the boundary.
        let mut s = "a".repeat(BODY_PREVIEW_LIMIT - 2);
        s.push('🦀'); // 4 bytes — straddles the limit
        let preview = body_preview(&s);
        // Should not panic and should slice at a char boundary.
        assert!(preview.len() <= BODY_PREVIEW_LIMIT);
        assert!(preview.is_char_boundary(preview.len()));
    }

    // ── Tone (preserved from previous tests) ────────────────────────

    fn tone_for(balance: f64, plan_amount: f64) -> Tone {
        let pct = if plan_amount > 0.0 {
            balance / plan_amount
        } else {
            1.0
        };
        if balance <= 0.0 {
            Tone::Crit
        } else if pct < 0.2 {
            Tone::Crit
        } else if pct < 0.5 {
            Tone::Warn
        } else {
            Tone::Ok
        }
    }

    #[test]
    fn tone_is_crit_when_balance_zero() {
        assert_eq!(tone_for(0.0, 10.0), Tone::Crit);
    }

    #[test]
    fn tone_is_crit_when_below_20_percent() {
        assert_eq!(tone_for(1.0, 10.0), Tone::Crit);
    }

    #[test]
    fn tone_is_warn_when_below_50_percent() {
        assert_eq!(tone_for(4.0, 10.0), Tone::Warn);
    }

    #[test]
    fn tone_is_ok_when_above_50_percent() {
        assert_eq!(tone_for(8.0, 10.0), Tone::Ok);
    }
}
