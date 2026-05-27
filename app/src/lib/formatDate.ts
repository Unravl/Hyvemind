/**
 * Normalize SQLite-style datetime strings ("YYYY-MM-DD HH:MM:SS") to ISO 8601 UTC.
 * Handles already-correct formats gracefully (no-op if already T-separated or Z-terminated).
 */
export function normalizeDateStr(s: string): string {
  if (!s || s.endsWith('Z') || s.includes('T')) return s;
  return s.replace(' ', 'T') + 'Z';
}

/**
 * Format a date string as a human-readable relative time.
 * Uses normalizeDateStr internally to correctly parse SQLite UTC timestamps.
 */
export function formatRelativeDate(dateStr: string): string {
  try {
    const date = new Date(normalizeDateStr(dateStr));
    const now = Date.now();
    const diffMs = now - date.getTime();
    if (isNaN(diffMs)) return dateStr;
    if (diffMs < 0) return "just now";
    const mins = Math.floor(diffMs / 60000);
    if (mins < 1) return "just now";
    if (mins < 60) return `${mins}m ago`;
    const hours = Math.floor(mins / 60);
    if (hours < 24) return `${hours}h ago`;
    const days = Math.floor(hours / 24);
    if (days === 1) return "yesterday";
    if (days < 7) return `${days}d ago`;
    return dateStr.slice(0, 10);
  } catch { return dateStr; }
}

export function timeAgo(ts: string): string {
  return formatRelativeDate(ts);
}
