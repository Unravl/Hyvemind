//! DeepSeek balance extension.
//!
//! Probes `GET /v1/user/balance` against the DeepSeek API and reports
//! the remaining credit balance as the headline metric. Tones:
//!   - `crit`   if balance ≤ 0 or remaining < 20% of plan
//!   - `warn`   if remaining < 50% of plan
//!   - `ok`     otherwise
//!
//! The "plan amount" is configurable per-extension via
//! `extension_settings.preferences["plan_amount"]` (decimal string,
//! USD); defaults to $10 when unset.
//!
//! Endpoint derivation: takes the configured provider endpoint (e.g.
//! `https://api.deepseek.com/v1` or `https://api.deepseek.com/v1/chat/completions`),
//! strips any trailing `/v1` or `/v1/chat/completions` suffix, and
//! appends `/v1/user/balance`.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::extensions::context::ExtensionContext;
use crate::extensions::traits::{ProviderExtension, UsageProvider};
use crate::extensions::types::{
    Capability, ExtensionError, ExtensionManifest, MetricKind, Tone, UsageMetric, UsageSnapshot,
};

/// Default DeepSeek endpoint used when no per-provider endpoint is
/// configured.
const DEFAULT_BASE: &str = "https://api.deepseek.com/v1";

fn default_true() -> bool {
    true
}

/// DeepSeek balance API response shape.
/// Discovered via live API: https://api.deepseek.com/v1/user/balance
#[derive(Debug, Clone, Deserialize, Serialize)]
struct BalanceResponse {
    #[serde(rename = "balance_infos", default)]
    balance_infos: Vec<BalanceInfo>,
    /// Whether the request itself succeeded at the transport level.
    #[serde(default = "default_true")]
    is_available: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct BalanceInfo {
    currency: String,
    /// Decimal string like "12.3456" or "0.00".
    total_balance: String,
    #[serde(default)]
    granted_balance: Option<String>,
    #[serde(default)]
    topped_up_balance: Option<String>,
}

pub struct DeepSeekBalance {
    provider_id: String,
    /// Pre-formatted extension id (`type_id:provider_id`).
    extension_id: String,
    /// Base URL up to but not including `/v1/user/balance`.
    base_url: String,
}

impl DeepSeekBalance {
    pub fn new(provider_id: impl Into<String>, endpoint: Option<&str>) -> Self {
        let provider_id = provider_id.into();
        let extension_id = format!("deepseek_balance:{}", provider_id);
        // Strip trailing `/v1/chat/completions`, `/v1`, and any trailing
        // slashes so we can append `/v1/user/balance` cleanly.
        let base_url = endpoint
            .map(|e| {
                e.trim_end_matches('/')
                    .trim_end_matches("/v1/chat/completions")
                    .trim_end_matches("/v1")
                    .trim_end_matches('/')
                    .to_string()
            })
            .unwrap_or_else(|| {
                DEFAULT_BASE
                    .trim_end_matches("/v1")
                    .trim_end_matches('/')
                    .to_string()
            });
        Self {
            provider_id,
            extension_id,
            base_url,
        }
    }
}

impl ProviderExtension for DeepSeekBalance {
    fn manifest(&self) -> ExtensionManifest {
        ExtensionManifest {
            id: self.extension_id.clone(),
            type_id: "deepseek_balance".to_string(),
            provider_id: self.provider_id.clone(),
            display_name: format!("DeepSeek Balance ({})", self.provider_id),
            description: "Displays remaining DeepSeek account credit.".to_string(),
            capabilities: vec![Capability::Usage, Capability::Billing],
            requires_api_key: true,
            docs_url: Some("https://api-docs.deepseek.com/quick_start/pricing".to_string()),
        }
    }

    fn usage_provider(&self) -> Option<&dyn UsageProvider> {
        Some(self)
    }
}

#[async_trait]
impl UsageProvider for DeepSeekBalance {
    async fn fetch(&self, ctx: &ExtensionContext) -> Result<UsageSnapshot, ExtensionError> {
        // ── Guard: require API key ────────────────────────────────────
        let api_key = ctx.api_key(&self.provider_id).await.ok_or_else(|| {
            ExtensionError::Auth("No DeepSeek API key configured — set it in Settings".to_string())
        })?;
        if api_key.is_empty() {
            return Err(ExtensionError::Auth("API key is empty".to_string()));
        }

        // ── Build balance URL ─────────────────────────────────────────
        let url = format!("{}/v1/user/balance", self.base_url);

        // ── Fetch ─────────────────────────────────────────────────────
        let resp = ctx
            .http()
            .get(&url)
            .bearer_auth(&api_key)
            .send()
            .await
            .map_err(|e| ExtensionError::Network(format!("HTTP request failed: {}", e)))?;

        let status = resp.status();
        if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
            return Err(ExtensionError::Auth(format!(
                "DeepSeek returned {}",
                status.as_u16()
            )));
        }
        if !status.is_success() {
            return Err(ExtensionError::Network(format!(
                "DeepSeek API returned {}",
                status.as_u16()
            )));
        }

