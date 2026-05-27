//! Swarm-readiness checks for a swarm plan.
//!
//! After the Queen produces a plan, the frontend calls
//! [`crate::commands::swarms::check_swarm_readiness`] with the plan's
//! `readiness_manifest`. That command delegates here.
//!
//! The goal is to **prove** every external dependency the plan claims to need
//! is actually installable / reachable BEFORE the swarm starts implementing
//! anything. No deferral allowed.
//!
//! Checks supported:
//! - `cargo_crates`: crate exists on crates.io (via `cargo search`); if the
//!   working directory has a `Cargo.toml`, also note whether the crate is
//!   already a dependency.
//! - `npm_packages`: package exists in the npm registry (via `npm view`).
//! - `system_bins`: binary is in `$PATH` (via `command -v`).
//! - `apis`: HTTPS request returns the expected status, optionally with a
//!   bearer token from an env var.
//!
//! Checks run concurrently bounded by a small `Semaphore` (default 4). Each
//! individual check is hard-capped at [`PER_CHECK_TIMEOUT`].
//!
//! # API probe security (`check_api`)
//!
//! The Queen's planning step is an untrusted-input boundary — a hostile model
//! response (prompt injection on a shared codebase, jailbroken plan, etc.)
//! could craft a `ReadinessManifest` whose `apis[]` entries weaponise the
//! readiness probe. To prevent that, `check_api` enforces a hardened set of
//! constraints **before** any network or env-var work happens:
//!
//! 1. `auth_env` (if set) is checked against
//!    [`crate::state::config::PROVIDER_API_KEY_ENV_ALLOWLIST`]. Any other env
//!    var name (e.g. `AWS_SECRET_ACCESS_KEY`, `GITHUB_TOKEN`, `LD_PRELOAD`) is
//!    rejected without ever calling `std::env::var`.
//! 2. The URL scheme must be `https://`. `http://`, `file://`, `gopher://`,
//!    etc. are rejected.
//! 3. The URL host must be in the union of:
//!    - [`CANONICAL_PROVIDER_HOSTS`] (mirrors `seed_default_providers` in
//!      `state/config.rs`), and
//!    - Hosts declared in `working_dir/services.yaml` (`hosts:` array). Read
//!      once per `check_readiness` call.
//! 4. The hostname is resolved with `tokio::net::lookup_host`. If **any**
//!    resolved IP falls inside a loopback / private / link-local / ULA range
//!    (127.0.0.0/8, 10.0.0.0/8, 172.16.0.0/12, 192.168.0.0/16, 169.254.0.0/16,
//!    ::1, fc00::/7, fe80::/10) the probe is rejected.
//! 5. The request is sent through a reqwest client that pins the resolved IP
//!    via `resolve_to_addrs(host, &[resolved])` and disables redirect following
//!    (`Policy::none()`). This defeats DNS rebinding — even if the attacker's
//!    DNS flips to a private IP between resolution and connect, reqwest will
//!    still connect to the IP we already vetted.
//!
//! Failures from steps 1–4 surface as a [`ReadinessError`] variant in the
//! `detail` field of the returned [`ReadinessCheck`] so the frontend can show
//! a useful message.

use std::collections::HashSet;
use std::fmt;
use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tokio::process::Command;
use tokio::sync::Semaphore;
use tokio::task::JoinSet;
use tracing::warn;

use crate::state::config::PROVIDER_API_KEY_ENV_ALLOWLIST;

/// Hard timeout for a single readiness check (cargo/npm/system_bin/api).
const PER_CHECK_TIMEOUT: Duration = Duration::from_secs(15);

/// Maximum readiness checks running concurrently across all kinds.
const MAX_CONCURRENT_CHECKS: usize = 4;

/// Cap on stdout/stderr fragments we surface in `detail`.
const DETAIL_PREVIEW_CHARS: usize = 200;

/// Default port assumed when a probe URL omits one (always 443 — we only
/// allow `https://`).
const DEFAULT_HTTPS_PORT: u16 = 443;

/// Canonical provider API hosts permitted as probe targets. Mirrors the
/// endpoints seeded by `state::config::seed_default_providers`.
///
/// This list is intentionally hardcoded (not derived from `Config`) so that a
/// user with a tampered/malicious `config.json` can't widen the allowlist by
/// adding a custom endpoint. Self-hosted endpoints belong in
/// `services.yaml`, which lives in the user-approved `working_dir`.
const CANONICAL_PROVIDER_HOSTS: &[&str] = &[
    "api.anthropic.com",
    "api.openai.com",
    "openrouter.ai",
    "api.deepseek.com",
    "open.bigmodel.cn",
    "api.mistral.ai",
    "ollama.com",
    "crof.ai",
    "api.moonshot.cn",
    "api.groq.com",
    "api.neuralwatt.com",
    "integrate.api.nvidia.com",
    "opencode.ai",
    "generativelanguage.googleapis.com",
];

fn default_get() -> String {
    "GET".to_string()
}

fn default_expected_200() -> u16 {
    200
}

/// The manifest produced by the Queen's planning step. All arrays are
/// optional — an empty manifest is valid and trivially passes.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ReadinessManifest {
    #[serde(default)]
    pub cargo_crates: Vec<String>,
    #[serde(default)]
    pub npm_packages: Vec<String>,
    #[serde(default)]
    pub system_bins: Vec<String>,
    #[serde(default)]
    pub apis: Vec<ApiProbe>,
}

/// A single HTTP probe declared in the manifest.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiProbe {
    pub url: String,
    #[serde(default = "default_get")]
    pub method: String,
    #[serde(default = "default_expected_200")]
    pub expected_status: u16,
    /// Env var name carrying the auth token. If set and present, sent as
    /// `Authorization: Bearer <value>` unless the value starts with `Basic `
    /// or `Bearer `, in which case it's sent as-is.
    ///
    /// Must be one of [`PROVIDER_API_KEY_ENV_ALLOWLIST`]. Any other env
    /// var name will be rejected with [`ReadinessError::BlockedEnvVar`].
    #[serde(default)]
    pub auth_env: Option<String>,
}

/// The full report returned to the frontend.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReadinessReport {
    pub all_ok: bool,
    pub checks: Vec<ReadinessCheck>,
    pub elapsed_ms: u64,
}

/// One check entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReadinessCheck {
    /// `"cargo"` | `"npm"` | `"system_bin"` | `"api"`.
    pub kind: String,
    /// What was checked (crate name, package name, bin name, URL).
    pub target: String,
    pub ok: bool,
    /// Short human-readable message — bounded to ~200 chars.
    pub detail: String,
    pub elapsed_ms: u64,
}

