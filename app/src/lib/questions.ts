/** Structured-question data shapes. Questions reach the frontend via the
 *  `structured_questions` chat event (Rust backend captures off Pi's
 *  `submit_questions` tool call) — no delimiter scanning is performed
 *  client-side. */

export interface TaskQuestionOption {
  id: string;
  label: string;
  hint?: string;
  recommended?: boolean;
}

export interface TaskQuestion {
  id: string;
  kind: "choice" | "text";
  title: string;
  sub?: string;
  options?: TaskQuestionOption[];
  placeholder?: string;
}

/** Deserialise a `submit_questions` tool-args JSON payload into a typed
 *  `TaskQuestion[]`. Returns `null` when the shape is malformed. */
export function questionsFromToolArgs(parsed: unknown): TaskQuestion[] | null {
  // The tool args envelope is `{questions: [...]}`. Accept a bare array
  // for resilience (some callers pre-unwrap the envelope).
  const arr = Array.isArray(parsed)
    ? parsed
    : parsed && typeof parsed === "object" && Array.isArray((parsed as { questions?: unknown }).questions)
      ? (parsed as { questions: unknown[] }).questions
      : null;
  if (!arr) return null;
  for (const q of arr) {
    if (!q || typeof q !== "object") return null;
    const obj = q as Record<string, unknown>;
    if (!obj.id || !obj.kind || !obj.title) return null;
    if (obj.kind !== "choice" && obj.kind !== "text") return null;
  }
  return arr as TaskQuestion[];
}

/**
 * Build a natural-language prompt from the user's answers to send back to the Pi agent.
 */
export function buildAnswerPrompt(
  questions: TaskQuestion[],
  // Allow `any` here because answers come from form state with mixed shapes.
  answers: Record<string, any>,
): string {
  const lines: string[] = ["Here are my answers to your questions:", ""];
  for (const q of questions) {
    const answer = answers[q.id];
    lines.push(`Q: ${q.title}`);
    if (answer === undefined || answer === "__skipped__") {
      lines.push("A: (skipped)");
    } else if (typeof answer === "object" && answer.custom) {
      lines.push(`A: ${answer.custom}`);
    } else if (q.kind === "choice" && q.options) {
      const opt = q.options.find((o) => o.id === answer);
      lines.push(`A: ${opt ? opt.label : answer}`);
    } else {
      lines.push(`A: ${answer}`);
    }
    lines.push("");
  }
  lines.push("Please proceed with the plan based on these answers.");
  return lines.join("\n");
}

