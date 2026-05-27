//! Anthropic usage extension — currently a graceful-unsupported stub.
//!
//! Anthropic does not expose a per-user usage HTTP endpoint without
//! admin-only authentication. We register the extension so the UI shows
//! it as "available but inactive" and document for future Admin API
//! support.
//!
//! NOTE: this stub is for the **API-key** Anthropic provider only. The
//! Claude Pro / Max subscription (`claude-sub`) surface is handled by
//! `claude_sub_usage.rs`, which reads Pi's stored OAuth token and hits
//! the undocumented `/api/oauth/usage` endpoint Claude Code's `/usage`
//! command uses.

use async_trait::async_trait;

use crate::extensions::context::ExtensionContext;
use crate::extensions::traits::{ProviderExtension, UsageProvider};
use crate::extensions::types::{Capability, ExtensionError, ExtensionManifest, UsageSnapshot};

pub struct AnthropicUsage {
    provider_id: String,
    extension_id: String,
}

impl AnthropicUsage {
    pub fn new(provider_id: impl Into<String>) -> Self {
        let provider_id = provider_id.into();
        let extension_id = format!("anthropic_usage:{}", provider_id);
        Self {
            provider_id,
            extension_id,
        }
    }
}

impl ProviderExtension for AnthropicUsage {
    fn manifest(&self) -> ExtensionManifest {
        ExtensionManifest {
            id: self.extension_id.clone(),
            type_id: "anthropic_usage".to_string(),
            provider_id: self.provider_id.clone(),
            display_name: format!("Anthropic Usage ({})", self.provider_id),
            description: "Anthropic does not expose a per-key usage endpoint without admin auth. \
                 This extension is registered but inactive — re-enable once Admin API \
                 support lands."
                .to_string(),
            capabilities: vec![Capability::Usage],
            requires_api_key: true,
            docs_url: Some("https://docs.anthropic.com/".to_string()),
        }
    }

    fn usage_provider(&self) -> Option<&dyn UsageProvider> {
        Some(self)
    }
}

#[async_trait]
impl UsageProvider for AnthropicUsage {
    async fn fetch(&self, _ctx: &ExtensionContext) -> Result<UsageSnapshot, ExtensionError> {
        Err(ExtensionError::Unsupported(
            "Anthropic does not expose usage data via API.".to_string(),
        ))
    }
}
