import React, { useState, useCallback, useMemo, useEffect, useContext, useRef } from "react";
import * as Sentry from "@sentry/react";
import { I } from "./components/icons";
import { ProjectContext, loadSavedProjects, projectFromPath, pathForCompare } from "./components/ProjectPicker";
import { ErrorModalProvider } from "./components/ErrorModal";
import { ToastProvider } from "./components/Toast";
import { PROJECTS, SWARMS, Project } from "./data/mock";
import {
  isTauri,
  isMac,
  MAC_TRAFFIC_LIGHT_GUTTER_PX,
} from "./lib/platform";
import * as ipc from "./lib/ipc";
import { TaskRuntimeProvider, ACTIVE_TASK_KEY, TASK_LIST_KEY } from "./lib/taskRuntime";
import { SettingsProvider, useSettings } from "./lib/SettingsProvider";
import { ProvidersProvider } from "./lib/ProvidersProvider";
import { NurseProvider } from "./lib/NurseProvider";
import { updateCompletionSoundConfig } from "./lib/sounds";
import { getStoredZoom, saveZoom, applyZoom, zoomIn, zoomOut, zoomReset } from "./lib/zoom";
import { QuickTaskDialog } from "./components/QuickTaskDialog";
import { InspectorOverlay } from "./components/InspectorOverlay";
import { ShortcutCheatsheet } from "./components/ShortcutCheatsheet";
import { ContextMenuProvider } from "./components/ContextMenu";
import { NurseDropdown } from "./components/NurseDropdown";
import { SessionsDropdown } from "./components/SessionsDropdown";
import { ExtensionProvider } from "./extensions/ExtensionProvider";
import { ExtensionTopbarSlot, registerWidgets } from "./extensions/registry";
import { TestRunProvider } from "./state/TestRunProvider";

// Register bespoke extension widgets once at module load (no React state
// is consulted; this is safe to call top-level).
registerWidgets();

import { TasksScreen } from "./screens/Tasks";
import { SwarmsScreen } from "./screens/Swarms";
import { NewSwarmScreen } from "./screens/NewSwarm";
import { SwarmControlScreen } from "./screens/SwarmControl";
import { HivemindsScreen } from "./screens/Hiveminds";
import { HivemindEditScreen } from "./screens/HivemindEdit";
import { ModelBrowserScreen } from "./screens/ModelBrowser";
import { SettingsScreen } from "./screens/Settings";
import { ReviewHistoryScreen } from "./screens/ReviewHistory";
import { DashboardScreen } from "./screens/Dashboard";
import { TestsScreen } from "./screens/Tests";
import { NurseScreen } from "./screens/Nurse";

/* ── Types ─────────────────────────────────────────────────── */

export type GoFn = (tab: string, params?: Record<string, any>) => void;
export type NavState = { tab: string; params: Record<string, any> };

/* ── Pi gate ──────────────────────────────────────────────────
 * Pi is the external coding agent subprocess every Task / Hivemind / Swarm
 * spawns. Without it on PATH, nothing in the product works, so we lock the
 * nav until the user installs it from Settings.
 */

type PiGate = {
  installed: boolean;
  loading: boolean;
  refresh: () => Promise<void>;
};

const PiStatusContext = React.createContext<PiGate>({
  installed: true,
  loading: false,
  refresh: async () => {},
});

export const usePiGate = () => useContext(PiStatusContext);

const LOCKED_WHEN_NO_PI = new Set([
  "dashboard",
  "tasks",
  "swarms",
  "new-swarm",
  "swarm-control",
  "hiveminds",
  "hivemind-edit",
  "model-browser",
  "review-history",
  "nurse",
]);

/* ── renderMd ──────────────────────────────────────────────── */

import { Markdown } from "./components/Markdown";

export function renderMd(text: string, variant: "assistant" | "user" = "assistant") {
  return <Markdown text={text} variant={variant} />;
}

/* ── Nav items ─────────────────────────────────────────────── */

const NAV = [
  { tab: "dashboard", label: "Dashboard", icon: I.dashboard },
  { tab: "tasks", label: "Tasks", icon: I.rocket },
  { tab: "swarms", label: "Swarms", icon: I.swarm },
  { tab: "hiveminds", label: "Hiveminds", icon: I.hex },
  { tab: "nurse", label: "Nurse", icon: I.heart },
  { tab: "tests", label: "Tests", icon: I.flask },
  { tab: "settings", label: "Settings", icon: I.gear },
];

