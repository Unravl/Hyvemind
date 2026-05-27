import React from "react";
import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, act, waitFor } from "@testing-library/react";
import type { SwarmActivityEvent } from "../../lib/events";

vi.mock("@tauri-apps/api/core", () => ({
  isTauri: () => true,
}));

let activitySubscriber: ((evt: SwarmActivityEvent) => void) | null = null;

vi.mock("../../lib/events", () => ({
  onSwarmActivity: vi.fn((cb: (e: SwarmActivityEvent) => void) => {
    activitySubscriber = cb;
    return Promise.resolve(() => {
      activitySubscriber = null;
    });
  }),
  safeUnlisten: (fn: unknown) => {
    if (typeof fn === "function") {
      try {
        (fn as () => void)();
      } catch {
        /* noop */
      }
    }
  },
}));

import { ActivityStream } from "../ActivityStream";
import {
  _resetSwarmActivityStoreForTests,
  getSwarmActivityState,
  subscribeSwarmActivity,
} from "../../lib/swarmActivityStore";
import { toStreamEntries } from "../../lib/swarmActivityReducer";
import type { ActiveSession, SessionMarkerEntry } from "../../lib/streamEntry";

interface HarnessProps {
  swarmId: string;
  status: "running" | "paused" | "completed" | "failed" | "cancelled" | string;
  showReasoning: boolean;
  showToolCalls: boolean;
  onActiveSessionChange?: (info: ActiveSession | null) => void;
}

function ActivityStreamHarness({
  swarmId,
  status,
  showReasoning,
  showToolCalls,
  onActiveSessionChange,
}: HarnessProps) {
  const subscribe = React.useCallback(
    (l: () => void) => subscribeSwarmActivity(swarmId, l),
    [swarmId],
  );
  const getSnapshot = React.useCallback(
    () => getSwarmActivityState(swarmId),
    [swarmId],
  );
  const state = React.useSyncExternalStore(subscribe, getSnapshot, getSnapshot);
  const entries = React.useMemo(() => toStreamEntries(state), [state]);
  return (
    <ActivityStream
      entries={entries}
      showReasoning={showReasoning}
      showToolCalls={showToolCalls}
      streaming={status === "running"}
      tailLimit={300}
      onActiveSessionChange={(info) => onActiveSessionChange?.(info)}
    />
  );
}

async function dispatch(evt: SwarmActivityEvent) {
  await act(async () => {
    activitySubscriber?.(evt);
    await Promise.resolve();
  });
}

function baseEvt(over: Partial<SwarmActivityEvent>): SwarmActivityEvent {
  return {
    swarm_id: "sw1",
    feature_id: "feat-1",
    agent: "scout",
    session_id: "sess-abc",
    timestamp: new Date().toISOString(),
    kind: "text",
    ...over,
  } as SwarmActivityEvent;
}

