//! Centralised, env-overridable tunables for the Hyvemind backend.
//!
//! Every knob a power user might want to tweak without rebuilding lives here.
//! Values are read from the process environment on every call, so an integration
//! test (or a user) can `std::env::set_var(...)` before invoking the relevant
//! subsystem and the new value takes effect immediately.
//!
//! ## Why functions and not `Lazy<T>` / `OnceLock<T>`
//!
//! Earlier drafts of this module used `once_cell::sync::Lazy` to cache the
//! parsed value on first read. That works fine in production but it bakes
//! whatever the env contained *at first access* into the binary for the rest
//! of the process — which makes overriding from a unit test fragile (the test
//! must run before any other code touches the tunable). Function-based reads
//! sidestep the cache problem entirely. The cost is one `getenv` + parse per
//! call, which is fine because every tunable here is read at construction /
//! initialisation time (`PiManager::new`, `ReviewEngine::new`, startup log
//! pruning) — never in a hot loop.
//!
//! All env var names are prefixed `HYVEMIND_` so they cannot collide with
//! provider keys (`ANTHROPIC_API_KEY`, etc.) or generic Tauri / Rust tooling
//! variables.

use std::str::FromStr;
use std::time::Duration;

/// Generic helper — read `name` from the environment and parse as `T`,
/// falling back to `default` if the variable is unset, empty, or unparseable.
pub fn from_env<T: FromStr>(name: &str, default: T) -> T {
    std::env::var(name)
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(default)
}

// ---------------------------------------------------------------------------
// Pi process pool
// ---------------------------------------------------------------------------

/// Ceiling for concurrent Pi subprocesses. Sized for "leave a swarm running
/// for hours" workloads where many features may be in flight at once.
///
/// Env: `HYVEMIND_PI_MAX_PROCESSES`, default `30`.
pub fn pi_max_processes() -> usize {
    from_env("HYVEMIND_PI_MAX_PROCESSES", 30)
}

// ---------------------------------------------------------------------------
// Hivemind review engine
// ---------------------------------------------------------------------------

/// Maximum number of concurrent model calls dispatched by the Hivemind
/// `ReviewEngine` across a single round.
///
/// Env: `HYVEMIND_CONCURRENCY_CAP`, default `8`.
pub fn hivemind_concurrency_cap() -> usize {
    from_env("HYVEMIND_CONCURRENCY_CAP", 8)
}

/// Default per-round timeout (seconds) when a `RoundConfig` does not supply
/// its own. The maximum across all rounds is recorded as the job's overall
/// timeout in the SQLite store.
///
/// Env: `HYVEMIND_ROUND_TIMEOUT_SECS`, default `450`.
pub fn hivemind_round_timeout_secs() -> u64 {
    from_env("HYVEMIND_ROUND_TIMEOUT_SECS", 450)
}

/// Default `max_tokens` for provider calls that don't supply their own
/// (currently the Anthropic provider — the OpenAI-compatible adapter omits
/// the field entirely and lets each upstream pick its default).
///
/// Env: `HYVEMIND_DEFAULT_MAX_TOKENS`, default `4096`.
pub fn default_max_tokens() -> u32 {
    from_env("HYVEMIND_DEFAULT_MAX_TOKENS", 4096)
}

// ---------------------------------------------------------------------------
// Response cache (scout / queen Hivemind reviews build local caches)
// ---------------------------------------------------------------------------

/// Maximum entries kept in the per-review response cache.
///
/// Env: `HYVEMIND_RESPONSE_CACHE_SIZE`, default `1000`.
pub fn response_cache_size() -> u64 {
    from_env("HYVEMIND_RESPONSE_CACHE_SIZE", 1000)
}

/// Per-entry TTL for the per-review response cache.
///
/// Env: `HYVEMIND_RESPONSE_CACHE_TTL_SECS`, default `3600` (1 hour).
pub fn response_cache_ttl_secs() -> u64 {
    from_env("HYVEMIND_RESPONSE_CACHE_TTL_SECS", 3600)
}

/// Convenience wrapper returning [`response_cache_ttl_secs`] as a [`Duration`].
pub fn response_cache_ttl() -> Duration {
    Duration::from_secs(response_cache_ttl_secs())
}

