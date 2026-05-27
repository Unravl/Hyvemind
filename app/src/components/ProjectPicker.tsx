import React, { createContext, useContext, useState, useCallback } from "react";
import { I } from "./icons";
import { PROJECTS, Project, AutoCommitOverride } from "../data/mock";
import { isTauri } from "../lib/tauri";
import * as ipc from "../lib/ipc";
import { open as openFolderDialog } from "@tauri-apps/plugin-dialog";
import { useErrorModal } from "./ErrorModal";
import { useSettings } from "../lib/SettingsProvider";

/* ── Language dot color map ────────────────────────────────── */

export const LANG_DOT: Record<string, string> = {
  rust: "bg-orange-400",
  ts: "bg-blue-400",
  tsx: "bg-blue-400",
  go: "bg-cyan-400",
  py: "bg-yellow-400",
  rb: "bg-red-400",
  java: "bg-amber-500",
  kotlin: "bg-purple-400",
  swift: "bg-orange-500",
  c: "bg-gray-400",
  cpp: "bg-pink-400",
  zig: "bg-amber-300",
};

/* ── Persistence helpers ──────────────────────────────────── */

const STORAGE_KEY = "hyvemind:projects";

export function loadSavedProjects(): Project[] {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (!raw) return [];
    const parsed: Project[] = JSON.parse(raw);
    return parsed.map((p) => {
      const ovr = p.autoCommitOverride;
      return {
        ...p,
        autoCommitOverride:
          ovr === "on" || ovr === "off" || ovr === "inherit" ? ovr : undefined,
      };
    });
  } catch {
    return [];
  }
}

function saveProjects(projects: Project[]) {
  localStorage.setItem(STORAGE_KEY, JSON.stringify(projects));
}

export function normalizeProjectPath(path: string): string {
  const withForwardSlashes = path.replace(/\\/g, "/");
  return withForwardSlashes.replace(/\/+$/, "") || withForwardSlashes;
}

