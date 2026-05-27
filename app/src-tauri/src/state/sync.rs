//! Lock type aliases for disambiguating sync vs async mutexes.
//!
//! Use `SyncMutex` / `SyncRwLock` (`std::sync::*`) **only** when the guard
//! never crosses an `.await`. The guard must be dropped before any await
//! point, otherwise the runtime can stall (a sync guard held across an
//! await blocks the executor thread and can deadlock the scheduler).
//!
//! Use `AsyncMutex` / `AsyncRwLock` (`tokio::sync::*`) when the guard must
//! persist across awaits, or when you need fairness / cancellation-safe
//! semantics. These are slower in the uncontended fast path but safe to
//! hold across `.await`.
//!
//! ## When to pick which
//!
//! - **`SyncMutex` / `SyncRwLock`** — pure in-memory data structure
//!   mutation (push to a Vec, insert into a HashMap, read a counter), all
//!   inside one synchronous critical section. The std locks are cheaper
//!   and play nice with `FnMut` callbacks that aren't async.
//! - **`AsyncMutex` / `AsyncRwLock`** — anything where the critical
//!   section performs I/O, calls an async function, or needs to live
//!   across multiple `.await` points. Also pick async when many callers
//!   may contend and you want tokio's fair-ish queueing.
//!
//! Prefer aliases over the fully-qualified paths in new code so reviewers
//! can tell at a glance which discipline applies.

pub type SyncMutex<T> = std::sync::Mutex<T>;
pub type SyncRwLock<T> = std::sync::RwLock<T>;
pub type AsyncMutex<T> = tokio::sync::Mutex<T>;
pub type AsyncRwLock<T> = tokio::sync::RwLock<T>;
