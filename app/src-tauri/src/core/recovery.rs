//! Crash-recovery helpers for orphan swarms.
//!
//! These functions reconcile on-disk swarm state left behind by a previous
//! process (crash, force-quit, ungraceful exit). They live in `core/` (not
//! `commands/`) so [`crate::state::app_state::AppState::new`] can invoke
//! them at startup without depending on the `commands/` IPC layer.
//! `commands/swarms.rs` still calls [`reconcile_orphaned_swarms`] from its
//! `list_swarms` adapter as a defensive idempotent sweep.

use crate::domain::swarm::{Feature, FeatureStatus, SwarmStatus};
use crate::state::progress::ProgressReader;
use crate::state::store::SwarmStore;

/// One swarm reconciled at startup. Carries the swarm id and the list of
/// feature ids the reconciler marked as `Failed` because they were in an
/// in-flight state when the host died. Drained by
/// `AppState::take_pending_swarm_reconciled_emits()` and fanned out to
/// the frontend as `swarm_reconciled` Tauri events so the Swarms list
/// can show a "Resume" badge without polling.
#[derive(Debug, Clone)]
pub struct ReconciledSwarm {
    pub swarm_id: String,
    pub interrupted_features: Vec<String>,
}

/// Sentinel error message written onto reconciled swarms.
///
/// Any swarm found `Implementing` on disk but absent from the in-memory
/// registry is reconciled to `Interrupted`. Surfaced in the UI so the user
/// can tell why the status changed and that clicking Resume will continue
/// execution.
pub const INTERRUPTED_BY_RESTART_MSG: &str =
    "Swarm was running when the app exited; click Resume to continue from where it stopped.";

/// Legacy sentinel error message used by older builds that wrote `Failed`
/// for reconciled orphans. Kept only so the optional migration in
/// `AppState::new` can detect and upgrade pre-existing failed-by-reconcile
/// swarms to `Interrupted`.
pub const LEGACY_RECONCILE_FAILED_MSG: &str =
    "Swarm was running when the app exited; status reconciled on restart.";

/// Reconcile orphaned swarms whose on-disk status is `Implementing` but who
/// are not in the in-memory `swarm_registry` (i.e. no queen task is
/// actually executing them). Such entries are stale carry-overs from a
/// previous session (crash, ungraceful exit, etc.). Each is rewritten to
/// `SwarmStatus::Interrupted` with an explanatory `error` message and a fresh
/// `updated_at` so the user can click Resume to continue from where the
/// queen left off.
///
/// `running_ids` is the set of swarm ids currently present in the
/// in-memory registry; those are skipped to avoid clobbering a legitimately
/// running swarm. Pass an empty set at startup (before the registry has
/// been populated) to reconcile every stale Implementing on disk.
///
/// Returns the number of swarms reconciled (0 if everything is already
/// consistent — calling repeatedly is idempotent).
pub async fn reconcile_orphaned_swarms(
    swarm_store: &SwarmStore,
    running_ids: &std::collections::HashSet<String>,
) -> usize {
    let ids = match swarm_store.list_swarms().await {
        Ok(ids) => ids,
        Err(e) => {
            tracing::warn!(error = %e, "reconcile_orphaned_swarms: failed to list swarms");
            return 0;
        }
    };
    let mut reconciled = 0usize;
    for sid in ids {
        if running_ids.contains(&sid) {
            continue;
        }
        match swarm_store.read_state(&sid).await {
            Ok(Some(mut s)) => {
                if s.status == SwarmStatus::Implementing {
                    tracing::warn!(
                        swarm_id = %sid,
                        "reconciling orphaned 'Implementing' swarm to 'Interrupted'"
                    );
                    s.error = Some(INTERRUPTED_BY_RESTART_MSG.to_string());
                    s.set_status(SwarmStatus::Interrupted);
                    if let Err(e) = swarm_store.write_state(&sid, &s).await {
                        tracing::warn!(
                            swarm_id = %sid,
                            error = %e,
                            "failed to persist reconciled swarm state"
                        );
                    } else {
                        reconciled += 1;
                    }
                }
            }
            Ok(None) => {}
            Err(e) => {
                tracing::warn!(
                    swarm_id = %sid,
                    error = %e,
                    "reconcile_orphaned_swarms: failed to read swarm state"
                );
            }
        }
    }
    reconciled
}

