//! Typed `IpcError` envelope returned from `#[tauri::command]` handlers.
//!
//! Replaces ad-hoc `Result<T, String>` boundaries with a structured envelope
//! the frontend can pattern-match on for user-facing messaging while still
//! preserving rich diagnostic context for support and Sentry.
//!
//! # Wire shape
//!
//! Serialized JSON envelope:
//!
//! ```json
//! {
//!   "kind": "not_found",
//!   "resource": "swarm",
//!   "resource_id": "swarm-abc",
//!   "message": "swarm 'swarm-abc' not found",
//!   "id": null,
//!   "details": null
//! }
//! ```
//!
//! `kind` is always present (discriminator); per-variant payload fields are
//! flattened next to it; `message`, `id`, and `details` are the envelope
//! fields shared by every variant. `details` may carry an `anyhow` error
//! chain or any other diagnostic JSON.
//!
//! # Why not just `String`?
//!
//! Free-form strings force the frontend to substring-match against backend
//! prose for any behavioural branching ("does this look like a 401? a not-
//! found? a circuit breaker open?"). The envelope makes that branching
//! explicit, stable, and migration-safe.
//!
//! # Note on the `NotFound { kind, id }` variant
//!
//! The audit plan (5.3) names the inner fields `kind` and `id`. Both would
//! collide with the discriminator `kind` (we use serde's internally-tagged
//! representation, so `kind` is the variant tag) and with the envelope's
//! own `id` field after flattening. We rename them to `resource` and
//! `resource_id` to keep the on-wire JSON valid and unambiguous; behaviour
//! and intent are unchanged.

use std::borrow::Cow;

use serde::Serialize;

/// Structured error envelope returned by every Tauri command in `commands/`.
#[derive(Debug, Serialize, thiserror::Error)]
pub struct IpcError {
    /// Discriminator + per-variant payload, flattened into the envelope.
    #[serde(flatten)]
    pub kind: IpcErrorKind,
    /// Human-readable summary suitable for surfacing in toasts / error modals.
    pub message: String,
    /// Optional entity id (session_id / swarm_id / review_id) the error
    /// pertains to. Frontend uses this to route the error back to the
    /// relevant UI surface (close the right modal, mark the right list row).
    pub id: Option<String>,
    /// Free-form diagnostic JSON. Populated by `From<anyhow::Error>` with
    /// `{"chain": [...]}` so support has the full error chain without us
    /// having to pre-format it. May carry per-call-site context too.
    pub details: Option<serde_json::Value>,
}

impl std::fmt::Display for IpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

/// Variant discriminator. Serialized as `{ "kind": "<variant>", ... }`.
#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum IpcErrorKind {
    /// The provider rejected the request because the API key is missing,
    /// invalid, or revoked (HTTP 401 / 403). The frontend should prompt the
    /// user to re-enter credentials.
    ProviderUnauthenticated,
    /// The provider returned 429 / rate-limit-exhausted. The frontend can
    /// suggest waiting or switching models.
    ProviderRateLimited,
    /// The per-provider circuit breaker is currently open; new calls are
    /// being short-circuited locally until the cooldown expires.
    CircuitBreakerOpen,
    /// A requested entity (swarm, session, review, hivemind, ...) does not
    /// exist or has already been torn down.
    NotFound {
        /// Resource category — `"swarm"`, `"session"`, `"review"`, etc.
        ///
        /// Renamed from the plan's `kind` to `resource` to avoid colliding
        /// with the variant discriminator `kind` after flattening.
        resource: String,
        /// Resource identifier the lookup was performed against.
        ///
        /// Renamed from the plan's `id` to `resource_id` to avoid colliding
        /// with the envelope's `id` field after flattening.
        resource_id: String,
    },
    /// Input validation failed (path traversal, empty id, oversized payload,
    /// malformed body, etc.). The error is user-attributable; retrying with
    /// the same input will fail the same way.
    Validation,
    /// User has not approved the requested action (e.g. working directory
    /// not in the allowlist; subscription auth missing). Frontend can show
    /// the corresponding consent modal.
    NotApproved,
    /// Catch-all for anything else (I/O error, internal logic bug, panic,
    /// upstream provider 5xx that isn't a rate limit). The `details.chain`
    /// field typically carries the full `anyhow` error chain.
    Internal,
}

