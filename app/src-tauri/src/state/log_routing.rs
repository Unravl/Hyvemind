//! Per-ID routing tracing layer.
//!
//! Replaces the old monolithic daily-file JSON layer. Each event is written to
//! a path derived from the span hierarchy's ID fields, so a single grep / cat
//! can pull all events for a single Tasks-view session, hivemind review, or
//! swarm agent run.
//!
//! Disk-I/O happens off the calling thread: a single worker thread owns the
//! file-handle LRU and pulls (path, line) messages off an mpsc channel. On
//! channel overflow we drop the line rather than block the async runtime —
//! losing TRACE-level events is preferable to stalling the queen / a Pi reader.

use std::collections::{HashMap, VecDeque};
use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{sync_channel, SyncSender, TrySendError};
use std::sync::Arc;
use std::time::Duration;

use serde_json::{Map, Value};
use tracing::field::{Field, Visit};
use tracing::{Event, Subscriber};
use tracing_subscriber::layer::Context;
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::Layer;

use crate::state::log_redact::RedactingWriter;
use crate::tunables;

// Bounded capacity for the tracing log channel — see
// [`tunables::log_channel_capacity`] (default 4096, env override
// `HYVEMIND_LOG_CHANNEL_CAPACITY`).
const MAX_OPEN_FILES: usize = 64;
const FLUSH_INTERVAL: Duration = Duration::from_secs(1);
const DROP_REPORT_INTERVAL: Duration = Duration::from_secs(5);

#[derive(Clone, Debug, Default)]
pub(crate) struct RoutingContext {
    pub session_id: Option<String>,
    pub review_id: Option<String>,
    pub swarm_id: Option<String>,
    pub agent: Option<String>,
    pub feature_id: Option<String>,
    pub run_id: Option<String>,
}

impl RoutingContext {
    pub fn merge_override(&mut self, other: &Self) {
        if other.session_id.is_some() {
            self.session_id = other.session_id.clone();
        }
        if other.review_id.is_some() {
            self.review_id = other.review_id.clone();
        }
        if other.swarm_id.is_some() {
            self.swarm_id = other.swarm_id.clone();
        }
        if other.agent.is_some() {
            self.agent = other.agent.clone();
        }
        if other.feature_id.is_some() {
            self.feature_id = other.feature_id.clone();
        }
        if other.run_id.is_some() {
            self.run_id = other.run_id.clone();
        }
    }
}

struct FieldVisitor {
    fields: Map<String, Value>,
    routing: RoutingContext,
}

impl FieldVisitor {
    fn new() -> Self {
        Self {
            fields: Map::new(),
            routing: RoutingContext::default(),
        }
    }

    fn capture_id(&mut self, name: &str, value: &str) {
        let v = value.to_string();
        match name {
            "session_id" => self.routing.session_id = Some(v),
            "review_id" => self.routing.review_id = Some(v),
            "swarm_id" => self.routing.swarm_id = Some(v),
            "agent" => self.routing.agent = Some(v),
            "feature_id" => self.routing.feature_id = Some(v),
            "run_id" => self.routing.run_id = Some(v),
            _ => {}
        }
    }
}

impl Visit for FieldVisitor {
    fn record_str(&mut self, field: &Field, value: &str) {
        let name = field.name();
        self.capture_id(name, value);
        self.fields
            .insert(name.to_string(), Value::String(value.to_string()));
    }

    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        let name = field.name();
        let formatted = format!("{:?}", value);
        // tracing wraps string values from `display`/`%` formatting in quotes
        // when going through Debug; for ID capture purposes we want the inner
        // string. Strip a single pair of leading/trailing double quotes if
        // present.
        let id_view = strip_outer_quotes(&formatted);
        self.capture_id(name, id_view);
        self.fields
            .insert(name.to_string(), Value::String(formatted));
    }

    fn record_i64(&mut self, field: &Field, value: i64) {
        self.fields
            .insert(field.name().to_string(), Value::from(value));
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        self.fields
            .insert(field.name().to_string(), Value::from(value));
    }

    fn record_bool(&mut self, field: &Field, value: bool) {
        self.fields
            .insert(field.name().to_string(), Value::Bool(value));
    }

    fn record_f64(&mut self, field: &Field, value: f64) {
        self.fields
            .insert(field.name().to_string(), Value::from(value));
    }
}

