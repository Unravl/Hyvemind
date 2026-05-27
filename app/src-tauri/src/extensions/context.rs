//! Per-call context handed to extension `fetch()` implementations.
//!
//! Owns short-lived handles to subsystems extensions may legitimately
//! need: the live config (for API keys and per-extension settings),
//! a shared `reqwest::Client`, the Pi process pool (for extensions that
//! probe Pi state), and the data directory.
//!
//! # Lock-ordering invariant
//!
//! Always acquire `config` **before** `provider_registry`, never the
//! reverse. Extension `fetch()` implementations must not hold the
//! config read lock across `await` points — read into a local, drop
//! the guard, then perform I/O.

use std::path::PathBuf;
use std::sync::Arc;

use tokio::sync::RwLock;

use crate::extensions::poller::MIN_REFRESH_INTERVAL_SECS;
use crate::pi::manager::PiManager;
use crate::state::config::{Config, EXTENSION_POLL_INTERVAL_MAX_SECS};

use super::types::ExtensionUserSettings;

pub struct ExtensionContext {
    config: Arc<RwLock<Config>>,
    http: reqwest::Client,
    #[allow(dead_code)]
    pi_manager: Arc<PiManager>,
    data_dir: PathBuf,
}

// Manual `Debug` impl that redacts the live config (which carries API
// keys in memory). Mirrors the `hivemind/providers.rs` pattern — even an
// accidental `?ctx` in a future tracing site can never leak a key.
impl std::fmt::Debug for ExtensionContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExtensionContext")
            .field("config", &"[REDACTED]")
            .field("http", &"reqwest::Client")
            .field("pi_manager", &"PiManager { .. }")
            .field("data_dir", &self.data_dir)
            .finish()
    }
}

impl ExtensionContext {
    pub fn new(
        config: Arc<RwLock<Config>>,
        http: reqwest::Client,
        pi_manager: Arc<PiManager>,
        data_dir: PathBuf,
    ) -> Self {
        Self {
            config,
            http,
            pi_manager,
            data_dir,
        }
    }

    /// Returns a snapshot of the API key for the given provider, if any.
    /// Each call re-reads from config so the secret doesn't outlive the
    /// config lock. Callers should capture the result into a local and
    /// drop the guard before starting I/O.
    pub async fn api_key(&self, provider_id: &str) -> Option<String> {
        let cfg = self.config.read().await;
        cfg.provider_keys.get(provider_id).cloned()
    }

    /// Read the persisted user settings for one extension.
    pub async fn extension_settings(&self, extension_id: &str) -> ExtensionUserSettings {
        let cfg = self.config.read().await;
        cfg.extension_settings
            .get(extension_id)
            .cloned()
            .unwrap_or_default()
    }

    /// Global poll interval for provider-extension fetches (seconds).
    ///
    /// Reads from `config.extension_poll_interval_secs` (persisted via
    /// the Settings UI) then clamps to `[MIN_REFRESH_INTERVAL_SECS,
    /// EXTENSION_POLL_INTERVAL_MAX_SECS]` for defense-in-depth even if
    /// the config file somehow carries an out-of-range value.
    pub async fn poll_interval_secs(&self) -> u64 {
        let cfg = self.config.read().await;
        cfg.extension_poll_interval_secs
            .clamp(MIN_REFRESH_INTERVAL_SECS, EXTENSION_POLL_INTERVAL_MAX_SECS)
    }

    /// Shared HTTP client. Extensions should use this rather than
    /// constructing their own so the connection pool is shared.
    pub fn http(&self) -> &reqwest::Client {
        &self.http
    }

    #[allow(dead_code)]
    pub fn pi_manager(&self) -> &Arc<PiManager> {
        &self.pi_manager
    }

    #[allow(dead_code)]
    pub fn data_dir(&self) -> &PathBuf {
        &self.data_dir
    }
}
