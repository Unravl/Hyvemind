import React, { useState, useRef, useLayoutEffect, useEffect, useCallback, useMemo } from "react";
import { GoFn } from "../App";
import { I } from "../components/icons";
import { Btn, StatusBadge, Pill } from "../components/atoms";
import { SWARMS, Swarm, MODELS } from "../data/mock";
import { isTauri } from "../lib/tauri";
import {
  SHOW_REASONING_KEY,
  SHOW_TOOL_CALLS_KEY,
  loadShowReasoning,
  loadShowToolCalls,
  parseCtx2,
} from "../lib/uiPrefs";
import { ActivityStream } from "../components/ActivityStream";
import { ActivityFooter } from "../components/ActivityFooter";
import {
  getSwarm,
  getSwarmProgress,
  getSwarmFeatures,
  getSwarmUsage,
  getPiSessionStats,
  pauseSwarm,
  resumeSwarm,
  stopSwarm,
  SwarmUsageSummary,
  PiSessionStats,
} from "../lib/ipc";
import { onSwarmEvent, safeUnlisten } from "../lib/events";
import { NurseMessage } from "../components/NurseMessage";
import type { NurseEntry } from "../lib/streamEntry";
import type {
  SwarmState,
  ProgressEvent as BackendProgressEvent,
  Feature as BackendFeature,
  FeatureStatus,
  IssueSeverity,
} from "../lib/types";
import {
  getSwarmActivityState,
  subscribeSwarmActivity,
} from "../lib/swarmActivityStore";
import { toStreamEntries } from "../lib/swarmActivityReducer";
import type { ActiveSession } from "../lib/streamEntry";
import { useHivemindReviewState } from "../lib/hivemindEventStore";
import { HivemindReviewLivePanel } from "../components/HivemindReviewLivePanel";
import { MergedPlanModal } from "../components/MergedPlanModal";
import { useTaskRuntime } from "../lib/taskRuntime";
import { formatCost, formatTokens, formatElapsed, isLiveSwarmStatus, swarmElapsedMs } from "../lib/format";

/* ── FlushPanel (local, richer version for control grid) ──── */

interface FlushPanelProps {
  title?: React.ReactNode;
  right?: React.ReactNode;
  children?: React.ReactNode;
  className?: string;
  bodyClass?: string;
  dense?: boolean;
  style?: React.CSSProperties;
}

function FlushPanel({
  title,
  right,
  children,
  className = "",
  bodyClass = "",
  dense = false,
  style,
}: FlushPanelProps) {
  return (
    <div
      className={`bg-ink-800 flex flex-col min-h-0 ${className}`}
      style={style}
    >
      {(title || right) && (
        <div
          className={`flex items-center justify-between ${
            dense ? "h-9 px-3" : "h-10 px-4"
          } border-b border-line shrink-0`}
        >
          <div className="text-[12px] uppercase tracking-[.14em] text-muted font-semibold flex items-center gap-2">
            {title}
          </div>
          <div className="flex items-center gap-1.5">{right}</div>
        </div>
      )}
      <div className={`flex-1 min-h-0 ${bodyClass}`}>{children}</div>
    </div>
  );
}

/* ── Stat ─────────────────────────────────────────────────── */

interface StatProps {
  icon: React.ReactNode;
  label: string;
  value: string;
  tone?: "honey" | "amber" | "red";
}

function Stat({ icon, label, value, tone }: StatProps) {
  const iconClass =
    tone === "honey"
      ? "text-honey-400"
      : tone === "amber"
        ? "text-amber-400"
        : tone === "red"
          ? "text-red-400"
          : "text-dim";
  const valueClass =
    tone === "honey"
      ? "text-honey-300"
      : tone === "amber"
        ? "text-amber-300"
        : tone === "red"
          ? "text-red-300"
          : "text-white";
  return (
    <div className="flex items-baseline gap-1.5 whitespace-nowrap">
      <span className={`self-center ${iconClass}`}>{icon}</span>
      <span className="text-dim text-[10px] uppercase tracking-wider font-semibold">
        {label}
      </span>
      <span className={`font-mono text-[11.5px] ${valueClass}`}>{value}</span>
    </div>
  );
}

/* ── Meta ─────────────────────────────────────────────────── */

interface MetaProps {
  label: string;
  value: string;
  mono?: boolean;
}

function Meta({ label, value, mono }: MetaProps) {
  return (
    <div className="min-w-0">
      <div className="text-[10.5px] uppercase tracking-wider text-muted font-semibold">
        {label}
      </div>
      <div
        className={`text-[12.5px] mt-0.5 truncate ${
          mono ? "font-mono text-white/85" : "text-white/85"
        }`}
      >
        {value}
      </div>
    </div>
  );
}

/* ── MetaChip ─────────────────────────────────────────────── */

type MetaChipTone = "neutral" | "honey" | "amber" | "red" | "green" | "mono";

interface MetaChipProps {
  icon?: React.ReactNode;
  label: string;
  tone?: MetaChipTone;
  title?: string;
  dotColor?: string;
}

const metaChipTone: Record<MetaChipTone, string> = {
  neutral: "bg-ink-600 text-muted",
  honey: "bg-honey-500/10 text-honey-400",
  amber: "bg-amber-500/10 text-amber-300",
  red: "bg-red-500/10 text-red-400",
  green: "bg-green-500/10 text-green-400",
  mono: "bg-ink-700 text-slate-300 font-mono",
};

function MetaChip({
  icon,
  label,
  tone = "neutral",
  title,
  dotColor,
}: MetaChipProps) {
  return (
    <span
      title={title}
      className={`inline-flex items-center gap-1.5 h-7 px-2.5 rounded-md text-[11.5px] font-medium ${metaChipTone[tone]}`}
    >
      {dotColor ? (
        <span className={`w-1.5 h-1.5 rounded-full ${dotColor}`} />
      ) : icon ? (
        <span className="inline-flex items-center">{icon}</span>
      ) : null}
      <span className="truncate">{label}</span>
    </span>
  );
}

/* Map feature status → MetaChip tone + dot colour class. Mirrors the colour
 * language used by StatusBadge but rendered as a chip with a coloured dot
 * prefix instead of a rounded-full pill. */
function statusMeta(
  s: FeatureStatus | undefined,
): { tone: MetaChipTone; dot: string } {
  switch (s) {
    case "completed":
      return { tone: "green", dot: "bg-green-400" };
    case "failed":
      return { tone: "red", dot: "bg-red-400" };
    case "skipped":
      return { tone: "mono", dot: "bg-slate-400" };
    case "scouting":
    case "implementing":
    case "reviewing":
    case "validating":
      return { tone: "honey", dot: "bg-honey-400" };
    case "pending":
    default:
      return { tone: "neutral", dot: "bg-neutral-400" };
  }
}

/* ── Pager ────────────────────────────────────────────────── */

interface PagerProps {
  page: number;
  pages: number;
  setPage: (p: number) => void;
}

function Pager({ page, pages, setPage }: PagerProps) {
  return (
    <span className="inline-flex items-center gap-1 text-[11px] font-mono normal-case tracking-normal">
      <button
        onClick={() => setPage(Math.max(0, page - 1))}
        disabled={page === 0}
        className="w-5 h-5 inline-flex items-center justify-center rounded border border-line text-muted hover:text-white hover:border-line-strong disabled:opacity-30 disabled:hover:text-muted disabled:hover:border-line"
      >
        {"‹"}
      </button>
      <span className="text-muted tabular-nums">
        {page + 1}/{pages}
      </span>
      <button
        onClick={() => setPage(Math.min(pages - 1, page + 1))}
        disabled={page >= pages - 1}
        className="w-5 h-5 inline-flex items-center justify-center rounded border border-line text-muted hover:text-white hover:border-line-strong disabled:opacity-30 disabled:hover:text-muted disabled:hover:border-line"
      >
        {"›"}
      </button>
    </span>
  );
}

