import React, {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useRef,
  useState,
} from "react";
import type { UnlistenFn } from "@tauri-apps/api/event";
import { onTestProgress, safeUnlisten, type TestProgressEvent } from "../lib/events";
import * as ipc from "../lib/ipc";
import type { TestRunRecord } from "../lib/ipc";

/**
 * State shape lifted out of `TestsScreen`. This context lives above the
 * screen router in `App.tsx` so the singleton `onTestProgress` listener
 * survives tab switches (the original bug: switching away from Tests
 * unmounted the screen and tore down the listener, hiding the panel
 * until the next event arrived after remount).
 */
export interface TestRunState {
  activeRunId: string | null;
  activePhase: string | null;
  /** "idle" | "starting" | "started" | "progress" | "completed" | "failed" */
  activeStatus: string;
  activeMessage: string;
  phaseLog: TestProgressEvent[];
  terminalRecord: TestRunRecord | null;
  busy: boolean;
  /**
   * Bumped to `Date.now()` whenever a terminal (`complete` / `failed`)
   * event lands. The screen watches this with a `useEffect` to refresh
   * its `history` list — keeps the screen-local history refresh decoupled
   * from listener lifetime.
   */
  lastTerminalAt: number | null;
}

export interface TestRunActions {
  /**
   * Called by `TestsScreen.onRunTest` right after `ipc.runStabilityTest()`
   * resolves. Resets `phaseLog` / `terminalRecord` and flips the panel into
   * the "starting" state so the UI shows up before the first event lands.
   */
  startRun: (runId: string) => void;
  /** Called from `runStabilityTest`'s catch branch — flips `busy=false`. */
  markStartFailed: (err: string) => void;
  /** Reserved for future "Clear" UX. Not used yet. */
  clear: () => void;
}

const initialState: TestRunState = {
  activeRunId: null,
  activePhase: null,
  activeStatus: "idle",
  activeMessage: "",
  phaseLog: [],
  terminalRecord: null,
  busy: false,
  lastTerminalAt: null,
};

type Ctx = TestRunState & TestRunActions;

const TestRunContext = createContext<Ctx | null>(null);

export function TestRunProvider({ children }: { children: React.ReactNode }) {
  const [activeRunId, setActiveRunId] = useState<string | null>(initialState.activeRunId);
  const [activePhase, setActivePhase] = useState<string | null>(initialState.activePhase);
  const [activeStatus, setActiveStatus] = useState<string>(initialState.activeStatus);
  const [activeMessage, setActiveMessage] = useState<string>(initialState.activeMessage);
  const [phaseLog, setPhaseLog] = useState<TestProgressEvent[]>(initialState.phaseLog);
  const [terminalRecord, setTerminalRecord] = useState<TestRunRecord | null>(
    initialState.terminalRecord,
  );
  const [busy, setBusy] = useState<boolean>(initialState.busy);
  const [lastTerminalAt, setLastTerminalAt] = useState<number | null>(
    initialState.lastTerminalAt,
  );

  // Single, app-lifetime `onTestProgress` listener. The original
  // bug-causing code lived inside `TestsScreen`'s `useEffect` and was
  // unmounted on every tab switch.
  const unlistenRef = useRef<UnlistenFn | null>(null);
  useEffect(() => {
    let mounted = true;
    (async () => {
      const u = await onTestProgress((evt) => {
        if (!mounted) return;
        setActiveRunId(evt.run_id);
        setActivePhase(evt.phase);
        setActiveStatus(evt.status);
        setActiveMessage(evt.message);
        setPhaseLog((prev) => [...prev, evt]);
        if (evt.phase === "complete" || evt.phase === "failed") {
          if (evt.record) setTerminalRecord(evt.record as TestRunRecord);
          setBusy(false);
          setLastTerminalAt(Date.now());
        }
      });
      if (!mounted) {
        // Race: provider unmounted during await. Drop the listener.
        safeUnlisten(u);
        return;
      }
      unlistenRef.current = u;
    })();
    return () => {
      mounted = false;
      safeUnlisten(unlistenRef.current);
      unlistenRef.current = null;
    };
  }, []);

  // Phase-2 rehydration: on provider mount, ask the backend whether a
  // stability test is already running (covers app-restart and the gap
  // between provider mount and the next progress event).
  useEffect(() => {
    let cancelled = false;
    (async () => {
      try {
        const snap = await ipc.getActiveTestRun();
        if (cancelled || !snap) return;
        // Don't clobber state we already learned from a live event.
        setActiveRunId((prev) => prev ?? snap.run_id);
        if (snap.last_phase) setActivePhase((prev) => prev ?? snap.last_phase ?? null);
        if (snap.last_status) {
          setActiveStatus((prev) => (prev === "idle" ? snap.last_status ?? prev : prev));
        }
        if (snap.last_message) {
          setActiveMessage((prev) => (prev ? prev : snap.last_message ?? ""));
        }
        setBusy(true);
      } catch {
        // No active run, or IPC not available (non-Tauri preview). Ignore.
      }
    })();
    return () => {
      cancelled = true;
    };
  }, []);

  const startRun = useCallback((runId: string) => {
    setPhaseLog([]);
    setTerminalRecord(null);
    setBusy(true);
    setActiveStatus("starting");
    setActiveMessage("Starting stability test…");
    setActiveRunId(runId);
  }, []);

  const markStartFailed = useCallback((_err: string) => {
    setBusy(false);
  }, []);

  const clear = useCallback(() => {
    setActiveRunId(null);
    setActivePhase(null);
    setActiveStatus("idle");
    setActiveMessage("");
    setPhaseLog([]);
    setTerminalRecord(null);
    setBusy(false);
    setLastTerminalAt(null);
  }, []);

  const value: Ctx = {
    activeRunId,
    activePhase,
    activeStatus,
    activeMessage,
    phaseLog,
    terminalRecord,
    busy,
    lastTerminalAt,
    startRun,
    markStartFailed,
    clear,
  };

  return <TestRunContext.Provider value={value}>{children}</TestRunContext.Provider>;
}

export function useTestRun(): Ctx {
  const ctx = useContext(TestRunContext);
  if (!ctx) {
    throw new Error("useTestRun must be used inside a <TestRunProvider>");
  }
  return ctx;
}
