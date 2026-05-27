use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;
use tauri::Emitter;
use tracing::{info, warn};

use crate::commands::util::validate_id;
use crate::state::app_state::AppState;
use crate::state::config::{Config, CustomPrompt};
use crate::state::ipc_error::IpcError;
use crate::state::secret_store::SecretStore;

/// Allowed values for `provider_type` in `add_provider`. Mirrors the canonical
/// set seeded by `seed_default_providers` in `state/config.rs`.
const ALLOWED_PROVIDER_TYPES: &[&str] = &["Anthropic", "OpenAI Compatible", "Subscription", "CLI"];

/// Validate an HTTP(S) endpoint URL. Rejects empty input, malformed URLs,
/// non-`http(s)` schemes, and plaintext `http://` to non-loopback hosts.
fn validate_endpoint(url: &str) -> Result<(), String> {
    let trimmed = url.trim();
    if trimmed.is_empty() {
        return Err("endpoint cannot be empty".to_string());
    }
    let parsed =
        reqwest::Url::parse(trimmed).map_err(|e| format!("endpoint is not a valid URL: {}", e))?;
    let scheme = parsed.scheme();
    if scheme != "http" && scheme != "https" {
        return Err(format!(
            "endpoint scheme must be http or https, got '{}'",
            scheme
        ));
    }
    if scheme == "http" {
        let host = parsed.host_str().unwrap_or("");
        let is_loopback = matches!(host, "localhost" | "127.0.0.1" | "::1" | "[::1]");
        if !is_loopback {
            return Err(
                "non-HTTPS endpoint must point to localhost (use https:// for remote hosts)"
                    .to_string(),
            );
        }
    }
    Ok(())
}

/// Validate and canonicalize a working-directory string supplied via IPC.
///
/// Rejects empty input and embedded NUL bytes, expands a leading `~/` to the
/// user's home directory, then `canonicalize()`s the path and asserts it is a
/// directory. Returned value is the canonical `PathBuf`.
fn validate_working_dir(p: &str) -> Result<PathBuf, String> {
    let trimmed = p.trim();
    if trimmed.is_empty() {
        return Err("path cannot be empty".to_string());
    }
    if trimmed.contains('\0') {
        return Err("path contains NUL byte".to_string());
    }

    // Expand a leading `~` (the only `~` form we support — no `~user/` lookup).
    let expanded: PathBuf = if trimmed == "~" || trimmed.starts_with("~/") {
        let home = dirs::home_dir().ok_or_else(|| "cannot resolve home directory".to_string())?;
        if trimmed == "~" {
            home
        } else {
            home.join(&trimmed[2..])
        }
    } else {
        PathBuf::from(trimmed)
    };

    let canonical = crate::commands::util::canonicalize_clean(&expanded)
        .map_err(|e| format!("cannot resolve path '{}': {}", expanded.display(), e))?;
    if !canonical.is_dir() {
        return Err(format!("path '{}' is not a directory", canonical.display()));
    }
    Ok(canonical)
}

/* ── Pi status types ─────────────────────────────────────── */

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PiInstallMethod {
    Npm,
    Homebrew,
    Unknown,
}

