//! Tenant-scoped idempotency keys.
//!
//! Provides a mechanism for safely retrying requests without
//! introducing duplicate side effects. Keys are scoped to a tenant,
//! meaning the same logical key can be used across different tenants.
//!
//! # Lock-poison policy
//!
//! All `std::sync::Mutex::lock()` calls in this module follow the
//! project-wide standard of `unwrap_or_else(|e| e.into_inner())`.
//! Poisoning here means a previous thread panicked while holding the
//! lock — but the protected data (a `HashMap` of consumed keys) is
//! still structurally valid: at worst a single insert was half-done,
//! and the worst case for an idempotency store is one extra
//! `DuplicateKey` rejection (which is a safe failure mode). Panicking
//! on every subsequent caller would be strictly worse than continuing
//! with the data we have.
//!
//! # Example
//!
//! ```rust,ignore
//! use hyvemind::core::idempotency::{IdempotencyKey, IdempotencyStore};
//!
//! let store = IdempotencyStore::new();
//! let key = IdempotencyKey::new("tenant-1", "request-001");
//!
//! // First use succeeds
//! assert!(store.consume(key.clone()).is_ok());
//!
//! // Duplicate for the same tenant+key fails
//! assert!(store.consume(key).is_err());
//!
//! // Same key in a different tenant is allowed
//! let other = IdempotencyKey::new("tenant-2", "request-001");
//! assert!(store.consume(other).is_ok());
//! ```

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Mutex;

// ---------------------------------------------------------------------------
// IdempotencyKey
// ---------------------------------------------------------------------------

/// A tenant-scoped idempotency key.
///
/// Two keys are equal only when both `tenant_id` and `key` match.
/// This ensures that different tenants cannot interfere with each
/// other's idempotency guarantees.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct IdempotencyKey {
    tenant_id: String,
    key: String,
}

impl IdempotencyKey {
    /// Create a new tenant-scoped idempotency key.
    ///
    /// Accepts any type that implements `Into<String>` (e.g. `&str`,
    /// `String`) for both parameters.
    pub fn new(tenant_id: impl Into<String>, key: impl Into<String>) -> Self {
        Self {
            tenant_id: tenant_id.into(),
            key: key.into(),
        }
    }

    /// The tenant that this key belongs to.
    pub fn tenant_id(&self) -> &str {
        &self.tenant_id
    }

    /// The raw idempotency key value (unique within a tenant).
    pub fn key(&self) -> &str {
        &self.key
    }
}

// ---------------------------------------------------------------------------
// IdempotencyError
// ---------------------------------------------------------------------------

/// Errors that can occur when interacting with an [`IdempotencyStore`].
#[derive(Debug, Clone, Serialize, Deserialize, thiserror::Error)]
pub enum IdempotencyError {
    /// The key has already been consumed for this tenant.
    #[error("duplicate idempotency key '{key}' for tenant '{tenant_id}'")]
    DuplicateKey {
        tenant_id: String,
        key: String,
    },

    /// Internal store error (e.g. poisoned lock).
    #[error("internal store error: {0}")]
    Internal(String),
}

// ---------------------------------------------------------------------------
// IdempotencyStore
// ---------------------------------------------------------------------------

/// An in-memory store for tenant-scoped idempotency keys.
///
/// Thread-safe via interior mutability. All public methods take `&self`.
///
/// The store tracks which keys have been consumed. Attempting to consume
/// a key a second time (same `tenant_id` *and* `key`) returns
/// [`IdempotencyError::DuplicateKey`].
#[derive(Debug)]
pub struct IdempotencyStore {
    keys: Mutex<HashMap<IdempotencyKey, bool>>,
}

impl IdempotencyStore {
    /// Create a new empty store.
    pub fn new() -> Self {
        Self {
            keys: Mutex::new(HashMap::new()),
        }
    }

    /// Check whether a key has already been consumed.
    pub fn contains(&self, key: &IdempotencyKey) -> bool {
        // Lock-poison policy: recover and continue. See module docs.
        self.keys
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains_key(key)
    }

    /// Try to consume an idempotency key.
    ///
    /// # Returns
    ///
    /// - `Ok(true)` — the key was newly inserted (first time seen).
    /// - `Err(IdempotencyError::DuplicateKey)` — the key was already
    ///   consumed (same tenant + same key).
    pub fn consume(&self, key: IdempotencyKey) -> Result<bool, IdempotencyError> {
        // Lock-poison policy: recover and continue. See module docs.
        let mut keys = self.keys.lock().unwrap_or_else(|e| e.into_inner());

        if keys.contains_key(&key) {
            return Err(IdempotencyError::DuplicateKey {
                tenant_id: key.tenant_id.clone(),
                key: key.key,
            });
        }

        keys.insert(key, true);
        Ok(true)
    }

    /// Remove a key from the store.
    ///
    /// Returns `true` if the key was present and removed.
    pub fn remove(&self, key: &IdempotencyKey) -> bool {
        // Lock-poison policy: recover and continue. See module docs.
        self.keys
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(key)
            .is_some()
    }

    /// Remove all keys from the store.
    pub fn clear(&self) {
        // Lock-poison policy: recover and continue. See module docs.
        self.keys
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
    }

    /// Return the number of keys currently tracked.
    pub fn len(&self) -> usize {
        // Lock-poison policy: recover and continue. See module docs.
        self.keys
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .len()
    }

    /// Returns `true` when no keys are tracked.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl Default for IdempotencyStore {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // --- IdempotencyKey ---

    #[test]
    fn test_key_new_with_str_slices() {
        let key = IdempotencyKey::new("tenant-1", "req-001");
        assert_eq!(key.tenant_id(), "tenant-1");
        assert_eq!(key.key(), "req-001");
    }