/* Feature status → display bucket used by the Tasks list. Pending / active
 * (anything in-flight) / done buckets correspond directly to the existing
 * mock visual states. Failed/skipped get their own pill rendering. */
type DisplayStatus = "pending" | "active" | "done" | "failed" | "skipped";

function displayStatus(s: FeatureStatus): DisplayStatus {
  switch (s) {
    case "completed":
      return "done";
    case "failed":
      return "failed";
    case "skipped":
      return "skipped";
    case "scouting":
    case "implementing":
    case "reviewing":
    case "validating":
      return "active";
    default:
      return "pending";
  }
}

/* Adapter: backend SwarmState → the Swarm shape used by the existing
 * top-bar / status-badge rendering. Only the fields the top bar reads are
 * meaningful; per-feature counts and cost come from real backend data
 * computed downstream.
 */
const swarmStateToSwarm = (s: SwarmState, feats: BackendFeature[]): Swarm => {
  const done = feats.filter((f) => f.status === "completed").length;
  return {
    id: s.id,
    name: s.name,
    status:
      s.status === "implementing"
        ? "running"
        : s.status === "interrupted"
          ? "paused"
          : s.status === "cancelled"
            ? "failed"
            : (s.status as Swarm["status"]),
    duration: "",
    cost: "",
    features: [done, feats.length],
    milestone: "",
    queen: s.model_settings.primary_model,
    worker: s.model_settings.primary_model,
    scout: s.model_settings.scout_model,
    hivemind: s.model_settings.hivemind_id || "none",
    cwd: s.working_directory,
  };
};

/* ── SwarmControlScreen ───────────────────────────────────── */

