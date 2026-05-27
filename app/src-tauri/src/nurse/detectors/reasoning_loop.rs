//! `ReasoningLoopDetector` — three-layered detection of in-place thinking loops.
//!
//! Pi sessions occasionally fall into a "thinking" rut where the model
//! re-emits the same reasoning chunk (or a paraphrase of it) over and over
//! without making forward progress. This detector spots the rut early via
//! three independent signals, layered cheap-to-expensive:
//!
//! 1. **SipHash exact-repeat fast-path** (per `ThinkingDelta`) — normalise the
//!    chunk and hash it; if the same hash recurs >= N times inside the
//!    window, raise `loop:exact:{hash}` (Warn -> Stalled after a 3-tick soak).
//! 2. **Compression-ratio drift** (every Nth ThinkingDelta) — keep the last
//!    ~4KB of normalised thinking in a FIFO buffer; if `zstd(buf)/buf` drops
//!    below the threshold for K consecutive samples, raise `loop:compression`.
//! 3. **MinHash paraphrase similarity** (only at `MessageEnd`) — sketch the
//!    full thinking block with 128 MinHash hashes (word-level 3-shingles) and
//!    compare Jaccard against the last 4 sketches. A calibration guard
//!    requires N consecutive paraphrase hits before escalating past `Info`.
//!
//! Any `ToolExecutionStart` or a long (>200 char) `TextDelta` clears every
//! `loop:*` signal — both mean the model is doing something other than
//! re-chewing the same thought.

use std::any::Any;
use std::collections::{HashMap, HashSet, VecDeque};
use std::hash::Hasher;
use std::time::{Duration, Instant};

use chrono::Utc;
use siphasher::sip::SipHasher24;
use smallvec::{smallvec, SmallVec};
use twox_hash::XxHash64;

use crate::nurse::config::{NurseProfile, ReasoningLoopDetectorConfig};
use crate::nurse::detector::{Detector, DetectorContext, SignalDelta, TickKind};
use crate::nurse::health::{Severity, Signal};
use crate::pi::events::PiEvent;

const DETECTOR_NAME: &str = "reasoning_loop";

const COMPRESSION_BUF_CAP: usize = 4096;
const COMPRESSION_RATIO_HISTORY: usize = 3;
const MINHASH_HASHES: usize = 128;
const MINHASH_RING_CAP: usize = 4;
const TEXT_CLEAR_THRESHOLD: usize = 200;
const ZSTD_COMPRESSION_LEVEL: i32 = 3;
/// Each Stalled escalation requires the signal to persist for this many fast
/// ticks (~30s at the engine's default tick cadence) after the initial Warn.
const ESCALATE_AFTER: Duration = Duration::from_secs(30);

pub struct ReasoningLoopDetector;

impl ReasoningLoopDetector {
    pub fn new() -> Self {
        Self
    }
}

impl Default for ReasoningLoopDetector {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Default)]
pub struct ReasoningLoopState {
    /// SipHash(normalised chunk) -> recent observation timestamps.
    exact_hashes: HashMap<u64, VecDeque<Instant>>,
    /// First-Warn instants per exact-repeat key for escalate-after-soak.
    exact_warned_at: HashMap<u64, Instant>,
    /// Hashes currently raised so we can emit Clear on rollover.
    exact_raised_keys: HashSet<u64>,

    /// Rolling FIFO of recent normalised thinking bytes, capped at 4KB.
    compression_buf: VecDeque<u8>,
    compression_samples_since_last: u32,
    recent_compression_ratios: VecDeque<f64>,
    compression_raised: bool,
    compression_warned_at: Option<Instant>,

    /// Per-message thinking accumulator (cleared on MessageStart).
    current_message_thinking_buf: String,
    /// Ring of the last few MinHash sketches.
    recent_sketches: VecDeque<[u64; MINHASH_HASHES]>,
    /// Consecutive paraphrase hits across MessageEnd boundaries — reset by
    /// any MessageEnd that doesn't hit.
    consecutive_paraphrase_hits: u32,
    paraphrase_raised: bool,
}

