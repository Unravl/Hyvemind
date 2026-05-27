import React from "react";
import { FocusTrap } from "focus-trap-react";
import { I } from "./icons";
import { relativeTimeDetailed } from "../lib/time";

/* ── Status config ─────────────────────────────────────────── */

export const STATUS: Record<
  string,
  { label: string; dot: string; bg: string; text: string; pulse?: string }
> = {
  running: {
    label: "Running",
    dot: "bg-green-400",
    bg: "bg-green-500/10",
    text: "text-green-400",
    pulse: "pulse-green",
  },
  paused: {
    label: "Paused",
    dot: "bg-honey-400",
    bg: "bg-honey-500/10",
    text: "text-honey-400",
    pulse: "pulse-amber",
  },
  completed: {
    label: "Completed",
    dot: "bg-blue-400",
    bg: "bg-blue-500/10",
    text: "text-blue-400",
  },
  failed: {
    label: "Failed",
    dot: "bg-red-400",
    bg: "bg-red-500/10",
    text: "text-red-400",
  },
  planning: {
    label: "Planning",
    dot: "bg-purple-400",
    bg: "bg-purple-500/10",
    text: "text-purple-400",
  },
};

/* ── Btn ───────────────────────────────────────────────────── */

interface BtnProps extends React.ButtonHTMLAttributes<HTMLButtonElement> {
  kind?: "primary" | "secondary" | "ghost" | "outline" | "danger" | "success";
  size?: "sm" | "md" | "lg";
  icon?: React.ReactNode;
  children?: React.ReactNode;
  /**
   * When true, marks the button as performing an async action. Sets
   * `aria-busy="true"` so screen readers can announce the in-flight state.
   * No live region is rendered — visual spinners should still be supplied
   * via `icon` / children. See `src/A11Y.md` for rationale.
   */
  loading?: boolean;
}

const btnKind: Record<string, string> = {
  primary:
    "bg-honey-500 text-ink-950 hover:bg-honey-400 active:bg-honey-600 font-semibold shadow-[0_1px_2px_rgba(0,0,0,.4)]",
  secondary:
    "bg-ink-600 text-slate-200 hover:bg-ink-500 active:bg-ink-700 border border-line",
  ghost:
    "bg-transparent text-muted hover:text-slate-200 hover:bg-ink-700/60",
  outline:
    "bg-transparent text-slate-200 border border-line hover:border-line-strong hover:bg-ink-700/40",
  danger:
    "bg-red-500/10 text-red-400 hover:bg-red-500/20 border border-red-500/20",
  success:
    "bg-green-500/10 text-green-400 hover:bg-green-500/20 border border-green-500/20",
};

const btnSize: Record<string, string> = {
  sm: "text-xs px-2.5 py-1 gap-1.5 rounded-md",
  md: "text-sm px-3.5 py-1.5 gap-2 rounded-lg",
  lg: "text-sm px-5 py-2.5 gap-2.5 rounded-lg",
};

export function Btn({
  kind = "secondary",
  size = "md",
  icon,
  children,
  className = "",
  loading = false,
  "aria-busy": ariaBusyProp,
  ...rest
}: BtnProps) {
  // Prefer an explicit aria-busy from the caller; otherwise derive from `loading`.
  const ariaBusy = ariaBusyProp ?? (loading ? true : undefined);
  return (
    <button
      className={`inline-flex items-center justify-center whitespace-nowrap transition-colors duration-100 cursor-pointer disabled:opacity-40 disabled:pointer-events-none focus:outline-none focus-visible:ring-2 focus-visible:ring-honey-500 focus-visible:ring-offset-2 focus-visible:ring-offset-ink-950 ${btnKind[kind]} ${btnSize[size]} ${className}`}
      aria-busy={ariaBusy}
      {...rest}
    >
      {icon}
      {children}
    </button>
  );
}

/* ── Panel ─────────────────────────────────────────────────── */

interface PanelProps {
  title?: React.ReactNode;
  right?: React.ReactNode;
  children?: React.ReactNode;
  className?: string;
  bodyClass?: string;
  noPad?: boolean;
}

