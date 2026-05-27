use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::{Row, SqlitePool};
use tracing::{debug, info};

// ---------------------------------------------------------------------------
// Data models (mirror the `jobs` and `job_steps` tables)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Job {
    pub id: String,
    pub plan: String,
    pub name: String,
    pub stance: String,
    pub status: String,
    pub num_rounds: i64,
    pub current_round: i64,
    pub timeout_seconds: i64,
    pub created_at: String,
    pub updated_at: String,
    pub completed_at: Option<String>,
    pub error: Option<String>,
    pub final_output: Option<String>,
    pub total_cost: f64,
    pub total_input_tokens: i64,
    pub total_output_tokens: i64,
    pub review_id: Option<String>,
    pub hivemind_id: Option<String>,
    pub task_id: Option<String>,
    pub project_path: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobStep {
    pub id: String,
    pub job_id: String,
    pub round_number: i64,
    pub sort_order: i64,
    pub model_id: String,
    pub provider: String,
    pub stance: String,
    pub status: String,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
    pub input_tokens: Option<i64>,
    pub output_tokens: Option<i64>,
    pub cost: Option<f64>,
    pub output: Option<String>,
    pub error: Option<String>,
    pub duration_ms: Option<i64>,
    pub prompt: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoundVerdict {
    pub id: String,
    pub job_id: String,
    pub round_number: i64,
    pub reviewer_model: String,
    pub suggestion: String,
    /// "accepted" | "rejected" | "modified"
    pub verdict: String,
    pub severity: Option<i64>,
    pub reason: Option<String>,
    pub created_at: String,
    /// Designated by the merge model as the round's standout finding.
    /// Defaults to false; at most one verdict per round should be true.
    #[serde(default)]
    pub best_find: bool,
    /// Other reviewers (besides `reviewer_model`) who independently raised
    /// the same finding. Stored in SQLite as a JSON array string.
    #[serde(default)]
    pub co_reviewers: Option<Vec<String>>,
}

// ---------------------------------------------------------------------------
// Review context session (mirrors the `review_context_sessions` table)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewContextSession {
    pub review_id: String,
    pub session_id: String,
    pub model_id: String,
    pub provider: String,
    pub created_at: String,
}

// ---------------------------------------------------------------------------
// Hivemind configuration (mirrors the `hiveminds` table)
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Merge run model (mirrors the `merge_runs` table)
// ---------------------------------------------------------------------------

/// Lifecycle record for a single hivemind merge phase (one per round, after
/// the model dispatch finishes). Persisted by IPC commands the frontend
/// invokes around its merge orchestration; rows in `status='running'` at
/// startup are swept to `interrupted` so the UI can offer recovery.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MergeRunInfo {
    pub id: String,
    pub job_id: String,
    /// Resolved via LEFT JOIN to `jobs.review_id` — `None` if the parent
    /// job has no logical review_id (legacy / standalone runs).
    pub review_id: Option<String>,
    pub round_number: i64,
    pub session_id: String,
    pub model_id: String,
    pub provider: String,
    pub thinking_level: String,
    pub status: String,
    pub started_at: String,
    pub completed_at: Option<String>,
    pub failed_at: Option<String>,
    pub error: Option<String>,
    pub output_path: String,
    pub output_len: i64,
}

/// Lightweight snapshot of a `jobs` row swept to `interrupted` at startup.
/// Carries the fields the `review_interrupted` Tauri event payload needs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InterruptedJobInfo {
    pub job_id: String,
    pub review_id: Option<String>,
    pub task_id: Option<String>,
    pub num_rounds: i64,
    /// Previous (pre-sweep) status, e.g. `pending`, `running`, `round_2`.
    /// Used to derive the resume phase without a second query.
    pub previous_status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HivemindConfig {
    pub id: String,
    pub name: String,
    pub description: String,
    pub rounds_config: String,
    pub inherit_orchestrator: bool,
    pub orchestrator_model: Option<String>,
    pub orchestrator_provider: Option<String>,
    pub orchestrator_thinking: String,
    pub orchestrator_context_window: Option<i64>,
    pub orchestrator_max_output: Option<i64>,
    pub created_at: String,
    pub updated_at: String,
}

// ---------------------------------------------------------------------------
// HivemindStore — SQLite-backed persistence for hivemind jobs
// ---------------------------------------------------------------------------

/// Two-pool split:
/// - `writer` is a single-connection pool used for every `execute` and every
///   `begin()` transaction. SQLite serializes writers anyway, so capping at 1
///   removes pool-internal contention and avoids "database is locked" races
///   under WAL.
/// - `reader` is a 4-connection pool opened `read_only=true` and pinned to
///   `query_only=ON` so accidental writes against it fault loudly. Under WAL,
///   readers never block writers and writers never block readers — splitting
///   the pools means UI list queries no longer wait on a long-running write
///   transaction holding the (only) writer connection.
///
/// Both pools apply the same perf-tuning PRAGMAs at connect time (WAL,
/// foreign_keys=ON, synchronous=NORMAL, 64 MB cache, MEMORY temp_store,
/// 256 MB mmap).
#[derive(Debug, Clone)]
pub struct HivemindStore {
    /// Writer pool — `execute` and `begin()`. Must be max_connections=1.
    writer: SqlitePool,
    /// Reader pool — `fetch_one`, `fetch_optional`, `fetch_all`. Read-only.
    reader: SqlitePool,
}

#[derive(Debug, Clone)]
pub struct LogicalRunPage {
    pub run_ids: Vec<String>,
    pub jobs: Vec<Job>,
    pub model_counts_by_job_id: HashMap<String, i64>,
}

/// Apply the shared PRAGMA set to a `SqliteConnectOptions`. Returns the same
/// builder so callers can chain `.read_only(true)` etc. before connecting.
///
/// `journal_mode=WAL`, `foreign_keys=ON`, and `synchronous=NORMAL` are set
/// through the typed sqlx APIs (so sqlx tracks them in its connection-init
/// SQL). The rest (`cache_size`, `temp_store`, `mmap_size`, `query_only`)
/// are emitted as raw PRAGMA statements via `.pragma(name, value)` — sqlx
/// runs each statement once on every fresh connection.
fn apply_shared_pragmas(options: SqliteConnectOptions) -> SqliteConnectOptions {
    options
        .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
        .foreign_keys(true)
        .synchronous(sqlx::sqlite::SqliteSynchronous::Normal)
        // 64 MB per-connection page cache. Negative value = KiB (sqlite
        // convention: positive = pages, negative = bytes / 1024).
        .pragma("cache_size", "-65536")
        .pragma("temp_store", "MEMORY")
        // 256 MB memory-mapped I/O window for reads. Has no effect on
        // platforms where mmap is unavailable; harmless to set.
        .pragma("mmap_size", "268435456")
        .busy_timeout(Duration::from_secs(5))
}

impl HivemindStore {
    /// Open (or create) the SQLite database at `db_path`, configure pragmas,
    /// run embedded migrations, and split into reader + writer pools.
    pub async fn new(db_path: &Path) -> Result<Self> {
        // Ensure parent directory exists
        if let Some(parent) = db_path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .context("Failed to create database directory")?;
        }

        // Writer pool first — owns file creation and migrations. SQLite
        // serializes writers, so capping at 1 connection removes
        // pool-internal contention.
        let writer_options = apply_shared_pragmas(SqliteConnectOptions::new().filename(db_path))
            .create_if_missing(true);

        let writer = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(writer_options)
            .await
            .context("Failed to open SQLite writer pool")?;

        // Run embedded migrations through the writer pool (the only one
        // permitted to mutate schema).
        sqlx::migrate!("./migrations")
            .run(&writer)
            .await
            .context("Failed to run database migrations")?;

        // Reader pool second — file now exists, migrations applied. Opens
        // `read_only=true` at the OS-file-handle level, and pins
        // `query_only=ON` at the connection level so any accidental write
        // through this pool faults at runtime rather than silently
        // succeeding (matters if a refactor routes a write to the wrong
        // pool).
        let reader_options = apply_shared_pragmas(SqliteConnectOptions::new().filename(db_path))
            .read_only(true)
            .pragma("query_only", "ON");

        let reader = SqlitePoolOptions::new()
            .max_connections(4)
            .connect_with(reader_options)
            .await
            .context("Failed to open SQLite reader pool")?;

        info!(path = %db_path.display(), "HivemindStore initialized (reader+writer pools)");

