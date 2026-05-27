//! Claude subscription usage (Pro / Max) — 5-hour and 7-day window
//! utilization for the `claude-sub` provider.
//!
//! Data source: `GET https://api.anthropic.com/api/oauth/usage`
//! (undocumented; same endpoint Claude Code's `/usage` command uses).
//! Requires the OAuth access token Pi stores in
//! `~/.pi/agent/auth.json` under the `anthropic` key. This is read-only:
//! we never refresh the token here — Pi owns auth.json writes and would
//! race us.
//!
//! NOTE on the documented Usage & Cost Admin API
//! (`/v1/organizations/usage_report/messages`,
//! `/v1/organizations/cost_report`): it requires an `sk-ant-admin…`
//! Admin API key and is org-only. Pro/Max subscribers can't use it, so
//! it would be a permanent `Unsupported` for `claude-sub`. That path is
//! already covered by `anthropic_usage.rs` for the API-key provider.
//!
//! Follow-ups (out of scope for this slice):
//!   - Admin-key Usage & Cost API support (different audience).
//!   - Proactive OAuth refresh against `console.anthropic.com/v1/oauth/token`
//!     (Pi already owns `auth.json` writes — racing them would be a bug).
//!   - Dollar-cost dollars (not exposed for sub plans).

use std::path::PathBuf;

use async_trait::async_trait;
use serde::Deserialize;

use crate::extensions::context::ExtensionContext;
use crate::extensions::traits::{ProviderExtension, UsageProvider};
use crate::extensions::types::{
    Capability, ExtensionError, ExtensionManifest, MetricKind, Tone, UsageMetric, UsageSnapshot,
};

const USAGE_URL: &str = "https://api.anthropic.com/api/oauth/usage";
/// Required `User-Agent`. The endpoint applies an aggressive per-token
/// rate-limit bucket to anything that doesn't impersonate Claude Code;
/// community tools (`claudeline`, `usage-monitor-for-claude`, …) all
/// rely on this. Bump as needed.
const CLAUDE_CODE_UA: &str = "claude-code/1.0.30";
const OAUTH_BETA: &str = "oauth-2025-04-20";

pub struct ClaudeSubUsage {
    provider_id: String,
    extension_id: String,
}

impl ClaudeSubUsage {
    pub fn new(provider_id: impl Into<String>) -> Self {
        let provider_id = provider_id.into();
        let extension_id = format!("claude_sub_usage:{}", provider_id);
        Self {
            provider_id,
            extension_id,
        }
    }
}

/// Tone ramp shared by all Claude utilization metrics.
/// Public to the crate so unit tests can target it directly.
pub(crate) fn tone_for(u: f64) -> Tone {
    if u >= 90.0 {
        Tone::Crit
    } else if u >= 60.0 {
        Tone::Warn
    } else {
        Tone::Ok
    }
}

#[derive(Debug, Deserialize)]
struct Window {
    utilization: f64,
    #[serde(default)]
    resets_at: Option<String>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct ExtraUsage {
    #[serde(default)]
    is_enabled: bool,
    #[serde(default)]
    utilization: Option<f64>,
    #[serde(default)]
    monthly_limit: Option<f64>,
    #[serde(default)]
    used_credits: Option<f64>,
}

#[derive(Debug, Deserialize)]
struct OauthUsage {
    #[serde(default)]
    five_hour: Option<Window>,
    #[serde(default)]
    seven_day: Option<Window>,
    #[serde(default)]
    seven_day_opus: Option<Window>,
    #[serde(default)]
    seven_day_sonnet: Option<Window>,
    #[serde(default)]
    extra_usage: Option<ExtraUsage>,
}

/// Read `~/.pi/agent/auth.json` and extract `anthropic.access`.
/// Synchronous; callers must wrap in `spawn_blocking`.
fn read_anthropic_oauth_token() -> Result<String, ExtensionError> {
    let path = dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".pi")
        .join("agent")
        .join("auth.json");
    if !path.exists() {
        return Err(ExtensionError::Auth(
            "Claude subscription not logged in (no ~/.pi/agent/auth.json)".into(),
        ));
    }
    let data = std::fs::read_to_string(&path)
        .map_err(|e| ExtensionError::Internal(format!("read auth.json: {e}")))?;
    let v: serde_json::Value = serde_json::from_str(&data)
        .map_err(|e| ExtensionError::Parse(format!("auth.json: {e}")))?;
    v.get("anthropic")
        .and_then(|obj| obj.get("access"))
        .and_then(|s| s.as_str())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .ok_or_else(|| ExtensionError::Auth("no anthropic.access token in auth.json".into()))
}

