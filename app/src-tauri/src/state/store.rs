use anyhow::{Context, Result};
use rand::RngCore;
use std::path::{Path, PathBuf};
use tokio::io::AsyncWriteExt;
use tracing::info;

use crate::core::handoff::DiscoveredIssue;
use crate::core::services::{parse_services_yaml, ServicesFile};
use crate::core::validation::{render_validation_contract, ValidationAssertion, ValidationState};
use crate::domain::swarm::{Feature, Milestone, SwarmState};

// ---------------------------------------------------------------------------
// SwarmStore -- file-based persistence for swarm data (async)
// ---------------------------------------------------------------------------

/// Manages per-swarm file persistence under `{data_dir}/swarms/`.
///
/// Every read/write is async, backed by `tokio::fs`, so that long-running
/// disk I/O can never block a Tokio worker thread. Audit 3.1 (Phase E):
/// prior to this change every method here used `std::fs::*` synchronously,
/// which meant a slow disk could stall the Queen orchestrator (which calls
/// `write_state` / `write_features` on every feature transition) and any
/// concurrent IPC handlers sharing the runtime.
///
/// Directory layout per swarm:
/// ```text
/// {base_dir}/{swarm_id}/
///   state.json
///   features.json
///   plan.md
///   progress_log.jsonl
///   handoffs/{feature_id}.json
/// ```
#[derive(Debug, Clone)]
pub struct SwarmStore {
    base_dir: PathBuf,
}

impl SwarmStore {
    /// Create a new store rooted at `{data_dir}/swarms`.
    pub fn new(data_dir: &Path) -> Self {
        Self {
            base_dir: data_dir.join("swarms"),
        }
    }

    /// Canonical directory for a given swarm.
    pub fn swarm_dir(&self, swarm_id: &str) -> PathBuf {
        self.base_dir.join(swarm_id)
    }

    /// Canonical path of the per-swarm append-only `activity_log.jsonl`.
    /// Owned here so `commands/swarms.rs` (the forwarder constructor +
    /// `get_swarm_activity_log` reader) share one definition; the actual
    /// JSONL contract lives in [`crate::state::activity_log`].
    pub fn activity_log_path(&self, swarm_id: &str) -> PathBuf {
        self.swarm_dir(swarm_id).join("activity_log.jsonl")
    }

    /// Create the full directory hierarchy for a new swarm.
    pub async fn init_swarm(&self, swarm_id: &str) -> Result<()> {
        let dir = self.swarm_dir(swarm_id);
        tokio::fs::create_dir_all(dir.join("handoffs"))
            .await
            .with_context(|| format!("failed to create swarm directory for '{}'", swarm_id))?;
        info!("initialised swarm directory at {}", dir.display());
        Ok(())
    }

    // -- Atomic writes -------------------------------------------------------

    /// Persist the swarm state atomically.
    pub async fn write_state(&self, swarm_id: &str, state: &SwarmState) -> Result<()> {
        let path = self.swarm_dir(swarm_id).join("state.json");
        let content = serde_json::to_vec_pretty(state).context("failed to serialise SwarmState")?;
        atomic_write(&path, &content).await
    }

    /// Persist the feature list atomically.
    pub async fn write_features(&self, swarm_id: &str, features: &[Feature]) -> Result<()> {
        let path = self.swarm_dir(swarm_id).join("features.json");
        let content =
            serde_json::to_vec_pretty(features).context("failed to serialise feature list")?;
        atomic_write(&path, &content).await
    }

    /// Persist the milestone list atomically.
    pub async fn write_milestones(&self, swarm_id: &str, milestones: &[Milestone]) -> Result<()> {
        let path = self.swarm_dir(swarm_id).join("milestones.json");
        let content =
            serde_json::to_vec_pretty(milestones).context("failed to serialise milestone list")?;
        atomic_write(&path, &content).await
    }

    // -- Discovered issues (Phase 5C) ---------------------------------------

