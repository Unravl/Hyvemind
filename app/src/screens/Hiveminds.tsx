import React, { useState, useCallback, useEffect, useMemo, useRef } from "react";
import { GoFn } from "../App";
import { I } from "../components/icons";
import { Btn, Pill, Input, ConfirmDialog } from "../components/atoms";
import { HIVEMINDS, MODELS } from "../data/mock";
import type { Hivemind } from "../data/mock";
import { HivemindEditModal } from "./HivemindEdit";
import { isTauri } from "../lib/tauri";
import { useTaskRuntime } from "../lib/taskRuntime";
import { createHivemind, listHiveminds as listHivemindsIpc, deleteHivemind } from "../lib/ipc";
import { useProviders } from "../lib/ProvidersProvider";
import type { HivemindSummary } from "../lib/types";

/* ── Local constants ──────────────────────────────────────── */

// TODO: Hivemind team definitions are local mock data. These will need
// `list_hiveminds` / `create_hivemind` / `update_hivemind` / `delete_hivemind`
// backend commands to be implemented for full CRUD support.

/* ── HivemindsScreen ──────────────────────────────────────── */

/** Normalize a provider name for key matching. */
function providerKey(raw: string): string {
  return raw.trim().toLowerCase();
}

const PROVIDER_COLORS: Record<string, string> = {
  anthropic: "#D97757",
  openai: "#10a37f",
  openrouter: "#4285f4",
  deepseek: "#7c3aed",
  ollama: "#6b7280",
  glm: "#06b6d4",
  groq: "#f97316",
  mistral: "#f59e0b",
  kimi: "#ec4899",
  crof: "#a855f7",
  "nvidia-nim": "#76b900",
  neuralwatt: "#14b8a6",
  "opencode-go": "#3b82f6",
};

/** Palette of distinct hues for custom/unmapped providers (deterministic, stable). */
const FALLBACK_PALETTE = [
  "#dc2626", "#ea580c", "#ca8a04", "#16a34a", "#0891b2",
  "#2563eb", "#7c3aed", "#db2777", "#65a30d", "#0d9488",
  "#9333ea", "#c026d3", "#0284c7", "#b45309", "#4f46e5",
  "#059669", "#d97706", "#be123c", "#4338ca", "#0e7490",
];

/** Stable, deterministic color for a custom provider name (non-empty).
 *  Uses djb2 hash → palette index.  Guards against the JS
 *  `Math.abs(-2147483648) === -2147483648` edge case. */
function hashToPaletteColor(name: string): string {
  let hash = 5381;
  for (let i = 0; i < name.length; i++) {
    hash = (hash * 33) ^ name.charCodeAt(i);
  }
  // Bound the result *before* Math.abs so we never hit the -2^31 trap.
  const idx = Math.abs(hash % FALLBACK_PALETTE.length);
  return FALLBACK_PALETTE[idx];
}

/** Unified provider color: known brands → brand color, empty/unknown → grey,
 *  custom name → deterministic palette color. Null-safe at runtime. */
function providerColor(provider: string | undefined | null): string {
  if (!provider) return "#9ca3af"; // neutral grey for missing/falsy
  const key = providerKey(provider);
  if (!key) return "#9ca3af";
  return PROVIDER_COLORS[key] || hashToPaletteColor(key);
}

const MOCK_LEGEND_KEYS = ["anthropic", "openAI", "openRouter", "deepseek", "unknown"];

type HivemindModelRef = {
  id: string;
  provider: string;
};

type DisplayHivemind = Omit<Hivemind, "rounds"> & {
  rounds: HivemindModelRef[][];
  inheritOrchestrator?: boolean;
  orchestratorModel?: string | null;
  orchestratorProvider?: string | null;
  orchestratorThinking?: string;
};

const MODEL_PROVIDER_BY_ID = new Map(MODELS.map((m) => [m.id, m.provider]));

