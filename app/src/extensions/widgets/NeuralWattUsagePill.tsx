import React from "react";
import type { SnapshotEntry } from "../types";

const TONE_CLASSES: Record<string, string> = {
  ok: "text-emerald-300 border-emerald-500/30 bg-emerald-500/8",
  warn: "text-amber-300 border-amber-500/40 bg-amber-500/10",
  crit: "text-red-300 border-red-500/40 bg-red-500/15",
  neutral: "text-muted border-line bg-ink-850",
};

/** Map a used-percentage value (0–100) to a display colour.
 *  Matches the Crof usage-pill colour logic. */
function usedPctColor(pct: number): string {
  if (pct >= 90) return "rgb(252 165 165)"; // red
  if (pct >= 75) return "rgb(251 146 60)"; // amber
  if (pct >= 60) return "rgb(252 211 77)"; // yellow
  return "rgb(110 231 183)"; // green
}

/** Bespoke NeuralWatt usage pill — shows energy consumption with
 *  a teal "NW" badge. Supports two display modes:
 *
 *  - **Subscription mode** (plan data available via `kwh_included`
 *    metric): progress bar showing `kwh_used` / `kwh_included` with
 *    a percentage label tinted by the used-proportion colour logic.
 *  - **Fallback mode** (no plan / pay-as-you-go): simple kWh display
 *    string, same as `DefaultUsagePill` but with the NW badge.
 *
 *  Falls through to a no-op when no headline is present or the
 *  status is not `ok`. */
export function NeuralWattUsagePill({ entry }: { entry: SnapshotEntry }) {
  const headline = entry.snapshot?.headline;
  if (entry.status !== "ok" || !headline) return null;

  const tone = (headline?.tone ?? "neutral") as string;
  const toneClass = TONE_CLASSES[tone] ?? TONE_CLASSES.neutral;

  // Detect subscription mode: the backend only populates kwh_included
  // when plan data exists and the value is > 0.0.
  const kwhIncluded = entry.snapshot?.metrics?.find(
    (m) => m.key === "kwh_included",
  );
  const hasPlanData = kwhIncluded != null && kwhIncluded.value > 0;

  if (hasPlanData) {
    // Subscription mode — progress bar + percentage label.
    const usedPct = Math.min(Math.max(Math.round(headline.value), 0), 100);
    const kwhUsed =
      entry.snapshot?.metrics?.find((m) => m.key === "kwh_used")?.value ?? 0;
    const fillColor = usedPctColor(usedPct);

    return (
      <div
        className={`flex items-center gap-1.5 px-2.5 h-7 rounded-md border font-mono text-[12px] ${toneClass}`}
        title={`NeuralWatt — ${kwhUsed.toFixed(4)} / ${kwhIncluded.value.toFixed(4)} kWh`}
      >
        <NeuralWattBadge />
        {/* Progress bar — 48 px wide, 8 px tall */}
        <div className="relative w-12 h-2 rounded-full bg-ink-800 border border-white/30 overflow-hidden shrink-0">
          <div
            className="h-full rounded-full transition-all"
            style={{
              width: `${usedPct}%`,
              backgroundColor: fillColor,
            }}
          />
        </div>
        <span
          className="font-semibold tabular-nums"
          style={{ color: fillColor }}
        >
          {usedPct}%
        </span>
      </div>
    );
  }

  // Fallback mode — simple kWh display with NW badge.
  return (
    <div
      className={`flex items-center gap-1.5 px-2.5 h-7 rounded-md border font-mono text-[12px] ${toneClass}`}
      title={`NeuralWatt (${entry.manifest.provider_id}) — ${headline.label}: ${headline.display}`}
    >
      <NeuralWattBadge />
      <span className="font-semibold">{headline.display}</span>
    </div>
  );
}

/** Teal "NW" badge matching the NeuralWatt brand accent (#14b8a6). */
function NeuralWattBadge() {
  return (
    <span
      className="inline-flex items-center justify-center px-1.5 h-4 rounded text-[9.5px] font-bold tracking-tight"
      style={{
        backgroundColor: "rgba(20, 184, 166, 0.15)",
        color: "#14b8a6",
        border: "1px solid rgba(20, 184, 166, 0.3)",
      }}
      aria-hidden
    >
      NW
    </span>
  );
}
