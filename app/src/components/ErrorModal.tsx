import React, { createContext, useContext, useState, useCallback, useEffect, useRef } from "react";
import * as Sentry from "@sentry/react";
import { FocusTrap } from "focus-trap-react";
import { I } from "./icons";

interface ErrorModalContextValue {
  showError: (title: string, detail?: string) => void;
}

const ErrorModalContext = createContext<ErrorModalContextValue>({
  showError: () => {},
});

export function useErrorModal() {
  return useContext(ErrorModalContext);
}

interface ErrorEntry {
  title: string;
  detail?: string;
}

const FILTER_PATTERNS: RegExp[] = [
  /^Warning:/,
  /React does not recognize/,
  /ResizeObserver loop/,
  /\[HMR\]/,
  /\[vite\]/,
  /Failed to send.*to Rust/,
  /plugin sentry not found/,
  // Third-party inline-script noise (WebKit-only phrasing, no Hyvemind frames):
  /null is not an object \(evaluating 'el\.dispatchEvent'\)/,
];

const HYVEMIND_FRAME_RE =
  /(\/src\/|\/assets\/|\.tsx[:?]|\.ts[:?]|\/@vite\/|\/@react-refresh|\/node_modules\/)/;
const BARE_DOCUMENT_FRAME_RE = /https?:\/\/[^/\s]+\/:\d+:\d+/;

function isExternalStack(detail: string | undefined): boolean {
  if (!detail) return false;
  if (HYVEMIND_FRAME_RE.test(detail)) return false;
  return BARE_DOCUMENT_FRAME_RE.test(detail);
}

function shouldSuppress(title: string, detail?: string): boolean {
  const haystack = `${title}\n${detail ?? ""}`;
  if (FILTER_PATTERNS.some((p) => p.test(haystack))) return true;
  if (isExternalStack(detail)) return true;
  return false;
}