fn strip_outer_quotes(s: &str) -> &str {
    let bytes = s.as_bytes();
    if bytes.len() >= 2 && bytes[0] == b'"' && bytes[bytes.len() - 1] == b'"' {
        &s[1..s.len() - 1]
    } else {
        s
    }
}

/// Validate-then-borrow helper for ID strings used as path components.
/// Returns `None` for empty / non `[A-Za-z0-9_-]+` values so untrusted IDs
/// can't traverse out of the debug dir.
fn ok_id(s: &Option<String>) -> Option<&str> {
    s.as_deref().filter(|v| {
        !v.is_empty()
            && v.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    })
}

fn resolve_path(debug_dir: &Path, ctx: &RoutingContext) -> PathBuf {
    if let Some(rid) = ok_id(&ctx.review_id) {
        return debug_dir.join("reviews").join(format!("{rid}.jsonl"));
    }
    if let Some(swid) = ok_id(&ctx.swarm_id) {
        let dir = debug_dir.join("swarms").join(swid);
        if let Some(agent) = ok_id(&ctx.agent) {
            let feat = ok_id(&ctx.feature_id);
            let run = ok_id(&ctx.run_id);
            let name = match (feat, run) {
                (Some(f), Some(r)) => format!("{agent}-{f}-{r}.jsonl"),
                (Some(f), None) => format!("{agent}-{f}.jsonl"),
                (None, Some(r)) => format!("{agent}-{r}.jsonl"),
                (None, None) => format!("{agent}.jsonl"),
            };
            return dir.join(name);
        }
        return dir.join("swarm.jsonl");
    }
    if let Some(sid) = ok_id(&ctx.session_id) {
        return debug_dir.join("sessions").join(format!("{sid}.jsonl"));
    }
    let today = chrono::Utc::now().format("%Y-%m-%d");
    debug_dir.join(format!("general.jsonl.{today}"))
}

struct WriterCache {
    map: HashMap<PathBuf, RedactingWriter<BufWriter<File>>>,
    order: VecDeque<PathBuf>,
}

impl WriterCache {
    fn new() -> Self {
        Self {
            map: HashMap::new(),
            order: VecDeque::new(),
        }
    }

    fn write_line(&mut self, path: &Path, line: &[u8]) -> std::io::Result<()> {
        if !self.map.contains_key(path) {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let file = OpenOptions::new().create(true).append(true).open(path)?;
            let writer = RedactingWriter::new(BufWriter::new(file));
            self.map.insert(path.to_path_buf(), writer);
            self.order.push_back(path.to_path_buf());
            self.evict_if_needed();
        } else {
            // Touch — move to back of order so it isn't first to be evicted.
            if let Some(idx) = self.order.iter().position(|p| p == path) {
                if let Some(p) = self.order.remove(idx) {
                    self.order.push_back(p);
                }
            }
        }

        let w = self
            .map
            .get_mut(path)
            .expect("writer was just inserted or already present");
        w.write_all(line)?;
        w.write_all(b"\n")?;
        Ok(())
    }

    fn evict_if_needed(&mut self) {
        while self.map.len() > MAX_OPEN_FILES {
            if let Some(oldest) = self.order.pop_front() {
                // Drop flushes BufWriter on drop.
                self.map.remove(&oldest);
            } else {
                break;
            }
        }
    }

    fn flush_all(&mut self) {
        for w in self.map.values_mut() {
            let _ = w.flush();
        }
    }
}

type Message = (PathBuf, Vec<u8>);

pub struct PerIdRoutingLayer {
    debug_dir: PathBuf,
    tx: SyncSender<Message>,
    dropped_events: Arc<AtomicU64>,
}