impl Detector for ReasoningLoopDetector {
    fn name(&self) -> &'static str {
        DETECTOR_NAME
    }

    fn description(&self) -> &'static str {
        "Detects in-place thinking loops via exact-repeat hash, compression-ratio drift, and MinHash paraphrase similarity."
    }

    fn on_session_started(&self, _ctx: &DetectorContext<'_>) -> Box<dyn Any + Send + Sync> {
        Box::new(ReasoningLoopState::default())
    }

    fn tick_kind(&self) -> TickKind {
        TickKind::Fast
    }

    fn observe(
        &self,
        event: &PiEvent,
        ctx: &mut DetectorContext<'_>,
    ) -> SmallVec<[SignalDelta; 2]> {
        let cfg = ctx.profile_config.reasoning_loop.clone();
        if !cfg.enabled {
            return SmallVec::new();
        }
        let now = ctx.now;
        let state = ctx
            .state
            .downcast_mut::<ReasoningLoopState>()
            .expect("ReasoningLoopState shape mismatch");

        let mut out: SmallVec<[SignalDelta; 2]> = SmallVec::new();

        match event {
            PiEvent::ThinkingDelta(raw) => {
                let normalised = normalise(raw);
                if normalised.is_empty() {
                    return out;
                }

                // ---- Layer 1: SipHash exact-repeat fast-path -----------
                if let Some(delta) = exact_repeat_observe(state, &normalised, now, &cfg) {
                    out.push(delta);
                }

                // ---- Layer 2 (sampled): compression-ratio drift --------
                append_to_compression_buf(state, normalised.as_bytes());
                state.compression_samples_since_last =
                    state.compression_samples_since_last.saturating_add(1);
                if state.compression_samples_since_last >= cfg.compression_sample_every_n_events
                    && state.compression_buf.len() >= 64
                {
                    state.compression_samples_since_last = 0;
                    if let Some(delta) = compression_observe(state, now, &cfg) {
                        out.push(delta);
                    }
                }

                // Layer 3 (MinHash) accumulates here but only fires at MessageEnd.
                if !state.current_message_thinking_buf.is_empty() {
                    state.current_message_thinking_buf.push(' ');
                }
                state.current_message_thinking_buf.push_str(&normalised);
            }

            PiEvent::TextDelta(raw) => {
                // Long text delta = the model is talking back to the user
                // (not chewing on the same thought). Clear all loop:*.
                if raw.chars().count() > TEXT_CLEAR_THRESHOLD {
                    push_clears_for_all_loop_signals(state, &mut out);
                }
                // Still contribute to the per-message buffer so MinHash sees
                // the full thinking-plus-text block.
                let normalised = normalise(raw);
                if !normalised.is_empty() {
                    if !state.current_message_thinking_buf.is_empty() {
                        state.current_message_thinking_buf.push(' ');
                    }
                    state.current_message_thinking_buf.push_str(&normalised);
                }
            }

            PiEvent::MessageStart => {
                state.current_message_thinking_buf.clear();
            }

            PiEvent::MessageEnd => {
                if let Some(delta) = paraphrase_observe(state, &cfg) {
                    out.push(delta);
                }
                state.current_message_thinking_buf.clear();
            }

            PiEvent::ToolExecutionStart { .. } => {
                push_clears_for_all_loop_signals(state, &mut out);
            }

            _ => {}
        }

        out
    }

    fn tick(&self, ctx: &mut DetectorContext<'_>) -> SmallVec<[SignalDelta; 2]> {
        let cfg = ctx.profile_config.reasoning_loop.clone();
        if !cfg.enabled {
            return SmallVec::new();
        }
        let now = ctx.now;
        let state = ctx
            .state
            .downcast_mut::<ReasoningLoopState>()
            .expect("ReasoningLoopState shape mismatch");
        let mut out: SmallVec<[SignalDelta; 2]> = SmallVec::new();

        // Promote any exact-repeat key that has been Warn'd for >= ESCALATE_AFTER.
        let to_escalate: Vec<u64> = state
            .exact_warned_at
            .iter()
            .filter_map(|(k, t)| {
                if now.saturating_duration_since(*t) >= ESCALATE_AFTER {
                    Some(*k)
                } else {
                    None
                }
            })
            .collect();
        for key in to_escalate {
            let hits = state.exact_hashes.get(&key).map(|q| q.len()).unwrap_or(0);
            out.push(SignalDelta::Raise(Signal {
                detector: DETECTOR_NAME,
                severity: Severity::Stalled,
                dedup_key: format!("loop:exact:{}", key),
                summary: format!(
                    "exact-repeat thinking chunk persisted for >= {}s ({} hits in window)",
                    ESCALATE_AFTER.as_secs(),
                    hits
                ),
                raised_at: Utc::now(),
                evidence: serde_json::json!({
                    "layer": "exact_repeat",
                    "siphash": key,
                    "recent_hits_in_window": hits,
                    "escalated_after_secs": ESCALATE_AFTER.as_secs(),
                }),
            }));
            // Keep `exact_warned_at` populated so Stalled doesn't immediately
            // un-escalate; clear-edge logic removes it on rollover.
        }

        // Same idea for the compression-ratio raise.
        if state.compression_raised {
            if let Some(warned_at) = state.compression_warned_at {
                if now.saturating_duration_since(warned_at) >= ESCALATE_AFTER {
                    out.push(SignalDelta::Raise(Signal {
                        detector: DETECTOR_NAME,
                        severity: Severity::Stalled,
                        dedup_key: "loop:compression".to_string(),
                        summary: format!(
                            "thinking compression ratio persisted < {} for >= {}s",
                            cfg.compression_ratio_threshold,
                            ESCALATE_AFTER.as_secs()
                        ),
                        raised_at: Utc::now(),
                        evidence: serde_json::json!({
                            "layer": "compression",
                            "threshold": cfg.compression_ratio_threshold,
                            "recent_ratios": state.recent_compression_ratios.iter().collect::<Vec<_>>(),
                            "escalated_after_secs": ESCALATE_AFTER.as_secs(),
                        }),
                    }));
                }
            }
        }

        out
    }

    fn config_schema(&self) -> Vec<crate::nurse::snapshot::TunableDef> {
        use crate::nurse::snapshot::{TunableDef, TunableDirection, TunableKind};
        vec![
            TunableDef {
                name: "siphash_repeats_to_fire".to_string(),
                kind: TunableKind::Stepper,
                unit: "repeats".to_string(),
                direction: TunableDirection::HigherLessSensitive,
                default: serde_json::json!(5),
                safe_range: serde_json::json!({"min": 2, "max": 10}),
                description:
                    "Number of identical (post-normalisation) thinking chunks inside the window before `loop:exact` fires."
                        .to_string(),
            },
            TunableDef {
                name: "siphash_min_chunk_chars".to_string(),
                kind: TunableKind::Stepper,
                unit: "chars".to_string(),
                direction: TunableDirection::HigherLessSensitive,
                default: serde_json::json!(64),
                safe_range: serde_json::json!({"min": 1, "max": 4096}),
                description:
                    "Minimum length of a normalised thinking chunk before it's even considered for SipHash hashing. Guards against tripping on coincidental repeats of trivial tokens (e.g. `i`, `the`, ` `). 64 ≈ one substantive sentence fragment."
                        .to_string(),
            },
            TunableDef {
                name: "compression_ratio_threshold".to_string(),
                kind: TunableKind::NumericRange,
                unit: "ratio".to_string(),
                direction: TunableDirection::HigherMoreSensitive,
                default: serde_json::json!(0.35),
                safe_range: serde_json::json!({"min": 0.05, "max": 0.9}),
                description:
                    "`zstd(buf).len() / buf.len()` must drop below this for three consecutive samples to fire `loop:compression`. Lower = louder text is more repetitive."
                        .to_string(),
            },
            TunableDef {
                name: "paraphrase_jaccard_threshold".to_string(),
                kind: TunableKind::NumericRange,
                unit: "jaccard".to_string(),
                direction: TunableDirection::HigherLessSensitive,
                default: serde_json::json!(0.7),
                safe_range: serde_json::json!({"min": 0.4, "max": 0.95}),
                description:
                    "MinHash-estimated Jaccard similarity above which a new thinking block counts as a paraphrase of a recent one."
                        .to_string(),
            },
            TunableDef {
                name: "min_paraphrase_raise_count".to_string(),
                kind: TunableKind::Stepper,
                unit: "hits".to_string(),
                direction: TunableDirection::HigherLessSensitive,
                default: serde_json::json!(2),
                safe_range: serde_json::json!({"min": 1, "max": 5}),
                description:
                    "Calibration guard. First hit is Info only; the Nth consecutive hit promotes to Warn; (N+1)th to Stalled. Any non-matching MessageEnd resets the counter."
                        .to_string(),
            },
            TunableDef {
                name: "enabled".to_string(),
                kind: TunableKind::Toggle,
                unit: "".to_string(),
                direction: TunableDirection::Neutral,
                default: serde_json::json!(true),
                safe_range: serde_json::json!({}),
                description:
                    "Master enable for the reasoning-loop detector on this profile.".to_string(),
            },
        ]
    }
}

