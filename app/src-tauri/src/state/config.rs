use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use tracing::{debug, info, warn};

use crate::extensions::types::ExtensionUserSettings;
use crate::state::secret_store::SecretStore;

/// Metadata for a configured provider.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderConfig {
    pub display_name: String,
    pub provider_type: String,
    #[serde(default)]
    pub endpoint: Option<String>,
    /// Whether this provider has an API key in the OS keyring. Used to avoid
    /// querying the keyring for unconfigured providers on every startup
    /// (which would prompt the user to authorise access for each entry).
    #[serde(default)]
    pub has_key: bool,
}

/// Application configuration loaded from disk and environment.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// Root data directory (default: ~/.hyvemind)
    pub data_dir: PathBuf,

    /// Provider API keys. Loaded from the OS keyring (with environment variable
    /// overrides). Held in memory only; **never serialized to disk** — the
    /// `skip_serializing` attribute ensures `save()` does not write keys to
    /// `config.json`. Existing plaintext entries from older configs are
    /// migrated to the keyring on first load.
    #[serde(default, skip_serializing)]
    pub provider_keys: HashMap<String, String>,

    /// Provider metadata (display name, type, endpoint).
    #[serde(default)]
    pub providers: HashMap<String, ProviderConfig>,

    /// Default model identifier in "provider_id/model_id" format (e.g. "anthropic/claude-sonnet-4-20250514")
    #[serde(default)]
    pub default_model: Option<String>,

    /// Maximum number of concurrent LLM requests
    #[serde(default = "default_concurrency_cap")]
    pub concurrency_cap: usize,

    /// Maximum number of PI (process isolation) sessions
    #[serde(default = "default_max_pi_processes")]
    pub max_pi_processes: usize,

    /// Path to the PI binary. Resolved from config or PATH lookup.
    #[serde(default)]
    pub pi_binary_path: Option<PathBuf>,

    /// Default project directory for new tasks.
    #[serde(default)]
    pub default_project_path: Option<String>,

    /// Whether to automatically commit changes when a task completes implementation.
    #[serde(default)]
    pub auto_commit_tasks: bool,

    /// Whether auto-commit titles should follow Conventional Commits style
    /// (e.g. `feat: …`, `fix: …`). Default off — plain titles.
    #[serde(default)]
    pub auto_commit_conventional: bool,

    /// Default hivemind ID for new tasks (None = no review).
    #[serde(default)]
    pub default_hivemind: Option<String>,

    /// Whether to play a notification sound when a task/swarm completes.
    #[serde(default)]
    pub task_completion_sound_enabled: bool,

    /// True once we've scanned the OS keyring for legacy entries and
    /// populated each `ProviderConfig.has_key` flag. After this is set,
    /// startup only queries the keyring for providers known to have a key,
    /// so users with no keys configured see zero auth prompts.
    #[serde(default)]
    pub keyring_discovered: bool,

    /// Which sound to play ("chime", "pop", "bell", "success", "tweet").
    #[serde(default = "default_task_completion_sound")]
    pub task_completion_sound: String,

    /// Send anonymized crash and error reports to Sentry. `None` means
    /// the user hasn't expressed a preference; treated as enabled.
    /// Toggle takes effect on next launch (Sentry is initialised once at
    /// startup before any other subsystem).
    #[serde(default)]
    pub crash_reporting_enabled: Option<bool>,

    /// Model to use for nurse diagnostic sessions (e.g. "anthropic/claude-haiku-4.5").
    /// Falls back to "none" (no model) if unset — the nurse will skip LLM evaluation
    /// calls until a model is explicitly configured.
    #[serde(default)]
    pub nurse_model: Option<String>,

    /// Seconds of inactivity before the nurse considers a session stalled.
    /// Default: 300 (5 minutes). Chosen conservatively to avoid false positives
    /// from long compilations, large tool calls with `timeout: 300`, etc.
    #[serde(default = "default_nurse_stall_threshold_secs")]
    pub nurse_stall_threshold_secs: u64,

    /// Whether the nurse service is enabled globally. Default: true.
    #[serde(default = "default_nurse_enabled")]
    pub nurse_enabled: bool,

    /// When `true`, the Nurse keeps detectors / observability running for
    /// every session but the dispatcher suppresses every intervention
    /// whose owner isn't a swarm agent. Default: `false` (current
    /// behaviour). Persisted across restarts.
    #[serde(default = "default_nurse_swarms_only")]
    pub nurse_swarms_only: bool,

    /// Whether the nurse may take destructive actions (restart/cancel sessions).
    /// **Deprecated** — Nurse is always autonomous in the batched-call architecture.
    /// Kept for backward-compat deserialization of older config files; ignored at runtime.
    #[serde(default)]
    pub nurse_allow_destructive: bool,

    /// Interval between Nurse evaluation ticks (one batched provider call per tick).
    /// Default: 60s.
    #[serde(default = "default_nurse_tick_interval_secs")]
    pub nurse_tick_interval_secs: u64,

    /// Provider name (e.g. "anthropic", "openrouter") used for the Nurse model.
    /// When `None`, derived from `nurse_model` by splitting on `/` — e.g.
    /// `"anthropic/claude-haiku-4.5"` → provider="anthropic", model="claude-haiku-4.5".
    #[serde(default)]
    pub nurse_provider: Option<String>,

    /// How often the frontend forces a one-shot Nurse evaluation of a
    /// running chat session (Tasks-view conversation, hivemind context
    /// gather, hivemind merge). Default: 300s. Clamped to [60, 3600] on
    /// load. Lower this to test Nurse against real flows.
    #[serde(default = "default_chat_check_in_secs")]
    pub chat_check_in_secs: u64,

    /// How often the batched LLM Nurse reviewer sweeps every active
    /// session and asks the classifier whether anything is wrong. `None`
    /// (the default) falls back to `tunables::nurse_batch_interval_secs()`
    /// (env-var override `HYVEMIND_NURSE_BATCH_INTERVAL_SECS`, default 120s).
    /// When `Some`, the user-set value wins. Clamped to
    /// [`NURSE_BATCH_INTERVAL_MIN_SECS`, `NURSE_BATCH_INTERVAL_MAX_SECS`]
    /// on load and in the IPC setter.
    #[serde(default)]
    pub nurse_batch_interval_secs: Option<u64>,

    /// Per-profile Nurse tuning. Keys are the five [`NurseProfile`]
    /// variants (`tasks`, `swarm`, `hivemind`, `test`, `default`).
    /// Missing entries fall back to [`ProfileConfig::default_for`] via
    /// [`NurseConfig::profile`]'s lookup chain — empty on a fresh install
    /// is the same as "use the code defaults for everything".
    /// Mutated by `set_nurse_profile`; consumed at engine tick time so
    /// edits take effect on the next tick.
    #[serde(default)]
    pub nurse_profiles:
        HashMap<crate::nurse::config::NurseProfile, crate::nurse::config::ProfileConfig>,

    /// Per-extension user preferences (enabled / show in topbar),
    /// keyed by the composite `extension_id` (`type_id:provider_id`).
    /// Missing entries default to `ExtensionUserSettings::default()`
    /// (everything enabled and visible).
    #[serde(default)]
    pub extension_settings: HashMap<String, ExtensionUserSettings>,

    /// In-app stability test config (Tests screen). Independent from the
    /// app's `default_model` so you can pin cheap models for daily smoke
    /// runs and richer ones for pre-release checks. Empty strings on a
    /// fresh install — the Tests screen substitutes `default_model` for
    /// any unset entry.
    #[serde(default)]
    pub stability_test: StabilityTestConfig,

    /// Global poll interval for provider-extension fetches (seconds).
    /// Overrides per-extension `refresh_interval_secs()` return values.
    /// Clamped to [`EXTENSION_POLL_INTERVAL_MIN_SECS`, `EXTENSION_POLL_INTERVAL_MAX_SECS`]
    /// on load. The poller also applies these bounds in the context accessor
    /// for defense-in-depth. Default: 120s.
    #[serde(default = "default_extension_poll_interval_secs")]
    pub extension_poll_interval_secs: u64,

    /// Phase 5A: global daily spending cap in USD. `None` (the default)
    /// means unlimited. When set, the Queen's between-batch budget check
    /// sums today's `usage_log` rows and pauses any running swarm if
    /// the cumulative cost meets or exceeds this value. Opt-in;
    /// backwards compatible with configs written before this field
    /// existed.
    #[serde(default)]
    pub daily_budget_usd: Option<f64>,

    /// User-defined custom prompts that can be appended to a Hivemind
    /// reviewer's system prompt on a per-model basis. Order is preserved
    /// so the Settings UI lists prompts in creation order. Empty by default.
    #[serde(default)]
    pub custom_prompts: Vec<CustomPrompt>,

    // -- Phase 2-6 rollout flags for the delimiter → structured-tool migration ----
    //
    // Each flag controls one surface's preferred path. When `true`, the
    // surface prefers the structured-tool path (Pi extension tool call or
    // provider-native `tools` / `tool_choice`) and only falls back to the
    // Phase-0 hardened delimiter parser when the model didn't / couldn't
    // use the structured path. When `false`, the surface stays on the
    // legacy delimiter path unconditionally.
    //
    // All four default to `false` so existing alpha users see no behaviour
    // change until they opt in. Phase 7 cleanup flips them to `true` by
    // default once the structured paths have been live for ≥2 weeks
    // without regression.
    //
    // Flags are evaluated at session/job START — never per chunk or
    // per event — so an in-flight session crossing a flag flip can't
    // produce a half-delimited / half-structured transcript.
    /// Worker handoff (Phase 2). Prefer `submit_handoff` extension tool.
    #[serde(default)]
    pub use_extension_handoff: bool,

    /// Tasks-view planning (Phase 3). Prefer the four planning tools
    /// (`submit_task_meta`, `submit_questions`, `submit_plan`,
    /// `submit_features`). Migrated atomically because the planning prompts
    /// reference all four formats together.
    #[serde(default)]
    pub use_extension_planning: bool,

    /// Stability test (Phase 4). Prefer `submit_stability_*` tools and the
    /// `submit_context` tool inside the runner.
    #[serde(default)]
    pub use_extension_stability: bool,

    /// Allowlist of approved working directories (audit item 1.11).
    ///
    /// Every IPC command that accepts a `working_dir` parameter validates the
    /// canonicalized path against this list — the path must equal one of
    /// these entries or be a strict descendant of one. Empty by default for
    /// fresh installs; on first save the user's `default_project_path` (if
    /// set) is automatically seeded into the list so existing users aren't
    /// blocked. Add new entries via the `request_working_dir_approval` IPC
    /// command after explicit user confirmation in the UI.
    #[serde(default)]
    pub approved_working_dirs: Vec<PathBuf>,
}

