import React from "react";
import { I } from "./icons";
import type { TaskQuestion } from "../lib/questions";

export interface QuestionsDockProps {
  questions: TaskQuestion[];
  onSubmit: (answers: { id: string; answer: string }[]) => void;
  /** Optional skip affordance — currently unused, reserved for future. */
  onCancel?: () => void;
  /** Persisted question index to restore on remount. */
  initialIdx?: number;
  /** Persisted partial answers to restore on remount. */
  initialAnswers?: Record<string, string>;
  /** Fired when the user advances to a different question or provides an
   *  answer, so the parent can persist partial progress across unmounts. */
  onProgress?: (idx: number, answers: Record<string, string>) => void;
}

/**
 * Bottom-docked Q&A panel that sits between the conversation stream and the
 * chat composer. Mirrors the visual band chrome of `HivemindReviewLivePanel`
 * so the two docks belong to the same "bottom-bar stack". Renders nothing
 * when there are no active questions.
 *
 * Lifecycle:
 *   1. Parent mounts this dock when `active.pendingQuestions?.length > 0`.
 *   2. User pages through questions; on advance past the last one we
 *      fire `onSubmit`.
 *   3. The reducer clears `pendingQuestions` and the parent unmounts us.
 *
 * Navigation: header band exposes Prev / Next icon buttons so the user
 * can review and edit prior answers before the final submit. The body
 * always renders a freeform input row (even for `choice` questions) so
 * a typed override can supersede the offered options.
 *
 * Post-submit behaviour: after the final answer the dock hides immediately
 * (returns null). The actual unmount from the DOM is parent-driven when the
 * reducer clears pendingQuestions.
 */
