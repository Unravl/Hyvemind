//! Shared types for the Provider Extension system.
//!
//! These types form the public API surface between extensions, the
//! registry/poller, and the IPC layer. All are `Serialize + Deserialize`
//! so they cross the Tauri IPC boundary unchanged.

use serde::{Deserialize, Serialize};

/// Capabilities an extension may advertise. Closed enum — adding a new
/// capability is a deliberate compile-time change, consistent with the
/// rest of Hyvemind's "compile-time everything" stance.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum Capability {
    Usage,
    Billing,
    RateLimitProbe,
    ModelCatalog,
}

/// Kind of a metric. Drives default frontend formatting and grouping.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MetricKind {
    Currency,
    Percentage,
    Tokens,
    Count,
    Duration,
}

/// Visual tone hint for a metric. The frontend maps these to colour
/// classes (e.g. `crit` → red, `warn` → amber, `ok` → emerald,
/// `neutral` → slate).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Tone {
    Ok,
    Warn,
    Crit,
    Neutral,
}

/// One key/value-ish metric in a snapshot.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsageMetric {
    pub key: String,
    pub label: String,
    /// Pre-formatted display string (e.g. `"$8.42"`, `"73%"`, `"2.1M / 5M tok"`).
    /// Extensions are responsible for locale/precision; the frontend
    /// renders this verbatim.
    pub display: String,
    /// Raw numeric value for charts / comparisons. Use NaN-safe
    /// transports (NaN/Infinity are not valid JSON; extensions must
    /// avoid them).
    pub value: f64,
    pub kind: MetricKind,
    pub tone: Tone,
}

/// A snapshot of usage/credits/etc. for one extension instance.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsageSnapshot {
    pub extension_id: String,
    pub provider_id: String,
    /// Unix timestamp in seconds (UTC), consistent with
    /// `chrono::Utc::now().timestamp()`.
    pub fetched_at: i64,
    /// Metric shown in the Topbar pill.
    pub headline: Option<UsageMetric>,
    /// All metrics shown in the popover / Settings panel.
    pub metrics: Vec<UsageMetric>,
    /// Provider-native payload for power users. Capped at 64 KB by the
    /// poller (measured via `serde_json::to_vec(&raw).len()`); larger
    /// responses are truncated to `None` with a warning log including
    /// the original size. This cap limits IPC bandwidth, not memory —
    /// the full response may have been deserialized already.
    pub raw: Option<serde_json::Value>,
}

/// Manifest describing one registered extension.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtensionManifest {
    /// Composite identifier — `format!("{}:{}", type_id, provider_id)`.
    /// Used as the HashMap key in the registry.
    pub id: String,
    /// Base identifier shared across instances (e.g. `"openrouter_credits"`).
    /// Used by the frontend widget registry for lookup.
    pub type_id: String,
    pub provider_id: String,
    pub display_name: String,
    pub description: String,
    pub capabilities: Vec<Capability>,
    pub requires_api_key: bool,
    pub docs_url: Option<String>,
}

/// Error returned by an extension's `fetch()` call.
#[derive(Debug, Clone, Serialize, Deserialize, thiserror::Error)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ExtensionError {
    /// The extension cannot operate against this provider (no API,
    /// admin-only, etc.). Poller stops calling after this is returned.
    #[error("unsupported: {0}")]
    Unsupported(String),
    /// HTTP / connection failure.
    #[error("network: {0}")]
    Network(String),
    /// Authentication failure (missing/invalid API key, 401/403).
    #[error("auth: {0}")]
    Auth(String),
    /// Response parsing failure.
    #[error("parse: {0}")]
    Parse(String),
    /// Unexpected internal failure.
    #[error("internal: {0}")]
    Internal(String),
}

impl ExtensionError {
    /// Short human-readable label for the variant — used in `last_error`
    /// fields surfaced to the frontend.
    pub fn user_message(&self) -> String {
        self.to_string()
    }
}

/// High-level status surfaced to the frontend for each extension entry.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SnapshotStatus {
    /// Registered but no snapshot fetched yet.
    Loading,
    /// Snapshot fetched successfully and is fresh.
    Ok,
    /// Last fetch failed; `last_error` is populated.
    Error,
    /// Extension does not support this provider (terminal state — poller
    /// stops after first attempt).
    Unsupported,
    /// User-disabled via settings.
    Disabled,
}

/// Per-extension user preferences persisted in `Config.extension_settings`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExtensionUserSettings {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_true")]
    pub show_in_topbar: bool,
    /// Extension-specific key-value preferences (e.g. display mode).
    /// Extensions that define custom preferences should document the
    /// keys they recognise; the poller reads these before each fetch.
    #[serde(default)]
    pub preferences: std::collections::HashMap<String, String>,
}

fn default_true() -> bool {
    true
}

impl Default for ExtensionUserSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            show_in_topbar: true,
            preferences: std::collections::HashMap::new(),
        }
    }
}

/// One entry in the shared `usage_snapshots` map.
///
/// Carries everything the frontend needs to render a row in the
/// Settings panel and (optionally) a Topbar pill, in a single payload.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotEntry {
    pub manifest: ExtensionManifest,
    pub snapshot: Option<UsageSnapshot>,
    pub last_error: Option<String>,
    pub last_fetched_at: Option<i64>,
    pub status: SnapshotStatus,
    pub user_settings: ExtensionUserSettings,
}
