//! ChatGPT (Codex) subscription usage — 5-hour (primary) and weekly
//! (secondary) window utilization for the `chatgpt` Subscription
//! provider.
//!
//! Data source: `GET https://chatgpt.com/backend-api/wham/usage`
//! (undocumented; same endpoint the official Codex CLI's `/status`
//! command uses — see upstream
//! `openai/codex` repo, `codex-rs/backend-client/src/client.rs`
//! `get_rate_limits_many`). Requires the OAuth access token Pi stores
//! in `~/.pi/agent/auth.json` under the `openai-codex` key (or
//! `openai` / `chatgpt` fallbacks — matching the lookup order in
//! `state/config.rs::check_pi_subscription_auth`). This is read-only:
//! we never refresh the token here — Pi owns auth.json writes and
//! would race us.
//!
//! NOTE on `ChatGPT-Account-Id`: Pi's `auth.json` may or may not store
//! `account_id` for `openai-codex`. The endpoint accepts requests
//! without the header for single-account users; multi-workspace
//! accounts may see incorrect `plan_type`. We surface the header when
//! present and accept the limitation otherwise.
//!
//! NOTE on Cloudflare: the upstream Codex CLI uses a custom cookie
//! store (`with_chatgpt_cloudflare_cookie_store`). Our `ctx.http()`
//! is a vanilla reqwest client with no cookie jar. If Cloudflare
//! starts gating this endpoint, requests will fail with 403 →
//! `ExtensionError::Auth`, which is recoverable and visible in
//! Settings.

use std::path::PathBuf;

use async_trait::async_trait;
use serde::Deserialize;

use crate::extensions::context::ExtensionContext;
use crate::extensions::traits::{ProviderExtension, UsageProvider};
use crate::extensions::types::{
    Capability, ExtensionError, ExtensionManifest, MetricKind, Tone, UsageMetric, UsageSnapshot,
};

const USAGE_URL: &str = "https://chatgpt.com/backend-api/wham/usage";
/// Required `User-Agent`. Match the static one in upstream
/// `backend-client::headers()` so we don't trip Cloudflare's bot
/// heuristics any more than necessary.
const CODEX_CLI_UA: &str = "codex_cli_rs/0.0.0";

pub struct ChatGptSubUsage {
    provider_id: String,
    extension_id: String,
}

impl ChatGptSubUsage {
    pub fn new(provider_id: impl Into<String>) -> Self {
        let provider_id = provider_id.into();
        let extension_id = format!("chatgpt_sub_usage:{}", provider_id);
        Self {
            provider_id,
            extension_id,
        }
    }
}

/// Tone ramp shared by all ChatGPT utilization metrics. Mirrors the
/// Claude side so the two pills read consistently. Public to the crate
/// so unit tests can target it directly.
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
    #[serde(default)]
    used_percent: f64,
    #[serde(default)]
    reset_at: Option<i64>,
    #[serde(default)]
    limit_window_seconds: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct RateLimit {
    #[serde(default)]
    primary_window: Option<Window>,
    #[serde(default)]
    secondary_window: Option<Window>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct Credits {
    #[serde(default)]
    has_credits: bool,
    #[serde(default)]
    unlimited: bool,
    #[serde(default)]
    balance: Option<String>,
}

#[derive(Debug, Deserialize)]
struct UsagePayload {
    #[serde(default)]
    plan_type: Option<String>,
    #[serde(default)]
    rate_limit: Option<RateLimit>,
    #[serde(default)]
    credits: Option<Credits>,
}

/// Read `~/.pi/agent/auth.json` and extract the ChatGPT/Codex OAuth
/// token + optional account_id. Synchronous; callers must wrap in
/// `spawn_blocking`.
fn read_chatgpt_oauth_token() -> Result<(String, Option<String>), ExtensionError> {
    let path = dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".pi")
        .join("agent")
        .join("auth.json");
    read_chatgpt_oauth_token_from(&path)
}