function splitProviderModel(value: string): HivemindModelRef {
  if (!value) return { id: "", provider: "" };
  const raw = value.trim();
  if (!raw) return { id: "", provider: "" };

  // Strip all leading slashes ("//foo" → "foo", "/foo" → "foo").
  let stripped = raw;
  while (stripped.startsWith("/")) {
    stripped = stripped.slice(1);
  }
  if (!stripped) return { id: "", provider: "" };

  const slash = stripped.indexOf("/");
  if (slash <= 0) {
    // No slash — the entire (stripped) string is the model ID.
    return { id: stripped, provider: MODEL_PROVIDER_BY_ID.get(stripped) || "" };
  }

  const maybeProvider = stripped.slice(0, slash).trim();
  const maybeId = stripped.slice(slash + 1).replace(/\/+$/, "").trim();

  if (!maybeId) {
    // Trailing slash with nothing after — treat as model ID with no provider.
    return { id: maybeProvider, provider: MODEL_PROVIDER_BY_ID.get(maybeProvider) || "" };
  }

  const provider = maybeProvider
    ? maybeProvider
    : MODEL_PROVIDER_BY_ID.get(maybeId) || "";
  return { id: maybeId, provider };
}

function modelRefFromConfig(
  model: { id?: string; provider?: string } | string
): HivemindModelRef {
  if (typeof model === "string") return splitProviderModel(model);

  const provider = (model.provider || "").trim();
  if (provider) {
    // Provider explicitly set — id is the literal model name, but may
    // still carry a redundant provider prefix from legacy configs.
    const id = (model.id || "").trim();
    const prefix = provider + "/";
    const cleanId = id.startsWith(prefix) ? id.slice(prefix.length) : id;
    return { id: cleanId, provider };
  }

  // No explicit provider — parse model.id as "provider/model" or bare ID.
  return splitProviderModel((model.id || "").trim());
}

function serializeModelRef(model: HivemindModelRef): string {
  return model.provider ? `${model.provider}/${model.id}` : model.id;
}

/** Convert a backend HivemindSummary into the display Hivemind shape. */
function summaryToHivemind(s: HivemindSummary): DisplayHivemind {
  let rounds: HivemindModelRef[][] = [];
  try {
    const parsed = JSON.parse(s.rounds_config);
    if (Array.isArray(parsed)) {
      rounds = parsed.map((r: { models?: ({ id?: string; provider?: string } | string)[] }) =>
        Array.isArray(r.models) ? r.models.map(modelRefFromConfig) : []
      );
    }
  } catch { /* fallback to empty */ }
  return {
    id: s.id,
    name: s.name,
    desc: s.description,
    runs: s.runs,
    rounds,
    inheritOrchestrator: s.inherit_orchestrator ?? false,
    orchestratorModel: s.orchestrator_model,
    orchestratorProvider: s.orchestrator_provider,
    orchestratorThinking: s.orchestrator_thinking,
  };
}

function mockToHivemind(hm: Hivemind): DisplayHivemind {
  return {
    ...hm,
    rounds: hm.rounds.map((round) => round.map(modelRefFromConfig)),
  };
}

function formatError(err: unknown): string {
  return err instanceof Error ? err.message : String(err);
}

function optionalString(value: string | null | undefined): string | undefined {
  const trimmed = value?.trim();
  return trimmed ? trimmed : undefined;
}

