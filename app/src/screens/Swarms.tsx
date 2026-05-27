import React, { useState, useRef, useEffect, useCallback } from "react";
import { GoFn } from "../App";
import { I } from "../components/icons";
import { Btn, StatusBadge, Pill, STATUS, Input, Select } from "../components/atoms";
import { useProject } from "../components/ProjectPicker";
import { SWARMS, MODELS, HIVEMINDS, PROJECTS, Swarm } from "../data/mock";
import { isTauri } from "../lib/tauri";
import { confirmDialog } from "../lib/confirm";
import {
  listSwarms,
  deleteSwarm,
  getSwarmFeatures,
  getSwarmMilestones,
  getSwarmUsage,
  listHiveminds,
  resumeSwarm,
  SwarmUsageSummary,
} from "../lib/ipc";
import { onSwarmEvent, onSwarmReconciled, safeUnlisten } from "../lib/events";
import { getCompletionSoundConfig, playCompletionSound } from "../lib/sounds";
import type { SwarmState, Feature, Milestone } from "../lib/types";
import { useTaskRuntime } from "../lib/taskRuntime";
import { formatCost, formatElapsed, isLiveSwarmStatus, swarmElapsedMs } from "../lib/format";
import { useErrorToast } from "../components/Toast";

/* ── Draft persistence key (shared with NewSwarm.tsx) ─────── */
const SWARM_DRAFT_KEY = "hyvemind:swarm-draft";

/* ── Local helpers ─────────────────────────────────────────── */

const stripModel = (id: string) => id.replace(/^claude-/, "");

interface ModelEntry {
  short: string;
  full: string;
  ctx: string;
  price: string;
}

const MODEL_GROUPS: Record<string, ModelEntry[]> = (() => {
  const groups: Record<string, ModelEntry[]> = {};
  MODELS.forEach((m) => {
    const short = stripModel(m.id);
    (groups[m.provider] = groups[m.provider] || []).push({
      short,
      full: m.id,
      ctx: m.ctx,
      price: m.price,
    });
  });
  return groups;
})();

const ROLE_META: Record<string, { label: string; glyph: string }> = {
  queen: { label: "Queen", glyph: "\u{1F451}" },
  scout: { label: "Scout", glyph: "\u{1F50D}" },
  worker: { label: "Worker", glyph: "\u{1F41D}" },
  guard: { label: "Guard", glyph: "\u{1F6E1}" },
};

const providerForModel = (short: string): string => {
  for (const [prov, models] of Object.entries(MODEL_GROUPS)) {
    if (models.find((m) => m.short === short)) return prov;
  }
  return Object.keys(MODEL_GROUPS)[0] || "anthropic";
};

/* ── SwarmState → mock Swarm adapter ──────────────────────── */

// Slim adapter: only carries the fields the legacy mock-data path needs and
// the role model strings used by `RoleChip` provider lookup. All numeric
// stats (duration / cost / feature counts), milestone/activity, and the
// hivemind label flow through dedicated props sourced from `rawState`,
// `swarmDetails`, and `hivemindNames` — not through this adapter. This
// eliminates the silent data-loss path where fabricated zeroes/empties
// masked missing IPC calls.
const swarmStateToSwarm = (s: SwarmState): Swarm => ({
  id: s.id,
  name: s.name,
  status:
    s.status === "implementing" ? "running"
    : s.status === "interrupted" ? "paused"
    : s.status === "cancelled" ? "failed"
    : s.status as any,
  // Legacy shape — unused by the new card render path; kept so the
  // Swarm type stays satisfied without churning the mock module.
  duration: "",
  cost: "",
  features: [0, 0],
  milestone: "",
  queen: s.model_settings.primary_model,
  // TODO(model_settings): no worker_model field yet — show primary_model so
  // the chip is at least populated.
  worker: s.model_settings.primary_model,
  scout: s.model_settings.scout_model,
  guard: s.model_settings.guard_model ?? s.model_settings.primary_model,
  // Legacy hivemind value preserved for the mock-only path; the live
  // Tauri path resolves the label via `hivemindNames` instead.
  hivemind: "none",
  cwd: s.working_directory,
  error: s.error ?? undefined,
});

/* ── Dropdown ──────────────────────────────────────────────── */

interface DropdownOption {
  value: string;
  label?: string;
  meta?: string;
  kind?: "group";
}

interface DropdownProps {
  value: string;
  label?: string;
  onPick: (v: string) => void;
  options: DropdownOption[];
  width?: number;
  align?: "left" | "right";
}