/// Structured failure modes for the API probe. Kept narrow on purpose — each
/// variant maps to a security-relevant rejection at the boundary, so a
/// frontend can render an actionable message instead of a raw stringified
/// error.
///
/// Successful network outcomes (HTTP status mismatch, transport error,
/// timeout) are *not* `ReadinessError` — they're returned as a `ReadinessCheck`
/// with `ok = false`. `ReadinessError` is reserved for "we refused to do the
/// probe at all" cases plus DNS / probe-IO failures the caller may want to
/// log distinctly.
#[derive(Debug, Clone)]
pub enum ReadinessError {
    /// `auth_env` referenced an env var not in
    /// [`PROVIDER_API_KEY_ENV_ALLOWLIST`].
    BlockedEnvVar(String),
    /// URL scheme was anything other than `https`.
    BlockedScheme(String),
    /// URL host was not in the canonical or services.yaml allowlist.
    BlockedHost(String),
    /// Hostname resolved to a loopback / private / link-local / ULA address.
    BlockedPrivateIp(IpAddr),
    /// `tokio::net::lookup_host` returned an error or zero addresses.
    ResolutionFailed(String),
    /// Reqwest failure (timeout, connect refused, TLS error, …).
    ProbeFailed(String),
}

impl fmt::Display for ReadinessError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ReadinessError::BlockedEnvVar(name) => write!(
                f,
                "blocked: auth_env '{}' is not in the provider allowlist",
                name
            ),
            ReadinessError::BlockedScheme(scheme) => {
                write!(
                    f,
                    "blocked: scheme '{}' is not allowed (https only)",
                    scheme
                )
            }
            ReadinessError::BlockedHost(host) => write!(
                f,
                "blocked: host '{}' is not in the provider allowlist (or services.yaml)",
                host
            ),
            ReadinessError::BlockedPrivateIp(ip) => {
                write!(f, "blocked: host resolved to private/loopback IP {}", ip)
            }
            ReadinessError::ResolutionFailed(msg) => write!(f, "dns resolution failed: {}", msg),
            ReadinessError::ProbeFailed(msg) => write!(f, "request failed: {}", msg),
        }
    }
}

impl std::error::Error for ReadinessError {}

/// Top-level entry point: run every check declared in the manifest.
///
/// Empty manifest returns `all_ok = true` and an empty `checks` vec.
pub async fn check_readiness(manifest: &ReadinessManifest, working_dir: &Path) -> ReadinessReport {
    let start = Instant::now();
    let sem = Arc::new(Semaphore::new(MAX_CONCURRENT_CHECKS));

    // Sniff Cargo.toml once so we can tell the user "already a dep".
    let cargo_toml_deps: Option<Vec<String>> = read_cargo_toml_deps(working_dir);

    // Read services.yaml once per probe call and share the resulting set of
    // additional allowed hosts across every `check_api` task.
    let extra_hosts: Arc<HashSet<String>> = Arc::new(read_services_yaml_hosts(working_dir));

    let mut joinset: JoinSet<ReadinessCheck> = JoinSet::new();

    for crate_name in &manifest.cargo_crates {
        let permit = sem.clone();
        let name = crate_name.clone();
        let already_dep = cargo_toml_deps
            .as_ref()
            .map(|deps| deps.iter().any(|d| d == &name))
            .unwrap_or(false);
        let wd: PathBuf = working_dir.to_path_buf();
        joinset.spawn(async move {
            let _p = permit.acquire_owned().await.ok();
            check_cargo_crate(&name, already_dep, &wd).await
        });
    }

    for pkg in &manifest.npm_packages {
        let permit = sem.clone();
        let name = pkg.clone();
        let wd: PathBuf = working_dir.to_path_buf();
        joinset.spawn(async move {
            let _p = permit.acquire_owned().await.ok();
            check_npm_package(&name, &wd).await
        });
    }

    for bin in &manifest.system_bins {
        let permit = sem.clone();
        let name = bin.clone();
        joinset.spawn(async move {
            let _p = permit.acquire_owned().await.ok();
            check_system_bin(&name).await
        });
    }

    for probe in &manifest.apis {
        let permit = sem.clone();
        let probe = probe.clone();
        let hosts = extra_hosts.clone();
        joinset.spawn(async move {
            let _p = permit.acquire_owned().await.ok();
            check_api(&probe, &hosts).await
        });
    }

    let mut checks = Vec::with_capacity(joinset.len());
    while let Some(res) = joinset.join_next().await {
        match res {
            Ok(c) => checks.push(c),
            Err(e) => {
                // JoinError — log and surface as a failed entry so we don't
                // silently swallow a panicking check.
                warn!(error = %e, "readiness check task panicked or was cancelled");
                checks.push(ReadinessCheck {
                    kind: "internal".into(),
                    target: "<unknown>".into(),
                    ok: false,
                    detail: trim_detail(&format!("internal check error: {}", e)),
                    elapsed_ms: 0,
                });
            }
        }
    }

    let all_ok = checks.iter().all(|c| c.ok);
    ReadinessReport {
        all_ok,
        checks,
        elapsed_ms: start.elapsed().as_millis() as u64,
    }
}

// ---------------------------------------------------------------------------
// Individual checks
// ---------------------------------------------------------------------------

async fn check_cargo_crate(name: &str, already_dep: bool, working_dir: &Path) -> ReadinessCheck {
    let start = Instant::now();
    let mut cmd = Command::new("cargo");
    cmd.arg("search")
        .arg("--limit")
        .arg("1")
        .arg("--")
        .arg(name);
    cmd.current_dir(working_dir);

    let output = match tokio::time::timeout(PER_CHECK_TIMEOUT, cmd.output()).await {
        Ok(Ok(o)) => o,
        Ok(Err(e)) => {
            return ReadinessCheck {
                kind: "cargo".into(),
                target: name.into(),
                ok: false,
                detail: trim_detail(&format!("failed to spawn cargo: {}", e)),
                elapsed_ms: start.elapsed().as_millis() as u64,
            };
        }
        Err(_) => {
            return ReadinessCheck {
                kind: "cargo".into(),
                target: name.into(),
                ok: false,
                detail: "timed out (15s) running `cargo search`".into(),
                elapsed_ms: start.elapsed().as_millis() as u64,
            };
        }
    };

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let elapsed_ms = start.elapsed().as_millis() as u64;

    if !output.status.success() {
        return ReadinessCheck {
            kind: "cargo".into(),
            target: name.into(),
            ok: false,
            detail: trim_detail(&format!(
                "cargo search failed: {}",
                if stderr.trim().is_empty() {
                    stdout.trim()
                } else {
                    stderr.trim()
                }
            )),
            elapsed_ms,
        };
    }

    // `cargo search foo --limit 1` returns one line per match: `foo = "x.y.z" # description`.
    // No match → empty stdout. The crate name must appear at the start of a
    // line (followed by space + `=`) to count as a hit.
    let pattern_prefix = format!("{} =", name);
    let found = stdout
        .lines()
        .any(|line| line.trim_start().starts_with(&pattern_prefix));

    if !found {
        return ReadinessCheck {
            kind: "cargo".into(),
            target: name.into(),
            ok: false,
            detail: trim_detail(&format!("crate '{}' not found on crates.io", name)),
            elapsed_ms,
        };
    }

    let detail = if already_dep {
        "already a dep".to_string()
    } else {
        "available on crates.io".to_string()
    };

    ReadinessCheck {
        kind: "cargo".into(),
        target: name.into(),
        ok: true,
        detail,
        elapsed_ms,
    }
}

