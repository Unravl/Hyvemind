# Hyvemind documentation index

This directory holds the topical, deep-dive documentation. The top-level docs (`CLAUDE.md`, `PRODUCT.md`, `AGENTS.md`, `CONTRIBUTING.md`) remain the canonical entry points; the files here are referenced from those.

## Read order for a new contributor

1. `../README.md` — what Hyvemind is (5-minute skim)
2. `../PRODUCT.md` — product vision, three systems, bee-colony agents, brand, glossary
3. `../CLAUDE.md` — technical/operational reference (file paths, IPC, debug recipes)
4. `../CONTRIBUTING.md` — dev setup, PR checklist, how to add a provider
5. Whichever doc in this directory matches what you're about to touch

## Docs in this directory

| Doc | What it covers |
|-----|----------------|
| `architecture.md` | System component map, sequence flows, crash-recovery flow, storage layout, concurrency-primitives map. Mermaid diagrams. |
| `bee-agents.md` | Per-agent deep-dive (Queen / Scout / Worker / Guard / Nurse + scout-review + stability-test pair): role, prompt summary, lifecycle, inputs/outputs, failure modes, code paths. Closes with "adding a new bee role." |
| `developer-cookbook.md` | Task-oriented recipes: add a Tauri command, add a tunable, add a screen, add a migration, bump Pi, add an event listener, enable HYVEMIND_DEBUG=1, reproduce crash recovery, etc. |
| `extension-authoring.md` | Three extension surfaces: (1) Rust provider extensions (usage/billing pollers), (2) Pi local TypeScript extensions, (3) frontend topbar widgets. |
| `frontend-architecture.md` | React shell deep-dive: provider tree, `taskRuntime.tsx` sub-contexts, reducer pattern, IPC wrapper, singleton event stores, design system, accessibility, recipes. |
| `hivemind-custom-prompts.md` | Ready-to-paste reviewer prompts for Hivemind teams. |
| `ipc-reference.md` | Per-command reference for all Tauri commands: signature, return type, error kinds, delegation target. Complements the bucket-summary table in `CLAUDE.md`. |
| `providers.md` | Overview of the LLM provider abstraction (the 5 concrete impls, dispatch, cost lookup, circuit-breaker integration). Defers to `app/src-tauri/src/providers/README.md` for the deep-dive. |

## Subsystem READMEs

Deep-dives for one Rust subsystem each:

| Subsystem | Path |
|-----------|------|
| Swarm engine | `../app/src-tauri/src/core/README.md` |
| Multi-model review engine | `../app/src-tauri/src/hivemind/README.md` |
| Pi subprocess management | `../app/src-tauri/src/pi/README.md` |
| Persistence + AppState | `../app/src-tauri/src/state/README.md` |
| Provider extensions | `../app/src-tauri/src/extensions/README.md` |
| LLM provider abstraction | `../app/src-tauri/src/providers/README.md` |
| Domain types | `../app/src-tauri/src/domain/README.md` |

## Frontend conventions

| Doc | Covers |
|-----|--------|
| `../app/src/A11Y.md` | Accessibility (aria-live policies, screen-reader rules, focus traps) |

## Keeping these docs alive

Each new doc has a "Documentation Maintenance" footer or trigger table that mirrors `CLAUDE.md §Documentation Maintenance`. When you change something a doc describes, update the doc in the same commit — the trigger tables tell you exactly which section needs an edit.