/// Internal variant taking an explicit path so tests can target a temp
/// file without overriding `HOME`.
fn read_chatgpt_oauth_token_from(
    path: &std::path::Path,
) -> Result<(String, Option<String>), ExtensionError> {
    if !path.exists() {
        return Err(ExtensionError::Auth(
            "ChatGPT subscription not logged in (no ~/.pi/agent/auth.json)".into(),
        ));
    }
    let data = std::fs::read_to_string(path)
        .map_err(|e| ExtensionError::Internal(format!("read auth.json: {e}")))?;
    let v: serde_json::Value = serde_json::from_str(&data)
        .map_err(|e| ExtensionError::Parse(format!("auth.json: {e}")))?;

    // Lookup order matches `state/config.rs::check_pi_subscription_auth`:
    // openai-codex → openai → chatgpt.
    let keys = ["openai-codex", "openai", "chatgpt"];
    for key in keys.iter() {
        if let Some(obj) = v.get(*key) {
            let token = obj
                .get("access")
                .and_then(|s| s.as_str())
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string());
            if let Some(token) = token {
                let account_id = obj
                    .get("account_id")
                    .and_then(|s| s.as_str())
                    .filter(|s| !s.is_empty())
                    .map(|s| s.to_string());
                return Ok((token, account_id));
            }
        }
    }

    Err(ExtensionError::Auth(
        "no openai-codex/openai/chatgpt access token in auth.json".into(),
    ))
}

/// Title-case a plan id like "plus" → "Plus", "enterprise" →
/// "Enterprise". Best-effort; ASCII-only.
fn title_case_plan(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut next_upper = true;
    for ch in s.chars() {
        if ch == '_' || ch == '-' || ch == ' ' {
            out.push(' ');
            next_upper = true;
            continue;
        }
        if next_upper {
            for u in ch.to_uppercase() {
                out.push(u);
            }
            next_upper = false;
        } else {
            for l in ch.to_lowercase() {
                out.push(l);
            }
        }
    }
    out
}

impl ProviderExtension for ChatGptSubUsage {
    fn manifest(&self) -> ExtensionManifest {
        ExtensionManifest {
            id: self.extension_id.clone(),
            type_id: "chatgpt_sub_usage".to_string(),
            provider_id: self.provider_id.clone(),
            display_name: "ChatGPT Subscription Usage".to_string(),
            description: "5-hour and weekly quota utilization for your ChatGPT (Codex) \
                 subscription. Reads the OAuth token Pi maintains in \
                 ~/.pi/agent/auth.json; does not require an API key."
                .to_string(),
            capabilities: vec![Capability::Usage, Capability::RateLimitProbe],
            requires_api_key: false,
            docs_url: Some("https://help.openai.com/en/articles/11369540".to_string()),
        }
    }

    fn usage_provider(&self) -> Option<&dyn UsageProvider> {
        Some(self)
    }
}