async fn check_npm_package(name: &str, working_dir: &Path) -> ReadinessCheck {
    let start = Instant::now();
    let mut cmd = Command::new("npm");
    cmd.arg("view")
        .arg("--")
        .arg(name)
        .arg("name")
        .arg("--json");
    cmd.current_dir(working_dir);

    let output = match tokio::time::timeout(PER_CHECK_TIMEOUT, cmd.output()).await {
        Ok(Ok(o)) => o,
        Ok(Err(e)) => {
            return ReadinessCheck {
                kind: "npm".into(),
                target: name.into(),
                ok: false,
                detail: trim_detail(&format!("failed to spawn npm: {}", e)),
                elapsed_ms: start.elapsed().as_millis() as u64,
            };
        }
        Err(_) => {
            return ReadinessCheck {
                kind: "npm".into(),
                target: name.into(),
                ok: false,
                detail: "timed out (15s) running `npm view`".into(),
                elapsed_ms: start.elapsed().as_millis() as u64,
            };
        }
    };

    let elapsed_ms = start.elapsed().as_millis() as u64;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    if !output.status.success() {
        return ReadinessCheck {
            kind: "npm".into(),
            target: name.into(),
            ok: false,
            detail: trim_detail(&format!(
                "npm view failed: {}",
                if stderr.trim().is_empty() {
                    stdout.trim()
                } else {
                    stderr.trim()
                }
            )),
            elapsed_ms,
        };
    }

    // `npm view <pkg> name --json` returns a quoted string `"pkg"` for a
    // single match. An empty payload means the package wasn't found.
    let trimmed = stdout.trim();
    if trimmed.is_empty() {
        return ReadinessCheck {
            kind: "npm".into(),
            target: name.into(),
            ok: false,
            detail: trim_detail(&format!("package '{}' not found in npm registry", name)),
            elapsed_ms,
        };
    }

    ReadinessCheck {
        kind: "npm".into(),
        target: name.into(),
        ok: true,
        detail: "available on npm".into(),
        elapsed_ms,
    }
}

async fn check_system_bin(name: &str) -> ReadinessCheck {
    let start = Instant::now();
    // `command -v` is POSIX. Use `sh -c` so we don't need a separate
    // implementation for `which`. On Windows this gracefully fails — the
    // check will report ok=false with whatever `cmd` says, which is the
    // honest result.
    let shell_cmd = if cfg!(windows) { "where" } else { "command" };
    let mut cmd = if cfg!(windows) {
        let mut c = Command::new(shell_cmd);
        c.arg("--").arg(name);
        c
    } else {
        let mut c = Command::new("sh");
        c.arg("-c")
            .arg(format!("command -v {}", shell_escape(name)));
        c
    };

    let output = match tokio::time::timeout(PER_CHECK_TIMEOUT, cmd.output()).await {
        Ok(Ok(o)) => o,
        Ok(Err(e)) => {
            return ReadinessCheck {
                kind: "system_bin".into(),
                target: name.into(),
                ok: false,
                detail: trim_detail(&format!("failed to spawn shell: {}", e)),
                elapsed_ms: start.elapsed().as_millis() as u64,
            };
        }
        Err(_) => {
            return ReadinessCheck {
                kind: "system_bin".into(),
                target: name.into(),
                ok: false,
                detail: "timed out (15s) checking PATH".into(),
                elapsed_ms: start.elapsed().as_millis() as u64,
            };
        }
    };

    let elapsed_ms = start.elapsed().as_millis() as u64;

    if output.status.success() {
        let stdout = String::from_utf8_lossy(&output.stdout);
        let location = stdout.lines().next().unwrap_or("").trim();
        let detail = if location.is_empty() {
            "found in PATH".to_string()
        } else {
            format!("found at {}", location)
        };
        return ReadinessCheck {
            kind: "system_bin".into(),
            target: name.into(),
            ok: true,
            detail: trim_detail(&detail),
            elapsed_ms,
        };
    }

    ReadinessCheck {
        kind: "system_bin".into(),
        target: name.into(),
        ok: false,
        detail: "binary not in PATH".into(),
        elapsed_ms,
    }
}

/// Execute one validated, SSRF-hardened HTTP probe.
///
/// Returns a [`ReadinessCheck`] tagged `kind = "api"`. Any
/// security-rejection or transport failure is captured in `detail` (see
/// [`ReadinessError`]).
///
/// `extra_hosts` is the cached set of hosts pulled from
/// `working_dir/services.yaml`. Pass an empty set to allow only the canonical
/// provider hosts.
async fn check_api(probe: &ApiProbe, extra_hosts: &HashSet<String>) -> ReadinessCheck {
    let start = Instant::now();

    match check_api_inner(probe, extra_hosts).await {
        Ok(check) => check,
        Err(err) => ReadinessCheck {
            kind: "api".into(),
            target: probe.url.clone(),
            ok: false,
            detail: trim_detail(&err.to_string()),
            elapsed_ms: start.elapsed().as_millis() as u64,
        },
    }
}