function Dropdown({
  value,
  label,
  onPick,
  options,
  width = 200,
  align = "left",
}: DropdownProps) {
  const [open, setOpen] = useState(false);
  const ref = useRef<HTMLSpanElement>(null);

  useEffect(() => {
    if (!open) return;
    const onDoc = (e: MouseEvent) => {
      if (ref.current && !ref.current.contains(e.target as Node))
        setOpen(false);
    };
    document.addEventListener("mousedown", onDoc);
    return () => document.removeEventListener("mousedown", onDoc);
  }, [open]);

  return (
    <span className="relative inline-flex" ref={ref}>
      <button
        type="button"
        onClick={(e) => {
          e.stopPropagation();
          setOpen((o) => !o);
        }}
        className={`group inline-flex items-center gap-1 h-6 font-mono text-[11.5px] px-1.5 rounded border leading-none ${
          open
            ? "bg-ink-700 border-line text-white"
            : "bg-ink-850 border-line/70 text-white/80 hover:border-line hover:bg-ink-700/60 hover:text-white"
        }`}
      >
        <span className="truncate max-w-[160px]">{value}</span>
        <svg
          width="9"
          height="9"
          viewBox="0 0 12 12"
          className="text-dim group-hover:text-muted shrink-0"
        >
          <path
            d="M3 5l3 3 3-3"
            stroke="currentColor"
            strokeWidth="1.4"
            fill="none"
            strokeLinecap="round"
            strokeLinejoin="round"
          />
        </svg>
      </button>
      {open && (
        <div
          className={`absolute z-30 top-full mt-1.5 bg-ink-800 border border-line rounded-lg shadow-2xl p-1.5 max-h-[320px] overflow-auto ${
            align === "right" ? "right-0" : "left-0"
          }`}
          style={{ width }}
        >
          {label && (
            <div className="text-[10px] uppercase tracking-wider text-dim font-semibold px-2 pt-1 pb-1.5">
              {label}
            </div>
          )}
          {options.map((o, i) => {
            if (o.kind === "group") {
              return (
                <div
                  key={`g-${i}`}
                  className="text-[9.5px] uppercase tracking-wider text-dim/80 px-2 pt-1.5 pb-0.5 first:pt-1"
                >
                  {o.label}
                </div>
              );
            }
            const selected = o.value === value;
            return (
              <button
                key={o.value}
                onClick={(e) => {
                  e.stopPropagation();
                  onPick(o.value);
                  setOpen(false);
                }}
                className={`w-full text-left px-2 py-1 rounded flex items-center gap-2 ${
                  selected
                    ? "bg-honey-500/15 text-honey-200"
                    : "hover:bg-ink-700 text-white/85"
                }`}
              >
                <span className="font-mono text-[11.5px] flex-1 truncate">
                  {o.label || o.value}
                </span>
                {o.meta && (
                  <span className="font-mono text-[10px] text-dim">
                    {o.meta}
                  </span>
                )}
                {selected && (
                  <svg width="11" height="11" viewBox="0 0 12 12">
                    <path
                      d="M2.5 6.5l2.5 2.5 4.5-5"
                      stroke="currentColor"
                      strokeWidth="1.6"
                      fill="none"
                      strokeLinecap="round"
                      strokeLinejoin="round"
                    />
                  </svg>
                )}
              </button>
            );
          })}
        </div>
      )}
    </span>
  );
}

/* ── RoleRow ───────────────────────────────────────────────── */

interface RoleRowProps {
  role: string;
  value: string;
  onChange: (v: string) => void;
}

function RoleRow({ role, value, onChange }: RoleRowProps) {
  const meta = ROLE_META[role];
  const provider = providerForModel(value);
  const providerOpts = Object.keys(MODEL_GROUPS).map((p) => ({
    value: p,
    label: p,
    meta: `${MODEL_GROUPS[p].length} models`,
  }));
  const modelOpts = (MODEL_GROUPS[provider] || []).map((m) => ({
    value: m.short,
    label: m.short,
    meta: m.ctx,
  }));

  const onPickProvider = (p: string) => {
    const first = MODEL_GROUPS[p]?.[0]?.short;
    if (first) onChange(first);
  };

  return (
    <div className="grid grid-cols-[16px_60px_1fr_1fr] items-center gap-2 text-[11.5px]">
      <span className="text-[13px] leading-none text-center select-none">
        {meta.glyph}
      </span>
      <span className="text-muted font-medium">{meta.label}</span>
      <Dropdown
        value={provider}
        label="Provider"
        options={providerOpts}
        onPick={onPickProvider}
      />
      <Dropdown
        value={value}
        label={`${meta.label} model · ${provider}`}
        options={modelOpts}
        onPick={onChange}
        width={240}
      />
    </div>
  );
}

/* ── HivemindRow ──────────────────────────────────────────── */

interface HivemindRowProps {
  value: string;
  onChange: (v: string) => void;
}

function HivemindRow({ value, onChange }: HivemindRowProps) {
  const opts = HIVEMINDS.map((h) => ({
    value: h.id,
    label: h.id,
    meta: h.rounds ? `${h.rounds.length} rounds` : undefined,
  }));
  const safeOpts = opts.length
    ? opts
    : ["enhance", "arch-council", "security-review", "perf-audit"].map(
        (v) => ({ value: v, label: v })
      );

  return (
    <div className="grid grid-cols-[16px_60px_1fr_1fr] items-center gap-2 text-[11.5px]">
      {I.hexFill({ size: 11, className: "text-honey-500 justify-self-center" })}
      <span className="text-muted font-medium">Hivemind</span>
      <div className="col-span-2 flex items-center gap-2 min-w-0">
        <Dropdown
          value={value}
          label="Hivemind for final review"
          options={safeOpts}
          onPick={onChange}
          width={240}
        />
        <span className="text-[10.5px] text-dim leading-tight">
          final review pass for Queen / Scout plan
        </span>
      </div>
    </div>
  );
}

/* ── RoleChip ─────────────────────────────────────────────── */

interface RoleChipProps {
  role: string;
  value: string;
  hivemind?: string;
}