impl IpcError {
    /// Build a `NotFound` envelope. Convenience constructor that fills in a
    /// reasonable default `message` (`"<resource> '<id>' not found"`).
    pub fn not_found(resource: impl Into<String>, id: impl Into<String>) -> Self {
        let resource = resource.into();
        let resource_id = id.into();
        let message = format!("{} '{}' not found", resource, resource_id);
        IpcError {
            kind: IpcErrorKind::NotFound {
                resource,
                resource_id: resource_id.clone(),
            },
            message,
            // The envelope-level `id` mirrors the resource id so frontend
            // routing helpers can read a single field regardless of variant.
            id: Some(resource_id),
            details: None,
        }
    }

    /// Build a `Validation` envelope.
    pub fn validation(message: impl Into<String>) -> Self {
        IpcError {
            kind: IpcErrorKind::Validation,
            message: message.into(),
            id: None,
            details: None,
        }
    }

    /// Build a `NotApproved` envelope.
    pub fn not_approved(message: impl Into<String>) -> Self {
        IpcError {
            kind: IpcErrorKind::NotApproved,
            message: message.into(),
            id: None,
            details: None,
        }
    }

    /// Build an `Internal` envelope from a stringy message.
    pub fn internal(message: impl Into<String>) -> Self {
        IpcError {
            kind: IpcErrorKind::Internal,
            message: message.into(),
            id: None,
            details: None,
        }
    }

    /// Build a `ProviderUnauthenticated` envelope.
    pub fn provider_unauthenticated(message: impl Into<String>) -> Self {
        IpcError {
            kind: IpcErrorKind::ProviderUnauthenticated,
            message: message.into(),
            id: None,
            details: None,
        }
    }

    /// Build a `ProviderRateLimited` envelope.
    pub fn provider_rate_limited(message: impl Into<String>) -> Self {
        IpcError {
            kind: IpcErrorKind::ProviderRateLimited,
            message: message.into(),
            id: None,
            details: None,
        }
    }

    /// Build a `CircuitBreakerOpen` envelope.
    pub fn circuit_breaker_open(message: impl Into<String>) -> Self {
        IpcError {
            kind: IpcErrorKind::CircuitBreakerOpen,
            message: message.into(),
            id: None,
            details: None,
        }
    }

    /// Attach an `id` to this envelope (mutating builder).
    pub fn with_id(mut self, id: impl Into<String>) -> Self {
        self.id = Some(id.into());
        self
    }

    /// Map a stringy provider/Pi error into a best-guess typed envelope.
    ///
    /// Inspects the error body for the well-known markers Pi / our provider
    /// layer emits (`"401"`, `"invalid_api_key"`, `"rate_limit_error"`,
    /// `"circuit breaker open"`) and degrades to `Internal` otherwise. Used
    /// by command call sites that hold a flat `String` from a deeper layer.
    pub fn from_provider_error(message: impl Into<String>) -> Self {
        let msg: String = message.into();
        let lower = msg.to_ascii_lowercase();
        if lower.contains("circuit breaker open") || lower.contains("circuit_breaker_open") {
            return Self::circuit_breaker_open(msg);
        }
        if lower.contains("rate_limit_error")
            || lower.contains("rate limited")
            || lower.contains("429")
        {
            return Self::provider_rate_limited(msg);
        }
        if lower.contains("401")
            || lower.contains("invalid_api_key")
            || lower.contains("authentication_error")
            || lower.contains("unauthorized")
        {
            return Self::provider_unauthenticated(msg);
        }
        Self::internal(msg)
    }
}

/// `anyhow::Error` → `IpcError` lifts the full error chain into
/// `details.chain` for diagnostic value while keeping the user-facing
/// `message` as the top-level error string.
impl From<anyhow::Error> for IpcError {
    fn from(e: anyhow::Error) -> Self {
        let chain: Vec<String> = e.chain().map(ToString::to_string).collect();
        let message = e.to_string();
        IpcError {
            kind: IpcErrorKind::Internal,
            message,
            id: None,
            details: Some(serde_json::json!({ "chain": chain })),
        }
    }
}