// ---------------------------------------------------------------------------
// Swarm scheduling
// ---------------------------------------------------------------------------

/// Upper clamp on `model_settings.max_concurrent_features` for an individual
/// swarm. The user-supplied value is clamped to `max(1, min(N, this))` before
/// being baked into the `QueenConfig`. Sized to match the Pi pool semaphore so
/// a single swarm can't starve other subsystems.
///
/// Env: `HYVEMIND_SWARM_FEATURE_PARALLELISM`, default `6`.
pub fn swarm_feature_parallelism_max() -> usize {
    from_env("HYVEMIND_SWARM_FEATURE_PARALLELISM", 6)
}

// ---------------------------------------------------------------------------
// Debug logging
// ---------------------------------------------------------------------------

/// Retention window (days) for files under `~/.hyvemind/debug/`. Anything
/// older than this is pruned on startup.
///
/// Env: `HYVEMIND_DEBUG_LOG_RETENTION_DAYS`, default `7`.
pub fn debug_log_retention_days() -> u64 {
    from_env("HYVEMIND_DEBUG_LOG_RETENTION_DAYS", 7)
}

/// Bounded capacity for the async tracing log channel that feeds the per-ID
/// routing layer's writer thread. Overflow drops events rather than blocking
/// the runtime.
///
/// Env: `HYVEMIND_LOG_CHANNEL_CAPACITY`, default `4096`.
pub fn log_channel_capacity() -> usize {
    from_env("HYVEMIND_LOG_CHANNEL_CAPACITY", 4096)
}

// ---------------------------------------------------------------------------
// Circuit breaker (per-provider)
// ---------------------------------------------------------------------------

/// Consecutive failures required to open the per-provider circuit breaker.
///
/// Env: `HYVEMIND_CIRCUIT_BREAKER_THRESHOLD`, default `5`.
pub fn circuit_breaker_threshold() -> u32 {
    from_env("HYVEMIND_CIRCUIT_BREAKER_THRESHOLD", 5)
}

/// Cooldown (seconds) the breaker stays Open before transitioning to HalfOpen
/// and probing the upstream.
///
/// Env: `HYVEMIND_CIRCUIT_BREAKER_COOLDOWN_SECS`, default `60`.
pub fn circuit_breaker_cooldown_secs() -> u64 {
    from_env("HYVEMIND_CIRCUIT_BREAKER_COOLDOWN_SECS", 60)
}

/// Convenience wrapper returning [`circuit_breaker_cooldown_secs`] as a
/// [`Duration`].
pub fn circuit_breaker_cooldown() -> Duration {
    Duration::from_secs(circuit_breaker_cooldown_secs())
}

// ---------------------------------------------------------------------------
// HTTP provider client timeouts
// ---------------------------------------------------------------------------

/// Default request timeout (seconds) for HTTP-based providers (Anthropic and
/// the OpenAI-compatible adapter used for OpenAI / OpenRouter / Ollama).
///
/// Env: `HYVEMIND_PROVIDER_TIMEOUT_SECS`, default `120`.
pub fn provider_timeout_secs() -> u64 {
    from_env("HYVEMIND_PROVIDER_TIMEOUT_SECS", 120)
}

/// Convenience wrapper returning [`provider_timeout_secs`] as a [`Duration`].
pub fn provider_timeout() -> Duration {
    Duration::from_secs(provider_timeout_secs())
}

// ---------------------------------------------------------------------------
// Nurse subsystem
// ---------------------------------------------------------------------------

/// Generic "read env var, parse as T, clamp into `[min, max]`" helper. Used
/// by every `HYVEMIND_NURSE_*` accessor so out-of-range values land on the
/// nearest bound rather than rejecting the input outright.
fn clamped_from_env<T>(name: &str, default: T, min: T, max: T) -> T
where
    T: FromStr + Copy + PartialOrd,
{
    let v = from_env(name, default);
    if v < min {
        min
    } else if v > max {
        max
    } else {
        v
    }
}

/// Capacity of the `tokio::sync::broadcast` Nurse bus that fans Pi events
/// out to detectors.
///
/// Env: `HYVEMIND_NURSE_BUS_CAPACITY`, default `4096`, clamp `[64, 65_536]`.
pub fn nurse_bus_capacity() -> usize {
    clamped_from_env("HYVEMIND_NURSE_BUS_CAPACITY", 4096, 64, 65_536)
}

