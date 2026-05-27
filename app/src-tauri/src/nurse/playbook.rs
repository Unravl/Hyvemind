//! Tier-2 Templated Steer playbook.
//!
//! Hand-curated table mapping a `(detector_name, dedup_key_prefix)` pair
//! to a canned `PlaybookAction` (Steer or LeaveIt). Consulted between the
//! storm-guard / budget gates and the Tier-3 LLM classifier. Matching is
//! `dedup_key.starts_with(playbook_key)` so families of related signatures
//! map to one entry.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PlaybookAction {
    Steer { message: String },
    LeaveIt { check_back_secs: u64 },
}

#[derive(Debug, Clone)]
pub struct PlaybookEntry {
    pub detector: &'static str,
    pub key_prefix: &'static str,
    pub action: PlaybookAction,
    pub rationale: &'static str,
}

/// Process-wide Tier-2 playbook.
#[derive(Debug)]
pub struct SteerPlaybook {
    entries: Vec<PlaybookEntry>,
}

impl SteerPlaybook {
    /// Empty playbook — useful in tests and for the dark-mode boot during
    /// step 3 of the migration before step 5 wires in real entries.
    pub fn empty() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    /// Default seed for production use. Step 5 of the migration loads this.
    pub fn seeded() -> Self {
        Self {
            entries: vec![
                PlaybookEntry {
                    detector: "context_saturation",
                    key_prefix: "ctx:critical",
                    action: PlaybookAction::Steer {
                        message: "Your context window is nearly exhausted. \
                                  Wrap up the current step and call \
                                  submit_handoff (or your role's terminal tool) now."
                            .to_string(),
                    },
                    rationale: "context near-full → submit handoff before respawn",
                },
                PlaybookEntry {
                    detector: "provider_health",
                    key_prefix: "breaker:",
                    action: PlaybookAction::LeaveIt {
                        check_back_secs: 30,
                    },
                    rationale: "provider breaker open → wait, do not steer",
                },
                PlaybookEntry {
                    detector: "tool_failure",
                    key_prefix: "tool_stuck:bash:permission_denied",
                    action: PlaybookAction::Steer {
                        message: "You don't have permission to run that bash \
                                  command. Try a different approach — read the \
                                  file directly, ask the user for elevated \
                                  permissions, or work around the restricted \
                                  resource."
                            .to_string(),
                    },
                    rationale: "permission-denied bash loop → suggest alternative",
                },
                PlaybookEntry {
                    detector: "retry_exhaustion",
                    key_prefix: "retry:exhausted",
                    action: PlaybookAction::Steer {
                        message: "Your model's automatic retry budget is exhausted. \
                                  Switch approach — split the task into smaller \
                                  steps, simplify the prompt, or try a different \
                                  tool path."
                            .to_string(),
                    },
                    rationale: "retry exhaustion → suggest decomposition",
                },
                PlaybookEntry {
                    detector: "reasoning_loop",
                    key_prefix: "loop:exact:",
                    action: PlaybookAction::Steer {
                        message: "You appear to be repeating the same line of \
                                  thinking. Try a different angle — restate the \
                                  problem in your own words, list what you've \
                                  already tried, or ask a clarifying question."
                            .to_string(),
                    },
                    rationale: "exact-thinking-loop → suggest angle change",
                },
            ],
        }
    }

    /// First-match lookup. Matching is `dedup_key.starts_with(key_prefix)`.
    pub fn lookup(&self, detector: &str, dedup_key: &str) -> Option<&PlaybookEntry> {
        self.entries
            .iter()
            .find(|e| e.detector == detector && dedup_key.starts_with(e.key_prefix))
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

impl Default for SteerPlaybook {
    fn default() -> Self {
        Self::seeded()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starts_with_matching_picks_first_entry() {
        let pb = SteerPlaybook::seeded();
        let m = pb
            .lookup("tool_failure", "tool_stuck:bash:permission_denied:abc")
            .expect("expected match");
        assert_eq!(m.detector, "tool_failure");
    }

    #[test]
    fn unknown_key_returns_none() {
        let pb = SteerPlaybook::seeded();
        assert!(pb.lookup("unknown_detector", "anything").is_none());
        assert!(pb.lookup("reasoning_loop", "loop:compression").is_none());
    }

    #[test]
    fn empty_playbook_matches_nothing() {
        let pb = SteerPlaybook::empty();
        assert!(pb.is_empty());
        assert!(pb.lookup("any", "any").is_none());
    }
}
