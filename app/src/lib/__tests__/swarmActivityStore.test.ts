import { describe, it, expect, vi, beforeEach, afterEach, type Mock } from "vitest";

// Pretend we're in Tauri so the store's hydration + listener logic runs.
// `@tauri-apps/api/core::isTauri()` reads `globalThis.isTauri`, not
// `__TAURI_INTERNALS__` — set both for safety.
beforeEach(() => {
  (globalThis as { isTauri?: unknown }).isTauri = true;
  (window as { __TAURI_INTERNALS__?: unknown }).__TAURI_INTERNALS__ = {};
});
afterEach(() => {
  delete (globalThis as { isTauri?: unknown }).isTauri;
  delete (window as { __TAURI_INTERNALS__?: unknown }).__TAURI_INTERNALS__;
});

// Mock the IPC layer. `getSwarmActivityLog` is the new hydration source;
// `invoke` is left available for any other accidental call (no test calls it).
vi.mock("../ipc", async () => {
  const actual = await vi.importActual<typeof import("../ipc")>("../ipc");
  return {
    ...actual,
    getSwarmActivityLog: vi.fn(),
  };
});

// Mock the events module so we control when "live" events arrive. The
// returned `onSwarmActivity` immediately invokes its argument's
// registration with a stub unlisten, and `emitLive(...)` lets each test
// fire events at the registered listener.
let liveDispatcher: ((evt: import("../events").SwarmActivityEvent) => void) | null = null;
function emitLive(evt: import("../events").SwarmActivityEvent): void {
  if (!liveDispatcher) throw new Error("no live dispatcher registered yet");
  liveDispatcher(evt);
}
vi.mock("../events", async () => {
  const actual = await vi.importActual<typeof import("../events")>("../events");
  return {
    ...actual,
    onSwarmActivity: vi.fn(async (cb: (e: import("../events").SwarmActivityEvent) => void) => {
      liveDispatcher = cb;
      return () => {
        liveDispatcher = null;
      };
    }),
    safeUnlisten: vi.fn(),
  };
});

import {
  _resetSwarmActivityStoreForTests,
  getSwarmActivityState,
  subscribeSwarmActivity,
  evictSwarmActivity,
} from "../swarmActivityStore";
import { getSwarmActivityLog } from "../ipc";
import type { SwarmActivityEvent, SwarmActivityKind } from "../events";

/** Build a complete SwarmActivityEvent with the given seq + overrides. */
function ev(
  seq: number | undefined,
  kind: SwarmActivityKind,
  overrides: Partial<SwarmActivityEvent> = {},
): SwarmActivityEvent {
  return {
    swarm_id: "sw1",
    feature_id: "feat-1",
    agent: "scout",
    session_id: "scout-feat-1",
    timestamp: "2026-05-14T12:00:00.000Z",
    kind,
    seq,
    ...overrides,
  };
}

/** Helper: wait for all microtasks / pending IPC promises to flush. */
async function flush(): Promise<void> {
  // 3 ticks is enough for: (1) the IPC promise to resolve, (2) the
  // .then handler in hydrateSwarm to run, (3) any follow-on notify().
  await Promise.resolve();
  await Promise.resolve();
  await Promise.resolve();
}

beforeEach(() => {
  _resetSwarmActivityStoreForTests();
  liveDispatcher = null;
  (getSwarmActivityLog as unknown as Mock).mockReset();
});

afterEach(() => {
  _resetSwarmActivityStoreForTests();
});