    #[test]
    fn test_key_new_with_strings() {
        let tenant = String::from("acme-corp");
        let key_val = String::from("txn-123");
        let key = IdempotencyKey::new(tenant, key_val);
        assert_eq!(key.tenant_id(), "acme-corp");
        assert_eq!(key.key(), "txn-123");
    }

    #[test]
    fn test_key_equality() {
        let a = IdempotencyKey::new("t1", "k1");
        let b = IdempotencyKey::new("t1", "k1");
        let c = IdempotencyKey::new("t1", "k2");
        let d = IdempotencyKey::new("t2", "k1");

        assert_eq!(a, b);
        assert_ne!(a, c);
        assert_ne!(a, d);
    }

    #[test]
    fn test_key_serialize_roundtrip() {
        let key = IdempotencyKey::new("tenant-1", "req-001");
        let json = serde_json::to_string(&key).unwrap();
        let deserialized: IdempotencyKey = serde_json::from_str(&json).unwrap();
        assert_eq!(key, deserialized);
    }

    // --- IdempotencyStore ---

    #[test]
    fn test_consume_first_time_ok() {
        let store = IdempotencyStore::new();
        let key = IdempotencyKey::new("tenant-1", "req-001");

        let result = store.consume(key);
        assert!(result.is_ok());
        assert!(result.unwrap());
    }

    #[test]
    fn test_duplicate_key_is_err() {
        let store = IdempotencyStore::new();
        let key = IdempotencyKey::new("tenant-1", "req-001");

        // First consume should succeed
        assert!(store.consume(key.clone()).is_ok());

        // Second consume of the same key should fail with DuplicateKey
        let result = store.consume(key);
        assert!(result.is_err(), "expected Err for duplicate key");
        assert!(
            matches!(result, Err(IdempotencyError::DuplicateKey { .. })),
            "expected DuplicateKey error variant"
        );
    }

    #[test]
    fn test_tenant_scoping_same_key_different_tenants() {
        let store = IdempotencyStore::new();
        let key_t1 = IdempotencyKey::new("tenant-1", "req-001");
        let key_t2 = IdempotencyKey::new("tenant-2", "req-001");

        // Same logical key in different tenants — both should succeed
        assert!(store.consume(key_t1).is_ok());
        assert!(store.consume(key_t2).is_ok());
    }

    #[test]
    fn test_tenant_scoping_same_key_same_tenant_err() {
        let store = IdempotencyStore::new();

        assert!(store.consume(IdempotencyKey::new("tenant-1", "req-001")).is_ok());
        assert!(store.consume(IdempotencyKey::new("tenant-1", "req-001")).is_err());
    }

    #[test]
    fn test_contains() {
        let store = IdempotencyStore::new();
        let key = IdempotencyKey::new("tenant-1", "req-001");

        assert!(!store.contains(&key));
        store.consume(key.clone()).unwrap();
        assert!(store.contains(&key));
    }

    #[test]
    fn test_remove() {
        let store = IdempotencyStore::new();
        let key = IdempotencyKey::new("tenant-1", "req-001");

        store.consume(key.clone()).unwrap();
        assert!(store.contains(&key));

        assert!(store.remove(&key));
        assert!(!store.contains(&key));

        // Removing a non-existent key returns false
        assert!(!store.remove(&key));
    }

    #[test]
    fn test_clear() {
        let store = IdempotencyStore::new();
        store.consume(IdempotencyKey::new("t1", "k1")).unwrap();
        store.consume(IdempotencyKey::new("t1", "k2")).unwrap();
        assert_eq!(store.len(), 2);

        store.clear();
        assert_eq!(store.len(), 0);
        assert!(store.is_empty());
    }

    #[test]
    fn test_empty_store() {
        let store = IdempotencyStore::new();
        assert!(store.is_empty());
        assert_eq!(store.len(), 0);
    }

    #[test]
    fn test_duplicate_key_error_contents() {
        let store = IdempotencyStore::new();
        let key = IdempotencyKey::new("tenant-1", "req-001");
        store.consume(key).unwrap();

        let dup = IdempotencyKey::new("tenant-1", "req-001");
        let result = store.consume(dup);
        match result {
            Err(IdempotencyError::DuplicateKey { tenant_id, key }) => {
                assert_eq!(tenant_id, "tenant-1");
                assert_eq!(key, "req-001");
            }
            other => panic!("expected DuplicateKey, got {:?}", other),
        }
    }

    #[test]
    fn test_duplicate_key_error_display() {
        let err = IdempotencyError::DuplicateKey {
            tenant_id: "acme".into(),
            key: "txn-42".into(),
        };
        let msg = err.to_string();
        assert!(msg.contains("acme"));
        assert!(msg.contains("txn-42"));
    }

    #[test]
    fn test_default_store_is_empty() {
        let store = IdempotencyStore::default();
        assert!(store.is_empty());
    }

    #[test]
    fn test_multiple_tenants_independent() {
        let store = IdempotencyStore::new();

        // Consume keys across several tenants
        for tenant in &["t1", "t2", "t3"] {
            for key_id in 1..=3 {
                let key = IdempotencyKey::new(*tenant, format!("key-{:02}", key_id));
                assert!(store.consume(key).is_ok(), "first consume should succeed");
            }
        }

        // Now all keys exist
        assert_eq!(store.len(), 9);

        // Re-consume any key should fail
        let dup = IdempotencyKey::new("t2", "key-02");
        assert!(store.consume(dup).is_err());

        // Key in a different tenant still works (already consumed earlier — err)
        let other = IdempotencyKey::new("t1", "key-02");
        assert!(store.consume(other).is_err());
    }
}