function RoleChip({ role, value, hivemind }: RoleChipProps) {
  if (!value) return null;
  const meta = ROLE_META[role];
  const provider = providerForModel(value);
  return (
    <span className="inline-flex items-center gap-1.5 h-6 px-1.5 rounded bg-ink-850 border border-line/70 text-[11px]">
      <span className="text-[12px] leading-none select-none">{meta.glyph}</span>
      <span className="text-muted">{meta.label}:</span>
      <span className="font-mono text-dim">{provider}</span>
      <span className="text-line">/</span>
      <span className="font-mono text-white/85">{value}</span>
      {hivemind && (
        <>
          <span className="text-line">{"·"}</span>
          {I.hexFill({ size: 9, className: "text-honey-500" })}
          <span className="font-mono text-honey-300">{hivemind}</span>
        </>
      )}
    </span>
  );
}

/* ── DeleteSwarmButton ─────────────────────────────────────── */

interface DeleteSwarmButtonProps {
  swarm: Swarm;
  /** When true, the swarm is currently in a live state (running/paused).
   *  We surface a stronger confirm copy so the user knows the running
   *  swarm will be stopped first. */
  isActive: boolean;
  onDelete?: () => void;
}

function DeleteSwarmButton({ swarm, isActive, onDelete }: DeleteSwarmButtonProps) {
  const [deleting, setDeleting] = useState(false);
  const toast = useErrorToast();
  const message = isActive
    ? `Delete swarm "${swarm.name}"? This will stop it and delete all of its data. This cannot be undone.`
    : `Delete swarm "${swarm.name}"? This cannot be undone.`;
  return (
    <Btn
      kind="danger"
      size="sm"
      disabled={deleting}
      onClick={async () => {
        const ok = await confirmDialog(message, {
          title: "Delete swarm",
          okLabel: "Delete",
          cancelLabel: "Cancel",
          kind: "warning",
        });
        if (!ok) return;
        if (!isTauri()) return;
        // `delete_swarm` internally stops a running swarm first — multi-second
        // round-trip. Without this in-flight state users double-click and
        // queue a redundant stop+delete on a now-gone swarm.
        setDeleting(true);
        try {
          await deleteSwarm(swarm.id);
          onDelete?.();
        } catch (e) {
          toast.error(`Failed to delete swarm "${swarm.name}"`, e);
          setDeleting(false);
        }
      }}
    >
      {deleting ? "Deleting\u2026" : "Delete"}
    </Btn>
  );
}

/* ── SwarmCard ─────────────────────────────────────────────── */

interface SwarmDetails {
  done: number;
  total: number;
  usage: SwarmUsageSummary | null;
}

interface SwarmCardProps {
  sw: Swarm;
  /** Authoritative backend state for this swarm, when available. Threaded
   *  through so the Edit button can pass the source-of-truth `SwarmState`
   *  (carrying `model_settings.*` including thinking levels and hivemind
   *  flags) to `NewSwarmScreen` instead of the lossy `Swarm` adapter. */
  rawState?: SwarmState;
  /** Per-swarm details fetched from the backend: feature counts and usage
   *  summary. Undefined on the mock-data path and during the first paint
   *  before fan-out IPC settles. */
  details?: SwarmDetails;
  /** Resolved hivemind label (name) for the swarm's `hivemind_id`. Only
   *  defined when at least one role is configured to consult a hivemind
   *  and the id resolves against `listHiveminds`. */
  hivemindLabel?: string;
  /** Audit 2.2: count of features the crash reconciler promoted to
   *  `Failed { interrupted: true, resumable: true }` after this swarm
   *  was found in an in-flight state on disk at app startup. Set by
   *  `SwarmsScreen` from `swarm_reconciled` event payloads plus a
   *  fallback count of `features.json[?].interrupted` for swarms that
   *  arrive via the 5s poll instead of the startup event. */
  interruptedCount?: number;
  /** Callback to refresh swarms list after a successful Resume. */
  onResume?: () => void;
  go: GoFn;
  showProject?: boolean;
  onDelete?: () => void;
}