#[derive(Debug, Clone, Serialize)]
pub struct PiStatusResponse {
    pub installed: bool,
    pub binary_path: Option<String>,
    pub resolved_path: Option<String>,
    /// The binary name that was found (e.g. "pi").
    pub binary_name: Option<String>,
    pub version: Option<String>,
    pub latest_version: Option<String>,
    pub is_outdated: bool,
    pub install_method: PiInstallMethod,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PiUpdateEvent {
    pub event_type: String,
    pub message: String,
}

/// Response returned by `get_settings`.
#[derive(Debug, Clone, Serialize)]
pub struct SettingsResponse {
    pub configured_providers: Vec<String>,
    pub default_model: Option<String>,
    pub default_hivemind: Option<String>,
    pub default_project_path: Option<String>,
    pub concurrency_cap: usize,
    pub max_pi_processes: usize,
    pub data_dir: String,
    pub source_dir: String,
    pub stable_mode: bool,
    pub debug_mode: bool,
    pub auto_commit_tasks: bool,
    pub auto_commit_conventional: bool,
    pub task_completion_sound_enabled: bool,
    pub task_completion_sound: String,
    pub crash_reporting_enabled: bool,
    pub chat_check_in_secs: u64,
    /// Global extension poll interval in seconds. Clamped to
    /// [`EXTENSION_POLL_INTERVAL_MIN_SECS`, `EXTENSION_POLL_INTERVAL_MAX_SECS`].
    pub extension_poll_interval_secs: u64,
    /// Phase 5A: global daily spending cap in USD. `null` means
    /// unlimited.
    pub daily_budget_usd: Option<f64>,
    /// Audit 1.11: approved working-directory allowlist. The frontend
    /// consults this before sending any IPC that carries a `working_dir`
    /// — if the user's pick isn't in this list, it shows an approval
    /// modal and calls `request_working_dir_approval` on Allow. Paths
    /// are canonicalized at insert time.
    pub approved_working_dirs: Vec<String>,
}

/// Information about a provider's configuration state.
#[derive(Debug, Clone, Serialize)]
pub struct ProviderInfo {
    pub name: String,
    pub display_name: String,
    pub provider_type: String,
    pub endpoint: Option<String>,
    pub configured: bool,
    pub model_count: usize,
    pub health: Option<bool>,
}

/// Model information returned to the frontend.
#[derive(Debug, Clone, Serialize)]
pub struct ModelInfoResponse {
    pub provider: String,
    pub model_id: String,
    pub context_window: usize,
    pub cost_per_1m_input: f64,
    pub cost_per_1m_output: f64,
}

fn build_settings_response(config: &Config) -> SettingsResponse {
    let source_dir = std::env::current_dir()
        .map(|p| p.display().to_string())
        .unwrap_or_default();

    // Dev runs are always stable mode (no HMR, no watcher).
    let stable_mode = true;
    let debug_mode = std::env::var("HYVEMIND_DEBUG").map_or(false, |v| v == "1");

    SettingsResponse {
        configured_providers: config.configured_providers(),
        default_model: config.default_model.clone(),
        default_hivemind: config.default_hivemind.clone(),
        default_project_path: config.default_project_path.clone(),
        concurrency_cap: config.concurrency_cap,
        max_pi_processes: config.max_pi_processes,
        data_dir: config.data_dir.display().to_string(),
        source_dir,
        stable_mode,
        debug_mode,
        auto_commit_tasks: config.auto_commit_tasks,
        auto_commit_conventional: config.auto_commit_conventional,
        task_completion_sound_enabled: config.task_completion_sound_enabled,
        task_completion_sound: config.task_completion_sound.clone(),
        crash_reporting_enabled: config.crash_reporting_enabled.unwrap_or(true),
        chat_check_in_secs: config.chat_check_in_secs,
        extension_poll_interval_secs: config.extension_poll_interval_secs,
        daily_budget_usd: config.daily_budget_usd,
        approved_working_dirs: config
            .approved_working_dirs
            .iter()
            .map(|p| p.display().to_string())
            .collect(),
    }
}

fn validate_runtime_settings(
    concurrency_cap: usize,
    max_pi_processes: usize,
) -> Result<(), String> {
    if concurrency_cap < 1 {
        return Err("concurrency cap must be at least 1".to_string());
    }
    if max_pi_processes < 1 {
        return Err("max Pi processes must be at least 1".to_string());
    }
    Ok(())
}

/// Get the current application settings.
///
/// Returns configuration values with API keys redacted (last 4 characters
/// only) and a list of configured providers.
#[tracing::instrument(skip(state))]
#[tauri::command]
pub async fn get_settings(state: tauri::State<'_, AppState>) -> Result<SettingsResponse, IpcError> {
    info!("get_settings invoked");

    let config = state.config.read().await;
    Ok(build_settings_response(&config))
}

/// Persist runtime settings that control review concurrency and Pi process limits.
#[tracing::instrument(skip(state))]
#[tauri::command]
pub async fn set_runtime_settings(
    state: tauri::State<'_, AppState>,
    concurrency_cap: usize,
    max_pi_processes: usize,
) -> Result<SettingsResponse, IpcError> {
    info!(
        concurrency_cap = concurrency_cap,
        max_pi_processes = max_pi_processes,
        "set_runtime_settings invoked"
    );

    validate_runtime_settings(concurrency_cap, max_pi_processes)?;

    let (response, data_dir, bytes) = {
        let mut config = state.config.write().await;
        config.concurrency_cap = concurrency_cap;
        config.max_pi_processes = max_pi_processes;
        let bytes = config
            .snapshot_to_bytes()
            .map_err(|e| format!("failed to serialize config: {}", e))?;
        let data_dir = config.data_dir.clone();
        let response = build_settings_response(&config);
        (response, data_dir, bytes)
    };
    Config::write_bytes(data_dir, bytes)
        .await
        .map_err(|e| format!("failed to save config: {}", e))?;
    Ok(response)
}

/// Set the auto-commit-tasks toggle.
#[tracing::instrument(skip(state))]
#[tauri::command]
pub async fn set_auto_commit_tasks(
    state: tauri::State<'_, AppState>,
    enabled: bool,
) -> Result<(), IpcError> {
    info!(enabled = enabled, "set_auto_commit_tasks invoked");
    let (data_dir, bytes) = {
        let mut config = state.config.write().await;
        config.auto_commit_tasks = enabled;
        let bytes = config
            .snapshot_to_bytes()
            .map_err(|e| format!("failed to serialize config: {}", e))?;
        (config.data_dir.clone(), bytes)
    };
    Config::write_bytes(data_dir, bytes)
        .await
        .map_err(|e| format!("failed to save config: {}", e))?;
    Ok(())
}

/// Toggle Conventional Commits style for auto-commit titles.
#[tracing::instrument(skip(state))]
#[tauri::command]
pub async fn set_auto_commit_conventional(
    state: tauri::State<'_, AppState>,
    enabled: bool,
) -> Result<(), IpcError> {
    info!(enabled = enabled, "set_auto_commit_conventional invoked");
    let (data_dir, bytes) = {
        let mut config = state.config.write().await;
        config.auto_commit_conventional = enabled;
        let bytes = config
            .snapshot_to_bytes()
            .map_err(|e| format!("failed to serialize config: {}", e))?;
        (config.data_dir.clone(), bytes)
    };
    Config::write_bytes(data_dir, bytes)
        .await
        .map_err(|e| format!("failed to save config: {}", e))?;
    Ok(())
}

/// Toggle crash reporting (Sentry) on or off. Persisted to config and
/// applied on next launch (Sentry initialises once at startup).
#[tracing::instrument(skip(state))]
#[tauri::command]
pub async fn set_crash_reporting(
    state: tauri::State<'_, AppState>,
    enabled: bool,
) -> Result<(), IpcError> {
    info!(enabled = enabled, "set_crash_reporting invoked");
    let (data_dir, bytes) = {
        let mut config = state.config.write().await;
        config.crash_reporting_enabled = Some(enabled);
        let bytes = config
            .snapshot_to_bytes()
            .map_err(|e| format!("failed to serialize config: {}", e))?;
        (config.data_dir.clone(), bytes)
    };
    Config::write_bytes(data_dir, bytes)
        .await
        .map_err(|e| format!("failed to save config: {}", e))?;
    Ok(())
}

/// Set the Nurse chat check-in interval (seconds). Clamped to
/// `[CHAT_CHECK_IN_MIN_SECS, CHAT_CHECK_IN_MAX_SECS]`. Persisted to config.
#[tracing::instrument(skip(state))]
#[tauri::command]
pub async fn set_chat_check_in_secs(
    state: tauri::State<'_, AppState>,
    secs: u64,
) -> Result<SettingsResponse, IpcError> {
    use crate::state::config::{CHAT_CHECK_IN_MAX_SECS, CHAT_CHECK_IN_MIN_SECS};
    if secs < CHAT_CHECK_IN_MIN_SECS || secs > CHAT_CHECK_IN_MAX_SECS {
        return Err(IpcError::internal(format!(
            "chat_check_in_secs must be between {} and {}",
            CHAT_CHECK_IN_MIN_SECS, CHAT_CHECK_IN_MAX_SECS
        )));
    }
    info!(secs = secs, "set_chat_check_in_secs invoked");
    let (response, data_dir, bytes) = {
        let mut config = state.config.write().await;
        config.chat_check_in_secs = secs;
        let bytes = config
            .snapshot_to_bytes()
            .map_err(|e| format!("failed to serialize config: {}", e))?;
        let response = build_settings_response(&config);
        (response, config.data_dir.clone(), bytes)
    };
    Config::write_bytes(data_dir, bytes)
        .await
        .map_err(|e| format!("failed to save config: {}", e))?;
    Ok(response)
}

/// Set the global extension poll interval (seconds). Clamped to
/// `[EXTENSION_POLL_INTERVAL_MIN_SECS, EXTENSION_POLL_INTERVAL_MAX_SECS]`.
/// Persisted to config. The poller reads this value from context before
/// each sleep cycle, so the change takes effect on the very next tick.
#[tracing::instrument(skip(state))]
#[tauri::command]
pub async fn set_extension_poll_interval_secs(
    state: tauri::State<'_, AppState>,
    secs: u64,
) -> Result<SettingsResponse, IpcError> {
    use crate::state::config::{
        EXTENSION_POLL_INTERVAL_MAX_SECS, EXTENSION_POLL_INTERVAL_MIN_SECS,
    };
    if secs < EXTENSION_POLL_INTERVAL_MIN_SECS || secs > EXTENSION_POLL_INTERVAL_MAX_SECS {
        return Err(IpcError::internal(format!(
            "extension_poll_interval_secs must be between {} and {}",
            EXTENSION_POLL_INTERVAL_MIN_SECS, EXTENSION_POLL_INTERVAL_MAX_SECS
        )));
    }
    info!(secs = secs, "set_extension_poll_interval_secs invoked");
    let (response, data_dir, bytes) = {
        let mut config = state.config.write().await;
        config.extension_poll_interval_secs = secs;
        let bytes = config
            .snapshot_to_bytes()
            .map_err(|e| format!("failed to serialize config: {}", e))?;
        let response = build_settings_response(&config);
        (response, config.data_dir.clone(), bytes)
    };
    Config::write_bytes(data_dir, bytes)
        .await
        .map_err(|e| format!("failed to save config: {}", e))?;
    Ok(response)
}

/// Phase 5A: persist the global daily spending cap. `None` means unlimited;
/// any value is clamped to non-negative (negatives are rejected). The cap
/// is consulted by `core::queen::run_swarm_full` between feature batches.
#[tracing::instrument(skip(state))]
#[tauri::command]
pub async fn set_daily_budget(
    state: tauri::State<'_, AppState>,
    usd: Option<f64>,
) -> Result<SettingsResponse, IpcError> {
    info!(usd = ?usd, "set_daily_budget invoked");
    if let Some(v) = usd {
        if !v.is_finite() || v < 0.0 {
            return Err(IpcError::internal(format!(
                "daily budget must be a non-negative number or null (got {})",
                v
            )));
        }
    }
    let (response, data_dir, bytes) = {
        let mut config = state.config.write().await;
        config.daily_budget_usd = usd;
        let bytes = config
            .snapshot_to_bytes()
            .map_err(|e| format!("failed to serialize config: {}", e))?;
        let response = build_settings_response(&config);
        (response, config.data_dir.clone(), bytes)
    };
    Config::write_bytes(data_dir, bytes)
        .await
        .map_err(|e| format!("failed to save config: {}", e))?;
    Ok(response)
}

/// Set the task completion sound settings (enabled/disabled + which sound).
#[tracing::instrument(skip(state))]
#[tauri::command]
pub async fn set_task_completion_sound(
    state: tauri::State<'_, AppState>,
    enabled: bool,
    sound: String,
) -> Result<SettingsResponse, IpcError> {
    info!(enabled = enabled, sound = %sound, "set_task_completion_sound invoked");

    let known_sounds = ["chime", "pop", "bell", "success", "tweet"];
    if sound.is_empty() || !known_sounds.contains(&sound.as_str()) {
        return Err(IpcError::internal(format!(
            "Unknown completion sound: \"{}\". Must be one of: {}",
            sound,
            known_sounds.join(", ")
        )));
    }

    let (response, data_dir, bytes) = {
        let mut config = state.config.write().await;
        config.task_completion_sound_enabled = enabled;
        config.task_completion_sound = sound;
        let bytes = config
            .snapshot_to_bytes()
            .map_err(|e| format!("failed to serialize config: {}", e))?;
        let response = build_settings_response(&config);
        (response, config.data_dir.clone(), bytes)
    };
    Config::write_bytes(data_dir, bytes)
        .await
        .map_err(|e| format!("failed to save config: {}", e))?;

    Ok(response)
}

/// Save an API key for a provider.
///
/// Persists to `~/.hyvemind/config.json` and updates the in-memory config.
#[tracing::instrument(skip(state, app, api_key))]
#[tauri::command]
pub async fn save_api_key(
    state: tauri::State<'_, AppState>,
    app: tauri::AppHandle,
    provider: String,
    api_key: String,
) -> Result<(), IpcError> {
    let provider = provider.trim().to_lowercase();
    info!(provider = %provider, "save_api_key invoked");

    if provider.is_empty() {
        return Err(IpcError::validation(
            "provider name cannot be empty".to_string(),
        ));
    }
    validate_id(&provider)?;
    if api_key.trim().is_empty() {
        return Err(IpcError::validation("API key cannot be empty".to_string()));
    }

    // Mutate the in-memory config under the write guard, snapshot the data
    // that needs to be persisted (provider-keys map + a clone of the config
    // for the file write), then drop the guard BEFORE doing any synchronous
    // keyring / disk I/O. The macOS Keychain round-trip can block for seconds
    // when the system shows an authorization prompt; doing it inside the
    // guard would block every other config reader app-wide.
    let (map, config_snapshot, env) = {
        let mut config = state.config.write().await;
        config.provider_keys.insert(provider.clone(), api_key);
        if let Some(pc) = config.providers.get_mut(&provider) {
            pc.has_key = true;
        }

        let map: BTreeMap<String, String> = config
            .provider_keys
            .iter()
            .filter(|(_, v)| !v.is_empty())
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        let env = config.pi_env_vars();
        let config_snapshot = config.clone();
        (map, config_snapshot, env)
    }; // guard drops here

    // Off-thread the blocking work: keyring write, file cache write, and
    // config.json atomic rewrite. The keyring call in particular can block
    // for several seconds on macOS while the Keychain prompt is up.
    tokio::task::spawn_blocking(move || -> Result<(), String> {
        // Canonical store: single combined keychain entry. One prompt
        // regardless of how many providers are configured.
        SecretStore::save_all(&map)
            .map_err(|e| format!("failed to write API key to keyring: {}", e))?;

        // Keep file-based credential cache in sync with keyring.
        if let Err(e) = SecretStore::save_to_file(&config_snapshot.data_dir, &map) {
            warn!(error = %e, "failed to update credentials file cache");
        }

        // Persist config metadata (no key data — `provider_keys` is
        // `skip_serializing` so this rewrites config.json without secrets).
        config_snapshot
            .save_blocking()
            .map_err(|e| format!("failed to save config: {}", e))?;
        Ok(())
    })
    .await
    .map_err(|e| format!("spawn_blocking join error: {}", e))??;

    // Propagate updated keys to the PiManager so future Pi sessions receive
    // the new API key as an environment variable.
    state.pi_manager.update_env_vars(env).await;

    // Rebuild the hivemind provider registry so subsequent reviews and
    // nurse calls see the new key without needing an app restart. Must
    // happen after the config write guard above is dropped — the helper
    // takes its own read lock on config.
    state.refresh_provider_registry().await;

    // Reconcile provider-extension registry against the (possibly
    // newly-keyed) provider so its pollers see the new key.
    state.refresh_extension_registry(&app).await;

    info!(provider = %provider, "API key saved successfully");
    Ok(())
}

/// Delete a stored API key for a provider.
///
/// Removes from in-memory config and persists the change to disk.
#[tracing::instrument(skip(state, app))]
#[tauri::command]
pub async fn delete_api_key(
    state: tauri::State<'_, AppState>,
    app: tauri::AppHandle,
    provider: String,
) -> Result<(), IpcError> {
    let provider = provider.trim().to_lowercase();
    info!(provider = %provider, "delete_api_key invoked");

    if provider.is_empty() {
        return Err(IpcError::validation(
            "provider name cannot be empty".to_string(),
        ));
    }
    validate_id(&provider)?;

    // Snapshot under the write guard, then drop the guard BEFORE doing any
    // keyring / disk I/O. See `save_api_key` for the rationale — the macOS
    // Keychain round-trip can block for seconds and would otherwise hold up
    // every other config reader.
    let (map, config_snapshot, env) = {
        let mut config = state.config.write().await;
        config.provider_keys.remove(&provider);
        if let Some(pc) = config.providers.get_mut(&provider) {
            pc.has_key = false;
        }

        let map: BTreeMap<String, String> = config
            .provider_keys
            .iter()
            .filter(|(_, v)| !v.is_empty())
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        let env = config.pi_env_vars();
        let config_snapshot = config.clone();
        (map, config_snapshot, env)
    }; // guard drops here

    // Off-thread the blocking work: rewrite the combined keychain entry
    // (single access for the whole map), refresh the file cache, clear any
    // legacy per-provider entry, then atomically rewrite config.json.
    let provider_for_blocking = provider.clone();
    tokio::task::spawn_blocking(move || -> Result<(), String> {
        SecretStore::save_all(&map).map_err(|e| format!("failed to update keyring: {}", e))?;

        if let Err(e) = SecretStore::save_to_file(&config_snapshot.data_dir, &map) {
            warn!(error = %e, "failed to update credentials file cache");
        }

        // Best-effort cleanup of any legacy per-provider entry left over
        // from before keychain consolidation. NoEntry is fine.
        if let Err(e) = SecretStore::delete(&provider_for_blocking) {
            warn!(provider = %provider_for_blocking, error = %e, "failed to delete legacy per-provider keyring entry; safe to ignore");
        }

        config_snapshot.save_blocking()
            .map_err(|e| format!("failed to save config: {}", e))?;
        Ok(())
    })
    .await
    .map_err(|e| format!("spawn_blocking join error: {}", e))??;

    // Propagate updated keys to the PiManager
    state.pi_manager.update_env_vars(env).await;

    // Rebuild the hivemind provider registry so subsequent reviews and
    // nurse calls stop seeing this provider (or see it as keyless and
    // skip registration) without needing an app restart.
    state.refresh_provider_registry().await;
    state.refresh_extension_registry(&app).await;

    info!(provider = %provider, "API key deleted");
    Ok(())
}

/// Set the default model.
#[tracing::instrument(skip(state, app))]
#[tauri::command]
pub async fn set_default_model(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    model: String,
) -> Result<(), IpcError> {
    let model = model.trim().to_string();
    info!(model = %model, "set_default_model invoked");

    if !model.contains('/') && !model.is_empty() {
        warn!("set_default_model: model '{}' does not contain '/' separator; expected 'provider_id/model_id' format (e.g. 'anthropic/claude-sonnet-4-20250514'). Bare model IDs still work but may not resolve correctly.", model);
    }

    let (data_dir, bytes, payload) = {
        let mut config = state.config.write().await;
        config.default_model = if model.is_empty() { None } else { Some(model) };
        let bytes = config
            .snapshot_to_bytes()
            .map_err(|e| format!("failed to serialize config: {}", e))?;
        let payload = serde_json::json!({ "model": config.default_model });
        (config.data_dir.clone(), bytes, payload)
    };
    Config::write_bytes(data_dir, bytes)
        .await
        .map_err(|e| format!("failed to save config: {}", e))?;

    // Emit event so the frontend can update its cached ref
    if let Err(e) = app.emit("default-model-changed", payload) {
        warn!("Failed to emit default-model-changed event: {}", e);
    }

    Ok(())
}

/// Set the default project path for new tasks.
#[tracing::instrument(skip(state, app))]
#[tauri::command]
pub async fn set_default_project_path(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    path: String,
) -> Result<(), IpcError> {
    let path = path.trim().to_string();
    info!(path = %path, "set_default_project_path invoked");

    // Empty string clears the setting; any non-empty value must point to an
    // existing directory after canonicalization. We store the canonical form
    // so downstream consumers don't have to re-resolve symlinks/`~`.
    let canonical_path: Option<String> = if path.is_empty() {
        None
    } else {
        let canonical = validate_working_dir(&path)?;
        Some(canonical.display().to_string())
    };

    let (data_dir, bytes, payload) = {
        let mut config = state.config.write().await;
        config.default_project_path = canonical_path.clone();
        // Audit 1.11: implicitly approve the chosen default project path. The
        // user picked it deliberately via a native folder dialog, so requiring
        // a second confirmation modal would be UX redundancy.
        if let Some(ref p) = canonical_path {
            let buf = PathBuf::from(p);
            config.add_approved_working_dir(buf);
        }
        let bytes = config
            .snapshot_to_bytes()
            .map_err(|e| format!("failed to serialize config: {}", e))?;
        let payload = serde_json::json!({ "path": config.default_project_path });
        (config.data_dir.clone(), bytes, payload)
    };
    Config::write_bytes(data_dir, bytes)
        .await
        .map_err(|e| format!("failed to save config: {}", e))?;

    // Emit event so the frontend can update its cached ref
    if let Err(e) = app.emit("default-project-path-changed", payload) {
        warn!("Failed to emit default-project-path-changed event: {}", e);
    }

    Ok(())
}

/// Request user-approval status for a working directory (audit 1.11).
///
/// The frontend calls this after a user picks a new directory that isn't
/// already in `Config::approved_working_dirs` and clicks "Allow" on the
/// approval modal. Validates the path (trim/expand/canonicalize), then adds
/// the canonical form to the allowlist. Returns `Ok(true)` if the path was
/// added (or already present), `Ok(false)` is reserved for a future
/// backend-driven decline path. Errors propagate validation failures (empty
/// path, non-existent directory, etc.) so the UI can surface them.
#[tracing::instrument(skip(state))]
#[tauri::command]
pub async fn request_working_dir_approval(
    state: tauri::State<'_, AppState>,
    path: String,
) -> Result<bool, IpcError> {
    info!(path = %path, "request_working_dir_approval invoked");
    let canonical = crate::commands::util::canonicalize_working_dir(&path)?;

    let (data_dir, bytes, added, approved_count) = {
        let mut config = state.config.write().await;
        let added = config.add_approved_working_dir(canonical.clone());
        let bytes = config
            .snapshot_to_bytes()
            .map_err(|e| format!("failed to serialize config: {}", e))?;
        let approved_count = config.approved_working_dirs.len();
        (config.data_dir.clone(), bytes, added, approved_count)
    };
    Config::write_bytes(data_dir, bytes)
        .await
        .map_err(|e| format!("failed to save config: {}", e))?;
    info!(
        path = %canonical.display(),
        newly_added = added,
        approved_count = approved_count,
        "working directory approval recorded"
    );
    // Returning `true` even when the entry was a no-op duplicate keeps the
    // frontend logic simple: any non-error response means "you may proceed".
    Ok(true)
}

/// List all known providers with their configuration status and model count.
#[tracing::instrument(skip(state))]
#[tauri::command]
pub async fn get_providers(
    state: tauri::State<'_, AppState>,
) -> Result<Vec<ProviderInfo>, IpcError> {
    info!("get_providers invoked");

    let config = state.config.read().await;
    let sub_auth = crate::state::config::check_pi_subscription_auth();

    let mut providers: Vec<ProviderInfo> = config
        .providers
        .iter()
        .map(|(id, pc)| {
            let is_configured = if pc.provider_type == "Subscription" {
                match id.as_str() {
                    "chatgpt" => sub_auth.chatgpt,
                    "claude-sub" => sub_auth.claude,
                    _ => false,
                }
            } else {
                config.provider_keys.contains_key(id)
            };
            ProviderInfo {
                name: id.clone(),
                display_name: pc.display_name.clone(),
                provider_type: pc.provider_type.clone(),
                endpoint: pc.endpoint.clone(),
                configured: is_configured,
                model_count: 0,
                health: if is_configured { Some(true) } else { None },
            }
        })
        .filter(|p| p.provider_type != "CLI")
        .collect();

    // Stable sort: configured first, then alphabetical by display name
    providers.sort_by(|a, b| {
        b.configured
            .cmp(&a.configured)
            .then_with(|| a.display_name.cmp(&b.display_name))
    });

    Ok(providers)
}

/// Set the default hivemind for new tasks.
#[tracing::instrument(skip(state, app))]
#[tauri::command]
pub async fn set_default_hivemind(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    hivemind_id: String,
) -> Result<(), IpcError> {
    let hivemind_id_value = hivemind_id.trim().to_string();
    info!(hivemind_id = %hivemind_id_value, "set_default_hivemind invoked");

    // Allow empty (means "clear default"); otherwise enforce the same allowlist
    // every other hivemind_id IPC uses, so a malformed id can't slip into the
    // persisted config and corrupt later path joins in hivemind workflows.
    if !hivemind_id_value.is_empty() {
        validate_id(&hivemind_id_value)?;
    }

    let (data_dir, bytes, payload) = {
        let mut config = state.config.write().await;
        config.default_hivemind = if hivemind_id_value.is_empty() {
            None
        } else {
            Some(hivemind_id_value)
        };
        let bytes = config
            .snapshot_to_bytes()
            .map_err(|e| format!("failed to serialize config: {}", e))?;
        let payload = serde_json::json!({ "hivemind": config.default_hivemind });
        (config.data_dir.clone(), bytes, payload)
    };
    Config::write_bytes(data_dir, bytes)
        .await
        .map_err(|e| format!("failed to save config: {}", e))?;

    // Emit event so the frontend can update its cached ref
    if let Err(e) = app.emit("default-hivemind-changed", payload) {
        warn!("Failed to emit default-hivemind-changed event: {}", e);
    }

    Ok(())
}

/// Check subscription auth status for ChatGPT and Claude subscription providers.
#[tracing::instrument]
#[tauri::command]
pub async fn check_subscription_auth(
) -> Result<crate::state::config::SubscriptionAuthStatus, IpcError> {
    info!("check_subscription_auth invoked");
    Ok(crate::state::config::check_pi_subscription_auth())
}

/// Add a new provider entry to the config.
#[tracing::instrument(skip(state, app))]
#[tauri::command]
pub async fn add_provider(
    state: tauri::State<'_, AppState>,
    app: tauri::AppHandle,
    id: String,
    display_name: String,
    provider_type: Option<String>,
    endpoint: Option<String>,
) -> Result<(), IpcError> {
    let id = id.trim().to_lowercase();
    let display_name = display_name.trim().to_string();
    info!(id = %id, "add_provider invoked");

    if id.is_empty() || display_name.is_empty() {
        return Err(IpcError::validation(
            "provider id and display name cannot be empty".to_string(),
        ));
    }
    validate_id(&id)?;

    // Validate provider_type against allowlist (default to OpenAI Compatible).
    let provider_type = provider_type
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "OpenAI Compatible".to_string());
    if !ALLOWED_PROVIDER_TYPES.contains(&provider_type.as_str()) {
        return Err(IpcError::internal(format!(
            "unknown provider_type '{}'. Must be one of: {}",
            provider_type,
            ALLOWED_PROVIDER_TYPES.join(", ")
        )));
    }

