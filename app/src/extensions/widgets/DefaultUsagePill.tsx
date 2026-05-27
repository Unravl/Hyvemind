import React from "react";
import type { SnapshotEntry } from "../types";

const TONE_CLASSES: Record<string, string> = {
  ok: "text-emerald-300 border-emerald-500/30 bg-emerald-500/5",
  warn: "text-amber-300 border-amber-500/30 bg-amber-500/5",
  crit: "text-red-300 border-red-500/30 bg-red-500/5",
  neutral: "text-muted border-line bg-ink-850",
};

/** Fallback Topbar pill rendered when an extension has not registered
 *  a bespoke widget. Reads `snapshot.headline` only. Renders nothing
 *  when status is not `ok`, or no headline is present. */
export function DefaultUsagePill({ entry }: { entry: SnapshotEntry }) {
  const headline = entry.snapshot?.headline;
  if (entry.status !== "ok" || !headline) return null;

  // Defensive null-tone fallback — accept malformed payloads without
  // crashing the topbar (treats missing tone as neutral).
  const tone = TONE_CLASSES[headline.tone ?? "neutral"] ?? TONE_CLASSES.neutral;
  return (
    <div
      className={`flex items-center gap-1.5 px-2.5 h-7 rounded-md border font-mono text-[12px] ${tone}`}
      title={`${entry.manifest.display_name} — ${headline.label}`}
    >
      <span className="font-medium">{headline.display}</span>
      <span className="text-dim text-[11px]">
        {entry.manifest.provider_id}
      </span>
    </div>
  );
}
