/* ── Sort order utilities ─────────────────────────────────── */
/* Uses integer renumbering (reassign 0, 1, 2, ... after each move)
 * rather than fractional indexing. Task lists are small enough that
 * O(n) renumbering per drag is negligible. */

import type { TaskListItem } from "./taskRuntime";
import { workspaceLabel } from "./categories";

/** Assign sortOrder to tasks that don't have one (migration).
 *  Idempotent: preserves existing sortOrder values, only assigns to tasks
 *  where typeof sortOrder !== 'number'. */
export function assignSortOrders(tasks: TaskListItem[]): TaskListItem[] {
  let needsMigration = false;
  for (const t of tasks) {
    if (typeof t.sortOrder !== "number") {
      needsMigration = true;
      break;
    }
  }
  if (!needsMigration) return tasks;

  let nextOrder = 0;
  // Find the max existing order to start assignments from
  for (const t of tasks) {
    if (typeof t.sortOrder === "number" && t.sortOrder >= nextOrder) {
      nextOrder = t.sortOrder + 1;
    }
  }

  return tasks.map((t) => {
    if (typeof t.sortOrder === "number") return t;
    return { ...t, sortOrder: nextOrder++ };
  });
}

/** Recompute sort order after a drag-and-drop reorder within a group.
 *  `groupTaskIds` is the ordered list of task IDs in the active group.
 *  Moves activeId to the position of overId within the group, then
 *  reassigns integer sort orders for that group's tasks only. */
export function reorderInGroup(
  allTasks: TaskListItem[],
  groupTaskIds: string[],
  activeId: string,
  overId: string,
): TaskListItem[] {
  const oldIdx = groupTaskIds.indexOf(activeId);
  const newIdx = groupTaskIds.indexOf(overId);
  if (oldIdx === -1 || newIdx === -1) return allTasks;
  if (oldIdx === newIdx) return allTasks;

  // Reorder the group's ID list
  const reordered = [...groupTaskIds];
  reordered.splice(oldIdx, 1);
  reordered.splice(newIdx, 0, activeId);

  // Build a position map for the group
  const posMap = new Map<string, number>();
  reordered.forEach((id, i) => posMap.set(id, i));

  return allTasks.map((t) => {
    const newOrder = posMap.get(t.id);
    if (newOrder !== undefined && newOrder !== t.sortOrder) {
      return { ...t, sortOrder: newOrder };
    }
    return t;
  });
}

/** Migrate tasks: populate project from projectPath, add createdAt.
 *  Idempotent: already-migrated tasks are returned unchanged (same references).
 *
 *  Fallback timestamps use a uniform "30 days ago" for all legacy tasks
 *  since the actual creation time is unknown. This places them in the "Older"
 *  time group, which is the least misleading bucket for tasks of unknown age.
 *  A uniform timestamp also avoids ordering artifacts from assigning sequential
 *  timestamps to tasks whose array order may reflect drag-and-drop reordering
 *  rather than creation time. */
export function migrateTasks(tasks: TaskListItem[]): TaskListItem[] {
  let changed = false;
  const fallbackCreatedAt = Date.now() - 30 * 86_400_000; // 30 days ago → "Older"
  const result = tasks.map((t) => {
    const needsProject = !t.project && t.projectPath;
    const needsCreatedAt = typeof t.createdAt !== "number";
    if (!needsProject && !needsCreatedAt) return t;
    changed = true;
    return {
      ...t,
      project: needsProject ? workspaceLabel(t.projectPath) : t.project,
      createdAt: needsCreatedAt ? fallbackCreatedAt : t.createdAt,
    };
  });
  return changed ? result : tasks;
}