    // Validate endpoint URL when present. Empty/None is allowed for provider
    // types like Subscription/CLI that don't speak HTTP directly.
    let endpoint = endpoint
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    if let Some(ref ep) = endpoint {
        validate_endpoint(ep)?;
    }

    // Snapshot under the write guard, then drop the guard BEFORE the
    // synchronous config.json atomic-rename so we don't hold up other
    // config readers app-wide on disk I/O.
    let (config_snapshot, env) = {
        let mut config = state.config.write().await;
        config.providers.insert(
            id.clone(),
            crate::state::config::ProviderConfig {
                display_name,
                provider_type,
                endpoint,
                has_key: false,
            },
        );
        let env = config.pi_env_vars();
        let config_snapshot = config.clone();
        (config_snapshot, env)
    }; // guard drops here

    tokio::task::spawn_blocking(move || -> Result<(), String> {
        config_snapshot
            .save_blocking()
            .map_err(|e| format!("failed to save config: {}", e))
    })
    .await
    .map_err(|e| format!("spawn_blocking join error: {}", e))??;

    // Propagate updated provider config (endpoints + manifest) to PiManager
    // so future Pi sessions receive the new provider definitions.
    state.pi_manager.update_env_vars(env).await;

    // Rebuild the hivemind provider registry so the newly-added provider
    // is visible to subsequent reviews/nurse calls. Note: a provider
    // added without an API key still won't be registered (the OpenAI
    // Compatible branch skips keyless non-localhost endpoints), but
    // refreshing now means a subsequent `save_api_key` flow will Just
    // Work without restart.
    state.refresh_provider_registry().await;
    state.refresh_extension_registry(&app).await;

    info!(id = %id, "provider added successfully");
    Ok(())
}

/// Refresh the model list, optionally for a specific provider.
///
/// Returns the full list of available models across all configured
/// providers (or just the specified one).
#[tracing::instrument(skip(_state))]
#[tauri::command]
pub async fn refresh_models(
    _state: tauri::State<'_, AppState>,
    provider: Option<String>,
) -> Result<Vec<ModelInfoResponse>, IpcError> {
    info!(provider = ?provider, "refresh_models invoked");

    Ok(get_model_catalog(&provider))
}

/// Build a model catalog from built-in model definitions.
///
/// In production, this would query provider APIs for live model lists.
fn get_model_catalog(provider_filter: &Option<String>) -> Vec<ModelInfoResponse> {
    let all_models = vec![
        ModelInfoResponse {
            provider: "anthropic".to_string(),
            model_id: "claude-sonnet-4-20250514".to_string(),
            context_window: 200_000,
            cost_per_1m_input: 3.0,
            cost_per_1m_output: 15.0,
        },
        ModelInfoResponse {
            provider: "anthropic".to_string(),
            model_id: "claude-opus-4-20250514".to_string(),
            context_window: 200_000,
            cost_per_1m_input: 15.0,
            cost_per_1m_output: 75.0,
        },
        ModelInfoResponse {
            provider: "anthropic".to_string(),
            model_id: "claude-haiku-4-20250514".to_string(),
            context_window: 200_000,
            cost_per_1m_input: 0.80,
            cost_per_1m_output: 4.0,
        },
        ModelInfoResponse {
            provider: "anthropic".to_string(),
            model_id: "claude-3-5-sonnet-20241022".to_string(),
            context_window: 200_000,
            cost_per_1m_input: 3.0,
            cost_per_1m_output: 15.0,
        },
        ModelInfoResponse {
            provider: "anthropic".to_string(),
            model_id: "claude-3-5-haiku-20241022".to_string(),
            context_window: 200_000,
            cost_per_1m_input: 0.80,
            cost_per_1m_output: 4.0,
        },
        ModelInfoResponse {
            provider: "openai".to_string(),
            model_id: "gpt-4o".to_string(),
            context_window: 128_000,
            cost_per_1m_input: 2.5,
            cost_per_1m_output: 10.0,
        },
        ModelInfoResponse {
            provider: "openai".to_string(),
            model_id: "gpt-4o-mini".to_string(),
            context_window: 128_000,
            cost_per_1m_input: 0.15,
            cost_per_1m_output: 0.60,
        },
        ModelInfoResponse {
            provider: "openai".to_string(),
            model_id: "o3-mini".to_string(),
            context_window: 200_000,
            cost_per_1m_input: 1.1,
            cost_per_1m_output: 4.4,
        },
        ModelInfoResponse {
            provider: "openai".to_string(),
            model_id: "o1".to_string(),
            context_window: 200_000,
            cost_per_1m_input: 15.0,
            cost_per_1m_output: 60.0,
        },
        ModelInfoResponse {
            provider: "openai".to_string(),
            model_id: "gpt-5".to_string(),
            context_window: 272_000,
            cost_per_1m_input: 0.0,
            cost_per_1m_output: 0.0,
        },
        ModelInfoResponse {
            provider: "openai".to_string(),
            model_id: "gpt-5-mini".to_string(),
            context_window: 272_000,
            cost_per_1m_input: 0.0,
            cost_per_1m_output: 0.0,
        },
        ModelInfoResponse {
            provider: "openai".to_string(),
            model_id: "gpt-4.1".to_string(),
            context_window: 1_000_000,
            cost_per_1m_input: 0.0,
            cost_per_1m_output: 0.0,
        },
        // DeepSeek (OpenAI Compatible)
        ModelInfoResponse {
            provider: "deepseek".to_string(),
            model_id: "deepseek-chat".to_string(),
            context_window: 128_000,
            cost_per_1m_input: 0.0,
            cost_per_1m_output: 0.0,
        },
        ModelInfoResponse {
            provider: "deepseek".to_string(),
            model_id: "deepseek-reasoner".to_string(),
            context_window: 128_000,
            cost_per_1m_input: 0.0,
            cost_per_1m_output: 0.0,
        },
        ModelInfoResponse {
            provider: "deepseek".to_string(),
            model_id: "deepseek-coder".to_string(),
            context_window: 128_000,
            cost_per_1m_input: 0.0,
            cost_per_1m_output: 0.0,
        },
        // Mistral
        ModelInfoResponse {
            provider: "mistral".to_string(),
            model_id: "mistral-large-latest".to_string(),
            context_window: 128_000,
            cost_per_1m_input: 0.0,
            cost_per_1m_output: 0.0,
        },
        ModelInfoResponse {
            provider: "mistral".to_string(),
            model_id: "mistral-small-latest".to_string(),
            context_window: 128_000,
            cost_per_1m_input: 0.0,
            cost_per_1m_output: 0.0,
        },
        // GLM (Zhipu)
        ModelInfoResponse {
            provider: "glm".to_string(),
            model_id: "glm-4-plus".to_string(),
            context_window: 128_000,
            cost_per_1m_input: 0.0,
            cost_per_1m_output: 0.0,
        },
        ModelInfoResponse {
            provider: "glm".to_string(),
            model_id: "glm-4.6".to_string(),
            context_window: 128_000,
            cost_per_1m_input: 0.0,
            cost_per_1m_output: 0.0,
        },
        // Groq
        ModelInfoResponse {
            provider: "groq".to_string(),
            model_id: "llama-3.3-70b-versatile".to_string(),
            context_window: 128_000,
            cost_per_1m_input: 0.0,
            cost_per_1m_output: 0.0,
        },
        ModelInfoResponse {
            provider: "groq".to_string(),
            model_id: "llama-3.1-8b-instant".to_string(),
            context_window: 128_000,
            cost_per_1m_input: 0.0,
            cost_per_1m_output: 0.0,
        },
        // Moonshot / Kimi
        ModelInfoResponse {
            provider: "kimi".to_string(),
            model_id: "kimi-k2-instruct".to_string(),
            context_window: 256_000,
            cost_per_1m_input: 0.0,
            cost_per_1m_output: 0.0,
        },
        ModelInfoResponse {
            provider: "moonshot".to_string(),
            model_id: "moonshot-v1-128k".to_string(),
            context_window: 128_000,
            cost_per_1m_input: 0.0,
            cost_per_1m_output: 0.0,
        },
        // ChatGPT subscription models (via OpenAI Codex — $0 with subscription)
        ModelInfoResponse {
            provider: "chatgpt".to_string(),
            model_id: "gpt-5.5".to_string(),
            context_window: 272_000,
            cost_per_1m_input: 0.0,
            cost_per_1m_output: 0.0,
        },
        ModelInfoResponse {
            provider: "chatgpt".to_string(),
            model_id: "gpt-5.4".to_string(),
            context_window: 272_000,
            cost_per_1m_input: 0.0,
            cost_per_1m_output: 0.0,
        },
        ModelInfoResponse {
            provider: "chatgpt".to_string(),
            model_id: "gpt-5.4-mini".to_string(),
            context_window: 272_000,
            cost_per_1m_input: 0.0,
            cost_per_1m_output: 0.0,
        },
        ModelInfoResponse {
            provider: "chatgpt".to_string(),
            model_id: "gpt-5.3-codex".to_string(),
            context_window: 272_000,
            cost_per_1m_input: 0.0,
            cost_per_1m_output: 0.0,
        },
        ModelInfoResponse {
            provider: "chatgpt".to_string(),
            model_id: "gpt-5.3-codex-spark".to_string(),
            context_window: 128_000,
            cost_per_1m_input: 0.0,
            cost_per_1m_output: 0.0,
        },
        ModelInfoResponse {
            provider: "chatgpt".to_string(),
            model_id: "gpt-5.2".to_string(),
            context_window: 272_000,
            cost_per_1m_input: 0.0,
            cost_per_1m_output: 0.0,
        },
        ModelInfoResponse {
            provider: "chatgpt".to_string(),
            model_id: "gpt-5.2-codex".to_string(),
            context_window: 272_000,
            cost_per_1m_input: 0.0,
            cost_per_1m_output: 0.0,
        },
        ModelInfoResponse {
            provider: "chatgpt".to_string(),
            model_id: "gpt-5.1".to_string(),
            context_window: 272_000,
            cost_per_1m_input: 0.0,
            cost_per_1m_output: 0.0,
        },
        ModelInfoResponse {
            provider: "chatgpt".to_string(),
            model_id: "gpt-5.1-codex-max".to_string(),
            context_window: 272_000,
            cost_per_1m_input: 0.0,
            cost_per_1m_output: 0.0,
        },
        ModelInfoResponse {
            provider: "chatgpt".to_string(),
            model_id: "gpt-5.1-codex-mini".to_string(),
            context_window: 272_000,
            cost_per_1m_input: 0.0,
            cost_per_1m_output: 0.0,
        },
        // Claude subscription models ($0 with subscription)
        ModelInfoResponse {
            provider: "claude-sub".to_string(),
            model_id: "claude-opus-4-7".to_string(),
            context_window: 1_000_000,
            cost_per_1m_input: 0.0,
            cost_per_1m_output: 0.0,
        },
        ModelInfoResponse {
            provider: "claude-sub".to_string(),
            model_id: "claude-opus-4-6".to_string(),
            context_window: 1_000_000,
            cost_per_1m_input: 0.0,
            cost_per_1m_output: 0.0,
        },
        ModelInfoResponse {
            provider: "claude-sub".to_string(),
            model_id: "claude-sonnet-4-6".to_string(),
            context_window: 1_000_000,
            cost_per_1m_input: 0.0,
            cost_per_1m_output: 0.0,
        },
        ModelInfoResponse {
            provider: "claude-sub".to_string(),
            model_id: "claude-opus-4-5".to_string(),
            context_window: 200_000,
            cost_per_1m_input: 0.0,
            cost_per_1m_output: 0.0,
        },
        ModelInfoResponse {
            provider: "claude-sub".to_string(),
            model_id: "claude-opus-4-5-20251101".to_string(),
            context_window: 200_000,
            cost_per_1m_input: 0.0,
            cost_per_1m_output: 0.0,
        },
        ModelInfoResponse {
            provider: "claude-sub".to_string(),
            model_id: "claude-sonnet-4-5".to_string(),
            context_window: 200_000,
            cost_per_1m_input: 0.0,
            cost_per_1m_output: 0.0,
        },
        ModelInfoResponse {
            provider: "claude-sub".to_string(),
            model_id: "claude-sonnet-4-5-20250929".to_string(),
            context_window: 200_000,
            cost_per_1m_input: 0.0,
            cost_per_1m_output: 0.0,
        },
        ModelInfoResponse {
            provider: "claude-sub".to_string(),
            model_id: "claude-opus-4-1".to_string(),
            context_window: 200_000,
            cost_per_1m_input: 0.0,
            cost_per_1m_output: 0.0,
        },
        ModelInfoResponse {
            provider: "claude-sub".to_string(),
            model_id: "claude-opus-4-1-20250805".to_string(),
            context_window: 200_000,
            cost_per_1m_input: 0.0,
            cost_per_1m_output: 0.0,
        },
        ModelInfoResponse {
            provider: "claude-sub".to_string(),
            model_id: "claude-opus-4-0".to_string(),
            context_window: 200_000,
            cost_per_1m_input: 0.0,
            cost_per_1m_output: 0.0,
        },
        ModelInfoResponse {
            provider: "claude-sub".to_string(),
            model_id: "claude-opus-4-20250514".to_string(),
            context_window: 200_000,
            cost_per_1m_input: 0.0,
            cost_per_1m_output: 0.0,
        },
        ModelInfoResponse {
            provider: "claude-sub".to_string(),
            model_id: "claude-sonnet-4-0".to_string(),
            context_window: 200_000,
            cost_per_1m_input: 0.0,
            cost_per_1m_output: 0.0,
        },
        ModelInfoResponse {
            provider: "claude-sub".to_string(),
            model_id: "claude-sonnet-4-20250514".to_string(),
            context_window: 200_000,
            cost_per_1m_input: 0.0,
            cost_per_1m_output: 0.0,
        },
        ModelInfoResponse {
            provider: "claude-sub".to_string(),
            model_id: "claude-haiku-4-5".to_string(),
            context_window: 200_000,
            cost_per_1m_input: 0.0,
            cost_per_1m_output: 0.0,
        },
        ModelInfoResponse {
            provider: "claude-sub".to_string(),
            model_id: "claude-haiku-4-5-20251001".to_string(),
            context_window: 200_000,
            cost_per_1m_input: 0.0,
            cost_per_1m_output: 0.0,
        },
        ModelInfoResponse {
            provider: "claude-sub".to_string(),
            model_id: "claude-3-7-sonnet-20250219".to_string(),
            context_window: 200_000,
            cost_per_1m_input: 0.0,
            cost_per_1m_output: 0.0,
        },
        ModelInfoResponse {
            provider: "claude-sub".to_string(),
            model_id: "claude-3-5-sonnet-20241022".to_string(),
            context_window: 200_000,
            cost_per_1m_input: 0.0,
            cost_per_1m_output: 0.0,
        },
        ModelInfoResponse {
            provider: "claude-sub".to_string(),
            model_id: "claude-3-5-haiku-20241022".to_string(),
            context_window: 200_000,
            cost_per_1m_input: 0.0,
            cost_per_1m_output: 0.0,
        },
        ModelInfoResponse {
            provider: "claude-sub".to_string(),
            model_id: "claude-3-opus-20240229".to_string(),
            context_window: 200_000,
            cost_per_1m_input: 0.0,
            cost_per_1m_output: 0.0,
        },
        // CroF
        ModelInfoResponse {
            provider: "crof".to_string(),
            model_id: "mimo-v2.5-pro-precision".to_string(),
            context_window: 128_000,
            cost_per_1m_input: 0.0,
            cost_per_1m_output: 0.0,
        },
        ModelInfoResponse {
            provider: "crof".to_string(),
            model_id: "crof-gpt-4o".to_string(),
            context_window: 128_000,
            cost_per_1m_input: 0.0,
            cost_per_1m_output: 0.0,
        },
        ModelInfoResponse {
            provider: "crof".to_string(),
            model_id: "crof-gpt-4o-mini".to_string(),
            context_window: 128_000,
            cost_per_1m_input: 0.0,
            cost_per_1m_output: 0.0,
        },
    ];

    match provider_filter {
        Some(p) => all_models
            .into_iter()
            .filter(|m| m.provider == *p)
            .collect(),
        None => all_models,
    }
}

