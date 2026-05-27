//! Tasks IPC surface — persistence, state snapshots, file listing, and
//! auto-commit for the Tasks view.
//!
//! This module covers the backend surface that is NOT the live
//! chat-streaming path (which lives in [`super::chat`]). It handles:
//!
//! * **Task message persistence** — save/load/delete the JSON-blob
//!   frontend UI state stored under `~/.hyvemind/task-messages/`.
//! * **State snapshots** — reconcile the frontend's message store with
//!   the backend's live Pi session state after focus changes or app
//!   restart.
//! * **Project file listing** — file-tree walk for the @-mention file
//!   picker, with gitignore-aware filtering and fuzzy scoring.
//! * **Auto-commit** — git-add + AI-generated commit title (or
//!   task-title fallback) for the "Auto-commit" button in the Tasks
//!   toolbar.
//!
//! All commands validate `task_id` via [`validate_task_id`] to prevent
//! path-traversal attacks before joining into the
//! `~/.hyvemind/task-messages/` directory.

use crate::commands::util::{validate_id, validate_task_id};
use crate::pi::events::PiEvent;
use crate::state::app_state::AppState;
use crate::state::ipc_error::IpcError;
use crate::state::store::atomic_write;
use serde::Serialize;
use std::sync::Arc;
use std::time::Duration;
use tokio::process::Command as TokioCommand;
use tracing::{debug, warn};

/// System prompt sent to the configured default model when generating a short
/// commit title for `auto_commit_task`. Exposed publicly so the
/// Settings → Prompts catalog renders the exact same string that runs in
/// production.
pub const AUTO_COMMIT_TITLE_PROMPT: &str = "You generate a short, concise git commit title (no more than 80 characters) summarising the changes in the staged diff below. Output only the title — no explanation, no prefix.";

/// Variant prompt used when the user has opted into Conventional Commits
/// style for auto-commit titles.
pub const AUTO_COMMIT_TITLE_PROMPT_CONVENTIONAL: &str = "You generate a Conventional Commits style git commit title summarising the changes in the staged diff below. Use `type(scope): subject` or `type: subject`, where type is one of: feat, fix, refactor, docs, style, test, chore, perf, build, ci. Keep the whole title under 80 characters. Output only the title — no explanation, no quotes.";

/// Static last-resort fallback title used when neither AI title generation
/// nor task-title fallback can produce a usable commit title.
pub const AUTO_COMMIT_FALLBACK_TITLE: &str = "Auto-commit";

/// Conventional-commits variant of the last-resort fallback title.
pub const AUTO_COMMIT_FALLBACK_TITLE_CONVENTIONAL: &str = "chore: auto-commit";

const AUTO_COMMIT_TITLE_MAX_CHARS: usize = 80;
const AUTO_COMMIT_CONVENTIONAL_FALLBACK_PREFIX: &str = "chore: ";

/// Persist the frontend's task message array to disk.
///
/// Writes `messages` (a JSON-serialized array of message objects) to
/// `~/.hyvemind/task-messages/{task_id}.json` via an atomic write
/// (write to tempfile, then rename). The frontend calls this on every
/// message mutation so the full UI state survives a reload.
///
/// `task_id` is validated via [`validate_task_id`] to reject
/// path-traversal payloads before path joining.
///
/// # Errors
///
/// * `task_id` fails validation (empty, contains `..`, `/`, or `\`).
/// * Disk write fails (full disk, permissions, etc.).
#[tauri::command]
pub async fn save_task_messages(
    state: tauri::State<'_, AppState>,
    task_id: String,
    messages: String,
) -> Result<(), IpcError> {
    validate_task_id(&task_id).map_err(IpcError::validation)?;
    let path = state.task_messages_dir.join(format!("{}.json", task_id));
    atomic_write(&path, messages.as_bytes())
        .await
        .map_err(|e| IpcError::internal(e.to_string()).with_id(task_id.clone()))
}

/// Load persisted task messages from disk.
///
/// Reads `~/.hyvemind/task-messages/{task_id}.json` and returns its
/// raw JSON contents. Returns `Ok(None)` if the file does not exist
/// (new task, or never saved).
///
/// `task_id` is validated via [`validate_task_id`] before path joining.
///
/// # Errors
///
/// * `task_id` fails validation.
/// * File exists but cannot be read (permissions, I/O error).
#[tauri::command]
pub async fn load_task_messages(
    state: tauri::State<'_, AppState>,
    task_id: String,
) -> Result<Option<String>, IpcError> {
    validate_task_id(&task_id).map_err(IpcError::validation)?;
    let path = state.task_messages_dir.join(format!("{}.json", task_id));
    if !path.exists() {
        return Ok(None);
    }
    std::fs::read_to_string(&path)
        .map(Some)
        .map_err(|e| IpcError::internal(e.to_string()).with_id(task_id.clone()))
}

/// Delete persisted task messages from disk.
///
/// Removes `~/.hyvemind/task-messages/{task_id}.json`. Idempotent:
/// returns `Ok(())` when the file does not exist.
///
/// `task_id` is validated via [`validate_task_id`] before path joining.
///
/// # Errors
///
/// * `task_id` fails validation.
/// * File exists but cannot be removed (permissions, I/O error other
///   than `NotFound`).
#[tauri::command]
pub async fn delete_task_messages(
    state: tauri::State<'_, AppState>,
    task_id: String,
) -> Result<(), IpcError> {
    validate_task_id(&task_id).map_err(IpcError::validation)?;
    let path = state.task_messages_dir.join(format!("{}.json", task_id));
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(IpcError::internal(e.to_string()).with_id(task_id.clone())),
    }
}

