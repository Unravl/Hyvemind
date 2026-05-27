import React, { useCallback, useEffect, useMemo, useState } from "react";
import { FocusTrap } from "focus-trap-react";
import { I } from "./icons";
import { Btn } from "./atoms";
import {
  SWARM_QUESTION_OTHER_PREFIX,
  SWARM_QUESTION_SKIPPED_VALUE,
  type SwarmQuestion,
} from "../lib/plan-mode";

/** Sentinel value used by the modal's internal state to represent the
 *  "Other (free text)" option. Kept private — when the modal builds the
 *  outgoing answer payload it materialises the literal `Other: <text>`
 *  string via `SWARM_QUESTION_OTHER_PREFIX`. */
const OTHER_VALUE = "__other__";

/** Result emitted to `onSubmit` — preserves question order so the consuming
 *  taskRuntime can stamp answers into the conversation in the same order the
 *  Queen-planning agent asked them. Each entry is the literal value that
 *  will end up in the `[Answers] {…}` payload:
 *    - For a chosen option: the option's `value` string.
 *    - For "Other": `Other: <free text>` (the prefix is part of the
 *      Queen-planning contract).
 *    - When the user skips: `skipped`. */
export interface SwarmQuestionAnswer {
  id: string;
  value: string;
}

interface SwarmQuestionModalProps {
  questions: SwarmQuestion[];
  /** Fired when the user submits answers to every question. */
  onSubmit: (answers: SwarmQuestionAnswer[]) => void;
  /** Fired when the user explicitly skips. Each question's answer value is
   *  set to the literal `skipped` sentinel — the Queen-planning prompt
   *  recognises this and can choose to proceed without the answer or
   *  re-ask. The modal cannot be dismissed by clicking the backdrop; this
   *  is the only escape hatch. */
  onSkip: () => void;
}

interface AnswerState {
  /** Selected option value, the OTHER_VALUE sentinel, or undefined when
   *  the question hasn't been answered yet. */
  choice?: string;
  /** Free-text body when `choice === OTHER_VALUE`. Trimmed lazily on submit. */
  otherText: string;
}

/** Render a blocking modal showing every question from the Queen-planning
 *  agent's latest swarm-question batch. The user picks one option per
 *  question (with an auto-appended "Other (free text)" escape hatch), then
 *  clicks Submit to send the answers back as the next user message.
 *
 *  Visual style mirrors `Modal` / `ErrorModal`: dark ink panel, honey accent,
 *  backdrop-blur. Backdrop click does NOT dismiss — the conversation is
 *  blocked on these answers; the only ways out are Submit (when every
 *  question has an answer) and Skip (which submits `skipped` for every id).
 *
 *  Accessibility:
 *    - The outer container has `role="dialog"` + `aria-modal="true"`.
 *    - The `<form>` has `aria-required="true"` and `noValidate` so the
 *      Submit button's own disabled state drives validation rather than the
 *      browser's native validity tooltips.
 *    - Each question is wrapped in a `<fieldset>` with a `<legend>` —
 *      screen readers announce "question N of M: <text>" before reading
 *      options.
 *    - The native radio inputs are visually hidden but remain focusable
 *      and labelled. */
