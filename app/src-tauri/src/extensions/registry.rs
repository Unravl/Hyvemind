//! In-memory registry of registered Provider Extensions.
//!
//! Keyed by composite `extension_id` (`type_id:provider_id`) for O(1)
//! lookup. Rejects duplicate IDs rather than silently overwriting —
//! callers handle the error explicitly.

use std::collections::HashMap;
use std::sync::Arc;

use super::traits::ProviderExtension;
use super::types::{ExtensionError, ExtensionManifest};

#[derive(Default)]
pub struct ExtensionRegistry {
    entries: HashMap<String, Arc<dyn ProviderExtension>>,
}

impl std::fmt::Debug for ExtensionRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let ids: Vec<&String> = self.entries.keys().collect();
        f.debug_struct("ExtensionRegistry")
            .field("entries", &ids)
            .finish()
    }
}

impl ExtensionRegistry {
    pub fn new() -> Self {
        Self {
            entries: HashMap::new(),
        }
    }

    /// Register a new extension. Returns `Err` if an extension with the
    /// same `manifest.id` is already registered.
    pub fn register(
        &mut self,
        extension: Arc<dyn ProviderExtension>,
    ) -> Result<(), ExtensionError> {
        let manifest = extension.manifest();
        if self.entries.contains_key(&manifest.id) {
            return Err(ExtensionError::Internal(format!(
                "duplicate extension id: {}",
                manifest.id
            )));
        }
        self.entries.insert(manifest.id, extension);
        Ok(())
    }

    /// Look up an extension by its composite `extension_id`.
    pub fn get(&self, extension_id: &str) -> Option<Arc<dyn ProviderExtension>> {
        self.entries.get(extension_id).cloned()
    }

    /// Return all manifests, sorted deterministically by `extension_id`.
    pub fn manifests(&self) -> Vec<ExtensionManifest> {
        let mut out: Vec<_> = self.entries.values().map(|e| e.manifest()).collect();
        out.sort_by(|a, b| a.id.cmp(&b.id));
        out
    }

    /// Iterate all registered extensions in deterministic order
    /// (sorted by extension_id).
    pub fn iter_sorted(&self) -> Vec<(String, Arc<dyn ProviderExtension>)> {
        let mut pairs: Vec<_> = self
            .entries
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        pairs.sort_by(|a, b| a.0.cmp(&b.0));
        pairs
    }

    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}