describe("ActivityStream (swarm surface)", () => {
  beforeEach(() => {
    activitySubscriber = null;
    _resetSwarmActivityStoreForTests();
  });

  it("suppresses reasoning blocks when showReasoning=false", async () => {
    const { rerender } = render(
      <ActivityStreamHarness
        swarmId="sw1"
        status="running"
        showReasoning
        showToolCalls
      />,
    );
    await dispatch(baseEvt({ kind: "agent_start", model: "claude" }));
    await dispatch(baseEvt({ kind: "thinking", text: "deliberating quietly" }));
    expect(screen.queryAllByText(/reasoning/i).length).toBeGreaterThan(0);

    rerender(
      <ActivityStreamHarness
        swarmId="sw1"
        status="running"
        showReasoning={false}
        showToolCalls
      />,
    );
    expect(screen.queryByText(/deliberating quietly/)).not.toBeInTheDocument();
    expect(screen.queryAllByText(/reasoning/i).length).toBe(0);
  });

  it("merges interleaved thinking → text → thinking into one bubble (single turn)", async () => {
    render(
      <ActivityStreamHarness
        swarmId="sw1"
        status="running"
        showReasoning
        showToolCalls
      />,
    );
    await dispatch(
      baseEvt({
        kind: "agent_start",
        session_id: "sess-split",
        model: "claude",
      }),
    );
    await dispatch(
      baseEvt({ kind: "thinking", session_id: "sess-split", text: "turn-1 thoughts" }),
    );
    expect(screen.queryAllByText(/^reasoning$/i).length).toBe(1);

    await dispatch(
      baseEvt({ kind: "text", session_id: "sess-split", text: "turn-1 output" }),
    );
    await dispatch(
      baseEvt({ kind: "thinking", session_id: "sess-split", text: "turn-2 thoughts" }),
    );

    // Without a tool-call boundary, a `thinking → text → thinking` sequence
    // must collapse into the *same* bubble — only one ReasoningBlock header
    // should be present.
    expect(screen.queryAllByText(/^reasoning$/i).length).toBe(1);
    // Streaming text + reasoning go through useRafThrottled while the bubble
    // is the streaming tail, so use waitFor for the rendered string content.
    await waitFor(() =>
      expect(screen.getByText(/turn-1 output/)).toBeInTheDocument(),
    );
    await waitFor(() =>
      expect(screen.getByText(/turn-1 thoughtsturn-2 thoughts/)).toBeInTheDocument(),
    );
  });

  it("renders a fresh bubble when text follows a tool call (per-turn text)", async () => {
    render(
      <ActivityStreamHarness
        swarmId="sw1"
        status="running"
        showReasoning
        showToolCalls
      />,
    );
    await dispatch(
      baseEvt({
        kind: "agent_start",
        session_id: "sess-tx",
        model: "claude",
      }),
    );
    await dispatch(
      baseEvt({ kind: "text", session_id: "sess-tx", text: "calling a tool…" }),
    );
    await dispatch(
      baseEvt({
        kind: "tool_start",
        session_id: "sess-tx",
        tool_call_id: "tc-a",
        tool_name: "shell",
      }),
    );
    await dispatch(
      baseEvt({
        kind: "tool_end",
        session_id: "sess-tx",
        tool_call_id: "tc-a",
      }),
    );
    await dispatch(
      baseEvt({ kind: "text", session_id: "sess-tx", text: "final answer here" }),
    );

    expect(screen.getAllByText(/calling a tool/i).length).toBeGreaterThan(0);
    expect(screen.getAllByText(/final answer here/i).length).toBeGreaterThan(0);
  });

  it("suppresses tool-call groups when showToolCalls=false", async () => {
    const { rerender } = render(
      <ActivityStreamHarness
        swarmId="sw1"
        status="running"
        showReasoning
        showToolCalls
      />,
    );
    await dispatch(baseEvt({ kind: "agent_start", model: "claude" }));
    await dispatch(
      baseEvt({
        kind: "tool_start",
        tool_call_id: "tc-1",
        tool_name: "shell",
      }),
    );
    await dispatch(
      baseEvt({
        kind: "tool_update",
        tool_call_id: "tc-1",
        tool_output: "echo hi",
      }),
    );
    expect(screen.getAllByText(/shell/i).length).toBeGreaterThan(0);

    rerender(
      <ActivityStreamHarness
        swarmId="sw1"
        status="running"
        showReasoning
        showToolCalls={false}
      />,
    );
    expect(screen.queryByText(/echo hi/)).not.toBeInTheDocument();
  });

  it("latest reasoning block stays expanded by default", async () => {
    render(
      <ActivityStreamHarness
        swarmId="sw1"
        status="running"
        showReasoning
        showToolCalls
      />,
    );
    await dispatch(
      baseEvt({
        kind: "agent_start",
        session_id: "sess-keep",
        model: "claude",
      }),
    );
    await dispatch(
      baseEvt({
        kind: "thinking",
        session_id: "sess-keep",
        text: "why hello there",
      }),
    );
    expect(screen.getByText(/why hello there/)).toBeInTheDocument();
  });

  it("interleaved thinking after text appends to the same reasoning block", async () => {
    render(
      <ActivityStreamHarness
        swarmId="sw1"
        status="running"
        showReasoning
        showToolCalls
      />,
    );
    await dispatch(
      baseEvt({
        kind: "agent_start",
        session_id: "sess-coll",
        model: "claude",
      }),
    );
    await dispatch(
      baseEvt({ kind: "thinking", session_id: "sess-coll", text: "old thoughts" }),
    );
    await dispatch(
      baseEvt({ kind: "text", session_id: "sess-coll", text: "shipped it" }),
    );
    await dispatch(
      baseEvt({ kind: "thinking", session_id: "sess-coll", text: "new thoughts" }),
    );

    // Both reasoning bursts merge into the same bubble; the text stays
    // visible alongside them. Streaming content is RAF-throttled, so use
    // waitFor for the rendered strings.
    await waitFor(() =>
      expect(screen.getByText(/old thoughtsnew thoughts/)).toBeInTheDocument(),
    );
    await waitFor(() =>
      expect(screen.getByText(/shipped it/)).toBeInTheDocument(),
    );
    // And only one reasoning header — the two bursts merged into one block.
    expect(screen.queryAllByText(/^reasoning$/i).length).toBe(1);
  });

  it("with showToolCalls=false, two reasoning bubbles separated by a tool-only bubble merge into one ReasoningBlock", async () => {
    render(
      <ActivityStreamHarness
        swarmId="sw1"
        status="running"
        showReasoning
        showToolCalls={false}
      />,
    );
    await dispatch(
      baseEvt({
        kind: "agent_start",
        session_id: "sess-merge",
        model: "claude",
      }),
    );
    await dispatch(
      baseEvt({ kind: "thinking", session_id: "sess-merge", text: "alpha think" }),
    );
    await dispatch(
      baseEvt({
        kind: "tool_start",
        session_id: "sess-merge",
        tool_call_id: "tc-m",
        tool_name: "shell",
      }),
    );
    await dispatch(
      baseEvt({
        kind: "tool_end",
        session_id: "sess-merge",
        tool_call_id: "tc-m",
      }),
    );
    await dispatch(
      baseEvt({ kind: "text", session_id: "sess-merge", text: "x" }),
    );
    await dispatch(
      baseEvt({ kind: "thinking", session_id: "sess-merge", text: "beta think" }),
    );

    const reasoningHeaders = screen.queryAllByText(/^reasoning$/i);
    expect(reasoningHeaders.length).toBe(1);
    await waitFor(() =>
      expect(screen.getByText(/alpha think/)).toBeInTheDocument(),
    );
    expect(screen.getByText(/beta think/)).toBeInTheDocument();
  });

  it("with showToolCalls=false, a visible text bubble between tool-bounded reasoning bubbles breaks the merge (two blocks render)", async () => {
    render(
      <ActivityStreamHarness
        swarmId="sw1"
        status="running"
        showReasoning
        showToolCalls={false}
      />,
    );
    // Construction: bubbles are split only by tool calls now, so we use tool
    // boundaries to force three bubbles: [reasoning + tool], [visible text +
    // tool], [reasoning]. The middle bubble's visible text disqualifies the
    // streamReasoningMerge from joining the two reasoning bubbles around it.
    await dispatch(
      baseEvt({
        kind: "agent_start",
        session_id: "sess-brk",
        model: "claude",
      }),
    );
    await dispatch(
      baseEvt({ kind: "thinking", session_id: "sess-brk", text: "turn-1 think" }),
    );
    await dispatch(
      baseEvt({
        kind: "tool_start",
        session_id: "sess-brk",
        tool_call_id: "tc-brk-1",
        tool_name: "shell",
      }),
    );
    await dispatch(
      baseEvt({
        kind: "tool_end",
        session_id: "sess-brk",
        tool_call_id: "tc-brk-1",
      }),
    );
    await dispatch(
      baseEvt({
        kind: "text",
        session_id: "sess-brk",
        text: "committed plan to disk",
      }),
    );
    await dispatch(
      baseEvt({
        kind: "tool_start",
        session_id: "sess-brk",
        tool_call_id: "tc-brk-2",
        tool_name: "shell",
      }),
    );
    await dispatch(
      baseEvt({
        kind: "tool_end",
        session_id: "sess-brk",
        tool_call_id: "tc-brk-2",
      }),
    );
    await dispatch(
      baseEvt({ kind: "thinking", session_id: "sess-brk", text: "turn-2 think" }),
    );
    const reasoningHeaders = screen.queryAllByText(/^reasoning$/i);
    expect(reasoningHeaders.length).toBe(2);
  });

  it("with showToolCalls=true, no visual merging happens across tool boundaries (regression guard)", async () => {
    render(
      <ActivityStreamHarness
        swarmId="sw1"
        status="running"
        showReasoning
        showToolCalls
      />,
    );
    // With tool calls visible, each tool boundary produces its own bubble and
    // there is no merge step — each ReasoningBlock renders independently.
    await dispatch(
      baseEvt({
        kind: "agent_start",
        session_id: "sess-nomerge",
        model: "claude",
      }),
    );
    await dispatch(
      baseEvt({ kind: "thinking", session_id: "sess-nomerge", text: "think 1" }),
    );
    await dispatch(
      baseEvt({
        kind: "tool_start",
        session_id: "sess-nomerge",
        tool_call_id: "tc-nm",
        tool_name: "shell",
      }),
    );
    await dispatch(
      baseEvt({
        kind: "tool_end",
        session_id: "sess-nomerge",
        tool_call_id: "tc-nm",
      }),
    );
    await dispatch(
      baseEvt({ kind: "thinking", session_id: "sess-nomerge", text: "think 2" }),
    );
    const reasoningHeaders = screen.queryAllByText(/^reasoning$/i);
    expect(reasoningHeaders.length).toBe(2);
  });

  it("does not double-format entry.t in the pretty-pill ended label (Invalid Date regression)", () => {
    const marker: SessionMarkerEntry = {
      kind: "session_marker",
      surface: "swarm",
      phase: "end",
      id: "sm-end",
      label: "",
      agent: "scout",
      sessionId: "sess-end",
      success: true,
      // Already pre-formatted as "hh:mm:ss" by the adapter — must not be
      // re-fed into Date(…) here, which would produce "Invalid Date".
      t: "13:24:33",
    };
    render(
      <ActivityStream
        entries={[marker]}
        showReasoning
        showToolCalls
        streaming={false}
        tailLimit={300}
      />,
    );
    // After the RelativeTime refactor, `13:24:33` renders inside a nested
    // <span> so the surrounding label is split across text nodes. Match
    // against the concatenated textContent of the enclosing span instead.
    expect(
      screen.getByText((_content, el) => {
        if (!el || el.tagName.toLowerCase() !== "span") return false;
        return (el.textContent ?? "").replace(/\s+/g, " ").includes("ended 13:24:33 ✓");
      }),
    ).toBeInTheDocument();
    expect(screen.queryByText(/Invalid Date/)).not.toBeInTheDocument();
  });

  it("fires onActiveSessionChange once per agent transition, not on every event", async () => {
    const onChange = vi.fn();
    render(
      <ActivityStreamHarness
        swarmId="sw1"
        status="running"
        showReasoning
        showToolCalls
        onActiveSessionChange={onChange}
      />,
    );

    await dispatch(
      baseEvt({
        kind: "agent_start",
        session_id: "sess-scout",
        agent: "scout",
        model: "claude-sonnet",
      }),
    );
    const callsAfterScoutStart = onChange.mock.calls.length;
    expect(callsAfterScoutStart).toBeGreaterThanOrEqual(1);
    expect(onChange).toHaveBeenLastCalledWith(
      expect.objectContaining({
        sessionId: "sess-scout",
        model: "claude-sonnet",
        agent: "scout",
      }),
    );

    await dispatch(baseEvt({ kind: "text", session_id: "sess-scout", text: "a" }));
    await dispatch(baseEvt({ kind: "text", session_id: "sess-scout", text: "b" }));
    await dispatch(baseEvt({ kind: "text", session_id: "sess-scout", text: "c" }));
    expect(onChange.mock.calls.length).toBe(callsAfterScoutStart);

    await dispatch(
      baseEvt({
        kind: "agent_start",
        session_id: "sess-worker",
        agent: "worker",
        model: "claude-opus",
      }),
    );
    expect(onChange.mock.calls.length).toBe(callsAfterScoutStart + 1);
    expect(onChange).toHaveBeenLastCalledWith(
      expect.objectContaining({
        sessionId: "sess-worker",
        model: "claude-opus",
        agent: "worker",
      }),
    );
  });
});
