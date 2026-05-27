import React from "react";
import type { SnapshotEntry } from "../types";

const TONE_CLASSES: Record<string, string> = {
  ok: "text-emerald-300 border-emerald-500/30 bg-emerald-500/8",
  warn: "text-amber-300 border-amber-500/40 bg-amber-500/10",
  crit: "text-red-300 border-red-500/40 bg-red-500/15",
  neutral: "text-muted border-line bg-ink-850",
};

/** Bespoke DeepSeek balance pill — shows remaining account credit
 *  with a teal "DS" badge. When the balance reaches zero or goes
 *  negative the pill flips to a red "Depleted" state.
 *
 *  Falls through to a no-op when no headline is present or the
 *  status is not `ok`. */
export function DeepseekBalancePill({ entry }: { entry: SnapshotEntry }) {
  const headline = entry.snapshot?.headline;
  // Defensive: null/undefined fallback in case of malformed payloads.
  if (entry.status !== "ok" || !headline) return null;

  const tone = (headline?.tone ?? "neutral") as string;
  const toneClass = TONE_CLASSES[tone] ?? TONE_CLASSES.neutral;

  // Zero / negative balance → "Depleted" state (always crit-ish).
  const isDepleted = headline.value <= 0;
  const containerClass = isDepleted ? TONE_CLASSES.crit : toneClass;

  return (
    <div
      className={`flex items-center gap-1.5 px-2.5 h-7 rounded-md border font-mono text-[12px] ${containerClass}`}
      title={`DeepSeek (${entry.manifest.provider_id}) — ${headline.label}: ${headline.display}`}
    >
      <DeepseekBadge />
      {isDepleted ? (
        <span className="font-semibold text-red-300">Depleted</span>
      ) : (
        <>
          <span className="font-semibold">{headline.display}</span>
        </>
      )}
    </div>
  );
}

/** Teal "DS" badge matching the DeepSeek brand accent. */
function DeepseekBadge() {
  return (
    <span
      className="inline-flex items-center justify-center px-1.5 h-4 rounded text-[9.5px] font-bold tracking-tight"
      style={{
        backgroundColor: "rgba(67, 217, 173, 0.15)",
        color: "#43d9ad",
        border: "1px solid rgba(67, 217, 173, 0.3)",
      }}
      aria-hidden
    >
      DS
    </span>
  );
}