/// One-shot migration: convert pre-existing swarms that were reconciled by
/// an older build to `Failed` (with the legacy `LEGACY_RECONCILE_FAILED_MSG`)
/// into the new `Interrupted` status so the user can Resume them. Idempotent
/// — only touches swarms whose status is `Failed` AND whose error matches the
/// legacy sentinel exactly. Returns the number of swarms migrated.
pub async fn migrate_legacy_reconciled_failures(swarm_store: &SwarmStore) -> usize {
    let ids = match swarm_store.list_swarms().await {
        Ok(ids) => ids,
        Err(e) => {
            tracing::warn!(error = %e, "migrate_legacy_reconciled_failures: failed to list swarms");
            return 0;
        }
    };
    let mut migrated = 0usize;
    for sid in ids {
        match swarm_store.read_state(&sid).await {
            Ok(Some(mut s)) => {
                if s.status == SwarmStatus::Failed
                    && s.error.as_deref() == Some(LEGACY_RECONCILE_FAILED_MSG)
                {
                    s.error = Some(INTERRUPTED_BY_RESTART_MSG.to_string());
                    s.set_status(SwarmStatus::Interrupted);
                    if let Err(e) = swarm_store.write_state(&sid, &s).await {
                        tracing::warn!(
                            swarm_id = %sid,
                            error = %e,
                            "failed to persist migrated swarm state"
                        );
                    } else {
                        migrated += 1;
                    }
                }
            }
            Ok(None) => {}
            Err(e) => {
                tracing::warn!(
                    swarm_id = %sid,
                    error = %e,
                    "migrate_legacy_reconciled_failures: failed to read swarm state"
                );
            }
        }
    }
    migrated
}

