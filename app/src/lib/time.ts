/** Detailed relative-time label with finer-grained tiers than `relativeTime`.
 *  Used for the live-ticking labels rendered next to chat bubbles and
 *  reasoning blocks in the Tasks view and inline Swarm chat.
 *
 *  Tiers:
 *  - `< 60s`       → "22s ago"
 *  - `< 60 min`    → "12m ago"
 *  - `< 24 h`      → "1h 12m ago" (drops the minutes segment when zero)
 *  - `>= 24 h`     → "2d 3h ago" (drops the hours segment when zero)
 *  - future/skew   → "0s ago"
 *  - undefined     → ""
 */
export function relativeTimeDetailed(
  createdAt: number | undefined,
  now: number = Date.now(),
): string {
  if (typeof createdAt !== "number") return "";
  const diff = now - createdAt;
  if (diff < 60_000) {
    const seconds = Math.max(0, Math.floor(diff / 1000));
    return `${seconds}s ago`;
  }
  if (diff < 3_600_000) {
    const minutes = Math.floor(diff / 60_000);
    return `${minutes}m ago`;
  }
  if (diff < 86_400_000) {
    const hours = Math.floor(diff / 3_600_000);
    const minutes = Math.floor((diff % 3_600_000) / 60_000);
    return minutes === 0 ? `${hours}h ago` : `${hours}h ${minutes}m ago`;
  }
  const days = Math.floor(diff / 86_400_000);
  const hours = Math.floor((diff % 86_400_000) / 3_600_000);
  return hours === 0 ? `${days}d ago` : `${days}d ${hours}h ago`;
}

/** Relative time label from an epoch timestamp. */
export function relativeTime(createdAt: number | undefined, fallback?: string): string {
  if (typeof createdAt !== "number") return fallback || "";
  const diff = Date.now() - createdAt;
  if (diff < 0) return "now"; // guard against clock skew / future timestamps
  const msMin = 60_000;
  const msHour = 3_600_000;
  const msDay = 86_400_000;
  if (diff < msMin) return "now";
  if (diff < msHour) return `${Math.floor(diff / msMin)}m`;
  if (diff < msDay) return `${Math.floor(diff / msHour)}h`;
  return `${Math.floor(diff / msDay)}d`;
}

/** Calendar-based time group from an epoch timestamp. */
export function timeGroup(createdAt: number | undefined): string {
  if (typeof createdAt !== "number") return "Older";
  const now = Date.now();
  if (createdAt > now) return "Today"; // future timestamps (clock skew)
  const today = new Date(); today.setHours(0, 0, 0, 0);
  const todayMs = today.getTime();
  const day = 86_400_000;
  if (createdAt >= todayMs) return "Today";
  if (createdAt >= todayMs - day) return "Yesterday";
  if (createdAt >= todayMs - 7 * day) return "This week";
  return "Older";
}
