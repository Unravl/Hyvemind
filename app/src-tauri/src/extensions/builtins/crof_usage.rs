//! CroF usage / daily-requests extension.
//!
//! Probes `GET {root}/usage_api/`, which returns:
//!   {
//!     "credits": 18.21,              // available credit balance
//!     "requests_plan": 2500,         // daily request cap (null if no plan)
//!     "usable_requests": 2352        // requests left today (null if no plan)
//!   }
//!
//! The endpoint is at the API root (e.g. `https://crof.ai/usage_api/`),
//! NOT under the `/v1` prefix used for chat completions.
//!
//! The headline metric is "N / M" (usable_requests / requests_plan) when
//! plan data is available; falls back to just the usable requests count;
//! then to "$X.XX" credits remaining. Tones:
//!   - `crit` if usable_requests < 50 or credits < $1.00
//!   - `warn` if usable_requests < 200 or credits < $5.00
//!   - `ok`   otherwise

use async_trait::async_trait;
use serde::Deserialize;

use crate::extensions::context::ExtensionContext;
use crate::extensions::traits::{ProviderExtension, UsageProvider};
use crate::extensions::types::{
    Capability, ExtensionError, ExtensionManifest, MetricKind, Tone, UsageMetric, UsageSnapshot,
};

/// Default CroF endpoint, used when the provider config has no
/// explicit endpoint set.
const DEFAULT_BASE: &str = "https://crof.ai/v1";

pub struct CrofUsage {
    provider_id: String,
    /// Pre-formatted extension id (`type_id:provider_id`).
    extension_id: String,
    /// Root of the CroF API. The usage endpoint lives at `{root}/usage_api/`,
    /// where root is derived by stripping the `/v1` version suffix if present.
    root_url: String,
}

impl CrofUsage {
    pub fn new(provider_id: impl Into<String>, endpoint: Option<&str>) -> Self {
        let provider_id = provider_id.into();
        let extension_id = format!("crof_usage:{}", provider_id);
        // The usage API is at the root level, not under /v1.
        // Strip any trailing /v1 or /v2 prefix from the configured endpoint.
        let base = endpoint
            .map(|e| e.trim_end_matches('/').to_string())
            .unwrap_or_else(|| DEFAULT_BASE.to_string());
        let root = base
            .trim_end_matches("/v1")
            .trim_end_matches("/v2")
            .to_string();
        Self {
            provider_id,
            extension_id,
            root_url: root,
        }
    }
}

impl ProviderExtension for CrofUsage {
    fn manifest(&self) -> ExtensionManifest {
        ExtensionManifest {
            id: self.extension_id.clone(),
            type_id: "crof_usage".to_string(),
            provider_id: self.provider_id.clone(),
            display_name: "Crof Usage".to_string(),
            description: "Daily requests remaining and credits for a CroF API key.".to_string(),
            capabilities: vec![Capability::Usage, Capability::Billing],
            requires_api_key: true,
            docs_url: Some("https://crof.ai/docs".to_string()),
        }
    }

    fn usage_provider(&self) -> Option<&dyn UsageProvider> {
        Some(self)
    }
}

/// Build the headline metric from a CroF usage response, pushing all
/// relevant metrics into `metrics`. Returns `None` when neither requests
/// nor credits data is present (caller should return Unsupported).
fn build_headline_from_response(
    parsed: &CrofUsageResponse,
    metrics: &mut Vec<UsageMetric>,
) -> Option<UsageMetric> {
    // Prefer usable requests as the headline — show "N / M" when plan is available.
    if let Some(remaining) = parsed.usable_requests {
        let tone = if remaining < 50.0 {
            Tone::Crit
        } else if remaining < 200.0 {
            Tone::Warn
        } else {
            Tone::Ok
        };
        let display = (remaining.floor() as i64).to_string();
        let m = UsageMetric {
            key: "usable_requests".to_string(),
            label: "Usable requests".to_string(),
            display: display.clone(),
            value: remaining,
            kind: MetricKind::Count,
            tone: tone.clone(),
        };
        metrics.push(m.clone());
        return Some(m);
    }

    // Fall back to credits.
    if let Some(credits) = parsed.credits {
        let tone = if credits < 1.0 {
            Tone::Crit
        } else if credits < 5.0 {
            Tone::Warn
        } else {
            Tone::Ok
        };
        let display = format!("${:.2}", credits);
        let m = UsageMetric {
            key: "credits".to_string(),
            label: "Credits remaining".to_string(),
            display: display.clone(),
            value: credits,
            kind: MetricKind::Currency,
            tone: tone.clone(),
        };
        metrics.push(m.clone());
        return Some(m);
    }

    None
}