/// A user-defined system prompt suffix. Referenced by id from a Hivemind's
/// `rounds_config[].models[].custom_prompt_id`. Resolved to a body string at
/// review dispatch time; dangling ids silently fall through to no suffix.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CustomPrompt {
    pub id: String,
    pub name: String,
    pub body: String,
    pub created_at: String,
    pub updated_at: String,
}

/// Persisted configuration for the in-app stability test (Tests screen).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StabilityTestConfig {
    /// Model used for the Task (planning + implementation phases).
    /// Empty string means "fall back to default_model".
    #[serde(default)]
    pub task_model: String,
    /// Saved Hivemind id to use for the review round. The Hivemind's
    /// `rounds_config` supplies both the reviewer model list and the
    /// round count. `None` (or missing) means the test cannot run.
    #[serde(default)]
    pub hivemind_id: Option<String>,
    /// Model used for the AI verifier session.
    /// Empty string means "fall back to default_model".
    #[serde(default)]
    pub verifier_model: String,
}

fn default_task_completion_sound() -> String {
    "chime".to_string()
}

fn default_nurse_stall_threshold_secs() -> u64 {
    300
}

fn default_nurse_enabled() -> bool {
    true
}

fn default_nurse_swarms_only() -> bool {
    false
}

fn default_nurse_tick_interval_secs() -> u64 {
    60
}

fn default_chat_check_in_secs() -> u64 {
    300
}

/// Allowed range for `chat_check_in_secs`. Values outside this range are
/// clamped on load (and rejected from the settings IPC).
pub const CHAT_CHECK_IN_MIN_SECS: u64 = 60;
pub const CHAT_CHECK_IN_MAX_SECS: u64 = 3600;

/// Allowed range for `nurse_batch_interval_secs`. Matches the tunable's
/// own clamp range so the env var and the user setting agree on bounds.
pub const NURSE_BATCH_INTERVAL_MIN_SECS: u64 = 30;
pub const NURSE_BATCH_INTERVAL_MAX_SECS: u64 = 3600;

/// Global extension poll interval range. `extension_poll_interval_secs`
/// is clamped to this range on load and in the context accessor.
/// The MIN matches `poller::MIN_REFRESH_INTERVAL_SECS` (30s) to keep
/// global and per-extension floors in agreement.
pub const EXTENSION_POLL_INTERVAL_MIN_SECS: u64 = 30;
pub const EXTENSION_POLL_INTERVAL_MAX_SECS: u64 = 3600;

fn default_extension_poll_interval_secs() -> u64 {
    120
}

/// Minimum allowed nurse stall threshold. Values below this are clamped
/// with a WARN log. Prevents users from setting an unrealistically aggressive
/// stall threshold (e.g. 5s) that would generate constant false positives.
pub const NURSE_MIN_STALL_THRESHOLD_SECS: u64 = 60;

fn default_concurrency_cap() -> usize {
    30
}

fn default_max_pi_processes() -> usize {
    // Default pool ceiling. Six is the default — each Pi process holds
    // 100-250 MB RSS, so a laptop-friendly pool keeps Hyvemind well under
    // a gigabyte of memory in the common case. Raise via Settings →
    // Advanced (or `HYVEMIND_PI_MAX_PROCESSES`) if you run larger swarms.
    crate::tunables::pi_max_processes()
}

impl Default for Config {
    fn default() -> Self {
        Self {
            data_dir: default_data_dir(),
            provider_keys: HashMap::new(),
            providers: HashMap::new(),
            default_model: None,
            concurrency_cap: default_concurrency_cap(),
            max_pi_processes: default_max_pi_processes(),
            pi_binary_path: None,
            default_project_path: None,
            auto_commit_tasks: false,
            auto_commit_conventional: false,
            default_hivemind: None,
            task_completion_sound_enabled: false,
            task_completion_sound: default_task_completion_sound(),
            keyring_discovered: false,
            crash_reporting_enabled: None,
            nurse_model: None,
            nurse_stall_threshold_secs: default_nurse_stall_threshold_secs(),
            nurse_enabled: default_nurse_enabled(),
            nurse_swarms_only: default_nurse_swarms_only(),
            nurse_allow_destructive: false,
            nurse_tick_interval_secs: default_nurse_tick_interval_secs(),
            nurse_provider: None,
            chat_check_in_secs: default_chat_check_in_secs(),
            nurse_batch_interval_secs: None,
            nurse_profiles: HashMap::new(),
            extension_settings: HashMap::new(),
            extension_poll_interval_secs: default_extension_poll_interval_secs(),
            stability_test: StabilityTestConfig::default(),
            daily_budget_usd: None,
            custom_prompts: Vec::new(),
            use_extension_handoff: false,
            use_extension_planning: false,
            use_extension_stability: false,
            approved_working_dirs: Vec::new(),
        }
    }
}