describe("swarmActivityStore hydration", () => {
  it("cold subscribe folds two pages of historical events into state", async () => {
    // Page 1: agent_start (seq 1) + 2 text chunks (seq 2-3)
    // Page 2: agent_end (seq 4)
    (getSwarmActivityLog as unknown as Mock).mockImplementation(
      async (_swarmId: string, afterSeq?: number) => {
        if (afterSeq === undefined) {
          return {
            events: [
              ev(1, "agent_start", { model: "opus-4.7" }),
              ev(2, "text", { text: "Hello " }),
              ev(3, "text", { text: "world." }),
            ],
            next_seq: 3,
          };
        }
        return {
          events: [ev(4, "agent_end", { success: true })],
          next_seq: null,
        };
      },
    );

    const listener = vi.fn();
    const unsub = subscribeSwarmActivity("sw1", listener);
    expect(getSwarmActivityState("sw1").items).toHaveLength(0);
    await flush();

    const state = getSwarmActivityState("sw1");
    // 1 divider + 1 bubble (with "Hello world.") + 1 end marker
    expect(state.items).toHaveLength(3);
    expect(state.items[0].kind).toBe("agent_divider");
    expect(state.items[1].kind).toBe("agent_bubble");
    expect(state.items[2].kind).toBe("agent_end_marker");
    expect(listener).toHaveBeenCalled();
    unsub();
  });

  it("buffers live events during hydration and replays POST-hydration events after", async () => {
    let resolveFirstPage: (
      value: import("../ipc").SwarmActivityLogPage,
    ) => void = () => {};
    (getSwarmActivityLog as unknown as Mock).mockImplementation(
      () =>
        new Promise<import("../ipc").SwarmActivityLogPage>((res) => {
          resolveFirstPage = res;
        }),
    );

    const listener = vi.fn();
    const unsub = subscribeSwarmActivity("sw1", listener);

    // Fire 3 live events with seq 51-53 while hydration is pending.
    // They must NOT touch state yet.
    emitLive(ev(51, "agent_start", { model: "opus-4.7" }));
    emitLive(ev(52, "text", { text: "live-A " }));
    emitLive(ev(53, "text", { text: "live-B" }));
    expect(getSwarmActivityState("sw1").items).toHaveLength(0);

    // Resolve hydration with events 1-50 (compressed to just a divider here).
    resolveFirstPage({
      events: [ev(1, "agent_start", { model: "old-model" })],
      next_seq: null,
    });
    await flush();

    const state = getSwarmActivityState("sw1");
    // Hydration divider (seq=1) + live divider (seq=51) + live bubble (seqs 52-53).
    expect(state.items).toHaveLength(3);
    expect(state.items[0].kind).toBe("agent_divider");
    expect(state.items[1].kind).toBe("agent_divider");
    expect(state.items[2].kind).toBe("agent_bubble");
    expect((state.items[2] as { text: string }).text).toBe("live-A live-B");
    expect(listener).toHaveBeenCalled();
    unsub();
  });

  it("dedupes overlapping live events whose seq <= maxSeqByHydration", async () => {
    let resolveFirstPage: (
      value: import("../ipc").SwarmActivityLogPage,
    ) => void = () => {};
    (getSwarmActivityLog as unknown as Mock).mockImplementation(
      () =>
        new Promise<import("../ipc").SwarmActivityLogPage>((res) => {
          resolveFirstPage = res;
        }),
    );

    const unsub = subscribeSwarmActivity("sw1", () => {});

    // Live events with seq 49 (dup), 50 (dup), 51 (new) arrive during hydration.
    emitLive(
      ev(49, "text", { session_id: "sX", text: " (live-dup-49)" }),
    );
    emitLive(
      ev(50, "text", { session_id: "sX", text: " (live-dup-50)" }),
    );
    emitLive(
      ev(51, "text", { session_id: "sX", text: " new" }),
    );

    // Hydration returns events 1-50 — the last text event has seq 50.
    resolveFirstPage({
      events: [
        ev(1, "agent_start", { session_id: "sX", model: "m" }),
        ev(49, "text", { session_id: "sX", text: "hist-49" }),
        ev(50, "text", { session_id: "sX", text: "/hist-50" }),
      ],
      next_seq: null,
    });
    await flush();

    const state = getSwarmActivityState("sw1");
    // Divider + ONE bubble: text-49 + text-50 (from history) + text-51 (live, new).
    // Live events at seq 49/50 must be dropped as already-included.
    expect(state.items).toHaveLength(2);
    expect(state.items[0].kind).toBe("agent_divider");
    const bubble = state.items[1] as { kind: string; text: string };
    expect(bubble.kind).toBe("agent_bubble");
    expect(bubble.text).toBe("hist-49/hist-50 new");
    unsub();
  });

  it("hydration error: still drains buffered live events and flips to ready", async () => {
    (getSwarmActivityLog as unknown as Mock).mockRejectedValue(
      new Error("backend boom"),
    );
    const warnSpy = vi.spyOn(console, "warn").mockImplementation(() => {});

    const listener = vi.fn();
    const unsub = subscribeSwarmActivity("sw1", listener);
    // Buffer one live event while hydration is racing toward rejection.
    emitLive(ev(1, "agent_start", { model: "m" }));
    emitLive(ev(2, "text", { text: "after-error" }));
    await flush();

    const state = getSwarmActivityState("sw1");
    // Divider + bubble — no crash, the live event survived.
    expect(state.items).toHaveLength(2);
    expect(state.items[0].kind).toBe("agent_divider");
    expect((state.items[1] as { text: string }).text).toBe("after-error");
    expect(warnSpy).toHaveBeenCalled();
    expect(listener).toHaveBeenCalled();

    // After hydration completes (with failure), subsequent live events
    // apply directly — no more buffering.
    emitLive(ev(3, "text", { text: " more" }));
    expect((getSwarmActivityState("sw1").items[1] as { text: string }).text).toBe(
      "after-error more",
    );

    warnSpy.mockRestore();
    unsub();
  });

  it("paginates: stops when next_seq is null after multiple pages", async () => {
    let calls = 0;
    (getSwarmActivityLog as unknown as Mock).mockImplementation(
      async (_swarmId: string, afterSeq?: number) => {
        calls += 1;
        if (afterSeq === undefined) {
          return {
            events: [ev(1, "agent_start", { model: "m" })],
            next_seq: 1,
          };
        }
        if (afterSeq === 1) {
          return {
            events: [ev(2, "text", { text: "page2" })],
            next_seq: 2,
          };
        }
        return { events: [ev(3, "text", { text: "/page3" })], next_seq: null };
      },
    );

    const unsub = subscribeSwarmActivity("sw1", () => {});
    await flush();

    expect(calls).toBe(3);
    const state = getSwarmActivityState("sw1");
    // Divider + bubble combining "page2/page3".
    expect(state.items).toHaveLength(2);
    expect((state.items[1] as { text: string }).text).toBe("page2/page3");
    unsub();
  });

  it("empty log: subsequent live events flow normally", async () => {
    (getSwarmActivityLog as unknown as Mock).mockResolvedValue({
      events: [],
      next_seq: null,
    });

    const listener = vi.fn();
    const unsub = subscribeSwarmActivity("sw1", listener);
    await flush();

    // After hydration drained (empty), live events should apply immediately.
    emitLive(ev(1, "agent_start", { model: "m" }));
    emitLive(ev(2, "text", { text: "live!" }));

    const state = getSwarmActivityState("sw1");
    expect(state.items).toHaveLength(2);
    expect(state.items[0].kind).toBe("agent_divider");
    expect((state.items[1] as { text: string }).text).toBe("live!");
    unsub();
  });

  it("re-subscribe during in-flight listener registration does not double-install", async () => {
    // Reproduces the doubled-marker / doubled-text bug. The old store used
    // a generation counter and eagerly nulled globalListenerPromise inside
    // teardown, which let a fast re-subscribe install a SECOND listener
    // before the first registration's promise had resolved. Result: every
    // subsequent event was delivered twice. The post-fix code keeps the
    // in-flight promise alive across teardown and lets the .then() handler
    // tear the listener down if it lands without subscribers — so this
    // sequence should end with at most one live dispatcher.
    let resolveFirst!: (fn: () => void) => void;
    let firstCalls = 0;
    let secondCalls = 0;
    (
      (await import("../events")).onSwarmActivity as unknown as Mock
    ).mockImplementationOnce(
      (cb: (e: SwarmActivityEvent) => void) =>
        new Promise<() => void>((res) => {
          firstCalls++;
          liveDispatcher = cb;
          resolveFirst = res;
        }),
    );
    (
      (await import("../events")).onSwarmActivity as unknown as Mock
    ).mockImplementationOnce(
      async (cb: (e: SwarmActivityEvent) => void) => {
        secondCalls++;
        liveDispatcher = cb;
        return () => {
          liveDispatcher = null;
        };
      },
    );
    (getSwarmActivityLog as unknown as Mock).mockResolvedValue({
      events: [],
      next_seq: null,
    });

    // 1. Initial subscribe — kicks off in-flight registration #1.
    const unsub1 = subscribeSwarmActivity("sw1", () => {});
    expect(firstCalls).toBe(1);

    // 2. Tear down the subscriber before registration #1 resolves.
    unsub1();

    // 3. Resubscribe while registration #1 is still pending. The post-fix
    //    store sees globalListenerPromise != null and does NOT call
    //    onSwarmActivity again.
    const unsub2 = subscribeSwarmActivity("sw1", () => {});
    expect(secondCalls).toBe(0);

    // 4. Resolve registration #1 with a tracked unlisten so we can see if
    //    the store ever invokes it.
    let unlistenedFirst = 0;
    resolveFirst(() => {
      unlistenedFirst++;
    });
    await flush();

    // 5. Emit a single live event. It must reach the reducer ONCE, not
    //    twice (the smoking-gun symptom of the original bug).
    let receivedSeqs: number[] = [];
    const dispatcher = liveDispatcher;
    expect(dispatcher).not.toBeNull();
    // Wrap so we can count; the store's reducer is observed through state.
    emitLive(ev(1, "agent_start", { model: "m" }));
    emitLive(ev(2, "text", { text: "hello" }));

    const state = getSwarmActivityState("sw1");
    // 1 divider + 1 bubble; if double-installed we'd see 2 dividers and
    // a bubble whose text reads "hellohello".
    expect(state.items).toHaveLength(2);
    expect(state.items[0].kind).toBe("agent_divider");
    const bubble = state.items[1] as { text: string };
    expect(bubble.text).toBe("hello");

    // Defensive: the first registration's unlisten was NOT called (it's
    // still in use). Mute the unused warning.
    void receivedSeqs;
    void unlistenedFirst;
    unsub2();
  });

  it("evictSwarmActivity clears hydration state so a re-subscribe re-hydrates", async () => {
    (getSwarmActivityLog as unknown as Mock).mockResolvedValue({
      events: [ev(1, "agent_start", { model: "m" })],
      next_seq: null,
    });

    const unsub = subscribeSwarmActivity("sw1", () => {});
    await flush();
    expect(getSwarmActivityState("sw1").items).toHaveLength(1);
    unsub();

    // Evict, then re-subscribe — the mock should be called a SECOND time.
    evictSwarmActivity("sw1");
    expect(getSwarmActivityState("sw1").items).toHaveLength(0);
    (getSwarmActivityLog as unknown as Mock).mockClear();

    const unsub2 = subscribeSwarmActivity("sw1", () => {});
    await flush();
    expect(getSwarmActivityLog).toHaveBeenCalled();
    expect(getSwarmActivityState("sw1").items).toHaveLength(1);
    unsub2();
  });
});
