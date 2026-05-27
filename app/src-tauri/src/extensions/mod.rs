//! Provider Extension system.
//!
//! See `README.md` in this directory for the contract / author guide.
//!
//! Quick layout:
//!   - `types.rs`    — wire types (`ExtensionManifest`, `UsageSnapshot`, …)
//!   - `traits.rs`   — `ProviderExtension` + capability sub-traits
//!   - `registry.rs` — in-memory registry keyed by composite extension_id
//!   - `context.rs`  — `ExtensionContext` passed to each `fetch()` call
//!   - `poller.rs`   — per-extension polling tasks
//!   - `builtins/`   — bundled extensions

use std::collections::HashMap;
use std::sync::Arc;

use tracing::warn;

use crate::state::config::ProviderConfig;

pub mod builtins;
pub mod context;
pub mod poller;
pub mod registry;
pub mod traits;
pub mod types;

#[cfg(test)]
mod tests;

pub use context::ExtensionContext;
pub use registry::ExtensionRegistry;
#[allow(unused_imports)]
pub use traits::{ProviderExtension, UsageProvider};
#[allow(unused_imports)]
pub use types::{
    Capability, ExtensionError, ExtensionManifest, ExtensionUserSettings, MetricKind,
    SnapshotEntry, SnapshotStatus, Tone, UsageMetric, UsageSnapshot,
};

/// Single place developers edit to add a new built-in extension.
///
/// Called at startup (from `AppState::new`) and re-called whenever the
/// live provider config changes (`refresh_extension_registry`). The
/// function is **idempotent** when called against a fresh
/// `ExtensionRegistry` — duplicate IDs are rejected by `register()`.
pub fn register_builtin_extensions(
    providers: &HashMap<String, ProviderConfig>,
    registry: &mut ExtensionRegistry,
) {
    // ── OpenRouter credits ───────────────────────────────────
    // One instance per provider whose endpoint points at OpenRouter.
    for (id, pc) in providers {
        let is_openrouter_endpoint = pc
            .endpoint
            .as_deref()
            .map(|e| e.contains("openrouter.ai"))
            .unwrap_or(false);
        if id == "openrouter" || is_openrouter_endpoint {
            let ext = builtins::openrouter_credits::OpenRouterCredits::new(
                id.clone(),
                pc.endpoint.as_deref(),
            );
            if let Err(e) = registry.register(Arc::new(ext)) {
                warn!(provider_id = %id, error = %e, "failed to register OpenRouter credits extension");
            }
        }
    }

    // ── Anthropic usage (graceful unsupported) ───────────────
    for (id, pc) in providers {
        if pc.provider_type == "Anthropic" {
            let ext = builtins::anthropic_usage::AnthropicUsage::new(id.clone());
            if let Err(e) = registry.register(Arc::new(ext)) {
                warn!(provider_id = %id, error = %e, "failed to register Anthropic usage extension");
            }
        }
    }

    // ── DeepSeek balance ──────────────────────────────────────
    //
    // Registers a `deepseek_balance:{provider_id}` extension for the
    // canonical `deepseek` provider and for any provider whose endpoint
    // points at api.deepseek.com (covers custom-named provider aliases).
    //
    // NOTE on composite-key collisions: the registry key is
    // `deepseek_balance:{provider_id}`. If a user ever configures
    // **multiple** providers pointing at DeepSeek with the same
    // `provider_id`, a disambiguation suffix (e.g. a short hash of the
    // endpoint URL) must be appended. For v1 we assume a single
    // `deepseek` provider.
    for (id, pc) in providers {
        let is_deepseek_endpoint = pc
            .endpoint
            .as_deref()
            .map(|e| e.contains("api.deepseek.com"))
            .unwrap_or(false);
        if id == "deepseek" || is_deepseek_endpoint {
            let ext = builtins::deepseek_balance::DeepSeekBalance::new(
                id.clone(),
                pc.endpoint.as_deref(),
            );
            if let Err(e) = registry.register(Arc::new(ext)) {
                warn!(provider_id = %id, error = %e, "failed to register DeepSeek balance extension");
            }
        }
    }

    // ── CroF usage ────────────────────────────────────────────
    for (id, pc) in providers {
        if pc
            .endpoint
            .as_deref()
            .map(|e| e.contains("crof.ai"))
            .unwrap_or(false)
        {
            let ext = builtins::crof_usage::CrofUsage::new(id.clone(), pc.endpoint.as_deref());
            if let Err(e) = registry.register(Arc::new(ext)) {
                warn!(provider_id = %id, error = %e, "failed to register CroF usage extension");
            }
        }
    }

    // ── NeuralWatt usage ──────────────────────────────────────────
    for (id, pc) in providers {
        if id == "neuralwatt"
            || pc
                .endpoint
                .as_deref()
                .map(|e| e.contains("neuralwatt.com"))
                .unwrap_or(false)
        {
            let ext = builtins::neuralwatt_usage::NeuralWattUsage::new(
                id.clone(),
                pc.endpoint.as_deref(),
            );
            if let Err(e) = registry.register(Arc::new(ext)) {
                warn!(provider_id = %id, error = %e, "failed to register NeuralWatt usage extension");
            }
        }
    }

    // ── Claude subscription usage (OAuth /api/oauth/usage) ───
    // Only attaches to the canonical `claude-sub` Subscription provider.
    // Reads the OAuth token from ~/.pi/agent/auth.json that Pi already
    // manages; no API key required.
    for (id, pc) in providers {
        if id == "claude-sub" && pc.provider_type == "Subscription" {
            let ext = builtins::claude_sub_usage::ClaudeSubUsage::new(id.clone());
            if let Err(e) = registry.register(Arc::new(ext)) {
                warn!(provider_id = %id, error = %e,
                      "failed to register Claude subscription usage extension");
            }
        }
    }

    // ── ChatGPT (Codex) subscription usage (backend-api /wham/usage) ─
    // Only attaches to the canonical `chatgpt` Subscription provider.
    // Reads the OAuth token from ~/.pi/agent/auth.json that Pi already
    // manages; no API key required.
    for (id, pc) in providers {
        if id == "chatgpt" && pc.provider_type == "Subscription" {
            let ext = builtins::chatgpt_sub_usage::ChatGptSubUsage::new(id.clone());
            if let Err(e) = registry.register(Arc::new(ext)) {
                warn!(provider_id = %id, error = %e,
                      "failed to register ChatGPT subscription usage extension");
            }
        }
    }

    // ── Neokens balance ──────────────────────────────────────────
    // Registers a `neokens_balance:{provider_id}` extension for the
    // canonical `neokens` provider and for any provider whose endpoint
    // points at neokens.com (apex or `api.` subdomain) or
    // quatarly.cloud (the upstream inference proxy users may have
    // configured directly per the docs' quick-start).
    for (id, pc) in providers {
        let is_neokens_endpoint = pc
            .endpoint
            .as_deref()
            .map(|e| {
                let lower = e.to_lowercase();
                lower.contains("neokens.com") || lower.contains("quatarly.cloud")
            })
            .unwrap_or(false);
        if id == "neokens" || is_neokens_endpoint {
            let ext =
                builtins::neokens_balance::NeokensBalance::new(id.clone(), pc.endpoint.as_deref());
            if let Err(e) = registry.register(Arc::new(ext)) {
                warn!(provider_id = %id, error = %e, "failed to register Neokens balance extension");
            }
        }
    }
}