impl PerIdRoutingLayer {
    pub fn new(debug_dir: PathBuf) -> Self {
        let (tx, rx) = sync_channel::<Message>(tunables::log_channel_capacity());
        let dropped_events = Arc::new(AtomicU64::new(0));

        let spawn_result = std::thread::Builder::new()
            .name("hyvemind-log-router".to_string())
            .spawn(move || {
                let mut cache = WriterCache::new();
                loop {
                    match rx.recv_timeout(FLUSH_INTERVAL) {
                        Ok((path, line)) => {
                            if let Err(e) = cache.write_line(&path, &line) {
                                eprintln!("log-routing: failed to write {}: {}", path.display(), e);
                            }
                        }
                        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                            cache.flush_all();
                        }
                        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                            cache.flush_all();
                            break;
                        }
                    }
                }
            });

        if let Err(e) = spawn_result {
            // The router is best-effort: if the worker thread can't be spawned
            // (resource exhaustion, etc.) the layer becomes a no-op rather than
            // panicking and taking the app down on startup. Subsequent writes
            // hit `try_send` which sees a Disconnected receiver and is dropped
            // silently, matching the existing channel-full behaviour.
            eprintln!(
                "log-routing: failed to spawn hyvemind-log-router thread: {}; debug log routing disabled",
                e
            );
        }

        // Spawn a periodic drop-reporter that emits a WARN every DROP_REPORT_INTERVAL
        // whenever the dropped-event counter has advanced since the last check. We
        // deliberately use a dedicated OS thread (not a tokio task) so the reporter
        // runs even before any async runtime is fully initialised and survives
        // runtime shutdowns — same lifetime model as the writer thread above.
        let dropped_for_reporter = Arc::clone(&dropped_events);
        std::thread::Builder::new()
            .name("hyvemind-log-router-drops".to_string())
            .spawn(move || {
                let mut last_seen: u64 = 0;
                loop {
                    std::thread::sleep(DROP_REPORT_INTERVAL);
                    let current = dropped_for_reporter.load(Ordering::Relaxed);
                    if current > last_seen {
                        let delta = current - last_seen;
                        tracing::warn!(
                            target: "hyvemind::log_routing",
                            dropped_delta = delta,
                            dropped_total = current,
                            interval_secs = DROP_REPORT_INTERVAL.as_secs(),
                            "log routing channel dropped events (channel full)"
                        );
                        last_seen = current;
                    }
                }
            })
            .expect("spawn log-router drop reporter thread");

        Self {
            debug_dir,
            tx,
            dropped_events,
        }
    }
}