/* ── Provider testing ──────────────────────────────────────── */

/// Extended model metadata (populated when the provider returns it).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelDetail {
    pub id: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub context_length: Option<u64>,
    #[serde(default)]
    pub max_output: Option<u64>,
    #[serde(default)]
    pub input_price: Option<f64>,
    #[serde(default)]
    pub output_price: Option<f64>,
}

/// Result of querying a provider's model list.
#[derive(Debug, Clone, Serialize)]
pub struct TestModelsResult {
    pub ok: bool,
    pub models: Vec<String>,
    /// Rich model metadata (only populated by providers that return it, e.g. OpenRouter).
    #[serde(default)]
    pub details: Vec<ModelDetail>,
    pub error: Option<String>,
}

/// Result of sending a test chat message.
#[derive(Debug, Clone, Serialize)]
pub struct TestChatResult {
    pub ok: bool,
    pub model: String,
    pub reply_preview: Option<String>,
    pub error: Option<String>,
}

/// Result of testing a provider through Pi RPC.
#[derive(Debug, Clone, Serialize)]
pub struct TestPiResult {
    pub ok: bool,
    pub model: String,
    pub reply_preview: Option<String>,
    pub error: Option<String>,
}

fn humanize_pi_test_error(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return "Pi test failed without an error message".to_string();
    }
    for prefix in [
        "failed to spawn Pi session:",
        "failed to send prompt:",
        "pi process is not available:",
    ] {
        if let Some(rest) = trimmed.strip_prefix(prefix) {
            return humanize_pi_test_error(rest);
        }
    }
    if let Some(idx) = trimmed.find("Error: Model ") {
        return trimmed[idx..].trim().to_string();
    }
    trimmed.to_string()
}

/// OpenAI-style /v1/models response (minimal — safety-net fallback parser).
#[derive(Deserialize)]
struct OaiModelsResponse {
    data: Vec<OaiModel>,
}

#[derive(Deserialize)]
struct OaiModel {
    id: String,
}

/// Rich OpenAI-compatible /v1/models response. Many providers extend the
/// canonical shape with `context_length`, `max_completion_tokens`, pricing,
/// etc. — we accept any of the common spellings via `serde(alias)`.
#[derive(Deserialize)]
struct OaiRichModelsResponse {
    data: Vec<OaiModelRich>,
}

#[derive(Deserialize)]
struct OaiModelRich {
    id: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(
        default,
        alias = "context_window",
        alias = "max_context_length",
        alias = "max_model_len"
    )]
    context_length: Option<u64>,
    #[serde(
        default,
        alias = "max_output_tokens",
        alias = "max_tokens",
        alias = "max_completion_tokens"
    )]
    max_output: Option<u64>,
    #[serde(default)]
    top_provider: Option<OpenRouterTopProvider>,
    #[serde(default)]
    pricing: Option<OpenRouterPricing>,
}

/// OpenRouter /v1/models response (richer)
#[derive(Deserialize)]
struct OpenRouterModelsResponse {
    data: Vec<OpenRouterModel>,
}

#[derive(Deserialize)]
struct OpenRouterModel {
    id: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    context_length: Option<u64>,
    #[serde(default)]
    top_provider: Option<OpenRouterTopProvider>,
    #[serde(default)]
    pricing: Option<OpenRouterPricing>,
}

#[derive(Deserialize)]
struct OpenRouterTopProvider {
    #[serde(default)]
    max_completion_tokens: Option<u64>,
}

#[derive(Deserialize)]
struct OpenRouterPricing {
    #[serde(default)]
    prompt: Option<String>,
    #[serde(default)]
    completion: Option<String>,
}

/// Merge a parsed `Vec<ModelDetail>` with the static catalog so callers see
/// metadata for models even when the provider's `/v1/models` is bare. For each
/// `model_id`, missing fields are filled from the catalog entry for
/// `(provider, model_id)`. Models present in `models` but absent from `details`
/// gain an entry. Returns a fresh `Vec<ModelDetail>` aligned 1:1 with `models`.
fn enrich_details_with_catalog(
    provider: &str,
    models: &[String],
    parsed: Vec<ModelDetail>,
) -> Vec<ModelDetail> {
    let mut by_id: std::collections::HashMap<String, ModelDetail> =
        parsed.into_iter().map(|d| (d.id.clone(), d)).collect();

    let prov_filter = Some(provider.to_string());
    let catalog = get_model_catalog(&prov_filter);
    let catalog_by_id: std::collections::HashMap<&str, &ModelInfoResponse> =
        catalog.iter().map(|m| (m.model_id.as_str(), m)).collect();

    let mut out = Vec::with_capacity(models.len());
    for id in models {
        let mut detail = by_id.remove(id).unwrap_or_else(|| ModelDetail {
            id: id.clone(),
            name: None,
            context_length: None,
            max_output: None,
            input_price: None,
            output_price: None,
        });
        if let Some(meta) = catalog_by_id.get(id.as_str()) {
            if detail.context_length.is_none() && meta.context_window > 0 {
                detail.context_length = Some(meta.context_window as u64);
            }
            if detail.input_price.is_none() && meta.cost_per_1m_input > 0.0 {
                detail.input_price = Some(meta.cost_per_1m_input);
            }
            if detail.output_price.is_none() && meta.cost_per_1m_output > 0.0 {
                detail.output_price = Some(meta.cost_per_1m_output);
            }
        }
        out.push(detail);
    }
    out
}

/// Deduplicate `models` (and the parallel `details` vector when present),
/// keeping the first occurrence of each id. Preserves the provider's original
/// ordering — important because curated lists (e.g. OpenAI returns newest
/// first) and downstream tests that pick a "random model" rely on order
/// stability.
///
/// Behaviour:
/// - `details.is_empty()` — dedup `models` only; return `details` unchanged.
/// - `details.len() == models.len()` — walk `models` once, keep first index
///   per unique id, project both vectors through the same kept indices so the
///   1:1 alignment promised by `enrich_details_with_catalog` is preserved.
/// - Lengths differ AND `details` non-empty — unexpected with current
///   parsers. Warn and fall back to deduping `models` only (`details`
///   unchanged) to avoid panics while surfacing the anomaly.
///
/// Motivated by NVIDIA NIM's `/v1/models` returning duplicate `id` entries
/// for the same model, which collide on React `key` and inflate the visible
/// count downstream.
fn dedup_models_and_details(
    models: Vec<String>,
    details: Vec<ModelDetail>,
) -> (Vec<String>, Vec<ModelDetail>) {
    use std::collections::HashSet;

    if details.is_empty() {
        let mut seen: HashSet<String> = HashSet::with_capacity(models.len());
        let mut out_models: Vec<String> = Vec::with_capacity(models.len());
        for id in models {
            if seen.insert(id.clone()) {
                out_models.push(id);
            }
        }
        return (out_models, details);
    }

    if details.len() == models.len() {
        let mut seen: HashSet<String> = HashSet::with_capacity(models.len());
        let mut kept: Vec<usize> = Vec::with_capacity(models.len());
        for (i, id) in models.iter().enumerate() {
            if seen.insert(id.clone()) {
                kept.push(i);
            }
        }
        let mut out_models: Vec<String> = Vec::with_capacity(kept.len());
        let mut out_details: Vec<ModelDetail> = Vec::with_capacity(kept.len());
        // Project both vectors through `kept`. We index by walking once and
        // consuming the originals to avoid cloning ModelDetail entries.
        let mut kept_iter = kept.into_iter().peekable();
        for (i, (m, d)) in models.into_iter().zip(details.into_iter()).enumerate() {
            match kept_iter.peek() {
                Some(&next) if next == i => {
                    out_models.push(m);
                    out_details.push(d);
                    kept_iter.next();
                }
                _ => { /* drop the duplicate */ }
            }
        }
        return (out_models, out_details);
    }

    // Unexpected: non-empty details with a length mismatch. Fall back to the
    // safe path so we don't panic, but log it so a future provider quirk is
    // visible in the logs.
    warn!(
        models_len = models.len(),
        details_len = details.len(),
        "dedup_models_and_details: unexpected length mismatch between models and details; deduping models only"
    );
    let mut seen: HashSet<String> = HashSet::with_capacity(models.len());
    let mut out_models: Vec<String> = Vec::with_capacity(models.len());
    for id in models {
        if seen.insert(id.clone()) {
            out_models.push(id);
        }
    }
    (out_models, details)
}

/// Returns true if at least one detail entry carries any rich metadata. Used
/// to decide whether to surface the rich-columns table on the frontend.
fn details_have_any_data(details: &[ModelDetail]) -> bool {
    details.iter().any(|d| {
        d.context_length.is_some()
            || d.max_output.is_some()
            || d.input_price.is_some()
            || d.output_price.is_some()
    })
}

/// OpenAI-style /v1/chat/completions response
#[derive(Deserialize)]
struct OaiChatResponse {
    choices: Vec<OaiChoice>,
}

#[derive(Deserialize)]
struct OaiChoice {
    message: OaiMessage,
}

#[derive(Deserialize)]
struct OaiMessage {
    content: Option<String>,
}

/// Anthropic /v1/models response
#[derive(Deserialize)]
struct AnthropicModelsResponse {
    data: Vec<AnthropicModel>,
}

#[derive(Deserialize)]
struct AnthropicModel {
    id: String,
}

/// Anthropic /v1/messages response
#[derive(Deserialize)]
struct AnthropicMessagesResponse {
    content: Vec<AnthropicContent>,
}

#[derive(Deserialize)]
struct AnthropicContent {
    text: Option<String>,
}

