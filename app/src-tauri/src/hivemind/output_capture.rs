//! Per-call durable capture of a hivemind reviewer's final output.
//!
//! When `HYVEMIND_DEBUG=1` is set, every `model_call_completed` event used to
//! inline the entire LLM output into `~/.hyvemind/reviews/{review_id}.jsonl`.
//! Those payloads ran 100KB+ each and dominated the on-disk footprint. The
//! capture writer mirrors the [`MergeCapture`](super::merge_capture) pattern:
//! the full output goes to a sibling text file and the JSONL entry only
//! references its path.
//!
//! Files land at:
//!
//! ```text
//! ~/.hyvemind/reviews/{review_id}/output-{model_id_safe}-r{round}-i{model_idx}.txt
//! ```
//!
//! where `model_id_safe` is the model id with `/` and `:` replaced by `_`, and
//! `model_idx` is the 0-based index of this reviewer instance within the round
//! (matches `job_steps.sort_order` in SQLite). Including the instance suffix
//! unconditionally guarantees that multiple calls against the same model id in
//! the same round (e.g. with different temperatures) write to distinct files —
//! consumers should not depend on the un-suffixed form existing.

use std::path::{Path, PathBuf};

/// Sanitise a model id for use as a single path component.
///
/// Provider-qualified ids like `anthropic/claude-opus-4.1` and tagged ids like
/// `ollama:llama3.1` collapse to `anthropic_claude-opus-4.1` /
/// `ollama_llama3.1` so the resulting file lives at one directory level.
pub fn sanitize_model_id(model_id: &str) -> String {
    model_id.replace(['/', ':'], "_")
}

/// Compute the absolute capture-file path for a given review/model/round/idx.
///
/// `reviews_dir` is expected to be `~/.hyvemind/reviews/` — i.e. the parent of
/// `ReviewLogger::path` (which is `{reviews_dir}/{review_id}.jsonl`).
///
/// `model_idx` is the 0-based instance index of this reviewer call within the
/// round; mirrors `job_steps.sort_order` so duplicate model ids in the same
/// round don't overwrite each other.
pub fn capture_path(
    reviews_dir: &Path,
    review_id: &str,
    model_id: &str,
    round: u32,
    model_idx: u32,
) -> PathBuf {
    reviews_dir.join(review_id).join(format!(
        "output-{}-r{}-i{}.txt",
        sanitize_model_id(model_id),
        round,
        model_idx
    ))
}

/// Write `output` atomically-enough for our purposes to the capture file and
/// return a path string relative to the `~/.hyvemind/` root when possible
/// (falls back to the absolute path string if the strip fails).
///
/// Silently returns `None` if any filesystem operation fails — the caller
/// should fall back to omitting `output_file` from the JSONL event rather
/// than aborting the review.
pub async fn write_capture(
    reviews_dir: &Path,
    review_id: &str,
    model_id: &str,
    round: u32,
    model_idx: u32,
    output: &str,
) -> Option<String> {
    let path = capture_path(reviews_dir, review_id, model_id, round, model_idx);
    if let Some(parent) = path.parent() {
        if tokio::fs::create_dir_all(parent).await.is_err() {
            return None;
        }
    }
    if tokio::fs::write(&path, output).await.is_err() {
        return None;
    }
    // `reviews_dir` is `{hyvemind_root}/reviews`, so its parent is the hyvemind
    // root. Prefer the root-relative form (e.g.
    // `reviews/hmr-abc/output-anthropic_claude-r1-i0.txt`) so log readers don't
    // need to know the user's home directory.
    let rel = reviews_dir
        .parent()
        .and_then(|root| path.strip_prefix(root).ok())
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|| path.to_string_lossy().to_string());
    Some(rel)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn sanitize_strips_slash_and_colon() {
        assert_eq!(
            sanitize_model_id("anthropic/claude-opus-4.1"),
            "anthropic_claude-opus-4.1"
        );
        assert_eq!(sanitize_model_id("ollama:llama3.1"), "ollama_llama3.1");
        assert_eq!(
            sanitize_model_id("openrouter/x-ai/grok:latest"),
            "openrouter_x-ai_grok_latest"
        );
        assert_eq!(
            sanitize_model_id("claude-sonnet-4-20250514"),
            "claude-sonnet-4-20250514"
        );
    }

    #[test]
    fn capture_path_is_under_review_id_dir() {
        let path = capture_path(
            Path::new("/tmp/reviews"),
            "hmr-abc",
            "anthropic/claude-opus-4.1",
            2,
            0,
        );
        assert_eq!(
            path,
            PathBuf::from("/tmp/reviews/hmr-abc/output-anthropic_claude-opus-4.1-r2-i0.txt")
        );
    }

    #[tokio::test]
    async fn write_capture_creates_dir_and_returns_relative_path() {
        let tmp = TempDir::new().expect("tempdir");
        let reviews_dir = tmp.path().join("reviews");

        let rel = write_capture(
            &reviews_dir,
            "hmr-x",
            "anthropic/claude-opus-4.1",
            1,
            0,
            "hello world",
        )
        .await
        .expect("write should succeed");

        assert_eq!(
            rel,
            "reviews/hmr-x/output-anthropic_claude-opus-4.1-r1-i0.txt"
        );

        let on_disk = reviews_dir
            .join("hmr-x")
            .join("output-anthropic_claude-opus-4.1-r1-i0.txt");
        let contents = tokio::fs::read_to_string(&on_disk)
            .await
            .expect("read back");
        assert_eq!(contents, "hello world");
    }

    /// Two calls against the same model id within the same round but at
    /// different `model_idx` slots must write to distinct files. This is the
    /// regression guard for the duplicate-reviewer-instance bug — without
    /// the `-i{model_idx}` suffix both calls would overwrite the same path.
    #[tokio::test]
    async fn write_capture_distinct_paths_for_duplicate_model_ids() {
        let tmp = TempDir::new().expect("tempdir");
        let reviews_dir = tmp.path().join("reviews");

        let rel_a = write_capture(
            &reviews_dir,
            "hmr-dup",
            "anthropic/claude-sonnet-4",
            1,
            0,
            "output from instance 0 (temp=0.2)",
        )
        .await
        .expect("instance 0 write should succeed");

        let rel_b = write_capture(
            &reviews_dir,
            "hmr-dup",
            "anthropic/claude-sonnet-4",
            1,
            1,
            "output from instance 1 (temp=0.7)",
        )
        .await
        .expect("instance 1 write should succeed");

        assert_ne!(rel_a, rel_b, "duplicate-instance paths must differ");
        assert!(
            rel_a.ends_with("-r1-i0.txt"),
            "instance 0 should land in -i0 file, got {}",
            rel_a
        );
        assert!(
            rel_b.ends_with("-r1-i1.txt"),
            "instance 1 should land in -i1 file, got {}",
            rel_b
        );

        // And the on-disk contents must be independent.
        let path_a = reviews_dir
            .join("hmr-dup")
            .join("output-anthropic_claude-sonnet-4-r1-i0.txt");
        let path_b = reviews_dir
            .join("hmr-dup")
            .join("output-anthropic_claude-sonnet-4-r1-i1.txt");
        assert_eq!(
            tokio::fs::read_to_string(&path_a).await.unwrap(),
            "output from instance 0 (temp=0.2)"
        );
        assert_eq!(
            tokio::fs::read_to_string(&path_b).await.unwrap(),
            "output from instance 1 (temp=0.7)"
        );
    }
}
