/** Worker handoffs are now captured server-side off the `submit_handoff`
 *  Pi extension tool call — they never appear in the visible text stream.
 *  These helpers are retained as no-ops so callers in ActivityStream don't
 *  need conditional logic for legacy text-based handoff detection. */

export function hasHandoffStart(_text: string): boolean {
  return false;
}

export function hasCompleteHandoff(_text: string): boolean {
  return false;
}