/// Test a provider's model list endpoint.
#[tracing::instrument(skip(state))]
#[tauri::command]
pub async fn test_provider_models(
    state: tauri::State<'_, AppState>,
    provider: String,
) -> Result<TestModelsResult, IpcError> {
    let provider = provider.trim().to_lowercase();
    info!(provider = %provider, "test_provider_models invoked");

    let config = state.config.read().await;
    let api_key = config
        .provider_keys
        .get(&provider)
        .cloned()
        .unwrap_or_default();
    let pc = config.providers.get(&provider).ok_or("unknown provider")?;
    let endpoint = pc.endpoint.clone().unwrap_or_default();
    let provider_type = pc.provider_type.clone();
    drop(config);

    if endpoint.is_empty() {
        return Ok(TestModelsResult {
            ok: false,
            models: vec![],
            details: vec![],
            error: Some("no endpoint configured".into()),
        });
    }
    if api_key.is_empty() && provider_type != "OpenAI Compatible"
        || api_key.is_empty() && !endpoint.contains("localhost")
    {
        // Ollama on localhost doesn't need a key
        if !endpoint.contains("localhost") {
            return Ok(TestModelsResult {
                ok: false,
                models: vec![],
                details: vec![],
                error: Some("no API key configured".into()),
            });
        }
    }

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .map_err(|e| e.to_string())?;

    if provider_type == "Anthropic" {
        // Anthropic uses x-api-key header and anthropic-version header
        let url = format!("{}/models", endpoint.trim_end_matches('/'));
        match client
            .get(&url)
            .header("x-api-key", &api_key)
            .header("anthropic-version", "2023-06-01")
            .send()
            .await
        {
            Ok(resp) if resp.status().is_success() => {
                let body = resp.text().await.unwrap_or_default();
                match serde_json::from_str::<AnthropicModelsResponse>(&body) {
                    Ok(parsed) => {
                        let models: Vec<String> = parsed.data.into_iter().map(|m| m.id).collect();
                        let (models, details) = dedup_models_and_details(models, vec![]);
                        Ok(TestModelsResult {
                            ok: true,
                            models,
                            details,
                            error: None,
                        })
                    }
                    Err(e) => Ok(TestModelsResult {
                        ok: false,
                        models: vec![],
                        details: vec![],
                        error: Some(format!("parse error: {}", e)),
                    }),
                }
            }
            Ok(resp) => {
                let status = resp.status();
                let body = resp.text().await.unwrap_or_default();
                warn!(provider = %provider, %status, "models query failed");
                Ok(TestModelsResult {
                    ok: false,
                    models: vec![],
                    details: vec![],
                    error: Some(format!(
                        "{}: {}",
                        status,
                        body.chars().take(200).collect::<String>()
                    )),
                })
            }
            Err(e) => Ok(TestModelsResult {
                ok: false,
                models: vec![],
                details: vec![],
                error: Some(e.to_string()),
            }),
        }
    } else {
        // OpenAI Compatible: GET /models with Bearer token
        let url = format!("{}/models", endpoint.trim_end_matches('/'));
        let mut req = client.get(&url);
        if !api_key.is_empty() {
            req = req.bearer_auth(&api_key);
        }
        match req.send().await {
            Ok(resp) if resp.status().is_success() => {
                let body = resp.text().await.unwrap_or_default();
                // Only parse OpenRouter's richer format for the openrouter provider
                if provider == "openrouter" {
                    if let Ok(parsed) = serde_json::from_str::<OpenRouterModelsResponse>(&body) {
                        let models: Vec<String> =
                            parsed.data.iter().map(|m| m.id.clone()).collect();
                        let details: Vec<ModelDetail> = parsed
                            .data
                            .into_iter()
                            .map(|m| {
                                let input_price = m
                                    .pricing
                                    .as_ref()
                                    .and_then(|p| p.prompt.as_ref())
                                    .and_then(|s| s.parse::<f64>().ok())
                                    .map(|per_tok| per_tok * 1_000_000.0);
                                let output_price = m
                                    .pricing
                                    .as_ref()
                                    .and_then(|p| p.completion.as_ref())
                                    .and_then(|s| s.parse::<f64>().ok())
                                    .map(|per_tok| per_tok * 1_000_000.0);
                                let max_output =
                                    m.top_provider.and_then(|tp| tp.max_completion_tokens);
                                ModelDetail {
                                    id: m.id,
                                    name: m.name,
                                    context_length: m.context_length,
                                    max_output,
                                    input_price,
                                    output_price,
                                }
                            })
                            .collect();
                        // dedup before enrich — both vectors are still 1:1 aligned at this point
                        let (models, details) = dedup_models_and_details(models, details);
                        let details = enrich_details_with_catalog(&provider, &models, details);
                        return Ok(TestModelsResult {
                            ok: true,
                            models,
                            details,
                            error: None,
                        });
                    }
                }
                // Generic OpenAI-compatible: try rich parser first, fall back to bare
                // `{ id }`-only shape so we never fail the whole list just because
                // the provider's payload doesn't carry rich fields.
                if let Ok(parsed) = serde_json::from_str::<OaiRichModelsResponse>(&body) {
                    let models: Vec<String> = parsed.data.iter().map(|m| m.id.clone()).collect();
                    let parsed_details: Vec<ModelDetail> = parsed
                        .data
                        .into_iter()
                        .map(|m| {
                            let input_price = m
                                .pricing
                                .as_ref()
                                .and_then(|p| p.prompt.as_ref())
                                .and_then(|s| s.parse::<f64>().ok())
                                .map(|per_tok| per_tok * 1_000_000.0);
                            let output_price = m
                                .pricing
                                .as_ref()
                                .and_then(|p| p.completion.as_ref())
                                .and_then(|s| s.parse::<f64>().ok())
                                .map(|per_tok| per_tok * 1_000_000.0);
                            let max_output = m
                                .max_output
                                .or_else(|| m.top_provider.and_then(|tp| tp.max_completion_tokens));
                            ModelDetail {
                                id: m.id,
                                name: m.name,
                                context_length: m.context_length,
                                max_output,
                                input_price,
                                output_price,
                            }
                        })
                        .collect();
                    // dedup before enrich — both vectors are still 1:1 aligned at this point
                    let (models, parsed_details) =
                        dedup_models_and_details(models, parsed_details);
                    let details = enrich_details_with_catalog(&provider, &models, parsed_details);
                    let details = if details_have_any_data(&details) {
                        details
                    } else {
                        vec![]
                    };
                    return Ok(TestModelsResult {
                        ok: true,
                        models,
                        details,
                        error: None,
                    });
                }
                match serde_json::from_str::<OaiModelsResponse>(&body) {
                    Ok(parsed) => {
                        let models: Vec<String> = parsed.data.into_iter().map(|m| m.id).collect();
                        let (models, _) = dedup_models_and_details(models, vec![]);
                        let details = enrich_details_with_catalog(&provider, &models, vec![]);
                        let details = if details_have_any_data(&details) {
                            details
                        } else {
                            vec![]
                        };
                        Ok(TestModelsResult {
                            ok: true,
                            models,
                            details,
                            error: None,
                        })
                    }
                    Err(e) => Ok(TestModelsResult {
                        ok: false,
                        models: vec![],
                        details: vec![],
                        error: Some(format!("parse error: {}", e)),
                    }),
                }
            }
            Ok(resp) => {
                let status = resp.status();
                let body = resp.text().await.unwrap_or_default();
                warn!(provider = %provider, %status, "models query failed");
                Ok(TestModelsResult {
                    ok: false,
                    models: vec![],
                    details: vec![],
                    error: Some(format!(
                        "{}: {}",
                        status,
                        body.chars().take(200).collect::<String>()
                    )),
                })
            }
            Err(e) => Ok(TestModelsResult {
                ok: false,
                models: vec![],
                details: vec![],
                error: Some(e.to_string()),
            }),
        }
    }
}

/* ── Pi status helpers ────────────────────────────────────── */

/// Extract a semver-like version string from text (e.g. "claude-code 1.0.34" → "1.0.34").
fn extract_version(text: &str) -> Option<String> {
    text.split_whitespace()
        .filter_map(|token| {
            let stripped = token.strip_prefix('v').unwrap_or(token);
            let parts: Vec<&str> = stripped.split('.').collect();
            if parts.len() >= 2 && parts.iter().all(|p| p.chars().all(|c| c.is_ascii_digit())) {
                Some(stripped.to_string())
            } else {
                None
            }
        })
        .next()
}

/// Parse a version string into (major, minor, patch) tuple.
fn parse_version_tuple(v: &str) -> Option<(u64, u64, u64)> {
    let parts: Vec<&str> = v.split('.').collect();
    if parts.len() < 2 {
        return None;
    }
    let major = parts[0].parse().ok()?;
    let minor = parts[1].parse().ok()?;
    let patch = parts.get(2).and_then(|p| p.parse().ok()).unwrap_or(0);
    Some((major, minor, patch))
}

/// Returns true if `installed` is strictly older than `latest`.
fn is_version_older(installed: &str, latest: &str) -> bool {
    match (parse_version_tuple(installed), parse_version_tuple(latest)) {
        (Some(i), Some(l)) => i < l,
        _ => false,
    }
}

/// Run the binary with `--version` and parse the output.
async fn get_installed_version(path: &std::path::Path) -> Option<String> {
    let output = tokio::process::Command::new(path)
        .arg("--version")
        .output()
        .await
        .ok()?;
    let text = String::from_utf8_lossy(&output.stdout).to_string();
    // Some CLIs print version on stderr
    let text = if text.trim().is_empty() {
        String::from_utf8_lossy(&output.stderr).to_string()
    } else {
        text
    };
    extract_version(&text)
}

/// Query npm registry for the latest version of a package.
async fn get_latest_npm_version(package: &str) -> Option<String> {
    #[derive(Deserialize)]
    struct NpmLatest {
        version: String,
    }

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .ok()?;

    let url = format!("https://registry.npmjs.org/{}/latest", package);
    let resp = client.get(&url).send().await.ok()?;

    if !resp.status().is_success() {
        return None;
    }

    let parsed: NpmLatest = resp.json().await.ok()?;
    Some(parsed.version)
}

/// Map a binary name to its npm package name. Returns None for unknown binaries.
fn npm_package_for_binary(binary_name: &str) -> Option<&'static str> {
    match binary_name {
        "pi" => Some("@earendil-works/pi-coding-agent"),
        _ => None,
    }
}

/// Extract the binary name (last path component, without extension) from a path.
fn binary_name_from_path(path: &std::path::Path) -> String {
    path.file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("unknown")
        .to_string()
}

/// Detect install method from the resolved (canonicalized) path.
fn detect_install_method(resolved: &str) -> PiInstallMethod {
    let lower = resolved.to_lowercase();
    if lower.contains("/lib/node_modules/") || lower.contains("/node_modules/.bin/") {
        PiInstallMethod::Npm
    } else if lower.contains("/cellar/") || lower.contains("/homebrew/") {
        PiInstallMethod::Homebrew
    } else {
        PiInstallMethod::Unknown
    }
}

/// Get the installation status of the Pi binary.
#[tracing::instrument(skip(state))]
#[tauri::command]
pub async fn get_pi_status(
    state: tauri::State<'_, AppState>,
) -> Result<PiStatusResponse, IpcError> {
    info!("get_pi_status invoked");

    let config = state.config.read().await;
    let binary_path = config.pi_binary_path.clone();
    drop(config);

    let Some(bin_path) = binary_path else {
        let pkg = npm_package_for_binary("pi").unwrap_or("<unknown>");
        return Ok(PiStatusResponse {
            installed: false,
            binary_path: None,
            resolved_path: None,
            binary_name: None,
            version: None,
            latest_version: None,
            is_outdated: false,
            install_method: PiInstallMethod::Unknown,
            error: Some(format!(
                "Pi binary not found on PATH. Install via: npm install -g {pkg}"
            )),
        });
    };

    let binary_str = bin_path.display().to_string();
    let bin_name = binary_name_from_path(&bin_path);

    // Resolve symlink. `canonicalize_clean` keeps Windows paths free of the
    // `\\?\` extended-length prefix so the value shown in Settings (and the
    // substring match performed by `detect_install_method`) sees a clean path.
    let resolved = crate::commands::util::canonicalize_clean(&bin_path)
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| binary_str.clone());

    let install_method = detect_install_method(&resolved);

    // Only query npm for latest version if we know the package name
    let npm_package = npm_package_for_binary(&bin_name);

    let (installed_version, latest_version) =
        tokio::join!(get_installed_version(&bin_path), async {
            match npm_package {
                Some(pkg) => get_latest_npm_version(pkg).await,
                None => None,
            }
        });

    let is_outdated = match (&installed_version, &latest_version) {
        (Some(i), Some(l)) => is_version_older(i, l),
        _ => false,
    };

    Ok(PiStatusResponse {
        installed: true,
        binary_path: Some(binary_str),
        resolved_path: Some(resolved),
        binary_name: Some(bin_name),
        version: installed_version,
        latest_version,
        is_outdated,
        install_method,
        error: None,
    })
}

/// Update the Pi binary via its detected package manager.
#[tracing::instrument(skip(app, state))]
#[tauri::command]
pub async fn update_pi(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
) -> Result<(), IpcError> {
    info!("update_pi invoked");

    let config = state.config.read().await;
    let binary_path = config.pi_binary_path.clone();
    drop(config);

    let bin_path = binary_path.ok_or("Pi binary not found")?;
    let bin_name = binary_name_from_path(&bin_path);

    let resolved = crate::commands::util::canonicalize_clean(&bin_path)
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| bin_path.display().to_string());

    let install_method = detect_install_method(&resolved);

    let npm_pkg = npm_package_for_binary(&bin_name)
        .ok_or_else(|| format!("Unknown binary '{}'. Please update manually.", bin_name))?;

    let (program, args): (&str, Vec<String>) = match install_method {
        PiInstallMethod::Npm => (
            "npm",
            vec!["install".into(), "-g".into(), format!("{}@latest", npm_pkg)],
        ),
        PiInstallMethod::Homebrew => ("brew", vec!["upgrade".into(), bin_name.clone()]),
        PiInstallMethod::Unknown => {
            return Err(IpcError::validation(
                "Cannot auto-update: unknown install method. Please update manually.",
            ));
        }
    };

    let emit = |event_type: &str, message: &str| {
        let _ = app.emit(
            "pi-update-progress",
            PiUpdateEvent {
                event_type: event_type.to_string(),
                message: message.to_string(),
            },
        );
    };

    emit(
        "started",
        &format!("Running: {} {}", program, args.join(" ")),
    );

    let output = tokio::process::Command::new(program)
        .args(&args)
        .output()
        .await
        .map_err(|e| IpcError::internal(format!("Failed to spawn update command: {}", e)))?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    if !stdout.is_empty() {
        emit("stdout", &stdout);
    }
    if !stderr.is_empty() {
        emit("stderr", &stderr);
    }

    if output.status.success() {
        // Refresh the binary path in config
        let mut config = state.config.write().await;
        config.refresh_pi_binary();
        drop(config);

        emit("completed", "Update completed successfully");
        Ok(())
    } else {
        let msg = format!("Update failed with exit code: {}", output.status);
        emit("failed", &msg);
        Err(IpcError::internal(msg))
    }
}

