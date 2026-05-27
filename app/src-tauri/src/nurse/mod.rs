//! Nurse subsystem — push-driven, detector-based session health monitor.
//!
//! See `nurse/README.md` for the architecture deep-dive (added in step 9
//! of the rewrite plan).
//!
//! Subscribes to a [`NurseBus`](bus::NurseBus) fed by `pi/session.rs`
//! and `pi/manager.rs`, runs per-session detectors that emit structured
//! [`SignalDelta`](detector::SignalDelta)s into a per-session
//! [`SessionHealth`](health::SessionHealth), and dispatches interventions
//! through a three-tier pipeline (Deterministic / Templated Steer / LLM
//! classifier).
//!
//! Public surface re-exported here is the bare minimum every caller
//! outside the module needs.

pub mod batch_review;
pub mod budget;
pub mod bus;
pub mod classifier;
pub mod config;
pub mod detector;
pub mod detectors;
pub mod dispatcher;
pub mod engine;
pub mod health;
pub mod intervention;
pub mod intervention_writer;
pub mod observability;
pub mod playbook;
pub mod prompt;
pub mod schema;
pub mod snapshot;
pub mod storm_guard;
pub mod supervisor;
pub mod synthesized;

pub use bus::{NurseBus, NurseBusEvent, SessionEndReason};
pub use config::{NurseConfig, NurseMode, NurseProfile, ProfileConfig};
pub use engine::NurseEngine;
pub use snapshot::{
    BatchTickSnapshotDto, NurseActionKind, NurseDecision, NurseDecisionDto, NurseDispatchTier,
    NurseEvent, NurseInterventionRecord, NurseLifecyclePayload, NurseLifecycleStatus,
    NurseServiceConfigSnapshot, NurseStatusSnapshot, SessionOwnerDto,
};
pub use synthesized::{describe_synthesized, InterventionOwner, SynthesizedKind};