/// Seed the providers map with default entries for any missing known providers.
fn seed_default_providers(providers: &mut HashMap<String, ProviderConfig>) {
    // Endpoints include the version prefix (e.g. /v1) so the test code
    // only needs to append /models, /chat/completions, or /messages.
    let defaults: &[(&str, &str, &str, Option<&str>)] = &[
        (
            "anthropic",
            "Anthropic",
            "Anthropic",
            Some("https://api.anthropic.com/v1"),
        ),
        (
            "openai",
            "OpenAI",
            "OpenAI Compatible",
            Some("https://api.openai.com/v1"),
        ),
        (
            "openrouter",
            "OpenRouter",
            "OpenAI Compatible",
            Some("https://openrouter.ai/api/v1"),
        ),
        (
            "deepseek",
            "DeepSeek",
            "OpenAI Compatible",
            Some("https://api.deepseek.com/v1"),
        ),
        (
            "glm",
            "GLM",
            "OpenAI Compatible",
            Some("https://open.bigmodel.cn/api/coding/paas/v4"),
        ),
        (
            "mistral",
            "Mistral",
            "OpenAI Compatible",
            Some("https://api.mistral.ai/v1"),
        ),
        (
            "ollama",
            "Ollama",
            "OpenAI Compatible",
            Some("https://ollama.com/v1"),
        ),
        ("chatgpt", "ChatGPT (Subscription)", "Subscription", None),
        ("claude-sub", "Claude (Subscription)", "Subscription", None),
        (
            "crof",
            "Crof",
            "OpenAI Compatible",
            Some("https://crof.ai/v1"),
        ),
        (
            "kimi",
            "Kimi",
            "OpenAI Compatible",
            Some("https://api.moonshot.cn/v1"),
        ),
        (
            "groq",
            "Groq",
            "OpenAI Compatible",
            Some("https://api.groq.com/openai/v1"),
        ),
        (
            "neuralwatt",
            "NeuralWatt",
            "OpenAI Compatible",
            Some("https://api.neuralwatt.com/v1"),
        ),
        (
            "nvidia-nim",
            "NVIDIA NIM",
            "OpenAI Compatible",
            Some("https://integrate.api.nvidia.com/v1"),
        ),
        (
            "opencode-go",
            "OpenCode Go",
            "OpenAI Compatible",
            Some("https://opencode.ai/zen/go/v1"),
        ),
    ];
    for (id, display, ptype, endpoint) in defaults {
        providers
            .entry(id.to_string())
            .or_insert_with(|| ProviderConfig {
                display_name: display.to_string(),
                provider_type: ptype.to_string(),
                endpoint: endpoint.map(|s| s.to_string()),
                has_key: false,
            });
    }
}

/// Status of Pi SDK subscription auth tokens.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubscriptionAuthStatus {
    pub chatgpt: bool,
    pub claude: bool,
    pub auth_file_exists: bool,
    pub error: Option<String>,
}

/// Check `~/.pi/agent/auth.json` for subscription provider auth tokens.
pub fn check_pi_subscription_auth() -> SubscriptionAuthStatus {
    let auth_path = dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".pi")
        .join("agent")
        .join("auth.json");

    if !auth_path.exists() {
        return SubscriptionAuthStatus {
            chatgpt: false,
            claude: false,
            auth_file_exists: false,
            error: None,
        };
    }

    let data = match std::fs::read_to_string(&auth_path) {
        Ok(d) => d,
        Err(e) => {
            return SubscriptionAuthStatus {
                chatgpt: false,
                claude: false,
                auth_file_exists: true,
                error: Some(format!("failed to read auth file: {}", e)),
            };
        }
    };

    let parsed: serde_json::Value = match serde_json::from_str(&data) {
        Ok(v) => v,
        Err(e) => {
            return SubscriptionAuthStatus {
                chatgpt: false,
                claude: false,
                auth_file_exists: true,
                error: Some(format!("failed to parse auth file: {}", e)),
            };
        }
    };

    // Check for provider entries that contain an "access" token.
    // The auth file uses provider IDs as top-level keys (e.g. "anthropic",
    // "openai-codex") with nested { type, access, refresh, expires } objects.
    let has_auth = |keys: &[&str]| -> bool {
        keys.iter().any(|key| {
            parsed
                .get(key)
                .and_then(|obj| obj.get("access"))
                .and_then(|v| v.as_str())
                .map_or(false, |s| !s.is_empty())
        })
    };

    let chatgpt = has_auth(&["openai-codex", "openai", "chatgpt"]);
    let claude = has_auth(&["anthropic", "claude"]);

    debug!(
        chatgpt = chatgpt,
        claude = claude,
        auth_path = %auth_path.display(),
        "checked Pi subscription auth"
    );

    SubscriptionAuthStatus {
        chatgpt,
        claude,
        auth_file_exists: true,
        error: None,
    }
}

fn default_data_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".hyvemind")
}

/// Hardcoded allowlist of environment-variable names that the readiness
/// probe (and other auth-using subsystems) may read from the process
/// environment. Mirrors the `known` table inside [`Config::pi_env_vars`].
///
/// Any env var name **not** in this list MUST be rejected before being
/// looked up — this prevents a malicious readiness manifest from
/// exfiltrating arbitrary secrets (`AWS_SECRET_ACCESS_KEY`, `GITHUB_TOKEN`,
/// `LD_PRELOAD`, etc.) by smuggling them as `auth_env`.
pub const PROVIDER_API_KEY_ENV_ALLOWLIST: &[&str] = &[
    "ANTHROPIC_API_KEY",
    "OPENAI_API_KEY",
    "OPENROUTER_API_KEY",
    "DEEPSEEK_API_KEY",
    "GLM_API_KEY",
    "MISTRAL_API_KEY",
    "CROF_API_KEY",
];

impl Config {
    fn provider_env_suffix(provider: &str) -> String {
        provider.to_uppercase().replace('-', "_")
    }

    fn api_key_env_var_for_provider(provider: &str) -> String {
        match provider {
            "anthropic" => "ANTHROPIC_API_KEY".to_string(),
            "openai" => "OPENAI_API_KEY".to_string(),
            "openrouter" => "OPENROUTER_API_KEY".to_string(),
            "deepseek" => "DEEPSEEK_API_KEY".to_string(),
            "glm" => "GLM_API_KEY".to_string(),
            "mistral" => "MISTRAL_API_KEY".to_string(),
            "crof" => "CROF_API_KEY".to_string(),
            other => format!("{}_API_KEY", Self::provider_env_suffix(other)),
        }
    }

    fn endpoint_env_var_for_provider(provider: &str) -> String {
        format!("{}_ENDPOINT", Self::provider_env_suffix(provider))
    }

    /// Build the non-secret provider manifest consumed by Hyvemind's bundled
    /// Pi extension. The manifest deliberately contains only ids, display
    /// names, endpoint metadata, and env var names; actual API keys remain in
    /// the individual `{PROVIDER}_API_KEY` env vars forwarded separately.
    fn pi_provider_manifest_json(&self) -> String {
        let mut providers: Vec<_> = self
            .providers
            .iter()
            .filter_map(|(id, pc)| {
                if pc.provider_type != "OpenAI Compatible" {
                    return None;
                }
                let endpoint = pc.endpoint.as_deref()?.trim();
                if endpoint.is_empty() {
                    return None;
                }
                let has_key = self
                    .provider_keys
                    .get(id)
                    .map(|k| !k.is_empty())
                    .unwrap_or(false);
                let is_local = endpoint.contains("localhost")
                    || endpoint.contains("127.0.0.1")
                    || endpoint.contains("[::1]");
                if !has_key && !is_local {
                    return None;
                }
                Some(serde_json::json!({
                    "id": id,
                    "displayName": &pc.display_name,
                    "baseUrl": endpoint,
                    "endpointEnvVar": Self::endpoint_env_var_for_provider(id),
                    "apiKeyEnvVar": Self::api_key_env_var_for_provider(id),
                }))
            })
            .collect();
        providers.sort_by(|a, b| {
            a.get("id")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .cmp(b.get("id").and_then(|v| v.as_str()).unwrap_or(""))
        });
        serde_json::json!({ "providers": providers }).to_string()
    }

