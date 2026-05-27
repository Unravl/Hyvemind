//! `ToolFailureDetector` — detects sessions retrying the same tool error,
//! plus bursts of mixed tool failures.
//!
//! Two trigger paths, both keyed per `tool_name`:
//!
//! - **Stuck**: a single `error_signature` accounts for ≥
//!   `stuck_concentration_ratio` of the failures inside a sliding
//!   `window_secs` window, after at least `min_failures_for_stuck` failures
//!   have accumulated. Severity is `Warn` on the first observation and
//!   escalates to `Stalled` after three consecutive ticks where the same
//!   `(tool_name, dominant_signature)` remains dominant.
//! - **Burst**: ≥ `burst_count` failures of any signature within
//!   `burst_count_secs` immediately raise `Stalled` regardless of diversity.
//!
//! Cleared on any successful `ToolExecutionEnd` for the same `tool_name`
//! (success carries no signature, so all stuck/burst dedup keys for the
//! tool clear in one sweep).
//!
//! The detector is intentionally tolerant of third-party result shapes:
//! a bare string / array / null `result` returns an empty signal delta
//! rather than panicking.

use std::any::Any;
use std::collections::{HashMap, HashSet, VecDeque};
use std::hash::Hasher;
use std::num::NonZeroUsize;
use std::sync::OnceLock;
use std::time::Instant;

use chrono::Utc;
use lru::LruCache;
use regex::Regex;
use smallvec::SmallVec;

use crate::nurse::config::ToolFailureDetectorConfig;
use crate::nurse::detector::{Detector, DetectorContext, SignalDelta, TickKind};
use crate::nurse::health::{Severity, Signal};
use crate::pi::events::PiEvent;

/// Max in-memory `raised_stuck_keys` per session. Bounded so a chatty
/// session with rotating signatures cannot grow the set without limit.
const MAX_RAISED_STUCK_KEYS: usize = 64;

/// Hard cap on the per-tool signature window. Acts as a memory safety
/// belt — the time-based trim is the primary control.
const MAX_WINDOW_ENTRIES_PER_TOOL: usize = 256;

/// Cap on raw stderr captured for evidence. Keeps every Signal serialised
/// payload well under the typical 4-8KB Tier-3 budget.
const EVIDENCE_STDERR_TAIL_BYTES: usize = 512;

/// Cap on bytes scanned out of result text/stderr to compute a signature.
/// Bounds the per-event work for pathologically large tool results.
const MAX_RESULT_SCAN_BYTES: usize = 32 * 1024;

/// Cached compiled regex for `cargo` `error[E####]:` extraction.
fn cargo_error_code_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"error\[(E\d{3,5})\]").expect("static regex compiles"))
}

/// Cached compiled regex for digit runs (used during normalisation).
fn digit_run_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\d+").expect("static regex compiles"))
}

/// Cached compiled regex for path-like substrings: a `/`-containing chunk
/// ending in `.ext`. Matches typical compiler paths like
/// `src/foo/bar.rs:12:5`.
fn path_like_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        // [^\s]* — no whitespace, must contain `/`, end at first whitespace.
        // We match the path portion up to and including the extension, plus
        // any optional `:line:col` tail Rust/TS tooling appends.
        Regex::new(r"[^\s]*/[^\s/]+\.[A-Za-z0-9]+(?::\d+(?::\d+)?)?")
            .expect("static regex compiles")
    })
}

pub struct ToolFailureDetector;

impl ToolFailureDetector {
    pub fn new() -> Self {
        Self
    }
}

impl Default for ToolFailureDetector {
    fn default() -> Self {
        Self::new()
    }
}

/// Per-session detector state.
pub struct ToolFailureState {
    /// `tool_call_id -> tool_name`, bounded by cfg.tool_call_id_cache_capacity.
    /// On miss (evicted), signal still emits with `tool_name = "unknown"`.
    tool_call_id_to_name: LruCache<String, String>,
    /// Per-tool sliding window of `(observed_at, signature)`. Trimmed on
    /// each `ToolExecutionEnd`.
    signature_window: HashMap<String, VecDeque<(Instant, String)>>,
    /// Stuck dedup keys we've raised since session start (or since last
    /// clear-on-success). Used to emit a `Clear` per key when a success
    /// arrives, since success has no signature to dedup against.
    raised_stuck_keys: HashSet<String>,
    /// Burst dedup keys currently raised, indexed by `tool_name`.
    raised_burst_keys: HashSet<String>,
    /// `dedup_key -> consecutive observations the same dominant signature
    /// has remained dominant`. Reset when the signature changes or clears.
    consecutive_stuck_ticks: HashMap<String, u32>,
}

impl ToolFailureState {
    fn new(capacity: usize) -> Self {
        let capacity = capacity.max(1);
        Self {
            tool_call_id_to_name: LruCache::new(
                NonZeroUsize::new(capacity).expect("capacity >= 1"),
            ),
            signature_window: HashMap::new(),
            raised_stuck_keys: HashSet::new(),
            raised_burst_keys: HashSet::new(),
            consecutive_stuck_ticks: HashMap::new(),
        }
    }
}

