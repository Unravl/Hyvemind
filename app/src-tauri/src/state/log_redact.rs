//! Unified log + telemetry redaction.
//!
//! This module is the single source of truth for secret-scrubbing across
//! Hyvemind. Two surfaces consume it:
//!
//! 1. `RedactingWriter` — wraps the `HYVEMIND_DEBUG=1` JSON file appender so
//!    any tracing event written to disk has API-key-shaped substrings,
//!    `_API_KEY`/`_TOKEN`/`_SECRET`/`_BEARER`/`_AUTH`/`_PASSWORD`/`_DSN`
//!    env-var assignments, and `/Users/<name>/...` paths scrubbed before
//!    they hit the filesystem.
//! 2. `sentry_setup::scrub_string` delegates here so Sentry telemetry uses
//!    the **same** pattern set as on-disk logs. Previously Sentry's regex
//!    set was broader than this file's hand-rolled scanner — that drift was
//!    the audit finding (Task 8.7).
//!
//! ## Why both a hand-rolled scanner AND regex?
//!
//! The original `RedactingWriter` was intentionally regex-free to avoid
//! pulling in `regex`/`once_cell`. `regex` is now in `Cargo.toml` (used by
//! Sentry scrubbing already), so we lift the patterns Sentry was relying on
//! into this module too. The hand-rolled byte scanner is kept for the
//! patterns it was already handling well (`Bearer …`, `sk-…`, `…_API_KEY=…`,
//! `x-api-key:`, `Authorization:`), and the broader Sentry-style regex set
//! runs as a second pass to cover the remaining shapes (generic `token=`,
//! `secret=`, `api-key=` with lower length threshold, etc.).
//!
//! Both passes are idempotent — running them in either order produces the
//! same final string (modulo placement of overlapping matches). The writer
//! path uses `redact_all` so on-disk debug logs and Sentry events both get
//! the same scrubbing.
//!
//! Patterns are lazily compiled exactly once (via `std::sync::OnceLock`) so
//! the hot path is allocation-free for the regex compilation step.

use std::io;
use std::sync::OnceLock;
use tracing_subscriber::fmt::MakeWriter;

const REDACTED: &str = "***REDACTED***";

/// Writer adapter that redacts secrets in each `write` call before
/// forwarding bytes to the inner writer.
///
/// IMPORTANT: tracing's JSON layer writes one full event per `write` call,
/// so scanning per-buffer is safe — we won't split a secret across two
/// writes in practice.
pub struct RedactingWriter<W: io::Write> {
    inner: W,
}

impl<W: io::Write> RedactingWriter<W> {
    pub fn new(inner: W) -> Self {
        Self { inner }
    }

    /// Mutable access to the wrapped writer (used by callers that need to
    /// reach through to the underlying file, e.g. for `sync_data()`).
    pub fn get_mut(&mut self) -> &mut W {
        &mut self.inner
    }
}

