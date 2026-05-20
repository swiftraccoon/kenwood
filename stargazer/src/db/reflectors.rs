//! Query functions for the `reflectors` table.
//!
//! The reflectors table is the central registry populated by Tier 1 discovery
//! sweeps. Each row represents a unique D-STAR reflector identified by its
//! callsign (e.g., `REF001`, `XLX320`, `DCS001`).
//!
//! Tier 1 fetchers call [`upsert`] after each sweep to insert newly-discovered
//! reflectors or update metadata (IP address, dashboard URL, last-seen time)
//! for existing ones. Tier 2 monitors query [`get_active`] to decide which
//! reflectors have recent activity and deserve live monitoring.

use chrono::{DateTime, Utc};
use sqlx::PgPool;

/// A single row from the `reflectors` table.
///
/// Maps directly to the table columns via `sqlx::FromRow`. All optional
/// columns use `Option<T>` so that partially-populated rows (e.g., reflectors
/// discovered from Pi-Star host files, which lack dashboard URLs) deserialize
/// cleanly.
#[derive(Debug, sqlx::FromRow)]
pub(crate) struct ReflectorRow {
    /// Reflector callsign (primary key), e.g. `"REF001"` or `"XLX320"`.
    pub(crate) callsign: String,

    /// Protocol family: `"dplus"`, `"dextra"`, or `"dcs"`.
    pub(crate) protocol: String,

    /// Reflector IP address (Postgres `INET` maps to `String` via sqlx).
    pub(crate) ip_address: Option<String>,

    /// URL of the reflector's web dashboard, if known.
    pub(crate) dashboard_url: Option<String>,

    /// Two-letter country code, if known.
    pub(crate) country: Option<String>,

    /// Timestamp of the most recent observation from any source.
    pub(crate) last_seen: Option<DateTime<Utc>>,

    /// Whether this reflector supports the XLX UDP JSON monitor protocol
    /// (port 10001), making it eligible for Tier 2 monitoring.
    pub(crate) tier2_available: Option<bool>,

    /// When this row was first inserted.
    pub(crate) created_at: Option<DateTime<Utc>>,
}

/// Inserts a new reflector or updates an existing one on callsign conflict.
///
/// Executes an `INSERT ... ON CONFLICT (callsign) DO UPDATE` so that repeated
/// discovery sweeps refresh metadata without requiring a separate
/// existence check. The nullable metadata columns (`ip_address`,
/// `dashboard_url`, `country`) are updated with `COALESCE`, so a caller that
/// passes `None` preserves whatever a richer source already stored instead of
/// erasing it — the ircDDB scraper, which has only a callsign, must not wipe
/// the IP that Pi-Star supplied.
///
/// # Errors
///
/// Returns `sqlx::Error` on connection or constraint failures.
pub(crate) async fn upsert(
    pool: &PgPool,
    callsign: &str,
    protocol: &str,
    ip_address: Option<&str>,
    dashboard_url: Option<&str>,
    country: Option<&str>,
) -> Result<(), sqlx::Error> {
    // On conflict, COALESCE(EXCLUDED.col, reflectors.col) keeps the stored
    // value whenever this caller supplied NULL — so a metadata-poor source
    // cannot erase data a richer source populated. A non-NULL value still
    // wins, so genuine changes propagate. `protocol` is overwritten
    // unconditionally: it is NOT NULL, and every source infers it identically
    // from the callsign prefix. callsign (PK) and created_at are never touched.
    let _result = sqlx::query(
        "INSERT INTO reflectors (callsign, protocol, ip_address, dashboard_url, country, last_seen)
         VALUES ($1, $2, $3::INET, $4, $5, now())
         ON CONFLICT (callsign) DO UPDATE SET
             protocol      = EXCLUDED.protocol,
             ip_address    = COALESCE(EXCLUDED.ip_address, reflectors.ip_address),
             dashboard_url = COALESCE(EXCLUDED.dashboard_url, reflectors.dashboard_url),
             country       = COALESCE(EXCLUDED.country, reflectors.country),
             last_seen     = now()",
    )
    .bind(callsign)
    .bind(protocol)
    .bind(ip_address)
    .bind(dashboard_url)
    .bind(country)
    .execute(pool)
    .await?;
    Ok(())
}

