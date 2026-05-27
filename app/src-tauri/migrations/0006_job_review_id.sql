ALTER TABLE jobs ADD COLUMN review_id TEXT;

CREATE INDEX IF NOT EXISTS idx_jobs_review_id ON jobs(review_id);
