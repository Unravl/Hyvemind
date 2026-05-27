ALTER TABLE jobs ADD COLUMN name TEXT NOT NULL DEFAULT '';

-- Backfill existing rows: use first 100 chars of plan where name is empty
UPDATE jobs SET name = substr(plan, 1, 100) WHERE name = '' AND plan != '';
