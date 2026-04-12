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
/// existence check.
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
    // ON CONFLICT UPDATE refreshes all mutable metadata columns and bumps
    // last_seen to the current time. The callsign (PK) and created_at are
    // left unchanged.
    let _result = sqlx::query(
        "INSERT INTO reflectors (callsign, protocol, ip_address, dashboard_url, country, last_seen)
         VALUES ($1, $2, $3::INET, $4, $5, now())
         ON CONFLICT (callsign) DO UPDATE SET
             protocol      = EXCLUDED.protocol,
             ip_address    = EXCLUDED.ip_address,
             dashboard_url = EXCLUDED.dashboard_url,
             country       = EXCLUDED.country,
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

/// Returns all reflectors that have been seen since the given timestamp.
///
/// Used by Tier 2 to select reflectors with recent activity for live
/// monitoring. Results are ordered by `last_seen DESC` so the most recently
/// active reflectors appear first.
///
/// # Errors
///
/// Returns `sqlx::Error` on query failure.
pub(crate) async fn get_active(
    pool: &PgPool,
    since: DateTime<Utc>,
) -> Result<Vec<ReflectorRow>, sqlx::Error> {
    // Filters on last_seen >= $1 and orders by recency.
    sqlx::query_as::<_, ReflectorRow>(
        "SELECT callsign, protocol, ip_address, dashboard_url, country,
                last_seen, tier2_available, created_at
         FROM reflectors
         WHERE last_seen >= $1
         ORDER BY last_seen DESC",
    )
    .bind(since)
    .fetch_all(pool)
    .await
}

/// Returns reflectors eligible for Tier 2 monitoring.
///
/// Filters for reflectors where `tier2_available = true` (supports the XLX UDP
/// JSON monitor protocol) AND `last_seen >= since` (has recent activity). The
/// additional `ip_address IS NOT NULL` check ensures we have a usable endpoint.
///
/// Results are ordered by `last_seen DESC` and capped at `limit` rows so the
/// orchestrator can respect its `max_concurrent_monitors` cap.
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
        "SELECT callsign, protocol, ip_address, dashboard_url, country,
                last_seen, tier2_available, created_at
         FROM reflectors
         WHERE tier2_available = true
           AND last_seen >= $1
           AND ip_address IS NOT NULL
         ORDER BY last_seen DESC
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