export function Panel({
  title,
  right,
  children,
  className = "",
  bodyClass = "",
  noPad,
}: PanelProps) {
  return (
    <div
      className={`rounded-xl border border-line bg-ink-800 shadow-panel ${className}`}
    >
      {(title || right) && (
        <div className="flex items-center justify-between px-4 py-3 border-b border-line">
          {typeof title === "string" ? (
            <h3 className="text-sm font-semibold text-slate-200">{title}</h3>
          ) : (
            title
          )}
          {right && <div className="flex items-center gap-2">{right}</div>}
        </div>
      )}
      <div className={noPad ? "" : `p-4 ${bodyClass}`}>{children}</div>
    </div>
  );
}

/* ── FlushPanel ────────────────────────────────────────────── */

interface FlushPanelProps {
  title?: React.ReactNode;
  right?: React.ReactNode;
  children?: React.ReactNode;
  className?: string;
}

export function FlushPanel({
  title,
  right,
  children,
  className = "",
}: FlushPanelProps) {
  return (
    <div className={`${className}`}>
      {(title || right) && (
        <div className="flex items-center justify-between mb-2">
          {typeof title === "string" ? (
            <h3 className="text-xs font-semibold text-muted uppercase tracking-wider">
              {title}
            </h3>
          ) : (
            title
          )}
          {right && <div className="flex items-center gap-2">{right}</div>}
        </div>
      )}
      {children}
    </div>
  );
}

/* ── StatusBadge ───────────────────────────────────────────── */

interface StatusBadgeProps {
  status: string;
  className?: string;
}

export function StatusBadge({ status, className = "" }: StatusBadgeProps) {
  const s = STATUS[status] || STATUS.planning;
  return (
    <span
      className={`inline-flex items-center gap-1.5 px-2 py-0.5 rounded-full text-xs font-medium ${s.bg} ${s.text} ${className}`}
    >
      <span
        className={`w-1.5 h-1.5 rounded-full ${s.dot} ${s.pulse || ""}`}
      />
      {s.label}
    </span>
  );
}

/* ── Pill ──────────────────────────────────────────────────── */

interface PillProps {
  tone?: "neutral" | "honey" | "blue" | "purple" | "green" | "red" | "mono" | "violet";
  children: React.ReactNode;
  className?: string;
  onClick?: () => void;
}

const pillTone: Record<string, string> = {
  neutral: "bg-ink-600 text-muted",
  honey: "bg-honey-500/10 text-honey-400",
  blue: "bg-blue-500/10 text-blue-400",
  purple: "bg-purple-500/10 text-purple-400",
  green: "bg-green-500/10 text-green-400",
  red: "bg-red-500/10 text-red-400",
  mono: "bg-ink-700 text-slate-300 font-mono",
  violet: "bg-violet-500/10 text-violet-400",
};

export function Pill({
  tone = "neutral",
  children,
  className = "",
  onClick,
}: PillProps) {
  const base = `inline-flex items-center px-2 py-0.5 rounded text-[11px] font-medium leading-tight ${pillTone[tone]} ${className}`;
  if (onClick) {
    return (
      <button className={`${base} cursor-pointer hover:brightness-125 focus:outline-none focus-visible:ring-2 focus-visible:ring-honey-500 focus-visible:ring-offset-2 focus-visible:ring-offset-ink-950`} onClick={onClick}>
        {children}
      </button>
    );
  }
  return <span className={base}>{children}</span>;
}

/* ── Input ─────────────────────────────────────────────────── */

interface InputProps extends React.InputHTMLAttributes<HTMLInputElement> {
  icon?: React.ReactNode;
  suffix?: React.ReactNode;
  wrapClass?: string;
}

export function Input({
  icon,
  suffix,
  wrapClass = "",
  className = "",
  ...rest
}: InputProps) {
  return (
    <div
      className={`flex items-center gap-2 bg-ink-850 border border-line rounded-lg px-3 py-1.5 focus-within:border-honey-500/40 transition-colors ${wrapClass}`}
    >
      {icon && <span className="text-muted shrink-0">{icon}</span>}
      <input
        className={`bg-transparent flex-1 text-sm text-slate-200 placeholder:text-dim focus:outline-none focus-visible:ring-2 focus-visible:ring-honey-500 focus-visible:ring-offset-2 focus-visible:ring-offset-ink-950 ${className}`}
        {...rest}
      />
      {suffix && <span className="text-muted shrink-0">{suffix}</span>}
    </div>
  );
}

/* ── Select ────────────────────────────────────────────────── */

interface SelectProps extends React.SelectHTMLAttributes<HTMLSelectElement> {
  options: { value: string; label: string }[];
  wrapClass?: string;
}

