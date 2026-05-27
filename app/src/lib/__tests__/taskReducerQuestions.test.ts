import { describe, it, expect } from "vitest";
import { applyTaskEvent, makeInitialTaskState, type TaskMessage } from "../taskReducer";
import type { TaskQuestion } from "../questions";

const MODEL = "claude-sub/claude-opus-4-7";

function initial() {
  return makeInitialTaskState("task-test", MODEL);
}

function q(id: string, title = `Question ${id}`): TaskQuestion {
  return { id, kind: "text", title };
}

/**
 * Regression: the Tasks-view "questions modal" used to flicker from a freshly
 * arrived (second) batch back to the original (first) batch every time a
 * subsequent `chunk` / `done` / `resync` event landed. Root cause was the
 * reducer's `messages.find((m) => m.who === "questions" ...)` calls in those
 * cases, which return the EARLIEST matching entry. The fix routes those
 * lookups through `findLastUnansweredQuestionsMessage` (newest-unanswered
 * batch wins).
 *
 * Later regression: the dock also reappeared with the ORIGINAL questions
 * when you switched away from a task after answering and came back, because
 * the helper returned the latest batch regardless of whether the user had
 * already replied. The helper now treats a `who: "user"` message after the
 * latest questions message as the "answered" signal (matches
 * `hasUnansweredQuestions`) and returns undefined in that case.
 *
 * These tests drive `applyTaskEvent` through the exact event sequence that
 * triggers each bug and assert `pendingQuestions` ids stay correct.
 */