/// Maximum per-event evidence-byte cap enforced on every `PiEvent` before
/// it is wrapped into a `NurseBusEvent::Event` and published. Caps the
/// worst-case broadcast-ring memory footprint.
///
/// Env: `HYVEMIND_NURSE_MAX_EVIDENCE_BYTES`, default `8192`,
/// clamp `[1024, 65_536]`.
pub fn nurse_max_evidence_bytes() -> usize {
    clamped_from_env("HYVEMIND_NURSE_MAX_EVIDENCE_BYTES", 8192, 1024, 65_536)
}

/// Default `Stalled` threshold (seconds) for the `StallDetector` when no
/// per-profile override exists.
///
/// Env: `HYVEMIND_NURSE_STALL_THRESHOLD_SECS`, default `180`,
/// clamp `[60, 3600]`.
pub fn nurse_stall_threshold_secs() -> u64 {
    clamped_from_env("HYVEMIND_NURSE_STALL_THRESHOLD_SECS", 180, 60, 3600)
}

/// Cadence (seconds) for the main `NurseEngine` periodic tick, which runs
/// Fast-tick detectors and the stale-session cleanup sweep.
///
/// Env: `HYVEMIND_NURSE_TICK_INTERVAL_SECS`, default `10`,
/// clamp `[5, 600]`.
pub fn nurse_tick_interval_secs() -> u64 {
    clamped_from_env("HYVEMIND_NURSE_TICK_INTERVAL_SECS", 10, 5, 600)
}

/// Timeout (seconds) on every Nurse classifier provider call. Preserves the
/// historical `NURSE_PROVIDER_TIMEOUT_SECS = 90` baked into the old
/// `nurse_service.rs`.
///
/// Env: `HYVEMIND_NURSE_PROVIDER_TIMEOUT_SECS`, default `90`,
/// clamp `[10, 600]`.
pub fn nurse_provider_timeout_secs() -> u64 {
    clamped_from_env("HYVEMIND_NURSE_PROVIDER_TIMEOUT_SECS", 90, 10, 600)
}

/// Cadence (seconds) for the dedicated `slow_probe_task` that runs
/// `Slow`-tick detectors (process-health liveness, provider-state snapshot
/// queries) off the main loop.
///
/// Env: `HYVEMIND_NURSE_SLOW_PROBE_INTERVAL_SECS`, default `10`,
/// clamp `[5, 600]`.
pub fn nurse_slow_probe_interval_secs() -> u64 {
    clamped_from_env("HYVEMIND_NURSE_SLOW_PROBE_INTERVAL_SECS", 10, 5, 600)
}

/// Retention window (days) for `~/.hyvemind/debug/nurse/decisions.jsonl.*`
/// and the matching per-decision `captures/{decision_id}-{prompt,response}.txt`
/// files.
///
/// Env: `HYVEMIND_NURSE_DECISION_LOG_RETENTION_DAYS`, default `30`,
/// clamp `[1, 365]`.
pub fn nurse_decision_log_retention_days() -> u64 {
    clamped_from_env("HYVEMIND_NURSE_DECISION_LOG_RETENTION_DAYS", 30, 1, 365)
}

/// Per-session `signals/{session_id}.jsonl` rotation threshold (bytes).
/// Rotates to `.1` / `.2` / `.3` when crossed; only the three most recent
/// files are retained per session.
///
/// Env: `HYVEMIND_NURSE_SIGNAL_STREAM_MAX_BYTES`, default `4194304` (4 MiB),
/// clamp `[65_536, 67_108_864]`.
pub fn nurse_signal_stream_max_bytes() -> u64 {
    clamped_from_env(
        "HYVEMIND_NURSE_SIGNAL_STREAM_MAX_BYTES",
        4 * 1024 * 1024,
        65_536,
        64 * 1024 * 1024,
    )
}

/// Retention window (days) for `~/.hyvemind/debug/nurse/bus.jsonl.*`.
///
/// Env: `HYVEMIND_NURSE_BUS_LOG_RETENTION_DAYS`, default `30`,
/// clamp `[1, 365]`.
pub fn nurse_bus_log_retention_days() -> u64 {
    clamped_from_env("HYVEMIND_NURSE_BUS_LOG_RETENTION_DAYS", 30, 1, 365)
}

