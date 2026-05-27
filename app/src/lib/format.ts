/* ── Formatting helpers ───────────────────────────────────── */

export function formatCost(cost: number): string {
  if (cost === 0) return "$0.00";
  if (cost < 0.01) return `$${cost.toFixed(4)}`;
  if (cost < 1) return `$${cost.toFixed(3)}`;
  return `$${cost.toFixed(2)}`;
}

export function formatTokens(n: number): string {
  if (n < 1000) return String(n);
  if (n < 1_000_000) return `${(n / 1000).toFixed(1)}k`;
  return `${(n / 1_000_000).toFixed(2)}M`;
}

export function formatElapsed(ms: number): string {
  if (ms <= 0) return "0s";
  const totalSec = Math.floor(ms / 1000);
  const h = Math.floor(totalSec / 3600);
  const m = Math.floor((totalSec % 3600) / 60);
  const s = totalSec % 60;
  if (h > 0) return `${h}h ${m}m`;
  if (m > 0) return `${m}m ${s}s`;
  return `${s}s`;
}

export function elapsedSinceIso(iso: string | undefined | null): number {
  if (!iso) return 0;
  const t = Date.parse(iso);
  if (Number.isNaN(t)) return 0;
  return Math.max(0, Date.now() - t);
}

/**
 * Whether a swarm status should be treated as "live" for elapsed-timer
 * purposes (i.e. the duration chip should tick once per second).
 *
 * Accepts both raw backend statuses (`implementing`) and the UI-mapped
 * statuses produced by `swarmStateToSwarm` (`running`). Callers don't need
 * to think about which one they have.
 *
 * `planning` is treated as live by design: during planning the user is
 * actively interacting with the Queen in Tasks, so the elapsed timer
 * should reflect that the session is in progress. If this ever changes,
 * editing this single function flips the behaviour across every consumer.
 *
 * Everything else (`paused`, `interrupted`, `completed`, `failed`,
 * `cancelled`, `undefined`, `null`, empty string, unknown values) is
 * treated as frozen.
 */
export function isLiveSwarmStatus(status: string | undefined | null): boolean {
  return status === "running" || status === "planning" || status === "implementing";
}

/**
 * Compute the elapsed milliseconds to display for a swarm card / top-bar
 * chip. Returns a live value (`Date.now() - createdAt`) for active swarms
 * and a frozen value (`updatedAt - createdAt`) for everything else, so the
 * displayed duration doesn't creep upward on poll-induced re-renders for
 * paused/completed/failed swarms.
 *
 * Fallback chain (in order):
 *   1. `createdAt` missing / null / unparseable → `fallbackMs ?? 0`.
 *   2. `isLiveSwarmStatus(status)` → `Math.max(0, Date.now() - createdAt)`.
 *   3. `updatedAt` parseable → `Math.max(0, updatedAt - createdAt)`
 *      (defensive clamp against clock skew between the two backend writes).
 *   4. Otherwise → `fallbackMs ?? 0` (abnormal state: backend always sets
 *      both `created_at` and `updated_at`).
 */
export function swarmElapsedMs({
  status,
  createdAt,
  updatedAt,
  fallbackMs,
}: {
  status: string | undefined | null;
  createdAt: string | undefined | null;
  updatedAt: string | undefined | null;
  fallbackMs?: number | undefined | null;
}): number {
  if (!createdAt) return fallbackMs ?? 0;
  const createdAtMs = Date.parse(createdAt);
  if (Number.isNaN(createdAtMs)) return fallbackMs ?? 0;
  if (isLiveSwarmStatus(status)) {
    return Math.max(0, Date.now() - createdAtMs);
  }
  if (updatedAt) {
    const updatedAtMs = Date.parse(updatedAt);
    if (!Number.isNaN(updatedAtMs)) {
      return Math.max(0, updatedAtMs - createdAtMs);
    }
  }
  return fallbackMs ?? 0;
}
