-- Add sort_order column to job_steps for deterministic model ordering
ALTER TABLE job_steps ADD COLUMN sort_order INTEGER NOT NULL DEFAULT 0;
