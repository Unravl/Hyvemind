import React, { useCallback, useEffect, useRef, useState } from "react";
import { I } from "./icons";
import { useErrorToast } from "./Toast";
import { useTaskRuntimeState } from "../lib/taskRuntime";
import {
  NURSE_SCENARIOS,
  findNurseScenario,
  type NurseScenario,
} from "../lib/nurse-scenarios";
import { killPiSession, sigkillPiSession, formatIpcError } from "../lib/ipc";

interface NurseTestDropdownProps {
  activeId: string;
  streaming: boolean;
  projectPath: string | null;
  /** Setter that mutates the composer's local `value` state so the textarea
   *  re-renders. Composer state is local (Composer.tsx owns it); we need
   *  this callback to push the bait prompt in. */
  setComposerValue: (v: string) => void;
}

interface PendingTrigger {
  scenarioId: string;
  taskId: string;
  /** Wall-ms when the scenario was selected, used purely for log/debug. */
  selectedAt: number;
  afterSecs: number;
  kind: "kill_pi" | "sigkill_pi" | "abort_pi";
}

export function NurseTestDropdown({
  activeId,
  streaming,
  projectPath,
  setComposerValue,
}: NurseTestDropdownProps) {
  const [open, setOpen] = useState(false);
  const rootRef = useRef<HTMLDivElement>(null);
  const toast = useErrorToast();
  const runtime = useTaskRuntimeState();
  const activeSessionId = runtime.tasks[activeId]?.sessionId ?? null;

  // One pending trigger at a time. The effect below watches for streaming +
  // sessionId to flip on, then arms the timer.
  const [pending, setPending] = useState<PendingTrigger | null>(null);
  const armedRef = useRef(false);
  const timerRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  // Outside-click dismiss.
  useEffect(() => {
    if (!open) return;
    const onDoc = (e: MouseEvent) => {
      if (rootRef.current && !rootRef.current.contains(e.target as Node)) {
        setOpen(false);
      }
    };
    document.addEventListener("mousedown", onDoc);
    return () => document.removeEventListener("mousedown", onDoc);
  }, [open]);

  // Cleanup on unmount or task switch.
  useEffect(() => {
    return () => {
      if (timerRef.current) {
        clearTimeout(timerRef.current);
        timerRef.current = null;
      }
    };
  }, []);

  // If the user switches tasks while a trigger is pending, drop it — the
  // scenario belongs to the task the dropdown was open over.
  useEffect(() => {
    if (pending && pending.taskId !== activeId) {
      if (timerRef.current) clearTimeout(timerRef.current);
      timerRef.current = null;
      armedRef.current = false;
      setPending(null);
    }
  }, [activeId, pending]);

  // Arm the timer the moment streaming + sessionId are both ready.
  useEffect(() => {
    if (!pending) return;
    if (armedRef.current) return;
    if (!streaming) return;
    if (!activeSessionId) return;
    if (pending.taskId !== activeId) return;

    armedRef.current = true;
    const sid = activeSessionId;
    const scenarioId = pending.scenarioId;
    const afterMs = pending.afterSecs * 1000;
    const kind = pending.kind;

    timerRef.current = setTimeout(() => {
      void (async () => {
        try {
          // `kill_pi` is the orderly IPC kill (graceful shutdown, session
          // removed from the engine). `sigkill_pi` is a raw SIGKILL on the
          // Pi PID that leaves the session in the engine map so
          // ProcessHealth can catch `!is_alive()` on its next slow tick —
          // which is what the "Process crash" scenario is actually
          // supposed to demonstrate. `abort_pi` is reserved for a future
          // steer-based variant.
          if (kind === "kill_pi") {
            await killPiSession(sid);
            toast.warning(
              `Nurse test fired: ${scenarioId}`,
              `Killed Pi session ${sid.slice(0, 8)}… (orderly shutdown).`,
            );
          } else if (kind === "sigkill_pi") {
            await sigkillPiSession(sid);
            toast.warning(
              `Nurse test fired: ${scenarioId}`,
              `SIGKILL'd Pi session ${sid.slice(0, 8)}… — watch for ProcessHealth on its next slow tick.`,
            );
          }
        } catch (err) {
          toast.error(
            `Nurse test side-trigger failed: ${scenarioId}`,
            formatIpcError(err),
          );
        } finally {
          armedRef.current = false;
          timerRef.current = null;
          setPending(null);
        }
      })();
    }, afterMs);
  }, [streaming, activeSessionId, pending, activeId, toast]);

  const handlePick = useCallback(
    (scenario: NurseScenario) => {
      setOpen(false);
      setComposerValue(scenario.prompt);

      if (scenario.needsProjectPath && !projectPath) {
        toast.warning(
          "No project path",
          `${scenario.label} needs an active project directory to be meaningful.`,
        );
      }

      if (scenario.sideTrigger) {
        // Clear any earlier pending trigger before installing a new one.
        if (timerRef.current) {
          clearTimeout(timerRef.current);
          timerRef.current = null;
        }
        armedRef.current = false;
        setPending({
          scenarioId: scenario.id,
          taskId: activeId,
          selectedAt: Date.now(),
          afterSecs: scenario.sideTrigger.afterSecs,
          kind: scenario.sideTrigger.kind,
        });
        toast.info(
          `Scenario armed: ${scenario.label}`,
          `Press Send — side-trigger (${scenario.sideTrigger.kind}) will fire ${scenario.sideTrigger.afterSecs}s after streaming starts.`,
        );
      } else {
        toast.info(
          `Scenario loaded: ${scenario.label}`,
          scenario.modelHint
            ? `Suggested model: ${scenario.modelHint}. Press Send.`
            : "Press Send to fire it.",
        );
      }
    },
    [activeId, projectPath, setComposerValue, toast],
  );

  const cancelPending = useCallback(() => {
    if (timerRef.current) {
      clearTimeout(timerRef.current);
      timerRef.current = null;
    }
    armedRef.current = false;
    const id = pending?.scenarioId;
    setPending(null);
    if (id) toast.info(`Scenario cancelled: ${id}`);
  }, [pending, toast]);

  const pendingScenario = pending ? findNurseScenario(pending.scenarioId) : null;

  const pureScenarios = NURSE_SCENARIOS.filter((s) => s.group === "pure-prompt");
  const sideTriggerScenarios = NURSE_SCENARIOS.filter(
    (s) => s.group === "side-trigger",
  );

  return (
    <div className="relative" ref={rootRef}>
      <button
        onClick={() => setOpen((v) => !v)}
        title="Bait prompts for testing the Nurse engine"
        className={`h-7 px-2 rounded-md flex items-center gap-1.5 text-[11px] font-medium transition-all shrink-0 ${
          pending
            ? "bg-amber-500/15 text-amber-300 border border-amber-500/30"
            : open
              ? "bg-ink-700/60 text-white/70"
              : "text-dim hover:text-white/60 hover:bg-ink-700/60"
        }`}
      >
        {I.bug({ size: 11 })}
        <span>Test Nurse</span>
        {I.chevD({
          size: 9,
          className: `transition ${open ? "rotate-180" : ""}`,
        })}
      </button>

      {open && (
        <div className="absolute bottom-[calc(100%+6px)] right-0 w-[360px] bg-ink-850 border border-line rounded-xl shadow-2xl z-30 overflow-hidden">
          <div className="px-3.5 py-2.5 border-b border-line text-[10.5px] uppercase tracking-[.18em] text-dim font-semibold flex items-center justify-between">
            <span>Nurse scenarios</span>
            {pending && (
              <button
                onClick={cancelPending}
                className="text-[10px] text-amber-300 hover:text-amber-200 normal-case tracking-normal"
              >
                Cancel armed
              </button>
            )}
          </div>

          <div className="max-h-[420px] overflow-y-auto">
            <ScenarioGroup
              title="Pure prompts"
              subtitle="Bait the model into stalling/looping naturally."
              scenarios={pureScenarios}
              onPick={handlePick}
              pendingId={pending?.scenarioId}
            />
            <ScenarioGroup
              title="With side-trigger"
              subtitle="Real Pi failure is triggered N seconds after streaming starts."
              scenarios={sideTriggerScenarios}
              onPick={handlePick}
              pendingId={pending?.scenarioId}
            />
          </div>

          {pendingScenario && (
            <div className="border-t border-line px-3.5 py-2 bg-amber-500/5 text-[11px] text-amber-200">
              Armed: <span className="font-mono">{pendingScenario.id}</span> — fires{" "}
              {pendingScenario.sideTrigger?.afterSecs}s after streaming begins.
            </div>
          )}
        </div>
      )}
    </div>
  );
}