impl<W: io::Write> io::Write for RedactingWriter<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        // If buf isn't valid UTF-8 (shouldn't happen for JSON layer output),
        // fall through unchanged.
        match std::str::from_utf8(buf) {
            Ok(s) => {
                let cleaned = redact_all(s);
                self.inner.write_all(cleaned.as_bytes())?;
                // Report the original buf len as written so callers don't
                // panic on a "short write".
                Ok(buf.len())
            }
            Err(_) => self.inner.write(buf),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

/// `MakeWriter` adapter that wraps stderr with a `RedactingWriter` so the
/// always-on stderr `fmt` layer scrubs secrets before they hit the terminal
/// / journald / Console.app. Used by `lib.rs::init_tracing`.
///
/// Each tracing event triggers a `make_writer()` call which returns a fresh
/// `RedactingWriter<io::StderrLock>` — the lock is held for the duration of
/// the write (so concurrent events serialise on stderr, matching the
/// behaviour of `tracing_subscriber::fmt::layer().with_writer(io::stderr)`).
#[derive(Clone, Copy, Default)]
pub struct RedactingStderr;

impl<'a> MakeWriter<'a> for RedactingStderr {
    type Writer = RedactingWriter<io::StderrLock<'a>>;

    fn make_writer(&'a self) -> Self::Writer {
        RedactingWriter::new(io::stderr().lock())
    }
}

/// Scan `s` for likely-secret patterns and replace each match with the
/// appropriate placeholder. This is the **unified** scrubber used by both
/// the on-disk debug log writer and the Sentry telemetry pipeline.
///
/// Pass order matters — running regex first lets Sentry's preferred
/// placeholder format (`[redacted-api-key]`, `Bearer [redacted]`,
/// `$1=[redacted]`, `[redacted-dsn]`) propagate to the on-disk logs too,
/// while the hand-rolled pass-2 still catches env-var assignments like
/// `ANTHROPIC_API_KEY=sk-ant-xyz` where the secret value is too short to
/// satisfy the regex's length threshold.
///
/// Pass 1 (regex set, lifted from the old `sentry_setup` scrubbers):
/// - `sk-(?:ant-|or-|proj-)?[A-Za-z0-9_\-]{12,}`             → `[redacted-api-key]`
/// - `(?i)Bearer\s+[A-Za-z0-9._\-]{12,}`                      → `Bearer [redacted]`
/// - `(?i)(api[_-]?key|token|secret|password|auth)\s*[=:]\s*…{12,}` → `$1=[redacted]`
/// - `https?://<keyish>@host/<digits>` (Sentry DSN)           → `[redacted-dsn]`
///
/// The 12-char threshold (down from Sentry's original 16) catches shorter
/// development tokens; real provider API keys are all well above that.
///
/// Pass 2 (hand-rolled byte scanner, kept for cases regex misses):
/// - `Bearer …` with any token length (defensive)
/// - `sk-ant-…`, `sk-…{20,}` (defensive — usually already replaced)
/// - `x-api-key: …`, `Authorization: …`
/// - `IDENTIFIER_<KIND>\s*[=:]\s*<value>` for any of
///   `_API_KEY` / `_TOKEN` / `_SECRET` / `_BEARER` / `_AUTH` /
///   `_PASSWORD` / `_DSN`. **This is what catches short env-var
///   assignments the regex can't see.**
///
/// Pass 2 outputs `***REDACTED***`. When both passes run, pass-1's
/// placeholders (`[redacted-…]`) survive pass-2 because they don't look
/// like secrets — so the same string is never replaced twice.
///
/// Pass 3: home-path rewrite (`/Users/<name>/` → `~/`).
pub fn redact_all(s: &str) -> String {
    let pass1 = apply_regex_scrubbers(s);
    let pass2 = redact(&pass1);
    rewrite_home_paths(&pass2)
}

/// Legacy hand-rolled scrubber, kept as `pass 1` of `redact_all`. Exposed
/// (`pub`) for unit testing the byte scanner in isolation and for callers
/// who only want the original behaviour.
pub fn redact(s: &str) -> String {
    // Walk once; whenever a pattern matches at the cursor, emit REDACTED
    // and advance past the match. This is O(n * patterns) but n is small
    // (one log line) and patterns are few.
    let bytes = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < bytes.len() {
        if let Some(end) = match_secret_at(bytes, i) {
            out.push_str(REDACTED);
            i = end;
        } else {
            // Push one UTF-8 char and advance.
            // s is &str so byte boundaries are char boundaries when stepping
            // via char_indices; fall back to bytes if mid-char.
            let ch_len = utf8_char_len(bytes[i]);
            let end = (i + ch_len).min(bytes.len());
            // Safety: we computed end on a char boundary as long as the
            // input was valid UTF-8 (it is — &str).
            out.push_str(&s[i..end]);
            i = end;
        }
    }
    out
}

/// Sensitive-field allowlist. Returns true when a structured-data key
/// (Sentry tag/extra key, breadcrumb data key, future log field key)
/// is known to carry secret-shaped values. Mirrors the set
/// `sentry_setup::is_sensitive_field` used to maintain locally; that
/// function now delegates here so both surfaces use the same allowlist.
///
/// Matching is case-insensitive against a curated list. Keep the list
/// narrow: false positives drop legitimate diagnostic context, but a
/// missed sensitive field can leak credentials.
pub fn is_sensitive_field(key: &str) -> bool {
    let lower = key.to_ascii_lowercase();
    matches!(
        lower.as_str(),
        "prompt"
            | "output"
            | "stdin"
            | "stdout"
            | "body"
            | "payload"
            | "response"
            | "request"
            | "api_key"
            | "apikey"
            | "api-key"
            | "key"
            | "token"
            | "secret"
            | "password"
            | "bearer"
            | "auth"
            | "authorization"
            | "dsn"
            | "messages"
            | "completion"
            | "transcript"
    )
}

/// Apply the regex pattern set lifted from `sentry_setup`. Returns a new
/// String — short-circuits to the original allocation when no patterns
/// match (the `regex::Regex::replace_all` API handles this).
fn apply_regex_scrubbers(input: &str) -> String {
    let mut out: std::borrow::Cow<'_, str> = std::borrow::Cow::Borrowed(input);
    for (re, repl) in scrubbers() {
        // replace_all returns Cow::Borrowed when there are no matches; only
        // allocate when something actually changed.
        match re.replace_all(&out, *repl) {
            std::borrow::Cow::Borrowed(_) => {}
            std::borrow::Cow::Owned(new) => {
                out = std::borrow::Cow::Owned(new);
            }
        }
    }
    out.into_owned()
}

/// Rewrite absolute home directories to `~/` so on-disk logs and Sentry
/// telemetry don't leak the host username.
fn rewrite_home_paths(input: &str) -> String {
    static PATH_RE: OnceLock<regex::Regex> = OnceLock::new();
    let re = PATH_RE.get_or_init(|| regex::Regex::new(r"/Users/[^/\s]+/").unwrap());
    re.replace_all(input, "~/").into_owned()
}

/// Lazily-compiled regex set for `redact_all` pass 2.
///
/// Threshold note: Sentry's original patterns required `{16,}` characters
/// after the prefix. We drop that to `{12,}` here so short development
/// tokens (CI keys, locally generated test tokens) are also scrubbed.
/// The shortest legitimate Anthropic / OpenAI / OpenRouter key is well
/// over 20 characters so this won't false-positive on real keys; it only
/// catches more shapes of leaked dev secrets.
fn scrubbers() -> &'static [(regex::Regex, &'static str)] {
    static CELL: OnceLock<Vec<(regex::Regex, &'static str)>> = OnceLock::new();
    CELL.get_or_init(|| {
        vec![
            // OpenAI-style + Anthropic + OpenRouter API keys.
            (
                regex::Regex::new(r"sk-(?:ant-|or-|proj-)?[A-Za-z0-9_\-]{12,}").unwrap(),
                "[redacted-api-key]",
            ),
            // Generic Bearer tokens in headers / log strings.
            (
                regex::Regex::new(r"(?i)Bearer\s+[A-Za-z0-9._\-]{12,}").unwrap(),
                "Bearer [redacted]",
            ),
            // Generic long base64-ish secrets after `key=` / `token=` /
            // `secret=` / `password=` / `auth=`.
            (
                regex::Regex::new(
                    r"(?i)(api[_-]?key|token|secret|password|auth)\s*[=:]\s*[A-Za-z0-9._\-/+]{12,}",
                )
                .unwrap(),
                "$1=[redacted]",
            ),
            // Sentry-style DSN: https://<key>@host/project
            (
                regex::Regex::new(r"https?://[A-Za-z0-9_\-]{8,}@[A-Za-z0-9.\-]+/\d+").unwrap(),
                "[redacted-dsn]",
            ),
        ]
    })
}

#[inline]
fn utf8_char_len(b: u8) -> usize {
    if b < 0x80 {
        1
    } else if b < 0xC0 {
        1 // continuation byte; defensive — shouldn't start here on &str
    } else if b < 0xE0 {
        2
    } else if b < 0xF0 {
        3
    } else {
        4
    }
}

/// Try every pattern at position `i`. Returns the byte index just past the
/// end of the longest match, or `None`.
fn match_secret_at(bytes: &[u8], i: usize) -> Option<usize> {
    let candidates = [
        match_bearer(bytes, i),
        match_sk_ant(bytes, i),
        match_sk_long(bytes, i),
        match_x_api_key(bytes, i),
        match_authorization(bytes, i),
        match_env_secret(bytes, i),
    ];
    candidates.into_iter().flatten().max()
}

/// `Bearer\s+[A-Za-z0-9_\-\.]+`
fn match_bearer(bytes: &[u8], i: usize) -> Option<usize> {
    let prefix = b"Bearer";
    if !starts_with_ci(bytes, i, prefix) {
        return None;
    }
    let mut j = i + prefix.len();
    // Require at least one whitespace.
    let ws_start = j;
    while j < bytes.len() && (bytes[j] == b' ' || bytes[j] == b'\t') {
        j += 1;
    }
    if j == ws_start {
        return None;
    }
    let tok_start = j;
    while j < bytes.len() && is_token_byte(bytes[j]) {
        j += 1;
    }
    if j == tok_start {
        return None;
    }
    Some(j)
}

/// `sk-ant-[A-Za-z0-9_\-]+`
fn match_sk_ant(bytes: &[u8], i: usize) -> Option<usize> {
    let prefix = b"sk-ant-";
    if !starts_with(bytes, i, prefix) {
        return None;
    }
    let mut j = i + prefix.len();
    let start = j;
    while j < bytes.len() && is_id_byte(bytes[j]) {
        j += 1;
    }
    if j == start {
        return None;
    }
    Some(j)
}

/// `sk-[A-Za-z0-9_\-]{20,}` (but not sk-ant- which is matched separately).
fn match_sk_long(bytes: &[u8], i: usize) -> Option<usize> {
    let prefix = b"sk-";
    if !starts_with(bytes, i, prefix) {
        return None;
    }
    // Don't double-match sk-ant- (handled by match_sk_ant for clarity).
    if starts_with(bytes, i, b"sk-ant-") {
        return None;
    }
    let mut j = i + prefix.len();
    let start = j;
    while j < bytes.len() && is_id_byte(bytes[j]) {
        j += 1;
    }
    if j - start < 20 {
        return None;
    }
    Some(j)
}

/// `(?i)x-api-key\s*[:=]\s*[\"']?[A-Za-z0-9_\-\.]+`
fn match_x_api_key(bytes: &[u8], i: usize) -> Option<usize> {
    let prefix = b"x-api-key";
    if !starts_with_ci(bytes, i, prefix) {
        return None;
    }
    let j = i + prefix.len();
    match_kv_tail(bytes, j)
}

/// `(?i)authorization\s*[:=]\s*[\"']?[A-Za-z0-9_\-\.\s]+` — restricted to a
/// single token after the separator to avoid eating the rest of the line.
///
/// Special case: if the value starts with `Bearer ` we **return None** and
/// let `match_bearer` redact just the token portion. This preserves
/// `Bearer [redacted]` placeholder output (Sentry's preferred format) when
/// `redact_all` is used, which improves log readability without losing any
/// secret. The legacy direct callers of `redact()` still get the secret
/// scrubbed via `match_bearer` firing immediately after this returns.
fn match_authorization(bytes: &[u8], i: usize) -> Option<usize> {
    let prefix = b"authorization";
    if !starts_with_ci(bytes, i, prefix) {
        return None;
    }
    let j = i + prefix.len();

    // If the value after `authorization:` starts with `Bearer ` (or `Bearer\t`),
    // defer to `match_bearer` so the visible `Bearer` keyword survives.
    let mut k = j;
    while k < bytes.len() && (bytes[k] == b' ' || bytes[k] == b'\t') {
        k += 1;
    }
    if k < bytes.len() && (bytes[k] == b':' || bytes[k] == b'=') {
        k += 1;
        while k < bytes.len() && (bytes[k] == b' ' || bytes[k] == b'\t') {
            k += 1;
        }
        if starts_with_ci(bytes, k, b"Bearer")
            && k + 6 < bytes.len()
            && (bytes[k + 6] == b' ' || bytes[k + 6] == b'\t')
        {
            return None;
        }
    }

    match_kv_tail(bytes, j)
}

/// `[A-Z][A-Z0-9_]{2,}_<KIND>\s*[=:]\s*[\"']?[A-Za-z0-9_\-\.]+` where
/// `<KIND>` is one of `API_KEY`, `TOKEN`, `SECRET`, `BEARER`, `AUTH`,
/// `PASSWORD`, `DSN`. This covers things like `ANTHROPIC_API_KEY=…`,
/// `GITHUB_TOKEN=…`, `WEBHOOK_SECRET=…`, `SENTRY_DSN=…`, etc.
///
/// Note: the prior `match_env_api_key` is folded into this — it now matches
/// any of the listed suffixes.
fn match_env_secret(bytes: &[u8], i: usize) -> Option<usize> {
    // Identifier must start with uppercase letter and consist of uppercase
    // alnum + underscore.
    if i >= bytes.len() || !bytes[i].is_ascii_uppercase() {
        return None;
    }
    let mut j = i + 1;
    while j < bytes.len() {
        let b = bytes[j];
        if b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_' {
            j += 1;
        } else {
            break;
        }
    }
    let id_end = j;
    let id_len = id_end - i;
    if id_len < 4 {
        // Need at least "X_DSN" (5 chars) or "X_AUTH" (6 chars) etc. The
        // shortest suffix is "_DSN" (4 chars) so identifier itself must be
        // at least 1 + 4 = 5 chars long.
        return None;
    }
    // Check for any recognised secret suffix. Longest first so we don't
    // mis-detect `_API_KEY` as `_KEY` (which isn't even in the set, but
    // ordering longest-first is the safe habit).
    const SUFFIXES: &[&[u8]] = &[
        b"_API_KEY",
        b"_PASSWORD",
        b"_BEARER",
        b"_SECRET",
        b"_TOKEN",
        b"_AUTH",
        b"_DSN",
    ];
    let has_suffix = SUFFIXES
        .iter()
        .any(|suffix| id_len >= suffix.len() && ends_with(bytes, i, id_end, suffix));
    if !has_suffix {
        return None;
    }
    match_kv_tail(bytes, id_end)
}