    /// Load configuration from `~/.hyvemind/config.json`, falling back to
    /// defaults if the file does not exist. Environment variables override
    /// persisted values for provider keys.
    pub fn load() -> Result<Self> {
        let default_dir = default_data_dir();
        let config_path = default_dir.join("config.json");

        let mut config = if config_path.exists() {
            let data = std::fs::read_to_string(&config_path)
                .with_context(|| format!("failed to read config from {}", config_path.display()))?;

            // Check for legacy commit_model field in raw JSON before deserializing.
            // commit_model has been removed from the Config struct, so we need to
            // peek at the raw value to migrate it to default_model.
            let needs_migration = {
                if let Ok(raw) = serde_json::from_str::<serde_json::Value>(&data) {
                    raw.get("commit_model")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string())
                } else {
                    None
                }
            };

            let mut parsed: Config = serde_json::from_str::<Config>(&data).with_context(|| {
                format!("failed to parse config from {}", config_path.display())
            })?;

            // Migrate legacy commit_model to default_model if default_model is not set
            if parsed.default_model.is_none() {
                if let Some(ref cm) = needs_migration {
                    let migrated = cm.trim().replace(':', "/");
                    if migrated.contains('/') {
                        warn!("migrating legacy commit_model '{}' to default_model '{}' — the commit_model field is removed; use the default model setting going forward", cm, migrated);
                        parsed.default_model = Some(migrated);
                    }
                }
            }

            // Clamp chat_check_in_secs to its allowed range.
            let original = parsed.chat_check_in_secs;
            parsed.chat_check_in_secs = parsed
                .chat_check_in_secs
                .clamp(CHAT_CHECK_IN_MIN_SECS, CHAT_CHECK_IN_MAX_SECS);
            if parsed.chat_check_in_secs != original {
                warn!(
                    original,
                    clamped = parsed.chat_check_in_secs,
                    "chat_check_in_secs out of range; clamped"
                );
            }

            // Clamp extension_poll_interval_secs to its allowed range.
            let original_poll = parsed.extension_poll_interval_secs;
            parsed.extension_poll_interval_secs = parsed.extension_poll_interval_secs.clamp(
                EXTENSION_POLL_INTERVAL_MIN_SECS,
                EXTENSION_POLL_INTERVAL_MAX_SECS,
            );
            if parsed.extension_poll_interval_secs != original_poll {
                warn!(
                    original = original_poll,
                    clamped = parsed.extension_poll_interval_secs,
                    "extension_poll_interval_secs out of range; clamped"
                );
            }

            // Clamp the optional Nurse batch interval. None means
            // "fall back to the env-var tunable" so we leave it alone.
            if let Some(v) = parsed.nurse_batch_interval_secs {
                let clamped = v.clamp(NURSE_BATCH_INTERVAL_MIN_SECS, NURSE_BATCH_INTERVAL_MAX_SECS);
                if clamped != v {
                    warn!(
                        original = v,
                        clamped, "nurse_batch_interval_secs out of range; clamped"
                    );
                    parsed.nurse_batch_interval_secs = Some(clamped);
                }
            }

            parsed
        } else {
            info!(
                "no config file found at {}, using defaults",
                config_path.display()
            );
            Config::default()
        };

        // Step 1: migrate any plaintext provider keys deserialized from
        // config.json into the OS keyring, then drop them from in-memory
        // until we re-load from the keyring. After this `save()` will not
        // write keys to disk (because `provider_keys` is `skip_serializing`).
        config.migrate_keys_to_keyring();

        // Step 2: seed default providers into the providers map.
        seed_default_providers(&mut config.providers);

        // Step 3: env var overrides go in BEFORE the keyring so any provider
        // covered by an env var skips the keyring query (avoids unnecessary
        // OS auth prompts).
        let env_mappings: &[(&str, &str)] = &[
            ("ANTHROPIC_API_KEY", "anthropic"),
            ("OPENAI_API_KEY", "openai"),
            ("OPENROUTER_API_KEY", "openrouter"),
            ("DEEPSEEK_API_KEY", "deepseek"),
            ("GLM_API_KEY", "glm"),
            ("MISTRAL_API_KEY", "mistral"),
            ("CROF_API_KEY", "crof"),
            ("KIMI_API_KEY", "kimi"),
            ("GROQ_API_KEY", "groq"),
            ("NEURALWATT_API_KEY", "neuralwatt"),
            ("NVIDIA_NIM_API_KEY", "nvidia-nim"),
            ("OPENCODE_GO_API_KEY", "opencode-go"),
        ];
        for (env_var, provider) in env_mappings {
            if let Ok(val) = std::env::var(env_var) {
                if !val.is_empty() {
                    info!("loaded {} from environment", env_var);
                    config.provider_keys.insert(provider.to_string(), val);
                }
            }
        }

        // Step 4: load keys from the keyring — but only for providers known
        // to have one (`has_key=true`). The first run after upgrading runs a
        // one-time discovery scan to populate `has_key` for users who already
        // had keyring entries from a prior version.
        config.load_keys_from_keyring();

        // Persist config now that any legacy plaintext keys have been pulled
        // out and migrated to the keyring. This rewrites config.json without
        // the `provider_keys` field thanks to `#[serde(skip_serializing)]`.
        //
        // `load()` runs during startup before AppState is constructed; using
        // the blocking variant is intentional — we can't await here because
        // `load` is synchronous, and the startup path is not on a hot lock.
        if let Err(e) = config.save_blocking() {
            warn!("failed to rewrite config.json after key migration: {}", e);
        }

        // Ensure data directory exists
        if !config.data_dir.exists() {
            std::fs::create_dir_all(&config.data_dir).with_context(|| {
                format!(
                    "failed to create data directory {}",
                    config.data_dir.display()
                )
            })?;
            info!("created data directory {}", config.data_dir.display());
        }

        // Always resolve the Pi binary from the bundled / dev / PATH search
        // order — any `pi_binary_path` override from `config.json` is ignored.
        // The bundled binary is the whole point of shipping Pi with Hyvemind;
        // we don't want a stale homebrew install silently winning over it.
        if let Some(p) = &config.pi_binary_path {
            info!(
                override = %p.display(),
                "ignoring pi_binary_path config override; using bundled Pi"
            );
        }
        config.pi_binary_path = resolve_pi_binary();

        tracing::debug!(
            provider_count = config.providers.len(),
            configured_key_count = config.provider_keys.len(),
            default_model = ?config.default_model,
            default_hivemind = ?config.default_hivemind,
            concurrency_cap = config.concurrency_cap,
            max_pi_processes = config.max_pi_processes,
            pi_binary_path = ?config.pi_binary_path,
            data_dir = %config.data_dir.display(),
            "config loaded"
        );

        // Audit 1.11: existing users who installed before the working-dir
        // allowlist landed should not be locked out of their own configured
        // project. If the allowlist is empty and they have a
        // default_project_path set, seed it now. The seed only fires once
        // (subsequent loads see a non-empty list and short-circuit).
        config.seed_approved_working_dirs_from_default();