impl Detector for ToolFailureDetector {
    fn name(&self) -> &'static str {
        "tool_failure"
    }

    fn description(&self) -> &'static str {
        "Detects sessions stuck retrying the same tool error, plus bursts of mixed tool failures."
    }

    fn on_session_started(&self, ctx: &DetectorContext<'_>) -> Box<dyn Any + Send + Sync> {
        Box::new(ToolFailureState::new(
            ctx.profile_config.tool_failure.tool_call_id_cache_capacity,
        ))
    }

    fn tick_kind(&self) -> TickKind {
        TickKind::Fast
    }

    fn observe(
        &self,
        event: &PiEvent,
        ctx: &mut DetectorContext<'_>,
    ) -> SmallVec<[SignalDelta; 2]> {
        let cfg = ctx.profile_config.tool_failure.clone();
        if !cfg.enabled {
            return SmallVec::new();
        }

        let state = ctx
            .state
            .downcast_mut::<ToolFailureState>()
            .expect("ToolFailureState shape mismatch");

        match event {
            PiEvent::ToolExecutionStart {
                tool_call_id, name, ..
            } => {
                if !tool_call_id.is_empty() && !name.is_empty() {
                    state
                        .tool_call_id_to_name
                        .put(tool_call_id.clone(), name.clone());
                }
                SmallVec::new()
            }
            PiEvent::ToolExecutionEnd {
                tool_call_id,
                result,
            } => handle_tool_end(state, &cfg, ctx.now, tool_call_id, result),
            _ => SmallVec::new(),
        }
    }

    fn config_schema(&self) -> Vec<crate::nurse::snapshot::TunableDef> {
        use crate::nurse::snapshot::{TunableDef, TunableDirection, TunableKind};
        vec![
            TunableDef {
                name: "window_secs".to_string(),
                kind: TunableKind::Stepper,
                unit: "seconds".to_string(),
                direction: TunableDirection::HigherMoreSensitive,
                default: serde_json::json!(300),
                safe_range: serde_json::json!({"min": 30, "max": 3600}),
                description:
                    "Sliding window over which tool failures are aggregated per signature. Higher = more sensitive (longer memory)."
                        .to_string(),
            },
            TunableDef {
                name: "min_failures_for_stuck".to_string(),
                kind: TunableKind::Stepper,
                unit: "events".to_string(),
                direction: TunableDirection::HigherLessSensitive,
                default: serde_json::json!(5),
                safe_range: serde_json::json!({"min": 2, "max": 50}),
                description:
                    "Minimum failures inside the window before the stuck path can fire. Higher = less sensitive."
                        .to_string(),
            },
            TunableDef {
                name: "stuck_concentration_ratio".to_string(),
                kind: TunableKind::NumericRange,
                unit: "ratio".to_string(),
                direction: TunableDirection::HigherLessSensitive,
                default: serde_json::json!(0.7),
                safe_range: serde_json::json!({"min": 0.5, "max": 1.0}),
                description:
                    "Dominant signature / total failures ratio that declares the tool stuck. Higher = less sensitive."
                        .to_string(),
            },
            TunableDef {
                name: "burst_count".to_string(),
                kind: TunableKind::Stepper,
                unit: "events".to_string(),
                direction: TunableDirection::HigherLessSensitive,
                default: serde_json::json!(6),
                safe_range: serde_json::json!({"min": 3, "max": 50}),
                description:
                    "Number of failures (any signature) inside `burst_count_secs` that immediately raises a Stalled burst. Higher = less sensitive."
                        .to_string(),
            },
            TunableDef {
                name: "enabled".to_string(),
                kind: TunableKind::Toggle,
                unit: "".to_string(),
                direction: TunableDirection::Neutral,
                default: serde_json::json!(true),
                safe_range: serde_json::json!({}),
                description: "Master enable for the tool-failure detector on this profile."
                    .to_string(),
            },
        ]
    }
}

// -----------------------------------------------------------------------
// Core logic
// -----------------------------------------------------------------------

