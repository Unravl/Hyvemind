-- Supports grouped pagination by COALESCE(review_id, id).
-- SQLite may still sort MAX(created_at) in memory for the outer ORDER BY; at
-- current history sizes this is acceptable. If hivemind-scoped histories grow
-- large, profile a composite expression index on
-- (hivemind_id, COALESCE(NULLIF(review_id, ''), id), created_at DESC).
CREATE INDEX IF NOT EXISTS idx_jobs_logical_run
  ON jobs(COALESCE(NULLIF(review_id, ''), id), created_at DESC);

-- Supports bare review_id lookups for logical-run child aggregation and exact
-- job disambiguation. Kept here even though older databases may already have
-- this from 0006; IF NOT EXISTS makes it idempotent.
CREATE INDEX IF NOT EXISTS idx_jobs_review_id ON jobs(review_id);
