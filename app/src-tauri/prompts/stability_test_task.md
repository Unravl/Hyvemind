I'm running a stability test of Hyvemind's planning pipeline. Please drive the following task to completion exactly as if I asked it normally — the test passes when each phase (clarifying questions → plan → implementation) fires correctly.

## Task

Add a tiny utility to the file `sample.txt` in the current working directory. The utility is a single greeting line of the form `"Hello, <NAME>!"`. The exact format depends on your clarifying questions below.

## Required behavior

1. **Clarifying questions, first.** Before producing the plan, submit **exactly two** clarifying questions:
   - One `choice` question (e.g. about greeting style or formatting), with **2–3 options**. Mark one option `"recommended": true`.
   - One `text` question for a free-form preference.

   Call `submit_stability_questions({ questions: [...] })` with the array. Then STOP and wait for answers. Do not produce the plan in the same message as the questions.

2. **Plan, second.** Once you receive answers, submit a minimal but well-formed plan to edit `sample.txt`. The plan must mention `sample.txt` explicitly under "Files to Modify". Keep it short — this is a smoke test, not a real feature.

   Call `submit_stability_plan({ plan_markdown: "..." })`.

3. **Implementation, third.** When you receive the implementation prompt (sent automatically after Hivemind review), make the edit to `sample.txt` so it contains the greeting line you proposed, then call `submit_stability_impl_complete({})` to signal the implementation phase is finished.

## Constraints

- Keep total token usage low.
- The greeting line MUST end up written to `sample.txt`.
- Do not touch any other files.
- This is an automated test; if anything is ambiguous, pick sensible defaults rather than asking extra questions.
