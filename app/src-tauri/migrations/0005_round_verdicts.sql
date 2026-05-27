CREATE TABLE IF NOT EXISTS round_verdicts (
    id TEXT PRIMARY KEY,
    job_id TEXT NOT NULL REFERENCES jobs(id) ON DELETE CASCADE,
    round_number INTEGER NOT NULL,
    reviewer_model TEXT NOT NULL,
    suggestion TEXT NOT NULL,
    verdict TEXT NOT NULL CHECK(verdict IN ('accepted','rejected','modified')),
    severity INTEGER,
    reason TEXT,
    created_at TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_round_verdicts_job ON round_verdicts(job_id);
CREATE INDEX IF NOT EXISTS idx_round_verdicts_job_round ON round_verdicts(job_id, round_number);
