import { useCallback, useEffect, useState } from "react";
import { Modal } from "./atoms";
import { Markdown } from "./Markdown";
import { I } from "./icons";

interface MergedPlanModalProps {
  open: boolean;
  onClose: () => void;
  /** Extracted plan markdown text. */
  planText: string;
  /** Modal title, e.g. "Merged plan \u2014 Round 2". */
  title: string;
  /** Optional subtitle (typically the review's sourceLabel). */
  subtitle?: string;
}

type ViewMode = "rendered" | "source";

const VIEW_STORAGE_KEY = "hivemind.mergedPlanView";

function readStoredView(): ViewMode {
  try {
    const raw = localStorage.getItem(VIEW_STORAGE_KEY);
    if (!raw) return "rendered";
    // Stored as a JSON string. Defensive parse so corrupt values fall back.
    const parsed = JSON.parse(raw);
    return parsed === "source" ? "source" : "rendered";
  } catch {
    return "rendered";
  }
}

function writeStoredView(mode: ViewMode) {
  try {
    localStorage.setItem(VIEW_STORAGE_KEY, JSON.stringify(mode));
  } catch {
    /* localStorage may be unavailable; ignore */
  }
}

export function MergedPlanModal({
  open,
  onClose,
  planText,
  title,
  subtitle,
}: MergedPlanModalProps) {
  const [view, setView] = useState<ViewMode>(() => readStoredView());
  const [copied, setCopied] = useState(false);

  useEffect(() => {
    writeStoredView(view);
  }, [view]);

  // Reset copy state whenever the modal closes/opens.
  useEffect(() => {
    if (!open) setCopied(false);
  }, [open]);

  const handleCopy = useCallback(() => {
    navigator.clipboard
      .writeText(planText)
      .then(() => {
        setCopied(true);
        setTimeout(() => setCopied(false), 2000);
      })
      .catch(() => {
        /* clipboard may be unavailable; ignore */
      });
  }, [planText]);

  if (!open) return null;

  return (
    // Do not pass `title` to the underlying Modal \u2014 we render our own
    // header inside children so the toggle + copy + char count sit in the
    // same bar as the title.
    <Modal open={open} onClose={onClose} wide closeOnEscape>
      <div className="w-[600px] max-w-full">
        {/* Header */}
        <div className="flex items-start gap-3 mb-3">
          <div className="min-w-0 flex-1">
            <h2 className="text-base font-semibold text-slate-200">{title}</h2>
            {subtitle && (
              <p className="text-[11px] text-dim mt-0.5 truncate">{subtitle}</p>
            )}
          </div>
          <div className="flex items-center gap-2 shrink-0">
            <span className="text-[10px] text-dim font-mono">
              {planText.length} chars
            </span>
            <button
              type="button"
              onClick={handleCopy}
              className="h-6 px-2 rounded-md inline-flex items-center gap-1.5 text-[10.5px] font-medium text-dim hover:text-white hover:bg-ink-700/60 transition-colors focus:outline-none focus-visible:ring-2 focus-visible:ring-honey-500 focus-visible:ring-offset-2 focus-visible:ring-offset-ink-800"
              aria-label="Copy plan to clipboard"
              title="Copy plan to clipboard"
            >
              {copied
                ? I.check({ size: 11, className: "text-emerald-400" })
                : I.copy({ size: 11 })}
              <span>{copied ? "Copied" : "Copy"}</span>
            </button>
            <button
              type="button"
              onClick={onClose}
              className="text-muted hover:text-slate-200 transition-colors"
              aria-label="Close"
              title="Close"
            >
              {I.x({ size: 18 })}
            </button>
          </div>
        </div>

        {/* Toggle: Rendered / Source */}
        <div
          role="radiogroup"
          aria-label="Plan view mode"
          className="inline-flex items-center gap-0.5 p-0.5 rounded-md bg-ink-900 border border-line mb-3"
        >
          {(["rendered", "source"] as const).map((mode) => {
            const active = view === mode;
            const label = mode === "rendered" ? "Rendered" : "Source";
            return (
              <button
                key={mode}
                type="button"
                role="radio"
                aria-checked={active}
                onClick={() => setView(mode)}
                className={`px-2 py-0.5 rounded text-[10.5px] font-medium transition-colors focus:outline-none focus-visible:ring-2 focus-visible:ring-honey-500 focus-visible:ring-offset-2 focus-visible:ring-offset-ink-800 ${
                  active
                    ? "bg-honey-500/15 text-honey-200"
                    : "text-dim hover:text-slate-200"
                }`}
              >
                {label}
              </button>
            );
          })}
        </div>

        {/* Body */}
        <div
          className="max-h-[70vh] overflow-y-auto rounded-md border border-line bg-ink-900/40 p-3"
          data-testid="merged-plan-body"
        >
          {view === "rendered" ? (
            <div className="text-[13px] leading-relaxed text-white/90">
              <Markdown text={planText} variant="assistant" />
            </div>
          ) : (
            <pre
              className="whitespace-pre-wrap font-mono text-[12px] text-slate-300 leading-relaxed"
              data-testid="merged-plan-source"
            >
              {planText}
            </pre>
          )}
        </div>
      </div>
    </Modal>
  );
}
