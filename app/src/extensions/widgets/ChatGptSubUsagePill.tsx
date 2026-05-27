import React, { useMemo } from "react";
import type { SnapshotEntry, UsageMetric } from "../types";

const TONE_CLASSES: Record<string, string> = {
  ok: "text-emerald-300 border-emerald-500/30 bg-emerald-500/8",
  warn: "text-amber-300 border-amber-500/40 bg-amber-500/10",
  crit: "text-red-300 border-red-500/40 bg-red-500/15",
  neutral: "text-muted border-line bg-ink-850",
};

/** Fallback window size when the backend doesn't ship
 *  `primary_window_seconds`. Matches the historical 5-hour Codex
 *  bucket — keeps the notch in the right spot for older payloads. */
const PRIMARY_WINDOW_MS_FALLBACK = 5 * 3600_000;

/** Pull a metric out of a snapshot by key. */
function findMetric(
  entry: SnapshotEntry,
  key: string,
): UsageMetric | undefined {
  return entry.snapshot?.metrics?.find((m) => m.key === key);
}

/** Fraction (0..1) of the current primary window that has elapsed,
 *  based on the backend's `primary_resets_at` timestamp (epoch
 *  seconds) and `primary_window_seconds` when present. Falls back to
 *  the 5-hour bucket size to avoid a visual regression. Returns 0 if
 *  the metric is missing or unparseable. */
function fiveHourElapsed(entry: SnapshotEntry): number {
  const reset = findMetric(entry, "primary_resets_at")?.value;
  if (!reset || reset <= 0) return 0;
  const windowSecs = findMetric(entry, "primary_window_seconds")?.value;
  const windowMs =
    windowSecs && windowSecs > 0
      ? windowSecs * 1000
      : PRIMARY_WINDOW_MS_FALLBACK;
  const resetMs = reset * 1000;
  const startedMs = resetMs - windowMs;
  const frac = (Date.now() - startedMs) / windowMs;
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

/** OpenAI-green "GPT" badge. */
function ChatGptBadge() {
  return (
    <span
      className="inline-flex items-center justify-center px-1.5 h-4 rounded text-[9.5px] font-bold tracking-tight"
      style={{
        backgroundColor: "rgba(16, 163, 127, 0.15)",
        color: "#10a37f",
        border: "1px solid rgba(16, 163, 127, 0.3)",
      }}
      aria-hidden
    >
      GPT
    </span>
  );
}

/** Color ramp matching CroF's usage-fill colours so the pills sit
 *  side-by-side without visual drift. */
function utilColor(usedPct: number): string {
  if (usedPct >= 90) return "rgb(252 165 165)";
  if (usedPct >= 75) return "rgb(251 146 60)";
  if (usedPct >= 60) return "rgb(252 211 77)";
  return "rgb(110 231 183)";
}

/** Tooltip body describing primary (5h) + secondary (weekly) windows
 *  plus plan + credits when present. */
function buildTooltip(entry: SnapshotEntry): string {
  const primary = findMetric(entry, "primary_utilization");
  const primaryReset = findMetric(entry, "primary_resets_at")?.value;
  const secondary = findMetric(entry, "secondary_utilization");
  const secondaryReset = findMetric(entry, "secondary_resets_at")?.value;
  const planType = findMetric(entry, "plan_type")?.display;
  const creditsBalance = findMetric(entry, "credits_balance")?.display;

  const lines = ["ChatGPT Subscription"];
  if (primary) {
    lines.push(
      `5 h:    ${primary.display}  (resets ${formatReset(primaryReset)})`,
    );
  }
  if (secondary) {
    lines.push(
      `Week:   ${secondary.display}  (resets ${formatReset(secondaryReset)})`,
    );
  } else {
    lines.push("Week:   —");
  }
  lines.push(`Plan:   ${planType ?? "—"}`);
  if (creditsBalance) {
    lines.push(`Credits: ${creditsBalance}`);
  }
  return lines.join("\n");
}

/** ChatGPT subscription usage pill — visually identical layout to
 *  the Claude pill: brand badge → progress bar with time-elapsed
 *  notch → percentage label. Supports `display_mode` preference:
 *    - "percentage" (default): badge + bar + "N%"
 *    - "ratio":                badge + "N% / 100%"
 *
 *  Falls through to no-op when status isn't ok or no headline exists.
 *  Renders a flat "Idle" pill when the backend reports no active
 *  session (tone=neutral, display="Idle"). */
export function ChatGptSubUsagePill({ entry }: { entry: SnapshotEntry }) {
  const headline = entry.snapshot?.headline;
  if (entry.status !== "ok" || !headline) return null;

  const tone = TONE_CLASSES[headline.tone] ?? TONE_CLASSES.neutral;
  const tooltip = buildTooltip(entry);
  const isIdle = headline.tone === "neutral" && headline.display === "Idle";
  const displayMode =
    entry.user_settings.preferences?.display_mode ?? "percentage";

  // Notch advances only when the snapshot's reset epoch changes — i.e.
  // every ~5 min on snapshot updates. Matches the Claude pill's
  // render-time computation pattern.
  const elapsed = useMemo(
    () => fiveHourElapsed(entry),
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [entry.snapshot?.fetched_at, findMetric(entry, "primary_resets_at")?.value],
  );

  // ── Idle: flat bar, muted label, no notch.
  if (isIdle) {
    return (
      <div
        className={`flex items-center gap-1.5 px-2.5 h-7 rounded-md border font-mono text-[12px] ${TONE_CLASSES.neutral}`}
        title={tooltip}
      >
        <ChatGptBadge />
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
        <ChatGptBadge />
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
      <ChatGptBadge />
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
        {/* Primary-window notch — thin white line at elapsed fraction */}
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