/**
 * Returns the NAV entries that should be visible in the sidebar / keyboard
 * shortcuts. The "tests" tab is debug-only — it's hidden unless
 * `settings.debug_mode === true` (driven by the `HYVEMIND_DEBUG=1` env
 * var; see `commands/settings.rs`). While the first `getSettings()` IPC
 * call is still in flight (`settings === null`) we treat the app as
 * non-debug to avoid a one-frame flash of the Tests tab in release
 * builds.
 */
function useVisibleNav() {
  const { settings } = useSettings();
  const debug = settings?.debug_mode === true;
  return useMemo(
    () => NAV.filter((item) => item.tab !== "tests" || debug),
    [debug],
  );
}

/* ── Sidebar ───────────────────────────────────────────────── */

function parentTab(tab: string): string {
  if (tab === "swarm-control" || tab === "new-swarm") return "swarms";
  if (tab === "review-history") return "hiveminds";
  return tab;
}

function pageLabel(tab: string): string {
  const m: Record<string, string> = {
    dashboard: "Dashboard",
    tasks: "Tasks",
    swarms: "Swarms",
    "new-swarm": "New Swarm",
    "swarm-control": "Swarm Control",
    hiveminds: "Hiveminds",
    "hivemind-edit": "Hivemind Edit",
    "model-browser": "Model Browser",
    settings: "Settings",
    tests: "Tests",
    "review-history": "Review History",
    nurse: "Nurse",
  };
  return m[tab] || tab;
}

function Sidebar({ nav, go, piInstalled }: { nav: NavState; go: GoFn; piInstalled: boolean }) {
  const pt = parentTab(nav.tab);
  const visibleNav = useVisibleNav();

  return (
    <aside className="w-[84px] shrink-0 bg-ink-950/80 border-r border-line flex flex-col items-stretch">
      {/* Logo */}
      <button
        onClick={() => { if (piInstalled) go("dashboard"); }}
        className="h-12 flex items-center justify-center border-b border-line"
        disabled={!piInstalled}
        title={piInstalled ? undefined : "Install Pi to unlock"}
      >
        <img src="/icon.png" alt="Hyvemind" className={`w-8 h-8 rounded-lg ${piInstalled ? "" : "opacity-40"}`} />
      </button>

      <nav className="flex-1 py-3 flex flex-col gap-1 px-2">
        {visibleNav.map((item) => {
          const active = pt === item.tab;
          const locked = !piInstalled && item.tab !== "settings";
          return (
            <button
              key={item.tab}
              onClick={() => { if (!locked) go(item.tab); }}
              disabled={locked}
              title={locked ? "Install Pi to unlock" : undefined}
              className={`group relative h-[64px] rounded-lg flex flex-col items-center justify-center gap-1.5 transition-colors ${
                locked
                  ? "opacity-40 cursor-not-allowed text-muted"
                  : active
                    ? "bg-honey-500/10 text-honey-300"
                    : "text-muted hover:text-white hover:bg-ink-700/60"
              }`}
            >
              {active && !locked && (
                <span className="absolute left-0 top-2.5 bottom-2.5 w-[2.5px] rounded-r bg-honey-500" />
              )}
              {item.icon({ size: 20, className: active && !locked ? "text-honey-400" : "" })}
              <span className={`text-[10.5px] font-medium tracking-wide ${active && !locked ? "text-honey-200" : ""}`}>
                {item.label}
              </span>
            </button>
          );
        })}
      </nav>
    </aside>
  );
}

/* ── Topbar ─────────────────────────────────────────────────── */

