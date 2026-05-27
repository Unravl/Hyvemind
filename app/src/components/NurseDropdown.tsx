import React, { useState, useRef, useEffect } from "react";
import { useNurseStatus } from "../hooks/useNurseStatus";
import { I } from "./icons";
import * as ipc from "../lib/ipc";
import type {
  BatchTickSnapshot,
  NurseHealth,
  NurseInterventionRecord,
  NurseServiceConfigSnapshot,
} from "../types/nurse";

/** Compute how full the pink pill's progress bar should be (0-100).
 *  100% = the next batched Nurse check-in is imminent.
 *  0%   = a check-in just completed and a fresh window is starting.
 *  Hidden (returns null) when the batch reviewer isn't attached or the
 *  user has disabled batched review. */
function computeTriggerProgress(
  batch: BatchTickSnapshot | null,
  now: number,
): number | null {
  if (!batch || !batch.enabled) return null;
  const intervalMs = Math.max(1, batch.interval_secs * 1000);
  const nextAt = batch.next_tick_at_unix_ms || now + intervalMs;
  const startAt = nextAt - intervalMs;
  const elapsed = Math.max(0, Math.min(intervalMs, now - startAt));
  return Math.round((elapsed / intervalMs) * 100);
}

/* ── Service-health colors ─────────────────────────────────── */

type HealthState = "ok" | "amber" | "red" | "grey";

const HEALTH_DOT: Record<HealthState, string> = {
  ok: "bg-emerald-400",
  amber: "bg-amber-400",
  red: "bg-red-400",
  grey: "bg-line-strong",
};

const HEALTH_STATE_LABEL: Record<HealthState, string> = {
  ok: "Running",
  amber: "Recovering",
  red: "Degraded",
  grey: "Disabled",
};

function deriveHealthState(
  config: NurseServiceConfigSnapshot,
  health: NurseHealth,
): HealthState {
  if (!config.enabled) return "grey";
  if (health.degraded) return "red";
  const staleAfterMs = config.tick_interval_secs * 2 * 1000;
  const isStale =
    health.last_tick_at === null ||
    Date.now() - health.last_tick_at > staleAfterMs;
  if (health.consecutive_failed_ticks > 0 || isStale) return "amber";
  return "ok";
}

/* ── Helpers ───────────────────────────────────────────────── */

function truncateSessionId(id: string, maxLen = 12): string {
  if (id.length <= maxLen) return id;
  return id.slice(0, maxLen) + "…";
}

function formatRelativeMs(ms: number | null): string {
  if (ms === null) return "—";
  const secs = Math.floor((Date.now() - ms) / 1000);
  if (secs < 0) return "just now";
  if (secs < 60) return `${secs}s ago`;
  if (secs < 3600) return `${Math.floor(secs / 60)}m ago`;
  return `${Math.floor(secs / 3600)}h ago`;
}

function formatTimestamp(ts: string): string {
  try {
    const d = new Date(ts);
    return d.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
  } catch {
    return ts;
  }
}

/* ── Intervention Row ──────────────────────────────────────── */

function InterventionRow({ record }: { record: NurseInterventionRecord }) {
  return (
    <div className="px-3 py-2 border-b border-line last:border-b-0">
      <div className="flex items-center gap-2 text-[11px]">
        <span className="text-muted">{formatTimestamp(record.timestamp)}</span>
        <span className="font-mono text-white/80 truncate" title={record.session_id}>
          {truncateSessionId(record.session_id)}
        </span>
        <span className="text-amber-300 font-medium">{record.level}</span>
      </div>
      {record.outcome && (
        <div className="text-[10px] text-muted mt-0.5 truncate">{record.outcome}</div>
      )}
    </div>
  );
}

/* ── Main Dropdown ─────────────────────────────────────────── */

