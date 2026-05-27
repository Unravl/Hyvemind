//! NeuralWatt usage / quota extension.
//!
//! Probes `GET {endpoint}/quota`, which returns account balance, usage,
//! subscription limits, and per-key allowance from NeuralWatt's quota
//! API. This is preferred over the `/usage/energy` endpoint because it
//! provides both the current usage and the plan limit in a single call,
//! enabling a percentage-of-limit bar display.
//!
//! Headline metric: kWh usage as a percentage of the plan's kWh
//! allowance (`kwh_used / kwh_included`). Tones:
//!   - `crit` if percentage >= 90%
//!   - `warn` if percentage >= 70%
//!   - `ok`   otherwise
//!
//! Falls back to the raw `energy_kwh` from `usage.current_month` when
//! no subscription data is available (pay-as-you-go accounts).

use async_trait::async_trait;
use serde::Deserialize;

use crate::extensions::context::ExtensionContext;
use crate::extensions::traits::{ProviderExtension, UsageProvider};
use crate::extensions::types::{
    Capability, ExtensionError, ExtensionManifest, MetricKind, Tone, UsageMetric, UsageSnapshot,
};

/// Default NeuralWatt endpoint. The quota API lives under the same
/// `/v1` prefix as the chat completions endpoint.
const DEFAULT_ENDPOINT: &str = "https://api.neuralwatt.com/v1";

pub struct NeuralWattUsage {
    provider_id: String,
    extension_id: String,
    endpoint: String,
}

impl NeuralWattUsage {
    pub fn new(provider_id: impl Into<String>, endpoint: Option<&str>) -> Self {
        let provider_id = provider_id.into();
        let extension_id = format!("neuralwatt_usage:{}", provider_id);
        let endpoint = endpoint
            .map(|e| e.trim_end_matches('/').to_string())
            .unwrap_or_else(|| DEFAULT_ENDPOINT.to_string());
        Self {
            provider_id,
            extension_id,
            endpoint,
        }
    }
}

impl ProviderExtension for NeuralWattUsage {
    fn manifest(&self) -> ExtensionManifest {
        ExtensionManifest {
            id: self.extension_id.clone(),
            type_id: "neuralwatt_usage".to_string(),
            provider_id: self.provider_id.clone(),
            display_name: "NeuralWatt Usage".to_string(),
            description: "Energy consumption and quota for a NeuralWatt API key.".to_string(),
            capabilities: vec![Capability::Usage],
            requires_api_key: true,
            docs_url: Some("https://portal.neuralwatt.com/docs/api/quota".to_string()),
        }
    }

    fn usage_provider(&self) -> Option<&dyn UsageProvider> {
        Some(self)
    }
}

// ── API response types ─────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct QuotaResponse {
    #[serde(default)]
    balance: Option<BalanceBlock>,
    #[serde(default)]
    usage: Option<UsageBlock>,
    #[serde(default)]
    subscription: Option<SubscriptionBlock>,
}

#[derive(Debug, Deserialize)]
struct BalanceBlock {
    #[serde(default)]
    credits_remaining_usd: Option<f64>,
}

#[derive(Debug, Deserialize)]
struct UsageBlock {
    #[serde(default)]
    current_month: Option<UsagePeriod>,
}

#[derive(Debug, Deserialize)]
struct UsagePeriod {
    #[serde(default)]
    requests: u64,
    #[serde(default)]
    energy_kwh: f64,
}

#[derive(Debug, Deserialize)]
struct SubscriptionBlock {
    #[serde(default)]
    kwh_included: Option<f64>,
    #[serde(default)]
    kwh_used: Option<f64>,
    #[serde(default)]
    kwh_remaining: Option<f64>,
    #[serde(default)]
    in_overage: Option<bool>,
}

// ── UsageProvider implementation ──────────────────────────────

