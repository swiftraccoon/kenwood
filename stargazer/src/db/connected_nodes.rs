//! Query functions for the `connected_nodes` table.
//!
//! The connected nodes table maintains a live snapshot of nodes (gateways and
//! hotspots) currently linked to each reflector module. It is updated by Tier 2
//! XLX monitors as they receive `"nodes"` push notifications.
//!
//! Unlike the append-only `activity_log`, this table uses `UPSERT` semantics:
//! each node can appear at most once per reflector (enforced by the composite
//! primary key `(reflector, node_callsign)`). When a monitor receives a fresh
//! node list, it upserts each entry and clears stale nodes that are no longer
//! present.
//!
//! # Staleness eviction
//!
//! [`clear_for_reflector`] deletes all nodes for a given reflector. This is
//! called before upserting the fresh snapshot so that nodes that have
//! disconnected since the last update are removed.

use chrono::{DateTime, Utc};
use sqlx::{PgExecutor, PgPool};

/// A single row from the `connected_nodes` table.
///
/// Maps directly to the table columns via `sqlx::FromRow`.
#[derive(Debug, sqlx::FromRow)]
pub(crate) struct ConnectedNodeRow {
    /// Reflector callsign (foreign key to `reflectors.callsign`).
    pub(crate) reflector: String,

    /// Node callsign (e.g. `"W1AW  B"`).
    pub(crate) node_callsign: String,

    /// Module letter on the reflector the node is linked to.
    pub(crate) module: Option<String>,

    /// Protocol used by the node (e.g. `"dextra"`, `"dplus"`).
    pub(crate) protocol: Option<String>,

    /// When the node first connected (as reported by the reflector).
    pub(crate) connected_since: Option<DateTime<Utc>>,

    /// When the node was last seen in a monitor update.
    pub(crate) last_heard: Option<DateTime<Utc>>,
}

/// Upserts a single connected node entry.
///
/// Inserts a new node, or updates an existing entry's `module` and
/// `last_heard` if the node is already tracked for this reflector. The
/// composite primary key `(reflector, node_callsign)` prevents duplicates.
///
/// The `connected_since` and `protocol` columns are never written — the XLX
/// monitor's node payload carries no machine-parseable connect time and no
/// per-node link protocol — so both remain `NULL`.
///
/// Accepts any sqlx executor (`&PgPool`, or a `&mut` transaction) so a caller
/// can group a clear-and-repopulate cycle into one transaction.
///
/// # Errors
///
/// Returns `sqlx::Error` on connection or foreign-key constraint failures
/// (the referenced reflector must exist in the `reflectors` table).
pub(crate) async fn upsert_node<'e, E>(
    executor: E,
    reflector: &str,
    node_callsign: &str,
    module: Option<&str>,
    now: DateTime<Utc>,
) -> Result<(), sqlx::Error>
where
    E: PgExecutor<'e>,
{
    let _result = sqlx::query(
        "INSERT INTO connected_nodes (reflector, node_callsign, module, last_heard)
         VALUES ($1, $2, $3, $4)
         ON CONFLICT (reflector, node_callsign) DO UPDATE SET
             module     = EXCLUDED.module,
             last_heard = EXCLUDED.last_heard",
    )
    .bind(reflector)
    .bind(node_callsign)
    .bind(module)
    .bind(now)
    .execute(executor)
    .await?;
    Ok(())
}

/// Deletes all connected node entries for a reflector.
///
/// Called before upserting a fresh node snapshot so that nodes that have
/// disconnected since the last update are removed. This simple
/// delete-then-reinsert pattern avoids the complexity of diff-based eviction.
///
/// Accepts any sqlx executor so the delete and the following upserts can run
/// in one transaction — a reader then never observes an empty node list.
///
/// # Errors
///
/// Returns `sqlx::Error` on query failure.
pub(crate) async fn clear_for_reflector<'e, E>(
    executor: E,
    reflector: &str,
) -> Result<(), sqlx::Error>
where
    E: PgExecutor<'e>,
{
    let _result = sqlx::query("DELETE FROM connected_nodes WHERE reflector = $1")
        .bind(reflector)
        .execute(executor)
        .await?;
    Ok(())
}

/// Returns all nodes currently linked to the given reflector.
///
/// Results are ordered by `last_heard DESC` (most recently active first),
/// with `NULLS LAST` so rows whose `last_heard` was not reported by the
/// monitor don't dominate the top of the list. Used by the HTTP API
/// (`GET /api/reflectors/{callsign}/nodes`) to expose the live node roster.
///
/// # Errors
///
/// Returns `sqlx::Error` on query failure.
pub(crate) async fn get_for_reflector(
    pool: &PgPool,
    reflector: &str,
) -> Result<Vec<ConnectedNodeRow>, sqlx::Error> {
    sqlx::query_as::<_, ConnectedNodeRow>(
        "SELECT reflector, node_callsign, module, protocol,
                connected_since, last_heard
         FROM connected_nodes
         WHERE reflector = $1
         ORDER BY last_heard DESC NULLS LAST",
    )
    .bind(reflector)
    .fetch_all(pool)
    .await
}
