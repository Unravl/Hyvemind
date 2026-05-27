import { useEffect, useMemo, useRef, useState } from "react";
import * as ipc from "../lib/ipc";
import type { GoFn } from "../App";
import type { TestProgressEvent } from "../lib/events";
import type {
  StabilityTestConfigDto,
  TestRunRecord,
  TestRunSummary,
} from "../lib/ipc";
import type { HivemindSummary } from "../lib/types";
import { parseRoundsConfigWithStatus } from "../lib/review-mode";
import { Btn, Panel, Pill, Select } from "../components/atoms";
import { I } from "../components/icons";
import { ModelBrowserModal } from "./ModelBrowser";
import { useTestRun } from "../state/TestRunProvider";
import { useSetting } from "../lib/SettingsProvider";

/* ── Phase pipeline definition ────────────────────────────────── */

const PHASES: Array<{ id: string; label: string }> = [
  { id: "setup", label: "Sandbox" },
  { id: "task_intake", label: "Task started" },
  { id: "waiting_questions", label: "Questions" },
  { id: "auto_answer", label: "Auto-answered" },
  { id: "waiting_plan", label: "Plan" },
  { id: "hivemind", label: "Hivemind review" },
  { id: "implement", label: "Implementation" },
  { id: "ai_verify", label: "AI verifier" },
  { id: "complete", label: "Done" },
];

type PhaseState = "pending" | "active" | "completed" | "failed";

/* ── Tests screen ─────────────────────────────────────────────── */