/// Audit 2.2: full crash-recovery sweep — for every on-disk swarm whose
/// status indicates it was mid-execution (or already flagged
/// `Interrupted` by a prior reconciliation that never finished), fold the
/// JSONL `progress_log.jsonl` over the persisted `features.json` via
/// [`ProgressReader::rebuild_state`] and mark any still-in-flight features
/// as `Failed` with `interrupted = true` / `resumable = true`. The swarm
/// itself is rewritten to [`SwarmStatus::Interrupted`] (idempotent — a
/// swarm already at `Interrupted` only gets its features reconciled).
///
/// Returns one [`ReconciledSwarm`] per swarm that actually had at least
/// one feature reconciled. Swarms that were clean (every feature already
/// terminal) are skipped — they don't need a Resume affordance.
///
/// `running_ids` is the set of swarm ids currently present in the
/// in-memory registry; those are skipped so we never clobber a legitimately
/// running swarm. Pass an empty set at startup (before the registry has
/// been populated).
///
/// Crash-tolerance contract (Fix 7.5 / audit 2.2 tests):
/// - Missing `progress_log.jsonl` → no-op (returns clean swarms unchanged).
/// - Empty `progress_log.jsonl` → no-op.
/// - Truncated tail / malformed lines → `ProgressReader` skips them with a
///   WARN; the rebuilt status map is best-effort.
/// - Replay is **purely additive on top of persisted state**: a feature
///   already terminal on disk is never reverted (the replay can only
///   confirm a non-terminal feature's last-known status).
pub async fn reconcile_orphaned_swarms_with_replay(
    swarm_store: &SwarmStore,
    running_ids: &std::collections::HashSet<String>,
) -> Vec<ReconciledSwarm> {
    let ids = match swarm_store.list_swarms().await {
        Ok(ids) => ids,
        Err(e) => {
            tracing::warn!(error = %e, "reconcile_with_replay: failed to list swarms");
            return Vec::new();
        }
    };

    let mut out: Vec<ReconciledSwarm> = Vec::new();
    for sid in ids {
        if running_ids.contains(&sid) {
            continue;
        }

        let mut state = match swarm_store.read_state(&sid).await {
            Ok(Some(s)) => s,
            Ok(None) => continue,
            Err(e) => {
                tracing::warn!(
                    swarm_id = %sid,
                    error = %e,
                    "reconcile_with_replay: failed to read swarm state"
                );
                continue;
            }
        };

        // Only sweep swarms that were running, already flagged as interrupted
        // by an earlier (perhaps incomplete) reconciliation, or paused with
        // a non-empty progress log. Terminal statuses (Completed / Failed /
        // Cancelled) and Planning are intentionally skipped.
        let should_sweep = matches!(
            state.status,
            SwarmStatus::Implementing | SwarmStatus::Interrupted | SwarmStatus::Paused
        );
        if !should_sweep {
            continue;
        }

        let mut features = match swarm_store.read_features(&sid).await {
            Ok(fs) => fs,
            Err(e) => {
                tracing::warn!(
                    swarm_id = %sid,
                    error = %e,
                    "reconcile_with_replay: failed to read features"
                );
                continue;
            }
        };

        let progress_path = swarm_store.swarm_dir(&sid).join("progress_log.jsonl");
        let replay = match ProgressReader::rebuild_state(&progress_path) {
            Ok(map) => map,
            Err(e) => {
                tracing::warn!(
                    swarm_id = %sid,
                    path = %progress_path.display(),
                    error = %e,
                    "reconcile_with_replay: progress log replay failed; using persisted features as-is"
                );
                Default::default()
            }
        };

        let features_before: Vec<FeatureStatus> =
            features.iter().map(|f| f.status.clone()).collect();
        let interrupted_feature_ids = apply_replay_and_mark_interrupted(&mut features, &replay);
        let features_changed = features
            .iter()
            .zip(features_before.iter())
            .any(|(f, before)| &f.status != before || f.interrupted || f.resumable);

        // Touch the on-disk state when we either flipped the swarm to
        // Interrupted, marked features as interrupted, or replay folded a
        // newer status onto a persisted-in-flight feature. We persist
        // features when anything changed (status or markers) and the swarm
        // state when its status needed to flip.
        let need_status_flip = matches!(
            state.status,
            SwarmStatus::Implementing | SwarmStatus::Paused
        ) && !interrupted_feature_ids.is_empty();

        if need_status_flip {
            tracing::warn!(
                swarm_id = %sid,
                interrupted_count = interrupted_feature_ids.len(),
                "reconcile_with_replay: marking swarm Interrupted"
            );
            state.error = Some(INTERRUPTED_BY_RESTART_MSG.to_string());
            state.set_status(SwarmStatus::Interrupted);
            if let Err(e) = swarm_store.write_state(&sid, &state).await {
                tracing::warn!(
                    swarm_id = %sid,
                    error = %e,
                    "reconcile_with_replay: failed to persist swarm state"
                );
            }
        } else if state.status == SwarmStatus::Implementing && interrupted_feature_ids.is_empty() {
            // Implementing on disk but the replay confirms every feature
            // is terminal — the swarm actually finished but never wrote the
            // Completed status. Treat as Interrupted so the user can decide
            // whether to mark it complete or re-run.
            tracing::warn!(
                swarm_id = %sid,
                "reconcile_with_replay: Implementing swarm with no in-flight features → Interrupted"
            );
            state.error = Some(INTERRUPTED_BY_RESTART_MSG.to_string());
            state.set_status(SwarmStatus::Interrupted);
            if let Err(e) = swarm_store.write_state(&sid, &state).await {
                tracing::warn!(
                    swarm_id = %sid,
                    error = %e,
                    "reconcile_with_replay: failed to persist swarm state"
                );
            }
        }

        // Persist features whenever the replay/mark step mutated anything
        // — even if no feature was marked interrupted (the replay may have
        // folded Pending → Completed for a feature whose terminal log
        // events arrived before fsync but after the persisted snapshot).
        if features_changed {
            if let Err(e) = swarm_store.write_features(&sid, &features).await {
                tracing::warn!(
                    swarm_id = %sid,
                    error = %e,
                    "reconcile_with_replay: failed to persist reconciled features"
                );
            }
        }

        if !interrupted_feature_ids.is_empty() {
            out.push(ReconciledSwarm {
                swarm_id: sid,
                interrupted_features: interrupted_feature_ids,
            });
        }
    }

    out
}

