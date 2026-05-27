import React from "react";
import type { SnapshotEntry } from "../types";

const TONE_CLASSES: Record<string, string> = {
  ok: "text-emerald-300 border-emerald-500/30 bg-emerald-500/8",
  warn: "text-amber-300 border-amber-500/40 bg-amber-500/10",
  crit: "text-red-300 border-red-500/40 bg-red-500/15",
  neutral: "text-muted border-line bg-ink-850",
};

/** Bespoke OpenRouter credits pill — shows remaining credits with a
 *  small "OR" badge. Falls through to a no-op when no headline is
 *  available. Demonstrates the registry-overridable widget pattern. */
export function OpenRouterCreditsPill({ entry }: { entry: SnapshotEntry }) {
  const headline = entry.snapshot?.headline;
  if (entry.status !== "ok" || !headline) return null;

  const tone = TONE_CLASSES[headline.tone] ?? TONE_CLASSES.neutral;
  return (
    <div
      className={`flex items-center gap-1.5 px-2.5 h-7 rounded-md border font-mono text-[12px] ${tone}`}
      title={`OpenRouter (${entry.manifest.provider_id}) — ${headline.label}: ${headline.display}`}
    >
      <span
        className="inline-flex items-center justify-center px-1.5 h-4 rounded text-[9.5px] font-bold tracking-tight bg-honey-500/15 text-honey-300 border border-honey-500/30"
        aria-hidden
      >
        OR
      </span>
      <span className="font-semibold">{headline.display}</span>
    </div>
  );
}
