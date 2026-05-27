import React from "react";
import { describe, it, expect, vi } from "vitest";
import { render, screen, act } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { ActivityStream } from "../ActivityStream";
import type {
  ChatBubbleEntry,
  CompleteEntry,
  PlanEntry,
  QuestionsEntry,
  SessionMarkerEntry,
  StreamEntry,
} from "../../lib/streamEntry";

vi.mock("../../App", () => ({
  renderMd: (text: string) => text,
}));

function userBubble(id: string, text: string): ChatBubbleEntry {
  return {
    kind: "chat_bubble",
    surface: "task",
    who: "user",
    id,
    text,
  };
}

function asstBubble(
  id: string,
  text: string,
  overrides: Partial<ChatBubbleEntry> = {},
): ChatBubbleEntry {
  return {
    kind: "chat_bubble",
    surface: "task",
    who: "asst",
    id,
    text,
    ...overrides,
  };
}

function planEntry(id: string, planText: string): PlanEntry {
  return { kind: "plan", surface: "task", id, planText };
}

function questionsEntry(
  id: string,
  questions: QuestionsEntry["questions"],
): QuestionsEntry {
  return { kind: "questions", surface: "task", id, questions };
}

function completeEntry(
  id: string,
  overrides: Partial<CompleteEntry> = {},
): CompleteEntry {
  return { kind: "complete", surface: "task", id, ...overrides };
}