function SwarmCard({
  sw,
  go,
  showProject = false,
  onDelete,
  rawState,
  details,
  hivemindLabel,
  interruptedCount,
  onResume,
}: SwarmCardProps) {
  const done = details?.done ?? 0;
  const total = details?.total ?? 0;
  const pct = total > 0 ? Math.round((done / total) * 100) : 0;
  const { localTasks, createTask, setActiveTask } = useTaskRuntime();
  const [cloneError, setCloneError] = useState<string | null>(null);
  const [cloning, setCloning] = useState(false);
  const [resuming, setResuming] = useState(false);
  const [resumeError, setResumeError] = useState<string | null>(null);
  const toast = useErrorToast();

  /** Generic Resume callback used by both:
   *  - paused-with-isInterrupted (audit 2.2 crash-recovery flow), and
   *  - failed/cancelled swarms (terminal-failure full retry).
   *  `resume_swarm` rehydrates features.json from disk, resets the
   *  appropriate set of features (in-flight, crash-interrupted, and on
   *  the failed/cancelled path, also terminal-failed) back to Pending
   *  with a fresh `fix_attempt_count`, and spawns a fresh queen. */
  const handleResume = useCallback(async () => {
    if (!isTauri()) return;
    setResumeError(null);
    setResuming(true);
    try {
      await resumeSwarm(sw.id);
      onResume?.();
      go("swarm-control", { swarm: sw });
    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e);
      setResumeError(msg);
      toast.error(`Failed to resume swarm "${sw.name}"`, e);
    } finally {
      setResuming(false);
    }
  }, [sw, onResume, toast, go]);

  const isInterrupted = rawState?.status === "interrupted";

  // Live-ticking duration: uses real-time Date.now()-created_at for active
  // swarms. Freezes at updated_at-created_at for paused/completed/failed/etc.
  // so the duration doesn't creep on poll-induced re-renders.
  //
  // Both rawState?.status and sw.status are in deps because sw.status is
  // derived from rawState?.status via swarmStateToSwarm. A single status
  // change may cause the effect to fire twice; the second run is a no-op
  // because the guard condition is already stable.
  const [, forceTick] = useState(0);
  useEffect(() => {
    const isLive = isLiveSwarmStatus(rawState?.status ?? sw.status);
    if (!isLive || !rawState?.created_at) return;
    const id = setInterval(() => forceTick((n) => n + 1), 1000);
    return () => clearInterval(id);
  }, [rawState?.status, sw.status, rawState?.created_at]);
  // rawState is undefined on the mock-data path — skip live ticking
  // and fall back to persisted usage.duration_ms.
  const elapsedMs = rawState?.created_at
    ? swarmElapsedMs({
        status: rawState.status,
        createdAt: rawState.created_at,
        updatedAt: rawState.updated_at,
        fallbackMs: details?.usage?.duration_ms,
      })
    : (details?.usage?.duration_ms ?? 0);
  const duration = formatElapsed(elapsedMs);
  const cost = formatCost(details?.usage?.cost ?? 0);
  const activity = rawState?.current_phase ?? "\u2014";

  // Fetch this swarm's persisted features + milestones, then navigate to the
  // new-swarm form pre-loaded with the cloned plan. The new form skips Queen
  // planning entirely and jumps straight to Scout/Worker execution on submit.
  const handleClonePlan = useCallback(async () => {
    setCloneError(null);
    if (!isTauri()) {
      setCloneError("Clone is only available in the desktop app.");
      return;
    }
    setCloning(true);
    try {
      const [features, milestones] = await Promise.all([
        getSwarmFeatures(sw.id),
        getSwarmMilestones(sw.id),
      ]);
      if (!features.length) {
        setCloneError(
          "This swarm has no plan to clone — it may have never reached execution.",
        );
        return;
      }
      go("new-swarm", {
        clonedPlan: {
          features,
          milestones,
          sourceSwarmId: sw.id,
          sourceSwarmName: sw.name,
        },
      });
    } catch (e) {
      console.error("Failed to clone swarm plan:", e);
      setCloneError(e instanceof Error ? e.message : String(e));
    } finally {
      setCloning(false);
    }
  }, [sw.id, sw.name, go]);

  const canClonePlan =
    sw.status === "completed" ||
    sw.status === "failed" ||
    sw.status === "paused" ||
    sw.status === "running";

  /** Continue/Open the planning conversation for this swarm in the Tasks
   *  view. If a task already exists linked to this swarm, activate it;
   *  otherwise create one and link it. */
  const openPlanningTask = useCallback(() => {
    const existing = localTasks.find((t) => t.swarmId === sw.id);
    if (existing) {
      setActiveTask(existing.id);
    } else {
      // Legacy swarms (created before NewSwarm qualified its model ids)
      // store a bare model id like "mimo-v2.5-pro-precision" — Pi cannot
      // resolve a provider from that and the session dies on first send.
      // Only forward the swarm's queen model when it's already qualified;
      // otherwise let createTask fall through to the user's configured
      // default model from settings, which always carries a provider.
      const queenModel =
        typeof sw.queen === "string" && sw.queen.includes("/") ? sw.queen : undefined;
      // Forward the authoritative hivemind id from the backend state when
      // either Queen or Scout consults a hivemind; the legacy `sw.hivemind`
      // is the literal "none" on the live path after the adapter slim-down.
      const hivemindForTask =
        rawState && (rawState.model_settings.use_hivemind_on_queen ||
          rawState.model_settings.use_hivemind_on_scout)
          ? rawState.model_settings.hivemind_id ?? null
          : sw.hivemind && sw.hivemind !== "none"
            ? sw.hivemind
            : null;
      createTask({
        swarmId: sw.id,
        projectPath: sw.cwd,
        model: queenModel,
        hivemind: hivemindForTask,
        title: sw.name,
        description: `Swarm planning — ${sw.cwd}`,
        setActive: true,
      });
    }
    go("tasks");
  }, [localTasks, createTask, setActiveTask, sw, rawState, go]);

  const queen = sw.queen;
  const scout = sw.scout;
  const worker = sw.worker;
  const guard = sw.guard;

  const swarmProject = PROJECTS.find((p) => p.cwd === sw.cwd);

  const isActive = sw.status === "running" || sw.status === "paused";

  // Status-driven primary actions, rendered in the header right. Destructive
  // (Delete) and maintenance (Edit / Clone Plan) actions live in the footer.
  const primaryActions = (() => {
    switch (sw.status) {
      case "running":
        return (
          <>
            <Btn
              kind="primary"
              size="sm"
              icon={I.arrow({ size: 13 })}
              onClick={() => go("swarm-control", { swarm: sw })}
            >
              Open Control
            </Btn>
            <Btn kind="outline" size="sm" icon={I.pause({ size: 13 })}>
              Pause
            </Btn>
          </>
        );
      case "paused":
        // Audit 2.2: when the underlying status is `interrupted` (crash
        // recovery promoted features to Failed { interrupted: true }),
        // the Resume button calls `resume_swarm` directly so the user
        // can re-queue the interrupted features without navigating away.
        if (isInterrupted) {
          return (
            <>
              <Btn
                kind="primary"
                size="sm"
                icon={I.play({ size: 13 })}
                disabled={resuming}
                onClick={handleResume}
              >
                {resuming ? "Resuming…" : "Resume"}
              </Btn>
              <Btn
                kind="outline"
                size="sm"
                icon={I.arrow({ size: 13 })}
                onClick={() => go("swarm-control", { swarm: sw })}
              >
                Open Control
              </Btn>
            </>
          );
        }
        return (
          <>
            <Btn
              kind="primary"
              size="sm"
              icon={I.play({ size: 13 })}
              onClick={() => go("swarm-control", { swarm: sw })}
            >
              Resume
            </Btn>
            <Btn
              kind="outline"
              size="sm"
              icon={I.arrow({ size: 13 })}
              onClick={() => go("swarm-control", { swarm: sw })}
            >
              Open Control
            </Btn>
          </>
        );
      case "planning":
        return (
          <Btn
            kind="primary"
            size="sm"
            icon={I.crown({ size: 13 })}
            onClick={openPlanningTask}
          >
            Continue Planning
          </Btn>
        );
      case "completed":
        return (
          <>
            <Btn
              kind="primary"
              size="sm"
              icon={I.arrow({ size: 13 })}
              onClick={() => go("swarm-control", { swarm: sw })}
            >
              Open Control
            </Btn>
            <Btn kind="ghost" size="sm">
              Export
            </Btn>
          </>
        );
      case "failed":
        return (
          <>
            <Btn
              kind="primary"
              size="sm"
              icon={I.play({ size: 13 })}
              disabled={resuming}
              onClick={handleResume}
            >
              {resuming ? "Resuming…" : "Resume"}
            </Btn>
            <Btn
              kind="outline"
              size="sm"
              icon={I.arrow({ size: 13 })}
              onClick={() => go("swarm-control", { swarm: sw })}
            >
              Open Control
            </Btn>
          </>
        );
    }
  })();

  return (
    <div className="bg-ink-800 border border-line rounded-xl p-4 card-hover">
      {/* HEADER ROW — title + project + cwd + primary actions */}
      <div className="flex items-start justify-between gap-4">
        <div className="flex-1 min-w-0 flex items-start gap-3">
          <div className="shrink-0 pt-0.5">
            <StatusBadge status={sw.status} />
          </div>
          <div className="flex-1 min-w-0">
            <div className="flex items-center gap-3 flex-wrap">
              <h3 className="text-[16px] font-bold text-white">{sw.name}</h3>
              {showProject && swarmProject && (
                <span className="inline-flex items-center gap-1.5 h-5 px-1.5 rounded bg-ink-700/70 border border-line text-[10.5px]">
                  {I.folder({ size: 10, className: "text-honey-400" })}
                  <span className="text-dim">{swarmProject.org}</span>
                  <span className="text-line">/</span>
                  <span className="text-white/85 font-medium">
                    {swarmProject.name}
                  </span>
                </span>
              )}
              <span className="font-mono text-[11.5px] text-dim truncate">
                {sw.cwd}
              </span>
            </div>
          </div>
        </div>
        <div className="flex items-center gap-1.5 shrink-0">
          {primaryActions}
        </div>
      </div>

      {/* STATS ROW — wrap, tabular-nums for alignment across cards */}
      <div className="mt-2 flex items-center gap-x-4 gap-y-2 flex-wrap text-[12px] text-muted">
        <span className="flex items-center gap-1.5">
          {I.clock({ size: 12, className: "text-dim" })}
          <span className="font-mono text-white/80 tabular-nums">{duration}</span>
        </span>
        <span className="flex items-center gap-1.5">
          {I.cost({ size: 12, className: "text-dim" })}
          <span className="font-mono text-white/80 tabular-nums">{cost}</span>
        </span>
        <span className="flex items-center gap-1.5">
          {I.list({ size: 12, className: "text-dim" })}
          <span className="font-mono text-white/80 tabular-nums">
            {done}/{total}
          </span>{" "}
          features
        </span>
        <span className="flex items-center gap-1.5">
          {I.crown({ size: 12, className: "text-dim" })}
          <span className="text-white/80">{activity}</span>
        </span>
        <span className="flex items-center gap-1.5">
          {I.hexFill({ size: 11, className: "text-honey-500" })}
          <span className="font-mono text-honey-300">{hivemindLabel ?? "none"}</span>
        </span>
      </div>

      {/* PROGRESS */}
      <div className="mt-3 flex items-center gap-3">
        <div className="flex-1 h-1.5 bg-ink-900 rounded-full overflow-hidden">
          <div
            className="h-full rounded-full bg-emerald-500"
            style={{ width: `${pct}%` }}
          />
        </div>
        <span className="text-[11px] text-dim font-mono tabular-nums w-10 text-right">
          {pct}%
        </span>
      </div>

      {/* ROLE CHIPS */}
      <div className="mt-3 grid grid-cols-2 gap-x-3 gap-y-1.5 text-[11.5px] text-dim">
        <RoleChip role="queen" value={queen} hivemind={hivemindLabel} />
        <RoleChip role="scout" value={scout} hivemind={hivemindLabel} />
        <RoleChip role="worker" value={worker} />
        <RoleChip role="guard" value={guard ?? ""} />
      </div>

      {/* ERROR */}
      {sw.error && (
        <div className="mt-3 text-[11.5px] text-red-300/90 flex items-start gap-1.5">
          <span className="text-red-400 mt-[1px]">⚠</span>
          <span className="leading-snug">{sw.error}</span>
        </div>
      )}

      {/* AUDIT 2.2 — interrupted-by-restart badge */}
      {isInterrupted && (interruptedCount ?? 0) > 0 && (
        <div className="mt-3 text-[11.5px] text-honey-300 flex items-start gap-1.5">
          <span className="text-honey-400 mt-[1px]">⟳</span>
          <span className="leading-snug">
            {interruptedCount === 1
              ? "1 feature was interrupted by an app restart. Click Resume to re-queue it."
              : `${interruptedCount} features were interrupted by an app restart. Click Resume to re-queue them.`}
          </span>
        </div>
      )}

      {resumeError && (
        <div className="mt-2 text-[11px] text-red-300/90 leading-snug">
          Resume failed: {resumeError}
        </div>
      )}

      {/* FOOTER — secondary actions (left) and destructive Delete (right) */}
      <div
        data-testid="swarm-card-footer"
        className="mt-3 pt-3 border-t border-line flex items-center justify-between"
      >
        <div className="flex gap-1.5">
          <Btn
            kind="outline"
            size="sm"
            icon={I.edit({ size: 13 })}
            onClick={() => go("new-swarm", { swarm: rawState ?? sw, edit: true })}
          >
            Edit Swarm
          </Btn>
          {canClonePlan && (
            <Btn
              kind="ghost"
              size="sm"
              icon={I.copy({ size: 13 })}
              onClick={handleClonePlan}
              disabled={cloning}
            >
              {cloning ? "Loading\u2026" : "Clone Plan"}
            </Btn>
          )}
        </div>
        <DeleteSwarmButton swarm={sw} isActive={isActive} onDelete={onDelete} />
      </div>

      {cloneError && (
        <div className="mt-2 text-[11px] text-red-300/90 leading-snug">
          {cloneError}
        </div>
      )}
    </div>
  );
}

