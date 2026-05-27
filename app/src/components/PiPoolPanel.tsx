/**
 * Dashboard widget showing live Pi pool state (active sessions, available
 * permits, graveyard size) with per-session kill buttons.
 *
 * Read-only by default — mounted into Dashboard with a refresh interval of
 * 5s. Mount-and-forget; the underlying IPC is cheap.
 */

import { useEffect, useState } from "react";
import * as ipc from "../lib/ipc";
import type { PiPoolStats } from "../lib/ipc";

const REFRESH_MS = 5000;

function fmtAge(ts: number): string {
  if (!ts) return "—";
  const ageMs = Date.now() - ts;
  if (ageMs < 0) return "—";
  if (ageMs < 1000) return `${ageMs}ms`;
  if (ageMs < 60_000) return `${Math.round(ageMs / 1000)}s`;
  if (ageMs < 3600_000) return `${Math.round(ageMs / 60_000)}m`;
  return `${Math.round(ageMs / 3600_000)}h`;
}

export function PiPoolPanel() {
  const [stats, setStats] = useState<PiPoolStats | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [killingId, setKillingId] = useState<string | null>(null);

  const refresh = async () => {
    try {
      const next = await ipc.getPiPoolStats();
      setStats(next);
      setError(null);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
  };

  useEffect(() => {
    refresh();
    const id = setInterval(refresh, REFRESH_MS);
    return () => clearInterval(id);
  }, []);

  const kill = async (sessionId: string) => {
    if (killingId) return;
    setKillingId(sessionId);
    try {
      await ipc.killPiSession(sessionId);
      await refresh();
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setKillingId(null);
    }
  };

  if (error) {
    return (
      <div style={{ padding: 12, color: "var(--danger, #c33)" }}>
        Pi pool stats unavailable: {error}
      </div>
    );
  }

  if (!stats) {
    return <div style={{ padding: 12, opacity: 0.6 }}>Loading…</div>;
  }

  const utilization =
    stats.max_processes > 0
      ? Math.round(((stats.max_processes - stats.available_permits) / stats.max_processes) * 100)
      : 0;
  const highUtil = utilization >= 75;

  return (
    <div style={{ padding: 12, fontSize: 13 }}>
      <div style={{ display: "flex", gap: 16, alignItems: "baseline", marginBottom: 8 }}>
        <strong>Pi pool</strong>
        <span>
          {stats.active_count} / {stats.max_processes} active
        </span>
        <span style={{ color: highUtil ? "var(--warn, #b88)" : undefined }}>
          {utilization}% used
        </span>
        {stats.graveyard_size > 0 && (
          <span style={{ opacity: 0.6 }}>graveyard: {stats.graveyard_size}</span>
        )}
      </div>
      {stats.sessions.length === 0 ? (
        <div style={{ opacity: 0.5 }}>No active sessions.</div>
      ) : (
        <table style={{ width: "100%", borderCollapse: "collapse" }}>
          <thead>
            <tr style={{ textAlign: "left", opacity: 0.7 }}>
              <th>ID</th>
              <th>Owner</th>
              <th>Turns</th>
              <th>Events</th>
              <th>Idle</th>
              <th>State</th>
              <th />
            </tr>
          </thead>
          <tbody>
            {stats.sessions.map((s) => (
              <tr key={s.id} style={{ borderTop: "1px solid var(--border, #2a2a2a)" }}>
                <td title={s.id} style={{ fontFamily: "ui-monospace, monospace" }}>
                  {s.id.length > 16 ? s.id.slice(0, 16) + "…" : s.id}
                </td>
                <td style={{ opacity: 0.8 }}>{s.owner}</td>
                <td>{s.turn_count}</td>
                <td>{s.event_count}</td>
                <td>{fmtAge(s.last_activity_ms)}</td>
                <td style={{ opacity: 0.8 }}>
                  {!s.is_alive ? "dead" : s.is_busy ? "busy" : s.is_pinned ? "pinned" : "idle"}
                </td>
                <td>
                  <button
                    type="button"
                    onClick={() => kill(s.id)}
                    disabled={killingId === s.id}
                    style={{
                      fontSize: 11,
                      padding: "2px 6px",
                      cursor: killingId === s.id ? "wait" : "pointer",
                    }}
                  >
                    {killingId === s.id ? "killing…" : "kill"}
                  </button>
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      )}
    </div>
  );
}
