# Frontend Architecture

Contributor-facing deep-dive on the Hyvemind React shell. CLAUDE.md `§Frontend Architecture` is the index (screens table, listener index, provider summary). **This file is the why and how** — provider-tree nesting, the six `taskRuntime.tsx` sub-contexts, the reducer surface, singleton event-store internals, IPC chokepoint, design-system primitives, and step-by-step recipes for the common changes a frontend agent makes.

If you only need "what file owns X" or "what event fires Y", read CLAUDE.md first. If you need to understand *why* there are six contexts inside `taskRuntime.tsx`, or *how* `swarmActivityStore` hydrates and dedupes against live events, read this.

---

## Stack & conventions

Defer to CLAUDE.md `§Frontend Architecture → Stack` for the full table. The non-obvious choices worth re-stating up front:

- **No state library**. Audit 6.7 (referenced throughout `taskRuntime.tsx`) split the monolithic context into six sliced contexts rather than reach for Redux. Use the narrowest hook you can.
- **No `react-router`**. Navigation is a `switch` on `nav.tab` in `app/src/App.tsx:363-390`. `go(tab, params)` is the entire navigation API.
- **No HMR**. `vite.config.ts` disables HMR; `tauri:dev` reloads the renderer in full on save to avoid races with the Tauri host's teardown.
- **No raw HTML in markdown**. `react-markdown` is loaded without `rehype-raw`. CSP and XSS guarantees rely on it (see CLAUDE.md `§CSP`).
- **No direct LLM provider HTTP from the renderer**. Every model call goes through Rust via Tauri IPC. CSP `connect-src` is locked down accordingly.
- **Tests**: Vitest + Testing Library + jsdom under `**/__tests__/`. Polyfills in `app/src/test/setup.ts`.

---

## Provider tree

Assembled in `app/src/App.tsx:627-727` inside one `<Sentry.ErrorBoundary>`. Nesting order matters: child providers may read from parents during initial render, and several providers register Tauri listeners that must outlive subscribers.

```
<Sentry.ErrorBoundary>                              App.tsx:628
  <SettingsProvider>                                App.tsx:664   — caches SettingsResponse
    <ProvidersProvider>                             App.tsx:665   — caches ProviderInfo[]
      <CompletionSoundLoader />                     App.tsx:666   — sound-config side effect
      <ErrorModalProvider onFix={…}>                App.tsx:667   — global error surface + Sentry capture
        <ToastProvider>                             App.tsx:671   — toast queue
          <ProjectContext.Provider>                 App.tsx:672   — selected project + localStorage list
            <PiStatusContext.Provider>              App.tsx:675   — Pi gate (locks nav when Pi missing)
              <ContextMenuProvider>                 App.tsx:676   — right-click menu state
                <ExtensionProvider>                 App.tsx:681   — topbar pill order + snapshot cache
                  <TestRunProvider>                 App.tsx:682   — active stability test run
                    <TaskRuntimeProvider go={go}>   App.tsx:683   — heavyweight runtime (see next section)
                      <Topbar /> <Sidebar /> <ScreenRouter />
                      <QuickTaskDialog /> <ShortcutCheatsheet /> <InspectorOverlay />
```

### What each provider owns and subscribes to