export function Select({
  options,
  wrapClass = "",
  className = "",
  ...rest
}: SelectProps) {
  return (
    <div className={`relative ${wrapClass}`}>
      <select
        className={`w-full appearance-none bg-ink-850 border border-line rounded-lg px-3 py-1.5 pr-8 text-sm text-slate-200 focus:border-honey-500/40 transition-colors focus:outline-none focus-visible:ring-2 focus-visible:ring-honey-500 focus-visible:ring-offset-2 focus-visible:ring-offset-ink-950 cursor-pointer ${className}`}
        {...rest}
      >
        {options.map((o) => (
          <option key={o.value} value={o.value}>
            {o.label}
          </option>
        ))}
      </select>
      <span className="absolute right-2 top-1/2 -translate-y-1/2 text-muted pointer-events-none">
        {I.chevD({ size: 14 })}
      </span>
    </div>
  );
}

/* ── RoundChip ─────────────────────────────────────────────── */

interface RoundChipProps {
  round: number;
  total: number;
  className?: string;
}

export function RoundChip({ round, total, className = "" }: RoundChipProps) {
  return (
    <span
      className={`inline-flex items-center gap-1 px-2 py-0.5 rounded text-[11px] font-mono font-medium bg-honey-500/10 text-honey-400 ${className}`}
    >
      R{round}/{total}
    </span>
  );
}

/* ── Modal ─────────────────────────────────────────────────── */

interface ModalProps {
  open: boolean;
  onClose: () => void;
  title?: string;
  /** Optional subtitle rendered below the title in the modal header. */
  subtitle?: string;
  children: React.ReactNode;
  wide?: boolean;
  /**
   * When `true`, `Modal` installs a capture-phase document-level keydown
   * listener while open that calls `onClose` on Escape. The listener is
   * intentionally owned by `Modal` (not the focus trap) because
   * `focus-trap-react`'s `onDeactivate` also fires on StrictMode unmount,
   * which previously caused the modal to flash open and close in dev.
   * Default is `false` to preserve callers (e.g. QuickTaskDialog) that own
   * their own Escape coordination via a window-level listener — those
   * callers would otherwise see `onClose` fire twice on a single Escape
   * press, breaking multi-step Escape flows like "first close the mention
   * picker, then close the dialog".
   */
  closeOnEscape?: boolean;
}

