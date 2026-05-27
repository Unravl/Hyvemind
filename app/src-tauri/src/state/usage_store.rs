use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sqlx::{Row, SqlitePool};
use tracing::debug;

use crate::hivemind::store::repeat_vars;

#[derive(Debug, Clone)]
pub struct UsageEntry {
    pub source: String,
    pub source_id: Option<String>,
    pub model_id: String,
    pub provider: String,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cache_read_tokens: i64,
    pub cache_write_tokens: i64,
    pub cost: f64,
    pub duration_ms: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelUsageSummary {
    pub model_id: String,
    pub provider: String,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub total_tokens: i64,
    pub calls: i64,
    pub cost: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderUsageSummary {
    pub provider: String,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub total_tokens: i64,
    pub calls: i64,
    pub cost: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CostSummary {
    pub today: f64,
    pub week: f64,
    pub month: f64,
    pub all_time: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActivityEntry {
    pub id: String,
    pub timestamp: String,
    pub source: String,
    pub source_id: Option<String>,
    pub model_id: String,
    pub provider: String,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cost: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct SessionUsageSummary {
    pub session_id: String,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cost: f64,
    pub duration_ms: i64,
}

#[derive(Debug, Clone)]
pub struct UsageStore {
    pool: SqlitePool,
}

impl UsageStore {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    /// Borrow the underlying SQLite pool for callers that need to run
    /// custom queries (e.g. per-swarm aggregations in commands::swarms).
    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }

    pub async fn record_usage(&self, entry: UsageEntry) -> Result<()> {
        let id = uuid::Uuid::new_v4().to_string();
        debug!(id = %id, source = %entry.source, model = %entry.model_id, "recording usage");

        sqlx::query(
            "INSERT INTO usage_log (id, source, source_id, model_id, provider, \
             input_tokens, output_tokens, cache_read_tokens, cache_write_tokens, cost, duration_ms) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        )
        .bind(&id)
        .bind(&entry.source)
        .bind(&entry.source_id)
        .bind(&entry.model_id)
        .bind(&entry.provider)
        .bind(entry.input_tokens)
        .bind(entry.output_tokens)
        .bind(entry.cache_read_tokens)
        .bind(entry.cache_write_tokens)
        .bind(entry.cost)
        .bind(entry.duration_ms)
        .execute(&self.pool)
        .await
        .context("Failed to record usage")?;

        Ok(())
    }

    pub async fn get_model_usage(&self, since: Option<&str>) -> Result<Vec<ModelUsageSummary>> {
        let rows = if let Some(since) = since {
            sqlx::query(
                "SELECT model_id, provider, \
                 SUM(input_tokens) as input_tokens, \
                 SUM(output_tokens) as output_tokens, \
                 SUM(input_tokens + output_tokens) as total_tokens, \
                 COUNT(*) as calls, \
                 SUM(cost) as cost \
                 FROM usage_log WHERE timestamp >= ?1 \
                 GROUP BY model_id, provider \
                 ORDER BY total_tokens DESC LIMIT 10",
            )
            .bind(since)
            .fetch_all(&self.pool)
            .await
            .context("Failed to get model usage")?
        } else {
            sqlx::query(
                "SELECT model_id, provider, \
                 SUM(input_tokens) as input_tokens, \
                 SUM(output_tokens) as output_tokens, \
                 SUM(input_tokens + output_tokens) as total_tokens, \
                 COUNT(*) as calls, \
                 SUM(cost) as cost \
                 FROM usage_log \
                 GROUP BY model_id, provider \
                 ORDER BY total_tokens DESC LIMIT 10",
            )
            .fetch_all(&self.pool)
            .await
            .context("Failed to get model usage")?
        };

        Ok(rows
            .iter()
            .map(|r| ModelUsageSummary {
                model_id: r.get("model_id"),
                provider: r.get("provider"),
                input_tokens: r.get("input_tokens"),
                output_tokens: r.get("output_tokens"),
                total_tokens: r.get("total_tokens"),
                calls: r.get("calls"),
                cost: r.get("cost"),
            })
            .collect())
    }

    pub async fn get_provider_usage(
        &self,
        since: Option<&str>,
    ) -> Result<Vec<ProviderUsageSummary>> {
        let rows = if let Some(since) = since {
            sqlx::query(
                "SELECT provider, \
                 SUM(input_tokens) as input_tokens, \
                 SUM(output_tokens) as output_tokens, \
                 SUM(input_tokens + output_tokens) as total_tokens, \
                 COUNT(*) as calls, \
                 SUM(cost) as cost \
                 FROM usage_log WHERE timestamp >= ?1 \
                 GROUP BY provider \
                 ORDER BY total_tokens DESC",
            )
            .bind(since)
            .fetch_all(&self.pool)
            .await
            .context("Failed to get provider usage")?
        } else {
            sqlx::query(
                "SELECT provider, \
                 SUM(input_tokens) as input_tokens, \
                 SUM(output_tokens) as output_tokens, \
                 SUM(input_tokens + output_tokens) as total_tokens, \
                 COUNT(*) as calls, \
                 SUM(cost) as cost \
                 FROM usage_log \
                 GROUP BY provider \
                 ORDER BY total_tokens DESC",
            )
            .fetch_all(&self.pool)
            .await
            .context("Failed to get provider usage")?
        };

        Ok(rows
            .iter()
            .map(|r| ProviderUsageSummary {
                provider: r.get("provider"),
                input_tokens: r.get("input_tokens"),
                output_tokens: r.get("output_tokens"),
                total_tokens: r.get("total_tokens"),
                calls: r.get("calls"),
                cost: r.get("cost"),
            })
            .collect())
    }

    /// Sum the `cost` column for all `usage_log` rows whose timestamp falls
    /// on the current UTC day. Used by Phase 5A budget enforcement in
    /// `core::queen::run_swarm_full` to decide whether the global daily
    /// cap has been exceeded.
    ///
    /// Returns `0.0` (not an error) when the table is empty. Callers
    /// MUST treat a fallible result as non-fatal — the budget check
    /// should never kill the swarm because the check itself failed.
    pub async fn daily_total_cost(&self) -> Result<f64> {
        // `date('now')` in SQLite is UTC; rows inserted with the default
        // CURRENT_TIMESTAMP / `datetime('now')` are also UTC. Comparing
        // the raw `timestamp` TEXT against `date('now')` works because the
        // ISO 8601 format sorts lexically.
        let total: f64 = sqlx::query(
            "SELECT COALESCE(SUM(cost), 0.0) as total FROM usage_log \
             WHERE timestamp >= date('now')",
        )
        .fetch_one(&self.pool)
        .await
        .context("Failed to get daily total cost")?
        .get("total");
        Ok(total)
    }

    pub async fn get_cost_summary(&self) -> Result<CostSummary> {
        let today: f64 = sqlx::query(
            "SELECT COALESCE(SUM(cost), 0.0) as total FROM usage_log \
             WHERE timestamp >= date('now')",
        )
        .fetch_one(&self.pool)
        .await
        .context("Failed to get today cost")?
        .get("total");

        let week: f64 = sqlx::query(
            "SELECT COALESCE(SUM(cost), 0.0) as total FROM usage_log \
             WHERE timestamp >= date('now', '-7 days')",
        )
        .fetch_one(&self.pool)
        .await
        .context("Failed to get week cost")?
        .get("total");

        let month: f64 = sqlx::query(
            "SELECT COALESCE(SUM(cost), 0.0) as total FROM usage_log \
             WHERE timestamp >= date('now', '-30 days')",
        )
        .fetch_one(&self.pool)
        .await
        .context("Failed to get month cost")?
        .get("total");

        let all_time: f64 = sqlx::query("SELECT COALESCE(SUM(cost), 0.0) as total FROM usage_log")
            .fetch_one(&self.pool)
            .await
            .context("Failed to get all-time cost")?
            .get("total");

        Ok(CostSummary {
            today,
            week,
            month,
            all_time,
        })
    }

    pub async fn get_usage_for_sessions(
        &self,
        session_ids: &[String],
    ) -> Result<Vec<SessionUsageSummary>> {
        if session_ids.is_empty() {
            return Ok(vec![]);
        }
        let sql = format!(
            "SELECT source_id, SUM(input_tokens) as total_input, \
                    SUM(output_tokens) as total_output, SUM(cost) as total_cost, \
                    SUM(duration_ms) as total_duration \
             FROM usage_log \
             WHERE source_id IN ({}) AND source_id IS NOT NULL \
             GROUP BY source_id",
            repeat_vars(session_ids.len())
        );
        let mut query = sqlx::query(&sql);
        for sid in session_ids {
            query = query.bind(sid);
        }
        let rows = query.fetch_all(&self.pool).await?;
        Ok(rows
            .iter()
            .map(|r| SessionUsageSummary {
                session_id: r.get("source_id"),
                input_tokens: r.get::<i64, _>("total_input"),
                output_tokens: r.get::<i64, _>("total_output"),
                cost: r.get::<f64, _>("total_cost"),
                duration_ms: r.get::<i64, _>("total_duration"),
            })
            .collect())
    }

    pub async fn get_recent_activity(&self, limit: u32) -> Result<Vec<ActivityEntry>> {
        let rows = sqlx::query(
            "SELECT id, timestamp, source, source_id, model_id, provider, \
             input_tokens, output_tokens, cost \
             FROM usage_log ORDER BY timestamp DESC LIMIT ?1",
        )
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .context("Failed to get recent activity")?;

        Ok(rows
            .iter()
            .map(|r| ActivityEntry {
                id: r.get("id"),
                timestamp: r.get("timestamp"),
                source: r.get("source"),
                source_id: r.get("source_id"),
                model_id: r.get("model_id"),
                provider: r.get("provider"),
                input_tokens: r.get("input_tokens"),
                output_tokens: r.get("output_tokens"),
                cost: r.get("cost"),
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hivemind::store::HivemindStore;
    use tempfile::TempDir;

    async fn fresh_usage_store() -> (UsageStore, TempDir) {
        // The hivemind store owns the migrations; the usage_log table is
        // created by `0002_usage_log.sql` and lives in the same SQLite
        // database. Reuse it here so daily_total_cost runs against a real
        // schema rather than a hand-rolled fixture.
        let tmp = TempDir::new().expect("tempdir");
        let db_path = tmp.path().join("hm.sqlite");
        let store = HivemindStore::new(&db_path).await.expect("open hm store");
        (UsageStore::new(store.pool().clone()), tmp)
    }

    fn entry(cost: f64) -> UsageEntry {
        UsageEntry {
            source: "test".into(),
            source_id: None,
            model_id: "model".into(),
            provider: "anthropic".into(),
            input_tokens: 0,
            output_tokens: 0,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
            cost,
            duration_ms: 0,
        }
    }

    #[tokio::test]
    async fn daily_total_cost_empty_table_is_zero() {
        let (store, _tmp) = fresh_usage_store().await;
        let total = store.daily_total_cost().await.expect("query");
        assert_eq!(total, 0.0);
    }

    #[tokio::test]
    async fn daily_total_cost_sums_todays_rows() {
        let (store, _tmp) = fresh_usage_store().await;
        // Insert three rows with default timestamp (`datetime('now')`,
        // UTC today). All three should land inside today's window.
        store.record_usage(entry(1.25)).await.expect("rec 1");
        store.record_usage(entry(0.75)).await.expect("rec 2");
        store.record_usage(entry(0.50)).await.expect("rec 3");

        let total = store.daily_total_cost().await.expect("query");
        assert!((total - 2.50).abs() < 1e-9, "expected ~2.50, got {}", total);
    }

    #[tokio::test]
    async fn daily_total_cost_excludes_yesterdays_rows() {
        let (store, _tmp) = fresh_usage_store().await;
        // Today's row.
        store.record_usage(entry(3.00)).await.expect("rec today");

        // A row dated explicitly to yesterday: pin the timestamp via a
        // raw insert so the WHERE clause's `>= date('now')` excludes it.
        sqlx::query(
            "INSERT INTO usage_log (id, timestamp, source, source_id, model_id, \
             provider, input_tokens, output_tokens, cache_read_tokens, \
             cache_write_tokens, cost, duration_ms) \
             VALUES (?1, date('now','-1 day'), 'test', NULL, 'model', \
             'anthropic', 0, 0, 0, 0, ?2, 0)",
        )
        .bind(uuid::Uuid::new_v4().to_string())
        .bind(99.99_f64)
        .execute(store.pool())
        .await
        .expect("insert yesterday");

        let total = store.daily_total_cost().await.expect("query");
        assert!(
            (total - 3.00).abs() < 1e-9,
            "expected only today's 3.00, got {}",
            total
        );
    }
}