fn handle_tool_end(
    state: &mut ToolFailureState,
    cfg: &ToolFailureDetectorConfig,
    now: Instant,
    tool_call_id: &str,
    result: &serde_json::Value,
) -> SmallVec<[SignalDelta; 2]> {
    // Robustness: tools that return bare scalars / arrays / null are not
    // failure-shaped; bail without crashing.
    let obj = match result.as_object() {
        Some(o) => o,
        None => return SmallVec::new(),
    };

    // Resolve tool_name from LRU (or "unknown" on eviction — we do NOT
    // drop the signal because that would silently lose data when the cache
    // is under pressure).
    let tool_name = state
        .tool_call_id_to_name
        .get(&tool_call_id.to_string())
        .cloned()
        .unwrap_or_else(|| "unknown".to_string());

    let extracted = ExtractedResult::from_object(obj);

    if !extracted.is_failure() {
        // Success path: clear every stuck/burst key for this tool_name.
        return clear_keys_for_tool(state, &tool_name);
    }

    let signature = compute_signature(&tool_name, obj, &extracted);

    // Push into the per-tool window.
    let window = state
        .signature_window
        .entry(tool_name.clone())
        .or_insert_with(VecDeque::new);
    window.push_back((now, signature.clone()));

    // Trim by window_secs.
    let window_dur = std::time::Duration::from_secs(cfg.window_secs.max(1));
    while let Some(&(ts, _)) = window.front() {
        if now.duration_since(ts) > window_dur {
            window.pop_front();
        } else {
            break;
        }
    }
    // Hard cap so a pathological tool can't blow memory if config sets
    // window_secs to a very large value with high failure throughput.
    while window.len() > MAX_WINDOW_ENTRIES_PER_TOOL {
        window.pop_front();
    }

    let total_failures = window.len() as u32;

    // Burst window check (separate, shorter window).
    let burst_dur = std::time::Duration::from_secs(cfg.burst_count_secs.max(1));
    let burst_count = window
        .iter()
        .rev()
        .take_while(|(ts, _)| now.duration_since(*ts) <= burst_dur)
        .count() as u32;

    let mut out: SmallVec<[SignalDelta; 2]> = SmallVec::new();

    if burst_count >= cfg.burst_count {
        let dedup_key = format!("tool_burst:{}", tool_name);
        let raw_tail = extracted.raw_tail();
        let exit_code = extracted.exit_code();
        out.push(SignalDelta::Raise(Signal {
            detector: "tool_failure",
            severity: Severity::Stalled,
            dedup_key: dedup_key.clone(),
            summary: format!(
                "{}× tool failures of {} in {}s (burst path)",
                burst_count, tool_name, cfg.burst_count_secs
            ),
            raised_at: Utc::now(),
            evidence: serde_json::json!({
                "tool_name": tool_name,
                "signature": signature,
                "raw_stderr_tail": raw_tail,
                "exit_code": exit_code,
                "dominant_signature_count": signature_count(window, &signature),
                "total_failures_in_window": total_failures,
                "ratio": ratio(signature_count(window, &signature), total_failures),
                "window_secs": cfg.window_secs,
                "burst_count_secs": cfg.burst_count_secs,
                "burst_count": burst_count,
                "path": "burst",
            }),
        }));
        track_raised(&mut state.raised_burst_keys, dedup_key);
    }

    // Stuck path — concentration of a single signature.
    if total_failures >= cfg.min_failures_for_stuck {
        // Compute per-signature counts as &str -> u32 to avoid extra
        // allocations during max-finding.
        let mut counts: HashMap<&str, u32> = HashMap::new();
        for (_, sig) in window.iter() {
            *counts.entry(sig.as_str()).or_insert(0) += 1;
        }
        let (dominant_sig, dominant_count) = counts
            .iter()
            .max_by_key(|(_, n)| **n)
            .map(|(s, n)| ((*s).to_string(), *n))
            .unwrap_or_else(|| (String::new(), 0));

        let ratio_val = ratio(dominant_count, total_failures);

        if !dominant_sig.is_empty() && ratio_val >= cfg.stuck_concentration_ratio {
            let dedup_key = format!("tool_stuck:{}:{}", tool_name, dominant_sig);
            // Track consecutive-stuck escalation. Reset siblings (same
            // tool_name, different signature) so we don't carry counters
            // forward when the dominant flips.
            let prefix = format!("tool_stuck:{}:", tool_name);
            let stale: Vec<String> = state
                .consecutive_stuck_ticks
                .keys()
                .filter(|k| k.starts_with(&prefix) && k.as_str() != dedup_key)
                .cloned()
                .collect();
            for k in stale {
                state.consecutive_stuck_ticks.remove(&k);
            }
            let ticks = state
                .consecutive_stuck_ticks
                .entry(dedup_key.clone())
                .or_insert(0);
            *ticks = ticks.saturating_add(1);
            let severity = if *ticks >= 3 {
                Severity::Stalled
            } else {
                Severity::Warn
            };

            let raw_tail = extracted.raw_tail();
            let exit_code = extracted.exit_code();
            out.push(SignalDelta::Raise(Signal {
                detector: "tool_failure",
                severity,
                dedup_key: dedup_key.clone(),
                summary: format!(
                    "{}× tool failures of {} with dominant signature ({:.0}%)",
                    total_failures,
                    tool_name,
                    ratio_val * 100.0
                ),
                raised_at: Utc::now(),
                evidence: serde_json::json!({
                    "tool_name": tool_name,
                    "signature": dominant_sig,
                    "raw_stderr_tail": raw_tail,
                    "exit_code": exit_code,
                    "dominant_signature_count": dominant_count,
                    "total_failures_in_window": total_failures,
                    "ratio": ratio_val,
                    "window_secs": cfg.window_secs,
                    "consecutive_stuck_ticks": *ticks,
                    "path": "stuck",
                }),
            }));
            track_raised(&mut state.raised_stuck_keys, dedup_key);
        }
    }

    out
}

fn signature_count(window: &VecDeque<(Instant, String)>, sig: &str) -> u32 {
    window.iter().filter(|(_, s)| s == sig).count() as u32
}

