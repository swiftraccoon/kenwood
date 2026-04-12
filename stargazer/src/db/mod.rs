//! Database connection pool, schema migrations, and typed query modules.
//!
//! This module manages the `PostgreSQL` connection pool via sqlx and provides
//! the schema migration entry point. Submodules contain typed query functions
//! for each table:
//!
//! | Module | Table | Purpose |
//! |---|---|---|
//! | [`reflectors`] | `reflectors` | Registry of discovered D-STAR reflectors. |
//! | [`activity`] | `activity_log` | Timestamped callsign observations. |
//! | [`connected_nodes`] | `connected_nodes` | Live snapshot of linked nodes per reflector. |
//! | [`streams`] | `streams` | Captured voice transmissions with audio. |
//! | [`uploads`] | `streams` | Upload queue lifecycle management. |
//!
//! The pool is shared across all tiers via `sqlx::PgPool`, which is cheaply
//! cloneable (it wraps an `Arc` internally).

pub(crate) mod activity;
pub(crate) mod connected_nodes;
mod migrations;
pub(crate) mod reflectors;
pub(crate) mod streams;
pub(crate) mod uploads;

use crate::config::PostgresConfig;

/// Creates and returns a `PostgreSQL` connection pool.
///
/// Connects to the database specified in `config.url` with the pool size
/// limited to `config.max_connections`.
///
/// # Errors
///
/// Returns `sqlx::Error` if the database is unreachable or the connection
/// string is malformed.
pub(crate) async fn connect(config: &PostgresConfig) -> Result<sqlx::PgPool, sqlx::Error> {
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(config.max_connections)
        .connect(&config.url)
        .await?;
    Ok(pool)
}

/// Runs embedded database migrations.
///
/// Executes idempotent `CREATE TABLE IF NOT EXISTS` and `CREATE INDEX IF NOT
/// EXISTS` statements to bring the schema up to date. Safe to call on every
/// application startup.
///
/// # Errors
///
/// Returns `sqlx::Error` if any migration statement fails.
pub(crate) async fn migrate(pool: &sqlx::PgPool) -> Result<(), sqlx::Error> {
    migrations::run(pool).await
}
