CREATE TABLE IF NOT EXISTS merge_runs (
    id TEXT PRIMARY KEY,
    job_id TEXT NOT NULL REFERENCES jobs(id) ON DELETE CASCADE,
    round_number INTEGER NOT NULL,
    session_id TEXT NOT NULL,
    model_id TEXT NOT NULL,
    provider TEXT NOT NULL,
    thinking_level TEXT NOT NULL DEFAULT 'high',
    status TEXT NOT NULL DEFAULT 'running'
        CHECK(status IN ('running','completed','failed','interrupted')),
    started_at TEXT NOT NULL DEFAULT (datetime('now')),
    completed_at TEXT,
    failed_at TEXT,
    error TEXT,
    output_path TEXT NOT NULL,
    output_len INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS idx_merge_runs_job ON merge_runs(job_id);
CREATE INDEX IF NOT EXISTS idx_merge_runs_status ON merge_runs(status);
CREATE UNIQUE INDEX IF NOT EXISTS idx_merge_runs_job_round
    ON merge_runs(job_id, round_number);