/// Parse an ISO-8601 timestamp into a unix-seconds f64 for the
/// `Duration` metric value. Returns 0.0 on parse failure (the display
/// string still carries the original ISO value).
fn iso_to_epoch_secs(s: &str) -> f64 {
    chrono::DateTime::parse_from_rfc3339(s)
        .map(|dt| dt.timestamp() as f64)
        .unwrap_or(0.0)
}

impl ProviderExtension for ClaudeSubUsage {
    fn manifest(&self) -> ExtensionManifest {
        ExtensionManifest {
            id: self.extension_id.clone(),
            type_id: "claude_sub_usage".to_string(),
            provider_id: self.provider_id.clone(),
            display_name: "Claude Subscription Usage".to_string(),
            description: "5-hour and weekly quota utilization for your Claude Pro / Max \
                 subscription. Reads the OAuth token Pi maintains in \
                 ~/.pi/agent/auth.json; does not require an API key."
                .to_string(),
            capabilities: vec![Capability::Usage, Capability::RateLimitProbe],
            requires_api_key: false,
            docs_url: Some(
                "https://support.anthropic.com/en/articles/11145838-using-claude-code-with-your-pro-or-max-plan"
                    .to_string(),
            ),
        }
    }

    fn usage_provider(&self) -> Option<&dyn UsageProvider> {
        Some(self)
    }
}