/// Match `\s*[:=]\s*[\"']?<token>` and return end index.
fn match_kv_tail(bytes: &[u8], start: usize) -> Option<usize> {
    let mut j = start;
    while j < bytes.len() && (bytes[j] == b' ' || bytes[j] == b'\t') {
        j += 1;
    }
    if j >= bytes.len() {
        return None;
    }
    if bytes[j] != b':' && bytes[j] != b'=' {
        return None;
    }
    j += 1;
    while j < bytes.len() && (bytes[j] == b' ' || bytes[j] == b'\t') {
        j += 1;
    }
    if j < bytes.len() && (bytes[j] == b'"' || bytes[j] == b'\'') {
        j += 1;
    }
    let tok_start = j;
    while j < bytes.len() && is_token_byte(bytes[j]) {
        j += 1;
    }
    if j == tok_start {
        return None;
    }
    Some(j)
}

#[inline]
fn is_id_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'-'
}

#[inline]
fn is_token_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'-' || b == b'.'
}

#[inline]
fn starts_with(bytes: &[u8], i: usize, needle: &[u8]) -> bool {
    if i + needle.len() > bytes.len() {
        return false;
    }
    &bytes[i..i + needle.len()] == needle
}

#[inline]
fn starts_with_ci(bytes: &[u8], i: usize, needle: &[u8]) -> bool {
    if i + needle.len() > bytes.len() {
        return false;
    }
    bytes[i..i + needle.len()]
        .iter()
        .zip(needle)
        .all(|(a, b)| a.eq_ignore_ascii_case(b))
}

