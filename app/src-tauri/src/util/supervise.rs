//! Panic-safety harness for fire-and-forget `tokio::spawn` bodies.
//!
//! ## Why
//!
//! A bare `tokio::spawn(async move { … })` swallows panics: the runtime catches
//! the unwind, logs it (only at INFO via tokio's default `EnvFilter`), and the
//! task's `JoinHandle::is_panicked()` flips — but nobody is awaiting that
//! handle for the fire-and-forget pipelines this app relies on (Queen
//! orchestrator, chat-event forwarder, Pi stdout/stderr/monitor tasks, etc.).
//!
//! The visible symptom is a phantom spinner in the UI: the spawn that owned
//! the response stream silently died, no synthetic error reaches the frontend,
//! the spawn-side state machine never gets the "I failed" tick. Audit item
//! 2.12 calls this out and asks for a thin wrapper that
//!
//! 1. catches the panic so the runtime sees a clean exit,
//! 2. logs it at ERROR with whatever ID context is in scope (swarm_id,
//!    session_id, review_id),
//! 3. runs a caller-supplied cleanup closure so the spawn site can emit the
//!    structured `error` event the UI is waiting on (and, where applicable,
//!    flip the persistent entity to `Failed`).
//!
//! ## API
//!
//! [`supervise!`] is the entry point. The body must be `UnwindSafe` or
//! wrappable in [`std::panic::AssertUnwindSafe`] — we apply
//! `AssertUnwindSafe` for the caller because every fire-and-forget body in
//! this codebase already owns its captured state (the panic does not leak
//! anything we did not already own, and the cleanup closure is responsible
//! for putting the world back in a consistent shape).
//!
//! ```ignore
//! use crate::supervise;
//!
//! tokio::spawn(supervise!(
//!     // The "context" — anything `Display`. Stamped into the ERROR log.
//!     context = format!("swarm={swarm_id}"),
//!     // Optional cleanup that runs only on panic. Receives the panic
//!     // message as a `String`. Use it to emit a synthetic error event
//!     // and/or mark a persistent entity Failed.
//!     on_panic = |panic_msg| {
//!         let _ = app_for_panic.emit("swarm-event", serde_json::json!({
//!             "swarm_id": swarm_id_for_panic,
//!             "event_type": "failed",
//!             "message": format!("internal task panicked: {panic_msg}"),
//!         }));
//!     },
//!     // The body. Exactly what you would have written inside `async move`.
//!     async move {
//!         // … original spawn body …
//!     }
//! ));
//! ```
//!
//! The body form without `on_panic =` is also valid — only ERROR-level
//! logging happens in that case.
//!
//! ## Test injection
//!
//! [`panic_for_test`] is a const-stable helper that simply panics. The audit
//! tests use it via a `cfg(test)` injection point inside each instrumented
//! spawn body so we can drive the supervisor without contriving real
//! upstream failures.

use std::future::Future;
use std::panic::AssertUnwindSafe;

use futures::FutureExt;

/// Run a future to completion with panic catching applied. On panic, return
/// the panic message as `Err(String)`; otherwise return `Ok(value)`.
///
/// Most call sites should use the [`supervise!`] macro rather than this
/// function — the macro layers on the logging and cleanup-closure behaviour
/// the audit requires.
pub async fn run_supervised<F, T>(fut: F) -> Result<T, String>
where
    F: Future<Output = T>,
{
    match AssertUnwindSafe(fut).catch_unwind().await {
        Ok(value) => Ok(value),
        Err(payload) => Err(panic_payload_to_string(payload)),
    }
}

/// Convert a `catch_unwind` payload into a human-readable string. Matches the
/// formatting used by the standard panic hook so log messages line up.
pub fn panic_payload_to_string(payload: Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = payload.downcast_ref::<&'static str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "<non-string panic payload>".to_string()
    }
}

/// Trigger a deterministic panic from inside an instrumented spawn body.
/// Only ever invoked when `HYVEMIND_PANIC_INJECT` matches `marker`.
///
/// All callers of this helper are `#[cfg(test)]`-gated, so the function
/// itself is too: in a release / lib build the env-var probe and its
/// stack frame don't exist at all.
#[cfg(test)]
#[inline]
pub fn maybe_panic_for_test(marker: &str) {
    if let Ok(want) = std::env::var("HYVEMIND_PANIC_INJECT") {
        if want == marker {
            panic!("HYVEMIND_PANIC_INJECT={marker}: forced panic for supervise test");
        }
    }
}

/// Force an unconditional panic. Used by unit tests that drive
/// `run_supervised` / the macro directly without needing the env-var dance.
#[allow(dead_code)]
pub fn panic_for_test(marker: &str) -> ! {
    panic!("forced panic for supervise test: {marker}");
}