fn ratio(count: u32, total: u32) -> f64 {
    if total == 0 {
        0.0
    } else {
        count as f64 / total as f64
    }
}

fn track_raised(set: &mut HashSet<String>, key: String) {
    if set.len() >= MAX_RAISED_STUCK_KEYS && !set.contains(&key) {
        // Drop an arbitrary entry to make room; this is best-effort.
        if let Some(victim) = set.iter().next().cloned() {
            set.remove(&victim);
        }
    }
    set.insert(key);
}

fn clear_keys_for_tool(
    state: &mut ToolFailureState,
    tool_name: &str,
) -> SmallVec<[SignalDelta; 2]> {
    let mut out: SmallVec<[SignalDelta; 2]> = SmallVec::new();

    let stuck_prefix = format!("tool_stuck:{}:", tool_name);
    let burst_key = format!("tool_burst:{}", tool_name);

    let to_clear: Vec<String> = state
        .raised_stuck_keys
        .iter()
        .filter(|k| k.starts_with(&stuck_prefix))
        .cloned()
        .collect();
    for k in to_clear {
        state.raised_stuck_keys.remove(&k);
        state.consecutive_stuck_ticks.remove(&k);
        out.push(SignalDelta::Clear {
            detector: "tool_failure",
            dedup_key: k,
        });
    }
    if state.raised_burst_keys.remove(&burst_key) {
        out.push(SignalDelta::Clear {
            detector: "tool_failure",
            dedup_key: burst_key,
        });
    }
    out
}

// -----------------------------------------------------------------------
// Failure parsing / signature computation
// -----------------------------------------------------------------------

struct ExtractedResult<'a> {
    /// Error string from `result["error"]` when it's a non-empty string.
    error_str: Option<&'a str>,
    /// Structured error object from `result["error"]` when it's an object.
    error_obj: Option<&'a serde_json::Map<String, serde_json::Value>>,
    is_error_flag: bool,
    exit_code: Option<i64>,
    stderr: Option<&'a str>,
    text: Option<&'a str>,
}

impl<'a> ExtractedResult<'a> {
    fn from_object(obj: &'a serde_json::Map<String, serde_json::Value>) -> Self {
        let error_str = obj
            .get("error")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty());
        let error_obj = obj.get("error").and_then(|v| v.as_object());
        let is_error_flag = obj
            .get("isError")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let exit_code = obj.get("exit_code").and_then(|v| v.as_i64());
        let stderr = obj
            .get("stderr")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty());
        let text = obj.get("text").and_then(|v| v.as_str());
        Self {
            error_str,
            error_obj,
            is_error_flag,
            exit_code,
            stderr,
            text,
        }
    }

    fn is_failure(&self) -> bool {
        if self.error_str.is_some() {
            return true;
        }
        if self.error_obj.is_some() {
            return true;
        }
        if self.is_error_flag {
            return true;
        }
        if let Some(code) = self.exit_code {
            if code != 0 {
                return true;
            }
        }
        if self.stderr.is_some() {
            return true;
        }
        if let Some(t) = self.text {
            if t.starts_with("Error:") {
                return true;
            }
        }
        false
    }

    fn exit_code(&self) -> i64 {
        self.exit_code.unwrap_or(-1)
    }

    fn raw_tail(&self) -> String {
        let s = self.stderr.or(self.error_str).or(self.text).unwrap_or("");
        tail(s, EVIDENCE_STDERR_TAIL_BYTES)
    }
}

fn tail(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut start = s.len().saturating_sub(max);
    while start < s.len() && !s.is_char_boundary(start) {
        start += 1;
    }
    s[start..].to_string()
}

fn compute_signature(
    tool_name: &str,
    obj: &serde_json::Map<String, serde_json::Value>,
    extracted: &ExtractedResult<'_>,
) -> String {
    // Structured-error path (any tool): prefer error.code / error.type.
    if let Some(err_obj) = extracted.error_obj {
        if let Some(code) = err_obj.get("code").and_then(|v| v.as_str()) {
            return normalise_short(code);
        }
        if let Some(ty) = err_obj.get("type").and_then(|v| v.as_str()) {
            return normalise_short(ty);
        }
        // Fall back to first 32 chars of the structured-error stringification.
        if let Some(raw) = obj.get("error").map(|v| v.to_string()) {
            let truncated: String = raw.chars().take(32).collect();
            return normalise_signature(&truncated);
        }
    }

    match tool_name {
        "cargo" => {
            // cargo: parse `error[E####]:` from stderr/text/error.
            let haystack = pick_signature_source(extracted);
            if let Some(cap) = cargo_error_code_re().captures(scan_window(&haystack)) {
                return cap
                    .get(1)
                    .map(|m| m.as_str().to_string())
                    .unwrap_or_default();
            }
            // Fallback: bash-style strategy.
            bash_signature(&haystack)
        }
        "bash" => {
            let haystack = pick_signature_source(extracted);
            bash_signature(&haystack)
        }
        _ => {
            // Generic: hash first 32 chars of normalised stderr (or
            // best-available text).
            let haystack = pick_signature_source(extracted);
            let scan = scan_window(&haystack);
            let truncated: String = scan.chars().take(32).collect();
            let normalised = normalise_signature(&truncated);
            hash_hex16(&normalised)
        }
    }
}

