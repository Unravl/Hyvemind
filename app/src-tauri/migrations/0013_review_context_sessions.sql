-- Replaces 0010_context_sessions (keyed by job_id, which is unavailable
-- at context-gather time). The old table is dropped because nothing
-- references it and it was never populated.
DROP TABLE IF EXISTS context_sessions;

CREATE TABLE IF NOT EXISTS review_context_sessions (
    review_id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL,
    model_id TEXT NOT NULL,
    provider TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);
-- Note: no FK to a reviews table; that table does not exist yet. Add a
-- CASCADE FK when/if a reviews table is introduced. Orphaned rows are
-- harmless (the UI hides the orchestrator block when data is absent).
