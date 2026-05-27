//! OpenRouter credits / key-info extension.
//!
//! Probes two endpoints with the same API key:
//!   1. `GET /api/v1/credits` — returns total credits balance.
//!   2. `GET /api/v1/auth/key` — returns per-key limits/usage.
//!
//! The headline metric is "$X.XX remaining" computed from:
//!   - `/credits` `total_credits - total_usage` (preferred, account-wide), or
//!   - `/auth/key` `limit_remaining` (or `limit - usage` when `limit_remaining` is absent).
//!
//! If neither endpoint produces useful data, a neutral "—" headline is shown so the
//! extension badge remains visible.
//!
//! Tones:
//!   - `crit` if remaining < $0.10
//!   - `warn` if remaining < $1.00
//!   - `ok`   otherwise

use async_trait::async_trait;
use serde::Deserialize;

use crate::extensions::context::ExtensionContext;
use crate::extensions::traits::{ProviderExtension, UsageProvider};
use crate::extensions::types::{
    Capability, ExtensionError, ExtensionManifest, MetricKind, Tone, UsageMetric, UsageSnapshot,
};

/// Default OpenRouter endpoint, used when the provider config has no
/// explicit endpoint set.
const DEFAULT_BASE: &str = "https://openrouter.ai/api/v1";

pub struct OpenRouterCredits {
    provider_id: String,
    /// Pre-formatted extension id (`type_id:provider_id`).
    extension_id: String,
    /// Base URL up to but not including `/auth/key`. Built from the
    /// provider's configured endpoint or `DEFAULT_BASE`.
    base_url: String,
}

impl OpenRouterCredits {
    pub fn new(provider_id: impl Into<String>, endpoint: Option<&str>) -> Self {
        let provider_id = provider_id.into();
        let extension_id = format!("openrouter_credits:{}", provider_id);
        let base_url = endpoint
            .map(|e| e.trim_end_matches('/').to_string())
            .unwrap_or_else(|| DEFAULT_BASE.to_string());
        Self {
            provider_id,
            extension_id,
            base_url,
        }
    }
}

impl ProviderExtension for OpenRouterCredits {
    fn manifest(&self) -> ExtensionManifest {
        ExtensionManifest {
            id: self.extension_id.clone(),
            type_id: "openrouter_credits".to_string(),
            provider_id: self.provider_id.clone(),
            display_name: format!("OpenRouter Credits ({})", self.provider_id),
            description: "Remaining credits and usage for an OpenRouter API key.".to_string(),
            capabilities: vec![Capability::Usage, Capability::Billing],
            requires_api_key: true,
            docs_url: Some(
                "https://openrouter.ai/docs/api/api-reference/credits/get-credits".to_string(),
            ),
        }
    }

    fn usage_provider(&self) -> Option<&dyn UsageProvider> {
        Some(self)
    }
}

#[derive(Debug, Deserialize)]
struct AuthKeyResponse {
    data: AuthKeyData,
}

#[derive(Debug, Deserialize)]
struct AuthKeyData {
    /// Total credit limit (USD). May be `None` for unlimited keys.
    limit: Option<f64>,
    /// Total usage so far (USD).
    usage: Option<f64>,
    /// Remaining credits (USD). Preferred when present.
    #[serde(default)]
    limit_remaining: Option<f64>,
    /// True when the key has no upper limit.
    #[serde(default)]
    is_free_tier: Option<bool>,
}

/// Response from `GET /api/v1/credits` (requires management key).
#[derive(Debug, Deserialize)]
struct CreditsResponse {
    data: CreditsData,
}

#[derive(Debug, Deserialize)]
struct CreditsData {
    total_credits: f64,
    total_usage: f64,
}

#[async_trait]
impl UsageProvider for OpenRouterCredits {
    async fn fetch(&self, ctx: &ExtensionContext) -> Result<UsageSnapshot, ExtensionError> {
        let api_key = ctx
            .api_key(&self.provider_id)
            .await
            .ok_or_else(|| ExtensionError::Auth("no API key configured".to_string()))?;
        if api_key.is_empty() {
            return Err(ExtensionError::Auth("API key is empty".to_string()));
        }

        // Phase 1: Try /credits for account-level balance (uses the same API key).
        // Errors are silently ignored — if /credits is unavailable we fall through
        // to /auth/key which always works with a valid OpenRouter key.
        let mut credits_remaining: Option<f64> = None;
        let mut credits_total_usage: Option<f64> = None;
        let mut credits_total: Option<f64> = None;

        let credits_url = format!("{}/credits", self.base_url);
        match ctx
            .http()
            .get(&credits_url)
            .bearer_auth(&api_key)
            .send()
            .await
        {
            Ok(resp) => {
                let status = resp.status();
                if status.is_success() {
                    if let Ok(body) = resp.text().await {
                        if let Ok(parsed) = serde_json::from_str::<CreditsResponse>(&body) {
                            let remaining =
                                (parsed.data.total_credits - parsed.data.total_usage).max(0.0);
                            credits_remaining = Some(remaining);
                            credits_total_usage = Some(parsed.data.total_usage);
                            credits_total = Some(parsed.data.total_credits);
                        }
                    }
                }
                // Non-success, 401/403, and parse errors silently ignored → fall through.
            }
            Err(_) => {
                // Network errors silently ignored → fall through.
            }
        }

        // Phase 2: Call /auth/key (regular key, existing behavior).
        let url = format!("{}/auth/key", self.base_url);
        let response = ctx
            .http()
            .get(&url)
            .bearer_auth(&api_key)
            .send()
            .await
            .map_err(|e| ExtensionError::Network(e.to_string()))?;

        let status = response.status();
        if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
            return Err(ExtensionError::Auth(format!(
                "OpenRouter returned {}",
                status.as_u16()
            )));
        }
        if !status.is_success() {
            return Err(ExtensionError::Network(format!(
                "OpenRouter returned HTTP {}",
                status.as_u16()
            )));
        }