/// Authoritative snapshot of a task's persisted state plus any live Pi
/// session details. Used by the frontend to resync after focus changes,
/// app restart, or webview reload — the goal is "render exactly what the
/// backend currently knows" without trusting potentially-stale UI state.
#[derive(Debug, Serialize)]
pub struct TaskStateSnapshot {
    /// The task ID this snapshot belongs to.
    pub task_id: String,
    /// Raw JSON contents of `~/.hyvemind/task-messages/{task_id}.json`,
    /// or `None` if no file exists yet.
    pub messages_json: Option<String>,
    /// True if a live Pi session matching `session_id` exists and is
    /// currently processing a prompt.
    pub session_busy: bool,
    /// True if a live Pi session matching `session_id` exists at all
    /// (alive process). False means the backend has no live state for it.
    pub session_alive: bool,
    /// Snapshot of the Pi session's in-memory transcript (append-only event log)
    /// if a matching session is live. `None` when no session is provided or found.
    pub transcript: Option<Vec<PiEvent>>,
}

/// Authoritative snapshot of a task's persisted and live state.
///
/// Returns a [`TaskStateSnapshot`] carrying the full persisted message
/// JSON (if any file exists) plus live Pi session details for the given
/// `session_id` (if present and alive). Used by the frontend to
/// resynchronize after focus changes, app restart, or webview reload
/// — the goal is "render exactly what the backend currently knows"
/// without trusting potentially-stale frontend state.
///
/// `task_id` is validated via [`validate_task_id`]; `session_id` (if
/// present) is validated via [`validate_id`]. Both gates block
/// path-traversal before any filesystem or session-lookup access.
///
/// # Parameters
///
/// * `task_id` — the task identifier (e.g. `"task-1"`).
/// * `session_id` — optional Pi session UUID. When present and alive,
///   the snapshot includes `session_alive`, `session_busy`, and the
///   in-memory transcript.
///
/// # Returns
///
/// [`TaskStateSnapshot`] with `task_id`, `messages_json`, `session_busy`,
/// `session_alive`, and optionally `transcript`.
///
/// # Errors
///
/// * `task_id` or `session_id` fails validation.
/// * Message file exists but cannot be read.
#[tauri::command]
pub async fn get_task_state(
    state: tauri::State<'_, AppState>,
    task_id: String,
    session_id: Option<String>,
) -> Result<TaskStateSnapshot, IpcError> {
    validate_task_id(&task_id).map_err(IpcError::validation)?;
    let path = state.task_messages_dir.join(format!("{}.json", task_id));
    let messages_json = if path.exists() {
        Some(
            std::fs::read_to_string(&path)
                .map_err(|e| IpcError::internal(e.to_string()).with_id(task_id.clone()))?,
        )
    } else {
        None
    };

    let mut session_alive = false;
    let mut session_busy = false;
    let mut transcript = None;
    if let Some(sid) = session_id {
        validate_id(&sid).map_err(IpcError::validation)?;
        if let Some(session) = state.pi_manager.get_session(&sid).await {
            session_alive = session.is_alive();
            session_busy = session.is_busy();
            transcript = Some(session.get_transcript().await);
        }
    }

    Ok(TaskStateSnapshot {
        task_id,
        messages_json,
        session_busy,
        session_alive,
        transcript,
    })
}

/* ── Project file listing (for @-mention picker) ───────────── */

/// Directories to skip during traversal, beyond what `.gitignore` already
/// removes. These cover common build/dependency directories that are not
/// always present in `.gitignore` (e.g., monorepo subdirs, vendored deps).
///
/// The comparison is case-insensitive to handle macOS HFS+ and Windows.
const SKIP_DIRS: &[&str] = &["node_modules", "target", "dist", "build", "__pycache__"];

/// Validate a project directory string from the frontend, enforcing the
/// approved-dirs allowlist (audit 1.11). Returns the canonical PathBuf.
async fn validate_project_dir(
    state: &tauri::State<'_, AppState>,
    p: &str,
) -> Result<std::path::PathBuf, String> {
    let approved = state.config.read().await.approved_working_dirs.clone();
    crate::commands::util::validate_approved_working_dir(p, &approved)
}

/// True if this `DirEntry` is a directory whose name matches the hardcoded
/// skip list (`node_modules`, `target`, etc.). Files are never rejected here.
/// Hidden files/dirs are already excluded by `WalkBuilder::hidden(true)`.
fn is_skipdir(e: &ignore::DirEntry) -> bool {
    let is_dir = e.file_type().map(|ft| ft.is_dir()).unwrap_or(false);
    if !is_dir {
        return false;
    }
    let Some(name) = e.path().file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    let lower = name.to_ascii_lowercase();
    SKIP_DIRS.iter().any(|d| *d == lower.as_str())
}

#[derive(Debug, Clone, Serialize)]
pub struct ProjectFileEntry {
    pub path: String,
    pub basename: String,
    pub score: i32,
}

/// Internal candidate carrying mtime for sort tiebreaking; not returned.
/// `Ord` derived so it can sit inside a `BinaryHeap<Reverse<(SystemTime, Candidate)>>`
/// for the empty-query top-N-by-mtime path. The derived ordering compares
/// fields in declaration order — used only as a tiebreaker when two entries
/// share an mtime, so the exact order is not user-visible.
#[derive(Debug, Clone, Eq, PartialEq, Ord, PartialOrd)]
struct Candidate {
    mtime: std::time::SystemTime,
    path: String,
    basename: String,
    score: i32,
}

/// Score a candidate against `query_lower`. Returns `None` if no match.
/// Scoring tiers are mutually exclusive (early-return on match) so each
/// candidate gets exactly one score.
fn score_candidate(basename: &str, rel_path: &str, query_lower: &str) -> Option<i32> {
    let bn_lower = basename.to_ascii_lowercase();
    if bn_lower == query_lower {
        return Some(1000);
    }
    if bn_lower.starts_with(query_lower) {
        return Some(800);
    }
    if let Some(idx) = bn_lower.find(query_lower) {
        return Some(std::cmp::max(201, 500 - idx as i32));
    }
    let path_lower = rel_path.to_ascii_lowercase();
    if let Some(idx) = path_lower.find(query_lower) {
        return Some(std::cmp::max(1, 200 - idx as i32));
    }
    None
}

/// Returns true if a relative path contains a control character that would
/// corrupt the plain-text `[Attached files]` block.
fn rel_path_has_control_chars(p: &str) -> bool {
    p.chars().any(|c| c == '\n' || c == '\r' || c == '\0')
}

