mod commands;
mod core;
mod domain;
mod extensions;
mod hivemind;
mod nurse;
mod pi;
mod providers;
mod sentry_setup;
mod state;
mod tunables;
mod util;

use std::sync::Arc;

use tauri::image::Image;
use tauri::Emitter;
#[cfg(target_os = "macos")]
use tauri::LogicalPosition;
use tauri::Manager;
#[cfg(target_os = "macos")]
use tauri::TitleBarStyle;
use tauri::WebviewUrl;
use tauri::WebviewWindowBuilder;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::Layer;

use state::app_state::AppState;

/// `scheme://host[:port]` — used to lock the main webview to its own origin.
fn origin_of(url: &tauri::Url) -> String {
    let scheme = url.scheme();
    let host = url.host_str().unwrap_or("");
    match url.port() {
        Some(port) => format!("{scheme}://{host}:{port}"),
        None => format!("{scheme}://{host}"),
    }
}

/// Best-effort prune of debug log files older than
/// [`tunables::debug_log_retention_days`] (default 7).
///
/// Covers three shapes of file under `~/.hyvemind/debug/`:
/// 1. Legacy / general daily files: `hyvemind-debug.jsonl.YYYY-MM-DD` and
///    `general.jsonl.YYYY-MM-DD` — parsed by date suffix.
/// 2. Per-ID files under `sessions/` and `reviews/` — pruned by mtime.
/// 3. Per-swarm dirs under `swarms/{id}/*.jsonl` — pruned by mtime; empty
///    `{id}/` directories are removed after their files go.
fn prune_old_debug_logs(debug_dir: &std::path::Path) {
    let retention_days = tunables::debug_log_retention_days();
    let cutoff_date = match chrono::Utc::now()
        .date_naive()
        .checked_sub_days(chrono::Days::new(retention_days))
    {
        Some(d) => d,
        None => return,
    };
    let cutoff_mtime = std::time::SystemTime::now().checked_sub(std::time::Duration::from_secs(
        retention_days * 24 * 60 * 60,
    ));

    if let Ok(entries) = std::fs::read_dir(debug_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            let name = match path.file_name().and_then(|n| n.to_str()) {
                Some(n) => n,
                None => continue,
            };
            let suffix = name
                .strip_prefix("hyvemind-debug.jsonl.")
                .or_else(|| name.strip_prefix("general.jsonl."));
            if let Some(suffix) = suffix {
                let date_str = suffix.get(..10).unwrap_or(suffix);
                if let Ok(d) = chrono::NaiveDate::parse_from_str(date_str, "%Y-%m-%d") {
                    if d < cutoff_date {
                        let _ = std::fs::remove_file(&path);
                    }
                }
            }
        }
    }

    let Some(cutoff_mtime) = cutoff_mtime else {
        return;
    };

    prune_jsonl_by_mtime(&debug_dir.join("sessions"), cutoff_mtime);
    prune_jsonl_by_mtime(&debug_dir.join("reviews"), cutoff_mtime);

    if let Ok(entries) = std::fs::read_dir(debug_dir.join("swarms")) {
        for entry in entries.flatten() {
            let swarm_dir = entry.path();
            if !swarm_dir.is_dir() {
                continue;
            }
            prune_jsonl_by_mtime(&swarm_dir, cutoff_mtime);
            // Try to remove the swarm dir if it is now empty.
            let _ = std::fs::remove_dir(&swarm_dir);
        }
    }
}

fn prune_jsonl_by_mtime(dir: &std::path::Path, cutoff: std::time::SystemTime) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
            continue;
        }
        let mtime = std::fs::metadata(&path).and_then(|m| m.modified());
        if let Ok(mtime) = mtime {
            if mtime < cutoff {
                let _ = std::fs::remove_file(&path);
            }
        }
    }
}