| Provider | File | Owns | Subscribes to |
|---|---|---|---|
| `SettingsProvider` | `app/src/lib/SettingsProvider.tsx:56-148` | `SettingsResponse` cache; `useSettings()`, `useSetting<K>()` | `default-model-changed`, `default-project-path-changed`, `default-hivemind-changed` (lines 97-140) |
| `ProvidersProvider` | `app/src/lib/ProvidersProvider.tsx:53-134` | `ProviderInfo[]` + memoised `configured` subset | `usage-snapshot-updated`, debounced 250 ms (lines 95-121) |
| `ErrorModalProvider` | `app/src/components/ErrorModal.tsx` | Global error modal + Sentry capture. `onFix` accepts a prompt that opens a Task in `tasks` tab | — |
| `ToastProvider` | `app/src/components/Toast.tsx` | Toast queue + dismissal timers | — |
| `ProjectContext.Provider` | `app/src/App.tsx:672-674` | Active project, projects list, add/remove/update (persisted to `hyvemind:projects` localStorage) | — |
| `PiStatusContext.Provider` | `app/src/App.tsx:61-79`, `:675` | `{ installed, loading, refresh }`. `LOCKED_WHEN_NO_PI` set at `App.tsx:69-79` gates navigation | — |
| `ContextMenuProvider` | `app/src/components/ContextMenu.tsx` | Right-click menu state, wires Quick Task and inspector entries | — |
| `ExtensionProvider` | `app/src/extensions/ExtensionProvider.tsx` | Topbar pill order (localStorage), usage snapshots per extension | `usage-snapshot-updated` |
| `TestRunProvider` | `app/src/state/TestRunProvider.tsx:81-110` | Active stability-test run state (rehydrates via `get_active_test_run` at mount) | `test-progress` (single app-lifetime listener) |
| `TaskRuntimeProvider` | `app/src/lib/taskRuntime.tsx:906-5087` | See next section | `chat-event`, `nurse-event`, `hivemind-progress` (via singleton store), `default-*-changed` (its own copies for ref-mirrored values) |

`SettingsProvider` and `ProvidersProvider` deliberately sit *inside* the Sentry boundary so a failed initial IPC fetch surfaces the friendly fallback panel rather than crashing the host.

---

## taskRuntime.tsx — the heavyweight

`app/src/lib/taskRuntime.tsx` is ~5,090 LOC. It owns every Tasks-view conversation, every Hivemind review flow that originates from a Task, swarm-question handling, plan-mode/implement-mode transitions, Nurse intervention bubbles, and the message reducer. **Touch it carefully** — most regressions come from breaking the audit-6.7 split that keeps unrelated screens from re-rendering on every streamed token.

### The six sub-contexts (audit 6.7)

The legacy `TaskRuntimeContext` exposed a single mega-object whose value changed on every streaming chunk — every consumer re-rendered on every token. Audit 6.7 split the public surface into six narrower contexts. The provider populates all six inside one render so they stay in sync; nothing about the runtime logic moved. The legacy `useTaskRuntime()` hook is preserved as a compatibility shim that composes the six slices.

| Context | Hook | Defined at | What it owns | When it changes |
|---|---|---|---|---|
| `TaskDraftContext` | `useTaskDrafts()` | `taskRuntime.tsx:835`, hook at `:855` | Composer draft text held in a **ref**, not state. `getDraft(id)` / `setDraft(id, value)`. | **Never re-renders.** Identity is stable — typing in the composer does not trigger any React re-render. Persisted to `hyvemind:task-drafts` localStorage with 500 ms debounce. |
| `TaskListContext` | `useTaskList()` | `:836`, hook `:862` | Sidebar list `localTasks: TaskListItem[]`, current `activeId`, plus mutators `setActiveTask` / `setLocalTasks`. | Task created / deleted / reordered / activated. |
| `TaskRuntimeStateContext` | `useTaskRuntimeState()` | `:837`, hook `:870` | Hot streaming state: `tasks: Record<string, TaskRuntimeState>`, `streamingTaskIds`, `updateTask`. | **On every chunk** for the streaming task — subscribe sparingly. The Tasks message panel reads this; the sidebar list does not. |
| `TaskActionsContext` | `useTaskActions()` | `:838`, hook `:877` | All dispatch handlers: `createTask`, `submitMessage`, `stopTask`, `deleteTask`, `triggerReviewForTask`, `retryReview`, `implementPlan`, `answerQuestions`, `submitSwarmAnswers`, `skipSwarmQuestions`, `resumeReview`, `replayReview`. | Effectively never after mount — every handler is `useCallback`-stabilised. |
| `HivemindOptionsContext` | `useHivemindOptions()` | `:839`, hook `:884` | Hivemind picker options `hivemindOptions: HivemindSummary[]` + `refreshHivemindOptions(prefetched?)`. | User creates / deletes / edits a Hivemind. |
| `DefaultsContext` | `useDefaults()` | `:840`, hook `:892` | `defaultModel`, `defaultProjectPath`, `defaultHivemind` — runtime's ref-mirrored copy. Source of truth is `SettingsProvider`; mirror exists so the provider can read defaults without an extra `useContext`. | When the corresponding `default-*-changed` Tauri event lands (effects at `:1194-1257`). |

