/* ── Shared UI preference helpers ─────────────────────────── */
/*
 * Keys and loaders for UI toggles that should persist across reloads
 * and be shared between screens (currently Tasks + Swarm Control).
 * Lifted from Tasks.tsx so multiple screens can share the same
 * localStorage namespace.
 */

export const SHOW_REASONING_KEY = "hyvemind:show-reasoning";
export const SHOW_TOOL_CALLS_KEY = "hyvemind:show-tool-calls";

export const loadShowReasoning = (): boolean => {
  try {
    const val = localStorage.getItem(SHOW_REASONING_KEY);
    return val === null ? true : val === "true";
  } catch {
    return true;
  }
};

export const loadShowToolCalls = (): boolean => {
  try {
    const val = localStorage.getItem(SHOW_TOOL_CALLS_KEY);
    return val === null ? true : val === "true";
  } catch {
    return true;
  }
};

/** Format a token count compactly: 999 → "999"; 1500 → "1.5k"; 10000 → "10k". */
export const fmtTok2 = (n: number): string =>
  n >= 1000 ? `${(n / 1000).toFixed(n >= 10000 ? 0 : 1)}k` : `${n}`;

/** Parse a context-window string like "200k", "1M", "128000" into a number. */
export const parseCtx2 = (s: string | undefined): number => {
  if (!s) return 200_000;
  const m = String(s).match(/([\d.]+)\s*([kKmM])?/);
  if (!m) return 200_000;
  const v = parseFloat(m[1]);
  const u = (m[2] || "").toLowerCase();
  return u === "m" ? v * 1_000_000 : u === "k" ? v * 1_000 : v;
};