/// Best source string for signature computation, in priority order.
fn pick_signature_source(extracted: &ExtractedResult<'_>) -> String {
    if let Some(s) = extracted.stderr {
        return s.to_string();
    }
    if let Some(s) = extracted.error_str {
        return s.to_string();
    }
    if let Some(s) = extracted.text {
        return s.to_string();
    }
    String::new()
}

/// Cap how much of the source we scan; pathological tools may dump MB
/// of output and we don't want to spend that on every observation.
fn scan_window(s: &str) -> &str {
    if s.len() <= MAX_RESULT_SCAN_BYTES {
        s
    } else {
        let mut end = MAX_RESULT_SCAN_BYTES;
        while end > 0 && !s.is_char_boundary(end) {
            end -= 1;
        }
        &s[..end]
    }
}

/// `bash`-strategy signature: strip ANSI, find the first line containing
/// `error:` / `Error:`, extract a short token-based fingerprint
/// (the first few words after the `error:` marker), hash. Falls back
/// to a hash of the lightly-normalised first 32 chars when no error
/// line is present.
///
/// We deliberately do NOT apply the digit-run → `N` normalisation here:
/// for bash output, distinct numeric values inside error messages
/// (e.g. distinct file ids, line numbers, process counts) are often the
/// *only* thing distinguishing genuinely different failures, and
/// collapsing them folds high-diversity error streams into a single
/// signature. Path normalisation is still applied because path-like
/// substrings are noisy but rarely the discriminator on their own.
fn bash_signature(source: &str) -> String {
    let scan = scan_window(source);
    let stripped_bytes = strip_ansi_escapes::strip(scan.as_bytes());
    let stripped = String::from_utf8_lossy(&stripped_bytes);
    let line = stripped
        .lines()
        .find(|l| l.contains("error:") || l.contains("Error:"))
        .map(|s| s.to_string());

    match line {
        Some(l) => {
            let fingerprint = bash_error_fingerprint(&l);
            hash_hex16(&fingerprint)
        }
        None => {
            let truncated: String = stripped.chars().take(32).collect();
            let fingerprint = lightly_normalise(&truncated);
            hash_hex16(&fingerprint)
        }
    }
}

/// Extract a short, digit-preserving fingerprint from a bash error line.
/// Takes the first 3 whitespace-separated tokens after the `error:` /
/// `Error:` marker (or the start of the line if no marker is found),
/// lowercases them, replaces path-like substrings with `<path>`, and
/// joins them with a single space. Keeps digits intact so e.g.
/// "error: distinct kind 0 happened" and "error: distinct kind 1
/// happened" produce different signatures.
fn bash_error_fingerprint(line: &str) -> String {
    let lower = line.to_lowercase();
    let after_marker = match lower.find("error:") {
        Some(idx) => &lower[idx + "error:".len()..],
        None => lower.as_str(),
    };
    let no_paths = path_like_re()
        .replace_all(after_marker.trim_start(), "<path>")
        .to_string();
    let tokens: Vec<&str> = no_paths.split_whitespace().take(3).collect();
    tokens.join(" ")
}