        Ok(Self { writer, reader })
    }

    /// Shared writer pool. Routes every `execute` and `begin()` in this
    /// module. Kept `pub` because external callers (`UsageStore`, ad-hoc
    /// queries in `commands/`) currently share the same pool — they need a
    /// pool that supports both reads and writes, which only the writer
    /// satisfies (the reader rejects writes).
    pub fn pool(&self) -> &SqlitePool {
        &self.writer
    }

    pub async fn count_jobs(&self) -> anyhow::Result<i64> {
        let row = sqlx::query("SELECT COUNT(*) as cnt FROM jobs")
            .fetch_one(&self.reader)
            .await?;
        Ok(row.get::<i64, _>("cnt"))
    }

    /// Count logical review runs, grouping backend child jobs by non-empty review_id.
    pub async fn count_logical_runs(&self, hivemind_id: Option<&str>) -> Result<u32> {
        let row = sqlx::query(
            "SELECT COUNT(DISTINCT COALESCE(NULLIF(review_id, ''), id)) as cnt \
             FROM jobs WHERE (?1 IS NULL OR hivemind_id = ?1)",
        )
        .bind(hivemind_id)
        .fetch_one(&self.reader)
        .await
        .context("Failed to count logical runs")?;
        Ok(row.get::<i64, _>("cnt") as u32)
    }

    // ------------------------------------------------------------------
    // Job operations
    // ------------------------------------------------------------------

    /// Insert a new job row.
    pub async fn create_job(
        &self,
        id: &str,
        plan: &str,
        stance: &str,
        num_rounds: i64,
        timeout_seconds: i64,
        review_id: Option<&str>,
        hivemind_id: Option<&str>,
        name: Option<&str>,
        task_id: Option<&str>,
        project_path: Option<&str>,
    ) -> Result<()> {
        debug!(id = %id, "Creating job");

        sqlx::query(
            "INSERT INTO jobs (id, plan, name, stance, num_rounds, timeout_seconds, review_id, hivemind_id, task_id, project_path) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        )
        .bind(id)
        .bind(plan)
        .bind(name.unwrap_or(""))
        .bind(stance)
        .bind(num_rounds)
        .bind(timeout_seconds)
        .bind(review_id)
        .bind(hivemind_id)
        .bind(task_id)
        .bind(project_path)
        .execute(&self.writer)
        .await
        .context("Failed to create job")?;

        Ok(())
    }

    /// Update just the status column (and touch updated_at).
    pub async fn update_job_status(&self, id: &str, status: &str) -> Result<()> {
        debug!(id = %id, status = %status, "Updating job status");

        sqlx::query("UPDATE jobs SET status = ?1, updated_at = datetime('now') WHERE id = ?2")
            .bind(status)
            .bind(id)
            .execute(&self.writer)
            .await
            .context("Failed to update job status")?;

        Ok(())
    }

    /// Mark a job as successfully completed with its final output and totals.
    pub async fn complete_job(
        &self,
        id: &str,
        final_output: &str,
        total_cost: f64,
        total_input_tokens: i64,
        total_output_tokens: i64,
    ) -> Result<()> {
        debug!(id = %id, "Completing job");

        sqlx::query(
            "UPDATE jobs \
             SET status = 'completed', \
                 final_output = ?1, \
                 total_cost = ?2, \
                 total_input_tokens = ?3, \
                 total_output_tokens = ?4, \
                 completed_at = datetime('now'), \
                 updated_at = datetime('now') \
             WHERE id = ?5",
        )
        .bind(final_output)
        .bind(total_cost)
        .bind(total_input_tokens)
        .bind(total_output_tokens)
        .bind(id)
        .execute(&self.writer)
        .await
        .context("Failed to complete job")?;

        Ok(())
    }

    /// Mark a job as failed with an error message.
    pub async fn fail_job(&self, id: &str, error: &str) -> Result<()> {
        debug!(id = %id, "Failing job");

        sqlx::query(
            "UPDATE jobs \
             SET status = 'failed', \
                 error = ?1, \
                 completed_at = datetime('now'), \
                 updated_at = datetime('now') \
             WHERE id = ?2",
        )
        .bind(error)
        .bind(id)
        .execute(&self.writer)
        .await
        .context("Failed to mark job as failed")?;

        Ok(())
    }

    /// Fetch a single job by ID (returns `None` if not found).
    pub async fn get_job(&self, id: &str) -> Result<Option<Job>> {
        let row = sqlx::query(
            "SELECT id, plan, name, stance, status, num_rounds, current_round, \
                    timeout_seconds, created_at, updated_at, completed_at, \
                    error, final_output, total_cost, total_input_tokens, \
                    total_output_tokens, review_id, hivemind_id, task_id, project_path \
             FROM jobs WHERE id = ?1",
        )
        .bind(id)
        .fetch_optional(&self.reader)
        .await
        .context("Failed to fetch job")?;

        Ok(row.map(|r| row_to_job(&r)))
    }

    /// Count child jobs that belong to a logical review_id.
    pub async fn count_jobs_with_review_id(&self, review_id: &str) -> Result<i64> {
        let row = sqlx::query("SELECT COUNT(*) as cnt FROM jobs WHERE review_id = ?1")
            .bind(review_id)
            .fetch_one(&self.reader)
            .await
            .context("Failed to count jobs by review_id")?;
        Ok(row.get::<i64, _>("cnt"))
    }

    /// Fetch all child jobs for a logical review_id in deterministic execution order.
    pub async fn list_jobs_by_review_id(&self, review_id: &str) -> Result<Vec<Job>> {
        let rows = sqlx::query(
            "SELECT id, plan, name, stance, status, num_rounds, current_round, \
                    timeout_seconds, created_at, updated_at, completed_at, \
                    error, final_output, total_cost, total_input_tokens, \
                    total_output_tokens, review_id, hivemind_id, task_id, project_path \
             FROM jobs WHERE review_id = ?1 ORDER BY created_at ASC, id ASC",
        )
        .bind(review_id)
        .fetch_all(&self.reader)
        .await
        .context("Failed to list jobs by review_id")?;

        Ok(rows.iter().map(row_to_job).collect())
    }

    /// Build one page of logical runs in a single CTE query.
    ///
    /// Previously this issued 3 sequential queries inside a transaction:
    /// (1) page logical run IDs, (2) fetch all child jobs for those run IDs,
    /// (3) fetch distinct model counts per job. The three round-trips serialized
    /// against any other writer holding the SQLite write lock and dominated UI
    /// page-load latency.
    ///
    /// The single-CTE form does the page-of-run-ids selection inline (`page`),
    /// joins it back to `jobs` to materialize the child rows, and uses a
    /// correlated subquery on `job_steps` for the per-job model count — all in
    /// one round trip. The outer `ORDER BY` preserves the original sort
    /// (logical run order by `latest DESC, run_id ASC`, then by `created_at
    /// ASC, id ASC` within each run) so callers see byte-identical output.
    pub async fn list_logical_run_page(
        &self,
        hivemind_id: Option<&str>,
        limit: i64,
        offset: i64,
    ) -> Result<LogicalRunPage> {
        let rows = sqlx::query(
            "WITH page AS ( \
               SELECT COALESCE(NULLIF(review_id, ''), id) AS run_id, MAX(created_at) AS latest \
               FROM jobs \
               WHERE (?1 IS NULL OR hivemind_id = ?1) \
               GROUP BY run_id \
               ORDER BY latest DESC, run_id ASC \
               LIMIT ?2 OFFSET ?3 \
             ) \
             SELECT j.id, j.plan, j.name, j.stance, j.status, j.num_rounds, j.current_round, \
                    j.timeout_seconds, j.created_at, j.updated_at, j.completed_at, j.error, \
                    j.final_output, j.total_cost, j.total_input_tokens, j.total_output_tokens, \
                    j.review_id, j.hivemind_id, j.task_id, j.project_path, \
                    page.run_id AS page_run_id, page.latest AS page_latest, \
                    (SELECT COUNT(DISTINCT provider || '/' || model_id) \
                     FROM job_steps WHERE job_id = j.id) AS model_count \
             FROM jobs j \
             JOIN page ON COALESCE(NULLIF(j.review_id, ''), j.id) = page.run_id \
             ORDER BY COALESCE(NULLIF(j.review_id, ''), j.id) ASC, j.created_at ASC, j.id ASC",
        )
        .bind(hivemind_id)
        .bind(limit)
        .bind(offset)
        .fetch_all(&self.reader)
        .await
        .context("Failed to fetch logical run page (CTE)")?;

        // Materialize the three output shapes in a single pass.
        //
        // Legacy behavior to preserve:
        //   * `run_ids` is sorted by page order: `latest DESC, run_id ASC`.
        //     The CTE rows are sorted by run_id alphabetically (to match the
        //     legacy in-run job ordering), so we collect (run_id, latest) and
        //     sort separately.
        //   * `jobs` is sorted by `run_id ASC, created_at ASC, id ASC` —
        //     this is what the SQL `ORDER BY` already produced.
        //   * `model_counts_by_job_id` only contains entries for jobs that
        //     have at least one step (the legacy GROUP BY produces no row for
        //     jobs with zero steps, so they're absent from the map).
        let mut run_pairs: HashMap<String, String> = HashMap::new();
        let mut jobs: Vec<Job> = Vec::with_capacity(rows.len());
        let mut model_counts_by_job_id: HashMap<String, i64> = HashMap::new();

        for row in &rows {
            let run_id: String = row.get("page_run_id");
            let latest: String = row.get("page_latest");
            run_pairs.entry(run_id).or_insert(latest);
            let job = row_to_job(row);
            // model_count is non-NULL because COUNT() returns 0 for no rows.
            let model_count: i64 = row.get("model_count");
            if model_count > 0 {
                model_counts_by_job_id.insert(job.id.clone(), model_count);
            }
            jobs.push(job);
        }

        let mut run_ids: Vec<(String, String)> = run_pairs.into_iter().collect();
        // Page order: latest DESC, then run_id ASC (matches `list_logical_run_ids_tx`).
        run_ids.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        let run_ids: Vec<String> = run_ids.into_iter().map(|(id, _)| id).collect();

        Ok(LogicalRunPage {
            run_ids,
            jobs,
            model_counts_by_job_id,
        })
    }

    /// Fetch steps for many backend job IDs.
    pub async fn fetch_steps_for_jobs(&self, job_ids: &[String]) -> Result<Vec<JobStep>> {
        if job_ids.is_empty() {
            return Ok(vec![]);
        }
        let mut out = Vec::new();
        for chunk in job_ids.chunks(999) {
            let sql = format!(
                "SELECT id, job_id, round_number, sort_order, model_id, provider, stance, status, \
                        started_at, completed_at, input_tokens, output_tokens, cost, output, error, duration_ms, prompt \
                 FROM job_steps WHERE job_id IN ({}) ORDER BY job_id, round_number ASC, sort_order ASC, id ASC",
                repeat_vars(chunk.len())
            );
            let mut query = sqlx::query(&sql);
            for id in chunk {
                query = query.bind(id);
            }
            let rows = query
                .fetch_all(&self.reader)
                .await
                .context("Failed to fetch steps for jobs")?;
            out.extend(rows.iter().map(row_to_job_step));
        }
        Ok(out)
    }

    /// Fetch orchestrator round verdicts for many backend job IDs.
    pub async fn fetch_round_verdicts_for_jobs(
        &self,
        job_ids: &[String],
    ) -> Result<Vec<RoundVerdict>> {
        if job_ids.is_empty() {
            return Ok(vec![]);
        }
        let mut out = Vec::new();
        for chunk in job_ids.chunks(999) {
            let sql = format!(
                "SELECT id, job_id, round_number, reviewer_model, suggestion, verdict, severity, reason, created_at, best_find, co_reviewers \
                 FROM round_verdicts WHERE job_id IN ({}) \
                 ORDER BY job_id, round_number ASC, reviewer_model ASC, id ASC",
                repeat_vars(chunk.len())
            );
            let mut query = sqlx::query(&sql);
            for id in chunk {
                query = query.bind(id);
            }
            let rows = query
                .fetch_all(&self.reader)
                .await
                .context("Failed to fetch round verdicts for jobs")?;
            out.extend(rows.iter().map(row_to_round_verdict));
        }
        Ok(out)
    }

    // ------------------------------------------------------------------
    // JobStep operations
    // ------------------------------------------------------------------

    /// Insert a new job step row.
    pub async fn create_job_step(
        &self,
        id: &str,
        job_id: &str,
        round: i64,
        sort_order: i64,
        model_id: &str,
        provider: &str,
        stance: &str,
        prompt: &str,
    ) -> Result<()> {
        debug!(id = %id, job_id = %job_id, round = round, "Creating job step");

        sqlx::query(
            "INSERT INTO job_steps (id, job_id, round_number, sort_order, model_id, provider, stance, prompt) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        )
        .bind(id)
        .bind(job_id)
        .bind(round)
        .bind(sort_order)
        .bind(model_id)
        .bind(provider)
        .bind(stance)
        .bind(prompt)
        .execute(&self.writer)
        .await
        .context("Failed to create job step")?;

        Ok(())
    }

    /// Mark a step as running and record its start time.
    pub async fn update_job_step_started(&self, id: &str) -> Result<()> {
        debug!(id = %id, "Marking job step as started");

        sqlx::query(
            "UPDATE job_steps SET status = 'running', started_at = datetime('now') WHERE id = ?1",
        )
        .bind(id)
        .execute(&self.writer)
        .await
        .context("Failed to update job step started")?;

        Ok(())
    }

    /// Mark a step as completed with its results.
    pub async fn complete_job_step(
        &self,
        id: &str,
        output: &str,
        input_tokens: i64,
        output_tokens: i64,
        cost: f64,
        duration_ms: i64,
    ) -> Result<()> {
        debug!(id = %id, "Completing job step");

        sqlx::query(
            "UPDATE job_steps \
             SET status = 'completed', \
                 output = ?1, \
                 input_tokens = ?2, \
                 output_tokens = ?3, \
                 cost = ?4, \
                 duration_ms = ?5, \
                 completed_at = datetime('now') \
             WHERE id = ?6",
        )
        .bind(output)
        .bind(input_tokens)
        .bind(output_tokens)
        .bind(cost)
        .bind(duration_ms)
        .bind(id)
        .execute(&self.writer)
        .await
        .context("Failed to complete job step")?;

        Ok(())
    }

    /// Mark a step as failed with an error message.
    pub async fn fail_job_step(&self, id: &str, error: &str) -> Result<()> {
        debug!(id = %id, error = %error, "Failing job step");
        sqlx::query(
            "UPDATE job_steps \
             SET status = 'failed', \
                 error = ?1, \
                 completed_at = datetime('now') \
             WHERE id = ?2",
        )
        .bind(error)
        .bind(id)
        .execute(&self.writer)
        .await
        .context("Failed to fail job step")?;
        Ok(())
    }

    /// Fetch all steps for a given job, ordered by round then id.
    pub async fn get_job_steps(&self, job_id: &str) -> Result<Vec<JobStep>> {
        let rows = sqlx::query(
            "SELECT id, job_id, round_number, sort_order, model_id, provider, stance, \
                    status, started_at, completed_at, input_tokens, output_tokens, \
                    cost, output, error, duration_ms, prompt \
             FROM job_steps WHERE job_id = ?1 \
             ORDER BY round_number ASC, sort_order ASC, id ASC",
        )
        .bind(job_id)
        .fetch_all(&self.reader)
        .await
        .context("Failed to fetch job steps")?;

        Ok(rows.iter().map(row_to_job_step).collect())
    }

    // ------------------------------------------------------------------
    // Round verdict operations
    // ------------------------------------------------------------------

    /// Replace all verdicts for (job_id, round_number) with the given set.
    /// Idempotent: re-saving overwrites previous verdicts for that round.
    pub async fn save_round_verdicts(
        &self,
        job_id: &str,
        round_number: i64,
        verdicts: &[RoundVerdict],
    ) -> Result<()> {
        debug!(
            job_id = %job_id,
            round = round_number,
            count = verdicts.len(),
            "Saving round verdicts"
        );

        // `begin()` always uses the writer pool — the reader pool is
        // read-only and rejects writes anyway.
        let mut tx = self.writer.begin().await.context("Failed to begin tx")?;
        sqlx::query("DELETE FROM round_verdicts WHERE job_id = ?1 AND round_number = ?2")
            .bind(job_id)
            .bind(round_number)
            .execute(&mut *tx)
            .await
            .context("Failed to clear existing round verdicts")?;

        for v in verdicts {
            // Serialize non-empty co_reviewers to a JSON array string. Treat
            // an empty vec as NULL to keep the column tight when there are
            // no co-reviewers.
            let co_reviewers_json: Option<String> = match v.co_reviewers.as_ref() {
                Some(list) if !list.is_empty() => {
                    Some(serde_json::to_string(list).context("Failed to serialize co_reviewers")?)
                }
                _ => None,
            };
            sqlx::query(
                "INSERT INTO round_verdicts \
                 (id, job_id, round_number, reviewer_model, suggestion, verdict, severity, reason, created_at, best_find, co_reviewers) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            )
            .bind(&v.id)
            .bind(job_id)
            .bind(round_number)
            .bind(&v.reviewer_model)
            .bind(&v.suggestion)
            .bind(&v.verdict)
            .bind(v.severity)
            .bind(&v.reason)
            .bind(&v.created_at)
            .bind(if v.best_find { 1_i64 } else { 0_i64 })
            .bind(co_reviewers_json)
            .execute(&mut *tx)
            .await
            .context("Failed to insert round verdict")?;
        }

        tx.commit()
            .await
            .context("Failed to commit round verdicts")?;
        Ok(())
    }

    /// Fetch all verdicts for the given job, ordered by round then reviewer.
    pub async fn list_round_verdicts(&self, job_id: &str) -> Result<Vec<RoundVerdict>> {
        let rows = sqlx::query(
            "SELECT id, job_id, round_number, reviewer_model, suggestion, \
                    verdict, severity, reason, created_at, best_find, co_reviewers \
             FROM round_verdicts WHERE job_id = ?1 \
             ORDER BY round_number ASC, reviewer_model ASC, id ASC",
        )
        .bind(job_id)
        .fetch_all(&self.reader)
        .await
        .context("Failed to fetch round verdicts")?;

        Ok(rows.iter().map(row_to_round_verdict).collect())
    }

    // ------------------------------------------------------------------
    // Hivemind config operations
    // ------------------------------------------------------------------

    pub async fn create_hivemind(
        &self,
        id: &str,
        name: &str,
        description: &str,
        rounds_config: &str,
        inherit_orchestrator: bool,
        orchestrator_model: Option<&str>,
        orchestrator_provider: Option<&str>,
        orchestrator_thinking: &str,
        orchestrator_context_window: Option<i64>,
        orchestrator_max_output: Option<i64>,
    ) -> Result<()> {
        debug!(id = %id, name = %name, "Creating hivemind config");

        sqlx::query(
            "INSERT INTO hiveminds (id, name, description, rounds_config, \
             inherit_orchestrator, orchestrator_model, orchestrator_provider, orchestrator_thinking, \
             orchestrator_context_window, orchestrator_max_output) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        )
        .bind(id)
        .bind(name)
        .bind(description)
        .bind(rounds_config)
        .bind(inherit_orchestrator)
        .bind(orchestrator_model)
        .bind(orchestrator_provider)
        .bind(orchestrator_thinking)
        .bind(orchestrator_context_window)
        .bind(orchestrator_max_output)
        .execute(&self.writer)
        .await
        .context("Failed to create hivemind config")?;

        Ok(())
    }

    pub async fn get_hivemind(&self, id: &str) -> Result<Option<HivemindConfig>> {
        let row = sqlx::query(
            "SELECT id, name, description, rounds_config, \
                    inherit_orchestrator, orchestrator_model, orchestrator_provider, orchestrator_thinking, \
                    orchestrator_context_window, orchestrator_max_output, \
                    created_at, updated_at \
             FROM hiveminds WHERE id = ?1",
        )
        .bind(id)
        .fetch_optional(&self.reader)
        .await
        .context("Failed to fetch hivemind config")?;

        Ok(row.map(|r| row_to_hivemind_config(&r)))
    }

    pub async fn list_hiveminds(&self, limit: i64, offset: i64) -> Result<Vec<HivemindConfig>> {
        let rows = sqlx::query(
            "SELECT id, name, description, rounds_config, \
                    inherit_orchestrator, orchestrator_model, orchestrator_provider, orchestrator_thinking, \
                    orchestrator_context_window, orchestrator_max_output, \
                    created_at, updated_at \
             FROM hiveminds ORDER BY created_at DESC LIMIT ?1 OFFSET ?2",
        )
        .bind(limit)
        .bind(offset)
        .fetch_all(&self.reader)
        .await
        .context("Failed to list hivemind configs")?;

        Ok(rows.iter().map(row_to_hivemind_config).collect())
    }

    pub async fn update_hivemind(
        &self,
        id: &str,
        name: &str,
        description: &str,
        rounds_config: &str,
        inherit_orchestrator: bool,
        orchestrator_model: Option<&str>,
        orchestrator_provider: Option<&str>,
        orchestrator_thinking: &str,
        orchestrator_context_window: Option<i64>,
        orchestrator_max_output: Option<i64>,
    ) -> Result<()> {
        debug!(id = %id, "Updating hivemind config");

        sqlx::query(
            "UPDATE hiveminds \
             SET name = ?1, description = ?2, rounds_config = ?3, \
                 inherit_orchestrator = ?4, orchestrator_model = ?5, \
                 orchestrator_provider = ?6, orchestrator_thinking = ?7, \
                 orchestrator_context_window = ?8, orchestrator_max_output = ?9, \
                 updated_at = datetime('now') \
             WHERE id = ?10",
        )
        .bind(name)
        .bind(description)
        .bind(rounds_config)
        .bind(inherit_orchestrator)
        .bind(orchestrator_model)
        .bind(orchestrator_provider)
        .bind(orchestrator_thinking)
        .bind(orchestrator_context_window)
        .bind(orchestrator_max_output)
        .bind(id)
        .execute(&self.writer)
        .await
        .context("Failed to update hivemind config")?;

        Ok(())
    }

    pub async fn delete_hivemind(&self, id: &str) -> Result<()> {
        debug!(id = %id, "Deleting hivemind config");

        sqlx::query("DELETE FROM hiveminds WHERE id = ?1")
            .bind(id)
            .execute(&self.writer)
            .await
            .context("Failed to delete hivemind config")?;

        Ok(())
    }

    pub async fn count_hivemind_runs(&self, hivemind_id: &str) -> Result<i64> {
        Ok(self.count_logical_runs(Some(hivemind_id)).await? as i64)
    }

    // ------------------------------------------------------------------
    // Review context session operations
    // ------------------------------------------------------------------

    /// Upsert a review context session mapping. On conflict (same review_id),
    /// updates the session/model/provider but preserves created_at.
    pub async fn upsert_review_context_session(
        &self,
        review_id: &str,
        session_id: &str,
        model_id: &str,
        provider: &str,
    ) -> Result<()> {
        sqlx::query(
            "INSERT INTO review_context_sessions (review_id, session_id, model_id, provider) \
             VALUES (?1, ?2, ?3, ?4) \
             ON CONFLICT(review_id) DO UPDATE SET \
               session_id = excluded.session_id, \
               model_id = excluded.model_id, \
               provider = excluded.provider",
        )
        .bind(review_id)
        .bind(session_id)
        .bind(model_id)
        .bind(provider)
        .execute(&self.writer)
        .await
        .context("Failed to upsert review context session")?;
        Ok(())
    }

    /// Fetch the context session for a review.
    pub async fn get_review_context_session(
        &self,
        review_id: &str,
    ) -> Result<Option<ReviewContextSession>> {
        let row = sqlx::query(
            "SELECT review_id, session_id, model_id, provider, created_at \
             FROM review_context_sessions WHERE review_id = ?1",
        )
        .bind(review_id)
        .fetch_optional(&self.reader)
        .await
        .context("Failed to fetch review context session")?;

        Ok(row.map(|r| ReviewContextSession {
            review_id: r.get("review_id"),
            session_id: r.get("session_id"),
            model_id: r.get("model_id"),
            provider: r.get("provider"),
            created_at: r.get("created_at"),
        }))
    }

    /// Delete a review context session (for cleanup).
    pub async fn delete_review_context_session(&self, review_id: &str) -> Result<()> {
        sqlx::query("DELETE FROM review_context_sessions WHERE review_id = ?1")
            .bind(review_id)
            .execute(&self.writer)
            .await
            .context("Failed to delete review context session")?;
        Ok(())
    }

    /// List merge runs for a logical review ID (across all child jobs).
    pub async fn list_merge_runs_by_review_id(&self, review_id: &str) -> Result<Vec<MergeRunInfo>> {
        let rows = sqlx::query(
            "SELECT m.id, m.job_id, j.review_id, m.round_number, m.session_id, \
                    m.model_id, m.provider, m.thinking_level, m.status, \
                    m.started_at, m.completed_at, m.failed_at, m.error, \
                    m.output_path, m.output_len \
             FROM merge_runs m \
             INNER JOIN jobs j ON j.id = m.job_id \
             WHERE j.review_id = ?1 \
             ORDER BY m.round_number ASC",
        )
        .bind(review_id)
        .fetch_all(&self.reader)
        .await
        .context("Failed to list merge runs by review_id")?;

        Ok(rows.iter().map(row_to_merge_run).collect())
    }

    // ------------------------------------------------------------------
    // Merge run operations
    // ------------------------------------------------------------------

    /// Insert (or UPSERT) a merge run row for `(job_id, round_number)`.
    ///
    /// The unique index on `(job_id, round_number)` means a retry replaces
    /// the prior row: a fresh `id`, the new session/model/provider/thinking,
    /// status reset to `'running'`, completed_at/failed_at/error cleared,
    /// `output_len` reset to 0, and a new `output_path`.
    ///
    /// Returns the newly inserted (or replacing) row id.
    pub async fn insert_merge_run(
        &self,
        id: &str,
        job_id: &str,
        round_number: i64,
        session_id: &str,
        model_id: &str,
        provider: &str,
        thinking_level: &str,
        output_path: &str,
    ) -> Result<String> {
        debug!(
            id = %id,
            job_id = %job_id,
            round = round_number,
            "Inserting merge run (UPSERT on job_id+round)"
        );

        sqlx::query(
            "INSERT INTO merge_runs \
                 (id, job_id, round_number, session_id, model_id, provider, \
                  thinking_level, status, started_at, completed_at, failed_at, \
                  error, output_path, output_len) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'running', datetime('now'), \
                     NULL, NULL, NULL, ?8, 0) \
             ON CONFLICT(job_id, round_number) DO UPDATE SET \
                 id            = excluded.id, \
                 session_id    = excluded.session_id, \
                 model_id      = excluded.model_id, \
                 provider      = excluded.provider, \
                 thinking_level = excluded.thinking_level, \
                 status        = 'running', \
                 started_at    = datetime('now'), \
                 completed_at  = NULL, \
                 failed_at     = NULL, \
                 error         = NULL, \
                 output_path   = excluded.output_path, \
                 output_len    = 0",
        )
        .bind(id)
        .bind(job_id)
        .bind(round_number)
        .bind(session_id)
        .bind(model_id)
        .bind(provider)
        .bind(thinking_level)
        .bind(output_path)
        .execute(&self.writer)
        .await
        .context("Failed to insert merge run")?;

        Ok(id.to_string())
    }

    /// Mark a merge run terminal: `status` must be `'completed'` or
    /// `'failed'`. Sets `completed_at` or `failed_at` to `datetime('now')`
    /// accordingly and stores the final `output_len`.
    pub async fn complete_merge_run(
        &self,
        job_id: &str,
        round_number: i64,
        status: &str,
        error: Option<&str>,
        output_len: i64,
    ) -> Result<()> {
        debug!(
            job_id = %job_id,
            round = round_number,
            status = %status,
            output_len = output_len,
            "Completing merge run"
        );

        if status != "completed" && status != "failed" {
            anyhow::bail!(
                "complete_merge_run: status must be 'completed' or 'failed', got '{}'",
                status
            );
        }

        let (completed_at_expr, failed_at_expr) = if status == "completed" {
            ("datetime('now')", "NULL")
        } else {
            ("NULL", "datetime('now')")
        };

        let sql = format!(
            "UPDATE merge_runs \
             SET status = ?1, \
                 error = ?2, \
                 output_len = ?3, \
                 completed_at = {}, \
                 failed_at    = {} \
             WHERE job_id = ?4 AND round_number = ?5",
            completed_at_expr, failed_at_expr
        );

        sqlx::query(&sql)
            .bind(status)
            .bind(error)
            .bind(output_len)
            .bind(job_id)
            .bind(round_number)
            .execute(&self.writer)
            .await
            .context("Failed to complete merge run")?;

        Ok(())
    }

    /// Fetch a single merge run by `(job_id, round_number)`. The
    /// `review_id` field is populated via LEFT JOIN to the parent
    /// `jobs.review_id` so callers can build emit payloads without a
    /// follow-up query.
    pub async fn get_merge_run(
        &self,
        job_id: &str,
        round_number: i64,
    ) -> Result<Option<MergeRunInfo>> {
        let row = sqlx::query(
            "SELECT m.id, m.job_id, j.review_id, m.round_number, m.session_id, \
                    m.model_id, m.provider, m.thinking_level, m.status, \
                    m.started_at, m.completed_at, m.failed_at, m.error, \
                    m.output_path, m.output_len \
             FROM merge_runs m LEFT JOIN jobs j ON j.id = m.job_id \
             WHERE m.job_id = ?1 AND m.round_number = ?2",
        )
        .bind(job_id)
        .bind(round_number)
        .fetch_optional(&self.reader)
        .await
        .context("Failed to fetch merge run")?;

        Ok(row.map(|r| row_to_merge_run(&r)))
    }

    /// Fetch every merge_run row for `job_id`, ordered by `round_number`
    /// descending. Used by frontend rehydrate-recovery to find the latest
    /// merge attempt for a review whose in-memory orchestration was lost
    /// to a webview reload or app restart.
    pub async fn list_merge_runs_for_job(&self, job_id: &str) -> Result<Vec<MergeRunInfo>> {
        let rows = sqlx::query(
            "SELECT m.id, m.job_id, j.review_id, m.round_number, m.session_id, \
                    m.model_id, m.provider, m.thinking_level, m.status, \
                    m.started_at, m.completed_at, m.failed_at, m.error, \
                    m.output_path, m.output_len \
             FROM merge_runs m LEFT JOIN jobs j ON j.id = m.job_id \
             WHERE m.job_id = ?1 \
             ORDER BY m.round_number DESC",
        )
        .bind(job_id)
        .fetch_all(&self.reader)
        .await
        .context("Failed to list merge runs for job")?;

        Ok(rows.iter().map(row_to_merge_run).collect())
    }

    /// Sweep all `status='running'` merge_runs into `'interrupted'` and
    /// return the affected rows (with `review_id` populated via LEFT JOIN).
    ///
    /// Idempotent: subsequent calls find no rows because status is no
    /// longer `'running'`. Run once at app startup before the frontend
    /// can open any reviews.
    pub async fn sweep_interrupted_merges(&self) -> Result<Vec<MergeRunInfo>> {
        // Phase 1: snapshot the rows that will be flipped. We capture them
        // *before* the UPDATE so we can return their pre-UPDATE state with
        // the synthesised terminal fields populated by the same statement.
        // (sqlx-sqlite supports UPDATE…RETURNING, but the LEFT JOIN to
        // jobs makes a select-then-update simpler and just as correct given
        // this only runs once at startup.)
        let rows = sqlx::query(
            "SELECT m.id, m.job_id, j.review_id, m.round_number, m.session_id, \
                    m.model_id, m.provider, m.thinking_level, m.status, \
                    m.started_at, m.completed_at, m.failed_at, m.error, \
                    m.output_path, m.output_len \
             FROM merge_runs m LEFT JOIN jobs j ON j.id = m.job_id \
             WHERE m.status = 'running'",
        )
        .fetch_all(&self.reader)
        .await
        .context("Failed to select running merge runs for sweep")?;

        if rows.is_empty() {
            return Ok(vec![]);
        }

        let interrupted_msg = "host process restarted before merge completed";

        // Phase 2: flip them. Use a single statement to keep this atomic
        // from the perspective of the app.
        sqlx::query(
            "UPDATE merge_runs \
             SET status     = 'interrupted', \
                 failed_at  = datetime('now'), \
                 error      = ?1 \
             WHERE status = 'running'",
        )
        .bind(interrupted_msg)
        .execute(&self.writer)
        .await
        .context("Failed to mark running merge runs as interrupted")?;

        let now = chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();
        let out: Vec<MergeRunInfo> = rows
            .iter()
            .map(|r| {
                let mut m = row_to_merge_run(r);
                m.status = "interrupted".to_string();
                m.failed_at = Some(now.clone());
                m.error = Some(interrupted_msg.to_string());
                m
            })
            .collect();

        Ok(out)
    }

    /// Sweep `jobs` rows left in a non-terminal state from a prior process
    /// into `'interrupted'`. Returns one [`InterruptedJobInfo`] per affected
    /// row so the caller can emit a `review_interrupted` event per job.
    ///
    /// Targets `status='pending'`, `status='running'`, and any
    /// `status LIKE 'round_%'` (the engine's per-round status), but does
    /// **not** touch rows already terminal — `completed`, `cancelled`,
    /// `failed`, or a prior `interrupted`. Idempotent: a second call finds
    /// nothing.
    pub async fn sweep_interrupted_jobs(&self) -> Result<Vec<InterruptedJobInfo>> {
        let rows = sqlx::query(
            "SELECT id, review_id, task_id, num_rounds, status \
             FROM jobs \
             WHERE status = 'pending' OR status = 'running' OR status LIKE 'round_%'",
        )
        .fetch_all(&self.reader)
        .await
        .context("Failed to select non-terminal jobs for sweep")?;

        if rows.is_empty() {
            return Ok(vec![]);
        }

        let interrupted_msg = "host process restarted before review completed";

        sqlx::query(
            "UPDATE jobs \
             SET status      = 'interrupted', \
                 completed_at = datetime('now'), \
                 updated_at  = datetime('now'), \
                 error       = ?1 \
             WHERE status = 'pending' OR status = 'running' OR status LIKE 'round_%'",
        )
        .bind(interrupted_msg)
        .execute(&self.writer)
        .await
        .context("Failed to mark running jobs as interrupted")?;

        let out = rows
            .iter()
            .map(|r| InterruptedJobInfo {
                job_id: r.get::<String, _>("id"),
                review_id: r.try_get("review_id").ok().flatten(),
                task_id: r.try_get("task_id").ok().flatten(),
                num_rounds: r.get::<i64, _>("num_rounds"),
                previous_status: r.get::<String, _>("status"),
            })
            .collect();
        Ok(out)
    }

    /// Return the most recent job row for `task_id`. Used by
    /// `get_resumable_review_for_task` to anchor phase derivation.
    pub async fn latest_job_for_task(&self, task_id: &str) -> Result<Option<Job>> {
        let row = sqlx::query(
            "SELECT id, plan, name, stance, status, num_rounds, current_round, \
                    timeout_seconds, created_at, updated_at, completed_at, \
                    error, final_output, total_cost, total_input_tokens, \
                    total_output_tokens, review_id, hivemind_id, task_id, project_path \
             FROM jobs WHERE task_id = ?1 \
             ORDER BY datetime(created_at) DESC, id DESC LIMIT 1",
        )
        .bind(task_id)
        .fetch_optional(&self.reader)
        .await
        .context("Failed to fetch latest job for task")?;

        Ok(row.map(|r| row_to_job(&r)))
    }

    /// Return (hivemind_id, count) for every supplied hivemind ID that has at
    /// least one job row. IDs with zero jobs are absent from the result map.
    pub async fn batch_count_hivemind_runs(
        &self,
        hivemind_ids: &[String],
    ) -> Result<HashMap<String, i64>> {
        if hivemind_ids.is_empty() {
            return Ok(HashMap::new());
        }
        // Build `?1, ?2, …` placeholders dynamically.
        let placeholders: Vec<String> = (1..=hivemind_ids.len())
            .map(|i| format!("?{}", i))
            .collect();
        let sql = format!(
            "SELECT hivemind_id, COUNT(DISTINCT COALESCE(NULLIF(review_id, ''), id)) as cnt \
             FROM jobs WHERE hivemind_id IN ({}) GROUP BY hivemind_id",
            placeholders.join(", ")
        );
        let mut query = sqlx::query(&sql);
        for id in hivemind_ids {
            query = query.bind(id);
        }
        let rows = query.fetch_all(&self.reader).await?;
        let mut map = HashMap::new();
        for row in rows {
            map.insert(
                row.get::<String, _>("hivemind_id"),
                row.get::<i64, _>("cnt"),
            );
        }
        Ok(map)
    }

    /// Fetch all jobs whose logical run ID matches `run_id`.
    /// A logical run is identified by `COALESCE(NULLIF(review_id, ''), id)`.
    pub async fn list_jobs_by_logical_run_id(&self, run_id: &str) -> Result<Vec<Job>> {
        let rows = sqlx::query(
            "SELECT id, plan, name, stance, status, num_rounds, current_round, \
                    timeout_seconds, created_at, updated_at, completed_at, \
                    error, final_output, total_cost, total_input_tokens, \
                    total_output_tokens, review_id, hivemind_id, task_id, project_path \
             FROM jobs \
             WHERE COALESCE(NULLIF(review_id, ''), id) = ?1 \
             ORDER BY created_at ASC, id ASC",
        )
        .bind(run_id)
        .fetch_all(&self.reader)
        .await
        .context("Failed to list jobs by logical run ID")?;

        Ok(rows.iter().map(row_to_job).collect())
    }

    /// Return `true` if any job in the logical run has one of the given statuses.
    pub async fn any_job_in_status_for_logical_run(
        &self,
        run_id: &str,
        statuses: &[&str],
    ) -> Result<bool> {
        if statuses.is_empty() {
            return Ok(false);
        }
        // Build placeholders for the IN clause
        let placeholders: Vec<String> = (1..=statuses.len()).map(|i| format!("?{}", i)).collect();
        let sql = format!(
            "SELECT COUNT(*) as cnt FROM jobs \
             WHERE COALESCE(NULLIF(review_id, ''), id) = ?{} \
             AND status IN ({})",
            statuses.len() + 1,
            placeholders.join(", ")
        );
        let mut query = sqlx::query(&sql);
        for s in statuses {
            query = query.bind(s);
        }
        query = query.bind(run_id);
        let row = query
            .fetch_one(&self.reader)
            .await
            .context("Failed to check job status for logical run")?;
        let count: i64 = row.get("cnt");
        Ok(count > 0)
    }

    /// Delete a logical review run and all its associated data (jobs, steps,
    /// verdicts, merge runs, context sessions, and on-disk review logs/outputs).
    ///
    /// A logical run is identified by `COALESCE(NULLIF(review_id, ''), id)`.
    /// This handles both parent-only jobs (where id == run_id) and parent+child
    /// groups (where review_id == run_id).
    pub async fn delete_logical_run(&self, run_id: &str) -> Result<()> {
        // 1. Find all job rows belonging to this logical run
        let jobs = self.list_jobs_by_logical_run_id(run_id).await?;

        // 2. Determine the review_id for on-disk artifact paths
        let review_id = jobs
            .iter()
            .find_map(|j| j.review_id.as_deref().filter(|r| !r.is_empty()))
            .or(Some(run_id)); // standalone job: id == run_id

        // 3. Delete review_context_sessions row
        if let Some(rid) = review_id {
            self.delete_review_context_session(rid).await?;
        }

        // 4. Delete all job rows (CASCADE handles job_steps, round_verdicts, merge_runs)
        for job in &jobs {
            sqlx::query("DELETE FROM jobs WHERE id = ?1")
                .bind(&job.id)
                .execute(&self.writer)
                .await
                .context(format!("Failed to delete job {}", job.id))?;
        }

        Ok(())
    }
}

pub fn repeat_vars(n: usize) -> String {
    std::iter::repeat("?").take(n).collect::<Vec<_>>().join(",")
}

// ---------------------------------------------------------------------------
// Row mapping helpers
// ---------------------------------------------------------------------------

fn row_to_job(r: &sqlx::sqlite::SqliteRow) -> Job {
    Job {
        id: r.get("id"),
        plan: r.get("plan"),
        name: r.get("name"),
        stance: r.get("stance"),
        status: r.get("status"),
        num_rounds: r.get("num_rounds"),
        current_round: r.get("current_round"),
        timeout_seconds: r.get("timeout_seconds"),
        created_at: r.get("created_at"),
        updated_at: r.get("updated_at"),
        completed_at: r.get("completed_at"),
        error: r.get("error"),
        final_output: r.get("final_output"),
        total_cost: r.get("total_cost"),
        total_input_tokens: r.get("total_input_tokens"),
        total_output_tokens: r.get("total_output_tokens"),
        review_id: r.get("review_id"),
        hivemind_id: r.get("hivemind_id"),
        task_id: r.try_get("task_id").ok().flatten(),
        project_path: r.try_get("project_path").ok().flatten(),
    }
}

fn row_to_job_step(r: &sqlx::sqlite::SqliteRow) -> JobStep {
    JobStep {
        id: r.get("id"),
        job_id: r.get("job_id"),
        round_number: r.get("round_number"),
        sort_order: r.get("sort_order"),
        model_id: r.get("model_id"),
        provider: r.get("provider"),
        stance: r.get("stance"),
        status: r.get("status"),
        started_at: r.get("started_at"),
        completed_at: r.get("completed_at"),
        input_tokens: r.get("input_tokens"),
        output_tokens: r.get("output_tokens"),
        cost: r.get("cost"),
        output: r.get("output"),
        error: r.get("error"),
        duration_ms: r.get("duration_ms"),
        prompt: r.try_get("prompt").ok(),
    }
}

fn row_to_round_verdict(r: &sqlx::sqlite::SqliteRow) -> RoundVerdict {
    // Tolerate missing/legacy columns: best_find defaults to false, and a
    // malformed co_reviewers JSON string degrades to None rather than
    // crashing the whole list query.
    let best_find = r
        .try_get::<i64, _>("best_find")
        .map(|v| v != 0)
        .unwrap_or(false);
    let co_reviewers = r
        .try_get::<Option<String>, _>("co_reviewers")
        .ok()
        .flatten()
        .and_then(|s| serde_json::from_str::<Vec<String>>(&s).ok());
    RoundVerdict {
        id: r.get("id"),
        job_id: r.get("job_id"),
        round_number: r.get("round_number"),
        reviewer_model: r.get("reviewer_model"),
        suggestion: r.get("suggestion"),
        verdict: r.get("verdict"),
        severity: r.get("severity"),
        reason: r.get("reason"),
        created_at: r.get("created_at"),
        best_find,
        co_reviewers,
    }
}

fn row_to_hivemind_config(r: &sqlx::sqlite::SqliteRow) -> HivemindConfig {
    HivemindConfig {
        id: r.get("id"),
        name: r.get("name"),
        description: r.get("description"),
        rounds_config: r.get("rounds_config"),
        inherit_orchestrator: r.get::<bool, _>("inherit_orchestrator"),
        orchestrator_model: r.get("orchestrator_model"),
        orchestrator_provider: r.get("orchestrator_provider"),
        orchestrator_thinking: r.get("orchestrator_thinking"),
        orchestrator_context_window: r.get("orchestrator_context_window"),
        orchestrator_max_output: r.get("orchestrator_max_output"),
        created_at: r.get("created_at"),
        updated_at: r.get("updated_at"),
    }
}

fn row_to_merge_run(r: &sqlx::sqlite::SqliteRow) -> MergeRunInfo {
    MergeRunInfo {
        id: r.get("id"),
        job_id: r.get("job_id"),
        review_id: r.try_get("review_id").ok().flatten(),
        round_number: r.get("round_number"),
        session_id: r.get("session_id"),
        model_id: r.get("model_id"),
        provider: r.get("provider"),
        thinking_level: r.get("thinking_level"),
        status: r.get("status"),
        started_at: r.get("started_at"),
        completed_at: r.get("completed_at"),
        failed_at: r.get("failed_at"),
        error: r.get("error"),
        output_path: r.get("output_path"),
        output_len: r.get("output_len"),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// Helper: open a fresh in-memory-ish HivemindStore in a tempdir.
    async fn fresh_store() -> (HivemindStore, TempDir) {
        let tmp = TempDir::new().expect("tempdir");
        let db_path = tmp.path().join("hm.sqlite");
        let store = HivemindStore::new(&db_path).await.expect("open store");
        (store, tmp)
    }

    /// Helper: insert a minimal parent job row so foreign keys hold.
    async fn make_job(store: &HivemindStore, job_id: &str, review_id: Option<&str>) {
        store
            .create_job(
                job_id, "plan", "neutral", 2, 300, review_id, None, None, None, None,
            )
            .await
            .expect("create job");
    }

    /// Helper: insert a job with an explicit `task_id` for resume tests.
    async fn make_job_with_task(
        store: &HivemindStore,
        job_id: &str,
        task_id: &str,
        review_id: Option<&str>,
        num_rounds: i64,
    ) {
        store
            .create_job(
                job_id,
                "plan",
                "neutral",
                num_rounds,
                300,
                review_id,
                None,
                None,
                Some(task_id),
                None,
            )
            .await
            .expect("create job with task");
    }

    #[tokio::test]
    async fn test_merge_run_lifecycle() {
        let (store, _tmp) = fresh_store().await;
        make_job(&store, "job-1", Some("hmr-life")).await;

        let id = store
            .insert_merge_run(
                "mr-1",
                "job-1",
                1,
                "sess-1",
                "glm/glm-5.1",
                "openrouter",
                "high",
                "/tmp/merge-r1.txt",
            )
            .await
            .expect("insert");
        assert_eq!(id, "mr-1");

        let info = store
            .get_merge_run("job-1", 1)
            .await
            .expect("get")
            .expect("row exists");
        assert_eq!(info.status, "running");
        assert_eq!(info.review_id.as_deref(), Some("hmr-life"));
        assert_eq!(info.output_len, 0);
        assert!(info.completed_at.is_none());

        store
            .complete_merge_run("job-1", 1, "completed", None, 1234)
            .await
            .expect("complete");

        let after = store
            .get_merge_run("job-1", 1)
            .await
            .expect("get")
            .expect("row exists");
        assert_eq!(after.status, "completed");
        assert_eq!(after.output_len, 1234);
        assert!(after.completed_at.is_some());
        assert!(after.failed_at.is_none());
    }

    #[tokio::test]
    async fn test_merge_run_unique_per_round() {
        let (store, _tmp) = fresh_store().await;
        make_job(&store, "job-2", None).await;

        store
            .insert_merge_run(
                "mr-a",
                "job-2",
                1,
                "sess-old",
                "model-old",
                "openrouter",
                "high",
                "/tmp/old.txt",
            )
            .await
            .expect("insert 1");
        store
            .complete_merge_run("job-2", 1, "failed", Some("oops"), 50)
            .await
            .expect("fail");

        // Second insert for the same (job_id, round) UPSERTs.
        store
            .insert_merge_run(
                "mr-b",
                "job-2",
                1,
                "sess-new",
                "model-new",
                "anthropic",
                "low",
                "/tmp/new.txt",
            )
            .await
            .expect("upsert");

        let info = store
            .get_merge_run("job-2", 1)
            .await
            .expect("get")
            .expect("row");
        assert_eq!(info.id, "mr-b");
        assert_eq!(info.session_id, "sess-new");
        assert_eq!(info.model_id, "model-new");
        assert_eq!(info.provider, "anthropic");
        assert_eq!(info.thinking_level, "low");
        assert_eq!(info.status, "running");
        assert_eq!(info.output_len, 0);
        assert!(info.completed_at.is_none());
        assert!(info.failed_at.is_none());
        assert!(info.error.is_none());
        assert_eq!(info.output_path, "/tmp/new.txt");
    }

    #[tokio::test]
    async fn test_sweep_marks_running_as_interrupted() {
        let (store, _tmp) = fresh_store().await;
        make_job(&store, "job-3", None).await;
        make_job(&store, "job-4", None).await;

        store
            .insert_merge_run("mr-3", "job-3", 1, "s3", "m", "p", "high", "/tmp/3.txt")
            .await
            .expect("insert 3");
        store
            .insert_merge_run("mr-4", "job-4", 2, "s4", "m", "p", "high", "/tmp/4.txt")
            .await
            .expect("insert 4");

        let swept = store.sweep_interrupted_merges().await.expect("sweep");
        assert_eq!(swept.len(), 2);
        for m in &swept {
            assert_eq!(m.status, "interrupted");
            assert!(m.failed_at.is_some());
            assert_eq!(
                m.error.as_deref(),
                Some("host process restarted before merge completed")
            );
        }

        // Verify the rows in the DB match.
        for (job, round) in [("job-3", 1), ("job-4", 2)] {
            let row = store
                .get_merge_run(job, round)
                .await
                .expect("get")
                .expect("row");
            assert_eq!(row.status, "interrupted");
            assert!(row.failed_at.is_some());
        }

        // Idempotency: a second sweep finds nothing.
        let again = store.sweep_interrupted_merges().await.expect("sweep 2");
        assert!(again.is_empty());
    }

    #[tokio::test]
    async fn test_list_merge_runs_for_job_orders_by_round_desc() {
        let (store, _tmp) = fresh_store().await;
        make_job(&store, "job-list", Some("hmr-list")).await;

        store
            .insert_merge_run(
                "mr-r1",
                "job-list",
                1,
                "s1",
                "m",
                "p",
                "high",
                "/tmp/r1.txt",
            )
            .await
            .expect("insert r1");
        store
            .insert_merge_run(
                "mr-r2",
                "job-list",
                2,
                "s2",
                "m",
                "p",
                "high",
                "/tmp/r2.txt",
            )
            .await
            .expect("insert r2");
        store
            .complete_merge_run("job-list", 1, "completed", None, 100)
            .await
            .expect("complete r1");

        let runs = store
            .list_merge_runs_for_job("job-list")
            .await
            .expect("list");
        assert_eq!(runs.len(), 2);
        assert_eq!(runs[0].round_number, 2);
        assert_eq!(runs[0].status, "running");
        assert_eq!(runs[1].round_number, 1);
        assert_eq!(runs[1].status, "completed");
        assert_eq!(runs[0].review_id.as_deref(), Some("hmr-list"));

        let none = store
            .list_merge_runs_for_job("job-missing")
            .await
            .expect("list missing");
        assert!(none.is_empty());
    }

    #[tokio::test]
    async fn test_sweep_interrupted_jobs_flips_non_terminal_states() {
        let (store, _tmp) = fresh_store().await;

        // pending — default status from create_job
        make_job(&store, "job-pending", Some("hmr-a")).await;
        // round_2 — transition to mid-run
        make_job(&store, "job-mid-round", Some("hmr-b")).await;
        store
            .update_job_status("job-mid-round", "round_2")
            .await
            .unwrap();
        // running — alternate engine state
        make_job(&store, "job-running", None).await;
        store
            .update_job_status("job-running", "running")
            .await
            .unwrap();
        // completed — must NOT be touched
        make_job(&store, "job-done", Some("hmr-c")).await;
        store
            .complete_job("job-done", "final", 0.0, 0, 0)
            .await
            .unwrap();

        let swept = store.sweep_interrupted_jobs().await.expect("sweep");
        let mut ids: Vec<String> = swept.iter().map(|j| j.job_id.clone()).collect();
        ids.sort();
        assert_eq!(
            ids,
            vec![
                "job-mid-round".to_string(),
                "job-pending".to_string(),
                "job-running".to_string(),
            ]
        );

        // Verify the previous_status was captured per row.
        let by_id: HashMap<_, _> = swept.into_iter().map(|j| (j.job_id.clone(), j)).collect();
        assert_eq!(by_id["job-pending"].previous_status, "pending");
        assert_eq!(by_id["job-mid-round"].previous_status, "round_2");
        assert_eq!(by_id["job-running"].previous_status, "running");

        // The completed job is still completed.
        let done = store.get_job("job-done").await.unwrap().unwrap();
        assert_eq!(done.status, "completed");

        // Idempotency: a second sweep finds nothing.
        let again = store.sweep_interrupted_jobs().await.expect("second sweep");
        assert!(again.is_empty());

        // DB rows reflect the new status.
        let pending = store.get_job("job-pending").await.unwrap().unwrap();
        assert_eq!(pending.status, "interrupted");
        assert_eq!(
            pending.error.as_deref(),
            Some("host process restarted before review completed")
        );
    }

    #[tokio::test]
    async fn test_latest_job_for_task_returns_most_recent() {
        let (store, _tmp) = fresh_store().await;
        // No rows for this task yet.
        assert!(store
            .latest_job_for_task("task-abc")
            .await
            .unwrap()
            .is_none());

        make_job_with_task(&store, "job-1", "task-abc", Some("hmr-1"), 2).await;
        make_job_with_task(&store, "job-2", "task-abc", Some("hmr-2"), 1).await;
        // Drive the ORDER BY deterministically by stamping explicit
        // created_at values instead of sleeping past SQLite's 1s
        // datetime('now') resolution. This avoids a real-clock wait.
        sqlx::query("UPDATE jobs SET created_at = ?1 WHERE id = ?2")
            .bind("2024-01-01 00:00:00")
            .bind("job-1")
            .execute(store.pool())
            .await
            .expect("stamp job-1 created_at");
        sqlx::query("UPDATE jobs SET created_at = ?1 WHERE id = ?2")
            .bind("2024-01-01 00:00:02")
            .bind("job-2")
            .execute(store.pool())
            .await
            .expect("stamp job-2 created_at");

        let latest = store
            .latest_job_for_task("task-abc")
            .await
            .unwrap()
            .expect("row");
        assert_eq!(latest.id, "job-2");
        assert_eq!(latest.task_id.as_deref(), Some("task-abc"));
    }

    /// Sanity-check the CTE query plan uses the expected indexes (smoke test).
    /// We don't pin exact SQLite plan strings (they vary by version) — we just
    /// confirm the plan mentions both the jobs table and job_steps without a
    /// full SCAN of jobs when filtered. This is documented as the
    /// "EXPLAIN QUERY PLAN ad-hoc" check from the task brief.
    #[tokio::test]
    async fn test_list_logical_run_page_cte_query_plan() {
        let (store, _tmp) = fresh_store().await;
        // Seed a row so the planner has stats to chew on.
        store
            .create_job(
                "job-plan",
                "p",
                "neutral",
                1,
                60,
                Some("hmr-plan"),
                Some("hm-plan"),
                None,
                None,
                None,
            )
            .await
            .unwrap();

        // Use the same SQL as list_logical_run_page so any drift here trips
        // the parity test before this one anyway.
        let plan_rows = sqlx::query(
            "EXPLAIN QUERY PLAN \
             WITH page AS ( \
               SELECT COALESCE(NULLIF(review_id, ''), id) AS run_id, MAX(created_at) AS latest \
               FROM jobs WHERE (?1 IS NULL OR hivemind_id = ?1) \
               GROUP BY run_id ORDER BY latest DESC, run_id ASC LIMIT ?2 OFFSET ?3 \
             ) \
             SELECT j.id, \
                    (SELECT COUNT(DISTINCT provider || '/' || model_id) \
                     FROM job_steps WHERE job_id = j.id) AS model_count \
             FROM jobs j JOIN page ON COALESCE(NULLIF(j.review_id, ''), j.id) = page.run_id \
             ORDER BY page.latest DESC, page.run_id ASC",
        )
        .bind(Option::<&str>::None)
        .bind(50_i64)
        .bind(0_i64)
        .fetch_all(store.pool())
        .await
        .expect("explain query plan");
        let plan_text: Vec<String> = plan_rows
            .iter()
            .map(|r| r.get::<String, _>("detail"))
            .collect();
        // The plan must reference both tables.
        let joined = plan_text.join("\n");
        assert!(
            joined.contains("jobs") || joined.contains("JOBS"),
            "expected plan to reference jobs table; got:\n{joined}"
        );
        assert!(
            joined.contains("job_steps") || joined.contains("JOB_STEPS"),
            "expected plan to reference job_steps table; got:\n{joined}"
        );
        // The job_steps lookup should hit idx_job_steps_job_id, not scan.
        // Confirmed plan (sqlite ~3.4x):
        //   CO-ROUTINE page
        //   SCAN jobs USING INDEX idx_jobs_logical_run
        //   USE TEMP B-TREE FOR ORDER BY
        //   SCAN page
        //   SEARCH j USING INDEX idx_jobs_logical_run (<expr>=?)
        //   CORRELATED SCALAR SUBQUERY 2
        //   SEARCH job_steps USING INDEX idx_job_steps_job_id (job_id=?)
        assert!(
            joined.contains("idx_job_steps_job_id")
                || joined.contains("USING INDEX")
                || joined.contains("USING COVERING INDEX"),
            "expected job_steps subquery to use idx_job_steps_job_id; got:\n{joined}"
        );
    }

    #[tokio::test]
    async fn test_sweep_returns_review_id_for_emit() {
        let (store, _tmp) = fresh_store().await;
        make_job(&store, "job-5", Some("hmr-foo")).await;

        store
            .insert_merge_run(
                "mr-5",
                "job-5",
                3,
                "sess",
                "model",
                "provider",
                "medium",
                "/tmp/5.txt",
            )
            .await
            .expect("insert");

        let swept = store.sweep_interrupted_merges().await.expect("sweep");
        assert_eq!(swept.len(), 1);
        assert_eq!(swept[0].review_id.as_deref(), Some("hmr-foo"));
        assert_eq!(swept[0].job_id, "job-5");
        assert_eq!(swept[0].round_number, 3);
        assert_eq!(swept[0].model_id, "model");
    }
}
