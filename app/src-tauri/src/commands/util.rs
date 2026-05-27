//! Shared utilities for Tauri command handlers.

use std::path::{Path, PathBuf};

/// Drop-in replacement for `std::fs::canonicalize` that strips the Windows
/// `\\?\` extended-length prefix from the result when it isn't needed
/// (path < MAX_PATH, no UNC server component). Identical to
/// `std::fs::canonicalize` on macOS and Linux.
///
/// **Why**: Bun 1.x on Windows segfaults during startup when handed an
/// `--extension \\?\C:\...` argument — its module loader can't parse the
/// extended-length prefix. Since Hyvemind ships a bun-compiled Pi binary
/// and resolves the bundled extension directory via canonicalize, every
/// Pi spawn would otherwise crash on Windows (see `~/.hyvemind/debug/
/// sessions/*.jsonl` for the `panic(main thread): Segmentation fault at
/// address 0x18` signature).
///
/// Use this helper at every canonicalize site whose output may be
/// (a) passed to a subprocess as a CLI arg or `current_dir`, (b)
/// persisted to config and later compared via `Path::starts_with`, or
/// (c) shown to the user. The allowlist and the working-dir check MUST
/// use the same canonicalizer or the prefix-comparison will spuriously
/// fail on Windows.
pub(crate) fn canonicalize_clean(p: impl AsRef<Path>) -> std::io::Result<PathBuf> {
    dunce::canonicalize(p)
}

/// Maximum serialized size (bytes) of a `serde_json::Value` payload accepted
/// at the IPC boundary. The Tauri WebView already trusts the renderer
/// process, but a buggy or compromised frontend should not be able to pin
/// the backend with arbitrary-sized JSON. 1 MiB easily covers any
/// well-formed feature list / model settings blob the UI sends today.
pub const MAX_JSON_PAYLOAD: usize = 1 * 1024 * 1024; // 1 MiB

/// Maximum length (bytes) of a `rounds_config` JSON string for a Hivemind
/// configuration. Even a 12-model, 5-round config serializes to a few
/// kilobytes; 64 KiB is a generous ceiling that still prevents pathological
/// payloads from reaching SQLite.
pub const MAX_ROUNDS_CONFIG: usize = 64 * 1024; // 64 KiB

/// Reject `serde_json::Value` payloads that exceed [`MAX_JSON_PAYLOAD`]
/// when serialised.
///
/// Called at the top of every Tauri command that accepts a free-form
/// `serde_json::Value` parameter, before any deserialization into a typed
/// struct. The cost is one `serde_json::to_vec` round-trip per payload —
/// negligible compared with the work the command itself performs.
pub(crate) fn check_payload_size(json: &serde_json::Value) -> Result<(), String> {
    let serialized = serde_json::to_vec(json).map_err(|e| e.to_string())?;
    if serialized.len() > MAX_JSON_PAYLOAD {
        return Err(format!("payload exceeds {} bytes", MAX_JSON_PAYLOAD));
    }
    Ok(())
}

/// Validate, canonicalize, and enforce allowlist membership for a working
/// directory string supplied via IPC (audit item 1.11).
///
/// Steps:
/// 1. Trim, reject empty input and NUL bytes.
/// 2. Expand a leading `~` (alone or `~/...`) to the user's home directory.
/// 3. Canonicalize the path — resolves symlinks, makes the path absolute.
/// 4. Assert the canonical path is a directory.
/// 5. Walk `approved_working_dirs`, canonicalizing each entry; accept iff the
///    candidate equals **or** is a strict descendant of any approved entry
///    (`Path::starts_with` after both sides are canonicalized — this defeats
///    symlinks that resolve outside the allowlist).
///
/// Returns the canonical `PathBuf` on success; a structured human-readable
/// error otherwise. The "not approved" error message is intentionally
/// distinct (`prefix: "working directory not approved:"`) so the frontend
/// can detect it and surface the approval modal.
pub(crate) fn validate_approved_working_dir(
    p: &str,
    approved_working_dirs: &[PathBuf],
) -> Result<PathBuf, String> {
    let canonical = canonicalize_working_dir(p)?;

    // Empty allowlist = reject everything (safe default; first-time users get
    // auto-seeded from default_project_path on save, see config.rs).
    if approved_working_dirs.is_empty() {
        return Err(format!(
            "working directory not approved: {} (no approved directories configured)",
            canonical.display()
        ));
    }

    for entry in approved_working_dirs {
        // Canonicalize the allowlist entry too. An entry that fails to
        // canonicalize (e.g. user deleted the dir) is skipped — we don't want
        // a single broken entry to block all the others.
        let approved_canon = match canonicalize_clean(entry) {
            Ok(p) => p,
            Err(_) => continue,
        };
        if canonical == approved_canon || canonical.starts_with(&approved_canon) {
            return Ok(canonical);
        }
    }

    Err(format!(
        "working directory not approved: {}",
        canonical.display()
    ))
}