describe("taskReducer pendingQuestions tracking", () => {
  it("tracks the most recent batch after a follow-up chunk", () => {
    const s0 = initial();
    const s1 = applyTaskEvent(
      s0,
      { kind: "structured_questions", questions: [q("q1-a")] },
      MODEL,
    );
    expect(s1.pendingQuestions?.map((x) => x.id)).toEqual(["q1-a"]);

    const s2 = applyTaskEvent(s1, { kind: "chunk", content: " " }, MODEL);
    expect(s2.pendingQuestions?.map((x) => x.id)).toEqual(["q1-a"]);

    const s3 = applyTaskEvent(
      s2,
      { kind: "structured_questions", questions: [q("q2-a")] },
      MODEL,
    );
    expect(s3.pendingQuestions?.map((x) => x.id)).toEqual(["q2-a"]);

    // The smoking gun: another `chunk` after the second batch arrived.
    // Pre-fix this reverted pendingQuestions back to `["q1-a"]`.
    const s4 = applyTaskEvent(s3, { kind: "chunk", content: " " }, MODEL);
    expect(s4.pendingQuestions?.map((x) => x.id)).toEqual(["q2-a"]);
  });

  it("done after a second questions batch does not revert to the first", () => {
    const s0 = initial();
    const s1 = applyTaskEvent(
      s0,
      { kind: "structured_questions", questions: [q("q1-a")] },
      MODEL,
    );
    const s2 = applyTaskEvent(
      s1,
      { kind: "structured_questions", questions: [q("q2-a")] },
      MODEL,
    );
    expect(s2.pendingQuestions?.map((x) => x.id)).toEqual(["q2-a"]);

    // Pre-fix the trailing `done` re-derived pendingQuestions from history
    // via `messages.find(...)` and reverted to the first batch.
    const s3 = applyTaskEvent(s2, { kind: "done" }, MODEL);
    expect(s3.pendingQuestions?.map((x) => x.id)).toEqual(["q2-a"]);
  });

  it("resync with multiple questions batches in history picks the latest", () => {
    const messages: TaskMessage[] = [
      { who: "questions", questions: [q("q1-a")], model: MODEL },
      { who: "user", text: "answers to q1-a", model: MODEL },
      { who: "questions", questions: [q("q2-a")], model: MODEL },
    ];
    const s0 = initial();
    const s1 = applyTaskEvent(s0, { kind: "resync", messages }, MODEL);
    expect(s1.pendingQuestions?.map((x) => x.id)).toEqual(["q2-a"]);
  });

  it("preserves the live questions on resync when ev.messages is shorter than history", () => {
    // resync uses ev.messages only when it grows the array; here the
    // incoming array is shorter than current state, so messages stay put
    // and the helper still finds the live (unanswered) questions message
    // in prev.messages. The earlier `?? prev.pendingQuestions` fallback
    // is gone, but this case never needed it: the questions message lives
    // in `prev.messages` and the helper picks it up directly.
    const s0 = applyTaskEvent(
      initial(),
      { kind: "structured_questions", questions: [q("q-live")] },
      MODEL,
    );
    expect(s0.pendingQuestions?.map((x) => x.id)).toEqual(["q-live"]);

    const s1 = applyTaskEvent(
      s0,
      { kind: "resync", messages: [{ who: "user", text: "hi", model: MODEL }] },
      MODEL,
    );
    expect(s1.pendingQuestions?.map((x) => x.id)).toEqual(["q-live"]);
  });

  it("chunk after a user reply clears pendingQuestions", () => {
    // Simulates the Tasks-view flow: agent asks questions -> user submits
    // answers (which appends a `who: "user"` message via answerQuestions)
    // -> Pi streams its first chunk in response. The dock must unmount.
    const s0 = initial();
    const s1 = applyTaskEvent(
      s0,
      { kind: "structured_questions", questions: [q("q1")] },
      MODEL,
    );
    expect(s1.pendingQuestions?.map((x) => x.id)).toEqual(["q1"]);

    // Inject the user's reply directly into messages (mirrors what
    // answerQuestions does in taskRuntime.tsx).
    const s2 = {
      ...s1,
      messages: [
        ...s1.messages,
        { who: "user" as const, text: "my answer", model: MODEL },
      ],
    };

    const s3 = applyTaskEvent(s2, { kind: "chunk", content: " " }, MODEL);
    expect(s3.pendingQuestions).toBeNull();
  });

  it("done after a user reply clears pendingQuestions", () => {
    const s0 = initial();
    const s1 = applyTaskEvent(
      s0,
      { kind: "structured_questions", questions: [q("q1")] },
      MODEL,
    );
    const s2 = {
      ...s1,
      messages: [
        ...s1.messages,
        { who: "user" as const, text: "my answer", model: MODEL },
      ],
    };

    const s3 = applyTaskEvent(s2, { kind: "done" }, MODEL);
    expect(s3.pendingQuestions).toBeNull();
  });

  it("resync with answered questions in history yields null pendingQuestions", () => {
    // The precise scenario that fires when the user switches away from a
    // task with answered questions and comes back: the runtime re-folds
    // the messages array through a `resync` event. The dock must stay
    // unmounted.
    const messages: TaskMessage[] = [
      { who: "questions", questions: [q("q1")], model: MODEL },
      { who: "user", text: "my answer", model: MODEL },
    ];
    const s0 = initial();
    const s1 = applyTaskEvent(s0, { kind: "resync", messages }, MODEL);
    expect(s1.pendingQuestions).toBeNull();
  });

  it("a follow-up questions batch after an answered one still surfaces", () => {
    // After the user answers batch 1, the agent may ask MORE questions.
    // The dock must reappear with the new batch.
    const s0 = initial();
    const s1 = applyTaskEvent(
      s0,
      { kind: "structured_questions", questions: [q("q1")] },
      MODEL,
    );
    const s2 = {
      ...s1,
      messages: [
        ...s1.messages,
        { who: "user" as const, text: "my answer", model: MODEL },
      ],
    };
    // First chunk after answering: dock should be cleared.
    const s3 = applyTaskEvent(s2, { kind: "chunk", content: " " }, MODEL);
    expect(s3.pendingQuestions).toBeNull();

    // Agent asks a follow-up batch.
    const s4 = applyTaskEvent(
      s3,
      { kind: "structured_questions", questions: [q("q2")] },
      MODEL,
    );
    expect(s4.pendingQuestions?.map((x) => x.id)).toEqual(["q2"]);

    // Subsequent chunk keeps the new batch surfaced.
    const s5 = applyTaskEvent(s4, { kind: "chunk", content: " " }, MODEL);
    expect(s5.pendingQuestions?.map((x) => x.id)).toEqual(["q2"]);
  });

  it("sanity: a single batch + chunk + done leaves pendingQuestions intact", () => {
    const s0 = initial();
    const s1 = applyTaskEvent(
      s0,
      { kind: "structured_questions", questions: [q("only")] },
      MODEL,
    );
    const s2 = applyTaskEvent(s1, { kind: "chunk", content: " " }, MODEL);
    const s3 = applyTaskEvent(s2, { kind: "done" }, MODEL);
    expect(s3.pendingQuestions?.map((x) => x.id)).toEqual(["only"]);
  });
});
