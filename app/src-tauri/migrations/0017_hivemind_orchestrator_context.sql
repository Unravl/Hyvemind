-- Persist the orchestrator's stored context_window and max_output so that the
-- frontend merge-prompt truncation step (and any future budget logic) can
-- consult the catalog/`/models`-provided values rather than a hardcoded table.
ALTER TABLE hiveminds ADD COLUMN orchestrator_context_window INTEGER;
ALTER TABLE hiveminds ADD COLUMN orchestrator_max_output INTEGER;
