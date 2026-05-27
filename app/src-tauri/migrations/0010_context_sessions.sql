CREATE TABLE IF NOT EXISTS context_sessions (
    job_id TEXT NOT NULL REFERENCES jobs(id) ON DELETE CASCADE,
    round_number INTEGER NOT NULL,
    session_id TEXT NOT NULL,
    model_id TEXT NOT NULL,
    provider TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    PRIMARY KEY (job_id, round_number)
);