export function HivemindsScreen({ go }: { go: GoFn }) {
  const [editing, setEditing] = useState<HivemindSummary | null>(null);
  const [editId, setEditId] = useState<string | undefined>(undefined);
  const [tauriHivemindSummaries, setTauriHivemindSummaries] = useState<HivemindSummary[]>([]);
  const [creating, setCreating] = useState(false);
  const [page, setPage] = useState(1);
  // audit 6.7 — read the configured-providers list from the shared
  // ProvidersProvider instead of issuing a separate `getProviders()`
  // call on mount. `configuredProviders` keeps its old name so the
  // downstream UI usages are unchanged.
  const { configured: configuredProviders } = useProviders();
  const [deletingId, setDeletingId] = useState<string | null>(null);
  const [deleteError, setDeleteError] = useState<string | null>(null);
  const [confirmDelete, setConfirmDelete] = useState<DisplayHivemind | null>(null);
  const [tauriHiveminds, setTauriHiveminds] = useState<DisplayHivemind[]>([]);
  const [cloningId, setCloningId] = useState<string | null>(null);
  const [cloneError, setCloneError] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const [loadError, setLoadError] = useState<string | null>(null);
  const mutationInFlightRef = useRef(false);
  const { refreshHivemindOptions } = useTaskRuntime();

  const tauriSummaryById = useMemo(
    () => new Map(tauriHivemindSummaries.map((summary) => [summary.id, summary])),
    [tauriHivemindSummaries],
  );

  const loadHiveminds = useCallback(async (options?: {
    rethrow?: boolean;
    clearMutationErrors?: boolean;
    initialLoad?: boolean;
  }) => {
    if (!isTauri()) {
      setLoading(false);
      return true;
    }

    if (options?.initialLoad) {
      setLoading(true);
      setLoadError(null);
    }

    try {
      const items = await listHivemindsIpc();
      setTauriHivemindSummaries(items);
      setTauriHiveminds(items.map(summaryToHivemind));
      setLoadError(null);

      // Sync the task runtime / Settings dropdown with the same data.
      // Passing `items` avoids a redundant IPC call.
      await refreshHivemindOptions(items);

      if (options?.clearMutationErrors) {
        setCloneError(null);
        setDeleteError(null);
      }

      return true;
    } catch (err) {
      console.error("Failed to load hiveminds:", err);
      if (options?.initialLoad) {
        setLoadError(formatError(err));
      }
      if (options?.rethrow) throw err;
      return false;
    } finally {
      if (options?.initialLoad) {
        setLoading(false);
      }
    }
  }, [refreshHivemindOptions]);

  const retryLoad = useCallback(() => {
    loadHiveminds({ initialLoad: true });
  }, [loadHiveminds]);

  useEffect(() => {
    if (!isTauri()) {
      setLoading(false);
      return;
    }
    loadHiveminds({ initialLoad: true });
    // Provider list now comes from <ProvidersProvider> — no
    // per-screen fetch (audit 6.7).
  }, [loadHiveminds]);

  const hivemindList = isTauri() ? tauriHiveminds : HIVEMINDS.map(mockToHivemind);
  const PAGE_SIZE = 4;
  const total = hivemindList.length;
  const pageCount = Math.max(1, Math.ceil(total / PAGE_SIZE));
  const safePage = Math.min(page, pageCount);
  const start = (safePage - 1) * PAGE_SIZE;
  const pageItems = hivemindList.slice(start, start + PAGE_SIZE);

  const executeDelete = useCallback(async (hm: DisplayHivemind) => {
    if (!isTauri()) return;
    if (mutationInFlightRef.current) return;

    mutationInFlightRef.current = true;
    setDeletingId(hm.id);
    setDeleteError(null);
    setCloneError(null);

    try {
      await deleteHivemind(hm.id);

      // Close the edit modal if it was open for this hivemind
      if (editId === hm.id) {
        setEditing(null);
        setEditId(undefined);
        setCreating(false);
      }

      await loadHiveminds({ clearMutationErrors: true });

      // Clamp page to new page count after deletion reduces total
      const newPageCount = Math.max(1, Math.ceil((hivemindList.length - 1) / PAGE_SIZE));
      setPage((p) => Math.min(p, newPageCount));
    } catch (err) {
      console.error("Failed to delete hivemind:", err);
      setDeleteError(formatError(err));
    } finally {
      setDeletingId(null);
      setConfirmDelete(null);
      mutationInFlightRef.current = false;
    }
  }, [loadHiveminds, editId, hivemindList.length]);

  const handleDeleteClick = useCallback((hm: DisplayHivemind) => {
    if (!isTauri()) return;
    if (mutationInFlightRef.current) return;

    setDeleteError(null);
    setCloneError(null);
    setConfirmDelete(hm);
  }, []);

  const confirmDeleteHm = useCallback(() => {
    if (confirmDelete) {
      executeDelete(confirmDelete);
    }
  }, [confirmDelete, executeDelete]);

  const cancelDelete = useCallback(() => {
    setConfirmDelete(null);
  }, []);

  const handleClone = useCallback(async (summary: HivemindSummary | null) => {
    if (!isTauri()) return;

    if (!summary) {
      setCloneError("Failed to clone: Could not find hivemind configuration to clone.");
      return;
    }

    if (mutationInFlightRef.current) return;

    mutationInFlightRef.current = true;
    setCloningId(summary.id);
    setCloneError(null);
    setDeleteError(null);

    try {
      await createHivemind(
        `${summary.name} (Clone)`,
        summary.description,
        summary.rounds_config,
        summary.inherit_orchestrator,
        optionalString(summary.orchestrator_model),
        optionalString(summary.orchestrator_provider),
        optionalString(summary.orchestrator_thinking),
      );

      try {
        await loadHiveminds({ rethrow: true, clearMutationErrors: true });
      } catch (refreshErr) {
        console.error("Cloned hivemind but failed to refresh list:", refreshErr);
        setCloneError(
          `Clone was created, but the list could not be refreshed: ${formatError(refreshErr)}`,
        );
      }
    } catch (err) {
      console.error("Failed to clone hivemind:", err);
      setCloneError(`Failed to clone: ${formatError(err)}`);
    } finally {
      setCloningId(null);
      mutationInFlightRef.current = false;
    }
  }, [loadHiveminds]);

  return (
    <div className="h-full overflow-auto">
      <div className="max-w-[1400px] mx-auto px-8 py-7">
        {/* Header */}
        <div className="flex items-end justify-between mb-6">
          <div>
            <h1 className="text-[28px] font-bold tracking-tight">Hiveminds</h1>
            <p className="text-muted text-[13.5px] mt-1 max-w-xl">
              Each pipeline runs models in rounds and synthesizes the verdicts. Drag to
              reorder rounds, click any model to swap.
            </p>
          </div>
          <div className="flex items-center gap-2">
            <Btn
              kind="primary"
              icon={I.plus({ size: 14 })}
              onClick={() => setCreating(true)}
            >
              New Hivemind
            </Btn>
          </div>
        </div>

        {/* Provider legend */}
        <div className="flex items-center gap-3 mb-5 text-[11px] text-muted">
          <span className="uppercase tracking-wider text-dim">Providers:</span>
          {isTauri()
            ? configuredProviders.map((p) => {
                const color = providerColor(p.name);
                const label = p.display_name || p.name;
                return (
                  <span key={p.name} className="flex items-center gap-1.5">
                    <span
                      className="w-2 h-2 rounded-full"
                      style={{ background: color }}
                    />
                    {label}
                  </span>
                );
              })
            : MOCK_LEGEND_KEYS.map((key) => {
                const color = providerColor(key);
                const label = key.charAt(0).toUpperCase() + key.slice(1);
                return (
                  <span key={key} className="flex items-center gap-1.5">
                    <span
                      className="w-2 h-2 rounded-full"
                      style={{ background: color }}
                    />
                    {label}
                  </span>
                );
              })}
        </div>

        {deleteError && (
          <div className="mb-3 px-4 py-2 rounded-lg bg-red-900/20 border border-red-500/30 text-[13px] text-red-400">
            Failed to delete: {deleteError}
          </div>
        )}

        {cloneError && (
          <div className="mb-3 px-4 py-2 rounded-lg bg-red-900/20 border border-red-500/30 text-[13px] text-red-400">
            {cloneError}
          </div>
        )}

        {/* Pipeline cards */}
        <div className="space-y-3">
          {loading ? (
            /* Loading skeletons */
            Array.from({ length: PAGE_SIZE }).map((_, i) => (
              <div
                key={`skel-${i}`}
                className="bg-ink-800 border border-line rounded-xl p-4 animate-pulse"
              >
                <div className="flex items-center gap-4">
                  {/* Left: name + meta skeleton */}
                  <div className="w-[200px] shrink-0 space-y-2">
                    <div className="flex items-center gap-2">
                      <div className="w-[13px] h-[13px] rounded-sm bg-ink-600" />
                      <div className="h-[18px] w-[120px] rounded bg-ink-600" />
                    </div>
                    <div className="h-[13px] w-full rounded bg-ink-600" />
                    <div className="h-[13px] w-3/4 rounded bg-ink-600" />
                    <div className="flex items-center gap-2 mt-2">
                      <div className="h-[12px] w-[50px] rounded bg-ink-600" />
                      <div className="h-[12px] w-[60px] rounded bg-ink-600" />
                    </div>
                  </div>
                  {/* Pipeline diagram skeleton */}
                  <div className="flex-1 min-w-0">
                    <div className="flex items-center gap-2">
                      <div className="flex-1">
                        <div className="h-[14px] w-[50px] rounded bg-ink-600 mx-auto mb-1.5" />
                        <div className="bg-ink-850 border border-line rounded-lg p-2 space-y-1">
                          <div className="h-[28px] rounded bg-ink-600" />
                          <div className="h-[28px] rounded bg-ink-600" />
                        </div>
                      </div>
                      <div className="w-[14px] shrink-0" />
                      <div className="flex-1">
                        <div className="h-[14px] w-[50px] rounded bg-ink-600 mx-auto mb-1.5" />
                        <div className="bg-ink-850 border border-line rounded-lg p-2 space-y-1">
                          <div className="h-[28px] rounded bg-ink-600" />
                        </div>
                      </div>
                    </div>
                  </div>
                  {/* Right: actions skeleton */}
                  <div className="flex flex-col gap-1 shrink-0 w-[112px]">
                    <div className="h-[28px] rounded-md bg-ink-600" />
                    <div className="h-[28px] rounded-md bg-ink-600" />
                    <div className="h-[28px] rounded-md bg-ink-600" />
                  </div>
                </div>
              </div>
            ))
          ) : loadError ? (
            /* Error state */
            <div className="bg-amber-950/20 border border-amber-500/30 rounded-xl px-5 py-6">
              <div className="flex items-start gap-4">
                <div className="w-9 h-9 rounded-full bg-amber-500/10 border border-amber-500/20 flex items-center justify-center shrink-0 mt-0.5">
                  {I.x({ size: 16, className: "text-amber-400" })}
                </div>
                <div className="flex-1 min-w-0">
                  <h3 className="text-[14px] font-semibold text-amber-300 mb-1">
                    Failed to load hiveminds
                  </h3>
                  <p className="text-[12.5px] text-amber-400/70 leading-relaxed mb-4">
                    {loadError}
                  </p>
                  <Btn
                    kind="outline"
                    size="sm"
                    icon={I.refresh({ size: 12 })}
                    onClick={retryLoad}
                  >
                    Retry
                  </Btn>
                </div>
              </div>
            </div>
          ) : total === 0 ? (
            /* Empty state */
            <div className="bg-ink-800 border border-line rounded-xl px-6 py-10 text-center">
              <div className="w-12 h-12 rounded-full bg-honey-500/10 border border-honey-500/20 flex items-center justify-center mx-auto mb-4">
                {I.spark({ size: 20, className: "text-honey-400" })}
              </div>
              <h3 className="text-[15px] font-semibold text-white mb-2">
                Create your first Hivemind
              </h3>
              <p className="text-[12.5px] text-muted max-w-md mx-auto mb-5 leading-relaxed">
                A Hivemind is a named team of models that review plans in parallel.
                Each model takes a stance — For, Against, or Neutral — and the
                outputs are merged across rounds to produce a meaningfully refined
                plan.
              </p>
              <Btn
                kind="primary"
                icon={I.plus({ size: 13 })}
                onClick={() => setCreating(true)}
              >
                Create Hivemind
              </Btn>
            </div>
          ) : (
            <>
              {pageItems.map((hm) => {
                const totalModels = hm.rounds.reduce((a, r) => a + r.length, 0);
                const summary = tauriSummaryById.get(hm.id) || null;
                const anyMutationInFlight = cloningId !== null || deletingId !== null;
                return (
                  <div
                    key={hm.id}
                    className="bg-ink-800 border border-line rounded-xl p-4 card-hover"
                  >
                    <div className="flex items-center gap-4">
                      {/* Left: name + meta */}
                      <div className="w-[200px] shrink-0">
                        <div className="flex items-center gap-2 mb-1">
                          {I.hexFill({ size: 13, className: "text-honey-500" })}
                          <h3 className="text-[15px] font-bold font-mono text-white truncate">
                            {hm.name}
                          </h3>
                        </div>
                        <p className="text-[11.5px] text-muted leading-snug line-clamp-2">
                          {hm.desc}
                        </p>
                        <div className="flex items-center gap-2 mt-2 text-[10.5px] text-dim font-mono">
                          <span>{hm.runs} runs</span>
                          <span>·</span>
                          <span>{totalModels} models</span>
                        </div>
                        {/* Orchestrator model */}
                        {hm.orchestratorModel != null && hm.orchestratorModel !== "" && !hm.inheritOrchestrator ? (
                          <div className="flex items-center gap-1.5 mt-1.5 text-[10px] text-dim">
                            {I.crown({ size: 10, className: "text-honey-500/60 shrink-0" })}
                            {hm.orchestratorProvider && (
                              <span
                                className="w-1.5 h-1.5 rounded-full shrink-0"
                                style={{ background: providerColor(hm.orchestratorProvider) }}
                              />
                            )}
                            <span
                              className="font-mono text-white/60 truncate"
                              title={`Orchestrator: ${hm.orchestratorProvider ? hm.orchestratorProvider + "/" : ""}${hm.orchestratorModel || ""}`}
                            >
                              {hm.orchestratorModel}
                            </span>
                          </div>
                        ) : hm.inheritOrchestrator === true ? (
                          <div className="flex items-center gap-1.5 mt-1.5 text-[10px] text-dim">
                            {I.crown({ size: 10, className: "text-dim shrink-0" })}
                            <span className="truncate" title="Orchestrator inherits the active task model">
                              inherits task model
                            </span>
                          </div>
                        ) : null}
                      </div>

                      {/* Pipeline diagram */}
                      <div className="flex-1 min-w-0">
                        <div className="flex items-center gap-2">
                          {hm.rounds.map((models, i) => (
                            <React.Fragment key={i}>
                              <div className="flex-1 min-w-0">
                                <div className="flex items-center justify-center mb-1.5 px-1">
                                  <span className="text-[10px] font-mono text-honey-400 tracking-wider">
                                    Round {i + 1}
                                  </span>
                                </div>
                                <div className="bg-ink-850 border border-line rounded-lg p-2 space-y-1">
                                  {models.map((m) => (
                                    <div
                                      key={serializeModelRef(m)}
                                      title={serializeModelRef(m)}
                                      className="flex items-center gap-1.5 px-1.5 py-1 rounded bg-ink-900/60 border border-line/60"
                                    >
                                      <span
                                        className="w-1.5 h-1.5 rounded-full shrink-0"
                                        style={{
                                          background: providerColor(m.provider),
                                        }}
                                      />
                                      <span className="font-mono text-[10.5px] text-white/85 truncate">
                                        {m.id}
                                      </span>
                                    </div>
                                  ))}
                                </div>
                              </div>
                              {i < hm.rounds.length - 1 && (
                                <div className="flex flex-col items-center shrink-0 self-stretch justify-center pt-4">
                                  {I.chevR({ size: 14, className: "text-dim" })}
                                </div>
                              )}
                            </React.Fragment>
                          ))}
                        </div>
                      </div>

                      {/* Right: actions */}
                      <div
                        data-testid={`hivemind-actions-${hm.id}`}
                        className="flex flex-col gap-1 shrink-0 w-[112px]"
                      >
                        <Btn
                          size="sm"
                          kind="ghost"
                          icon={I.list({ size: 11 })}
                          onClick={() => go("review-history", { hivemind: hm })}
                          className="border border-green-500/60 text-green-400 hover:text-green-300 hover:border-green-400 hover:bg-green-500/10"
                        >
                          History
                        </Btn>
                        <Btn
                          size="sm"
                          kind="ghost"
                          icon={I.edit({ size: 11 })}
                          onClick={() => {
                            setEditId(hm.id);
                            setEditing(summary);
                          }}
                        >
                          Edit
                        </Btn>
                        {isTauri() && (
                          <Btn
                            size="sm"
                            kind="ghost"
                            icon={I.copy({ size: 11 })}
                            onClick={() => handleClone(summary)}
                            disabled={anyMutationInFlight}
                          >
                            {cloningId === hm.id ? "Cloning..." : "Clone"}
                          </Btn>
                        )}
                        {isTauri() && (
                          <Btn
                            size="sm"
                            kind="danger"
                            icon={I.trash({ size: 11 })}
                            onClick={() => handleDeleteClick(hm)}
                            disabled={anyMutationInFlight}
                          >
                            {deletingId === hm.id ? "Deleting..." : "Delete"}
                          </Btn>
                        )}
                      </div>
                    </div>
                  </div>
                );
              })}

              {/* Add row */}
              <button
                onClick={() => setCreating(true)}
                className="w-full bg-ink-850/40 border border-dashed border-line-strong rounded-xl py-5 flex items-center justify-center gap-2 text-muted hover:text-honey-300 hover:border-honey-500/40 transition-colors"
              >
                {I.plus({ size: 14 })}
                <span className="text-[13px] font-medium">
                  Compose new hivemind pipeline
                </span>
              </button>
            </>
          )}
        </div>

        {/* Pagination — only when there are items */}
        {!loading && !loadError && total > 0 && (
          <div className="flex items-center justify-between mt-5 px-1">
            <div className="text-[11.5px] text-dim font-mono">
              Showing{" "}
              <span className="text-muted">
                {start + 1}–{Math.min(start + PAGE_SIZE, total)}
              </span>{" "}
              of <span className="text-muted">{total}</span>
            </div>
            <div className="flex items-center gap-1">
              <button
                onClick={() => setPage((p) => Math.max(1, p - 1))}
                disabled={safePage <= 1}
                className="h-7 px-2 rounded-md border border-line bg-ink-850 text-muted hover:text-white hover:border-line-strong disabled:opacity-40 disabled:hover:text-muted disabled:hover:border-line text-[11.5px] flex items-center gap-1"
              >
                {I.chevL({ size: 12 })} Prev
              </button>
              {Array.from({ length: pageCount }, (_, i) => i + 1).map((n) => (
                <button
                  key={n}
                  onClick={() => setPage(n)}
                  className={`h-7 min-w-7 px-2 rounded-md border text-[11.5px] font-mono transition-colors ${
                    n === safePage
                      ? "bg-honey-500/10 border-honey-500/40 text-honey-300"
                      : "bg-ink-850 border-line text-muted hover:text-white hover:border-line-strong"
                  }`}
                >
                  {n}
                </button>
              ))}
              <button
                onClick={() => setPage((p) => Math.min(pageCount, p + 1))}
                disabled={safePage >= pageCount}
                className="h-7 px-2 rounded-md border border-line bg-ink-850 text-muted hover:text-white hover:border-line-strong disabled:opacity-40 disabled:hover:text-muted disabled:hover:border-line text-[11.5px] flex items-center gap-1"
              >
                Next {I.chevR({ size: 12 })}
              </button>
            </div>
          </div>
        )}
      </div>

      <ConfirmDialog
        open={!!confirmDelete}
        title="Delete Hivemind"
        message={`Delete hivemind "${confirmDelete?.name || confirmDelete?.id || ""}"? This cannot be undone.`}
        confirmLabel="Delete"
        cancelLabel="Cancel"
        onConfirm={confirmDeleteHm}
        onCancel={cancelDelete}
        danger={true}
        loading={!!deletingId}
      />

      <HivemindEditModal
        open={!!editing || creating}
        onClose={() => {
          setEditing(null);
          setEditId(undefined);
          setCreating(false);
        }}
        onSave={() => loadHiveminds({ clearMutationErrors: true })}
        hivemind={editing}
        creating={creating}
        editId={editId}
      />

    </div>
  );
}