function NurseDropdownInner({
  onOpenSettings,
  onOpenChange,
}: {
  onOpenSettings?: () => void;
  onOpenChange?: (open: boolean) => void;
}) {
  const { status, refresh } = useNurseStatus();
  const [toggling, setToggling] = useState(false);
  const [optimisticEnabled, setOptimisticEnabled] = useState<boolean | null>(null);
  const [togglingSwarmsOnly, setTogglingSwarmsOnly] = useState(false);
  const [optimisticSwarmsOnly, setOptimisticSwarmsOnly] = useState<
    boolean | null
  >(null);

  const handleToggle = async (nextEnabled: boolean) => {
    setOptimisticEnabled(nextEnabled);
    setToggling(true);
    try {
      await ipc.setNurseConfig({ enabled: nextEnabled });
      await refresh();
    } catch (err) {
      console.error("Failed to toggle Nurse:", err);
      setOptimisticEnabled(null);
    } finally {
      setToggling(false);
      setOptimisticEnabled(null);
    }
  };

  const handleToggleSwarmsOnly = async (next: boolean) => {
    setOptimisticSwarmsOnly(next);
    setTogglingSwarmsOnly(true);
    try {
      await ipc.setNurseConfig({ swarms_only: next });
      await refresh();
    } catch (err) {
      console.error("Failed to toggle Nurse swarms-only:", err);
      setOptimisticSwarmsOnly(null);
    } finally {
      setTogglingSwarmsOnly(false);
      setOptimisticSwarmsOnly(null);
    }
  };

  const [open, setOpen] = useState(false);
  const ref = useRef<HTMLDivElement>(null);
  // 1-second interpolation tick so the pink progress bar advances smoothly
  // between batch status updates (which only arrive after each tick).
  const [now, setNow] = useState<number>(() => Date.now());
  useEffect(() => {
    const id = window.setInterval(() => setNow(Date.now()), 1000);
    return () => window.clearInterval(id);
  }, []);

  // All open-state transitions go through this so the parent (Topbar) can
  // disable its drag zones while the dropdown is open. The outside-click
  // listener stays in the bubble phase — Topbar drag zones are disabled
  // while the dropdown is open, so empty-bar clicks dismiss cleanly.
  const setOpenAndNotify = (next: boolean | ((current: boolean) => boolean)) => {
    setOpen((current) => {
      const resolved = typeof next === "function" ? next(current) : next;
      onOpenChange?.(resolved);
      return resolved;
    });
  };

  // Close on outside click
  useEffect(() => {
    if (!open) return;
    const handler = (e: MouseEvent) => {
      if (ref.current && !ref.current.contains(e.target as Node)) {
        setOpenAndNotify(false);
      }
    };
    document.addEventListener("mousedown", handler);
    return () => document.removeEventListener("mousedown", handler);
  }, [open]);

  const { stats, recent_interventions: interventions, config, health, batch } = status;
  const isEnabled = optimisticEnabled !== null ? optimisticEnabled : config.enabled;
  const swarmsOnly =
    optimisticSwarmsOnly !== null
      ? optimisticSwarmsOnly
      : (config.swarms_only ?? false);
  const healthState = deriveHealthState({ ...config, enabled: isEnabled }, health);
  const triggerProgress = computeTriggerProgress(batch ?? null, now);
  const llmCallsTotal = batch?.llm_calls_total ?? 0;

  const prevProgressRef = useRef<number | null>(null);
  useEffect(() => {
    prevProgressRef.current = triggerProgress;
  }, [triggerProgress]);

  const isResetting =
    triggerProgress !== null &&
    prevProgressRef.current !== null &&
    triggerProgress < prevProgressRef.current;

  return (
    <div className="relative" ref={ref}>
      {/* Trigger pill — pink batch-check countdown.
       *  The bar fills as the next batched Nurse review approaches (100%
       *  ≈ a check-in is about to fire; 0% just after one completed).
       *  Heart icon + small health dot in the corner preserve the legacy
       *  status surface; click anywhere on the pill toggles the dropdown. */}
      <button
        onClick={() => setOpenAndNotify((v) => !v)}
        aria-expanded={open}
        className={`flex items-center gap-1.5 px-2.5 h-7 rounded-md border font-mono text-[12px] transition duration-200 relative ${
          isEnabled
            ? "text-pink-300 border-pink-500/30 bg-pink-500/8 hover:border-pink-400/50"
            : "text-white/30 border-white/10 bg-white/3 hover:border-white/20"
        }`}
        title={
          triggerProgress === null
            ? `Nurse monitor — ${HEALTH_STATE_LABEL[healthState]}`
            : `Nurse — ${HEALTH_STATE_LABEL[healthState]}. Next batch check-in ${100 - triggerProgress}% away. ${llmCallsTotal} LLM call${llmCallsTotal === 1 ? "" : "s"} this session.`
        }
      >
        <NurseBadge enabled={isEnabled} />
        {!isEnabled ? (
          <>
            <span className="text-[12px] text-white/30 transition-colors duration-200">Nurse</span>
            <div className="relative w-12 h-2 rounded-full bg-ink-950/40 border border-white/10 overflow-hidden shrink-0">
              <div
                className="h-full rounded-full bg-pink-900/40"
                style={{ width: "0%" }}
              />
            </div>
          </>
        ) : (
          <div className="relative w-12 h-2 rounded-full bg-ink-800 border border-white/30 overflow-hidden shrink-0">
            <div
              className={`h-full rounded-full bg-pink-300 transition-[width] ${
                isResetting ? "duration-[150ms] ease-out" : "duration-1000 ease-linear"
              }`}
              style={{ width: `${triggerProgress ?? 0}%` }}
            />
          </div>
        )}
      </button>

      {/* Dropdown panel */}
      {open && (
        <div className="absolute right-0 top-full mt-1.5 w-80 max-h-[480px] bg-ink-900 border border-line rounded-xl shadow-2xl z-50 flex flex-col overflow-hidden">
          {/* Header */}
          <div className="px-3 py-2.5 border-b border-line bg-ink-850">
            <div className="flex items-center justify-between">
              <div className="text-[12px] text-white font-medium">
                Nurse Monitor
              </div>
              <div className="flex items-center gap-2">
                <span className="text-[10px] text-muted select-none">
                  {config.enabled ? "Enabled" : "Disabled"}
                </span>
                <Toggle
                  value={config.enabled}
                  disabled={toggling}
                  onChange={handleToggle}
                />
              </div>
            </div>
            <div className="text-[10px] text-muted mt-1">
              Monitoring {stats.monitored_count} session{stats.monitored_count !== 1 ? "s" : ""}
              {stats.stall_count > 0 && (
                <span className="text-amber-300"> · {stats.stall_count} stall{stats.stall_count !== 1 ? "s" : ""}</span>
              )}
              {swarmsOnly && (
                <span className="text-pink-300"> · swarms only</span>
              )}
            </div>
            <div
              className="flex items-center justify-between mt-2"
              title="Only intervene on swarm agents. Tasks and Hiveminds are observed but not steered."
            >
              <div>
                <div className="text-[11px] text-white/85">Swarms only</div>
                <div className="text-[10px] text-muted leading-tight">
                  Tasks &amp; Hiveminds observed, not steered
                </div>
              </div>
              <Toggle
                value={swarmsOnly}
                disabled={togglingSwarmsOnly || !config.enabled}
                onChange={handleToggleSwarmsOnly}
                ariaLabel="Swarms only"
              />
            </div>
            {/* LLM call counter — Tier 3 single-session + batched reviewer.
             *  Resets to 0 on app start; not persisted. */}
            <div className="text-[10px] text-muted mt-1 flex items-center gap-1">
              <span className="text-pink-300/80">LLM calls this session:</span>
              <span className="font-mono text-white/85 tabular-nums">{llmCallsTotal}</span>
            </div>
          </div>

          {/* Service health */}
          {health.degraded ? (
            <div className="px-3 py-2 border-b border-line bg-red-500/10">
              <div className="text-[11px] text-red-300 font-medium">
                Nurse is in degraded mode. Recovery in progress.
              </div>
            </div>
          ) : (
            <div className="px-3 py-2 border-b border-line bg-ink-900/40">
              <div className="text-[10px] text-dim font-medium uppercase tracking-wider mb-1">
                Service health
              </div>
              <div className="text-[11px] text-muted flex flex-col gap-0.5">
                <div>
                  Last tick:{" "}
                  <span className="text-white/80">{formatRelativeMs(health.last_tick_at)}</span>
                </div>
                <div>
                  Last successful:{" "}
                  <span className="text-white/80">{formatRelativeMs(health.last_successful_tick_at)}</span>
                </div>
                <div>
                  Failures in a row:{" "}
                  <span className="text-white/80">{health.consecutive_failed_ticks}</span>
                </div>
                <div>
                  Status: <span className="text-white/80">{HEALTH_STATE_LABEL[healthState]}</span>
                </div>
              </div>
            </div>
          )}

          {/* Intervention log */}
          {interventions.length > 0 && (
            <div className="border-t border-line max-h-40 overflow-y-auto">
              <div className="px-3 py-1.5 text-[10px] text-dim font-medium uppercase tracking-wider">
                Recent Interventions
              </div>
              {interventions.slice(0, 10).map((r) => (
                <InterventionRow key={r.id} record={r} />
              ))}
            </div>
          )}

          {/* Empty state */}
          {interventions.length === 0 && (
            <div className="px-3 py-6 text-center text-[12px] text-muted">
              No recent interventions
            </div>
          )}

          {/* Footer */}
          {onOpenSettings && (
            <div className="border-t border-line px-3 py-2">
              <button
                onClick={() => {
                  setOpenAndNotify(false);
                  onOpenSettings();
                }}
                className="text-[11px] text-honey-300 hover:text-honey-200 transition"
              >
                Nurse Settings →
              </button>
            </div>
          )}
        </div>
      )}
    </div>
  );
}