describe("ActivityStream (Tasks surface)", () => {
  it("renders the emptyState.primary text when there are no entries", () => {
    render(
      <ActivityStream
        entries={[]}
        showReasoning
        showToolCalls
        streaming={false}
        tailLimit={1000}
        emptyState={{ primary: "Start a conversation to begin." }}
      />,
    );
    expect(screen.getByText("Start a conversation to begin.")).toBeInTheDocument();
  });

  it("renders a plan entry with Implement button when showImplement=true", async () => {
    const onImplement = vi.fn();
    render(
      <ActivityStream
        entries={[planEntry("plan-1", "## Plan body\nFeature A\nFeature B")]}
        showReasoning
        showToolCalls
        streaming={false}
        tailLimit={1000}
        onImplementPlan={onImplement}
        planCard={{ showImplement: true }}
      />,
    );
    // PlanCard renders the plan title chrome and an Implement button.
    const implementBtn = await screen.findByRole("button", { name: /Implement/i });
    expect(implementBtn).toBeInTheDocument();
    await userEvent.setup().click(implementBtn);
    expect(onImplement).toHaveBeenCalledTimes(1);
  });

  it("does not render questions entries inline (handled by QuestionsDock)", () => {
    const entries: StreamEntry[] = [
      questionsEntry("q-1", [
        {
          id: "scope",
          kind: "choice",
          title: "Pick a scope",
          options: [
            { id: "small", label: "Small change" },
            { id: "big", label: "Big rewrite" },
          ],
        },
      ]),
    ];
    render(
      <ActivityStream
        entries={entries}
        showReasoning
        showToolCalls
        streaming={false}
        tailLimit={1000}
      />,
    );
    // Questions entries no longer render inside the conversation pane;
    // QuestionsDock owns the Q&A surface above the chat composer.
    expect(screen.queryByText("Pick a scope")).not.toBeInTheDocument();
    expect(screen.queryByText("Small change")).not.toBeInTheDocument();
    expect(
      screen.queryByText(/Questions card pinned below/),
    ).not.toBeInTheDocument();
  });

  it("renders a complete entry with the Task Complete chip", () => {
    render(
      <ActivityStream
        entries={[completeEntry("c-1")]}
        showReasoning
        showToolCalls
        streaming={false}
        tailLimit={1000}
      />,
    );
    expect(screen.getByText("Task Complete")).toBeInTheDocument();
  });

  it("renders the summary line underneath the Task Complete chip when text is set", () => {
    render(
      <ActivityStream
        entries={[
          completeEntry("c-2", {
            text: "Shipped the new tool plumbing.",
            successState: "success",
          }),
        ]}
        showReasoning
        showToolCalls
        streaming={false}
        tailLimit={1000}
      />,
    );
    expect(screen.getByText("Task Complete")).toBeInTheDocument();
    expect(
      screen.getByText("Shipped the new tool plumbing."),
    ).toBeInTheDocument();
  });

  it("renders the amber 'Task Partially Complete' chip when successState is 'partial'", () => {
    const { container } = render(
      <ActivityStream
        entries={[
          completeEntry("c-3", {
            text: "Two of three steps shipped.",
            successState: "partial",
          }),
        ]}
        showReasoning
        showToolCalls
        streaming={false}
        tailLimit={1000}
      />,
    );
    expect(
      screen.getByText("Task Partially Complete"),
    ).toBeInTheDocument();
    // Amber border class applied to the chip.
    expect(
      container.querySelector(".border-amber-500\\/30"),
    ).not.toBeNull();
  });

  it("renders the red 'Task Failed' chip when successState is 'failure'", () => {
    const { container } = render(
      <ActivityStream
        entries={[
          completeEntry("c-4", {
            text: "Could not satisfy the verification step.",
            successState: "failure",
          }),
        ]}
        showReasoning
        showToolCalls
        streaming={false}
        tailLimit={1000}
      />,
    );
    expect(screen.getByText("Task Failed")).toBeInTheDocument();
    expect(
      container.querySelector(".border-rose-500\\/30"),
    ).not.toBeNull();
  });

  it("renders a user chat_bubble with its text", () => {
    render(
      <ActivityStream
        entries={[userBubble("u-1", "hello world")]}
        showReasoning
        showToolCalls
        streaming={false}
        tailLimit={1000}
      />,
    );
    const txt = screen.getByText("hello world");
    expect(txt).toBeInTheDocument();
    // User bubbles include a "You" label in the meta row.
    expect(screen.getByText("You")).toBeInTheDocument();
  });

  it("renders a Hivemind merge bubble using the scoring pill wrapper", () => {
    const merge = asstBubble("a-1", "verdicts go here", {
      reviewKind: {
        phase: "merge",
        round: 1,
        reviewId: "rev-x",
        sessionId: "sess-x",
      },
    });
    render(
      <ActivityStream
        entries={[merge]}
        showReasoning
        showToolCalls
        streaming={false}
        tailLimit={1000}
      />,
    );
    // Honey-tinted Hivemind merge wrapper.
    expect(screen.getByText(/Hivemind merge/)).toBeInTheDocument();
    // MergeScoringPill renders a "Models scored" / "Scoring models…" label.
    expect(screen.getByText("Models scored")).toBeInTheDocument();
  });

  it("merge bubble shows streaming body + tool calls alongside MergeScoringPill", () => {
    // Mirrors the new structured merge stream: the bubble carries
    // both rendered text (markdown body) and a tool call card (e.g.
    // submit_plan) so the user can read the merge orchestrator's
    // reasoning + plan as it streams while the "Scoring models…"
    // pill sits above.
    const merge = asstBubble("a-merge", "streaming merge body content", {
      reviewKind: {
        phase: "merge",
        round: 1,
        reviewId: "rev-stream",
        sessionId: "sess-stream",
      },
      tools: [
        { tool_call_id: "call-1", name: "submit_plan", output: "", done: false },
      ],
    });
    render(
      <ActivityStream
        entries={[merge]}
        showReasoning
        showToolCalls
        streaming
        tailLimit={1000}
      />,
    );
    // Streaming "Scoring models…" pill while merge runs.
    expect(screen.getByText("Scoring models…")).toBeInTheDocument();
    // Bubble body renders the streaming markdown alongside the pill.
    expect(screen.getByText(/streaming merge body content/)).toBeInTheDocument();
    // Tool call card for submit_plan renders.
    expect(screen.getByText("submit_plan")).toBeInTheDocument();
    // Hivemind merge honey wrapper still wraps the whole thing.
    expect(screen.getByText(/Hivemind merge/)).toBeInTheDocument();
  });

  it("collapses to a 'show all' button when entry count exceeds tailLimit", async () => {
    const user = userEvent.setup();
    const entries: StreamEntry[] = Array.from({ length: 1500 }, (_, i) =>
      userBubble(`u-${i}`, `msg-${i}`),
    );
    render(
      <ActivityStream
        entries={entries}
        showReasoning
        showToolCalls
        streaming={false}
        tailLimit={1000}
      />,
    );
    // 1500 - 1000 = 500 hidden.
    const reveal = await screen.findByRole("button", {
      name: /500 earlier events.*show all/i,
    });
    expect(reveal).toBeInTheDocument();
    expect(screen.queryByText("msg-0")).not.toBeInTheDocument();
    expect(screen.getByText("msg-1499")).toBeInTheDocument();
    await act(async () => {
      await user.click(reveal);
    });
    expect(screen.getByText("msg-0")).toBeInTheDocument();
  });

  it("renders the pretty agent pill for a Tasks-surface planning session_marker", () => {
    const marker: SessionMarkerEntry = {
      kind: "session_marker",
      surface: "task",
      phase: "start",
      id: "sm-1",
      label: "Planning session started",
      agent: "planning",
      sessionId: "sess-plan-1",
    };
    const { container } = render(
      <ActivityStream
        entries={[marker]}
        showReasoning
        showToolCalls
        streaming
        tailLimit={1000}
      />,
    );
    // Pill text "planning" (uppercased via CSS).
    expect(screen.getByText("planning")).toBeInTheDocument();
    // The pretty-pill branch styles the chip with the blue tone classes.
    expect(container.querySelector(".border-blue-500\\/30.bg-blue-500\\/10")).not.toBeNull();
  });

  it("renders the usage row underneath the pretty pill when entry.usage is set", () => {
    const marker: SessionMarkerEntry = {
      kind: "session_marker",
      surface: "task",
      phase: "end",
      id: "sm-2",
      label: "",
      agent: "planning",
      sessionId: "sess-plan-2",
      success: true,
      usage: {
        input: 1234,
        output: 567,
        contextPercent: 12,
        cost: 0.0234,
        tokPerSec: 42,
      },
    };
    render(
      <ActivityStream
        entries={[marker]}
        showReasoning
        showToolCalls
        streaming={false}
        tailLimit={1000}
      />,
    );
    expect(screen.getByText(/Context 12%/)).toBeInTheDocument();
    expect(screen.getByText(/42\s+t\/s/)).toBeInTheDocument();
    expect(screen.getByText(/\$0\.0234/)).toBeInTheDocument();
  });

  it("renders a contextual sub-label inside the pill when entry.label differs from the default", () => {
    const marker: SessionMarkerEntry = {
      kind: "session_marker",
      surface: "task",
      phase: "start",
      id: "sm-3",
      label: "Hivemind review resumed — final plan",
      agent: "hivemind-merge",
      sessionId: "sess-hm",
    };
    render(
      <ActivityStream
        entries={[marker]}
        showReasoning
        showToolCalls
        streaming
        tailLimit={1000}
      />,
    );
    // Default label renders.
    expect(screen.getByText("hivemind merge")).toBeInTheDocument();
    // The differing entry.label is shown as a dim sub-label.
    expect(screen.getByText(/Hivemind review resumed/)).toBeInTheDocument();
  });

  it("scrolls to bottom when conversationKey changes (opt-in)", async () => {
    const initial: StreamEntry[] = [userBubble("u-a", "task a message")];
    const { rerender } = render(
      <ActivityStream
        entries={initial}
        showReasoning
        showToolCalls
        streaming={false}
        tailLimit={1000}
        conversationKey="task-a"
      />,
    );
    const scrollEl = screen.getByTestId(
      "activity-stream-scroll",
    ) as HTMLDivElement;
    // jsdom doesn't lay out — stub the geometry the component reads.
    Object.defineProperty(scrollEl, "scrollHeight", {
      configurable: true,
      value: 5000,
    });
    Object.defineProperty(scrollEl, "clientHeight", {
      configurable: true,
      value: 800,
    });
    scrollEl.scrollTop = 0;
    // Switch conversation context.
    const next: StreamEntry[] = [userBubble("u-b", "task b message")];
    rerender(
      <ActivityStream
        entries={next}
        showReasoning
        showToolCalls
        streaming={false}
        tailLimit={1000}
        conversationKey="task-b"
      />,
    );
    await act(async () => {
      await new Promise<void>((r) => requestAnimationFrame(() => r()));
    });
    expect(scrollEl.scrollTop).toBe(scrollEl.scrollHeight);
  });

  it("does NOT force-scroll when conversationKey is omitted (SwarmControl opt-out)", async () => {
    const initial: StreamEntry[] = [userBubble("u-a", "swarm a message")];
    const { rerender } = render(
      <ActivityStream
        entries={initial}
        showReasoning
        showToolCalls
        streaming={false}
        tailLimit={1000}
      />,
    );
    const scrollEl = screen.getByTestId(
      "activity-stream-scroll",
    ) as HTMLDivElement;
    Object.defineProperty(scrollEl, "scrollHeight", {
      configurable: true,
      value: 5000,
    });
    Object.defineProperty(scrollEl, "clientHeight", {
      configurable: true,
      value: 800,
    });
    // Simulate the user having scrolled up in the previous "conversation".
    // First arm follow at the bottom so the direction-aware handleScroll has
    // a high prior scrollTop, then drop to 100 to register an upward gesture.
    scrollEl.scrollTop = 4200;
    scrollEl.dispatchEvent(new Event("scroll"));
    scrollEl.scrollTop = 100;
    // Mark not-at-bottom so the entries-driven autoscroll won't re-pin either.
    scrollEl.dispatchEvent(new Event("scroll"));
    const next: StreamEntry[] = [userBubble("u-b", "swarm b message")];
    rerender(
      <ActivityStream
        entries={next}
        showReasoning
        showToolCalls
        streaming={false}
        tailLimit={1000}
      />,
    );
    await act(async () => {
      await new Promise<void>((r) => requestAnimationFrame(() => r()));
    });
    // Scroll position should be untouched because there is no conversationKey.
    expect(scrollEl.scrollTop).toBe(100);
  });

  it("re-pins to bottom on ResizeObserver fire when at bottom", async () => {
    // Install a controllable ResizeObserver double so we can trigger the
    // callback synchronously. Records all observed elements and exposes a
    // `trigger()` helper that fires the callback once with all current
    // entries (geometry doesn't matter — the component only checks
    // `isAtBottomRef`).
    const observers: Array<{ cb: ResizeObserverCallback; targets: Element[] }> = [];
    const prevRO = (globalThis as any).ResizeObserver;
    (globalThis as any).ResizeObserver = class {
      private cb: ResizeObserverCallback;
      private targets: Element[] = [];
      constructor(cb: ResizeObserverCallback) {
        this.cb = cb;
        observers.push({ cb, targets: this.targets });
      }
      observe(el: Element) {
        this.targets.push(el);
      }
      unobserve(el: Element) {
        this.targets = this.targets.filter((t) => t !== el);
      }
      disconnect() {
        this.targets = [];
      }
    };
    const trigger = () => {
      for (const o of observers) {
        o.cb(
          o.targets.map((t) => ({
            target: t,
            contentRect: { width: 0, height: 0, top: 0, left: 0, right: 0, bottom: 0, x: 0, y: 0, toJSON: () => ({}) },
            borderBoxSize: [],
            contentBoxSize: [],
            devicePixelContentBoxSize: [],
          })) as unknown as ResizeObserverEntry[],
          {} as ResizeObserver,
        );
      }
    };

    try {
      render(
        <ActivityStream
          entries={[asstBubble("a-1", "hello")]}
          showReasoning
          showToolCalls
          streaming={false}
          tailLimit={1000}
        />,
      );
      const scrollEl = screen.getByTestId(
        "activity-stream-scroll",
      ) as HTMLDivElement;
      // Stub geometry so handleScroll's < 80 threshold sees "at bottom".
      Object.defineProperty(scrollEl, "scrollHeight", {
        configurable: true,
        value: 5000,
      });
      Object.defineProperty(scrollEl, "clientHeight", {
        configurable: true,
        value: 800,
      });
      scrollEl.scrollTop = 4200; // 5000 - 800 = 4200 max; max - scrollTop = 0 < 80
      scrollEl.dispatchEvent(new Event("scroll"));

      // Simulate a sibling dock (QuestionsDock / HivemindReviewLivePanel)
      // mounting and shrinking the scroll viewport — handleScroll is NOT
      // re-invoked because scrollTop didn't move, so isAtBottomRef stays true.
      Object.defineProperty(scrollEl, "clientHeight", {
        configurable: true,
        value: 500,
      });
      // Reset scrollTop to 0 to prove the observer effect did the re-pin.
      scrollEl.scrollTop = 0;

      // Fire the observer; rAF runs the pin.
      expect(observers.length).toBeGreaterThan(0);
      trigger();
      await act(async () => {
        await new Promise<void>((r) => requestAnimationFrame(() => r()));
      });
      expect(scrollEl.scrollTop).toBe(scrollEl.scrollHeight);
    } finally {
      (globalThis as any).ResizeObserver = prevRO;
    }
  });

  it("does NOT re-pin on ResizeObserver fire when user scrolled up", async () => {
    const observers: Array<{ cb: ResizeObserverCallback; targets: Element[] }> = [];
    const prevRO = (globalThis as any).ResizeObserver;
    (globalThis as any).ResizeObserver = class {
      private cb: ResizeObserverCallback;
      private targets: Element[] = [];
      constructor(cb: ResizeObserverCallback) {
        this.cb = cb;
        observers.push({ cb, targets: this.targets });
      }
      observe(el: Element) {
        this.targets.push(el);
      }
      unobserve(el: Element) {
        this.targets = this.targets.filter((t) => t !== el);
      }
      disconnect() {
        this.targets = [];
      }
    };
    const trigger = () => {
      for (const o of observers) {
        o.cb(
          o.targets.map((t) => ({
            target: t,
            contentRect: { width: 0, height: 0, top: 0, left: 0, right: 0, bottom: 0, x: 0, y: 0, toJSON: () => ({}) },
            borderBoxSize: [],
            contentBoxSize: [],
            devicePixelContentBoxSize: [],
          })) as unknown as ResizeObserverEntry[],
          {} as ResizeObserver,
        );
      }
    };

    try {
      render(
        <ActivityStream
          entries={[asstBubble("a-1", "hello")]}
          showReasoning
          showToolCalls
          streaming={false}
          tailLimit={1000}
        />,
      );
      const scrollEl = screen.getByTestId(
        "activity-stream-scroll",
      ) as HTMLDivElement;
      Object.defineProperty(scrollEl, "scrollHeight", {
        configurable: true,
        value: 5000,
      });
      Object.defineProperty(scrollEl, "clientHeight", {
        configurable: true,
        value: 800,
      });
      // First arm follow at the bottom so the direction-aware handleScroll
      // has a high prior scrollTop to compare against. Without this prior,
      // the upward gesture below wouldn't register as "user scrolled up"
      // (the ref starts at 0).
      scrollEl.scrollTop = 4200; // 5000 - 800 = 4200 max → atBottom = true
      scrollEl.dispatchEvent(new Event("scroll"));
      // Now simulate the user wheeling up — scrollTop decreased from 4200 to 100.
      scrollEl.scrollTop = 100;
      scrollEl.dispatchEvent(new Event("scroll"));

      trigger();
      await act(async () => {
        await new Promise<void>((r) => requestAnimationFrame(() => r()));
      });
      // The observer's pin must respect isAtBottomRef === false.
      expect(scrollEl.scrollTop).toBe(100);
    } finally {
      (globalThis as any).ResizeObserver = prevRO;
    }
  });

  it("keeps follow state when scrollHeight grows mid-stream without user scroll", async () => {
    // Regression: when reasoning and a chat message stream simultaneously
    // the scroll container's scrollHeight jumps in a single tick. A scroll
    // event fired against this newly-grown layout (same scrollTop, larger
    // scrollHeight) used to flip isAtBottomRef to false because
    // `max - scrollTop` exceeded the 80px threshold. The direction-aware
    // handleScroll must treat layout growth as a non-event and leave
    // follow engaged so pinToBottom's rAF can catch up.
    const observers: Array<{ cb: ResizeObserverCallback; targets: Element[] }> = [];
    const prevRO = (globalThis as any).ResizeObserver;
    (globalThis as any).ResizeObserver = class {
      private cb: ResizeObserverCallback;
      private targets: Element[] = [];
      constructor(cb: ResizeObserverCallback) {
        this.cb = cb;
        observers.push({ cb, targets: this.targets });
      }
      observe(el: Element) {
        this.targets.push(el);
      }
      unobserve(el: Element) {
        this.targets = this.targets.filter((t) => t !== el);
      }
      disconnect() {
        this.targets = [];
      }
    };
    const trigger = () => {
      for (const o of observers) {
        o.cb(
          o.targets.map((t) => ({
            target: t,
            contentRect: { width: 0, height: 0, top: 0, left: 0, right: 0, bottom: 0, x: 0, y: 0, toJSON: () => ({}) },
            borderBoxSize: [],
            contentBoxSize: [],
            devicePixelContentBoxSize: [],
          })) as unknown as ResizeObserverEntry[],
          {} as ResizeObserver,
        );
      }
    };

    try {
      render(
        <ActivityStream
          entries={[asstBubble("a-1", "hello")]}
          showReasoning
          showToolCalls
          streaming
          tailLimit={1000}
        />,
      );
      const scrollEl = screen.getByTestId(
        "activity-stream-scroll",
      ) as HTMLDivElement;
      // Initial geometry: user is at the bottom.
      Object.defineProperty(scrollEl, "scrollHeight", {
        configurable: true,
        value: 2000,
      });
      Object.defineProperty(scrollEl, "clientHeight", {
        configurable: true,
        value: 800,
      });
      scrollEl.scrollTop = 1200; // max = 1200; atBottom = true
      scrollEl.dispatchEvent(new Event("scroll"));

      // Reasoning + chat message arrive in the same tick — scrollHeight
      // jumps from 2000 → 4000 while scrollTop stays at 1200. The browser
      // fires a scroll event because the relative scroll position changed
      // (now far from the new bottom), but scrollTop itself did not move.
      Object.defineProperty(scrollEl, "scrollHeight", {
        configurable: true,
        value: 4000,
      });
      // scrollTop unchanged at 1200.
      scrollEl.dispatchEvent(new Event("scroll"));

      // Drop scrollTop to 0 to prove the next pin actually runs (i.e. that
      // follow was preserved through the growth-only scroll event).
      scrollEl.scrollTop = 0;

      // Fire the ResizeObserver; rAF runs the pin.
      expect(observers.length).toBeGreaterThan(0);
      trigger();
      await act(async () => {
        await new Promise<void>((r) => requestAnimationFrame(() => r()));
      });
      // Pin succeeded → scrollTop snapped to the (new) scrollHeight.
      expect(scrollEl.scrollTop).toBe(scrollEl.scrollHeight);
    } finally {
      (globalThis as any).ResizeObserver = prevRO;
    }
  });

  it("renders the 'Creating Plan…' spinner when delimiterLoading='plan'", () => {
    render(
      <ActivityStream
        entries={[asstBubble("a-1", "")]}
        showReasoning
        showToolCalls
        streaming
        tailLimit={1000}
        delimiterLoading="plan"
      />,
    );
    expect(screen.getByText(/Creating Plan…/)).toBeInTheDocument();
  });
});
