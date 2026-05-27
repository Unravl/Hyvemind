import { readMergeOutput } from "./ipc";

/**
 * Load the merged plan markdown for a specific Hivemind review round.
 *
 * The backend's merge orchestrator writes the final `plan_markdown` body
 * (sourced from the `submit_plan` tool args) to
 * `~/.hyvemind/reviews/{review_id}/merge-r{N}.txt`. This helper:
 *
 *   1. Calls the `read_merge_output` IPC and gracefully handles failure.
 *   2. Returns `null` for empty / whitespace-only output (treat as absent).
 *   3. Returns the trimmed file contents otherwise.
 *
 * Never throws.
 */
export async function loadMergedPlan(
  jobId: string,
  round: number,
): Promise<string | null> {
  let raw: string;
  try {
    raw = await readMergeOutput({ jobId, round });
  } catch {
    return null;
  }
  const trimmed = raw?.trim() ?? "";
  return trimmed.length > 0 ? trimmed : null;
}
