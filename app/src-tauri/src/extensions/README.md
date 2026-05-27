# Provider Extensions

In-tree modules that publish per-provider data (credits, usage, auth
status, future billing/rate-limit/catalog probes) into the app's Topbar
pills and Settings → **Provider Extensions** panel.

> Distinct from the **Pi Extensions** panel under Settings, which manages
> npm packages loaded into the Pi sidecar process. These two systems
> share a name but are otherwise unrelated.

## Adding a new extension in 4 steps

1. **Create a module** under `extensions/builtins/`, e.g.
   `extensions/builtins/fake_provider.rs`. Implement
   [`ProviderExtension`] (always) and one or more capability traits
   (currently [`UsageProvider`]).
2. **Register the module** in `extensions/builtins/mod.rs`:
   ```rust
   pub mod fake_provider;
   ```
3. **Wire it into `register_builtin_extensions`** in
   `extensions/mod.rs` (one match line per provider it should attach to).
4. *(Optional)* **Ship a bespoke Topbar widget** by adding a React
   component under `app/src/extensions/widgets/` and registering it in
   `app/src/extensions/registry.tsx` via `widgetRegistry.register(typeId, …)`.
   No bespoke widget? The default pill renders from
   `snapshot.headline.display` automatically.

No core/state/IPC code edits required.

## Worked example: OpenRouter Credits

`extensions/builtins/openrouter_credits.rs` (~200 LOC end-to-end):

```rust
pub struct OpenRouterCredits { provider_id: String, extension_id: String, base_url: String }

impl ProviderExtension for OpenRouterCredits {
    fn manifest(&self) -> ExtensionManifest { /* id, type_id, provider_id, capabilities=[Usage,Billing] */ }
    fn usage_provider(&self) -> Option<&dyn UsageProvider> { Some(self) }
}

#[async_trait]
impl UsageProvider for OpenRouterCredits {
    async fn fetch(&self, ctx: &ExtensionContext) -> Result<UsageSnapshot, ExtensionError> {
        let key = ctx.api_key(&self.provider_id).await
            .ok_or_else(|| ExtensionError::Auth("no API key".into()))?;
        let resp = ctx.http().get(format!("{}/auth/key", self.base_url))
            .bearer_auth(&key).send().await
            .map_err(|e| ExtensionError::Network(e.to_string()))?;
        // … parse, build metrics, return UsageSnapshot
    }
    fn refresh_interval_secs(&self) -> u64 { 300 }
}
```

Registration line in `extensions/mod.rs`:
```rust
for (id, pc) in providers {
    if id == "openrouter" || pc.endpoint.as_deref().map_or(false, |e| e.contains("openrouter.ai")) {
        let ext = builtins::openrouter_credits::OpenRouterCredits::new(id.clone(), pc.endpoint.as_deref());
        registry.register(Arc::new(ext)).ok();
    }
}
```

Bespoke widget registration (optional) in `app/src/extensions/registry.tsx`:
```ts
reg.register("openrouter_credits", OpenRouterCreditsPill);
```

## Contract rules — must-follow

- **Use `ctx.http()`** for every outbound HTTP request — never construct
  your own `reqwest::Client`. The shared client carries the project
  timeout (30 s) and reuses the connection pool.
- **Never hold a config guard across `.await`**. `ctx.api_key()` and
  `ctx.extension_settings()` clone-and-drop internally — call them, bind
  the result to a local, then perform I/O.
- **No `Debug` of `ExtensionContext` outside the redacting impl.** The
  context carries the live config (with API keys in memory). Accidental
  `?ctx` tracing would otherwise leak secrets.
- **Minimum `refresh_interval_secs` is 30 s.** Lower values are silently
  clamped at the poller boundary with a `warn!`.
- **`Unsupported` is terminal.** Returning `ExtensionError::Unsupported`
  parks the poller permanently for this run; the row stays visible in
  Settings labelled "Unsupported". Use this when the underlying API
  doesn't exist (e.g., Anthropic per-user usage without admin auth).
- **`Auth` / `Network` / `Parse` / `Internal` are transient.** The
  poller retries with exponential backoff; **after ~10 consecutive
  transient failures the row is effectively unusable** (it keeps
  retrying but at the backoff ceiling). Manual refresh from the
  Settings UI resets the failure counter.
- **`MissingApiKey` auto-recovers.** When a user supplies a new key via
  `save_api_key`, the existing flow calls `refresh_extension_registry`,
  which respawns all pollers. `spawn_pollers` performs an explicit
  **startup-refresh fan-out** (one `tokio::spawn` per extension that
  calls `perform_fetch_once` immediately) before the periodic loops
  begin; the periodic loops themselves only handle subsequent refreshes
  on their normal interval. The missing-key error therefore self-heals
  on the very next kickoff without a manual refresh click, and the
  `startup initial refresh: dispatching/complete` debug log lines make
  the behaviour verifiable.
- **`raw` is capped at 64 KB.** Larger raw payloads are dropped with a
  `warn!`; the structured `metrics` field still surfaces.
- **`UsageMetric.value: f64` is for ordering/thresholding only.** Always
  set `display` to the canonical string; the frontend renders `display`
  verbatim and does not re-format `value`.
- **Manual refresh has a 5-second cooldown.** Two rapid IPC calls
  yielding back-to-back fetches are rejected with a cooldown error.
- **Panic safety.** Tokio isolates panicking tasks: a panicking
  `fetch()` aborts its own task without disturbing the other poller
  tasks. The Settings row will silently stop updating; surface
  unrecoverable conditions explicitly via `ExtensionError::Internal`
  instead of panicking.

## Frontend hooks

- `useExtensions()` in `app/src/extensions/useExtensions.ts` returns
  `{ snapshots, isLoading, refresh, updateSettings }`.
- The `usage-snapshot-updated` Tauri event signals a row changed;
  `<ExtensionProvider>` consumes it and merges into local state.
- `<ExtensionTopbarSlot />` renders one pill per visible-ok snapshot.

## Tests

`extensions/tests.rs` covers: registry registration, duplicate-id
rejection, capability serde, manifests sorted deterministically,
poller `Ok`/`Err`/`Unsupported` paths, cancellation, and per-provider
matching in `register_builtin_extensions`.
