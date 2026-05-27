import React, { useMemo } from "react";
import type { SnapshotEntry, UsageMetric } from "../types";

const TONE_CLASSES: Record<string, string> = {
  ok: "text-emerald-300 border-emerald-500/30 bg-emerald-500/8",
  warn: "text-amber-300 border-amber-500/40 bg-amber-500/10",
  crit: "text-red-300 border-red-500/40 bg-red-500/15",
  neutral: "text-muted border-line bg-ink-850",
};

const FIVE_H_MS = 5 * 3600_000;

/** Pull a metric out of a snapshot by key. */
function findMetric(
  entry: SnapshotEntry,
  key: string,
): UsageMetric | undefined {
  return entry.snapshot?.metrics?.find((m) => m.key === key);
}

/** Fraction (0..1) of the current 5-hour window that has elapsed, based
 *  on the backend's `five_hour_resets_at` timestamp (epoch seconds).
 *  Returns 0 if the metric is missing or unparseable. */
function fiveHourElapsed(entry: SnapshotEntry): number {
  const reset = findMetric(entry, "five_hour_resets_at")?.value;
  if (!reset || reset <= 0) return 0;
  const resetMs = reset * 1000;
  const startedMs = resetMs - FIVE_H_MS;
  const frac = (Date.now() - startedMs) / FIVE_H_MS;
  return Math.min(Math.max(frac, 0), 1);
}

/** Format an epoch-seconds timestamp as either "in Xh Ym" (< 24h ahead)
 *  or "DD Mon" (≥ 24h ahead). Past timestamps return "—". */
function formatReset(epochSecs?: number): string {
  if (!epochSecs || epochSecs <= 0) return "—";
  const deltaMs = epochSecs * 1000 - Date.now();
  if (deltaMs <= 0) return "now";
  if (deltaMs < 24 * 3600_000) {
    const totalMin = Math.round(deltaMs / 60_000);
    const h = Math.floor(totalMin / 60);
    const m = totalMin % 60;
    if (h <= 0) return `in ${m}m`;
    return `in ${h}h ${m}m`;
  }
  const d = new Date(epochSecs * 1000);
  return d.toLocaleDateString(undefined, { day: "2-digit", month: "short" });
}

/** Anthropic-coral "CL" badge. */
function ClaudeBadge() {
  return (
    <span
      className="inline-flex items-center justify-center px-1.5 h-4 rounded text-[9.5px] font-bold tracking-tight"
      style={{
        backgroundColor: "rgba(204, 120, 92, 0.15)",
        color: "#cc785c",
        border: "1px solid rgba(204, 120, 92, 0.3)",
      }}
      aria-hidden
    >
      CL
    </span>
  );
}

/** Color ramp matching CroF's usage-fill colours so the two pills sit
 *  side-by-side without visual drift. */
function utilColor(usedPct: number): string {
  if (usedPct >= 90) return "rgb(252 165 165)";
  if (usedPct >= 75) return "rgb(251 146 60)";
  if (usedPct >= 60) return "rgb(252 211 77)";
  return "rgb(110 231 183)";
}

/** Tooltip body describing weekly window + per-model utilization. */
function buildTooltip(entry: SnapshotEntry): string {
  const five = findMetric(entry, "five_hour_utilization");
  const fiveReset = findMetric(entry, "five_hour_resets_at")?.value;
  const week = findMetric(entry, "seven_day_utilization");
  const weekReset = findMetric(entry, "seven_day_resets_at")?.value;
  const sonnet = findMetric(entry, "seven_day_sonnet_utilization");
  const opus = findMetric(entry, "seven_day_opus_utilization");

  const lines = ["Claude Subscription"];
  if (five) {
    lines.push(`5 h:    ${five.display}  (resets ${formatReset(fiveReset)})`);
  }
  if (week) {
    lines.push(`Week:   ${week.display}  (resets ${formatReset(weekReset)})`);
  } else {
    lines.push("Week:   —");
  }
  lines.push(`Sonnet: ${sonnet ? sonnet.display : "—"}`);
  lines.push(`Opus:   ${opus ? opus.display : "—"}`);
  return lines.join("\n");
}

/** Claude subscription usage pill — visually identical layout to the
 *  CroF pill: brand badge → progress bar with time-elapsed notch →
 *  percentage label. Supports `display_mode` preference:
 *    - "percentage" (default): badge + bar + "N%"
 *    - "ratio":                badge + "N% / 100%"
 *
 *  Falls through to no-op when status isn't ok or no headline exists.
 *  Renders a flat "Idle" pill when the backend reports no active
 *  session (tone=neutral, display="Idle"). */
export function ClaudeSubUsagePill({ entry }: { entry: SnapshotEntry }) {
  const headline = entry.snapshot?.headline;
  if (entry.status !== "ok" || !headline) return null;

  const tone = TONE_CLASSES[headline.tone] ?? TONE_CLASSES.neutral;
  const tooltip = buildTooltip(entry);
  const isIdle = headline.tone === "neutral" && headline.display === "Idle";
  const displayMode =
    entry.user_settings.preferences?.display_mode ?? "percentage";

  // Notch advances only when the snapshot's reset epoch changes — i.e.
  // every ~5 min on snapshot updates. Matches CroF's render-time
  // computation pattern.
  const elapsed = useMemo(
    () => fiveHourElapsed(entry),
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [entry.snapshot?.fetched_at, findMetric(entry, "five_hour_resets_at")?.value],
  );

  // ── Idle: flat bar, muted label, no notch.
  if (isIdle) {
    return (
      <div
        className={`flex items-center gap-1.5 px-2.5 h-7 rounded-md border font-mono text-[12px] ${TONE_CLASSES.neutral}`}
        title={tooltip}
      >
        <ClaudeBadge />
        <div className="relative w-12 h-2 rounded-full bg-ink-800 border border-white/30 overflow-hidden shrink-0" />
        <span className="font-semibold tabular-nums">Idle</span>
      </div>
    );
  }

  // ── Ratio mode: skip the bar, just show "N% / 100%".
  if (displayMode === "ratio") {
    return (
      <div
        className={`flex items-center gap-1.5 px-2.5 h-7 rounded-md border font-mono text-[12px] ${tone}`}
        title={tooltip}
      >
        <ClaudeBadge />
        <span className="font-semibold tabular-nums">
          {headline.display} / 100%
        </span>
      </div>
    );
  }

  // ── Percentage mode (default): badge + bar + notch + readout.
  const usedPct = Math.min(Math.max(Math.round(headline.value), 0), 100);
  const fillColor = utilColor(usedPct);

  return (
    <div
      className={`flex items-center gap-1.5 px-2.5 h-7 rounded-md border font-mono text-[12px] ${tone}`}
      title={tooltip}
    >
      <ClaudeBadge />
      {/* Progress bar with time-elapsed notch */}
      <div className="relative w-12 h-2 rounded-full bg-ink-800 border border-white/30 overflow-hidden shrink-0">
        {/* Utilization fill */}
        <div
          className="h-full rounded-full transition-all"
          style={{
            width: `${usedPct}%`,
            backgroundColor: fillColor,
          }}
        />
        {/* 5h-window notch — thin white line at elapsed fraction */}
        <div
          className="absolute top-0 w-px h-full pointer-events-none"
          style={{
            left: `${elapsed * 100}%`,
            backgroundColor: "rgba(255,255,255,1)",
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
