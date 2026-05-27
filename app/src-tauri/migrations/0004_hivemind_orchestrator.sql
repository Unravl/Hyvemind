ALTER TABLE hiveminds ADD COLUMN inherit_orchestrator INTEGER NOT NULL DEFAULT 1;
ALTER TABLE hiveminds ADD COLUMN orchestrator_model TEXT;
ALTER TABLE hiveminds ADD COLUMN orchestrator_provider TEXT;
ALTER TABLE hiveminds ADD COLUMN orchestrator_thinking TEXT NOT NULL DEFAULT 'high';
