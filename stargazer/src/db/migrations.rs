//! Embedded SQL schema migrations.
//!
//! All tables use `IF NOT EXISTS` for idempotent re-execution. The schema is
//! applied as a single transaction via [`run`] on every startup, so adding new
//! tables or indices is a no-op when they already exist.
//!
//! # Tables
//!
//! | Table | Purpose |
//! |---|---|
//! | `reflectors` | Registry of discovered D-STAR reflectors (Tier 1). |
//! | `activity_log` | Timestamped observations of callsign activity (Tier 2). |
//! | `connected_nodes` | Snapshot of nodes currently linked to a reflector. |
//! | `streams` | Captured voice transmissions with audio and metadata (Tier 3). |
//!
//! # Relationships
//!
//! `activity_log.reflector` and `connected_nodes.reflector` both reference
//! `reflectors.callsign` via foreign key. `streams.reflector` is intentionally
//! **not** foreign-keyed so that streams captured from transiently-seen
//! reflectors survive registry cleanup.

use sqlx::PgPool;

/// SQL statements executed in order to bring the schema up to date.
///
/// Each statement is idempotent (`IF NOT EXISTS`), so running this on an
/// already-migrated database is safe and fast.
const SCHEMA_SQL: &str = r"
-- Reflector registry populated by Tier 1 discovery sweeps.
-- Primary key is the reflector callsign (e.g. 'REF001', 'XLX001', 'DCS001').
CREATE TABLE IF NOT EXISTS reflectors (
    callsign        TEXT PRIMARY KEY,
    protocol        TEXT NOT NULL,
    ip_address      INET,
    dashboard_url   TEXT,
    country         TEXT,
    last_seen       TIMESTAMPTZ,
    tier2_available BOOLEAN DEFAULT FALSE,
    created_at      TIMESTAMPTZ DEFAULT now()
);

-- Timestamped callsign observations from Tier 2 XLX monitors or Tier 1
-- scrapes. Each row records one callsign heard on a reflector module at a
-- specific instant, with 'source' indicating the origin (e.g. 'xlx_monitor',
-- 'ircddb', 'pistar').
CREATE TABLE IF NOT EXISTS activity_log (
    id          BIGSERIAL PRIMARY KEY,
    reflector   TEXT NOT NULL REFERENCES reflectors(callsign),
    module      CHAR(1),
    callsign    TEXT NOT NULL,
    source      TEXT NOT NULL,
    observed_at TIMESTAMPTZ NOT NULL
);
-- Composite index for the common query pattern: activity on a specific
-- reflector/module within a time window.
CREATE INDEX IF NOT EXISTS idx_activity_log_lookup
    ON activity_log (reflector, module, observed_at);

-- Snapshot of nodes currently linked to each reflector module. Updated by
-- Tier 2 monitors; stale entries are evicted when a monitor reconnects.
-- The composite primary key (reflector + node_callsign) means each node can
-- only appear once per reflector.
CREATE TABLE IF NOT EXISTS connected_nodes (
    reflector       TEXT NOT NULL REFERENCES reflectors(callsign),
    node_callsign   TEXT NOT NULL,
    module          CHAR(1),
    protocol        TEXT,
    connected_since TIMESTAMPTZ,
    last_heard      TIMESTAMPTZ,
    PRIMARY KEY (reflector, node_callsign)
);

-- Voice transmissions captured by Tier 3 deep connections. Each row is one
-- D-STAR voice stream (header + N voice frames + optional EOT). The audio_mp3
-- column holds the decoded PCM->MP3 blob once encoding completes.
-- upload_status tracks the Rdio upload lifecycle: pending -> uploaded | failed.
CREATE TABLE IF NOT EXISTS streams (
    id              BIGSERIAL PRIMARY KEY,
    reflector       TEXT NOT NULL,
    module          CHAR(1) NOT NULL,
    protocol        TEXT NOT NULL,
    stream_id       INTEGER NOT NULL,
    callsign        TEXT NOT NULL,
    suffix          TEXT,
    ur_call         TEXT,
    dstar_text      TEXT,
    dprs_lat        DOUBLE PRECISION,
    dprs_lon        DOUBLE PRECISION,
    started_at      TIMESTAMPTZ NOT NULL,
    ended_at        TIMESTAMPTZ,
    frame_count     INTEGER DEFAULT 0,
    audio_mp3       BYTEA,
    upload_status   TEXT DEFAULT 'pending',
    upload_attempts INTEGER DEFAULT 0,
    last_upload_error TEXT,
    uploaded_at     TIMESTAMPTZ,
    created_at      TIMESTAMPTZ DEFAULT now()
);
-- Index for the upload processor to find pending streams efficiently.
CREATE INDEX IF NOT EXISTS idx_streams_upload
    ON streams (upload_status);
-- Composite index for querying streams by reflector, module, and time.
CREATE INDEX IF NOT EXISTS idx_streams_lookup
    ON streams (reflector, module, started_at);
";

/// Executes the embedded schema DDL against the database.
///
/// All statements use `IF NOT EXISTS`, so this is safe to call on every
/// application startup. Runs as a single implicit transaction via
/// `sqlx::raw_sql`.
///
/// # Errors
///
/// Returns `sqlx::Error` if any DDL statement fails (e.g., permission denied,
/// type mismatch with an existing column).
pub(crate) async fn run(pool: &PgPool) -> Result<(), sqlx::Error> {
    let _result = sqlx::raw_sql(SCHEMA_SQL).execute(pool).await?;
    tracing::info!("schema migrations applied successfully");
    Ok(())
}