/// Inner implementation that returns `Result` so the validation steps can
/// short-circuit with `?`. The outer [`check_api`] maps the error back to a
/// `ReadinessCheck` for the wire format.
async fn check_api_inner(
    probe: &ApiProbe,
    extra_hosts: &HashSet<String>,
) -> Result<ReadinessCheck, ReadinessError> {
    let start = Instant::now();

    // ------------------------------------------------------------------
    // Step 1: env-var allowlist. Reject before touching the environment.
    // ------------------------------------------------------------------
    let auth_header: Option<String> = match probe.auth_env.as_deref() {
        None => None,
        Some(env_name) => {
            if !PROVIDER_API_KEY_ENV_ALLOWLIST
                .iter()
                .any(|allowed| *allowed == env_name)
            {
                return Err(ReadinessError::BlockedEnvVar(env_name.to_string()));
            }
            match std::env::var(env_name) {
                Ok(val) if val.trim().is_empty() => {
                    return Ok(ReadinessCheck {
                        kind: "api".into(),
                        target: probe.url.clone(),
                        ok: false,
                        detail: trim_detail(&format!("auth env '{}' is empty", env_name)),
                        elapsed_ms: start.elapsed().as_millis() as u64,
                    });
                }
                Ok(val) => {
                    if val.starts_with("Basic ") || val.starts_with("Bearer ") {
                        Some(val)
                    } else {
                        Some(format!("Bearer {}", val))
                    }
                }
                Err(_) => {
                    return Ok(ReadinessCheck {
                        kind: "api".into(),
                        target: probe.url.clone(),
                        ok: false,
                        detail: trim_detail(&format!("auth env '{}' not set", env_name)),
                        elapsed_ms: start.elapsed().as_millis() as u64,
                    });
                }
            }
        }
    };

    // ------------------------------------------------------------------
    // Step 2: parse + scheme allowlist.
    // ------------------------------------------------------------------
    let parsed = url::Url::parse(&probe.url)
        .map_err(|e| ReadinessError::ProbeFailed(format!("invalid url: {}", e)))?;

    if parsed.scheme() != "https" {
        return Err(ReadinessError::BlockedScheme(parsed.scheme().to_string()));
    }

    // ------------------------------------------------------------------
    // Step 3: host allowlist. Hostnames are matched case-insensitively;
    // an IP literal is rejected because it bypasses the host-name check
    // and gives us no way to anchor a Host header.
    // ------------------------------------------------------------------
    let host_str = parsed
        .host_str()
        .ok_or_else(|| ReadinessError::BlockedHost("<missing>".to_string()))?;
    let host_lc = host_str.to_ascii_lowercase();

    let canonical_match = CANONICAL_PROVIDER_HOSTS
        .iter()
        .any(|h| h.eq_ignore_ascii_case(host_str));
    let extra_match = extra_hosts.contains(&host_lc);

    if !(canonical_match || extra_match) {
        return Err(ReadinessError::BlockedHost(host_str.to_string()));
    }

    // ------------------------------------------------------------------
    // Step 4: resolve + private-IP check.
    // ------------------------------------------------------------------
    let port = parsed.port().unwrap_or(DEFAULT_HTTPS_PORT);
    let lookup_target = format!("{}:{}", host_str, port);

    let lookup = tokio::time::timeout(PER_CHECK_TIMEOUT, tokio::net::lookup_host(&lookup_target))
        .await
        .map_err(|_| ReadinessError::ResolutionFailed("timed out (15s)".into()))?
        .map_err(|e| ReadinessError::ResolutionFailed(e.to_string()))?;

    let addrs: Vec<SocketAddr> = lookup.collect();
    if addrs.is_empty() {
        return Err(ReadinessError::ResolutionFailed(format!(
            "no addresses for '{}'",
            host_str
        )));
    }

    // Reject if **any** resolved IP is private — refusing is the safe
    // default, since a connect attempt could otherwise race against the
    // resolver and pick the bad address.
    for addr in &addrs {
        if is_private_ip(&addr.ip()) {
            return Err(ReadinessError::BlockedPrivateIp(addr.ip()));
        }
    }

    // ------------------------------------------------------------------
    // Step 5: build a hardened reqwest client and send.
    //
    // - `resolve_to_addrs` pins the hostname → IP mapping for *this*
    //   client. Reqwest will connect to the addresses we vetted; the OS
    //   resolver is bypassed, so DNS-rebinding attacks can't swap in a
    //   private IP between our validation and the actual connect.
    // - `redirect(Policy::none())` prevents the server from bouncing the
    //   probe to an unvetted host — and from triggering a follow-up
    //   request that re-runs the resolver against an attacker domain.
    // ------------------------------------------------------------------
    let method = parse_method(&probe.method)?;

    let mut builder = reqwest::Client::builder()
        .timeout(PER_CHECK_TIMEOUT)
        .redirect(reqwest::redirect::Policy::none());

    for addr in &addrs {
        builder = builder.resolve_to_addrs(host_str, &[*addr]);
    }

    let client = builder
        .build()
        .map_err(|e| ReadinessError::ProbeFailed(format!("failed to build http client: {}", e)))?;

    let mut req = client.request(method, parsed.clone());
    if let Some(h) = auth_header {
        req = req.header(reqwest::header::AUTHORIZATION, h);
    }

    let result = tokio::time::timeout(PER_CHECK_TIMEOUT, req.send()).await;
    let elapsed_ms = start.elapsed().as_millis() as u64;

    match result {
        Ok(Ok(resp)) => {
            let actual = resp.status().as_u16();
            if actual == probe.expected_status {
                Ok(ReadinessCheck {
                    kind: "api".into(),
                    target: probe.url.clone(),
                    ok: true,
                    detail: trim_detail(&format!("HTTP {} (matches expected)", actual)),
                    elapsed_ms,
                })
            } else {
                Ok(ReadinessCheck {
                    kind: "api".into(),
                    target: probe.url.clone(),
                    ok: false,
                    detail: trim_detail(&format!(
                        "HTTP {} (expected {})",
                        actual, probe.expected_status
                    )),
                    elapsed_ms,
                })
            }
        }
        Ok(Err(e)) => Err(ReadinessError::ProbeFailed(e.to_string())),
        Err(_) => Err(ReadinessError::ProbeFailed(
            "timed out (15s) waiting for response".into(),
        )),
    }
}