/// Install the Pi binary via npm.
///
/// Runs `npm install -g @earendil-works/pi-coding-agent`. If npm itself is
/// missing, returns a clear error pointing the user to nodejs.org rather
/// than trying to auto-install Node.
#[tracing::instrument(skip(app, state))]
#[tauri::command]
pub async fn install_pi(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
) -> Result<(), IpcError> {
    info!("install_pi invoked");

    let emit = |event_type: &str, message: &str| {
        let _ = app.emit(
            "pi-install-progress",
            PiUpdateEvent {
                event_type: event_type.to_string(),
                message: message.to_string(),
            },
        );
    };

    // Verify npm is on PATH first. Same pattern as resolve_pi_binary().
    let npm_check = tokio::process::Command::new("which")
        .arg("npm")
        .output()
        .await;
    let npm_missing = match npm_check {
        Ok(o) => !o.status.success() || o.stdout.is_empty(),
        Err(_) => true,
    };
    if npm_missing {
        let msg =
            "Node.js / npm not found. Install Node.js from https://nodejs.org/ and try again."
                .to_string();
        emit("failed", &msg);
        return Err(IpcError::validation(msg));
    }

    let pkg = npm_package_for_binary("pi").unwrap_or("@earendil-works/pi-coding-agent");
    let args = ["install".to_string(), "-g".to_string(), pkg.to_string()];

    emit("started", &format!("Running: npm {}", args.join(" ")));

    let output = tokio::process::Command::new("npm")
        .args(&args)
        .output()
        .await
        .map_err(|e| {
            let msg = format!("Failed to spawn npm: {}", e);
            emit("failed", &msg);
            IpcError::internal(msg)
        })?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    if !stdout.is_empty() {
        emit("stdout", &stdout);
    }
    if !stderr.is_empty() {
        emit("stderr", &stderr);
    }

    if output.status.success() {
        let mut config = state.config.write().await;
        config.refresh_pi_binary();
        drop(config);
        emit("completed", "Pi installed successfully");
        Ok(())
    } else {
        let msg = format!(
            "npm install failed (exit {}). If this is a permissions error, see https://docs.npmjs.com/resolving-eacces-permissions-errors-when-installing-packages-globally",
            output.status
        );
        emit("failed", &msg);
        Err(IpcError::internal(msg))
    }
}

/* ── Open Pi in a terminal ─────────────────────────────── */

/// Escape a string for safe interpolation inside an AppleScript double-quoted
/// string literal. Only `\` and `"` need escaping inside such a literal.
fn applescript_escape(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for ch in input.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            other => out.push(other),
        }
    }
    out
}

/// Wrap `input` in single quotes for safe interpolation inside a POSIX shell
/// command. Any literal `'` is escaped by closing the quoted run, emitting an
/// escaped quote, then reopening.
fn shell_single_quote(input: &str) -> String {
    let mut out = String::with_capacity(input.len() + 2);
    out.push('\'');
    for ch in input.chars() {
        if ch == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(ch);
        }
    }
    out.push('\'');
    out
}

/// Description of the terminal-launch command we constructed. Used by tests
/// so we can assert on the argv shape without actually spawning a terminal.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)]
pub(crate) struct TerminalLaunchPlan {
    pub program: String,
    pub args: Vec<String>,
    /// Display name of the chosen launcher (for logging).
    pub launcher: String,
}

#[cfg(target_os = "macos")]
fn build_terminal_plan(
    binary: &std::path::Path,
    cwd: &std::path::Path,
) -> Result<TerminalLaunchPlan, IpcError> {
    let bin_q = shell_single_quote(&binary.display().to_string());
    let cwd_q = shell_single_quote(&cwd.display().to_string());
    let inner = format!("cd {} && exec {}", cwd_q, bin_q);
    let escaped = applescript_escape(&inner);
    let script = format!(
        "tell application \"Terminal\"\n  do script \"{}\"\n  activate\nend tell",
        escaped
    );
    Ok(TerminalLaunchPlan {
        program: "osascript".to_string(),
        args: vec!["-e".to_string(), script],
        launcher: "Terminal.app".to_string(),
    })
}

#[cfg(target_os = "linux")]
fn build_terminal_plan(
    binary: &std::path::Path,
    cwd: &std::path::Path,
) -> Result<TerminalLaunchPlan, IpcError> {
    let bin_q = shell_single_quote(&binary.display().to_string());
    let cwd_q = shell_single_quote(&cwd.display().to_string());
    let inner = format!("cd {} && exec {}", cwd_q, bin_q);

    let candidates = [
        "x-terminal-emulator",
        "gnome-terminal",
        "konsole",
        "xfce4-terminal",
        "alacritty",
        "kitty",
        "xterm",
    ];

    for name in candidates {
        if which::which(name).is_ok() {
            let args: Vec<String> = match name {
                // gnome-terminal: `-- bash -c "<cmd>"` is the modern invocation
                "gnome-terminal" => vec![
                    "--".to_string(),
                    "bash".to_string(),
                    "-c".to_string(),
                    inner.clone(),
                ],
                // konsole: `-e bash -c "<cmd>"` works; `--` form also valid
                "konsole" => vec![
                    "-e".to_string(),
                    "bash".to_string(),
                    "-c".to_string(),
                    inner.clone(),
                ],
                // xfce4-terminal expects `--command "<single string>"`
                "xfce4-terminal" => vec![
                    "--command".to_string(),
                    format!("bash -c {}", shell_single_quote(&inner)),
                ],
                // alacritty / kitty / xterm / x-terminal-emulator all accept
                // `-e bash -c "<cmd>"`.
                _ => vec![
                    "-e".to_string(),
                    "bash".to_string(),
                    "-c".to_string(),
                    inner.clone(),
                ],
            };
            return Ok(TerminalLaunchPlan {
                program: name.to_string(),
                args,
                launcher: name.to_string(),
            });
        }
    }

    let _ = (bin_q, cwd_q);
    Err(IpcError::validation("No terminal emulator found on PATH"))
}

#[cfg(target_os = "windows")]
fn build_terminal_plan(
    binary: &std::path::Path,
    cwd: &std::path::Path,
) -> Result<TerminalLaunchPlan, IpcError> {
    let bin = binary.display().to_string();
    let cwd = cwd.display().to_string();
    // `cmd /c start "" cmd /k "cd /d <cwd> && <bin>"` opens a new console
    // window. The empty `""` is `start`'s window-title argument.
    let inner = format!("cd /d \"{}\" && \"{}\"", cwd, bin);
    Ok(TerminalLaunchPlan {
        program: "cmd".to_string(),
        args: vec![
            "/c".to_string(),
            "start".to_string(),
            "".to_string(),
            "cmd".to_string(),
            "/k".to_string(),
            inner,
        ],
        launcher: "cmd.exe".to_string(),
    })
}

#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
fn build_terminal_plan(
    _binary: &std::path::Path,
    _cwd: &std::path::Path,
) -> Result<TerminalLaunchPlan, IpcError> {
    Err(IpcError::validation(
        "Open Pi is not supported on this platform",
    ))
}

/// Test-only re-export for asserting on argv shape without spawning anything.
#[cfg(test)]
pub(crate) fn build_terminal_command_for_test(
    binary: &std::path::Path,
    cwd: &std::path::Path,
) -> Result<TerminalLaunchPlan, IpcError> {
    build_terminal_plan(binary, cwd)
}

fn spawn_terminal_with_pi(
    binary: &std::path::Path,
    cwd: &std::path::Path,
) -> Result<String, IpcError> {
    let plan = build_terminal_plan(binary, cwd)?;
    let mut cmd = tokio::process::Command::new(&plan.program);
    cmd.args(&plan.args);
    // Do not await; just spawn and drop. kill_on_drop defaults to false, so
    // the terminal outlives Hyvemind.
    cmd.spawn()
        .map_err(|e| IpcError::internal(format!("Failed to spawn '{}': {}", plan.program, e)))?;
    Ok(plan.launcher)
}

/// Launch the bundled Pi binary in a new terminal window.
///
/// The Pi binary is a TUI/interactive agent — it cannot run as a headless
/// child of the Tauri webview. This command opens a real terminal
/// (macOS Terminal.app, the first available Linux terminal emulator, or
/// `cmd` on Windows) and execs Pi inside it.
#[tracing::instrument(skip(state))]
#[tauri::command]
pub async fn open_pi_terminal(state: tauri::State<'_, AppState>) -> Result<(), IpcError> {
    info!("open_pi_terminal invoked");

    let config = state.config.read().await;
    let binary_path = config.pi_binary_path.clone();
    let default_project_path = config.default_project_path.clone();
    drop(config);

    let bin = binary_path
        .ok_or_else(|| IpcError::validation("Pi binary not found. Install Pi and try again."))?;
    if !bin.exists() {
        return Err(IpcError::validation(format!(
            "Pi binary missing on disk at {}",
            bin.display()
        )));
    }

    let cwd: PathBuf = match default_project_path.as_deref().map(PathBuf::from) {
        Some(p) if p.is_dir() => p,
        _ => dirs::home_dir().unwrap_or_else(|| PathBuf::from(".")),
    };

    let launcher = spawn_terminal_with_pi(&bin, &cwd)?;
    info!(
        binary = %bin.display(),
        cwd = %cwd.display(),
        launcher = %launcher,
        "open_pi_terminal spawned terminal"
    );
    Ok(())
}

/// Test a provider by sending a hello prompt to a model.
#[tracing::instrument(skip(state))]
#[tauri::command]
pub async fn test_provider_chat(
    state: tauri::State<'_, AppState>,
    provider: String,
    model: String,
) -> Result<TestChatResult, IpcError> {
    let provider = provider.trim().to_lowercase();
    let model = model.trim().to_string();
    info!(provider = %provider, model = %model, "test_provider_chat invoked");

    let config = state.config.read().await;
    let api_key = config
        .provider_keys
        .get(&provider)
        .cloned()
        .unwrap_or_default();
    let pc = config.providers.get(&provider).ok_or("unknown provider")?;
    let endpoint = pc.endpoint.clone().unwrap_or_default();
    let provider_type = pc.provider_type.clone();
    drop(config);

    if endpoint.is_empty() {
        return Ok(TestChatResult {
            ok: false,
            model,
            reply_preview: None,
            error: Some("no endpoint configured".into()),
        });
    }

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|e| e.to_string())?;

    if provider_type == "Anthropic" {
        let url = format!("{}/messages", endpoint.trim_end_matches('/'));
        let body = serde_json::json!({
            "model": model,
            "max_tokens": 64,
            "messages": [{"role": "user", "content": "Say hello in one sentence."}]
        });
        match client
            .post(&url)
            .header("x-api-key", &api_key)
            .header("anthropic-version", "2023-06-01")
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .await
        {
            Ok(resp) if resp.status().is_success() => {
                let text = resp.text().await.unwrap_or_default();
                match serde_json::from_str::<AnthropicMessagesResponse>(&text) {
                    Ok(parsed) => {
                        let preview = parsed.content.first().and_then(|c| c.text.clone());
                        Ok(TestChatResult {
                            ok: true,
                            model,
                            reply_preview: preview,
                            error: None,
                        })
                    }
                    Err(e) => Ok(TestChatResult {
                        ok: false,
                        model,
                        reply_preview: None,
                        error: Some(format!("parse error: {}", e)),
                    }),
                }
            }
            Ok(resp) => {
                let status = resp.status();
                let body_text = resp.text().await.unwrap_or_default();
                Ok(TestChatResult {
                    ok: false,
                    model,
                    reply_preview: None,
                    error: Some(format!(
                        "{}: {}",
                        status,
                        body_text.chars().take(200).collect::<String>()
                    )),
                })
            }
            Err(e) => Ok(TestChatResult {
                ok: false,
                model,
                reply_preview: None,
                error: Some(e.to_string()),
            }),
        }
    } else {
        // OpenAI Compatible
        let url = format!("{}/chat/completions", endpoint.trim_end_matches('/'));
        let body = serde_json::json!({
            "model": model,
            "max_tokens": 64,
            "messages": [{"role": "user", "content": "Say hello in one sentence."}]
        });
        let mut req = client
            .post(&url)
            .header("content-type", "application/json")
            .json(&body);
        if !api_key.is_empty() {
            req = req.bearer_auth(&api_key);
        }
        match req.send().await {
            Ok(resp) if resp.status().is_success() => {
                let text = resp.text().await.unwrap_or_default();
                match serde_json::from_str::<OaiChatResponse>(&text) {
                    Ok(parsed) => {
                        let preview = parsed
                            .choices
                            .first()
                            .and_then(|c| c.message.content.clone());
                        Ok(TestChatResult {
                            ok: true,
                            model,
                            reply_preview: preview,
                            error: None,
                        })
                    }
                    Err(e) => Ok(TestChatResult {
                        ok: false,
                        model,
                        reply_preview: None,
                        error: Some(format!("parse error: {}", e)),
                    }),
                }
            }
            Ok(resp) => {
                let status = resp.status();
                let body_text = resp.text().await.unwrap_or_default();
                Ok(TestChatResult {
                    ok: false,
                    model,
                    reply_preview: None,
                    error: Some(format!(
                        "{}: {}",
                        status,
                        body_text.chars().take(200).collect::<String>()
                    )),
                })
            }
            Err(e) => Ok(TestChatResult {
                ok: false,
                model,
                reply_preview: None,
                error: Some(e.to_string()),
            }),
        }
    }
}

/* ── Pi RPC provider test ──────────────────────────────────── */