export function SwarmControlScreen({ go, swarm: swarmProp }: { go: GoFn; swarm?: any }) {
  const fallback = swarmProp || (isTauri() ? null : SWARMS[0]);
  const [sw, setSw] = useState<Swarm | null>(fallback);
  const [backendState, setBackendState] = useState<SwarmState | null>(null);
  const [features, setFeatures] = useState<BackendFeature[]>([]);
  const [usage, setUsage] = useState<SwarmUsageSummary | null>(null);
  const [backendProgressLog, setBackendProgressLog] = useState<BackendProgressEvent[]>([]);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [actionLoading, setActionLoading] = useState<string | null>(null);
  // Brief visual confirmation when the swarm-id chip is clicked. Clears
  // ~1.2s after each copy so it never sticks.
  const [idCopied, setIdCopied] = useState(false);
  const [, forceTick] = useState(0);

  // Toggles for the bottom context bar. Shared with Tasks via localStorage
  // keys in `lib/uiPrefs`, so flipping in either screen affects the other.
  const [showReasoning, setShowReasoning] = useState(loadShowReasoning);
  const [showToolCalls, setShowToolCalls] = useState(loadShowToolCalls);
  const [activeSession, setActiveSession] = useState<ActiveSession>({
    sessionId: null,
    model: null,
    agent: null,
  });
  const handleActiveSessionChange = useCallback((next: ActiveSession | null) => {
    setActiveSession(next ?? { sessionId: null, model: null, agent: null });
  }, []);
  // Live token stats for the currently-active Pi session. Polled every 2s
  // from `get_pi_session_stats`; reset to `null` the instant
  // `activeSession.sessionId` changes so the bottom bar never shows stale
  // numbers from the previous agent.
  const [activeSessionStats, setActiveSessionStats] = useState<PiSessionStats | null>(null);
  // Single screen-level merged-plan modal slot, shared by the queen and
  // scout review panels. The user can only click one at a time, so the
  // most recent click wins. Hoisted here so the modal's lifetime is
  // decoupled from the review panels' own mount/unmount cycles.
  const [activeMergedPlan, setActiveMergedPlan] = useState<
    { round: number; text: string; subtitle?: string } | null
  >(null);
  const swarmId: string = swarmProp?.id || fallback?.id || "";

  const fetchSwarm = useCallback(async () => {
    if (!isTauri()) return;
    try {
      const [state, feats] = await Promise.all([
        getSwarm(swarmId),
        getSwarmFeatures(swarmId).catch(() => [] as BackendFeature[]),
      ]);
      setBackendState(state);
      setFeatures(feats);
      setSw(swarmStateToSwarm(state, feats));
      setError(null);
    } catch (e) {
      console.error("Failed to load swarm:", e);
      setError(e instanceof Error ? e.message : String(e));
    }
  }, [swarmId]);

  const fetchProgressLog = useCallback(async () => {
    if (!isTauri() || !swarmId) return;
    try {
      const log = await getSwarmProgress(swarmId);
      setBackendProgressLog(log);
    } catch {
      // Progress log may not exist yet — first launch hasn't started writing.
    }
  }, [swarmId]);

  const fetchUsage = useCallback(async () => {
    if (!isTauri() || !swarmId) return;
    try {
      setUsage(await getSwarmUsage(swarmId));
    } catch {
      // Usage table may not be populated until a Pi session reports stats.
    }
  }, [swarmId]);

  useEffect(() => {
    if (!isTauri()) return;
    setLoading(true);
    Promise.all([fetchSwarm(), fetchProgressLog(), fetchUsage()]).finally(() => setLoading(false));

    // Poll every 3s as a backup. The event listener below will trigger
    // refreshes on backend pushes; the polling catches state changes that
    // happen without an event (e.g. status flips during teardown).
    const interval = setInterval(() => {
      fetchSwarm();
      fetchProgressLog();
      fetchUsage();
    }, 3000);

    // Debounce swarm-event-triggered refreshes. A streaming swarm pushes
    // many events per second; without coalescing, each one triggered three
    // IPC round-trips (fetchSwarm + fetchProgressLog + fetchUsage), which
    // compounded with the activity stream's own load to starve the webview.
    // 250ms is fast enough to feel live and slow enough to absorb bursts.
    let refetchTimer: ReturnType<typeof setTimeout> | null = null;
    const scheduleRefetch = () => {
      if (refetchTimer !== null) return;
      refetchTimer = setTimeout(() => {
        refetchTimer = null;
        fetchSwarm();
        fetchProgressLog();
        fetchUsage();
      }, 250);
    };

    // Listen to swarm events filtered by swarm_id.
    let unlisten: (() => void) | undefined;
    let mounted = true;
    onSwarmEvent((evt) => {
      if (!mounted) return;
      if (evt.swarm_id === swarmId) {
        scheduleRefetch();
      }
    }).then((fn) => {
      if (mounted) unlisten = fn;
      else safeUnlisten(fn);
    });

    // Hivemind-progress events are consumed via the singleton store
    // (`useHivemindReviewState` below) so this effect no longer
    // registers its own listener for that channel.

    return () => {
      clearInterval(interval);
      if (refetchTimer !== null) clearTimeout(refetchTimer);
      mounted = false;
      safeUnlisten(unlisten);
    };
  }, [fetchSwarm, fetchProgressLog, fetchUsage, swarmId]);

  // Separate effect for the 1Hz elapsed clock — avoids touching the
  // fetch/poll/event infrastructure when status changes.
  // If backendState transitions from a live status to undefined/null,
  // isLiveSwarmStatus returns false and the interval is cleanly cleared.
  // The next poll that restores backendState will restart the clock.
  useEffect(() => {
    if (!isLiveSwarmStatus(backendState?.status)) return;
    const id = setInterval(() => forceTick((n) => n + 1), 1000);
    return () => clearInterval(id);
  }, [backendState?.status, backendState?.created_at]);

  // Live per-active-agent stats. Reset to null the moment `sessionId` changes
  // (new agent took over) so the bottom bar clears instead of showing the
  // previous agent's numbers; then poll every 2s while the session id holds.
  // When `sessionId` is null (no live agent) the effect is a no-op.
  useEffect(() => {
    setActiveSessionStats(null);
    const sid = activeSession.sessionId;
    if (!isTauri() || !sid) return;
    let cancelled = false;
    const poll = async () => {
      try {
        const stats = await getPiSessionStats(sid);
        if (!cancelled) setActiveSessionStats(stats);
      } catch {
        // Session evicted/torn-down — leave existing stats so we don't flicker.
      }
    };
    poll();
    const interval = setInterval(poll, 2000);
    return () => {
      cancelled = true;
      clearInterval(interval);
    };
  }, [activeSession.sessionId]);

  const handlePause = async () => {
    if (!isTauri()) return;
    setActionLoading("pause");
    try { await pauseSwarm(swarmId); await fetchSwarm(); } catch (e) { console.warn("Failed to pause swarm:", e); setError(String(e)); }
    setActionLoading(null);
  };

  const handleResume = async () => {
    if (!isTauri()) return;
    setActionLoading("resume");
    try { await resumeSwarm(swarmId); await fetchSwarm(); } catch (e) { console.warn("Failed to resume swarm:", e); setError(String(e)); }
    setActionLoading(null);
  };

  const handleStop = async () => {
    if (!isTauri()) return;
    setActionLoading("stop");
    try { await stopSwarm(swarmId); await fetchSwarm(); } catch (e) { console.warn("Failed to stop swarm:", e); setError(String(e)); }
    setActionLoading(null);
  };

  // Active feature: first non-terminal feature, falling back to first.
  const activeFeature = useMemo<BackendFeature | null>(() => {
    if (features.length === 0) return null;
    const inFlight = features.find((f) =>
      ["scouting", "implementing", "reviewing", "validating"].includes(f.status),
    );
    if (inFlight) return inFlight;
    const pending = features.find((f) => f.status === "pending");
    return pending || features[0];
  }, [features]);

  // History selection: when set, the Active-feature / Live-activity / Hivemind
  // panels show the picked feature's data instead of the auto-active one.
  // `null` means "live mode" — keep following the auto-detected active feature.
  const [selectedFeatureId, setSelectedFeatureId] = useState<string | null>(null);

  const displayFeature = useMemo<BackendFeature | null>(() => {
    if (selectedFeatureId) {
      const pinned = features.find((f) => f.id === selectedFeatureId);
      if (pinned) return pinned;
    }
    return activeFeature;
  }, [selectedFeatureId, features, activeFeature]);

  const isLiveMode =
    selectedFeatureId === null || selectedFeatureId === activeFeature?.id;

  // Per-swarm Hivemind review state sourced from the singleton store
  // (one global `hivemind-progress` listener shared across all
  // subscribers — see `hivemindEventStore.ts`). Subscribe to the
  // Queen's master-plan review and (when a feature is focused) that
  // feature's per-Scout review under the attribution keys produced by
  // `attributionKeyFromEvent`.
  const queenReview = useHivemindReviewState(`swarm:${swarmId}:queen`);
  const featReview = useHivemindReviewState(
    displayFeature ? `swarm:${swarmId}:feat:${displayFeature.id}` : "",
  );

  const handleSelectFeature = useCallback(
    (id: string) => {
      setSelectedFeatureId((prev) =>
        id === activeFeature?.id ? null : prev === id ? null : id,
      );
    },
    [activeFeature?.id],
  );

  // Phase pipeline state derived from the displayed feature's current status.
  // The feature stays in `scouting` while the optional Hivemind review on the
  // Scout's plan runs (queen.rs only flips it to `implementing` once the Worker
  // is dispatched), so we split `scoutActive` into:
  //   - scoutPiActive: the Scout Pi subprocess is producing tokens
  //   - scoutReviewActive: Scout finished, Hivemind is reviewing its plan
  // Both feed the umbrella `scoutActive` so the pipeline indicator stays lit
  // across the whole scout phase.
  const phaseState = useMemo(() => {
    const status = displayFeature?.status;
    const scoutDone = status
      ? !["pending", "scouting"].includes(status)
      : false;
    const scoutReviewActive =
      status === "scouting" && featReview?.status === "running";
    const scoutPiActive = status === "scouting" && !scoutReviewActive;
    const scoutActive = scoutPiActive || scoutReviewActive;
    const workerDone = status
      ? ["completed", "validating"].includes(status)
      : false;
    const workerActive = status === "implementing" || status === "reviewing";
    const guardDone = status === "completed";
    const guardActive = status === "validating";
    return {
      scoutDone,
      scoutActive,
      scoutPiActive,
      scoutReviewActive,
      workerDone,
      workerActive,
      guardDone,
      guardActive,
    };
  }, [displayFeature, featReview?.status]);


  // Derived stats: features done/total + percent.
  const done = features.filter((f) => f.status === "completed").length;
  const total = features.length;
  const pct = total > 0 ? Math.round((done / total) * 100) : 0;

  // Nurse interventions surfaced from the progress log. Each
  // `nurse_intervention` event carries a fully-populated lifecycle payload
  // in its metadata — we collapse all events with the same intervention_id
  // down to the most recent stage (so a `started → completed` pair only
  // produces one card, in its final state).
  const nurseInterventions = useMemo<(NurseEntry & { t: string })[]>(() => {
    const latest = new Map<string, NurseEntry & { t: string }>();
    for (const e of backendProgressLog) {
      if (e.event_type !== "nurse_intervention") continue;
      const meta = (e.metadata ?? {}) as Record<string, unknown>;
      const interventionId =
        typeof meta.intervention_id === "string" ? meta.intervention_id : e.timestamp;
      const status =
        typeof meta.status === "string"
          ? (meta.status as NurseEntry["status"])
          : "completed";
      const entry: NurseEntry & { t: string } = {
        kind: "nurse",
        surface: "swarm",
        id: interventionId,
        t: e.timestamp,
        interventionId,
        level: typeof meta.level === "string" ? meta.level : "steer",
        observation:
          typeof meta.observation === "string"
            ? meta.observation
            : e.message,
        action: typeof meta.action === "string" ? meta.action : "",
        reasoning:
          typeof meta.full_reasoning === "string"
            ? meta.full_reasoning
            : typeof meta.reasoning_delta === "string"
              ? meta.reasoning_delta
              : undefined,
        status,
        error: typeof meta.error === "string" ? meta.error : undefined,
        sessionId: typeof meta.session_id === "string" ? meta.session_id : undefined,
        featureId: e.feature_id,
      };
      latest.set(interventionId, entry);
    }
    return Array.from(latest.values()).sort((a, b) => a.t.localeCompare(b.t));
  }, [backendProgressLog]);

  // Progress log shaped for the side "Progress log" panel.
  // Phase 5C: `discovered_issue` events are rendered as colored chips in
  // the Active-feature panel — filter them out here so they don't show
  // up twice.
  const progressLog = useMemo(
    () =>
      backendProgressLog
        .filter((e) => e.event_type !== "discovered_issue")
        .map((e) => {
          const kind = e.event_type.includes("failed")
            ? "review"
            : e.event_type.includes("validated")
              ? "done"
              : e.event_type.includes("guard")
                ? "review"
                : e.event_type.includes("implemented")
                  ? "tool"
                  : "queen";
          return {
            t: new Date(e.timestamp).toLocaleTimeString([], {
              hour: "2-digit",
              minute: "2-digit",
            }),
            kind,
            msg: e.event_type.replace(/_/g, " "),
            detail: e.message,
          };
        })
        .reverse(),
    [backendProgressLog],
  );

  // Elapsed time from swarm's created_at; cost / tokens from per-swarm usage.
  // Freezes at updated_at-created_at when the swarm isn't actively running.
  const elapsedMs = swarmElapsedMs({
    status: backendState?.status,
    createdAt: backendState?.created_at,
    updatedAt: backendState?.updated_at,
  });
  const elapsedStr = formatElapsed(elapsedMs);
  const costStr = formatCost(usage?.cost ?? 0);
  const tokInStr = formatTokens(usage?.input_tokens ?? 0);
  const tokOutStr = formatTokens(usage?.output_tokens ?? 0);
  const tokCacheStr = formatTokens(
    (usage?.cache_read_tokens ?? 0) + (usage?.cache_write_tokens ?? 0),
  );

  // Phase 5A: surface the per-swarm budget cap and current spend ratio
  // when the swarm has an explicit cap. Amber at 80%, red at 100%.
  const swarmBudget =
    typeof backendState?.model_settings.swarm_budget_usd === "number"
      ? backendState.model_settings.swarm_budget_usd
      : null;
  const currentSpend = usage?.cost ?? 0;
  const budgetTone: "honey" | "amber" | "red" =
    swarmBudget == null || swarmBudget <= 0
      ? "honey"
      : currentSpend >= swarmBudget
        ? "red"
        : currentSpend >= swarmBudget * 0.8
          ? "amber"
          : "honey";
  const costDisplay = swarmBudget != null
    ? `${costStr} / ${formatCost(swarmBudget)}`
    : costStr;

  // Context-bar stats for the *active agent* — bottom bar reads from the
  // per-session live poll (`activeSessionStats`), so it shows the current
  // Pi session's in/out tokens and Context% against that model's window.
  // Falls back to the static MODELS catalog for the window size when Pi
  // hasn't reported it yet. t/s is suppressed when the swarm isn't
  // actively running.
  const ctxStats = useMemo(() => {
    const m = activeSession.model ?? backendState?.model_settings.primary_model ?? "";
    const bare = m.includes("/") ? m.split("/").slice(1).join("/") : m;
    const meta = MODELS.find((x) => x.id === bare);
    const liveWindow = activeSessionStats?.context_window ?? 0;
    const ctxNum = liveWindow > 0 ? liveWindow : meta ? parseCtx2(meta.ctx) : 200_000;
    const tokIn = activeSessionStats?.input ?? 0;
    const tokOut = activeSessionStats?.output ?? 0;
    const pct = activeSessionStats
      ? Math.min(100, Math.round(activeSessionStats.context_percent))
      : 0;
    const ctxLabel =
      ctxNum >= 1_000_000
        ? `${(ctxNum / 1_000_000).toFixed(0)}M`
        : `${Math.round(ctxNum / 1000)}k`;
    // Active-session t/s isn't reported by Pi today — leave it as 0 until
    // we plumb a streaming delta-based estimator. Don't reuse the swarm-wide
    // duration_ms here; it conflates phases.
    const tokPerSec = sw?.status === "running" && activeSessionStats ? 0 : 0;
    return { ctxNum, pct, ctxLabel, tokPerSec, tokIn, tokOut };
  }, [
    activeSession.model,
    backendState?.model_settings.primary_model,
    activeSessionStats,
    sw?.status,
  ]);

  const activeModelLabel =
    activeSession.model ?? backendState?.model_settings.primary_model ?? null;
  const displayActiveModel = activeModelLabel
    ? activeModelLabel.includes("/")
      ? activeModelLabel.split("/").slice(1).join("/")
      : activeModelLabel
    : null;

  // Subscribe to swarm activity store and adapt to StreamEntry[].
  const subscribeActivity = useCallback(
    (l: () => void) => subscribeSwarmActivity(swarmId, l),
    [swarmId],
  );
  const getActivitySnapshot = useCallback(
    () => getSwarmActivityState(swarmId),
    [swarmId],
  );
  const swarmActivityState = React.useSyncExternalStore(
    subscribeActivity,
    getActivitySnapshot,
    getActivitySnapshot,
  );
  const activityEntries = useMemo(
    () => toStreamEntries(swarmActivityState, displayFeature?.id),
    [swarmActivityState, displayFeature?.id],
  );

  // Features pagination
  const featuresBodyRef = useRef<HTMLDivElement>(null);
  const [featuresPerPage, setFeaturesPerPage] = useState(8);

  useLayoutEffect(() => {
    const el = featuresBodyRef.current;
    if (!el) return;
    const recompute = () => {
      const sample = el.querySelector("[data-feature-row]");
      const rowH = sample ? sample.getBoundingClientRect().height : 26;
      const h = el.clientHeight;
      if (rowH > 0 && h > 0) {
        const fit = Math.max(1, Math.floor(h / rowH));
        setFeaturesPerPage((prev) => (prev === fit ? prev : fit));
      }
    };
    recompute();
    const ro = new ResizeObserver(recompute);
    ro.observe(el);
    return () => ro.disconnect();
  }, []);

  const featurePages = Math.max(1, Math.ceil(features.length / featuresPerPage));
  const activeFeatureIdx = Math.max(
    0,
    features.findIndex((f) => f.id === activeFeature?.id),
  );
  const [featurePage, setFeaturePage] = useState(0);

  useEffect(() => {
    setFeaturePage(Math.floor(activeFeatureIdx / featuresPerPage));
  }, [featuresPerPage, activeFeatureIdx]);

  const safeFeaturePage = Math.min(featurePage, featurePages - 1);
  const featureSlice = features.slice(
    safeFeaturePage * featuresPerPage,
    (safeFeaturePage + 1) * featuresPerPage,
  );

  // ─── Phase 5C: Worker-reported discovered issues ─────────────────
  //
  // Pull `discovered_issue` events out of the backend progress log and
  // expose them as a separate, severity-coloured list. Acknowledge/Dismiss
  // state is React-local: no backend IPC, no persisted "dismissed" status.
  // The user can refresh the page and see the issues again — that is fine
  // per the spec.
  type DiscoveredIssueEntry = {
    /** Stable key derived from timestamp + feature + description so the
     *  React-local dismiss set survives event-list re-derivations. */
    key: string;
    timestamp: string;
    feature_id?: string;
    severity: IssueSeverity;
    description: string;
    suggested_fix?: string | null;
    message: string;
  };

  const discoveredIssues = useMemo<DiscoveredIssueEntry[]>(() => {
    return backendProgressLog
      .filter((e) => e.event_type === "discovered_issue")
      .filter((e) => !displayFeature || e.feature_id === displayFeature.id)
      .map((e) => {
        const meta = (e.metadata ?? {}) as Record<string, unknown>;
        const rawSeverity = typeof meta.severity === "string" ? meta.severity : "info";
        const severity: IssueSeverity =
          rawSeverity === "warn" || rawSeverity === "error" ? rawSeverity : "info";
        const description =
          typeof meta.description === "string" ? meta.description : e.message;
        const suggested_fix =
          typeof meta.suggested_fix === "string" ? meta.suggested_fix : null;
        return {
          key: `${e.timestamp}|${e.feature_id ?? ""}|${description.slice(0, 64)}`,
          timestamp: e.timestamp,
          feature_id: e.feature_id,
          severity,
          description,
          suggested_fix,
          message: e.message,
        };
      });
  }, [backendProgressLog, displayFeature]);

  // React-local: keys the user has dismissed. Not persisted.
  const [dismissedIssues, setDismissedIssues] = useState<Set<string>>(new Set());
  // React-local: keys the user has acknowledged. Acknowledged issues stay
  // visible but with a muted style; dismissed issues are hidden.
  const [ackedIssues, setAckedIssues] = useState<Set<string>>(new Set());

  const visibleIssues = useMemo(
    () => discoveredIssues.filter((i) => !dismissedIssues.has(i.key)),
    [discoveredIssues, dismissedIssues],
  );

  const handleAckIssue = useCallback((key: string) => {
    setAckedIssues((prev) => {
      const next = new Set(prev);
      next.add(key);
      return next;
    });
  }, []);

  const handleDismissIssue = useCallback((key: string) => {
    setDismissedIssues((prev) => {
      const next = new Set(prev);
      next.add(key);
      return next;
    });
  }, []);

  // Progress-log pagination
  const LOG_PER_PAGE = 5;
  const logPages = Math.max(1, Math.ceil(progressLog.length / LOG_PER_PAGE));
  const [logPage, setLogPage] = useState(0);
  // When new events arrive, jump to the latest page so the most recent
  // activity is visible without manual paging.
  useEffect(() => {
    setLogPage(0);
  }, [logPages]);
  const safeLogPage = Math.min(logPage, logPages - 1);
  const logSlice = progressLog.slice(
    safeLogPage * LOG_PER_PAGE,
    (safeLogPage + 1) * LOG_PER_PAGE,
  );

  // Hivemind: only meaningful when the user configured a hivemind for this
  // swarm. Otherwise we render a "not configured" hint rather than mocked
  // round verdicts.
  const hivemindId = backendState?.model_settings.hivemind_id ?? null;
  const hivemindEnabled = Boolean(
    hivemindId &&
      (backendState?.model_settings.use_hivemind_on_scout ||
        backendState?.model_settings.use_hivemind_on_queen),
  );
  const { hivemindOptions } = useTaskRuntime();
  const hivemindName = useMemo(() => {
    if (!hivemindId) return null;
    return hivemindOptions.find((h) => h.id === hivemindId)?.name ?? hivemindId;
  }, [hivemindOptions, hivemindId]);

  if (!sw) {
    return (
      <div className="h-full flex items-center justify-center">
        <div className="text-center">
          {loading ? (
            <div className="text-[13px] text-muted flex items-center gap-2">
              <span className="w-4 h-4 border-2 border-honey-400 border-t-transparent rounded-full animate-spin" />
              Loading swarm...
            </div>
          ) : error ? (
            <div className="text-[13px] text-red-400">{error}</div>
          ) : (
            <div className="text-[13px] text-muted">No swarm selected</div>
          )}
        </div>
      </div>
    );
  }

  return (
    <div className="h-full flex flex-col">
      {/* Top status bar */}
      <div className="shrink-0 border-b border-line bg-ink-850/40">
        <div className="px-5 py-2.5 flex items-center gap-3 flex-wrap">
          <div className="flex items-center gap-2.5 min-w-0">
            <div className="w-8 h-8 rounded-md bg-honey-500/15 border border-honey-500/30 flex items-center justify-center relative shrink-0">
              {I.swarm({ size: 14, className: "text-honey-400" })}
              <span className="absolute -top-0.5 -right-0.5 w-2 h-2 rounded-full bg-emerald-400 pulse-green" />
            </div>
            <div className="min-w-0">
              <div className="flex items-center gap-2 flex-wrap">
                <span className="text-[13px] font-bold whitespace-nowrap">
                  Swarm Control
                </span>
                <span className="text-[13px] font-semibold text-honey-300 truncate">
                  {sw.name}
                </span>
                <StatusBadge status={sw.status} />
              </div>
              <div className="text-[10.5px] text-dim font-mono mt-px flex items-center gap-2 min-w-0">
                <span className="truncate">{sw.cwd}</span>
                {swarmId && (
                  <button
                    type="button"
                    onClick={async () => {
                      try {
                        await navigator.clipboard.writeText(swarmId);
                        setIdCopied(true);
                        window.setTimeout(() => setIdCopied(false), 1200);
                      } catch (e) {
                        console.warn("Failed to copy swarm id to clipboard", e);
                      }
                    }}
                    title={`Click to copy full swarm id: ${swarmId}`}
                    className="shrink-0 inline-flex items-center gap-1 px-1.5 h-4 rounded bg-ink-800 border border-line hover:border-honey-500/40 text-dim hover:text-honey-300 transition-colors"
                  >
                    {I.copy({ size: 9 })}
                    <span>{swarmId.slice(0, 8)}</span>
                    {idCopied && (
                      <span className="text-honey-400 ml-0.5">copied</span>
                    )}
                  </button>
                )}
              </div>
            </div>
          </div>

          <div className="flex-1" />

          <div className="flex items-baseline gap-3 text-[11px] flex-wrap">
            <Stat icon={I.clock({ size: 11 })} label="elapsed" value={elapsedStr} />
            <Stat icon={I.cost({ size: 11 })} label="cost" value={costDisplay} tone={budgetTone} />
            <div className="flex items-baseline gap-1.5 whitespace-nowrap">
              <span className="self-center text-dim">
                {I.spark({ size: 11 })}
              </span>
              <span className="text-dim text-[10px] uppercase tracking-wider font-semibold">
                Tokens In/Out
              </span>
              <span className="font-mono text-[11.5px] text-white">
                {tokInStr}
              </span>
              <span className="text-dim text-[11px]">/</span>
              <span className="font-mono text-[11.5px] text-white">
                {tokOutStr}
              </span>
              <span className="text-dim text-[10px] uppercase tracking-wider font-semibold">
                Cached
              </span>
              <span className="font-mono text-[11.5px] text-white">
                {tokCacheStr}
              </span>
            </div>
          </div>

          <div className="flex items-center gap-1.5">
            {error && <span className="text-[11px] text-red-400 mr-1">{error}</span>}
            {backendState?.status === "interrupted" && (
              <span title={backendState?.error ?? undefined}>
                <Pill tone="honey">Interrupted by restart</Pill>
              </span>
            )}
            {sw.status === "running" && (
              <Btn kind="outline" size="sm" icon={I.pause({ size: 12 })} onClick={handlePause} disabled={actionLoading === "pause"}>
                {actionLoading === "pause" ? "..." : "Pause"}
              </Btn>
            )}
            {sw.status === "paused" && (
              <Btn kind="primary" size="sm" icon={I.play({ size: 12 })} onClick={handleResume} disabled={actionLoading === "resume"}>
                {actionLoading === "resume" ? "..." : "Resume"}
              </Btn>
            )}
            {sw.status === "failed" && (
              <Btn kind="primary" size="sm" icon={I.play({ size: 12 })} onClick={handleResume} disabled={actionLoading === "resume"}>
                {actionLoading === "resume" ? "..." : "Resume"}
              </Btn>
            )}
            {(sw.status === "running" || sw.status === "paused") && (
              <Btn kind="ghost" size="sm" onClick={handleStop} disabled={actionLoading === "stop"}>
                {actionLoading === "stop" ? "..." : "Stop"}
              </Btn>
            )}
          </div>
        </div>

        {/* Progress bar */}
        <div className="px-5 pb-2.5 flex items-center gap-3">
          <div className="flex-1 h-1.5 bg-ink-900 rounded-full overflow-hidden relative">
            <div className="h-full bg-emerald-500/80" style={{ width: `${pct}%` }} />
          </div>
          <span className="text-[10.5px] text-muted font-mono tabular-nums shrink-0">
            {done}/{total} {"·"} {pct}%
          </span>
        </div>
      </div>

      {/* Nurse activity strip: surfaces every NurseIntervention event from
          the progress log as an inline Nurse card. Only renders when at
          least one intervention has happened on this swarm so it doesn't
          take up vertical space on healthy runs. */}
      {nurseInterventions.length > 0 && (
        <div className="border-b border-line bg-ink-900/40 max-h-64 overflow-auto px-4 py-2">
          <div className="text-xs uppercase tracking-wider text-honey-400 mb-1">
            Nurse activity
          </div>
          {nurseInterventions.map((n) => (
            <NurseMessage key={n.interventionId + ":" + n.t} entry={n} />
          ))}
        </div>
      )}

      {/* Body grid */}
      <div className="flex-1 min-h-0 overflow-hidden">
        <div className="grid grid-cols-1 xl:grid-cols-[1.85fr_0.85fr_0.85fr] xl:grid-rows-[minmax(0,0.6fr)_minmax(0,1.4fr)] gap-0 h-full">
          {/* ROW 1, COL 1 -- Active feature */}
          <div className="flex flex-col min-w-0 min-h-0 xl:col-start-1 xl:row-start-1 border-b border-r border-line">
            <FlushPanel
              title={
                <>
                  {I.scope({ size: 13, className: "text-honey-400" })}
                  <span>{isLiveMode ? "Active feature" : "Feature history"}</span>
                  <span className="text-honey-300 font-mono normal-case tracking-normal text-[12px] ml-2 truncate">
                    {displayFeature?.name ?? "—"}
                  </span>
                </>
              }
              className="flex-1 min-h-0 bg-ink-800"
            >
              <div className="px-4 py-3.5 h-full overflow-auto">
                {displayFeature ? (
                  <>
                    {/* Phase pipeline */}
                    <div className="flex items-center gap-1.5 flex-wrap">
                      {[
                        {
                          id: "scout",
                          label: "Scout",
                          done: phaseState.scoutDone,
                          active: phaseState.scoutActive,
                          icon: I.scope({ size: 12 }),
                        },
                        {
                          id: "worker",
                          label: "Worker",
                          done: phaseState.workerDone,
                          active: phaseState.workerActive,
                          icon: I.swarm({ size: 12 }),
                        },
                        {
                          id: "guard",
                          label: "Guard",
                          done: phaseState.guardDone,
                          active: phaseState.guardActive,
                          icon: I.shield({ size: 12 }),
                        },
                      ].map((p, i, arr) => (
                        <React.Fragment key={p.id}>
                          <div
                            className={`flex items-center gap-1.5 h-7 px-2.5 rounded-md border text-[11.5px] font-medium ${
                              p.active
                                ? "bg-honey-500/15 text-honey-300 border-transparent"
                                : p.done
                                  ? "bg-emerald-500/10 text-emerald-300 border-transparent"
                                  : "bg-ink-850 text-dim border-transparent"
                            }`}
                          >
                            {p.done ? I.check({ size: 12 }) : p.icon}
                            {p.label}
                          </div>
                          {i < arr.length - 1 && I.chevR({ size: 12, className: "text-dim" })}
                        </React.Fragment>
                      ))}
                      {displayFeature.milestone && (
                        <div
                          className="ml-auto flex items-center gap-1.5 h-7 px-2.5 rounded-md border bg-ink-850 text-dim border-transparent text-[11.5px] font-medium"
                          title="Milestone"
                        >
                          {I.tag({ size: 12 })}
                          {displayFeature.milestone}
                        </div>
                      )}
                    </div>
                  </>
                ) : (
                  <div className="flex items-center justify-center py-2">
                    <MetaChip label="no active feature" />
                  </div>
                )}

                <div className="mt-3">
                  <div className="text-[11px] uppercase tracking-wider text-muted font-semibold mb-1.5">
                    Description
                  </div>
                  <p className="text-[12.5px] leading-relaxed text-white/85 whitespace-pre-wrap">
                    {displayFeature?.description?.trim() ||
                      (total === 0
                        ? "No features have been queued yet. Launch the swarm from a planning task to begin."
                        : "(no description provided for this feature)")}
                  </p>
                </div>

                {/* Phase 5C: Worker-reported discovered issues.
                  *
                  * Severity colours: info=blue, warn=amber, error=red.
                  * NEVER blocks execution — these are async notifications.
                  * Ack/Dismiss are React-local; not persisted to disk. */}
                {visibleIssues.length > 0 && (
                  <div className="mt-4">
                    <div className="flex items-center justify-between mb-1.5">
                      <div className="text-[11px] uppercase tracking-wider text-muted font-semibold">
                        Discovered issues
                      </div>
                      <span className="text-[10.5px] text-dim font-mono tabular-nums">
                        {visibleIssues.length} active
                        {ackedIssues.size > 0
                          ? ` · ${ackedIssues.size} acknowledged`
                          : ""}
                      </span>
                    </div>
                    <div className="space-y-1.5">
                      {visibleIssues.map((issue) => {
                        const acked = ackedIssues.has(issue.key);
                        const tone =
                          issue.severity === "error"
                            ? {
                                bg: "bg-red-500/10",
                                border: "border-red-500/30",
                                text: "text-red-200",
                                tag: "text-red-300",
                                dot: "bg-red-400",
                              }
                            : issue.severity === "warn"
                              ? {
                                  bg: "bg-amber-500/10",
                                  border: "border-amber-500/30",
                                  text: "text-amber-100",
                                  tag: "text-amber-300",
                                  dot: "bg-amber-400",
                                }
                              : {
                                  bg: "bg-blue-500/10",
                                  border: "border-blue-500/30",
                                  text: "text-blue-100",
                                  tag: "text-blue-300",
                                  dot: "bg-blue-400",
                                };
                        return (
                          <div
                            key={issue.key}
                            className={`rounded-md border px-2.5 py-1.5 text-[12px] ${tone.bg} ${tone.border} ${
                              acked ? "opacity-50" : ""
                            }`}
                            title={
                              issue.suggested_fix
                                ? `Suggested fix: ${issue.suggested_fix}`
                                : undefined
                            }
                          >
                            <div className="flex items-start gap-2">
                              <span
                                className={`mt-1 w-1.5 h-1.5 rounded-full ${tone.dot} shrink-0`}
                              />
                              <div className="min-w-0 flex-1">
                                <div className="flex items-center gap-1.5 flex-wrap">
                                  <span
                                    className={`text-[10.5px] uppercase tracking-wider font-semibold ${tone.tag}`}
                                  >
                                    {issue.severity}
                                  </span>
                                  {issue.feature_id && (
                                    <span className="text-[10.5px] text-dim font-mono">
                                      {issue.feature_id}
                                    </span>
                                  )}
                                  {acked && (
                                    <span className="text-[10px] uppercase tracking-wider text-dim font-medium">
                                      acknowledged
                                    </span>
                                  )}
                                </div>
                                <div className={`mt-0.5 leading-snug ${tone.text}`}>
                                  {issue.description}
                                </div>
                                {issue.suggested_fix && (
                                  <div className="mt-1 text-[11.5px] text-dim leading-snug">
                                    <span className="text-muted">Fix:</span>{" "}
                                    {issue.suggested_fix}
                                  </div>
                                )}
                              </div>
                              <div className="flex flex-col gap-1 shrink-0">
                                {!acked && (
                                  <button
                                    type="button"
                                    onClick={() => handleAckIssue(issue.key)}
                                    className="text-[10.5px] uppercase tracking-wider font-semibold text-muted hover:text-white px-1.5 py-0.5 rounded border border-line hover:border-line-strong transition-colors"
                                  >
                                    Ack
                                  </button>
                                )}
                                <button
                                  type="button"
                                  onClick={() => handleDismissIssue(issue.key)}
                                  className="text-[10.5px] uppercase tracking-wider font-semibold text-muted hover:text-white px-1.5 py-0.5 rounded border border-line hover:border-line-strong transition-colors"
                                >
                                  Dismiss
                                </button>
                              </div>
                            </div>
                          </div>
                        );
                      })}
                    </div>
                  </div>
                )}
              </div>
            </FlushPanel>
          </div>

          {/* COL 2, ROW 1 -- Tasks */}
          <div className="flex flex-col min-w-0 min-h-0 xl:col-start-2 xl:row-start-1 border-b border-r border-line">
            <FlushPanel
              className="flex-1 min-h-0 bg-ink-800"
              title={
                <>
                  {I.list({ size: 13, className: "text-honey-400" })}
                  <span>Tasks</span>
                </>
              }
              right={
                <>
                  <Pill tone="honey">{done}/{total} tasks</Pill>
                  <Pager page={safeFeaturePage} pages={featurePages} setPage={setFeaturePage} />
                </>
              }
            >
              <div ref={featuresBodyRef} className="divide-y divide-line/50 h-full overflow-hidden">
                {features.length === 0 ? (
                  <div className="px-3 py-4 text-[12px] text-dim">
                    No features queued yet.
                  </div>
                ) : (
                  featureSlice.map((f, idx) => {
                    const ds = displayStatus(f.status);
                    const isActive = ds === "active" && f.id === activeFeature?.id;
                    const isSelectedHistory =
                      selectedFeatureId !== null &&
                      f.id === selectedFeatureId &&
                      f.id !== activeFeature?.id;
                    const absoluteIdx = safeFeaturePage * featuresPerPage + idx + 1;
                    // Phase 2: synthetic validator features (id starts with
                    // `validate-`) render with a Guard-style shield icon and
                    // a distinctive blue accent so the user can see at a
                    // glance which rows are autonomous milestone gates vs
                    // implementation work.
                    const isValidator = f.id.startsWith("validate-");
                    return (
                      <button
                        type="button"
                        key={f.id}
                        data-feature-row
                        onClick={() => handleSelectFeature(f.id)}
                        title={
                          f.id === activeFeature?.id
                            ? "Currently active — click to follow live"
                            : isSelectedHistory
                              ? "Viewing history — click to return to live"
                              : "View this task's history"
                        }
                        className={`w-full text-left flex items-center gap-2.5 px-3 py-1.5 text-[12px] hover:bg-ink-700/40 transition-colors ${
                          isActive ? "bg-honey-500/8" : ""
                        } ${isValidator ? "bg-blue-500/5" : ""} ${
                          isSelectedHistory ? "ring-1 ring-inset ring-honey-500/40 bg-honey-500/[0.05]" : ""
                        }`}
                      >
                        <span className="font-mono text-dim text-[10.5px] w-6 text-right tabular-nums shrink-0">
                          {String(absoluteIdx).padStart(2, "0")}
                        </span>
                        {isValidator
                          ? I.shield({
                              size: 12,
                              className: "text-blue-400 shrink-0",
                              sw: 2.5,
                            })
                          : ds === "done"
                            ? I.check({
                                size: 12,
                                className: "text-emerald-400 shrink-0",
                                sw: 2.5,
                              })
                            : ds === "active"
                              ? (
                                <span className="w-2 h-2 rounded-full bg-honey-400 pulse-amber shrink-0" />
                              )
                              : ds === "pending"
                                ? (
                                  <span className="w-2 h-2 rounded-full border border-line-strong shrink-0" />
                                )
                                : ds === "failed"
                                  ? (
                                    <span className="w-2 h-2 rounded-full bg-red-400 shrink-0" />
                                  )
                                  : ds === "skipped"
                                    ? (
                                      <span className="w-2 h-2 rounded-full bg-line-strong shrink-0" />
                                    )
                                    : null}
                        <span
                          className={`font-mono truncate flex-1 ${
                            isValidator
                              ? ds === "done"
                                ? "text-blue-200"
                                : "text-blue-300"
                              : ds === "done"
                                ? "text-muted"
                                : isActive
                                  ? "text-honey-200 font-medium"
                                  : ds === "failed"
                                    ? "text-red-300"
                                    : "text-dim"
                          }`}
                        >
                          {f.name}
                        </span>
                        {isValidator && (
                          <span className="text-[10.5px] text-blue-400 font-medium shrink-0 uppercase tracking-wider">
                            Validator
                          </span>
                        )}
                        {!isValidator && ds === "failed" && (
                          <span className="text-[10.5px] text-red-400 font-medium shrink-0">
                            failed
                          </span>
                        )}
                      </button>
                    );
                  })
                )}
              </div>
            </FlushPanel>
          </div>

          {/* COL 3, ROW 1 -- Progress log */}
          <div className="flex flex-col min-w-0 min-h-0 xl:col-start-3 xl:row-start-1 border-b border-line">
            <FlushPanel
              className="flex-1 min-h-0 bg-ink-800"
              title={
                <>
                  {I.clock({ size: 13, className: "text-honey-400" })}
                  <span>Progress log</span>
                </>
              }
              right={
                <Pager page={safeLogPage} pages={logPages} setPage={setLogPage} />
              }
            >
              <div className="px-3 py-2 space-y-0.5 text-[12px] h-full overflow-auto">
                {logSlice.length === 0 ? (
                  <div className="text-dim py-3 text-center text-[11.5px]">
                    No events recorded yet.
                  </div>
                ) : (
                  logSlice.map((e, i) => {
                    const tone =
                      e.kind === "review"
                        ? "text-honey-300"
                        : e.kind === "queen"
                          ? "text-honey-300"
                          : e.kind === "done"
                            ? "text-emerald-300"
                            : e.kind === "tool"
                              ? "text-blue-300"
                              : "text-white/85";
                    return (
                      <div
                        key={i}
                        className="grid grid-cols-[60px_1fr] gap-3 py-1.5 border-b border-line/40 last:border-0"
                      >
                        <span className="font-mono text-[10.5px] text-dim pt-px">{e.t}</span>
                        <div className="font-mono text-[11.5px] leading-tight">
                          <div className={tone}>{e.msg}</div>
                          <div className="text-dim truncate">{e.detail}</div>
                        </div>
                      </div>
                    );
                  })
                )}
              </div>
            </FlushPanel>
          </div>

          {/* COL 3, ROW 2 -- Hivemind review */}
          <div className="flex flex-col min-w-0 min-h-0 xl:col-start-3 xl:row-start-2">
            <FlushPanel
              title={
                <>
                  {I.hexFill({ size: 11, className: "text-honey-400" })}
                  <span>Hivemind</span>
                </>
              }
              right={
                hivemindEnabled ? (
                  <Pill tone="honey">
                    <span title={hivemindId ?? undefined}>{hivemindName}</span>
                  </Pill>
                ) : (
                  <span className="text-[11px] text-dim font-mono normal-case tracking-normal">
                    not configured
                  </span>
                )
              }
              className="flex-1 min-h-0 bg-ink-800"
            >
              <div className="h-full overflow-auto">
                <div className="p-3 space-y-4 overflow-auto">
                  {(() => {
                    if (queenReview || featReview) {
                      return (
                        <>
                          {queenReview && (
                            <div>
                              <div className="text-[11px] uppercase tracking-wider text-honey-300 font-mono mb-2">
                                Master Plan Review
                              </div>
                              <HivemindReviewLivePanel
                                state={queenReview}
                                sourceLabel={queenReview.sourceLabel ?? "Queen master plan"}
                                compact
                                onViewMergedPlan={({ round, text }) =>
                                  setActiveMergedPlan({
                                    round,
                                    text,
                                    subtitle:
                                      queenReview.sourceLabel ?? "Queen master plan",
                                  })
                                }
                              />
                            </div>
                          )}
                          {featReview && (
                            <div>
                              {queenReview && (
                                <div className="text-[11px] uppercase tracking-wider text-honey-300 font-mono mb-2">
                                  Scout Plan Review
                                </div>
                              )}
                              <HivemindReviewLivePanel
                                state={featReview}
                                sourceLabel={
                                  featReview.sourceLabel ??
                                  (displayFeature ? `Scout: ${displayFeature.id}` : undefined)
                                }
                                compact
                                onViewMergedPlan={({ round, text }) =>
                                  setActiveMergedPlan({
                                    round,
                                    text,
                                    subtitle:
                                      featReview.sourceLabel ??
                                      (displayFeature ? `Scout: ${displayFeature.id}` : undefined),
                                  })
                                }
                              />
                            </div>
                          )}
                        </>
                      );
                    }
                    if (hivemindEnabled) {
                      return (
                        <div className="rounded-lg border border-line bg-ink-850 p-3 text-[12px] text-dim leading-relaxed">
                          Hivemind review on{" "}
                          {backendState?.model_settings.use_hivemind_on_queen
                            ? "queen"
                            : "scout"}{" "}
                          is configured. Review rounds and per-model verdicts will appear
                          here once a feature reaches that phase.
                        </div>
                      );
                    }
                    return (
                      <div className="rounded-lg border border-line bg-ink-850 p-3 text-[12px] text-dim leading-relaxed">
                        No Hivemind review is configured for this swarm. Attach one in the
                        swarm settings to have multi-model verdicts run on Scout plans or
                        the Queen's decomposition.
                      </div>
                    );
                  })()}
                </div>
              </div>
            </FlushPanel>
          </div>

          {/* ROW 2, COLS 1-2 -- Live activity stream */}
          <div className="flex flex-col min-w-0 min-h-0 xl:col-start-1 xl:col-span-2 xl:row-start-2 border-r border-line">
            <FlushPanel
              title={
                <div className="flex items-center gap-2 flex-wrap">
                  {I.swarm({ size: 13, className: "text-honey-400" })}
                  <span>{isLiveMode ? "Live activity" : "Activity history"}</span>
                  {displayFeature && (
                    <>
                      <span className="inline-flex items-center gap-1.5 h-6 px-2 rounded-md bg-blue-500/15 text-blue-200 border border-blue-500/30 text-[11.5px] font-medium normal-case tracking-normal">
                        {I.swarm({ size: 11 })}
                        {phaseState.scoutReviewActive
                          ? "Scout · Hivemind review"
                          : phaseState.scoutPiActive
                            ? "Scout"
                            : phaseState.workerActive
                              ? "Worker"
                              : phaseState.guardActive
                                ? "Guard"
                                : isLiveMode
                                  ? "Idle"
                                  : displayFeature.status}
                      </span>
                      <span className="text-dim font-mono normal-case tracking-normal text-[12px] truncate">
                        {displayFeature.name}
                      </span>
                    </>
                  )}
                </div>
              }
              right={
                <>
                  {isLiveMode ? (
                    sw.status === "running" && (
                      <span className="inline-flex items-center gap-1.5 text-[11px] text-emerald-300 font-mono whitespace-nowrap">
                        <span className="w-1.5 h-1.5 rounded-full bg-emerald-400 animate-pulse" />
                        live
                      </span>
                    )
                  ) : (
                    <>
                      <span className="inline-flex items-center gap-1.5 text-[11px] text-honey-300 font-mono whitespace-nowrap">
                        <span className="w-1.5 h-1.5 rounded-full bg-honey-400" />
                        history
                      </span>
                      <Btn
                        kind="outline"
                        size="sm"
                        onClick={() => setSelectedFeatureId(null)}
                      >
                        Back to live
                      </Btn>
                    </>
                  )}
                  {backendState?.model_settings.primary_model && (
                    <Pill tone="blue">{backendState.model_settings.primary_model}</Pill>
                  )}
                </>
              }
              className="flex-1 min-h-0"
              bodyClass="flex flex-col"
            >
              <ActivityStream
                entries={activityEntries}
                showReasoning={showReasoning}
                showToolCalls={showToolCalls}
                streaming={isLiveMode && sw.status === "running"}
                tailLimit={300}
                onActiveSessionChange={handleActiveSessionChange}
                emptyState={{
                  icon: I.swarm({ size: 13, className: "text-dim" }),
                  primary: !isLiveMode
                    ? "No activity was recorded for this feature."
                    : phaseState.scoutReviewActive
                      ? "Scout finished — Hivemind is reviewing the plan (see Hivemind panel)."
                      : sw.status === "running" || sw.status === "paused"
                        ? "Waiting for the first agent…"
                        : "Launch the swarm to start producing activity.",
                }}
              />
              <ActivityFooter
                activeSession={{
                  sessionId: activeSession.sessionId,
                  model: displayActiveModel,
                  agent: activeSession.agent,
                }}
                ctx={{
                  pct: ctxStats.pct,
                  label: ctxStats.ctxLabel,
                  tokIn: ctxStats.tokIn,
                  tokOut: ctxStats.tokOut,
                  tokPerSec: ctxStats.tokPerSec,
                }}
                showReasoning={showReasoning}
                showToolCalls={showToolCalls}
                onToggleReasoning={() =>
                  setShowReasoning((v) => {
                    const n = !v;
                    try {
                      localStorage.setItem(SHOW_REASONING_KEY, String(n));
                    } catch {
                      /* noop */
                    }
                    return n;
                  })
                }
                onToggleToolCalls={() =>
                  setShowToolCalls((v) => {
                    const n = !v;
                    try {
                      localStorage.setItem(SHOW_TOOL_CALLS_KEY, String(n));
                    } catch {
                      /* noop */
                    }
                    return n;
                  })
                }
              />
            </FlushPanel>
          </div>
        </div>
      </div>

      {/* Screen-level merged-plan modal. Shared by the queen and scout
          review panels (only one click at a time, most recent wins).
          Hoisted out of the review panels so the modal survives any
          parent re-mount triggered by tab switches or future dock-mode
          changes. */}
      <MergedPlanModal
        open={activeMergedPlan != null}
        title={
          activeMergedPlan
            ? `Merged plan \u2014 Round ${activeMergedPlan.round}`
            : "Merged plan"
        }
        subtitle={activeMergedPlan?.subtitle}
        planText={activeMergedPlan?.text ?? ""}
        onClose={() => setActiveMergedPlan(null)}
      />
    </div>
  );
}
