import React, { useEffect, useRef, useState } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, render } from "@testing-library/react";

/**
 * Mirror the two project-path effects from `TasksScreen` in a small harness
 * so we can drive their dependencies (active task id, picker value, default
 * project path) directly and assert the side effects.
 *
 * We borrow the scaffold pattern from `Tasks.mergedPlanSurvivesCollapse.test.tsx`
 * — mounting the real `TasksScreen` would require stubbing ~20 IPC commands,
 * Pi sessions, sound effects, and event listeners. The bug under test is
 * entirely contained in two effects; testing those effects in isolation
 * gives us the same regression coverage without the noise.
 *
 * Keep this harness in lock-step with the effects in `Tasks.tsx`. If they
 * change, update both sides.
 */

// `pathForCompare` is pulled from the real ProjectPicker module so we exercise
// the same case/slash normalization the real screen uses.
import { pathForCompare, projectFromPath } from "../../components/ProjectPicker";
import { workspaceLabel } from "../../lib/categories";

// Force `isTauri()` to true so the effects' early-return doesn't bail.
vi.mock("../../lib/tauri", () => ({ isTauri: () => true }));

type Project = ReturnType<typeof projectFromPath>;
interface TaskLike {
  id: string;
  projectPath?: string;
  project?: string;
}

interface HarnessProps {
  /** External controllers used in the test to drive the effects. */
  apiRef: { current: HarnessApi | null };
  initialActiveId: string;
  initialTasks: Record<string, { projectPath?: string }>;
  initialLocalTasks: TaskLike[];
  initialProject: Project | null;
  initialProjects: Project[];
  initialDefaultProjectPath: string;
  /** Spies exposed to the test. */
  setProject: (p: Project | null) => void;
  addProject: (p: Project) => void;
}

interface HarnessApi {
  setActiveId: (id: string) => void;
  setProjectExternal: (p: Project | null) => void;
  setDefaultProjectPath: (p: string) => void;
  /** Read the latest harness state for assertions. */
  getLocalTasks: () => TaskLike[];
  getTasks: () => Record<string, { projectPath?: string }>;
  getProject: () => Project | null;
}