/// Outcome of a [`super_watchdog`] run, used by tests + tracing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SuperWatchdogOutcome {
    /// The first incarnation exited cleanly — no respawn happened.
    CleanFirstExit,
    /// The first incarnation panicked / failed to join; the respawn then
    /// exited cleanly.
    RespawnSucceeded,
    /// Both the first incarnation and the respawn died. The caller has
    /// already logged the unrecoverable error.
    FatalSecondCrash,
}

/// Generic respawn-once super-watchdog used at the outermost Nurse spawn
/// site in `lib.rs` (audit 2.12).
///
/// Behaviour:
/// 1. Awaits the supplied `first_handle`.
/// 2. If it joined cleanly, returns `CleanFirstExit`.
/// 3. If it panicked or failed to join, logs at ERROR, then calls
///    `respawn` to spawn a second incarnation and awaits that handle.
/// 4. If the second incarnation exits cleanly, returns `RespawnSucceeded`.
/// 5. If it ALSO crashes, logs the explicit "unrecoverable" marker at
///    ERROR and returns `FatalSecondCrash`. The caller is expected to
///    have logged enough context (e.g. "nurse unrecoverable …") via the
///    pre-emit prefix string.
///
/// The respawn closure is invoked **only** when needed — production
/// nurse code passes a closure that re-calls `NurseService::start(...)`.
pub async fn super_watchdog<F>(
    component: &str,
    first_handle: tokio::task::JoinHandle<()>,
    respawn: F,
) -> SuperWatchdogOutcome
where
    F: FnOnce() -> tokio::task::JoinHandle<()>,
{
    match first_handle.await {
        Ok(()) => {
            tracing::info!(
                component = %component,
                "super_watchdog: first incarnation exited cleanly"
            );
            return SuperWatchdogOutcome::CleanFirstExit;
        }
        Err(e) if e.is_panic() => {
            tracing::error!(
                component = %component,
                error = %e,
                "super_watchdog: first incarnation PANICKED — respawning ONCE"
            );
        }
        Err(e) => {
            tracing::error!(
                component = %component,
                error = %e,
                "super_watchdog: first incarnation join failed — respawning ONCE"
            );
        }
    }

    let respawn_handle = respawn();
    match respawn_handle.await {
        Ok(()) => {
            tracing::info!(
                component = %component,
                "super_watchdog: respawn exited cleanly"
            );
            SuperWatchdogOutcome::RespawnSucceeded
        }
        Err(e) => {
            tracing::error!(
                component = %component,
                error = %e,
                "super_watchdog: {component} unrecoverable — respawn ALSO crashed. \
                 Restart the app to recover.",
            );
            SuperWatchdogOutcome::FatalSecondCrash
        }
    }
}

