# Changelog

All notable changes to Hyvemind will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added
- `CHANGELOG.md`, `CODE_OF_CONDUCT.md`, `.editorconfig`
- GitHub issue templates (bug report, feature request) and PR template
- CI workflow (`ci.yml`): Rust + frontend lint, type-check, test on every PR and push to `main`
- Build workflow (`build.yml`): multi-platform Tauri compile sanity check (macOS arm64/x64, Linux, Windows)
- Release workflow (`release.yml`): tag-triggered, builds installers for all platforms and attaches them to a GitHub Release
- `description`, `license`, `repository`, `homepage`, `keywords` metadata in `app/package.json` and `app/src-tauri/Cargo.toml`
- `bundle.publisher` and `bundle.copyright` in `app/src-tauri/tauri.conf.json`

### Fixed
- Removed unresolved merge conflict markers from `.gitignore`

### Removed
- Pi-pool prewarming on Tasks-screen mount (`prewarm_pi_session` IPC). The mount effect double-fired under React StrictMode and made the topbar "sessions" count jump from 0 to 2 on a fresh app open. First message now pays the 1-2s Pi spawn cost; nothing spins up until the user sends.

## [0.1.0-alpha] - 2026-05-15

First public alpha. Early-access only — being shared with friends and testers.

### Added
- **Tasks** — full end-to-end planning conversation (token streaming, tool calls, reasoning blocks, session resume, history)
- **Hivemind** — multi-model review engine with For/Against/Neutral stances, cross-round merge, 4 provider backends (Anthropic, OpenAI, OpenRouter, Ollama), 3-state circuit breakers per provider, response cache (moka), exponential backoff with jitter, SQLite-backed (WAL mode) job persistence
- **Settings** — API key management via OS keychain, provider configuration, model discovery, connectivity tests
- **Dashboard** — usage stats, cost tracking, recent activity
- **Pi integration** — JSONL RPC client, session pool with global semaphore (default 6), bundled bun-compiled binary pinned to `scripts/pi-version.txt`
- **UI shell** — 5 main screens (Dashboard, Tasks, Swarms, Hiveminds, Settings), full custom design system (honey-on-ink palette, Inter + JetBrains Mono)
- **Swarms (partial)** — types, Queen orchestrator, Scout, Worker, Guard, scheduler, store; end-to-end autonomous loop in progress
- **Reliability primitives** — atomic file writes (`tempfile + rename`), append-only JSONL progress logs, IPC event batching (100 ms / 50 token coalescence)
- **Debug logging** — structured per-ID JSONL logs under `~/.hyvemind/debug/` when `HYVEMIND_DEBUG=1`

### Known limitations
- **Nurse** agent (the headline reliability feature) — plumbing in place; agent logic is the next major build
- Swarm crash-recovery from JSONL progress log — partial
- macOS bundles are not yet code-signed or notarised — Gatekeeper will warn on first launch
- Linux & Windows are produced by CI but not actively tested

[Unreleased]: https://github.com/Unravl/Hyvemind/compare/v0.1.0-alpha...HEAD
[0.1.0-alpha]: https://github.com/Unravl/Hyvemind/releases/tag/v0.1.0-alpha