/// Test a provider by sending a prompt through Pi RPC.
///
/// Spawns a temporary Pi session with the given model, sends a hello prompt,
/// and verifies the response. This confirms the provider is properly registered
/// with Pi's extension system and can route requests end-to-end.
#[tracing::instrument(skip(state))]
#[tauri::command]
pub async fn test_provider_pi(
    state: tauri::State<'_, AppState>,
    provider: String,
    model: String,
) -> Result<TestPiResult, IpcError> {
    let provider = provider.trim().to_lowercase();
    let model = model.trim().to_string();
    info!(provider = %provider, model = %model, "test_provider_pi invoked");

    // Map subscription provider IDs to Pi's native provider/model names.
    let full_model = match provider.as_str() {
        "chatgpt" => format!("openai-codex/{}", model),
        "claude-sub" => format!("anthropic/{}", model),
        _ => format!("{}/{}", provider, model),
    };
    let session_id = format!("pi-test-{}", uuid::Uuid::new_v4());

    let working_dir = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));

    // Spawn a temporary Pi session with minimal settings.
    let mut options = crate::pi::rpc::PiSessionOptions::for_model(&full_model);
    options.thinking_level = crate::pi::rpc::ThinkingLevel::Off;

    let session = match state
        .pi_manager
        .spawn_session_with_options(&session_id, &options, &working_dir)
        .await
    {
        Ok(s) => s,
        Err(e) => {
            return Ok(TestPiResult {
                ok: false,
                model: full_model,
                reply_preview: None,
                error: Some(humanize_pi_test_error(&format!(
                    "failed to spawn Pi session: {}",
                    e
                ))),
            });
        }
    };
    // Mark as Review-owned so the eviction loop doesn't kill the test
    // session out from under the user.
    session.set_owner(crate::pi::session::SessionOwner::Review {
        job_id: session_id.clone(),
    });

    // Send a simple prompt and collect the response with a timeout.
    if let Err(e) = session
        .send_prompt(
            "Say hello in exactly one sentence. Do not use any tools.",
            None,
        )
        .await
    {
        let _ = state.pi_manager.kill_session(&session_id).await;
        return Ok(TestPiResult {
            ok: false,
            model: full_model,
            reply_preview: None,
            error: Some(humanize_pi_test_error(&format!(
                "failed to send prompt: {}",
                e
            ))),
        });
    }

    let result = match tokio::time::timeout(
        std::time::Duration::from_secs(60),
        session.collect_response(),
    )
    .await
    {
        Ok(Ok(response)) => {
            let preview = response.chars().take(200).collect::<String>();
            TestPiResult {
                ok: true,
                model: full_model,
                reply_preview: Some(preview),
                error: None,
            }
        }
        Ok(Err(e)) => TestPiResult {
            ok: false,
            model: full_model,
            reply_preview: None,
            error: Some(humanize_pi_test_error(&format!("{}", e))),
        },
        Err(_) => TestPiResult {
            ok: false,
            model: full_model,
            reply_preview: None,
            error: Some("Pi test timed out after 60s".to_string()),
        },
    };

    // Always clean up the test session.
    let _ = state.pi_manager.kill_session(&session_id).await;

    Ok(result)
}

/* ── Prompts catalog ─────────────────────────────────────── */

/// Read-only descriptor for one system prompt that ships in the binary.
///
/// Used by the Settings → Prompts tab to show every prompt the app sends to
/// an LLM agent without exposing edits. Bodies are pulled from the same
/// constants/functions that the production code paths use, so the catalog
/// cannot drift.
#[derive(Debug, Clone, Serialize)]
pub struct SystemPromptInfo {
    /// Stable identifier (e.g. `"bee.scout"`). Used as the React key.
    pub id: String,
    /// Top-level grouping for the UI (e.g. `"Bee Agents"`, `"Hivemind"`).
    pub category: String,
    /// Display name (e.g. `"Scout"`, `"Reviewer Base"`).
    pub name: String,
    /// One-line explanation of why the prompt exists / what it does.
    pub description: String,
    /// Source path or `file:line` reference where the prompt lives in the
    /// repo. Shown verbatim in the UI as a chip.
    pub source: String,
    /// The full prompt body, exactly as sent to the agent (with literal
    /// `{STANCE_SUFFIX}` for the reviewer base template).
    pub body: String,
}

/// Build the static catalog of backend-owned prompts.
///
/// Frontend-defined prompts (Tasks plan / review-context / review-merge and
/// the user-prompt template builders) are catalogued client-side in
/// `app/src/lib/promptCatalog.ts`, so they live alongside the constants they
/// describe.
fn build_prompt_catalog() -> Vec<SystemPromptInfo> {
    use crate::core::{guard, queen, scout, worker};
    use crate::hivemind::engine::{Stance, REVIEWER_BASE_TEMPLATE};
    use crate::nurse::prompt as nurse;

    vec![
        SystemPromptInfo {
            id: "bee.queen".into(),
            category: "Bee Agents".into(),
            name: "Queen (runtime)".into(),
            description:
                "Runtime/fix-feature spec for the Queen orchestrator. The actual fix-feature \
                 dispatch is deterministic Rust (`create_fix_features` in `core/queen.rs`); \
                 this prompt documents the contract Worker/Guard sessions assume when Queen \
                 synthesises a fix-feature after a Guard failure."
                    .into(),
            source: "app/src-tauri/prompts/queen_system.md".into(),
            body: queen::default_system_prompt().to_string(),
        },
        SystemPromptInfo {
            id: "bee.scout".into(),
            category: "Bee Agents".into(),
            name: "Scout".into(),
            description:
                "Per-feature planner. Reads the working directory and produces a step-by-step \
                 implementation plan, complexity rating, and risk list for one feature."
                    .into(),
            source: "app/src-tauri/prompts/scout_system.md".into(),
            body: scout::default_system_prompt().to_string(),
        },
        SystemPromptInfo {
            id: "bee.worker".into(),
            category: "Bee Agents".into(),
            name: "Worker".into(),
            description:
                "Implementer. Takes a Scout plan and writes the actual code, then emits a \
                 structured handoff JSON block when finished."
                    .into(),
            source: "app/src-tauri/prompts/worker_system.md".into(),
            body: worker::default_system_prompt().to_string(),
        },
        SystemPromptInfo {
            id: "bee.guard".into(),
            category: "Bee Agents".into(),
            name: "Guard".into(),
            description: "Validator. Runs after milestone-tied features complete and checks each \
                 milestone assertion against the working directory."
                .into(),
            source: "app/src-tauri/prompts/guard_system.md".into(),
            body: guard::default_system_prompt().to_string(),
        },
        SystemPromptInfo {
            id: "bee.nurse".into(),
            category: "Bee Agents".into(),
            name: "Nurse".into(),
            description:
                "Heartbeat. Monitors long-running worker sessions for stalls and escalates \
                 intervention (steer → restart → diagnose) to keep swarms alive."
                    .into(),
            source: "app/src-tauri/prompts/nurse_system.md".into(),
            body: nurse::default_system_prompt().to_string(),
        },
        SystemPromptInfo {
            id: "hivemind.reviewer_base".into(),
            category: "Hivemind".into(),
            name: "Reviewer Base".into(),
            description: "Base system prompt for every Hivemind model call. The literal \
                 `{STANCE_SUFFIX}` token is replaced at runtime with the per-stance bias text \
                 below before the prompt is sent to the model."
                .into(),
            source: "app/src-tauri/src/hivemind/engine.rs (REVIEWER_BASE_TEMPLATE)".into(),
            body: REVIEWER_BASE_TEMPLATE.to_string(),
        },
        SystemPromptInfo {
            id: "hivemind.stance.against".into(),
            category: "Hivemind".into(),
            name: "Stance: Against".into(),
            description: "Bias suffix appended to the reviewer base when a model is assigned the \
                 Against stance. Pushes the reviewer to scrutinise risks while still \
                 acknowledging sound ideas."
                .into(),
            source: "app/src-tauri/src/hivemind/engine.rs (Stance::Against)".into(),
            body: Stance::Against.system_prompt_suffix().to_string(),
        },
        // For and Neutral stance variants were removed — the backend always hardcodes "against".
        SystemPromptInfo {
            id: "tasks.auto_commit_title".into(),
            category: "Other".into(),
            name: "Auto-commit Title".into(),
            description:
                "Sent to the configured default model after a Task completes when auto-commit \
                 is enabled. Generates a short git commit title from the staged diff."
                    .into(),
            source: "app/src-tauri/src/commands/tasks.rs (AUTO_COMMIT_TITLE_PROMPT)".into(),
            body: crate::commands::tasks::AUTO_COMMIT_TITLE_PROMPT.to_string(),
        },
        SystemPromptInfo {
            id: "tasks.auto_commit_title_conventional".into(),
            category: "Other".into(),
            name: "Auto-commit Title (Conventional Commits)".into(),
            description:
                "Variant of the auto-commit title prompt used when the \"Use Conventional \
                 Commits style\" toggle is enabled. Asks for a `type: subject` style title."
                    .into(),
            source: "app/src-tauri/src/commands/tasks.rs (AUTO_COMMIT_TITLE_PROMPT_CONVENTIONAL)"
                .into(),
            body: crate::commands::tasks::AUTO_COMMIT_TITLE_PROMPT_CONVENTIONAL.to_string(),
        },
    ]
}

/// Return the static catalog of backend-owned system prompts for the
/// Settings → Prompts tab. Bodies are baked into the binary at compile time.
#[tracing::instrument]
#[tauri::command]
pub async fn get_system_prompts() -> Result<Vec<SystemPromptInfo>, IpcError> {
    Ok(build_prompt_catalog())
}

/* ── User-defined custom prompts ─────────────────────────── */

const CUSTOM_PROMPT_NAME_MAX: usize = 100;
const CUSTOM_PROMPT_BODY_MAX: usize = 32 * 1024;

fn validate_custom_prompt(name: &str, body: &str) -> Result<(), String> {
    let name_trimmed = name.trim();
    if name_trimmed.is_empty() {
        return Err("name cannot be empty".to_string());
    }
    if name_trimmed.chars().count() > CUSTOM_PROMPT_NAME_MAX {
        return Err(format!(
            "name must be at most {} characters",
            CUSTOM_PROMPT_NAME_MAX
        ));
    }
    if body.trim().is_empty() {
        return Err("body cannot be empty".to_string());
    }
    if body.len() > CUSTOM_PROMPT_BODY_MAX {
        return Err(format!(
            "body must be at most {} bytes",
            CUSTOM_PROMPT_BODY_MAX
        ));
    }
    Ok(())
}

/// List all user-defined custom prompts, in creation order.
#[tracing::instrument(skip(state))]
#[tauri::command]
pub async fn list_custom_prompts(
    state: tauri::State<'_, AppState>,
) -> Result<Vec<CustomPrompt>, IpcError> {
    Ok(state.config.read().await.custom_prompts.clone())
}

/// Create or update a custom prompt. When `id` is `None`, a new prompt is
/// appended with a freshly generated UUID. When `id` is `Some`, the matching
/// entry is updated in place; an unknown id returns an error.
#[tracing::instrument(skip(state, body))]
#[tauri::command]
pub async fn save_custom_prompt(
    state: tauri::State<'_, AppState>,
    id: Option<String>,
    name: String,
    body: String,
) -> Result<CustomPrompt, IpcError> {
    validate_custom_prompt(&name, &body)?;

    if let Some(ref existing_id) = id {
        validate_id(existing_id)?;
    }

    let now = chrono::Utc::now().to_rfc3339();
    let trimmed_name = name.trim().to_string();

    let (saved, data_dir, bytes) = {
        let mut config = state.config.write().await;
        let saved = match id {
            Some(existing_id) => {
                let entry = config
                    .custom_prompts
                    .iter_mut()
                    .find(|p| p.id == existing_id)
                    .ok_or_else(|| format!("custom prompt not found: {}", existing_id))?;
                entry.name = trimmed_name;
                entry.body = body;
                entry.updated_at = now;
                entry.clone()
            }
            None => {
                let new = CustomPrompt {
                    id: uuid::Uuid::new_v4().to_string(),
                    name: trimmed_name,
                    body,
                    created_at: now.clone(),
                    updated_at: now,
                };
                config.custom_prompts.push(new.clone());
                new
            }
        };

        let bytes = config
            .snapshot_to_bytes()
            .map_err(|e| format!("failed to serialize config: {}", e))?;
        (saved, config.data_dir.clone(), bytes)
    };

    Config::write_bytes(data_dir, bytes)
        .await
        .map_err(|e| format!("failed to save config: {}", e))?;
    Ok(saved)
}