/// Normalize a filesystem path string for **equality / prefix comparison**.
///
/// Differs from `normalizeProjectPath` in that it also lowercases the path
/// when it begins with a Windows drive letter — those filesystems are
/// case-insensitive, so a path picked from the OS dialog can differ in case
/// from the persisted allowlist entry (and from what the backend hands back
/// after `dunce::canonicalize`). Without this, the frontend pre-flight
/// "is this path approved?" check spuriously fails on Windows and the
/// approval modal loops even after the user clicks Allow.
///
/// POSIX paths are left case-sensitive (Linux can legitimately have two
/// directories that differ only in case).
export function pathForCompare(path: string): string {
  let n = path.replace(/[\/\\]+$/, "").replace(/\\/g, "/");
  if (/^[A-Za-z]:\//.test(n)) n = n.toLowerCase();
  return n;
}

/// True iff `path` exactly equals — or is a strict descendant of — `root`,
/// after both sides are normalized via [`pathForCompare`]. Used by the
/// approval-modal pre-flight check in ProjectPicker and NewSwarm.
export function isUnderApprovedRoot(path: string, root: string): boolean {
  const p = pathForCompare(path);
  const r = pathForCompare(root);
  return p === r || p.startsWith(r + "/");
}

/** Build a minimal Project from a folder path */
export function projectFromPath(folderPath: string): Project {
  const normalizedPath = normalizeProjectPath(folderPath);
  const parts = normalizedPath.split("/").filter(Boolean);
  const name = parts[parts.length - 1] || "project";
  const org = parts[parts.length - 2] || "local";
  return {
    id: normalizedPath,
    name,
    org,
    cwd: normalizedPath,
    branch: "",
    dirty: 0,
    lang: "",
    activeSwarms: 0,
    chats: 0,
    lastTouched: "just added",
  };
}

/* ── Context ───────────────────────────────────────────────── */

interface ProjectContextValue {
  project: Project | null;
  setProject: (p: Project | null) => void;
  projects: Project[];
  addProject: (p: Project) => void;
  removeProject: (id: string) => void;
  updateProject: (id: string, patch: Partial<Project>) => void;
}

export const ProjectContext = createContext<ProjectContextValue>({
  project: null,
  setProject: () => {},
  projects: [],
  addProject: () => {},
  removeProject: () => {},
  updateProject: () => {},
});

export function useProject() {
  return useContext(ProjectContext);
}

/* ── ProjectPicker ─────────────────────────────────────────── */

interface ProjectPickerProps {
  variant?: "header" | "inline";
  className?: string;
}

export function ProjectPicker({
  variant = "header",
  className = "",
}: ProjectPickerProps) {
  const { project, setProject, projects, addProject, removeProject, updateProject } = useProject();
  const { showError } = useErrorModal();
  const [open, setOpen] = useState(false);
  const [filter, setFilter] = useState("");
  // audit 6.7 — read the cached settings instead of issuing a fresh
  // `getSettings()` call every time the picker opens.
  const { settings } = useSettings();
  const globalAutoCommit: boolean | null = settings?.auto_commit_tasks ?? null;
  // Audit 1.11: when the user picks a directory not in the allowlist we
  // surface an explicit "give the AI read/write access to this folder"
  // confirmation before sending the IPC. `pendingApproval` carries the
  // raw folder path and the as-yet-unsaved Project record.
  const [pendingApproval, setPendingApproval] = useState<
    { path: string; project: Project } | null
  >(null);
  const [approving, setApproving] = useState(false);

  const filtered = projects.filter(
    (p) =>
      p.name.toLowerCase().includes(filter.toLowerCase()) ||
      p.org.toLowerCase().includes(filter.toLowerCase()),
  );

  // Compare two filesystem paths in a robust-but-cheap way for the
  // allowlist-membership check. Canonicalization happens server-side, so we
  // only need a string match here for the happy path. `pathForCompare`
  // normalizes separators and lowercases Windows drive-letter paths so the
  // allowlist's `C:\Users\…` matches a freshly-selected `C:\users\…`.
  // False negatives just trigger an extra approval-modal flash — never a
  // security issue.
  const pathsEqual = (a: string, b: string) =>
    pathForCompare(a) === pathForCompare(b);

  const handleAddProject = useCallback(async () => {
    if (!isTauri()) return;
    try {
      const selected = await openFolderDialog({
        directory: true,
        multiple: false,
        title: "Select project folder",
      });
      if (!selected) return;
      const rawPath = selected as string;
      const p = projectFromPath(rawPath);

      // Audit 1.11: consult the backend's approved-working-dirs allowlist
      // before sending any future IPC that would carry this path. If the
      // chosen folder isn't already approved (or a descendant of an
      // approved root), pop the confirmation modal.
      let approvedRoots: string[] = [];
      try {
        const settings = await ipc.getSettings();
        approvedRoots = settings.approved_working_dirs ?? [];
      } catch {
        // If we can't read the allowlist, fall through to the modal —
        // safer to ask than to silently add without consent.
      }
      const alreadyApproved = approvedRoots.some((root) =>
        isUnderApprovedRoot(rawPath, root),
      );

      if (alreadyApproved) {
        addProject(p);
        setProject(p);
        setOpen(false);
        setFilter("");
        return;
      }

      // Not yet approved — show the modal and let the user decide.
      setPendingApproval({ path: rawPath, project: p });
    } catch (e) {
      showError("Failed to open folder picker", String(e));
    }
  }, [addProject, setProject, showError]);

  const handleApprovalAllow = useCallback(async () => {
    if (!pendingApproval) return;
    setApproving(true);
    try {
      await ipc.requestWorkingDirApproval(pendingApproval.path);
      addProject(pendingApproval.project);
      setProject(pendingApproval.project);
      setPendingApproval(null);
      setOpen(false);
      setFilter("");
    } catch (e) {
      showError("Failed to approve working directory", String(e));
    } finally {
      setApproving(false);
    }
  }, [pendingApproval, addProject, setProject, showError]);

  const handleApprovalCancel = useCallback(() => {
    setPendingApproval(null);
  }, []);

  const handleRemove = useCallback((e: React.MouseEvent, id: string) => {
    e.stopPropagation();
    removeProject(id);
    if (project?.id === id) setProject(null);
  }, [project, setProject, removeProject]);

  const renderList = () => (
    <>
      <div className="p-2 border-b border-line">
        <div className="flex items-center gap-2 bg-ink-850 border border-line rounded-lg px-2.5 py-1.5">
          {I.search({ size: 14, className: "text-muted shrink-0" })}
          <input
            autoFocus
            placeholder="Filter projects..."
            value={filter}
            onChange={(e) => setFilter(e.target.value)}
            className="bg-transparent flex-1 text-sm text-slate-200 placeholder:text-dim focus:outline-none focus-visible:ring-2 focus-visible:ring-honey-500 focus-visible:ring-offset-2 focus-visible:ring-offset-ink-950"
          />
        </div>
      </div>
      <div className="max-h-60 overflow-y-auto py-1">
        {filtered.map((p) => (
          <button
            key={p.id}
            onClick={() => {
              setProject(p);
              setOpen(false);
              setFilter("");
            }}
            className={`w-full flex items-center gap-2.5 px-3 py-2 text-left hover:bg-ink-700/60 transition-colors cursor-pointer group ${
              project?.id === p.id ? "bg-ink-700/40" : ""
            }`}
          >
            <span
              className={`w-2 h-2 rounded-full shrink-0 ${LANG_DOT[p.lang] || "bg-muted"}`}
            />
            <div className="min-w-0 flex-1">
              <div className="flex items-center gap-1">
                <span className="text-xs text-muted">{p.org}/</span>
                <span className="text-sm font-medium text-slate-200 truncate">
                  {p.name}
                </span>
              </div>
              <div className="flex items-center gap-2 mt-0.5">
                <span className="text-[10px] text-dim font-mono truncate">
                  {p.branch || p.cwd}
                </span>
                {p.dirty > 0 && (
                  <span className="text-[10px] text-honey-500">
                    {p.dirty} dirty
                  </span>
                )}
              </div>
            </div>
            {p.activeSwarms > 0 && (
              <span className="text-[10px] text-green-400 shrink-0">
                {p.activeSwarms} active
              </span>
            )}
            {isTauri() && (
              <span
                onClick={(e) => handleRemove(e, p.id)}
                className="text-dim hover:text-red-400 shrink-0 opacity-0 group-hover:opacity-100 transition-opacity"
              >
                {I.x({ size: 12 })}
              </span>
            )}
            {project?.id === p.id && (
              <span className="text-honey-500 shrink-0">
                {I.check({ size: 14 })}
              </span>
            )}
          </button>
        ))}
        {filtered.length === 0 && !isTauri() && (
          <div className="px-3 py-4 text-center text-xs text-dim">
            No projects match
          </div>
        )}
        {filtered.length === 0 && isTauri() && (
          <div className="px-3 py-4 text-center text-xs text-dim">
            {projects.length === 0 ? "No projects added yet" : "No projects match"}
          </div>
        )}
      </div>
      {project && (() => {
        const current: AutoCommitOverride = project.autoCommitOverride ?? "inherit";
        const globalLabel =
          globalAutoCommit === null
            ? "Global: unknown"
            : `Following global \u00b7 ${globalAutoCommit ? "ON" : "OFF"}`;
        const caption =
          current === "inherit"
            ? globalLabel
            : `Override \u00b7 ${current === "on" ? "ON" : "OFF"}`;
        const segments: { value: AutoCommitOverride; label: string }[] = [
          { value: "inherit", label: "Inherit" },
          { value: "on", label: "On" },
          { value: "off", label: "Off" },
        ];
        return (
          <div className="border-t border-line px-3 py-2.5">
            <div className="flex items-center justify-between mb-1">
              <span className="text-xs font-medium text-slate-200">
                Auto-commit to git
              </span>
              <span
                className="text-dim"
                title="Overrides the global Auto-commit setting in Settings for this project."
              >
                {I.info ? I.info({ size: 12 }) : null}
              </span>
            </div>
            <div className="text-[10px] text-dim mb-1.5">{caption}</div>
            <div className="flex items-center gap-1 bg-ink-850 border border-line rounded-lg p-0.5">
              {segments.map((seg) => {
                const active = current === seg.value;
                return (
                  <button
                    key={seg.value}
                    onClick={() =>
                      updateProject(project.id, { autoCommitOverride: seg.value })
                    }
                    className={`flex-1 text-xs px-2 py-1 rounded-md transition-colors cursor-pointer ${
                      active
                        ? "bg-honey-500/15 text-honey-300"
                        : "text-muted hover:text-slate-200"
                    }`}
                  >
                    {seg.label}
                  </button>
                );
              })}
            </div>
          </div>
        );
      })()}
      {isTauri() && (
        <div className="border-t border-line p-1.5">
          <button
            onClick={handleAddProject}
            className="w-full flex items-center gap-2 px-2.5 py-2 rounded-lg text-sm text-honey-300 hover:bg-honey-500/10 transition-colors"
          >
            {I.plus({ size: 14, className: "text-honey-400" })}
            <span className="font-medium">Add project</span>
          </button>
        </div>
      )}
    </>
  );

  // Audit 1.11: approval modal shared across both variants. Lives in a
  // portal-like fixed overlay so it sits above the dropdown.
  const approvalModal = pendingApproval && (
    <>
      <div
        className="fixed inset-0 z-[60] bg-ink-950/70"
        onClick={approving ? undefined : handleApprovalCancel}
      />
      <div
        role="dialog"
        aria-modal="true"
        aria-labelledby="wd-approval-title"
        className="fixed inset-0 z-[61] flex items-center justify-center p-4"
        onClick={(e) => e.stopPropagation()}
      >
        <div className="bg-ink-800 border border-line rounded-xl shadow-2xl w-full max-w-md overflow-hidden">
          <div className="px-5 py-4 border-b border-line">
            <h2
              id="wd-approval-title"
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
              onClick={handleApprovalCancel}
              disabled={approving}
              className="px-3 py-1.5 rounded-lg text-sm text-slate-300 hover:bg-ink-700/60 transition-colors cursor-pointer disabled:opacity-50"
            >
              Cancel
            </button>
            <button
              type="button"
              onClick={handleApprovalAllow}
              disabled={approving}
              className="px-3 py-1.5 rounded-lg text-sm font-medium bg-honey-500 text-ink-950 hover:bg-honey-400 transition-colors cursor-pointer disabled:opacity-50"
            >
              {approving ? "Approving…" : "Allow"}
            </button>
          </div>
        </div>
      </div>
    </>
  );

  if (variant === "inline") {
    return (
      <div className={`relative ${className}`}>
        <button
          onClick={() => setOpen(!open)}
          className="flex items-center gap-2 px-2.5 py-1 rounded-lg bg-ink-700/60 hover:bg-ink-600 border border-line text-sm text-slate-200 transition-colors cursor-pointer"
        >
          {project ? (
            <>
              <span
                className={`w-2 h-2 rounded-full ${LANG_DOT[project.lang] || "bg-muted"}`}
              />
              <span className="text-muted">{project.org}/</span>
              <span className="font-medium">{project.name}</span>
            </>
          ) : (
            <>
              {I.folder({ size: 14, className: "text-muted" })}
              <span className="text-muted">Select project</span>
            </>
          )}
          {I.chevD({ size: 14, className: "text-muted" })}
        </button>

        {open && (
          <>
            <div
              className="fixed inset-0 z-40"
              onClick={() => setOpen(false)}
            />
            <div className="absolute top-full left-0 mt-1 z-50 w-[360px] bg-ink-800 border border-line rounded-xl shadow-2xl overflow-hidden">
              {renderList()}
            </div>
          </>
        )}
        {approvalModal}
      </div>
    );
  }

  // header variant
  return (
    <div className={`relative ${className}`}>
      <button
        onClick={() => setOpen(!open)}
        className="flex items-center gap-2 px-3 py-2 rounded-xl bg-ink-700/40 hover:bg-ink-700/70 border border-line text-sm transition-colors cursor-pointer w-full"
      >
        {project ? (
          <>
            <span
              className={`w-2.5 h-2.5 rounded-full ${LANG_DOT[project.lang] || "bg-muted"}`}
            />
            <div className="flex-1 text-left min-w-0">
              <div className="text-xs text-muted">Project</div>
              <div className="text-sm font-semibold text-slate-200 truncate">
                {project.name}
              </div>
            </div>
          </>
        ) : (
          <>
            {I.folder({ size: 16, className: "text-muted" })}
            <span className="text-muted text-sm">Select project</span>
          </>
        )}
        {I.chevD({ size: 14, className: "text-muted shrink-0" })}
      </button>

      {open && (
        <>
          <div
            className="fixed inset-0 z-40"
            onClick={() => setOpen(false)}
          />
          <div className="absolute top-full left-0 mt-1 z-50 w-[360px] bg-ink-800 border border-line rounded-xl shadow-2xl overflow-hidden">
            {renderList()}
          </div>
        </>
      )}
      {approvalModal}
    </div>
  );
}

/* ── ProjectCrumb ──────────────────────────────────────────── */

interface ProjectCrumbProps {
  className?: string;
}

export function ProjectCrumb({ className = "" }: ProjectCrumbProps) {
  const { project } = useProject();
  if (!project) return null;
  return (
    <span className={`inline-flex items-center gap-1.5 text-sm ${className}`}>
      <span
        className={`w-2 h-2 rounded-full ${LANG_DOT[project.lang] || "bg-muted"}`}
      />
      <span className="text-muted">{project.org}/</span>
      <span className="font-medium text-slate-200">{project.name}</span>
    </span>
  );
}