export function Modal({ open, onClose, title, subtitle, children, wide, closeOnEscape = false }: ModalProps) {
  const titleId = React.useId();

  // Keep the latest onClose accessible from the document listener without
  // re-registering on every parent render (inline `onClose={() => …}` is
  // a common pattern at call sites and would otherwise churn the effect).
  // Use useLayoutEffect so the ref is current before the next paint /
  // user-input cycle — a passive useEffect could leave the ref stale for
  // one frame if the parent rerenders with a new onClose and the user
  // presses Escape before passive effects flush.
  const onCloseRef = React.useRef(onClose);
  React.useLayoutEffect(() => {
    onCloseRef.current = onClose;
  }, [onClose]);

  // Own Escape-to-close here rather than via focus-trap-react's
  // `onDeactivate`. focus-trap-react fires `onDeactivate` on every unmount
  // (including React 18 StrictMode's intentional mount → unmount → mount
  // cycle in dev), which previously caused the modal to flash open and
  // close immediately. See focus-trap-react#738.
  //
  // NOTE: Only one `closeOnEscape` caller exists today (MergedPlanModal).
  // If a future modal also sets `closeOnEscape` and they can stack, both
  // `onClose`s will fire on a single Escape because `stopImmediatePropagation()`
  // only prevents listeners *later in document's keydown listener list*
  // from firing. A stack-aware solution (e.g. module-level stack tracking
  // the top modal) would be needed before adding a second `closeOnEscape`
  // caller.
  React.useEffect(() => {
    if (!open || !closeOnEscape) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        // Match the original focus-trap behaviour: consume the event so
        // it can't also trigger browser defaults (e.g. exit fullscreen).
        // stopImmediatePropagation prevents any listeners later in
        // document's keydown listener list from firing; listeners
        // registered before ours have already executed by the time
        // ours runs.
        e.preventDefault();
        e.stopImmediatePropagation();
        onCloseRef.current();
      }
    };
    // Capture phase: ensures an open modal owns Escape before any
    // bubble-phase target/ancestor listener that might call
    // stopPropagation, and before other document-level handlers
    // registered in bubble phase.
    document.addEventListener("keydown", onKey, true);
    return () => document.removeEventListener("keydown", onKey, true);
  }, [open, closeOnEscape]);

  if (!open) return null;
  return (
    <FocusTrap
      focusTrapOptions={{
        // Backdrop click is handled by the surrounding handler (onClose).
        // Escape-to-close is owned by `Modal` itself via a capture-phase
        // document keydown listener — see the `useEffect` above. We keep
        // `escapeDeactivates: false` here so the trap doesn't consume
        // Escape before our listener sees it.
        escapeDeactivates: false,
        clickOutsideDeactivates: false,
        // Allow clicks outside the trap (e.g. on the backdrop) without the
        // trap aggressively re-focusing first.
        allowOutsideClick: true,
        // Fallback when the modal has no naturally focusable child yet (the
        // trap library raises if it can't find an initial focus target).
        fallbackFocus: "[data-modal-panel]",
        // Returns focus to the trigger that had focus before the trap
        // activated — this is the default but stating it for intent.
        returnFocusOnDeactivate: true,
        // jsdom never resolves computed display, so the default "full"
        // display check rejects every element. Use "none" so tabbable
        // detection works under vitest/jsdom (no functional effect in real
        // browsers — see https://github.com/focus-trap/tabbable#displaycheck).
        tabbableOptions: { displayCheck: "none" },
      }}
    >
      <div data-modal className="fixed inset-0 z-50 flex items-center justify-center">
        <div
          className="absolute inset-0 bg-black/60 backdrop-blur-sm"
          onClick={onClose}
        />
        <div
          data-modal-panel
          tabIndex={-1}
          role="dialog"
          aria-modal="true"
          aria-labelledby={title ? titleId : undefined}
          className={`relative bg-ink-800 border border-line rounded-2xl shadow-2xl max-h-[85vh] overflow-y-auto ${
            wide ? "w-[640px]" : "w-[480px]"
          }`}
        >
          {title && (
            <div className="flex items-center justify-between px-5 py-4 border-b border-line">
              <div className="min-w-0">
                <h2 id={titleId} className="text-base font-semibold text-slate-200">{title}</h2>
                {subtitle && (
                  <p className="text-[11px] text-dim mt-0.5">{subtitle}</p>
                )}
              </div>
              <button
                onClick={onClose}
                aria-label="Close"
                className="text-muted hover:text-slate-200 transition-colors"
              >
                {I.x({ size: 18 })}
              </button>
            </div>
          )}
          <div className="p-5">{children}</div>
        </div>
      </div>
    </FocusTrap>
  );
}

/* ── ConfirmDialog ──────────────────────────────────────────── */

interface ConfirmDialogProps {
  open: boolean;
  title?: string;
  message: string;
  confirmLabel?: string;
  cancelLabel?: string;
  onConfirm: () => void;
  onCancel: () => void;
  danger?: boolean;
  loading?: boolean;
}