#[async_trait]
impl UsageProvider for ChatGptSubUsage {
    async fn fetch(&self, ctx: &ExtensionContext) -> Result<UsageSnapshot, ExtensionError> {
        // ── 1. Read OAuth token off disk ─────────────────────────────
        let (token, account_id) = tokio::task::spawn_blocking(read_chatgpt_oauth_token)
            .await
            .map_err(|e| ExtensionError::Internal(format!("spawn_blocking join: {e}")))??;

        // ── 2. Hit the /wham/usage endpoint ──────────────────────────
        let mut req = ctx
            .http()
            .get(USAGE_URL)
            .bearer_auth(&token)
            .header("Accept", "application/json")
            .header("User-Agent", CODEX_CLI_UA);
        if let Some(acct) = account_id.as_deref() {
            req = req.header("ChatGPT-Account-Id", acct);
        }

        let response = req
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
        let parsed: UsagePayload =
            serde_json::from_str(&body_text).map_err(|e| ExtensionError::Parse(e.to_string()))?;
        let raw: serde_json::Value =
            serde_json::from_str(&body_text).map_err(|e| ExtensionError::Parse(e.to_string()))?;

        // ── 3. Build headline & metrics ──────────────────────────────
        let mut metrics: Vec<UsageMetric> = Vec::new();

        let primary = parsed
            .rate_limit
            .as_ref()
            .and_then(|r| r.primary_window.as_ref());
        let secondary = parsed
            .rate_limit
            .as_ref()
            .and_then(|r| r.secondary_window.as_ref());

        // Headline = primary (5h) utilization.
        let headline = match primary {
            Some(w) if w.reset_at.is_some() || w.used_percent > 0.0 => {
                let u = w.used_percent;
                let tone = tone_for(u);
                UsageMetric {
                    key: "primary_utilization".to_string(),
                    label: "5h utilization".to_string(),
                    display: format!("{:.0}%", u),
                    value: u,
                    kind: MetricKind::Percentage,
                    tone,
                }
            }
            _ => UsageMetric {
                key: "primary_utilization".to_string(),
                label: "5h utilization".to_string(),
                display: "Idle".to_string(),
                value: 0.0,
                kind: MetricKind::Percentage,
                tone: Tone::Neutral,
            },
        };
        metrics.push(headline.clone());

        if let Some(w) = primary {
            if let Some(reset) = w.reset_at {
                let display = chrono::DateTime::from_timestamp(reset, 0)
                    .map(|d| d.to_rfc3339())
                    .unwrap_or_else(|| reset.to_string());
                metrics.push(UsageMetric {
                    key: "primary_resets_at".to_string(),
                    label: "5h resets at".to_string(),
                    display,
                    value: reset as f64,
                    kind: MetricKind::Duration,
                    tone: Tone::Neutral,
                });
            }
            if let Some(window_secs) = w.limit_window_seconds {
                metrics.push(UsageMetric {
                    key: "primary_window_seconds".to_string(),
                    label: "5h window".to_string(),
                    display: format!("{}s", window_secs),
                    value: window_secs as f64,
                    kind: MetricKind::Duration,
                    tone: Tone::Neutral,
                });
            }
        }

        if let Some(w) = secondary {
            metrics.push(UsageMetric {
                key: "secondary_utilization".to_string(),
                label: "7d utilization".to_string(),
                display: format!("{:.0}%", w.used_percent),
                value: w.used_percent,
                kind: MetricKind::Percentage,
                tone: tone_for(w.used_percent),
            });
            if let Some(reset) = w.reset_at {
                let display = chrono::DateTime::from_timestamp(reset, 0)
                    .map(|d| d.to_rfc3339())
                    .unwrap_or_else(|| reset.to_string());
                metrics.push(UsageMetric {
                    key: "secondary_resets_at".to_string(),
                    label: "7d resets at".to_string(),
                    display,
                    value: reset as f64,
                    kind: MetricKind::Duration,
                    tone: Tone::Neutral,
                });
            }
            if let Some(window_secs) = w.limit_window_seconds {
                metrics.push(UsageMetric {
                    key: "secondary_window_seconds".to_string(),
                    label: "7d window".to_string(),
                    display: format!("{}s", window_secs),
                    value: window_secs as f64,
                    kind: MetricKind::Duration,
                    tone: Tone::Neutral,
                });
            }
        }

        if let Some(plan) = parsed.plan_type.as_deref() {
            if !plan.is_empty() {
                metrics.push(UsageMetric {
                    key: "plan_type".to_string(),
                    label: "Plan".to_string(),
                    display: title_case_plan(plan),
                    value: 0.0,
                    kind: MetricKind::Count,
                    tone: Tone::Neutral,
                });
            }
        }

        if let Some(credits) = parsed.credits.as_ref() {
            if let Some(balance) = credits.balance.as_deref() {
                if !balance.is_empty() {
                    let value = balance.parse::<f64>().unwrap_or(0.0);
                    metrics.push(UsageMetric {
                        key: "credits_balance".to_string(),
                        label: "Credits balance".to_string(),
                        display: balance.to_string(),
                        value,
                        kind: MetricKind::Currency,
                        tone: Tone::Neutral,
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
        // 5 minutes — match the Claude side. The endpoint is unofficial
        // and may rate-limit; a 5-minute floor keeps us under any
        // reasonable per-token bucket while still feeling live.
        300
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

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
        // Cribbed from upstream openai/codex test fixtures for
        // `RateLimitStatusPayload`.
        let body = r#"{
            "plan_type": "plus",
            "rate_limit": {
                "primary_window":   { "used_percent": 42, "limit_window_seconds": 18000, "reset_at": 1730000000 },
                "secondary_window": { "used_percent": 84, "limit_window_seconds": 604800, "reset_at": 1730500000 }
            },
            "credits": { "has_credits": true, "unlimited": false, "balance": "9.99" }
        }"#;
        let parsed: UsagePayload = serde_json::from_str(body).unwrap();
        assert_eq!(parsed.plan_type.as_deref(), Some("plus"));
        let rl = parsed.rate_limit.as_ref().unwrap();
        let p = rl.primary_window.as_ref().unwrap();
        assert_eq!(p.used_percent, 42.0);
        assert_eq!(p.reset_at, Some(1730000000));
        assert_eq!(p.limit_window_seconds, Some(18000));
        let s = rl.secondary_window.as_ref().unwrap();
        assert_eq!(s.used_percent, 84.0);
        assert_eq!(s.reset_at, Some(1730500000));
        let credits = parsed.credits.as_ref().unwrap();
        assert!(credits.has_credits);
        assert!(!credits.unlimited);
        assert_eq!(credits.balance.as_deref(), Some("9.99"));
    }

    #[test]
    fn manifest_id_is_composite() {
        let ext = ChatGptSubUsage::new("chatgpt");
        assert_eq!(ext.manifest().id, "chatgpt_sub_usage:chatgpt");
        assert_eq!(ext.manifest().type_id, "chatgpt_sub_usage");
        assert_eq!(ext.manifest().provider_id, "chatgpt");
        assert!(!ext.manifest().requires_api_key);
    }

    #[test]
    fn title_case_plan_handles_common_shapes() {
        assert_eq!(title_case_plan("plus"), "Plus");
        assert_eq!(title_case_plan("PRO"), "Pro");
        assert_eq!(title_case_plan("enterprise_v2"), "Enterprise V2");
        assert_eq!(title_case_plan("team-x"), "Team X");
    }

    fn write_auth(path: &std::path::Path, json: &str) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        let mut f = std::fs::File::create(path).unwrap();
        f.write_all(json.as_bytes()).unwrap();
    }

    #[test]
    fn read_token_prefers_openai_codex_key_over_openai_then_chatgpt() {
        let tmp = tempfile::tempdir().unwrap();

        // Case 1: only `chatgpt` present.
        let p1 = tmp.path().join("c1.json");
        write_auth(
            &p1,
            r#"{ "chatgpt": { "access": "tok-chatgpt", "account_id": "acct-c" } }"#,
        );
        let (tok, acct) = read_chatgpt_oauth_token_from(&p1).unwrap();
        assert_eq!(tok, "tok-chatgpt");
        assert_eq!(acct.as_deref(), Some("acct-c"));

        // Case 2: `openai` + `chatgpt` — openai wins.
        let p2 = tmp.path().join("c2.json");
        write_auth(
            &p2,
            r#"{
                "openai":  { "access": "tok-openai" },
                "chatgpt": { "access": "tok-chatgpt" }
            }"#,
        );
        let (tok, acct) = read_chatgpt_oauth_token_from(&p2).unwrap();
        assert_eq!(tok, "tok-openai");
        assert_eq!(acct, None);

        // Case 3: all three present — openai-codex wins.
        let p3 = tmp.path().join("c3.json");
        write_auth(
            &p3,
            r#"{
                "openai-codex": { "access": "tok-codex", "account_id": "acct-x" },
                "openai":       { "access": "tok-openai" },
                "chatgpt":      { "access": "tok-chatgpt" }
            }"#,
        );
        let (tok, acct) = read_chatgpt_oauth_token_from(&p3).unwrap();
        assert_eq!(tok, "tok-codex");
        assert_eq!(acct.as_deref(), Some("acct-x"));

        // Case 4: missing file → Auth error.
        let p4 = tmp.path().join("nope.json");
        let err = read_chatgpt_oauth_token_from(&p4).unwrap_err();
        assert!(matches!(err, ExtensionError::Auth(_)));

        // Case 5: present file but no usable key → Auth error.
        let p5 = tmp.path().join("c5.json");
        write_auth(&p5, r#"{ "anthropic": { "access": "tok-anth" } }"#);
        let err = read_chatgpt_oauth_token_from(&p5).unwrap_err();
        assert!(matches!(err, ExtensionError::Auth(_)));
    }
}