        let body_text = resp
            .text()
            .await
            .map_err(|e| ExtensionError::Network(e.to_string()))?;
        let body: BalanceResponse = serde_json::from_str(&body_text).map_err(|e| {
            ExtensionError::Parse(format!("Failed to parse balance response: {}", e))
        })?;

        // ── Extract balance ───────────────────────────────────────────
        let balance_info = body
            .balance_infos
            .first()
            .ok_or_else(|| ExtensionError::Internal("No balance info in response".to_string()))?;

        let balance: f64 = balance_info.total_balance.parse().map_err(|e| {
            ExtensionError::Parse(format!(
                "Non-numeric balance '{}': {}",
                balance_info.total_balance, e
            ))
        })?;

        // ── Determine tone ─────────────────────────────────────────────
        let plan_amount = ctx
            .extension_settings(&self.extension_id)
            .await
            .preferences
            .get("plan_amount")
            .and_then(|v| v.parse::<f64>().ok())
            .unwrap_or(10.0); // default threshold in $

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

        // ── Build snapshot ────────────────────────────────────────────
        let display = if balance_info.currency.eq_ignore_ascii_case("CNY") {
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

        let mut metrics: Vec<UsageMetric> = Vec::new();
        metrics.push(headline.clone());
        metrics.push(UsageMetric {
            key: "plan_amount".to_string(),
            label: "Plan Amount".to_string(),
            display: format!("${:.2}", plan_amount),
            value: plan_amount,
            kind: MetricKind::Currency,
            tone: Tone::Neutral,
        });

        // Optional supplementary metrics from the response.
        if let Some(g) = balance_info
            .granted_balance
            .as_deref()
            .and_then(|s| s.parse::<f64>().ok())
        {
            metrics.push(UsageMetric {
                key: "granted_balance".to_string(),
                label: "Granted".to_string(),
                display: format!("${:.2}", g),
                value: g,
                kind: MetricKind::Currency,
                tone: Tone::Neutral,
            });
        }
        if let Some(t) = balance_info
            .topped_up_balance
            .as_deref()
            .and_then(|s| s.parse::<f64>().ok())
        {
            metrics.push(UsageMetric {
                key: "topped_up_balance".to_string(),
                label: "Topped Up".to_string(),
                display: format!("${:.2}", t),
                value: t,
                kind: MetricKind::Currency,
                tone: Tone::Neutral,
            });
        }

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

    #[test]
    fn endpoint_derivation_default() {
        let ext = DeepSeekBalance::new("deepseek", None);
        assert_eq!(ext.base_url, "https://api.deepseek.com");
    }

    #[test]
    fn endpoint_derivation_strips_v1() {
        let ext = DeepSeekBalance::new("deepseek", Some("https://api.deepseek.com/v1"));
        assert_eq!(ext.base_url, "https://api.deepseek.com");
    }

    #[test]
    fn endpoint_derivation_strips_chat_completions() {
        let ext = DeepSeekBalance::new(
            "deepseek",
            Some("https://api.deepseek.com/v1/chat/completions"),
        );
        assert_eq!(ext.base_url, "https://api.deepseek.com");
    }

    #[test]
    fn endpoint_derivation_strips_trailing_slash() {
        let ext = DeepSeekBalance::new("deepseek", Some("https://api.deepseek.com/v1/"));
        assert_eq!(ext.base_url, "https://api.deepseek.com");
    }

    #[test]
    fn manifest_id_is_composite() {
        let ext = DeepSeekBalance::new("deepseek", None);
        assert_eq!(ext.manifest().id, "deepseek_balance:deepseek");
        assert_eq!(ext.manifest().type_id, "deepseek_balance");
        assert_eq!(ext.manifest().provider_id, "deepseek");
    }

    #[test]
    fn balance_response_parses() {
        let body = r#"{
            "balance_infos": [
                {"currency": "USD", "total_balance": "12.34"}
            ],
            "is_available": true
        }"#;
        let parsed: BalanceResponse = serde_json::from_str(body).unwrap();
        assert!(parsed.is_available);
        assert_eq!(parsed.balance_infos.len(), 1);
        assert_eq!(parsed.balance_infos[0].currency, "USD");
        assert_eq!(parsed.balance_infos[0].total_balance, "12.34");
    }
}
