/**
 * Bait prompts + side-triggers for testing the Nurse engine from the Tasks view.
 * Pick a scenario from the "Test Nurse ▾" dropdown in the composer; the runtime
 * fills the prompt (and, where applicable, schedules a side-action like
 * `sigkill_pi_session` after streaming starts).
 *
 * Nurse detectors don't care about the *purpose* of a session, only its event
 * stream and process state — so a legitimately-baited prompt is indistinguishable
 * from a real failure mode.
 */

export type NurseScenarioGroup = "pure-prompt" | "side-trigger";

export type NurseScenarioSideTrigger =
  | { kind: "kill_pi"; afterSecs: number }
  | { kind: "sigkill_pi"; afterSecs: number }
  | { kind: "abort_pi"; afterSecs: number };

export interface NurseScenario {
  id: string;
  label: string;
  group: NurseScenarioGroup;
  /** One-line description for the dropdown menu item. */
  description: string;
  /** Which detector this is designed to trip — surfaced as a small label. */
  trips: string;
  /** Bait prompt — paste into the composer. */
  prompt: string;
  /** Suggested model id. The dropdown surfaces this as a hint, not an override. */
  modelHint?: string;
  /** Optional side-action that fires N seconds after the prompt is sent. */
  sideTrigger?: NurseScenarioSideTrigger;
  /** If true, the scenario only works if the active Task is pointed at a real
   *  working directory (the bait references files on disk). */
  needsProjectPath?: boolean;
}

const STALL_SILENCE: NurseScenario = {
  id: "stall-silence",
  label: "Stall — silent pause",
  group: "pure-prompt",
  description:
    "Ask the model to stay completely silent for 5 minutes before responding. Trips the post-prompt-silence path.",
  trips: "StallDetector (post-prompt silence)",
  prompt:
    "This is a test of my session-monitoring system. Please simulate a stuck session: do not emit any text, thinking, or tool calls for exactly 5 full minutes. After the 5 minutes have elapsed, output the single word 'recovered'. During the wait, produce ZERO output of any kind — no streaming, no tool calls, no acknowledgement. Begin the silent wait now.",
  modelHint: "deepseek/deepseek-chat",
};

const REASONING_LOOP_EXACT: NurseScenario = {
  id: "reasoning-loop-exact",
  label: "Reasoning loop — exact repeat",
  group: "pure-prompt",
  description:
    "Ask the model to output the same line 300 times verbatim. Trips siphash exact-repeat detection.",
  trips: "ReasoningLoopDetector (exact)",
  prompt:
    "For a stress test of my output-parsing pipeline I need you to output the exact sentence 'The cat sat on the mat.' three hundred (300) times in a row, one occurrence per line, with no other text, no numbering, no commentary. Do not stop early. Do not paraphrase. Just the same sentence, 300 times.",
  modelHint: "deepseek/deepseek-chat",
};

const PARAPHRASE_LOOP: NurseScenario = {
  id: "paraphrase-loop",
  label: "Reasoning loop — paraphrase",
  group: "pure-prompt",
  description:
    "Ask the model to restate 'water is wet' 200 different ways. Trips the minhash paraphrase path.",
  trips: "ReasoningLoopDetector (paraphrase)",
  prompt:
    "I am stress-testing a monitoring system that detects when a model is stuck restating the same idea over and over. Please help by restating the single fact 'water is wet' in 200 distinct paraphrases, one paraphrase per line. Never use the exact same wording twice, but every line must mean the same thing: water is wet. Do not break out of the pattern; do not summarise; do not add explanation. Just 200 paraphrases.",
  modelHint: "deepseek/deepseek-chat",
};

const TOOL_FAILURE_BURST: NurseScenario = {
  id: "tool-failure-burst",
  label: "Tool failure — ENOENT burst",
  group: "pure-prompt",
  description:
    "Tell the model to read 15 files at deliberately-nonexistent paths and retry each on failure. Trips the failure-clustering detector.",
  trips: "ToolFailureDetector",
  prompt:
    "I need you to read the following 15 files in order using your read tool. They are part of a deliberate test of my error-handling pipeline; some or all may not exist. If a read fails, retry it 3 more times before moving on. Do NOT skip any path:\n\n" +
    Array.from({ length: 15 }, (_, i) =>
      `${i + 1}. /tmp/__hyvemind_nurse_test_${String.fromCharCode(97 + i)}_${Math.random().toString(36).slice(2, 8)}.txt`,
    ).join("\n") +
    "\n\nProceed through the list. Report the final outcome only after attempting all 15 with 3 retries each.",
};

const CONTEXT_SATURATION: NurseScenario = {
  id: "context-saturation",
  label: "Context saturation",
  group: "pure-prompt",
  description:
    "Walk the largest 20 files in the working directory, summarising each in 500 words. Trips the context-window percentage detector.",
  trips: "ContextSaturationDetector",
  prompt:
    "Find the 20 largest source files under the current working directory (recursively, excluding any node_modules / target / build / .git directories). For EACH file, read it in full and then produce a 500-word summary of its contents. Continue through all 20 without stopping. Do not skip large files; do not truncate reads.",
  needsProjectPath: true,
  modelHint: "anthropic/claude-haiku-4-5",
};

const PROCESS_CRASH: NurseScenario = {
  id: "process-crash",
  label: "Process crash — SIGKILL at 30s",
  group: "side-trigger",
  description:
    "Start a long-running counting task, then raw-SIGKILL the Pi subprocess 30 seconds in (bypassing the orderly shutdown path). Trips the subprocess-liveness detector on its next slow tick.",
  trips: "ProcessHealthDetector",
  prompt:
    "Begin a long-running enumeration: count from 1 to 1,000,000. For each number, output the number followed by a brief comment about whether it is prime, even, or a perfect square. One number per line. Do not stop; do not summarise; just keep counting. This will run for a long time on purpose.",
  sideTrigger: { kind: "sigkill_pi", afterSecs: 30 },
};

/**
 * Full catalog, in display order. Side-trigger scenarios are grouped after the
 * pure-prompt ones in the dropdown.
 */
export const NURSE_SCENARIOS: NurseScenario[] = [
  STALL_SILENCE,
  REASONING_LOOP_EXACT,
  PARAPHRASE_LOOP,
  TOOL_FAILURE_BURST,
  CONTEXT_SATURATION,
  PROCESS_CRASH,
];

export function findNurseScenario(id: string): NurseScenario | undefined {
  return NURSE_SCENARIOS.find((s) => s.id === id);
}
