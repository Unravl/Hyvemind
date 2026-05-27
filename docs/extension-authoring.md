# Extension Authoring Guide

> Audience: contributors extending Hyvemind without modifying core. Three distinct surfaces share the word "extension" — pick the one that matches your use case.
>
> The authoritative contract reference for Part 1 lives at `app/src-tauri/src/extensions/README.md`. This guide is the how-to that sits on top of it: when in doubt about *behaviour* (retry semantics, lock-ordering, error variants), read the README; when in doubt about *how to add one*, read here.

Hyvemind supports three kinds of extension:

| Type | Layer | Lives in | When to use |
|---|---|---|---|
| Rust provider extension | Backend | `app/src-tauri/src/extensions/builtins/` | Polling a provider's usage / credit / balance API on a schedule and surfacing the snapshot in the topbar + Settings panel |
| Pi local extension | TypeScript, inside the Pi sidecar | `app/src-tauri/pi-extensions/` | Adding a tool Pi can call (e.g. the `hyvemind-providers` provider-registration tool, the `hyvemind-handoff` structured-output tools) |
| Frontend topbar widget | React renderer | `app/src/extensions/widgets/` | Adding a bespoke visual pill in the topbar driven by a usage snapshot (defaults to the generic pill when absent) |

A single feature often touches more than one. Adding a new usage-tracked provider typically means a Rust extension (Part 1) plus an optional bespoke topbar widget (Part 3). Adding a new structured tool that every Pi session should expose means a Pi local extension (Part 2).

No core / state / IPC code edits are required for any of the three.

---

## Part 1 — Rust provider extensions

### What they do

A Rust provider extension is an in-tree module that **periodically polls one provider for one scalar metric** (credits remaining, monthly usage, OAuth token expiry, rate-limit window). The poller writes a `UsageSnapshot` into the shared snapshot map and emits a `usage-snapshot-updated` Tauri event; the frontend then renders the snapshot as a topbar pill and a Settings row.

Do **not** use this surface for:

