import { useState } from "react";
import type { ActiveSignal } from "../../lib/nurseTypes";
import { SEVERITY_BG, SEVERITY_TEXT } from "./SeverityBadge";

/**
 * Compact chip representing one active detector signal on a session.
 * Click expands an inline evidence-JSON panel for the chip's row.
 */
export function SignalChip({
  signal,
  className = "",
}: {
  signal: ActiveSignal;
  className?: string;
}) {
  const [open, setOpen] = useState(false);
  const hasEvidence = signal.evidence !== undefined && signal.evidence !== null;

  return (
    <span className={`inline-flex flex-col gap-0.5 ${className}`}>
      <button
        type="button"
        onClick={() => hasEvidence && setOpen((v) => !v)}
        className={`inline-flex items-center gap-1 px-1.5 py-0.5 rounded text-[10.5px] font-mono ${SEVERITY_BG[signal.severity]} ${SEVERITY_TEXT[signal.severity]} ${hasEvidence ? "cursor-pointer hover:brightness-125" : "cursor-default"}`}
        title={signal.description}
        aria-expanded={hasEvidence ? open : undefined}
      >
        {signal.detector}
        {signal.dedup_key && signal.dedup_key !== signal.detector && (
          <span className="opacity-60">:{shortenDedup(signal.dedup_key, signal.detector)}</span>
        )}
      </button>
      {open && hasEvidence && (
        <pre className="text-[10px] text-muted bg-ink-900 border border-line rounded p-1.5 max-h-32 overflow-auto whitespace-pre-wrap break-words">
          {safeJson(signal.evidence)}
        </pre>
      )}
    </span>
  );
}

/** `loop:exact:abc12345xyz` → `exact:abc1234…` when too long. */
function shortenDedup(key: string, detector: string): string {
  const stripped = key.startsWith(`${detector}:`)
    ? key.slice(detector.length + 1)
    : key;
  if (stripped.length <= 18) return stripped;
  return stripped.slice(0, 16) + "…";
}

function safeJson(v: unknown): string {
  try {
    return JSON.stringify(v, null, 2);
  } catch {
    return String(v);
  }
}
