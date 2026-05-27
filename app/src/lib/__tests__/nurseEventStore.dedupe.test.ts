import { describe, it, expect, vi, beforeEach, afterEach, type Mock } from "vitest";

// Pretend we're in Tauri so the store actually registers a listener.
beforeEach(() => {
  (globalThis as { isTauri?: unknown }).isTauri = true;
  (window as { __TAURI_INTERNALS__?: unknown }).__TAURI_INTERNALS__ = {};
});
afterEach(() => {
  delete (globalThis as { isTauri?: unknown }).isTauri;
  delete (window as { __TAURI_INTERNALS__?: unknown }).__TAURI_INTERNALS__;
});

// Mock the events module so we can observe how many times `onNurseEvent`
// is called and feed in synthetic events.
let liveDispatcher: ((evt: import("../../types/nurse").NurseEvent) => void) | null = null;
function emitLive(evt: import("../../types/nurse").NurseEvent): void {
  if (!liveDispatcher) throw new Error("no live dispatcher registered yet");
  liveDispatcher(evt);
}
vi.mock("../events", async () => {
  const actual = await vi.importActual<typeof import("../events")>("../events");
  return {
    ...actual,
    onNurseEvent: vi.fn(
      async (cb: (e: import("../../types/nurse").NurseEvent) => void) => {
        liveDispatcher = cb;
        return () => {
          liveDispatcher = null;
        };
      },
    ),
    safeUnlisten: vi.fn(),
  };
});

import {
  _resetNurseEventStoreForTests,
  recentEvents,
  subscribe,
  subscribeForSession,
} from "../nurseEventStore";
import { onNurseEvent } from "../events";

beforeEach(() => {
  _resetNurseEventStoreForTests();
  (onNurseEvent as unknown as Mock).mockClear();
  liveDispatcher = null;
});

afterEach(() => {
  _resetNurseEventStoreForTests();
});

/** Wait for microtasks to flush so async `.then` chains resolve. */
async function flush() {
  await Promise.resolve();
  await Promise.resolve();
}

describe("nurseEventStore: single Tauri listener", () => {
  it("registers exactly one listener regardless of how many subscribers attach", async () => {
    const unsub1 = subscribe(() => {});
    const unsub2 = subscribe(() => {});
    const unsub3 = subscribeForSession("sess-1", () => {});
    await flush();

    expect(onNurseEvent).toHaveBeenCalledTimes(1);

    unsub1();
    unsub2();
    unsub3();
  });

  it("delivers each event to global subscribers exactly once", async () => {
    const cb = vi.fn();
    const unsub = subscribe(cb);
    await flush();

    emitLive({
      event_type: "Intervention",
      id: "int-1",
      session_id: "sess-1",
      timestamp: "2026-05-19T00:00:00.000Z",
      level: "steer",
      analysis: "test",
      action_taken: {
        level: "steer",
        session_id: "sess-1",
        message: "hi",
        timestamp: "2026-05-19T00:00:00.000Z",
      },
      outcome: null,
    });

    expect(cb).toHaveBeenCalledTimes(1);
    unsub();
  });

  it("session-scoped subscribers only receive events for their session", async () => {
    const cbA = vi.fn();
    const cbB = vi.fn();
    const u1 = subscribeForSession("sess-A", cbA);
    const u2 = subscribeForSession("sess-B", cbB);
    await flush();

    const mkEv = (sid: string): import("../../types/nurse").NurseEvent => ({
      event_type: "Lifecycle",
      intervention_id: `int-${sid}`,
      status: "started",
      level: "steer",
      session_id: sid,
      observation: "obs",
      action: "act",
      timestamp: "2026-05-19T00:00:00.000Z",
    });
    emitLive(mkEv("sess-A"));
    emitLive(mkEv("sess-B"));
    emitLive(mkEv("sess-A"));

    expect(cbA).toHaveBeenCalledTimes(2);
    expect(cbB).toHaveBeenCalledTimes(1);
    u1();
    u2();
  });

  it("StatusUpdate is delivered to global subscribers but NOT session subscribers", async () => {
    const cbGlobal = vi.fn();
    const cbSession = vi.fn();
    subscribe(cbGlobal);
    subscribeForSession("sess-A", cbSession);
    await flush();

    emitLive({
      event_type: "StatusUpdate",
      stats: {
        monitored_count: 0,
        stall_count: 0,
        intervention_count: 0,
        last_check_at: null,
        is_running: true,
      },
      sessions: [],
      recent_interventions: [],
      config: {
        enabled: true,
        stall_threshold_secs: 300,
        nurse_model: "x",
        max_interventions: 3,
        tick_interval_secs: 60,
        nurse_provider: null,
      },
      health: {
        last_tick_at: null,
        last_successful_tick_at: null,
        consecutive_failed_ticks: 0,
        consecutive_bad_parse_ticks: 0,
        consecutive_skipped_ticks: 0,
        degraded: false,
      },
    });

    expect(cbGlobal).toHaveBeenCalledTimes(1);
    expect(cbSession).toHaveBeenCalledTimes(0);
  });

  it("ring buffer keeps recent events for late subscribers", async () => {
    subscribe(() => {});
    await flush();

    for (let i = 0; i < 5; i++) {
      emitLive({
        event_type: "UserNotice",
        session_id: `sess-${i}`,
        level: "info",
        message: `msg-${i}`,
        timestamp: "2026-05-19T00:00:00.000Z",
      });
    }

    const seen = recentEvents();
    expect(seen).toHaveLength(5);
    expect(
      seen.map((e) => (e as { message?: string }).message).join(","),
    ).toBe("msg-0,msg-1,msg-2,msg-3,msg-4");
  });

  it("subscribe/unsubscribe churn does not double-install the listener", async () => {
    const u1 = subscribe(() => {});
    await flush();
    u1();
    await flush();
    // Listener stayed in place across teardown OR was torn down once
    // — either way, a re-subscribe must NOT trigger a second listen()
    // before the first registration resolves. Re-mount and check.
    const u2 = subscribe(() => {});
    await flush();
    // Tauri's onNurseEvent may have been called once or twice depending
    // on teardown timing — but never more than that, and exactly one
    // dispatcher is wired at any moment.
    expect((onNurseEvent as unknown as Mock).mock.calls.length).toBeLessThanOrEqual(2);
    u2();
  });
});
