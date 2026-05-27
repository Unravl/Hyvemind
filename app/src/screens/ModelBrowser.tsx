import React, { useState, useEffect, useMemo, useCallback, useRef } from "react";
import { GoFn } from "../App";
import { I } from "../components/icons";
import { Btn, Pill, Input, Select } from "../components/atoms";
import { MODELS, PROVIDERS } from "../data/mock";
import type { Model } from "../data/mock";
import { isTauri } from "../lib/tauri";
import { refreshModels, testProviderModels } from "../lib/ipc";
import { useProviders } from "../lib/ProvidersProvider";
import type { ModelInfoResponse, ModelDetail, ProviderInfo } from "../lib/types";

/* ── Mapping helper ──────────────────────────────────────── */

const fmtTokens = (n: number | null | undefined): string => {
  if (!n) return "";
  if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(n % 1_000_000 === 0 ? 0 : 1)}M`;
  if (n >= 1_000) return `${(n / 1_000).toFixed(n % 1_000 === 0 ? 0 : 1)}k`;
  return String(n);
};

const fmtPrice = (n: number | null | undefined): string => {
  if (n == null) return "";
  return `$${n.toFixed(2)}`;
};

const modelInfoToModel = (m: ModelInfoResponse): Model => ({
  id: m.model_id,
  provider: m.provider,
  ctx: fmtTokens(m.context_window),
  out: "",
  tags: [],
  price: `${fmtPrice(m.cost_per_1m_input)} / ${fmtPrice(m.cost_per_1m_output)}`,
  type: "text\u2192text",
  ctxNum: m.context_window || undefined,
  inputPrice: m.cost_per_1m_input || undefined,
  outputPrice: m.cost_per_1m_output || undefined,
});

const detailToModel = (d: ModelDetail, provider: string): Model => ({
  id: d.id,
  provider,
  ctx: fmtTokens(d.context_length),
  out: fmtTokens(d.max_output),
  tags: [],
  price: d.input_price != null || d.output_price != null
    ? `${fmtPrice(d.input_price)} / ${fmtPrice(d.output_price)}`
    : "",
  type: "",
  ctxNum: d.context_length ?? undefined,
  outNum: d.max_output ?? undefined,
  inputPrice: d.input_price ?? undefined,
  outputPrice: d.output_price ?? undefined,
});

/* ── Local constants ──────────────────────────────────────── */

const THINKING_OPTS = [
  { value: "off", label: "Off" },
  { value: "low", label: "Low" },
  { value: "medium", label: "Medium" },
  { value: "high", label: "High" },
];

/* ── Persisted provider preference ────────────────────────── */

const MODEL_BROWSER_LAST_PROVIDER_KEY = "hyvemind:model-browser-last-provider";

function readLastProvider(): string | null {
  try {
    return localStorage.getItem(MODEL_BROWSER_LAST_PROVIDER_KEY);
  } catch {
    return null;
  }
}

function writeLastProvider(p: string) {
  try {
    localStorage.setItem(MODEL_BROWSER_LAST_PROVIDER_KEY, p);
  } catch {
    /* ignore (e.g. private mode / unavailable storage) */
  }
}

/* ── ModelBrowserModal ────────────────────────────────────── */

interface ModelBrowserModalProps {
  open: boolean;
  onClose: () => void;
  onSelect?: (model: Model, opts: { thinking: string }) => void;
  initialProvider?: string;
  initialModel?: string;
  selectLabel?: string;
}

export function ModelBrowserModal({ open, onClose, onSelect, initialProvider, initialModel, selectLabel }: ModelBrowserModalProps) {
  const [provider, setProvider] = useState("");
  const [query, setQuery] = useState("");
  const [selected, setSelected] = useState<Model | null>(null);
  const [thinking, setThinking] = useState("high");
  const [models, setModels] = useState<Model[]>(isTauri() ? [] : MODELS);
  const [providerList, setProviderList] = useState<string[]>(isTauri() ? [] : PROVIDERS);
  const [modelsLoading, setModelsLoading] = useState(false);
  const [modelsError, setModelsError] = useState<string | null>(null);
  const providerDataRef = useRef<ProviderInfo[]>([]);
  // audit 6.7 — read provider list from the shared ProvidersProvider
  // instead of fetching on every modal open.
  const { providers: providersFromCtx } = useProviders();
  const autoSelectDoneRef = useRef(false);
  // Fetch live models from provider APIs, enriched with static catalog metadata.
  // Subscription providers have no API endpoint — use the static catalog directly.
  const fetchModelsFor = useCallback(async (targets: string[]) => {
    if (!isTauri() || targets.length === 0) { setModels([]); return; }
    setModels([]);
    setModelsLoading(true);
    setModelsError(null);

    const timeout = new Promise<never>((_, reject) =>
      setTimeout(() => reject(new Error("Timed out after 30 seconds. Check your provider endpoints and API keys.")), 30_000)
    );

    const subProviders = new Set(
      providerDataRef.current.filter((p) => p.provider_type === "Subscription").map((p) => p.name)
    );

    try {
      await Promise.race([
        (async () => {
          const catalog = await refreshModels();
          const cat = new Map(catalog.map((m) => [`${m.provider}/${m.model_id}`, m]));
          const results = await Promise.allSettled(
            targets.map(async (p) => {
              // Subscription providers: return models from static catalog
              if (subProviders.has(p)) {
                return catalog
                  .filter((m) => m.provider === p)
                  .map(modelInfoToModel);
              }
              const res = await testProviderModels(p);
              if (!res.ok) return [] as Model[];
              const detailMap = new Map<string, ModelDetail>(
                (res.details ?? []).map((d) => [d.id, d])
              );
              return res.models.map((id): Model => {
                const detail = detailMap.get(id);
                if (detail) return detailToModel(detail, p);
                const meta = cat.get(`${p}/${id}`);
                if (meta) return modelInfoToModel(meta);
                return { id, provider: p, ctx: "", out: "", tags: [], price: "", type: "" };
              });
            })
          );
          // Defensive — backend dedupes per-provider but flatten across
          // providers and any future quirky provider could still produce
          // duplicate React keys. Keying by `${provider}/${id}` mirrors the
          // <button key=...> downstream so we never render two rows with the
          // same key (which breaks React reconciliation and the click->highlight
          // mapping). Note that the upstream `detailMap` keeps the *last*
          // occurrence on duplicate `d.id`, while this filter keeps the *first*
          // Model — benign because duplicate ids from the same provider carry
          // identical metadata in practice.
          const flattened = results
            .filter((r): r is PromiseFulfilledResult<Model[]> => r.status === "fulfilled")
            .flatMap((r) => r.value);
          const seen = new Set<string>();
          const deduped = flattened.filter((m) => {
            const key = `${m.provider}/${m.id}`;
            if (seen.has(key)) return false;
            seen.add(key);
            return true;
          });
          setModels(deduped);
        })(),
        timeout,
      ]);
    } catch (err) {
      console.error("Failed to load models:", err);
      setModelsError(err instanceof Error ? err.message : String(err));
    } finally {
      setModelsLoading(false);
    }
  }, []);

  // When modal opens: fetch configured providers, then query models for selected provider
  useEffect(() => {
    if (!open) return;
    setQuery("");
    setSelected(null);

    const persisted = readLastProvider();

    // Build the priority chain — each entry is checked against the configured list.
    // Priority: provider parsed from initialModel → initialProvider → persisted → first provider.
    const fromModel =
      initialModel && initialModel.includes("/") ? initialModel.split("/")[0] : null;
    const candidates = [fromModel, initialProvider, persisted].filter(Boolean) as string[];

    autoSelectDoneRef.current = false;

    if (!isTauri()) {
      const mockStart = candidates.find((c) => PROVIDERS.includes(c)) ?? PROVIDERS[0];
      setProvider(mockStart);
      return;
    }

    // Read configured providers from the shared cache (audit 6.7).
    providerDataRef.current = providersFromCtx;
    const configured = providersFromCtx.filter((p) => p.configured).map((p) => p.name);
    setProviderList(configured);
    const effectiveStart =
      candidates.find((c) => configured.includes(c)) ?? configured[0];
    if (effectiveStart) {
      setProvider(effectiveStart);
      fetchModelsFor([effectiveStart]);
    }
  }, [open, initialProvider, initialModel, fetchModelsFor]);

  // Auto-select model when models load and initialModel is set
  useEffect(() => {
    if (!initialModel || autoSelectDoneRef.current || models.length === 0) return;
    const parts = initialModel.split("/");
    const modelId = parts.length > 1 ? parts[1] : parts[0];
    const providerFromModel = parts.length > 1 ? parts[0] : undefined;
    const match = models.find(m =>
      (!providerFromModel || m.provider === providerFromModel) &&
      m.id === modelId
    );
    if (match) {
      setSelected(match);
      autoSelectDoneRef.current = true;
    }
  }, [models, initialModel]);

  // When user clicks a provider tab, clear list and fetch fresh
  const handleProviderChange = useCallback((p: string) => {
    setProvider(p);
    setSelected(null);
    writeLastProvider(p);
    if (!isTauri()) return;
    const targets = [p];
    fetchModelsFor(targets);
  }, [providerList, fetchModelsFor]);

  const handleRefresh = useCallback(() => {
    if (!isTauri()) return;
    const targets = [provider];
    fetchModelsFor(targets);
  }, [provider, providerList, fetchModelsFor]);

  const filtered = useMemo(() => {
    return models.filter(
      (m) =>
        m.provider === provider &&
        (query === "" ||
          m.id.toLowerCase().includes(query.toLowerCase()) ||
          m.provider.includes(query.toLowerCase()))
    );
  }, [provider, query, models]);

  // Show rich columns when any model has pricing/context data
  const hasDetails = useMemo(
    () => filtered.some((m) => m.price || m.out || m.ctx),
    [filtered]
  );

  if (!open) return null;

  return (
    <div className="fixed inset-0 z-[60] flex items-center justify-center">
      <div
        className="absolute inset-0 bg-black/60 backdrop-blur-sm"
        onClick={onClose}
      />
      <div className="relative bg-ink-800 border border-line rounded-2xl shadow-2xl h-[85vh] flex flex-col w-full max-w-[920px]">
        <div className="px-5 py-4 border-b border-line flex items-center justify-between shrink-0">
          <div className="flex items-center gap-2.5">
            {I.search({ size: 16, className: "text-honey-400" })}
            <div className="text-[15px] font-semibold">Choose model</div>
            <span className="text-[12px] text-dim">{filtered.length} available</span>
          </div>
          <button onClick={onClose} aria-label="Close" className="text-muted hover:text-white">
            {I.x({ size: 18 })}
          </button>
        </div>

        {/* Two-panel: provider sidebar + search/list */}
        <div className="flex flex-1 min-h-0">
          {/* Left: provider sidebar */}
          <div className="w-44 shrink-0 border-r border-line overflow-auto py-2">
            {providerList.map((p) => (
              <button
                key={p}
                onClick={() => handleProviderChange(p)}
                className={`w-full text-left px-4 py-2 text-[13px] transition-colors ${
                  provider === p
                    ? "text-honey-200 bg-honey-500/8 border-r-2 border-honey-400 font-medium"
                    : "text-muted hover:text-white hover:bg-ink-800/60"
                }`}
              >
                {p.charAt(0).toUpperCase() + p.slice(1)}
              </button>
            ))}
          </div>

          {/* Right: search + model list */}
          <div className="flex-1 flex flex-col min-w-0">
            <div className="px-5 pt-4 shrink-0">
              {modelsError && (
                <div className="mb-2 text-[11px] text-red-400">{modelsError}</div>
              )}
              <div className="flex items-center gap-2 mt-3">
                <Input
                  icon={I.search({ size: 14 })}
                  placeholder="Search models, e.g. opus, deepseek-v3, 200k..."
                  value={query}
                  onChange={(e) => setQuery(e.target.value)}
                  wrapClass="flex-1"
                />
                <button
                  onClick={handleRefresh}
                  className="h-8 w-8 shrink-0 rounded-md border border-line bg-ink-900 flex items-center justify-center text-muted hover:text-honey-300 hover:border-honey-500/40 transition-colors"
                  title="Refresh models"
                >
                  {I.refresh({ size: 14 })}
                </button>
              </div>
            </div>

            <div className="flex-1 min-h-0 overflow-auto px-5 py-3">
              <div className="space-y-1.5">
                {modelsLoading && filtered.length === 0 && (
                  <div className="flex flex-col items-center justify-center py-16 gap-3">
                    <span className="inline-block w-6 h-6 border-2 border-honey-400 border-t-transparent rounded-full animate-spin" />
                    <span className="text-[13px] text-muted">Querying provider models...</span>
                  </div>
                )}
                {hasDetails && (
                  <div className="grid grid-cols-[2fr_.6fr_.5fr_.5fr_1fr] gap-3 px-3 pb-1.5 text-[10px] uppercase tracking-wider text-dim font-semibold">
                    <div>Model</div>
                    <div>Context</div>
                    <div>Output</div>
                    <div>Input $/1M</div>
                    <div>Output $/1M</div>
                  </div>
                )}
                {filtered.map((m) => {
                  const isSel = selected?.id === m.id && selected?.provider === m.provider;
                  return (
                    <button
                      key={`${m.provider}/${m.id}`}
                      onClick={() => setSelected(m)}
                      className={`w-full text-left rounded-md border px-3 py-2.5 grid ${
                        hasDetails
                          ? "grid-cols-[2fr_.6fr_.5fr_.5fr_1fr]"
                          : "grid-cols-[2fr_.8fr_.6fr]"
                      } gap-3 items-center transition-colors ${
                        isSel
                          ? "border-honey-500/60 bg-honey-500/5 ring-honey-soft"
                          : "border-line bg-ink-850 hover:border-line-strong"
                      }`}
                    >
                      <div className="flex items-center gap-2 min-w-0">
                        {isSel && I.check({ size: 13, className: "text-honey-400 shrink-0" })}
                        <span className="font-mono text-[13px] text-white truncate">{m.id}</span>
                      </div>
                      {hasDetails ? (
                        <>
                          <span className="text-[11.5px] text-muted font-mono">{m.ctx || "—"}</span>
                          <span className="text-[11.5px] text-muted font-mono">{m.out || "—"}</span>
                          <span className="text-[11.5px] text-white/85 font-mono tabular-nums">{m.price ? m.price.split(" / ")[0] : "—"}</span>
                          <span className="text-[11.5px] text-white/85 font-mono tabular-nums">{m.price ? m.price.split(" / ")[1] : "—"}</span>
                        </>
                      ) : (
                        <span className="text-[11.5px] text-muted font-mono">{m.ctx ? `${m.ctx} ctx` : ""}</span>
                      )}
                    </button>
                  );
                })}
                {!modelsLoading && filtered.length === 0 && !modelsError && (
                  <div className="text-center text-muted text-[12.5px] py-12">
                    No models match. Try a different provider or query.
                  </div>
                )}
              </div>
            </div>
          </div>
        </div>

        {/* Footer */}
        <div className="px-5 py-3.5 border-t border-line bg-ink-850 flex items-center gap-3 shrink-0">
          <div className="flex items-center gap-2">
            <span className="text-[11px] uppercase tracking-wider text-dim font-semibold">Thinking</span>
            <Select
              value={thinking}
              onChange={(e) => setThinking(e.target.value)}
              options={THINKING_OPTS}
              className="w-28"
            />
          </div>
          <div className="flex-1" />
          <Btn kind="ghost" onClick={onClose}>Cancel</Btn>
          <Btn
            kind="primary"
            icon={I.plus({ size: 14 })}
            onClick={() => selected && onSelect?.(selected, { thinking })}
          >
            {selectLabel || "Add Model"}
          </Btn>
        </div>
      </div>
    </div>
  );
}

/* ── Screen wrapper (for App.tsx route compatibility) ────── */

export const ModelBrowserScreen = ({ go, params }: { go: GoFn; params?: Record<string, any> }) => {
  const returnTo = params?.returnTo || "hiveminds";
  const selectLabel = params?.selectLabel as string | undefined;
  const initialProvider = params?.provider as string | undefined;

  const handleSelect = (model: Model) => {
    if (params?.onSelectModel) {
      params.onSelectModel(model.id);
    }
    go(returnTo);
  };

  return (
    <div className="h-full flex items-center justify-center text-muted">
      <ModelBrowserModal
        open={true}
        onClose={() => go(returnTo)}
        onSelect={(model) => handleSelect(model)}
        selectLabel={selectLabel}
        initialProvider={initialProvider}
      />
    </div>
  );
};