/// Per-capture-file (prompt or response) cap in bytes. Captures larger than
/// this are truncated with an explicit marker.
///
/// Env: `HYVEMIND_NURSE_CAPTURE_MAX_BYTES`, default `1048576` (1 MiB),
/// clamp `[16_384, 16_777_216]`.
pub fn nurse_capture_max_bytes() -> u64 {
    clamped_from_env(
        "HYVEMIND_NURSE_CAPTURE_MAX_BYTES",
        1024 * 1024,
        16_384,
        16 * 1024 * 1024,
    )
}

/// Bounded depth of the `mpsc::channel` from the engine to each
/// observability writer task. Overflow drops the newest record and
/// increments `NurseHealth.observability_dropped`.
///
/// Env: `HYVEMIND_NURSE_OBSERVABILITY_QUEUE_DEPTH`, default `2048`,
/// clamp `[128, 65_536]`.
pub fn nurse_observability_queue_depth() -> usize {
    clamped_from_env(
        "HYVEMIND_NURSE_OBSERVABILITY_QUEUE_DEPTH",
        2048,
        128,
        65_536,
    )
}

/// Cadence (seconds) for the batched Nurse review sweep. Every tick the
/// `BatchReviewer` snapshots the recent transcript of every active
/// streaming session, batches them into a SINGLE LLM call, parses
/// per-session decisions, and dispatches them through the existing
/// applier pipeline. Designed to catch repetition / stuck loops that
/// the heuristic detectors miss because the symptom isn't a clean
/// siphash match.
///
/// Env: `HYVEMIND_NURSE_BATCH_INTERVAL_SECS`, default `120`,
/// clamp `[30, 3600]`.
pub fn nurse_batch_interval_secs() -> u64 {
    clamped_from_env("HYVEMIND_NURSE_BATCH_INTERVAL_SECS", 120, 30, 3600)
}

/// Maximum number of `PiEvent`s pulled from each session's recent
/// transcript when building a batched-review prompt.
///
/// Env: `HYVEMIND_NURSE_BATCH_EVENTS_PER_SESSION`, default `200`,
/// clamp `[20, 1000]`.
pub fn nurse_batch_events_per_session() -> usize {
    clamped_from_env("HYVEMIND_NURSE_BATCH_EVENTS_PER_SESSION", 200, 20, 1000)
}

