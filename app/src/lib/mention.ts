/**
 * Detect whether the user is currently typing an `@`-mention at the cursor.
 *
 * A mention is a run of non-whitespace, non-`@` characters following an `@`
 * that is itself preceded by whitespace or beginning-of-string. The cursor
 * must fall within (or immediately after) the run; if it has moved past
 * whitespace, the mention is no longer active.
 *
 * Returns the span (start of the `@`, end of the `[^\s@]*` token) so callers
 * can derive the query (`value.slice(startIdx + 1, cursor)`) and the
 * replacement target (`value.slice(0, startIdx) + value.slice(tokenEnd)`).
 */
export interface MentionSpan {
  /** Index of the `@` character. */
  startIdx: number;
  /** Exclusive end of the `[^\s@]*` run after `@`. */
  tokenEnd: number;
}

/**
 * Scan backwards from `cursor` looking for an `@` that starts a valid mention.
 * Returns `null` if no active mention.
 */
export function detectMention(value: string, cursor: number): MentionSpan | null {
  if (cursor <= 0 || cursor > value.length) return null;

  // Walk backwards from cursor - 1, accepting non-whitespace, non-`@` chars.
  let i = cursor - 1;
  while (i >= 0) {
    const ch = value[i];
    if (ch === "@") {
      // Validate the `@` is at BOF or preceded by whitespace.
      if (i === 0 || /\s/.test(value[i - 1])) {
        const startIdx = i;
        // Compute tokenEnd: extend forward from cursor while still in `[^\s@]*`.
        let end = cursor;
        while (end < value.length) {
          const c = value[end];
          if (c === "@" || /\s/.test(c)) break;
          end++;
        }
        return { startIdx, tokenEnd: end };
      }
      // `@` not preceded by whitespace (e.g., email pattern) — no mention.
      return null;
    }
    if (/\s/.test(ch)) {
      // Hit whitespace before finding `@` — cursor is past a closed mention
      // (or just not in a mention at all).
      return null;
    }
    i--;
  }
  // Reached start of string without finding `@`.
  return null;
}