impl<S> Layer<S> for PerIdRoutingLayer
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_new_span(
        &self,
        attrs: &tracing::span::Attributes<'_>,
        id: &tracing::span::Id,
        ctx: Context<'_, S>,
    ) {
        let Some(span) = ctx.span(id) else { return };
        let mut visitor = FieldVisitor::new();
        attrs.record(&mut visitor);
        let mut merged = if let Some(parent) = span.parent() {
            parent
                .extensions()
                .get::<RoutingContext>()
                .cloned()
                .unwrap_or_default()
        } else {
            RoutingContext::default()
        };
        merged.merge_override(&visitor.routing);
        span.extensions_mut().insert(merged);
    }

    fn on_record(
        &self,
        id: &tracing::span::Id,
        values: &tracing::span::Record<'_>,
        ctx: Context<'_, S>,
    ) {
        let Some(span) = ctx.span(id) else { return };
        let mut visitor = FieldVisitor::new();
        values.record(&mut visitor);
        let mut ext = span.extensions_mut();
        if let Some(existing) = ext.get_mut::<RoutingContext>() {
            existing.merge_override(&visitor.routing);
        } else {
            ext.insert(visitor.routing);
        }
    }

    fn on_event(&self, event: &Event<'_>, ctx: Context<'_, S>) {
        let metadata = event.metadata();

        let mut routing = RoutingContext::default();
        let mut span_chain: Vec<Map<String, Value>> = Vec::new();
        if let Some(scope) = ctx.event_scope(event) {
            for span in scope.from_root() {
                if let Some(rc) = span.extensions().get::<RoutingContext>() {
                    routing.merge_override(rc);
                }
                let mut entry = Map::new();
                entry.insert("name".to_string(), Value::String(span.name().to_string()));
                span_chain.push(entry);
            }
        }

        let mut visitor = FieldVisitor::new();
        event.record(&mut visitor);
        routing.merge_override(&visitor.routing);

        let path = resolve_path(&self.debug_dir, &routing);

        let mut record = Map::new();
        record.insert(
            "timestamp".to_string(),
            Value::String(chrono::Utc::now().to_rfc3339()),
        );
        record.insert(
            "level".to_string(),
            Value::String(metadata.level().to_string()),
        );
        record.insert(
            "target".to_string(),
            Value::String(metadata.target().to_string()),
        );
        record.insert("fields".to_string(), Value::Object(visitor.fields));
        record.insert(
            "spans".to_string(),
            Value::Array(span_chain.into_iter().map(Value::Object).collect()),
        );

        let mut line = match serde_json::to_vec(&Value::Object(record)) {
            Ok(v) => v,
            Err(_) => return,
        };
        // Defensive: strip any embedded newlines so the file stays line-delimited.
        for b in line.iter_mut() {
            if *b == b'\n' || *b == b'\r' {
                *b = b' ';
            }
        }

        match self.tx.try_send((path, line)) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) | Err(TrySendError::Disconnected(_)) => {
                // Channel full or writer thread gone — drop the line rather than
                // block the calling thread. Count the drop so the periodic
                // reporter (and any IPC consumer) can surface burst losses.
                self.dropped_events.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn ctx() -> RoutingContext {
        RoutingContext::default()
    }

    #[test]
    fn resolve_review_priority() {
        let mut c = ctx();
        c.review_id = Some("hmr-abc".to_string());
        c.swarm_id = Some("swm-xyz".to_string());
        c.session_id = Some("sess-1".to_string());
        let p = resolve_path(Path::new("/d"), &c);
        assert_eq!(p, PathBuf::from("/d/reviews/hmr-abc.jsonl"));
    }

    #[test]
    fn resolve_swarm_with_agent_and_feature_run() {
        let mut c = ctx();
        c.swarm_id = Some("swm-1".to_string());
        c.agent = Some("worker".to_string());
        c.feature_id = Some("feat-2".to_string());
        c.run_id = Some("run-7".to_string());
        let p = resolve_path(Path::new("/d"), &c);
        assert_eq!(
            p,
            PathBuf::from("/d/swarms/swm-1/worker-feat-2-run-7.jsonl")
        );
    }

    #[test]
    fn resolve_swarm_agent_only() {
        let mut c = ctx();
        c.swarm_id = Some("swm-1".to_string());
        c.agent = Some("queen".to_string());
        let p = resolve_path(Path::new("/d"), &c);
        assert_eq!(p, PathBuf::from("/d/swarms/swm-1/queen.jsonl"));
    }

    #[test]
    fn resolve_swarm_agent_with_feature_only() {
        let mut c = ctx();
        c.swarm_id = Some("swm-1".to_string());
        c.agent = Some("worker".to_string());
        c.feature_id = Some("feat-2".to_string());
        let p = resolve_path(Path::new("/d"), &c);
        assert_eq!(p, PathBuf::from("/d/swarms/swm-1/worker-feat-2.jsonl"));
    }

    #[test]
    fn resolve_swarm_agent_with_run_only() {
        let mut c = ctx();
        c.swarm_id = Some("swm-1".to_string());
        c.agent = Some("worker".to_string());
        c.run_id = Some("run-7".to_string());
        let p = resolve_path(Path::new("/d"), &c);
        assert_eq!(p, PathBuf::from("/d/swarms/swm-1/worker-run-7.jsonl"));
    }

    #[test]
    fn resolve_swarm_alone() {
        let mut c = ctx();
        c.swarm_id = Some("swm-9".to_string());
        let p = resolve_path(Path::new("/d"), &c);
        assert_eq!(p, PathBuf::from("/d/swarms/swm-9/swarm.jsonl"));
    }

    #[test]
    fn resolve_session() {
        let mut c = ctx();
        c.session_id = Some("sess-abc".to_string());
        let p = resolve_path(Path::new("/d"), &c);
        assert_eq!(p, PathBuf::from("/d/sessions/sess-abc.jsonl"));
    }

    #[test]
    fn resolve_general_fallback() {
        let c = ctx();
        let p = resolve_path(Path::new("/d"), &c);
        let s = p.to_string_lossy().to_string();
        assert!(s.starts_with("/d/general.jsonl."), "got {}", s);
        // Should end with a 10-char date.
        let suffix = s.trim_start_matches("/d/general.jsonl.");
        assert_eq!(suffix.len(), 10);
    }

    #[test]
    fn rejects_path_traversal_review_id() {
        let mut c = ctx();
        c.review_id = Some("../../etc/passwd".to_string());
        let p = resolve_path(Path::new("/d"), &c);
        let s = p.to_string_lossy().to_string();
        assert!(s.starts_with("/d/general.jsonl."), "leaked: {}", s);
    }

    #[test]
    fn rejects_slash_in_session_id() {
        let mut c = ctx();
        c.session_id = Some("foo/bar".to_string());
        let p = resolve_path(Path::new("/d"), &c);
        let s = p.to_string_lossy().to_string();
        assert!(s.starts_with("/d/general.jsonl."), "leaked: {}", s);
    }

    #[test]
    fn rejects_backslash_in_swarm_id() {
        let mut c = ctx();
        c.swarm_id = Some("foo\\bar".to_string());
        let p = resolve_path(Path::new("/d"), &c);
        let s = p.to_string_lossy().to_string();
        assert!(s.starts_with("/d/general.jsonl."), "leaked: {}", s);
    }

    #[test]
    fn bad_review_id_falls_through_to_session() {
        let mut c = ctx();
        c.review_id = Some("../oops".to_string());
        c.session_id = Some("sess-good".to_string());
        let p = resolve_path(Path::new("/d"), &c);
        assert_eq!(p, PathBuf::from("/d/sessions/sess-good.jsonl"));
    }

    #[test]
    fn empty_id_rejected() {
        let mut c = ctx();
        c.review_id = Some(String::new());
        c.session_id = Some("sess-x".to_string());
        let p = resolve_path(Path::new("/d"), &c);
        assert_eq!(p, PathBuf::from("/d/sessions/sess-x.jsonl"));
    }

    #[test]
    fn merge_override_overrides_set_fields() {
        let mut a = RoutingContext {
            session_id: Some("a-sess".to_string()),
            review_id: Some("a-rev".to_string()),
            ..Default::default()
        };
        let b = RoutingContext {
            review_id: Some("b-rev".to_string()),
            swarm_id: Some("b-swm".to_string()),
            ..Default::default()
        };
        a.merge_override(&b);
        assert_eq!(a.session_id.as_deref(), Some("a-sess"));
        assert_eq!(a.review_id.as_deref(), Some("b-rev"));
        assert_eq!(a.swarm_id.as_deref(), Some("b-swm"));
    }

    #[test]
    fn merge_override_preserves_unset() {
        let mut a = RoutingContext {
            session_id: Some("keep".to_string()),
            ..Default::default()
        };
        let b = RoutingContext::default();
        a.merge_override(&b);
        assert_eq!(a.session_id.as_deref(), Some("keep"));
    }

    #[test]
    fn field_visitor_captures_known_ids_via_record_str() {
        let mut v = FieldVisitor::new();
        // Build a synthetic field via a real tracing event would be heavy;
        // instead exercise capture_id directly which record_str delegates to.
        v.capture_id("session_id", "sess-1");
        v.capture_id("review_id", "hmr-2");
        v.capture_id("swarm_id", "swm-3");
        v.capture_id("agent", "worker");
        v.capture_id("feature_id", "feat-4");
        v.capture_id("run_id", "run-5");
        v.capture_id("unknown", "ignored");

        assert_eq!(v.routing.session_id.as_deref(), Some("sess-1"));
        assert_eq!(v.routing.review_id.as_deref(), Some("hmr-2"));
        assert_eq!(v.routing.swarm_id.as_deref(), Some("swm-3"));
        assert_eq!(v.routing.agent.as_deref(), Some("worker"));
        assert_eq!(v.routing.feature_id.as_deref(), Some("feat-4"));
        assert_eq!(v.routing.run_id.as_deref(), Some("run-5"));
    }

    #[test]
    fn strip_outer_quotes_works() {
        assert_eq!(strip_outer_quotes("\"hello\""), "hello");
        assert_eq!(strip_outer_quotes("hello"), "hello");
        assert_eq!(strip_outer_quotes("\""), "\"");
        assert_eq!(strip_outer_quotes(""), "");
    }
}
