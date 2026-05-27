import { describe, it, expect } from "vitest";
import { assignSortOrders, reorderInGroup, migrateTasks } from "../sortOrder";
import type { TaskListItem } from "../taskRuntime";

/** Helper to build a minimal TaskListItem for testing. */
function item(
  id: string,
  overrides: Partial<TaskListItem> = {},
): TaskListItem {
  return {
    id,
    group: "Active",
    title: `Task ${id}`,
    project: "test",
    model: "test-model",
    phase: "intake",
    when: "now",
    preview: "",
    ...overrides,
  };
}

describe("assignSortOrders", () => {
  it("assigns sequential orders to tasks missing sortOrder", () => {
    const tasks = [item("a"), item("b"), item("c")];
    const result = assignSortOrders(tasks);
    expect(result.map((t) => t.sortOrder)).toEqual([0, 1, 2]);
  });

  it("preserves existing sortOrder values (partial migration)", () => {
    const tasks = [
      item("a", { sortOrder: 5 }),
      item("b"),
      item("c", { sortOrder: 10 }),
      item("d"),
    ];
    const result = assignSortOrders(tasks);
    expect(result[0].sortOrder).toBe(5);
    expect(result[2].sortOrder).toBe(10);
    // b and d should get orders starting after the max existing (10)
    expect(result[1].sortOrder).toBe(11);
    expect(result[3].sortOrder).toBe(12);
  });

  it("is idempotent on already-migrated lists", () => {
    const tasks = [
      item("a", { sortOrder: 0 }),
      item("b", { sortOrder: 1 }),
      item("c", { sortOrder: 2 }),
    ];
    const result = assignSortOrders(tasks);
    // Should return the same reference — no migration needed
    expect(result).toBe(tasks);
  });

  it("handles empty array", () => {
    expect(assignSortOrders([])).toEqual([]);
  });
});

describe("reorderInGroup", () => {
  it("correctly swaps two adjacent tasks within a group", () => {
    const tasks = [
      item("a", { sortOrder: 0, group: "Active" }),
      item("b", { sortOrder: 1, group: "Active" }),
      item("c", { sortOrder: 2, group: "Active" }),
    ];
    const groupIds = ["a", "b", "c"];
    const result = reorderInGroup(tasks, groupIds, "b", "a");
    // b should now be at position 0, a at position 1
    expect(result.find((t) => t.id === "b")!.sortOrder).toBe(0);
    expect(result.find((t) => t.id === "a")!.sortOrder).toBe(1);
    expect(result.find((t) => t.id === "c")!.sortOrder).toBe(2);
  });

  it("correctly moves a task across multiple positions within a group", () => {
    const tasks = [
      item("a", { sortOrder: 0 }),
      item("b", { sortOrder: 1 }),
      item("c", { sortOrder: 2 }),
      item("d", { sortOrder: 3 }),
    ];
    const groupIds = ["a", "b", "c", "d"];
    // Move "a" to where "d" is
    const result = reorderInGroup(tasks, groupIds, "a", "d");
    // splice(0,1) removes a → [b, c, d], splice(3, 0, a) → [b, c, d, a]
    expect(result.find((t) => t.id === "b")!.sortOrder).toBe(0);
    expect(result.find((t) => t.id === "c")!.sortOrder).toBe(1);
    expect(result.find((t) => t.id === "d")!.sortOrder).toBe(2);
    expect(result.find((t) => t.id === "a")!.sortOrder).toBe(3);
  });

  it("reassigns contiguous integer sort orders for the affected group only", () => {
    const tasks = [
      item("a", { sortOrder: 10, group: "Active" }),
      item("b", { sortOrder: 20, group: "Active" }),
      item("c", { sortOrder: 30, group: "Active" }),
    ];
    const groupIds = ["a", "b", "c"];
    const result = reorderInGroup(tasks, groupIds, "c", "a");
    const orders = result.map((t) => t.sortOrder!);
    // All orders should be contiguous integers
    expect(orders.sort()).toEqual([0, 1, 2]);
  });

  it("does not modify tasks outside the target group", () => {
    const tasks = [
      item("a", { sortOrder: 0, group: "Active" }),
      item("b", { sortOrder: 1, group: "Active" }),
      item("x", { sortOrder: 5, group: "Today" }),
      item("y", { sortOrder: 6, group: "Today" }),
    ];
    const groupIds = ["a", "b"]; // only Active group
    const result = reorderInGroup(tasks, groupIds, "b", "a");
    // Today tasks should be unchanged
    expect(result.find((t) => t.id === "x")!.sortOrder).toBe(5);
    expect(result.find((t) => t.id === "y")!.sortOrder).toBe(6);
  });

  it("returns allTasks unchanged when activeId not in groupTaskIds", () => {
    const tasks = [item("a", { sortOrder: 0 }), item("b", { sortOrder: 1 })];
    const result = reorderInGroup(tasks, ["a", "b"], "z", "a");
    expect(result).toBe(tasks);
  });

  it("returns allTasks unchanged when activeId equals overId", () => {
    const tasks = [item("a", { sortOrder: 0 }), item("b", { sortOrder: 1 })];
    const result = reorderInGroup(tasks, ["a", "b"], "a", "a");
    expect(result).toBe(tasks);
  });
});

describe("migrateTasks", () => {
  it("populates project from projectPath when project is empty", () => {
    const tasks = [
      item("a", { project: "", projectPath: "/Users/moh/Desktop/Work/Hyvemind" }),
    ];
    const result = migrateTasks(tasks);
    expect(result[0].project).toBe("Hyvemind");
  });

  it("assigns createdAt to tasks missing it", () => {
    const tasks = [item("a")];
    const result = migrateTasks(tasks);
    expect(typeof result[0].createdAt).toBe("number");
    // Fallback is 30 days ago
    const thirtyDaysAgo = Date.now() - 30 * 86_400_000;
    expect(result[0].createdAt).toBeGreaterThanOrEqual(thirtyDaysAgo - 1000);
    expect(result[0].createdAt).toBeLessThanOrEqual(thirtyDaysAgo + 1000);
  });

  it("returns same reference when already migrated", () => {
    const tasks = [
      item("a", { project: "Hyvemind", projectPath: "/some/path", createdAt: 12345 }),
      item("b", { project: "Other", projectPath: "/other/path", createdAt: 67890 }),
    ];
    const result = migrateTasks(tasks);
    expect(result).toBe(tasks);
  });

  it("keeps project empty when projectPath is empty", () => {
    const tasks = [item("a", { project: "", projectPath: "" })];
    const result = migrateTasks(tasks);
    expect(result[0].project).toBe("");
  });

  it("handles empty array", () => {
    const result = migrateTasks([]);
    expect(result).toEqual([]);
  });
});
