//! Query functions for the `activity_log` table.
//!
//! The activity log records timestamped observations of callsign activity on
//! reflector modules. Each row represents one callsign heard at a specific
//! instant, tagged with the data source that reported it.
//!
//! Sources include:
//! - `"xlx_monitor"` — Tier 2 XLX UDP JSON monitor real-time events.
//! - `"ircddb"` — Tier 1 ircDDB last-heard page scrapes.
//! - `"pistar"` — Tier 1 Pi-Star host file activity hints.
//!
//! The table is append-only by design. Old rows are not updated; queries filter
//! by `observed_at` to select recent activity windows.

use chrono::{DateTime, Utc};
use sqlx::PgPool;

/// A single row from the `activity_log` table.
///
/// Maps directly to the table columns via `sqlx::FromRow`.
#[derive(Debug, sqlx::FromRow)]
pub(crate) struct ActivityRow {
    /// Auto-generated row identifier.
    ///
    /// Populated by `sqlx::FromRow` from the `BIGSERIAL` primary key and
    /// surfaced in `ActivityView` so clients can deduplicate rows across
    /// paged queries.
    pub(crate) id: i64,

    /// Reflector callsign (foreign key to `reflectors.callsign`).
    pub(crate) reflector: String,

    /// Module letter (A-Z), or `None` if the source did not specify.
    pub(crate) module: Option<String>,

    /// Callsign of the operator or node that was heard.
    pub(crate) callsign: String,

    /// Data source that produced this observation.
    pub(crate) source: String,

    /// When the activity was observed.
    pub(crate) observed_at: DateTime<Utc>,
}

/// Inserts a single activity observation.
///
/// Each call appends one row. Callers should batch observations where possible
/// to reduce round-trips, but individual inserts are acceptable for real-time
/// Tier 2 events.
///
/// # Errors
///
/// Returns `sqlx::Error` on connection or foreign-key constraint failures
/// (the referenced reflector must exist in the `reflectors` table).
pub(crate) async fn insert_observation(
    pool: &PgPool,
    reflector: &str,
    module: Option<&str>,
    callsign: &str,
    source: &str,
    observed_at: DateTime<Utc>,
) -> Result<(), sqlx::Error> {
    // Simple INSERT with all columns provided by the caller.
    let _result = sqlx::query(
        "INSERT INTO activity_log (reflector, module, callsign, source, observed_at)
         VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(reflector)
    .bind(module)
    .bind(callsign)
    .bind(source)
    .bind(observed_at)
    .execute(pool)
    .await?;
    Ok(())
}

/// Returns recent activity across all reflectors since the given timestamp.
///
/// Results are ordered by `observed_at DESC` (most recent first) and capped
/// at `limit` rows. Used by the HTTP API to serve the global activity feed
/// (`GET /api/activity`).
///
/// # Errors
///
/// Returns `sqlx::Error` on query failure.
pub(crate) async fn get_recent(
    pool: &PgPool,
    since: DateTime<Utc>,
    limit: i64,
) -> Result<Vec<ActivityRow>, sqlx::Error> {
    // Time-bounded query with row limit, ordered by recency.
    sqlx::query_as::<_, ActivityRow>(
        "SELECT id, reflector, module, callsign, source, observed_at
         FROM activity_log
         WHERE observed_at >= $1
         ORDER BY observed_at DESC
         LIMIT $2",
    )
    .bind(since)
    .bind(limit)
    .fetch_all(pool)
    .await
}

/// Returns activity for a specific reflector since the given timestamp.
///
/// Results are ordered by `observed_at DESC` (most recent first). Used by
/// the HTTP API and Tier 2 promotion logic to assess per-reflector activity
/// levels.
///
/// # Errors
///
/// Returns `sqlx::Error` on query failure.
pub(crate) async fn get_for_reflector(
    pool: &PgPool,
    reflector: &str,
    since: DateTime<Utc>,
) -> Result<Vec<ActivityRow>, sqlx::Error> {
    // Filters by reflector callsign and time window; uses the
    // idx_activity_log_lookup composite index (reflector, module, observed_at).
    sqlx::query_as::<_, ActivityRow>(
        "SELECT id, reflector, module, callsign, source, observed_at
         FROM activity_log
         WHERE reflector = $1 AND observed_at >= $2
         ORDER BY observed_at DESC",
    )
    .bind(reflector)
    .bind(since)
    .fetch_all(pool)
    .await
}
