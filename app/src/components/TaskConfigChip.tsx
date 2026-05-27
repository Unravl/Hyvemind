import React, { useState, useRef, useEffect } from "react";
import { I } from "./icons";
import { Select } from "./atoms";
import type { HivemindSummary } from "../lib/types";
import { ModelBrowserModal } from "../screens/ModelBrowser";

const TASK_HIVEMINDS = ["enhance", "arch-council", "security-review", "perf-audit"];

/* ── TaskConfigChip ───────────────────────────────────────── */

export const TaskConfigChip = ({
  model,
  onModelChange,
  hivemind,
  onHivemindChange,
  onThinkingChange,
  onContextWindowChange,
  hivemindOptions,
}: {
  model: string;
  onModelChange: (model: string) => void;
  hivemind: string | null;
  onHivemindChange: (h: string | null) => void;
  onThinkingChange?: (t: string) => void;
  /** Called with the selected model's context-window size (tokens) when
   *  known. Used by Tasks.tsx to seed `activeTask.contextWindowHint`, which
   *  the bottom-of-view meter reads as a fallback when Pi reports 0. */
  onContextWindowChange?: (ctx: number | undefined) => void;
  hivemindOptions?: HivemindSummary[] | null;
}) => {
  const [open, setOpen] = useState(false);
  const [showBrowser, setShowBrowser] = useState(false);
  const ref = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!open) return;
    const onDoc = (e: MouseEvent) => {
      if (ref.current && !ref.current.contains(e.target as Node))
        setOpen(false);
    };
    document.addEventListener("mousedown", onDoc);
    return () => document.removeEventListener("mousedown", onDoc);
  }, [open]);

  const hivemindSelectOptions = hivemindOptions
    ? hivemindOptions.map((h) => ({ value: h.id, label: h.name }))
    : TASK_HIVEMINDS.map((h) => ({ value: h, label: h }));
  const hivemindDisplayName = hivemindOptions
    ? (hivemindOptions.find((h) => h.id === hivemind)?.name || "none")
    : (hivemind || "none");
  const providerName = model.includes("/") ? model.split("/")[0] : "";
  const displayModel = model.includes("/") ? model.split("/").slice(1).join("/") : model;

  return (
    <div className="relative" ref={ref}>
      <button
        onClick={() => setOpen((v) => !v)}
        className={`h-12 px-3.5 flex items-center gap-3 rounded-lg border bg-ink-800/60 hover:bg-ink-800 transition ${
          open ? "border-honey-500/50" : "border-line"
        }`}
      >
        <div className="flex flex-col items-start">
          <div className="text-[10px] uppercase tracking-wider text-dim font-semibold leading-none mb-1.5">
            {providerName ? providerName.charAt(0).toUpperCase() + providerName.slice(1) : "Model"}
          </div>
          <div className="flex items-center gap-1.5 text-[12.5px] font-mono leading-none">
            <span className="text-blue-200">{displayModel || "none"}</span>
          </div>
        </div>
        <div className="w-px h-6 bg-line" />
        <div className="flex flex-col items-start">
          <div className="text-[10px] uppercase tracking-wider text-dim font-semibold leading-none mb-1.5">
            Hivemind
          </div>
          <div className="flex items-center gap-1.5 text-[12.5px] font-mono leading-none text-honey-200">
            {I.hexFill({ size: 11, className: "text-honey-400" })}
            {hivemindDisplayName}
          </div>
        </div>
        {I.chevD({
          size: 11,
          className: `text-dim ml-1 transition ${open ? "rotate-180" : ""}`,
        })}
      </button>

      {open && (
        <div className="absolute right-0 top-[calc(100%+6px)] w-[300px] bg-ink-850 border border-line rounded-xl shadow-2xl z-30 overflow-hidden">
          <div className="px-3.5 py-2.5 border-b border-line text-[10.5px] uppercase tracking-[.18em] text-dim font-semibold">
            Task config
          </div>

          <div className="px-3.5 pt-3 pb-3 space-y-2.5">
            <div>
              <div className="text-[10.5px] uppercase tracking-wider text-dim font-semibold mb-1">
                Model
              </div>
              <button
                onClick={() => setShowBrowser(true)}
                className="w-full h-8 px-2.5 rounded-md bg-ink-900 border border-line hover:border-honey-500/40 text-left flex items-center justify-between gap-2 group"
              >
                <span className="font-mono text-[12.5px] truncate text-white">
                  {displayModel || "Choose model..."}
                </span>
                {I.search({ size: 13, className: "text-dim group-hover:text-honey-400" })}
              </button>
            </div>
          </div>

          <div className="border-t border-line px-3.5 pt-3 pb-3">
            <div className="text-[10.5px] uppercase tracking-wider text-dim font-semibold mb-1">
              Hivemind review
            </div>
            <Select
              wrapClass="w-full"
              className="w-full"
              value={hivemind || ""}
              onChange={(e) => onHivemindChange(e.target.value || null)}
              options={[
                { value: "", label: "None -- skip review" },
                ...hivemindSelectOptions,
              ]}
            />
          </div>
        </div>
      )}

      <ModelBrowserModal
        open={showBrowser}
        onClose={() => setShowBrowser(false)}
        onSelect={(m, opts) => {
          onModelChange(`${m.provider}/${m.id}`);
          if (opts?.thinking) onThinkingChange?.(opts.thinking);
          onContextWindowChange?.(m.ctxNum);
          setShowBrowser(false);
        }}
        initialModel={model}
      />
    </div>
  );
};
