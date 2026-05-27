CREATE TABLE IF NOT EXISTS jobs (
    id TEXT PRIMARY KEY,
    plan TEXT NOT NULL,
    stance TEXT NOT NULL DEFAULT 'against',
    status TEXT NOT NULL DEFAULT 'pending',
    num_rounds INTEGER NOT NULL DEFAULT 1,
    current_round INTEGER NOT NULL DEFAULT 0,
    timeout_seconds INTEGER NOT NULL DEFAULT 300,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now')),
    completed_at TEXT,
    error TEXT,
    final_output TEXT,
    total_cost REAL NOT NULL DEFAULT 0.0,
    total_input_tokens INTEGER NOT NULL DEFAULT 0,
    total_output_tokens INTEGER NOT NULL DEFAULT 0,
    hivemind_id TEXT
);

CREATE TABLE IF NOT EXISTS job_steps (
    id TEXT PRIMARY KEY,
    job_id TEXT NOT NULL REFERENCES jobs(id) ON DELETE CASCADE,
    round_number INTEGER NOT NULL,
    model_id TEXT NOT NULL,
    provider TEXT NOT NULL,
    stance TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'pending',
    started_at TEXT,
    completed_at TEXT,
    input_tokens INTEGER,
    output_tokens INTEGER,
    cost REAL,
    output TEXT,
    error TEXT,
    duration_ms INTEGER
);

CREATE TABLE IF NOT EXISTS schema_migrations (
    version INTEGER PRIMARY KEY,
    applied_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE INDEX IF NOT EXISTS idx_job_steps_job_id ON job_steps(job_id);
CREATE INDEX IF NOT EXISTS idx_jobs_status ON jobs(status);
CREATE INDEX IF NOT EXISTS idx_jobs_hivemind_id ON jobs(hivemind_id);
