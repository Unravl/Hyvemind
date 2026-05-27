//! Sentry initialization, scrubbing, and kill-switch.
//!
//! Privacy: Hyvemind handles user prompts, source code in working directories,
//! and provider API keys. Sentry events are scrubbed in `before_send` to strip
//! API-key-shaped substrings, rewrite absolute home paths, and drop events
//! originating from modules that handle raw model traffic.

use std::borrow::Cow;
use std::sync::Arc;

use sentry::protocol::{Breadcrumb, Event};
use sentry::ClientOptions;

/// Hold this for the lifetime of the process. When dropped, Sentry flushes
/// pending events and shuts down. The inner `Option` is `None` when Sentry
/// was disabled (kill switch, opted-out, or no DSN); the field is never
/// read directly — its `Drop` is the only side-effect that matters.
pub struct SentryGuard {
    _guard: Option<sentry::ClientInitGuard>,
}

/// Initialise Sentry. Returns a guard that must be held for the process
/// lifetime. Returns an inactive guard if any of:
/// - `SENTRY_DISABLED=1` is set (kill switch)
/// - `crash_reporting_enabled` is false
/// - no DSN is available (neither baked at compile time nor in env)
pub fn init(crash_reporting_enabled: bool) -> SentryGuard {
    if std::env::var("SENTRY_DISABLED").as_deref() == Ok("1") {
        return SentryGuard { _guard: None };
    }
    if !crash_reporting_enabled {
        return SentryGuard { _guard: None };
    }

    // Compile-time bake takes precedence; env override for dev.
    let dsn: Option<String> = option_env!("SENTRY_DSN")
        .map(str::to_string)
        .or_else(|| std::env::var("SENTRY_DSN").ok())
        .filter(|s| !s.is_empty());

    let Some(dsn) = dsn else {
        return SentryGuard { _guard: None };
    };

    let environment: Cow<'static, str> = if cfg!(debug_assertions) {
        "dev".into()
    } else {
        "release".into()
    };

    let options = ClientOptions {
        dsn: dsn.parse().ok(),
        release: sentry::release_name!(),
        environment: Some(environment),
        send_default_pii: false,
        max_breadcrumbs: 50,
        attach_stacktrace: true,
        before_send: Some(Arc::new(scrub_event)),
        before_breadcrumb: Some(Arc::new(scrub_breadcrumb)),
        ..Default::default()
    };

    let guard = sentry::init(options);
    SentryGuard {
        _guard: Some(guard),
    }
}

// ── Scrubbing ─────────────────────────────────────────────────────────

/// Modules whose events are dropped entirely — they handle raw model
/// traffic (prompts, completions, request bodies) that must never leave
/// the device.
const BLOCKED_LOGGERS: &[&str] = &[
    "hyvemind::pi::rpc",
    "hyvemind::pi::events",
    "hyvemind::providers",
    "hyvemind::hivemind::merge_capture",
    "hyvemind::hivemind::output_capture",
];

fn is_blocked_logger(logger: &str) -> bool {
    BLOCKED_LOGGERS.iter().any(|b| logger.starts_with(b))
}

