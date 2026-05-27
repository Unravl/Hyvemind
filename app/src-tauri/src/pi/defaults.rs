//! Sensible defaults for the bundled Pi extensions.
//!
//! Pi's `pi-web-access` extension defaults its `web_search` tool to the
//! `summary-review` workflow, which spins up an ephemeral HTTP server and
//! pops a browser window asking the user to review/approve search results
//! before they are sent back to the agent. That works for an interactive
//! TUI, but inside Hyvemind's headless RPC sessions it hangs the model
//! call indefinitely waiting on input no one is providing. Seed
//! `~/.pi/web-search.json` with `workflow: "none"` on startup so the tool
//! returns raw results unattended. Users can still opt back in by editing
//! the file (we only insert the key when it's missing).

use std::fs;
use std::io;
use std::path::Path;

use serde_json::{json, Value};

/// Outcome of a defaults-seeding pass. Surfaced via logs only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SeedAction {
    Created,
    Patched,
    AlreadySet,
    SkippedMalformed,
    SkippedNoHome,
}

/// Ensure `~/.pi/web-search.json` exists with `workflow: "none"` so
/// `web_search` tool calls don't block waiting on the curator UI.
///
/// Best-effort: any I/O failure is logged and swallowed — defaults are a
/// nicety, not a hard requirement.
pub fn ensure_web_search_workflow_default() -> SeedAction {
    let Some(home) = dirs::home_dir() else {
        tracing::warn!("pi defaults: home dir unavailable, skipping web-search seed");
        return SeedAction::SkippedNoHome;
    };
    seed_at(&home.join(".pi").join("web-search.json"))
}

fn seed_at(path: &Path) -> SeedAction {
    if let Some(parent) = path.parent() {
        if let Err(err) = fs::create_dir_all(parent) {
            tracing::warn!(
                error = %err,
                path = %parent.display(),
                "pi defaults: failed to create config dir, skipping"
            );
            return SeedAction::SkippedMalformed;
        }
    }

    match fs::read_to_string(path) {
        Ok(existing) => patch_existing(path, &existing),
        Err(err) if err.kind() == io::ErrorKind::NotFound => create_default(path),
        Err(err) => {
            tracing::warn!(
                error = %err,
                path = %path.display(),
                "pi defaults: failed to read web-search.json, skipping"
            );
            SeedAction::SkippedMalformed
        }
    }
}

fn create_default(path: &Path) -> SeedAction {
    let body = json!({ "workflow": "none" });
    if let Err(err) = write_pretty(path, &body) {
        tracing::warn!(
            error = %err,
            path = %path.display(),
            "pi defaults: failed to write web-search.json"
        );
        return SeedAction::SkippedMalformed;
    }
    tracing::info!(
        path = %path.display(),
        "pi defaults: seeded web-search.json with workflow=\"none\""
    );
    SeedAction::Created
}

fn patch_existing(path: &Path, existing: &str) -> SeedAction {
    let mut value: Value = match serde_json::from_str(existing) {
        Ok(v) => v,
        Err(err) => {
            tracing::warn!(
                error = %err,
                path = %path.display(),
                "pi defaults: web-search.json is not valid JSON, leaving it alone"
            );
            return SeedAction::SkippedMalformed;
        }
    };
    let Some(obj) = value.as_object_mut() else {
        tracing::warn!(
            path = %path.display(),
            "pi defaults: web-search.json is not a JSON object, leaving it alone"
        );
        return SeedAction::SkippedMalformed;
    };
    if obj.contains_key("workflow") {
        return SeedAction::AlreadySet;
    }
    obj.insert("workflow".to_string(), Value::String("none".to_string()));
    if let Err(err) = write_pretty(path, &value) {
        tracing::warn!(
            error = %err,
            path = %path.display(),
            "pi defaults: failed to patch web-search.json"
        );
        return SeedAction::SkippedMalformed;
    }
    tracing::info!(
        path = %path.display(),
        "pi defaults: added workflow=\"none\" to existing web-search.json"
    );
    SeedAction::Patched
}

fn write_pretty(path: &Path, value: &Value) -> io::Result<()> {
    let mut body =
        serde_json::to_string_pretty(value).map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;
    body.push('\n');
    fs::write(path, body)
}

#[allow(dead_code)]
pub(crate) fn seed_at_for_test(path: &Path) -> SeedAction {
    seed_at(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use tempfile::tempdir;

    fn read(path: &PathBuf) -> Value {
        serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap()
    }

    #[test]
    fn creates_default_when_missing() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("nested").join("web-search.json");
        assert_eq!(seed_at(&path), SeedAction::Created);
        assert_eq!(read(&path), json!({ "workflow": "none" }));
    }

    #[test]
    fn patches_existing_without_workflow() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("web-search.json");
        fs::write(&path, r#"{ "exaApiKey": "exa-xyz" }"#).unwrap();
        assert_eq!(seed_at(&path), SeedAction::Patched);
        let v = read(&path);
        assert_eq!(v["workflow"], json!("none"));
        assert_eq!(v["exaApiKey"], json!("exa-xyz"));
    }

    #[test]
    fn respects_existing_workflow_choice() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("web-search.json");
        fs::write(&path, r#"{ "workflow": "summary-review" }"#).unwrap();
        assert_eq!(seed_at(&path), SeedAction::AlreadySet);
        assert_eq!(read(&path)["workflow"], json!("summary-review"));
    }

    #[test]
    fn leaves_malformed_alone() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("web-search.json");
        let original = "this is not json {{";
        fs::write(&path, original).unwrap();
        assert_eq!(seed_at(&path), SeedAction::SkippedMalformed);
        assert_eq!(fs::read_to_string(&path).unwrap(), original);
    }

    #[test]
    fn leaves_non_object_json_alone() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("web-search.json");
        fs::write(&path, "[1, 2, 3]").unwrap();
        assert_eq!(seed_at(&path), SeedAction::SkippedMalformed);
        assert_eq!(fs::read_to_string(&path).unwrap(), "[1, 2, 3]");
    }
}
