import React, { createContext, useCallback, useContext, useEffect, useMemo, useRef, useState } from "react";
import { I } from "./icons";

/* ── Background-failure toast surface ────────────────────────────────────────
 *
 * Why this exists: the global `ErrorModalProvider` intercepts every
 * `console.error` and pops a modal. That UX is correct for *unexpected* errors
 * (bug-quality stack traces) — but it's far too heavy for **routine background
 * IPC failures** (Dashboard poll missed a tick, `setDefaultModel` failed to
 * persist, etc.) where the user is mid-task and shouldn't have a modal slam
 * over the screen.
 *
 * `useErrorToast()` is the lighter alternative for those background sites.
 * It:
 *   - displays a non-modal, stacked, dismissible toast in the bottom-right
 *   - auto-dismisses after `durationMs` (default 6s)
 *   - keeps a `console.warn(...)` trail so the failure stays visible in
 *     devtools (we intentionally **don't** use `console.error` here — that
 *     would re-route into the ErrorModal interception and produce both a
 *     toast and a modal for the same event)
 *
 * Call sites: `.catch((e) => toast.error("Failed to X", e))`.
 */

export type ToastKind = "error" | "warning" | "info" | "success";

interface ToastEntry {
  id: number;
  kind: ToastKind;
  title: string;
  detail?: string;
}

interface ToastContextValue {
  push: (kind: ToastKind, title: string, detail?: string, durationMs?: number) => void;
  error: (title: string, detail?: unknown, durationMs?: number) => void;
  warning: (title: string, detail?: unknown, durationMs?: number) => void;
  info: (title: string, detail?: unknown, durationMs?: number) => void;
  success: (title: string, detail?: unknown, durationMs?: number) => void;
}

const noop = () => {};
const ToastContext = createContext<ToastContextValue>({
  push: noop,
  error: noop,
  warning: noop,
  info: noop,
  success: noop,
});

export function useErrorToast(): ToastContextValue {
  return useContext(ToastContext);
}

function describeDetail(detail: unknown): string | undefined {
  if (detail === undefined || detail === null) return undefined;
  if (detail instanceof Error) return detail.message || String(detail);
  if (typeof detail === "string") return detail;
  try {
    return JSON.stringify(detail);
  } catch {
    return String(detail);
  }
}

const DEFAULT_DURATION_MS = 6000;

export function ToastProvider({ children }: { children: React.ReactNode }) {
  const [toasts, setToasts] = useState<ToastEntry[]>([]);
  const idRef = useRef(0);
  const timeoutsRef = useRef<Map<number, ReturnType<typeof setTimeout>>>(new Map());

  const dismiss = useCallback((id: number) => {
    setToasts((prev) => prev.filter((t) => t.id !== id));
    const timer = timeoutsRef.current.get(id);
    if (timer) {
      clearTimeout(timer);
      timeoutsRef.current.delete(id);
    }
  }, []);

  const push = useCallback(
    (kind: ToastKind, title: string, detail?: string, durationMs: number = DEFAULT_DURATION_MS) => {
      idRef.current += 1;
      const id = idRef.current;
      const entry: ToastEntry = { id, kind, title, detail };
      setToasts((prev) => [...prev, entry].slice(-5)); // cap at 5 stacked
      if (durationMs > 0) {
        const timer = setTimeout(() => dismiss(id), durationMs);
        timeoutsRef.current.set(id, timer);
      }
    },
    [dismiss],
  );

  // Cleanup any pending timers on unmount.
  useEffect(() => {
    const timers = timeoutsRef.current;
    return () => {
      for (const t of timers.values()) clearTimeout(t);
      timers.clear();
    };
  }, []);

  // Intentionally `console.warn` (not `console.error`) inside the helpers so
  // we don't double-fire with the ErrorModal interceptor. The toast itself is
  // the user-facing surface; warn keeps the trace visible in devtools.
  const error = useCallback<ToastContextValue["error"]>(
    (title, detail, durationMs) => {
      const d = describeDetail(detail);
      console.warn(`[toast.error] ${title}`, detail);
      push("error", title, d, durationMs);
    },
    [push],
  );
  const warning = useCallback<ToastContextValue["warning"]>(
    (title, detail, durationMs) => {
      const d = describeDetail(detail);
      console.warn(`[toast.warning] ${title}`, detail);
      push("warning", title, d, durationMs);
    },
    [push],
  );
  const info = useCallback<ToastContextValue["info"]>(
    (title, detail, durationMs) => {
      const d = describeDetail(detail);
      push("info", title, d, durationMs);
    },
    [push],
  );
  const success = useCallback<ToastContextValue["success"]>(
    (title, detail, durationMs) => {
      const d = describeDetail(detail);
      push("success", title, d, durationMs);
    },
    [push],
  );

  const value = useMemo<ToastContextValue>(
    () => ({ push, error, warning, info, success }),
    [push, error, warning, info, success],
  );

  return (
    <ToastContext.Provider value={value}>
      {children}
      <ToastViewport toasts={toasts} onDismiss={dismiss} />
    </ToastContext.Provider>
  );
}

const KIND_STYLES: Record<ToastKind, { ring: string; iconBg: string; iconColor: string; title: string }> = {
  error: {
    ring: "border-red-500/30",
    iconBg: "bg-red-500/15 border-red-500/30",
    iconColor: "text-red-400",
    title: "text-white",
  },
  warning: {
    ring: "border-honey-500/30",
    iconBg: "bg-honey-500/15 border-honey-500/30",
    iconColor: "text-honey-400",
    title: "text-white",
  },
  info: {
    ring: "border-blue-500/30",
    iconBg: "bg-blue-500/15 border-blue-500/30",
    iconColor: "text-blue-400",
    title: "text-white",
  },
  success: {
    ring: "border-green-500/30",
    iconBg: "bg-green-500/15 border-green-500/30",
    iconColor: "text-green-400",
    title: "text-white",
  },
};

function ToastViewport({
  toasts,
  onDismiss,
}: {
  toasts: ToastEntry[];
  onDismiss: (id: number) => void;
}) {
  if (toasts.length === 0) return null;
  return (
    <div
      data-toast-viewport
      className="fixed bottom-4 right-4 z-[90] flex flex-col gap-2 max-w-[380px] pointer-events-none"
      role="region"
      aria-label="Notifications"
    >
      {toasts.map((t) => {
        const s = KIND_STYLES[t.kind];
        return (
          <div
            key={t.id}
            className={`pointer-events-auto bg-ink-800 border ${s.ring} rounded-xl shadow-2xl overflow-hidden`}
            role="status"
            aria-live="polite"
          >
            <div className="flex items-start gap-3 px-3.5 py-3">
              <div
                className={`mt-0.5 w-6 h-6 rounded-full border flex items-center justify-center shrink-0 ${s.iconBg}`}
              >
                {I.x({ size: 12, className: s.iconColor })}
              </div>
              <div className="flex-1 min-w-0">
                <div className={`text-[12.5px] font-semibold ${s.title} break-words`}>{t.title}</div>
                {t.detail && (
                  <div className="text-[11.5px] text-muted mt-0.5 break-words font-mono whitespace-pre-wrap max-h-32 overflow-auto">
                    {t.detail}
                  </div>
                )}
              </div>
              <button
                onClick={() => onDismiss(t.id)}
                className="shrink-0 text-muted hover:text-white transition-colors"
                aria-label="Dismiss notification"
              >
                {I.x({ size: 14 })}
              </button>
            </div>
          </div>
        );
      })}
    </div>
  );
}