        let body = response
            .text()
            .await
            .map_err(|e| ExtensionError::Network(e.to_string()))?;
        let parsed: AuthKeyResponse =
            serde_json::from_str(&body).map_err(|e| ExtensionError::Parse(e.to_string()))?;
        let raw: serde_json::Value =
            serde_json::from_str(&body).map_err(|e| ExtensionError::Parse(e.to_string()))?;

        // Compute remaining from /auth/key: prefer `limit_remaining`; otherwise
        // `limit - usage` when both are present; otherwise None.
        let auth_remaining = match parsed.data.limit_remaining {
            Some(r) => Some(r),
            None => match (parsed.data.limit, parsed.data.usage) {
                (Some(l), Some(u)) => Some((l - u).max(0.0)),
                _ => None,
            },
        };

        let mut metrics: Vec<UsageMetric> = Vec::new();

        // Prefer /credits remaining (total account balance) over /auth/key.
        let headline: Option<UsageMetric> = if let Some(r) = credits_remaining {
            let tone = if r < 0.10 {
                Tone::Crit
            } else if r < 1.0 {
                Tone::Warn
            } else {
                Tone::Ok
            };
            let m = UsageMetric {
                key: "remaining".to_string(),
                label: "Remaining".to_string(),
                display: format!("${:.2}", r),
                value: r,
                kind: MetricKind::Currency,
                tone,
            };
            metrics.push(m.clone());
            Some(m)
        } else if let Some(r) = auth_remaining {
            // Fall back to /auth/key remaining (per-key limit).
            let tone = if r < 0.10 {
                Tone::Crit
            } else if r < 1.0 {
                Tone::Warn
            } else {
                Tone::Ok
            };
            let m = UsageMetric {
                key: "remaining".to_string(),
                label: "Remaining".to_string(),
                display: format!("${:.2}", r),
                value: r,
                kind: MetricKind::Currency,
                tone,
            };
            metrics.push(m.clone());
            Some(m)
        } else if parsed.data.is_free_tier.unwrap_or(false) {
            let m = UsageMetric {
                key: "tier".to_string(),
                label: "Tier".to_string(),
                display: "Free".to_string(),
                value: 0.0,
                kind: MetricKind::Count,
                tone: Tone::Neutral,
            };
            metrics.push(m.clone());
            Some(m)
        } else {
            // Neutral fallback: show the badge is active but no data available.
            let m = UsageMetric {
                key: "unavailable".to_string(),
                label: "Credits unavailable".to_string(),
                display: "\u{2014}".to_string(),
                value: 0.0,
                kind: MetricKind::Count,
                tone: Tone::Neutral,
            };
            metrics.push(m.clone());
            Some(m)
        };

        // Add /credits account-level metrics if available.
        if let Some(tu) = credits_total_usage {
            metrics.push(UsageMetric {
                key: "total_usage".to_string(),
                label: "Account spent".to_string(),
                display: format!("${:.2}", tu),
                value: tu,
                kind: MetricKind::Currency,
                tone: Tone::Neutral,
            });
        }
        if let Some(tc) = credits_total {
            metrics.push(UsageMetric {
                key: "total_credits".to_string(),
                label: "Account credits".to_string(),
                display: format!("${:.2}", tc),
                value: tc,
                kind: MetricKind::Currency,
                tone: Tone::Neutral,
            });
        }

        // Add /auth/key per-key metrics.
        if let Some(usage) = parsed.data.usage {
            metrics.push(UsageMetric {
                key: "usage".to_string(),
                label: "Total spent".to_string(),
                display: format!("${:.2}", usage),
                value: usage,
                kind: MetricKind::Currency,
                tone: Tone::Neutral,
            });
        }
        if let Some(limit) = parsed.data.limit {
            metrics.push(UsageMetric {
                key: "limit".to_string(),
                label: "Limit".to_string(),
                display: format!("${:.2}", limit),
                value: limit,
                kind: MetricKind::Currency,
                tone: Tone::Neutral,
            });
        }

        Ok(UsageSnapshot {
            extension_id: self.extension_id.clone(),
            provider_id: self.provider_id.clone(),
            fetched_at: chrono::Utc::now().timestamp(),
            headline,
            metrics,
            raw: Some(raw),
        })
    }

    fn refresh_interval_secs(&self) -> u64 {
        300
    }
}
