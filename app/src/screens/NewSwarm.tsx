import React, { useEffect, useRef, useState } from "react";
import { GoFn } from "../App";
import { I } from "../components/icons";
import { Btn, Input, Select } from "../components/atoms";
import { HIVEMINDS } from "../data/mock";
import { ModelBrowserModal } from "./ModelBrowser";
import { isTauri } from "../lib/tauri";
import { open as openFolderDialog } from "@tauri-apps/plugin-dialog";
import * as ipc from "../lib/ipc";
import type { Feature, HivemindSummary, Milestone, ModelSettings, SwarmState } from "../lib/types";
import { useTaskRuntime } from "../lib/taskRuntime";
import { useSetting } from "../lib/SettingsProvider";
import { isUnderApprovedRoot } from "../components/ProjectPicker";

/** Payload passed when navigating from `Swarms.tsx` via Clone Plan. The
 *  features + milestones are read straight from the source swarm's disk
 *  artifacts via `getSwarmFeatures` / `getSwarmMilestones`. The new swarm
 *  inherits this plan and skips Queen planning entirely. */
export interface ClonedPlanPayload {
  features: Feature[];
  milestones: Milestone[];
  sourceSwarmId: string;
  sourceSwarmName: string;
}

const FALLBACK_CWD = "~/code/atlas/services/payments";

/* ── Draft persistence ──────────────────────────────────────── */

const SWARM_DRAFT_KEY = "hyvemind:swarm-draft";
const DRAFT_MAX_AGE_MS = 24 * 60 * 60 * 1000; // 24 hours

interface SwarmDraft {
  name: string;
  cwd: string;
  roleModels: Record<string, { model: string; provider: string }>;
  roleHive: Record<string, string | undefined>;
  roleThinking: Record<string, string>;
  savedAt: number;
}

function loadDraft(): SwarmDraft | null {
  try {
    const raw = localStorage.getItem(SWARM_DRAFT_KEY);
    if (!raw) return null;
    const draft = JSON.parse(raw) as SwarmDraft;
    if (!draft.savedAt || Date.now() - draft.savedAt > DRAFT_MAX_AGE_MS) {
      localStorage.removeItem(SWARM_DRAFT_KEY);
      return null;
    }
    return draft;
  } catch {
    return null;
  }
}

function saveDraft(draft: SwarmDraft) {
  try {
    localStorage.setItem(SWARM_DRAFT_KEY, JSON.stringify(draft));
  } catch {
    /* quota exceeded */
  }
}

function clearDraft() {
  try {
    localStorage.removeItem(SWARM_DRAFT_KEY);
  } catch {
    /* ignore */
  }
}

/* ── Local types and constants ─────────────────────────────── */

interface RoleDef {
  id: string;
  label: string;
  icon: string;
  desc: string;
  model: string;
  provider: string;
  canReview: boolean;
  reviewSub?: string;
}

const ROLES: RoleDef[] = [
  {
    id: "queen",
    label: "Queen",
    icon: "\u{1F451}",
    desc: "Plans the work and decomposes into features",
    model: "",
    provider: "anthropic",
    canReview: true,
    reviewSub: "Before features.json is locked",
  },
  {
    id: "scout",
    label: "Scout",
    icon: "\u{1F50D}",
    desc: "Specs each feature: preconditions, behavior, tests",
    model: "",
    provider: "anthropic",
    canReview: true,
    reviewSub: "Per-feature spec gate",
  },
  {
    id: "worker",
    label: "Worker",
    icon: "\u{1F41D}",
    desc: "Implements features in parallel",
    model: "",
    provider: "deepseek",
    canReview: false,
  },
  {
    id: "guard",
    label: "Guard",
    icon: "\u{1F6E1}",
    desc: "Final gate: tests, lints, runs CI before commit",
    model: "",
    provider: "openai",
    canReview: false,
  },
];

/* ── Section ──────────────────────────────────────────────── */

function Section({
  title,
  subtitle,
  children,
}: {
  title: string;
  subtitle: string;
  children: React.ReactNode;
}) {
  return (
    <div className="mb-6">
      <div className="flex items-baseline gap-3 mb-3">
        <h2 className="text-[14px] font-semibold text-white">{title}</h2>
        <span className="text-[12px] text-dim">{subtitle}</span>
      </div>
      {children}
    </div>
  );
}

/* ── Field ────────────────────────────────────────────────── */

function Field({
  label,
  children,
}: {
  label: string;
  children: React.ReactNode;
}) {
  return (
    <div>
      <label className="text-[11px] uppercase tracking-wider text-muted font-semibold">
        {label}
      </label>
      <div className="mt-1.5">{children}</div>
    </div>
  );
}

/* ── NewSwarmScreen ───────────────────────────────────────── */

