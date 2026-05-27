-- Composite indexes for hot query patterns.

-- Covers get_job_steps and fetch_steps_for_jobs:
--   WHERE job_id = ? ORDER BY round_number ASC, sort_order ASC, id ASC
--   WHERE job_id IN (...) ORDER BY job_id, round_number ASC, sort_order ASC, id ASC
-- The existing idx_job_steps_job_id covers the WHERE filter but SQLite still
-- needs a separate sort step. This composite lets it walk the index in order.
CREATE INDEX IF NOT EXISTS idx_job_steps_job_round_sort
  ON job_steps(job_id, round_number, sort_order, id);

-- Covers list_jobs_by_hivemind and count_logical_runs (hivemind filter):
--   WHERE hivemind_id = ? ORDER BY created_at DESC LIMIT ? OFFSET ?
-- The existing idx_jobs_hivemind_id covers the filter but not the sort.
-- This composite eliminates the separate ORDER BY sort.
CREATE INDEX IF NOT EXISTS idx_jobs_hivemind_created
  ON jobs(hivemind_id, created_at DESC);
