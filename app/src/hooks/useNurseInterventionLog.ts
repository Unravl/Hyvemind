import { useCallback, useEffect, useRef, useState } from "react";
import * as ipc from "../lib/ipc";
import { isTauri } from "../lib/tauri";
import { formatIpcError } from "../lib/ipc";
import type {
  InterventionLogQuery,
  NurseInterventionRecord,
} from "../lib/nurseTypes";

interface Result {
  rows: NurseInterventionRecord[];
  hasMore: boolean;
  isLoading: boolean;
  error: string | null;
  loadMore: () => Promise<void>;
  /** Patch filter args. Resets the cursor and re-fetches the first page. */
  setQuery: (q: InterventionLogQuery) => void;
  /** Clear the in-memory intervention ring and refresh the first page. */
  clear: () => Promise<void>;
}

const DEFAULT_LIMIT = 50;

/**
 * Paginated Nurse intervention log driven by the
 * `get_nurse_intervention_log` IPC. Filter changes reset the cursor;
 * `loadMore()` appends the next page using `next_before_ts` returned
 * by the backend.
 *
 * If the IPC handler isn't wired yet (returns `not_found`), this hook
 * surfaces an empty list + a soft error string so the screen can show
 * a friendly empty state.
 */
export function useNurseInterventionLog(
  initialQuery: InterventionLogQuery = {},
): Result {
  const [query, setQueryState] = useState<InterventionLogQuery>({
    limit: DEFAULT_LIMIT,
    ...initialQuery,
  });
  const [rows, setRows] = useState<NurseInterventionRecord[]>([]);
  const [cursor, setCursor] = useState<string | null>(null);
  const [hasMore, setHasMore] = useState(false);
  const [isLoading, setIsLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // Guard against late responses overwriting newer queries.
  const fetchSeq = useRef(0);

  const fetchPage = useCallback(
    async (q: InterventionLogQuery, append: boolean) => {
      if (!isTauri()) {
        setRows([]);
        setHasMore(false);
        return;
      }
      const seq = ++fetchSeq.current;
      setIsLoading(true);
      setError(null);
      try {
        const page = await ipc.getNurseInterventionLog(q);
        if (seq !== fetchSeq.current) return;
        // Defensive coercion: older Hyvemind backends returned a bare
        // `Vec<NurseInterventionRecord>` array (pre-pagination) instead
        // of the `{ rows, next_before_ts }` envelope. Tolerate either
        // shape so a schema drift never crashes the screen with
        // `undefined is not an object (evaluating 'rows.length')`.
        const nextRows: NurseInterventionRecord[] = Array.isArray(page)
          ? page
          : Array.isArray(page?.rows)
            ? page.rows
            : [];
        const nextCursor: string | null = Array.isArray(page)
          ? null
          : (page?.next_before_ts ?? null);
        setRows((prev) => (append ? [...prev, ...nextRows] : nextRows));
        setCursor(nextCursor);
        setHasMore(nextCursor !== null);
      } catch (err) {
        if (seq !== fetchSeq.current) return;
        setError(formatIpcError(err));
        if (!append) {
          setRows([]);
          setHasMore(false);
        }
      } finally {
        if (seq === fetchSeq.current) setIsLoading(false);
      }
    },
    [],
  );

  // Fetch on mount + whenever the query changes.
  useEffect(() => {
    fetchPage(query, false);
  }, [query, fetchPage]);

  const setQuery = useCallback((q: InterventionLogQuery) => {
    setQueryState((prev) => ({ limit: DEFAULT_LIMIT, ...prev, ...q }));
  }, []);

  const loadMore = useCallback(async () => {
    if (!hasMore || !cursor || isLoading) return;
    await fetchPage({ ...query, before_ts: cursor }, true);
  }, [hasMore, cursor, isLoading, query, fetchPage]);

  const clear = useCallback(async () => {
    if (!isTauri()) return;
    await ipc.clearNurseInterventionLog();
    await fetchPage(query, false);
  }, [query, fetchPage]);

  return { rows, hasMore, isLoading, error, loadMore, setQuery, clear };
}