/// Lightweight normalisation (no digit folding) used for the bash
/// fallback path and other places where we want to preserve numeric
/// discriminators.
fn lightly_normalise(s: &str) -> String {
    let lower = s.to_lowercase();
    let no_paths = path_like_re().replace_all(&lower, "<path>").to_string();
    no_paths.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Lowercase + collapse whitespace + replace digit-runs with `N` +
/// replace path-like substrings with `<path>`.
fn normalise_signature(s: &str) -> String {
    let lower = s.to_lowercase();
    // Path substitution first (before digits get nuked inside paths).
    let no_paths = path_like_re().replace_all(&lower, "<path>").to_string();
    // Digit runs.
    let no_digits = digit_run_re().replace_all(&no_paths, "N").to_string();
    // Collapse all whitespace runs to a single space, then trim.
    let collapsed = no_digits.split_whitespace().collect::<Vec<_>>().join(" ");
    collapsed
}

/// Light normalisation for short tokens (error codes / types) where we
/// want to preserve casing of the resulting hex-like code but still strip
/// surrounding whitespace.
fn normalise_short(s: &str) -> String {
    s.trim().to_string()
}

/// Hash to first 16 hex chars of an XxHash64 digest.
fn hash_hex16(s: &str) -> String {
    let mut h = twox_hash::XxHash64::with_seed(0);
    h.write(s.as_bytes());
    let digest = h.finish();
    let hex = format!("{:016x}", digest);
    hex[..16].to_string()
}

// -----------------------------------------------------------------------
// Tests
// -----------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nurse::config::{NurseProfile, ProfileConfig};
    use crate::nurse::snapshot::ProviderStateSnapshot;
    use crate::pi::events::PiEvent;
    use std::sync::{OnceLock, Weak};

    fn default_profile_config() -> &'static ProfileConfig {
        static CFG: OnceLock<ProfileConfig> = OnceLock::new();
        CFG.get_or_init(|| ProfileConfig::default_for(NurseProfile::Default))
    }

    fn make_ctx<'a>(
        state: &'a mut Box<dyn Any + Send + Sync>,
        weak: &'a Weak<crate::pi::session::PiSession>,
        snap: &'a ProviderStateSnapshot,
        now: Instant,
    ) -> DetectorContext<'a> {
        DetectorContext {
            session: weak,
            state: state.as_mut(),
            now,
            now_wall_ms: 0,
            profile: NurseProfile::Default,
            profile_config: default_profile_config(),
            provider_state: snap,
            provider: None,
            model_id: None,
        }
    }

    fn start_event(id: &str, name: &str) -> PiEvent {
        PiEvent::ToolExecutionStart {
            tool_call_id: id.to_string(),
            name: name.to_string(),
            args: serde_json::Value::Null,
        }
    }

    fn bash_error_end(id: &str, stderr: &str) -> PiEvent {
        PiEvent::ToolExecutionEnd {
            tool_call_id: id.to_string(),
            result: serde_json::json!({
                "exit_code": 1,
                "stderr": stderr,
            }),
        }
    }

    fn fresh_state(cap: usize) -> Box<dyn Any + Send + Sync> {
        Box::new(ToolFailureState::new(cap))
    }

    #[test]
    fn config_schema_has_units_and_directions() {
        let d = ToolFailureDetector::new();
        let schema = d.config_schema();
        assert!(!schema.is_empty());
        for entry in &schema {
            if !matches!(entry.kind, crate::nurse::snapshot::TunableKind::Toggle) {
                assert!(!entry.unit.is_empty(), "missing unit for {}", entry.name);
            }
            assert!(
                !entry.description.is_empty(),
                "missing description for {}",
                entry.name
            );
        }
    }

    #[test]
    fn high_diversity_no_signal_below_ratio() {
        let d = ToolFailureDetector::new();
        let weak: Weak<crate::pi::session::PiSession> = Weak::new();
        let snap = ProviderStateSnapshot::default();
        let mut state = fresh_state(1024);

        let now = Instant::now();
        // 5 bash failures, 5 distinct sigs.
        for i in 0..5 {
            let id = format!("call-{}", i);
            let stderr = format!("error: distinct kind {} happened\n", i);
            let mut ctx = make_ctx(&mut state, &weak, &snap, now);
            let _ = d.observe(&start_event(&id, "bash"), &mut ctx);
            let mut ctx = make_ctx(&mut state, &weak, &snap, now);
            let out = d.observe(&bash_error_end(&id, &stderr), &mut ctx);
            // Either nothing raised or burst path didn't trigger (5 < 6).
            for delta in &out {
                if let SignalDelta::Raise(sig) = delta {
                    assert!(
                        !sig.dedup_key.starts_with("tool_stuck:bash:"),
                        "unexpected stuck raise: {}",
                        sig.dedup_key
                    );
                }
            }
        }
    }

    #[test]
    fn dominant_signature_fires_tool_stuck() {
        let d = ToolFailureDetector::new();
        let weak: Weak<crate::pi::session::PiSession> = Weak::new();
        let snap = ProviderStateSnapshot::default();
        let mut state = fresh_state(1024);

        let now = Instant::now();
        // 4 identical + 1 distinct => 4/5 = 0.8 > 0.7.
        for i in 0..4 {
            let id = format!("rep-{}", i);
            let mut ctx = make_ctx(&mut state, &weak, &snap, now);
            let _ = d.observe(&start_event(&id, "bash"), &mut ctx);
            let mut ctx = make_ctx(&mut state, &weak, &snap, now);
            let _ = d.observe(
                &bash_error_end(&id, "error: cannot find file foo.rs\n"),
                &mut ctx,
            );
        }
        let mut ctx = make_ctx(&mut state, &weak, &snap, now);
        let _ = d.observe(&start_event("odd-1", "bash"), &mut ctx);
        let mut ctx = make_ctx(&mut state, &weak, &snap, now);
        let out = d.observe(
            &bash_error_end("odd-1", "error: totally different message\n"),
            &mut ctx,
        );

        let mut raised = false;
        for delta in &out {
            if let SignalDelta::Raise(sig) = delta {
                if sig.dedup_key.starts_with("tool_stuck:bash:") {
                    assert_eq!(sig.severity, Severity::Warn);
                    raised = true;
                }
            }
        }
        assert!(
            raised,
            "expected a tool_stuck:bash:* raise on dominant signature"
        );
    }

    #[test]
    fn burst_fires_independent_of_diversity() {
        let d = ToolFailureDetector::new();
        let weak: Weak<crate::pi::session::PiSession> = Weak::new();
        let snap = ProviderStateSnapshot::default();
        let mut state = fresh_state(1024);

        let now = Instant::now();
        // 6 failures of distinct signatures (within default 60s burst window).
        let mut last_out: SmallVec<[SignalDelta; 2]> = SmallVec::new();
        for i in 0..6 {
            let id = format!("burst-{}", i);
            let stderr = format!("error: unique sig number {}\n", i);
            let mut ctx = make_ctx(&mut state, &weak, &snap, now);
            let _ = d.observe(&start_event(&id, "bash"), &mut ctx);
            let mut ctx = make_ctx(&mut state, &weak, &snap, now);
            last_out = d.observe(&bash_error_end(&id, &stderr), &mut ctx);
        }

        let mut burst_seen = false;
        for delta in &last_out {
            if let SignalDelta::Raise(sig) = delta {
                if sig.dedup_key == "tool_burst:bash" {
                    assert_eq!(sig.severity, Severity::Stalled);
                    burst_seen = true;
                }
            }
        }
        assert!(
            burst_seen,
            "expected tool_burst:bash on 6 distinct failures within burst window"
        );
    }

    #[test]
    fn tool_call_id_to_name_bounded_1024() {
        let d = ToolFailureDetector::new();
        let weak: Weak<crate::pi::session::PiSession> = Weak::new();
        let snap = ProviderStateSnapshot::default();
        let mut state = fresh_state(1024);

        let now = Instant::now();
        for i in 0..2000 {
            let id = format!("c-{:05}", i);
            let mut ctx = make_ctx(&mut state, &weak, &snap, now);
            let _ = d.observe(&start_event(&id, "bash"), &mut ctx);
        }

        let st = state
            .downcast_ref::<ToolFailureState>()
            .expect("state shape");
        assert_eq!(st.tool_call_id_to_name.len(), 1024);
    }

    #[test]
    fn eviction_falls_back_to_unknown() {
        let d = ToolFailureDetector::new();
        let weak: Weak<crate::pi::session::PiSession> = Weak::new();
        let snap = ProviderStateSnapshot::default();
        // Small cache so we can force the early-id to be evicted.
        let mut state = fresh_state(4);

        let now = Instant::now();
        // Register an "early" id, then evict it.
        let early = "early-call".to_string();
        {
            let mut ctx = make_ctx(&mut state, &weak, &snap, now);
            let _ = d.observe(&start_event(&early, "bash"), &mut ctx);
        }
        for i in 0..8 {
            let id = format!("filler-{}", i);
            let mut ctx = make_ctx(&mut state, &weak, &snap, now);
            let _ = d.observe(&start_event(&id, "cargo"), &mut ctx);
        }

        // End of the evicted id should still emit (not be dropped) with
        // "unknown" tool_name; that means the window for "unknown" grows
        // and burst path may eventually fire — we just need this end to
        // not panic and to NOT be silently ignored.
        let mut ctx = make_ctx(&mut state, &weak, &snap, now);
        let _ = d.observe(&bash_error_end(&early, "error: boom\n"), &mut ctx);
        let st = state
            .downcast_ref::<ToolFailureState>()
            .expect("state shape");
        let unknown_window = st.signature_window.get("unknown");
        assert!(
            unknown_window.map(|w| !w.is_empty()).unwrap_or(false),
            "evicted id's end should still record into 'unknown' window"
        );
    }

    #[test]
    fn clear_on_any_success_for_tool_name() {
        let d = ToolFailureDetector::new();
        let weak: Weak<crate::pi::session::PiSession> = Weak::new();
        let snap = ProviderStateSnapshot::default();
        let mut state = fresh_state(1024);

        let now = Instant::now();
        // Drive a dominant-signature stuck raise.
        for i in 0..5 {
            let id = format!("dom-{}", i);
            let mut ctx = make_ctx(&mut state, &weak, &snap, now);
            let _ = d.observe(&start_event(&id, "bash"), &mut ctx);
            let mut ctx = make_ctx(&mut state, &weak, &snap, now);
            let _ = d.observe(
                &bash_error_end(&id, "error: same signature here\n"),
                &mut ctx,
            );
        }
        let raised_before = state
            .downcast_ref::<ToolFailureState>()
            .expect("state")
            .raised_stuck_keys
            .len();
        assert!(
            raised_before > 0,
            "expected at least one stuck key raised before clear"
        );

        // Successful tool end for the same tool_name.
        let mut ctx = make_ctx(&mut state, &weak, &snap, now);
        let _ = d.observe(&start_event("ok-call", "bash"), &mut ctx);
        let mut ctx = make_ctx(&mut state, &weak, &snap, now);
        let out = d.observe(
            &PiEvent::ToolExecutionEnd {
                tool_call_id: "ok-call".to_string(),
                result: serde_json::json!({"exit_code": 0, "stdout": "ok"}),
            },
            &mut ctx,
        );

        assert!(
            out.iter().any(|d| matches!(d, SignalDelta::Clear { dedup_key, .. } if dedup_key.starts_with("tool_stuck:bash:"))),
            "expected a tool_stuck:bash:* clear after success"
        );
        let st = state.downcast_ref::<ToolFailureState>().expect("state");
        assert_eq!(
            st.raised_stuck_keys.len(),
            0,
            "all stuck keys for bash should be cleared on success"
        );
    }

    #[test]
    fn malformed_result_returns_empty_signal() {
        let d = ToolFailureDetector::new();
        let weak: Weak<crate::pi::session::PiSession> = Weak::new();
        let snap = ProviderStateSnapshot::default();
        let mut state = fresh_state(1024);

        let now = Instant::now();
        let mut ctx = make_ctx(&mut state, &weak, &snap, now);
        let _ = d.observe(&start_event("m-1", "bash"), &mut ctx);

        for shape in [
            serde_json::json!("a bare string result"),
            serde_json::json!(["array", "result"]),
            serde_json::Value::Null,
            serde_json::json!(42),
        ] {
            let event = PiEvent::ToolExecutionEnd {
                tool_call_id: "m-1".to_string(),
                result: shape,
            };
            let mut ctx = make_ctx(&mut state, &weak, &snap, now);
            let out = d.observe(&event, &mut ctx);
            assert!(out.is_empty(), "expected empty delta for malformed result");
        }
    }

    #[test]
    fn consecutive_stuck_ticks_escalate_to_stalled() {
        let d = ToolFailureDetector::new();
        let weak: Weak<crate::pi::session::PiSession> = Weak::new();
        let snap = ProviderStateSnapshot::default();
        let mut state = fresh_state(1024);

        let now = Instant::now();
        // Seed the window with 5 identical failures so it's stuck.
        for i in 0..5 {
            let id = format!("seed-{}", i);
            let mut ctx = make_ctx(&mut state, &weak, &snap, now);
            let _ = d.observe(&start_event(&id, "bash"), &mut ctx);
            let mut ctx = make_ctx(&mut state, &weak, &snap, now);
            let _ = d.observe(&bash_error_end(&id, "error: same stuck thing\n"), &mut ctx);
        }

        // Each subsequent failure with the same signature increments the
        // consecutive-tick counter. By the 3rd observe with same dominant
        // sig, severity should escalate.
        let mut last_severity = Severity::Info;
        let mut last_dedup = String::new();
        for i in 0..3 {
            let id = format!("more-{}", i);
            let mut ctx = make_ctx(&mut state, &weak, &snap, now);
            let _ = d.observe(&start_event(&id, "bash"), &mut ctx);
            let mut ctx = make_ctx(&mut state, &weak, &snap, now);
            let out = d.observe(&bash_error_end(&id, "error: same stuck thing\n"), &mut ctx);
            for delta in &out {
                if let SignalDelta::Raise(sig) = delta {
                    if sig.dedup_key.starts_with("tool_stuck:bash:") {
                        last_severity = sig.severity;
                        last_dedup = sig.dedup_key.clone();
                    }
                }
            }
        }
        assert!(
            !last_dedup.is_empty(),
            "expected at least one tool_stuck raise across the loop"
        );
        assert_eq!(
            last_severity,
            Severity::Stalled,
            "expected escalation to Stalled after consecutive dominant ticks"
        );
    }

    #[test]
    fn cargo_signature_extracts_error_code() {
        // E0277 should fall out of the normalised stderr.
        let mut state = fresh_state(1024);
        let weak: Weak<crate::pi::session::PiSession> = Weak::new();
        let snap = ProviderStateSnapshot::default();
        let d = ToolFailureDetector::new();
        let now = Instant::now();

        let mut ctx = make_ctx(&mut state, &weak, &snap, now);
        let _ = d.observe(&start_event("c-1", "cargo"), &mut ctx);
        let mut ctx = make_ctx(&mut state, &weak, &snap, now);
        let _ = d.observe(
            &PiEvent::ToolExecutionEnd {
                tool_call_id: "c-1".to_string(),
                result: serde_json::json!({
                    "exit_code": 1,
                    "stderr": "error[E0277]: the trait bound `T: Foo` is not satisfied\n  --> src/lib.rs:10:5\n",
                }),
            },
            &mut ctx,
        );

        let st = state.downcast_ref::<ToolFailureState>().expect("state");
        let window = st.signature_window.get("cargo").expect("cargo window");
        assert_eq!(window.len(), 1);
        assert_eq!(window.front().unwrap().1.as_str(), "E0277");
    }

    #[test]
    fn structured_error_uses_code_then_type() {
        let mut state = fresh_state(1024);
        let weak: Weak<crate::pi::session::PiSession> = Weak::new();
        let snap = ProviderStateSnapshot::default();
        let d = ToolFailureDetector::new();
        let now = Instant::now();

        let mut ctx = make_ctx(&mut state, &weak, &snap, now);
        let _ = d.observe(&start_event("s-1", "custom"), &mut ctx);
        let mut ctx = make_ctx(&mut state, &weak, &snap, now);
        let _ = d.observe(
            &PiEvent::ToolExecutionEnd {
                tool_call_id: "s-1".to_string(),
                result: serde_json::json!({
                    "error": { "code": "ENOENT", "type": "fs_error" },
                }),
            },
            &mut ctx,
        );
        let st = state.downcast_ref::<ToolFailureState>().expect("state");
        let window = st.signature_window.get("custom").expect("custom window");
        assert_eq!(window.front().unwrap().1.as_str(), "ENOENT");
    }
}