/* ── SwarmsScreen ─────────────────────────────────────────── */

export function SwarmsScreen({ go }: { go: GoFn }) {
  const { project } = useProject();
  const toast = useErrorToast();
  const [filter, setFilter] = useState("all");
  const [swarms, setSwarms] = useState<Swarm[]>(isTauri() ? [] : SWARMS);
  /* ── Draft banner state ────────────────────────────────── */
  const [draftBanner, setDraftBanner] = useState<{ name: string; savedAt: number } | null>(null);
  // Authoritative backend state keyed by swarm id, captured at fetch time
  // so the Edit button can pass the raw `SwarmState` to NewSwarmScreen
  // instead of the lossy mock `Swarm` shape.
  const [rawStates, setRawStates] = useState<Record<string, SwarmState>>({});
  // Per-swarm details (feature counts + usage) keyed by swarm id. Filled
  // by a fan-out of `getSwarmFeatures` + `getSwarmUsage` after `listSwarms`
  // resolves. Each call is `Promise.allSettled` so one swarm's failure
  // can't blank the rest of the list.
  const [swarmDetails, setSwarmDetails] = useState<
    Record<string, { done: number; total: number; usage: SwarmUsageSummary | null }>
  >({});
  // Audit 2.2: per-swarm count of features the crash reconciler marked
  // `Failed { interrupted: true }`. Sourced from two places:
  // (a) the `swarm_reconciled` Tauri event fired once at app startup,
  //     which is authoritative for swarms reconciled this session.
  // (b) the periodic feature-fetch below, which also tallies any feature
  //     with `interrupted === true` so a swarm that arrived via the 5s
  //     poll (e.g. focus came back after a window-hide) is still shown
  //     with the badge.
  const [interruptedCounts, setInterruptedCounts] = useState<Record<string, number>>({});
  // Resolved hivemind names keyed by hivemind id. Refetched on mount,
  // swarm-event, and tab focus — not on the 5s poll (wasteful).
  const [hivemindNames, setHivemindNames] = useState<Record<string, string>>({});
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const filters = [
    "all",
    "running",
    "planning",
    "paused",
    "completed",
    "failed",
  ];

  const fetchSwarms = useCallback(async () => {
    if (!isTauri()) return;
    try {
      const states = await listSwarms();
      setSwarms(states.map(swarmStateToSwarm));
      const map: Record<string, SwarmState> = {};
      for (const st of states) map[st.id] = st;
      setRawStates(map);
      setError(null);

      // Fan-out: per-swarm feature counts + usage. Independent settles so one
      // bad swarm can't blank the others. N×2 IPC per poll — fine for local
      // disk reads; flag if a user reports lag at ≥30 swarms.
      const next: Record<string, { done: number; total: number; usage: SwarmUsageSummary | null }> = {};
      const nextInterrupted: Record<string, number> = {};
      await Promise.allSettled(
        states.map(async (s) => {
          const [featRes, usageRes] = await Promise.allSettled([
            getSwarmFeatures(s.id),
            getSwarmUsage(s.id),
          ]);
          const features = featRes.status === "fulfilled" ? featRes.value : [];
          const usage = usageRes.status === "fulfilled" ? usageRes.value : null;
          const done = features.filter((f) => f.status === "completed").length;
          const total = features.length;
          next[s.id] = { done, total, usage };
          // Audit 2.2: tally features the reconciler marked interrupted.
          const interrupted = features.filter((f) => f.interrupted === true).length;
          if (interrupted > 0) nextInterrupted[s.id] = interrupted;
        }),
      );
      setSwarmDetails(next);
      setInterruptedCounts(nextInterrupted);
    } catch (e) {
      console.error("Failed to load swarms:", e);
      setError(e instanceof Error ? e.message : String(e));
    }
  }, []);

  // Fetch hivemind names — used to resolve `model_settings.hivemind_id`
  // into a human-readable label on each card. Falls back silently on
  // error; the card will show a truncated UUID instead. We surface the
  // failure via a toast so non-developers don't see the swarm cards
  // showing raw UUIDs with no explanation.
  const fetchHivemindNames = useCallback(async () => {
    if (!isTauri()) return;
    try {
      const hms = await listHiveminds();
      const names: Record<string, string> = {};
      for (const h of hms) names[h.id] = h.name;
      setHivemindNames(names);
    } catch (e) {
      toast.error("Failed to load hivemind names", e);
    }
    // toast.error is a stable memoised callback from ToastProvider.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  useEffect(() => {
    if (!isTauri()) return;
    setLoading(true);
    fetchSwarms().finally(() => setLoading(false));
    fetchHivemindNames();

    // Poll every 5s as backup
    const interval = setInterval(fetchSwarms, 5000);

    // Listen to swarm events to trigger refresh
    let unlisten: (() => void) | undefined;
    let unlistenReconciled: (() => void) | undefined;
    let mounted = true;
    onSwarmEvent((evt) => {
      if (!mounted) return;
      // Play completion sound on swarm completed (not on failed)
      if (evt.event_type === "completed") {
        const cs = getCompletionSoundConfig();
        if (cs.enabled) {
          playCompletionSound(cs.sound);
        }
      }
      fetchSwarms();
      // Refresh hivemind labels on swarm-event so renamed hiveminds
      // propagate without waiting for tab-focus.
      fetchHivemindNames();
    }).then((fn) => {
      if (mounted) unlisten = fn;
      else safeUnlisten(fn);
    });

    // Audit 2.2: subscribe to the one-shot `swarm_reconciled` startup
    // event so the Resume badge shows immediately, not only after the
    // first 5s poll. Seed `interruptedCounts` from the event payload so
    // the badge can render before the feature-fetch fan-out completes.
    onSwarmReconciled((evt) => {
      if (!mounted) return;
      setInterruptedCounts((prev) => ({
        ...prev,
        [evt.swarm_id]: evt.interrupted_features.length,
      }));
      // Also kick a fetch so the swarm card status flips to `paused`
      // (interrupted maps to paused in the UI adapter).
      fetchSwarms();
    }).then((fn) => {
      if (mounted) unlistenReconciled = fn;
      else safeUnlisten(fn);
    });

    // Refresh hivemind labels when the tab regains focus so a rename in
    // another view shows up next time the user comes back.
    const onVis = () => {
      if (document.visibilityState === "visible") fetchHivemindNames();
    };
    document.addEventListener("visibilitychange", onVis);

    return () => {
      clearInterval(interval);
      mounted = false;
      safeUnlisten(unlisten);
      safeUnlisten(unlistenReconciled);
      document.removeEventListener("visibilitychange", onVis);
    };
  }, [fetchSwarms, fetchHivemindNames]);

  /* ── Check for draft on mount ───────────────────────────── */
  useEffect(() => {
    try {
      const raw = localStorage.getItem(SWARM_DRAFT_KEY);
      if (!raw) return;
      const draft = JSON.parse(raw);
      if (draft && draft.savedAt && (draft.name || draft.roleModels)) {
        setDraftBanner({ name: draft.name || "", savedAt: draft.savedAt });
      }
    } catch {
      /* ignore malformed draft */
    }
  }, []);

  const baseList = swarms;
  const list = baseList.filter(
    (s) => filter === "all" || s.status === filter
  );

  return (
    <div className="h-full overflow-auto">
      <div className="max-w-[1400px] mx-auto px-8 py-7">
        <div className="flex items-end justify-between mb-6">
          <div>
            <h1 className="text-[28px] font-bold tracking-tight">Swarms</h1>
            <p className="text-muted text-[13.5px] mt-1 max-w-xl">
              Long-running coding sessions across every project. A Queen plans,
              Workers build, Scout specs features, Hivemind reviews each pass.
            </p>
          </div>
          <Btn
            kind="primary"
            icon={I.plus({ size: 14 })}
            onClick={() => go("new-swarm")}
          >
            New Swarm
          </Btn>
        </div>

        {/* Draft resume banner */}
        {draftBanner && (
          <div className="mb-4 rounded-lg border border-honey-500/40 bg-honey-500/10 px-4 py-3 flex items-center justify-between gap-4">
            <div className="flex items-center gap-2.5 text-[13px] text-honey-200">
              {I.edit({ size: 15, className: "text-honey-400 shrink-0" })}
              <span>
                You have an unsaved swarm draft{" "}
                {draftBanner.name && (
                  <>
                    (<span className="font-mono">{draftBanner.name}</span>){" "}
                  </>
                )}
                from{" "}
                {(() => {
                  const diff = Date.now() - draftBanner.savedAt;
                  const mins = Math.round(diff / 60000);
                  if (mins < 2) return "a moment ago";
                  if (mins < 60) return `${mins} minutes ago`;
                  const hrs = Math.floor(mins / 60);
                  if (hrs === 1) return "1 hour ago";
                  return `${hrs} hours ago`;
                })()}
              </span>
            </div>
            <div className="flex items-center gap-2 shrink-0">
              <Btn
                kind="primary"
                size="sm"
                icon={I.edit({ size: 13 })}
                onClick={() => go("new-swarm")}
              >
                Continue editing
              </Btn>
              <Btn
                kind="ghost"
                size="sm"
                onClick={() => {
                  localStorage.removeItem(SWARM_DRAFT_KEY);
                  setDraftBanner(null);
                }}
              >
                Discard draft
              </Btn>
            </div>
          </div>
        )}

        {/* Stats strip */}
        <div className="grid grid-cols-5 gap-3 mb-5">
          {[
            {
              l: "Active",
              v: baseList.filter(
                (s) => s.status === "running" || s.status === "planning"
              ).length,
              c: "text-emerald-300",
            },
            {
              l: "Paused",
              v: baseList.filter((s) => s.status === "paused").length,
              c: "text-honey-300",
            },
            { l: "Completed (7d)", v: baseList.filter((s) => s.status === "completed").length, c: "text-white" },
            {
              l: "Failed (7d)",
              v: baseList.filter((s) => s.status === "failed").length,
              c: "text-red-300",
            },
            { l: "Total", v: baseList.length, c: "text-honey-300" },
          ].map((s, i) => (
            <div
              key={i}
              className="bg-ink-800 border border-line rounded-lg px-4 py-3"
            >
              <div className="text-[11px] uppercase tracking-wider text-muted">
                {s.l}
              </div>
              <div
                className={`text-[22px] font-semibold mt-1 tabular-nums ${s.c}`}
              >
                {s.v}
              </div>
            </div>
          ))}
        </div>

        {/* Filters */}
        <div className="flex items-center gap-1.5 mb-4">
          {filters.map((f) => (
            <button
              key={f}
              onClick={() => setFilter(f)}
              className={`h-7 px-3 rounded-full text-[11.5px] font-medium border ${
                filter === f
                  ? "bg-honey-500/15 text-honey-300 border-honey-500/40"
                  : "bg-ink-850 text-muted border-line hover:text-white"
              }`}
            >
              {f}
              {f !== "all" && (
                <span className="ml-1.5 text-dim">
                  {baseList.filter((s) => s.status === f).length}
                </span>
              )}
            </button>
          ))}
        </div>

        {/* Loading / Error */}
        {loading && (
          <div className="text-[12px] text-dim mb-3 flex items-center gap-2">
            <span className="w-3 h-3 border-2 border-honey-400 border-t-transparent rounded-full animate-spin" />
            Loading swarms...
          </div>
        )}
        {error && (
          <div className="text-[12px] text-red-400 mb-3">
            Failed to load swarms: {error}
          </div>
        )}

        {/* Cards */}
        {list.length === 0 ? (
          <div className="border border-dashed border-line rounded-xl px-6 py-12 text-center">
            <div className="inline-flex items-center justify-center w-12 h-12 rounded-full bg-ink-800 border border-line mb-3">
              {I.swarm({ size: 20, className: "text-dim" })}
            </div>
            <div className="text-[14.5px] font-semibold text-white">
              No swarms in{" "}
              <span className="font-mono text-honey-300">
                {project?.org}/{project?.name}
              </span>{" "}
              yet
            </div>
            <div className="text-[12.5px] text-muted mt-1 mb-4">
              Start a swarm to plan and ship features in this project.
            </div>
            <div className="flex items-center justify-center gap-2">
              <Btn
                kind="primary"
                size="sm"
                icon={I.plus({ size: 13 })}
                onClick={() => go("new-swarm")}
              >
                New Swarm
              </Btn>
            </div>
          </div>
        ) : (
          <div className="space-y-3">
            {list.map((sw) => {
              const rs = rawStates[sw.id];
              const hivemindEnabled =
                !!rs &&
                (rs.model_settings.use_hivemind_on_queen ||
                  rs.model_settings.use_hivemind_on_scout);
              const hivemindId = rs?.model_settings.hivemind_id;
              const hivemindLabel =
                hivemindEnabled && hivemindId
                  ? hivemindNames[hivemindId] ?? hivemindId.slice(0, 8) + "\u2026"
                  : undefined;
              return (
                <SwarmCard
                  key={sw.id}
                  sw={sw}
                  go={go}
                  showProject={true}
                  onDelete={fetchSwarms}
                  rawState={rs}
                  details={swarmDetails[sw.id]}
                  hivemindLabel={hivemindLabel}
                  interruptedCount={interruptedCounts[sw.id]}
                  onResume={fetchSwarms}
                />
              );
            })}
          </div>
        )}
      </div>
    </div>
  );
}