/// `std::io::Error` → `IpcError::Internal`. Carries `kind` (e.g. `"NotFound"`,
/// `"PermissionDenied"`) in `details` so the frontend can adapt the toast
/// when needed (most callers just surface `message`).
impl From<std::io::Error> for IpcError {
    fn from(e: std::io::Error) -> Self {
        let io_kind = format!("{:?}", e.kind());
        IpcError {
            kind: IpcErrorKind::Internal,
            message: e.to_string(),
            id: None,
            details: Some(serde_json::json!({ "io_kind": io_kind })),
        }
    }
}

/// `PiManagerError` → typed envelope. Maps `SessionNotFound` to `NotFound`
/// and everything else to `Internal`.
impl From<crate::pi::manager::PiManagerError> for IpcError {
    fn from(e: crate::pi::manager::PiManagerError) -> Self {
        use crate::pi::manager::PiManagerError as E;
        match &e {
            E::SessionNotFound { session_id } => IpcError::not_found("session", session_id.clone()),
            E::SessionExists { session_id } => {
                IpcError::validation(format!("session '{}' already exists", session_id))
                    .with_id(session_id.clone())
            }
            E::BinaryNotFound { path } => {
                IpcError::internal(format!("pi binary not found at {}", path.display()))
            }
            E::Rpc(_) => IpcError::from_provider_error(e.to_string()),
        }
    }
}

/// Convenience so existing `?` paths that used `String` continue to compile
/// when the underlying error is already a `String` (e.g. helper validators
/// like `validate_id` that still return `Result<(), String>`).
impl From<String> for IpcError {
    fn from(s: String) -> Self {
        IpcError::internal(s)
    }
}

impl From<&str> for IpcError {
    fn from(s: &str) -> Self {
        IpcError::internal(s.to_string())
    }
}

impl From<Cow<'_, str>> for IpcError {
    fn from(s: Cow<'_, str>) -> Self {
        IpcError::internal(s.into_owned())
    }
}

/// Required for Tauri 2's command return-type machinery: anything returned
/// as `Err(_)` from a `#[tauri::command]` must `Serialize`. `IpcError`
/// already does; this is a sanity-typed compatibility shim so callers can
/// `?` through `serde_json::Error` (e.g. ad-hoc JSON construction inside a
/// command body) without ceremony.
impl From<serde_json::Error> for IpcError {
    fn from(e: serde_json::Error) -> Self {
        IpcError::internal(format!("json error: {}", e))
    }
}

// Note: `Serialize for &IpcError` is provided automatically by the blanket
// `impl<'a, T: ?Sized + Serialize> Serialize for &'a T` in serde, so no
// explicit reference impl is needed here.

#[cfg(test)]
mod tests {
    use super::*;

    /// `NotFound` constructor populates `resource`, `resource_id`, the
    /// envelope `id`, and a default `message` in one shot.
    #[test]
    fn not_found_constructor_sets_all_fields() {
        let e = IpcError::not_found("swarm", "swarm-abc");
        match e.kind {
            IpcErrorKind::NotFound {
                ref resource,
                ref resource_id,
            } => {
                assert_eq!(resource, "swarm");
                assert_eq!(resource_id, "swarm-abc");
            }
            _ => panic!("expected NotFound variant"),
        }
        assert_eq!(e.id.as_deref(), Some("swarm-abc"));
        assert!(e.message.contains("swarm"));
        assert!(e.message.contains("swarm-abc"));
    }

    /// `Validation` constructor sets the kind and preserves the message
    /// without touching `id` / `details`.
    #[test]
    fn validation_constructor_preserves_message() {
        let e = IpcError::validation("bad input");
        assert!(matches!(e.kind, IpcErrorKind::Validation));
        assert_eq!(e.message, "bad input");
        assert!(e.id.is_none());
        assert!(e.details.is_none());
    }

    /// `From<anyhow::Error>` should lift the full chain into
    /// `details.chain` and degrade `kind` to `Internal`.
    #[test]
    fn from_anyhow_includes_chain_in_details() {
        let root = std::io::Error::new(std::io::ErrorKind::PermissionDenied, "denied");
        let wrapped = anyhow::Error::from(root).context("opening file");
        let outer = wrapped.context("reading config");

        let envelope: IpcError = outer.into();

        assert!(matches!(envelope.kind, IpcErrorKind::Internal));
        let details = envelope
            .details
            .expect("anyhow chain should populate details");
        let chain = details
            .get("chain")
            .and_then(|v| v.as_array())
            .expect("details.chain must be an array");
        // outer "reading config", inner "opening file", root "denied"
        assert_eq!(chain.len(), 3);
        assert_eq!(chain[0].as_str(), Some("reading config"));
        assert_eq!(chain[1].as_str(), Some("opening file"));
        assert_eq!(chain[2].as_str(), Some("denied"));
    }