export function ConfirmDialog({
  open,
  title = "Confirm",
  message,
  confirmLabel = "Delete",
  cancelLabel = "Cancel",
  onConfirm,
  onCancel,
  danger = true,
  loading = false,
}: ConfirmDialogProps) {
  if (!open) return null;
  return (
    <FocusTrap
      focusTrapOptions={{
        escapeDeactivates: false,
        clickOutsideDeactivates: false,
        allowOutsideClick: true,
        // Default initial focus to the Cancel button so a stray Enter doesn't
        // confirm a destructive action.
        initialFocus: "[data-confirm-cancel]",
        returnFocusOnDeactivate: true,
        // jsdom display-check workaround — see Modal above.
        tabbableOptions: { displayCheck: "none" },
      }}
    >
      <div data-modal className="fixed inset-0 z-50 flex items-center justify-center">
        <div
          className="absolute inset-0 bg-black/60 backdrop-blur-sm"
          onClick={onCancel}
        />
        <div className="relative bg-ink-800 border border-line rounded-2xl shadow-2xl w-[320px] p-5">
          {/* Warning icon */}
          <div className="flex justify-center mb-3">
            <div className="w-10 h-10 rounded-full bg-red-500/10 flex items-center justify-center">
              <svg
                xmlns="http://www.w3.org/2000/svg"
                width="20"
                height="20"
                viewBox="0 0 24 24"
                fill="none"
                stroke="currentColor"
                strokeWidth="2"
                strokeLinecap="round"
                strokeLinejoin="round"
                className="text-red-400"
              >
                <path d="M12 9v4M12 17h.01" />
                <path d="M10.3 3.9 3.3 17a2 2 0 0 0 1.7 3h14a2 2 0 0 0 1.7-3L13.7 3.9a2 2 0 0 0-3.4 0Z" />
              </svg>
            </div>
          </div>
          <h3 className="text-base font-semibold text-slate-200 text-center mb-2">
            {title}
          </h3>
          <p className="text-sm text-muted text-center mb-5 leading-snug">
            {message}
          </p>
          <div className="flex items-center gap-3 justify-end">
            <button
              data-confirm-cancel
              onClick={onCancel}
              disabled={loading}
              className="flex-1 inline-flex items-center justify-center whitespace-nowrap text-xs px-2.5 py-1.5 gap-1.5 rounded-md bg-transparent text-muted hover:text-slate-200 hover:bg-ink-700/60 transition-colors disabled:opacity-40"
            >
              {cancelLabel}
            </button>
            <button
              onClick={onConfirm}
              disabled={loading}
              className={`flex-1 inline-flex items-center justify-center whitespace-nowrap text-xs px-2.5 py-1.5 gap-1.5 rounded-md transition-colors cursor-pointer disabled:opacity-40 disabled:pointer-events-none ${
                danger
                  ? "bg-red-500/10 text-red-400 hover:bg-red-500/20 border border-red-500/20"
                  : "bg-honey-500 text-ink-950 hover:bg-honey-400 active:bg-honey-600 font-semibold shadow-[0_1px_2px_rgba(0,0,0,.4)]"
              }`}
            >
              {loading ? "Deleting…" : confirmLabel}
            </button>
          </div>
        </div>
      </div>
    </FocusTrap>
  );
}

/* ── HivemindReviewCard ────────────────────────────────────── */

interface ReviewVerdict {
  model: string;
  verdict: "pass" | "fail" | "warn";
  summary: string;
}

interface HivemindReviewCardProps {
  round: number;
  verdicts: ReviewVerdict[];
  className?: string;
}

export function HivemindReviewCard({
  round,
  verdicts,
  className = "",
}: HivemindReviewCardProps) {
  const verdictColor: Record<string, string> = {
    pass: "text-green-400",
    fail: "text-red-400",
    warn: "text-honey-400",
  };
  const verdictIcon: Record<string, React.ReactNode> = {
    pass: I.check({ size: 14, className: "text-green-400" }),
    fail: I.x({ size: 14, className: "text-red-400" }),
    warn: I.spark({ size: 14, className: "text-honey-400" }),
  };

  return (
    <div
      className={`rounded-lg border border-line bg-ink-850 p-3 ${className}`}
    >
      <div className="flex items-center gap-2 mb-2">
        {I.hex({ size: 14, className: "text-honey-500" })}
        <span className="text-xs font-semibold text-slate-300">
          Hivemind Round {round}
        </span>
      </div>
      <div className="space-y-1.5">
        {verdicts.map((v, i) => (
          <div key={i} className="flex items-start gap-2">
            <span className="mt-0.5 shrink-0">{verdictIcon[v.verdict]}</span>
            <div className="min-w-0 flex-1">
              <span
                className={`text-xs font-medium ${verdictColor[v.verdict]}`}
              >
                {v.model}
              </span>
              <p className="text-xs text-muted leading-snug">{v.summary}</p>
            </div>
          </div>
        ))}
      </div>
    </div>
  );
}

/* ── ToolCallCard ──────────────────────────────────────────── */

interface ToolCallCardProps {
  name: string;
  output: string;
  done: boolean;
  className?: string;
}