    /// Append a single Worker-reported [`DiscoveredIssue`] to the per-swarm
    /// `discovered_issues.jsonl` log.
    ///
    /// This is an append-only audit trail — atomic-write is intentionally
    /// not used. Each call opens the file, writes one JSON line (with a
    /// `recorded_at` timestamp wrapper), and closes it. If the parent
    /// directory does not yet exist (e.g. swarm was never `init_swarm`'d),
    /// it is created. Callers should treat failures as non-fatal; the
    /// queen.rs caller logs a `warn` and continues.
    pub async fn append_discovered_issue(
        &self,
        swarm_id: &str,
        feature_id: &str,
        issue: &DiscoveredIssue,
    ) -> Result<()> {
        let dir = self.swarm_dir(swarm_id);
        tokio::fs::create_dir_all(&dir)
            .await
            .with_context(|| format!("failed to create swarm dir {}", dir.display()))?;
        let path = dir.join("discovered_issues.jsonl");

        // Wrap the issue with a timestamp + feature_id so the JSONL file is
        // self-describing without needing to cross-reference progress_log.
        let entry = serde_json::json!({
            "recorded_at": chrono::Utc::now().to_rfc3339(),
            "feature_id": feature_id,
            "severity": issue.severity.to_string(),
            "description": issue.description,
            "suggested_fix": issue.suggested_fix,
        });
        let mut line =
            serde_json::to_string(&entry).context("failed to serialise DiscoveredIssue entry")?;
        line.push('\n');

        let mut file = tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .await
            .with_context(|| format!("failed to open {} for append", path.display()))?;
        file.write_all(line.as_bytes())
            .await
            .with_context(|| format!("failed to append to {}", path.display()))?;
        Ok(())
    }

    // -- Validation contract (Phase 2) --------------------------------------

    /// Render a human-readable Markdown contract grouping every
    /// `ValidationAssertion` by milestone and write it atomically to
    /// `validation-contract.md`.
    pub async fn write_validation_contract(
        &self,
        swarm_id: &str,
        milestones: &[Milestone],
        assertions: &[ValidationAssertion],
    ) -> Result<()> {
        let path = self.swarm_dir(swarm_id).join("validation-contract.md");
        let rendered = render_validation_contract(milestones, assertions);
        atomic_write(&path, rendered.as_bytes()).await
    }

    /// Persist the per-assertion validation state atomically as JSON.
    pub async fn write_validation_state(
        &self,
        swarm_id: &str,
        state: &ValidationState,
    ) -> Result<()> {
        let path = self.swarm_dir(swarm_id).join("validation-state.json");
        let content =
            serde_json::to_vec_pretty(state).context("failed to serialise validation state")?;
        atomic_write(&path, &content).await
    }