export const NurseDropdown = React.memo(NurseDropdownInner);

/** Pink "NUR" brand badge — matches the pink-500 brand stroke. */
function NurseBadge({ enabled }: { enabled: boolean }) {
  return (
    <span
      className="inline-flex items-center justify-center px-1.5 h-4 rounded text-[9.5px] font-bold tracking-tight transition-colors duration-200"
      style={{
        backgroundColor: enabled ? "rgba(244, 114, 182, 0.15)" : "rgba(255, 255, 255, 0.04)",
        color: enabled ? "#f472b6" : "rgba(255, 255, 255, 0.3)",
        border: enabled ? "1px solid rgba(244, 114, 182, 0.3)" : "1px solid rgba(255, 255, 255, 0.1)",
      }}
      aria-hidden
    >
      NUR
    </span>
  );
}

function Toggle({
  value,
  onChange,
  disabled,
  ariaLabel,
}: {
  value: boolean;
  onChange: (next: boolean) => void;
  disabled?: boolean;
  ariaLabel?: string;
}) {
  return (
    <button
      type="button"
      onClick={() => !disabled && onChange(!value)}
      aria-pressed={value}
      aria-label={ariaLabel}
      disabled={disabled}
      className={`relative w-8 h-4 rounded-full transition-colors duration-200 outline-none focus:ring-1 focus:ring-honey-300 ${
        value ? "bg-emerald-500" : "bg-ink-700"
      } ${disabled ? "opacity-50 cursor-not-allowed" : ""}`}
    >
      <span
        className={`absolute top-0.5 w-3 h-3 rounded-full bg-white transition-all duration-200 ${
          value ? "left-[18px]" : "left-0.5"
        }`}
      />
    </button>
  );
}