/// Delete a custom prompt by id. Missing ids are treated as a no-op so the
/// UI can call this without first checking existence. Hivemind rows that
/// reference the deleted id are left untouched — they silently resolve to
/// no suffix at review-dispatch time.
#[tracing::instrument(skip(state))]
#[tauri::command]
pub async fn delete_custom_prompt(
    state: tauri::State<'_, AppState>,
    id: String,
) -> Result<(), IpcError> {
    validate_id(&id)?;
    let (data_dir, bytes) = {
        let mut config = state.config.write().await;
        config.custom_prompts.retain(|p| p.id != id);
        let bytes = config
            .snapshot_to_bytes()
            .map_err(|e| format!("failed to serialize config: {}", e))?;
        (config.data_dir.clone(), bytes)
    };
    Config::write_bytes(data_dir, bytes)
        .await
        .map_err(|e| format!("failed to save config: {}", e))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_validate_runtime_settings_accepts_positive_values() {
        assert!(validate_runtime_settings(1, 1).is_ok());
        assert!(validate_runtime_settings(8, 6).is_ok());
    }

    #[test]
    fn test_validate_runtime_settings_rejects_zero_concurrency_cap() {
        assert!(validate_runtime_settings(0, 1).is_err());
    }

    #[test]
    fn test_validate_runtime_settings_rejects_zero_max_pi_processes() {
        assert!(validate_runtime_settings(1, 0).is_err());
    }

    #[test]
    fn test_extract_version_from_cli_output() {
        assert_eq!(extract_version("claude-code 1.0.34"), Some("1.0.34".into()));
        assert_eq!(extract_version("1.2.3"), Some("1.2.3".into()));
        assert_eq!(extract_version("v1.0.0 something"), Some("1.0.0".into()));
        assert_eq!(extract_version("no version here"), None);
        assert_eq!(extract_version(""), None);
        assert_eq!(extract_version("2.11"), Some("2.11".into()));
    }

    #[test]
    fn test_is_version_older() {
        assert!(is_version_older("1.0.0", "1.0.1"));
        assert!(is_version_older("1.0.34", "1.1.0"));
        assert!(is_version_older("0.9.9", "1.0.0"));
        assert!(!is_version_older("1.0.1", "1.0.0"));
        assert!(!is_version_older("1.0.0", "1.0.0"));
        assert!(!is_version_older("2.0.0", "1.9.9"));
    }

    #[test]
    fn test_detect_install_method_npm() {
        assert!(matches!(
            detect_install_method("/usr/local/lib/node_modules/@anthropic-ai/claude-code/cli.js"),
            PiInstallMethod::Npm
        ));
        assert!(matches!(
            detect_install_method("/home/user/.nvm/versions/node/v20/lib/node_modules/.bin/claude"),
            PiInstallMethod::Npm
        ));
    }

    #[test]
    fn test_detect_install_method_homebrew() {
        assert!(matches!(
            detect_install_method("/opt/homebrew/Cellar/claude-code/1.0.34/bin/claude"),
            PiInstallMethod::Homebrew
        ));
        assert!(matches!(
            detect_install_method("/usr/local/homebrew/bin/claude"),
            PiInstallMethod::Homebrew
        ));
    }

    #[test]
    fn test_detect_install_method_unknown() {
        assert!(matches!(
            detect_install_method("/usr/local/bin/claude"),
            PiInstallMethod::Unknown
        ));
    }

    // ------------------------------------------------------------------
    // validate_endpoint
    // ------------------------------------------------------------------

    #[test]
    fn test_validate_endpoint_accepts_https() {
        assert!(validate_endpoint("https://api.openai.com/v1").is_ok());
        assert!(validate_endpoint("https://example.com").is_ok());
    }

    #[test]
    fn test_validate_endpoint_accepts_loopback_http() {
        assert!(validate_endpoint("http://localhost:11434/v1").is_ok());
        assert!(validate_endpoint("http://127.0.0.1:8080").is_ok());
    }

    #[test]
    fn test_validate_endpoint_rejects_remote_http() {
        assert!(validate_endpoint("http://evil.example.com").is_err());
    }

    #[test]
    fn test_validate_endpoint_rejects_bad_schemes() {
        assert!(validate_endpoint("file:///etc/passwd").is_err());
        assert!(validate_endpoint("ftp://example.com").is_err());
        assert!(validate_endpoint("javascript:alert(1)").is_err());
    }

    #[test]
    fn test_validate_endpoint_rejects_empty_and_garbage() {
        assert!(validate_endpoint("").is_err());
        assert!(validate_endpoint("   ").is_err());
        assert!(validate_endpoint("not a url").is_err());
    }

    // ------------------------------------------------------------------
    // validate_working_dir
    // ------------------------------------------------------------------

    #[test]
    fn test_validate_working_dir_rejects_empty() {
        assert!(validate_working_dir("").is_err());
        assert!(validate_working_dir("   ").is_err());
    }

    #[test]
    fn test_validate_working_dir_rejects_nul_byte() {
        assert!(validate_working_dir("/tmp/\0/x").is_err());
    }

    #[test]
    fn test_validate_working_dir_accepts_existing_dir() {
        // /tmp is a directory on every platform we ship to.
        let canonical = validate_working_dir("/tmp").expect("/tmp must canonicalize");
        assert!(canonical.is_dir());
    }

    #[test]
    fn test_validate_working_dir_rejects_nonexistent() {
        assert!(validate_working_dir("/this/path/does/not/exist/hopefully").is_err());
    }

    // ------------------------------------------------------------------
    // build_prompt_catalog
    // ------------------------------------------------------------------

    #[test]
    fn test_prompt_catalog_has_expected_entries() {
        let catalog = build_prompt_catalog();
        // 5 bee agents (queen + scout + worker + guard + nurse) + 2 hivemind
        // (base + 1 stance) + 2 auto-commit (plain + Conventional Commits variant)
        assert_eq!(catalog.len(), 9, "expected 9 backend prompts");
    }

    #[test]
    fn test_prompt_catalog_entries_are_well_formed() {
        for entry in build_prompt_catalog() {
            assert!(!entry.id.is_empty(), "id empty: {:?}", entry);
            assert!(!entry.category.is_empty(), "category empty: {:?}", entry);
            assert!(!entry.name.is_empty(), "name empty: {:?}", entry);
            assert!(
                !entry.description.is_empty(),
                "description empty: {:?}",
                entry
            );
            assert!(!entry.source.is_empty(), "source empty: {:?}", entry);
            assert!(!entry.body.is_empty(), "body empty: {:?}", entry);
        }
    }

    #[test]
    fn test_prompt_catalog_ids_are_unique() {
        let catalog = build_prompt_catalog();
        let mut ids: Vec<&str> = catalog.iter().map(|e| e.id.as_str()).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), catalog.len(), "duplicate prompt ids in catalog");
    }

    // ------------------------------------------------------------------
    // Rich OpenAI-compatible /v1/models parser + catalog merge
    // ------------------------------------------------------------------

    #[test]
    fn test_oai_rich_parser_groq_shape() {
        // Groq returns `context_window` and `max_completion_tokens` at the top
        // level of each model entry.
        let body = r#"{
            "data": [
                {
                    "id": "llama-3.3-70b-versatile",
                    "context_window": 131072,
                    "max_completion_tokens": 32768
                },
                {
                    "id": "llama-3.1-8b-instant",
                    "context_window": 131072,
                    "max_completion_tokens": 8192
                }
            ]
        }"#;
        let parsed: OaiRichModelsResponse =
            serde_json::from_str(body).expect("groq-shaped payload must parse");
        assert_eq!(parsed.data.len(), 2);
        assert_eq!(parsed.data[0].id, "llama-3.3-70b-versatile");
        assert_eq!(parsed.data[0].context_length, Some(131072));
        assert_eq!(parsed.data[0].max_output, Some(32768));
        assert_eq!(parsed.data[1].max_output, Some(8192));
    }

    #[test]
    fn test_oai_rich_parser_accepts_max_output_tokens_alias() {
        // Some providers spell it `max_output_tokens`.
        let body = r#"{
            "data": [{"id": "foo", "max_output_tokens": 16384, "context_length": 200000}]
        }"#;
        let parsed: OaiRichModelsResponse = serde_json::from_str(body).unwrap();
        assert_eq!(parsed.data[0].max_output, Some(16384));
        assert_eq!(parsed.data[0].context_length, Some(200000));
    }

    #[test]
    fn test_enrich_details_with_catalog_fills_missing_for_openai_gpt4o() {
        // Plain OpenAI returns `{ id }` only. Catalog merge should fill
        // context_window + pricing from `get_model_catalog`.
        let models = vec!["gpt-4o".to_string(), "gpt-4o-mini".to_string()];
        let merged = enrich_details_with_catalog("openai", &models, vec![]);
        assert_eq!(merged.len(), 2);
        let gpt4o = &merged[0];
        assert_eq!(gpt4o.id, "gpt-4o");
        assert_eq!(gpt4o.context_length, Some(128_000));
        assert_eq!(gpt4o.input_price, Some(2.5));
        assert_eq!(gpt4o.output_price, Some(10.0));
        let mini = &merged[1];
        assert_eq!(mini.context_length, Some(128_000));
    }

    #[test]
    fn test_enrich_details_does_not_clobber_provider_values() {
        // Provider-reported values must win over catalog fallback values.
        let parsed = vec![ModelDetail {
            id: "gpt-4o".to_string(),
            name: None,
            context_length: Some(999_999),
            max_output: Some(42),
            input_price: Some(1.23),
            output_price: None,
        }];
        let merged = enrich_details_with_catalog("openai", &["gpt-4o".to_string()], parsed);
        assert_eq!(merged[0].context_length, Some(999_999));
        assert_eq!(merged[0].input_price, Some(1.23));
        // output_price was None on the parsed side, catalog provides 10.0
        assert_eq!(merged[0].output_price, Some(10.0));
        assert_eq!(merged[0].max_output, Some(42));
    }

    #[test]
    fn test_dedup_models_and_details_removes_duplicates_preserving_order() {
        // NIM-style: duplicate ids appear in `models`, with parallel `details`.
        let models = vec![
            "m-a".to_string(),
            "m-b".to_string(),
            "m-a".to_string(), // dup
            "m-c".to_string(),
            "m-b".to_string(), // dup
        ];
        let mk = |id: &str, ctx: u64| ModelDetail {
            id: id.to_string(),
            name: None,
            context_length: Some(ctx),
            max_output: None,
            input_price: None,
            output_price: None,
        };
        let details = vec![
            mk("m-a", 1000),
            mk("m-b", 2000),
            mk("m-a", 1001), // dup
            mk("m-c", 3000),
            mk("m-b", 2001), // dup
        ];

        let (out_models, out_details) = dedup_models_and_details(models, details);

        assert_eq!(out_models, vec!["m-a", "m-b", "m-c"]);
        assert_eq!(out_details.len(), 3);
        // 1:1 alignment invariant.
        for (i, m) in out_models.iter().enumerate() {
            assert_eq!(m, &out_details[i].id, "models[{}] must align with details[{}]", i, i);
        }
        // First-occurrence wins: m-a's context_length is the first one (1000), not 1001.
        assert_eq!(out_details[0].context_length, Some(1000));
        assert_eq!(out_details[1].context_length, Some(2000));
        assert_eq!(out_details[2].context_length, Some(3000));
    }

    #[test]
    fn test_dedup_models_and_details_handles_empty_details() {
        // Anthropic + bare-OAI branches pass `details: vec![]`.
        let models = vec!["a".to_string(), "a".to_string(), "b".to_string()];
        let (out_models, out_details) = dedup_models_and_details(models, vec![]);
        assert_eq!(out_models, vec!["a", "b"]);
        assert!(out_details.is_empty());
    }

    #[test]
    fn test_dedup_models_and_details_noop_when_unique() {
        let models = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        let mk = |id: &str| ModelDetail {
            id: id.to_string(),
            name: None,
            context_length: None,
            max_output: None,
            input_price: None,
            output_price: None,
        };
        let details = vec![mk("a"), mk("b"), mk("c")];
        let (out_models, out_details) = dedup_models_and_details(models.clone(), details);
        assert_eq!(out_models, models);
        assert_eq!(out_details.len(), 3);
        assert_eq!(out_details[0].id, "a");
        assert_eq!(out_details[1].id, "b");
        assert_eq!(out_details[2].id, "c");
    }

    #[test]
    fn test_dedup_runs_before_catalog_enrich() {
        // Integration: duplicated ids fed through dedup + catalog enrich must
        // yield exactly one merged entry per unique id, with catalog metadata
        // intact.
        let models = vec![
            "gpt-4o".to_string(),
            "gpt-4o".to_string(),
            "gpt-4o-mini".to_string(),
        ];
        let (models, details) = dedup_models_and_details(models, vec![]);
        assert_eq!(models, vec!["gpt-4o", "gpt-4o-mini"]);
        let merged = enrich_details_with_catalog("openai", &models, details);
        assert_eq!(merged.len(), 2);
        // Both should carry catalog data.
        assert_eq!(merged[0].id, "gpt-4o");
        assert!(merged[0].context_length.is_some());
        assert!(merged[0].input_price.is_some());
        assert_eq!(merged[1].id, "gpt-4o-mini");
        assert!(merged[1].context_length.is_some());
    }

    #[test]
    fn test_enrich_details_unknown_model_passes_through_empty() {
        let merged = enrich_details_with_catalog(
            "openai",
            &["some-bespoke-model-not-in-catalog".to_string()],
            vec![],
        );
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].context_length, None);
        assert_eq!(merged[0].input_price, None);
        assert!(!details_have_any_data(&merged));
    }

    #[test]
    fn test_details_have_any_data() {
        let empty = vec![ModelDetail {
            id: "x".into(),
            name: None,
            context_length: None,
            max_output: None,
            input_price: None,
            output_price: None,
        }];
        assert!(!details_have_any_data(&empty));
        let with_ctx = vec![ModelDetail {
            id: "x".into(),
            name: None,
            context_length: Some(1),
            max_output: None,
            input_price: None,
            output_price: None,
        }];
        assert!(details_have_any_data(&with_ctx));
    }

    #[test]
    fn test_applescript_escape_basic() {
        assert_eq!(applescript_escape("hello"), "hello");
        assert_eq!(applescript_escape("with space"), "with space");
    }

    #[test]
    fn test_applescript_escape_quotes_and_backslash() {
        // A literal quote becomes \" and a literal backslash becomes \\.
        assert_eq!(applescript_escape("say \"hi\""), "say \\\"hi\\\"");
        assert_eq!(applescript_escape("C:\\Users\\me"), "C:\\\\Users\\\\me");
        assert_eq!(applescript_escape("a\\b\"c"), "a\\\\b\\\"c");
    }

    #[test]
    fn test_shell_single_quote_plain() {
        assert_eq!(shell_single_quote("hello"), "'hello'");
        assert_eq!(
            shell_single_quote("/Applications/Hyvemind.app/Contents/MacOS/pi"),
            "'/Applications/Hyvemind.app/Contents/MacOS/pi'"
        );
    }

    #[test]
    fn test_shell_single_quote_with_space() {
        assert_eq!(
            shell_single_quote("/Users/me/My Projects/repo"),
            "'/Users/me/My Projects/repo'"
        );
    }

    #[test]
    fn test_shell_single_quote_with_apostrophe() {
        // 'don\''t' is the canonical POSIX-safe single-quoted form.
        assert_eq!(shell_single_quote("don't"), "'don'\\''t'");
    }

    #[test]
    fn test_shell_single_quote_with_backslash() {
        // Backslashes are literal inside single quotes; no escaping needed.
        assert_eq!(shell_single_quote("C:\\Users\\me"), "'C:\\Users\\me'");
    }

    #[test]
    fn test_build_terminal_command_for_test_current_host() {
        // Smoke-test the per-OS branch on the current host: should produce
        // a non-empty program/launcher and at least one arg on macOS/Windows.
        // On Linux it may error if no terminal emulator is present (CI).
        let bin = std::path::PathBuf::from("/tmp/pi-binary");
        let cwd = std::path::PathBuf::from("/tmp");
        let plan = build_terminal_command_for_test(&bin, &cwd);
        #[cfg(target_os = "macos")]
        {
            let p = plan.expect("macOS plan should always build");
            assert_eq!(p.program, "osascript");
            assert_eq!(p.args.len(), 2);
            assert_eq!(p.args[0], "-e");
            assert!(p.args[1].contains("tell application \"Terminal\""));
            assert!(p.args[1].contains("/tmp/pi-binary"));
            assert!(p.args[1].contains("cd '/tmp'"));
        }
        #[cfg(target_os = "windows")]
        {
            let p = plan.expect("Windows plan should always build");
            assert_eq!(p.program, "cmd");
            assert!(p.args.iter().any(|a| a == "/c"));
            assert!(p.args.iter().any(|a| a == "start"));
            assert!(p.args.iter().any(|a| a.contains("pi-binary")));
        }
        #[cfg(target_os = "linux")]
        {
            // Either we found a terminal and got a plan, or we didn't and
            // the validation error fires — both are acceptable in CI.
            match plan {
                Ok(p) => {
                    assert!(!p.program.is_empty());
                    assert!(p.args.iter().any(|a| a.contains("pi-binary")));
                }
                Err(_) => {}
            }
        }
    }

    #[test]
    fn test_macos_plan_escapes_apostrophe_in_path() {
        // Only meaningful on macOS, but the helpers are cross-platform.
        let inner_cwd = shell_single_quote("/Users/me/o'brien");
        assert_eq!(inner_cwd, "'/Users/me/o'\\''brien'");
    }

    #[test]
    fn test_reviewer_base_template_has_stance_placeholder() {
        // Production code path replaces `{STANCE_SUFFIX}` — guard against the
        // template drifting away from the placeholder name.
        use crate::hivemind::engine::REVIEWER_BASE_TEMPLATE;
        assert!(
            REVIEWER_BASE_TEMPLATE.contains("{STANCE_SUFFIX}"),
            "REVIEWER_BASE_TEMPLATE must contain {{STANCE_SUFFIX}} placeholder"
        );
    }
}
