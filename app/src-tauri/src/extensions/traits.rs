//! Trait surface for Provider Extensions.
//!
//! - `ProviderExtension` is the always-required base trait.
//! - Optional capability traits (`UsageProvider` to start) are exposed
//!   via accessor methods on `ProviderExtension`. Adding a new
//!   capability is: declare a new trait + add an accessor.
//!
//! Visibility in the topbar is **not** a trait method — it's a user
//! preference (`ExtensionUserSettings.show_in_topbar`) to avoid a dual
//! source of truth.

use async_trait::async_trait;

use super::context::ExtensionContext;
use super::types::{ExtensionError, ExtensionManifest, UsageSnapshot};

/// Required base trait. Every extension implements this and returns a
/// manifest plus optional capability views.
pub trait ProviderExtension: Send + Sync + 'static {
    fn manifest(&self) -> ExtensionManifest;

    /// Return `Some(self)` if the extension can produce usage snapshots.
    /// Default: `None` (no capability).
    fn usage_provider(&self) -> Option<&dyn UsageProvider> {
        None
    }

    // Reserved for future capability accessors:
    //   fn billing_provider(&self) -> Option<&dyn BillingProvider> { None }
    //   fn rate_limit_probe(&self) -> Option<&dyn RateLimitProbe> { None }
    //   fn model_catalog(&self) -> Option<&dyn ModelCatalog> { None }
}

/// Capability: produce a `UsageSnapshot` from one network or local probe.
#[async_trait]
pub trait UsageProvider: Send + Sync {
    /// Run one fetch. Must not hold any external locks across `await`
    /// points — see the lock-ordering invariant on `ExtensionContext`.
    async fn fetch(&self, ctx: &ExtensionContext) -> Result<UsageSnapshot, ExtensionError>;

    /// How often the poller should re-fetch (seconds).
    ///
    /// Retained for backward compatibility with existing extension
    /// implementations. As of the global poll interval feature, the
    /// poller reads `context.poll_interval_secs()` (the user's
    /// Settings-panel value) instead of this per-extension method.
    /// Subclasses that override this still compile but the return
    /// value is not used by the poller.
    #[allow(dead_code)]
    fn refresh_interval_secs(&self) -> u64 {
        300
    }
}
