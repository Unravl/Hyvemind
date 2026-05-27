import React from "react";
import { describe, it, expect, vi, afterEach } from "vitest";
import { render, screen, cleanup } from "@testing-library/react";
import userEvent from "@testing-library/user-event";

import { SwarmQuestionModal } from "../SwarmQuestionModal";
import type { SwarmQuestion } from "../../lib/plan-mode";

const TWO_QUESTIONS: SwarmQuestion[] = [
  {
    id: "scope-realtime",
    question: "How should this support real-time updates?",
    options: [
      { value: "yes-websocket", label: "Yes via WebSocket", hint: "Lowest latency" },
      { value: "yes-polling", label: "Yes via polling 5s" },
      { value: "no", label: "Not in scope" },
    ],
  },
  {
    id: "auth-mode",
    question: "Which auth mode should the swarm assume?",
    options: [
      { value: "anon", label: "Anonymous" },
      { value: "jwt", label: "JWT bearer" },
    ],
  },
];

const ONE_QUESTION: SwarmQuestion[] = [TWO_QUESTIONS[0]];

afterEach(() => {
  cleanup();
});

describe("SwarmQuestionModal", () => {
  it("does not render when there are no questions", () => {
    const { container } = render(
      <SwarmQuestionModal questions={[]} onSubmit={vi.fn()} onSkip={vi.fn()} />,
    );
    expect(container.firstChild).toBeNull();
  });

  it("renders one fieldset per question with every option plus 'Other'", () => {
    render(
      <SwarmQuestionModal questions={TWO_QUESTIONS} onSubmit={vi.fn()} onSkip={vi.fn()} />,
    );
    const fieldsets = document.querySelectorAll("fieldset");
    expect(fieldsets.length).toBe(2);
    // Q1: 3 listed options + auto-appended Other.
    expect(screen.getByLabelText(/Yes via WebSocket/i)).toBeInTheDocument();
    expect(screen.getByLabelText(/Yes via polling 5s/i)).toBeInTheDocument();
    expect(screen.getByLabelText(/Not in scope/i)).toBeInTheDocument();
    expect(screen.getAllByLabelText(/Other \(free text\)/i)).toHaveLength(2);
    // Hint visible
    expect(screen.getByText(/Lowest latency/i)).toBeInTheDocument();
  });

  it("Submit button is disabled until every question has an answer, then enabled", async () => {
    const user = userEvent.setup();
    const onSubmit = vi.fn();
    render(
      <SwarmQuestionModal questions={TWO_QUESTIONS} onSubmit={onSubmit} onSkip={vi.fn()} />,
    );
    const submit = screen.getByRole("button", { name: /submit answers/i });
    expect(submit).toBeDisabled();
    // Answer only Q1 — still disabled.
    await user.click(screen.getByLabelText(/Yes via WebSocket/i));
    expect(submit).toBeDisabled();
    // Answer Q2 — enabled.
    await user.click(screen.getByLabelText(/JWT bearer/i));
    expect(submit).toBeEnabled();
  });

  it("fires onSubmit with an ordered {id, value} list once every question is answered", async () => {
    const user = userEvent.setup();
    const onSubmit = vi.fn();
    render(
      <SwarmQuestionModal questions={TWO_QUESTIONS} onSubmit={onSubmit} onSkip={vi.fn()} />,
    );
    await user.click(screen.getByLabelText(/Yes via WebSocket/i));
    await user.click(screen.getByLabelText(/JWT bearer/i));
    await user.click(screen.getByRole("button", { name: /submit answers/i }));
    expect(onSubmit).toHaveBeenCalledTimes(1);
    expect(onSubmit).toHaveBeenCalledWith([
      { id: "scope-realtime", value: "yes-websocket" },
      { id: "auth-mode", value: "jwt" },
    ]);
  });

  it("reveals a free-text input when Other is selected and prefixes the answer with 'Other: '", async () => {
    const user = userEvent.setup();
    const onSubmit = vi.fn();
    render(
      <SwarmQuestionModal questions={ONE_QUESTION} onSubmit={onSubmit} onSkip={vi.fn()} />,
    );
    // Free-text input is not in the DOM until Other is selected.
    expect(screen.queryByPlaceholderText(/type your answer/i)).toBeNull();
    await user.click(screen.getByLabelText(/Other \(free text\)/i));
    const input = screen.getByPlaceholderText(/type your answer/i);
    expect(input).toBeInTheDocument();
    // Submit is still disabled with an empty Other text.
    const submit = screen.getByRole("button", { name: /submit answers/i });
    expect(submit).toBeDisabled();
    await user.type(input, "rust nightly toolchain");
    expect(submit).toBeEnabled();
    await user.click(submit);
    expect(onSubmit).toHaveBeenCalledWith([
      { id: "scope-realtime", value: "Other: rust nightly toolchain" },
    ]);
  });

  it("trims whitespace inside the Other free-text value", async () => {
    const user = userEvent.setup();
    const onSubmit = vi.fn();
    render(
      <SwarmQuestionModal questions={ONE_QUESTION} onSubmit={onSubmit} onSkip={vi.fn()} />,
    );
    await user.click(screen.getByLabelText(/Other \(free text\)/i));
    await user.type(
      screen.getByPlaceholderText(/type your answer/i),
      "   websocket only   ",
    );
    await user.click(screen.getByRole("button", { name: /submit answers/i }));
    expect(onSubmit).toHaveBeenCalledWith([
      { id: "scope-realtime", value: "Other: websocket only" },
    ]);
  });

  it("fires onSkip when the user clicks 'Skip these questions'", async () => {
    const user = userEvent.setup();
    const onSkip = vi.fn();
    render(
      <SwarmQuestionModal questions={TWO_QUESTIONS} onSubmit={vi.fn()} onSkip={onSkip} />,
    );
    await user.click(screen.getByRole("button", { name: /skip these questions/i }));
    expect(onSkip).toHaveBeenCalledTimes(1);
  });

  it("does not close on backdrop click — the planning flow is blocked on these answers", async () => {
    const user = userEvent.setup();
    const onSubmit = vi.fn();
    const onSkip = vi.fn();
    render(
      <SwarmQuestionModal questions={TWO_QUESTIONS} onSubmit={onSubmit} onSkip={onSkip} />,
    );
    // The backdrop is the first child div with the bg-black/60 class.
    const backdrop = document.querySelector("[data-swarm-question-modal] [aria-hidden='true']");
    expect(backdrop).not.toBeNull();
    await user.click(backdrop!);
    expect(onSubmit).not.toHaveBeenCalled();
    expect(onSkip).not.toHaveBeenCalled();
  });

  it("resets internal answers when the question set changes (new batch from the Queen)", async () => {
    const user = userEvent.setup();
    const onSubmit = vi.fn();
    const { rerender } = render(
      <SwarmQuestionModal questions={ONE_QUESTION} onSubmit={onSubmit} onSkip={vi.fn()} />,
    );
    await user.click(screen.getByLabelText(/Yes via WebSocket/i));
    expect(screen.getByRole("button", { name: /submit answers/i })).toBeEnabled();
    // Queen emits a fresh batch with a different id — the modal should re-enter
    // the "no answers yet" state and the submit button should be disabled again.
    rerender(
      <SwarmQuestionModal
        questions={[{ ...TWO_QUESTIONS[1] }]}
        onSubmit={onSubmit}
        onSkip={vi.fn()}
      />,
    );
    expect(screen.getByRole("button", { name: /submit answers/i })).toBeDisabled();
  });
});