// ---------------------------------------------------------------------------
// Layer 1 — SipHash exact-repeat
// ---------------------------------------------------------------------------

fn exact_repeat_observe(
    state: &mut ReasoningLoopState,
    normalised: &str,
    now: Instant,
    cfg: &ReasoningLoopDetectorConfig,
) -> Option<SignalDelta> {
    // Skip short fragments — LLM thinking streams emit lots of short
    // chunks (single words, punctuation, even single letters) and
    // coincidental repeats of trivial tokens like "i" or "the" trip the
    // detector without representing a real reasoning loop. The user's
    // first observation of this detector misfire was a 1-char "i"
    // preview firing inside a normal long-thinking turn.
    if normalised.len() < cfg.siphash_min_chunk_chars {
        return None;
    }
    let hash = siphash24(normalised.as_bytes());
    let window = Duration::from_secs(cfg.siphash_window_secs);
    let entry = state.exact_hashes.entry(hash).or_default();
    // Drop expired entries off the front.
    while let Some(front) = entry.front() {
        if now.saturating_duration_since(*front) > window {
            entry.pop_front();
        } else {
            break;
        }
    }
    entry.push_back(now);
    let count = entry.len();

    if count >= cfg.siphash_repeats_to_fire as usize {
        let already_raised = state.exact_raised_keys.contains(&hash);
        state.exact_raised_keys.insert(hash);
        state.exact_warned_at.entry(hash).or_insert(now);
        if !already_raised {
            return Some(SignalDelta::Raise(Signal {
                detector: DETECTOR_NAME,
                severity: Severity::Warn,
                dedup_key: format!("loop:exact:{}", hash),
                summary: format!(
                    "thinking chunk repeated {} times in {}s window",
                    count, cfg.siphash_window_secs
                ),
                raised_at: Utc::now(),
                evidence: serde_json::json!({
                    "layer": "exact_repeat",
                    "siphash": hash,
                    "hits_in_window": count,
                    "window_secs": cfg.siphash_window_secs,
                    "preview": preview(normalised, 160),
                }),
            }));
        }
    }
    None
}

fn siphash24(bytes: &[u8]) -> u64 {
    let mut h = SipHasher24::new();
    h.write(bytes);
    h.finish()
}

// ---------------------------------------------------------------------------
// Layer 2 — compression-ratio drift
// ---------------------------------------------------------------------------

fn append_to_compression_buf(state: &mut ReasoningLoopState, bytes: &[u8]) {
    // Insert a single-byte separator between distinct chunks so that
    // boundary-spanning repeats still compress well.
    if !state.compression_buf.is_empty() {
        if state.compression_buf.len() >= COMPRESSION_BUF_CAP {
            state.compression_buf.pop_front();
        }
        state.compression_buf.push_back(b' ');
    }
    for &b in bytes {
        if state.compression_buf.len() >= COMPRESSION_BUF_CAP {
            state.compression_buf.pop_front();
        }
        state.compression_buf.push_back(b);
    }
}