/// Wrap a fire-and-forget `tokio::spawn` body so a panic is caught, logged at
/// ERROR with the supplied ID context, and (optionally) handed to a cleanup
/// closure that can emit a synthetic frontend event and mark persistent state
/// `Failed`.
///
/// See the [module-level docs](self) for the rationale and full examples.
///
/// ### Forms
///
/// ```ignore
/// supervise!(context = "swarm=abc", body)
/// supervise!(context = "swarm=abc", on_panic = |msg| { … }, body)
/// ```
///
/// The body must be an `async` block expression (most call sites already had
/// `async move { … }` — the macro re-uses the block verbatim).
#[macro_export]
macro_rules! supervise {
    (
        context = $ctx:expr,
        on_panic = $cleanup:expr,
        $body:expr $(,)?
    ) => {{
        async move {
            let __ctx = $ctx;
            let __res = $crate::util::supervise::run_supervised(async move { $body.await }).await;
            if let ::std::result::Result::Err(__panic_msg) = __res {
                ::tracing::error!(
                    context = %__ctx,
                    panic = %__panic_msg,
                    "supervised task PANICKED — invoking cleanup",
                );
                // The cleanup closure is `FnOnce(String)`.
                let __cleanup = $cleanup;
                (__cleanup)(__panic_msg);
            }
        }
    }};
    (
        context = $ctx:expr,
        $body:expr $(,)?
    ) => {{
        async move {
            let __ctx = $ctx;
            let __res = $crate::util::supervise::run_supervised(async move { $body.await }).await;
            if let ::std::result::Result::Err(__panic_msg) = __res {
                ::tracing::error!(
                    context = %__ctx,
                    panic = %__panic_msg,
                    "supervised task PANICKED",
                );
            }
        }
    }};
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    use super::*;

    #[tokio::test]
    async fn run_supervised_returns_ok_for_non_panicking_future() {
        let v = run_supervised(async { 7_u32 }).await.unwrap();
        assert_eq!(v, 7);
    }

    #[tokio::test]
    async fn run_supervised_captures_panic_string() {
        let res = run_supervised(async { panic!("boom-str") }).await;
        let err = res.expect_err("expected Err on panic");
        assert!(err.contains("boom-str"), "got: {err}");
    }

    #[tokio::test]
    async fn run_supervised_captures_panic_static_str() {
        let res = run_supervised(async {
            panic_for_test("boom-static");
        })
        .await;
        let err = res.expect_err("expected Err on panic");
        assert!(err.contains("boom-static"), "got: {err}");
    }

    #[tokio::test]
    async fn supervise_macro_runs_cleanup_on_panic() {
        let counter = Arc::new(AtomicUsize::new(0));
        let counter_in = counter.clone();
        let captured = Arc::new(std::sync::Mutex::new(String::new()));
        let captured_in = captured.clone();

        let supervised = supervise!(
            context = "test-cleanup-context",
            on_panic = move |msg: String| {
                counter_in.fetch_add(1, Ordering::SeqCst);
                *captured_in.lock().unwrap() = msg;
            },
            async move { panic_for_test("kaboom") }
        );

        tokio::spawn(supervised).await.unwrap();

        assert_eq!(counter.load(Ordering::SeqCst), 1, "cleanup should run once");
        let got = captured.lock().unwrap().clone();
        assert!(got.contains("kaboom"), "captured panic msg: {got}");
    }

    #[tokio::test]
    async fn supervise_macro_skips_cleanup_on_success() {
        let counter = Arc::new(AtomicUsize::new(0));
        let counter_in = counter.clone();

        let supervised = supervise!(
            context = "test-skip-cleanup",
            on_panic = move |_msg: String| {
                counter_in.fetch_add(1, Ordering::SeqCst);
            },
            async move { /* normal exit */ }
        );

        tokio::spawn(supervised).await.unwrap();

        assert_eq!(
            counter.load(Ordering::SeqCst),
            0,
            "cleanup must not run on success"
        );
    }

    #[tokio::test]
    async fn supervise_macro_without_cleanup_does_not_crash_runtime() {
        let supervised = supervise!(context = "no-cleanup-form", async move {
            panic_for_test("silent")
        });
        // The whole point: this returns Ok even though the inner body
        // panicked.
        tokio::spawn(supervised).await.unwrap();
    }

    #[test]
    fn maybe_panic_for_test_is_inert_without_env_var() {
        // Just calling it must be a no-op when the env var is unset.
        // Use a marker the test suite cannot accidentally match.
        std::env::remove_var("HYVEMIND_PANIC_INJECT");
        maybe_panic_for_test("inert-marker-xyz");
    }

    #[tokio::test]
    async fn super_watchdog_clean_first_exit_does_not_respawn() {
        let respawned = Arc::new(AtomicUsize::new(0));
        let respawned_in = respawned.clone();
        let first = tokio::spawn(async move { /* clean exit */ });
        let outcome = super_watchdog("test_clean", first, move || {
            respawned_in.fetch_add(1, Ordering::SeqCst);
            tokio::spawn(async move { /* unused */ })
        })
        .await;
        assert_eq!(outcome, SuperWatchdogOutcome::CleanFirstExit);
        assert_eq!(
            respawned.load(Ordering::SeqCst),
            0,
            "respawn must not run after a clean first exit"
        );
    }

    #[tokio::test]
    async fn super_watchdog_respawn_succeeds_after_first_panic() {
        let respawned = Arc::new(AtomicUsize::new(0));
        let respawned_in = respawned.clone();
        let first = tokio::spawn(async move { panic_for_test("crashed-first") });
        let outcome = super_watchdog("test_respawn", first, move || {
            respawned_in.fetch_add(1, Ordering::SeqCst);
            tokio::spawn(async move { /* clean */ })
        })
        .await;
        assert_eq!(outcome, SuperWatchdogOutcome::RespawnSucceeded);
        assert_eq!(
            respawned.load(Ordering::SeqCst),
            1,
            "respawn must run exactly once after first panic"
        );
    }

    #[tokio::test]
    async fn super_watchdog_fatal_when_respawn_also_panics() {
        let respawned = Arc::new(AtomicUsize::new(0));
        let respawned_in = respawned.clone();
        let first = tokio::spawn(async move { panic_for_test("crashed-first") });
        let outcome = super_watchdog("test_fatal", first, move || {
            respawned_in.fetch_add(1, Ordering::SeqCst);
            tokio::spawn(async move { panic_for_test("crashed-second") })
        })
        .await;
        assert_eq!(outcome, SuperWatchdogOutcome::FatalSecondCrash);
        assert_eq!(
            respawned.load(Ordering::SeqCst),
            1,
            "respawn must be attempted once and only once even when it panics"
        );
    }
}