export function ToolCallCard({ name, output, done, className = "" }: ToolCallCardProps) {
  const [expanded, setExpanded] = React.useState(false);
  const contentId = React.useId();
  return (
    <div className={`rounded-md overflow-hidden my-0.5 ${className}`}>
      <button
        onClick={() => setExpanded((v) => !v)}
        aria-expanded={expanded}
        aria-controls={contentId}
        className="w-full px-2.5 py-1.5 flex items-center gap-2 text-[11.5px] hover:bg-white/[0.03] transition-colors rounded-md"
      >
        {done
          ? I.check({ size: 11, className: "text-white/30 shrink-0" })
          : <span className="w-3 h-3 rounded-full border-[1.5px] border-white/20 border-t-white/50 animate-spin shrink-0" />}
        <span className="font-mono text-white/40 text-[11px]">{name}</span>
        <span className="flex-1" />
        <span className={`text-white/20 transition-transform ${expanded ? "rotate-180" : ""}`}>
          {I.chevD({ size: 10 })}
        </span>
      </button>
      {expanded && output && (
        <div id={contentId} className="px-2.5 pb-2">
          <pre className="text-[10.5px] text-white/30 font-mono whitespace-pre-wrap break-words max-h-48 overflow-auto">
            {output}
          </pre>
        </div>
      )}
    </div>
  );
}

/* ── ToolCallGroup (collapsed sequential same-type) ─────────── */

interface ToolCallGroupProps {
  name: string;
  calls: { tool_call_id: string; output: string; done: boolean }[];
  className?: string;
}

export function ToolCallGroup({ name, calls, className = "" }: ToolCallGroupProps) {
  const [expanded, setExpanded] = React.useState(false);
  const contentId = React.useId();
  const count = calls.length;
  const anyRunning = calls.some((c) => !c.done);

  if (count === 1) {
    return <ToolCallCard name={name} output={calls[0].output} done={calls[0].done} className={className} />;
  }

  return (
    <div className={`my-0.5 ${className}`}>
      <button
        onClick={() => setExpanded((v) => !v)}
        aria-expanded={expanded}
        aria-controls={contentId}
        className="w-full px-2.5 py-1.5 flex items-center gap-2 text-[11.5px] hover:bg-white/[0.03] transition-colors rounded-md"
      >
        {anyRunning
          ? <span className="w-3 h-3 rounded-full border-[1.5px] border-white/20 border-t-white/50 animate-spin shrink-0" />
          : I.check({ size: 11, className: "text-white/30 shrink-0" })}
        <span className="font-mono text-white/40 text-[11px]">{name}</span>
        <span className="text-white/20 text-[10.5px] font-mono">×{count}</span>
        <span className="flex-1" />
        <span className={`text-white/20 transition-transform ${expanded ? "rotate-180" : ""}`}>
          {I.chevD({ size: 10 })}
        </span>
      </button>
      {expanded && (
        <div id={contentId} className="pl-3">
          {calls.map((c, i) => (
            <ToolCallCard
              key={c.tool_call_id}
              name={`${name} ${i + 1}`}
              output={c.output}
              done={c.done}
            />
          ))}
        </div>
      )}
    </div>
  );
}
/* ── RelativeTime ─────────────────────────────── */

/** Live-updating relative-time label.
 *
 *  Ticks every 1s while the bubble is <2 minutes old, then every 30s.
 *  Falls back to the optional pre-formatted `fallback` string (e.g. the
 *  legacy `"14:35:22"` wall-clock or `"now"` token) when no `createdAt`
 *  is provided — used for replayed historical messages and demo data
 *  that predate the timestamped pipeline.
 */
export function RelativeTime({
  createdAt,
  fallback,
  className,
}: {
  createdAt?: number;
  fallback?: string;
  className?: string;
}) {
  const [now, setNow] = React.useState(() => Date.now());
  React.useEffect(() => {
    if (typeof createdAt !== "number") return;
    let cancelled = false;
    let timeoutId: number | undefined;
    const schedule = () => {
      if (cancelled) return;
      const ageMs = Date.now() - createdAt;
      const delay = ageMs < 120_000 ? 1000 : 30_000;
      timeoutId = window.setTimeout(() => {
        if (cancelled) return;
        setNow(Date.now());
        schedule();
      }, delay);
    };
    schedule();
    return () => {
      cancelled = true;
      if (timeoutId !== undefined) window.clearTimeout(timeoutId);
    };
  }, [createdAt]);
  if (typeof createdAt !== "number") {
    return fallback ? <span className={className}>{fallback}</span> : null;
  }
  return <span className={className}>{relativeTimeDetailed(createdAt, now)}</span>;
}