function Topbar({
  nav,
  go,
  onOpenQuickTask,
  inspectorOn,
  onToggleInspector,
  onOpenNurseSettings,
}: {
  nav: NavState;
  go: GoFn;
  onOpenQuickTask: () => void;
  inspectorOn: boolean;
  onToggleInspector: () => void;
  onOpenNurseSettings?: () => void;
}) {
  type Crumb = { label: string; onClick?: () => void };
  const crumbs: Crumb[] = useMemo(() => {
    const tab = nav.tab;
    if (tab === "dashboard")      return [{ label: "Dashboard" }];
    if (tab === "tasks")          return [{ label: "Tasks" }];
    if (tab === "hiveminds")      return [{ label: "Hiveminds" }];
    if (tab === "swarms")         return [{ label: "Swarms" }];
    if (tab === "settings")       return [{ label: "Settings" }];
    if (tab === "tests")           return [{ label: "Tests" }];
    if (tab === "nurse")          return [{ label: "Nurse" }];
    if (tab === "new-swarm")      return [{ label: "Swarms", onClick: () => go("swarms") }, { label: nav.params.edit ? `Edit ${nav.params.swarm?.name || "Swarm"}` : "New Swarm" }];
    if (tab === "swarm-control")  return [{ label: "Swarms", onClick: () => go("swarms") }, { label: nav.params.swarm?.name || "Swarm" }];
    if (tab === "review-history") return [{ label: "Hiveminds", onClick: () => go("hiveminds") }, { label: "All Reviews" }];
    return [{ label: tab }];
  }, [nav, go]);

  const [costToday, setCostToday] = useState<number>(0);

  // Only mark explicit non-interactive zones as drag regions.
  // Do not wrap buttons/dropdowns/overlays in a drag region; Tauri drag
  // behavior is attribute-presence based and can interfere with clicks.
  const hasMacTrafficLights = isTauri() && isMac();
  const [nurseDropdownOpen, setNurseDropdownOpen] = useState(false);
  const [sessionsDropdownOpen, setSessionsDropdownOpen] = useState(false);
  const dragRegionProps =
    hasMacTrafficLights && !nurseDropdownOpen && !sessionsDropdownOpen
      ? { "data-tauri-drag-region": true }
      : {};

  useEffect(() => {
    if (!isTauri()) return;

    let cancelled = false;
    let timer: number | undefined;

    const tick = async () => {
      try {
        const stats = await ipc.getDashboardStats();
        if (cancelled) return;
        setCostToday(stats.cost_today ?? 0);
      } catch (err) {
        if (!cancelled) console.error("Failed to poll dashboard stats:", err);
      }
      if (!cancelled) timer = window.setTimeout(tick, 5_000);
    };

    // Start polling immediately, but only when the window is visible.
    const onVisibility = () => {
      if (document.hidden) {
        if (timer) { clearTimeout(timer); timer = undefined; }
      } else if (!timer && !cancelled) {
        tick();
      }
    };

    tick();
    document.addEventListener("visibilitychange", onVisibility);

    return () => {
      cancelled = true;
      if (timer) clearTimeout(timer);
      document.removeEventListener("visibilitychange", onVisibility);
    };
  }, []);

  return (
    <header
      {...dragRegionProps}
      className="h-12 shrink-0 border-b border-line bg-gradient-to-b from-ink-900/95 to-ink-900/75 backdrop-blur shadow-[inset_0_-1px_0_rgba(245,185,25,0.06)] flex items-center pr-4 gap-3 relative z-50"
      style={{ paddingLeft: hasMacTrafficLights ? MAC_TRAFFIC_LIGHT_GUTTER_PX : 20 }}
    >
      {/*
       * macOS traffic-light gutter. The native traffic lights float above the
       * webview at trafficLightPosition.{x,y}. We reserve space on the left of
       * the Topbar so the wordmark sits to the right of those buttons.
       *
       * TODO: When Tauri fullscreen on macOS hides the traffic lights, this
       * gutter remains visible. Revisit with fullscreen/resize events if the
       * empty gap feels wrong in fullscreen.
       */}
      {hasMacTrafficLights && (
        <div
          {...dragRegionProps}
          aria-hidden="true"
          className="absolute left-0 top-0 h-full"
          style={{ width: MAC_TRAFFIC_LIGHT_GUTTER_PX }}
        />
      )}

      {/* Wordmark — normal button, not a drag region. */}
      {/* 3px visual breathing room inside the gutter — keep in sync if GUTTER_PX changes */}
      <button data-tauri-drag-region={false} onClick={() => go("dashboard")} className="flex items-center gap-2.5 mr-2 ml-[3px]">
        <span className="text-[13px] font-extrabold tracking-tight text-honey-400" style={{ letterSpacing: "-0.01em" }}>
          Hyvemind
        </span>
      </button>

      {/* Breadcrumbs */}
      <div className="flex items-center gap-2 text-[12px] min-w-0">
        {crumbs.map((c, i) => {
          const isLast = i === crumbs.length - 1;
          return (
            <React.Fragment key={i}>
              {i > 0 && <span {...dragRegionProps} className="text-dim">/</span>}
              {c.onClick && !isLast ? (
                <button data-tauri-drag-region={false} onClick={c.onClick} className="text-muted hover:text-white truncate">{c.label}</button>
              ) : (
                <span
                  {...dragRegionProps}
                  className={isLast ? "text-white font-medium truncate" : "text-muted truncate"}
                >
                  {c.label}
                </span>
              )}
            </React.Fragment>
          );
        })}
      </div>

      <div {...dragRegionProps} className="flex-1 h-full" />

      {/* Right cluster */}
      <div className="flex items-center gap-2 text-[12px]">
        <div data-tauri-drag-region={false} className="flex items-center">
          <ExtensionTopbarSlot />
        </div>
        {import.meta.env.DEV && (
          <button
            data-tauri-drag-region={false}
            data-no-inspect
            onClick={onToggleInspector}
            title={inspectorOn ? "Inspector ON — click element to capture, Esc to cancel" : "Toggle inspector (dev only)"}
            aria-label={inspectorOn ? "Inspector active — click element to capture" : "Toggle inspector"}
            className={`flex items-center justify-center w-7 h-7 rounded-md border transition ${
              inspectorOn
                ? "border-honey-500 bg-honey-500/20 text-honey-200"
                : "border-line bg-ink-850 text-muted hover:text-white hover:border-line-strong"
            }`}
          >
            {I.crosshair({ size: 12 })}
          </button>
        )}
        <button
          data-tauri-drag-region={false}
          onClick={onOpenQuickTask}
          className="flex items-center gap-1.5 px-2.5 h-7 rounded-md border border-honey-500/40 bg-honey-500/10 text-honey-200 hover:bg-honey-500/15 transition"
          title={`Quick Task (C or ${isMac() ? "⌘⇧T" : "Ctrl+Shift+T"})`}
        >
          {I.rocket({ size: 11, className: "text-honey-400" })}
          <span className="text-[11px] font-medium">Quick Task</span>
        </button>
        <div className="flex items-center gap-1.5 text-muted px-2.5 h-7 rounded-md border border-line bg-ink-850 font-mono">
          {I.cost({ size: 11, className: "text-honey-400" })}
          <span className="text-white/85">{isTauri() ? `$${costToday.toFixed(2)}` : "$8.42"}</span>
          <span className="text-dim">today</span>
        </div>
        <div data-tauri-drag-region={false} className="flex items-center">
          <SessionsDropdown onOpenChange={setSessionsDropdownOpen} />
        </div>
        <div data-tauri-drag-region={false} className="flex items-center">
          <NurseDropdown
            onOpenSettings={onOpenNurseSettings}
            onOpenChange={setNurseDropdownOpen}
          />
        </div>
      </div>
    </header>
  );
}

