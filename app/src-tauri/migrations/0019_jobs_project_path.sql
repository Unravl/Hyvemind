ALTER TABLE jobs ADD COLUMN project_path TEXT;

CREATE INDEX IF NOT EXISTS idx_jobs_project_path ON jobs(project_path);
