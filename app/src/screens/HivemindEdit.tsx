import React, { useState, useEffect } from "react";
import { GoFn } from "../App";
import { I } from "../components/icons";
import { Btn, Input, Select } from "../components/atoms";
import { MODELS } from "../data/mock";
import type { HivemindSummary } from "../lib/types";
import { ModelBrowserModal } from "./ModelBrowser";
import { isTauri } from "../lib/tauri";
import { createHivemind, updateHivemind, listCustomPrompts, type CustomPrompt } from "../lib/ipc";

/* ── Local constants ──────────────────────────────────────── */

const THINKING_OPTS = [
  { value: "off", label: "Off" },
  { value: "low", label: "Low" },
  { value: "medium", label: "Medium" },
  { value: "high", label: "High" },
];

const DEFAULT_MAX_TOKENS = 65535;
const DEFAULT_TIMEOUT = 450;

/* ── Types ────────────────────────────────────────────────── */

interface RoundModel {
  id: string;
  provider: string;
  thinking: string;
  maxTokens: number;
  /** Stored context window (tokens) captured from `ModelDetail.context_length`
   *  at selection time. Optional for backwards-compatibility with older
   *  configs that didn't capture this; consumed by the merge-prompt
   *  truncation logic at review time. */
  contextWindow?: number;
  /** Stored model max_output (tokens) captured at selection time. */
  maxOutput?: number;
  /** Per-model sampling overrides. Both are optional — leaving them
   *  undefined omits the field from the outbound provider request body
   *  so the provider's default applies. */
  temperature?: number;
  topP?: number;
  /** ID of a user-defined custom prompt from Settings → Prompts. When set,
   *  the prompt body is appended to the end of this model's system prompt
   *  at review-dispatch time. Dangling ids (the prompt was deleted) are
   *  silently resolved to no suffix by the backend. */
  customPromptId?: string;
}

interface Round {
  models: RoundModel[];
  timeout: number;
}

/* ── Helpers ──────────────────────────────────────────────── */

const seedFromHM = (hm: HivemindSummary | null): Round[] => {
  if (!hm)
    return [
      {
        models: [{ id: "", provider: "anthropic", thinking: "high", maxTokens: DEFAULT_MAX_TOKENS }],
        timeout: DEFAULT_TIMEOUT,
      },
    ];
  try {
    const parsed = JSON.parse(hm.rounds_config);
    if (Array.isArray(parsed)) {
      return parsed.map((r: { models?: { id?: string; provider?: string; thinking?: string; max_tokens?: number; context_window?: number; max_output?: number; temperature?: number; top_p?: number; custom_prompt_id?: string }[]; timeout?: number }) => ({
        timeout: r.timeout ?? DEFAULT_TIMEOUT,
        models: Array.isArray(r.models)
          ? r.models.map((m): RoundModel => {
              const out: RoundModel = {
                id: m.id || "",
                provider: m.provider || MODELS.find((x) => x.id === m.id)?.provider || "anthropic",
                thinking: m.thinking || "high",
                maxTokens: m.max_tokens ?? DEFAULT_MAX_TOKENS,
              };
              if (typeof m.context_window === "number" && m.context_window > 0) {
                out.contextWindow = m.context_window;
              }
              if (typeof m.max_output === "number" && m.max_output > 0) {
                out.maxOutput = m.max_output;
              }
              if (typeof m.temperature === "number" && Number.isFinite(m.temperature)) {
                out.temperature = m.temperature;
              }
              if (typeof m.top_p === "number" && Number.isFinite(m.top_p)) {
                out.topP = m.top_p;
              }
              if (typeof m.custom_prompt_id === "string" && m.custom_prompt_id.length > 0) {
                out.customPromptId = m.custom_prompt_id;
              }
              return out;
            })
          : [{ id: "", provider: "anthropic", thinking: "high", maxTokens: DEFAULT_MAX_TOKENS }],
      }));
    }
  } catch { /* fallback */ }
  return [
    {
      models: [{ id: "", provider: "anthropic", thinking: "high", maxTokens: DEFAULT_MAX_TOKENS }],
      timeout: DEFAULT_TIMEOUT,
    },
  ];
};