- One-shot probes (call the provider directly; the poller lives for the app's lifetime)
- Mutating provider state (the `UsageProvider::fetch` trait method is read-only by design)
- Anything that needs sub-30-second cadence (`MIN_REFRESH_INTERVAL_SECS = 30` at `app/src-tauri/src/extensions/poller.rs:38`; lower values are silently clamped)
- LLM dispatch (that's the entirely separate provider trait in `app/src-tauri/src/providers/`)

### The `ProviderExtension` trait

Declared at `app/src-tauri/src/extensions/traits.rs:19-32`. Every extension implements it:

```rust
pub trait ProviderExtension: Send + Sync + 'static {
    fn manifest(&self) -> ExtensionManifest;
    fn usage_provider(&self) -> Option<&dyn UsageProvider> { None }
    // billing_provider / rate_limit_probe / model_catalog reserved for future capabilities
}
```

The trait is intentionally minimal — the manifest is the only required method; every other capability is an opt-in accessor that defaults to `None`. Adding a new capability is a deliberate compile-time change (declare a new trait, add an accessor on `ProviderExtension`, extend the `Capability` enum at `app/src-tauri/src/extensions/types.rs:14-19`).

### `UsageProvider` (the only capability today)

Declared at `app/src-tauri/src/extensions/traits.rs:36-53`:

```rust
#[async_trait]
pub trait UsageProvider: Send + Sync {
    async fn fetch(&self, ctx: &ExtensionContext) -> Result<UsageSnapshot, ExtensionError>;
    fn refresh_interval_secs(&self) -> u64 { 300 }  // retained for back-compat; poller reads global
}
```

Note on `refresh_interval_secs`: the poller now reads the user-tunable global value from `context.poll_interval_secs()` (`app/src-tauri/src/extensions/context.rs:87-91`) instead of the per-extension method. The trait method is retained `#[allow(dead_code)]` for backward compatibility — implement it for documentation, but expect the global setting to win.

`BillingProvider`, `RateLimitProbe`, and `ModelCatalog` are reserved as comments at `app/src-tauri/src/extensions/traits.rs:29-31` for future work.

### Wire-level types

All defined at `app/src-tauri/src/extensions/types.rs`. They are `Serialize + Deserialize` and cross the Tauri IPC boundary unchanged.

| Type | Location | Purpose |
|---|---|---|
| `ExtensionManifest` | `types.rs:82-96` | `{ id, type_id, provider_id, display_name, description, capabilities, requires_api_key, docs_url }`. `id` is the composite `format!("{type_id}:{provider_id}")` and is the registry's HashMap key. |
| `UsageSnapshot` | `types.rs:62-79` | `{ extension_id, provider_id, fetched_at, headline, metrics, raw }`. `headline` drives the topbar pill; `metrics` is the full list shown in the Settings popover. `raw` is capped at 64 KB by the poller (`RAW_PAYLOAD_CAP_BYTES` at `poller.rs:45`). |
| `UsageMetric` | `types.rs:45-59` | `{ key, label, display, value, kind, tone }`. `display` is rendered verbatim by the frontend; `value: f64` is only for ordering / thresholding. Always set `display` to the canonical string with the units you want shown. |
| `ExtensionError` | `types.rs:99-118` | `Unsupported` is terminal — the poller exits. `Auth`, `Network`, `Parse`, `Internal` are transient and backed-off via `BackoffCalculator`. |
| `MetricKind`, `Tone` | `types.rs:23-42` | Enums for grouping/formatting and colour hints. Tone maps to `crit`/`warn`/`ok`/`neutral` CSS classes on the frontend. |

### `ExtensionContext` — what `fetch()` sees

Defined at `app/src-tauri/src/extensions/context.rs:26-32`. Owned by the poller, passed by reference to every `fetch()`:

| Accessor | Returns | Notes |
|---|---|---|
| `ctx.http()` | `&reqwest::Client` (`context.rs:95-97`) | **Always use this** — never construct your own `reqwest::Client`. Shared connection pool, 30 s timeout. |
| `ctx.api_key(provider_id).await` | `Option<String>` (`context.rs:67-70`) | Re-reads from config every call. Clone the result into a local and drop the guard before I/O — don't hold a config read lock across `await`. |
| `ctx.extension_settings(extension_id).await` | `ExtensionUserSettings` (`context.rs:73-79`) | User toggles for `enabled` / `show_in_topbar` / `preferences`. |
| `ctx.poll_interval_secs().await` | `u64` (`context.rs:87-91`) | Clamped to `[MIN_REFRESH_INTERVAL_SECS, EXTENSION_POLL_INTERVAL_MAX_SECS]`. The poller already reads this — extensions rarely need it. |
| `ctx.pi_manager()` | `&Arc<PiManager>` (`context.rs:99-102`) | For extensions that probe Pi state. Not currently used by any builtin. |
| `ctx.data_dir()` | `&PathBuf` (`context.rs:104-107`) | `~/.hyvemind/`. |

The `Debug` impl is **manually redacted** (`context.rs:37-46`) so the live config (which carries API keys in memory) can never leak via `?ctx` in a tracing site. Do not derive `Debug` on this type — match the redacting pattern.

### The polling loop

Defined at `app/src-tauri/src/extensions/poller.rs`. The lifecycle is:

1. **Startup-refresh fan-out** (`poller.rs:102-187`) — one `tokio::spawn` per extension that calls `perform_fetch_once_with_cancel` immediately, in parallel. Acquires the per-extension `RefreshLocks` mutex so it serialises against any concurrent manual `refresh_usage_snapshot` IPC call.
2. **Periodic loop** (`poller.rs:189-309`) — one `tokio::spawn` per extension. Sleeps `ctx.poll_interval_secs()`, then fetches, then repeats. Re-reads the interval each tick so Settings changes take effect on the next sleep without restarting the task.
3. **Outcome handling** (`poller.rs:284-307`) — `Ok` / `Disabled` reset the backoff counter; `Unsupported` exits the loop permanently; `Err` increments `consecutive_errors` and computes `BackoffCalculator::default().calculate(consecutive_errors)` for the next sleep.

Each fetch is wrapped in `tokio::time::timeout(FETCH_TIMEOUT_SECS, …)` (`poller.rs:42`, default 30 s) and raced against the shared `CancellationToken` so shutdown / registry refresh aborts in-flight HTTP without waiting.

Successful snapshots are written into the shared `RwLock<HashMap<String, SnapshotEntry>>` and a `usage-snapshot-updated` event is emitted via `emit_event` at `poller.rs:506-537`. The event payload deliberately omits `snapshot.raw` to keep IPC bandwidth small — frontend code that needs `raw` re-fetches via `get_usage_snapshots()`.

### Worked example — `openrouter_credits.rs` end-to-end

Source: `app/src-tauri/src/extensions/builtins/openrouter_credits.rs`. Worth reading in full (~320 lines, single file).

1. **Struct + constructor** (`openrouter_credits.rs:32-54`). Captures `provider_id`, the pre-formatted `extension_id` (`"openrouter_credits:{provider_id}"`), and a `base_url` derived from the provider's configured endpoint or `DEFAULT_BASE = "https://openrouter.ai/api/v1"`.
2. **`ProviderExtension` impl** (`openrouter_credits.rs:56-75`). Builds the manifest with `Capability::Usage + Capability::Billing` and `requires_api_key: true`. Returns `Some(self)` from `usage_provider()` so the poller knows there's a fetch capability.
3. **Response shapes** (`openrouter_credits.rs:77-106`). Two `Deserialize` structs for the two endpoints (`/auth/key` and `/credits`).
4. **`UsageProvider::fetch`** (`openrouter_credits.rs:108-318`):
   - Pulls the API key via `ctx.api_key(&self.provider_id).await` and errors with `ExtensionError::Auth` if absent (`openrouter_credits.rs:110-117`).
   - Phase 1: probes `/credits` for account-wide balance (best-effort; failures swallowed to fall through to phase 2 — `openrouter_credits.rs:122-152`).
   - Phase 2: hits `/auth/key` (required). Maps HTTP `401`/`403` to `ExtensionError::Auth`, other non-2xx to `Network`, JSON parse errors to `Parse` (`openrouter_credits.rs:154-185`).
   - Builds the headline metric: prefers `/credits` remaining over `/auth/key` remaining, with tone gradient `crit < $0.10`, `warn < $1.00`, else `ok` (`openrouter_credits.rs:200-260`).
   - Returns a `UsageSnapshot` with `chrono::Utc::now().timestamp()`, the assembled `metrics`, and the raw JSON body for power users.
5. **`refresh_interval_secs` override** (`openrouter_credits.rs:316-318`). Returns 300 — kept for back-compat, ignored by the poller.

Registration line at `app/src-tauri/src/extensions/mod.rs:58-66` attaches one instance per provider whose `id == "openrouter"` or whose endpoint contains `openrouter.ai`.

### Adding a new one — step by step

1. **Create the module** at `app/src-tauri/src/extensions/builtins/yourprovider_usage.rs`. Implement `ProviderExtension` + `UsageProvider`. Use `openrouter_credits.rs` as the template.
2. **Declare the module** at `app/src-tauri/src/extensions/builtins/mod.rs`:
   ```rust
   pub mod yourprovider_usage;
   ```
3. **Register instances** in `register_builtin_extensions` at `app/src-tauri/src/extensions/mod.rs:46-169`. Add a `for (id, pc) in providers { … }` block mirroring the existing patterns. Decide the matching rule: by canonical `id`, by endpoint substring, by `provider_type`, or any combination. Call `registry.register(Arc::new(ext)).ok()` — duplicates are rejected by `registry.rs:36-49`, which is desired.
4. **Run `cargo test`** from `app/src-tauri`. The suite at `app/src-tauri/src/extensions/tests.rs` covers registry registration, duplicate-id rejection, capability serde, manifest sort order, poller outcomes, cancellation, and per-provider matching. Add a unit test for any non-trivial parsing in your module.
5. **Verify polling fires + emits `usage-snapshot-updated`**. Configure the provider in Settings, launch with `HYVEMIND_DEBUG=1`, watch `~/.hyvemind/debug/general.jsonl.YYYY-MM-DD` for the `startup initial refresh: dispatching/complete` lines emitted by `spawn_pollers`. Confirm the Settings → Provider Extensions row appears and a topbar pill renders if `snapshot.headline` is set and the user kept `show_in_topbar` enabled.

### Contract rules — must-follow

See the canonical list at `app/src-tauri/src/extensions/README.md:72-115`. Critical points:

- **Never construct your own `reqwest::Client`** — use `ctx.http()`.
- **Never hold a config read guard across `await`** — read into a local, drop, then I/O.
- **`Unsupported` is terminal**; transient errors auto-backoff up to ~10 attempts and then sit at the ceiling. Manual refresh from Settings resets the counter.
- **`MissingApiKey` self-heals** — saving a new key triggers `refresh_extension_registry`, which respawns pollers and runs an immediate fetch via the startup fan-out.
- **`raw` capped at 64 KB**; larger payloads are dropped with a warn log.
- **`UsageMetric.value` is for ordering/thresholding only** — always set `display`.
- **Manual refresh has a 5-second cooldown**.
- **Panic safety**: tokio isolates panicking tasks; surface unrecoverable conditions via `ExtensionError::Internal` instead of panicking.

---

## Part 2 — Pi local extensions

### What they do

Pi (`@earendil-works/pi-coding-agent`) is the TypeScript coding-agent runtime Hyvemind delegates to. Pi supports loadable extensions that register new **tools** (functions the model can call) or new **providers** (LLM backends). Hyvemind ships two of its own under `app/src-tauri/pi-extensions/`:

- **`hyvemind-providers`** — reads a manifest from the `HYVEMIND_PI_PROVIDERS_JSON` env var, calls `pi.registerProvider(id, …)` for each entry, and merges fetched `/models` responses with a static fallback table so OpenAI-compatible providers configured in Hyvemind's Settings appear inside Pi without a Pi restart.
- **`hyvemind-handoff`** — calls `pi.registerTool(…)` for ~16 structured-output tools (`submit_handoff`, `submit_plan`, `submit_questions`, `submit_features`, `submit_task_meta`, `submit_context`, `submit_review_prompt`, `submit_scout_result`, `submit_guard_result`, `submit_verdicts`, `submit_review`, and the four stability-test variants). Each `execute` body is a no-op echo — the Rust backend captures the model's tool-call `args` directly off the JSONL `tool_execution_start` event and bypasses delimiter parsing entirely.

### Structure of a Pi local extension

Two files per extension, both at `app/src-tauri/pi-extensions/<name>/`:

| File | Purpose |
|---|---|
| `index.ts` | Default-exports `async function (pi: ExtensionAPI) { … }` that registers tools / providers. See `hyvemind-providers/index.ts:230-256` for a provider example, `hyvemind-handoff/index.ts:431-467` for a tool-registration example. |
| `package.json` | `{ name, version, type: "module", pi: { extensions: ["./index.ts"] } }`. The `pi.extensions` array tells Pi which entry files to load. See `hyvemind-providers/package.json:1-12` and `hyvemind-handoff/package.json:1-12`. |

### How Pi discovers an extension

The discovery flow is wired into `scripts/build-pi.sh`:

1. The `LOCAL_EXTENSIONS` array at `scripts/build-pi.sh:25` lists the directory names under `app/src-tauri/pi-extensions/` to bundle.
2. `copy_local_extensions` at `scripts/build-pi.sh:28-39` is called both on cache-hit (line 53) and on a fresh build (line 124). It recursive-copies each listed directory into `app/src-tauri/binaries/pi-extensions/<name>/`.
3. The bundled directory is shipped via `tauri.conf.json`'s `bundle.resources` glob and loaded by Pi via jiti at runtime (Pi extensions are **not** bun-compiled into the binary — they need real on-disk module resolution).
4. The flat `node_modules/` that ships alongside the extension packages (`scripts/build-pi.sh:131-139`) is what resolves their `import` statements.

`npm run prepare-pi` (invoked automatically by `tauri:dev` / `tauri:build`) runs `build-pi.sh`; the stamp file at `app/src-tauri/binaries/.pi-version` short-circuits when the pinned version hasn't changed — but `copy_local_extensions` still runs in the cache-hit path, so editing a local extension and re-running `tauri:dev` always picks up the new source.

### Adding a new Pi local extension — step by step

1. **Create the directory** at `app/src-tauri/pi-extensions/your-extension/`.
2. **Write `package.json`** mirroring `hyvemind-handoff/package.json:1-12`:
   ```json
   { "name": "your-extension", "version": "0.1.0", "private": true,
     "type": "module", "pi": { "extensions": ["./index.ts"] } }
   ```
3. **Write `index.ts`** with a default export `async function (pi: ExtensionAPI) { … }`. Import types from `@earendil-works/pi-coding-agent` (use `@mariozechner/pi-coding-agent` if the package version you're targeting still uses the old scope — `hyvemind-providers/index.ts:1` does). Call `pi.registerTool(…)` for tools and `pi.registerProvider(…)` for providers.
4. **Add it to `scripts/build-pi.sh`** by appending the directory name to the `LOCAL_EXTENSIONS` array at `scripts/build-pi.sh:25`.
5. **Do not bump `pi-version.txt`** unless you're also upgrading the upstream Pi runtime — local-extension changes are independent of the pinned Pi version.
6. **Test in `tauri:dev`**. Watch stderr for the `[your-extension] …` logs you `console.error`'d in `index.ts` — Pi forwards extension stderr to its own stderr, which Hyvemind captures into `~/.hyvemind/debug/sessions/{session_id}.jsonl` (with `HYVEMIND_DEBUG=1`). If the extension fails to load, Pi will log it; if `registerTool` rejects a schema, wrap the call in try/catch like `hyvemind-handoff/index.ts:436-464` does so one bad tool doesn't take the rest down.

### Worked example — `hyvemind-handoff/index.ts`

Source: `app/src-tauri/pi-extensions/hyvemind-handoff/index.ts` (~470 lines).

1. **No-op echo** (`hyvemind-handoff/index.ts:29-34`). Every tool's `execute` returns the same boilerplate `"Submission received."` text. The actual `args` payload was captured by the Rust side off the JSONL stream — the model only needs confirmation that its submission landed.
2. **TypeBox-compatible JSON-Schema literals** (`hyvemind-handoff/index.ts:38-307`). Each tool defines a plain `as const` JSON-Schema object. Pi validates them with Ajv at call time. Keeping them as literals avoids pulling TypeBox in as a runtime dependency.
3. **Tool table** (`hyvemind-handoff/index.ts:316-429`). Array of `{ name, label, description, parameters }` — the registration loop at `hyvemind-handoff/index.ts:436-465` iterates this and calls `pi.registerTool` for each. Per-tool try/catch ensures a single rejected schema doesn't take the others down.
4. **Schema-to-Rust pairing comment** (`hyvemind-handoff/index.ts:11-21`). When you change a schema, the comment lists the matching Rust/TS deserialiser you also need to update — for example, `submit_handoff` pairs with `app/src-tauri/src/core/handoff.rs::WorkerHandoff`.

`hyvemind-providers/index.ts` is the other worth-reading example: it shows how to fetch a remote `/models` list with a 2.5 s timeout (`hyvemind-providers/index.ts:202-221`), merge with a static fallback (`hyvemind-providers/index.ts:223-228`), apply per-model reasoning-capability overrides (`hyvemind-providers/index.ts:62-109`), and ultimately call `pi.registerProvider(…)` (`hyvemind-providers/index.ts:246-253`).

### Pi extension authoring tips

- Use `console.error(…)` not `console.log(…)` for status output. Pi reserves stdout for the JSONL RPC protocol; anything you write to stdout will desync Pi's transport.
- Keep startup cheap — the extension is loaded synchronously on every Pi process spawn. Defer heavy work into the body of `registerTool`'s `execute` callback or behind a `setTimeout`.
- Do not rely on `npm install` in the extension directory — `scripts/build-pi.sh` only copies the source files, not per-extension `node_modules`. Shared deps must come from the flat `node_modules/` produced by the build script's `bun add` of the `EXTENSIONS` array.

---

## Part 3 — Frontend topbar widgets

### What they do

A frontend topbar widget is a React component that renders a bespoke pill for one extension's snapshot. When an extension has no registered widget, `DefaultUsagePill` (`app/src/extensions/widgets/DefaultUsagePill.tsx`) renders a generic pill from `snapshot.headline.display` automatically — so you only write a bespoke widget when you want non-default visual treatment (badges, custom layout, sparklines).

### Registry pattern

The registry lives at `app/src/extensions/registry.tsx:39-67`. It's a module-level singleton keyed by `manifest.type_id` (e.g. `"openrouter_credits"`) so every instance of a multi-instance extension shares one widget:

```tsx
class ExtensionWidgetRegistry {
  register(typeId: string, widget: ExtensionWidget) { … }
  get(typeId: string): ExtensionWidget | undefined { … }
}
export const widgetRegistry = new ExtensionWidgetRegistry();

export function registerWidgets(reg = widgetRegistry) {
  reg.register("openrouter_credits", OpenRouterCreditsPill);
  // …
}
```

`registerWidgets()` is called once during app init. The lookup happens inside `ExtensionTopbarSlot` at `app/src/extensions/registry.tsx:158-166`:

```tsx
const Widget = widgetRegistry.get(entry.manifest.type_id) ?? DefaultUsagePill;
return <SortablePill key={entry.manifest.id} id={entry.manifest.id}>
  <Widget entry={entry} />
</SortablePill>;
```

### Widget interface

`ExtensionWidget` is declared at `app/src/extensions/registry.tsx:34`:

```tsx
export type ExtensionWidget = ComponentType<{ entry: SnapshotEntry }>;
```

The component receives a single `entry: SnapshotEntry` (mirror of the Rust type — see `app/src/extensions/types.ts:66-73`) and should:

- Return `null` when `entry.status !== "ok"` or `entry.snapshot?.headline` is absent (see `OpenRouterCreditsPill.tsx:16` and `DefaultUsagePill.tsx:15` for the canonical guard).
- Map `headline.tone` (`"ok" | "warn" | "crit" | "neutral"`) to your CSS class table. The `TONE_CLASSES` constant in both example pills is the convention.
- Set a `title` attribute for the hover tooltip — the topbar pill is small so the full label/value lives in the tooltip.
- Read `headline.display` directly. Never re-format `headline.value`; the backend already produced the canonical string.

### Subscribing to `usage-snapshot-updated`

You typically do **not** subscribe in the widget. Subscription is centralised in `app/src/extensions/ExtensionProvider.tsx:65-118`, which:

1. Loads the initial snapshot list via `ipc.getUsageSnapshots()` (`ExtensionProvider.tsx:55`).
2. Registers a single `usage-snapshot-updated` listener via `onUsageSnapshotUpdated` (`ExtensionProvider.tsx:77-110`) that does partial updates by `extension_id`.
3. Polls `getUsageSnapshots()` every 60 s as a safety net (`ExtensionProvider.tsx:40, 72-74`) in case an event is missed (e.g. webview backgrounded).
4. Exposes `{ snapshots, isLoading, refresh, updateSettings }` via `useExtensions()` (`app/src/extensions/useExtensions.ts:6-14`).

Inside a widget you only need `entry: SnapshotEntry`. If you need cross-cutting access (e.g. for a manual-refresh button), call `useExtensions()` and use `refresh(extensionId)` / `updateSettings(extensionId, …)`.

### Adding a new widget — step by step

1. **Create the file** at `app/src/extensions/widgets/YourProviderPill.tsx`. Copy `OpenRouterCreditsPill.tsx` (`app/src/extensions/widgets/OpenRouterCreditsPill.tsx`) as the template — it's ~30 LOC and shows the tone map + tooltip pattern.
2. **Register it** in `app/src/extensions/registry.tsx:59-67` by adding a line to `registerWidgets`:
   ```tsx
   reg.register("yourprovider_usage", YourProviderPill);
   ```
   The `type_id` string must exactly match the `type_id` on your Rust extension's `ExtensionManifest`.
3. **Write a Vitest** at `app/src/extensions/__tests__/YourProviderPill.test.tsx`. Existing tests under `app/src/extensions/__tests__/` cover render-when-ok, render-nothing-on-error, tone-class mapping, and the default-pill fallback. Run with `npm test` from `app/`.
4. **Verify in `tauri:dev`**. The pill auto-appears once `snapshot.status === "ok"`, `snapshot.headline` is set, and `user_settings.show_in_topbar === true`. Drag to reorder (the order persists to `localStorage` via `app/src/extensions/topbarOrder.ts`).

The topbar reorder + persistence is handled by `applyOrder` and `saveOrder` at `app/src/extensions/topbarOrder.ts:47-69` — you don't need to touch this code, but be aware that pill order is per-user and stale IDs are dropped automatically.

---

## Cross-cutting concerns

### Testing

| Surface | How to run |
|---|---|
| Rust provider extension | `cargo test` from `app/src-tauri`. The suite at `app/src-tauri/src/extensions/tests.rs` covers registry registration, duplicate-id rejection, capability serde, deterministic manifest sort, poller outcomes (`Ok` / `Err` / `Unsupported`), cancellation, and per-provider matching in `register_builtin_extensions`. Add a unit test for any non-trivial parsing in your `fetch()`. |
| Pi local extension | Manual via `npm run tauri:dev` from `app/`. There is no automated test harness for Pi extensions — they need a live Pi process. Add `console.error("[your-ext] …")` markers in `index.ts` and search for them in `~/.hyvemind/debug/sessions/*.jsonl`. |
| Frontend topbar widget | `npm test` from `app/`. Mirror an existing test under `app/src/extensions/__tests__/` (e.g. `DefaultUsagePill.test.tsx`, `OpenRouterCreditsPill` equivalent). Cover: renders when `status === "ok"`, returns `null` on error/loading/disabled, tone class mapping, tooltip text. |

### Debugging

| Surface | Where to look |
|---|---|
| Rust provider extension | Launch with `HYVEMIND_DEBUG=1`. Polling events land in `~/.hyvemind/debug/general.jsonl.YYYY-MM-DD` (the per-extension span name is `ext_poller`, see `app/src-tauri/src/extensions/poller.rs:180, 207`). Look for `startup initial refresh: dispatching/complete`, `extension poller starting`, `extension fetch failed — backing off`. |
| Pi local extension | Events fire inside the host Pi subprocess, so they show up in the per-session debug log: `~/.hyvemind/debug/sessions/{session_id}.jsonl`. The extension's own `console.error` output is captured here too. Pi schema-rejection messages from `pi.registerTool` show up at extension-load time, before any prompt is sent. |
| Frontend topbar widget | Open the Tauri devtools (Cmd+Option+I on macOS) and watch the console. `ExtensionProvider.tsx:135` warns on rejected manual refreshes (5-second cooldown collisions, in-flight collisions). Snapshot deltas land via `usage-snapshot-updated` — log them inside `onUsageSnapshotUpdated` if you suspect the event isn't firing. |

### CI checks per kind

| Surface | Checks |
|---|---|
| Rust provider extension | `cargo check`, `cargo test`, `cargo fmt --check`, `cargo clippy -- -D warnings` (all run from `app/src-tauri` per `CLAUDE.md §Quick Reference`). |
| Pi local extension | No direct CI gate. The Pi build (`scripts/build-pi.sh`) runs as part of `npm run tauri:build` and will fail with `[build-pi] missing local extension source: …` if the directory is absent or `package.json` is malformed. There is no static type-check of Pi extension `index.ts` — TypeScript-check yours locally with `npx tsc --noEmit -p .` inside the extension directory if you want one. |
| Frontend topbar widget | `npm test`, `npx tsc --noEmit` (from `app/`). The widget tests run in jsdom — anything that calls `localStorage` directly should be guarded (see `app/src/extensions/topbarOrder.ts:16-19, 32-35` for the convention). |

### Related reading

- `app/src-tauri/src/extensions/README.md` — contract reference for Part 1 (must-follow rules, retry semantics, lock-ordering).
- `docs/providers.md` — the **other** Hyvemind provider surface (LLM dispatch). Do not confuse the two: `providers/mod.rs` is for LLM calls; `extensions/builtins/` is for polling provider metadata.
- `docs/architecture.md` — system component map, including where the extension subsystem sits relative to `AppState`.
- `docs/developer-cookbook.md` — task-oriented recipes for adjacent contributor work (adding an IPC command, a tunable, a screen).
- `PRODUCT.md §7 "Provider abstraction"` — product-level rationale for the two-surface split.
- `CLAUDE.md §Tauri Events` — `usage-snapshot-updated` listener inventory.
