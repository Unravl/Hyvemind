CREATE TABLE IF NOT EXISTS usage_log (
    id          TEXT PRIMARY KEY,
    timestamp   TEXT NOT NULL DEFAULT (datetime('now')),
    source      TEXT NOT NULL,
    source_id   TEXT,
    model_id    TEXT NOT NULL,
    provider    TEXT NOT NULL,
    input_tokens  INTEGER NOT NULL DEFAULT 0,
    output_tokens INTEGER NOT NULL DEFAULT 0,
    cache_read_tokens INTEGER NOT NULL DEFAULT 0,
    cache_write_tokens INTEGER NOT NULL DEFAULT 0,
    cost        REAL NOT NULL DEFAULT 0.0,
    duration_ms INTEGER NOT NULL DEFAULT 0
);

CREATE INDEX idx_usage_log_timestamp ON usage_log(timestamp);
CREATE INDEX idx_usage_log_model ON usage_log(model_id);
CREATE INDEX idx_usage_log_provider ON usage_log(provider);
CREATE INDEX idx_usage_log_source ON usage_log(source);