fn compression_observe(
    state: &mut ReasoningLoopState,
    now: Instant,
    cfg: &ReasoningLoopDetectorConfig,
) -> Option<SignalDelta> {
    let contiguous: Vec<u8> = state.compression_buf.iter().copied().collect();
    if contiguous.is_empty() {
        return None;
    }
    let compressed = match zstd::encode_all(&contiguous[..], ZSTD_COMPRESSION_LEVEL) {
        Ok(v) => v,
        Err(_) => return None,
    };
    let ratio = (compressed.len() as f64) / (contiguous.len() as f64);

    if state.recent_compression_ratios.len() >= COMPRESSION_RATIO_HISTORY {
        state.recent_compression_ratios.pop_front();
    }
    state.recent_compression_ratios.push_back(ratio);

    let trending_down = is_strictly_non_increasing(&state.recent_compression_ratios);
    let all_below = state.recent_compression_ratios.len() == COMPRESSION_RATIO_HISTORY
        && state
            .recent_compression_ratios
            .iter()
            .all(|r| *r < cfg.compression_ratio_threshold);

    if all_below && trending_down {
        let already = state.compression_raised;
        state.compression_raised = true;
        state.compression_warned_at.get_or_insert(now);
        if !already {
            return Some(SignalDelta::Raise(Signal {
                detector: DETECTOR_NAME,
                severity: Severity::Warn,
                dedup_key: "loop:compression".to_string(),
                summary: format!(
                    "thinking compression ratio {:.3} below threshold {:.3} for {} consecutive samples",
                    ratio, cfg.compression_ratio_threshold, COMPRESSION_RATIO_HISTORY
                ),
                raised_at: Utc::now(),
                evidence: serde_json::json!({
                    "layer": "compression",
                    "ratio": ratio,
                    "threshold": cfg.compression_ratio_threshold,
                    "recent_ratios": state.recent_compression_ratios.iter().collect::<Vec<_>>(),
                    "buf_bytes": contiguous.len(),
                }),
            }));
        }
    }
    None
}

fn is_strictly_non_increasing(q: &VecDeque<f64>) -> bool {
    let mut prev: Option<f64> = None;
    for v in q {
        if let Some(p) = prev {
            if *v > p {
                return false;
            }
        }
        prev = Some(*v);
    }
    true
}

// ---------------------------------------------------------------------------
// Layer 3 — MinHash paraphrase similarity
// ---------------------------------------------------------------------------

fn paraphrase_observe(
    state: &mut ReasoningLoopState,
    cfg: &ReasoningLoopDetectorConfig,
) -> Option<SignalDelta> {
    let block = std::mem::take(&mut state.current_message_thinking_buf);
    if block.chars().count() < cfg.paraphrase_min_block_chars {
        return None;
    }
    let words: Vec<&str> = block.split_whitespace().collect();
    if words.len() < 3 {
        return None;
    }
    let shingles: Vec<String> = shingles3(&words).into_iter().map(|s| s.join(" ")).collect();
    if shingles.is_empty() {
        return None;
    }
    let sketch = minhash128(&shingles);

    let mut best: f64 = 0.0;
    let mut best_index: Option<usize> = None;
    for (i, prior) in state.recent_sketches.iter().enumerate() {
        let j = jaccard128(&sketch, prior);
        if j > best {
            best = j;
            best_index = Some(i);
        }
    }

    if state.recent_sketches.len() >= MINHASH_RING_CAP {
        state.recent_sketches.pop_front();
    }
    state.recent_sketches.push_back(sketch);

    if best > cfg.paraphrase_jaccard_threshold {
        state.consecutive_paraphrase_hits = state.consecutive_paraphrase_hits.saturating_add(1);
        let hits = state.consecutive_paraphrase_hits;
        let raise_at = cfg.min_paraphrase_raise_count.max(1);
        let severity = if hits == 1 {
            Severity::Info
        } else if hits <= raise_at {
            Severity::Warn
        } else {
            Severity::Stalled
        };
        state.paraphrase_raised = true;
        Some(SignalDelta::Raise(Signal {
            detector: DETECTOR_NAME,
            severity,
            dedup_key: "loop:paraphrase".to_string(),
            summary: format!(
                "paraphrase Jaccard {:.2} > {:.2} (consecutive hits {})",
                best, cfg.paraphrase_jaccard_threshold, hits
            ),
            raised_at: Utc::now(),
            evidence: serde_json::json!({
                "layer": "paraphrase",
                "jaccard": best,
                "threshold": cfg.paraphrase_jaccard_threshold,
                "matched_index_in_ring": best_index,
                "consecutive_hits": hits,
                "min_raise_count": raise_at,
                "block_chars": block.chars().count(),
            }),
        }))
    } else {
        // Non-matching MessageEnd resets the counter and clears any raise.
        let was_raised = state.paraphrase_raised;
        state.consecutive_paraphrase_hits = 0;
        state.paraphrase_raised = false;
        if was_raised {
            Some(SignalDelta::Clear {
                detector: DETECTOR_NAME,
                dedup_key: "loop:paraphrase".to_string(),
            })
        } else {
            None
        }
    }
}

// ---------------------------------------------------------------------------
// Clear helpers
// ---------------------------------------------------------------------------

