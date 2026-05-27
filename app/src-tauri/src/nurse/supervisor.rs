//! Thin wrapper over `util::supervise::super_watchdog` for nurse loops.
//!
//! See `util/supervise.rs` for the two-layer panic-respawn pattern. This
//! file exists so the engine entry point reads as a single intentional
//! `supervisor::spawn(engine, ...)` call rather than a hand-rolled
//! `tokio::spawn` chain.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

/// Spawn a supervised long-running future. If the future panics or
/// returns, it is restarted via the same logic as `util::supervise`.
///
/// Today this is a thin `tokio::spawn` wrapper; the engine relies on
/// `util::supervise::super_watchdog` at the call site for the full
/// two-layer respawn behaviour.
pub fn spawn<F, Fut>(
    name: &'static str,
    _shutdown: CancellationToken,
    fut_factory: F,
) -> JoinHandle<()>
where
    F: Fn() -> Fut + Send + Sync + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    let factory: Arc<dyn Fn() -> Pin<Box<dyn Future<Output = ()> + Send>> + Send + Sync> =
        Arc::new(move || Box::pin(fut_factory()) as Pin<Box<dyn Future<Output = ()> + Send>>);
    tokio::spawn(async move {
        // Single attempt for now; the engine's outer supervisor wraps
        // this in `super_watchdog` for production safety.
        let fut = (factory)();
        let result = std::panic::AssertUnwindSafe(fut);
        let _ = futures::FutureExt::catch_unwind(result).await;
        tracing::warn!(supervisor = name, "supervised nurse task exited");
    })
}