export function TestsScreen({ go }: { go: GoFn }) {
  const [config, setConfig] = useState<StabilityTestConfigDto | null>(null);
  const [history, setHistory] = useState<TestRunSummary[]>([]);
  const [expandedRun, setExpandedRun] = useState<string | null>(null);
  const [runDetail, setRunDetail] = useState<TestRunRecord | null>(null);
  const [error, setError] = useState<string | null>(null);

  // Lifted state lives in the app-level provider so it survives tab
  // switches and the `onTestProgress` listener isn't torn down on unmount.
  const {
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
  } = useTestRun();

  const phasesReached = useMemo(() => {
    const set = new Set<string>();
    for (const e of phaseLog) set.add(e.phase);
    return set;
  }, [phaseLog]);

  // Editable model config form state.
  const [taskModel, setTaskModel] = useState("");
  const [verifierModel, setVerifierModel] = useState("");
  const [hivemindId, setHivemindId] = useState<string | null>(null);
  const [hivemindOptions, setHivemindOptions] = useState<HivemindSummary[]>([]);
  // audit 6.7 — read the default model from the shared SettingsProvider
  // instead of issuing a duplicate `getSettings()` call on mount.
  const defaultModel = useSetting("default_model") ?? "";
  const [showTaskModelBrowser, setShowTaskModelBrowser] = useState(false);
  const [showVerifierModelBrowser, setShowVerifierModelBrowser] = useState(false);

  // Initial load.
  useEffect(() => {
    let cancelled = false;
    (async () => {
      try {
        const [cfg, runs, hms] = await Promise.all([
          ipc.getStabilityTestConfig(),
          ipc.listTestRuns(50),
          ipc.listHiveminds().catch(() => [] as HivemindSummary[]),
        ]);
        if (cancelled) return;
        setConfig(cfg);
        setTaskModel(cfg.taskModel);
        setVerifierModel(cfg.verifierModel);
        setHivemindId(cfg.hivemindId ?? null);
        setHivemindOptions(hms);
        setHistory(runs);
      } catch (e) {
        if (!cancelled) setError(`failed to load tests config: ${e}`);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, []);

  // History reload on terminal events. The provider owns the live
  // `onTestProgress` listener and bumps `lastTerminalAt` when a run ends.
  // We gate with a ref to avoid the spurious reload that would otherwise
  // fire on first mount when `lastTerminalAt` is already non-null from a
  // previous run that completed while this screen was unmounted — actually
  // we *want* exactly one reload in that case so the History row appears,
  // so the ref starts at `null` and matches the initial value.
  const lastSeenTerminalAt = useRef<number | null>(null);
  useEffect(() => {
    if (lastTerminalAt === null) return;
    if (lastSeenTerminalAt.current === lastTerminalAt) return;
    lastSeenTerminalAt.current = lastTerminalAt;
    ipc.listTestRuns(50).then((r) => setHistory(r)).catch(() => {});
  }, [lastTerminalAt]);

  const onSaveConfig = async () => {
    const next: StabilityTestConfigDto = {
      taskModel: taskModel.trim(),
      verifierModel: verifierModel.trim(),
      hivemindId: hivemindId && hivemindId.trim() ? hivemindId.trim() : null,
    };
    try {
      const saved = await ipc.setStabilityTestConfig(next);
      setConfig(saved);
      setError(null);
    } catch (e) {
      setError(`failed to save config: ${e}`);
    }
  };

  // Derived hint for the currently-selected Hivemind.
  const selectedHivemind = useMemo(
    () => hivemindOptions.find((h) => h.id === hivemindId) ?? null,
    [hivemindOptions, hivemindId],
  );
  const hivemindHint = useMemo(() => {
    if (!selectedHivemind) return null;
    const { rounds, ok, error: parseErr } = parseRoundsConfigWithStatus(selectedHivemind.rounds_config);
    if (!ok) {
      return { kind: "error" as const, text: `⚠ rounds_config parse error${parseErr ? `: ${parseErr}` : ""}` };
    }
    const flat: string[] = [];
    const seen = new Set<string>();
    for (const r of rounds) {
      for (const m of r.models) {
        const key = `${m.provider}/${m.id}`;
        if (!seen.has(key) && m.provider && m.id) {
          seen.add(key);
          flat.push(key);
        }
      }
    }
    const modelText = flat.length > 0 ? flat.join(", ") : "no models";
    return {
      kind: "ok" as const,
      text: `${rounds.length} round${rounds.length === 1 ? "" : "s"} · ${modelText}`,
    };
  }, [selectedHivemind]);

  const onRunTest = async () => {
    if (busy) return;
    setError(null);
    try {
      const { run_id } = await ipc.runStabilityTest();
      startRun(run_id);
    } catch (e) {
      const msg = `failed to start test: ${e}`;
      setError(msg);
      markStartFailed(msg);
    }
  };

  const onCancel = async () => {
    try {
      await ipc.cancelTestRun();
    } catch (e) {
      setError(`cancel failed: ${e}`);
    }
  };

  const onExpand = async (runId: string) => {
    if (expandedRun === runId) {
      setExpandedRun(null);
      setRunDetail(null);
      return;
    }
    setExpandedRun(runId);
    try {
      const detail = await ipc.getTestRun(runId);
      setRunDetail(detail);
    } catch (e) {
      setError(`failed to load run: ${e}`);
    }
  };

  /* ── Render ────────────────────────────────────────────────── */

  return (
    <div className="h-full overflow-auto p-6 space-y-6">
      <div className="flex items-center justify-between gap-3">
        <div>
          <h1 className="text-xl font-semibold text-white">Stability Tests</h1>
          <p className="text-sm text-muted mt-1">
            Drives a real Task through the full pipeline (questions → plan →
            Hivemind → implementation) and asks an AI verifier to confirm it
            ran correctly. Run this before releases.
          </p>
        </div>
        <div className="flex items-center gap-2">
          {busy ? (
            <Btn kind="outline" onClick={onCancel}>
              Cancel
            </Btn>
          ) : null}
          <Btn
            kind="primary"
            onClick={onRunTest}
            disabled={busy || !taskModel.trim() || !verifierModel.trim() || !hivemindId}
            icon={I.rocket ? I.rocket({ size: 14 }) : null}
          >
            {busy ? "Running…" : "Run Stability Test"}
          </Btn>
        </div>
      </div>

      {error && (
        <div className="rounded-md border border-red-500/40 bg-red-500/10 px-4 py-2 text-sm text-red-300">
          {error}
        </div>
      )}

      {/* Config */}
      <Panel
        title="Test Configuration"
        right={
          <Btn kind="secondary" size="sm" onClick={onSaveConfig}>
            Save config
          </Btn>
        }
      >
        <div className="grid grid-cols-1 md:grid-cols-2 gap-4">
          <ConfigField
            label="Task model"
            help={`Used for the planning + implementation agent. Falls back to default model (${defaultModel || "unset"}) if unset.`}
          >
            <ModelPickerButton
              value={taskModel}
              placeholder="Choose model…"
              onClick={() => setShowTaskModelBrowser(true)}
            />
          </ConfigField>
          <ConfigField
            label="Verifier model"
            help={`Used for the AI verifier session that produces the final pass/fail JSON verdict. Falls back to default model (${defaultModel || "unset"}) if unset.`}
          >
            <ModelPickerButton
              value={verifierModel}
              placeholder="Choose model…"
              onClick={() => setShowVerifierModelBrowser(true)}
            />
          </ConfigField>
          <ConfigField
            label="Hivemind"
            help="Round count is honored; per-round model variation is flattened for the smoke test."
          >
            {hivemindOptions.length === 0 ? (
              <div className="rounded-md border border-line bg-ink-850/60 px-3 py-2 text-xs text-muted flex items-center justify-between gap-2">
                <span>No Hiveminds saved — create one in the Hiveminds tab.</span>
                <Btn kind="outline" size="sm" onClick={() => go("hiveminds")}>
                  Open Hiveminds
                </Btn>
              </div>
            ) : (
              <>
                <Select
                  value={hivemindId ?? ""}
                  onChange={(e) => setHivemindId(e.target.value || null)}
                  options={[
                    { value: "", label: "None — fail run if missing" },
                    ...hivemindOptions.map((h) => ({ value: h.id, label: h.name })),
                  ]}
                />
                {hivemindHint && (
                  <p
                    className={`text-[11px] leading-snug ${
                      hivemindHint.kind === "error" ? "text-red-300" : "text-muted"
                    }`}
                  >
                    {hivemindHint.text}
                  </p>
                )}
                {!hivemindId && (
                  <p className="text-[11px] text-amber-300/80 leading-snug">
                    Pick a Hivemind to enable runs.
                  </p>
                )}
              </>
            )}
          </ConfigField>
        </div>
      </Panel>

      {/* Active run */}
      {(busy || activeRunId) && (
        <Panel
          title={
            <div className="flex items-center gap-2">
              <span className="text-sm font-semibold text-slate-200">
                Active run
              </span>
              {activeRunId && (
                <span className="text-xs text-muted font-mono">
                  {activeRunId}
                </span>
              )}
            </div>
          }
          right={
            <Pill tone={statusToTone(activeStatus)}>
              {prettyStatus(activeStatus)}
            </Pill>
          }
        >
          <div className="space-y-3">
            <PhasePipeline activePhase={activePhase} phasesReached={phasesReached} terminal={terminalRecord?.status} />
            <p className="text-xs text-muted">{activeMessage}</p>
            {terminalRecord && (
              <RunDetail record={terminalRecord} />
            )}
          </div>
        </Panel>
      )}

      {/* Model browser modals */}
      <ModelBrowserModal
        open={showTaskModelBrowser}
        onClose={() => setShowTaskModelBrowser(false)}
        selectLabel="Use as Task model"
        initialModel={taskModel}
        onSelect={(model) => {
          setTaskModel(`${model.provider}/${model.id}`);
          setShowTaskModelBrowser(false);
        }}
      />
      <ModelBrowserModal
        open={showVerifierModelBrowser}
        onClose={() => setShowVerifierModelBrowser(false)}
        selectLabel="Use as Verifier model"
        initialModel={verifierModel}
        onSelect={(model) => {
          setVerifierModel(`${model.provider}/${model.id}`);
          setShowVerifierModelBrowser(false);
        }}
      />

      {/* History */}
      <Panel title="History" right={<span className="text-xs text-muted">{history.length} run(s)</span>}>
        {history.length === 0 ? (
          <p className="text-sm text-muted">No test runs yet. Click <strong>Run Stability Test</strong> above.</p>
        ) : (
          <div className="space-y-1">
            {history.map((r) => (
              <HistoryRow
                key={r.run_id}
                summary={r}
                expanded={expandedRun === r.run_id}
                detail={expandedRun === r.run_id ? runDetail : null}
                onToggle={() => onExpand(r.run_id)}
              />
            ))}
          </div>
        )}
      </Panel>
    </div>
  );
}

/* ── Subcomponents ────────────────────────────────────────────── */

function ModelPickerButton({
  value,
  placeholder,
  onClick,
}: {
  value: string;
  placeholder: string;
  onClick: () => void;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      className="flex items-center gap-2 bg-ink-850 border border-line rounded-lg px-3 py-1.5 text-sm hover:border-honey-500/40 transition-colors cursor-pointer w-full"
    >
      {value ? (
        <span className="font-mono text-slate-200 truncate">
          {value.includes("/") ? value.replace("/", " / ") : value}
        </span>
      ) : (
        <span className="text-dim">{placeholder}</span>
      )}
      <span className="text-muted ml-auto shrink-0">{I.chevR({ size: 12 })}</span>
    </button>
  );
}

function ConfigField({
  label,
  help,
  children,
}: {
  label: string;
  help?: string;
  children: React.ReactNode;
}) {
  return (
    <div className="space-y-1.5">
      <label className="text-xs font-medium text-slate-300">{label}</label>
      {children}
      {help && <p className="text-[11px] text-muted leading-snug">{help}</p>}
    </div>
  );
}

function PhasePipeline({
  activePhase,
  phasesReached,
  terminal,
}: {
  activePhase: string | null;
  phasesReached: Set<string>;
  terminal?: string;
}) {
  return (
    <div className="flex flex-wrap items-center gap-2">
      {PHASES.map((p) => {
        const state: PhaseState =
          terminal === "failed" && p.id === activePhase
            ? "failed"
            : terminal === "passed" || phasesReached.has(p.id) && p.id !== activePhase
              ? "completed"
              : p.id === activePhase
                ? "active"
                : "pending";
        const cls =
          state === "completed"
            ? "bg-emerald-500/15 text-emerald-300 border-emerald-500/30"
            : state === "active"
              ? "bg-honey-500/15 text-honey-300 border-honey-500/30"
              : state === "failed"
                ? "bg-red-500/15 text-red-300 border-red-500/30"
                : "bg-ink-700/40 text-muted border-line";
        return (
          <span
            key={p.id}
            className={`inline-flex items-center gap-1.5 px-2 py-0.5 rounded-md border text-[11px] ${cls}`}
          >
            {state === "active" && (
              <span className="w-1.5 h-1.5 rounded-full bg-honey-400 animate-pulse" />
            )}
            {state === "completed" && <span className="text-emerald-400">✓</span>}
            {state === "failed" && <span>!</span>}
            {p.label}
          </span>
        );
      })}
    </div>
  );
}

function RunDetail({ record }: { record: TestRunRecord }) {
  return (
    <div className="mt-3 rounded-md border border-line bg-ink-850/60 p-3 space-y-3">
      <div className="flex items-center gap-3 text-xs">
        <Pill tone={record.status === "passed" ? "green" : "red"}>
          {record.status.toUpperCase()}
        </Pill>
        <span className="text-muted">{(record.duration_ms / 1000).toFixed(1)}s</span>
        <span className="text-muted">${record.total_cost.toFixed(4)}</span>
        {record.hivemind_job_id && (
          <span className="text-muted font-mono text-[10px]">hm: {record.hivemind_job_id.slice(0, 8)}</span>
        )}
      </div>
      <div>
        <h4 className="text-[11px] font-semibold uppercase tracking-wider text-muted mb-1">Programmatic gates</h4>
        <div className="space-y-0.5">
          {record.gates.map((g, i) => (
            <div key={i} className="flex items-start gap-2 text-xs">
              <span className={g.passed ? "text-emerald-400" : "text-red-400"}>
                {g.passed ? "✓" : "✗"}
              </span>
              <span className="font-mono text-[11px] text-slate-300">{g.name}</span>
              <span className="text-muted">— {g.detail}</span>
            </div>
          ))}
        </div>
      </div>
      {record.verdict && (
        <div>
          <h4 className="text-[11px] font-semibold uppercase tracking-wider text-muted mb-1">
            AI verifier ({(record.verdict.confidence * 100).toFixed(0)}% confidence)
          </h4>
          <p className="text-xs text-slate-300 mb-1">
            <Pill tone={record.verdict.passed ? "green" : "red"}>
              {record.verdict.passed ? "PASS" : "FAIL"}
            </Pill>
            <span className="ml-2">{record.verdict.summary}</span>
          </p>
          {record.verdict.issues.length > 0 && (
            <ul className="text-xs text-red-300/80 list-disc list-inside space-y-0.5">
              {record.verdict.issues.map((iss, i) => (
                <li key={i}>{iss}</li>
              ))}
            </ul>
          )}
        </div>
      )}
      {record.error && (
        <div className="text-xs text-red-300">
          <strong>Error:</strong> {record.error}
        </div>
      )}
    </div>
  );
}

function HistoryRow({
  summary,
  expanded,
  detail,
  onToggle,
}: {
  summary: TestRunSummary;
  expanded: boolean;
  detail: TestRunRecord | null;
  onToggle: () => void;
}) {
  const tone =
    summary.status === "passed"
      ? "green"
      : summary.status === "failed" || summary.status === "error"
        ? "red"
        : summary.status === "cancelled"
          ? "neutral"
          : "honey";
  return (
    <div className="rounded-md border border-line/60 bg-ink-850/40 overflow-hidden">
      <button
        onClick={onToggle}
        className="w-full flex items-center gap-3 px-3 py-2 text-left hover:bg-ink-700/40 transition-colors"
      >
        <Pill tone={tone}>{summary.status.toUpperCase()}</Pill>
        <span className="text-xs font-mono text-muted truncate flex-1">{summary.run_id}</span>
        <span className="text-xs text-muted">
          {summary.pass_count}/{summary.pass_count + summary.fail_count} gates
        </span>
        <span className="text-xs text-muted">
          {(summary.duration_ms / 1000).toFixed(1)}s
        </span>
        <span className="text-xs text-muted font-mono">
          ${summary.total_cost.toFixed(4)}
        </span>
        <span className="text-muted">{expanded ? "▾" : "▸"}</span>
      </button>
      {expanded && detail && (
        <div className="px-3 pb-3">
          <RunDetail record={detail} />
        </div>
      )}
    </div>
  );
}

/* ── Helpers ──────────────────────────────────────────────────── */

function statusToTone(status: string): "green" | "red" | "honey" | "neutral" {
  if (status === "completed") return "green";
  if (status === "failed") return "red";
  if (status === "started" || status === "progress" || status === "starting") return "honey";
  return "neutral";
}

function prettyStatus(status: string): string {
  if (status === "idle") return "Idle";
  if (status === "starting") return "Starting…";
  if (status === "started") return "Running";
  if (status === "progress") return "Running";
  if (status === "completed") return "Completed";
  if (status === "failed") return "Failed";
  return status;
}