#[async_trait]
impl UsageProvider for NeuralWattUsage {
    async fn fetch(&self, ctx: &ExtensionContext) -> Result<UsageSnapshot, ExtensionError> {
        let api_key = ctx
            .api_key(&self.provider_id)
            .await
            .ok_or_else(|| ExtensionError::Auth("no API key configured".to_string()))?;
        if api_key.is_empty() {
            return Err(ExtensionError::Auth("API key is empty".to_string()));
        }

        let url = format!("{}/quota", self.endpoint);
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
                "NeuralWatt returned {}",
                status.as_u16()
            )));
        }
        if !status.is_success() {
            return Err(ExtensionError::Network(format!(
                "NeuralWatt returned HTTP {}",
                status.as_u16()
            )));
        }

        let body = response
            .text()
            .await
            .map_err(|e| ExtensionError::Network(e.to_string()))?;
        let parsed: QuotaResponse =
            serde_json::from_str(&body).map_err(|e| ExtensionError::Parse(e.to_string()))?;
        let raw: serde_json::Value =
            serde_json::from_str(&body).map_err(|e| ExtensionError::Parse(e.to_string()))?;

        let mut metrics: Vec<UsageMetric> = Vec::new();

        // ── Headline: percentage of plan kWh used ────────────────
        // Prefer subscription-level kWh data; fall back to current_month.energy_kwh.
        let headline = match &parsed.subscription {
            Some(sub) if sub.kwh_included.is_some() && sub.kwh_included.unwrap() > 0.0 => {
                let used = sub.kwh_used.unwrap_or(0.0);
                let max = sub.kwh_included.unwrap();
                let pct = (used / max * 100.0).clamp(0.0, 100.0);
                let tone = if pct >= 90.0 {
                    Tone::Crit
                } else if pct >= 70.0 {
                    Tone::Warn
                } else {
                    Tone::Ok
                };
                UsageMetric {
                    key: "kwh_pct".to_string(),
                    label: "Energy used".to_string(),
                    display: format!("{:.1}%", pct),
                    value: pct,
                    kind: MetricKind::Percentage,
                    tone,
                }
            }
            _ => {
                // Fallback: just show the raw monthly energy_kwh as a float.
                let kwh = parsed
                    .usage
                    .as_ref()
                    .and_then(|u| u.current_month.as_ref())
                    .map(|p| p.energy_kwh)
                    .unwrap_or(0.0);
                let tone = if kwh > 1.0 {
                    Tone::Crit
                } else if kwh > 0.5 {
                    Tone::Warn
                } else {
                    Tone::Ok
                };
                UsageMetric {
                    key: "energy_kwh".to_string(),
                    label: "Energy".to_string(),
                    display: format!("{:.4} kWh", kwh),
                    value: kwh,
                    kind: MetricKind::Percentage,
                    tone,
                }
            }
        };

        metrics.push(headline.clone());

        // ── Subscription / quota secondary metrics ────────────────
        if let Some(sub) = &parsed.subscription {
            if let Some(used) = sub.kwh_used {
                metrics.push(UsageMetric {
                    key: "kwh_used".to_string(),
                    label: "kWh used".to_string(),
                    display: format!("{:.4} kWh", used),
                    value: used,
                    kind: MetricKind::Count,
                    tone: Tone::Neutral,
                });
            }
            if let Some(incl) = sub.kwh_included {
                metrics.push(UsageMetric {
                    key: "kwh_included".to_string(),
                    label: "kWh included".to_string(),
                    display: format!("{:.4} kWh", incl),
                    value: incl,
                    kind: MetricKind::Count,
                    tone: Tone::Neutral,
                });
            }
            if let Some(rem) = sub.kwh_remaining {
                metrics.push(UsageMetric {
                    key: "kwh_remaining".to_string(),
                    label: "kWh remaining".to_string(),
                    display: format!("{:.4} kWh", rem),
                    value: rem,
                    kind: MetricKind::Count,
                    tone: Tone::Neutral,
                });
            }
            if let Some(ov) = sub.in_overage {
                metrics.push(UsageMetric {
                    key: "in_overage".to_string(),
                    label: "Overage".to_string(),
                    display: if ov { "Yes" } else { "No" }.to_string(),
                    value: if ov { 1.0 } else { 0.0 },
                    kind: MetricKind::Count,
                    tone: if ov { Tone::Warn } else { Tone::Ok },
                });
            }
        }

        // ── Current month usage ─────────────────────────────────────
        if let Some(period) = parsed.usage.as_ref().and_then(|u| u.current_month.as_ref()) {
            metrics.push(UsageMetric {
                key: "month_energy_kwh".to_string(),
                label: "Month energy".to_string(),
                display: format!("{:.4} kWh", period.energy_kwh),
                value: period.energy_kwh,
                kind: MetricKind::Count,
                tone: Tone::Neutral,
            });
            metrics.push(UsageMetric {
                key: "month_requests".to_string(),
                label: "Month requests".to_string(),
                display: period.requests.to_string(),
                value: period.requests as f64,
                kind: MetricKind::Count,
                tone: Tone::Neutral,
            });
        }

        // ── Balance ─────────────────────────────────────────────────
        if let Some(bal) = &parsed.balance {
            if let Some(cr) = bal.credits_remaining_usd {
                metrics.push(UsageMetric {
                    key: "credits_remaining_usd".to_string(),
                    label: "Credits remaining".to_string(),
                    display: format!("${:.2}", cr),
                    value: cr,
                    kind: MetricKind::Currency,
                    tone: if cr < 1.0 {
                        Tone::Crit
                    } else if cr < 5.0 {
                        Tone::Warn
                    } else {
                        Tone::Ok
                    },
                });
            }
        }

        Ok(UsageSnapshot {
            extension_id: self.extension_id.clone(),
            provider_id: self.provider_id.clone(),
            fetched_at: chrono::Utc::now().timestamp(),
            headline: Some(headline),
            metrics,
            raw: Some(raw),
        })
    }

    fn refresh_interval_secs(&self) -> u64 {
        300
    }
}
