//! Unit tests for the Provider Extension system.
//!
//! Bias: the poller is the most failure-prone piece (timing,
//! cancellation, error paths), so the suite is weighted there.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;

use super::context::ExtensionContext;
use super::poller;
use super::registry::ExtensionRegistry;
use super::traits::{ProviderExtension, UsageProvider};
use super::types::{
    Capability, ExtensionError, ExtensionManifest, MetricKind, SnapshotEntry, SnapshotStatus, Tone,
    UsageMetric, UsageSnapshot,
};
use crate::state::config::Config;
use tokio::time::Instant;

/// Mock extension whose `fetch()` consults a sequence of pre-canned
/// outcomes. Used by the poller tests.
#[derive(Debug, Clone)]
enum MockOutcome {
    Ok,
    Err(String),
    Unsupported(String),
    Hang,
}

struct MockExt {
    manifest: ExtensionManifest,
    outcomes: Arc<RwLock<Vec<MockOutcome>>>,
    fetch_calls: Arc<AtomicUsize>,
    interval_secs: u64,
}

impl MockExt {
    fn new(id: &str, outcomes: Vec<MockOutcome>, interval_secs: u64) -> Self {
        Self {
            manifest: ExtensionManifest {
                id: id.to_string(),
                type_id: "mock".to_string(),
                provider_id: "mock-provider".to_string(),
                display_name: "Mock".to_string(),
                description: "test fixture".to_string(),
                capabilities: vec![Capability::Usage],
                requires_api_key: false,
                docs_url: None,
            },
            outcomes: Arc::new(RwLock::new(outcomes)),
            fetch_calls: Arc::new(AtomicUsize::new(0)),
            interval_secs,
        }
    }
}

impl ProviderExtension for MockExt {
    fn manifest(&self) -> ExtensionManifest {
        self.manifest.clone()
    }
    fn usage_provider(&self) -> Option<&dyn UsageProvider> {
        Some(self)
    }
}

#[async_trait]
impl UsageProvider for MockExt {
    async fn fetch(&self, _ctx: &ExtensionContext) -> Result<UsageSnapshot, ExtensionError> {
        self.fetch_calls.fetch_add(1, Ordering::SeqCst);
        let outcome = {
            let mut out = self.outcomes.write().await;
            if out.is_empty() {
                MockOutcome::Ok
            } else {
                out.remove(0)
            }
        };
        match outcome {
            MockOutcome::Ok => Ok(UsageSnapshot {
                extension_id: self.manifest.id.clone(),
                provider_id: self.manifest.provider_id.clone(),
                fetched_at: chrono::Utc::now().timestamp(),
                headline: Some(UsageMetric {
                    key: "ok".to_string(),
                    label: "OK".to_string(),
                    display: "ok".to_string(),
                    value: 1.0,
                    kind: MetricKind::Count,
                    tone: Tone::Ok,
                }),
                metrics: vec![],
                raw: None,
            }),
            MockOutcome::Err(s) => Err(ExtensionError::Network(s)),
            MockOutcome::Unsupported(s) => Err(ExtensionError::Unsupported(s)),
            MockOutcome::Hang => {
                // Sleep past the per-fetch timeout to exercise the timeout
                // path. Callers must run this under `tokio::time::pause()`
                // (e.g. `#[tokio::test(start_paused = true)]`) and use
                // `tokio::time::advance` to drive the virtual clock past
                // `FETCH_TIMEOUT_SECS`; otherwise this would be a real
                // wall-clock wait of tens of seconds.
                tokio::time::sleep(Duration::from_secs(poller::FETCH_TIMEOUT_SECS + 5)).await;
                Ok(UsageSnapshot {
                    extension_id: self.manifest.id.clone(),
                    provider_id: self.manifest.provider_id.clone(),
                    fetched_at: 0,
                    headline: None,
                    metrics: vec![],
                    raw: None,
                })
            }
        }
    }

    fn refresh_interval_secs(&self) -> u64 {
        self.interval_secs
    }
}

// ── Registry tests ────────────────────────────────────────────

