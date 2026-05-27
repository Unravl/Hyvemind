import React from "react";
import { describe, it, expect, vi } from "vitest";
import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { QuestionsDock } from "../QuestionsDock";
import type { TaskQuestion } from "../../lib/questions";

function choiceQ(id: string, title: string, options: { id: string; label: string }[]): TaskQuestion {
  return { id, kind: "choice", title, options };
}

function textQ(id: string, title: string, placeholder?: string): TaskQuestion {
  return { id, kind: "text", title, placeholder };
}

describe("QuestionsDock", () => {
  it("renders nothing when questions is empty", () => {
    const { container } = render(
      <QuestionsDock questions={[]} onSubmit={vi.fn()} />,
    );
    expect(container.firstChild).toBeNull();
  });

  it("renders nothing when questions is null-ish", () => {
    const { container } = render(
      // @ts-expect-error — runtime null guard sanity check.
      <QuestionsDock questions={null} onSubmit={vi.fn()} />,
    );
    expect(container.firstChild).toBeNull();
  });

  it("renders the first question and a '1 / N' progress chip", () => {
    const questions: TaskQuestion[] = [
      choiceQ("scope", "Pick a scope", [
        { id: "small", label: "Small change" },
        { id: "big", label: "Big rewrite" },
      ]),
      choiceQ("speed", "How fast?", [
        { id: "now", label: "Right now" },
        { id: "later", label: "Whenever" },
      ]),
    ];
    render(<QuestionsDock questions={questions} onSubmit={vi.fn()} />);
    expect(screen.getByText("Pick a scope")).toBeInTheDocument();
    expect(screen.getByText("1 / 2")).toBeInTheDocument();
    // The dock section has the documented aria-label.
    expect(
      screen.getByLabelText("Pending question prompt"),
    ).toBeInTheDocument();
  });

  it("clicking a choice option advances the index ('2 / N')", async () => {
    const user = userEvent.setup();
    const questions: TaskQuestion[] = [
      choiceQ("scope", "Pick a scope", [
        { id: "small", label: "Small change" },
        { id: "big", label: "Big rewrite" },
      ]),
      choiceQ("speed", "How fast?", [
        { id: "now", label: "Right now" },
        { id: "later", label: "Whenever" },
      ]),
    ];
    render(<QuestionsDock questions={questions} onSubmit={vi.fn()} />);
    await user.click(screen.getByText("Small change"));
    expect(screen.getByText("How fast?")).toBeInTheDocument();
    expect(screen.getByText("2 / 2")).toBeInTheDocument();
  });

  it("fires onSubmit with [{id, answer}] after the last choice click", async () => {
    const user = userEvent.setup();
    const onSubmit = vi.fn();
    const questions: TaskQuestion[] = [
      choiceQ("scope", "Pick a scope", [
        { id: "small", label: "Small change" },
        { id: "big", label: "Big rewrite" },
      ]),
    ];
    render(<QuestionsDock questions={questions} onSubmit={onSubmit} />);
    await user.click(screen.getByText("Small change"));
    expect(onSubmit).toHaveBeenCalledTimes(1);
    expect(onSubmit.mock.calls[0][0]).toEqual([
      { id: "scope", answer: "small" },
    ]);
  });

  it("text question Enter on empty input is a no-op", async () => {
    const user = userEvent.setup();
    const onSubmit = vi.fn();
    const questions: TaskQuestion[] = [
      textQ("note", "Anything else?", "Type your answer…"),
    ];
    render(<QuestionsDock questions={questions} onSubmit={onSubmit} />);
    const input = screen.getByPlaceholderText("Type your answer…");
    input.focus();
    // Press Enter on empty input — should be a no-op (no submit, no advance).
    await user.keyboard("{Enter}");
    expect(onSubmit).not.toHaveBeenCalled();
    // Still on the same question.
    expect(screen.getByText("Anything else?")).toBeInTheDocument();
    expect(screen.getByText("1 / 1")).toBeInTheDocument();
  });

  it("resets idx / answers / custom when the question-set identity changes", async () => {
    const user = userEvent.setup();
    const onSubmit = vi.fn();
    const initial: TaskQuestion[] = [
      choiceQ("scope", "Pick a scope", [
        { id: "small", label: "Small change" },
        { id: "big", label: "Big rewrite" },
      ]),
      choiceQ("speed", "How fast?", [
        { id: "now", label: "Right now" },
        { id: "later", label: "Whenever" },
      ]),
    ];
    const { rerender } = render(
      <QuestionsDock questions={initial} onSubmit={onSubmit} />,
    );
    // Advance to Q2.
    await user.click(screen.getByText("Small change"));
    expect(screen.getByText("How fast?")).toBeInTheDocument();
    expect(screen.getByText("2 / 2")).toBeInTheDocument();
    // New batch arrives with different ids → reset.
    const next: TaskQuestion[] = [
      choiceQ("style", "Pick a style", [
        { id: "minimal", label: "Minimal" },
        { id: "lush", label: "Lush" },
      ]),
    ];
    rerender(<QuestionsDock questions={next} onSubmit={onSubmit} />);
    expect(screen.getByText("Pick a style")).toBeInTheDocument();
    expect(screen.getByText("1 / 1")).toBeInTheDocument();
    expect(screen.queryByText("How fast?")).not.toBeInTheDocument();
    expect(screen.queryByText("Pick a scope")).not.toBeInTheDocument();
  });

  it("hides dock immediately after submitting the final answer", async () => {
    const user = userEvent.setup();
    const onSubmit = vi.fn();
    const questions: TaskQuestion[] = [
      choiceQ("scope", "Pick a scope", [
        { id: "small", label: "Small change" },
        { id: "big", label: "Big rewrite" },
      ]),
    ];
    const { container } = render(
      <QuestionsDock questions={questions} onSubmit={onSubmit} />,
    );
    await user.click(screen.getByText("Small change"));
    // First click fires submit exactly once.
    expect(onSubmit).toHaveBeenCalledTimes(1);
    // The dock hides immediately — no DOM remains for a second click.
    expect(container.firstChild).toBeNull();
    // NOTE: The ref-based double-submit guard (submittedRef.current in
    // commit) still exists in source but can no longer be exercised via
    // DOM interaction because the second click targets a detached element.
    // The guard remains as defence-in-depth alongside the early return.
  });

  it("renders nothing after submit (terminal state hidden)", async () => {
    const user = userEvent.setup();
    const questions: TaskQuestion[] = [
      choiceQ("scope", "Pick a scope", [
        { id: "small", label: "Small change" },
        { id: "big", label: "Big rewrite" },
      ]),
    ];
    const { container } = render(
      <QuestionsDock questions={questions} onSubmit={vi.fn()} />,
    );
    await user.click(screen.getByText("Small change"));
    // After submit the dock hides entirely — no terminal bar visible.
    expect(container.firstChild).toBeNull();
    expect(screen.queryByText(/All set/)).not.toBeInTheDocument();
    expect(screen.queryByText("Pick a scope")).not.toBeInTheDocument();
    // Assert the section wrapper is also gone (catches false-negative
    // where an empty shell element remains in the DOM).
    expect(
      screen.queryByLabelText("Pending question prompt"),
    ).not.toBeInTheDocument();
  });

  it("header exposes Previous / Next buttons with correct disabled states", async () => {
    const user = userEvent.setup();
    const questions: TaskQuestion[] = [
      choiceQ("scope", "Pick a scope", [
        { id: "small", label: "Small change" },
        { id: "big", label: "Big rewrite" },
      ]),
      choiceQ("speed", "How fast?", [
        { id: "now", label: "Right now" },
        { id: "later", label: "Whenever" },
      ]),
    ];
    render(<QuestionsDock questions={questions} onSubmit={vi.fn()} />);
    const prev = screen.getByLabelText("Previous question");
    const next = screen.getByLabelText("Next question");
    // First question: Prev disabled, Next disabled (no answer staged).
    expect(prev).toBeDisabled();
    expect(next).toBeDisabled();
    // Stage an answer.
    await user.click(screen.getByText("Small change"));
    // Now on Q2: Prev should be enabled, Next still disabled (Q2 not answered).
    const prev2 = screen.getByLabelText("Previous question");
    const next2 = screen.getByLabelText("Next question");
    expect(prev2).not.toBeDisabled();
    expect(next2).toBeDisabled();
  });

  it("clicking Prev returns to the previous question with the prior choice still highlighted", async () => {
    const user = userEvent.setup();
    const questions: TaskQuestion[] = [
      choiceQ("scope", "Pick a scope", [
        { id: "small", label: "Small change" },
        { id: "big", label: "Big rewrite" },
      ]),
      choiceQ("speed", "How fast?", [
        { id: "now", label: "Right now" },
        { id: "later", label: "Whenever" },
      ]),
    ];
    render(<QuestionsDock questions={questions} onSubmit={vi.fn()} />);
    await user.click(screen.getByText("Small change"));
    expect(screen.getByText("How fast?")).toBeInTheDocument();
    // Navigate back.
    await user.click(screen.getByLabelText("Previous question"));
    expect(screen.getByText("Pick a scope")).toBeInTheDocument();
    expect(screen.getByText("1 / 2")).toBeInTheDocument();
    // The previously-selected option carries the highlight class
    // and aria-pressed.
    const selectedBtn = screen.getByText("Small change").closest("button");
    expect(selectedBtn).not.toBeNull();
    expect(selectedBtn).toHaveClass("border-honey-500/60");
    expect(selectedBtn).toHaveAttribute("aria-pressed", "true");
    // The unselected option should not be highlighted.
    const otherBtn = screen.getByText("Big rewrite").closest("button");
    expect(otherBtn).toHaveAttribute("aria-pressed", "false");
  });

  it("choice question shows a freeform input below the options", () => {
    const questions: TaskQuestion[] = [
      choiceQ("scope", "Pick a scope", [
        { id: "small", label: "Small change" },
        { id: "big", label: "Big rewrite" },
      ]),
    ];
    render(<QuestionsDock questions={questions} onSubmit={vi.fn()} />);
    // The placeholder defaults to "Type your answer…" when none is given.
    expect(screen.getByPlaceholderText("Type your answer…")).toBeInTheDocument();
    // Hint label for choice questions.
    expect(screen.getByText("Or type your own answer")).toBeInTheDocument();
    // aria-label on the custom input.
    expect(screen.getByLabelText("Custom answer")).toBeInTheDocument();
  });

  it("freeform override on a choice question is sent as the typed string", async () => {
    const user = userEvent.setup();
    const onSubmit = vi.fn();
    const questions: TaskQuestion[] = [
      choiceQ("scope", "Pick a scope", [
        { id: "small", label: "Small change" },
        { id: "big", label: "Big rewrite" },
      ]),
    ];
    render(<QuestionsDock questions={questions} onSubmit={onSubmit} />);
    const input = screen.getByLabelText("Custom answer");
    await user.type(input, "something completely different");
    await user.keyboard("{Enter}");
    expect(onSubmit).toHaveBeenCalledTimes(1);
    expect(onSubmit.mock.calls[0][0]).toEqual([
      { id: "scope", answer: "something completely different" },
    ]);
  });

  it("Next on the last question fires onSubmit", async () => {
    const user = userEvent.setup();
    const onSubmit = vi.fn();
    const questions: TaskQuestion[] = [
      choiceQ("scope", "Pick a scope", [
        { id: "small", label: "Small change" },
        { id: "big", label: "Big rewrite" },
      ]),
      choiceQ("speed", "How fast?", [
        { id: "now", label: "Right now" },
        { id: "later", label: "Whenever" },
      ]),
    ];
    render(<QuestionsDock questions={questions} onSubmit={onSubmit} />);
    // Click-to-advance through Q1 (stages Q1's answer and lands on Q2).
    await user.click(screen.getByText("Small change"));
    expect(screen.getByText("How fast?")).toBeInTheDocument();
    // Click on Q2 option fires submit via the fast path.
    await user.click(screen.getByText("Right now"));
    expect(onSubmit).toHaveBeenCalledTimes(1);
    expect(onSubmit.mock.calls[0][0]).toEqual([
      { id: "scope", answer: "small" },
      { id: "speed", answer: "now" },
    ]);
  });

  it("Next on the last question (pure-nav path) submits using staged answers", async () => {
    const user = userEvent.setup();
    const onSubmit = vi.fn();
    const questions: TaskQuestion[] = [
      choiceQ("scope", "Pick a scope", [
        { id: "small", label: "Small change" },
        { id: "big", label: "Big rewrite" },
      ]),
      choiceQ("speed", "How fast?", [
        { id: "now", label: "Right now" },
        { id: "later", label: "Whenever" },
      ]),
    ];
    render(<QuestionsDock questions={questions} onSubmit={onSubmit} />);
    // Stage Q1.
    await user.click(screen.getByText("Small change"));
    // On Q2: stage answer via option click → onSubmit fires (fast path).
    // To exercise the pure-nav path, we need to be on the last question
    // with a staged answer but advance via Next, not option click.
    // Achieve this by clicking option on Q2 (auto-submits), then re-render
    // is not possible without reset. Instead, validate via custom-input
    // priority: type into the input on Q2 and press Next.
    const customInput = screen.getByLabelText("Custom answer");
    await user.type(customInput, "as soon as possible");
    await user.click(screen.getByLabelText("Next question"));
    expect(onSubmit).toHaveBeenCalledTimes(1);
    expect(onSubmit.mock.calls[0][0]).toEqual([
      { id: "scope", answer: "small" },
      { id: "speed", answer: "as soon as possible" },
    ]);
  });
});