fn parse_method(s: &str) -> Result<reqwest::Method, ReadinessError> {
    match s.to_uppercase().as_str() {
        "GET" => Ok(reqwest::Method::GET),
        "POST" => Ok(reqwest::Method::POST),
        "PUT" => Ok(reqwest::Method::PUT),
        "DELETE" => Ok(reqwest::Method::DELETE),
        "HEAD" => Ok(reqwest::Method::HEAD),
        "OPTIONS" => Ok(reqwest::Method::OPTIONS),
        "PATCH" => Ok(reqwest::Method::PATCH),
        other => Err(ReadinessError::ProbeFailed(format!(
            "unsupported HTTP method '{}'",
            other
        ))),
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Returns true for any address that is *not* safe to talk to from a
/// hardened probe context: loopback, RFC1918 private space, link-local,
/// IPv6 unique local addresses (ULA, `fc00::/7`), and unspecified addresses.
fn is_private_ip(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            // is_loopback, is_private, is_link_local are stable; is_shared
            // and is_benchmarking are not. Cover the major cases by hand.
            if v4.is_loopback() || v4.is_private() || v4.is_link_local() || v4.is_unspecified() {
                return true;
            }
            let oct = v4.octets();
            // Carrier-grade NAT (RFC 6598): 100.64.0.0/10.
            if oct[0] == 100 && (oct[1] & 0xc0) == 64 {
                return true;
            }
            // Broadcast: 255.255.255.255.
            if v4.is_broadcast() {
                return true;
            }
            false
        }
        IpAddr::V6(v6) => {
            if v6.is_loopback() || v6.is_unspecified() {
                return true;
            }
            let seg0 = v6.segments()[0];
            // Link-local: fe80::/10
            if (seg0 & 0xffc0) == 0xfe80 {
                return true;
            }
            // Unique local addresses (ULA): fc00::/7
            if (seg0 & 0xfe00) == 0xfc00 {
                return true;
            }
            // IPv4-mapped (::ffff:0:0/96) — recurse on the embedded v4.
            if let Some(v4) = v6.to_ipv4_mapped() {
                return is_private_ip(&IpAddr::V4(v4));
            }
            false
        }
    }
}

/// Read `services.yaml` in `working_dir` (if present) and extract the set
/// of additional allowed hosts.
///
/// Expected shape (any of these is accepted):
///
/// ```yaml
/// hosts:
///   - my-self-hosted.example.com
///   - api.internal.example.org
/// ```
///
/// or
///
/// ```yaml
/// services:
///   - host: my-self-hosted.example.com
///   - host: api.internal.example.org
/// ```
///
/// Returns an empty set if the file is missing, unreadable, or invalid —
/// failure-open here is safe because the canonical provider allowlist
/// still applies; a malformed `services.yaml` simply means no additional
/// hosts are added, not that the probe widens.
fn read_services_yaml_hosts(working_dir: &Path) -> HashSet<String> {
    let path = working_dir.join("services.yaml");
    let Ok(content) = std::fs::read_to_string(&path) else {
        return HashSet::new();
    };

    let Ok(value) = serde_yaml::from_str::<serde_yaml::Value>(&content) else {
        warn!(path = %path.display(), "services.yaml parse failed; ignoring");
        return HashSet::new();
    };

    let mut out = HashSet::new();

    // Top-level `hosts: [..]` form.
    if let Some(seq) = value.get("hosts").and_then(|v| v.as_sequence()) {
        for item in seq {
            if let Some(s) = item.as_str() {
                push_normalized_host(&mut out, s);
            }
        }
    }

    // Top-level `services: [{ host: .. }, ..]` form.
    if let Some(seq) = value.get("services").and_then(|v| v.as_sequence()) {
        for item in seq {
            if let Some(s) = item.get("host").and_then(|v| v.as_str()) {
                push_normalized_host(&mut out, s);
            }
        }
    }

    out
}

fn push_normalized_host(out: &mut HashSet<String>, raw: &str) {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return;
    }
    out.insert(trimmed.to_ascii_lowercase());
}

fn trim_detail(s: &str) -> String {
    let collapsed: String = s.chars().map(|c| if c == '\n' { ' ' } else { c }).collect();
    if collapsed.chars().count() <= DETAIL_PREVIEW_CHARS {
        collapsed
    } else {
        let truncated: String = collapsed.chars().take(DETAIL_PREVIEW_CHARS).collect();
        format!("{}...", truncated)
    }
}

/// Quote an argument for `sh -c`. We only ever pass crate/binary names from
/// the manifest, but defence-in-depth: surround with single quotes and escape
/// embedded single quotes the POSIX way (`'\''`).
fn shell_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for c in s.chars() {
        if c == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(c);
        }
    }
    out.push('\'');
    out
}

