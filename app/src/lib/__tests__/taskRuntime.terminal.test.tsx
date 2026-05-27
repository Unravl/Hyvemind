import React, { useEffect } from "react";
import {
  describe,
  it,
  expect,
  vi,
  beforeEach,
  afterEach,
  type Mock,
} from "vitest";
import { render, act, cleanup } from "@testing-library/react";

// ── Tauri presence: the provider's effects no-op when isTauri() is false.
vi.mock("../tauri", () => ({ isTauri: () => true }));

// ── Project context: provider expects `useProject()` to return something.
vi.mock("../../components/ProjectPicker", () => ({
  useProject: () => ({
    project: null,
    setProject: vi.fn(),
    projects: [],
    addProject: vi.fn(),
    removeProject: vi.fn(),
    updateProject: vi.fn(),
  }),
}));

// ── @tauri-apps/api/event: capture the registered listeners so we can
// fan events at them. Each `listen(name, cb)` returns a Promise<UnlistenFn>.
const eventListeners = new Map<string, Array<(event: { payload: any }) => void>>();
vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(async (name: string, cb: (event: { payload: any }) => void) => {
    const arr = eventListeners.get(name) || [];
    arr.push(cb);
    eventListeners.set(name, arr);
    return () => {
      const list = eventListeners.get(name);
      if (!list) return;
      const idx = list.indexOf(cb);
      if (idx >= 0) list.splice(idx, 1);
    };
  }),
}));

// ── HivemindEventStore: stub the subscribe function so the provider's
// effect doesn't blow up. Returns a no-op unsubscribe.
vi.mock("../hivemindEventStore", () => ({
  subscribeHivemindEventListener: vi.fn(() => () => {}),
}));

// ── Sounds: avoid trying to construct Audio() in jsdom.
vi.mock("../sounds", () => ({
  getCompletionSoundConfig: () => ({ enabled: false, sound: "ding" }),
  playCompletionSound: vi.fn(),
}));

// ── IPC layer: every method the provider may touch becomes a jest spy
// returning a benign value. The two we actually care about for assertions
// are `killPiSession` and `deleteChatSession`.
vi.mock("../ipc", () => ({
  // The methods this PR exercises:
  killPiSession: vi.fn(() => Promise.resolve()),
  deleteChatSession: vi.fn(() => Promise.resolve()),
  stopChat: vi.fn(() => Promise.resolve()),
  // Everything else the provider may touch at mount or in effects:
  loadTaskMessages: vi.fn(() => Promise.resolve("")),
  saveTaskMessages: vi.fn(() => Promise.resolve()),
  deleteTaskMessages: vi.fn(() => Promise.resolve()),
  reconcileActiveSessions: vi.fn(() => Promise.resolve([])),
  refreshModels: vi.fn(() => Promise.resolve([])),
  setDefaultModel: vi.fn(() => Promise.resolve()),
  listHiveminds: vi.fn(() => Promise.resolve([])),
  getSettings: vi.fn(() => Promise.resolve({})),
  logReviewEvent: vi.fn(() => Promise.resolve()),
  checkChatSession: vi.fn(() => Promise.resolve({})),
  formatIpcError: (e: unknown) => String(e),
  sendMessage: vi.fn(() => Promise.resolve("")),
  getSessionLastAssistantText: vi.fn(() => Promise.resolve("")),
  startReview: vi.fn(() => Promise.resolve("")),
  cancelReview: vi.fn(() => Promise.resolve()),
  getResumableReviewForTask: vi.fn(() => Promise.resolve(null)),
  getReviewState: vi.fn(() => Promise.resolve(null)),
  registerContextSession: vi.fn(() => Promise.resolve()),
  autoCommitTask: vi.fn(() => Promise.resolve({ ok: false, message: "" })),
  getSwarm: vi.fn(() => Promise.resolve(null)),
  startSwarm: vi.fn(() => Promise.resolve()),
}));

import * as ipc from "../ipc";
import {
  TaskRuntimeProvider,
  useTaskActions,
  useTaskRuntimeState,
  TASK_LIST_KEY,
} from "../taskRuntime";

// localStorage keys the provider reads at mount time. TASK_LIST_KEY is
// exported; TASK_SESSIONS_KEY is internal — duplicate the literal here.
const TASK_SESSIONS_KEY = "hyvemind:task-sessions";

// ── Test consumer: exposes the actions + current state via callbacks so
// each test can drive the provider deterministically.
interface Captured {
  actions: ReturnType<typeof useTaskActions> | null;
  state: ReturnType<typeof useTaskRuntimeState> | null;
}
function makeCaptured(): Captured {
  return { actions: null, state: null };
}
function Consumer({ captured }: { captured: Captured }) {
  captured.actions = useTaskActions();
  captured.state = useTaskRuntimeState();
  return null;
}

