import React from "react";
import {
  describe,
  it,
  expect,
  vi,
  beforeEach,
  afterEach,
} from "vitest";
import { render, act, cleanup } from "@testing-library/react";

// ── Tauri presence: the provider's effects no-op when isTauri() is false.
vi.mock("../tauri", () => ({ isTauri: () => true }));

// ── Project context
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

// ── @tauri-apps/api/event: stubbed listen — we don't need to fire events
// for this suite; we drive state directly via updateTask.
vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(async () => () => {}),
}));

// ── HivemindEventStore
vi.mock("../hivemindEventStore", () => ({
  subscribeHivemindEventListener: vi.fn(() => () => {}),
}));

// ── Sounds
vi.mock("../sounds", () => ({
  getCompletionSoundConfig: () => ({ enabled: false, sound: "ding" }),
  playCompletionSound: vi.fn(),
}));

// ── IPC layer — only the methods touched at mount need to be stubbed.
vi.mock("../ipc", () => ({
  killPiSession: vi.fn(() => Promise.resolve()),
  deleteChatSession: vi.fn(() => Promise.resolve()),
  stopChat: vi.fn(() => Promise.resolve()),
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
  startReview: vi.fn(() => Promise.resolve("")),
  cancelReview: vi.fn(() => Promise.resolve()),
  getResumableReviewForTask: vi.fn(() => Promise.resolve(null)),
  getReviewState: vi.fn(() => Promise.resolve(null)),
  registerContextSession: vi.fn(() => Promise.resolve()),
  autoCommitTask: vi.fn(() => Promise.resolve({ ok: false, message: "" })),
  getSwarm: vi.fn(() => Promise.resolve(null)),
  startSwarm: vi.fn(() => Promise.resolve()),
}));

import {
  TaskRuntimeProvider,
  useTaskRuntimeState,
  TASK_LIST_KEY,
  type AwaitingInputKind,
} from "../taskRuntime";
import type { TaskQuestion } from "../questions";

const TASK_SESSIONS_KEY = "hyvemind:task-sessions";

interface Captured {
  state: ReturnType<typeof useTaskRuntimeState> | null;
}
function Consumer({ captured }: { captured: Captured }) {
  captured.state = useTaskRuntimeState();
  return null;
}

function seedTask(opts: { taskId: string; phase?: string; swarmId?: string | null }) {
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
      swarmId: opts.swarmId ?? undefined,
    },
  ];
  const sessions = {
    [opts.taskId]: {
      sessionId: null,
      model: "anthropic/claude-sonnet-4",
      taskPhase: opts.phase ?? "intake",
    },
  };
  localStorage.setItem(TASK_LIST_KEY, JSON.stringify(list));
  localStorage.setItem(TASK_SESSIONS_KEY, JSON.stringify(sessions));
}

async function flushMount(): Promise<void> {
  for (let i = 0; i < 5; i++) {
    await act(async () => {
      await Promise.resolve();
    });
  }
}

function renderProvider() {
  const captured: Captured = { state: null };
  const utils = render(
    <TaskRuntimeProvider>
      <Consumer captured={captured} />
    </TaskRuntimeProvider>,
  );
  return { ...utils, captured };
}

const Q: TaskQuestion = {
  id: "q1",
  text: "Which framework?",
  type: "open",
} as any;

beforeEach(() => {
  localStorage.clear();
  vi.clearAllMocks();
});

afterEach(() => {
  cleanup();
});