fn push_clears_for_all_loop_signals(
    state: &mut ReasoningLoopState,
    out: &mut SmallVec<[SignalDelta; 2]>,
) {
    for key in state.exact_raised_keys.drain() {
        out.push(SignalDelta::Clear {
            detector: DETECTOR_NAME,
            dedup_key: format!("loop:exact:{}", key),
        });
    }
    state.exact_hashes.clear();
    state.exact_warned_at.clear();

    if state.compression_raised {
        out.push(SignalDelta::Clear {
            detector: DETECTOR_NAME,
            dedup_key: "loop:compression".to_string(),
        });
    }
    state.compression_raised = false;
    state.compression_warned_at = None;
    state.recent_compression_ratios.clear();
    state.compression_samples_since_last = 0;
    state.compression_buf.clear();

    if state.paraphrase_raised {
        out.push(SignalDelta::Clear {
            detector: DETECTOR_NAME,
            dedup_key: "loop:paraphrase".to_string(),
        });
    }
    state.paraphrase_raised = false;
    state.consecutive_paraphrase_hits = 0;
}

// ---------------------------------------------------------------------------
// Pure helpers
// ---------------------------------------------------------------------------

fn normalise(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut last_was_space = true; // collapses leading whitespace too
    for ch in s.chars() {
        if ch.is_whitespace() {
            if !last_was_space {
                out.push(' ');
                last_was_space = true;
            }
        } else if ch.is_ascii_punctuation() {
            // strip
            continue;
        } else {
            for low in ch.to_lowercase() {
                out.push(low);
            }
            last_was_space = false;
        }
    }
    if out.ends_with(' ') {
        out.pop();
    }
    out
}

fn shingles3<'a>(words: &'a [&'a str]) -> Vec<[&'a str; 3]> {
    if words.len() < 3 {
        return Vec::new();
    }
    let mut out = Vec::with_capacity(words.len() - 2);
    for w in words.windows(3) {
        out.push([w[0], w[1], w[2]]);
    }
    out
}

fn minhash128(shingles: &[String]) -> [u64; MINHASH_HASHES] {
    let mut sketch = [u64::MAX; MINHASH_HASHES];
    for shingle in shingles {
        for i in 0..MINHASH_HASHES {
            let mut h = XxHash64::with_seed(i as u64);
            h.write(shingle.as_bytes());
            let v = h.finish();
            if v < sketch[i] {
                sketch[i] = v;
            }
        }
    }
    sketch
}

fn jaccard128(a: &[u64; MINHASH_HASHES], b: &[u64; MINHASH_HASHES]) -> f64 {
    let mut eq = 0usize;
    for i in 0..MINHASH_HASHES {
        if a[i] == b[i] {
            eq += 1;
        }
    }
    (eq as f64) / (MINHASH_HASHES as f64)
}