/** Collapsible reasoning/thinking block — violet color scheme. */
export function ReasoningBlock({
  reasoning,
  streaming,
  keepExpanded = false,
  durationMs,
  createdAt,
}: {
  reasoning: string;
  streaming: boolean;
  keepExpanded?: boolean;
  durationMs?: number;
  createdAt?: number;
}) {
  const [expanded, setExpanded] = React.useState(streaming || keepExpanded);
  const contentId = React.useId();
  const contentRef = React.useRef<HTMLDivElement>(null);

  // Auto-scroll while streaming
  React.useEffect(() => {
    if (streaming && expanded && contentRef.current) {
      contentRef.current.scrollTop = contentRef.current.scrollHeight;
    }
  }, [reasoning, streaming, expanded]);

  // Auto-collapse when streaming finishes (but NOT when keepExpanded is true)
  React.useEffect(() => {
    if (!streaming && !keepExpanded && expanded) {
      setExpanded(false);
    }
  }, [streaming, keepExpanded]);

  // Collapse when keepExpanded transitions from true → false (new user msg, etc.)
  React.useEffect(() => {
    if (!keepExpanded && expanded) {
      setExpanded(false);
    }
  }, [keepExpanded]);

  const lines = reasoning.split("\n").length;

  const fmtDuration = (ms: number) => {
    if (ms < 1000) return "<1s";
    const s = Math.round(ms / 1000);
    if (s < 60) return `${s}s`;
    const m = Math.floor(s / 60);
    const rem = s % 60;
    return rem > 0 ? `${m}m ${rem}s` : `${m}m`;
  };

  return (
    <div className="rounded-lg border border-violet-500/25 bg-violet-500/5 overflow-hidden my-1.5">
      <button
        onClick={() => setExpanded((v) => !v)}
        aria-expanded={expanded}
        aria-controls={contentId}
        className="w-full px-3 py-2 flex items-center gap-2 text-[12px] hover:bg-violet-500/10 transition-colors"
      >
        {streaming
          ? <span className="w-3 h-3 rounded-full border-2 border-violet-400 border-t-transparent animate-spin shrink-0" />
          : I.brain({ size: 12, className: "text-violet-400 shrink-0" })}
        <span className="font-mono text-violet-300 font-medium">reasoning</span>
        {!streaming && (
          <span className="text-[10.5px] text-violet-400/60 font-mono">{lines} {lines === 1 ? "line" : "lines"}</span>
        )}
        {!streaming && durationMs != null && (
          <span className="text-[10.5px] text-violet-400/60 font-mono">· {fmtDuration(durationMs)}</span>
        )}
        {createdAt != null && (
          <span className="text-[10.5px] text-violet-400/60 font-mono">· <RelativeTime createdAt={createdAt} /></span>
        )}
        {streaming && (
          <span className="text-[10.5px] text-violet-400/60">thinking…</span>
        )}
        <span className="flex-1" />
        <span className="text-dim text-[10.5px]">
          {expanded ? "collapse" : "expand"}
        </span>
        <span className={`text-dim transition-transform ${expanded ? "rotate-180" : ""}`}>
          {I.chevD({ size: 11 })}
        </span>
      </button>
      {expanded && (
        <div id={contentId} className="px-3 pb-2 border-t border-violet-500/15">
          <div ref={contentRef} className="text-[12px] text-violet-200/80 font-mono whitespace-pre-wrap break-words leading-relaxed mt-1.5 max-h-64 overflow-auto">
            {reasoning}
            {streaming && <span className="inline-block w-1.5 h-3.5 bg-violet-400 ml-0.5 animate-pulse" />}
          </div>
        </div>
      )}
    </div>
  );
}

/** Compact pulsing brain indicator shown when reasoning is hidden but active. */
export function InlineReasoningIndicator({ streaming = false }: { streaming?: boolean }) {
  return (
    <div className="flex items-center gap-1.5 my-1.5 px-1">
      <span className={streaming ? "pulse-brain" : ""}>
        {I.brain({ size: 14, className: "text-violet-400" })}
      </span>
      <span className="text-[11px] text-violet-400/60 font-mono">
        {streaming ? "thinking..." : "reasoning"}
      </span>
    </div>
  );
}

/* ── Kbd ───────────────────────────────────────────────────── */

interface KbdProps {
  children: React.ReactNode;
  className?: string;
}

export function Kbd({ children, className = "" }: KbdProps) {
  return (
    <kbd
      className={`inline-flex items-center justify-center min-w-[20px] h-5 px-1.5 rounded bg-ink-700 border border-line text-[10px] font-mono text-muted ${className}`}
    >
      {children}
    </kbd>
  );
}