#[derive(Debug, Deserialize)]
struct CrofUsageResponse {
    /// How many requests the key may still send today.
    /// Null if not on a subscription plan. CroF returns this as a fractional
    /// number (e.g. `781.25`) when partial-request weighting is in effect.
    #[serde(default)]
    usable_requests: Option<f64>,
    /// Daily request cap. Null if not on a subscription plan. Accepted as
    /// float for the same reason as `usable_requests`.
    #[serde(default)]
    requests_plan: Option<f64>,
    /// Available credit balance in USD.
    #[serde(default)]
    credits: Option<f64>,
}

#[async_trait]
impl UsageProvider for CrofUsage {
    async fn fetch(&self, ctx: &ExtensionContext) -> Result<UsageSnapshot, ExtensionError> {
        let api_key = ctx
            .api_key(&self.provider_id)
            .await
            .ok_or_else(|| ExtensionError::Auth("no API key configured".to_string()))?;
        if api_key.is_empty() {
            return Err(ExtensionError::Auth("API key is empty".to_string()));
        }

        let url = format!("{}/usage_api/", self.root_url);
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
                "CroF returned {}",
                status.as_u16()
            )));
        }
        if !status.is_success() {
            return Err(ExtensionError::Network(format!(
                "CroF returned HTTP {}",
                status.as_u16()
            )));
        }

        let body = response
            .text()
            .await
            .map_err(|e| ExtensionError::Network(e.to_string()))?;
        let parsed: CrofUsageResponse =
            serde_json::from_str(&body).map_err(|e| ExtensionError::Parse(e.to_string()))?;
        let raw: serde_json::Value =
            serde_json::from_str(&body).map_err(|e| ExtensionError::Parse(e.to_string()))?;

        let mut metrics: Vec<UsageMetric> = Vec::new();

        // Determine headline: prefer usable requests, fall back to credits.
        let headline: Option<UsageMetric> = build_headline_from_response(&parsed, &mut metrics);
        if headline.is_none() {
            return Err(ExtensionError::Unsupported(
                "usage endpoint returned neither usable_requests nor credits".to_string(),
            ));
        }

        // Add secondary metrics.
        if let Some(remaining) = parsed.usable_requests {
            if !metrics.iter().any(|m| m.key == "usable_requests") {
                metrics.push(UsageMetric {
                    key: "usable_requests".to_string(),
                    label: "Usable requests".to_string(),
                    display: (remaining.floor() as i64).to_string(),
                    value: remaining,
                    kind: MetricKind::Count,
                    tone: if remaining < 50.0 {
                        Tone::Crit
                    } else if remaining < 200.0 {
                        Tone::Warn
                    } else {
                        Tone::Ok
                    },
                });
            }
        }
        if let Some(plan) = parsed.requests_plan {
            if !metrics.iter().any(|m| m.key == "requests_plan") {
                metrics.push(UsageMetric {
                    key: "requests_plan".to_string(),
                    label: "Daily request cap".to_string(),
                    display: (plan.floor() as i64).to_string(),
                    value: plan,
                    kind: MetricKind::Count,
                    tone: Tone::Neutral,
                });
            }
        }
        if let Some(credits) = parsed.credits {
            if !metrics.iter().any(|m| m.key == "credits") {
                metrics.push(UsageMetric {
                    key: "credits".to_string(),
                    label: "Credits remaining".to_string(),
                    display: format!("${:.2}", credits),
                    value: credits,
                    kind: MetricKind::Currency,
                    tone: if credits < 1.0 {
                        Tone::Crit
                    } else if credits < 5.0 {
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
            headline,
            metrics,
            raw: Some(raw),
        })
    }

    fn refresh_interval_secs(&self) -> u64 {
        120
    }
}