> **Naming caveat**: CLAUDE.md `§State management` (and earlier audit notes) lists slices called `ChatSessionsContext`, `HivemindFlowContext`, `ReviewFlowContext`, etc. Those names never landed — the implementation defines the six contexts in the table above. Treat this doc as authoritative; CLAUDE.md will be reconciled in a later pass.

### The reducer pattern (`lib/taskReducer.ts`)

`app/src/lib/taskReducer.ts` (~2,300 LOC) is a pure-functional reducer over `TaskRuntimeState`. The provider wraps every Tauri event in a `TaskEvent` and folds it into the right task with `applyTaskEvent(state, event, defaultModel)`.

**Entry points the provider uses:**

| Function | Location | Purpose |
|---|---|---|
| `applyTaskEvent` | `taskReducer.ts:994` | Master reducer. Returns the new `TaskRuntimeState`, or referentially the same state if the event was a no-op. The provider's `setTasks` short-circuits on identity to avoid re-render storms. |
| `mapChatEventToTaskEvent` | `taskReducer.ts:2004` | Translates `chat-event` (Pi stdout streaming, tool calls, usage, completion) into the reducer's internal `TaskEvent` union. |
| `mapHivemindEventToTaskEvent` | `taskReducer.ts:2214` | Translates `hivemind-progress` (round / merge / completion) into `TaskEvent`s. |
| `mapNurseEventToTaskEvent` | `taskReducer.ts:2263` | Translates `nurse-event` Lifecycle variants into the inline Nurse-card stream events. |
| `makeInitialTaskState` | `taskReducer.ts` | Builds a fresh `TaskRuntimeState` on `createTask`. |
| `resetSessionStats` | `taskReducer.ts` | Wipes per-session usage/TPS on a session boundary (used when respawning Pi). |
| `reviewInterruptedFromSnapshot` | `taskReducer.ts` | Converts a `ResumableReviewSnapshot` (IPC) into the `ReviewInterruptedState` the UI renders for crash recovery. |
| `hasUnansweredQuestions` | `taskReducer.ts` | Predicate used to gate plan progression. |

**Invariants the reducer relies on:**

- **Pure functions, no side effects.** The reducer never reads `window`, `localStorage`, or `tasksRef.current` — all of those live in the provider. This is what keeps the unit-test suite under `lib/__tests__/taskReducer*.test.ts` deterministic.
- **Identity-preserving short-circuit.** When an event would produce the same state, return the input by reference. The provider's `setTasks((prev) => next === cur ? prev : { ...prev, [id]: next })` pattern (seen throughout `taskRuntime.tsx`, e.g. `:1728-1733`) relies on this.
- **Messages append-only inside an asst bubble**. Each streaming chunk patches the last assistant message in place via index lookup; the array is rebuilt with `[...messages]` only when a new bubble is opened.

### How the `chat-event` stream wires into reducer dispatches

The single most important event path. From `taskRuntime.tsx:1675-2362`:

1. `onChatEvent` registers one Tauri listener for `"chat-event"` (the runtime is *not* multiplexed through a singleton store — there's no second consumer outside the runtime).
2. Lookup `taskId` via `sessionIdToTaskIdRef.current[e.session_id]`. Drop the event if no task owns the session (defensive — a late event after task delete).
3. **Special cases handled inline before reducer dispatch**:
   - `structured_review_prompt` (lines 1693-1716): captures the context Pi's `submit_review_prompt` tool args into `flow.reviewPromptFromTool` so the context-done branch can synthesise the round-1 prompt without text scraping.
   - `usage` (lines 1723-1740): routes to the reducer regardless of internal/external session so the Tasks-view bottom bar always reflects the agent producing tokens right now.
   - `isReviewInternal` (context-gather / merge Pi sessions): dispatches `internal_pi_*` reducer events that surface the agent's chunks/thinking/tool calls inside the parent Tasks bubble.
4. Otherwise: `mapChatEventToTaskEvent(e)` → `applyTaskEvent(cur, ev, defaultModel)` → `setTasks` with identity short-circuit.
5. On `done` events, the provider also fires a recovery path that reads the last-assistant text from disk via `getSessionLastAssistantText` (lines 2295-2351) — if the JSONL on disk has materially more text than the streamed bubble, the missing chunks are patched back in. This protects against IPC drops under load.

The `nurse-event` and `hivemind-progress` listeners (lines 2364-2404 and 2406-end) follow the same shape: resolve the owning task, map to a `TaskEvent`, fold via `applyTaskEvent`, identity short-circuit through `setTasks`. The hivemind listener subscribes via `subscribeHivemindEventListener` (singleton store) rather than opening its own Tauri channel — this is the only way to share one listener with the `ReviewHistory` and live-panel consumers.

### Ref soup — the orientation map

The provider uses ~20 refs to keep cross-cutting state that mustn't trigger re-renders. The important ones (around `taskRuntime.tsx:922-975`):

- `sessionIdToTaskIdRef` — primary route from Tauri event → owning task
- `internalSessionIdsRef` — set of Pi session ids the runtime spawned for context/merge (so chat-events on them route to the internal-Pi inline card instead of the main user/asst stream)
- `sessionIdToReviewIdRef` — secondary route for hivemind events that lack `review_id`
- `reviewFlowsRef` — per-task `ReviewFlowState` (active review lifecycle, phase, current round, captured tool args, watchdog timers)
- `reviewAccumulatorsRef` — per-task plaintext accumulator for round outputs
- `chatWatchdogsRef` / `chatWatchdogEpochRef` — Nurse check-in watchdogs (armed on `start`, cleared on `done`/`error`, epoch-guarded against late Nurse responses)
- `mergeLastChunkAtRef` / `contextLastEventAtRef` — reconciler idle-time bookkeeping
- `modelCatalogCacheRef` / `modelCatalogProviderLoadedRef` — lazy in-runtime cache of `refreshModels()` results keyed by `provider/model_id → context_window`

Always read these via `*Ref.current` and never let them leak into a render. Adding a new long-lived per-task value? Default to a ref; only promote to state if a component genuinely needs to re-render on every change.

---

## IPC wrapper (`lib/ipc.ts`)

Single chokepoint at `app/src/lib/ipc.ts:97-120` wraps `@tauri-apps/api/core::invoke`. Every IPC call in the codebase goes through it. Three things happen:

1. **Forward to `rawInvoke`**. Forwards name-only when `args === undefined` so callers like `invoke("list_swarms")` match Tauri's single-arg overload and existing `toHaveBeenCalledWith(name)` test contracts.
2. **Sentry capture on rejection** with tags `source: "ipc"`, `ipc_command: <name>`, and (when the typed envelope is present) `ipc_error_kind`. The captured value is synthesised as an `Error` so the Sentry dashboard groups failures meaningfully.
3. **Re-throw** so existing caller error handling (`console.error` → `ErrorModal` → toast) is unchanged.

### The `IpcError` discriminator

Mirrors `app/src-tauri/src/state/ipc_error.rs`. Defined at `lib/ipc.ts:17-31`:

| `kind` | When | UI surface |
|---|---|---|
| `provider_unauthenticated` | Provider key missing/rejected | Toast or error modal |
| `provider_rate_limited` | 429 from upstream | Toast |
| `circuit_breaker_open` | Per-provider breaker tripped | Toast — "Provider unavailable" |
| `not_found` | Resource missing; payload carries `resource` + `resource_id` | Inline message |
| `validation` | Request failed validation | Toast — message used directly |
| `not_approved` | User hasn't approved working dir | Approval modal (`request_working_dir_approval`) |
| `internal` | Anything else | Error modal |

`isIpcError(err)` (`:39-55`) narrows by `kind`. `formatIpcError(err)` (`:65-91`) turns any rejection — typed envelope, `Error`, raw string — into a single user-facing string with per-kind lead-in copy. **Use `formatIpcError` at every display site (toast / modal).**

### When to use what

- **Calling an IPC command from a component**: import the typed wrapper from `lib/ipc.ts` (`ipc.startReview(...)` not raw `invoke(...)`). Every command is wrapped — adding a new one means adding a wrapper, not bypassing the chokepoint.
- **Surfacing errors**: `formatIpcError(err)` → `ToastProvider` or `useErrorModal()`. Never render `String(err)` directly — typed envelopes are objects, not strings.
- **Routing per-kind**: branch on `isIpcError(err) && err.kind === "not_approved"` (or whichever kind) before falling back to the generic toast. The approval modal already does this in `App.tsx`.

---

## Singleton event stores

Two Tauri channels are consumed by multiple screens. We register **one** listener per channel and fan events out in JS. Pattern: per-store module owns the global `listen()` registration, exposes a `subscribe(key, callback)` API, and reference-counts subscribers so the global listener is torn down when nobody needs it.

### `lib/hivemindEventStore.ts` (170 LOC)

Owns the global `hivemind-progress` listener for the whole app.

- **Three subscription modes**:
  - `subscribeHivemindReview(key, listener)` (`:99-116`) — keyed by attribution (`task:${id}`, `swarm:${id}:queen`, `swarm:${id}:feat:${fid}`, or `job:${id}` fallback). Receives notifications when the derived `ReviewState` for that key changes.
  - `useHivemindReviewState(key)` (`:122-131`) — `useSyncExternalStore` hook on top of `subscribe + getSnapshot`.
  - `subscribeHivemindEventListener(listener)` (`:145-153`) — raw event firehose; receives every event. Used by `taskRuntime.tsx:2415` (multi-task fan-out) and `ReviewHistory.tsx:1552` (review-detail panel).
- **Derived state**: every incoming event is folded through `applyHivemindEvent(prev, evt)` (in `lib/hivemindReducer.ts`) into a per-attribution-key `ReviewState`. Keyed subscribers only fire when their slice changes.
- **Lazy global registration**: `ensureGlobalListener` (`:39-69`) installs the Tauri channel on first subscribe; idempotent (no-op if already registered or registration in flight).
- **Test helper**: `_resetHivemindEventStoreForTests()` (`:157-166`) tears everything down. Use from Vitest setup if a test needs a clean slate.

### `lib/swarmActivityStore.ts` (302 LOC)

Owns the global `swarm-activity` listener and **hydrates** from `get_swarm_activity_log` on first subscribe per swarm.

#### Fan-out + hydration sequence

`subscribeSwarmActivity(swarmId, listener)` (`:249-280`) does:

1. `ensureGlobalListener` — installs `swarm-activity` channel if not already installed.
2. Track new subscriber in `listeners.get(swarmId)`.
3. On **first** subscribe per swarm, mark `hydrationStatus = "loading"` and fire-and-forget `hydrateSwarm(swarmId)`.

`hydrateSwarm` (`:189-223`):
- Pages through `getSwarmActivityLog(swarmId, afterSeq)` up to 200 pages (safety bound).
- Folds each event through `applyActivityEvent`, tracks `maxSeenSeq` for dedup.
- On completion (success or error): drains `liveBuffer.get(swarmId)` — events received during hydration. Drops any with `seq <= maxSeqByHydration` as already-included.
- Flips `hydrationStatus = "ready"` and notifies subscribers. Live events from that point apply directly through the global listener.

The **listener body** (`:101-133`) checks `hydrationStatus[evt.swarm_id]`:
- If `"loading"`: push into `liveBuffer` and return.
- Else: apply through reducer and notify.

This is what lets `SwarmControl` open after a swarm has been running for an hour and replay the full activity history without missing live events that fired between hydration start and finish.

#### LRU bookkeeping

`MAX_SWARMS = 50` (`:13`). `accessOrder` array tracks LRU. Touch on every state read (`getSwarmActivityState`, `:244-247`) and every reducer apply. Evict oldest when over the cap.

### When to add a new singleton store

Add one when an event needs **multi-screen consumption**. If only one provider/component subscribes, a local `useEffect(() => { const u = listen(...); return () => u.then(f => f()); }, [])` is fine. The moment a second consumer appears, promote it — opening two Tauri listeners on the same channel doubles every event (the runtime does *not* deduplicate). Model new stores on `hivemindEventStore.ts` for simple fan-out, `swarmActivityStore.ts` if you also need hydration + seq-based dedup.

---

## Event listener index

Mirrors CLAUDE.md `§Event listener index`, with concrete file:line refs so you can jump straight to the consumer.

| Event | Subscriber | File:line | Notes |
|---|---|---|---|
| `chat-event` | `TaskRuntimeProvider` | `app/src/lib/taskRuntime.tsx:1681` | Sole consumer — not multiplexed |
| `hivemind-progress` | singleton store | `app/src/lib/hivemindEventStore.ts:42` | Fan-out via `subscribeHivemindReview` / `subscribeHivemindEventListener` |
|  ↳ Task fan-out | `TaskRuntimeProvider` | `app/src/lib/taskRuntime.tsx:2415` | Multi-task routing by `activeReviewJobId` or `reviewFlowsRef.currentJobId` |
|  ↳ Review history | `ReviewHistoryScreen` | `app/src/screens/ReviewHistory.tsx:1552` | Live updates while a saved review is being re-run |
| `swarm-event` | `SwarmsScreen` | `app/src/screens/Swarms.tsx:965` | Debounced list refresh on lifecycle change |
|  ↳ Detail | `SwarmControlScreen` | `app/src/screens/SwarmControl.tsx:415` | Re-fetch features/progress on the focused swarm |
| `swarm-activity` | singleton store | `app/src/lib/swarmActivityStore.ts:101` | Hydrated + seq-deduped; subscribers via `subscribeSwarmActivity` |
| `swarm_reconciled` | `SwarmsScreen` | `app/src/screens/Swarms.tsx:987` | Render Resume badges immediately after startup replay |
| `nurse-event` | `TaskRuntimeProvider` | `app/src/lib/taskRuntime.tsx:2376` | Inline Nurse cards on the matching task |
|  ↳ Status snapshot | `useNurseStatus` | `app/src/hooks/useNurseStatus.ts:62` | Topbar Nurse dropdown |
| `test-progress` | `TestRunProvider` | `app/src/state/TestRunProvider.tsx:88` | App-lifetime listener; provider remounts don't re-register |
|  ↳ Detail | `TestsScreen` | `app/src/screens/Tests.tsx:102` | Per-row updates + terminal-time bumps |
| `usage-snapshot-updated` | `ExtensionProvider` | `app/src/extensions/ExtensionProvider.tsx:77` | Topbar pill refresh |
|  ↳ Provider list | `ProvidersProvider` | `app/src/lib/ProvidersProvider.tsx:100` | Debounced re-fetch of `get_providers` |
| `default-model-changed` | `SettingsProvider` | `app/src/lib/SettingsProvider.tsx:104` | Patches `default_model` in the cached `SettingsResponse` |
| `default-project-path-changed` | `SettingsProvider` | `app/src/lib/SettingsProvider.tsx:114` | Patches `default_project_path` |
| `default-hivemind-changed` | `SettingsProvider` | `app/src/lib/SettingsProvider.tsx:124` | Patches `default_hivemind` |
| `pi-update-progress` | `SettingsScreen` | `app/src/screens/Settings.tsx:993` | Pi update progress bar |
| `pi-install-progress` | `SettingsScreen` | (mirror of update path) | Pi install progress bar |

All wrappers are defined once in `app/src/lib/events.ts:25-130` — never call `listen()` against a string literal in a component; use the typed `on*` wrapper. `safeUnlisten(fn)` (`events.ts:15-23`) swallows the harmless async rejection Tauri's internal unlisten emits during teardown races.

---

## Design system

The component library is bespoke at `app/src/components/atoms.tsx` (~770 LOC). No shadcn, no Radix, no Headless UI. Reach for an atom before writing a new primitive.

| Atom | Location | Purpose |
|---|---|---|
| `STATUS` table | `atoms.tsx:7-43` | Single source of truth for status colour tokens (`running`, `paused`, `completed`, `failed`, `planning`). Always import — don't redefine. |
| `Btn` | `atoms.tsx:82-104` | All buttons. Variants `primary | secondary | ghost | outline | danger | success`, sizes `sm | md | lg`. Pass `loading` for `aria-busy`. |
| `Panel`, `FlushPanel` | `:117`, `:153` | Card surface with optional title bar |
| `StatusBadge` | `:185` | Pulsing status pill driven by `STATUS` |
| `Pill` | `:219` | Small inline label with icon |
| `Input`, `Select` | `:244`, `:272` | Form primitives in the ink-and-honey palette |
| `RoundChip` | `:305` | Hivemind round indicator (`Round 2/3`) |
| `Modal`, `ConfirmDialog` | `:336`, `:417` | Focus-trapped via `focus-trap-react`. Use these for any blocking dialog. |
| `HivemindReviewCard` | `:516` | Review summary card used in Tasks and ReviewHistory |
| `ToolCallCard`, `ToolCallGroup` | `:570`, `:609` | Tool-call rendering in the assistant stream |
| `ReasoningBlock`, `InlineReasoningIndicator` | `:653`, `:742` | Extended-thinking scratchpad rendering |
| `Kbd` | `:762` | Keyboard chord glyph for shortcut hints |

**Tailwind palette tokens** (defined in `app/tailwind.config.ts`):

- Ink scale `ink-950 → ink-400` for backgrounds and chrome
- Honey scale, primary accent `honey-500 (#F5B919)`, used for active nav, primary buttons, the wordmark
- Line palette `line / line-soft / line-strong`
- Custom utilities in `app/src/index.css`: `.hex-bg` (animated hex tiles), `.shimmer` (token streaming), `.pulse-{green,amber,cyan,brain}` (status dots), `.honey-edge` (double-border inset), `.card-hover`

Fonts: **Inter** (sans), **JetBrains Mono** (mono).

---

## Accessibility

Defer to `app/src/A11Y.md` for the full convention guide. One-paragraph summary: `aria-live="polite"` for streaming and status; `aria-live="assertive"` (or `role="alert"`) for errors and modal interruptions only; streaming bubbles are `aria-atomic={false}` so each chunk announces incrementally; modals are focus-trapped via `focus-trap-react` inside `atoms.tsx::Modal` / `ConfirmDialog` / `ErrorModal`. Manual VoiceOver pass required for any feature touching the chat stream, swarm activity, or modal flow — there is no automated SR test.

---

## Recipes

### Add a new screen

1. **Create the file** under `app/src/screens/NewThing.tsx`. Export a single component that accepts `{ go: GoFn }` and any screen-specific params off `nav.params`.
2. **Import in `App.tsx`** alongside the other `screens/*` imports (`App.tsx:32-42`).
3. **Register in `ScreenRouter`** at `App.tsx:363-390`. Add a `case "new-thing"` that returns `<NewThingScreen go={go} {...nav.params} />`.
4. **Add to `NAV`** at `App.tsx:91-98` if it's top-level (sidebar entry). Provide `tab`, `label`, `icon`. Tabs are reachable via `Cmd/Ctrl + N` (1-indexed positional shortcut, `App.tsx:546-566`).
5. **If hidden under another tab** (e.g. `swarm-control` under `swarms`), wire `parentTab()` and `pageLabel()` at `App.tsx:102-123` so breadcrumbs and the sidebar highlight resolve to the parent.
6. **If Pi-gated**, add the tab id to `LOCKED_WHEN_NO_PI` at `App.tsx:69-79`. `go()` (`:537-543`) refuses to navigate when Pi is missing.
7. **Update CLAUDE.md** `§Frontend Architecture → Screens` table and `PRODUCT.md §8` if the screen is a new user-facing concept.

### Add a new event listener

**Single consumer** — register in a `useEffect` inside the component or provider that owns the state:

```ts
useEffect(() => {
  if (!isTauri()) return;
  let unlisten: UnlistenFn | undefined;
  let mounted = true;
  onMyEvent((evt) => {
    if (!mounted) return;
    // …reduce into local state…
  }).then((fn) => {
    if (mounted) unlisten = fn;
    else safeUnlisten(fn);
  });
  return () => {
    mounted = false;
    safeUnlisten(unlisten);
  };
}, []);
```

The `mounted` flag + late-resolution unlisten is the canonical shape — Tauri's `listen()` is async, so the cleanup has to handle the case where the component unmounts before the registration resolves. Always use `safeUnlisten` from `lib/events.ts:15`.

**Multi-consumer** — promote to a singleton store under `lib/`. Model on:

- `lib/hivemindEventStore.ts` for simple keyed fan-out
- `lib/swarmActivityStore.ts` if you also need hydration + seq-based dedup

The pattern: module-scope `Map<key, Set<Listener>>`, module-scope `globalUnlisten`, lazy `ensureGlobalListener()` on first subscribe, reference-counted teardown when the last subscriber leaves.

**Both cases**: add the wrapper to `lib/events.ts` so the event name is typed in one place, and update CLAUDE.md `§Frontend Architecture → Event listener index`.

### Add a context provider

1. **Decide root vs. scoped**. Root-level (auth, settings, theme, error surface, IPC-cached data) goes high in `App.tsx`. Scoped state (e.g. wizard-step shared between two siblings) goes at the lowest common ancestor.
2. **Define the context** with a `null` default and a fallback value for unit tests that mount a single screen without the full tree — see `SettingsProvider.tsx:54, 154-160` for the canonical pattern.
3. **Subscribe to needed Tauri events** in a `useEffect`, using the canonical mounted/unlisten shape above.
4. **Memoise the context value** with `useMemo` over the precise deps that should trigger re-renders. The whole point of `taskRuntime.tsx`'s split is that a streaming chunk only re-renders `runtimeStateApi`, not `actionsApi` or `defaultsApi`.
5. **Insert into `App.tsx`'s provider tree** at the right depth. Anything reading `SettingsProvider` / `ProvidersProvider` must nest inside them.
6. **Update CLAUDE.md** `§State management` table.

### Wire a new IPC command from the backend

1. **Add the wrapper** to `lib/ipc.ts`. Pattern: `export const myCommand = (...) => invoke<ReturnType>("my_command", { args });`. Keep argument names in camelCase — Tauri serialises them to the snake_case Rust handler params automatically. Group with the existing module-section comment (`// ── Hivemind ──`, etc.).
2. **Import types** for the return shape from `lib/types.ts` (or add them there if new). Backend types live in `app/src-tauri/src/state/ipc_error.rs` and the per-command response structs; the TS mirror lives in `lib/types.ts`.
3. **Call from the component / provider** via `await ipc.myCommand(...)`. Wrap in try/catch:

   ```ts
   try {
     const result = await ipc.myCommand(arg);
     // …happy path
   } catch (err) {
     const msg = formatIpcError(err);
     // Branch on isIpcError(err) && err.kind === "not_approved" if special handling needed
     showToast(msg, "error"); // or open ErrorModal via useErrorModal()
   }
   ```

4. **Sentry capture is automatic** — the IPC chokepoint at `lib/ipc.ts:97` already tags with `ipc_command` and `ipc_error_kind`. Don't capture again from the caller.
5. **If the command emits a Tauri event** that the UI should listen to, follow the "Add a new event listener" recipe above and update CLAUDE.md's two relevant tables (`§Tauri Events` + `§Event listener index`).
6. **Update CLAUDE.md** `§Tauri Commands (IPC)` bucket counts and the per-handler list.