describe("awaitingInputTaskIds derivation", () => {
  it("emits 'questions' when pendingQuestions is non-empty", async () => {
    seedTask({ taskId: "t-q" });
    const { captured } = renderProvider();
    await flushMount();

    await act(async () => {
      captured.state!.updateTask("t-q", (t) => ({
        ...t,
        streaming: false,
        pendingQuestions: [Q],
      }));
    });
    await flushMount();

    expect(captured.state!.awaitingInputTaskIds["t-q"]).toBe<AwaitingInputKind>(
      "questions",
    );
  });

  it("emits 'swarm-questions' when pendingSwarmQuestions is non-empty", async () => {
    seedTask({ taskId: "t-sq", swarmId: "swarm-1" });
    const { captured } = renderProvider();
    await flushMount();

    await act(async () => {
      captured.state!.updateTask("t-sq", (t) => ({
        ...t,
        streaming: false,
        pendingSwarmQuestions: [
          { id: "sq1", text: "What stack?", type: "open" } as any,
        ],
      }));
    });
    await flushMount();

    expect(captured.state!.awaitingInputTaskIds["t-sq"]).toBe<AwaitingInputKind>(
      "swarm-questions",
    );
  });

  it("emits 'plan-ready' when plan-ready + auto off + no review + no swarmId", async () => {
    seedTask({ taskId: "t-pr" });
    const { captured } = renderProvider();
    await flushMount();

    await act(async () => {
      captured.state!.updateTask("t-pr", (t) => ({
        ...t,
        streaming: false,
        phase: "plan-ready",
        autoMode: "off",
        reviewProgress: null,
        swarmId: null,
      }));
    });
    await flushMount();

    expect(captured.state!.awaitingInputTaskIds["t-pr"]).toBe<AwaitingInputKind>(
      "plan-ready",
    );
  });

  it("emits 'swarm-plan-ready' when plan-ready + auto off + no review + swarmId set", async () => {
    seedTask({ taskId: "t-spr", swarmId: "swarm-1" });
    const { captured } = renderProvider();
    await flushMount();

    await act(async () => {
      captured.state!.updateTask("t-spr", (t) => ({
        ...t,
        streaming: false,
        phase: "plan-ready",
        autoMode: "off",
        reviewProgress: null,
        swarmId: "swarm-1",
      }));
    });
    await flushMount();

    expect(captured.state!.awaitingInputTaskIds["t-spr"]).toBe<AwaitingInputKind>(
      "swarm-plan-ready",
    );
  });

  it("streaming wins: no entry even if pendingQuestions is set", async () => {
    seedTask({ taskId: "t-stream" });
    const { captured } = renderProvider();
    await flushMount();

    await act(async () => {
      captured.state!.updateTask("t-stream", (t) => ({
        ...t,
        streaming: true,
        pendingQuestions: [Q],
      }));
    });
    await flushMount();

    expect(captured.state!.awaitingInputTaskIds["t-stream"]).toBeUndefined();
  });

  it("autoMode='full' suppresses 'plan-ready' entry", async () => {
    seedTask({ taskId: "t-auto" });
    const { captured } = renderProvider();
    await flushMount();

    await act(async () => {
      captured.state!.updateTask("t-auto", (t) => ({
        ...t,
        streaming: false,
        phase: "plan-ready",
        autoMode: "full",
        reviewProgress: null,
        swarmId: null,
      }));
    });
    await flushMount();

    expect(captured.state!.awaitingInputTaskIds["t-auto"]).toBeUndefined();
  });

  it("autoMode='review' still emits 'plan-ready' so user can click Implement", async () => {
    seedTask({ taskId: "t-auto-review" });
    const { captured } = renderProvider();
    await flushMount();

    await act(async () => {
      captured.state!.updateTask("t-auto-review", (t) => ({
        ...t,
        streaming: false,
        phase: "plan-ready",
        autoMode: "review",
        reviewProgress: null,
        swarmId: null,
      }));
    });
    await flushMount();

    expect(captured.state!.awaitingInputTaskIds["t-auto-review"]).toBe<AwaitingInputKind>(
      "plan-ready",
    );
  });

  it("pendingFeaturesRefresh suppresses 'swarm-plan-ready' entry (waiting on Queen, not user)", async () => {
    seedTask({ taskId: "t-pfr", swarmId: "swarm-1" });
    const { captured } = renderProvider();
    await flushMount();

    await act(async () => {
      captured.state!.updateTask("t-pfr", (t) => ({
        ...t,
        streaming: false,
        phase: "plan-ready",
        autoMode: "off",
        reviewProgress: null,
        swarmId: "swarm-1",
        pendingFeaturesRefresh: true,
        featuresRefreshFailed: false,
      }));
    });
    await flushMount();

    expect(captured.state!.awaitingInputTaskIds["t-pfr"]).toBeUndefined();
  });

  it("featuresRefreshFailed re-enables 'swarm-plan-ready' entry (user needs to act)", async () => {
    seedTask({ taskId: "t-frf", swarmId: "swarm-1" });
    const { captured } = renderProvider();
    await flushMount();

    await act(async () => {
      captured.state!.updateTask("t-frf", (t) => ({
        ...t,
        streaming: false,
        phase: "plan-ready",
        autoMode: "off",
        reviewProgress: null,
        swarmId: "swarm-1",
        pendingFeaturesRefresh: false,
        featuresRefreshFailed: true,
      }));
    });
    await flushMount();

    expect(captured.state!.awaitingInputTaskIds["t-frf"]).toBe<AwaitingInputKind>(
      "swarm-plan-ready",
    );
  });

  it("reviewProgress non-null suppresses 'plan-ready' entry", async () => {
    seedTask({ taskId: "t-rev" });
    const { captured } = renderProvider();
    await flushMount();

    await act(async () => {
      captured.state!.updateTask("t-rev", (t) => ({
        ...t,
        streaming: false,
        phase: "plan-ready",
        autoMode: "off",
        reviewProgress: { currentRound: 1, totalRounds: 2 } as any,
        swarmId: null,
      }));
    });
    await flushMount();

    expect(captured.state!.awaitingInputTaskIds["t-rev"]).toBeUndefined();
  });
});