/// Walk the project directory tree for the @-mention file picker.
///
/// Recursively lists files under `working_dir`, respecting
/// `.gitignore` / `.git/info/exclude` / global gitignore rules, and
/// skipping common build/dependency directories (`node_modules`,
/// `target`, `dist`, `build`, `__pycache__`).
///
/// # Parameters
///
/// * `working_dir` — the project root directory. Validated (trim,
///   expand `~`, canonicalize, must-be-dir) before the walk.
/// * `query` — optional fuzzy filter. When non-empty, each file is
///   scored against the query (basename exact match = 1000, basename
///   prefix = 800, basename contains = 500..201, path contains =
///   200..1) and results are ranked by score then recency. When empty,
///   returns the most-recently-modified files.
/// * `limit` — maximum entries to return (default 50, hard cap 200).
///
/// # Returns
///
/// `Ok(Vec<ProjectFileEntry>)` with each entry carrying `path` (relative
/// to `working_dir`, forward-slash normalized), `basename`, and `score`.
///
/// # Errors
///
/// * `working_dir` is empty, contains a null byte, does not exist, or
///   is not a directory.
/// * The `spawn_blocking` walk task panics (I/O error inside the walk).
#[tauri::command]
pub async fn list_project_files(
    state: tauri::State<'_, AppState>,
    working_dir: String,
    query: String,
    limit: Option<usize>,
) -> Result<Vec<ProjectFileEntry>, IpcError> {
    let canonical_root = validate_project_dir(&state, &working_dir)
        .await
        .map_err(IpcError::not_approved)?;
    list_project_files_walk(canonical_root, query, limit)
        .await
        .map_err(IpcError::internal)
}

/// Walk a canonicalized project root and return the file entries.
///
/// Split out of `list_project_files` so unit tests can exercise the walk
/// without constructing an `AppState` fixture (the IPC wrapper handles the
/// approved-dirs allowlist check; the walk is pure-filesystem and easily
/// testable on its own).
async fn list_project_files_walk(
    canonical_root: std::path::PathBuf,
    query: String,
    limit: Option<usize>,
) -> Result<Vec<ProjectFileEntry>, String> {
    let limit = limit.unwrap_or(50).min(200);
    let query_lower = query.trim().to_ascii_lowercase();
    let empty_query = query_lower.is_empty();

    // Run the filesystem walk on a blocking thread so we don't stall the
    // tokio runtime on a slow disk / large repo.
    let results = tokio::task::spawn_blocking(move || -> Vec<ProjectFileEntry> {
        use ignore::WalkBuilder;
        use std::cmp::Reverse;
        use std::collections::BinaryHeap;

        let mut walker = WalkBuilder::new(&canonical_root);
        walker
            .hidden(true)
            .git_ignore(true)
            .git_exclude(true)
            .git_global(true)
            // Respect .gitignore even when not inside a git repo — useful for
            // freshly-cloned dirs, tempdir-based tests, and project paths
            // that don't have `.git/` at the root.
            .require_git(false)
            .filter_entry(|e| !is_skipdir(e));

        let mut visited: usize = 0;
        const MAX_VISITED: usize = 20_000;

        // For empty query: keep a bounded min-heap (by mtime ascending so
        // the *oldest* is at the top → pop oldest when full). After walk,
        // drain and sort by mtime desc.
        let mut empty_heap: BinaryHeap<Reverse<(std::time::SystemTime, Candidate)>> =
            BinaryHeap::new();

        // For non-empty query: accumulate all matching candidates, then
        // sort and truncate after the walk.
        let mut candidates: Vec<Candidate> = Vec::with_capacity(2000);

        for entry in walker.build() {
            visited += 1;
            if visited >= MAX_VISITED {
                break;
            }
            let entry = match entry {
                Ok(e) => e,
                Err(_) => continue,
            };
            // Skip non-file entries.
            let is_file = entry.file_type().map(|ft| ft.is_file()).unwrap_or(false);
            if !is_file {
                continue;
            }

            let rel_path = match entry.path().strip_prefix(&canonical_root) {
                Ok(p) => p,
                Err(_) => continue, // defense-in-depth; symlinks aren't followed
            };
            let rel_path_str = rel_path.to_string_lossy().to_string();
            // Normalize Windows backslashes to forward slashes so the
            // returned paths are uniform across platforms.
            let rel_path_str = if cfg!(windows) {
                rel_path_str.replace('\\', "/")
            } else {
                rel_path_str
            };
            if rel_path_has_control_chars(&rel_path_str) {
                continue;
            }
            let basename = entry
                .path()
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("")
                .to_string();
            if basename.is_empty() {
                continue;
            }

            let mtime = entry
                .metadata()
                .ok()
                .and_then(|m| m.modified().ok())
                .unwrap_or(std::time::UNIX_EPOCH);

            if empty_query {
                let cand = Candidate {
                    path: rel_path_str,
                    basename,
                    score: 0,
                    mtime,
                };
                if empty_heap.len() < limit {
                    empty_heap.push(Reverse((mtime, cand)));
                } else if let Some(top) = empty_heap.peek() {
                    // top.0 .0 is oldest (because Reverse). Replace if newer.
                    if mtime > top.0 .0 {
                        empty_heap.pop();
                        empty_heap.push(Reverse((mtime, cand)));
                    }
                }
            } else if let Some(score) = score_candidate(&basename, &rel_path_str, &query_lower) {
                candidates.push(Candidate {
                    path: rel_path_str,
                    basename,
                    score,
                    mtime,
                });
            }
        }

        if empty_query {
            let mut items: Vec<Candidate> =
                empty_heap.into_iter().map(|Reverse((_, c))| c).collect();
            items.sort_by(|a, b| b.mtime.cmp(&a.mtime).then_with(|| a.path.cmp(&b.path)));
            items
                .into_iter()
                .take(limit)
                .map(|c| ProjectFileEntry {
                    path: c.path,
                    basename: c.basename,
                    score: c.score,
                })
                .collect()
        } else {
            candidates.sort_by(|a, b| {
                b.score
                    .cmp(&a.score)
                    .then_with(|| b.mtime.cmp(&a.mtime))
                    .then_with(|| a.path.cmp(&b.path))
            });
            candidates.truncate(limit);
            candidates
                .into_iter()
                .map(|c| ProjectFileEntry {
                    path: c.path,
                    basename: c.basename,
                    score: c.score,
                })
                .collect()
        }
    })
    .await
    .map_err(|e| format!("file walk task failed: {}", e))?;

    Ok(results)
}