/* ── Screen Router ─────────────────────────────────────────── */

function ScreenRouter({ nav, go, zoomLevel, onZoomChange }: { nav: NavState; go: GoFn; zoomLevel: number; onZoomChange: (level: number) => void }) {
  switch (nav.tab) {
    case "dashboard":
      return <DashboardScreen go={go} />;
    case "tasks":
      return <TasksScreen go={go} prefill={nav.params.prefill} />;
    case "swarms":
      return <SwarmsScreen go={go} />;
    case "new-swarm":
      return <NewSwarmScreen go={go} swarm={nav.params.swarm} edit={nav.params.edit} clonedPlan={nav.params.clonedPlan} />;
    case "swarm-control":
      return <SwarmControlScreen go={go} swarm={nav.params.swarm} />;
    case "hiveminds":
      return <HivemindsScreen go={go} />;
    case "hivemind-edit":
      return <HivemindEditScreen go={go} />;
    case "model-browser":
      return <ModelBrowserScreen go={go} params={nav.params} />;
    case "settings":
      return <SettingsScreen go={go} zoomLevel={zoomLevel} onZoomChange={onZoomChange} />;
    case "tests":
      return <TestsScreen go={go} />;
    case "nurse":
      return <NurseScreen go={go} />;
    case "review-history":
      return <ReviewHistoryScreen go={go} hivemind={nav.params.hivemind} />;
    default:
      return <DashboardScreen go={go} />;
  }
}

/* ── App ───────────────────────────────────────────────────── */

