fn main() {
    tauri_build::build();

    // Sentry DSN bake. Precedence:
    //   1. SENTRY_DSN already in the build environment (CI / one-off shell)
    //   2. SENTRY_DSN= line in app/.env (the maintainer's day-to-day path)
    //
    // Whatever wins is emitted as a rustc env var so option_env!("SENTRY_DSN")
    // in src/sentry_setup.rs picks it up at compile time. Works for both
    // debug and release builds — drop the DSN in app/.env once and every
    // `cargo run` / `tauri dev` / `tauri build` after that has Sentry on.
    let dsn = std::env::var("SENTRY_DSN")
        .ok()
        .or_else(|| read_dsn_from_env_file("../.env"))
        .or_else(|| read_dsn_from_env_file(".env"));

    if let Some(dsn) = dsn.filter(|s| !s.is_empty()) {
        println!("cargo:rustc-env=SENTRY_DSN={}", dsn);
    }

    // Re-run the build script when either source changes so an updated DSN
    // takes effect without a `cargo clean`.
    println!("cargo:rerun-if-changed=../.env");
    println!("cargo:rerun-if-changed=.env");
    println!("cargo:rerun-if-env-changed=SENTRY_DSN");
}

/// Minimal `.env` parser: looks for a line `SENTRY_DSN=...` (with optional
/// surrounding quotes), ignoring comments and blanks. Not a full dotenv
/// implementation — we only need this one key.
fn read_dsn_from_env_file(path: &str) -> Option<String> {
    let content = std::fs::read_to_string(path).ok()?;
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(rest) = line.strip_prefix("SENTRY_DSN=") {
            let v = rest.trim().trim_matches(|c| c == '"' || c == '\'');
            if !v.is_empty() {
                return Some(v.to_string());
            }
        }
    }
    None
}