/* ── Auto-commit ──────────────────────────────────────────── */

/// Parse a `"provider_id/model_id"` string into its components.
/// Provider name is lowercased for case-insensitive registry lookup.
/// Returns `None` if the string is empty, missing the separator, or either part is empty after trimming.
fn parse_default_model(s: &str) -> Option<(String, String)> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    let parts: Vec<&str> = s.splitn(2, '/').collect();
    if parts.len() != 2 {
        return None;
    }
    let provider = parts[0].trim().to_lowercase();
    let model_id = parts[1].trim().to_string();
    if provider.is_empty() || model_id.is_empty() {
        return None;
    }
    Some((provider, model_id))
}

/// Sanitize an AI-generated commit title.
/// - Trims whitespace
/// - Strips surrounding double-quotes (if both present)
/// - Removes leading "hyvemind:" prefix (case-insensitive) to avoid duplication
/// - Collapses all internal whitespace sequences to single space
/// - Removes all control characters (ASCII and Unicode)
/// - Truncates to 80 characters
/// Returns None if the result is empty after sanitization.
fn sanitize_commit_title_text(raw: &str, max_chars: usize) -> Option<String> {
    let cleaned: String = raw
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();

    let collapsed = cleaned.split_whitespace().collect::<Vec<&str>>().join(" ");

    let sanitized = collapsed.trim();
    if sanitized.is_empty() {
        return None;
    }

    Some(sanitized.chars().take(max_chars).collect())
}

fn sanitize_ai_title(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }

    let unquoted = if trimmed.starts_with('"') && trimmed.ends_with('"') && trimmed.len() >= 2 {
        &trimmed[1..trimmed.len() - 1]
    } else {
        trimmed
    };

    let unquoted = unquoted.trim();
    if unquoted.is_empty() {
        return None;
    }

    let without_prefix = if unquoted.len() >= 9 && unquoted[..9].eq_ignore_ascii_case("hyvemind:") {
        unquoted[9..].trim()
    } else {
        unquoted
    };

    sanitize_commit_title_text(without_prefix, AUTO_COMMIT_TITLE_MAX_CHARS)
}