    /// Read the persisted validation state. Returns an empty state if the
    /// file does not exist (swarm was created before Phase 2, or no
    /// assertions have been verified yet).
    pub async fn read_validation_state(&self, swarm_id: &str) -> Result<ValidationState> {
        let path = self.swarm_dir(swarm_id).join("validation-state.json");
        match tokio::fs::read_to_string(&path).await {
            Ok(data) => serde_json::from_str(&data)
                .with_context(|| format!("failed to parse {}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(ValidationState::default()),
            Err(e) => Err(e).with_context(|| format!("failed to read {}", path.display())),
        }
    }

    // -- Reads ---------------------------------------------------------------

    /// Read the persisted feature list. Returns an empty vec if the file
    /// does not exist (swarm created but never started).
    pub async fn read_features(&self, swarm_id: &str) -> Result<Vec<Feature>> {
        let path = self.swarm_dir(swarm_id).join("features.json");
        match tokio::fs::read_to_string(&path).await {
            Ok(data) => serde_json::from_str::<Vec<Feature>>(&data)
                .with_context(|| format!("failed to parse {}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(e) => Err(e).with_context(|| format!("failed to read {}", path.display())),
        }
    }

    /// Read the persisted milestone list. Returns an empty vec if the file
    /// does not exist (the swarm was created before milestones were wired in,
    /// or planning produced no milestones).
    pub async fn read_milestones(&self, swarm_id: &str) -> Result<Vec<Milestone>> {
        let path = self.swarm_dir(swarm_id).join("milestones.json");
        match tokio::fs::read_to_string(&path).await {
            Ok(data) => serde_json::from_str::<Vec<Milestone>>(&data)
                .with_context(|| format!("failed to parse {}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(e) => Err(e).with_context(|| format!("failed to read {}", path.display())),
        }
    }

    // -- Optional per-swarm context artifacts (Phase 3) ----------------------
    //
    // These three files are optional scaffolding the user can populate (or
    // a future Queen post-acceptance step can author) to give Workers richer
    // per-project context. Each read returns Ok(None) when the file is
    // absent — they are best-effort enhancements, never required.

    /// Read the optional `services.yaml` file for a swarm, parsed into a
    /// [`ServicesFile`]. Returns `Ok(None)` when the file does not exist.
    pub async fn read_services_yaml(&self, swarm_id: &str) -> Result<Option<ServicesFile>> {
        let path = self.swarm_dir(swarm_id).join("services.yaml");
        match tokio::fs::read_to_string(&path).await {
            Ok(data) => {
                let parsed = parse_services_yaml(&data)
                    .with_context(|| format!("failed to parse {}", path.display()))?;
                Ok(Some(parsed))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e).with_context(|| format!("failed to read {}", path.display())),
        }
    }

    /// Read the optional `AGENTS.md` file (project conventions / off-limits
    /// paths) for a swarm. Returns `Ok(None)` when absent.
    pub async fn read_agents_md(&self, swarm_id: &str) -> Result<Option<String>> {
        let path = self.swarm_dir(swarm_id).join("AGENTS.md");
        match tokio::fs::read_to_string(&path).await {
            Ok(data) => Ok(Some(data)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e).with_context(|| format!("failed to read {}", path.display())),
        }
    }

    /// Read the optional `notes.md` file (architecture overview / env notes /
    /// validation surfaces — the consolidated replacement for Factory's
    /// `library/*.md` triplet) for a swarm. Returns `Ok(None)` when absent.
    pub async fn read_notes_md(&self, swarm_id: &str) -> Result<Option<String>> {
        let path = self.swarm_dir(swarm_id).join("notes.md");
        match tokio::fs::read_to_string(&path).await {
            Ok(data) => Ok(Some(data)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e).with_context(|| format!("failed to read {}", path.display())),
        }
    }

    /// Read the persisted swarm state.
    pub async fn read_state(&self, swarm_id: &str) -> Result<Option<SwarmState>> {
        let path = self.swarm_dir(swarm_id).join("state.json");
        match tokio::fs::read_to_string(&path).await {
            Ok(data) => {
                let state: SwarmState = serde_json::from_str(&data)
                    .with_context(|| format!("failed to parse {}", path.display()))?;
                Ok(Some(state))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e).with_context(|| format!("failed to read {}", path.display())),
        }
    }

    /// Delete all persisted data for a swarm.
    pub async fn delete_swarm(&self, swarm_id: &str) -> Result<()> {
        let dir = self.swarm_dir(swarm_id);
        match tokio::fs::remove_dir_all(&dir).await {
            Ok(()) => {
                info!("deleted swarm directory at {}", dir.display());
                Ok(())
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e)
                .with_context(|| format!("failed to delete swarm directory {}", dir.display())),
        }
    }

    /// List all known swarm IDs (directory names under `base_dir`).
    pub async fn list_swarms(&self) -> Result<Vec<String>> {
        let mut dir = match tokio::fs::read_dir(&self.base_dir).await {
            Ok(d) => d,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => {
                return Err(e)
                    .with_context(|| format!("failed to list {}", self.base_dir.display()));
            }
        };
        let mut ids = Vec::new();
        while let Some(entry) = dir
            .next_entry()
            .await
            .with_context(|| format!("failed to read entry under {}", self.base_dir.display()))?
        {
            let file_type = entry.file_type().await.with_context(|| {
                format!("failed to stat entry under {}", self.base_dir.display())
            })?;
            if file_type.is_dir() {
                if let Some(name) = entry.file_name().to_str() {
                    ids.push(name.to_string());
                }
            }
        }
        ids.sort();
        Ok(ids)
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Generate a temp filename in `parent` modelled on `tempfile::NamedTempFile`
/// (a `.tmp` suffix plus a chunk of randomness so concurrent writers don't
/// collide). We roll our own because `tempfile::NamedTempFile` is sync-only
/// and uses `O_CREAT|O_EXCL` plus a thread-local RNG; emulating that in
/// tokio costs us nothing and keeps the whole write path off the blocking
/// std::fs path.
fn temp_path_in(parent: &Path) -> PathBuf {
    let mut bytes = [0u8; 12];
    rand::thread_rng().fill_bytes(&mut bytes);
    // hex-encode without pulling in another crate.
    let mut suffix = String::with_capacity(bytes.len() * 2 + 5);
    suffix.push_str(".tmp.");
    for b in bytes.iter() {
        suffix.push_str(&format!("{:02x}", b));
    }
    parent.join(suffix)
}

/// Write `content` to `path` atomically via a temporary file and rename.
///
/// **Durability invariant**: `tokio::fs::rename` performs `rename(2)`, which
/// is atomic with respect to readers but the *directory entry change* itself
/// is only durable after fsync on the parent directory. On ext4/xfs/APFS a
/// power loss between `rename` and the next directory commit can leave the
/// target missing or still pointing at an old inode. We therefore fsync the
/// parent directory after the rename — see `sync_parent_dir`.
///
/// On failure the temp file is best-effort removed; if cleanup fails the
/// error is logged and swallowed (the user-facing error from the actual
/// failed step is what matters).
pub(crate) async fn atomic_write(path: &Path, content: &[u8]) -> Result<()> {
    let parent = path.parent().context("path has no parent directory")?;
    tracing::trace!(
        path = %path.display(),
        content_len = content.len(),
        "atomic_write"
    );
    tokio::fs::create_dir_all(parent)
        .await
        .with_context(|| format!("failed to create parent directory {}", parent.display()))?;

    let tmp = temp_path_in(parent);

    // RAII cleanup: on any early return remove the half-written temp file so
    // we don't leak `.tmp.*` cruft into the swarm directory.
    let cleanup_path = tmp.clone();
    let mut cleanup_armed = true;
    let result: Result<()> = async {
        let mut file = tokio::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&tmp)
            .await
            .with_context(|| format!("failed to create tempfile in {}", parent.display()))?;

        file.write_all(content)
            .await
            .context("failed to write to tempfile")?;

        // Crash-durability: flush + fsync the file BEFORE rename so the
        // rename is over data that already hit disk.
        file.flush().await.context("failed to flush tempfile")?;
        file.sync_all().await.context("failed to sync tempfile")?;
        drop(file);

        tokio::fs::rename(&tmp, path)
            .await
            .with_context(|| format!("failed to rename tempfile to {}", path.display()))?;

        // Make the rename itself durable.
        sync_parent_dir(path)
            .await
            .with_context(|| format!("failed to fsync parent directory of {}", path.display()))?;

        Ok(())
    }
    .await;

    if result.is_ok() {
        cleanup_armed = false;
    }
    if cleanup_armed {
        // Best-effort cleanup; ignore NotFound (rename already moved it).
        if let Err(e) = tokio::fs::remove_file(&cleanup_path).await {
            if e.kind() != std::io::ErrorKind::NotFound {
                tracing::warn!(
                    tempfile = %cleanup_path.display(),
                    error = %e,
                    "atomic_write: failed to clean up tempfile after error"
                );
            }
        }
    }
    result
}

/// fsync the parent directory of `path` so a preceding rename(2) is durable
/// across a power loss.
///
/// On POSIX filesystems (ext4, xfs, btrfs, APFS) the rename done by
/// `tokio::fs::rename` is atomic for readers but the directory entry update
/// itself is only guaranteed to survive a crash after the directory has been
/// fsynced. Without this, a power loss between rename and the next implicit
/// directory commit can resurrect the old name or leave it dangling.
///
/// On Windows directory fsync is unsupported (and unnecessary — NTFS handles
/// metadata journaling itself), so this is a no-op.
#[cfg(unix)]
pub(crate) async fn sync_parent_dir(path: &Path) -> std::io::Result<()> {
    let parent = match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
        // No parent (root or empty) — nothing to fsync.
        _ => return Ok(()),
    };
    let dir = tokio::fs::File::open(&parent).await?;
    dir.sync_all().await
}

/// Windows no-op (NTFS metadata journaling makes directory fsync unnecessary
/// and the OS does not support opening a directory for fsync the same way).
#[cfg(not(unix))]
pub(crate) async fn sync_parent_dir(_path: &Path) -> std::io::Result<()> {
    Ok(())
}

/// Synchronous sibling of [`sync_parent_dir`].
///
/// Kept for the small handful of pure-sync callers that already run on a
/// blocking thread (e.g. `Config::write_bytes_blocking` invoked via
/// `tokio::task::spawn_blocking`, `SecretStore::save_to_file` invoked from a
/// keyring-bound code path). Those callers can't `.await`, and pulling them
/// into an async context just to await directory fsync is more invasive than
/// keeping a sync wrapper around `std::fs::File::sync_all`.
#[cfg(unix)]
pub(crate) fn sync_parent_dir_blocking(path: &Path) -> std::io::Result<()> {
    let parent = match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => return Ok(()),
    };
    let dir = std::fs::File::open(parent)?;
    dir.sync_all()
}

#[cfg(not(unix))]
pub(crate) fn sync_parent_dir_blocking(_path: &Path) -> std::io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn make_milestone(
        id: &str,
        name: &str,
        features: Vec<&str>,
        assertions: Vec<&str>,
    ) -> Milestone {
        Milestone {
            id: id.to_string(),
            name: name.to_string(),
            features: features.into_iter().map(String::from).collect(),
            assertions: assertions.into_iter().map(String::from).collect(),
            sealed: false,
        }
    }

    #[tokio::test]
    async fn test_write_and_read_features_roundtrip() {
        let tmp = TempDir::new().expect("tempdir");
        let store = SwarmStore::new(tmp.path());
        let swarm_id = "swarm-feat-rt";
        store.init_swarm(swarm_id).await.expect("init_swarm");

        let features = vec![
            Feature::new("f1".into(), "Feature 1".into(), "do thing".into()),
            Feature::new("f2".into(), "Feature 2".into(), "do other thing".into()),
        ];
        store
            .write_features(swarm_id, &features)
            .await
            .expect("write_features");

        let read = store.read_features(swarm_id).await.expect("read_features");
        assert_eq!(read.len(), 2);
        assert_eq!(read[0].id, "f1");
        assert_eq!(read[0].name, "Feature 1");
        assert_eq!(read[1].id, "f2");
    }

    #[tokio::test]
    async fn test_read_features_returns_empty_if_missing() {
        let tmp = TempDir::new().expect("tempdir");
        let store = SwarmStore::new(tmp.path());
        let swarm_id = "swarm-feat-empty";
        store.init_swarm(swarm_id).await.expect("init_swarm");
        let read = store.read_features(swarm_id).await.expect("read_features");
        assert!(read.is_empty());
    }

    #[tokio::test]
    async fn test_write_and_read_milestones_roundtrip() {
        let tmp = TempDir::new().expect("tempdir");
        let store = SwarmStore::new(tmp.path());
        let swarm_id = "swarm-rt";
        store.init_swarm(swarm_id).await.expect("init_swarm");

        let milestones = vec![
            make_milestone("m1", "Setup", vec!["f1", "f2"], vec!["builds"]),
            make_milestone("m2", "Polish", vec!["f3"], vec!["lints clean"]),
        ];

        store
            .write_milestones(swarm_id, &milestones)
            .await
            .expect("write_milestones");

        let read = store
            .read_milestones(swarm_id)
            .await
            .expect("read_milestones");
        assert_eq!(read.len(), 2);
        assert_eq!(read[0].id, "m1");
        assert_eq!(read[0].name, "Setup");
        assert_eq!(read[0].features, vec!["f1", "f2"]);
        assert_eq!(read[0].assertions, vec!["builds"]);
        assert!(!read[0].sealed);

        assert_eq!(read[1].id, "m2");
        assert_eq!(read[1].features, vec!["f3"]);
        assert!(!read[1].sealed);
    }

    #[tokio::test]
    async fn test_read_milestones_returns_empty_if_missing() {
        let tmp = TempDir::new().expect("tempdir");
        let store = SwarmStore::new(tmp.path());
        let swarm_id = "swarm-empty";
        store.init_swarm(swarm_id).await.expect("init_swarm");

        // No milestones.json written yet.
        let read = store
            .read_milestones(swarm_id)
            .await
            .expect("read_milestones");
        assert!(read.is_empty());
    }

    #[tokio::test]
    async fn test_milestones_atomic_write() {
        let tmp = TempDir::new().expect("tempdir");
        let store = SwarmStore::new(tmp.path());
        let swarm_id = "swarm-atomic";
        store.init_swarm(swarm_id).await.expect("init_swarm");

        let first = vec![make_milestone("m1", "First", vec!["f1"], vec![])];
        store
            .write_milestones(swarm_id, &first)
            .await
            .expect("write 1");

        let second = vec![
            make_milestone("mA", "Alpha", vec!["fA"], vec!["aA"]),
            make_milestone("mB", "Beta", vec!["fB1", "fB2"], vec!["aB"]),
        ];
        store
            .write_milestones(swarm_id, &second)
            .await
            .expect("write 2");

        // Second write must fully replace the first -- no torn / partial content.
        let read = store
            .read_milestones(swarm_id)
            .await
            .expect("read_milestones");
        assert_eq!(read.len(), 2);
        assert_eq!(read[0].id, "mA");
        assert_eq!(read[1].id, "mB");
        // Crucially, the old "m1" must not survive.
        assert!(read.iter().all(|m| m.id != "m1"));
    }

    // -- Phase 3: optional context-artifact reads ---------------------------

    #[tokio::test]
    async fn test_read_services_yaml_absent_returns_none() {
        let tmp = TempDir::new().expect("tempdir");
        let store = SwarmStore::new(tmp.path());
        let swarm_id = "swarm-svc-absent";
        store.init_swarm(swarm_id).await.expect("init_swarm");

        let read = store.read_services_yaml(swarm_id).await.expect("read");
        assert!(read.is_none());
    }

    #[tokio::test]
    async fn test_read_services_yaml_present() {
        let tmp = TempDir::new().expect("tempdir");
        let store = SwarmStore::new(tmp.path());
        let swarm_id = "swarm-svc-present";
        store.init_swarm(swarm_id).await.expect("init_swarm");

        let yaml =
            "commands:\n  test: \"cargo test\"\nservices:\n  - name: redis\n    port: 6379\n";
        tokio::fs::write(store.swarm_dir(swarm_id).join("services.yaml"), yaml)
            .await
            .expect("write yaml");

        let read = store
            .read_services_yaml(swarm_id)
            .await
            .expect("read")
            .expect("present");
        assert_eq!(
            read.commands.get("test").map(String::as_str),
            Some("cargo test")
        );
        assert_eq!(read.services.len(), 1);
        assert_eq!(read.services[0].name, "redis");
        assert_eq!(read.services[0].port, Some(6379));
    }

    #[tokio::test]
    async fn test_read_agents_md_absent_returns_none() {
        let tmp = TempDir::new().expect("tempdir");
        let store = SwarmStore::new(tmp.path());
        let swarm_id = "swarm-agents-absent";
        store.init_swarm(swarm_id).await.expect("init_swarm");

        let read = store.read_agents_md(swarm_id).await.expect("read");
        assert!(read.is_none());
    }

    #[tokio::test]
    async fn test_read_agents_md_present() {
        let tmp = TempDir::new().expect("tempdir");
        let store = SwarmStore::new(tmp.path());
        let swarm_id = "swarm-agents-present";
        store.init_swarm(swarm_id).await.expect("init_swarm");

        let body = "# Project Conventions\n- Use snake_case for all identifiers.\n";
        tokio::fs::write(store.swarm_dir(swarm_id).join("AGENTS.md"), body)
            .await
            .expect("write AGENTS.md");

        let read = store
            .read_agents_md(swarm_id)
            .await
            .expect("read")
            .expect("present");
        assert!(read.contains("Project Conventions"));
        assert!(read.contains("snake_case"));
    }

    #[tokio::test]
    async fn test_read_notes_md_absent_returns_none() {
        let tmp = TempDir::new().expect("tempdir");
        let store = SwarmStore::new(tmp.path());
        let swarm_id = "swarm-notes-absent";
        store.init_swarm(swarm_id).await.expect("init_swarm");

        let read = store.read_notes_md(swarm_id).await.expect("read");
        assert!(read.is_none());
    }

    #[tokio::test]
    async fn test_read_notes_md_present() {
        let tmp = TempDir::new().expect("tempdir");
        let store = SwarmStore::new(tmp.path());
        let swarm_id = "swarm-notes-present";
        store.init_swarm(swarm_id).await.expect("init_swarm");

        let body = "# Architecture Overview\n\nA Rust + Tauri app.\n";
        tokio::fs::write(store.swarm_dir(swarm_id).join("notes.md"), body)
            .await
            .expect("write notes.md");

        let read = store
            .read_notes_md(swarm_id)
            .await
            .expect("read")
            .expect("present");
        assert!(read.contains("Architecture Overview"));
    }

    #[tokio::test]
    async fn test_read_services_yaml_malformed_errors() {
        let tmp = TempDir::new().expect("tempdir");
        let store = SwarmStore::new(tmp.path());
        let swarm_id = "swarm-svc-bad";
        store.init_swarm(swarm_id).await.expect("init_swarm");

        // Genuinely malformed YAML: unbalanced quote.
        let yaml = "commands:\n  install: \"unterminated\n  test: bad";
        tokio::fs::write(store.swarm_dir(swarm_id).join("services.yaml"), yaml)
            .await
            .expect("write yaml");

        let result = store.read_services_yaml(swarm_id).await;
        assert!(result.is_err());
    }

    // -- Validation contract / state (Phase 2) -----------------------------

    #[tokio::test]
    async fn test_read_validation_state_absent_returns_default() {
        let tmp = TempDir::new().expect("tempdir");
        let store = SwarmStore::new(tmp.path());
        let swarm_id = "swarm-val-absent";
        store.init_swarm(swarm_id).await.expect("init_swarm");

        let read = store
            .read_validation_state(swarm_id)
            .await
            .expect("read default");
        assert!(read.assertions.is_empty());
    }

    #[tokio::test]
    async fn test_write_and_read_validation_state_roundtrip() {
        use crate::core::validation::{AssertionStatus, ValidationState};

        let tmp = TempDir::new().expect("tempdir");
        let store = SwarmStore::new(tmp.path());
        let swarm_id = "swarm-val-rt";
        store.init_swarm(swarm_id).await.expect("init_swarm");

        let mut state = ValidationState::default();
        state.record("VAL-FND-001", AssertionStatus::Passed, None);
        state.record(
            "VAL-FND-002",
            AssertionStatus::Failed,
            Some("compile error".into()),
        );
        store
            .write_validation_state(swarm_id, &state)
            .await
            .expect("write state");

        let read = store
            .read_validation_state(swarm_id)
            .await
            .expect("read state");
        assert_eq!(read.assertions.len(), 2);
        assert_eq!(
            read.assertions.get("VAL-FND-001").unwrap().status,
            AssertionStatus::Passed
        );
        let fnd2 = read.assertions.get("VAL-FND-002").expect("VAL-FND-002");
        assert_eq!(fnd2.status, AssertionStatus::Failed);
        assert_eq!(fnd2.last_error.as_deref(), Some("compile error"));
    }

    // -- Discovered issues (Phase 5C) ---------------------------------------

    #[tokio::test]
    async fn test_append_discovered_issue_writes_valid_jsonl() {
        use crate::core::handoff::{DiscoveredIssue, IssueSeverity};

        let tmp = TempDir::new().expect("tempdir");
        let store = SwarmStore::new(tmp.path());
        let swarm_id = "swarm-issues";
        store.init_swarm(swarm_id).await.expect("init_swarm");

        let one = DiscoveredIssue {
            severity: IssueSeverity::Warn,
            description: "noticed deprecated API in fooBar()".to_string(),
            suggested_fix: Some("migrate to v2 of the SDK".to_string()),
        };
        let two = DiscoveredIssue {
            severity: IssueSeverity::Info,
            description: "lockfile out of date".to_string(),
            suggested_fix: None,
        };

        store
            .append_discovered_issue(swarm_id, "feat-1", &one)
            .await
            .expect("append 1");
        store
            .append_discovered_issue(swarm_id, "feat-2", &two)
            .await
            .expect("append 2");

        let path = store.swarm_dir(swarm_id).join("discovered_issues.jsonl");
        let body = tokio::fs::read_to_string(&path).await.expect("read jsonl");
        let lines: Vec<&str> = body.lines().filter(|l| !l.trim().is_empty()).collect();
        assert_eq!(lines.len(), 2, "expected exactly 2 lines, got: {:?}", lines);

        // Both lines must be valid JSON with the expected schema.
        let line1: serde_json::Value =
            serde_json::from_str(lines[0]).expect("line 1 is valid JSON");
        assert_eq!(line1["feature_id"], "feat-1");
        assert_eq!(line1["severity"], "warn");
        assert_eq!(line1["description"], "noticed deprecated API in fooBar()");
        assert_eq!(line1["suggested_fix"], "migrate to v2 of the SDK");
        assert!(
            line1["recorded_at"].is_string(),
            "recorded_at must be a string timestamp"
        );

        let line2: serde_json::Value =
            serde_json::from_str(lines[1]).expect("line 2 is valid JSON");
        assert_eq!(line2["feature_id"], "feat-2");
        assert_eq!(line2["severity"], "info");
        // suggested_fix=None should round-trip as null in JSON.
        assert!(line2["suggested_fix"].is_null());
    }

    #[tokio::test]
    async fn test_delete_swarm_removes_activity_log() {
        use std::io::Write as _;

        let tmp = TempDir::new().expect("tempdir");
        let store = SwarmStore::new(tmp.path());
        let swarm_id = "swarm-delete-activity";
        store.init_swarm(swarm_id).await.expect("init_swarm");

        // Drop a synthetic activity_log.jsonl into the swarm directory.
        let activity_path = store.activity_log_path(swarm_id);
        let mut f = std::fs::File::create(&activity_path).expect("create activity log");
        writeln!(f, "{{\"seq\":1,\"kind\":\"text\",\"text\":\"hi\"}}").expect("write line");
        drop(f);
        assert!(activity_path.exists(), "precondition: activity log present");

        store.delete_swarm(swarm_id).await.expect("delete_swarm");

        assert!(
            !activity_path.exists(),
            "activity log must be removed by delete_swarm"
        );
        assert!(
            !store.swarm_dir(swarm_id).exists(),
            "swarm directory must be removed by delete_swarm"
        );
    }

    #[test]
    fn test_activity_log_path_is_inside_swarm_dir() {
        let tmp = TempDir::new().expect("tempdir");
        let store = SwarmStore::new(tmp.path());
        let swarm_id = "swarm-activity-path";
        let p = store.activity_log_path(swarm_id);
        assert_eq!(
            p.file_name().and_then(|s| s.to_str()),
            Some("activity_log.jsonl")
        );
        assert_eq!(p.parent(), Some(store.swarm_dir(swarm_id).as_path()));
    }

    #[tokio::test]
    async fn test_append_discovered_issue_creates_dir_if_missing() {
        use crate::core::handoff::{DiscoveredIssue, IssueSeverity};

        let tmp = TempDir::new().expect("tempdir");
        let store = SwarmStore::new(tmp.path());
        let swarm_id = "swarm-no-init";
        // Deliberately do NOT call init_swarm — append must auto-create.

        let issue = DiscoveredIssue {
            severity: IssueSeverity::Error,
            description: "missing migration".to_string(),
            suggested_fix: None,
        };
        store
            .append_discovered_issue(swarm_id, "feat-x", &issue)
            .await
            .expect("append without init");

        let path = store.swarm_dir(swarm_id).join("discovered_issues.jsonl");
        assert!(path.exists(), "jsonl file must be created");
    }

    #[tokio::test]
    async fn test_write_validation_contract_renders_markdown() {
        use crate::core::validation::assign_assertion_ids;

        let tmp = TempDir::new().expect("tempdir");
        let store = SwarmStore::new(tmp.path());
        let swarm_id = "swarm-contract";
        store.init_swarm(swarm_id).await.expect("init_swarm");

        let milestones = vec![make_milestone(
            "fnd",
            "Foundations",
            vec!["f1"],
            vec!["cargo check passes", "tests green"],
        )];
        let assigned = assign_assertion_ids(&milestones);

        store
            .write_validation_contract(swarm_id, &milestones, &assigned)
            .await
            .expect("write contract");

        let path = store.swarm_dir(swarm_id).join("validation-contract.md");
        let body = tokio::fs::read_to_string(&path).await.expect("read");
        assert!(body.contains("# Validation Contract"));
        assert!(body.contains("VAL-FND-001"));
        assert!(body.contains("cargo check passes"));
        assert!(body.contains("Foundations"));
    }

    // -- Audit 3.1 regression: long write must not block other tokio tasks --

    #[tokio::test(flavor = "current_thread")]
    async fn write_state_does_not_starve_concurrent_tick() {
        // Audit 3.1: SwarmStore methods were sync std::fs::* and would stall
        // any tokio worker thread for the duration of the disk write. On a
        // current-thread runtime that meant a long `write_state` could
        // starve concurrent tasks entirely.
        //
        // This test simulates the Queen's situation: a tight orchestration
        // loop (the "tick") must keep ticking while `write_state` is in
        // flight. We bound the tick latency at 200ms — well above any sane
        // tempdir write, well below what a fully-blocking sync write would
        // have produced on a current-thread runtime if either future stalled
        // the other.
        use std::sync::Arc;
        use std::time::{Duration, Instant};

        let tmp = TempDir::new().expect("tempdir");
        let store = Arc::new(SwarmStore::new(tmp.path()));
        let swarm_id = "swarm-no-starve";
        store.init_swarm(swarm_id).await.expect("init");

        // Build a large-ish SwarmState (lots of features) so the serialise
        // + write actually has bytes to push.
        use crate::domain::swarm::{ModelSettings, SwarmConfig, SwarmState};
        let mut config = SwarmConfig {
            name: "starve-test".into(),
            description: "x".repeat(2_000),
            working_directory: tmp.path().display().to_string(),
            model_settings: ModelSettings::default(),
            features: Vec::new(),
            milestones: Vec::new(),
        };
        config.features.extend((0..200).map(|i| {
            Feature::new(
                format!("feat-{i}"),
                format!("Feature {i}"),
                "y".repeat(1_000),
            )
        }));
        let state = SwarmState::from_config(&config);

        // Spawn a concurrent "tick" that yields and sleeps 5ms in a loop,
        // tracking the worst gap between iterations. If `write_state`
        // monopolised the current-thread runtime the gap would jump to the
        // full duration of the synchronous write.
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stop_ref = Arc::clone(&stop);
        let tick = tokio::spawn(async move {
            let mut worst_gap = Duration::ZERO;
            let mut last = Instant::now();
            while !stop_ref.load(std::sync::atomic::Ordering::Relaxed) {
                tokio::time::sleep(Duration::from_millis(5)).await;
                let now = Instant::now();
                let gap = now.duration_since(last);
                if gap > worst_gap {
                    worst_gap = gap;
                }
                last = now;
            }
            worst_gap
        });

        // Fire several concurrent writes; each one must yield to the runtime.
        for _ in 0..5 {
            store
                .write_state(swarm_id, &state)
                .await
                .expect("write_state");
        }

        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        let worst_gap = tick.await.expect("tick joined");

        // 200ms is a generous ceiling — on a CI box doing a JSON serialise +
        // ~200KB write the tick should never stall this long if the store
        // truly yields. Anything north of this means SwarmStore is back to
        // blocking the runtime.
        assert!(
            worst_gap < Duration::from_millis(200),
            "concurrent tick stalled {:?}; write_state is blocking the runtime (audit 3.1 regression)",
            worst_gap
        );
    }
}