export function ErrorModalProvider({
  children,
  onFix,
}: {
  children: React.ReactNode;
  onFix?: (prompt: string) => void;
}) {
  const [error, setError] = useState<ErrorEntry | null>(null);
  const [copied, setCopied] = useState(false);

  const lastErrorRef = useRef<{ msg: string; time: number }>({ msg: "", time: 0 });

  const showError = useCallback((title: string, detail?: string) => {
    const key = title + (detail || "");
    const now = Date.now();
    if (lastErrorRef.current.msg === key && now - lastErrorRef.current.time < 2000) return;
    lastErrorRef.current = { msg: key, time: now };
    setError({ title, detail });
    setCopied(false);
  }, []);

  // Global error interception: console.error, uncaught exceptions, unhandled rejections
  useEffect(() => {
    const originalConsoleError = console.error;

    const extractMessage = (args: unknown[]): { title: string; detail?: string } | null => {
      const parts = args.map((a) =>
        a instanceof Error ? a.message : typeof a === "string" ? a : String(a)
      );
      const joined = parts.join(" ").trim();
      if (!joined) return null;
      // First string arg is title, Error objects become detail
      const errObj = args.find((a) => a instanceof Error) as Error | undefined;
      const title = typeof args[0] === "string" ? args[0] : joined.slice(0, 120);
      const detail = errObj
        ? errObj.stack || errObj.message
        : parts.length > 1
          ? parts.slice(1).join("\n")
          : undefined;
      return { title, detail };
    };

    console.error = (...args: unknown[]) => {
      originalConsoleError.apply(console, args);
      const parsed = extractMessage(args);
      if (!parsed) return;
      const suppressed = shouldSuppress(parsed.title, parsed.detail);
      if (!suppressed) showError(parsed.title, parsed.detail);
      const errObj = args.find((a) => a instanceof Error) as Error | undefined;
      Sentry.captureException(errObj ?? new Error(parsed.title), {
        tags: {
          source: "console.error",
          suppressed: suppressed ? "filtered" : "false",
        },
      });
    };

    const handleError = (event: ErrorEvent) => {
      const title = event.message || "Uncaught error";
      const detail = event.error?.stack || `${event.filename}:${event.lineno}`;
      const suppressed = shouldSuppress(title, detail);
      if (!suppressed) showError(title, detail);
      Sentry.captureException(event.error ?? new Error(title), {
        tags: {
          source: "window.error",
          suppressed: suppressed ? "filtered" : "false",
        },
      });
    };

    const handleRejection = (event: PromiseRejectionEvent) => {
      const reason = event.reason;
      const rawTitle = reason instanceof Error ? reason.message : String(reason);
      const title = "Unhandled promise rejection: " + rawTitle;
      const detail = reason instanceof Error ? reason.stack : undefined;
      const suppressed = shouldSuppress(title, detail);
      if (!suppressed) showError(title, detail);
      Sentry.captureException(reason instanceof Error ? reason : new Error(rawTitle), {
        tags: {
          source: "unhandledrejection",
          suppressed: suppressed ? "filtered" : "false",
        },
      });
    };

    window.addEventListener("error", handleError);
    window.addEventListener("unhandledrejection", handleRejection);

    return () => {
      console.error = originalConsoleError;
      window.removeEventListener("error", handleError);
      window.removeEventListener("unhandledrejection", handleRejection);
    };
  }, [showError]);

  const close = useCallback(() => {
    setError(null);
    setCopied(false);
  }, []);

  // Esc closes the error modal. We attach a window listener (not via the
  // FocusTrap library) so a) focus may legitimately have moved off the modal
  // and b) the trap library's own escapeDeactivates is disabled (we drive
  // close ourselves so focus-return runs).
  useEffect(() => {
    if (!error) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.stopPropagation();
        close();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [error, close]);

  const handleCopy = useCallback(() => {
    if (!error) return;
    const text = error.detail ? `${error.title}\n\n${error.detail}` : error.title;
    navigator.clipboard.writeText(text).then(() => {
      setCopied(true);
      setTimeout(() => setCopied(false), 2000);
    });
  }, [error]);

  const handleFix = useCallback(() => {
    if (!error || !onFix) return;
    const prompt = [
      `Fix this Hyvemind Bug: ${error.title}`,
      "",
      error.detail ? `Error details:\n\`\`\`\n${error.detail}\n\`\`\`` : null,
      "",
      `Source: ErrorModal catch handler`,
      `Timestamp: ${new Date().toISOString()}`,
      "",
      "Diagnose the root cause and implement a fix.",
    ]
      .filter((l) => l !== null)
      .join("\n");
    close();
    onFix(prompt);
  }, [error, onFix, close]);

  return (
    <ErrorModalContext.Provider value={{ showError }}>
      {children}
      {error && (
        <FocusTrap
          focusTrapOptions={{
            // Backdrop click + Dismiss button drive close, not the trap.
            escapeDeactivates: false,
            clickOutsideDeactivates: false,
            allowOutsideClick: true,
            // Default focus to Dismiss so Enter doesn't accidentally trigger
            // the Fix action when present (which is a destructive task creation).
            initialFocus: "[data-error-dismiss]",
            returnFocusOnDeactivate: true,
            // jsdom display-check workaround; see Modal in atoms.tsx.
            tabbableOptions: { displayCheck: "none" },
          }}
        >
          <div data-modal className="fixed inset-0 z-[100] flex items-center justify-center">
            <div
              className="absolute inset-0 bg-black/60 backdrop-blur-sm"
              onClick={close}
            />
            <div
              className="relative w-[440px] max-w-[90vw] bg-ink-800 border border-red-500/30 rounded-2xl shadow-2xl overflow-hidden"
              role="alert"
              aria-live="assertive"
              aria-atomic="true"
            >
              <div className="flex items-center gap-3 px-5 py-4 border-b border-line">
                <div className="w-8 h-8 rounded-full bg-red-500/15 border border-red-500/30 flex items-center justify-center shrink-0">
                  {I.x({ size: 16, className: "text-red-400" })}
                </div>
                <h2 className="text-[14px] font-semibold text-white flex-1 min-w-0">
                  {error.title}
                </h2>
                <button
                  onClick={close}
                  aria-label="Close error"
                  className="text-muted hover:text-white transition-colors shrink-0"
                >
                  {I.x({ size: 16 })}
                </button>
              </div>
              {error.detail && (
                <div className="px-5 py-4">
                  <pre className="text-[12px] text-red-300/80 font-mono whitespace-pre-wrap break-words max-h-[40vh] overflow-auto bg-ink-900 rounded-lg p-3 border border-line">
                    {error.detail}
                  </pre>
                </div>
              )}
              <div className="px-5 pb-4 flex items-center gap-2">
                {onFix && (
                  <button
                    onClick={handleFix}
                    className="px-3.5 py-1.5 rounded-lg text-sm font-medium bg-honey-500 text-ink-900 hover:bg-honey-400 transition-colors flex items-center gap-1.5"
                  >
                    {I.spark({ size: 13 })}
                    Fix
                  </button>
                )}
                <div className="flex-1" />
                <button
                  onClick={handleCopy}
                  className="px-3.5 py-1.5 rounded-lg text-sm font-medium bg-ink-600 text-slate-200 hover:bg-ink-500 border border-line transition-colors flex items-center gap-1.5"
                >
                  {I.copy({ size: 13 })}
                  {copied ? "Copied" : "Copy"}
                </button>
                <button
                  data-error-dismiss
                  onClick={close}
                  className="px-3.5 py-1.5 rounded-lg text-sm font-medium bg-ink-600 text-slate-200 hover:bg-ink-500 border border-line transition-colors"
                >
                  Dismiss
                </button>
              </div>
            </div>
          </div>
        </FocusTrap>
      )}
    </ErrorModalContext.Provider>
  );
}