fn preview(s: &str, max: usize) -> String {
    let mut out = String::new();
    for ch in s.chars().take(max) {
        out.push(ch);
    }
    if s.chars().count() > max {
        out.push_str("…");
    }
    out
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nurse::config::{NurseProfile, ProfileConfig};
    use crate::nurse::snapshot::ProviderStateSnapshot;
    use std::sync::{OnceLock, Weak};

    fn default_profile_config() -> &'static ProfileConfig {
        static CFG: OnceLock<ProfileConfig> = OnceLock::new();
        CFG.get_or_init(|| ProfileConfig::default_for(NurseProfile::Default))
    }

    fn fresh_state() -> Box<dyn Any + Send + Sync> {
        Box::new(ReasoningLoopState::default())
    }

    fn ctx_with<'a>(
        state: &'a mut dyn Any,
        provider_state: &'a ProviderStateSnapshot,
        weak: &'a Weak<crate::pi::session::PiSession>,
        now: Instant,
    ) -> DetectorContext<'a> {
        DetectorContext {
            session: weak,
            state,
            now,
            now_wall_ms: 0,
            profile: NurseProfile::Default,
            profile_config: default_profile_config(),
            provider_state,
            provider: None,
            model_id: None,
        }
    }

    fn count_raises_with_key(out: &SmallVec<[SignalDelta; 2]>, key_prefix: &str) -> usize {
        out.iter()
            .filter(|d| match d {
                SignalDelta::Raise(s) => s.dedup_key.starts_with(key_prefix),
                _ => false,
            })
            .count()
    }

    fn first_raise<'a>(out: &'a SmallVec<[SignalDelta; 2]>) -> Option<&'a Signal> {
        out.iter().find_map(|d| match d {
            SignalDelta::Raise(s) => Some(s),
            _ => None,
        })
    }

    fn first_raise_with_key<'a>(
        out: &'a SmallVec<[SignalDelta; 2]>,
        key: &str,
    ) -> Option<&'a Signal> {
        out.iter().find_map(|d| match d {
            SignalDelta::Raise(s) if s.dedup_key == key => Some(s),
            _ => None,
        })
    }

    #[test]
    fn siphash_exact_repeat_fires_after_five_in_window() {
        let d = ReasoningLoopDetector::new();
        let weak: Weak<crate::pi::session::PiSession> = Weak::new();
        let provider_state = ProviderStateSnapshot::default();
        let mut state = fresh_state();
        let now = Instant::now();
        // Must be >= siphash_min_chunk_chars (default 64) so the chunk
        // even enters the exact-repeat path.
        let payload = "Let me think about this problem carefully and step through it from the top.";
        assert!(payload.len() >= 64);

        // Default `siphash_repeats_to_fire` is 5: first four should NOT
        // raise; fifth should.
        let mut last_out = SmallVec::new();
        for i in 0..5 {
            let mut ctx = ctx_with(state.as_mut(), &provider_state, &weak, now);
            let out = d.observe(&PiEvent::ThinkingDelta(payload.into()), &mut ctx);
            if i < 4 {
                assert!(
                    count_raises_with_key(&out, "loop:exact:") == 0,
                    "raise too early at i={}",
                    i
                );
            } else {
                last_out = out;
            }
        }
        assert_eq!(count_raises_with_key(&last_out, "loop:exact:"), 1);
        let sig = first_raise(&last_out).unwrap();
        assert_eq!(sig.severity, Severity::Warn);
        assert!(sig.dedup_key.starts_with("loop:exact:"));
        // Subsequent identical hits should NOT re-raise (dedup).
        let mut ctx = ctx_with(state.as_mut(), &provider_state, &weak, now);
        let again = d.observe(&PiEvent::ThinkingDelta(payload.into()), &mut ctx);
        assert_eq!(count_raises_with_key(&again, "loop:exact:"), 0);
    }

    #[test]
    fn compression_ratio_drift_fires_after_three_consecutive_low_samples() {
        let d = ReasoningLoopDetector::new();
        let weak: Weak<crate::pi::session::PiSession> = Weak::new();
        let provider_state = ProviderStateSnapshot::default();
        let mut state = fresh_state();
        let now = Instant::now();

        // Use a highly compressible repeating string so the ratio is very low.
        let payload = "abc ".repeat(64); // 256 chars, super repetitive
        let cfg = ReasoningLoopDetectorConfig::for_profile(NurseProfile::Default);

        let mut compression_raises = 0usize;
        // Send enough events that we trigger the compression sample at least 3
        // times: sample fires every `compression_sample_every_n_events`.
        let per_sample = cfg.compression_sample_every_n_events as usize;
        let total = per_sample * (COMPRESSION_RATIO_HISTORY + 1);
        for _ in 0..total {
            let mut ctx = ctx_with(state.as_mut(), &provider_state, &weak, now);
            let out = d.observe(&PiEvent::ThinkingDelta(payload.clone()), &mut ctx);
            compression_raises += out
                .iter()
                .filter(|d| matches!(d, SignalDelta::Raise(s) if s.dedup_key == "loop:compression"))
                .count();
        }
        assert!(
            compression_raises >= 1,
            "expected at least one loop:compression raise after {} consecutive low samples",
            COMPRESSION_RATIO_HISTORY
        );
    }

    #[test]
    fn minhash_runs_only_at_message_end_boundary() {
        let d = ReasoningLoopDetector::new();
        let weak: Weak<crate::pi::session::PiSession> = Weak::new();
        let provider_state = ProviderStateSnapshot::default();
        let mut state = fresh_state();
        let now = Instant::now();

        // Build a long block of distinct words so the exact-repeat / compression
        // layers don't fire and pollute the count.
        let mut words = Vec::new();
        for i in 0..400 {
            words.push(format!("word{}", i));
        }
        let block = words.join(" ");

        // Send the whole block as 100 ThinkingDeltas. No MessageStart/End yet.
        // Also slip in MessageStart to seed an open message.
        {
            let mut ctx = ctx_with(state.as_mut(), &provider_state, &weak, now);
            let _ = d.observe(&PiEvent::MessageStart, &mut ctx);
        }
        let chunk_size = block.len() / 100 + 1;
        let mut paraphrase_raises = 0usize;
        for chunk in block.as_bytes().chunks(chunk_size) {
            let s = std::str::from_utf8(chunk).unwrap_or("").to_string();
            let mut ctx = ctx_with(state.as_mut(), &provider_state, &weak, now);
            let out = d.observe(&PiEvent::ThinkingDelta(s), &mut ctx);
            paraphrase_raises += out
                .iter()
                .filter(|d| matches!(d, SignalDelta::Raise(s) if s.dedup_key == "loop:paraphrase"))
                .count();
        }
        assert_eq!(
            paraphrase_raises, 0,
            "MinHash must not fire on ThinkingDelta — only at MessageEnd"
        );

        // First MessageEnd seeds the sketch ring but has nothing to compare
        // against, so still no paraphrase raise.
        {
            let mut ctx = ctx_with(state.as_mut(), &provider_state, &weak, now);
            let out = d.observe(&PiEvent::MessageEnd, &mut ctx);
            assert_eq!(
                count_raises_with_key(&out, "loop:paraphrase"),
                0,
                "first MessageEnd has no prior sketch to match"
            );
        }

        // Now send the SAME block again as another message — MessageEnd should
        // fire `loop:paraphrase` at Info (first hit calibration).
        {
            let mut ctx = ctx_with(state.as_mut(), &provider_state, &weak, now);
            let _ = d.observe(&PiEvent::MessageStart, &mut ctx);
        }
        for chunk in block.as_bytes().chunks(chunk_size) {
            let s = std::str::from_utf8(chunk).unwrap_or("").to_string();
            let mut ctx = ctx_with(state.as_mut(), &provider_state, &weak, now);
            let _ = d.observe(&PiEvent::ThinkingDelta(s), &mut ctx);
        }
        let mut ctx = ctx_with(state.as_mut(), &provider_state, &weak, now);
        let out = d.observe(&PiEvent::MessageEnd, &mut ctx);
        let raised = first_raise_with_key(&out, "loop:paraphrase").expect("paraphrase raise");
        assert_eq!(raised.severity, Severity::Info);
    }

    #[test]
    fn minhash_skipped_for_short_blocks() {
        let d = ReasoningLoopDetector::new();
        let weak: Weak<crate::pi::session::PiSession> = Weak::new();
        let provider_state = ProviderStateSnapshot::default();
        let mut state = fresh_state();
        let now = Instant::now();
        let short = "tiny block of thinking text"; // < 512 chars

        {
            let mut ctx = ctx_with(state.as_mut(), &provider_state, &weak, now);
            let _ = d.observe(&PiEvent::MessageStart, &mut ctx);
        }
        {
            let mut ctx = ctx_with(state.as_mut(), &provider_state, &weak, now);
            let _ = d.observe(&PiEvent::ThinkingDelta(short.into()), &mut ctx);
        }
        // Send the SAME short block again so any 100%-overlap MinHash would
        // definitely fire — proves the skip is the only thing keeping it quiet.
        {
            let mut ctx = ctx_with(state.as_mut(), &provider_state, &weak, now);
            let out = d.observe(&PiEvent::MessageEnd, &mut ctx);
            assert_eq!(count_raises_with_key(&out, "loop:paraphrase"), 0);
        }
        {
            let mut ctx = ctx_with(state.as_mut(), &provider_state, &weak, now);
            let _ = d.observe(&PiEvent::MessageStart, &mut ctx);
            let _ = d.observe(&PiEvent::ThinkingDelta(short.into()), &mut ctx);
            let out = d.observe(&PiEvent::MessageEnd, &mut ctx);
            assert_eq!(count_raises_with_key(&out, "loop:paraphrase"), 0);
        }
    }

    /// Replay a long block of thinking content across MessageStart..MessageEnd
    /// and return the resulting deltas from the closing MessageEnd.
    fn replay_block_and_close(
        d: &ReasoningLoopDetector,
        state: &mut Box<dyn Any + Send + Sync>,
        provider_state: &ProviderStateSnapshot,
        weak: &Weak<crate::pi::session::PiSession>,
        now: Instant,
        block: &str,
    ) -> SmallVec<[SignalDelta; 2]> {
        {
            let mut ctx = ctx_with(state.as_mut(), provider_state, weak, now);
            let _ = d.observe(&PiEvent::MessageStart, &mut ctx);
        }
        let chunk_size = (block.len() / 20).max(1);
        for chunk in block.as_bytes().chunks(chunk_size) {
            let s = std::str::from_utf8(chunk).unwrap_or("").to_string();
            let mut ctx = ctx_with(state.as_mut(), provider_state, weak, now);
            let _ = d.observe(&PiEvent::ThinkingDelta(s), &mut ctx);
        }
        let mut ctx = ctx_with(state.as_mut(), provider_state, weak, now);
        d.observe(&PiEvent::MessageEnd, &mut ctx)
    }

    fn long_block(prefix: &str) -> String {
        let mut words = Vec::new();
        for i in 0..400 {
            words.push(format!("{}-w{}", prefix, i));
        }
        words.join(" ")
    }

    #[test]
    fn paraphrase_first_hit_is_info_only_then_warn_then_stalled() {
        let d = ReasoningLoopDetector::new();
        let weak: Weak<crate::pi::session::PiSession> = Weak::new();
        let provider_state = ProviderStateSnapshot::default();
        let mut state = fresh_state();
        let now = Instant::now();
        let block = long_block("a");

        // Seed sketch ring.
        let out = replay_block_and_close(&d, &mut state, &provider_state, &weak, now, &block);
        assert!(first_raise_with_key(&out, "loop:paraphrase").is_none());

        // First paraphrase hit -> Info.
        let out = replay_block_and_close(&d, &mut state, &provider_state, &weak, now, &block);
        let s = first_raise_with_key(&out, "loop:paraphrase").expect("info raise");
        assert_eq!(s.severity, Severity::Info);

        // Second consecutive hit -> Warn.
        let out = replay_block_and_close(&d, &mut state, &provider_state, &weak, now, &block);
        let s = first_raise_with_key(&out, "loop:paraphrase").expect("warn raise");
        assert_eq!(s.severity, Severity::Warn);

        // Third -> Stalled.
        let out = replay_block_and_close(&d, &mut state, &provider_state, &weak, now, &block);
        let s = first_raise_with_key(&out, "loop:paraphrase").expect("stalled raise");
        assert_eq!(s.severity, Severity::Stalled);
    }

    #[test]
    fn paraphrase_counter_resets_on_non_match() {
        let d = ReasoningLoopDetector::new();
        let weak: Weak<crate::pi::session::PiSession> = Weak::new();
        let provider_state = ProviderStateSnapshot::default();
        let mut state = fresh_state();
        let now = Instant::now();
        let a = long_block("alpha");
        let b = long_block("bravo");

        // Seed.
        let _ = replay_block_and_close(&d, &mut state, &provider_state, &weak, now, &a);
        // First match -> Info.
        let out = replay_block_and_close(&d, &mut state, &provider_state, &weak, now, &a);
        assert_eq!(
            first_raise_with_key(&out, "loop:paraphrase")
                .unwrap()
                .severity,
            Severity::Info
        );
        // Second match -> Warn.
        let out = replay_block_and_close(&d, &mut state, &provider_state, &weak, now, &a);
        assert_eq!(
            first_raise_with_key(&out, "loop:paraphrase")
                .unwrap()
                .severity,
            Severity::Warn
        );

        // Non-match resets and emits a clear.
        let out = replay_block_and_close(&d, &mut state, &provider_state, &weak, now, &b);
        assert!(out.iter().any(|d| matches!(
            d,
            SignalDelta::Clear { detector: DETECTOR_NAME, dedup_key } if dedup_key == "loop:paraphrase"
        )));

        // Following match starts back at Info.
        let out = replay_block_and_close(&d, &mut state, &provider_state, &weak, now, &b);
        assert_eq!(
            first_raise_with_key(&out, "loop:paraphrase")
                .unwrap()
                .severity,
            Severity::Info
        );
    }

    #[test]
    fn clears_on_tool_call_start() {
        let d = ReasoningLoopDetector::new();
        let weak: Weak<crate::pi::session::PiSession> = Weak::new();
        let provider_state = ProviderStateSnapshot::default();
        let mut state = fresh_state();
        let now = Instant::now();
        // Must be >= siphash_min_chunk_chars (default 64) so the chunk
        // even enters the exact-repeat path.
        let payload = "again and again and again and again and again and again and again.";
        assert!(payload.len() >= 64);

        // Drive an exact-repeat raise first (default repeats_to_fire = 5).
        for _ in 0..5 {
            let mut ctx = ctx_with(state.as_mut(), &provider_state, &weak, now);
            let _ = d.observe(&PiEvent::ThinkingDelta(payload.into()), &mut ctx);
        }
        // Sanity: state should have an open raise.
        {
            let s = state.as_mut().downcast_mut::<ReasoningLoopState>().unwrap();
            assert!(!s.exact_raised_keys.is_empty(), "expected an exact raise");
        }

        // Tool start clears all loop:* signals.
        let mut ctx = ctx_with(state.as_mut(), &provider_state, &weak, now);
        let out = d.observe(
            &PiEvent::ToolExecutionStart {
                tool_call_id: "tc1".into(),
                name: "read".into(),
                args: serde_json::Value::Null,
            },
            &mut ctx,
        );
        assert!(out.iter().any(|d| matches!(d, SignalDelta::Clear { .. })));
        let s = state.as_mut().downcast_mut::<ReasoningLoopState>().unwrap();
        assert!(s.exact_raised_keys.is_empty());
        assert!(!s.compression_raised);
        assert!(!s.paraphrase_raised);
    }

    #[test]
    fn clears_on_long_textdelta_above_200_chars() {
        let d = ReasoningLoopDetector::new();
        let weak: Weak<crate::pi::session::PiSession> = Weak::new();
        let provider_state = ProviderStateSnapshot::default();
        let mut state = fresh_state();
        let now = Instant::now();
        // Must be >= siphash_min_chunk_chars (default 64) so the chunk
        // even enters the exact-repeat path.
        let payload = "stuck thinking chunk that keeps coming back around in the same exact words.";
        assert!(payload.len() >= 64);

        // Drive an exact-repeat raise first (default repeats_to_fire = 5).
        for _ in 0..5 {
            let mut ctx = ctx_with(state.as_mut(), &provider_state, &weak, now);
            let _ = d.observe(&PiEvent::ThinkingDelta(payload.into()), &mut ctx);
        }

        // 250-char text delta should clear.
        let long_text = "x".repeat(250);
        let mut ctx = ctx_with(state.as_mut(), &provider_state, &weak, now);
        let out = d.observe(&PiEvent::TextDelta(long_text), &mut ctx);
        assert!(out.iter().any(|d| matches!(d, SignalDelta::Clear { .. })));

        // Short text delta must NOT clear.
        // Set up another raise first.
        for _ in 0..5 {
            let mut ctx = ctx_with(state.as_mut(), &provider_state, &weak, now);
            let _ = d.observe(&PiEvent::ThinkingDelta(payload.into()), &mut ctx);
        }
        let short_text = "hi".to_string();
        let mut ctx = ctx_with(state.as_mut(), &provider_state, &weak, now);
        let out = d.observe(&PiEvent::TextDelta(short_text), &mut ctx);
        assert!(!out.iter().any(|d| matches!(d, SignalDelta::Clear { .. })));
    }

    // ----- pure-helper coverage ---------------------------------------------

    #[test]
    fn normalise_lowercases_collapses_ws_strips_punct() {
        assert_eq!(normalise("  Hello,  WORLD!\n"), "hello world");
        assert_eq!(normalise("a\t b   c"), "a b c");
        assert_eq!(normalise(",,,"), "");
    }

    #[test]
    fn shingles3_basic() {
        let words = vec!["a", "b", "c", "d"];
        let out = shingles3(&words);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0], ["a", "b", "c"]);
        assert_eq!(out[1], ["b", "c", "d"]);

        let words = vec!["a", "b"];
        assert!(shingles3(&words).is_empty());
    }

    #[test]
    fn jaccard_is_one_for_identical_and_low_for_different() {
        let a = vec!["foo bar baz".to_string(), "bar baz qux".to_string()];
        let b = vec!["foo bar baz".to_string(), "bar baz qux".to_string()];
        let c = vec![
            "totally different content".to_string(),
            "unrelated other words".to_string(),
        ];
        let sa = minhash128(&a);
        let sb = minhash128(&b);
        let sc = minhash128(&c);
        assert!((jaccard128(&sa, &sb) - 1.0).abs() < 1e-9);
        assert!(jaccard128(&sa, &sc) < 0.5);
    }

    #[test]
    fn config_schema_has_units_and_direction() {
        let d = ReasoningLoopDetector::new();
        let schema = d.config_schema();
        assert!(!schema.is_empty());
        for entry in &schema {
            if matches!(
                entry.kind,
                crate::nurse::snapshot::TunableKind::Stepper
                    | crate::nurse::snapshot::TunableKind::NumericRange
            ) {
                assert!(!entry.unit.is_empty(), "missing unit for {}", entry.name);
            }
            assert!(
                !entry.description.is_empty(),
                "missing description for {}",
                entry.name
            );
        }
    }
}