export function QuestionsDock({
  questions,
  onSubmit,
  initialIdx,
  initialAnswers,
  onProgress,
}: QuestionsDockProps) {
  // Clamp initialIdx to valid range so corrupted persisted data or reducer
  // bugs never cause out-of-bounds access.
  const safeInitialIdx = React.useMemo(
    () => Math.min(initialIdx ?? 0, Math.max((questions?.length ?? 1) - 1, 0)),
    [initialIdx, questions?.length],
  );

  const [idx, setIdx] = React.useState(safeInitialIdx);
  const [answers, setAnswers] = React.useState<Record<string, string>>(initialAnswers ?? {});
  const [custom, setCustom] = React.useState("");
  const [submitted, setSubmitted] = React.useState(false);
  const submittedRef = React.useRef(false);

  // Track the latest onProgress callback via ref so effects never close
  // over a stale reference.
  const onProgressRef = React.useRef(onProgress);
  onProgressRef.current = onProgress;

  // Stable key derived from the current question batch's question ids.
  const questionsKey = (questions ?? []).map((q) => q.id).join("|");

  // Derive a stable dependency key from `initialAnswers` via JSON.stringify
  // to avoid firing the effect on every parent re-render (which would happen
  // if the parent passes a fresh `{}` each time).
  const [initialAnswersKey] = React.useState(() => JSON.stringify(initialAnswers ?? {}));

  // Restore persisted state from props when the questions batch changes or
  // when a new persisted state arrives from a different source. Fires when
  // questionsKey, initialIdx, or initialAnswers content changes.
  React.useEffect(() => {
    const clampedIdx = Math.min(initialIdx ?? 0, Math.max((questions?.length ?? 1) - 1, 0));
    setIdx(clampedIdx);
    setAnswers(initialAnswers ?? {});
    setCustom("");
    setSubmitted(false);
    submittedRef.current = false;
    // Notify parent of the restored position.
    onProgressRef.current?.(clampedIdx, initialAnswers ?? {});
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [questionsKey, initialIdx, initialAnswersKey]);

  // Track latest answers for use in callbacks that might close over stale closures.
  const answersRef = React.useRef(answers);
  answersRef.current = answers;

  // Restore the freeform input when the visible question changes. We
  // only seed `custom` from the previously-stored answer when that answer
  // was a *freeform override* (i.e. not one of the current question's
  // option ids). Otherwise we leave `custom` empty so the highlighted
  // option button stands alone.
  //
  // Important: depend on [idx, questionsKey] NOT [answers] — depending on
  // `answers` would clobber the input on every keystroke.
  React.useEffect(() => {
    if (!questions || questions.length === 0) return;
    if (submittedRef.current) return; // skip when dock is in terminal state
    const safeIdx = Math.min(idx, questions.length - 1);
    const cur = questions[safeIdx];
    if (!cur) return;
    const stored = answers[cur.id];
    if (stored === undefined) {
      setCustom("");
      return;
    }
    const isOptionId =
      cur.kind === "choice" &&
      !!cur.options &&
      cur.options.some((o) => o.id === stored);
    setCustom(isOptionId ? "" : stored);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [idx, questionsKey]);

  // Defensive early return — matches HivemindReviewLivePanel's pattern.
  // Placed AFTER hooks to comply with the Rules of Hooks.
  if (!questions || questions.length === 0) return null;

  const total = questions.length;
  const done = submitted || idx >= total;

  // --- Handler functions (moved before early return) --------------------
  const fireSubmit = (snapshot: Record<string, string>) => {
    submittedRef.current = true;
    setSubmitted(true);
    if (onSubmit) {
      // Preserve original-question ordering when handing answers back.
      onSubmit(
        questions
          .filter((qq) => snapshot[qq.id] !== undefined)
          .map((qq) => ({ id: qq.id, answer: snapshot[qq.id] })),
      );
    }
  };

  const commit = (qid: string, value: string) => {
    if (submittedRef.current) return; // double-submit guard
    const next = { ...answers, [qid]: value };
    setAnswers(next);
    setCustom("");
    onProgressRef.current?.(idx + 1, next);
    const nextIdx = idx + 1;
    setIdx(nextIdx);
    if (nextIdx >= total) {
      fireSubmit(next);
    }
  };

  const goPrev = () => {
    if (submittedRef.current) return;
    if (idx === 0) return;
    const newIdx = idx - 1;
    setIdx(newIdx);
    onProgressRef.current?.(newIdx, answersRef.current);
  };

  const goNext = () => {
    if (submittedRef.current) return;
    // Custom input takes priority — treat it as a fresh staged answer.
    if (hasCustom) {
      commit(q.id, trimmedCustom);
      return;
    }
    if (!hasStaged) return; // disabled state guard
    if (idx < total - 1) {
      const newIdx = idx + 1;
      setIdx(newIdx);
      onProgressRef.current?.(newIdx, answers); // persist idx after pure nav
      return;
    }
    // Pure navigation on the final question: submit using the current
    // staged answers without rewriting anything.
    fireSubmit(answers);
  };

  // --- JSX constants (unchanged, moved before early return) -------------
  const navBtnBase =
    "w-6 h-6 rounded-md flex items-center justify-center border border-transparent transition-colors";
  const navBtnEnabled = "hover:bg-ink-700 hover:border-line text-white/80";
  const navBtnDisabled = "opacity-40 cursor-not-allowed pointer-events-none";

  // --- Early return ---------------------------------------------------
  if (done) return null;

  // --- Derived state (only reached when NOT done) -----------------------
  const q: TaskQuestion = questions[Math.min(idx, total - 1)];
  const stagedAnswer = answers[q.id];
  const hasStaged = stagedAnswer !== undefined;
  const trimmedCustom = custom.trim();
  const hasCustom = trimmedCustom.length > 0;
  const prevDisabled = idx === 0;
  const nextDisabled = !hasStaged && !hasCustom;

  return (
    <section
      aria-label="Pending question prompt"
      className="shrink-0 border-t border-line bg-ink-900/80"
    >
      <div className="max-w-[860px] mx-auto px-6 py-2.5">
        <div className="rounded-xl border border-line bg-ink-850 overflow-hidden">
          <div className="px-4 h-11 border-b border-line flex items-center gap-2.5 bg-ink-800/60">
            <div className="w-6 h-6 rounded-md bg-honey-500/15 border border-honey-500/30 flex items-center justify-center">
              {I.chat({ size: 11, className: "text-honey-400" })}
            </div>
            <div className="text-[12.5px] font-semibold text-white">
              A few questions before I plan
            </div>
            <div className="flex-1" />
            <button
              type="button"
              onClick={goPrev}
              disabled={prevDisabled}
              aria-label="Previous question"
              className={`${navBtnBase} ${prevDisabled ? navBtnDisabled : navBtnEnabled}`}
            >
              {I.chevL({ size: 12 })}
            </button>
            <div
              role="status"
              aria-live="polite"
              className="text-[11px] text-dim font-mono"
            >
              {Math.min(idx + 1, total)} / {total}
            </div>
            <button
              type="button"
              onClick={goNext}
              disabled={nextDisabled}
              aria-label="Next question"
              className={`${navBtnBase} ${nextDisabled ? navBtnDisabled : navBtnEnabled}`}
            >
              {I.chevR({ size: 12 })}
            </button>
          </div>
          <div className="px-4 py-3 space-y-2.5">
            <div className="text-[13px] text-white/90">{q.title}</div>
            {q.sub && (
              <div className="text-[11.5px] text-dim">{q.sub}</div>
            )}
            {q.kind === "choice" && q.options && (
              <div className="flex flex-col gap-1.5">
                {q.options.map((opt) => {
                  // Highlight only when the staged answer matches AND
                  // the user hasn't typed a freeform override yet.
                  const isSelected =
                    stagedAnswer === opt.id && !hasCustom;
                  const selectedCls = isSelected
                    ? "border-honey-500/60 bg-honey-500/10"
                    : "border-line hover:border-honey-500/40 bg-ink-900 hover:bg-honey-500/5";
                  return (
                    <button
                      key={opt.id}
                      type="button"
                      onClick={() => commit(q.id, opt.id)}
                      disabled={submitted}
                      aria-pressed={isSelected}
                      className={`text-left px-3 py-2 rounded-md border transition-colors disabled:opacity-50 ${selectedCls}`}
                    >
                      <div className="text-[12.5px] text-white/90">
                        {opt.label}
                      </div>
                      {opt.hint && (
                        <div className="text-[11px] text-dim mt-0.5">
                          {opt.hint}
                        </div>
                      )}
                    </button>
                  );
                })}
              </div>
            )}
            {q.kind === "choice" && q.options && q.options.length > 0 && (
              <div className="pt-1 text-[11px] text-dim">
                Or type your own answer
              </div>
            )}
            <div className="flex gap-2">
              <input
                value={custom}
                onChange={(e) => setCustom(e.target.value)}
                onKeyDown={(e) => {
                  if (e.key !== "Enter") return;
                  if (submitted) return;
                  const trimmed = custom.trim();
                  if (!trimmed) return; // empty Enter is now a no-op
                  commit(q.id, trimmed);
                }}
                placeholder={q.placeholder || "Type your answer\u2026"}
                aria-label={
                  q.kind === "choice" ? "Custom answer" : "Your answer"
                }
                disabled={submitted}
                className="flex-1 h-9 px-3 rounded-md bg-ink-900 border border-line text-[12.5px] text-white placeholder:text-dim focus:border-honey-500/40 focus:outline-none focus-visible:ring-2 focus-visible:ring-honey-500 focus-visible:ring-offset-2 focus-visible:ring-offset-ink-950 disabled:opacity-50"
              />
              <button
                type="button"
                onClick={() => {
                  if (submitted) return;
                  const trimmed = custom.trim();
                  if (!trimmed) return;
                  commit(q.id, trimmed);
                }}
                disabled={submitted || !hasCustom}
                className="h-9 px-3 rounded-md bg-honey-500 text-ink-900 text-[12.5px] font-semibold disabled:opacity-50 disabled:cursor-not-allowed"
              >
                Send
              </button>
            </div>
          </div>
        </div>
      </div>
    </section>
  );
}