/// Per-session character cap for the rendered transcript block in a
/// batched-review prompt. Total prompt size scales linearly with the
/// number of active sessions, so this is the dominant lever for batch
/// cost control. 3000 chars ≈ 750 tokens per session.
///
/// Env: `HYVEMIND_NURSE_BATCH_CHARS_PER_SESSION`, default `3000`,
/// clamp `[500, 30_000]`.
pub fn nurse_batch_chars_per_session() -> usize {
    clamped_from_env("HYVEMIND_NURSE_BATCH_CHARS_PER_SESSION", 3000, 500, 30_000)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper: clear a var, set it to `value`, run `f`, then clear again.
    /// Avoids leaking state between tests — but note that env mutation
    /// is process-global, so tests in this module are intentionally given
    /// distinct env var names to avoid races with parallel test execution.
    fn with_env<R>(name: &str, value: &str, f: impl FnOnce() -> R) -> R {
        // SAFETY: `set_var`/`remove_var` are unsafe in 2024 edition; this
        // crate is on 2021 where they're safe. The block below scopes the
        // change to one test, but tests still run in parallel with other
        // tests in the binary — see the unique-name convention.
        std::env::set_var(name, value);
        let out = f();
        std::env::remove_var(name);
        out
    }

    #[test]
    fn from_env_returns_default_when_unset() {
        // Unique name to avoid collision with concurrent tests.
        std::env::remove_var("HYVEMIND_TEST_UNSET_KEY_5_8");
        let v: usize = from_env("HYVEMIND_TEST_UNSET_KEY_5_8", 42);
        assert_eq!(v, 42);
    }

    #[test]
    fn from_env_parses_valid_value() {
        with_env("HYVEMIND_TEST_VALID_KEY_5_8", "99", || {
            let v: usize = from_env("HYVEMIND_TEST_VALID_KEY_5_8", 42);
            assert_eq!(v, 99);
        });
    }

    #[test]
    fn from_env_falls_back_on_unparseable_value() {
        with_env("HYVEMIND_TEST_BAD_KEY_5_8", "not-a-number", || {
            let v: usize = from_env("HYVEMIND_TEST_BAD_KEY_5_8", 7);
            assert_eq!(v, 7);
        });
    }

    #[test]
    fn from_env_falls_back_on_empty_value() {
        with_env("HYVEMIND_TEST_EMPTY_KEY_5_8", "", || {
            let v: usize = from_env("HYVEMIND_TEST_EMPTY_KEY_5_8", 13);
            assert_eq!(v, 13);
        });
    }

    /// Verifies that a real tunable accessor honours an env override at
    /// runtime — this is the property the centralisation buys power users.
    /// Uses a tunable that is unlikely to be set in CI; if your CI sets
    /// `HYVEMIND_PI_MAX_PROCESSES`, swap to a less common one.
    #[test]
    fn pi_max_processes_honours_env_override() {
        // Snapshot the default first, then override.
        std::env::remove_var("HYVEMIND_PI_MAX_PROCESSES");
        assert_eq!(pi_max_processes(), 30, "default should be 30");

        with_env("HYVEMIND_PI_MAX_PROCESSES", "12", || {
            assert_eq!(pi_max_processes(), 12);
        });

        // Cleared again — default restored.
        assert_eq!(pi_max_processes(), 30);
    }

    #[test]
    fn nurse_env_vars_clamp_to_safe_ranges() {
        // bus capacity
        std::env::remove_var("HYVEMIND_NURSE_BUS_CAPACITY");
        assert_eq!(nurse_bus_capacity(), 4096);
        with_env("HYVEMIND_NURSE_BUS_CAPACITY", "0", || {
            assert_eq!(nurse_bus_capacity(), 64);
        });
        with_env("HYVEMIND_NURSE_BUS_CAPACITY", "999999", || {
            assert_eq!(nurse_bus_capacity(), 65_536);
        });
        with_env("HYVEMIND_NURSE_BUS_CAPACITY", "512", || {
            assert_eq!(nurse_bus_capacity(), 512);
        });

        // evidence bytes
        with_env("HYVEMIND_NURSE_MAX_EVIDENCE_BYTES", "100", || {
            assert_eq!(nurse_max_evidence_bytes(), 1024);
        });
        with_env("HYVEMIND_NURSE_MAX_EVIDENCE_BYTES", "999999", || {
            assert_eq!(nurse_max_evidence_bytes(), 65_536);
        });

        // stall threshold
        with_env("HYVEMIND_NURSE_STALL_THRESHOLD_SECS", "0", || {
            assert_eq!(nurse_stall_threshold_secs(), 60);
        });
        with_env("HYVEMIND_NURSE_STALL_THRESHOLD_SECS", "999999", || {
            assert_eq!(nurse_stall_threshold_secs(), 3600);
        });

        // tick interval
        with_env("HYVEMIND_NURSE_TICK_INTERVAL_SECS", "1", || {
            assert_eq!(nurse_tick_interval_secs(), 5);
        });

        // provider timeout
        with_env("HYVEMIND_NURSE_PROVIDER_TIMEOUT_SECS", "5", || {
            assert_eq!(nurse_provider_timeout_secs(), 10);
        });

        // slow probe interval
        with_env("HYVEMIND_NURSE_SLOW_PROBE_INTERVAL_SECS", "0", || {
            assert_eq!(nurse_slow_probe_interval_secs(), 5);
        });
    }

    #[test]
    fn duration_wrappers_compose_correctly() {
        std::env::remove_var("HYVEMIND_CIRCUIT_BREAKER_COOLDOWN_SECS");
        assert_eq!(circuit_breaker_cooldown(), Duration::from_secs(60));

        with_env("HYVEMIND_CIRCUIT_BREAKER_COOLDOWN_SECS", "5", || {
            assert_eq!(circuit_breaker_cooldown(), Duration::from_secs(5));
        });
    }
}