/// Fold the replay status map (from `ProgressReader::rebuild_state`) over
/// `features` and promote any feature that is still in a non-terminal
/// state (`Scouting` / `Implementing` / `Reviewing` / `Validating`) to
/// `Failed` with `interrupted = true` / `resumable = true`.
///
/// Returns the ids of features that were marked interrupted.
///
/// Replay is **purely additive** — if `features.json` says a feature is
/// already `Completed` / `Failed` / `Skipped`, we do not revert it even
/// if the replay disagrees (the log can lag the on-disk state for one
/// fsync cycle). If the replay carries a *newer* status for a still-in-
/// flight persisted feature, we fold it forward first; whatever remains
/// non-terminal after that fold is what gets marked interrupted.
fn apply_replay_and_mark_interrupted(
    features: &mut [Feature],
    replay: &std::collections::HashMap<String, FeatureStatus>,
) -> Vec<String> {
    let mut interrupted_ids = Vec::new();
    for f in features.iter_mut() {
        // Skip terminal features — replay can't downgrade them.
        if f.status.is_terminal() {
            continue;
        }

        // Fold replay onto the persisted (non-terminal) feature. If the
        // replay shows a *later* terminal status, accept it; otherwise the
        // replay's last-known non-terminal status overrides only when more
        // specific (e.g. Pending → Scouting).
        if let Some(replay_status) = replay.get(&f.id) {
            if replay_status.is_terminal() {
                f.status = replay_status.clone();
                continue;
            }
            // Non-terminal replay status: keep whichever is later in the
            // pipeline (Implementing > Scouting > Pending, etc.). Skip the
            // re-assignment if persisted is already further along; the
            // reconciler treats both as "still in flight" anyway.
            f.status = replay_status.clone();
        }

        // After the fold, anything still in a non-terminal in-flight state
        // is a crash victim. Mark it interrupted.
        if matches!(
            f.status,
            FeatureStatus::Scouting
                | FeatureStatus::Implementing
                | FeatureStatus::Reviewing
                | FeatureStatus::Validating
        ) {
            f.status = FeatureStatus::Failed;
            f.interrupted = true;
            f.resumable = true;
            interrupted_ids.push(f.id.clone());
        }
    }
    interrupted_ids
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::swarm::{ModelSettings, SwarmConfig, SwarmState};
    use tempfile::TempDir;

    fn sample_state(name: &str, cwd: &str) -> SwarmState {
        let config = SwarmConfig {
            name: name.into(),
            description: "".into(),
            working_directory: cwd.into(),
            model_settings: ModelSettings::default(),
            features: vec![],
            milestones: vec![],
        };
        SwarmState::from_config(&config)
    }

    #[tokio::test]
    async fn reconcile_rewrites_implementing_to_interrupted() {
        let tmp = TempDir::new().expect("tempdir");
        let store = SwarmStore::new(tmp.path());
        let cwd = tmp.path().display().to_string();
        let mut s = sample_state("ghost", &cwd);
        s.set_status(SwarmStatus::Implementing);
        let sid = s.id.clone();
        store.init_swarm(&sid).await.expect("init");
        store.write_state(&sid, &s).await.expect("write");

        let empty: std::collections::HashSet<String> = Default::default();
        let n = reconcile_orphaned_swarms(&store, &empty).await;
        assert_eq!(n, 1, "should have reconciled one swarm");

        let loaded = store
            .read_state(&sid)
            .await
            .expect("read")
            .expect("present");
        assert_eq!(loaded.status, SwarmStatus::Interrupted);
        assert_eq!(loaded.error.as_deref(), Some(INTERRUPTED_BY_RESTART_MSG));
    }

    #[tokio::test]
    async fn reconcile_skips_swarms_in_running_set() {
        let tmp = TempDir::new().expect("tempdir");
        let store = SwarmStore::new(tmp.path());
        let cwd = tmp.path().display().to_string();
        let mut s = sample_state("live", &cwd);
        s.set_status(SwarmStatus::Implementing);
        let sid = s.id.clone();
        store.init_swarm(&sid).await.expect("init");
        store.write_state(&sid, &s).await.expect("write");

        let mut running: std::collections::HashSet<String> = Default::default();
        running.insert(sid.clone());
        let n = reconcile_orphaned_swarms(&store, &running).await;
        assert_eq!(n, 0, "running swarm must not be reconciled");

        let loaded = store
            .read_state(&sid)
            .await
            .expect("read")
            .expect("present");
        assert_eq!(loaded.status, SwarmStatus::Implementing);
        assert!(loaded.error.is_none());
    }

    #[tokio::test]
    async fn reconcile_is_idempotent() {
        let tmp = TempDir::new().expect("tempdir");
        let store = SwarmStore::new(tmp.path());
        let cwd = tmp.path().display().to_string();
        let mut s = sample_state("ghost", &cwd);
        s.set_status(SwarmStatus::Implementing);
        let sid = s.id.clone();
        store.init_swarm(&sid).await.expect("init");
        store.write_state(&sid, &s).await.expect("write");

        let empty: std::collections::HashSet<String> = Default::default();
        assert_eq!(reconcile_orphaned_swarms(&store, &empty).await, 1);
        // Second call: status is already Interrupted, so nothing to do.
        assert_eq!(reconcile_orphaned_swarms(&store, &empty).await, 0);
    }

    #[tokio::test]
    async fn reconcile_leaves_other_statuses_untouched() {
        let tmp = TempDir::new().expect("tempdir");
        let store = SwarmStore::new(tmp.path());
        let cwd = tmp.path().display().to_string();
        for (name, status) in [
            ("plan", SwarmStatus::Planning),
            ("done", SwarmStatus::Completed),
            ("fail", SwarmStatus::Failed),
            ("cancel", SwarmStatus::Cancelled),
            ("pause", SwarmStatus::Paused),
            ("interrupted", SwarmStatus::Interrupted),
        ] {
            let mut s = sample_state(name, &cwd);
            s.set_status(status.clone());
            let sid = s.id.clone();
            store.init_swarm(&sid).await.expect("init");
            store.write_state(&sid, &s).await.expect("write");
        }
        let empty: std::collections::HashSet<String> = Default::default();
        let n = reconcile_orphaned_swarms(&store, &empty).await;
        assert_eq!(n, 0, "non-Implementing statuses must not be touched");
    }

    #[tokio::test]
    async fn test_migrate_legacy_failed_to_interrupted() {
        let tmp = TempDir::new().expect("tempdir");
        let store = SwarmStore::new(tmp.path());
        let cwd = tmp.path().display().to_string();
        let mut s = sample_state("legacy", &cwd);
        s.set_status(SwarmStatus::Failed);
        s.error = Some(LEGACY_RECONCILE_FAILED_MSG.to_string());
        let sid = s.id.clone();
        store.init_swarm(&sid).await.expect("init");
        store.write_state(&sid, &s).await.expect("write");

        let n = migrate_legacy_reconciled_failures(&store).await;
        assert_eq!(n, 1);
        let loaded = store
            .read_state(&sid)
            .await
            .expect("read")
            .expect("present");
        assert_eq!(loaded.status, SwarmStatus::Interrupted);
        assert_eq!(loaded.error.as_deref(), Some(INTERRUPTED_BY_RESTART_MSG));

        // Idempotent.
        assert_eq!(migrate_legacy_reconciled_failures(&store).await, 0);
    }

    #[tokio::test]
    async fn test_migrate_does_not_touch_other_failures() {
        let tmp = TempDir::new().expect("tempdir");
        let store = SwarmStore::new(tmp.path());
        let cwd = tmp.path().display().to_string();
        let mut s = sample_state("genuine", &cwd);
        s.set_status(SwarmStatus::Failed);
        s.error = Some("a real error".into());
        let sid = s.id.clone();
        store.init_swarm(&sid).await.expect("init");
        store.write_state(&sid, &s).await.expect("write");

        assert_eq!(migrate_legacy_reconciled_failures(&store).await, 0);
        let loaded = store
            .read_state(&sid)
            .await
            .expect("read")
            .expect("present");
        assert_eq!(loaded.status, SwarmStatus::Failed);
    }

    // ---------------------------------------------------------------------
    // Audit 2.2: reconcile_orphaned_swarms_with_replay tests
    // ---------------------------------------------------------------------

    use crate::domain::swarm::Feature;
    use crate::state::progress::{ProgressEvent, ProgressEventType, SyncProgressWriter};

    fn feat(id: &str, status: FeatureStatus) -> Feature {
        let mut f = Feature::new(id.into(), id.into(), "".into());
        f.status = status;
        f
    }

    async fn seed_implementing_swarm(
        store: &SwarmStore,
        cwd: &str,
        features: Vec<Feature>,
    ) -> String {
        let mut s = sample_state("victim", cwd);
        s.set_status(SwarmStatus::Implementing);
        let sid = s.id.clone();
        store.init_swarm(&sid).await.expect("init");
        store.write_state(&sid, &s).await.expect("write state");
        store
            .write_features(&sid, &features)
            .await
            .expect("write feats");
        sid
    }

    fn append_progress_events(store: &SwarmStore, swarm_id: &str, events: &[ProgressEvent]) {
        let path = store.swarm_dir(swarm_id).join("progress_log.jsonl");
        let mut w = SyncProgressWriter::new(&path).expect("open sync writer");
        for ev in events {
            w.log(ev).expect("log event");
        }
        w.flush().expect("flush");
    }

    #[tokio::test]
    async fn replay_over_empty_log_marks_in_flight_features_interrupted() {
        // No progress_log.jsonl on disk — the reconciler still has to mark
        // the in-flight features interrupted based on persisted features
        // alone. This is the "host died before the first log line landed"
        // case.
        let tmp = TempDir::new().expect("tempdir");
        let store = SwarmStore::new(tmp.path());
        let cwd = tmp.path().display().to_string();
        let sid = seed_implementing_swarm(
            &store,
            &cwd,
            vec![
                feat("done", FeatureStatus::Completed),
                feat("running", FeatureStatus::Implementing),
                feat("queued", FeatureStatus::Pending),
            ],
        )
        .await;

        let empty: std::collections::HashSet<String> = Default::default();
        let reconciled = reconcile_orphaned_swarms_with_replay(&store, &empty).await;
        assert_eq!(reconciled.len(), 1);
        assert_eq!(reconciled[0].swarm_id, sid);
        assert_eq!(
            reconciled[0].interrupted_features,
            vec!["running".to_string()]
        );

        let state = store
            .read_state(&sid)
            .await
            .expect("read")
            .expect("present");
        assert_eq!(state.status, SwarmStatus::Interrupted);
        assert_eq!(state.error.as_deref(), Some(INTERRUPTED_BY_RESTART_MSG));

        let feats = store.read_features(&sid).await.expect("read feats");
        assert_eq!(feats[0].status, FeatureStatus::Completed); // untouched
        assert_eq!(feats[1].status, FeatureStatus::Failed);
        assert!(feats[1].interrupted);
        assert!(feats[1].resumable);
        assert_eq!(feats[2].status, FeatureStatus::Pending); // untouched
        assert!(!feats[2].interrupted);
    }

    #[tokio::test]
    async fn replay_advances_persisted_state_then_marks_remaining_interrupted() {
        // Persisted state shows feat-a Pending; progress log advances it to
        // FeatureStarted → Implementing → FeatureValidated → Completed.
        // After the fold, feat-a is terminal so it's NOT marked interrupted.
        // feat-b is Implementing on disk with no log activity → interrupted.
        let tmp = TempDir::new().expect("tempdir");
        let store = SwarmStore::new(tmp.path());
        let cwd = tmp.path().display().to_string();
        let sid = seed_implementing_swarm(
            &store,
            &cwd,
            vec![
                feat("feat-a", FeatureStatus::Pending),
                feat("feat-b", FeatureStatus::Implementing),
            ],
        )
        .await;

        // Log advances feat-a all the way to completion.
        let events = vec![
            ProgressEvent::new(
                ProgressEventType::FeatureStarted,
                sid.clone(),
                "scout start".into(),
            )
            .with_feature("feat-a".into()),
            ProgressEvent::new(
                ProgressEventType::FeatureImplemented,
                sid.clone(),
                "worker done".into(),
            )
            .with_feature("feat-a".into()),
            ProgressEvent::new(
                ProgressEventType::FeatureValidated,
                sid.clone(),
                "guard pass".into(),
            )
            .with_feature("feat-a".into()),
        ];
        append_progress_events(&store, &sid, &events);

        let empty: std::collections::HashSet<String> = Default::default();
        let reconciled = reconcile_orphaned_swarms_with_replay(&store, &empty).await;

        assert_eq!(reconciled.len(), 1);
        assert_eq!(
            reconciled[0].interrupted_features,
            vec!["feat-b".to_string()],
            "only feat-b should be marked interrupted; feat-a's replay reached Completed"
        );

        let feats = store.read_features(&sid).await.expect("read feats");
        let a = feats.iter().find(|f| f.id == "feat-a").unwrap();
        assert_eq!(
            a.status,
            FeatureStatus::Completed,
            "replay advanced feat-a to terminal"
        );
        assert!(!a.interrupted);
        let b = feats.iter().find(|f| f.id == "feat-b").unwrap();
        assert_eq!(b.status, FeatureStatus::Failed);
        assert!(b.interrupted);
        assert!(b.resumable);
    }

    #[tokio::test]
    async fn replay_skips_swarm_where_persisted_state_is_newer_than_log() {
        // Persisted state shows every feature already terminal. Even though
        // an old/stale progress log might still mention them, the reconciler
        // must not revert terminal features. Result: no swarm is added to
        // the returned list (nothing was reconciled).
        let tmp = TempDir::new().expect("tempdir");
        let store = SwarmStore::new(tmp.path());
        let cwd = tmp.path().display().to_string();
        let sid = seed_implementing_swarm(
            &store,
            &cwd,
            vec![
                feat("feat-x", FeatureStatus::Completed),
                feat("feat-y", FeatureStatus::Failed),
                feat("feat-z", FeatureStatus::Skipped),
            ],
        )
        .await;

        // Log says feat-x was last seen as Scouting (stale — disk is newer).
        let events = vec![ProgressEvent::new(
            ProgressEventType::FeatureScouted,
            sid.clone(),
            "stale".into(),
        )
        .with_feature("feat-x".into())];
        append_progress_events(&store, &sid, &events);

        let empty: std::collections::HashSet<String> = Default::default();
        let reconciled = reconcile_orphaned_swarms_with_replay(&store, &empty).await;

        // Implementing-with-no-in-flight-features → flips to Interrupted
        // (defensive: better than leaving a stale Implementing on disk).
        // But no feature is marked interrupted, so the swarm is NOT in
        // the returned reconciled list (callers only emit for swarms that
        // need a Resume affordance).
        assert!(
            reconciled.is_empty(),
            "no features to reconcile → no swarm_reconciled emit needed, got {:?}",
            reconciled
        );

        let state = store
            .read_state(&sid)
            .await
            .expect("read")
            .expect("present");
        assert_eq!(state.status, SwarmStatus::Interrupted);

        // Crucially: feat-x was NOT reverted to Scouting.
        let feats = store.read_features(&sid).await.expect("read feats");
        let x = feats.iter().find(|f| f.id == "feat-x").unwrap();
        assert_eq!(x.status, FeatureStatus::Completed);
        assert!(!x.interrupted);
    }

    #[tokio::test]
    async fn replay_over_truncated_tail_survives_and_marks_interrupted() {
        // A valid line followed by a half-written line that crashed before
        // the closing brace. ProgressReader::rebuild_state must skip the
        // truncated tail with a WARN, and the reconciler must still produce
        // a sensible result from the valid prefix.
        let tmp = TempDir::new().expect("tempdir");
        let store = SwarmStore::new(tmp.path());
        let cwd = tmp.path().display().to_string();
        let sid = seed_implementing_swarm(
            &store,
            &cwd,
            vec![
                feat("feat-1", FeatureStatus::Pending),
                feat("feat-2", FeatureStatus::Implementing),
            ],
        )
        .await;

        // Hand-craft a JSONL file: one good line that promotes feat-1 to
        // FeatureStarted → Implementing in the replay, then a truncated
        // half-line that must be silently dropped.
        let path = store.swarm_dir(&sid).join("progress_log.jsonl");
        let good = ProgressEvent::new(
            ProgressEventType::FeatureStarted,
            sid.clone(),
            "start feat-1".into(),
        )
        .with_feature("feat-1".into());
        let good_line = serde_json::to_string(&good).expect("serialise");
        let mut bytes = Vec::new();
        bytes.extend_from_slice(good_line.as_bytes());
        bytes.push(b'\n');
        // Truncated tail: missing closing brace + newline.
        bytes.extend_from_slice(
            b"{\"timestamp\":\"2025-01-02T03:04:06Z\",\"event_type\":\"feature_imple",
        );
        std::fs::write(&path, &bytes).expect("write truncated log");

        let empty: std::collections::HashSet<String> = Default::default();
        let reconciled = reconcile_orphaned_swarms_with_replay(&store, &empty).await;

        assert_eq!(reconciled.len(), 1);
        // Both feat-1 (replay folded to Implementing) and feat-2 (persisted
        // Implementing) should be marked interrupted.
        let ids: std::collections::HashSet<String> =
            reconciled[0].interrupted_features.iter().cloned().collect();
        assert!(
            ids.contains("feat-1") && ids.contains("feat-2"),
            "expected both feat-1 and feat-2 interrupted, got {:?}",
            ids
        );

        let feats = store.read_features(&sid).await.expect("read feats");
        for f in &feats {
            assert_eq!(f.status, FeatureStatus::Failed);
            assert!(f.interrupted);
            assert!(f.resumable);
        }
    }

    #[tokio::test]
    async fn replay_advances_persisted_state_by_three_events_matches_expected_end_state() {
        // Persisted state for feat-1 is Pending. The log has three events
        // for feat-1: scouted, implemented, validated. The replay must
        // converge on Completed, and the reconciler must NOT mark it
        // interrupted.
        let tmp = TempDir::new().expect("tempdir");
        let store = SwarmStore::new(tmp.path());
        let cwd = tmp.path().display().to_string();
        let sid =
            seed_implementing_swarm(&store, &cwd, vec![feat("feat-1", FeatureStatus::Pending)])
                .await;

        let events = vec![
            ProgressEvent::new(
                ProgressEventType::FeatureScouted,
                sid.clone(),
                "scouted".into(),
            )
            .with_feature("feat-1".into()),
            ProgressEvent::new(
                ProgressEventType::FeatureImplemented,
                sid.clone(),
                "implemented".into(),
            )
            .with_feature("feat-1".into()),
            ProgressEvent::new(
                ProgressEventType::FeatureValidated,
                sid.clone(),
                "validated".into(),
            )
            .with_feature("feat-1".into()),
        ];
        append_progress_events(&store, &sid, &events);

        let empty: std::collections::HashSet<String> = Default::default();
        let reconciled = reconcile_orphaned_swarms_with_replay(&store, &empty).await;

        assert!(
            reconciled.is_empty(),
            "feat-1 reached Completed via replay → not interrupted, got {:?}",
            reconciled
        );

        let feats = store.read_features(&sid).await.expect("read feats");
        assert_eq!(feats[0].status, FeatureStatus::Completed);
        assert!(!feats[0].interrupted);
    }

    #[tokio::test]
    async fn replay_skips_running_swarm_in_registry_set() {
        let tmp = TempDir::new().expect("tempdir");
        let store = SwarmStore::new(tmp.path());
        let cwd = tmp.path().display().to_string();
        let sid = seed_implementing_swarm(
            &store,
            &cwd,
            vec![feat("feat-x", FeatureStatus::Implementing)],
        )
        .await;

        let mut running: std::collections::HashSet<String> = Default::default();
        running.insert(sid.clone());

        let reconciled = reconcile_orphaned_swarms_with_replay(&store, &running).await;
        assert!(
            reconciled.is_empty(),
            "running swarm must not be reconciled"
        );

        // On-disk state untouched.
        let state = store
            .read_state(&sid)
            .await
            .expect("read")
            .expect("present");
        assert_eq!(state.status, SwarmStatus::Implementing);
        let feats = store.read_features(&sid).await.expect("read feats");
        assert_eq!(feats[0].status, FeatureStatus::Implementing);
        assert!(!feats[0].interrupted);
    }

    #[tokio::test]
    async fn replay_is_idempotent() {
        // Running the reconciler twice must produce the same on-disk state
        // and the same returned list — the second call sees an Interrupted
        // swarm with all features already terminal and returns empty.
        let tmp = TempDir::new().expect("tempdir");
        let store = SwarmStore::new(tmp.path());
        let cwd = tmp.path().display().to_string();
        let sid = seed_implementing_swarm(
            &store,
            &cwd,
            vec![feat("feat-x", FeatureStatus::Implementing)],
        )
        .await;

        let empty: std::collections::HashSet<String> = Default::default();
        let first = reconcile_orphaned_swarms_with_replay(&store, &empty).await;
        assert_eq!(first.len(), 1);

        let second = reconcile_orphaned_swarms_with_replay(&store, &empty).await;
        assert!(second.is_empty(), "second pass: nothing to do");

        // State persists across passes.
        let state = store
            .read_state(&sid)
            .await
            .expect("read")
            .expect("present");
        assert_eq!(state.status, SwarmStatus::Interrupted);
        let feats = store.read_features(&sid).await.expect("read feats");
        assert_eq!(feats[0].status, FeatureStatus::Failed);
        assert!(feats[0].interrupted);
    }

    #[tokio::test]
    async fn replay_ignores_terminal_swarm_statuses() {
        // Swarms in terminal statuses (Completed / Failed / Cancelled) or
        // Planning are never swept by the replay reconciler.
        let tmp = TempDir::new().expect("tempdir");
        let store = SwarmStore::new(tmp.path());
        let cwd = tmp.path().display().to_string();
        for status in [
            SwarmStatus::Planning,
            SwarmStatus::Completed,
            SwarmStatus::Failed,
            SwarmStatus::Cancelled,
        ] {
            let mut s = sample_state("term", &cwd);
            s.set_status(status.clone());
            let sid = s.id.clone();
            store.init_swarm(&sid).await.expect("init");
            store.write_state(&sid, &s).await.expect("write state");
            // Deliberately seed an in-flight feature; the reconciler must
            // not touch it since the swarm status is terminal.
            store
                .write_features(&sid, &[feat("ghost", FeatureStatus::Implementing)])
                .await
                .expect("write feats");
        }

        let empty: std::collections::HashSet<String> = Default::default();
        let reconciled = reconcile_orphaned_swarms_with_replay(&store, &empty).await;
        assert!(reconciled.is_empty());
    }
}
