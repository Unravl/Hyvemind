//! Pure data-type definitions shared across the `core` and `state` layers.
//!
//! The `domain` module sits **below** both `core` and `state` in the
//! dependency graph. It contains only data structures, their inherent
//! impls, serde wiring, and small helpers — no I/O, no agent loops, no
//! references to `pi::*`, `state::*`, or `core::queen`/`scout`/`worker`.
//!
//! Splitting these types out breaks what used to be a `core` ↔ `state`
//! module cycle (where `state/store.rs` imported `core::swarm::*` while
//! `core/queen.rs` imported `state::store::SwarmStore`).
//!
//! New domain types should be added here whenever they are referenced by
//! both layers.

pub mod swarm;
