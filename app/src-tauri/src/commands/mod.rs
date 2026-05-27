//! `commands/` only hosts `#[tauri::command]` adapters that delegate to
//! other modules; do not put business logic here. Domain types and the
//! functions they call should live next to the subsystem that owns the
//! data — `core/`, `hivemind/`, `pi/`, `state/`, etc. — so the `core/`
//! and `state/` layers never need to import back into `commands/`.

pub mod chat;
pub mod dashboard;
pub mod extensions;
pub mod hivemind;
pub mod nurse;
pub mod sessions;
pub mod settings;
pub mod swarms;
pub mod tasks;
pub mod tests;
pub mod util;