fn scrub_event(mut event: Event<'static>) -> Option<Event<'static>> {
    if let Some(logger) = event.logger.as_deref() {
        if is_blocked_logger(logger) {
            return None;
        }
    }

    // Scrub message
    if let Some(msg) = event.message.as_mut() {
        *msg = scrub_string(msg);
    }

    // Scrub exception values (panic messages, formatted errors).
    for ex in event.exception.values.iter_mut() {
        if let Some(v) = ex.value.as_mut() {
            *v = scrub_string(v);
        }
    }

    // Scrub log entry text (used by sentry-tracing).
    if let Some(entry) = event.logentry.as_mut() {
        entry.message = scrub_string(&entry.message);
    }

    // Drop sensitive tags + extras by key.
    event.tags.retain(|k, _| !is_sensitive_field(k));
    event.extra.retain(|k, _| !is_sensitive_field(k));

    // Scrub remaining string-shaped extras and tags.
    for (_, v) in event.tags.iter_mut() {
        *v = scrub_string(v);
    }
    for (_, v) in event.extra.iter_mut() {
        if let Some(s) = v.as_str() {
            *v = serde_json::Value::String(scrub_string(s));
        }
    }

    Some(event)
}

fn scrub_breadcrumb(mut bc: Breadcrumb) -> Option<Breadcrumb> {
    if let Some(category) = bc.category.as_deref() {
        if is_blocked_logger(category) {
            return None;
        }
    }
    if let Some(msg) = bc.message.as_mut() {
        *msg = scrub_string(msg);
    }
    bc.data.retain(|k, _| !is_sensitive_field(k));
    for (_, v) in bc.data.iter_mut() {
        if let Some(s) = v.as_str() {
            *v = serde_json::Value::String(scrub_string(s));
        }
    }
    Some(bc)
}

/// Returns true when a Sentry tag/extra key is known to carry secrets.
///
/// Delegates to `crate::state::log_redact::is_sensitive_field` so the
/// allowlist is the same one used for on-disk debug logs — audit fix 8.7
/// unified the two sets that had previously drifted out of sync.
fn is_sensitive_field(key: &str) -> bool {
    crate::state::log_redact::is_sensitive_field(key)
}

/// Strip API-key-shaped substrings and rewrite `/Users/<name>/...` to `~/...`.
///
/// Delegates to `crate::state::log_redact::redact_all` so Sentry telemetry
/// and on-disk debug logs run through the **same** pattern set. Previously
/// Sentry's regex set was broader than `log_redact`'s hand-rolled scanner;
/// audit fix 8.7 unified them.
pub(crate) fn scrub_string(input: &str) -> String {
    crate::state::log_redact::redact_all(input)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scrubs_anthropic_key() {
        let s = scrub_string("auth header: sk-ant-api03-AbCdEfGhIjKlMnOpQrStUvWxYz1234567890");
        assert!(s.contains("[redacted-api-key]"));
        assert!(!s.contains("AbCdEfGh"));
    }

    #[test]
    fn scrubs_openai_key() {
        let s = scrub_string("OPENAI_API_KEY=sk-proj-AbCdEfGhIjKlMnOpQrStUvWx");
        assert!(s.contains("[redacted-api-key]"));
    }

    #[test]
    fn scrubs_bearer_header() {
        let s = scrub_string("Authorization: Bearer eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9");
        assert!(s.contains("Bearer [redacted]"));
        assert!(!s.contains("eyJhbGc"));
    }

    #[test]
    fn rewrites_home_path() {
        let s = scrub_string("file: /Users/haydenevan/Documents/Hyvemind/app/src/main.rs");
        assert_eq!(s, "file: ~/Documents/Hyvemind/app/src/main.rs");
    }

    #[test]
    fn passes_through_safe_text() {
        let s = scrub_string("scout finished feature feat-001 in 1234 ms");
        assert_eq!(s, "scout finished feature feat-001 in 1234 ms");
    }

    #[test]
    fn blocked_logger_recognised() {
        assert!(is_blocked_logger("hyvemind::pi::rpc"));
        assert!(is_blocked_logger("hyvemind::pi::rpc::sub"));
        assert!(!is_blocked_logger("hyvemind::core::queen"));
    }

    #[test]
    fn sensitive_field_recognised() {
        assert!(is_sensitive_field("prompt"));
        assert!(is_sensitive_field("api_key"));
        assert!(!is_sensitive_field("session_id"));
    }

    #[test]
    fn identity_input_returns_unchanged() {
        // With no key-shaped substrings the output equals the input
        // (modulo home-path rewrites, which there are none of here).
        assert_eq!(
            scrub_string("plain log line with no secrets"),
            "plain log line with no secrets"
        );
    }
}
