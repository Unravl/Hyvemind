//! Per-swarm context artifacts read from disk and injected into agent prompts.
//!
//! At runtime, a swarm may have any of the following optional files in its
//! directory (`~/.hyvemind/swarms/<id>/`):
//!
//! - `AGENTS.md` — project conventions, off-limits paths, testing rules
//! - `notes.md` — architecture overview, env/dependency notes, validation
//!   surfaces (the consolidated replacement for Factory's `library/*.md`
//!   triplet)
//! - `services.yaml` — named shell commands (install/test/build/…) plus
//!   ambient service definitions (databases, queues, …)
//!
//! All three are optional. `SwarmContext::load_for` reads whatever exists and
//! returns a value whose fields default to `None` / empty so downstream
//! prompt assembly can skip cleanly when nothing is present.

use anyhow::Result;
use std::sync::Arc;

use crate::core::services::ServicesFile;
use crate::state::store::SwarmStore;

/// Bundle of optional on-disk artifacts that inject per-swarm context into
/// agent prompts (currently Worker, eventually Guard / readiness checks).
///
/// Every field is independently optional. An "all-None" `SwarmContext` is
/// the legacy / backwards-compatible case — Workers should produce exactly
/// the prompt they did before this struct existed.
#[derive(Debug, Clone, Default)]
pub struct SwarmContext {
    pub agents_md: Option<String>,
    pub notes_md: Option<String>,
    pub services: Option<ServicesFile>,
}

impl SwarmContext {
    /// Load whatever swarm-context artifacts exist for `swarm_id`.
    ///
    /// All three reads degrade to `None` when the underlying file is absent;
    /// the only way this returns `Err` is if a file *exists* but cannot be
    /// read or parsed (genuine corruption or I/O failure). Even in that case
    /// callers may prefer to discard the error and proceed with `empty()`
    /// rather than fail the feature — Phase 3 context is a best-effort
    /// enhancement, never a hard requirement.
    pub async fn load_for(store: &Arc<SwarmStore>, swarm_id: &str) -> Result<Self> {
        Ok(Self {
            agents_md: store.read_agents_md(swarm_id).await?,
            notes_md: store.read_notes_md(swarm_id).await?,
            services: store.read_services_yaml(swarm_id).await?,
        })
    }

    /// Returns `true` when none of the three artifacts are present.
    pub fn is_empty(&self) -> bool {
        self.agents_md.is_none()
            && self.notes_md.is_none()
            && self
                .services
                .as_ref()
                .map(|s| s.commands.is_empty() && s.services.is_empty())
                .unwrap_or(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::services::{Service, ServicesFile};
    use std::collections::HashMap;

    #[test]
    fn test_empty_is_empty() {
        let ctx = SwarmContext::default();
        assert!(ctx.is_empty());
    }

    #[test]
    fn test_with_agents_md_not_empty() {
        let ctx = SwarmContext {
            agents_md: Some("hi".to_string()),
            ..Default::default()
        };
        assert!(!ctx.is_empty());
    }

    #[test]
    fn test_services_with_only_empty_collections_is_empty() {
        let ctx = SwarmContext {
            services: Some(ServicesFile::default()),
            ..Default::default()
        };
        // An empty ServicesFile counts as "no context to inject".
        assert!(ctx.is_empty());
    }

    #[test]
    fn test_services_with_commands_not_empty() {
        let mut commands = HashMap::new();
        commands.insert("test".to_string(), "cargo test".to_string());
        let ctx = SwarmContext {
            services: Some(ServicesFile {
                commands,
                services: Vec::new(),
            }),
            ..Default::default()
        };
        assert!(!ctx.is_empty());
    }

    #[test]
    fn test_services_with_services_not_empty() {
        let svc = Service {
            name: "pg".to_string(),
            port: Some(5432),
            ..Default::default()
        };
        let ctx = SwarmContext {
            services: Some(ServicesFile {
                commands: HashMap::new(),
                services: vec![svc],
            }),
            ..Default::default()
        };
        assert!(!ctx.is_empty());
    }
}