interface ScenarioGroupProps {
  title: string;
  subtitle: string;
  scenarios: NurseScenario[];
  onPick: (s: NurseScenario) => void;
  pendingId?: string;
}

function ScenarioGroup({
  title,
  subtitle,
  scenarios,
  onPick,
  pendingId,
}: ScenarioGroupProps) {
  if (scenarios.length === 0) return null;
  return (
    <div>
      <div className="px-3.5 pt-2.5 pb-1">
        <div className="text-[10.5px] uppercase tracking-wider text-dim font-semibold">
          {title}
        </div>
        <div className="text-[10.5px] text-dim/80 leading-tight mt-0.5">
          {subtitle}
        </div>
      </div>
      <div>
        {scenarios.map((s) => {
          const isPending = pendingId === s.id;
          return (
            <button
              key={s.id}
              onClick={() => onPick(s)}
              className={`w-full text-left px-3.5 py-2 hover:bg-ink-800/80 transition border-l-2 ${
                isPending ? "border-amber-400 bg-amber-500/5" : "border-transparent"
              }`}
            >
              <div className="flex items-center justify-between gap-2">
                <span className="text-[12.5px] text-white/90">{s.label}</span>
                {s.sideTrigger && (
                  <span className="text-[9.5px] uppercase tracking-wider text-amber-300/80 font-semibold">
                    +{s.sideTrigger.afterSecs}s {s.sideTrigger.kind.replace("_", " ")}
                  </span>
                )}
              </div>
              <div className="text-[10.5px] text-dim leading-snug mt-0.5">
                {s.description}
              </div>
              <div className="text-[10px] text-dim/80 font-mono mt-1">
                trips: {s.trips}
                {s.modelHint && <span className="ml-2">model: {s.modelHint}</span>}
                {s.needsProjectPath && (
                  <span className="ml-2 text-amber-400/80">needs project</span>
                )}
              </div>
            </button>
          );
        })}
      </div>
    </div>
  );
}