/// Trim/expand/canonicalize a path string from IPC without applying the
/// allowlist check. Shared by `validate_approved_working_dir` and the
/// approval-request command (which needs to canonicalize before adding the
/// path to the allowlist).
pub(crate) fn canonicalize_working_dir(p: &str) -> Result<PathBuf, String> {
    let trimmed = p.trim();
    if trimmed.is_empty() {
        return Err("working directory is empty".into());
    }
    if trimmed.contains('\0') {
        return Err("working directory contains null byte".into());
    }

    let expanded: PathBuf = if trimmed == "~" || trimmed.starts_with("~/") {
        let home = dirs::home_dir().ok_or_else(|| "home dir not available".to_string())?;
        if trimmed == "~" {
            home
        } else {
            home.join(&trimmed[2..])
        }
    } else if let Some(rest) = trimmed.strip_prefix("~") {
        // Backwards-compat: the old per-module validators accepted bare
        // "~name" (no slash) by joining the remainder onto home. Preserve
        // that behaviour so an existing config entry like "~foo" still
        // canonicalizes.
        let home = dirs::home_dir().ok_or_else(|| "home dir not available".to_string())?;
        home.join(rest)
    } else {
        PathBuf::from(trimmed)
    };

    let canonical =
        canonicalize_clean(&expanded).map_err(|e| format!("working directory invalid: {}", e))?;
    if !canonical.is_dir() {
        return Err(format!("not a directory: {}", canonical.display()));
    }
    Ok(canonical)
}

/// Returns `true` if a `Path` is **strictly within** any approved root after
/// both sides are canonicalized. Helper used by callers that already have a
/// canonical path in hand and don't need re-canonicalization.
#[allow(dead_code)]
pub(crate) fn is_under_approved_root(path: &Path, approved_working_dirs: &[PathBuf]) -> bool {
    let canon_path = match canonicalize_clean(path) {
        Ok(p) => p,
        Err(_) => return false,
    };
    for entry in approved_working_dirs {
        if let Ok(approved_canon) = canonicalize_clean(entry) {
            if canon_path == approved_canon || canon_path.starts_with(&approved_canon) {
                return true;
            }
        }
    }
    false
}

/// Reject IDs that could cause path traversal or other filesystem mischief
/// when they're concatenated into a path under `reviews_dir` / similar.
///
/// Defense in depth: even commands that only use the ID for a SQLite/HashMap
/// lookup call this — the validation is cheap and rejects clearly malformed
/// IDs at the IPC boundary before they touch any subsystem.
///
/// Accepted character set: ASCII alphanumerics + `-` + `_`. Use
/// [`validate_extension_id`] when the id may also include `:` (composite
/// `type_id:provider_id` ids from the Provider Extensions module).
///
/// ## Per-field exceptions / allowed character review
///
/// `validate_id` rejects `%`, `_`-only-IDs (`_` is allowed inside), `/`, `:`,
/// `.` (when leading), `\`, `\0`, and anything outside `[A-Za-z0-9_-]`. The
/// IDs that flow through Tauri IPC are reviewed below:
///
/// - `swarm_id`, `review_id`, `job_id`, `run_id`, `hivemind_id`, `task_id`,
///   `feature_id` — internal slug/uuid forms used as filesystem path
///   components under `~/.hyvemind/{swarms,reviews,chat-sessions,...}`.
///   All conform (alphanumeric, `-`, `_`) and stay under the 64-char cap.
/// - `session_id` — Pi-issued UUIDs **and** swarm-minted composites of the
///   form `{role}-{swarm_uuid}-{feature_id}` (e.g.
///   `worker-550e8400-e29b-41d4-a716-446655440000-migrate-legacy-state-store`).
///   The composite can exceed 64 chars with realistic LLM-generated feature
///   slugs, so session_ids use [`validate_session_id`] with a 128-char cap
///   and the same path-traversal defenses — every other ID stays on the
///   64-char budget because their lengths feed into filesystem-path budgets
///   under `~/.hyvemind/{swarms,reviews,task-messages,…}`.
/// - `extension_id` — composite `type_id:provider_id`. Uses
///   [`validate_extension_id`] which additionally permits `:`.
///
/// No legitimate ID requires `%`, `/`, `\`, leading `.`, or unicode. If a new
/// ID-shaped field is added that needs those characters, add a separate
/// `validate_*_id` helper (do not relax the shared allowlist).
pub(crate) fn validate_id(id: &str) -> Result<(), String> {
    if id.is_empty()
        || id.len() > 64
        || id.contains("..")
        || id.contains('/')
        || id.contains('\\')
        || id.contains('\0')
        || id.starts_with('.')
        || !id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(format!("invalid id: {:?}", id));
    }
    Ok(())
}