/* ── HivemindEditModal ────────────────────────────────────── */

interface HivemindEditModalProps {
  open: boolean;
  onClose: () => void;
  onSave?: () => void;
  hivemind: HivemindSummary | null;
  creating: boolean;
  editId?: string;
}

export function HivemindEditModal({ open, onClose, onSave, hivemind, creating, editId }: HivemindEditModalProps) {
  const [name, setName] = useState("");
  const [desc, setDesc] = useState("");
  const [rounds, setRounds] = useState<Round[]>([]);
  const [browser, setBrowser] = useState<{ ri: number; mi: number } | null>(null);
  const [saving, setSaving] = useState(false);
  const [inheritOrchestrator, setInheritOrchestrator] = useState(true);
  const [orchModel, setOrchModel] = useState<string>("");
  const [orchProvider, setOrchProvider] = useState<string>("anthropic");
  const [orchThinking, setOrchThinking] = useState<string>("high");
  const [orchContextWindow, setOrchContextWindow] = useState<number | null>(null);
  const [orchMaxOutput, setOrchMaxOutput] = useState<number | null>(null);
  const [orchBrowser, setOrchBrowser] = useState(false);
  const [customPrompts, setCustomPrompts] = useState<CustomPrompt[]>([]);

  useEffect(() => {
    if (!open) return;
    setName(creating ? "" : hivemind?.name || "");
    setDesc(creating ? "" : hivemind?.description || "");
    setRounds(seedFromHM(creating ? null : hivemind));
    setInheritOrchestrator(hivemind?.inherit_orchestrator ?? true);
    setOrchModel(hivemind?.orchestrator_model || "");
    setOrchProvider(hivemind?.orchestrator_provider || "anthropic");
    setOrchThinking(hivemind?.orchestrator_thinking || "high");
    setOrchContextWindow(hivemind?.orchestrator_context_window ?? null);
    setOrchMaxOutput(hivemind?.orchestrator_max_output ?? null);
    if (isTauri()) {
      listCustomPrompts()
        .then(setCustomPrompts)
        .catch(() => setCustomPrompts([]));
    }
  }, [open, hivemind?.id, creating]);

  const updateModel = (ri: number, mi: number, patch: Partial<RoundModel>) => {
    setRounds((rs) =>
      rs.map((r, i) =>
        i !== ri
          ? r
          : {
              ...r,
              models: r.models.map((m, j) => (j !== mi ? m : { ...m, ...patch })),
            }
      )
    );
  };

  const addModel = (ri: number) =>
    setRounds((rs) =>
      rs.map((r, i) =>
        i !== ri
          ? r
          : {
              ...r,
              models: [
                ...r.models,
                { id: "", provider: "anthropic", thinking: "high", maxTokens: DEFAULT_MAX_TOKENS },
              ],
            }
      )
    );

  const removeModel = (ri: number, mi: number) =>
    setRounds((rs) =>
      rs.map((r, i) =>
        i !== ri ? r : { ...r, models: r.models.filter((_, j) => j !== mi) }
      )
    );

  const addRound = () =>
    setRounds((rs) => [
      ...rs,
      {
        timeout: DEFAULT_TIMEOUT,
        models: [{ id: "", provider: "anthropic", thinking: "high", maxTokens: DEFAULT_MAX_TOKENS }],
      },
    ]);

  const removeRound = (ri: number) =>
    setRounds((rs) => rs.filter((_, i) => i !== ri));

  const cloneRound = (ri: number) =>
    setRounds((rs) => {
      if (!rs[ri]) return rs;
      return [...rs, structuredClone(rs[ri])];
    });

  if (!open) return null;

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center">
      <div
        className="absolute inset-0 bg-black/60 backdrop-blur-sm"
        onClick={onClose}
      />
      <div className="relative bg-ink-800 border border-line rounded-2xl shadow-2xl max-h-[85vh] flex flex-col w-full max-w-4xl">
        {/* Header */}
        <div className="px-5 py-4 border-b border-line flex items-center justify-between shrink-0">
          <div className="flex items-center gap-2.5">
            <div className="w-8 h-8 rounded-md bg-honey-500/15 border border-honey-500/30 flex items-center justify-center">
              {I.hexFill({ size: 14, className: "text-honey-400" })}
            </div>
            <div>
              <div className="text-[11px] uppercase tracking-[.18em] text-honey-400 font-semibold">
                {creating ? "Create" : "Edit"} Hivemind
              </div>
              <div className="text-[15px] font-semibold">
                {creating ? "New review team" : hivemind?.name}
              </div>
            </div>
          </div>
          <button onClick={onClose} aria-label="Close" className="text-muted hover:text-white">
            {I.x({ size: 18 })}
          </button>
        </div>

        {/* Body */}
        <div className="flex-1 min-h-0 overflow-auto p-5 space-y-5">
          {/* Name */}
          <div>
            <label className="text-[11px] uppercase tracking-wider text-muted font-semibold">
              Team name
            </label>
            <Input
              value={name}
              onChange={(e) => setName(e.target.value)}
              placeholder="e.g. enhance, security-audit"
              wrapClass="mt-1.5 max-w-md"
            />
          </div>

          {/* Description (optional) */}
          <div>
            <label className="text-[11px] uppercase tracking-wider text-muted font-semibold flex items-center gap-2">
              Description
              <span className="text-dim normal-case tracking-normal text-[10.5px] font-normal">
                optional · shown on the Hiveminds list
              </span>
            </label>
            <textarea
              value={desc}
              onChange={(e) => setDesc(e.target.value)}
              placeholder="What is this hivemind for? When should the team reach for it?"
              rows={2}
              className="mt-1.5 w-full bg-ink-900 border border-line focus:border-honey-500/40 focus:outline-none focus-visible:ring-2 focus-visible:ring-honey-500 focus-visible:ring-offset-2 focus-visible:ring-offset-ink-950 rounded-md px-3 py-2 text-[12.5px] text-white/90 placeholder:text-dim resize-none"
            />
          </div>

          {/* Orchestrator model */}
          <div className="rounded-lg border border-line bg-ink-850 p-4 space-y-3">
            <div className="flex items-center justify-between">
              <div>
                <div className="text-[11px] uppercase tracking-wider text-muted font-semibold">
                  Orchestrator Model
                </div>
                <div className="text-[11.5px] text-dim mt-0.5">
                  The model used for context-gathering and merge sessions during reviews.
                </div>
              </div>
              <button
                onClick={() => setInheritOrchestrator((v) => !v)}
                className={`relative w-10 h-[22px] rounded-full border transition-colors ${
                  inheritOrchestrator
                    ? "bg-honey-500/20 border-honey-500/40"
                    : "bg-ink-900 border-line"
                }`}
              >
                <span
                  className={`absolute top-[2px] w-4 h-4 rounded-full transition-all ${
                    inheritOrchestrator
                      ? "left-[22px] bg-honey-400"
                      : "left-[2px] bg-dim"
                  }`}
                />
              </button>
            </div>
            <div className="text-[12px] text-muted">
              {inheritOrchestrator
                ? "Inherits the model active in the task when the review starts."
                : "Uses a specific model for context-gathering and merge sessions."}
            </div>
            {!inheritOrchestrator && (
              <div className="flex items-center gap-2 pt-1">
                <button
                  onClick={() => setOrchBrowser(true)}
                  className="h-8 px-2.5 rounded-md bg-ink-900 border border-line hover:border-honey-500/40 text-left flex items-center justify-between gap-2 group flex-1 min-w-0"
                >
                  <span
                    className={`font-mono text-[12.5px] truncate flex-1 min-w-0 ${
                      orchModel ? "text-white" : "text-dim"
                    }`}
                  >
                    {orchModel || "Click to choose model..."}
                  </span>
                  {orchModel && <span className="text-[10.5px] text-muted ml-1.5 shrink-0">{orchProvider}</span>}
                  {I.search({
                    size: 13,
                    className: "text-dim group-hover:text-honey-400 shrink-0",
                  })}
                </button>
                <Select
                  value={orchThinking}
                  onChange={(e) => setOrchThinking(e.target.value)}
                  options={THINKING_OPTS}
                />
              </div>
            )}
          </div>

          {/* Rounds */}
          <div className="space-y-4">
            {rounds.map((round, ri) => (
              <div key={ri} data-testid={`round-${ri}`} className="rounded-lg border border-line bg-ink-850 overflow-hidden">
                {/* Round header */}
                <div className="flex items-center justify-between px-4 h-11 bg-ink-800/60 border-b border-line">
                  <div className="flex items-center gap-2.5">
                    <span className="font-mono text-[12.5px] text-honey-400 font-semibold">
                      Round {ri + 1}
                    </span>
                    <span className="text-dim text-[12px]">
                      {round.models.length} model{round.models.length === 1 ? "" : "s"}
                    </span>
                  </div>
                  <div className="flex items-center gap-2">
                    <span className="text-[11px] text-muted">Timeout</span>
                    <Input
                      suffix={<span className="text-[11px]">s</span>}
                      type="number"
                      value={round.timeout}
                      onChange={(e) =>
                        setRounds((rs) =>
                          rs.map((r, i) =>
                            i !== ri ? r : { ...r, timeout: +e.target.value }
                          )
                        )
                      }
                      wrapClass="w-24"
                      className="[&_input]:font-mono"
                    />
                    <Btn
                      kind="ghost"
                      size="sm"
                      icon={I.copy({ size: 13 })}
                      aria-label={`Clone round ${ri + 1}`}
                      onClick={() => cloneRound(ri)}
                    >
                      Clone
                    </Btn>
                    {rounds.length > 1 && (
                      <Btn kind="danger" size="sm" onClick={() => removeRound(ri)}>
                        Remove round
                      </Btn>
                    )}
                  </div>
                </div>

                {/* Table */}
                <div className="px-3 pt-2 pb-3">
                  <div className="grid grid-cols-[1fr_110px_100px_80px_80px_150px_30px] gap-2 px-2 py-1.5 text-[10.5px] uppercase tracking-wider text-dim font-semibold">
                    <div>Model</div>
                    <div>Thinking</div>
                    <div>Max tokens</div>
                    <div>Temp</div>
                    <div>Top P</div>
                    <div>Prompt</div>
                    <div></div>
                  </div>
                  <div className="space-y-1.5">
                    {round.models.map((m, mi) => (
                      <div
                        key={mi}
                        className="grid grid-cols-[1fr_110px_100px_80px_80px_150px_30px] gap-2 items-center bg-ink-900/60 border border-line/70 rounded-md px-2 py-1.5"
                      >
                        <button
                          onClick={() => setBrowser({ ri, mi })}
                          className="h-8 px-2.5 rounded-md bg-ink-900 border border-line hover:border-honey-500/40 text-left flex items-center justify-between gap-2 group min-w-0"
                        >
                          <span
                            className={`font-mono text-[12.5px] truncate flex-1 min-w-0 ${
                              m.id ? "text-white" : "text-dim"
                            }`}
                          >
                            {m.id || "Click to choose model..."}
                          </span>
                          {m.id && <span className="text-[10.5px] text-muted ml-1.5 shrink-0">{m.provider}</span>}
                          {I.search({
                            size: 13,
                            className: "text-dim group-hover:text-honey-400 shrink-0",
                          })}
                        </button>
                        <Select
                          value={m.thinking}
                          onChange={(e) =>
                            updateModel(ri, mi, { thinking: e.target.value })
                          }
                          options={THINKING_OPTS}
                        />
                        <Input
                          type="number"
                          value={m.maxTokens}
                          onChange={(e) =>
                            updateModel(ri, mi, { maxTokens: +e.target.value })
                          }
                          className="[&_input]:font-mono"
                          title="Auto-filled from model's max output when known."
                          aria-label="Max output tokens (auto-filled from model's max output when known)"
                        />
                        <Input
                          type="number"
                          step="0.05"
                          value={m.temperature ?? ""}
                          placeholder="auto"
                          onChange={(e) => {
                            const v = e.target.value;
                            if (v === "") {
                              updateModel(ri, mi, { temperature: undefined });
                            } else {
                              const n = Number(v);
                              if (Number.isFinite(n)) updateModel(ri, mi, { temperature: n });
                            }
                          }}
                          className="[&_input]:font-mono"
                          title="Sampling temperature. Leave blank to use the provider's default."
                          aria-label="Temperature (leave blank to use provider default)"
                        />
                        <Input
                          type="number"
                          step="0.05"
                          value={m.topP ?? ""}
                          placeholder="auto"
                          onChange={(e) => {
                            const v = e.target.value;
                            if (v === "") {
                              updateModel(ri, mi, { topP: undefined });
                            } else {
                              const n = Number(v);
                              if (Number.isFinite(n)) updateModel(ri, mi, { topP: n });
                            }
                          }}
                          className="[&_input]:font-mono"
                          title="Nucleus sampling top_p. Leave blank to use the provider's default."
                          aria-label="Top P (leave blank to use provider default)"
                        />
                        <Select
                          value={m.customPromptId ?? ""}
                          onChange={(e) =>
                            updateModel(ri, mi, {
                              customPromptId: e.target.value || undefined,
                            })
                          }
                          options={[
                            { value: "", label: "None" },
                            ...customPrompts.map((p) => ({ value: p.id, label: p.name })),
                          ]}
                          title="Appends a saved custom prompt to the end of this model's system prompt. Manage prompts in Settings → Prompts."
                          aria-label="Custom prompt (appended to system prompt)"
                        />
                        <button
                          data-testid="remove-model"
                          onClick={() => removeModel(ri, mi)}
                          className="text-dim hover:text-red-400 flex items-center justify-center h-8"
                        >
                          {I.x({ size: 14 })}
                        </button>
                      </div>
                    ))}
                  </div>
                  <button
                    onClick={() => addModel(ri)}
                    className="mt-2 w-full h-8 rounded-md border border-dashed border-line text-muted hover:text-honey-300 hover:border-honey-500/40 text-[12.5px] flex items-center justify-center gap-1.5"
                  >
                    {I.plus({ size: 13 })} Add model
                  </button>
                </div>
              </div>
            ))}

            <button
              onClick={addRound}
              className="w-full h-11 rounded-lg border border-dashed border-honey-500/40 text-honey-300 hover:bg-honey-500/5 text-[13px] font-medium flex items-center justify-center gap-2"
            >
              {I.plus({ size: 14 })} Add round
            </button>
          </div>

          {/* Settings hint */}
          <div className="flex items-start gap-3 p-3 rounded-md bg-blue-500/5 border border-blue-500/20 text-[12px] text-blue-200/90">
            {I.spark({ size: 14, className: "text-blue-300 mt-0.5 shrink-0" })}
            <div>
              <div className="font-medium text-blue-200">How rounds work</div>
              <div className="text-blue-200/70 mt-0.5">
                Each round runs all its models in parallel. Output of round N becomes context
                for round N+1. The final round acts as the synthesis judge.
              </div>
            </div>
          </div>
        </div>

        {/* Footer */}
        <div className="px-5 py-3.5 border-t border-line bg-ink-850 flex items-center justify-between shrink-0">
          <div className="text-[12px] text-dim font-mono">
            {rounds.reduce((a, r) => a + r.models.length, 0)} models · {rounds.length} round
            {rounds.length === 1 ? "" : "s"}
          </div>
          <div className="flex items-center gap-2">
            <Btn kind="ghost" onClick={onClose}>
              Cancel
            </Btn>
            <Btn kind="primary" icon={I.check({ size: 14 })} disabled={saving} onClick={async () => {
              const roundsConfig = JSON.stringify(
                rounds.map((r) => ({
                  models: r.models.map((m) => {
                    const out: {
                      id: string;
                      provider: string;
                      thinking: string;
                      max_tokens: number;
                      context_window?: number;
                      max_output?: number;
                      temperature?: number;
                      top_p?: number;
                      custom_prompt_id?: string;
                    } = {
                      id: m.id,
                      provider: m.provider,
                      thinking: m.thinking,
                      max_tokens: m.maxTokens,
                    };
                    if (typeof m.contextWindow === "number" && m.contextWindow > 0) {
                      out.context_window = m.contextWindow;
                    }
                    if (typeof m.maxOutput === "number" && m.maxOutput > 0) {
                      out.max_output = m.maxOutput;
                    }
                    if (typeof m.temperature === "number" && Number.isFinite(m.temperature)) {
                      out.temperature = m.temperature;
                    }
                    if (typeof m.topP === "number" && Number.isFinite(m.topP)) {
                      out.top_p = m.topP;
                    }
                    if (typeof m.customPromptId === "string" && m.customPromptId.length > 0) {
                      out.custom_prompt_id = m.customPromptId;
                    }
                    return out;
                  }),
                  timeout: r.timeout,
                }))
              );
              if (isTauri()) {
                setSaving(true);
                try {
                  const orchM = inheritOrchestrator ? undefined : orchModel || undefined;
                  const orchP = inheritOrchestrator ? undefined : orchProvider || undefined;
                  const orchT = inheritOrchestrator ? undefined : orchThinking || undefined;
                  const orchCw = inheritOrchestrator ? null : orchContextWindow;
                  const orchMo = inheritOrchestrator ? null : orchMaxOutput;
                  if (creating) {
                    await createHivemind(name, desc, roundsConfig, inheritOrchestrator, orchM, orchP, orchT, orchCw, orchMo);
                  } else if (editId) {
                    await updateHivemind(editId, name, desc, roundsConfig, inheritOrchestrator, orchM, orchP, orchT, orchCw, orchMo);
                  }
                  onSave?.();
                } catch (err) {
                  console.error("Failed to save hivemind:", err);
                } finally {
                  setSaving(false);
                }
              }
              onClose();
            }}>
              {saving ? "Saving..." : "Save Hivemind"}
            </Btn>
          </div>
        </div>

        {/* Nested model browser (round models) */}
        <ModelBrowserModal
          open={!!browser}
          onClose={() => setBrowser(null)}
          initialProvider={browser ? rounds[browser.ri]?.models[browser.mi]?.provider : undefined}
          onSelect={(model, opts) => {
            if (browser) {
              // Auto-fill maxTokens from the model's reported max output
              // (capped to keep runaway provider-reported numbers sane). If
              // the model doesn't expose a max_output, leave the existing
              // value alone so user tweaks aren't clobbered.
              const patch: Partial<RoundModel> = {
                id: model.id,
                provider: model.provider,
                ...opts,
              };
              if (typeof model.outNum === "number" && model.outNum > 0) {
                patch.maxTokens = Math.min(model.outNum, 200_000);
                patch.maxOutput = model.outNum;
              }
              // Capture the model's context window for downstream merge-prompt
              // budgeting. Both subscription providers (claude-sub/chatgpt)
              // and `/models`-backed providers populate `ctxNum` via the
              // catalog merge in commands/settings.rs::merge_with_catalog.
              if (typeof model.ctxNum === "number" && model.ctxNum > 0) {
                patch.contextWindow = model.ctxNum;
              }
              updateModel(browser.ri, browser.mi, patch);
            }
            setBrowser(null);
          }}
        />
        {/* Orchestrator model browser */}
        <ModelBrowserModal
          open={orchBrowser}
          onClose={() => setOrchBrowser(false)}
          initialProvider={orchProvider}
          onSelect={(model) => {
            setOrchModel(model.id);
            setOrchProvider(model.provider);
            // Capture orchestrator context window + max output for
            // downstream merge budgeting (see resolveMergeContextWindow
            // in taskRuntime.tsx).
            if (typeof model.ctxNum === "number" && model.ctxNum > 0) {
              setOrchContextWindow(model.ctxNum);
            } else {
              setOrchContextWindow(null);
            }
            if (typeof model.outNum === "number" && model.outNum > 0) {
              setOrchMaxOutput(model.outNum);
            } else {
              setOrchMaxOutput(null);
            }
            setOrchBrowser(false);
          }}
        />
      </div>
    </div>
  );
}

/* ── Screen wrapper (for App.tsx route compatibility) ────── */

export const HivemindEditScreen = ({ go }: { go: GoFn }) => (
  <div className="h-full flex items-center justify-center text-muted">
    <HivemindEditModal
      open={true}
      onClose={() => go("hiveminds")}
      hivemind={null}
      creating={true}
    />
  </div>
);
