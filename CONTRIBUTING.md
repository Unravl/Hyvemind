# Contributing to Hyvemind

Thanks for your interest in contributing! Hyvemind is in alpha — the codebase is moving
quickly, but we welcome bug reports, feature ideas, and pull requests.

By participating in this project you agree to abide by the [Code of Conduct](CODE_OF_CONDUCT.md).

## Before you start

- **Browse open issues** — especially anything tagged `good first issue` or `help wanted`.
- **For non‑trivial changes, open a discussion or issue first.** This avoids you sinking
  time into work that conflicts with planned direction.
- **Read `PRODUCT.md`** for the product vision and `CLAUDE.md` for the architecture map.
  Both live at the repo root.

## Development setup

### Prerequisites

| Tool | Version | Why |
|---|---|---|
| **Rust** | stable (via [rustup](https://rustup.rs/)) | Tauri backend |
| **Node.js** | 20+ | Frontend toolchain |
| **npm** | bundled with Node | Package install + scripts |
| **Bun** | latest | Compiles the bundled Pi runtime (`scripts/build-pi.sh`) |
| **Tauri prerequisites** | per‑platform | See [tauri.app/start/prerequisites](https://tauri.app/start/prerequisites/) |

#### Linux

On Linux you also need the system libraries Tauri links against:

```bash
sudo apt-get update
sudo apt-get install -y \
  libwebkit2gtk-4.1-dev \
  libappindicator3-dev \
  librsvg2-dev \
  patchelf \
  libgtk-3-dev \
  libssl-dev
```

macOS ships these equivalents in the system SDK, so no extra step is needed.

### Clone & run

```bash
git clone https://github.com/Unravl/Hyvemind.git
cd Hyvemind/app
npm install
npm run tauri:dev
```

The first `tauri:dev` invokes `scripts/build-pi.sh` automatically (2–5 min on first run,
free thereafter thanks to a stamp file at `app/src-tauri/binaries/.pi-version`).
See the [First run](README.md#first-run) section of the README for what's happening during
this step.

### Useful commands

```bash
# Backend (run from app/src-tauri/)
cargo check                     # Type-check
cargo test                      # Run the Rust test suite
cargo fmt --check               # Verify formatting
cargo clippy -- -D warnings     # Lint with warnings as errors

# Frontend (run from app/)
npm test                        # Run vitest
npm run test:watch              # Run vitest in watch mode
npx tsc --noEmit                # Type-check
npm run build                   # Build the production frontend bundle
```

CI runs the same commands. If they pass locally, your PR's CI will pass too.

### Rust hot-reload (`--no-watch`)

The `tauri:dev` script in `app/package.json` includes `--no-watch`, which means Tauri
does **not** rebuild the Rust backend when you change `.rs` files. Only Vite's frontend
hot-reload is active.

To pick up Rust changes:

```bash
cd app/src-tauri
cargo build       # rebuild the backend
cd ../..
npm run tauri:dev # restart the app
```

`--no-watch` is intentional — Cargo's watch mode under Tauri's process tree can leave
stale subprocesses and doesn't surface compile errors in a useful way. A manual rebuild
followed by restart is the more reliable workflow.

### Debug logs

Hyvemind writes structured per‑ID JSONL logs to disk when launched with
`HYVEMIND_DEBUG=1` (which `tauri:dev` already sets). They live under `~/.hyvemind/debug/`
and are organised by session, review, and swarm. See
[`CLAUDE.md` → Debug Mode](CLAUDE.md#debug-mode-checking-logs) for the full guide.

### Sync vs async locks

The Rust backend mixes `std::sync::Mutex`/`RwLock` and `tokio::sync::Mutex`/`RwLock`.
Holding a `std::sync::*` guard across an `.await` is a latent bug — it blocks the
executor thread and can deadlock the scheduler. To make the discipline obvious at
call sites, prefer the type aliases in `app/src-tauri/src/state/sync.rs`:

| Alias | Backing type | Use when |
|---|---|---|
| `SyncMutex<T>` | `std::sync::Mutex<T>` | Guard never crosses `.await`. Pure in-memory critical section. |
| `SyncRwLock<T>` | `std::sync::RwLock<T>` | Same, but reader-heavy. |
| `AsyncMutex<T>` | `tokio::sync::Mutex<T>` | Guard must persist across `.await` (I/O, async fn calls). |
| `AsyncRwLock<T>` | `tokio::sync::RwLock<T>` | Same, reader-heavy and `.await`-crossing. |

Import them via `use crate::state::sync::{SyncMutex, AsyncMutex, ...}`. New code
should use the aliases instead of the fully-qualified `std::sync::*` /
`tokio::sync::*` paths so reviewers can tell at a glance whether the guard is
safe to hold across awaits.

## Pull request process

1. **Fork** the repo and create a topic branch off `main`:
   `git checkout -b fix/short-description` or `feat/short-description`.
2. **Keep PRs focused.** One logical change per PR. Easier to review, easier to revert.
3. **Write tests** for new behaviour where it makes sense — Vitest for frontend, Cargo
   tests for backend.
4. **Run tests + formatters locally** before pushing (see commands above).
5. **Update docs** if you change user‑visible behaviour. README, CHANGELOG, or the
   relevant prompt under `app/src-tauri/prompts/`.
6. **Push & open a PR** against `main`. Fill in the PR template.
7. **CI must pass** — all `ci.yml` jobs are required for merge.
8. **Address review feedback** by pushing additional commits. Squashing happens at merge
   time.

### Commit message style

Conventional but not strict. Prefix with the area touched if it helps:

```
[Hivemind] add per-round timeout knob
[Tasks] fix scrollback jitter when streaming long messages
[CI] cache cargo registry between jobs
docs: clarify Pi bundling story
```

The maintainer occasionally rewrites/squashes history before release. Don't depend on PR
commit shape being preserved long‑term.

### Adding a provider

Hyvemind uses an enum-dispatch pattern for LLM providers. The files you touch depend on
whether your provider speaks an existing API shape or needs a new one.

**OpenAI-compatible provider** — the common case (DeepSeek, Groq, Mistral, etc.)

1. Add a default entry to `seed_default_providers()` in
   `app/src-tauri/src/state/config.rs` (the `defaults` slice — each entry is
   `(id, display_name, provider_type, endpoint)`).
2. Add model definitions to `get_model_catalog()` in
   `app/src-tauri/src/commands/settings.rs`.
3. The frontend discovers providers from the config — no UI changes needed.

**Provider needing a new API shape** (non-OpenAI-compatible, or with custom auth)

1. Add a provider struct and its `call()` implementation in or alongside
   `app/src-tauri/src/providers/mod.rs`.
2. Add a variant to the `ProviderKind` enum and the corresponding match arm in
   `ProviderKind::call()`.
3. Add the new `provider_type` to the `ALLOWED_PROVIDER_TYPES` whitelist in
   `app/src-tauri/src/commands/settings.rs`.
4. Add dispatch logic in `ProviderRegistry::refresh_from_config_with_pi()` (in
   `providers/mod.rs`) to construct and register your variant from config.
5. Then follow steps 1–2 from the OpenAI-compatible list above (seed defaults, add
   models).

## Reporting bugs

Open a [bug report](https://github.com/Unravl/Hyvemind/issues/new?template=bug_report.yml)
and include:

- Hyvemind version + OS
- Steps to reproduce
- Expected vs actual behaviour
- Relevant logs from `~/.hyvemind/debug/` (scrub API keys before pasting!)

## Security

Don't open public issues for security vulnerabilities. See [`SECURITY.md`](SECURITY.md)
for responsible disclosure.

## License

By contributing, you agree that your contributions will be licensed under the
[MIT License](LICENSE).