export function NewSwarmScreen({
  go,
  swarm,
  edit,
  clonedPlan,
}: {
  go: GoFn;
  swarm?: any;
  edit?: boolean;
  clonedPlan?: ClonedPlanPayload;
}) {
  const isEdit = !!edit && !!swarm;
  // Clone mode is mutually exclusive with edit: Edit reseeds from an existing
  // swarm's state; Clone bootstraps a brand-new swarm with another swarm's
  // features+milestones locked in.
  const isClone = !!clonedPlan && !isEdit;
  const { createTask } = useTaskRuntime();
  // audit 6.7 — pull the default project path from the shared
  // SettingsProvider instead of issuing a per-mount `getSettings()` call.
  const defaultProjectPath = useSetting("default_project_path");

  // Edit mode accepts either:
  //   - the raw `SwarmState` from the backend (preferred path; carries the
  //     authoritative `model_settings` with thinking levels, hivemind flags,
  //     guard_model, etc.) — navigated via `go("new-swarm", { swarm: state, edit: true })`
  //   - the lossy mock `Swarm` shape from Swarms.tsx (legacy fallback), which
  //     flattens roles to bare strings and omits thinking levels.
  // Read defensively so both shapes work.
  const ms = isEdit ? (swarm.model_settings as any | undefined) : undefined;

  const initialRoles = isEdit
    ? ROLES.map((r) => {
        // Prefer the structured model_settings field when present.
        let modelId: string | undefined;
        if (ms) {
          if (r.id === "queen") modelId = ms.primary_model;
          else if (r.id === "scout") modelId = ms.scout_model;
          else if (r.id === "guard") modelId = ms.guard_model || ms.primary_model;
          // worker has no dedicated field in ModelSettings yet;
          // fall through to the legacy adapter value for now.
        }
        if (!modelId) {
          modelId = swarm[r.id] as string | undefined;
        }
        if (!modelId) return r;
        // Legacy bare ids (no slash, no claude-* prefix) were stored on old
        // mock Swarm cards; reconstruct a usable form. Anything already
        // qualified ("provider/model") or claude-* passes through untouched.
        const full =
          modelId.includes("/") || modelId.startsWith("claude-")
            ? modelId
            : `claude-${modelId}`;
        // If the model id is already provider-qualified, surface the
        // provider from the id so the chip / browser don't disagree with
        // it. "crof/foo" → provider=crof.
        const provider = full.includes("/") ? full.split("/", 1)[0] : r.provider;
        return { ...r, model: full, provider };
      })
    : ROLES;

  /* ── Draft restore (pure create mode only; never in edit or clone) ─── */
  const draft = !isEdit && !isClone ? loadDraft() : null;

  const [name, setName] = useState(
    isEdit ? swarm.name : draft?.name ?? "payments-rewrite"
  );
  const [cwd, setCwd] = useState(
    isEdit
      ? (swarm.working_directory || swarm.cwd || FALLBACK_CWD)
      : draft?.cwd ?? FALLBACK_CWD
  );
  const [roles, setRoles] = useState<RoleDef[]>(() => {
    if (isEdit) return initialRoles;
    if (draft) {
      // Merge saved roleModels back into the static ROLES array
      return ROLES.map((r) => {
        const saved = draft.roleModels[r.id];
        if (saved) {
          return { ...r, model: saved.model, provider: saved.provider };
        }
        return r;
      });
    }
    return initialRoles;
  });
  const [roleHive, setRoleHive] = useState<
    Record<string, string | undefined>
  >(() => {
    if (!isEdit) {
      if (draft?.roleHive) {
        return draft.roleHive;
      }
      return { queen: "enhance", scout: "enhance" };
    }
    // Source of truth: `model_settings.hivemind_id` + the per-role `use_*` flags.
    // Treat the literal string "none" (from the legacy mock adapter) as no hivemind.
    const rawId = ms?.hivemind_id ?? swarm.hivemind;
    const hid = !rawId || rawId === "none" ? undefined : (rawId as string);
    const queenOn = ms ? !!ms.use_hivemind_on_queen : !!hid;
    const scoutOn = ms ? !!ms.use_hivemind_on_scout : !!hid;
    return {
      queen: queenOn && hid ? hid : undefined,
      scout: scoutOn && hid ? hid : undefined,
    };
  });

  const [roleThinking, setRoleThinking] = useState<Record<string, string>>(() => {
    const defaults = {
      queen: "high",
      scout: "high",
      worker: "medium",
      guard: "medium",
    };
    if (!isEdit) {
      if (draft?.roleThinking) {
        return { ...defaults, ...draft.roleThinking };
      }
      return defaults;
    }
    if (!ms) return defaults;
    return {
      queen: ms.queen_thinking_level || defaults.queen,
      scout: ms.scout_thinking_level || defaults.scout,
      worker: ms.worker_thinking_level || defaults.worker,
      guard: ms.guard_thinking_level || defaults.guard,
    };
  });
  // Phase 5A Advanced section: per-swarm concurrency cap (1..=6) and
  // optional per-swarm USD budget. Empty string means "unlimited".
  const [maxConcurrentFeatures, setMaxConcurrentFeatures] = useState<string>(() => {
    if (isEdit && ms && typeof ms.max_concurrent_features === "number") {
      return String(ms.max_concurrent_features);
    }
    return "1";
  });
  const [swarmBudgetUsd, setSwarmBudgetUsd] = useState<string>(() => {
    if (isEdit && ms && typeof ms.swarm_budget_usd === "number") {
      return String(ms.swarm_budget_usd);
    }
    return "";
  });
  const [advancedOpen, setAdvancedOpen] = useState<boolean>(
    () => isEdit && (
      (ms?.max_concurrent_features ?? 1) !== 1 ||
      typeof ms?.swarm_budget_usd === "number"
    )
  );
  const [browser, setBrowser] = useState<number | null>(null);
  const [submitting, setSubmitting] = useState(false);
  const [submitError, setSubmitError] = useState<string | null>(null);
  const [loadingEdit, setLoadingEdit] = useState<boolean>(isEdit && isTauri());
  const [tauriHiveminds, setTauriHiveminds] = useState<HivemindSummary[]>([]);
  const userEditedRef = useRef(false);
  // Audit 1.11: when a chosen path isn't in the backend allowlist, show
  // the same "give the AI read/write access to this directory" modal that
  // ProjectPicker uses. `afterApprove` lets a submit handler reuse the
  // same modal — on Allow the original submit re-runs and passes the
  // allowlist check the second time around.
  const [pendingApproval, setPendingApproval] = useState<
    { path: string; afterApprove: () => void | Promise<void> } | null
  >(null);
  const [approving, setApproving] = useState(false);

  // Look up the current allowlist and check whether `path` is approved
  // (exact match or strict descendant of an approved root). Used by the
  // Browse button and by each submit handler.
  const isPathApproved = async (path: string): Promise<boolean> => {
    let approvedRoots: string[] = [];
    try {
      const settings = await ipc.getSettings();
      approvedRoots = settings.approved_working_dirs ?? [];
    } catch {
      // If we can't read the allowlist, treat it as unapproved so the
      // modal pops and the user can explicitly opt in.
      return false;
    }
    // Both sides must be normalized for the comparison — the allowlist may
    // hold `C:\Users\…` (backslashes, from `dunce::canonicalize`) while the
    // freshly-picked path can carry forward slashes or differ in drive-letter
    // case. See `pathForCompare` in ProjectPicker for the rationale.
    return approvedRoots.some((root) => isUnderApprovedRoot(path, root));
  };

  useEffect(() => {
    if (!isTauri()) return;
    let cancelled = false;
    ipc
      .listHiveminds()
      .then((items) => {
        if (!cancelled) setTauriHiveminds(items);
      })
      .catch((err) => console.warn("Failed to load hiveminds", err));
    return () => {
      cancelled = true;
    };
  }, []);

  useEffect(() => {
    // audit 6.7 — read the default path from SettingsProvider's cache.
    if (isEdit || !isTauri()) return;
    if (userEditedRef.current) return;
    if (defaultProjectPath) setCwd(defaultProjectPath);
  }, [isEdit, setCwd, userEditedRef, defaultProjectPath]);

  // Edit mode: load the authoritative `SwarmState` from the backend and
  // reseed form state from `model_settings`. The list adapter
  // (`swarmStateToSwarm`) flattens for display — it can't faithfully
  // represent both hivemind flags or per-role thinking levels — so we
  // never trust the navigation prop as a source of truth in Tauri mode.
  useEffect(() => {
    if (!isEdit || !isTauri() || !swarm?.id) {
      setLoadingEdit(false);
      return;
    }
    let cancelled = false;
    ipc
      .getSwarm(swarm.id)
      .then((s: SwarmState) => {
        if (cancelled) return;
        setName(s.name);
        setCwd(s.working_directory);
        // Prevent the cwd-from-settings effect from clobbering our load.
        userEditedRef.current = true;
        const mss = s.model_settings;
        setRoleHive({
          queen: mss.use_hivemind_on_queen ? mss.hivemind_id ?? undefined : undefined,
          scout: mss.use_hivemind_on_scout ? mss.hivemind_id ?? undefined : undefined,
        });
        setRoleThinking({
          queen: mss.queen_thinking_level || "high",
          scout: mss.scout_thinking_level || "high",
          worker: mss.worker_thinking_level || "medium",
          guard: mss.guard_thinking_level || "medium",
        });
        if (typeof mss.max_concurrent_features === "number") {
          setMaxConcurrentFeatures(String(mss.max_concurrent_features));
        }
        if (typeof mss.swarm_budget_usd === "number") {
          setSwarmBudgetUsd(String(mss.swarm_budget_usd));
          setAdvancedOpen(true);
        } else {
          setSwarmBudgetUsd("");
        }
        if (
          (mss.max_concurrent_features ?? 1) !== 1 ||
          typeof mss.swarm_budget_usd === "number"
        ) {
          setAdvancedOpen(true);
        }
        setRoles((rs) =>
          rs.map((r) => {
            let full: string | null | undefined;
            if (r.id === "queen") full = mss.primary_model;
            else if (r.id === "scout") full = mss.scout_model;
            else if (r.id === "guard")
              full = mss.guard_model ?? mss.primary_model;
            else if (r.id === "worker") full = mss.primary_model;
            else return r;
            if (!full) return r;
            const idx = full.indexOf("/");
            if (idx > 0) {
              return {
                ...r,
                provider: full.slice(0, idx),
                model: full.slice(idx + 1),
              };
            }
            return { ...r, model: full };
          }),
        );
      })
      .catch((err) =>
        console.warn("Failed to load swarm state for edit", err),
      )
      .finally(() => {
        if (!cancelled) setLoadingEdit(false);
      });
    return () => {
      cancelled = true;
    };
  // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [isEdit, swarm?.id]);

  /* ── Debounced draft save (create mode only) ─────────────── */
  const draftSaveTimerRef = useRef<number | null>(null);

  useEffect(() => {
    // Don't save drafts in edit or clone mode — both are source-anchored
    // intents and shouldn't overwrite an unrelated pure-create draft.
    if (isEdit || isClone) return;

    if (draftSaveTimerRef.current != null) {
      clearTimeout(draftSaveTimerRef.current);
    }

    draftSaveTimerRef.current = window.setTimeout(() => {
      draftSaveTimerRef.current = null;
      const roleModels: Record<string, { model: string; provider: string }> = {};
      for (const r of roles) {
        roleModels[r.id] = { model: r.model, provider: r.provider };
      }
      saveDraft({
        name,
        cwd,
        roleModels,
        roleHive,
        roleThinking,
        savedAt: Date.now(),
      });
    }, 500);

    return () => {
      if (draftSaveTimerRef.current != null) {
        clearTimeout(draftSaveTimerRef.current);
      }
    };
  }, [isEdit, isClone, name, cwd, roles, roleHive, roleThinking]);

  /* ── Flush pending draft writes before page unload ──────── */
  useEffect(() => {
    if (isEdit || isClone) return;
    const onUnload = () => {
      if (draftSaveTimerRef.current != null) {
        clearTimeout(draftSaveTimerRef.current);
      }
      const roleModels: Record<string, { model: string; provider: string }> = {};
      for (const r of roles) {
        roleModels[r.id] = { model: r.model, provider: r.provider };
      }
      saveDraft({
        name,
        cwd,
        roleModels,
        roleHive,
        roleThinking,
        savedAt: Date.now(),
      });
    };
    window.addEventListener("beforeunload", onUnload);
    return () => window.removeEventListener("beforeunload", onUnload);
  }, [isEdit, name, cwd, roles, roleHive, roleThinking]);

  const setHiveFor = (id: string, v: string | undefined) =>
    setRoleHive((h) => ({ ...h, [id]: v }));

  // Combine a role's bare `model` id with its `provider` to produce the
  // `provider/model_id` form Pi expects ("crof/mimo-v2.5-pro-precision").
  // Bare ids land in the swarm config when a user only browses by provider
  // and never types a slash themselves; Pi cannot resolve a provider from
  // a bare id and the planning session dies at first send. Roles default
  // to a hardcoded provider, so we can always reconstruct.
  const qualify = (r?: RoleDef): string | null => {
    if (!r) return null;
    const id = r.model || "";
    if (!id) return null;
    if (id.includes("/")) return id;
    return r.provider ? `${r.provider}/${id}` : id;
  };

  // Build the ModelSettings payload from the current form state. Shared
  // by create (handleStartPlanning) and edit (handleSaveChanges) so the
  // two paths can't drift.
  const buildModelSettings = (): ModelSettings => {
    const queenRole = roles.find((r) => r.id === "queen");
    const scoutRole = roles.find((r) => r.id === "scout");
    const guardRole = roles.find((r) => r.id === "guard");
    // Parse advanced fields. Out-of-range / non-numeric values fall back
    // to the safe defaults (1 concurrent feature, no budget cap).
    const parsedConcurrency = Math.max(
      1,
      Math.min(6, Math.floor(Number(maxConcurrentFeatures) || 1))
    );
    const trimmedBudget = swarmBudgetUsd.trim();
    let parsedBudget: number | null = null;
    if (trimmedBudget !== "") {
      const n = Number(trimmedBudget);
      if (Number.isFinite(n) && n >= 0) parsedBudget = n;
    }
    return {
      primary_model: qualify(queenRole) || "",
      scout_model: qualify(scoutRole) || "",
      guard_model: qualify(guardRole),
      scout_thinking_level: roleThinking.scout || "high",
      worker_thinking_level: roleThinking.worker || "medium",
      guard_thinking_level: roleThinking.guard || "medium",
      queen_thinking_level: roleThinking.queen || "high",
      use_hivemind_on_queen: !!roleHive.queen,
      use_hivemind_on_scout: !!roleHive.scout,
      hivemind_id: roleHive.queen || roleHive.scout || null,
      max_concurrent_features: parsedConcurrency,
      swarm_budget_usd: parsedBudget,
    };
  };

  const handleStartPlanning = async () => {
    const queenRole = roles.find((r) => r.id === "queen");
    const scoutRole = roles.find((r) => r.id === "scout");
    if (!queenRole?.model || !scoutRole?.model) {
      setSubmitError("Queen and Scout models are required. Click each role row to select a model from the browser.");
      return;
    }
    if (isTauri()) {
      // Audit 1.11: pre-flight allowlist check so a manually-typed path
      // pops the approval modal instead of falling through to a raw
      // "working directory not approved" backend error.
      if (!(await isPathApproved(cwd))) {
        setPendingApproval({ path: cwd, afterApprove: handleStartPlanning });
        return;
      }
      setSubmitting(true);
      setSubmitError(null);
      try {
        const modelSettings = buildModelSettings();
        const fullQueenModel = modelSettings.primary_model;
        const swarmState = await ipc.createSwarm(name, "", cwd, modelSettings);
        clearDraft();
        // Open the swarm's planning conversation as a Tasks-view task so the
        // user gets the full Tasks UX (reasoning blocks, tool calls, plan
        // delimiters, attachments). The task is marked with `swarmId` so the
        // runtime uses QUEEN_PLANNING_SYSTEM_PROMPT and the PlanCard shows
        // "Launch Swarm" instead of "Implement".
        //
        createTask({
          swarmId: swarmState.id,
          projectPath: cwd,
          model: fullQueenModel || undefined,
          thinking: roleThinking.queen || "high",
          hivemind: roleHive.queen ?? null,
          title: name,
          description: `Swarm planning — ${cwd}`,
          setActive: true,
        });
        go("tasks");
      } catch (e) {
        console.error("Failed to create swarm:", e);
        setSubmitError(e instanceof Error ? e.message : String(e));
      } finally {
        setSubmitting(false);
      }
    } else {
      // Mock mode: still create a task so the layout matches Tauri.
      clearDraft();
      createTask({
        swarmId: `mock-${Date.now()}`,
        projectPath: cwd,
        model: roles.find((r) => r.id === "queen")?.model,
        thinking: roleThinking.queen || "high",
        hivemind: roleHive.queen ?? null,
        title: name,
        description: `Swarm planning — ${cwd}`,
        setActive: true,
      });
      go("tasks");
    }
  };

  /** Clone-mode submit: create a fresh swarm shell, then call startSwarm
   *  with the cloned features+milestones immediately. No planning task is
   *  created — Queen orchestration is skipped and Scout/Worker/Guard begin
   *  on the next tick. Synthetic validator features are stripped from the
   *  payload (the Queen orchestrator re-injects them via
   *  inject_milestone_validators on start_swarm). */
  const handleStartClone = async () => {
    if (!clonedPlan) return;
    const queenRole = roles.find((r) => r.id === "queen");
    const scoutRole = roles.find((r) => r.id === "scout");
    if (!queenRole?.model || !scoutRole?.model) {
      setSubmitError(
        "Queen and Scout models are required. Click each role row to select a model from the browser.",
      );
      return;
    }
    if (!isTauri()) {
      // Mock mode: nothing to launch; just bounce back to the swarms list.
      go("swarms");
      return;
    }
    if (!(await isPathApproved(cwd))) {
      setPendingApproval({ path: cwd, afterApprove: handleStartClone });
      return;
    }
    setSubmitting(true);
    setSubmitError(null);
    try {
      const modelSettings = buildModelSettings();
      const swarmState = await ipc.createSwarm(name, "", cwd, modelSettings);
      clearDraft();
      const featuresInput = clonedPlan.features
        .filter((f) => !f.id.startsWith("validate-"))
        .map((f) => ({
          id: f.id,
          name: f.name,
          description: f.description,
          dependencies: f.dependencies,
          milestone: f.milestone,
        }));
      const milestonesInput = clonedPlan.milestones.map((m) => ({
        id: m.id,
        name: m.name,
        features: m.features,
        assertions: m.assertions,
      }));
      await ipc.startSwarm(swarmState.id, featuresInput, milestonesInput);
      go("swarm-control", { swarm: swarmState });
    } catch (e) {
      console.error("Failed to clone-and-start swarm:", e);
      setSubmitError(e instanceof Error ? e.message : String(e));
    } finally {
      setSubmitting(false);
    }
  };

  const handleSaveChanges = async () => {
    const queenRole = roles.find((r) => r.id === "queen");
    const scoutRole = roles.find((r) => r.id === "scout");
    if (!queenRole?.model || !scoutRole?.model) {
      setSubmitError("Queen and Scout models are required. Click each role row to select a model from the browser.");
      return;
    }
    if (!isTauri() || !swarm?.id) {
      go("swarms");
      return;
    }
    if (!(await isPathApproved(cwd))) {
      setPendingApproval({ path: cwd, afterApprove: handleSaveChanges });
      return;
    }
    setSubmitting(true);
    setSubmitError(null);
    try {
      await ipc.updateSwarm(swarm.id, name, cwd, buildModelSettings());
      go("swarms");
    } catch (e) {
      console.error("Failed to update swarm:", e);
      setSubmitError(e instanceof Error ? e.message : String(e));
    } finally {
      setSubmitting(false);
    }
  };

  return (
    <div className="h-full overflow-auto">
      <div className="max-w-[960px] mx-auto px-8 py-7">
        <div className="mb-6">
          <button
            onClick={() => go("swarms")}
            className="text-muted hover:text-white text-[12px] flex items-center gap-1.5 mb-3"
          >
            {I.chevR({ size: 12, className: "rotate-180" })} Back to Swarms
          </button>
          <h1 className="text-[26px] font-bold tracking-tight">
            {isEdit ? "Edit Swarm" : isClone ? "Clone Swarm" : "Configure a swarm"}
          </h1>
          <p className="text-muted text-[13.5px] mt-1">
            {isEdit
              ? "Update agent models, working directory, and hivemind review for this swarm."
              : isClone
                ? "Pick new identity, agents, and working directory. Plan is locked from the source swarm — Queen planning will be skipped."
                : "Pick agent models, point at a working directory, and decide whether the Queen and Scout consult a hivemind before committing."}
          </p>
        </div>

        {/* Clone banner + locked plan preview */}
        {isClone && clonedPlan && (() => {
          const humanFeatures = clonedPlan.features.filter(
            (f) => !f.id.startsWith("validate-"),
          );
          const milestoneCount = clonedPlan.milestones.length;
          return (
            <div className="mb-6">
              <div className="rounded-lg border border-honey-500/40 bg-honey-500/10 px-4 py-3 flex items-center justify-between gap-4">
                <div className="flex items-center gap-2.5 text-[13px] text-honey-200">
                  {I.copy({ size: 15, className: "text-honey-400 shrink-0" })}
                  <span>
                    Cloning plan from{" "}
                    <span className="font-mono">{clonedPlan.sourceSwarmName}</span>{" "}
                    — {humanFeatures.length} features, {milestoneCount} milestones.
                    Queen planning will be skipped.
                  </span>
                </div>
                <Btn
                  kind="ghost"
                  size="sm"
                  onClick={() => go("swarms")}
                >
                  Cancel clone
                </Btn>
              </div>
              {milestoneCount === 0 && (
                <div className="mt-2 text-[11.5px] text-muted">
                  No milestones — Guard validation will be disabled for this swarm.
                </div>
              )}
              <details className="mt-3 rounded-lg border border-line bg-ink-850 overflow-hidden">
                <summary className="cursor-pointer select-none px-4 py-2.5 text-[12.5px] text-white/85 hover:bg-ink-800/40 list-none flex items-center justify-between">
                  <span>Preview plan ({humanFeatures.length} features, {milestoneCount} milestones)</span>
                  <span className="text-dim text-[11px]">click to expand</span>
                </summary>
                <div className="border-t border-line/60 px-4 py-3.5 space-y-3">
                  {milestoneCount > 0 && (
                    <div>
                      <div className="text-[10.5px] uppercase tracking-wider text-muted font-semibold mb-1.5">
                        Milestones
                      </div>
                      <ul className="space-y-1">
                        {clonedPlan.milestones.map((m) => (
                          <li key={m.id} className="text-[12px] text-white/85">
                            <span className="font-mono text-honey-300">{m.id}</span>
                            <span className="text-dim"> — </span>
                            <span>{m.name}</span>
                            <span className="text-dim text-[11px]">
                              {" · "}
                              {m.assertions.length} assertions · {m.features.length} features
                            </span>
                          </li>
                        ))}
                      </ul>
                    </div>
                  )}
                  <div>
                    <div className="text-[10.5px] uppercase tracking-wider text-muted font-semibold mb-1.5">
                      Features
                    </div>
                    <ul className="space-y-1">
                      {humanFeatures.map((f) => (
                        <li key={f.id} className="text-[12px] text-white/85">
                          <span className="font-mono text-honey-300">{f.id}</span>
                          <span className="text-dim"> · </span>
                          <span>{f.name}</span>
                          {f.milestone && (
                            <>
                              <span className="text-dim"> → </span>
                              <span className="font-mono text-dim text-[11px]">
                                {f.milestone}
                              </span>
                            </>
                          )}
                          {f.description && (
                            <div className="text-[11px] text-muted leading-snug pl-3">
                              {f.description.length > 80
                                ? `${f.description.slice(0, 80)}…`
                                : f.description}
                            </div>
                          )}
                        </li>
                      ))}
                    </ul>
                  </div>
                </div>
              </details>
            </div>
          );
        })()}

        {/* Identity */}
        <Section title="01 · Identity" subtitle="Name and target directory">
          <div className="grid grid-cols-2 gap-4">
            <Field label="Swarm name">
              <Input
                value={name}
                onChange={(e) => setName(e.target.value)}
                placeholder="payments-rewrite"
              />
            </Field>
            <Field label="Working directory">
              <div className="flex gap-2">
                <Input
                  value={cwd}
                  onChange={(e) => {
                    userEditedRef.current = true;
                    setCwd(e.target.value);
                  }}
                  icon={I.folder({ size: 13 })}
                  wrapClass="flex-1"
                  className="font-mono"
                />
                <Btn
                  kind="outline"
                  size="md"
                  onClick={async () => {
                    if (!isTauri()) {
                      console.warn("Folder picker is only available in Tauri desktop mode");
                      return;
                    }
                    try {
                      const selected = await openFolderDialog({
                        directory: true,
                        multiple: false,
                        title: "Select working directory",
                      });
                      if (!selected) return;
                      const picked = selected as string;
                      if (await isPathApproved(picked)) {
                        userEditedRef.current = true;
                        setCwd(picked);
                        return;
                      }
                      setPendingApproval({
                        path: picked,
                        afterApprove: () => {
                          userEditedRef.current = true;
                          setCwd(picked);
                        },
                      });
                    } catch (err) {
                      console.warn("Folder picker dialog failed", err);
                    }
                  }}
                >
                  Browse
                </Btn>
              </div>
            </Field>
          </div>
        </Section>

        {/* Agents */}
        <Section
          title="02 · Agent models"
          subtitle="Each role gets its own model + provider. Queen and Scout can optionally consult a hivemind before handing off."
        >
          <div className="rounded-lg border border-line bg-ink-850 overflow-hidden">
            <div className="grid grid-cols-[140px_1fr_30px] gap-3 px-4 py-2.5 text-[10.5px] uppercase tracking-wider text-dim font-semibold border-b border-line bg-ink-800/40">
              <div>Role</div>
              <div>Model</div>
              <div></div>
            </div>
            {roles.map((r, i) => {
              const hive = roleHive[r.id];
              const reviewing = !!hive;
              return (
                <div
                  key={r.id}
                  className="border-b border-line/50 last:border-0"
                >
                  <div className="grid grid-cols-[140px_1fr_30px] gap-3 items-center px-4 py-2.5">
                    <div className="flex items-center gap-2">
                      <span className="text-base">{r.icon}</span>
                      <div>
                        <div className="text-[13px] font-semibold text-white">
                          {r.label}
                        </div>
                        <div className="text-[10.5px] text-dim leading-tight">
                          {r.desc}
                        </div>
                      </div>
                    </div>
                    <button
                      onClick={() => setBrowser(i)}
                      className="h-8 px-2.5 rounded-md bg-ink-900 border border-line hover:border-honey-500/40 text-left flex items-center justify-between gap-2 group"
                    >
                      <span className="font-mono text-[12.5px] text-white/85 truncate">
                        {r.model || <span className="text-dim italic text-[11px]">Select a model</span>}
                      </span>
                      <span className="text-[10.5px] text-muted ml-1.5">{r.provider}</span>
                      <span className="text-[10px] text-honey-300/70 ml-1">{roleThinking[r.id] || "—"}</span>
                      {I.search({ size: 13, className: "text-dim group-hover:text-honey-400" })}
                    </button>
                    <button className="text-dim hover:text-white">
                      {I.chevD({ size: 14 })}
                    </button>
                  </div>

                  {r.canReview && (
                    <div className="px-4 pb-3 -mt-0.5">
                      <div
                        className={`rounded-md border ${
                          reviewing
                            ? "border-honey-500/30 bg-honey-500/[0.04]"
                            : "border-line/60 bg-ink-900/40"
                        } px-3 py-2`}
                      >
                        <div className="flex items-center gap-3">
                          <button
                            type="button"
                            onClick={() =>
                              setHiveFor(
                                r.id,
                                reviewing
                                  ? undefined
                                  : isTauri()
                                    ? tauriHiveminds[0]?.id
                                    : "enhance"
                              )
                            }
                            className={`w-4 h-4 rounded border flex items-center justify-center shrink-0 ${
                              reviewing
                                ? "bg-honey-500 border-honey-400"
                                : "border-line-strong bg-ink-900"
                            }`}
                          >
                            {reviewing &&
                              I.check({
                                size: 10,
                                className: "text-ink-900",
                                sw: 2.5,
                              })}
                          </button>
                          <div className="flex items-center gap-1.5 text-[11.5px]">
                            {I.hexFill({
                              size: 11,
                              className: "text-honey-500",
                            })}
                            <span className="text-muted">
                              Review with hivemind
                            </span>
                            <span className="text-dim">{"·"}</span>
                            <span className="text-dim text-[11px]">
                              {r.reviewSub}
                            </span>
                          </div>
                          <div className="flex-1" />
                          <Select
                            value={hive || ""}
                            onChange={(e) =>
                              setHiveFor(
                                r.id,
                                e.target.value || undefined
                              )
                            }
                            options={[
                              {
                                value: "",
                                label: "None \u2014 skip review",
                              },
                              ...(isTauri()
                                ? tauriHiveminds.map((h) => ({
                                    value: h.id,
                                    label: h.name,
                                  }))
                                : HIVEMINDS.map((h) => ({
                                    value: h.id,
                                    label: h.name,
                                  }))),
                            ]}
                            className="w-[200px]"
                          />
                        </div>
                      </div>
                    </div>
                  )}
                </div>
              );
            })}
          </div>
        </Section>

        {/* Advanced — execution + budget knobs (Phase 5A). Closed by
            default; opens automatically in edit mode when any advanced
            field deviates from the defaults. */}
        <Section
          title="03 · Advanced"
          subtitle="Concurrency, budget caps. Defaults are safe — open only if you need to override."
        >
          <div className="rounded-lg border border-line bg-ink-850 overflow-hidden">
            <button
              type="button"
              onClick={() => setAdvancedOpen((v) => !v)}
              className="w-full px-4 py-2.5 flex items-center justify-between hover:bg-ink-800/40"
              aria-expanded={advancedOpen}
            >
              <span className="text-[12.5px] text-white/85">
                {advancedOpen ? "Hide advanced settings" : "Show advanced settings"}
              </span>
              <span
                className={`text-dim transition-transform ${
                  advancedOpen ? "rotate-180" : ""
                }`}
              >
                {I.chevD({ size: 14 })}
              </span>
            </button>
            {advancedOpen && (
              <div className="border-t border-line/60 px-4 py-3.5 space-y-3">
                <div className="grid grid-cols-2 gap-4">
                  <Field label="Parallel features (1 - 6)">
                    <Input
                      type="number"
                      min={1}
                      max={6}
                      step={1}
                      aria-label="Max concurrent features"
                      value={maxConcurrentFeatures}
                      onChange={(e) =>
                        setMaxConcurrentFeatures(e.target.value)
                      }
                      className="font-mono"
                      placeholder="1"
                    />
                  </Field>
                  <Field label="Per-swarm budget (USD, optional)">
                    <Input
                      type="number"
                      min={0}
                      step={0.01}
                      aria-label="Per-swarm budget USD"
                      value={swarmBudgetUsd}
                      onChange={(e) => setSwarmBudgetUsd(e.target.value)}
                      className="font-mono"
                      placeholder="unlimited"
                    />
                  </Field>
                </div>
                <div className="text-[11.5px] text-muted leading-relaxed">
                  Default 1 is sequential; the bee-colony mental model fires
                  one feature at a time. Per-swarm budget pauses the swarm
                  once lifetime cost meets the cap; leave blank for unlimited.
                </div>
              </div>
            )}
          </div>
        </Section>

        {/* Footer */}
        <div className="flex items-center justify-end mt-7 pt-5 border-t border-line">
          <div className="flex items-center gap-2">
            <Btn kind="ghost" onClick={() => go("swarms")}>
              Cancel
            </Btn>
            {submitError && (
              <span className="text-[12px] text-red-400 mr-2">{submitError}</span>
            )}
            {isEdit ? (
              <Btn
                kind="primary"
                icon={I.check({ size: 14 })}
                onClick={handleSaveChanges}
                disabled={submitting || loadingEdit}
              >
                {loadingEdit
                  ? "Loading…"
                  : submitting
                    ? "Saving..."
                    : "Save changes"}
              </Btn>
            ) : isClone ? (
              <Btn
                kind="primary"
                icon={I.play({ size: 14 })}
                onClick={handleStartClone}
                disabled={submitting}
              >
                {submitting ? "Starting..." : "Start Swarm \u2192"}
              </Btn>
            ) : (
              <Btn
                kind="primary"
                icon={I.crown({ size: 14 })}
                onClick={handleStartPlanning}
                disabled={submitting}
              >
                {submitting ? "Creating..." : "Start Planning \u2192"}
              </Btn>
            )}
          </div>
        </div>
      </div>

      <ModelBrowserModal
        open={browser !== null}
        onClose={() => setBrowser(null)}
        initialProvider={browser !== null ? roles[browser].provider : undefined}
        onSelect={(model, opts) => {
          if (browser !== null) {
            setRoles((rs) =>
              rs.map((x, j) =>
                j !== browser ? x : { ...x, model: model.id, provider: model.provider }
              )
            );
            if (opts?.thinking) {
              const roleId = roles[browser].id;
              setRoleThinking((prev) => ({ ...prev, [roleId]: opts.thinking }));
            }
          }
          setBrowser(null);
        }}
      />

      {pendingApproval && (
        <>
          <div
            className="fixed inset-0 z-[60] bg-ink-950/70"
            onClick={approving ? undefined : () => setPendingApproval(null)}
          />
          <div
            role="dialog"
            aria-modal="true"
            aria-labelledby="ns-wd-approval-title"
            className="fixed inset-0 z-[61] flex items-center justify-center p-4"
            onClick={(e) => e.stopPropagation()}
          >
            <div className="bg-ink-800 border border-line rounded-xl shadow-2xl w-full max-w-md overflow-hidden">
              <div className="px-5 py-4 border-b border-line">
                <h2
                  id="ns-wd-approval-title"
                  className="text-base font-semibold text-slate-100"
                >
                  Approve working directory?
                </h2>
              </div>
              <div className="px-5 py-4 space-y-3">
                <p className="text-sm text-slate-300 leading-relaxed">
                  Hyvemind will give the AI read/write access to this directory
                  and all of its descendants. Only approve project folders you
                  actively trust the AI to modify.
                </p>
                <div className="bg-ink-900 border border-line rounded-lg px-3 py-2 text-xs font-mono text-slate-200 break-all">
                  {pendingApproval.path}
                </div>
              </div>
              <div className="px-5 py-3 border-t border-line flex items-center justify-end gap-2">
                <button
                  type="button"
                  onClick={() => setPendingApproval(null)}
                  disabled={approving}
                  className="px-3 py-1.5 rounded-lg text-sm text-slate-300 hover:bg-ink-700/60 transition-colors cursor-pointer disabled:opacity-50"
                >
                  Cancel
                </button>
                <button
                  type="button"
                  onClick={async () => {
                    if (!pendingApproval) return;
                    setApproving(true);
                    try {
                      await ipc.requestWorkingDirApproval(pendingApproval.path);
                      const cb = pendingApproval.afterApprove;
                      setPendingApproval(null);
                      // Run the original caller (sets cwd for Browse, or
                      // re-invokes the submit handler — which now passes
                      // the allowlist check and proceeds).
                      await cb();
                    } catch (err) {
                      setSubmitError(
                        err instanceof Error ? err.message : String(err),
                      );
                    } finally {
                      setApproving(false);
                    }
                  }}
                  disabled={approving}
                  className="px-3 py-1.5 rounded-lg text-sm font-medium bg-honey-500 text-ink-950 hover:bg-honey-400 transition-colors cursor-pointer disabled:opacity-50"
                >
                  {approving ? "Approving…" : "Allow"}
                </button>
              </div>
            </div>
          </div>
        </>
      )}
    </div>
  );
}