/// Initialise the tracing subscriber.
///
/// - **Always**: stderr fmt layer at INFO (or `RUST_LOG` if set).
/// - **When `HYVEMIND_DEBUG=1`**: adds a per-ID routing layer at TRACE level
///   writing to `~/.hyvemind/debug/` — one file per session / review / swarm
///   agent, plus a daily `general.jsonl.YYYY-MM-DD` fallback.
fn init_tracing() {
    let stderr_filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

    // Apply filter per-layer (not globally) so the JSON file layer
    // can use TRACE independently of the stderr INFO filter.
    //
    // `RedactingStderr` wraps stderr with the same secret scrubber the
    // per-ID file layer uses, so anything logged at INFO/WARN/ERROR is
    // redacted before reaching the dev terminal, systemd journal, or
    // macOS Console.app. See `state/log_redact.rs`.
    let stderr_layer = tracing_subscriber::fmt::layer()
        .with_writer(state::log_redact::RedactingStderr)
        .with_filter(stderr_filter);

    // Capture only ERROR-level tracing events as Sentry events; everything
    // else is ignored so the Pi/Hivemind firehose doesn't flood breadcrumbs.
    // No-op when Sentry is disabled.
    let sentry_layer = sentry_tracing::layer().event_filter(|md| {
        if *md.level() == tracing::Level::ERROR {
            sentry_tracing::EventFilter::Event
        } else {
            sentry_tracing::EventFilter::Ignore
        }
    });

    let registry = tracing_subscriber::registry()
        .with(stderr_layer)
        .with(sentry_layer);

    if std::env::var("HYVEMIND_DEBUG").as_deref() == Ok("1") {
        let debug_dir = dirs::home_dir()
            .unwrap_or_else(|| std::path::PathBuf::from("."))
            .join(".hyvemind")
            .join("debug");

        if let Err(e) = std::fs::create_dir_all(&debug_dir) {
            eprintln!(
                "failed to create debug directory {}: {}",
                debug_dir.display(),
                e
            );
            registry.init();
            return;
        }

        // Best-effort retention sweep before opening today's appender.
        prune_old_debug_logs(&debug_dir);

        let layer = state::log_routing::PerIdRoutingLayer::new(debug_dir.clone())
            .with_filter(tracing_subscriber::filter::LevelFilter::TRACE);
        registry.with(layer).init();

        tracing::warn!(
            debug_dir = %debug_dir.display(),
            "HYVEMIND_DEBUG=1 enabled \u{2014} TRACE-level logs writing to {}; pruning files older than {} days",
            debug_dir.display(),
            tunables::debug_log_retention_days(),
        );
    } else {
        registry.init();
    }
}

