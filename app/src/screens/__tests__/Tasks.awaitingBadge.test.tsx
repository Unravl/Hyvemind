import React from "react";
import { describe, it, expect, vi } from "vitest";
import { render, screen, cleanup } from "@testing-library/react";
import { afterEach } from "vitest";

// ── Tauri presence: TasksSidebar uses `isTauri()` to decide whether to
// use `extraTasks` or the mock TASK_LIST. We want the real `extraTasks`
// path so the test fixture flows through.
vi.mock("../../lib/tauri", () => ({ isTauri: () => true }));

// ── ContextMenu: TasksSidebar calls `useContextMenu()` to register a
// task-actions handler. Stub it.
vi.mock("../../components/ContextMenu", () => ({
  useContextMenu: () => ({ registerTaskActions: vi.fn() }),
}));

// ── ProjectPicker exports (used by the screen-level code but referenced
// indirectly via Tasks.tsx imports).
vi.mock("../../components/ProjectPicker", () => ({
  ProjectPicker: () => null,
  useProject: () => ({
    project: null,
    setProject: vi.fn(),
    projects: [],
    addProject: vi.fn(),
  }),
  projectFromPath: (p: string | null) => p ?? "",
  pathForCompare: (p: string | null) => p ?? "",
}));

// ── taskRuntime: TasksSidebar itself doesn't consume the hook, but the
// module is imported by Tasks.tsx and pulled in transitively.
vi.mock("../../lib/taskRuntime", async () => {
  const actual = await vi.importActual<typeof import("../../lib/taskRuntime")>(
    "../../lib/taskRuntime",
  );
  return {
    ...actual,
    useTaskRuntime: () => ({
      getDraft: () => "",
      setDraft: vi.fn(),
    }),
    useTaskRuntimeState: () => ({
      tasks: {},
      streamingTaskIds: {},
      awaitingInputTaskIds: {},
      updateTask: vi.fn(),
    }),
    useDefaults: () => ({
      defaultModel: "",
      defaultProjectPath: "",
      defaultHivemind: "",
    }),
  };
});

vi.mock("../../components/FileMentionPicker", () => ({
  FileMentionPicker: () => null,
}));
vi.mock("../../components/NurseTestDropdown", () => ({
  NurseTestDropdown: () => null,
}));

import { TasksSidebar } from "../Tasks";
import type { TaskListItem, AwaitingInputKind } from "../../lib/taskRuntime";

afterEach(() => cleanup());

function mkTask(overrides: Partial<TaskListItem> = {}): TaskListItem {
  return {
    id: "task-1",
    group: "Active",
    title: "My Task",
    project: "",
    model: "anthropic/claude-sonnet-4",
    phase: "intake",
    when: "now",
    preview: "preview text",
    createdAt: Date.now(),
    ...overrides,
  };
}

function renderSidebar(
  task: TaskListItem,
  awaiting: AwaitingInputKind | null,
  opts?: { activeId?: string },
) {
  const awaitingMap: Record<string, AwaitingInputKind> = {};
  if (awaiting) awaitingMap[task.id] = awaiting;
  return render(
    <TasksSidebar
      activeId={opts?.activeId ?? "other-task"}
      extraTasks={[task]}
      streamingTaskIds={{}}
      awaitingInputTaskIds={awaitingMap}
      projectFilter="__all__"
      onProjectFilterChange={vi.fn()}
      sortMode="newest"
      onSortModeChange={vi.fn()}
    />,
  );
}

describe("TasksSidebar awaiting-input badge", () => {
  it("renders 'needs answer' for 'questions' kind", () => {
    renderSidebar(mkTask({ phase: "questions" }), "questions");
    const badge = screen.getByLabelText("Awaiting your answer");
    expect(badge).toBeTruthy();
    expect(badge.textContent).toContain("needs answer");
    expect(badge.getAttribute("title")).toBe("needs answer");
  });

  it("renders 'needs answer' for 'swarm-questions' kind", () => {
    renderSidebar(mkTask({ phase: "plan" }), "swarm-questions");
    const badge = screen.getByLabelText("Awaiting your answer");
    expect(badge.textContent).toContain("needs answer");
  });

  it("renders 'ready to implement' for 'plan-ready' kind", () => {
    renderSidebar(mkTask({ phase: "plan-ready" }), "plan-ready");
    const badge = screen.getByLabelText("Plan ready — awaiting your action");
    expect(badge.textContent).toContain("ready to implement");
    expect(badge.getAttribute("title")).toBe("ready to implement");
  });

  it("renders 'ready to launch' for 'swarm-plan-ready' kind", () => {
    renderSidebar(mkTask({ phase: "plan-ready" }), "swarm-plan-ready");
    const badge = screen.getByLabelText("Plan ready — awaiting your action");
    expect(badge.textContent).toContain("ready to launch");
    expect(badge.getAttribute("title")).toBe("ready to launch");
  });

  it("does not render the badge when no awaiting kind is set", () => {
    renderSidebar(mkTask({ phase: "intake" }), null);
    expect(screen.queryByLabelText(/Awaiting/)).toBeNull();
    expect(screen.queryByLabelText(/Plan ready/)).toBeNull();
  });

  it("does not render the badge when the task is the active selection", () => {
    const task = mkTask({ phase: "plan-ready" });
    renderSidebar(task, "plan-ready", { activeId: task.id });
    expect(screen.queryByLabelText(/Plan ready/)).toBeNull();
  });
});