#[async_trait]
impl UsageProvider for ClaudeSubUsage {
    async fn fetch(&self, ctx: &ExtensionContext) -> Result<UsageSnapshot, ExtensionError> {
        // ── 1. Read the OAuth token off disk ─────────────────────────
        // Disk I/O is sync; offload so we never hold the runtime worker.
        let token = tokio::task::spawn_blocking(read_anthropic_oauth_token)
            .await
            .map_err(|e| ExtensionError::Internal(format!("spawn_blocking join: {e}")))??;

        // ── 2. Hit the OAuth usage endpoint ──────────────────────────
        let response = ctx
            .http()
            .get(USAGE_URL)
            .bearer_auth(&token)
            .header("anthropic-beta", OAUTH_BETA)
            .header("User-Agent", CLAUDE_CODE_UA)
            .send()
            .await
            .map_err(|e| ExtensionError::Network(e.to_string()))?;

        let status = response.status();
        if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
            return Err(ExtensionError::Auth(format!(
                "OAuth token rejected ({}). Re-login via Pi.",
                status.as_u16()
            )));
        }
        if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
            return Err(ExtensionError::Network(
                "rate limited — backing off".to_string(),
            ));
        }
        if !status.is_success() {
            return Err(ExtensionError::Network(format!("HTTP {}", status.as_u16())));
        }

        let body_text = response
            .text()
            .await
            .map_err(|e| ExtensionError::Network(e.to_string()))?;
        let parsed: OauthUsage =
            serde_json::from_str(&body_text).map_err(|e| ExtensionError::Parse(e.to_string()))?;
        let raw: serde_json::Value =
            serde_json::from_str(&body_text).map_err(|e| ExtensionError::Parse(e.to_string()))?;

        // ── 3. Build headline & metrics ──────────────────────────────
        let mut metrics: Vec<UsageMetric> = Vec::new();

        // Headline = five-hour utilization.
        let headline = match parsed.five_hour.as_ref() {
            Some(w) if w.resets_at.is_some() || w.utilization > 0.0 => {
                let u = w.utilization;
                let tone = tone_for(u);
                UsageMetric {
                    key: "five_hour_utilization".to_string(),
                    label: "5h utilization".to_string(),
                    display: format!("{:.0}%", u),
                    value: u,
                    kind: MetricKind::Percentage,
                    tone,
                }
            }
            _ => UsageMetric {
                key: "five_hour_utilization".to_string(),
                label: "5h utilization".to_string(),
                display: "Idle".to_string(),
                value: 0.0,
                kind: MetricKind::Percentage,
                tone: Tone::Neutral,
            },
        };
        metrics.push(headline.clone());

        if let Some(w) = parsed.five_hour.as_ref() {
            if let Some(reset) = w.resets_at.as_deref() {
                metrics.push(UsageMetric {
                    key: "five_hour_resets_at".to_string(),
                    label: "5h resets at".to_string(),
                    display: reset.to_string(),
                    value: iso_to_epoch_secs(reset),
                    kind: MetricKind::Duration,
                    tone: Tone::Neutral,
                });
            }
        }

        if let Some(w) = parsed.seven_day.as_ref() {
            metrics.push(UsageMetric {
                key: "seven_day_utilization".to_string(),
                label: "7d utilization".to_string(),
                display: format!("{:.0}%", w.utilization),
                value: w.utilization,
                kind: MetricKind::Percentage,
                tone: tone_for(w.utilization),
            });
            if let Some(reset) = w.resets_at.as_deref() {
                metrics.push(UsageMetric {
                    key: "seven_day_resets_at".to_string(),
                    label: "7d resets at".to_string(),
                    display: reset.to_string(),
                    value: iso_to_epoch_secs(reset),
                    kind: MetricKind::Duration,
                    tone: Tone::Neutral,
                });
            }
        }

        if let Some(w) = parsed.seven_day_opus.as_ref() {
            metrics.push(UsageMetric {
                key: "seven_day_opus_utilization".to_string(),
                label: "Opus (7d)".to_string(),
                display: format!("{:.0}%", w.utilization),
                value: w.utilization,
                kind: MetricKind::Percentage,
                tone: tone_for(w.utilization),
            });
        }
        if let Some(w) = parsed.seven_day_sonnet.as_ref() {
            metrics.push(UsageMetric {
                key: "seven_day_sonnet_utilization".to_string(),
                label: "Sonnet (7d)".to_string(),
                display: format!("{:.0}%", w.utilization),
                value: w.utilization,
                kind: MetricKind::Percentage,
                tone: tone_for(w.utilization),
            });
        }

        if let Some(extra) = parsed.extra_usage.as_ref() {
            if extra.is_enabled {
                metrics.push(UsageMetric {
                    key: "extra_usage_is_enabled".to_string(),
                    label: "Extra usage".to_string(),
                    display: "Enabled".to_string(),
                    value: 1.0,
                    kind: MetricKind::Count,
                    tone: Tone::Neutral,
                });
                if let Some(u) = extra.utilization {
                    metrics.push(UsageMetric {
                        key: "extra_usage_utilization".to_string(),
                        label: "Extra usage".to_string(),
                        display: format!("{:.0}%", u),
                        value: u,
                        kind: MetricKind::Percentage,
                        tone: tone_for(u),
                    });
                }
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
        // 5 minutes — the endpoint hard-rate-limits per access token;
        // community tools land at ~180 s as the safe floor and we leave
        // a margin. The global `extension_poll_interval_secs` clamp can
        // stretch it further but never below
        // `poller::MIN_REFRESH_INTERVAL_SECS = 30`.
        300
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tone_ramp_matches_thresholds() {
        assert_eq!(tone_for(0.0), Tone::Ok);
        assert_eq!(tone_for(59.999), Tone::Ok);
        assert_eq!(tone_for(60.0), Tone::Warn);
        assert_eq!(tone_for(89.999), Tone::Warn);
        assert_eq!(tone_for(90.0), Tone::Crit);
        assert_eq!(tone_for(100.0), Tone::Crit);
    }

    #[test]
    fn parses_canonical_response() {
        let body = r#"{
            "five_hour":        { "utilization": 33.0, "resets_at": "2026-04-11T07:00:00Z" },
            "seven_day":        { "utilization": 13.0, "resets_at": "2026-04-17T00:59:59Z" },
            "seven_day_opus":   null,
            "seven_day_sonnet": { "utilization":  1.0, "resets_at": "2026-04-16T03:00:00Z" },
            "extra_usage":      { "is_enabled": false, "monthly_limit": null, "used_credits": null, "utilization": null }
        }"#;
        let parsed: OauthUsage = serde_json::from_str(body).unwrap();
        assert!(parsed.five_hour.is_some());
        assert_eq!(parsed.five_hour.as_ref().unwrap().utilization, 33.0);
        assert!(parsed.seven_day_opus.is_none());
        assert_eq!(parsed.seven_day_sonnet.as_ref().unwrap().utilization, 1.0);
        assert!(!parsed.extra_usage.as_ref().unwrap().is_enabled);
    }

    #[test]
    fn manifest_id_is_composite() {
        let ext = ClaudeSubUsage::new("claude-sub");
        assert_eq!(ext.manifest().id, "claude_sub_usage:claude-sub");
        assert_eq!(ext.manifest().type_id, "claude_sub_usage");
        assert_eq!(ext.manifest().provider_id, "claude-sub");
        assert!(!ext.manifest().requires_api_key);
    }

    #[test]
    fn iso_to_epoch_secs_handles_rfc3339() {
        let secs = iso_to_epoch_secs("2026-04-11T07:00:00Z");
        assert!(secs > 1_700_000_000.0);
        assert_eq!(iso_to_epoch_secs("garbage"), 0.0);
    }
}
