import React, { useMemo } from "react";
import type { SnapshotEntry } from "../types";

/** ICT is UTC+7 — reset happens at 12:00 ICT = 05:00 UTC. */
const RESET_HOUR_UTC = 5;
const HOUR_MS = 3600_000;

/** Fraction of time elapsed in the 24 h cycle (0.0–1.0) since 12pm ICT reset. */
function cycleElapsed(): number {
  const now = Date.now();
  const utcHours = new Date(now).getUTCHours();
  const utcMinutes = new Date(now).getUTCMinutes();
  const utcSeconds = new Date(now).getUTCSeconds();
  const msToday = ((utcHours * 60 + utcMinutes) * 60 + utcSeconds) * 1000;
  // The day starts (reset = 0%) at RESET_HOUR_UTC (05:00 UTC = 12:00 ICT).
  const msSinceReset = (msToday - RESET_HOUR_UTC * HOUR_MS + 24 * HOUR_MS) % (24 * HOUR_MS);
  return msSinceReset / (24 * HOUR_MS);
}

const TONE_CLASSES: Record<string, string> = {
  ok: "text-emerald-300 border-emerald-500/30 bg-emerald-500/8",
  warn: "text-amber-300 border-amber-500/40 bg-amber-500/10",
  crit: "text-red-300 border-red-500/40 bg-red-500/15",
  neutral: "text-muted border-line bg-ink-850",
};

/** Bespoke CroF usage pill — shows daily requests remaining with a
 *  purple "CF" badge. Supports two display modes via preferences:
 *  - "ratio" (default): "2200 / 2500" with "req / d" label
 *  - "percentage": "88%" with a progress bar and "req / d" label
 *
 *  Falls back to credits remaining when requests data is unavailable.
 *  Falls through to a no-op when no headline is present. */
export function CrofUsagePill({ entry }: { entry: SnapshotEntry }) {
  const headline = entry.snapshot?.headline;
  if (entry.status !== "ok" || !headline) return null;

  // Only apply display-mode switching for the requests headline.
  // Credits just show as-is.
  const isRequests =
    headline.key === "usable_requests" &&
    entry.snapshot?.metrics?.some((m) => m.key === "requests_plan");

  if (!isRequests) {
    return <SimpleCrofPill entry={entry} headline={headline} />;
  }

  return <RequestsCrofPill entry={entry} headline={headline} />;
}

type Headline = NonNullable<NonNullable<SnapshotEntry["snapshot"]>["headline"]>;

/** Pill for the credits headline (no N/M, just $X.XX). */
function SimpleCrofPill({
  entry,
  headline,
}: {
  entry: SnapshotEntry;
  headline: Headline;
}) {
  const tone = TONE_CLASSES[headline.tone] ?? TONE_CLASSES.neutral;
  return (
    <div
      className={`flex items-center gap-1.5 px-2.5 h-7 rounded-md border font-mono text-[12px] ${tone}`}
      title={`Crof (${entry.manifest.provider_id}) — ${headline.label}: ${headline.display}`}
    >
      <CrofBadge />
      <span className="font-semibold">{headline.display}</span>
    </div>
  );
}

/** Pill for the requests headline — supports ratio and % modes. */
function RequestsCrofPill({
  entry,
  headline,
}: {
  entry: SnapshotEntry;
  headline: Headline;
}) {
  const tone = TONE_CLASSES[headline.tone] ?? TONE_CLASSES.neutral;
  const displayMode =
    entry.user_settings.preferences?.display_mode ?? "percentage";

  // Find the requests_plan from metrics
  const planMetric = entry.snapshot?.metrics?.find(
    (m) => m.key === "requests_plan",
  );
  const plan = planMetric?.value ?? 0;
  const remaining = headline.value;
  const pct = plan > 0 ? Math.round((remaining / plan) * 100) : 100;
  const usedPct = Math.min(Math.max(100 - pct, 0), 100);

  // Day-cycle notch: fraction elapsed since 12pm ICT reset.
  const dayElapsed = useMemo(() => cycleElapsed(), []);

  return (
    <div
      className={`flex items-center gap-1.5 px-2.5 h-7 rounded-md border font-mono text-[12px] ${tone}`}
      title={`Crof (${entry.manifest.provider_id}) — ${remaining} / ${plan} requests today`}
    >
      <CrofBadge />
      {displayMode === "percentage" ? (
        <>
          {/* Progress bar with day-cycle notch */}
          <div className="relative w-12 h-2 rounded-full bg-ink-800 border border-white/30 overflow-hidden shrink-0">
            {/* Request-usage fill */}
            <div
              className="h-full rounded-full transition-all"
              style={{
                width: `${usedPct}%`,
                backgroundColor:
                  usedPct >= 90
                    ? "rgb(252 165 165)"
                    : usedPct >= 75
                      ? "rgb(251 146 60)"
                      : usedPct >= 60
                        ? "rgb(252 211 77)"
                        : "rgb(110 231 183)",
              }}
            />
            {/* Day-cycle notch — thin white line at time-elapsed fraction */}
            <div
              className="absolute top-0 w-px h-full pointer-events-none"
              style={{
                left: `${dayElapsed * 100}%`,
                backgroundColor: "rgba(255,255,255,1)",
              }}
            />
          </div>
          <span
            className="font-semibold tabular-nums"
            style={{
              color:
                usedPct >= 90
                  ? "rgb(252 165 165)"
                  : usedPct >= 75
                    ? "rgb(251 146 60)"
                    : usedPct >= 60
                      ? "rgb(252 211 77)"
                      : "rgb(110 231 183)",
            }}
          >
            {usedPct}%
          </span>
        </>
      ) : (
        <span className="font-semibold tabular-nums">
          {remaining} / {plan}
        </span>
      )}
    </div>
  );
}

/** Purple "CF" badge matching the #a855f7 crof brand colour. */
function CrofBadge() {
  return (
    <span
      className="inline-flex items-center justify-center px-1.5 h-4 rounded text-[9.5px] font-bold tracking-tight"
      style={{
        backgroundColor: "rgba(168, 85, 247, 0.15)",
        color: "#a855f7",
        border: "1px solid rgba(168, 85, 247, 0.3)",
      }}
      aria-hidden
    >
      CF
    </span>
  );
}