function Harness({
  apiRef,
  initialActiveId,
  initialTasks,
  initialLocalTasks,
  initialProject,
  initialProjects,
  initialDefaultProjectPath,
  setProject: setProjectSpy,
  addProject: addProjectSpy,
}: HarnessProps) {
  const [activeId, setActiveId] = useState(initialActiveId);
  const [tasks, setTasks] = useState(initialTasks);
  const [localTasks, setLocalTasks] = useState<TaskLike[]>(initialLocalTasks);
  const [project, setProjectInternal] = useState<Project | null>(initialProject);
  const [projects, setProjects] = useState<Project[]>(initialProjects);
  const [defaultProjectPath, setDefaultProjectPath] = useState(
    initialDefaultProjectPath,
  );

  // Wrap setProject so the test sees every call (mirrors the real
  // ProjectPicker.setProject which the harness routes back into local state).
  const setProject = (p: Project | null) => {
    setProjectSpy(p);
    setProjectInternal(p);
  };

  const addProject = (p: Project) => {
    addProjectSpy(p);
    setProjects((prev) =>
      prev.some((x) => pathForCompare(x.cwd) === pathForCompare(p.cwd))
        ? prev
        : [...prev, p],
    );
  };

  const updateTask = (id: string, patch: (t: any) => any) => {
    setTasks((prev) => {
      const cur = prev[id] ?? {};
      return { ...prev, [id]: patch(cur) };
    });
  };

  // Expose imperative controls to the test.
  apiRef.current = {
    setActiveId,
    setProjectExternal: setProject,
    setDefaultProjectPath,
    getLocalTasks: () => localTasks,
    getTasks: () => tasks,
    getProject: () => project,
  };

  /* ── Restore per-task project path on switch ─────────────── */
  // EXACT mirror of the post-fix effect in Tasks.tsx.
  const defaultProjectPathRef = useRef(defaultProjectPath);
  defaultProjectPathRef.current = defaultProjectPath;
  useEffect(() => {
    const cur = tasks[activeId];
    const taskItem = localTasks.find((t) => t.id === activeId);
    const explicit = cur?.projectPath || taskItem?.projectPath || "";
    const resolved = explicit || defaultProjectPathRef.current || "";

    if (!resolved) {
      if (project !== null) setProject(null);
      return;
    }

    const key = pathForCompare(resolved);
    const existing = projects.find((p) => pathForCompare(p.cwd) === key);
    if (existing) {
      if (!project || pathForCompare(project.cwd) !== key) setProject(existing);
    } else {
      const p = projectFromPath(resolved);
      addProject(p);
      setProject(p);
    }

    if (!explicit) {
      setLocalTasks((prev) =>
        prev.map((t) =>
          t.id === activeId
            ? { ...t, projectPath: resolved, project: workspaceLabel(resolved) }
            : t,
        ),
      );
      updateTask(activeId, (t) => ({ ...t, projectPath: resolved }));
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [activeId]);

  /* ── Sync ProjectPicker → active task projectPath ───────── */
  // EXACT mirror of the post-fix effect in Tasks.tsx.
  const prevProjectRef = useRef(project?.cwd);
  useEffect(() => {
    const cwd = project?.cwd;
    const projectChanged = prevProjectRef.current !== cwd;
    prevProjectRef.current = cwd;
    if (!cwd) return;

    const cur = tasks[activeId];
    const item = localTasks.find((t) => t.id === activeId);
    const needsBackfill = !(cur?.projectPath || item?.projectPath);

    if (!projectChanged && !needsBackfill) return;

    setLocalTasks((prev) =>
      prev.map((t) =>
        t.id === activeId
          ? { ...t, projectPath: cwd, project: workspaceLabel(cwd) }
          : t,
      ),
    );
    updateTask(activeId, (t) => ({ ...t, projectPath: cwd }));
  }, [project?.cwd, activeId, tasks, localTasks]);

  return <div data-testid="harness-project-cwd">{project?.cwd ?? ""}</div>;
}

/* ── Test scaffold ─────────────────────────────────────────── */

function makeProject(cwd: string): Project {
  return projectFromPath(cwd);
}

interface ScenarioOpts {
  tasks: Record<string, { projectPath?: string }>;
  localTasks: TaskLike[];
  initialActiveId: string;
  initialProject: Project | null;
  initialProjects: Project[];
  defaultProjectPath?: string;
}

function mount(opts: ScenarioOpts) {
  const setProject = vi.fn();
  const addProject = vi.fn();
  const apiRef: { current: HarnessApi | null } = { current: null };
  const view = render(
    <Harness
      apiRef={apiRef}
      initialActiveId={opts.initialActiveId}
      initialTasks={opts.tasks}
      initialLocalTasks={opts.localTasks}
      initialProject={opts.initialProject}
      initialProjects={opts.initialProjects}
      initialDefaultProjectPath={opts.defaultProjectPath ?? ""}
      setProject={setProject}
      addProject={addProject}
    />,
  );
  return { view, setProject, addProject, apiRef };
}

describe("TasksScreen per-task projectPath effects", () => {
  beforeEach(() => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
  });
  afterEach(() => {
    vi.runOnlyPendingTimers();
    vi.useRealTimers();
    vi.clearAllMocks();
  });

  it("switches the picker to the new task's projectPath", async () => {
    const projA = makeProject("/path/A");
    const projB = makeProject("/path/B");
    const { setProject, apiRef } = mount({
      tasks: {
        "t-a": { projectPath: "/path/A" },
        "t-b": { projectPath: "/path/B" },
      },
      localTasks: [
        { id: "t-a", projectPath: "/path/A" },
        { id: "t-b", projectPath: "/path/B" },
      ],
      initialActiveId: "t-a",
      initialProject: projA,
      initialProjects: [projA, projB],
    });

    // Clear any initial-mount calls; we care about the switch.
    setProject.mockClear();

    await act(async () => {
      apiRef.current!.setActiveId("t-b");
    });

    // The picker should switch to /path/B, never inherit /path/A.
    expect(setProject).toHaveBeenCalled();
    const callsB = setProject.mock.calls;
    const lastBArg = callsB[callsB.length - 1][0];
    expect(lastBArg?.cwd).toBe("/path/B");
    expect(apiRef.current!.getProject()?.cwd).toBe("/path/B");
  });

  it("falls back to defaultProjectPath when the task has none, and back-fills the task", async () => {
    const projA = makeProject("/path/A");
    const projD = makeProject("/path/D");
    const { setProject, apiRef } = mount({
      tasks: {
        "t-a": { projectPath: "/path/A" },
        "t-empty": {}, // no projectPath
      },
      localTasks: [
        { id: "t-a", projectPath: "/path/A" },
        { id: "t-empty" }, // no projectPath
      ],
      initialActiveId: "t-a",
      initialProject: projA,
      initialProjects: [projA, projD],
      defaultProjectPath: "/path/D",
    });

    setProject.mockClear();

    await act(async () => {
      apiRef.current!.setActiveId("t-empty");
    });

    // The picker should switch to the DEFAULT, not silently inherit /path/A.
    expect(setProject).toHaveBeenCalled();
    const callsD = setProject.mock.calls;
    const lastDArg = callsD[callsD.length - 1][0];
    expect(lastDArg?.cwd).toBe("/path/D");

    // And the task's projectPath should be back-filled to the default.
    const backfilled = apiRef.current!.getLocalTasks().find((t) => t.id === "t-empty");
    expect(backfilled?.projectPath).toBe("/path/D");
    expect(apiRef.current!.getTasks()["t-empty"]?.projectPath).toBe("/path/D");
  });

  it("clears the picker (setProject(null)) when both task and default are empty", async () => {
    const projA = makeProject("/path/A");
    const { setProject, apiRef } = mount({
      tasks: {
        "t-a": { projectPath: "/path/A" },
        "t-empty": {},
      },
      localTasks: [
        { id: "t-a", projectPath: "/path/A" },
        { id: "t-empty" },
      ],
      initialActiveId: "t-a",
      initialProject: projA,
      initialProjects: [projA],
      defaultProjectPath: "",
    });

    setProject.mockClear();

    await act(async () => {
      apiRef.current!.setActiveId("t-empty");
    });

    // Picker should be cleared, NOT left pointing at /path/A.
    expect(setProject).toHaveBeenCalledWith(null);
    expect(apiRef.current!.getProject()).toBeNull();
  });

  it("back-fills the active task's projectPath when the user manually picks a project from the dropdown", async () => {
    const projA = makeProject("/path/A");
    const projE = makeProject("/path/E");
    const { apiRef } = mount({
      tasks: { "t-empty": {} },
      localTasks: [{ id: "t-empty" }],
      initialActiveId: "t-empty",
      // Picker starts at A (perhaps from a previous task); the restore
      // effect would normally fall back to a default and clear/back-fill,
      // but here we want to test the "user clicks E in the picker" path.
      initialProject: projA,
      initialProjects: [projA, projE],
      defaultProjectPath: "/path/A", // ensures restore back-fill doesn't beat us
    });

    // Pre-condition: after mount, restore effect has back-filled to /path/A.
    expect(apiRef.current!.getLocalTasks()[0]?.projectPath).toBe("/path/A");

    // User picks E from the dropdown — drive the sync effect.
    await act(async () => {
      apiRef.current!.setProjectExternal(projE);
    });

    // The task's projectPath now reflects the user's pick, even though it
    // was previously empty / mid-back-fill.
    expect(apiRef.current!.getLocalTasks()[0]?.projectPath).toBe("/path/E");
    expect(apiRef.current!.getTasks()["t-empty"]?.projectPath).toBe("/path/E");
  });
});