/// Same as [`validate_id`] but with a 128-char cap, scoped to `session_id`
/// arguments at the IPC boundary.
///
/// Pi-issued UUIDs comfortably fit under 64 chars, but the swarm engine
/// mints composite session ids of the form `{role}-{swarm_uuid}-{feature_id}`
/// (see `core/queen.rs::run_feature_full`). With a 36-char UUID swarm_id,
/// the prefix alone is 44 chars for `worker-` (or 43 for `scout-` / `guard-`),
/// leaving only 20-21 chars for `feature_id` under the shared 64-char budget.
/// Realistic LLM-generated slugs like `add-task-completion-handler` (27) or
/// `migrate-legacy-state-store` (25) blow past that limit and trip the
/// validator inside `get_pi_session_stats`, leaving SwarmControl's bottom
/// bar stuck at `↑0 / ↓0 / 0%` for the lifetime of the Worker.
///
/// 128 is conservative: worst-case `worker-{36}-{feature_id}` needs
/// `feature_id > 84` chars to exceed 128, which is well beyond any
/// realistic slug. All path-traversal defenses are preserved — only the
/// upper-length gate moves.
///
/// Filesystem-path safety is unaffected: composite session ids are NEVER
/// joined directly into `~/.hyvemind/swarms/{...}` — only the `swarm_id`
/// (validated with [`validate_id`], still 64-cap) is. Session ids feed the
/// in-memory Pi session table and, for Tasks-view sessions, the
/// `~/.hyvemind/chat-sessions/{sid}.jsonl` filename — the path-traversal
/// rejects here cover that file-naming case.
pub(crate) fn validate_session_id(id: &str) -> Result<(), String> {
    if id.is_empty()
        || id.len() > 128
        || id.contains("..")
        || id.contains('/')
        || id.contains('\\')
        || id.contains('\0')
        || id.starts_with('.')
        || !id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(format!("invalid session_id: {:?}", id));
    }
    Ok(())
}

/// Same as [`validate_id`] but additionally accepts `:` so composite
/// `type_id:provider_id` ids from the Provider Extensions module pass.
/// Length budget is bumped to 128 to accommodate the longer composite.
pub(crate) fn validate_extension_id(id: &str) -> Result<(), String> {
    if id.is_empty()
        || id.len() > 128
        || id.contains("..")
        || id.contains('/')
        || id.contains('\\')
        || id.contains('\0')
        || id.starts_with('.')
        || !id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == ':')
    {
        return Err(format!("invalid extension id: {:?}", id));
    }
    Ok(())
}

/// Same as [`validate_id`] but with a review_id-specific error message.
/// Review IDs follow the `hmr-` prefix convention (e.g. `hmr-a1b2c3d4`) and
/// are used as filesystem path components under `~/.hyvemind/reviews/`.
pub(crate) fn validate_review_id(id: &str) -> Result<(), String> {
    if id.is_empty()
        || id.len() > 64
        || id.contains("..")
        || id.contains('/')
        || id.contains('\\')
        || id.contains('\0')
        || id.starts_with('.')
        || !id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(format!("invalid review_id: {:?}", id));
    }
    Ok(())
}

/// Same as [`validate_id`] but with a task_id-specific error message.
/// Task IDs follow the `task-{numeric}` or `task-{uuid}` convention and are
/// used as filesystem path components under `~/.hyvemind/task-messages/`.
pub(crate) fn validate_task_id(id: &str) -> Result<(), String> {
    if id.is_empty()
        || id.len() > 64
        || id.contains("..")
        || id.contains('/')
        || id.contains('\\')
        || id.contains('\0')
        || id.starts_with('.')
        || !id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(format!("invalid task_id: {:?}", id));
    }
    Ok(())
}