    /// A 401 from the provider layer must be classified as
    /// `ProviderUnauthenticated`.
    #[test]
    fn provider_401_maps_to_provider_unauthenticated() {
        let e = IpcError::from_provider_error(
            r#"401 {"type":"error","error":{"type":"authentication_error","message":"x"}}"#,
        );
        assert!(matches!(e.kind, IpcErrorKind::ProviderUnauthenticated));
    }

    /// A rate-limit response maps to `ProviderRateLimited`.
    #[test]
    fn provider_429_maps_to_provider_rate_limited() {
        let e = IpcError::from_provider_error(
            r#"429 {"type":"error","error":{"type":"rate_limit_error","message":"x"}}"#,
        );
        assert!(matches!(e.kind, IpcErrorKind::ProviderRateLimited));
    }

    /// Local circuit-breaker errors map to `CircuitBreakerOpen`.
    #[test]
    fn circuit_breaker_maps_to_circuit_breaker_open() {
        let e = IpcError::from_provider_error("circuit breaker open for provider anthropic");
        assert!(matches!(e.kind, IpcErrorKind::CircuitBreakerOpen));
    }

    /// `PiManagerError::SessionNotFound` lifts cleanly into the
    /// `NotFound { resource: "session", ... }` variant — this is the
    /// canonical mapping a command using `?` would produce.
    #[test]
    fn pi_session_not_found_maps_to_not_found_envelope() {
        let pi_err = crate::pi::manager::PiManagerError::SessionNotFound {
            session_id: "sess-xyz".to_string(),
        };
        let e: IpcError = pi_err.into();
        match e.kind {
            IpcErrorKind::NotFound {
                ref resource,
                ref resource_id,
            } => {
                assert_eq!(resource, "session");
                assert_eq!(resource_id, "sess-xyz");
            }
            _ => panic!("expected NotFound variant"),
        }
        assert_eq!(e.id.as_deref(), Some("sess-xyz"));
    }

    /// JSON shape: discriminator `kind` is flattened next to the envelope
    /// fields. Verifies no nested `{kind:{...}}` and that the renamed
    /// `resource` / `resource_id` payload appears at the top level.
    #[test]
    fn serializes_with_flat_kind_discriminator() {
        let e = IpcError::not_found("swarm", "abc");
        let v = serde_json::to_value(&e).expect("serialize");
        assert_eq!(v["kind"].as_str(), Some("not_found"));
        assert_eq!(v["resource"].as_str(), Some("swarm"));
        assert_eq!(v["resource_id"].as_str(), Some("abc"));
        assert_eq!(v["id"].as_str(), Some("abc"));
        assert!(v["message"].is_string());
    }

    /// Unit variants serialize to just `{ "kind": "...", "message": ... }`
    /// (no extra payload keys).
    #[test]
    fn unit_variant_serializes_without_extra_payload_keys() {
        let e = IpcError::provider_unauthenticated("bad key");
        let v = serde_json::to_value(&e).expect("serialize");
        assert_eq!(v["kind"].as_str(), Some("provider_unauthenticated"));
        assert_eq!(v["message"].as_str(), Some("bad key"));
        // No leak of variant-internal fields.
        assert!(v.get("resource").is_none());
        assert!(v.get("resource_id").is_none());
    }

    /// `with_id` builder composes cleanly.
    #[test]
    fn builders_compose() {
        let e = IpcError::validation("bad input").with_id("task-1");
        assert_eq!(e.id.as_deref(), Some("task-1"));
    }

    /// `From<String>` and `From<&str>` keep `?` ergonomic at call sites that
    /// still hold a flat string error.
    #[test]
    fn from_string_and_str_degrade_to_internal() {
        let s: IpcError = "boom".to_string().into();
        assert!(matches!(s.kind, IpcErrorKind::Internal));
        assert_eq!(s.message, "boom");

        let r: IpcError = "boom2".into();
        assert!(matches!(r.kind, IpcErrorKind::Internal));
        assert_eq!(r.message, "boom2");
    }
}