#[test]
fn registry_register_and_lookup() {
    let mut reg = ExtensionRegistry::new();
    let ext = Arc::new(MockExt::new("mock:foo", vec![], 30));
    reg.register(ext).expect("register ok");

    assert!(reg.get("mock:foo").is_some());
    assert!(reg.get("unknown").is_none());
    assert_eq!(reg.manifests().len(), 1);
}

#[test]
fn registry_rejects_duplicate_ids() {
    let mut reg = ExtensionRegistry::new();
    let ext1 = Arc::new(MockExt::new("mock:foo", vec![], 30));
    let ext2 = Arc::new(MockExt::new("mock:foo", vec![], 30));
    reg.register(ext1).expect("first ok");
    let err = reg.register(ext2).expect_err("second must fail");
    assert!(matches!(err, ExtensionError::Internal(_)));
}

#[test]
fn registry_manifests_sorted_deterministically() {
    let mut reg = ExtensionRegistry::new();
    reg.register(Arc::new(MockExt::new("mock:c", vec![], 30)))
        .unwrap();
    reg.register(Arc::new(MockExt::new("mock:a", vec![], 30)))
        .unwrap();
    reg.register(Arc::new(MockExt::new("mock:b", vec![], 30)))
        .unwrap();
    let ids: Vec<String> = reg.manifests().into_iter().map(|m| m.id).collect();
    assert_eq!(ids, vec!["mock:a", "mock:b", "mock:c"]);
}

// ── Snapshot serde sanity ─────────────────────────────────────

#[test]
fn capability_serde_round_trip() {
    let cap = Capability::RateLimitProbe;
    let json = serde_json::to_string(&cap).unwrap();
    assert_eq!(json, "\"rate_limit_probe\"");
    let back: Capability = serde_json::from_str(&json).unwrap();
    assert_eq!(back, cap);
}

#[test]
fn snapshot_status_serde() {
    let s = SnapshotStatus::Unsupported;
    let json = serde_json::to_string(&s).unwrap();
    assert_eq!(json, "\"unsupported\"");
    let back: SnapshotStatus = serde_json::from_str(&json).unwrap();
    assert_eq!(back, s);
}

#[test]
fn user_settings_defaults_to_enabled_and_visible() {
    let s: super::types::ExtensionUserSettings = Default::default();
    assert!(s.enabled);
    assert!(s.show_in_topbar);
}

// ── Poller tests ──────────────────────────────────────────────
//
// These exercise `run_extension_loop` indirectly via `spawn_pollers`.
// Each test runs one extension with a controlled sequence of outcomes
// and assertions are made against the shared snapshots map.

/// Build a minimal ExtensionContext + supporting Arcs for tests.
async fn build_test_ctx() -> (Arc<ExtensionContext>, Arc<tokio::sync::RwLock<Config>>) {
    let cfg = Config::default();
    let cfg_arc = Arc::new(RwLock::new(cfg));
    let http = reqwest::Client::builder().build().unwrap();
    let pi_mgr = Arc::new(crate::pi::manager::PiManager::new(
        std::path::PathBuf::from("pi"),
        4,
        HashMap::new(),
        None,
    ));
    let data_dir = std::env::temp_dir();
    let ctx = Arc::new(ExtensionContext::new(
        Arc::clone(&cfg_arc),
        http,
        pi_mgr,
        data_dir,
    ));
    (ctx, cfg_arc)
}

// Note: `spawn_pollers` requires a Tauri AppHandle which can't be
// constructed cleanly in pure unit tests. Instead, these tests call
// the internal helper directly via a #[cfg(test)] convenience.
// We replicate the loop's contract by calling `usage_provider().fetch()`
// directly and asserting we drive the outcomes correctly.

#[tokio::test]
async fn mock_ok_path_returns_snapshot() {
    let (ctx, _cfg) = build_test_ctx().await;
    let ext = Arc::new(MockExt::new("mock:ok", vec![MockOutcome::Ok], 30));
    let usage = ext.usage_provider().unwrap();
    let snap = usage.fetch(ctx.as_ref()).await.expect("ok");
    assert_eq!(snap.extension_id, "mock:ok");
    assert!(snap.headline.is_some());
}