        Ok(config)
    }

    /// Build a map of environment variables to forward to Pi subprocesses.
    ///
    /// Translates provider keys from the internal `provider_keys` map to
    /// the standard environment variable names that the Pi extension reads.
    /// Also exports endpoint URLs as `{PROVIDER}_ENDPOINT` so the Pi
    /// extension can route to custom OpenAI-compatible endpoints.
    pub fn pi_env_vars(&self) -> HashMap<String, String> {
        let known: &[(&str, &str)] = &[
            ("anthropic", "ANTHROPIC_API_KEY"),
            ("openai", "OPENAI_API_KEY"),
            ("openrouter", "OPENROUTER_API_KEY"),
            ("deepseek", "DEEPSEEK_API_KEY"),
            ("glm", "GLM_API_KEY"),
            ("mistral", "MISTRAL_API_KEY"),
            ("crof", "CROF_API_KEY"),
        ];

        let mut env = HashMap::new();
        for (provider, env_var) in known {
            if let Some(key) = self.provider_keys.get(*provider) {
                if !key.is_empty() {
                    env.insert(env_var.to_string(), key.clone());
                }
            }
        }
        // Forward remaining provider keys as {PROVIDER_UPPER}_API_KEY
        for (provider, key) in &self.provider_keys {
            if key.is_empty() {
                continue;
            }
            if known.iter().any(|(p, _)| *p == provider.as_str()) {
                continue;
            }
            let env_var = Self::api_key_env_var_for_provider(provider);
            env.insert(env_var, key.clone());
        }

        // Export endpoint URLs for providers that declare a custom endpoint,
        // so Pi can route OpenAI-compatible providers (DeepSeek, Mistral, etc.)
        // to the right base URL via standard {PROVIDER}_ENDPOINT env vars.
        for (provider, pc) in &self.providers {
            if let Some(endpoint) = &pc.endpoint {
                if !endpoint.is_empty() {
                    let env_var = Self::endpoint_env_var_for_provider(provider);
                    env.insert(env_var, endpoint.clone());
                }
            }
        }

        let manifest = self.pi_provider_manifest_json();
        if manifest != r#"{"providers":[]}"# {
            env.insert("HYVEMIND_PI_PROVIDERS_JSON".to_string(), manifest);
        }

        env
    }

    /// Return the list of providers that have a key configured.
    pub fn configured_providers(&self) -> Vec<String> {
        self.providers
            .keys()
            .filter(|p| self.provider_keys.contains_key(p.as_str()))
            .filter(|p| {
                self.providers
                    .get(p.as_str())
                    .map_or(true, |pc| pc.provider_type != "CLI")
            })
            .cloned()
            .collect()
    }

    /// Re-resolve the Pi binary path (e.g. after an update).
    pub fn refresh_pi_binary(&mut self) {
        self.pi_binary_path = resolve_pi_binary();
    }

    /// Note legacy plaintext API keys deserialized from `config.json` so the
    /// rest of the load pipeline knows they exist.
    ///
    /// Old behaviour wrote each key to its own keyring entry here. With
    /// consolidated storage, the keys are simply left in `provider_keys` —
    /// `load_keys_from_keyring` will fold them into the single combined
    /// keychain entry in one shot, avoiding per-provider auth prompts.
    /// `save()` still won't persist them to disk because `provider_keys`
    /// is `#[serde(skip_serializing)]`.
    fn migrate_keys_to_keyring(&mut self) {
        if self.provider_keys.is_empty() {
            return;
        }
        let mut count = 0usize;
        for (provider, key) in self.provider_keys.iter() {
            if key.is_empty() {
                continue;
            }
            count += 1;
            warn!(provider = %provider, "found legacy plaintext API key in config.json — will be folded into combined keychain entry");
        }
        debug!(
            legacy_key_count = count,
            "legacy plaintext key migration scheduled"
        );
    }

    /// Populate `provider_keys` from the OS keyring.
    ///
    /// Fast path: a single keychain access reads the combined provider-keys
    /// blob, so a user with N configured providers sees exactly one OS auth
    /// prompt instead of N. If the combined entry doesn't exist yet (first
    /// run after the consolidation refactor, or fresh install with legacy
    /// per-provider entries), the legacy per-provider scan runs once and
    /// the combined entry is written so all future startups take the fast
    /// path.
    fn load_keys_from_keyring(&mut self) {
        // ── Fast path: file-based credential cache ──────────────────
        //
        // Try reading `{data_dir}/.credentials` first. If it exists and
        // parses, we populate provider_keys from it with zero OS keyring
        // calls — no macOS Keychain authorization dialog.
        match SecretStore::load_from_file(&self.data_dir) {
            Ok(Some(map)) => {
                for (id, key) in map {
                    if key.is_empty() {
                        continue;
                    }
                    // Env var already populated this entry — env wins.
                    if self.provider_keys.contains_key(&id) {
                        continue;
                    }
                    self.provider_keys.insert(id, key);
                }
                self.mark_has_key_for_loaded_providers();
                self.keyring_discovered = true;
                return;
            }
            Ok(None) => {
                // File cache doesn't exist yet — fall through to keyring.
            }
            Err(e) => {
                warn!(error = %e, "credentials file corrupt; falling back to keyring");
                SecretStore::delete_file(&self.data_dir);
            }
        }

        // ── Keyring path ────────────────────────────────────────────
        //
        // File cache was absent or corrupt. Read from the OS keyring
        // (which may trigger an auth dialog on macOS), then write the
        // file cache so future launches skip the keyring entirely.
        match SecretStore::load_all() {
            Ok(Some(map)) => {
                for (id, key) in map {
                    if key.is_empty() {
                        continue;
                    }
                    // Env var already populated this entry — env wins.
                    if self.provider_keys.contains_key(&id) {
                        continue;
                    }
                    self.provider_keys.insert(id, key);
                }
                self.mark_has_key_for_loaded_providers();
                self.keyring_discovered = true;
                self.persist_credentials_file();
                return;
            }
            Ok(None) => {
                // Combined entry doesn't exist yet — fall through to migration.
            }
            Err(e) => {
                warn!(error = %e, "combined keyring blob unparseable; falling back to per-provider scan");
            }
        }

        // Migration path. Walk the per-provider entries one last time so
        // existing users don't lose their keys, then write the combined
        // entry. The old per-provider entries are left in place — they're
        // harmless dead weight after migration.
        self.legacy_load_keys_from_keyring();
        self.mark_has_key_for_loaded_providers();
        self.persist_combined_keys_blob();
        self.persist_credentials_file();
    }

    /// Old per-provider scan, retained only for one-time migration.
    fn legacy_load_keys_from_keyring(&mut self) {
        let needs_discovery = !self.keyring_discovered;
        let provider_ids: Vec<String> = self.providers.keys().cloned().collect();
        for id in provider_ids {
            if self.provider_keys.contains_key(&id) {
                continue;
            }
            let known_has_key = self.providers.get(&id).map_or(false, |pc| pc.has_key);
            if !needs_discovery && !known_has_key {
                continue;
            }
            if let Some(key) = SecretStore::get(&id) {
                if !key.is_empty() {
                    self.provider_keys.insert(id, key);
                }
            }
        }
        if needs_discovery {
            self.keyring_discovered = true;
        }
    }

    /// Write the file-based credential cache from the current `provider_keys`.
    ///
    /// Best-effort: warn-logs on failure. Only called after a successful
    /// keyring read so we only cache known-good credential sets.
    fn persist_credentials_file(&self) {
        let map: BTreeMap<String, String> = self
            .provider_keys
            .iter()
            .filter(|(_, v)| !v.is_empty())
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        if let Err(e) = SecretStore::save_to_file(&self.data_dir, &map) {
            warn!(error = %e, "failed to write credentials file cache");
        }
    }

    /// Mirror the in-memory `provider_keys` map into the combined keychain
    /// entry. Best-effort: warn-logs on failure so a misconfigured keyring
    /// can't crash startup.
    fn persist_combined_keys_blob(&self) {
        let map: BTreeMap<String, String> = self
            .provider_keys
            .iter()
            .filter(|(_, v)| !v.is_empty())
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        if let Err(e) = SecretStore::save_all(&map) {
            warn!(error = %e, "failed to persist combined provider-keys blob");
        }
    }

    /// Set `has_key=true` on every provider that has a non-empty entry in
    /// `provider_keys`. Called after both the fast and migration load paths
    /// so the UI/config reflects the actually-loaded keys.
    fn mark_has_key_for_loaded_providers(&mut self) {
        let ids: Vec<String> = self
            .provider_keys
            .iter()
            .filter(|(_, v)| !v.is_empty())
            .map(|(k, _)| k.clone())
            .collect();
        for id in ids {
            if let Some(pc) = self.providers.get_mut(&id) {
                pc.has_key = true;
            }
        }
    }

    /// Cheap one-shot read of just the `crash_reporting_enabled` field from
    /// `~/.hyvemind/config.json`. Used by `sentry_setup::init` which must run
    /// before `AppState::new()` (Sentry needs to be live before any other
    /// subsystem can fail).
    ///
    /// Returns `true` on any error (missing file, parse failure, missing
    /// field) — the default is to enable crash reporting; the user opts out
    /// via the Settings toggle.
    pub fn peek_crash_reporting() -> bool {
        let path = default_data_dir().join("config.json");
        let Ok(data) = std::fs::read_to_string(&path) else {
            return true;
        };
        let Ok(raw) = serde_json::from_str::<serde_json::Value>(&data) else {
            return true;
        };
        match raw.get("crash_reporting_enabled") {
            Some(serde_json::Value::Bool(b)) => *b,
            // null or missing → default true
            _ => true,
        }
    }

    /// Serialise the current configuration to bytes.
    ///
    /// Cheap and synchronous — intended to be called while holding the
    /// `Config` write guard. Pair with [`Config::write_bytes`] (which
    /// performs the actual disk write off the runtime) to avoid blocking
    /// other readers behind a slow file write.
    ///
    /// **Important**: `provider_keys` is `#[serde(skip_serializing)]` so
    /// API keys are never written to disk via this path.
    pub fn snapshot_to_bytes(&self) -> Result<Vec<u8>> {
        serde_json::to_vec_pretty(self)
            .context("failed to serialize config")
            .map(|mut v| {
                // serde_json::to_vec_pretty does not add a trailing newline.
                // Keep parity with the historical `to_string_pretty` write so
                // round-tripping via `serde_json::from_str` is identical.
                v.push(b'\n');
                v
            })
    }

    /// Add `canonical_path` to the approved working-directories allowlist if
    /// not already present. Caller is responsible for canonicalizing before
    /// calling this (typically via `commands::util::canonicalize_working_dir`).
    /// Returns `true` if the path was added, `false` if it was already in the
    /// allowlist (or matched an existing entry after canonicalization).
    pub fn add_approved_working_dir(&mut self, canonical_path: PathBuf) -> bool {
        // De-duplicate by canonical comparison. We canonicalize each existing
        // entry on the fly; entries that fail to canonicalize are skipped
        // (a broken entry shouldn't block a fresh add).
        //
        // `canonicalize_clean` (dunce) keeps Windows paths free of the `\\?\`
        // extended-length prefix so the equality check matches the cleaned
        // `canonical_path` produced by `commands::util::canonicalize_clean`.
        for entry in &self.approved_working_dirs {
            if let Ok(existing_canon) = crate::commands::util::canonicalize_clean(entry) {
                if existing_canon == canonical_path {
                    return false;
                }
            }
        }
        self.approved_working_dirs.push(canonical_path);
        true
    }

    /// Auto-seed the approved-working-dirs allowlist with the user's
    /// existing `default_project_path` if (a) the allowlist is empty and (b)
    /// `default_project_path` is set and canonicalizable. This keeps users
    /// who installed before audit item 1.11 from being silently locked out
    /// of their own configured project on the next launch.
    ///
    /// Idempotent: safe to call on every `save()` because the seed only
    /// fires when the allowlist is empty.
    pub fn seed_approved_working_dirs_from_default(&mut self) {
        if !self.approved_working_dirs.is_empty() {
            return;
        }
        let Some(ref dp) = self.default_project_path else {
            return;
        };
        let raw = dp.trim();
        if raw.is_empty() {
            return;
        }
        // `canonicalize_clean` strips the Windows `\\?\` prefix so persisted
        // allowlist entries match the cleaned working_dir produced at IPC time
        // (otherwise the `Path::starts_with` check would spuriously fail).
        match crate::commands::util::canonicalize_clean(std::path::Path::new(raw)) {
            Ok(canonical) => {
                info!(
                    path = %canonical.display(),
                    "seeding approved_working_dirs from default_project_path"
                );
                self.approved_working_dirs.push(canonical);
            }
            Err(e) => {
                warn!(
                    path = %raw,
                    error = %e,
                    "could not canonicalize default_project_path while seeding approved_working_dirs; skipping"
                );
            }
        }
    }

    /// Atomically write the serialised config bytes to `{data_dir}/config.json`.
    ///
    /// Runs the synchronous file I/O inside `tokio::task::spawn_blocking`
    /// so the calling task never blocks the runtime. The data directory
    /// is created if missing (first-run boot path).
    ///
    /// Atomic: writes to a sibling temp file in the same directory then
    /// renames it over the target — a crash mid-write can never leave
    /// `config.json` truncated.
    ///
    /// Callers that already hold the `Config` write guard should:
    ///   1. Call [`Config::snapshot_to_bytes`] under the guard.
    ///   2. Drop the guard.
    ///   3. `await` `Config::write_bytes(data_dir, bytes)`.
    pub async fn write_bytes(data_dir: PathBuf, bytes: Vec<u8>) -> Result<()> {
        tokio::task::spawn_blocking(move || Self::write_bytes_blocking(&data_dir, &bytes))
            .await
            .context("config write task panicked")?
    }

    /// Synchronous core of [`Config::write_bytes`]. Used directly by
    /// [`Config::save_blocking`] and the `spawn_blocking` closure.
    fn write_bytes_blocking(data_dir: &std::path::Path, bytes: &[u8]) -> Result<()> {
        let config_path = data_dir.join("config.json");

        // Ensure data_dir exists. On a brand-new install the directory
        // may not exist yet — create it before attempting the temp-file
        // dance so `new_in` doesn't fail.
        if !data_dir.exists() {
            std::fs::create_dir_all(data_dir).with_context(|| {
                format!("failed to create config data dir {}", data_dir.display())
            })?;
        }

        // Write to a sibling temp file then atomically rename. The temp
        // file lives in the same directory as the target so the rename
        // stays within one filesystem (POSIX rename atomicity guarantee).
        let tmp = tempfile::NamedTempFile::new_in(data_dir)
            .with_context(|| format!("failed to create temp file in {}", data_dir.display()))?;
        std::fs::write(tmp.path(), bytes).with_context(|| {
            format!("failed to write config temp file {}", tmp.path().display())
        })?;
        tmp.persist(&config_path).with_context(|| {
            format!(
                "failed to atomically persist config to {}",
                config_path.display()
            )
        })?;
        // fsync the parent directory so the rename itself is durable across a
        // power loss — see `crate::state::store::sync_parent_dir_blocking`
        // (we're already on a blocking thread via `spawn_blocking`, so the
        // sync sibling is the right call here).
        crate::state::store::sync_parent_dir_blocking(&config_path).with_context(|| {
            format!(
                "failed to fsync parent directory of config {}",
                config_path.display()
            )
        })?;
        Ok(())
    }

    /// Synchronous save used at startup (before the tokio runtime is in
    /// a position to spawn_blocking) and in unit tests. Blocks the
    /// current thread on the file I/O.
    pub fn save_blocking(&self) -> Result<()> {
        let bytes = self.snapshot_to_bytes()?;
        Self::write_bytes_blocking(self.data_dir.as_path(), &bytes)
    }
}

