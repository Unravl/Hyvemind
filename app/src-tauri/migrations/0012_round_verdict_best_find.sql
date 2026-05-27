-- Best-find marker + co-reviewers for round verdicts.
-- The merge model designates one verdict per round as the standout finding
-- (best_find = 1). When multiple reviewers independently raised the same
-- finding, co_reviewers stores the additional reviewers (besides the primary
-- reviewer_model) as a JSON array of canonical "provider/model_id" strings.
ALTER TABLE round_verdicts ADD COLUMN best_find INTEGER NOT NULL DEFAULT 0;
ALTER TABLE round_verdicts ADD COLUMN co_reviewers TEXT;

CREATE INDEX IF NOT EXISTS idx_round_verdicts_best_find
    ON round_verdicts(job_id, round_number, best_find);
