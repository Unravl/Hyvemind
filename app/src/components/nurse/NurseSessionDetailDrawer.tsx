import { useEffect, useState, useRef } from "react";
import { FocusTrap } from "focus-trap-react";
import * as ipc from "../../lib/ipc";
import { isTauri } from "../../lib/tauri";
import { formatIpcError } from "../../lib/ipc";
import { Btn, ConfirmDialog, Input } from "../atoms";
import { I } from "../icons";
import { SignalChip } from "./SignalChip";
import type {
  SessionDetailSnapshot,
  NurseManualAction,
} from "../../lib/nurseTypes";

interface Props {
  sessionId: string | null;
  onClose: () => void;
}

/**
 * Right-slide detail drawer for one monitored session. Renders the
 * full SessionHealth, transcript tail, escalation timeline, and
 * manual action buttons. Steer / Cancel / Force-restart each open a
 * focus-trapped confirmation dialog before firing
 * `nurse_manual_action`.
 */
export function NurseSessionDetailDrawer({ sessionId, onClose }: Props) {
  const [detail, setDetail] = useState<SessionDetailSnapshot | null>(null);
  const [isLoading, setIsLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // Confirmation modal state for manual actions.
  const [confirm, setConfirm] = useState<{
    kind: "steer" | "cancel" | "force_restart";
    message: string;
  } | null>(null);
  const [actionBusy, setActionBusy] = useState(false);
  const [actionError, setActionError] = useState<string | null>(null);

  useEffect(() => {
    if (!sessionId) {
      setDetail(null);
      return;
    }
    let cancelled = false;
    const load = async () => {
      if (!isTauri()) {
        setIsLoading(false);
        return;
      }
      setIsLoading(true);
      setError(null);
      try {
        const d = await ipc.getNurseSessionDetail(sessionId);
        if (!cancelled) setDetail(d);
      } catch (err) {
        if (!cancelled) {
          setError(formatIpcError(err));
          setDetail(null);
        }
      } finally {
        if (!cancelled) setIsLoading(false);
      }
    };
    load();
    return () => {
      cancelled = true;
    };
  }, [sessionId]);

  // Own Escape-to-close here rather than via focus-trap-react's onDeactivate.
  // focus-trap-react fires onDeactivate on every unmount (including React 18
  // StrictMode's intentional double-mount in dev), which causes the drawer
  // to flash open and close immediately.
  const onCloseRef = useRef(onClose);
  useEffect(() => {
    onCloseRef.current = onClose;
  }, [onClose]);

  useEffect(() => {
    if (!sessionId) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.preventDefault();
        e.stopPropagation();
        onCloseRef.current();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [sessionId]);

  if (!sessionId) return null;

  const runManualAction = async () => {
    if (!confirm || !sessionId) return;
    setActionBusy(true);
    setActionError(null);
    try {
      let action: NurseManualAction;
      switch (confirm.kind) {
        case "steer":
          action = { kind: "steer", message: confirm.message };
          break;
        case "cancel":
          action = {
            kind: "cancel",
            message: confirm.message || undefined,
          };
          break;
        case "force_restart":
          action = { kind: "force_restart" };
          break;
      }
      await ipc.nurseManualAction(sessionId, action);
      setConfirm(null);
    } catch (err) {
      setActionError(formatIpcError(err));
    } finally {
      setActionBusy(false);
    }
  };

  return (
    <FocusTrap
      focusTrapOptions={{
        escapeDeactivates: false,
        clickOutsideDeactivates: false,
        allowOutsideClick: true,
        fallbackFocus: "[data-drawer-panel]",
        returnFocusOnDeactivate: true,
        tabbableOptions: { displayCheck: "none" },
      }}
    >
      <div
        className="fixed inset-0 z-40"
        role="dialog"
        aria-modal="true"
        aria-label="Session detail"
      >
        {/* Backdrop */}
        <div
          className="absolute inset-0 bg-black/40"
          onClick={onClose}
        />
        {/* Slide panel */}
        <aside
          data-drawer-panel
          tabIndex={-1}
          className="absolute right-0 top-0 bottom-0 w-[480px] max-w-full bg-ink-900 border-l border-line shadow-2xl flex flex-col"
        >
          <header className="flex items-center justify-between px-4 py-3 border-b border-line">
            <div className="min-w-0">
              <div className="text-[11px] text-dim uppercase tracking-wider">
                Session
              </div>
              <div
                className="text-sm text-white font-mono truncate"
                title={sessionId}
              >
                {sessionId}
              </div>
            </div>
            <button
              onClick={onClose}
              aria-label="Close drawer"
              className="text-muted hover:text-white"
            >
              {I.x({ size: 18 })}
            </button>
          </header>

          <div className="flex-1 overflow-y-auto px-4 py-4 space-y-4">
            {isLoading && (
              <div className="text-[12px] text-muted">Loading…</div>
            )}
            {error && (
              <div className="text-[12px] text-red-300">
                Failed to load session detail: {error}
              </div>
            )}
            {!isLoading && !error && !detail && (
              <div className="text-[12px] text-muted">
                No detail available. The backend may not yet implement
                `get_nurse_session_detail`.
              </div>
            )}

            {detail && (
              <>
                {/* Active signals */}
                <Section title="Active signals">
                  {(detail.session.active_signals ?? []).length === 0 ? (
                    <div className="text-[11px] text-dim">
                      None right now.
                    </div>
                  ) : (
                    <div className="flex flex-wrap gap-1">
                      {(detail.session.active_signals ?? []).map((s) => (
                        <SignalChip key={s.dedup_key} signal={s} />
                      ))}
                    </div>
                  )}
                </Section>

                {/* Decisions */}
                <Section title="Decisions">
                  {detail.decisions.length === 0 ? (
                    <div className="text-[11px] text-dim">
                      No decisions recorded.
                    </div>
                  ) : (
                    <ul className="space-y-1.5">
                      {detail.decisions.map((d) => (
                        <li
                          key={d.decision_id}
                          className="rounded border border-line bg-ink-850 px-2.5 py-1.5"
                        >
                          <div className="flex items-center gap-2 text-[11px]">
                            <span className="text-muted font-mono">
                              {new Date(d.started_at).toLocaleTimeString()}
                            </span>
                            <span className="text-honey-300 font-medium">
                              {d.tier_used}
                            </span>
                            {d.action && (
                              <span className="text-white/80">
                                → {d.action}
                              </span>
                            )}
                            <span className="ml-auto text-dim">
                              {d.status}
                            </span>
                          </div>
                        </li>
                      ))}
                    </ul>
                  )}
                </Section>

                {/* Detector last-tick map */}
                {Object.keys(detail.detector_last_tick).length > 0 && (
                  <Section title="Detector activity">
                    <ul className="text-[11px] text-muted space-y-0.5">
                      {Object.entries(detail.detector_last_tick).map(
                        ([name, ts]) => (
                          <li
                            key={name}
                            className="flex items-center justify-between"
                          >
                            <span className="font-mono">{name}</span>
                            <span className="text-dim">
                              {new Date(ts).toLocaleTimeString()}
                            </span>
                          </li>
                        ),
                      )}
                    </ul>
                  </Section>
                )}

                {/* Transcript tail */}
                <Section title="Recent transcript">
                  {detail.transcript_tail.length === 0 ? (
                    <div className="text-[11px] text-dim">
                      No transcript captured.
                    </div>
                  ) : (
                    <pre className="text-[10.5px] text-slate-300 bg-ink-950 border border-line rounded p-2 whitespace-pre-wrap break-words max-h-64 overflow-y-auto font-mono">
                      {detail.transcript_tail
                        .map(
                          (e) =>
                            `${new Date(e.timestamp).toLocaleTimeString()} [${e.kind}] ${e.text ?? e.tool_name ?? ""}`,
                        )
                        .join("\n")}
                    </pre>
                  )}
                </Section>
              </>
            )}
          </div>

          {/* Manual actions */}
          <footer className="border-t border-line px-4 py-3 space-y-2">
            <div className="text-[10px] text-dim uppercase tracking-wider">
              Manual actions
            </div>
            <div className="flex flex-wrap gap-2">
              <Btn
                size="sm"
                kind="outline"
                onClick={() =>
                  setConfirm({ kind: "steer", message: "" })
                }
              >
                Steer now
              </Btn>
              <Btn
                size="sm"
                kind="outline"
                onClick={() =>
                  setConfirm({ kind: "cancel", message: "" })
                }
              >
                Cancel now
              </Btn>
              <Btn
                size="sm"
                kind="danger"
                onClick={() =>
                  setConfirm({ kind: "force_restart", message: "" })
                }
              >
                Force restart
              </Btn>
            </div>
            {actionError && (
              <div className="text-[11px] text-red-300">{actionError}</div>
            )}
          </footer>
        </aside>

        {/* Confirmation modal */}
        {confirm && confirm.kind === "force_restart" && (
          <ConfirmDialog
            open
            title="Force restart Pi session?"
            message="The session will be killed and respawned with a fresh transcript. Any in-flight work is lost."
            confirmLabel="Force restart"
            onConfirm={runManualAction}
            onCancel={() => setConfirm(null)}
            danger
            loading={actionBusy}
          />
        )}
        {confirm &&
          (confirm.kind === "steer" || confirm.kind === "cancel") && (
            <FocusTrap
              focusTrapOptions={{
                escapeDeactivates: false,
                clickOutsideDeactivates: false,
                allowOutsideClick: true,
                fallbackFocus: "[data-confirm-panel]",
                tabbableOptions: { displayCheck: "none" },
              }}
            >
              <div className="fixed inset-0 z-50 flex items-center justify-center">
                <div
                  className="absolute inset-0 bg-black/60"
                  onClick={() => setConfirm(null)}
                />
                <div
                  data-confirm-panel
                  className="relative bg-ink-800 border border-line rounded-2xl w-[420px] p-5 shadow-2xl"
                >
                  <h3 className="text-base font-semibold text-white mb-2">
                    {confirm.kind === "steer"
                      ? "Steer this session"
                      : "Cancel this session"}
                  </h3>
                  <p className="text-[12px] text-muted mb-3">
                    {confirm.kind === "steer"
                      ? "Send a steer message to the running session."
                      : "Send an optional explanation when cancelling."}
                  </p>
                  <Input
                    autoFocus
                    placeholder={
                      confirm.kind === "steer"
                        ? "What should the agent do next?"
                        : "Why are you cancelling? (optional)"
                    }
                    value={confirm.message}
                    onChange={(e) =>
                      setConfirm({ ...confirm, message: e.target.value })
                    }
                  />
                  {actionError && (
                    <div className="mt-2 text-[11px] text-red-300">
                      {actionError}
                    </div>
                  )}
                  <div className="mt-4 flex items-center gap-2 justify-end">
                    <Btn
                      size="sm"
                      kind="ghost"
                      onClick={() => setConfirm(null)}
                      disabled={actionBusy}
                    >
                      Cancel
                    </Btn>
                    <Btn
                      size="sm"
                      kind={confirm.kind === "cancel" ? "danger" : "primary"}
                      onClick={runManualAction}
                      loading={actionBusy}
                      disabled={
                        confirm.kind === "steer" && !confirm.message.trim()
                      }
                    >
                      {confirm.kind === "steer" ? "Steer" : "Cancel session"}
                    </Btn>
                  </div>
                </div>
              </div>
            </FocusTrap>
          )}
      </div>
    </FocusTrap>
  );
}

function Section({
  title,
  children,
}: {
  title: string;
  children: React.ReactNode;
}) {
  return (
    <section>
      <div className="text-[10px] text-dim uppercase tracking-wider mb-1.5">
        {title}
      </div>
      {children}
    </section>
  );
}