/// Host target triple derived from `std::env::consts::ARCH` / `OS`.
///
/// Used to locate the bundled Pi binary when running via `cargo run`
/// (where Tauri's `externalBin` triple-suffix is still in place — the suffix
/// is only stripped when Tauri installs the binary into the final `.app`).
/// Returns an empty string for unsupported platforms; callers treat empty as
/// "no triple-suffixed candidate to probe".
fn host_target_triple() -> &'static str {
    match (std::env::consts::ARCH, std::env::consts::OS) {
        ("aarch64", "macos") => "aarch64-apple-darwin",
        ("x86_64", "macos") => "x86_64-apple-darwin",
        ("x86_64", "linux") => "x86_64-unknown-linux-gnu",
        ("aarch64", "linux") => "aarch64-unknown-linux-gnu",
        ("x86_64", "windows") => "x86_64-pc-windows-msvc.exe",
        _ => "",
    }
}

/// Returns true if `binary` has its required runtime siblings (`package.json`
/// and `dist/`) in the same directory. The bun-compiled Pi reads its own
/// `package.json` for the version and probes `dist/modes/interactive/assets/`
/// at startup — without those siblings it crashes on the very first IPC turn,
/// so the resolver MUST reject candidates that don't carry them.
///
/// In particular, Tauri's `externalBin` copies *just the binary* to
/// `target/{debug,release}/pi` for `cargo run` — the support files only ride
/// along in production via `bundle.resources`. We auto-route around that.
fn pi_binary_is_usable(binary: &std::path::Path) -> bool {
    let Some(dir) = binary.parent() else {
        return false;
    };
    dir.join("package.json").exists() && dir.join("dist").exists()
}