#[inline]
fn ends_with(bytes: &[u8], i: usize, end: usize, suffix: &[u8]) -> bool {
    if end < i + suffix.len() {
        return false;
    }
    &bytes[end - suffix.len()..end] == suffix
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    // ── Pass-1 (hand-rolled scanner) tests ────────────────────────────

    #[test]
    fn redacts_authorization_bearer_anthropic_key() {
        let input = "Authorization: Bearer sk-ant-abc123def456";
        let out = redact(input);
        assert!(out.contains("***REDACTED***"), "out = {}", out);
        assert!(
            !out.contains("sk-ant-abc123def456"),
            "secret leaked: {}",
            out
        );
    }

    #[test]
    fn redacts_env_api_key_assignment() {
        let input = "ANTHROPIC_API_KEY=sk-ant-xyz";
        let out = redact(input);
        assert!(out.contains("***REDACTED***"), "out = {}", out);
        assert!(!out.contains("sk-ant-xyz"), "secret leaked: {}", out);
    }

    #[test]
    fn plain_text_unchanged() {
        let input = "the quick brown fox jumps over the lazy dog";
        assert_eq!(redact(input), input);
    }

    #[test]
    fn redacts_long_sk_token() {
        let input = "key=sk-proj-abcdefghijklmnopqrstuvwxyz1234";
        let out = redact(input);
        assert!(out.contains("***REDACTED***"));
        assert!(!out.contains("sk-proj-abcdefghijklmnopqrstuvwxyz1234"));
    }

    #[test]
    fn does_not_redact_short_sk() {
        // sk-foo is too short to match the long-key rule.
        let input = "model: sk-foo";
        let out = redact(input);
        assert_eq!(out, input);
    }

    #[test]
    fn redacts_x_api_key_header() {
        let input = r#""x-api-key": "sk-ant-foo""#;
        let out = redact(input);
        assert!(out.contains("***REDACTED***"));
        assert!(!out.contains("sk-ant-foo"));
    }

    #[test]
    fn redacting_writer_scrubs_before_inner() {
        let mut buf: Vec<u8> = Vec::new();
        {
            let mut w = RedactingWriter::new(&mut buf);
            w.write_all(b"ANTHROPIC_API_KEY=sk-ant-zzz tail").unwrap();
            w.flush().unwrap();
        }
        let s = String::from_utf8(buf).unwrap();
        // Writer now runs the full redact_all pipeline; either placeholder
        // is acceptable, the secret itself must not leak.
        assert!(
            s.contains("***REDACTED***") || s.contains("[redacted-api-key]"),
            "writer output didn't scrub: {}",
            s
        );
        assert!(!s.contains("sk-ant-zzz"), "secret leaked: {}", s);
    }

    // ── Pass-1 new env-var suffix tests ───────────────────────────────

    #[test]
    fn redacts_env_token_assignment() {
        let out = redact("GITHUB_TOKEN=ghp_AbCdEfGhIjKlMnOpQrStUvWxYz");
        assert!(out.contains("***REDACTED***"), "out = {}", out);
        assert!(!out.contains("ghp_AbCdEfGh"), "secret leaked: {}", out);
    }

    #[test]
    fn redacts_env_secret_assignment() {
        let out = redact("WEBHOOK_SECRET=abc123def456");
        assert!(out.contains("***REDACTED***"));
        assert!(!out.contains("abc123def456"));
    }

    #[test]
    fn redacts_env_bearer_assignment() {
        let out = redact("API_BEARER=eyJhbGciOiJIUzI1NiJ9");
        assert!(out.contains("***REDACTED***"));
        assert!(!out.contains("eyJhbGciOiJIUzI1NiJ9"));
    }

    #[test]
    fn redacts_env_auth_assignment() {
        let out = redact("PROVIDER_AUTH=auth-value-xyz");
        assert!(out.contains("***REDACTED***"));
        assert!(!out.contains("auth-value-xyz"));
    }

    #[test]
    fn redacts_env_password_assignment() {
        let out = redact("DB_PASSWORD=hunter2supersecret");
        assert!(out.contains("***REDACTED***"));
        assert!(!out.contains("hunter2supersecret"));
    }

    #[test]
    fn redacts_env_dsn_assignment() {
        let out = redact("SENTRY_DSN=https://abc@sentry.io/1");
        assert!(out.contains("***REDACTED***"));
        assert!(!out.contains("https://abc@sentry.io/1"));
    }

    #[test]
    fn does_not_redact_unrelated_env_var() {
        // FOO_BAR isn't a recognised secret suffix.
        let input = "USER_LOCALE=en-US";
        let out = redact(input);
        assert_eq!(out, input);
    }

    // ── redact_all (pass 1 + pass 2 + path rewrite) tests ─────────────

    #[test]
    fn redact_all_handles_short_bearer_token() {
        // Length is 16+ but contains chars match the regex set; pass-1's
        // Bearer matcher will also fire. Either way the secret must not
        // remain.
        let out = redact_all("Bearer eyJhbGciOiJIUzI1Ni");
        assert!(!out.contains("eyJhbGciOiJIUzI1Ni"), "out = {}", out);
        assert!(
            out.contains("Bearer [redacted]") || out.contains("***REDACTED***"),
            "out = {}",
            out
        );
    }

    #[test]
    fn redact_all_handles_dev_length_token_via_regex() {
        // 12 chars — under pass-1's `_API_KEY` env requirement, but caught
        // by the lowered regex threshold (12).
        let out = redact_all("token=abcd1234efgh");
        assert!(!out.contains("abcd1234efgh"), "out = {}", out);
    }

    #[test]
    fn redact_all_redacts_generic_api_key_assignment() {
        let out = redact_all("api-key: AbCdEf123456GhIjKl");
        assert!(!out.contains("AbCdEf123456GhIjKl"), "out = {}", out);
    }

    #[test]
    fn redact_all_redacts_secret_kv() {
        let out = redact_all("secret=abc123def456ghi");
        assert!(!out.contains("abc123def456ghi"), "out = {}", out);
    }

    #[test]
    fn redact_all_redacts_sentry_dsn() {
        let out = redact_all("dsn=https://abcdef1234567890@sentry.example.com/42");
        assert!(
            !out.contains("abcdef1234567890@sentry.example.com/42"),
            "out = {}",
            out
        );
    }

    #[test]
    fn redact_all_rewrites_home_path() {
        let out = redact_all("opened /Users/alice/Documents/secret.txt for reading");
        assert_eq!(out, "opened ~/Documents/secret.txt for reading");
    }

    #[test]
    fn redact_all_preserves_safe_text() {
        let s = "scout finished feature feat-001 in 1234 ms";
        assert_eq!(redact_all(s), s);
    }

    #[test]
    fn redact_all_redacts_anthropic_key_anywhere() {
        let out = redact_all("auth header: sk-ant-api03-AbCdEfGhIjKlMnOpQrStUvWxYz1234567890");
        // Pass-1 fires on sk-ant-…; secret content must be gone.
        assert!(!out.contains("AbCdEfGh"), "out = {}", out);
    }

    // ── is_sensitive_field tests ──────────────────────────────────────

    #[test]
    fn sensitive_field_basic() {
        assert!(is_sensitive_field("prompt"));
        assert!(is_sensitive_field("api_key"));
        assert!(is_sensitive_field("token"));
        assert!(is_sensitive_field("secret"));
        assert!(is_sensitive_field("password"));
        assert!(is_sensitive_field("bearer"));
        assert!(is_sensitive_field("auth"));
        assert!(is_sensitive_field("authorization"));
        assert!(is_sensitive_field("dsn"));
    }

    #[test]
    fn sensitive_field_case_insensitive() {
        assert!(is_sensitive_field("API_KEY"));
        assert!(is_sensitive_field("Authorization"));
        assert!(is_sensitive_field("Token"));
        assert!(is_sensitive_field("SECRET"));
    }

    #[test]
    fn sensitive_field_negative() {
        assert!(!is_sensitive_field("session_id"));
        assert!(!is_sensitive_field("model"));
        assert!(!is_sensitive_field("user_id"));
        assert!(!is_sensitive_field("feature_id"));
    }

    #[test]
    fn sensitive_field_apikey_variants() {
        assert!(is_sensitive_field("apikey"));
        assert!(is_sensitive_field("api-key"));
        assert!(is_sensitive_field("api_key"));
    }

    /// Regression test for the stderr layer wiring. Builds a
    /// `tracing_subscriber::fmt` layer with the same `RedactingWriter`
    /// wrapping shape used by `lib.rs::init_tracing` and asserts that a
    /// log event containing an Anthropic key is scrubbed before it reaches
    /// the underlying writer. If someone removes the `RedactingStderr`
    /// wrapper from `init_tracing` this test still passes — its purpose
    /// is to prove the wrapping shape itself works end-to-end through
    /// tracing's fmt layer, so the `RedactingStderr` adapter is known
    /// to scrub correctly.
    #[test]
    fn fmt_layer_with_redacting_writer_scrubs_event_payload() {
        use std::sync::{Arc, Mutex};
        use tracing::Level;
        use tracing_subscriber::fmt::MakeWriter;
        use tracing_subscriber::layer::SubscriberExt;
        use tracing_subscriber::Layer;

        /// Shared in-memory sink — every `make_writer()` call hands out a
        /// `RedactingWriter` over the same `Vec<u8>` so the test can read
        /// the rendered output after the event fires.
        #[derive(Clone)]
        struct SharedBuf(Arc<Mutex<Vec<u8>>>);

        struct LockedSharedBuf(Arc<Mutex<Vec<u8>>>);

        impl std::io::Write for LockedSharedBuf {
            fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(buf);
                Ok(buf.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }

        impl<'a> MakeWriter<'a> for SharedBuf {
            type Writer = RedactingWriter<LockedSharedBuf>;
            fn make_writer(&'a self) -> Self::Writer {
                RedactingWriter::new(LockedSharedBuf(Arc::clone(&self.0)))
            }
        }

        let sink = SharedBuf(Arc::new(Mutex::new(Vec::new())));

        let layer = tracing_subscriber::fmt::layer()
            .with_writer(sink.clone())
            .with_ansi(false)
            .with_target(false)
            .with_filter(tracing_subscriber::filter::LevelFilter::from_level(
                Level::INFO,
            ));

        let subscriber = tracing_subscriber::registry().with(layer);
        // Scope the dispatcher so it only intercepts events fired inside
        // this block — keeps the test hermetic regardless of other tests
        // touching the global default subscriber.
        tracing::subscriber::with_default(subscriber, || {
            tracing::info!("leaked secret: sk-ant-abc123def456 trailing text");
        });

        let bytes = sink.0.lock().unwrap().clone();
        let rendered = String::from_utf8(bytes).expect("fmt output is utf-8");
        assert!(
            rendered.contains("***REDACTED***") || rendered.contains("[redacted-api-key]"),
            "expected REDACTED marker in stderr output, got: {rendered}"
        );
        assert!(
            !rendered.contains("sk-ant-abc123def456"),
            "secret leaked through fmt layer: {rendered}"
        );
    }
}
