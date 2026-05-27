//! Parser for `services.yaml`, the per-swarm command + service registry.
//!
//! `services.yaml` lives at `~/.hyvemind/swarms/<id>/services.yaml` and gives
//! Workers (and future Guard / readiness checks) a single source of truth for
//! the shell commands that drive a project (install, typecheck, test, build,
//! dev) as well as any long-running ambient services (databases, queues, etc.)
//! the swarm should be aware of.
//!
//! All fields are optional and degrade gracefully:
//! - missing file -> caller treats as "no context"
//! - missing `commands` map -> empty HashMap
//! - missing `services` list -> empty Vec
//! - missing optional service fields -> `None`
//!
//! See `prompts/templates/services.yaml.tmpl` for an annotated example.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// The parsed `services.yaml` file.
///
/// Both top-level fields default to empty when absent so that a stub file
/// containing only `commands:` (or only `services:`) parses cleanly.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ServicesFile {
    /// Named shell commands, e.g. `install`, `test`, `typecheck`, `build`,
    /// `dev`. The key is the logical name; the value is the literal shell
    /// string to execute.
    #[serde(default)]
    pub commands: HashMap<String, String>,

    /// Ambient services the project depends on (databases, queues, etc.).
    #[serde(default)]
    pub services: Vec<Service>,
}

/// A single ambient service definition.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Service {
    /// Human-readable service name (e.g. `postgres`, `redis`).
    pub name: String,

    /// Optional TCP port the service listens on locally.
    #[serde(default)]
    pub port: Option<u16>,

    /// Optional host the service is reachable at (defaults to `localhost`
    /// when omitted by the caller's reading code).
    #[serde(default)]
    pub host: Option<String>,

    /// Optional shell command to start the service.
    #[serde(default)]
    pub start: Option<String>,

    /// Optional shell command to stop the service.
    #[serde(default)]
    pub stop: Option<String>,

    /// Optional shell command that exits 0 when the service is healthy.
    #[serde(default)]
    pub healthcheck: Option<String>,
}

/// Parse a `services.yaml` string into a `ServicesFile`.
///
/// An empty input string is treated as an empty (but valid) file and returns
/// a `ServicesFile` with empty `commands` and `services`.
pub fn parse_services_yaml(text: &str) -> Result<ServicesFile> {
    // serde_yaml errors on a completely empty document; short-circuit to a
    // default so callers can rely on Ok(Default::default()) for blank files.
    if text.trim().is_empty() {
        return Ok(ServicesFile::default());
    }
    let parsed: ServicesFile =
        serde_yaml::from_str(text).context("failed to parse services.yaml")?;
    Ok(parsed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_full_services_yaml() {
        let yaml = r#"
commands:
  install: "npm install"
  typecheck: "npm run typecheck"
  test: "npm test"
  build: "npm run build"
  dev: "npm run dev"
services:
  - name: postgres
    port: 5432
    host: localhost
    start: "docker compose up -d postgres"
    stop: "docker compose stop postgres"
    healthcheck: "pg_isready -h localhost"
"#;
        let parsed = parse_services_yaml(yaml).expect("parse");
        assert_eq!(parsed.commands.len(), 5);
        assert_eq!(
            parsed.commands.get("install").map(String::as_str),
            Some("npm install")
        );
        assert_eq!(
            parsed.commands.get("test").map(String::as_str),
            Some("npm test")
        );
        assert_eq!(parsed.services.len(), 1);
        let pg = &parsed.services[0];
        assert_eq!(pg.name, "postgres");
        assert_eq!(pg.port, Some(5432));
        assert_eq!(pg.host.as_deref(), Some("localhost"));
        assert_eq!(pg.start.as_deref(), Some("docker compose up -d postgres"));
        assert_eq!(pg.stop.as_deref(), Some("docker compose stop postgres"));
        assert_eq!(pg.healthcheck.as_deref(), Some("pg_isready -h localhost"));
    }

    #[test]
    fn test_parse_only_commands() {
        let yaml = r#"
commands:
  test: "cargo test"
"#;
        let parsed = parse_services_yaml(yaml).expect("parse");
        assert_eq!(parsed.commands.len(), 1);
        assert_eq!(
            parsed.commands.get("test").map(String::as_str),
            Some("cargo test")
        );
        assert!(parsed.services.is_empty());
    }

    #[test]
    fn test_parse_only_services() {
        let yaml = r#"
services:
  - name: redis
    port: 6379
"#;
        let parsed = parse_services_yaml(yaml).expect("parse");
        assert!(parsed.commands.is_empty());
        assert_eq!(parsed.services.len(), 1);
        assert_eq!(parsed.services[0].name, "redis");
        assert_eq!(parsed.services[0].port, Some(6379));
        assert!(parsed.services[0].start.is_none());
    }

    #[test]
    fn test_parse_empty_yaml() {
        let parsed = parse_services_yaml("").expect("parse empty");
        assert!(parsed.commands.is_empty());
        assert!(parsed.services.is_empty());

        let parsed_ws = parse_services_yaml("   \n  \n").expect("parse whitespace");
        assert!(parsed_ws.commands.is_empty());
        assert!(parsed_ws.services.is_empty());
    }

    #[test]
    fn test_parse_yaml_with_comments_only() {
        let yaml = "# just a comment\n# nothing else here\n";
        let parsed = parse_services_yaml(yaml).expect("parse comments-only");
        assert!(parsed.commands.is_empty());
        assert!(parsed.services.is_empty());
    }

    #[test]
    fn test_parse_malformed_yaml() {
        // Unbalanced quotes / bad indentation -> error
        let yaml = "commands:\n  install: \"unterminated\n  test: bad";
        let result = parse_services_yaml(yaml);
        assert!(result.is_err(), "expected malformed yaml to error");
    }

    #[test]
    fn test_parse_service_missing_name_errors() {
        // `name` is required on Service.
        let yaml = r#"
services:
  - port: 1234
"#;
        let result = parse_services_yaml(yaml);
        assert!(result.is_err(), "expected missing `name` field to error");
    }

    #[test]
    fn test_services_file_default_is_empty() {
        let sf = ServicesFile::default();
        assert!(sf.commands.is_empty());
        assert!(sf.services.is_empty());
    }
}
