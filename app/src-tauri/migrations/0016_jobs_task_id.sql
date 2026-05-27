ALTER TABLE jobs ADD COLUMN task_id TEXT;

CREATE INDEX IF NOT EXISTS idx_jobs_task_id ON jobs(task_id);