/// Same as [`validate_id`] but with a hivemind_id-specific error message.
/// Hivemind IDs are user-defined slugs or UUIDs used as path components and
/// SQLite lookup keys in Hivemind CRUD operations.
pub(crate) fn validate_hivemind_id(id: &str) -> Result<(), String> {
    if id.is_empty()
        || id.len() > 64
        || id.contains("..")
        || id.contains('/')
        || id.contains('\\')
        || id.contains('\0')
        || id.starts_with('.')
        || !id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(format!("invalid hivemind_id: {:?}", id));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_id_accepts_typical_ids() {
        assert!(validate_id("550e8400-e29b-41d4-a716-446655440000").is_ok());
        assert!(validate_id("hmr-a1b2c3d4").is_ok());
        assert!(validate_id("job_with_underscores").is_ok());
        assert!(validate_id("ABC123def").is_ok());
    }

    #[test]
    fn validate_id_rejects_dangerous_inputs() {
        assert!(validate_id("").is_err());
        assert!(validate_id("..").is_err());
        assert!(validate_id("foo/bar").is_err());
        assert!(validate_id("foo\\bar").is_err());
        assert!(validate_id(".hidden").is_err());
        assert!(validate_id("foo\0bar").is_err());
        assert!(validate_id("foo bar").is_err());
        assert!(validate_id("foo:bar").is_err()); // colon rejected for regular ids
    }

    #[test]
    fn validate_extension_id_accepts_composite() {
        assert!(validate_extension_id("openrouter_credits:openrouter").is_ok());
    }

    #[test]
    fn validate_extension_id_rejects_path_separators() {
        assert!(validate_extension_id("foo/bar").is_err());
        assert!(validate_extension_id("../etc").is_err());
        assert!(validate_extension_id("").is_err());
        assert!(validate_extension_id(".hidden").is_err());
    }

    /// Regression test for the path-traversal vulnerability where
    /// `delete_chat_session({session_id: "../../../../etc/myfile"})` resolved
    /// to `~/.hyvemind/chat-sessions/../../../../etc/myfile.jsonl`. Both ID
    /// validators MUST reject `..` payloads at the IPC boundary so an
    /// attacker-controlled id can never traverse outside its bucket dir.
    #[test]
    fn validate_id_rejects_dotdot_traversal_payload() {
        let attacks = [
            "..",
            "../etc/myfile",
            "../../../../etc/passwd",
            "foo/../bar",
            "foo/..",
            "..foo",
            "foo..bar",
            "foo..",
        ];
        for atk in attacks {
            assert!(
                validate_id(atk).is_err(),
                "validate_id must reject traversal payload {:?}",
                atk
            );
            assert!(
                validate_extension_id(atk).is_err(),
                "validate_extension_id must reject traversal payload {:?}",
                atk
            );
        }
    }

    #[test]
    fn check_payload_size_accepts_small_payloads() {
        let v = serde_json::json!({
            "name": "a swarm",
            "features": ["a", "b", "c"],
            "nested": { "key": "value" }
        });
        assert!(check_payload_size(&v).is_ok());
    }

    #[test]
    fn check_payload_size_accepts_payload_near_cap() {
        // ~ (MAX - 1024) bytes — comfortably under the cap.
        let big_string = "x".repeat(MAX_JSON_PAYLOAD - 1024);
        let v = serde_json::json!({ "blob": big_string });
        assert!(check_payload_size(&v).is_ok());
    }

    #[test]
    fn check_payload_size_rejects_oversized_payload() {
        // 2 MiB of `x` inside a JSON string — well over the 1 MiB cap.
        let huge = "x".repeat(MAX_JSON_PAYLOAD + 1024);
        let v = serde_json::json!({ "blob": huge });
        let err = check_payload_size(&v).expect_err("must reject");
        assert!(
            err.contains("exceeds"),
            "expected size-exceeded error, got {err}"
        );
    }

    #[test]
    fn check_payload_size_rejects_oversized_array() {
        // ~ 1.05 MiB of one-byte numbers in an array; serializes to ~3 MiB
        // including commas. Demonstrates that array bloat is caught too.
        let arr: Vec<u8> = (0..MAX_JSON_PAYLOAD).map(|_| 0u8).collect();
        let v = serde_json::json!(arr);
        assert!(check_payload_size(&v).is_err());
    }

    #[test]
    fn max_payload_constants_are_sane() {
        assert!(MAX_ROUNDS_CONFIG < MAX_JSON_PAYLOAD);
        assert_eq!(MAX_JSON_PAYLOAD, 1024 * 1024);
        assert_eq!(MAX_ROUNDS_CONFIG, 64 * 1024);
    }

    // ── Sibling validators ──

    #[test]
    fn validate_review_id_accepts_review_ids() {
        assert!(validate_review_id("hmr-a1b2c3d4").is_ok());
        assert!(validate_review_id("hmr-550e8400-e29b-41d4-a716-446655440000").is_ok());
    }

    #[test]
    fn validate_review_id_rejects_invalid() {
        assert!(validate_review_id("").is_err());
        assert!(validate_review_id("..").is_err());
        assert!(validate_review_id("foo/bar").is_err());
        assert!(validate_review_id("foo:bar").is_err());
        assert!(validate_review_id(".hidden").is_err());
        assert!(validate_review_id("foo bar").is_err());
    }

    #[test]
    fn validate_task_id_accepts_task_ids() {
        assert!(validate_task_id("task-1").is_ok());
        assert!(validate_task_id("task-550e8400-e29b-41d4-a716-446655440000").is_ok());
        assert!(validate_task_id("task-abc-123").is_ok());
    }

    #[test]
    fn validate_task_id_rejects_invalid() {
        assert!(validate_task_id("").is_err());
        assert!(validate_task_id("..").is_err());
        assert!(validate_task_id("foo/bar").is_err());
        assert!(validate_task_id("task-1:extra").is_err());
        assert!(validate_task_id(".hidden").is_err());
    }

    #[test]
    fn validate_hivemind_id_accepts_hivemind_ids() {
        assert!(validate_hivemind_id("enhance").is_ok());
        assert!(validate_hivemind_id("arch-council").is_ok());
        assert!(validate_hivemind_id("550e8400-e29b-41d4-a716-446655440000").is_ok());
        assert!(validate_hivemind_id("security_review").is_ok());
    }

    #[test]
    fn validate_hivemind_id_rejects_invalid() {
        assert!(validate_hivemind_id("").is_err());
        assert!(validate_hivemind_id("..").is_err());
        assert!(validate_hivemind_id("foo/bar").is_err());
        assert!(validate_hivemind_id("foo:bar").is_err());
        assert!(validate_hivemind_id(".hidden").is_err());
    }

    // --- Malformed-input IPC tests (Fix 7.5) -------------------------------
    //
    // The task asks specifically for: empty string, "..", very long string,
    // unicode, null bytes. These are the highest-blast-radius inputs to the
    // commands that build filesystem paths from IDs (delete_swarm,
    // delete_chat_session, start_swarm, start_review, log_review_event).
    // Each command calls validate_id at the IPC boundary, so the tests
    // assert against validate_id directly with the worst-case inputs.

    #[test]
    fn validate_id_rejects_very_long_string() {
        // 65 chars = 1 over the cap. The 64-byte limit is what keeps
        // pathological frontend payloads from creating runaway filenames.
        let s = "a".repeat(65);
        assert!(validate_id(&s).is_err(), "65-char id must be rejected");
        // 1024 chars: clearly pathological.
        let s = "a".repeat(1024);
        assert!(validate_id(&s).is_err(), "1024-char id must be rejected");
        // 64 chars: the exact upper bound — should still pass.
        let s = "a".repeat(64);
        assert!(validate_id(&s).is_ok(), "64-char id must pass");
    }

    #[test]
    fn validate_id_rejects_unicode_chars() {
        // Non-ASCII letters and digits look innocent but can be used in
        // homoglyph attacks ("admín" vs "admin") or break filesystem path
        // assumptions. They must be rejected by the strict ASCII allowlist.
        assert!(validate_id("admín").is_err(), "Spanish í must be rejected");
        assert!(validate_id("中文id").is_err(), "Chinese must be rejected");
        assert!(validate_id("emoji-🍯").is_err(), "emoji must be rejected");
        assert!(
            validate_id("zalg\u{0301}o").is_err(),
            "combining diacritic must be rejected"
        );
        // RTL override is a classic display-spoofing char.
        assert!(
            validate_id("\u{202E}rev").is_err(),
            "RTL override must be rejected"
        );
    }

    #[test]
    fn validate_id_rejects_null_byte_in_middle_and_end() {
        // Null bytes truncate paths in many C-level filesystem APIs and
        // bypass naive string-prefix checks.
        assert!(validate_id("foo\0bar").is_err());
        assert!(validate_id("foo\0").is_err());
        assert!(validate_id("\0").is_err());
    }

    #[test]
    fn validate_id_rejects_path_traversal_forms() {
        // The exact string "..".
        assert!(validate_id("..").is_err());
        // Embedded in a longer string — `contains("..")` matches anywhere.
        assert!(validate_id("foo..bar").is_err());
        assert!(validate_id("a/../b").is_err());
        // URL-encoded traversal — the percent sign itself isn't alphanumeric,
        // so the allowlist catches it.
        assert!(validate_id("%2e%2e").is_err());
        // Windows-style separator.
        assert!(validate_id("foo\\bar").is_err());
    }

    #[test]
    fn validate_id_rejects_empty_and_whitespace() {
        assert!(validate_id("").is_err());
        assert!(validate_id(" ").is_err());
        assert!(validate_id("\t").is_err());
        assert!(validate_id("\n").is_err());
        // Whitespace-padded valid-looking id: still rejected because the
        // allowlist disallows the space chars.
        assert!(validate_id(" hmr-abc ").is_err());
    }

    #[test]
    fn validate_id_rejects_leading_dot() {
        // `.hidden` would create dotfiles or shadow . / .. when treated as
        // a directory name.
        assert!(validate_id(".hidden").is_err());
        assert!(validate_id(".").is_err());
    }

    #[test]
    fn validate_id_accepts_all_known_legitimate_id_formats() {
        // UUID v4 (Pi session ids).
        assert!(validate_id("550e8400-e29b-41d4-a716-446655440000").is_ok());
        // Hivemind review id ("hmr-XXXXXXXX").
        assert!(validate_id("hmr-a1b2c3d4").is_ok());
        // Swarm id (UUID).
        assert!(validate_id("4cdc4078-69f7-4d70-bbb8-a94b2a00114d").is_ok());
        // Snake/kebab/PascalCase ids.
        assert!(validate_id("hivemind_id_42").is_ok());
        assert!(validate_id("MixedCase-Test_123").is_ok());
    }

    // --- JSON-Value-shaped IPC argument tests ------------------------------
    //
    // log_review_event takes `data: serde_json::Value`. A buggy frontend can
    // send any JSON: null, wrong-type at a field, missing required fields.
    // These never reach the IPC if validate_id rejects the review_id first,
    // but for inputs that pass validate_id, the serde_json::Value path must
    // simply accept any shape without panicking — confirm the basic
    // serde_json behaviour we rely on.

    #[test]
    fn json_value_accepts_null_and_wrong_types_without_panic() {
        // Wire shape of a `log_review_event` payload — different field types.
        let payloads = [
            serde_json::json!(null),
            serde_json::json!({}),
            serde_json::json!({"event": "x", "round": "not-a-number"}),
            serde_json::json!([1, 2, 3]),
            serde_json::json!("just a string"),
            serde_json::json!(42),
        ];
        for p in &payloads {
            // The IPC handler reads `data: serde_json::Value`. Round-trip
            // through Value → string → Value to mirror what Tauri does over
            // the bridge. Must never panic.
            let s = serde_json::to_string(p).expect("serialize");
            let back: serde_json::Value = serde_json::from_str(&s).expect("parse");
            assert_eq!(back, *p);
        }
    }

    // ---- validate_approved_working_dir (audit 1.11) -------------------------

    #[test]
    fn approved_wd_rejects_empty_allowlist() {
        let td = tempfile::TempDir::new().expect("tempdir");
        let result =
            validate_approved_working_dir(&td.path().to_string_lossy(), &Vec::<PathBuf>::new());
        assert!(result.is_err(), "empty allowlist must reject all paths");
        let err = result.unwrap_err();
        assert!(
            err.contains("not approved"),
            "expected 'not approved' marker in error, got: {}",
            err
        );
    }

    #[test]
    fn approved_wd_accepts_exact_match() {
        let td = tempfile::TempDir::new().expect("tempdir");
        let canon = canonicalize_clean(td.path()).expect("canonicalize tempdir");
        let allowlist = vec![canon.clone()];
        let result = validate_approved_working_dir(&td.path().to_string_lossy(), &allowlist);
        assert!(result.is_ok(), "exact match must be accepted: {:?}", result);
        assert_eq!(result.unwrap(), canon);
    }

    #[test]
    fn approved_wd_accepts_subdirectory() {
        let td = tempfile::TempDir::new().expect("tempdir");
        let canon_root = canonicalize_clean(td.path()).expect("canonicalize tempdir");
        let subdir = canon_root.join("subdir");
        std::fs::create_dir(&subdir).expect("mkdir subdir");
        let allowlist = vec![canon_root.clone()];
        let result = validate_approved_working_dir(&subdir.to_string_lossy(), &allowlist);
        assert!(
            result.is_ok(),
            "subdirectory must be accepted: {:?}",
            result
        );
        assert_eq!(
            result.unwrap(),
            canonicalize_clean(&subdir).expect("canon subdir")
        );
    }

    #[test]
    fn approved_wd_accepts_sandbox_subdir_when_sandbox_root_in_allowlist() {
        // Regression: the chat-side `validate_working_dir` appends
        // `state.test_sandbox_dir` to the cloned allowlist so the Tests-screen
        // stability runner can target a freshly-scaffolded
        // `~/.hyvemind/test-sandbox/{run_id}/` directory without the user
        // having to approve it via ProjectPicker. This test mirrors that
        // composition: an empty user allowlist plus the injected sandbox
        // root must accept a descendant run directory.
        let td = tempfile::TempDir::new().expect("tempdir");
        let sandbox_root = canonicalize_clean(td.path()).expect("canonicalize tempdir");
        let run_dir = sandbox_root.join("20260516-164912-a00a7b95");
        std::fs::create_dir(&run_dir).expect("mkdir run dir");
        // Simulate the chat-side composition: empty user allowlist + injected sandbox root.
        let approved = vec![sandbox_root.clone()];
        let got = validate_approved_working_dir(&run_dir.to_string_lossy(), &approved)
            .expect("sandbox subdir must be accepted");
        assert_eq!(got, canonicalize_clean(&run_dir).expect("canon run dir"));
    }

    #[test]
    fn approved_wd_rejects_sibling_directory() {
        // Create two sibling directories under a common parent. Only one is
        // approved. The other must be rejected — and `starts_with` should NOT
        // be tricked by a shared prefix string.
        let td = tempfile::TempDir::new().expect("tempdir");
        let canon_parent = canonicalize_clean(td.path()).expect("canon");
        let approved = canon_parent.join("approved");
        let sibling = canon_parent.join("approved-but-not");
        std::fs::create_dir(&approved).expect("mkdir approved");
        std::fs::create_dir(&sibling).expect("mkdir sibling");
        let allowlist = vec![approved];
        let result = validate_approved_working_dir(&sibling.to_string_lossy(), &allowlist);
        assert!(
            result.is_err(),
            "sibling whose path starts with approved name must be rejected"
        );
    }

    #[cfg(unix)]
    #[test]
    fn approved_wd_rejects_symlink_that_escapes_allowlist() {
        // Symlink target outside the allowlist must be rejected — that's the
        // whole point of canonicalizing BEFORE the prefix check.
        let allowed_td = tempfile::TempDir::new().expect("allowed tempdir");
        let other_td = tempfile::TempDir::new().expect("other tempdir");
        let canon_allowed = canonicalize_clean(allowed_td.path()).expect("canon allowed");
        let canon_other = canonicalize_clean(other_td.path()).expect("canon other");
        let link_inside_allowed = canon_allowed.join("escape");
        std::os::unix::fs::symlink(&canon_other, &link_inside_allowed)
            .expect("create symlink to outside");
        let allowlist = vec![canon_allowed];
        // The symlink LIVES inside the allowlist but RESOLVES outside it.
        // `canonicalize` follows the symlink, so the check should reject it.
        let result =
            validate_approved_working_dir(&link_inside_allowed.to_string_lossy(), &allowlist);
        assert!(
            result.is_err(),
            "symlink that resolves outside the allowlist must be rejected; got {:?}",
            result
        );
    }

    #[test]
    fn approved_wd_rejects_completely_unrelated_path() {
        let approved_td = tempfile::TempDir::new().expect("approved");
        let other_td = tempfile::TempDir::new().expect("other");
        let allowlist = vec![canonicalize_clean(approved_td.path()).expect("canon")];
        let result = validate_approved_working_dir(&other_td.path().to_string_lossy(), &allowlist);
        assert!(
            result.is_err(),
            "unrelated path must be rejected: {:?}",
            result
        );
    }

    #[test]
    fn canonicalize_working_dir_rejects_empty_and_null_byte() {
        assert!(canonicalize_working_dir("").is_err());
        assert!(canonicalize_working_dir("   ").is_err());
        assert!(canonicalize_working_dir("/tmp/\0bad").is_err());
    }

    #[test]
    fn canonicalize_working_dir_accepts_existing_dir() {
        let td = tempfile::TempDir::new().expect("tempdir");
        let result = canonicalize_working_dir(&td.path().to_string_lossy());
        assert!(result.is_ok());
        assert_eq!(
            result.unwrap(),
            canonicalize_clean(td.path()).expect("canon tempdir")
        );
    }

    /// Regression: on Windows, `std::fs::canonicalize` returns paths with the
    /// `\\?\` extended-length prefix. Those paths crash Bun 1.x when handed
    /// to it as `--extension` arguments, taking down every Pi spawn. The
    /// `canonicalize_clean` helper MUST strip that prefix for short paths.
    #[cfg(windows)]
    #[test]
    fn canonicalize_clean_strips_extended_length_prefix() {
        let td = tempfile::TempDir::new().expect("tempdir");
        let cleaned = canonicalize_clean(td.path()).expect("canon");
        let s = cleaned.to_string_lossy();
        assert!(
            !s.starts_with(r"\\?\"),
            "canonicalize_clean must strip the \\\\?\\ prefix on Windows; got {s:?}"
        );
        // Sanity: std's canonicalize would have left it on, proving the
        // wrapper actually does something (regression safety against a
        // future maintainer swapping it back to std::fs::canonicalize).
        let std_canon = std::fs::canonicalize(td.path()).expect("std canon");
        let std_s = std_canon.to_string_lossy();
        assert!(
            std_s.starts_with(r"\\?\"),
            "expected std::fs::canonicalize to emit \\\\?\\ prefix on Windows (test premise broken if not); got {std_s:?}"
        );
    }

    // ---- validate_session_id (audit: SwarmControl token-stats bar) -------

    #[test]
    fn validate_session_id_accepts_uuid_v4() {
        // Pi-issued Tasks-view session ids — 36 chars.
        assert!(validate_session_id("550e8400-e29b-41d4-a716-446655440000").is_ok());
        assert!(validate_session_id("4cdc4078-69f7-4d70-bbb8-a94b2a00114d").is_ok());
    }

    #[test]
    fn validate_session_id_accepts_worker_composite_with_long_feature_id() {
        // Real-world composite minted by core/queen.rs for the SwarmControl
        // bug: worker- + 36-char UUID + - + LLM-generated slug. This used to
        // fail validate_id at 64 chars; must now pass.
        let sid = format!(
            "worker-{}-add-task-completion-handler-with-undo",
            "550e8400-e29b-41d4-a716-446655440000"
        );
        assert!(
            validate_session_id(&sid).is_ok(),
            "worker composite must be accepted (len={}): {:?}",
            sid.len(),
            sid
        );
        // Scout / Guard variants also need to fit comfortably.
        let scout = format!(
            "scout-{}-migrate-legacy-state-store",
            "4cdc4078-69f7-4d70-bbb8-a94b2a00114d"
        );
        assert!(validate_session_id(&scout).is_ok());
        let guard = format!(
            "guard-{}-migrate-legacy-state-store",
            "4cdc4078-69f7-4d70-bbb8-a94b2a00114d"
        );
        assert!(validate_session_id(&guard).is_ok());
    }

    #[test]
    fn validate_session_id_accepts_at_128_chars() {
        let s = "a".repeat(128);
        assert!(validate_session_id(&s).is_ok(), "128-char id must pass");
    }

    #[test]
    fn validate_session_id_rejects_at_129() {
        let s = "a".repeat(129);
        assert!(
            validate_session_id(&s).is_err(),
            "129-char id must be rejected"
        );
        let s = "a".repeat(1024);
        assert!(validate_session_id(&s).is_err());
    }

    #[test]
    fn validate_session_id_rejects_path_traversal() {
        assert!(validate_session_id("").is_err());
        assert!(validate_session_id("..").is_err());
        assert!(validate_session_id("foo/../bar").is_err());
        assert!(validate_session_id("../etc/passwd").is_err());
        assert!(validate_session_id(".hidden").is_err());
        assert!(validate_session_id(".").is_err());
        assert!(validate_session_id("foo\0bar").is_err());
        assert!(validate_session_id("foo\\bar").is_err());
        assert!(validate_session_id("foo/bar").is_err());
        assert!(validate_session_id("foo bar").is_err());
        // Unicode rejected by ASCII allowlist.
        assert!(validate_session_id("adm\u{00ED}n").is_err());
        assert!(validate_session_id("\u{4E2D}\u{6587}id").is_err());
        assert!(validate_session_id("emoji-\u{1F36F}").is_err());
        // RTL override is a classic display-spoofing char.
        assert!(validate_session_id("\u{202E}rev").is_err());
        // Colon not allowed (extension ids only).
        assert!(validate_session_id("foo:bar").is_err());
    }

    /// Lock in that the 64-char limit on regular IDs stays in place — the
    /// 128-char relaxation is strictly scoped to `validate_session_id`.
    #[test]
    fn validate_id_still_rejects_over_64() {
        let s = "a".repeat(64);
        assert!(validate_id(&s).is_ok(), "64-char id must still pass");
        let s = "a".repeat(65);
        assert!(
            validate_id(&s).is_err(),
            "65-char id must still be rejected (validate_session_id relaxation must not bleed into validate_id)"
        );
        // The composite that motivated the fix MUST be rejected by validate_id.
        let composite = format!(
            "worker-{}-add-task-completion-handler-with-undo",
            "550e8400-e29b-41d4-a716-446655440000"
        );
        assert!(
            validate_id(&composite).is_err(),
            "composite session id must still be rejected by validate_id (forces session_id call sites to use validate_session_id)"
        );
    }

    #[test]
    fn approved_wd_skips_broken_allowlist_entries() {
        // A broken entry (deleted path) should not prevent a valid entry
        // from matching.
        let td = tempfile::TempDir::new().expect("tempdir");
        let canon = canonicalize_clean(td.path()).expect("canon");
        let allowlist = vec![PathBuf::from("/this/path/does/not/exist"), canon.clone()];
        let result = validate_approved_working_dir(&td.path().to_string_lossy(), &allowlist);
        assert!(
            result.is_ok(),
            "broken allowlist entry must not block valid entry: {:?}",
            result
        );
    }
}