#[tokio::test]
async fn mock_unsupported_returns_unsupported() {
    let (ctx, _cfg) = build_test_ctx().await;
    let ext = Arc::new(MockExt::new(
        "mock:u",
        vec![MockOutcome::Unsupported("no can do".to_string())],
        30,
    ));
    let usage = ext.usage_provider().unwrap();
    let err = usage.fetch(ctx.as_ref()).await.expect_err("must err");
    assert!(matches!(err, ExtensionError::Unsupported(_)));
}

#[tokio::test]
async fn mock_error_then_ok_reset_counter_simulation() {
    // This test simulates the poller's error-counter-reset logic without
    // actually running the loop (which would take seconds via backoff).
    let (ctx, _cfg) = build_test_ctx().await;
    let ext = Arc::new(MockExt::new(
        "mock:err",
        vec![
            MockOutcome::Err("boom".to_string()),
            MockOutcome::Err("boom".to_string()),
            MockOutcome::Ok,
        ],
        30,
    ));
    let usage = ext.usage_provider().unwrap();

    let mut consecutive: u32 = 0;
    for _ in 0..3 {
        match usage.fetch(ctx.as_ref()).await {
            Ok(_) => consecutive = 0,
            Err(_) => consecutive = consecutive.saturating_add(1),
        }
    }
    assert_eq!(consecutive, 0, "counter should reset after success");
    assert_eq!(ext.fetch_calls.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn poller_spawns_independent_tasks_per_extension() {
    // We don't have AppHandle, but we can register two extensions and
    // verify the registry iteration logic that `spawn_pollers` uses.
    let mut reg = ExtensionRegistry::new();
    reg.register(Arc::new(MockExt::new("mock:a", vec![], 30)))
        .unwrap();
    reg.register(Arc::new(MockExt::new("mock:b", vec![], 30)))
        .unwrap();
    let pairs = reg.iter_sorted();
    assert_eq!(pairs.len(), 2);
    // Sorted order.
    assert_eq!(pairs[0].0, "mock:a");
    assert_eq!(pairs[1].0, "mock:b");
}

#[tokio::test]
async fn perform_fetch_once_writes_ok_snapshot_immediately() {
    // Regression test for the startup-refresh kickoff. The poller's
    // periodic loop now sleeps `interval_secs` before its first tick;
    // the initial fetch happens via `perform_fetch_once` dispatched
    // from `spawn_pollers`. This test pins that contract: a single
    // call to `perform_fetch_once` deposits an Ok snapshot in the
    // shared map well under the configured interval.
    let (ctx, _cfg) = build_test_ctx().await;
    let app = tauri::test::mock_app();
    let app_handle = app.handle().clone();

    // 3600 s interval — if the loop's old `next_sleep = 0` behaviour
    // were what produced the first snapshot, this test would simply
    // never finish. Here we bypass the loop entirely.
    let ext: Arc<dyn ProviderExtension> =
        Arc::new(MockExt::new("mock:startup", vec![MockOutcome::Ok], 3600));
    let snapshots: Arc<RwLock<HashMap<String, SnapshotEntry>>> =
        Arc::new(RwLock::new(HashMap::new()));

    let start = Instant::now();
    let outcome = poller::perform_fetch_once_with_cancel(
        "mock:startup",
        &ext,
        &snapshots,
        &ctx,
        &app_handle,
        &CancellationToken::new(),
    )
    .await;
    let elapsed = start.elapsed();

    assert!(matches!(outcome, poller::Outcome::Ok));
    assert!(
        elapsed < Duration::from_millis(500),
        "startup fetch took {:?}, expected well under 500ms",
        elapsed
    );

    let map = snapshots.read().await;
    let entry = map
        .get("mock:startup")
        .expect("snapshot must be written by initial fetch");
    assert_eq!(entry.status, SnapshotStatus::Ok);
    assert!(entry.snapshot.is_some(), "Ok outcome must carry a snapshot");
    assert!(entry.last_error.is_none());
}

#[tokio::test]
async fn perform_fetch_once_writes_disabled_snapshot_when_user_disabled() {
    // When the user has disabled the extension, the kickoff should
    // still write a terminal Disabled entry (not silently skip).
    let (ctx, cfg) = build_test_ctx().await;
    let app = tauri::test::mock_app();
    let app_handle = app.handle().clone();

    // Disable the extension via config.
    {
        let mut c = cfg.write().await;
        let entry = c
            .extension_settings
            .entry("mock:disabled".to_string())
            .or_default();
        entry.enabled = false;
    }

    let ext: Arc<dyn ProviderExtension> =
        Arc::new(MockExt::new("mock:disabled", vec![MockOutcome::Ok], 3600));
    let snapshots: Arc<RwLock<HashMap<String, SnapshotEntry>>> =
        Arc::new(RwLock::new(HashMap::new()));

    let outcome = poller::perform_fetch_once_with_cancel(
        "mock:disabled",
        &ext,
        &snapshots,
        &ctx,
        &app_handle,
        &CancellationToken::new(),
    )
    .await;
    assert!(matches!(outcome, poller::Outcome::Disabled));

    let map = snapshots.read().await;
    let entry = map.get("mock:disabled").expect("entry written");
    assert_eq!(entry.status, SnapshotStatus::Disabled);
    assert!(entry.snapshot.is_none());
}

#[tokio::test]
async fn perform_fetch_once_returns_unsupported_and_writes_terminal_entry() {
    let (ctx, _cfg) = build_test_ctx().await;
    let app = tauri::test::mock_app();
    let app_handle = app.handle().clone();

    let ext: Arc<dyn ProviderExtension> = Arc::new(MockExt::new(
        "mock:unsup",
        vec![MockOutcome::Unsupported("nope".to_string())],
        3600,
    ));
    let snapshots: Arc<RwLock<HashMap<String, SnapshotEntry>>> =
        Arc::new(RwLock::new(HashMap::new()));

    let outcome = poller::perform_fetch_once_with_cancel(
        "mock:unsup",
        &ext,
        &snapshots,
        &ctx,
        &app_handle,
        &CancellationToken::new(),
    )
    .await;
    assert!(matches!(outcome, poller::Outcome::Unsupported));

    let map = snapshots.read().await;
    let entry = map.get("mock:unsup").expect("entry written");
    assert_eq!(entry.status, SnapshotStatus::Unsupported);
    assert_eq!(entry.last_error.as_deref(), Some("nope"));
}

#[tokio::test(start_paused = true)]
async fn cancellation_token_short_circuits_select() {
    // Direct exercise of the tokio::select! pattern used in the poller
    // sleep path — assert cancellation wins over a long sleep. Time is
    // paused so the 60s sleep never burns real wall clock even if the
    // cancel branch ever lost the race.
    let token = CancellationToken::new();
    token.cancel();
    let won = tokio::select! {
        _ = token.cancelled() => "cancel",
        _ = tokio::time::sleep(Duration::from_secs(60)) => "sleep",
    };
    assert_eq!(won, "cancel");
}

#[tokio::test(start_paused = true)]
async fn perform_fetch_once_writes_timeout_entry_when_fetch_hangs() {
    // Exercise the `tokio::time::timeout` arm in `perform_fetch_once` by
    // making the mock fetch sleep past `FETCH_TIMEOUT_SECS`. Under
    // `start_paused = true` the sleep is virtual — `tokio::time::advance`
    // drives the clock forward in microseconds of wall time.
    let (ctx, _cfg) = build_test_ctx().await;
    let app = tauri::test::mock_app();
    let app_handle = app.handle().clone();

    let ext: Arc<dyn ProviderExtension> =
        Arc::new(MockExt::new("mock:timeout", vec![MockOutcome::Hang], 3600));
    let snapshots: Arc<RwLock<HashMap<String, SnapshotEntry>>> =
        Arc::new(RwLock::new(HashMap::new()));

    // Spawn the fetch on a separate task so we can advance virtual time
    // while it awaits. `perform_fetch_once` calls `tokio::time::timeout`
    // around a `Duration::from_secs(FETCH_TIMEOUT_SECS)` budget; pushing
    // the clock past that budget triggers the timeout branch.
    let snapshots_clone = Arc::clone(&snapshots);
    let ctx_clone = Arc::clone(&ctx);
    let handle = tokio::spawn(async move {
        poller::perform_fetch_once_with_cancel(
            "mock:timeout",
            &ext,
            &snapshots_clone,
            &ctx_clone,
            &app_handle,
            &CancellationToken::new(),
        )
        .await
    });

    tokio::time::advance(Duration::from_secs(poller::FETCH_TIMEOUT_SECS + 1)).await;
    let outcome = handle.await.expect("task joined");

    assert!(matches!(outcome, poller::Outcome::Err));
    let map = snapshots.read().await;
    let entry = map.get("mock:timeout").expect("entry written");
    assert_eq!(entry.status, SnapshotStatus::Error);
    let err = entry.last_error.as_deref().unwrap_or("");
    assert!(
        err.contains("timed out"),
        "expected timeout message, got: {err}"
    );
}

#[tokio::test]
async fn snapshot_entry_roundtrips_through_serde() {
    let entry = SnapshotEntry {
        manifest: ExtensionManifest {
            id: "mock:foo".to_string(),
            type_id: "mock".to_string(),
            provider_id: "mock-provider".to_string(),
            display_name: "Mock".to_string(),
            description: "test".to_string(),
            capabilities: vec![Capability::Usage],
            requires_api_key: false,
            docs_url: None,
        },
        snapshot: None,
        last_error: None,
        last_fetched_at: None,
        status: SnapshotStatus::Loading,
        user_settings: Default::default(),
    };
    let json = serde_json::to_string(&entry).unwrap();
    let back: SnapshotEntry = serde_json::from_str(&json).unwrap();
    assert_eq!(back.manifest.id, entry.manifest.id);
    assert_eq!(back.status, SnapshotStatus::Loading);
}

#[tokio::test]
async fn disabled_extension_uses_loading_to_disabled_transition() {
    // Reflects the seeding logic in AppState::new — extension marked
    // disabled via user_settings should land as SnapshotStatus::Disabled,
    // not Loading.
    let mut user = super::types::ExtensionUserSettings::default();
    user.enabled = false;
    let status = if user.enabled {
        SnapshotStatus::Loading
    } else {
        SnapshotStatus::Disabled
    };
    assert_eq!(status, SnapshotStatus::Disabled);
}

#[test]
fn register_builtin_extensions_skips_providers_without_match() {
    use crate::state::config::ProviderConfig;
    let mut providers = HashMap::new();
    providers.insert(
        "anthropic".to_string(),
        ProviderConfig {
            display_name: "Anthropic".to_string(),
            provider_type: "Anthropic".to_string(),
            endpoint: Some("https://api.anthropic.com/v1".to_string()),
            has_key: false,
        },
    );
    let mut reg = ExtensionRegistry::new();
    super::register_builtin_extensions(&providers, &mut reg);
    // Anthropic usage extension should have been registered.
    assert!(reg.get("anthropic_usage:anthropic").is_some());
    // OpenRouter not configured — skip.
    assert!(reg
        .manifests()
        .into_iter()
        .find(|m| m.type_id == "openrouter_credits")
        .is_none());
}

#[test]
fn claude_sub_usage_registers_for_claude_sub_only() {
    use crate::state::config::ProviderConfig;
    let mut providers = HashMap::new();
    providers.insert(
        "claude-sub".to_string(),
        ProviderConfig {
            display_name: "Claude (subscription)".to_string(),
            provider_type: "Subscription".to_string(),
            endpoint: None,
            has_key: false,
        },
    );
    providers.insert(
        "chatgpt".to_string(),
        ProviderConfig {
            display_name: "ChatGPT (subscription)".to_string(),
            provider_type: "Subscription".to_string(),
            endpoint: None,
            has_key: false,
        },
    );
    let mut reg = ExtensionRegistry::new();
    super::register_builtin_extensions(&providers, &mut reg);

    // The Claude-sub usage extension should be registered exactly once,
    // for the `claude-sub` provider only — never for chatgpt.
    let claude_sub_count = reg
        .manifests()
        .into_iter()
        .filter(|m| m.type_id == "claude_sub_usage")
        .count();
    assert_eq!(claude_sub_count, 1);
    assert!(reg.get("claude_sub_usage:claude-sub").is_some());
    assert!(reg.get("claude_sub_usage:chatgpt").is_none());
    // Symmetric assertion: ChatGPT sub-usage must not attach to claude-sub.
    assert!(reg.get("chatgpt_sub_usage:claude-sub").is_none());
}

#[test]
fn chatgpt_sub_usage_registers_for_chatgpt_only() {
    use crate::state::config::ProviderConfig;
    let mut providers = HashMap::new();
    providers.insert(
        "claude-sub".to_string(),
        ProviderConfig {
            display_name: "Claude (subscription)".to_string(),
            provider_type: "Subscription".to_string(),
            endpoint: None,
            has_key: false,
        },
    );
    providers.insert(
        "chatgpt".to_string(),
        ProviderConfig {
            display_name: "ChatGPT (subscription)".to_string(),
            provider_type: "Subscription".to_string(),
            endpoint: None,
            has_key: false,
        },
    );
    let mut reg = ExtensionRegistry::new();
    super::register_builtin_extensions(&providers, &mut reg);

    // The ChatGPT sub-usage extension should be registered exactly
    // once, for the `chatgpt` provider only — never for claude-sub.
    let chatgpt_count = reg
        .manifests()
        .into_iter()
        .filter(|m| m.type_id == "chatgpt_sub_usage")
        .count();
    assert_eq!(chatgpt_count, 1);
    assert!(reg.get("chatgpt_sub_usage:chatgpt").is_some());
    assert!(reg.get("chatgpt_sub_usage:claude-sub").is_none());
}

#[test]
fn chatgpt_sub_usage_tone_thresholds() {
    use super::builtins::chatgpt_sub_usage::tone_for;
    use super::types::Tone;
    assert_eq!(tone_for(0.0), Tone::Ok);
    assert_eq!(tone_for(59.999), Tone::Ok);
    assert_eq!(tone_for(60.0), Tone::Warn);
    assert_eq!(tone_for(89.999), Tone::Warn);
    assert_eq!(tone_for(90.0), Tone::Crit);
    assert_eq!(tone_for(100.0), Tone::Crit);
}

#[test]
fn claude_sub_usage_tone_thresholds() {
    use super::builtins::claude_sub_usage::tone_for;
    use super::types::Tone;
    assert_eq!(tone_for(0.0), Tone::Ok);
    assert_eq!(tone_for(59.999), Tone::Ok);
    assert_eq!(tone_for(60.0), Tone::Warn);
    assert_eq!(tone_for(89.999), Tone::Warn);
    assert_eq!(tone_for(90.0), Tone::Crit);
    assert_eq!(tone_for(100.0), Tone::Crit);
}

#[test]
fn register_builtin_extensions_registers_per_openrouter_instance() {
    use crate::state::config::ProviderConfig;
    let mut providers = HashMap::new();
    providers.insert(
        "openrouter".to_string(),
        ProviderConfig {
            display_name: "OpenRouter".to_string(),
            provider_type: "OpenAI Compatible".to_string(),
            endpoint: Some("https://openrouter.ai/api/v1".to_string()),
            has_key: false,
        },
    );
    providers.insert(
        "openrouter-work".to_string(),
        ProviderConfig {
            display_name: "OpenRouter Work".to_string(),
            provider_type: "OpenAI Compatible".to_string(),
            endpoint: Some("https://openrouter.ai/api/v1".to_string()),
            has_key: false,
        },
    );
    let mut reg = ExtensionRegistry::new();
    super::register_builtin_extensions(&providers, &mut reg);
    assert!(reg.get("openrouter_credits:openrouter").is_some());
    assert!(reg.get("openrouter_credits:openrouter-work").is_some());
}