/// Returns reflectors with at least one activity observation since `since`.
///
/// Used by the HTTP API (`GET /api/reflectors`). A reflector that only
/// appears in directory sweeps but has never been heard does not qualify:
/// eligibility is driven by the `activity_log` table, not by `last_seen`
/// (which every discovery sweep bumps). Results are ordered most-recently-
/// active first.
///
/// # Errors
///
/// Returns `sqlx::Error` on query failure.
pub(crate) async fn get_active(
    pool: &PgPool,
    since: DateTime<Utc>,
) -> Result<Vec<ReflectorRow>, sqlx::Error> {
    // "Active" means a real heard-event, so join against the most recent
    // activity_log observation per reflector within the window rather than
    // trusting reflectors.last_seen (bumped by directory sweeps too).
    sqlx::query_as::<_, ReflectorRow>(
        "SELECT r.callsign, r.protocol, r.ip_address, r.dashboard_url,
                r.country, r.last_seen, r.tier2_available, r.created_at
         FROM reflectors r
         JOIN (
             SELECT reflector, MAX(observed_at) AS last_activity
             FROM activity_log
             WHERE observed_at >= $1
             GROUP BY reflector
         ) a ON a.reflector = r.callsign
         ORDER BY a.last_activity DESC",
    )
    .bind(since)
    .fetch_all(pool)
    .await
}

/// Returns reflectors eligible for Tier 2 monitoring.
///
/// A reflector qualifies when it supports the XLX UDP JSON monitor protocol
/// (`tier2_available = true`), has a usable endpoint (`ip_address IS NOT
/// NULL`), and has produced at least one `activity_log` observation since
/// `since` — real heard activity, not mere presence in a directory listing.
/// Every discovery sweep bumps `last_seen`, so that column cannot tell the
/// two apart; the `activity_log` join can.
///
/// Results are ordered most-recently-active first and capped at `limit` rows
/// so the orchestrator can respect its `max_concurrent_monitors` cap.
///
/// # Errors
///
/// Returns `sqlx::Error` on query failure.
pub(crate) async fn get_tier2_eligible(
    pool: &PgPool,
    since: DateTime<Utc>,
    limit: i64,
) -> Result<Vec<ReflectorRow>, sqlx::Error> {
    sqlx::query_as::<_, ReflectorRow>(
        "SELECT r.callsign, r.protocol, r.ip_address, r.dashboard_url,
                r.country, r.last_seen, r.tier2_available, r.created_at
         FROM reflectors r
         JOIN (
             SELECT reflector, MAX(observed_at) AS last_activity
             FROM activity_log
             WHERE observed_at >= $1
             GROUP BY reflector
         ) a ON a.reflector = r.callsign
         WHERE r.tier2_available = true
           AND r.ip_address IS NOT NULL
         ORDER BY a.last_activity DESC
         LIMIT $2",
    )
    .bind(since)
    .bind(limit)
    .fetch_all(pool)
    .await
}

/// Returns the total number of rows in the `reflectors` table.
///
/// Used by the HTTP API `/metrics` endpoint. This counts every known
/// reflector regardless of `last_seen`, giving a registry-size snapshot.
///
/// # Errors
///
/// Returns `sqlx::Error` on query failure.
pub(crate) async fn count_total(pool: &PgPool) -> Result<i64, sqlx::Error> {
    let row: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM reflectors")
        .fetch_one(pool)
        .await?;
    Ok(row.0)
}

/// Sets the `tier2_available` flag for a reflector.
///
/// Called by Tier 1 XLX API fetcher when it determines whether a reflector
/// supports the UDP JSON monitor protocol. The flag controls whether Tier 2
/// attempts to connect.
///
/// # Errors
///
/// Returns `sqlx::Error` on query failure.
pub(crate) async fn set_tier2_available(
    pool: &PgPool,
    callsign: &str,
    available: bool,
) -> Result<(), sqlx::Error> {
    // Simple UPDATE targeting a single row by primary key.
    let _result = sqlx::query("UPDATE reflectors SET tier2_available = $1 WHERE callsign = $2")
        .bind(available)
        .bind(callsign)
        .execute(pool)
        .await?;
    Ok(())
}