pub fn run() {
    // Sentry must be initialised before the tracing subscriber so the
    // sentry-tracing layer has a live client to dispatch to. Held for the
    // process lifetime; dropping flushes pending events.
    let _sentry_guard = sentry_setup::init(state::config::Config::peek_crash_reporting());

    init_tracing();

    // Panic capture is provided by sentry's default `PanicIntegration` (the
    // `panic` feature is enabled in Cargo.toml), so no explicit hook setup
    // is needed here.

    let mut context = tauri::generate_context!();

    // Embed app icon at compile time so it shows in dev mode.
    if let Ok(icon) = Image::from_bytes(include_bytes!("../icons/icon.png")) {
        context.set_default_window_icon(Some(icon));
    }

    let mut builder = tauri::Builder::default();
    // Always register the Sentry plugin so the frontend IPC transport has a
    // handler. When Sentry is disabled, a no-op client silently drops any
    // envelopes from the renderer, preventing "plugin sentry not found" errors.
    let sentry_client = sentry::Hub::current().client().unwrap_or_else(|| {
        std::sync::Arc::new(sentry::Client::from(sentry::ClientOptions {
            dsn: None,
            ..Default::default()
        }))
    });
    builder = builder.plugin(tauri_plugin_sentry::init_with_no_injection(&sentry_client));
    builder
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_dialog::init())
        .setup(move |app| {
            // Build the main window programmatically so we can attach an
            // on_navigation guard. Origin is locked to the configured devUrl
            // in dev (devUrl in tauri.conf.json) and to Tauri's custom scheme
            // in production. Any other URL (e.g. a worker's dev server that
            // grabbed the port, or a clicked external link) is blocked here.
            // `devUrl` is always populated from tauri.conf.json, so we
            // can't use its presence to detect dev mode — gate on
            // debug_assertions, which is set in `cargo tauri dev` and
            // cleared in `cargo tauri build`.
            let allowed_origins: Vec<String> = if cfg!(debug_assertions) {
                match app.config().build.dev_url.as_ref() {
                    Some(url) => vec![origin_of(url)],
                    None => vec![],
                }
            } else {
                vec![
                    // macOS / iOS use the custom tauri:// scheme.
                    "tauri://localhost".to_string(),
                    // Linux / Windows use http://tauri.localhost.
                    "http://tauri.localhost".to_string(),
                ]
            };

            let builder =
                WebviewWindowBuilder::new(app, "main", WebviewUrl::App("index.html".into()))
                    .title("Hyvemind")
                    .inner_size(1440.0, 819.0)
                    .min_inner_size(1280.0, 720.0) // 160px / 99px headroom under the 1440x819 default; raise default if min grows
                    .resizable(true);
            // macOS-only: overlay the title bar so the webview can paint behind
            // the traffic-light buttons and the brand can sit flush with the top
            // edge. These builder methods don't exist on the Linux/Windows
            // WebviewWindowBuilder, so we gate them behind cfg.
            #[cfg(target_os = "macos")]
            let builder = builder
                .title_bar_style(TitleBarStyle::Overlay)
                .hidden_title(true)
                .traffic_light_position(LogicalPosition::new(14.0, 25.0));
            builder
                .on_navigation(move |url| {
                    let origin = origin_of(url);
                    let allowed = allowed_origins.iter().any(|o| o == &origin);
                    if !allowed {
                        tracing::warn!(
                            target: "security",
                            url = %url,
                            "blocked webview navigation"
                        );
                    }
                    allowed
                })
                .build()?;

            let app_state = tauri::async_runtime::block_on(AppState::new(app.handle()))?;

            // Build the Nurse bus and attach it to PiManager BEFORE any
            // Pi session is spawned. The bus is the push-mode feed every
            // `touch_activity`, `set_owner`, spawn, and kill goes through;
            // the NurseEngine subscribes once and runs detectors per
            // session.
            let nurse_bus = Arc::new(crate::nurse::bus::NurseBus::new());
            app_state.pi_manager.set_nurse_bus(Arc::clone(&nurse_bus));
            // Drain the list of merge runs the startup sweep flagged as
            // `interrupted`. We capture them BEFORE moving `app_state`
            // into Tauri's managed state so the data isn't trapped behind
            // a `tauri::State` borrow.
            let interrupted = app_state.take_pending_interrupted_emits();
            // Audit 2.2: capture the list of swarms the startup
            // progress_log replay sweep reconciled (each one carries the
            // ids of features marked failed-by-interruption). Emit after
            // `app.manage(state)` so the frontend's `swarm_reconciled`
            // listener can render a "Resume" badge per swarm.
            let swarm_reconciled = app_state.take_pending_swarm_reconciled_emits();

            // Seed sensible defaults for bundled Pi extensions. Specifically:
            // disable the `pi-web-access` curator workflow so `web_search`
            // tool calls don't hang waiting on a browser-popup approval that
            // headless RPC sessions can't respond to. Idempotent and
            // non-destructive — see `pi::defaults` for the policy.
            pi::defaults::ensure_web_search_workflow_default();

            // Reconcile the in-memory graveyard from any session transcripts
            // left on disk by a prior process. After a crash the manager has
            // no knowledge of the N in-flight Pi sessions that were live at
            // the time, so the first follow-up `send_message` would silently
            // start a fresh session and orphan the prior transcript. Seeding
            // the graveyard from `~/.hyvemind/chat-sessions/*.jsonl` lets the
            // next spawn pick the existing file up with `--continue
            // --session <path>`. Best-effort; failures are logged and the
            // app continues.
            {
                let hyvemind_home = dirs::home_dir()
                    .unwrap_or_else(|| std::path::PathBuf::from("."))
                    .join(".hyvemind");
                let pi_manager = Arc::clone(&app_state.pi_manager);
                let inserted = tauri::async_runtime::block_on(async move {
                    pi_manager.reconcile_graveyard_from_disk(&hyvemind_home).await
                });
                if inserted > 0 {
                    tracing::info!(
                        inserted,
                        "graveyard reconciliation: recovered orphan Pi session transcripts from disk"
                    );
                }
            }

            app.manage(app_state);

            // Construct the v2 push-mode NurseEngine and wire its three-tier
            // dispatcher live. Construction ordering matters:
            //   1. NurseEngine::new (observability writers spawn here)
            //   2. attach_app_handle (lets the engine emit nurse-event + lookup AppState)
            //   3. construct LlmClassifier (Arc<AsyncRwLock<ProviderRegistry>> from AppState)
            //   4. construct DefaultApplier (stateless)
            //   5. construct Dispatcher with Arc::downgrade(&engine) — Weak prevents
            //      a reference cycle between engine and dispatcher
            //   6. attach_dispatcher
            //   7. engine.start() — refuses with Err if any OnceCell still empty
            //
            // `start()` returning Err here is fatal-logged; without the
            // engine running, Nurse interventions stop firing entirely.
            {
                let state: tauri::State<'_, AppState> = app.state();
                let pi_manager = Arc::clone(&state.pi_manager);
                let provider_registry = Arc::clone(&state.provider_registry);
                let mut nurse_config = crate::nurse::config::NurseConfig::default();
                // Overlay user-saved fields from `config.json` so engine ticks read
                // the persisted values rather than only the hardcoded defaults.
                // Without this, `nurse_model` / `nurse_provider` / `nurse_enabled`
                // set via Settings or `set_nurse_config` are dropped on every
                // restart and Tier 3 silently skips with `classifier_skipped_no_model`.
                let (
                    user_profiles,
                    persisted_enabled,
                    persisted_model,
                    persisted_provider,
                    persisted_batch_interval,
                    persisted_swarms_only,
                ) = tauri::async_runtime::block_on(async {
                    let cfg = state.config.read().await;
                    (
                        cfg.nurse_profiles.clone(),
                        cfg.nurse_enabled,
                        cfg.nurse_model.clone(),
                        cfg.nurse_provider.clone(),
                        cfg.nurse_batch_interval_secs,
                        cfg.nurse_swarms_only,
                    )
                });
                nurse_config.enabled = persisted_enabled;
                nurse_config.nurse_model = persisted_model;
                nurse_config.nurse_provider = persisted_provider;
                nurse_config.nurse_batch_interval_secs = persisted_batch_interval;
                nurse_config.swarms_only = persisted_swarms_only;
                for (profile, cfg) in user_profiles {
                    nurse_config.profiles.insert(profile, cfg);
                }
                let nurse_bus_for_engine = Arc::clone(&nurse_bus);
                let app_handle_for_engine = app.handle().clone();
                // `NurseEngine::new` and `engine.start()` both call
                // `tokio::spawn` internally (observability writers + run loop),
                // so wrap the whole construct-attach-start sequence in
                // `block_on` to enter the Tauri async runtime.
                let engine_result = tauri::async_runtime::block_on(async move {
                    let engine = crate::nurse::engine::NurseEngine::new(
                        nurse_bus_for_engine,
                        Arc::clone(&pi_manager),
                        nurse_config,
                    )?;
                    let engine = Arc::new(engine);
                    engine.attach_app_handle(app_handle_for_engine);

                    // Construct classifier + applier + dispatcher and attach
                    // BEFORE start(). The dispatcher holds a Weak<NurseEngine>
                    // to avoid an Arc cycle (engine.dispatcher → Dispatcher
                    // → Weak<NurseEngine>).
                    // Blanket impl is `impl ClassifierBackend for Arc<LlmClassifier>`,
                    // so build the inner Arc first then upcast to the trait object.
                    let provider_registry_for_batch = Arc::clone(&provider_registry);
                    // Share the engine's `llm_calls_total` counter so Tier 3
                    // single-session classifier calls land in the same tally
                    // as batched-reviewer calls (the topbar shows the sum).
                    let llm_counter = Arc::clone(&engine.health.llm_calls_total);
                    let llm = Arc::new(
                        crate::nurse::classifier::LlmClassifier::new(provider_registry)
                            .with_call_counter(llm_counter),
                    );
                    let classifier: Arc<dyn crate::nurse::dispatcher::ClassifierBackend> =
                        Arc::new(llm);
                    let applier: Arc<dyn crate::nurse::dispatcher::ActionApplier> =
                        crate::nurse::intervention::DefaultApplier::new_arc();
                    let pi_killer: Arc<dyn crate::nurse::intervention::SessionKiller> =
                        pi_manager.clone();
                    let dispatcher = Arc::new(crate::nurse::dispatcher::Dispatcher::new(
                        Arc::downgrade(&engine),
                        classifier,
                        applier,
                        pi_killer,
                    ));
                    engine.attach_dispatcher(Arc::clone(&dispatcher));

                    // Batched LLM reviewer — periodic ticker enabled by
                    // default. Holds Weak refs to engine + dispatcher so
                    // the engine remains the sole strong-ref owner of
                    // both. Skipped silently when `nurse_batch_enabled`
                    // is false in config (checked per-tick).
                    let batch_reviewer = Arc::new(crate::nurse::batch_review::BatchReviewer::new(
                        Arc::downgrade(&engine),
                        provider_registry_for_batch,
                        Arc::downgrade(&dispatcher),
                    ));
                    engine.attach_batch_reviewer(batch_reviewer);

                    let _run_handle = Arc::clone(&engine)
                        .start()
                        .map_err(|e| anyhow::anyhow!("nurse engine start: {}", e))?;
                    Ok::<_, anyhow::Error>(engine)
                });
                match engine_result {
                    Ok(engine) => {
                        state.attach_nurse_engine(engine);
                        tracing::info!("nurse engine v2 started with live dispatcher");
                    }
                    Err(e) => {
                        tracing::error!(
                            error = %e,
                            "nurse engine v2 failed to start; Nurse interventions will not fire until the next restart"
                        );
                    }
                }
            }

            // Spawn the unified Pi maintenance loop (idle eviction,
            // context-bloat respawn, auto_commit_locks sweep,
            // pi_pool_stats observability log). Lives for the process
            // lifetime; aborted when the runtime shuts down.
            //
            // Audit 6.2: the merge-capture stale-entry sweep that used to
            // run inside this loop now lives in
            // `crate::hivemind::merge_capture::sweep_idle_captures`. It is
            // wired in the block immediately below so the `pi/` subtree
            // no longer needs to import anything from `hivemind/`.
            {
                let state: tauri::State<'_, AppState> = app.state();
                let maintenance_state = crate::pi::eviction::MaintenanceState {
                    pi_manager: Arc::clone(&state.pi_manager),
                    auto_commit_locks: Arc::clone(&state.auto_commit_locks),
                };
                let app_handle = app.handle().clone();
                // Same reason as the nurse spawn above — `spawn_maintenance_loop`
                // calls `tokio::spawn` internally and needs an active runtime.
                let _maintenance_handle = tauri::async_runtime::block_on(async move {
                    crate::pi::eviction::spawn_maintenance_loop(maintenance_state, app_handle)
                });
            }

            // Spawn the hivemind merge-capture sweep. Runs on the same
            // 30s cadence as the Pi maintenance loop but lives here in the
            // wiring layer so the `pi/` subtree can stay decoupled from
            // `hivemind/` (audit 6.2). Each tick snapshots the live Pi
            // session IDs from the manager and forwards them to
            // `sweep_idle_captures` along with the default TTL.
            {
                let state: tauri::State<'_, AppState> = app.state();
                let pi_manager = Arc::clone(&state.pi_manager);
                let merge_capture = Arc::clone(&state.merge_capture);
                let _sweep_handle = tauri::async_runtime::block_on(async move {
                    tokio::spawn(async move {
                        let mut ticker = tokio::time::interval(
                            crate::pi::eviction::MAINTENANCE_INTERVAL,
                        );
                        ticker.tick().await;
                        loop {
                            ticker.tick().await;
                            let live: std::collections::HashSet<String> = pi_manager
                                .list_sessions()
                                .await
                                .into_iter()
                                .map(|(id, _)| id)
                                .collect();
                            let removed =
                                crate::hivemind::merge_capture::sweep_idle_captures(
                                    &merge_capture,
                                    &live,
                                    crate::hivemind::merge_capture::DEFAULT_MERGE_CAPTURE_TTL,
                                );
                            if removed > 0 {
                                tracing::info!(
                                    removed,
                                    "merge_capture sweep removed stale entries"
                                );
                            }
                        }
                    })
                });
            }

            // Spawn provider-extension pollers (one task per
            // UsageProvider-capable extension). Lives until the
            // CancellationToken is cancelled in RunEvent::Exit or by
            // refresh_extension_registry.
            {
                let state: tauri::State<'_, AppState> = app.state();
                let registry = Arc::clone(&state.extension_registry);
                let snapshots = Arc::clone(&state.usage_snapshots);
                let context = Arc::clone(&state.extension_context);
                let refresh_locks = Arc::clone(&state.extension_refresh_locks);
                let app_handle = app.handle().clone();
                tauri::async_runtime::block_on(async {
                    let token = {
                        let guard = state.extension_poller_cancel.lock().await;
                        guard.clone()
                    };
                    crate::extensions::poller::spawn_pollers(
                        registry,
                        snapshots,
                        context,
                        refresh_locks,
                        token,
                        app_handle,
                    )
                    .await;
                });
            }

            if !interrupted.is_empty() {
                let handle = app.handle().clone();
                let state: tauri::State<'_, AppState> = app.state();
                let store = Arc::clone(&state.hivemind_store);
                tauri::async_runtime::spawn(async move {
                    for entry in interrupted {
                        match entry {
                            state::app_state::PendingInterruptedEmit::Merge(info) => {
                                let _ = handle.emit(
                                    "hivemind-progress",
                                    serde_json::json!({
                                        "job_id": info.job_id,
                                        "review_id": info.review_id,
                                        "event_type": "merge_interrupted",
                                        "round": info.round_number,
                                        "model_id": info.model_id,
                                        "message": "Merge interrupted by host restart",
                                        "output_len": info.output_len,
                                    }),
                                );
                            }
                            state::app_state::PendingInterruptedEmit::Job(job) => {
                                let (phase, round, total_rounds, message) =
                                    crate::hivemind::phase::derive_phase_for_emit(
                                        &store,
                                        &job.job_id,
                                    )
                                    .await;
                                let _ = handle.emit(
                                    "hivemind-progress",
                                    serde_json::json!({
                                        "job_id": job.job_id,
                                        "review_id": job.review_id,
                                        "task_id": job.task_id,
                                        "event_type": "review_interrupted",
                                        "phase": phase,
                                        "round": round,
                                        "total_rounds": total_rounds,
                                        "output_len": 0,
                                        "message": message,
                                    }),
                                );
                            }
                        }
                    }
                });
            }

            // Audit 2.2: fan out `swarm_reconciled` events for any swarms
            // the progress_log replay reconciler marked Interrupted with
            // failed-by-interruption features. The frontend's
            // `swarm_reconciled` listener uses this to surface a Resume
            // affordance on the Swarms list immediately, without waiting
            // for the next 5s poll. The emit runs on a detached task so
            // the setup hook returns promptly; the frontend's listener
            // attaches synchronously during App mount so a small async
            // delay is acceptable.
            if !swarm_reconciled.is_empty() {
                let handle = app.handle().clone();
                tauri::async_runtime::spawn(async move {
                    for entry in swarm_reconciled {
                        let _ = handle.emit(
                            "swarm_reconciled",
                            serde_json::json!({
                                "swarm_id": entry.swarm_id,
                                "interrupted_features": entry.interrupted_features,
                            }),
                        );
                    }
                });
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::chat::send_message,
            commands::chat::stop_chat,
            commands::chat::get_chat_history,
            commands::chat::list_chat_sessions,
            commands::chat::delete_chat_session,
            commands::chat::is_session_busy,
            commands::chat::get_session_last_assistant_text,
            commands::hivemind::start_review,
            commands::hivemind::get_review_status,
            commands::hivemind::list_reviews,
            commands::hivemind::create_hivemind,
            commands::hivemind::list_hiveminds,
            commands::hivemind::update_hivemind,
            commands::hivemind::delete_hivemind,
            commands::hivemind::get_review_step_outputs,
            commands::hivemind::get_review_state,
            commands::hivemind::log_review_event,
            commands::hivemind::get_review_plan,
            commands::hivemind::cancel_review,
            commands::hivemind::delete_review,
            commands::hivemind::save_round_verdicts,
            commands::hivemind::list_round_verdicts,
            commands::hivemind::read_merge_output,
            commands::hivemind::register_context_session,
            commands::hivemind::get_orchestrator_usage,
            commands::hivemind::get_resumable_review_for_task,
            #[cfg(debug_assertions)]
            commands::hivemind::clear_response_cache,
            commands::swarms::create_swarm,
            commands::swarms::update_swarm,
            commands::swarms::start_swarm,
            commands::swarms::pause_swarm,
            commands::swarms::resume_swarm,
            commands::swarms::stop_swarm,
            commands::swarms::get_swarm,
            commands::swarms::list_swarms,
            commands::swarms::delete_swarm,
            commands::swarms::get_swarm_progress,
            commands::swarms::get_swarm_activity_log,
            commands::swarms::get_swarm_features,
            commands::swarms::get_swarm_milestones,
            commands::swarms::get_swarm_usage,
            commands::swarms::check_swarm_readiness,
            commands::settings::get_settings,
            commands::settings::get_system_prompts,
            commands::settings::list_custom_prompts,
            commands::settings::save_custom_prompt,
            commands::settings::delete_custom_prompt,
            commands::settings::set_runtime_settings,
            commands::settings::set_default_model,
            commands::settings::set_default_hivemind,
            commands::settings::set_default_project_path,
            commands::settings::request_working_dir_approval,
            commands::settings::save_api_key,
            commands::settings::delete_api_key,
            commands::settings::get_providers,
            commands::settings::add_provider,
            commands::settings::test_provider_models,
            commands::settings::test_provider_chat,
            commands::settings::test_provider_pi,
            commands::settings::refresh_models,
            commands::settings::get_pi_status,
            commands::settings::update_pi,
            commands::settings::install_pi,
            commands::settings::open_pi_terminal,
            commands::settings::check_subscription_auth,
            commands::dashboard::get_dashboard_stats,
            commands::dashboard::get_model_usage,
            commands::dashboard::get_provider_usage,
            commands::dashboard::get_cost_summary,
            commands::dashboard::get_recent_activity,
            commands::settings::set_auto_commit_tasks,
            commands::settings::set_auto_commit_conventional,
            commands::settings::set_task_completion_sound,
            commands::settings::set_crash_reporting,
            commands::settings::set_chat_check_in_secs,
            commands::settings::set_extension_poll_interval_secs,
            commands::settings::set_daily_budget,
            commands::extensions::list_extensions,
            commands::extensions::get_usage_snapshots,
            commands::extensions::refresh_usage_snapshot,
            commands::extensions::update_extension_settings,
            commands::tasks::save_task_messages,
            commands::tasks::load_task_messages,
            commands::tasks::delete_task_messages,
            commands::tasks::get_task_state,
            commands::tasks::auto_commit_task,
            commands::tasks::list_project_files,
            commands::nurse::get_nurse_status,
            commands::nurse::set_nurse_config,
            commands::nurse::check_chat_session,
            commands::nurse::get_nurse_engine_status,
            commands::nurse::clear_nurse_intervention_log,
            commands::nurse::get_nurse_intervention_log,
            commands::nurse::get_nurse_detector_stats,
            commands::nurse::get_nurse_session_detail,
            commands::nurse::record_nurse_intervention_feedback,
            commands::nurse::nurse_manual_action,
            commands::nurse::get_nurse_detector_schemas,
            commands::nurse::get_nurse_decision_chain,
            commands::nurse::get_nurse_decisions_for_session,
            commands::nurse::get_nurse_signal_stream,
            commands::nurse::get_nurse_capture,
            commands::nurse::export_nurse_diagnostic_bundle,
            commands::nurse::get_nurse_profile,
            commands::nurse::set_nurse_profile,
            commands::sessions::list_active_pi_sessions,
            commands::sessions::kill_pi_session,
            commands::sessions::sigkill_pi_session,
            commands::sessions::reconcile_active_sessions,
            commands::sessions::pi_pool_stats,
            commands::sessions::get_pi_session_stats,
            commands::tests::run_stability_test,
            commands::tests::cancel_test_run,
            commands::tests::get_active_test_run,
            commands::tests::list_test_runs,
            commands::tests::get_test_run,
            commands::tests::get_stability_test_config,
            commands::tests::set_stability_test_config,
        ])
        .build(context)
        .expect("error building tauri application")
        .run(move |app_handle, event| {
            if let tauri::RunEvent::Exit = event {
                // Cancel the nurse engine run loop + observability writers.
                let state: tauri::State<'_, AppState> = app_handle.state();
                if let Some(engine) = state.nurse_engine() {
                    engine.shutdown();
                }
                // Cancel all provider-extension poller tasks.
                {
                    let cancel = state.extension_poller_cancel.blocking_lock();
                    cancel.cancel();
                }
                // Best-effort: persist a terminal `Interrupted` status for any
                // swarm still marked `Implementing` or `Paused` in the
                // registry, so the next launch sees the correct state on disk
                // rather than a ghost "running" entry. The user can click
                // Resume on the next launch to continue from where the queen
                // left off. Capped with a short timeout so a sluggish disk
                // can't block process exit.
                let swarm_registry = Arc::clone(&state.swarm_registry);
                let swarm_store = Arc::clone(&state.swarm_store);
                tauri::async_runtime::block_on(async {
                    let fut = async {
                        for st in swarm_registry.list_all().await {
                            if !matches!(
                                st.status,
                                domain::swarm::SwarmStatus::Implementing
                                    | domain::swarm::SwarmStatus::Paused
                            ) {
                                continue;
                            }
                            // Safety guard: re-read disk and skip if the
                            // queen task already finished and persisted a
                            // terminal state while we were tearing down.
                            match swarm_store.read_state(&st.id).await {
                                Ok(Some(disk)) => {
                                    if matches!(
                                        disk.status,
                                        domain::swarm::SwarmStatus::Completed
                                            | domain::swarm::SwarmStatus::Failed
                                            | domain::swarm::SwarmStatus::Cancelled
                                    ) {
                                        tracing::info!(
                                            swarm_id = %st.id,
                                            disk_status = %disk.status,
                                            "on-exit: disk already terminal, skipping Interrupted write"
                                        );
                                        continue;
                                    }
                                }
                                Ok(None) => {
                                    // No disk record — nothing to write.
                                    continue;
                                }
                                Err(e) => {
                                    tracing::warn!(
                                        swarm_id = %st.id,
                                        error = %e,
                                        "on-exit: failed to re-read swarm state"
                                    );
                                    continue;
                                }
                            }
                            let mut updated = st.clone();
                            updated.error = Some(
                                crate::core::recovery::INTERRUPTED_BY_RESTART_MSG
                                    .to_string(),
                            );
                            updated.set_status(
                                domain::swarm::SwarmStatus::Interrupted,
                            );
                            if let Err(e) =
                                swarm_store.write_state(&updated.id, &updated).await
                            {
                                tracing::warn!(
                                    swarm_id = %updated.id,
                                    error = %e,
                                    "failed to persist Interrupted status on exit"
                                );
                            } else {
                                tracing::info!(
                                    swarm_id = %updated.id,
                                    "persisted Interrupted status on graceful exit"
                                );
                            }
                        }
                    };
                    let _ = tokio::time::timeout(
                        std::time::Duration::from_secs(2),
                        fut,
                    )
                    .await;
                });
                // Kill all Pi subprocesses concurrently with a per-session
                // timeout. `kill_on_drop(true)` on each child handles the
                // fallback if force_kill times out, so the exit path won't
                // block indefinitely.
                let pi_manager = Arc::clone(&state.pi_manager);
                tauri::async_runtime::block_on(async move {
                    let _ = pi_manager.shutdown_all().await;
                });
            }
        });
}
