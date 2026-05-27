import { useState } from "react";
import type { NurseInterventionRecord } from "../../lib/nurseTypes";
import { Pill } from "../atoms";
import { I } from "../icons";
import { SignalChip } from "./SignalChip";
import { NurseInterventionDetail } from "./NurseInterventionDetail";

const TIER_TONE: Record<string, "honey" | "blue" | "violet" | "green" | "neutral"> = {
  deterministic: "green",
  templated: "blue",
  llm: "violet",
  synthesized: "honey",
  manual: "neutral",
};

const ACTION_TONE: Record<
  string,
  "neutral" | "honey" | "blue" | "purple" | "green" | "red"
> = {
  leave_it: "neutral",
  steer: "honey",
  restart: "purple",
  cancel: "red",
};

export function NurseInterventionRow({
  record,
}: {
  record: NurseInterventionRecord;
}) {
  const [open, setOpen] = useState(false);
  const tier = record.tier ?? "synthesized";
  const action = (record.action_taken?.level || record.level || "unknown").toLowerCase();
  const outcomeGlyph =
    record.success === true ? "✓" : record.success === false ? "✗" : "·";
  const outcomeColor =
    record.success === true
      ? "text-emerald-400"
      : record.success === false
        ? "text-red-400"
        : "text-dim";

  return (
    <li className="border-b border-line last:border-b-0">
      <button
        type="button"
        onClick={() => setOpen((v) => !v)}
        aria-expanded={open}
        className="w-full text-left px-3 py-2 hover:bg-ink-700/40 transition flex items-start gap-2"
      >
        <span className={`shrink-0 w-5 text-center font-bold ${outcomeColor}`}>
          {outcomeGlyph}
        </span>
        <div className="flex-1 min-w-0">
          <div className="flex items-center gap-2 flex-wrap">
            <span className="text-[11px] text-muted font-mono">
              {new Date(record.timestamp).toLocaleString()}
            </span>
            <Pill tone={ACTION_TONE[action] ?? "neutral"}>{action}</Pill>
            <Pill tone={TIER_TONE[tier] ?? "neutral"}>{tier}</Pill>
            {record.profile && (
              <span className="text-[10px] text-dim">{record.profile}</span>
            )}
            <span
              className="text-[10px] text-muted ml-auto font-mono truncate max-w-[160px]"
              title={record.session_id}
            >
              {record.session_id.slice(0, 12)}
            </span>
          </div>
          {record.triggering_signals && record.triggering_signals.length > 0 && (
            <div className="mt-1 flex flex-wrap gap-1">
              {record.triggering_signals.slice(0, 5).map((s) => (
                <SignalChip key={s.dedup_key} signal={s} />
              ))}
            </div>
          )}
          {record.analysis && (
            <div className="mt-1 text-[11.5px] text-slate-300 line-clamp-2">
              {record.analysis}
            </div>
          )}
        </div>
        <span
          className={`text-dim transition-transform ${open ? "rotate-180" : ""}`}
        >
          {I.chevD({ size: 11 })}
        </span>
      </button>
      {open && (
        <div className="px-3 pb-3 bg-ink-900/50">
          <NurseInterventionDetail record={record} />
        </div>
      )}
    </li>
  );
}