/// Locate the Pi binary, preferring the bundled artifact over a PATH lookup.
///
/// Each tier requires the candidate binary's sibling `package.json` + `dist/`
/// to be present (see `pi_binary_is_usable`); tiers where the binary exists
/// without its support files are skipped with a `warn!`.
///
/// Resolution order:
/// 1. `current_exe().parent()/pi` — production `.app/Contents/MacOS/pi`.
/// 2. `current_exe().parent()/binaries/pi-<host-triple>` — `cargo run` layout
///    where the dev binary still has its triple suffix.
/// 3. Repo-relative dev fallback — walk up from `current_exe` looking for
///    `app/src-tauri/binaries/pi-<host-triple>` (the source-tree build output
///    where support files are guaranteed to be siblings).
/// 4. PATH lookup via `which pi` — last-resort dev fallback. Emits a WARN.
fn resolve_pi_binary() -> Option<PathBuf> {
    let triple = host_target_triple();
    let triple_name = if triple.is_empty() {
        None
    } else {
        Some(format!("pi-{}", triple))
    };

    let try_candidate = |label: &'static str, candidate: PathBuf| -> Option<PathBuf> {
        if !candidate.exists() {
            return None;
        }
        if !pi_binary_is_usable(&candidate) {
            warn!(
                candidate = %candidate.display(),
                "skipping Pi binary {label}: missing sibling package.json or dist/ \
                 (Tauri's externalBin copies the binary alone; support files only \
                 ship via bundle.resources). Falling through to the next tier.",
                label = label,
            );
            return None;
        }
        info!(source = label, "found PI binary at {}", candidate.display());
        Some(candidate)
    };

    if let Ok(exe) = std::env::current_exe() {
        if let Some(exe_dir) = exe.parent() {
            // (1) Bundled .app layout — Tauri puts externalBin at
            // .app/Contents/MacOS/pi alongside the main executable.
            if let Some(p) = try_candidate("bundled", exe_dir.join("pi")) {
                return Some(p);
            }

            // (2) exe-adjacent binaries/pi-<triple> (rare layout but cheap to probe).
            if let Some(name) = &triple_name {
                if let Some(p) = try_candidate("dev", exe_dir.join("binaries").join(name)) {
                    return Some(p);
                }
            }

            // (3) Walk up to find the source-tree binaries/ directory. Covers
            //     `cargo run` from src-tauri/, from app/, and from the repo root.
            if let Some(name) = &triple_name {
                let mut cursor = exe_dir;
                for _ in 0..8 {
                    let probes = [
                        cursor
                            .join("app")
                            .join("src-tauri")
                            .join("binaries")
                            .join(name),
                        cursor.join("src-tauri").join("binaries").join(name),
                        cursor.join("binaries").join(name),
                    ];
                    for probe in probes {
                        if let Some(p) = try_candidate("dev", probe) {
                            return Some(p);
                        }
                    }
                    match cursor.parent() {
                        Some(p) => cursor = p,
                        None => break,
                    }
                }
            }
        }
    }

    // (4) PATH lookup — last-resort dev fallback. PATH-installed Pi is an
    //     npm-global install where package.json lives in node_modules, so we
    //     skip the sibling check for this tier only.
    let output = std::process::Command::new("which")
        .arg("pi")
        .output()
        .ok()?;
    if output.status.success() {
        let path_str = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if !path_str.is_empty() {
            let path = PathBuf::from(&path_str);
            if path.exists() {
                warn!(
                    source = "path",
                    "found PI binary at {} (production builds should ship a bundled binary)",
                    path.display()
                );
                return Some(path);
            }
        }
    }

    warn!("no PI binary found (bundled, dev, or PATH)");
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    // ------------------------------------------------------------------
    // Config defaults
    // ------------------------------------------------------------------

    #[test]
    fn test_config_default_values() {
        let config = Config::default();
        assert_eq!(config.concurrency_cap, 30);
        assert_eq!(config.max_pi_processes, crate::tunables::pi_max_processes());
        assert!(config.default_model.is_none());
        assert!(config.pi_binary_path.is_none());
        assert!(config.provider_keys.is_empty());
    }

    // ------------------------------------------------------------------
    // seed_default_providers
    // ------------------------------------------------------------------

    #[test]
    fn test_seed_default_providers_adds_all_known() {
        let mut providers = HashMap::new();
        seed_default_providers(&mut providers);

        assert!(providers.contains_key("anthropic"));
        assert!(providers.contains_key("openai"));
        assert!(providers.contains_key("openrouter"));
        assert!(providers.contains_key("deepseek"));
        assert!(providers.contains_key("ollama"));

        assert_eq!(providers["anthropic"].display_name, "Anthropic");
        assert_eq!(providers["anthropic"].provider_type, "Anthropic");
        assert_eq!(providers["openai"].provider_type, "OpenAI Compatible");
        assert!(providers["anthropic"].endpoint.is_some());
    }

    #[test]
    fn test_seed_default_providers_preserves_existing() {
        let mut providers = HashMap::new();
        providers.insert(
            "anthropic".to_string(),
            ProviderConfig {
                display_name: "My Custom Anthropic".to_string(),
                provider_type: "Custom".to_string(),
                endpoint: Some("https://custom.example.com".to_string()),
                has_key: false,
            },
        );

        seed_default_providers(&mut providers);

        // Existing entry should NOT be overwritten
        assert_eq!(providers["anthropic"].display_name, "My Custom Anthropic");
        assert_eq!(providers["anthropic"].provider_type, "Custom");
        assert_eq!(
            providers["anthropic"].endpoint,
            Some("https://custom.example.com".to_string())
        );

        // But other entries should still be added
        assert!(providers.contains_key("openai"));
    }

    // ------------------------------------------------------------------
    // pi_env_vars
    // ------------------------------------------------------------------

    #[test]
    fn test_pi_env_vars_maps_keys() {
        let mut config = Config::default();
        config
            .provider_keys
            .insert("anthropic".to_string(), "sk-ant-key".to_string());
        config
            .provider_keys
            .insert("openai".to_string(), "sk-openai-key".to_string());

        let env = config.pi_env_vars();
        assert_eq!(
            env.get("ANTHROPIC_API_KEY"),
            Some(&"sk-ant-key".to_string())
        );
        assert_eq!(
            env.get("OPENAI_API_KEY"),
            Some(&"sk-openai-key".to_string())
        );
        // Should NOT include keys that aren't present
        assert!(!env.contains_key("OPENROUTER_API_KEY"));
    }

    #[test]
    fn test_pi_env_vars_skips_empty_keys() {
        let mut config = Config::default();
        config
            .provider_keys
            .insert("anthropic".to_string(), "".to_string()); // empty!

        let env = config.pi_env_vars();
        assert!(!env.contains_key("ANTHROPIC_API_KEY"));
    }

    #[test]
    fn test_pi_env_vars_empty_when_no_keys() {
        let config = Config::default();
        let env = config.pi_env_vars();
        assert!(env.is_empty());
    }

    #[test]
    fn test_pi_env_vars_all_providers() {
        let mut config = Config::default();
        seed_default_providers(&mut config.providers);
        for provider in &[
            "anthropic",
            "openai",
            "openrouter",
            "deepseek",
            "glm",
            "mistral",
        ] {
            config
                .provider_keys
                .insert(provider.to_string(), format!("key-{}", provider));
        }

        let env = config.pi_env_vars();
        assert_eq!(env.get("ANTHROPIC_API_KEY").unwrap(), "key-anthropic");
        assert_eq!(env.get("OPENAI_API_KEY").unwrap(), "key-openai");
        assert_eq!(env.get("DEEPSEEK_API_KEY").unwrap(), "key-deepseek");
    }

    #[test]
    fn test_pi_env_vars_exports_endpoints() {
        let mut config = Config::default();
        seed_default_providers(&mut config.providers);

        let env = config.pi_env_vars();
        // Seeded providers with endpoints should produce {PROVIDER}_ENDPOINT vars
        assert_eq!(
            env.get("DEEPSEEK_ENDPOINT"),
            Some(&"https://api.deepseek.com/v1".to_string())
        );
        assert_eq!(
            env.get("OPENAI_ENDPOINT"),
            Some(&"https://api.openai.com/v1".to_string())
        );
    }

    #[test]
    fn test_pi_env_vars_forwards_unknown_providers() {
        let mut config = Config::default();
        config
            .provider_keys
            .insert("anthropic".to_string(), "sk-ant".to_string());
        config
            .provider_keys
            .insert("opencode-go".to_string(), "sk-oc".to_string());
        config
            .provider_keys
            .insert("my-custom".to_string(), "sk-custom".to_string());

        let env = config.pi_env_vars();
        // Known provider mapped normally
        assert_eq!(env.get("ANTHROPIC_API_KEY").unwrap(), "sk-ant");
        // Unknown providers mapped dynamically
        assert_eq!(env.get("OPENCODE_GO_API_KEY").unwrap(), "sk-oc");
        assert_eq!(env.get("MY_CUSTOM_API_KEY").unwrap(), "sk-custom");
    }

    // ------------------------------------------------------------------
    // configured_providers
    // ------------------------------------------------------------------

    #[test]
    fn test_configured_providers_filters_by_keys() {
        let mut config = Config::default();
        seed_default_providers(&mut config.providers);
        config
            .provider_keys
            .insert("anthropic".to_string(), "key1".into());
        config
            .provider_keys
            .insert("openai".to_string(), "key2".into());
        // openrouter exists in providers but has NO key

        let configured = config.configured_providers();
        assert!(configured.contains(&"anthropic".to_string()));
        assert!(configured.contains(&"openai".to_string()));
        assert!(!configured.contains(&"openrouter".to_string()));
    }

    #[test]
    fn test_configured_providers_empty_when_no_keys() {
        let config = Config::default();
        let configured = config.configured_providers();
        assert!(configured.is_empty());
    }

    // ------------------------------------------------------------------
    // save() atomicity
    // ------------------------------------------------------------------

    /// `save()` must write atomically: a crash mid-write should leave
    /// the previous `config.json` intact. We can't `kill -9` a unit
    /// test, but we can drop a `NamedTempFile` without `persist()`
    /// being called — the equivalent failure mode.
    #[test]
    fn save_is_atomic_via_tempfile_persist() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let mut config = Config::default();
        config.data_dir = tmp.path().to_path_buf();

        // First successful save establishes the baseline file.
        config.data_dir_default_model_for_test("model-A".to_string());
        config.save_blocking().expect("baseline save");
        let baseline =
            std::fs::read_to_string(tmp.path().join("config.json")).expect("read baseline");
        assert!(baseline.contains("model-A"));

        // Simulate a mid-write crash: drop the temp file without
        // persisting. The previous file must remain untouched.
        {
            let dropped = tempfile::NamedTempFile::new_in(tmp.path()).expect("new tempfile");
            std::fs::write(dropped.path(), b"partial corrupt bytes").expect("write tmp");
            // dropped here — NamedTempFile drop removes the temp file.
        }
        let after_crash =
            std::fs::read_to_string(tmp.path().join("config.json")).expect("read after");
        assert_eq!(
            after_crash, baseline,
            "baseline config.json must be untouched after simulated mid-write failure"
        );

        // Second successful save should rotate cleanly.
        config.data_dir_default_model_for_test("model-B".to_string());
        config.save_blocking().expect("second save");
        let after = std::fs::read_to_string(tmp.path().join("config.json"))
            .expect("read after second save");
        assert!(after.contains("model-B"));
        assert!(!after.contains("model-A"));
    }

    /// `extension_settings` round-trips through serde and survives a
    /// missing field in the on-disk payload (forward-compat with old
    /// config files).
    #[test]
    fn config_round_trips_with_extension_settings() {
        use crate::extensions::types::ExtensionUserSettings;
        let mut cfg = Config::default();
        cfg.extension_settings.insert(
            "openrouter_credits:openrouter".to_string(),
            ExtensionUserSettings {
                enabled: false,
                show_in_topbar: true,
                preferences: Default::default(),
            },
        );
        let json = serde_json::to_string(&cfg).expect("serialize");
        let back: Config = serde_json::from_str(&json).expect("deserialize");
        let entry = back
            .extension_settings
            .get("openrouter_credits:openrouter")
            .expect("entry survives round-trip");
        assert!(!entry.enabled);
        assert!(entry.show_in_topbar);

        // Forward-compat: an old config file with no `extension_settings`
        // field still deserialises (defaults to empty map).
        let legacy_json = serde_json::to_value(&cfg)
            .map(|mut v| {
                v.as_object_mut().unwrap().remove("extension_settings");
                v.to_string()
            })
            .expect("value");
        let legacy: Config =
            serde_json::from_str(&legacy_json).expect("legacy config deserialises");
        assert!(legacy.extension_settings.is_empty());
    }

    // ------------------------------------------------------------------
    // save() does not block concurrent RwLock readers
    // ------------------------------------------------------------------

    /// Regression test for Fix 3.2 — when one task is mid-save (the
    /// snapshot has been taken under the write guard, the guard has
    /// been dropped, and `write_bytes` is in flight inside
    /// `spawn_blocking`), other tasks attempting `read().await` on the
    /// shared `RwLock<Config>` must NOT be blocked.
    ///
    /// We model the previous, buggy behaviour by overloading the data
    /// directory with a hook that makes the disk write artificially
    /// slow, then confirm a reader task completes well before the
    /// writer does. With the old in-guard sync write a reader would be
    /// blocked for the full slow-write duration; with the refactored
    /// save the reader returns immediately.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn save_does_not_block_concurrent_readers() {
        use std::sync::Arc;
        use std::time::{Duration, Instant};
        use tokio::sync::RwLock;

        let tmp = tempfile::TempDir::new().expect("tempdir");
        let mut config = Config::default();
        config.data_dir = tmp.path().to_path_buf();
        config.data_dir_default_model_for_test("model-X".to_string());

        // Bake the initial config to disk so subsequent saves rotate
        // an existing file (closer to the production code path).
        config.save_blocking().expect("initial save");

        let shared = Arc::new(RwLock::new(config));

        // ── Writer task ────────────────────────────────────────────
        //
        // Mirrors the production pattern: acquire write guard,
        // snapshot under the guard, drop the guard, then await the
        // slow disk write off the runtime.
        let writer_shared = Arc::clone(&shared);
        let writer = tokio::spawn(async move {
            let (bytes, data_dir) = {
                let mut guard = writer_shared.write().await;
                guard.data_dir_default_model_for_test("model-Y".to_string());
                let bytes = guard.snapshot_to_bytes().expect("snapshot");
                let data_dir = guard.data_dir.clone();
                // Guard drops here, BEFORE the slow disk operation.
                (bytes, data_dir)
            };

            // Inject artificial slowness inside the spawn_blocking
            // body so the disk-write phase dominates the timeline.
            // We do this by composing our own write that sleeps then
            // delegates to `Config::write_bytes`. Since we want to test
            // the *real* `write_bytes`, we sleep first then call it.
            tokio::task::spawn_blocking(move || {
                std::thread::sleep(Duration::from_millis(400));
                Config::write_bytes_blocking(data_dir.as_path(), &bytes)
            })
            .await
            .expect("blocking task")
            .expect("write_bytes");
        });

        // Give the writer a beat to reach the spawn_blocking phase
        // (i.e. to have already released the guard).
        tokio::time::sleep(Duration::from_millis(50)).await;

        // ── Reader task ────────────────────────────────────────────
        let reader_shared = Arc::clone(&shared);
        let reader_start = Instant::now();
        let model = {
            let guard = reader_shared.read().await;
            guard.default_model.clone()
        };
        let reader_elapsed = reader_start.elapsed();

        // The reader must complete fast — well under the writer's
        // total runtime (≥400 ms). 100 ms is a generous ceiling that
        // still catches the regression (the old in-guard sync write
        // would have blocked the reader for the full 400 ms+).
        assert!(
            reader_elapsed < Duration::from_millis(150),
            "reader was blocked for {:?} — Config::save must not hold the lock during disk I/O",
            reader_elapsed
        );
        // Reader observed either the pre-save value or the post-save
        // value — both are acceptable because the writer hasn't
        // necessarily landed yet.
        assert!(
            matches!(model.as_deref(), Some("model-X") | Some("model-Y")),
            "unexpected default_model observed by reader: {:?}",
            model
        );

        // Let the writer finish; subsequent reads must see model-Y.
        writer.await.expect("writer join");
        let after = shared.read().await.default_model.clone();
        assert_eq!(after.as_deref(), Some("model-Y"));

        // And the on-disk file must reflect the new value too.
        let on_disk =
            std::fs::read_to_string(tmp.path().join("config.json")).expect("read config.json");
        assert!(on_disk.contains("model-Y"));
    }
}

// Test-only helpers: stash on Config to keep the public API clean while
// letting the atomic-save test mutate something observable across saves.
#[cfg(test)]
impl Config {
    fn data_dir_default_model_for_test(&mut self, model: String) {
        self.default_model = Some(model);
    }
}