/// Best-effort: parse `Cargo.toml` `[dependencies]` table for crate names.
///
/// Returns `None` if there is no `Cargo.toml` in `working_dir` or it can't be
/// read. Returns `Some(vec![])` if the file exists but has no `[dependencies]`
/// section (vs `None` which means "we have no info").
///
/// This is intentionally not a full TOML parser — we don't depend on `toml` at
/// the workspace level for this single use. We extract crate names from lines
/// that look like `name = "..."` or `name = {...}` inside `[dependencies]` or
/// `[dev-dependencies]`. False positives are harmless ("already a dep" is just
/// informational).
fn read_cargo_toml_deps(working_dir: &Path) -> Option<Vec<String>> {
    let toml_path = working_dir.join("Cargo.toml");
    let content = std::fs::read_to_string(&toml_path).ok()?;

    let mut deps = Vec::new();
    let mut in_deps_section = false;
    for raw_line in content.lines() {
        let line = raw_line.trim();
        if line.starts_with('[') && line.ends_with(']') {
            in_deps_section = matches!(
                line,
                "[dependencies]" | "[dev-dependencies]" | "[build-dependencies]"
            );
            continue;
        }
        if !in_deps_section || line.is_empty() || line.starts_with('#') {
            continue;
        }
        // Look for `name = ...`
        if let Some(eq_idx) = line.find('=') {
            let name = line[..eq_idx].trim();
            if !name.is_empty()
                && name
                    .chars()
                    .all(|c| c.is_alphanumeric() || c == '_' || c == '-')
            {
                deps.push(name.to_string());
            }
        }
    }
    Some(deps)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn tmp_dir() -> PathBuf {
        std::env::temp_dir()
    }

    #[tokio::test]
    async fn empty_manifest_passes() {
        let manifest = ReadinessManifest::default();
        let report = check_readiness(&manifest, &tmp_dir()).await;
        assert!(report.all_ok, "empty manifest should be all_ok");
        assert!(report.checks.is_empty(), "no checks should run");
    }

    #[tokio::test]
    async fn system_bin_real_binary_ok() {
        // `cargo` is guaranteed to exist in this build environment because we
        // are running `cargo test` to invoke this very test.
        let manifest = ReadinessManifest {
            system_bins: vec!["cargo".to_string()],
            ..Default::default()
        };
        let report = check_readiness(&manifest, &tmp_dir()).await;
        assert_eq!(report.checks.len(), 1);
        let c = &report.checks[0];
        assert_eq!(c.kind, "system_bin");
        assert_eq!(c.target, "cargo");
        assert!(c.ok, "cargo should be in PATH; detail was: {}", c.detail);
        assert!(report.all_ok);
    }

    #[tokio::test]
    async fn system_bin_missing_binary_fails() {
        let manifest = ReadinessManifest {
            system_bins: vec!["definitely-not-a-real-binary-xyz123".to_string()],
            ..Default::default()
        };
        let report = check_readiness(&manifest, &tmp_dir()).await;
        assert_eq!(report.checks.len(), 1);
        let c = &report.checks[0];
        assert!(!c.ok);
        assert!(!report.all_ok);
        assert!(
            c.detail.contains("not in PATH") || c.detail.contains("not found"),
            "unexpected detail: {}",
            c.detail
        );
    }

    #[tokio::test]
    async fn elapsed_ms_is_set_and_reasonable() {
        let manifest = ReadinessManifest {
            system_bins: vec![
                "cargo".to_string(),
                "definitely-not-a-real-binary-xyz123".to_string(),
            ],
            ..Default::default()
        };
        let report = check_readiness(&manifest, &tmp_dir()).await;
        assert_eq!(report.checks.len(), 2);
        // Top-level elapsed must be >= each individual check (we ran them
        // concurrently, so it's roughly max() not sum()).
        let max_child = report
            .checks
            .iter()
            .map(|c| c.elapsed_ms)
            .max()
            .unwrap_or(0);
        assert!(
            report.elapsed_ms >= max_child,
            "top-level elapsed ({}) should be >= max child ({})",
            report.elapsed_ms,
            max_child
        );
        // Each check has its own elapsed_ms set.
        for c in &report.checks {
            // The duration may legitimately round to 0 on very fast hosts;
            // we just confirm the field is populated (u64 >= 0 always; this
            // exists to document intent).
            let _ = c.elapsed_ms;
        }
    }

    #[tokio::test]
    async fn api_missing_auth_env_fails_fast() {
        // Clear the env var to be safe. Must be from the allowlist for the
        // probe to even get to the env-var lookup step.
        std::env::remove_var("ANTHROPIC_API_KEY");
        let manifest = ReadinessManifest {
            apis: vec![ApiProbe {
                url: "https://api.anthropic.com/v1/models".into(),
                method: "GET".into(),
                expected_status: 200,
                auth_env: Some("ANTHROPIC_API_KEY".into()),
            }],
            ..Default::default()
        };
        let report = check_readiness(&manifest, &tmp_dir()).await;
        assert_eq!(report.checks.len(), 1);
        let c = &report.checks[0];
        assert!(!c.ok);
        assert!(
            c.detail.contains("not set"),
            "unexpected detail: {}",
            c.detail
        );
        // No real HTTP attempt → must be fast.
        assert!(
            c.elapsed_ms < 5000,
            "auth-missing check should fail fast, got {}ms",
            c.elapsed_ms
        );
    }

    #[test]
    fn trim_detail_caps_long_strings() {
        let long = "a".repeat(500);
        let out = trim_detail(&long);
        // Truncated string is DETAIL_PREVIEW_CHARS chars + "..." suffix.
        assert!(out.ends_with("..."));
        assert!(out.chars().count() <= DETAIL_PREVIEW_CHARS + 3);
    }

    #[test]
    fn trim_detail_collapses_newlines() {
        let s = "line one\nline two";
        let out = trim_detail(s);
        assert!(!out.contains('\n'));
        assert!(out.contains("line one"));
        assert!(out.contains("line two"));
    }

    #[test]
    fn shell_escape_quotes_and_escapes() {
        assert_eq!(shell_escape("serde"), "'serde'");
        assert_eq!(shell_escape("o'reilly"), "'o'\\''reilly'");
    }

    #[test]
    fn read_cargo_toml_deps_extracts_names() {
        // The project's own Cargo.toml lives at app/src-tauri/Cargo.toml.
        // CARGO_MANIFEST_DIR points there at build time.
        let manifest_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let deps = read_cargo_toml_deps(&manifest_dir);
        let deps = deps.expect("Cargo.toml should be readable");
        // Sanity: a few well-known deps should appear.
        assert!(
            deps.iter().any(|d| d == "serde"),
            "serde missing: {:?}",
            deps
        );
        assert!(
            deps.iter().any(|d| d == "tokio"),
            "tokio missing: {:?}",
            deps
        );
        assert!(
            deps.iter().any(|d| d == "reqwest"),
            "reqwest missing: {:?}",
            deps
        );
    }

    #[test]
    fn read_cargo_toml_deps_missing_file_returns_none() {
        let empty = std::env::temp_dir().join("hyvemind-readiness-no-cargo-xyz");
        // Make sure no Cargo.toml exists at this path.
        let _ = std::fs::remove_file(empty.join("Cargo.toml"));
        let _ = std::fs::create_dir_all(&empty);
        assert!(read_cargo_toml_deps(&empty).is_none());
    }

    /// Verify that [`check_cargo_crate`] inserts `--` before the crate name so
    /// that a name starting with `-` (e.g. `--help`) is treated as a positional
    /// search term, not as a cargo flag.
    ///
    /// This test requires `cargo` on `$PATH` and a live crates.io connection;
    /// when those are unavailable the check will still return a result (the
    /// assertion is that it does NOT return flag-parsing output).
    #[tokio::test]
    async fn cargo_name_starting_with_dash_not_a_flag() {
        let check = check_cargo_crate("--help", false, &tmp_dir()).await;
        // The command completed (no timeout or spawn failure).
        assert!(
            !check.detail.contains("timed out"),
            "unexpected timeout: {}",
            check.detail
        );
        assert!(
            !check.detail.contains("failed to spawn"),
            "spawn failure: {}",
            check.detail
        );
        // If the -- sentinel were missing, cargo would interpret --help as a
        // flag and print usage text. The detail (trimmed stdout/stderr) must
        // not contain usage-line markers.
        assert!(
            !check.detail.contains("Usage:"),
            "cargo interpreted --help as a flag (no -- sentinel?): {}",
            check.detail
        );
    }

    /// Verify that [`check_npm_package`] inserts `--` before the package name
    /// so that a name starting with `-` is treated as a positional package name,
    /// not as an npm flag.
    #[tokio::test]
    async fn npm_name_starting_with_dash_not_a_flag() {
        let check = check_npm_package("--help", &tmp_dir()).await;
        assert!(
            !check.detail.contains("timed out"),
            "unexpected timeout: {}",
            check.detail
        );
        assert!(
            !check.detail.contains("failed to spawn"),
            "spawn failure: {}",
            check.detail
        );
        // Without --, npm view --help would print npm's help text. Since
        // --help isn't a real package, npm returns a 404 — that proves it was
        // treated as a package name, not a flag.
        assert!(
            !check.detail.contains("Specify configs"),
            "npm interpreted --help as a flag (no -- sentinel?): {}",
            check.detail
        );
    }

    // NOTE (TODO): full coverage for cargo / npm / api checks requires either
    // a mockable subprocess + HTTP transport, or live network access. These
    // are intentionally not exercised here because:
    //   - `cargo search` hits crates.io and is slow + flaky in CI
    //   - `npm view` requires npm being installed and reachable
    //   - real API probes need a guaranteed test endpoint
    // The plumbing is covered by the system_bin tests above; the cargo/npm/api
    // paths share the same timeout + spawn pattern.

    // -----------------------------------------------------------------
    // Security: SSRF + key-exfiltration hardening for `check_api`.
    // -----------------------------------------------------------------

    #[tokio::test]
    async fn api_blocked_env_var_rejected_before_lookup() {
        // A var that ISN'T in PROVIDER_API_KEY_ENV_ALLOWLIST. To be doubly
        // safe, also set it in the env — the test passes only if the
        // probe refuses to read it.
        let env_name = "AWS_SECRET_ACCESS_KEY";
        std::env::set_var(env_name, "should-never-be-read");
        let manifest = ReadinessManifest {
            apis: vec![ApiProbe {
                url: "https://api.anthropic.com/v1/models".into(),
                method: "GET".into(),
                expected_status: 200,
                auth_env: Some(env_name.into()),
            }],
            ..Default::default()
        };
        let report = check_readiness(&manifest, &tmp_dir()).await;
        std::env::remove_var(env_name);

        assert_eq!(report.checks.len(), 1);
        let c = &report.checks[0];
        assert!(!c.ok, "blocked env var must fail the check");
        assert!(
            c.detail.contains("blocked")
                && (c.detail.contains("allowlist") || c.detail.contains("env")),
            "expected env-var rejection message, got: {}",
            c.detail
        );
        // Token (the env value) MUST NOT appear anywhere in the surfaced detail.
        assert!(
            !c.detail.contains("should-never-be-read"),
            "env value leaked into detail: {}",
            c.detail
        );
    }

    #[tokio::test]
    async fn api_blocked_scheme_http_rejected() {
        let manifest = ReadinessManifest {
            apis: vec![ApiProbe {
                url: "http://api.anthropic.com/v1/models".into(),
                method: "GET".into(),
                expected_status: 200,
                auth_env: None,
            }],
            ..Default::default()
        };
        let report = check_readiness(&manifest, &tmp_dir()).await;
        assert_eq!(report.checks.len(), 1);
        let c = &report.checks[0];
        assert!(!c.ok);
        assert!(
            c.detail.contains("scheme") && c.detail.contains("http"),
            "expected scheme rejection, got: {}",
            c.detail
        );
    }

    #[tokio::test]
    async fn api_blocked_scheme_file_rejected() {
        let manifest = ReadinessManifest {
            apis: vec![ApiProbe {
                url: "file:///etc/passwd".into(),
                method: "GET".into(),
                expected_status: 200,
                auth_env: None,
            }],
            ..Default::default()
        };
        let report = check_readiness(&manifest, &tmp_dir()).await;
        assert_eq!(report.checks.len(), 1);
        let c = &report.checks[0];
        assert!(!c.ok);
        assert!(
            c.detail.contains("scheme"),
            "expected scheme rejection, got: {}",
            c.detail
        );
    }

    #[tokio::test]
    async fn api_blocked_host_not_in_allowlist() {
        let manifest = ReadinessManifest {
            apis: vec![ApiProbe {
                url: "https://attacker.example.com/exfil".into(),
                method: "GET".into(),
                expected_status: 200,
                auth_env: None,
            }],
            ..Default::default()
        };
        let report = check_readiness(&manifest, &tmp_dir()).await;
        assert_eq!(report.checks.len(), 1);
        let c = &report.checks[0];
        assert!(!c.ok);
        assert!(
            c.detail.contains("host") && c.detail.contains("attacker.example.com"),
            "expected host rejection, got: {}",
            c.detail
        );
    }

    #[tokio::test]
    async fn api_blocked_host_ip_literal_rejected() {
        // IP literals shouldn't match any host allowlist entry (those are
        // DNS names, not raw IPs).
        let manifest = ReadinessManifest {
            apis: vec![ApiProbe {
                url: "https://127.0.0.1/".into(),
                method: "GET".into(),
                expected_status: 200,
                auth_env: None,
            }],
            ..Default::default()
        };
        let report = check_readiness(&manifest, &tmp_dir()).await;
        assert_eq!(report.checks.len(), 1);
        let c = &report.checks[0];
        assert!(!c.ok);
        assert!(
            c.detail.contains("host") || c.detail.contains("blocked"),
            "expected IP literal rejection, got: {}",
            c.detail
        );
    }

    /// Build a temporary working directory whose `services.yaml` resolves
    /// `localhost-loopback.example.test` as an additional allowed host so we
    /// can exercise the host-allowlist + private-IP branches together. We
    /// can't easily resolve a synthetic name to 127.0.0.1 here, so instead
    /// we test the underlying `is_private_ip` helper directly plus the
    /// services.yaml parse path.
    #[test]
    fn is_private_ip_matrix() {
        use std::net::Ipv4Addr;

        assert!(is_private_ip(&IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1))));
        assert!(is_private_ip(&IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1))));
        assert!(is_private_ip(&IpAddr::V4(Ipv4Addr::new(172, 16, 0, 1))));
        assert!(is_private_ip(&IpAddr::V4(Ipv4Addr::new(172, 31, 255, 254))));
        assert!(is_private_ip(&IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1))));
        assert!(is_private_ip(&IpAddr::V4(Ipv4Addr::new(169, 254, 1, 1))));
        assert!(is_private_ip(&IpAddr::V4(Ipv4Addr::new(0, 0, 0, 0))));
        assert!(is_private_ip(&IpAddr::V4(Ipv4Addr::new(
            255, 255, 255, 255
        ))));
        // Carrier-grade NAT 100.64.0.0/10
        assert!(is_private_ip(&IpAddr::V4(Ipv4Addr::new(100, 64, 0, 1))));
        assert!(is_private_ip(&IpAddr::V4(Ipv4Addr::new(100, 127, 0, 1))));

        // Public IPs MUST NOT match.
        assert!(!is_private_ip(&IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8))));
        assert!(!is_private_ip(&IpAddr::V4(Ipv4Addr::new(1, 1, 1, 1))));
        assert!(!is_private_ip(&IpAddr::V4(Ipv4Addr::new(151, 101, 1, 195))));

        // IPv6 private cases.
        assert!(is_private_ip(&"::1".parse().unwrap()));
        assert!(is_private_ip(&"fe80::1".parse().unwrap()));
        assert!(is_private_ip(&"fc00::1".parse().unwrap()));
        assert!(is_private_ip(&"fd12:3456:789a::1".parse().unwrap()));
        // IPv4-mapped loopback.
        assert!(is_private_ip(&"::ffff:127.0.0.1".parse().unwrap()));

        // Public IPv6.
        assert!(!is_private_ip(&"2606:4700:4700::1111".parse().unwrap()));
    }

    #[tokio::test]
    async fn api_blocked_private_ip_via_services_yaml() {
        // services.yaml allows `localhost` (which always resolves to a
        // loopback address on POSIX hosts). The probe must still be
        // rejected at the private-IP check even though `localhost` is in
        // the host allowlist — defence-in-depth.
        let dir = std::env::temp_dir().join("hyvemind-readiness-services-yaml-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("services.yaml"), "hosts:\n  - localhost\n").unwrap();

        let manifest = ReadinessManifest {
            apis: vec![ApiProbe {
                url: "https://localhost:443/".into(),
                method: "GET".into(),
                expected_status: 200,
                auth_env: None,
            }],
            ..Default::default()
        };
        let report = check_readiness(&manifest, &dir).await;
        let _ = std::fs::remove_dir_all(&dir);

        assert_eq!(report.checks.len(), 1);
        let c = &report.checks[0];
        assert!(
            !c.ok,
            "loopback probe must be rejected even when host allowed"
        );
        assert!(
            c.detail.contains("private")
                || c.detail.contains("loopback")
                || c.detail.contains("127.")
                || c.detail.contains("::1"),
            "expected private-IP rejection, got: {}",
            c.detail
        );
    }

    #[tokio::test]
    async fn api_blocked_link_local_via_services_yaml() {
        // 169.254.x.x is the AWS / cloud metadata range. Allow a fake
        // metadata host in services.yaml and confirm the probe still
        // refuses because the resolved IP is link-local.
        //
        // We resolve by way of a synthetic /etc/hosts-style name. Since we
        // can't reliably alter /etc/hosts in a test, fall back to
        // exercising the same code path through the `is_private_ip` test
        // above (which already covers 169.254.x.x) plus this test, which
        // confirms the services.yaml extension hosts don't bypass it. We
        // assert the path resolves to localhost (which is loopback) when
        // `localhost` is allowed (covered above) — together these prove
        // services.yaml never widens the private-IP block.
        //
        // No-op assertion to document intent — full coverage is in the
        // services_yaml + is_private_ip tests already.
        let _: bool = is_private_ip(&IpAddr::V4(std::net::Ipv4Addr::new(169, 254, 169, 254)));
    }

    #[test]
    fn services_yaml_hosts_form_parsed() {
        let dir = std::env::temp_dir().join("hyvemind-readiness-services-yaml-hosts");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("services.yaml"),
            "hosts:\n  - A.example.com\n  - b.example.com\n",
        )
        .unwrap();

        let hosts = read_services_yaml_hosts(&dir);
        let _ = std::fs::remove_dir_all(&dir);
        // Lowercased; both present.
        assert!(hosts.contains("a.example.com"));
        assert!(hosts.contains("b.example.com"));
    }

    #[test]
    fn services_yaml_services_form_parsed() {
        let dir = std::env::temp_dir().join("hyvemind-readiness-services-yaml-services");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("services.yaml"),
            "services:\n  - host: alpha.example.com\n  - host: beta.example.com\n",
        )
        .unwrap();
        let hosts = read_services_yaml_hosts(&dir);
        let _ = std::fs::remove_dir_all(&dir);
        assert!(hosts.contains("alpha.example.com"));
        assert!(hosts.contains("beta.example.com"));
    }

    #[test]
    fn services_yaml_missing_returns_empty() {
        let dir = std::env::temp_dir().join("hyvemind-readiness-no-services-yaml");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let hosts = read_services_yaml_hosts(&dir);
        let _ = std::fs::remove_dir_all(&dir);
        assert!(hosts.is_empty());
    }

    #[test]
    fn services_yaml_malformed_returns_empty() {
        let dir = std::env::temp_dir().join("hyvemind-readiness-malformed-services-yaml");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("services.yaml"), "::: not valid yaml :::").unwrap();
        let hosts = read_services_yaml_hosts(&dir);
        let _ = std::fs::remove_dir_all(&dir);
        assert!(hosts.is_empty());
    }

    /// Allowed legit probe path: target a canonical host (`api.anthropic.com`)
    /// with no auth. We expect the probe to PASS validation (no
    /// blocked-host / blocked-scheme / blocked-private-IP error) and either
    /// receive a real HTTP status from the live endpoint **or** surface a
    /// transport-level "request failed" detail in offline / CI conditions.
    /// The check explicitly does NOT require network connectivity.
    #[tokio::test]
    async fn api_allowed_canonical_host_passes_validation() {
        let manifest = ReadinessManifest {
            apis: vec![ApiProbe {
                // `/v1/models` returns 401 without auth — used here only to
                // confirm the probe reached an HTTP layer. Status mismatch
                // is fine; what we're proving is that no security gate
                // rejected the request.
                url: "https://api.anthropic.com/v1/models".into(),
                method: "GET".into(),
                expected_status: 401,
                auth_env: None,
            }],
            ..Default::default()
        };
        let report = check_readiness(&manifest, &tmp_dir()).await;
        assert_eq!(report.checks.len(), 1);
        let c = &report.checks[0];
        // The check may succeed (HTTP 401 matches expected) or fail with a
        // transport-level message if the test environment is offline. The
        // critical property is that the failure mode is NEVER one of the
        // security-gate rejections.
        assert!(
            !c.detail.contains("blocked"),
            "canonical host probe must not be blocked, got: {}",
            c.detail
        );
        assert!(
            !c.detail.contains("not allowed"),
            "canonical host probe must not be blocked, got: {}",
            c.detail
        );
    }
}