/** Seed a single hydrated task into localStorage so the provider's mount
 *  effect builds it into the `tasks` map with the desired sessionId and
 *  populates `sessionIdToTaskIdRef.current[sessionId] = taskId`. */
function seedTask(opts: {
  taskId: string;
  sessionId: string | null;
  phase?: string;
}) {
  const list = [
    {
      id: opts.taskId,
      group: "Active",
      title: "Test Task",
      project: "",
      model: "anthropic/claude-sonnet-4",
      phase: opts.phase ?? "intake",
      when: "now",
      preview: "",
      active: true,
      projectPath: null,
      createdAt: Date.now(),
    },
  ];
  const sessions = {
    [opts.taskId]: {
      sessionId: opts.sessionId,
      model: "anthropic/claude-sonnet-4",
      taskPhase: opts.phase ?? "intake",
    },
  };
  localStorage.setItem(TASK_LIST_KEY, JSON.stringify(list));
  localStorage.setItem(TASK_SESSIONS_KEY, JSON.stringify(sessions));
}

/** Wait for the provider's async mount effects (hydration from disk +
 *  localStorage) to settle. Three ticks covers: (1) the hydration promise
 *  resolution, (2) the subsequent setTasks setState, (3) any post-load
 *  effect that runs. */
async function flushMount(): Promise<void> {
  for (let i = 0; i < 5; i++) {
    await act(async () => {
      await Promise.resolve();
    });
  }
}

function renderProvider() {
  const captured = makeCaptured();
  const utils = render(
    <TaskRuntimeProvider>
      <Consumer captured={captured} />
    </TaskRuntimeProvider>,
  );
  return { ...utils, captured };
}

beforeEach(() => {
  localStorage.clear();
  eventListeners.clear();
  vi.clearAllMocks();
});

afterEach(() => {
  cleanup();
});