export function App() {
  const [nav, setNav] = useState<NavState>({ tab: "dashboard", params: {} });
  const [quickOpen, setQuickOpen] = useState(false);
  const [quickPrefill, setQuickPrefill] = useState<string>("");
  const [inspectorOn, setInspectorOn] = useState(false);
  const [cheatsheetOpen, setCheatsheetOpen] = useState(false);
  const [zoomLevel, setZoomLevel] = useState<number>(getStoredZoom);
  const zoomRef = useRef(zoomLevel);
  const initProjects = isTauri() ? loadSavedProjects() : PROJECTS;
  // Pick the initial picker value from the task that's about to be
  // active, so the first frame after launch shows that task's project
  // rather than whatever happened to be first in the persisted list.
  const initProject = (() => {
    if (!isTauri()) return PROJECTS[0] || null;
    try {
      const list = JSON.parse(localStorage.getItem(TASK_LIST_KEY) || "[]");
      const activeId = localStorage.getItem(ACTIVE_TASK_KEY);
      const active = Array.isArray(list)
        ? list.find((t: any) => t && t.id === activeId)
        : null;
      const cwd = active && typeof active.projectPath === "string" ? active.projectPath : "";
      if (cwd) {
        const key = pathForCompare(cwd);
        const match = initProjects.find((p) => pathForCompare(p.cwd) === key);
        if (match) return match;
        // Active task references a folder we don't yet have in the list;
        // synthesize one so the picker can show it on the first frame.
        return projectFromPath(cwd);
      }
    } catch {
      /* fall through */
    }
    return initProjects[0] || null;
  })();
  const [savedProjects, setSavedProjects] = useState<Project[]>(initProjects);
  const [project, setProject] = useState<Project | null>(initProject);

  // Pi gate: default to "installed" outside Tauri (web preview / SSR) so the
  // mock UI isn't locked. Inside Tauri, start in loading=true and resolve via
  // get_pi_status on mount.
  const [piInstalled, setPiInstalled] = useState<boolean>(!isTauri());
  const [piLoading, setPiLoading] = useState<boolean>(isTauri());

  const refreshPi = useCallback(async () => {
    if (!isTauri()) {
      setPiInstalled(true);
      setPiLoading(false);
      return;
    }
    try {
      const s = await ipc.getPiStatus();
      setPiInstalled(s.installed);
    } catch {
      setPiInstalled(false);
    } finally {
      setPiLoading(false);
    }
  }, []);

  useEffect(() => { refreshPi(); }, [refreshPi]);

  // Force-land on Settings the moment we discover Pi is missing.
  useEffect(() => {
    if (!piLoading && !piInstalled && nav.tab !== "settings") {
      setNav({ tab: "settings", params: {} });
    }
    // Intentionally NOT depending on nav.tab — we only want to redirect on
    // the loading→loaded transition, not trap the user after install.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [piLoading, piInstalled]);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if ((e.metaKey || e.ctrlKey) && e.shiftKey && (e.key === "T" || e.key === "t")) {
        e.preventDefault();
        setQuickOpen(true);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  // Quick Task (Linear-style): plain "C" with no modifier and no input focused.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      // Already open? Don't re-fire (the dialog's own textarea would also
      // swallow this via the input check below, but belt-and-suspenders).
      if (quickOpen) return;

      // Modifier keys are reserved for other shortcuts (Cmd+C copy, etc.).
      if (e.metaKey || e.ctrlKey || e.altKey) return;

      // IME composition — Korean/Japanese/Chinese input passes through
      // keydown with isComposing=true; consuming would break IME UX.
      if (e.isComposing || e.key === "Process") return;

      // Skip auto-repeat so holding "C" doesn't bounce.
      if (e.repeat) return;

      // Only process the actual C key (both lower and upper case).
      // shiftKey is deliberately NOT excluded so that Shift+C (capital C)
      // also triggers Quick Task, matching Linear's behaviour; Cmd/Ctrl/Alt
      // are already filtered out above so native copy is never interrupted.
      if (e.key !== "c" && e.key !== "C") return;

      // Don't hijack typing — same guard as the "?" cheatsheet listener.
      const el = document.activeElement;
      if (el) {
        const tag = el.tagName.toLowerCase();
        if (tag === "input" || tag === "textarea" || tag === "select") return;
        if ((el as HTMLElement).isContentEditable) return;
      }

      // Don't open Quick Task on top of another modal (cheatsheet,
      // ErrorModal, SwarmQuestionModal, QuickTaskDialog itself, …).
      // Modal in atoms.tsx tags every dialog with role+aria-modal.
      if (document.querySelector('[role="dialog"][aria-modal="true"]')) return;

      // Don't open Quick Task on top of the dev-only inspector overlay.
      if (inspectorOn) return;

      e.preventDefault();
      setQuickOpen(true);
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [quickOpen, inspectorOn]);

  // Cheatsheet: open on `?` when no input/textarea/select/contenteditable is focused
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      const el = document.activeElement;
      if (el) {
        const tag = el.tagName.toLowerCase();
        if (tag === "input" || tag === "textarea" || tag === "select") return;
        if ((el as HTMLElement).isContentEditable) return;
      }

      if (e.key === "?") {
        e.preventDefault();
        setCheatsheetOpen((v) => !v);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  // Apply zoom on mount
  useEffect(() => {
    applyZoom(zoomLevel);
  }, []); // once on mount

  // Zoom change handler (shared with Settings UI and keyboard shortcuts)
  const changeZoom = useCallback(async (level: number) => {
    setZoomLevel(level);
    zoomRef.current = level;
    saveZoom(level);
    await applyZoom(level);
  }, []);

  // Global keyboard shortcuts for zoom (Tauri only — in browser dev, native Cmd+/- works)
  useEffect(() => {
    if (!isTauri()) return;
    const onKey = (e: KeyboardEvent) => {
      if (!(e.metaKey || e.ctrlKey)) return;
      if (e.code === "Equal" || e.code === "NumpadAdd") {
        e.preventDefault();
        changeZoom(zoomIn(zoomRef.current));
      } else if (e.code === "Minus" || e.code === "NumpadSubtract") {
        e.preventDefault();
        changeZoom(zoomOut(zoomRef.current));
      } else if (e.code === "Digit0" || e.code === "Numpad0") {
        e.preventDefault();
        changeZoom(zoomReset());
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [changeZoom]);

  // Completion-sound config load moved to `<CompletionSoundLoader>`
  // below, which reads from <SettingsProvider> instead of issuing a
  // duplicate `getSettings()` call.

  const projects = isTauri() ? savedProjects : PROJECTS;

  const addProject = useCallback((p: Project) => {
    setSavedProjects((prev) => {
      if (prev.some((x) => x.id === p.id)) return prev;
      const next = [...prev, p];
      localStorage.setItem("hyvemind:projects", JSON.stringify(next));
      return next;
    });
  }, []);

  const removeProject = useCallback((id: string) => {
    setSavedProjects((prev) => {
      const next = prev.filter((x) => x.id !== id);
      localStorage.setItem("hyvemind:projects", JSON.stringify(next));
      return next;
    });
  }, []);

  const updateProject = useCallback((id: string, patch: Partial<Project>) => {
    setSavedProjects((prev) => {
      const next = prev.map((x) => (x.id === id ? { ...x, ...patch } : x));
      localStorage.setItem("hyvemind:projects", JSON.stringify(next));
      return next;
    });
    setProject((curr) => (curr && curr.id === id ? { ...curr, ...patch } : curr));
  }, []);

  const go: GoFn = useCallback(
    (tab, params = {}) => {
      if (!piInstalled && LOCKED_WHEN_NO_PI.has(tab)) return;
      setNav({ tab, params });
    },
    [piInstalled],
  );

  const openQuickTask = useCallback(() => setQuickOpen(true), []);
  const toggleInspector = useCallback(() => setInspectorOn((v) => !v), []);
  // Topbar Nurse dropdown's "Nurse Settings →" link now opens the
  // dedicated Nurse screen rather than the (now-stub) Settings section.
  const openNurseSettings = useCallback(() => go("nurse"), [go]);

  const closeQuick = useCallback(() => {
    setQuickOpen(false);
    setQuickPrefill("");
  }, []);

  // Resolves the project that matches the app's own source_dir, creating
  // it (and persisting it) if it isn't already saved. Returns true when a
  // project was resolved/selected, false otherwise (IPC failure, empty
  // source_dir, or non-Tauri runtime). Safe to call from any handler.
  //
  // Note: this still calls `ipc.getSettings()` directly because it
  // needs to be safe to invoke before SettingsProvider has finished
  // its first fetch (it runs from Inspector / ErrorModal handlers).
  // SettingsProvider caches the same value for screens that don't
  // need this guarantee. See audit 6.7.
  const resolveSourceDirProject = useCallback(async (): Promise<boolean> => {
    if (!isTauri()) return false;
    try {
      const settings = await ipc.getSettings();
      const srcDirRaw = (settings.source_dir || "").trim();
      if (!srcDirRaw) return false;
      // projectFromPath sets `id === cwd === folderPath`, so matching by
      // either field is equivalent for projects added via this helper.
      // We match on a normalized `cwd` because the intent is "same on-disk
      // directory" — robust to a saved project whose `cwd` carries a
      // trailing separator, mixed slashes (Windows), or differs in
      // drive-letter case. `pathForCompare` handles all three.
      const srcDirKey = pathForCompare(srcDirRaw);
      const existing = projects.find((p) => pathForCompare(p.cwd) === srcDirKey);
      if (existing) {
        setProject(existing);
      } else {
        const p = projectFromPath(srcDirRaw);
        addProject(p);
        setProject(p);
      }
      return true;
    } catch (err) {
      console.warn("[App] Failed to resolve source_dir project:", err);
      return false;
    }
  }, [projects, setProject, addProject]);

  const onInspectorSelect = useCallback(async (text: string) => {
    setInspectorOn(false);
    setQuickPrefill(text);
    // Inspector is always used on this app — force project to source_dir.
    // We deliberately await the resolution before opening the dialog so it
    // never renders with a stale project selection.
    await resolveSourceDirProject();
    setQuickOpen(true);
  }, [resolveSourceDirProject]);

  const onInspectorCancel = useCallback(() => setInspectorOn(false), []);

  return (
    <Sentry.ErrorBoundary
      fallback={({ error, resetError }) => (
        <div className="h-full flex items-center justify-center bg-ink-950 text-slate-200 p-8">
          <div className="max-w-lg w-full bg-ink-800 border border-red-500/30 rounded-2xl p-6">
            <h1 className="text-lg font-semibold text-white mb-2">Something broke.</h1>
            <p className="text-sm text-slate-400 mb-4">
              The UI hit an unrecoverable error. The crash has been reported. Try resetting the view; if it keeps happening, restart the app.
            </p>
            <pre className="text-[12px] text-red-300/80 font-mono whitespace-pre-wrap break-words max-h-[40vh] overflow-auto bg-ink-900 rounded-lg p-3 border border-line mb-4">
              {error instanceof Error ? error.message : String(error)}
            </pre>
            <div className="flex items-center gap-3">
              <button
                onClick={async () => {
                  const errorMessage = error instanceof Error ? error.message : String(error);
                  const errorStack = error instanceof Error ? error.stack : undefined;
                  const prompt = [
                    `Fix this Hyvemind Bug: ${errorMessage}`,
                    "",
                    errorStack ? `Error details:\n\`\`\`\n${errorStack}\n\`\`\`` : null,
                    "",
                    `Source: Sentry ErrorBoundary fallback`,
                    `Timestamp: ${new Date().toISOString()}`,
                    "",
                    "Diagnose the root cause and implement a fix.",
                  ]
                    .filter((l) => l !== null)
                    .join("\n");
                  await resolveSourceDirProject();
                  go("tasks", { prefill: prompt });
                  resetError();
                }}
                className="px-3.5 py-1.5 rounded-lg text-sm font-medium bg-honey-500 text-ink-900 hover:bg-honey-400 transition-colors flex items-center gap-1.5"
              >
                {I.spark({ size: 13 })}
                Fix
              </button>
              <button
                onClick={resetError}
                className="px-3.5 py-1.5 rounded-lg text-sm font-medium bg-ink-600 text-slate-200 hover:bg-ink-500 border border-line transition-colors"
              >
                Reset view
              </button>
            </div>
          </div>
        </div>
      )}
    >
    {/*
     * Provider tree (audit 6.7): SettingsProvider + ProvidersProvider
     * sit near the top so every screen below can read the cached
     * SettingsResponse / ProviderInfo[] without issuing duplicate
     * IPC calls. They're inside the ErrorBoundary so any IPC fault
     * during the initial fetch is caught by the boundary rather
     * than the host crashing.
     *
     * Order matters minimally: SettingsProvider and ProvidersProvider
     * have no cross-dependencies; ErrorModalProvider doesn't read
     * either; ProjectContext / PiStatusContext / ContextMenuProvider
     * are unchanged. The TaskRuntimeProvider stays innermost so its
     * sub-contexts (the six new slices defined in taskRuntime.tsx)
     * keep their existing scope.
     */}
    <SettingsProvider>
    <ProvidersProvider>
    <NurseProvider>
    <CompletionSoundLoader />
    <ErrorModalProvider onFix={async (prompt) => {
      await resolveSourceDirProject();
      go("tasks", { prefill: prompt });
    }}>
    <ToastProvider>
      <ProjectContext.Provider
        value={{ project, setProject, projects, addProject, removeProject, updateProject }}
      >
        <PiStatusContext.Provider value={{ installed: piInstalled, loading: piLoading, refresh: refreshPi }}>
        <ContextMenuProvider
          onQuickTask={(text) => { setQuickPrefill(text); setQuickOpen(true); }}
          onToggleInspector={() => setInspectorOn((v) => !v)}
          inspectorOn={inspectorOn}
        >
        <ExtensionProvider>
        <TestRunProvider>
        <TaskRuntimeProvider go={go}>
          <div className="h-full flex flex-col hex-bg">
            {/* Full-width Topbar — spans the window so macOS trafficLightPosition
                coordinates land within the bar's coordinate system. */}
            <Topbar
              nav={nav}
              go={go}
              onOpenQuickTask={openQuickTask}
              inspectorOn={inspectorOn}
              onToggleInspector={toggleInspector}
              onOpenNurseSettings={openNurseSettings}
            />

            <div className="flex-1 min-h-0 flex">
              {/* Sidebar */}
              <Sidebar nav={nav} go={go} piInstalled={piInstalled} />

              {/* Screen content */}
              <main className="flex-1 min-w-0 min-h-0 relative overflow-hidden">
                <ScreenRouter nav={nav} go={go} zoomLevel={zoomLevel} onZoomChange={changeZoom} />
              </main>
            </div>
          </div>
          <QuickTaskDialog open={quickOpen} onClose={closeQuick} prefill={quickPrefill} />
          <NavShortcuts go={go} />
          <ShortcutCheatsheet
            open={cheatsheetOpen}
            onClose={() => setCheatsheetOpen(false)}
          />
          <InspectorOverlay
            enabled={inspectorOn}
            pageLabel={pageLabel(nav.tab)}
            onSelect={onInspectorSelect}
            onCancel={onInspectorCancel}
          />
        </TaskRuntimeProvider>
        </TestRunProvider>
        </ExtensionProvider>
        </ContextMenuProvider>
        </PiStatusContext.Provider>
      </ProjectContext.Provider>
    </ToastProvider>
    </ErrorModalProvider>
    </NurseProvider>
    </ProvidersProvider>
    </SettingsProvider>
    </Sentry.ErrorBoundary>
  );
}

/** Tab-switching keyboard shortcuts: Cmd+1..N (Ctrl on Windows/Linux).
 *  N matches the *visible* nav length, so in release builds Cmd+5 jumps
 *  to Settings (the 5th visible entry) and in debug builds Cmd+5 jumps
 *  to Tests with Cmd+6 reaching Settings. Mounted inside the provider
 *  tree so `useVisibleNav()` can read SettingsProvider. */
function NavShortcuts({ go }: { go: GoFn }) {
  const nav = useVisibleNav();
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      // Don't fire when the user is typing in an input, textarea, select, or contenteditable
      const el = document.activeElement;
      if (el) {
        const tag = el.tagName.toLowerCase();
        if (tag === "input" || tag === "textarea" || tag === "select") return;
        if ((el as HTMLElement).isContentEditable) return;
      }

      if (!(e.metaKey || e.ctrlKey)) return;

      const num = parseInt(e.key, 10);
      if (num >= 1 && num <= nav.length) {
        e.preventDefault();
        go(nav[num - 1].tab);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [go, nav]);
  return null;
}

/** Reads the cached SettingsResponse and pushes the task-completion
 *  sound config into the global sound module. Subscribes via
 *  SettingsProvider so it picks up live changes (no extra IPC). */
function CompletionSoundLoader() {
  const { settings } = useSettings();
  useEffect(() => {
    if (!isTauri() || !settings) return;
    updateCompletionSoundConfig(
      settings.task_completion_sound_enabled,
      settings.task_completion_sound,
    );
  }, [settings]);
  return null;
}
