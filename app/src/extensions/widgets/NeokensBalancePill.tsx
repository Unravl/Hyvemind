import React from "react";
import type { SnapshotEntry } from "../types";

const TONE_CLASSES: Record<string, string> = {
  ok: "text-purple-300 border-purple-500/30 bg-purple-500/8",
  warn: "text-amber-300 border-amber-500/40 bg-amber-500/10",
  crit: "text-red-300 border-red-500/40 bg-red-500/15",
  neutral: "text-muted border-line bg-ink-850",
};

/**
 * Bespoke Neokens balance pill showing remaining credits
 * with a distinct purple "NK" badge. Handles depleted states gracefully.
 */
export function NeokensBalancePill({ entry }: { entry: SnapshotEntry }) {
  const headline = entry.snapshot?.headline;
  if (entry.status !== "ok" || !headline) return null;

  const tone = (headline?.tone ?? "neutral") as string;
  const toneClass = TONE_CLASSES[tone] ?? TONE_CLASSES.neutral;

  // Zero / negative balance -> "Depleted" state
  const isDepleted = headline.value <= 0;
  const containerClass = isDepleted ? TONE_CLASSES.crit : toneClass;

  return (
    <div
      className={`flex items-center gap-1.5 px-2.5 h-7 rounded-md border font-mono text-[12px] ${containerClass}`}
      title={`Neokens (${entry.manifest.provider_id}) — ${headline.label}: ${headline.display}`}
    >
      <NeokensBadge />
      {isDepleted ? (
        <span className="font-semibold text-red-300">Depleted</span>
      ) : (
        <span className="font-semibold">{headline.display}</span>
      )}
    </div>
  );
}

/** Violet "NK" badge matching the Neokens visual theme. */
function NeokensBadge() {
  return (
    <span
      className="inline-flex items-center justify-center px-1.5 h-4 rounded text-[9.5px] font-bold tracking-tight"
      style={{
        backgroundColor: "rgba(139, 92, 246, 0.15)",
        color: "#a78bfa",
        border: "1px solid rgba(139, 92, 246, 0.3)",
      }}
      aria-hidden
    >
      NK
    </span>
  );
}