describe("TaskRuntimeProvider terminal cleanup", () => {
  it("marking a task done (manual mark-done) kills its Pi session", async () => {
    seedTask({ taskId: "task-1", sessionId: "sid-main" });
    const { captured } = renderProvider();
    await flushMount();

    // Sanity: the task hydrated with the expected sessionId.
    expect(captured.state?.tasks["task-1"]?.sessionId).toBe("sid-main");

    // Drive the same state mutation that `handleMarkDone` does.
    await act(async () => {
      captured.state!.updateTask("task-1", (t) => ({
        ...t,
        phase: "implement-done",
        streaming: false,
      }));
    });
    // Let the effect fire + the in-effect setState re-render settle.
    await flushMount();

    expect(ipc.killPiSession).toHaveBeenCalledWith("sid-main");
    expect((ipc.killPiSession as Mock).mock.calls.length).toBe(1);
  });

  it("agent submit_task_complete also kills the Pi session", async () => {
    seedTask({ taskId: "task-2", sessionId: "sid-agent" });
    renderProvider();
    await flushMount();

    // Fire a chat-event of type structured_task_complete on the task's
    // session. The reducer transitions phase → implement-done, the
    // terminal effect then force-kills the session.
    const chatListeners = eventListeners.get("chat-event") || [];
    expect(chatListeners.length).toBeGreaterThan(0);
    await act(async () => {
      for (const cb of chatListeners) {
        cb({
          payload: {
            session_id: "sid-agent",
            event_type: "structured_task_complete",
            content: JSON.stringify({
              summary: "done",
              success_state: "success",
            }),
          },
        });
      }
    });
    await flushMount();

    expect(ipc.killPiSession).toHaveBeenCalledWith("sid-agent");
  });

  it("routes answered-question follow-up events after the original session was evicted", async () => {
    seedTask({ taskId: "task-evicted", sessionId: "sid-evicted" });
    const { captured } = renderProvider();
    await flushMount();

    const evictionListeners = eventListeners.get("pi-session-evicted") || [];
    expect(evictionListeners.length).toBeGreaterThan(0);
    await act(async () => {
      for (const cb of evictionListeners) {
        cb({ payload: { session_id: "sid-evicted" } });
      }
    });
    await flushMount();

    await act(async () => {
      await captured.actions!.answerQuestions(
        "task-evicted",
        [
          {
            id: "count_source",
            kind: "choice",
            title: "Which count should the new Sessions button show?",
            options: [
              {
                id: "monitored",
                label: "Use monitored-sessions count",
              },
            ],
          },
        ],
        { count_source: "monitored" },
      );
    });
    await flushMount();

    expect(ipc.sendMessage).toHaveBeenCalledWith(
      expect.stringContaining("Use monitored-sessions count"),
      "anthropic/claude-sonnet-4",
      "sid-evicted",
      undefined,
      "high",
      expect.any(String),
      "read_only",
    );

    const chatListeners = eventListeners.get("chat-event") || [];
    expect(chatListeners.length).toBeGreaterThan(0);
    await act(async () => {
      for (const cb of chatListeners) {
        cb({
          payload: {
            session_id: "sid-evicted",
            event_type: "structured_plan",
            content: JSON.stringify({ plan_markdown: "## Fixed plan\n\nUse monitored sessions." }),
          },
        });
        cb({
          payload: {
            session_id: "sid-evicted",
            event_type: "done",
            content: "",
          },
        });
      }
    });
    await flushMount();

    const task = captured.state!.tasks["task-evicted"];
    expect(task.planText).toContain("Use monitored sessions.");
    expect(task.streaming).toBe(false);
    expect(task.messages.some((m) => m.who === "plan" && m.planText?.includes("Use monitored sessions."))).toBe(true);
  });

  it("deleting a task calls deleteChatSession for every owned session id", async () => {
    seedTask({ taskId: "task-3", sessionId: "sid-primary" });
    const { captured } = renderProvider();
    await flushMount();

    // Attach an internal Pi session via updateTask. (The reverse-map
    // entry for `sid-primary` is already populated by the hydration
    // path at mount, so collectTaskSessionIds will see both ids.)
    await act(async () => {
      captured.state!.updateTask("task-3", (t) => ({
        ...t,
        internalPi: {
          sessionId: "sid-internal",
          role: "context",
          targetTaskId: "task-3",
          startedAt: Date.now(),
        } as any,
      }));
    });
    await flushMount();

    await act(async () => {
      captured.actions!.deleteTask("task-3");
    });
    await flushMount();

    const calls = (ipc.deleteChatSession as Mock).mock.calls.map((c) => c[0]);
    expect(calls).toContain("sid-primary");
    expect(calls).toContain("sid-internal");
    // Exactly once per sid — collectTaskSessionIds dedupes via a Set.
    expect(calls.filter((c) => c === "sid-primary").length).toBe(1);
    expect(calls.filter((c) => c === "sid-internal").length).toBe(1);
  });

  it("implement-done effect is idempotent across re-renders", async () => {
    seedTask({ taskId: "task-4", sessionId: "sid-idem" });
    const { captured } = renderProvider();
    await flushMount();

    await act(async () => {
      captured.state!.updateTask("task-4", (t) => ({
        ...t,
        phase: "implement-done",
      }));
    });
    await flushMount();

    // Force one more render by touching unrelated state (a new updateTask
    // that returns a new object). The terminal effect MUST not fire again.
    await act(async () => {
      captured.state!.updateTask("task-4", (t) => ({
        ...t,
        error: "something",
      }));
    });
    await flushMount();

    expect((ipc.killPiSession as Mock).mock.calls.length).toBe(1);
    expect((ipc.killPiSession as Mock).mock.calls[0][0]).toBe("sid-idem");
  });

  it("deleting a task already in implement-done does not double-kill via the terminal effect", async () => {
    seedTask({ taskId: "task-5", sessionId: "sid-d", phase: "implement-done" });
    const { captured } = renderProvider();
    await flushMount();

    // The hydration seeded `phase: "implement-done"` directly. The
    // terminal effect should have fired exactly once at mount-time.
    expect((ipc.killPiSession as Mock).mock.calls.length).toBe(1);
    expect((ipc.killPiSession as Mock).mock.calls[0][0]).toBe("sid-d");

    // Now delete — sessionId on the task has been nulled by the terminal
    // effect, but the runtime captured `sidsToKill` from refs BEFORE the
    // teardown via collectTaskSessionIds. After the terminal effect's
    // updateTask call has cleared sessionId AND scrubbed the reverse map,
    // collectTaskSessionIds may return [] for this task. The terminal
    // effect MUST NOT re-fire killPiSession, which is the property the
    // guard set enforces.
    const killCallsBeforeDelete = (ipc.killPiSession as Mock).mock.calls.length;
    await act(async () => {
      captured.actions!.deleteTask("task-5");
    });
    await flushMount();

    // killPiSession not invoked again by the terminal effect.
    expect((ipc.killPiSession as Mock).mock.calls.length).toBe(
      killCallsBeforeDelete,
    );
  });
});