fn looks_like_conventional_commit_title(title: &str) -> bool {
    let Some((head, subject)) = title.split_once(':') else {
        return false;
    };

    if subject.trim().is_empty() {
        return false;
    }

    let head = head.trim();
    let head = head.strip_suffix('!').unwrap_or(head);

    let type_part = if let Some(open_idx) = head.find('(') {
        if !head.ends_with(')') || open_idx == 0 {
            return false;
        }

        let ty = &head[..open_idx];
        let scope = &head[open_idx + 1..head.len() - 1];

        if scope.trim().is_empty() || scope.contains('(') || scope.contains(')') {
            return false;
        }

        ty
    } else {
        head
    };

    let mut chars = type_part.chars();
    let Some(first) = chars.next() else {
        return false;
    };

    first.is_ascii_lowercase()
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

fn fallback_commit_title(task_title: &str, conventional: bool) -> String {
    let Some(sanitized) = sanitize_commit_title_text(task_title, AUTO_COMMIT_TITLE_MAX_CHARS)
    else {
        debug!("task title sanitized to empty, using static auto-commit fallback");
        return if conventional {
            AUTO_COMMIT_FALLBACK_TITLE_CONVENTIONAL.to_string()
        } else {
            AUTO_COMMIT_FALLBACK_TITLE.to_string()
        };
    };

    if !conventional {
        return sanitized;
    }

    if looks_like_conventional_commit_title(&sanitized) {
        return sanitized;
    }

    let subject_max =
        AUTO_COMMIT_TITLE_MAX_CHARS.saturating_sub(AUTO_COMMIT_CONVENTIONAL_FALLBACK_PREFIX.len());

    let subject = if sanitized.chars().count() > subject_max {
        sanitized.chars().take(subject_max).collect::<String>()
    } else {
        sanitized
    };

    format!("{}{}", AUTO_COMMIT_CONVENTIONAL_FALLBACK_PREFIX, subject)
}

/// Check whether a diff text contains binary-file indicators that
/// would produce useless input for an LLM.
fn diff_is_binary(text: &str) -> bool {
    text.contains("Binary files") || text.contains("GIT binary patch")
}

/// Truncate diff text to approximately `max_chars` characters,
/// breaking at a line boundary. If no newline is found within the
/// limit, returns the truncated text as-is (single long line).
fn truncate_diff(text: &str, max_chars: usize) -> String {
    if text.len() <= max_chars {
        return text.to_string();
    }
    // Truncate at the character level, then find last newline
    let truncated: String = text.chars().take(max_chars).collect();
    if let Some(pos) = truncated.rfind('\n') {
        truncated[..pos].to_string()
    } else {
        truncated // single line diff that's very long
    }
}

#[derive(Debug, Serialize)]
pub struct AutoCommitResult {
    pub ok: bool,
    pub message: String,
    /// The commit hash if successful
    pub commit_hash: Option<String>,
}

/// Stage all changes and create a git commit with an AI-generated title.
///
/// The flow:
///
/// 1. Acquire a per-directory mutex (serialises concurrent
///    auto-commits in the same repo).
/// 2. `git rev-parse --is-inside-work-tree` — bail if not a repo.
/// 3. `git status --porcelain` — bail if no changes.
/// 4. `git add -A` — stage everything.
/// 5. If a default model is configured and the staged diff is text
///    (not binary), call the provider to generate a commit title via
///    [`AUTO_COMMIT_TITLE_PROMPT`] (or
///    [`AUTO_COMMIT_TITLE_PROMPT_CONVENTIONAL`] when Conventional
///    Commits mode is enabled). Falls back to sanitizing `task_title`
///    (or a static `"Auto-commit"` / `"chore: auto-commit"` last
///    resort) if AI generation fails or is unavailable.
/// 6. `git commit -m <title>`.
/// 7. `git rev-parse --short HEAD` for the commit hash.
///
/// On failure at any step, staged changes are reset via `git reset
/// HEAD`.
///
/// # Parameters
///
/// * `working_dir` — the repository root. Must exist and be a
///   directory. Not validated as aggressively as `validate_working_dir`
///   (no `~` expansion, no canonicalisation) — just a `trim().is_empty()`
///   and `is_dir()` guard.
/// * `task_title` — the user-visible task title, used as a fallback
///   commit message when AI title generation is unavailable or fails.
///
/// # Returns
///
/// `Ok(AutoCommitResult)` with `ok: true` and the commit hash on
/// success, or `ok: false` with a human-readable `message` on a
/// non-fatal guard condition (not a repo, no changes, git binary
/// missing, etc.).
///
/// # Errors
///
/// This command does NOT return `Err` for expected guard conditions
/// (the `ok: false` path covers those). An `Err` is only returned for
/// programming errors (e.g. a poisoned mutex on `auto_commit_locks`).
#[tauri::command]
pub async fn auto_commit_task(
    state: tauri::State<'_, AppState>,
    working_dir: String,
    task_title: String,
) -> Result<AutoCommitResult, IpcError> {
    // Guard: empty working_dir
    if working_dir.trim().is_empty() {
        return Ok(AutoCommitResult {
            ok: false,
            message: "working directory not set".into(),
            commit_hash: None,
        });
    }

    // Audit 1.11: auto-commit shells out to `git -C <dir>` and writes new
    // commits — this is a destructive write operation against the user's
    // filesystem, so the approved-dirs allowlist applies. We surface the
    // rejection through the AutoCommitResult shape so the UI can render it
    // the same way it renders any other "not a worktree" failure.
    let dir = match validate_project_dir(&state, &working_dir).await {
        Ok(p) => p,
        Err(e) => {
            return Ok(AutoCommitResult {
                ok: false,
                message: format!("working directory rejected: {}", e),
                commit_hash: None,
            });
        }
    };

    // Belt and braces — validate_project_dir already enforces is_dir().
    if !dir.is_dir() {
        return Ok(AutoCommitResult {
            ok: false,
            message: "working directory does not exist".into(),
            commit_hash: None,
        });
    }

    // Acquire per-directory lock to serialize git operations in the same repo
    let dir_mutex = {
        let mut locks = state.auto_commit_locks.lock().unwrap();
        locks
            .entry(dir.clone())
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .clone()
    };
    let _guard = dir_mutex.lock().await;

    // Check if we're inside a git worktree
    let rev_parse = match TokioCommand::new("git")
        .args(["rev-parse", "--is-inside-work-tree"])
        .current_dir(&dir)
        .output()
        .await
    {
        Ok(out) => out,
        Err(e) => {
            return Ok(AutoCommitResult {
                ok: false,
                message: format!("git is not available: {}", e),
                commit_hash: None,
            });
        }
    };

    let is_worktree =
        rev_parse.status.success() && String::from_utf8_lossy(&rev_parse.stdout).trim() == "true";

    if !is_worktree {
        return Ok(AutoCommitResult {
            ok: false,
            message: "not a git repository (or worktree)".into(),
            commit_hash: None,
        });
    }

    // Check if there are changes to commit
    let status_output = match TokioCommand::new("git")
        .args(["status", "--porcelain"])
        .current_dir(&dir)
        .output()
        .await
    {
        Ok(out) => out,
        Err(e) => {
            return Ok(AutoCommitResult {
                ok: false,
                message: format!("failed to run git status: {}", e),
                commit_hash: None,
            });
        }
    };

    if !status_output.status.success() {
        let stderr = String::from_utf8_lossy(&status_output.stderr);
        return Ok(AutoCommitResult {
            ok: false,
            message: format!("git status failed: {}", stderr.trim()),
            commit_hash: None,
        });
    }

    let status_text = String::from_utf8_lossy(&status_output.stdout);
    if status_text.trim().is_empty() {
        return Ok(AutoCommitResult {
            ok: true,
            message: "no changes to commit".into(),
            commit_hash: None,
        });
    }

    // git add -A
    let add_output = match TokioCommand::new("git")
        .args(["add", "-A"])
        .current_dir(&dir)
        .output()
        .await
    {
        Ok(out) => out,
        Err(e) => {
            return Ok(AutoCommitResult {
                ok: false,
                message: format!("failed to run git add: {}", e),
                commit_hash: None,
            });
        }
    };

    if !add_output.status.success() {
        let stderr = String::from_utf8_lossy(&add_output.stderr);
        return Ok(AutoCommitResult {
            ok: false,
            message: format!("git add failed: {}", stderr.trim()),
            commit_hash: None,
        });
    }

    // ── Read default model config and parse it ──
    let config = state.config.read().await;
    let default_model = config
        .default_model
        .as_ref()
        .filter(|s| !s.trim().is_empty())
        .cloned();
    let parsed_default_model = default_model.as_ref().and_then(|s| parse_default_model(s));
    let conventional = config.auto_commit_conventional;
    drop(config);

    // ── Capture staged diff (only if default_model parsed successfully) ──
    let staged_diff: Option<String> = if parsed_default_model.is_some() {
        let diff_output = match TokioCommand::new("git")
            .args(["diff", "--cached"])
            .current_dir(&dir)
            .output()
            .await
        {
            Ok(out) if out.status.success() => String::from_utf8_lossy(&out.stdout).to_string(),
            _ => String::new(),
        };

        if diff_output.trim().is_empty() {
            debug!("staged diff is empty after git add, skipping AI title");
            None
        } else if diff_is_binary(&diff_output) {
            debug!("staged diff contains binary files, skipping AI title");
            None
        } else {
            Some(truncate_diff(&diff_output, 4000))
        }
    } else {
        if let Some(model_str) = default_model {
            if !model_str.trim().is_empty() {
                warn!("default_model '{}' could not be parsed as 'provider_id/model_id', falling back to task title for auto-commit", model_str);
            }
        }
        None
    };

    // Release per-directory lock before AI call (avoids blocking other tasks on same repo)
    drop(_guard);

    // ── AI title generation (outside per-directory lock) ──
    let ai_title: Option<String> = match (parsed_default_model.as_ref(), staged_diff) {
        (Some((ref provider_name, ref model_id)), Some(diff_text)) => {
            let system_prompt = if conventional {
                AUTO_COMMIT_TITLE_PROMPT_CONVENTIONAL
            } else {
                AUTO_COMMIT_TITLE_PROMPT
            };
            let user_prompt = format!("Staged diff:\n```\n{}\n```\n\nCommit title:", diff_text);

            let provider = {
                let registry = state.provider_registry.read().await;
                registry.get(provider_name)
            };
            match provider {
                Some(provider_arc) => {
                    let req =
                        crate::providers::CallRequest::new(model_id, system_prompt, &user_prompt)
                            .with_temperature(Some(0.0))
                            .with_max_tokens(Some(100))
                            .with_timeout(Some(Duration::from_secs(15)));
                    let result = provider_arc.call(req).await;

                    match result {
                        Ok(response) => {
                            let title = sanitize_ai_title(&response.output);
                            if title.is_none() {
                                debug!(
                                    "AI commit title was empty after sanitization, falling back"
                                );
                            }
                            title
                        }
                        Err(e) => {
                            debug!("LLM call for commit title failed: {}", e);
                            None
                        }
                    }
                }
                None => {
                    warn!("default_model provider '{}' not found in registry, falling back to task title", provider_name);
                    None
                }
            }
        }
        _ => None,
    };

    // ── Re-acquire lock for commit ──
    let _guard = dir_mutex.lock().await;

    // Fallback to the sanitized task title when AI title generation isn't possible.
    // If the task title is empty or unusable after sanitization, use the static
    // last-resort fallback.
    let commit_msg = ai_title.unwrap_or_else(|| fallback_commit_title(&task_title, conventional));

    // git commit
    let commit_output = match TokioCommand::new("git")
        .args(["commit", "-m", &commit_msg])
        .current_dir(&dir)
        .output()
        .await
    {
        Ok(out) => out,
        Err(e) => {
            // Spawn failure — unstage before returning
            let _ = TokioCommand::new("git")
                .args(["reset", "HEAD"])
                .current_dir(&dir)
                .output()
                .await;
            return Ok(AutoCommitResult {
                ok: false,
                message: format!("failed to run git commit: {}", e),
                commit_hash: None,
            });
        }
    };

    if !commit_output.status.success() {
        let stderr = String::from_utf8_lossy(&commit_output.stderr);
        // Unstage to avoid polluting the index
        let _ = TokioCommand::new("git")
            .args(["reset", "HEAD"])
            .current_dir(&dir)
            .output()
            .await;
        return Ok(AutoCommitResult {
            ok: false,
            message: format!("git commit failed: {}", stderr.trim()),
            commit_hash: None,
        });
    }

    // Get the commit hash
    let hash_output = TokioCommand::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .current_dir(&dir)
        .output()
        .await
        .ok();

    let commit_hash = hash_output.and_then(|o| {
        if o.status.success() {
            Some(String::from_utf8_lossy(&o.stdout).trim().to_string())
        } else {
            None
        }
    });

    Ok(AutoCommitResult {
        ok: true,
        message: format!("committed: {}", commit_msg),
        commit_hash,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_path_traversal() {
        assert!(validate_task_id("").is_err());
        assert!(validate_task_id("..").is_err());
        assert!(validate_task_id("../etc/passwd").is_err());
        assert!(validate_task_id("foo/bar").is_err());
        assert!(validate_task_id("foo\\bar").is_err());
        assert!(validate_task_id("task-1").is_ok());
        assert!(validate_task_id("task-abc-123").is_ok());
    }

    #[test]
    fn auto_commit_result_serializes() {
        let r = AutoCommitResult {
            ok: true,
            message: "done".into(),
            commit_hash: Some("abc1234".into()),
        };
        let json = serde_json::to_string(&r).unwrap();
        assert!(json.contains(r#""ok":true"#));
        assert!(json.contains("abc1234"));
    }

    #[test]
    fn auto_commit_result_failure_serializes() {
        let r = AutoCommitResult {
            ok: false,
            message: "not a git repository".into(),
            commit_hash: None,
        };
        let json = serde_json::to_string(&r).unwrap();
        assert!(json.contains(r#""ok":false"#));
        assert!(json.contains(r#""commit_hash":null"#));
    }

    // ── parse_default_model tests ──

    #[test]
    fn parse_default_model_valid() {
        assert_eq!(
            parse_default_model("anthropic/claude-sonnet-4-20250514"),
            Some(("anthropic".into(), "claude-sonnet-4-20250514".into()))
        );
    }

    #[test]
    fn parse_default_model_normalizes_case() {
        assert_eq!(
            parse_default_model("Anthropic/claude-sonnet-4-20250514"),
            Some(("anthropic".into(), "claude-sonnet-4-20250514".into()))
        );
    }

    #[test]
    fn parse_default_model_handles_whitespace() {
        assert_eq!(
            parse_default_model("  anthropic / claude-sonnet-4  "),
            Some(("anthropic".into(), "claude-sonnet-4".into()))
        );
    }

    #[test]
    fn parse_default_model_invalid() {
        assert_eq!(parse_default_model(""), None);
        assert_eq!(parse_default_model("modelonly"), None);
        assert_eq!(parse_default_model("/gpt4"), None);
        assert_eq!(parse_default_model("openai/"), None);
    }

    #[test]
    fn parse_default_model_extra_slashes() {
        // splitn(2) keeps extra slashes in model part
        assert_eq!(
            parse_default_model("openai/gpt-4/32k"),
            Some(("openai".into(), "gpt-4/32k".into()))
        );
    }

    #[test]
    fn parse_default_model_empty_string_is_none() {
        assert_eq!(parse_default_model(""), None);
        assert_eq!(parse_default_model("   "), None);
    }

    // ── sanitize_ai_title tests ──

    #[test]
    fn sanitize_ai_title_removes_surrounding_quotes() {
        assert_eq!(
            sanitize_ai_title("\"add login feature\""),
            Some("add login feature".into())
        );
    }

    #[test]
    fn sanitize_ai_title_strips_hyvemind_prefix() {
        assert_eq!(
            sanitize_ai_title("hyvemind: add tests"),
            Some("add tests".into())
        );
        assert_eq!(
            sanitize_ai_title("HYVEMIND: add tests"),
            Some("add tests".into())
        );
    }

    #[test]
    fn sanitize_ai_title_handles_newlines_and_control_chars() {
        assert_eq!(
            sanitize_ai_title("add\nlogin\nfeature"),
            Some("add login feature".into())
        );
        assert_eq!(
            sanitize_ai_title("add\x00login\x1ffeature"),
            Some("add login feature".into())
        );
    }

    #[test]
    fn sanitize_ai_title_truncates_to_80_chars() {
        let long = "a".repeat(100);
        let result = sanitize_ai_title(&long);
        assert_eq!(result.as_deref(), Some("a".repeat(80).as_str()));
    }

    #[test]
    fn sanitize_ai_title_returns_none_for_empty() {
        assert_eq!(sanitize_ai_title(""), None);
        assert_eq!(sanitize_ai_title("   "), None);
        assert_eq!(sanitize_ai_title("\"\""), None);
    }

    #[test]
    fn sanitize_ai_title_collapses_whitespace() {
        assert_eq!(
            sanitize_ai_title("add   login   feature"),
            Some("add login feature".into())
        );
    }

    // ── diff_is_binary tests ──

    #[test]
    fn diff_is_binary_detects_binary_files() {
        assert!(diff_is_binary(
            "Binary files a/logo.png and b/logo.png differ"
        ));
        assert!(diff_is_binary("GIT binary patch\ndelta abc123"));
        assert!(!diff_is_binary(
            "--- a/src/main.rs\n+++ b/src/main.rs\n@@ -1 +1 @@\n-hello\n+hello world"
        ));
    }

    // ── truncate_diff tests ──

    #[test]
    fn truncate_diff_preserves_lines_under_limit() {
        let text = "line1\nline2\nline3";
        assert_eq!(truncate_diff(text, 4000), text);
    }

    #[test]
    fn truncate_diff_breaks_at_newline() {
        let text = "this is a very long line that exceeds the limit.............................................\nsecond line";
        let result = truncate_diff(text, 40);
        assert!(result.len() <= 40);
        assert!(!result.contains('\n')); // single line truncated
    }

    #[test]
    fn diff_truncation_always_produces_at_least_one_line() {
        let text = "a".repeat(5000) + "\n" + &"b".repeat(100);
        let result = truncate_diff(&text, 4000);
        assert!(!result.is_empty());
        // should keep the first line at least partially
        assert!(result.len() >= 1);
    }

    // ── list_project_files / score_candidate / validate_project_dir ──

    use std::fs;
    use std::path::Path;

    fn touch(p: &Path) {
        if let Some(parent) = p.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(p, b"").unwrap();
    }

    #[test]
    fn score_basename_equals_query() {
        assert_eq!(
            score_candidate("agents.md", "docs/agents.md", "agents.md"),
            Some(1000)
        );
    }

    #[test]
    fn score_basename_startswith_returns_800_not_500() {
        // "age" at index 0 of "AGENTS.md" must score 800 (startswith tier),
        // NOT fall through to the contains tier which would compute 500.
        let s = score_candidate("AGENTS.md", "AGENTS.md", "age").unwrap();
        assert_eq!(s, 800);
    }

    #[test]
    fn score_basename_contains_floor_outranks_path_contains() {
        // basename contains at large index still uses floor 201,
        // which beats anything path-contains can produce (max 200).
        let bn = "a".repeat(600) + "needle" + &"a".repeat(2);
        let score = score_candidate(&bn, &bn, "needle").unwrap();
        assert!(score >= 201, "got {}", score);
        assert!(score > 200);
    }

    #[test]
    fn score_path_contains_only() {
        let s = score_candidate("foo.rs", "src/needle/foo.rs", "needle");
        assert!(matches!(s, Some(n) if n > 0 && n <= 200));
    }

    #[test]
    fn score_no_match_returns_none() {
        assert_eq!(score_candidate("foo.rs", "src/foo.rs", "xyz"), None);
    }

    #[test]
    fn rel_path_control_chars_detected() {
        assert!(rel_path_has_control_chars("foo\nbar"));
        assert!(rel_path_has_control_chars("foo\rbar"));
        assert!(rel_path_has_control_chars("foo\0bar"));
        assert!(!rel_path_has_control_chars("foo/bar.md"));
    }

    // The IPC-level `validate_project_dir` now consults `Config::approved_working_dirs`
    // via `AppState`; allowlist-shape and canonicalization tests live in
    // `commands::util::tests`. The behavioural file-walk tests below call the
    // pure `list_project_files_walk` helper directly so they don't need an
    // AppState fixture.

    #[tokio::test]
    async fn list_project_files_honors_gitignore() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        fs::write(root.join(".gitignore"), "ignored.txt\n").unwrap();
        touch(&root.join("kept.txt"));
        touch(&root.join("ignored.txt"));
        let canon = root.canonicalize().expect("canon");
        let res = list_project_files_walk(canon, "".to_string(), Some(50))
            .await
            .unwrap();
        let paths: Vec<_> = res.iter().map(|e| e.path.as_str()).collect();
        assert!(paths.contains(&"kept.txt"));
        assert!(!paths.contains(&"ignored.txt"));
    }

    #[tokio::test]
    async fn list_project_files_skips_node_modules() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        touch(&root.join("node_modules/lodash/index.js"));
        touch(&root.join("target/debug/something.rs"));
        touch(&root.join("src/main.rs"));
        let canon = root.canonicalize().expect("canon");
        let res = list_project_files_walk(canon, "".to_string(), Some(50))
            .await
            .unwrap();
        let paths: Vec<_> = res.iter().map(|e| e.path.as_str()).collect();
        assert!(paths.iter().any(|p| p.ends_with("main.rs")));
        assert!(!paths.iter().any(|p| p.contains("node_modules")));
        assert!(!paths.iter().any(|p| p.contains("target")));
    }

    #[tokio::test]
    async fn list_project_files_skips_node_modules_case_insensitive() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        // On case-sensitive filesystems this exists as a separate dir;
        // on macOS HFS+ it collides with node_modules, which is also fine.
        touch(&root.join("Node_Modules/pkg/x.js"));
        touch(&root.join("src/main.rs"));
        let canon = root.canonicalize().expect("canon");
        let res = list_project_files_walk(canon, "".to_string(), Some(50))
            .await
            .unwrap();
        let paths: Vec<_> = res.iter().map(|e| e.path.as_str()).collect();
        assert!(!paths
            .iter()
            .any(|p| p.to_ascii_lowercase().contains("node_modules")));
    }

    #[tokio::test]
    async fn list_project_files_query_ranks_age_for_agents_md() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        touch(&root.join("AGENTS.md"));
        touch(&root.join("package.json"));
        let canon = root.canonicalize().expect("canon");
        let res = list_project_files_walk(canon, "age".to_string(), Some(50))
            .await
            .unwrap();
        // AGENTS.md should be returned and ranked higher than package.json
        let agents = res.iter().position(|e| e.path == "AGENTS.md");
        let pkg = res.iter().position(|e| e.path == "package.json");
        assert!(agents.is_some(), "results: {:?}", res);
        if let (Some(a), Some(p)) = (agents, pkg) {
            assert!(a < p);
        }
    }

    #[tokio::test]
    async fn list_project_files_empty_query_returns_mtime_desc() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        touch(&root.join("old.txt"));
        std::thread::sleep(std::time::Duration::from_millis(20));
        touch(&root.join("new.txt"));
        let canon = root.canonicalize().expect("canon");
        let res = list_project_files_walk(canon, "".to_string(), Some(50))
            .await
            .unwrap();
        // Most-recently-modified should appear before older one.
        let new_idx = res.iter().position(|e| e.path == "new.txt");
        let old_idx = res.iter().position(|e| e.path == "old.txt");
        assert!(new_idx.is_some() && old_idx.is_some());
        assert!(new_idx.unwrap() < old_idx.unwrap());
    }

    // The nonexistent-dir / file-as-workdir failure paths are exercised by
    // `commands::util::canonicalize_working_dir` tests; the walk helper itself
    // assumes its input is already canonical (the IPC wrapper guarantees
    // that), so it has no failure path to cover.

    // ── fallback_commit_title tests ──

    #[test]
    fn fallback_commit_title_uses_task_title_plain_and_conventional() {
        assert_eq!(
            fallback_commit_title("Add login feature", false),
            "Add login feature"
        );

        assert_eq!(
            fallback_commit_title("Add login feature", true),
            "chore: Add login feature"
        );
    }

    #[test]
    fn fallback_commit_title_keeps_existing_conventional_prefix() {
        assert_eq!(
            fallback_commit_title("feat: add login", true),
            "feat: add login"
        );

        assert_eq!(
            fallback_commit_title("fix(auth): handle login error", true),
            "fix(auth): handle login error"
        );

        assert_eq!(
            fallback_commit_title("fix:add login", true),
            "fix:add login"
        );

        assert_eq!(
            fallback_commit_title("improvement: add tests", true),
            "improvement: add tests"
        );
    }

    #[test]
    fn fallback_commit_title_truncates_conventional_fallback_to_80_chars() {
        let long = "a".repeat(100);
        let result = fallback_commit_title(&long, true);

        assert!(result.starts_with("chore: "));
        assert_eq!(result.chars().count(), AUTO_COMMIT_TITLE_MAX_CHARS);
    }

    #[test]
    fn fallback_commit_title_truncates_existing_conventional_title_to_80_chars() {
        let long = format!("feat: {}", "a".repeat(100));
        let result = fallback_commit_title(&long, true);

        assert!(result.starts_with("feat: "));
        assert_eq!(result.chars().count(), AUTO_COMMIT_TITLE_MAX_CHARS);
    }

    #[test]
    fn fallback_commit_title_uses_static_last_resort_for_empty_title() {
        assert_eq!(fallback_commit_title("", false), AUTO_COMMIT_FALLBACK_TITLE);

        assert_eq!(
            fallback_commit_title(" \n\t\u{0}", true),
            AUTO_COMMIT_FALLBACK_TITLE_CONVENTIONAL
        );
    }

    #[test]
    fn fallback_commit_title_sanitizes_whitespace_and_control_chars() {
        assert_eq!(
            fallback_commit_title("Add\n  login\tfeature", false),
            "Add login feature"
        );

        assert_eq!(fallback_commit_title("Add\u{0}login", false), "Add login");
    }

    #[test]
    fn looks_like_conventional_commit_title_accepts_common_forms() {
        assert!(looks_like_conventional_commit_title("feat: add login"));
        assert!(looks_like_conventional_commit_title(
            "fix(auth): handle login error"
        ));
        assert!(looks_like_conventional_commit_title("fix!: critical bug"));
        assert!(looks_like_conventional_commit_title(
            "feat(api)!: add endpoint"
        ));
        assert!(looks_like_conventional_commit_title("fix:add login"));
        assert!(looks_like_conventional_commit_title(
            "improvement: add tests"
        ));
    }

    #[test]
    fn looks_like_conventional_commit_title_rejects_invalid_forms() {
        assert!(!looks_like_conventional_commit_title("feat:   "));
        assert!(!looks_like_conventional_commit_title(
            "feat(scope: add login"
        ));
        assert!(!looks_like_conventional_commit_title("Feat: add login"));
        assert!(!looks_like_conventional_commit_title(
            "feat!(scope): add login"
        ));
    }
}