export function SwarmQuestionModal({ questions, onSubmit, onSkip }: SwarmQuestionModalProps) {
  const [answers, setAnswers] = useState<Record<string, AnswerState>>({});

  // Reset internal state when the question set changes (the Queen may emit a
  // new batch in the same task — we don't want stale answers from the prior
  // batch to leak through).
  const questionsKey = useMemo(() => questions.map((q) => q.id).join("|"), [questions]);
  useEffect(() => {
    setAnswers({});
  }, [questionsKey]);

  const setChoice = useCallback((qid: string, choice: string) => {
    setAnswers((prev) => ({
      ...prev,
      [qid]: { ...(prev[qid] ?? { otherText: "" }), choice },
    }));
  }, []);

  const setOtherText = useCallback((qid: string, otherText: string) => {
    setAnswers((prev) => ({
      ...prev,
      [qid]: { ...(prev[qid] ?? { otherText: "" }), otherText, choice: OTHER_VALUE },
    }));
  }, []);

  /** A question is "answered" when:
   *   - the user picked a concrete option (`choice` is a value from
   *     `q.options` or the OTHER_VALUE sentinel WITH a non-empty trimmed
   *     `otherText`). */
  const isAnswered = useCallback(
    (q: SwarmQuestion): boolean => {
      const a = answers[q.id];
      if (!a || !a.choice) return false;
      if (a.choice === OTHER_VALUE) return a.otherText.trim().length > 0;
      return true;
    },
    [answers],
  );

  const allAnswered = questions.every(isAnswered);

  const handleSubmit = useCallback(
    (e?: React.FormEvent<HTMLFormElement>) => {
      e?.preventDefault();
      if (!allAnswered) return;
      const out: SwarmQuestionAnswer[] = questions.map((q) => {
        const a = answers[q.id];
        // `allAnswered` guarantees `a` and `a.choice` are present.
        if (a.choice === OTHER_VALUE) {
          return { id: q.id, value: `${SWARM_QUESTION_OTHER_PREFIX}${a.otherText.trim()}` };
        }
        return { id: q.id, value: a.choice ?? "" };
      });
      onSubmit(out);
    },
    [allAnswered, answers, onSubmit, questions],
  );

  // Trap focus + block Escape from dismissing the modal — the answers are
  // required to unblock the Queen.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.preventDefault();
        e.stopPropagation();
      }
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, []);

  if (questions.length === 0) return null;

  return (
    <FocusTrap
      focusTrapOptions={{
        // Escape is intentionally blocked by the keydown listener above to
        // keep the user on the questions — disable the trap's own
        // Esc-deactivation so the two paths agree.
        escapeDeactivates: false,
        // Backdrop click is a no-op for this modal (planning is blocked on
        // the answers). Disabling click-outside-deactivates makes that
        // explicit at the trap level too.
        clickOutsideDeactivates: false,
        allowOutsideClick: false,
        // First focusable is the Skip link in the footer, but conceptually
        // the user should be reading the questions first. Fall back to the
        // dialog container if no focusable child exists yet.
        fallbackFocus: "[data-swarm-question-modal]",
        returnFocusOnDeactivate: true,
        // jsdom display-check workaround; see Modal in atoms.tsx.
        tabbableOptions: { displayCheck: "none" },
      }}
    >
      <div
        data-modal
        data-swarm-question-modal
        role="dialog"
        aria-modal="true"
        aria-labelledby="swarm-question-modal-title"
        tabIndex={-1}
        className="fixed inset-0 z-[90] flex items-center justify-center"
      >
      {/* Backdrop — click is a no-op so the user can't accidentally dismiss
          the modal mid-planning. */}
      <div className="absolute inset-0 bg-black/60 backdrop-blur-sm" aria-hidden="true" />
      <div className="relative w-[560px] max-w-[92vw] max-h-[85vh] bg-ink-800 border border-honey-500/30 rounded-2xl shadow-2xl flex flex-col overflow-hidden">
        {/* Header */}
        <div className="flex items-center gap-3 px-5 py-4 border-b border-line shrink-0">
          <div className="w-8 h-8 rounded-full bg-honey-500/15 border border-honey-500/30 flex items-center justify-center shrink-0">
            {I.chat({ size: 14, className: "text-honey-400" })}
          </div>
          <div className="flex-1 min-w-0">
            <h2 id="swarm-question-modal-title" className="text-[14px] font-semibold text-white">
              {questions.length === 1 ? "A question before I plan" : `${questions.length} questions before I plan`}
            </h2>
            <div className="text-[11.5px] text-dim mt-0.5">
              The Queen is waiting on your answers. Pick one option per question.
            </div>
          </div>
        </div>

        {/* Body — scrollable form */}
        <form
          aria-required="true"
          noValidate
          onSubmit={handleSubmit}
          className="flex-1 overflow-y-auto px-5 py-4 space-y-5"
        >
          {questions.map((q, qi) => {
            const a = answers[q.id];
            const selected = a?.choice;
            const otherSelected = selected === OTHER_VALUE;
            const optionGroupName = `swarm-q-${q.id}`;
            return (
              <fieldset key={q.id} className="space-y-2">
                <legend className="flex items-start gap-2.5 w-full">
                  <span className="font-mono text-[10.5px] text-honey-400 mt-0.5 shrink-0 bg-honey-500/10 border border-honey-500/25 rounded px-1.5 py-0.5">
                    Q{qi + 1}/{questions.length}
                  </span>
                  <span className="text-[13.5px] font-semibold text-white leading-snug">
                    {q.question}
                  </span>
                </legend>
                <div className="space-y-1.5 pl-1">
                  {q.options.map((opt) => {
                    const isPicked = selected === opt.value;
                    return (
                      <label
                        key={opt.value}
                        className={`flex items-start gap-2.5 px-3 py-2 rounded-md border cursor-pointer transition-colors ${
                          isPicked
                            ? "bg-honey-500/10 border-honey-500/50"
                            : "bg-ink-800 border-line hover:bg-honey-500/5 hover:border-honey-500/30"
                        }`}
                      >
                        <input
                          type="radio"
                          name={optionGroupName}
                          value={opt.value}
                          checked={isPicked}
                          onChange={() => setChoice(q.id, opt.value)}
                          className="sr-only"
                        />
                        <span
                          className={`mt-0.5 w-3.5 h-3.5 rounded-full border-2 flex-shrink-0 ${
                            isPicked ? "border-honey-400 bg-honey-400/40" : "border-line-strong"
                          }`}
                          aria-hidden="true"
                        />
                        <span className="flex-1 min-w-0">
                          <span className="text-[12.5px] text-white/90 font-medium">{opt.label}</span>
                          {opt.hint && (
                            <span className="block text-[11px] text-dim font-mono mt-0.5">{opt.hint}</span>
                          )}
                        </span>
                      </label>
                    );
                  })}
                  {/* Auto-appended "Other (free text)" escape hatch — the
                      Queen-planning prompt promises this. Selecting it
                      reveals the free-text input below. */}
                  <label
                    className={`flex items-start gap-2.5 px-3 py-2 rounded-md border cursor-pointer transition-colors ${
                      otherSelected
                        ? "bg-honey-500/10 border-honey-500/50"
                        : "bg-ink-800 border-line hover:bg-honey-500/5 hover:border-honey-500/30"
                    }`}
                  >
                    <input
                      type="radio"
                      name={optionGroupName}
                      value={OTHER_VALUE}
                      checked={otherSelected}
                      onChange={() => setChoice(q.id, OTHER_VALUE)}
                      className="sr-only"
                    />
                    <span
                      className={`mt-0.5 w-3.5 h-3.5 rounded-full border-2 flex-shrink-0 ${
                        otherSelected ? "border-honey-400 bg-honey-400/40" : "border-line-strong"
                      }`}
                      aria-hidden="true"
                    />
                    <span className="flex-1 min-w-0">
                      <span className="text-[12.5px] text-white/90 font-medium">Other (free text)</span>
                      <span className="block text-[11px] text-dim font-mono mt-0.5">
                        Provide your own answer.
                      </span>
                    </span>
                  </label>
                  {otherSelected && (
                    <input
                      type="text"
                      aria-label={`Free-text answer for question ${qi + 1}`}
                      value={a?.otherText ?? ""}
                      onChange={(e) => setOtherText(q.id, e.target.value)}
                      placeholder="Type your answer…"
                      className="w-full px-3 py-2 mt-1 rounded-md border border-honey-500/40 bg-ink-900 text-[12.5px] text-white/90 placeholder:text-dim focus:outline-none focus:border-honey-400 focus-visible:ring-2 focus-visible:ring-honey-500 focus-visible:ring-offset-2 focus-visible:ring-offset-ink-950"
                      autoFocus
                    />
                  )}
                </div>
              </fieldset>
            );
          })}
        </form>

        {/* Footer */}
        <div className="flex items-center gap-2 px-5 py-4 border-t border-line shrink-0">
          <button
            type="button"
            onClick={onSkip}
            className="text-[11.5px] text-dim hover:text-muted underline-offset-2 hover:underline transition-colors"
          >
            Skip these questions
          </button>
          <div className="flex-1" />
          <Btn
            kind="primary"
            onClick={() => handleSubmit()}
            disabled={!allAnswered}
            aria-disabled={!allAnswered}
            title={allAnswered ? undefined : "Answer every question to continue"}
          >
            Submit answers
          </Btn>
        </div>
      </div>
      </div>
    </FocusTrap>
  );
}

/** Helper exposed for tests + the runtime: build the canonical
 *  skip-all-questions answer payload. Each question id maps to the literal
 *  `skipped` sentinel so the outgoing `[Answers] {…}` message round-trips
 *  cleanly through `buildSwarmAnswerPrompt`. */
export function buildSkipAnswers(questions: SwarmQuestion[]): SwarmQuestionAnswer[] {
  return questions.map((q) => ({ id: q.id, value: SWARM_QUESTION_SKIPPED_VALUE }));
}
