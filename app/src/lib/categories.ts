/* ── Workspace color utilities ─────────────────────────────── */
/* Workspace "categories" are derived automatically from each task's
 * project field — users never create or manage them. This module
 * just assigns deterministic colors so the same workspace always
 * gets the same dot / pill color in the sidebar. */

/** Static class strings for workspace colors.
 *  Uses complete Tailwind class literals — no template interpolation. */
export const WORKSPACE_COLORS = [
  { key: "honey",  dot: "bg-honey-400",  pill: "bg-honey-500/15 text-honey-300",  border: "border-honey-500/30" },
  { key: "blue",   dot: "bg-blue-400",   pill: "bg-blue-500/15 text-blue-300",    border: "border-blue-500/30" },
  { key: "purple", dot: "bg-purple-400", pill: "bg-purple-500/15 text-purple-300", border: "border-purple-500/30" },
  { key: "green",  dot: "bg-green-400",  pill: "bg-green-500/15 text-green-300",  border: "border-green-500/30" },
  { key: "red",    dot: "bg-red-400",    pill: "bg-red-500/15 text-red-300",      border: "border-red-500/30" },
  { key: "violet", dot: "bg-violet-400", pill: "bg-violet-500/15 text-violet-300", border: "border-violet-500/30" },
] as const;

export type WorkspaceStyle = (typeof WORKSPACE_COLORS)[number];

/** Deterministic hash of a string to a small integer. */
function simpleHash(s: string): number {
  let h = 0;
  for (let i = 0; i < s.length; i++) {
    h = ((h << 5) - h + s.charCodeAt(i)) | 0;
  }
  return Math.abs(h);
}

/** Return the color style for a workspace name. Deterministic — same name
 *  always maps to the same color. */
export function workspaceColor(name: string): WorkspaceStyle {
  const idx = simpleHash(name.toLowerCase()) % WORKSPACE_COLORS.length;
  return WORKSPACE_COLORS[idx];
}

/** Derive a human-readable workspace label from a project field or path.
 *  - "/Users/moh/Desktop/Work/Hyvemind" → "Hyvemind"
 *  - "payments" → "payments"
 *  - "" / undefined → "No workspace" */
export function workspaceLabel(project: string | undefined): string {
  if (!project) return "No workspace";
  // If it looks like a path, take the last segment
  if (project.includes("/") || project.includes("\\")) {
    const parts = project.split(/[/\\]/).filter(Boolean);
    return parts[parts.length - 1] || "No workspace";
  }
  return project;
}

/** Derive the workspace key used for grouping. Uses the raw project string
 *  (or projectPath fallback) so tasks in the same project always group
 *  together. Falls back to "" for ungrouped tasks. */
export function workspaceKey(task: { project?: string; projectPath?: string }): string {
  return task.project || task.projectPath || "";
}
