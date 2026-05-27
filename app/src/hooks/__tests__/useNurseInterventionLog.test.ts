import { describe, it, expect, vi, beforeEach, afterEach, type Mock } from "vitest";
import { renderHook, act, waitFor } from "@testing-library/react";

beforeEach(() => {
  (globalThis as { isTauri?: unknown }).isTauri = true;
  (window as { __TAURI_INTERNALS__?: unknown }).__TAURI_INTERNALS__ = {};
});
afterEach(() => {
  delete (globalThis as { isTauri?: unknown }).isTauri;
  delete (window as { __TAURI_INTERNALS__?: unknown }).__TAURI_INTERNALS__;
});

vi.mock("../../lib/ipc", async () => {
  const actual = await vi.importActual<typeof import("../../lib/ipc")>("../../lib/ipc");
  return {
    ...actual,
    getNurseInterventionLog: vi.fn(),
    clearNurseInterventionLog: vi.fn(),
  };
});

import { useNurseInterventionLog } from "../useNurseInterventionLog";
import { getNurseInterventionLog, clearNurseInterventionLog } from "../../lib/ipc";

beforeEach(() => {
  (getNurseInterventionLog as unknown as Mock).mockReset();
  (clearNurseInterventionLog as unknown as Mock).mockReset();
});

describe("useNurseInterventionLog", () => {
  it("calls the IPC with the initial query on mount", async () => {
    (getNurseInterventionLog as unknown as Mock).mockResolvedValue({
      rows: [],
      next_before_ts: null,
    });
    renderHook(() => useNurseInterventionLog({ profile: "tasks" }));
    await waitFor(() =>
      expect(getNurseInterventionLog).toHaveBeenCalledWith(
        expect.objectContaining({ profile: "tasks", limit: 50 }),
      ),
    );
  });

  it("appends rows when loadMore is called and threads next_before_ts", async () => {
    (getNurseInterventionLog as unknown as Mock).mockImplementation(
      async (q: { before_ts?: string }) => {
        if (!q.before_ts) {
          return {
            rows: [
              {
                id: "a",
                session_id: "s1",
                timestamp: "2026-05-19T01:00:00.000Z",
                level: "steer",
                analysis: "",
                action_taken: {
                  level: "steer",
                  session_id: "s1",
                  message: "x",
                  timestamp: "2026-05-19T01:00:00.000Z",
                },
                outcome: null,
              },
            ],
            next_before_ts: "2026-05-19T00:30:00.000Z",
          };
        }
        return {
          rows: [
            {
              id: "b",
              session_id: "s1",
              timestamp: "2026-05-19T00:30:00.000Z",
              level: "cancel",
              analysis: "",
              action_taken: {
                level: "cancel",
                session_id: "s1",
                message: "y",
                timestamp: "2026-05-19T00:30:00.000Z",
              },
              outcome: null,
            },
          ],
          next_before_ts: null,
        };
      },
    );

    const { result } = renderHook(() => useNurseInterventionLog({}));
    await waitFor(() => expect(result.current.rows).toHaveLength(1));
    expect(result.current.hasMore).toBe(true);

    await act(async () => {
      await result.current.loadMore();
    });

    expect(result.current.rows.map((r) => r.id)).toEqual(["a", "b"]);
    expect(result.current.hasMore).toBe(false);

    // The second IPC call must thread the cursor.
    const secondCallArgs = (getNurseInterventionLog as unknown as Mock).mock
      .calls[1][0];
    expect(secondCallArgs.before_ts).toBe("2026-05-19T00:30:00.000Z");
  });

  it("resets the row list when the query changes (filter change)", async () => {
    (getNurseInterventionLog as unknown as Mock).mockImplementation(
      async (q: { profile?: string | null }) => {
        return {
          rows: q.profile === "swarm" ? [{ id: "swarm-1" }] : [{ id: "task-1" }],
          next_before_ts: null,
        };
      },
    );

    const { result } = renderHook(() => useNurseInterventionLog({}));
    await waitFor(() => expect(result.current.rows.length).toBe(1));
    expect((result.current.rows[0] as { id: string }).id).toBe("task-1");

    await act(async () => {
      result.current.setQuery({ profile: "swarm" });
    });
    await waitFor(() => {
      expect((result.current.rows[0] as { id: string }).id).toBe("swarm-1");
    });
    // The query change resets the cursor — the new fetch sees no `before_ts`.
    const calls = (getNurseInterventionLog as unknown as Mock).mock.calls;
    const lastCall = calls[calls.length - 1][0];
    expect(lastCall.before_ts ?? null).toBeNull();
  });

  it("tolerates a bare-array response from legacy backends without throwing", async () => {
    // Legacy Hyvemind backends returned `Vec<NurseInterventionRecord>`
    // directly instead of `{ rows, next_before_ts }`. The hook must
    // surface `rows: []` (or the coerced array) without ever calling
    // `.length` on `undefined`.
    (getNurseInterventionLog as unknown as Mock).mockResolvedValue([
      {
        id: "legacy-1",
        session_id: "sid",
        timestamp: "2026-05-19T01:00:00.000Z",
        level: "steer",
        analysis: "",
        action_taken: {
          level: "steer",
          session_id: "sid",
          message: "x",
          timestamp: "2026-05-19T01:00:00.000Z",
        },
        outcome: null,
      },
    ]);
    const { result } = renderHook(() => useNurseInterventionLog({}));
    await waitFor(() => expect(result.current.isLoading).toBe(false));
    expect(result.current.error).toBeNull();
    expect(result.current.rows).toHaveLength(1);
    expect((result.current.rows[0] as { id: string }).id).toBe("legacy-1");
    expect(result.current.hasMore).toBe(false);
  });

  it("surfaces an error string on IPC rejection", async () => {
    (getNurseInterventionLog as unknown as Mock).mockRejectedValue(
      new Error("not_found: backend not wired"),
    );
    const { result } = renderHook(() => useNurseInterventionLog({}));
    await waitFor(() => {
      expect(result.current.error).toMatch(/not_found/);
    });
    expect(result.current.rows).toEqual([]);
  });

  it("calls clear IPC and refreshes to empty rows on clear", async () => {
    (getNurseInterventionLog as unknown as Mock).mockImplementation(
      async (_q: unknown) => {
        return {
          rows: [
            {
              id: "clear-test-1",
              session_id: "s1",
              timestamp: "2026-05-19T01:00:00.000Z",
              level: "steer",
              analysis: "",
              action_taken: {
                level: "steer",
                session_id: "s1",
                message: "x",
                timestamp: "2026-05-19T01:00:00.000Z",
              },
              outcome: null,
            },
          ],
          next_before_ts: null,
        };
      },
    );
    (clearNurseInterventionLog as unknown as Mock).mockResolvedValue(undefined);

    const { result } = renderHook(() => useNurseInterventionLog({}));
    await waitFor(() => expect(result.current.rows).toHaveLength(1));
    expect((result.current.rows[0] as { id: string }).id).toBe("clear-test-1");

    // After clear, the second fetchPage call returns empty.
    (getNurseInterventionLog as unknown as Mock).mockResolvedValue({
      rows: [],
      next_before_ts: null,
    });
    await act(async () => {
      await result.current.clear();
    });

    expect(clearNurseInterventionLog).toHaveBeenCalled();
    expect(result.current.rows).toEqual([]);
  });
});
