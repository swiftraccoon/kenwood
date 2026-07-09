//! Tier 2: XLX live monitoring via UDP JSON protocol.
//!
//! Maintains concurrent UDP connections to active XLX reflectors on port 10001,
//! receiving real-time push notifications about connected nodes, heard stations,
//! and on-air/off-air events.
//!
//! The current monitor pool is eligibility-driven:
//!
//! - Reflectors detected as active by Tier 1 are connected (up to the
//!   configured maximum).
//! - Newly active reflectors are connected as Tier 1 detects them.
//! - A monitor ends after its 30-second receive deadline and may be selected
//!   again on a later refresh. Configured idle-pool eviction is not yet wired.
//!
//! Events are written to the `activity_log` and `connected_nodes` `PostgreSQL`
//! tables. On-air events are the planned Tier 3 promotion trigger; today they
//! are logged only.

mod monitor;
mod protocol;

use std::collections::HashMap;
use std::net::IpAddr;
use std::time::Duration;

use chrono::Utc;
use tokio::task::{Id, JoinSet};

use crate::config::Tier2Config;
use crate::db;

use self::monitor::XlxMonitor;
use self::protocol::MonitorMessage;

/// How often to re-query the database for newly active reflectors.
///
/// This is independent of the XLX monitor recv timeout; it controls how
/// quickly the orchestrator discovers reflectors that Tier 1 has flagged
/// as active since the last check.
const REFRESH_INTERVAL: Duration = Duration::from_secs(60);

/// Runs the Tier 2 XLX monitoring loop.
///
/// Spawns one tokio task per monitored reflector and runs until cancelled.
///
/// # Design
///
/// Each reflector is monitored by an independent [`monitor_loop`] task that
/// owns its own UDP socket. A reflector pushing a burst of events therefore
/// cannot delay another reflector's events, and a slow database write blocks
/// only the monitor that issued it. Tasks live in a [`JoinSet`]; when one
/// ends — the reflector went unresponsive, or (unexpectedly) the task
/// panicked — its pool slot is freed for the next refresh to reuse.
///
/// # Startup and refresh
///
/// On startup, and then every [`REFRESH_INTERVAL`], the orchestrator queries
/// the database for XLX reflectors with `tier2_available = true` and recent
/// `activity_log` activity, and spawns a monitor task for each not already
/// running — up to `max_concurrent_monitors`.
///
/// # Errors
///
/// Returns an error only on a fatal, non-retryable failure. The current
/// implementation runs indefinitely until the task is cancelled.
pub(crate) async fn run(
    config: Tier2Config,
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    tracing::info!(
        max_concurrent_monitors = config.max_concurrent_monitors,
        idle_disconnect_secs = config.idle_disconnect_secs,
        activity_threshold_secs = config.activity_threshold_secs,
        "tier2 XLX monitoring starting"
    );

    // One task per monitored reflector. `task_reflectors` maps each task's
    // id back to its reflector callsign so a finished or panicked task frees
    // the correct pool slot.
    let mut monitors: JoinSet<()> = JoinSet::new();
    let mut task_reflectors: HashMap<Id, String> = HashMap::new();
    let mut refresh = tokio::time::interval(REFRESH_INTERVAL);

    // Initial connect pass.
    spawn_eligible_monitors(&config, &pool, &mut monitors, &mut task_reflectors).await;
    tracing::info!(
        active_monitors = task_reflectors.len(),
        "tier2 initial monitor pool established"
    );

    loop {
        tokio::select! {
            // Refresh timer: spawn monitors for newly eligible reflectors.
            _ = refresh.tick() => {
                spawn_eligible_monitors(&config, &pool, &mut monitors, &mut task_reflectors).await;
            }

            // A monitor task finished. `join_next_with_id` yields `None` when
            // the set is empty, which disables this branch so `select!` just
            // waits for the next refresh.
            Some(joined) = monitors.join_next_with_id() => {
                let (id, panic) = match joined {
                    Ok((id, ())) => (id, None),
                    Err(e) => (e.id(), Some(e)),
                };
                if let Some(reflector) = task_reflectors.remove(&id) {
                    match panic {
                        None => tracing::info!(
                            reflector = %reflector,
                            "tier2 monitor stopped, slot freed"
                        ),
                        Some(e) => tracing::warn!(
                            reflector = %reflector,
                            error = %e,
                            "tier2 monitor task panicked, slot freed"
                        ),
                    }
                }
            }
        }
    }
}

/// Queries the database for tier2-eligible reflectors and spawns a monitor
/// task for any that are not already being monitored.
///
/// Respects the `max_concurrent_monitors` cap. Only spawns for reflectors
/// that have a valid IP address, `tier2_available = true`, and recent
/// activity. A reflector that already has a live task is skipped — xlxd's
/// JSON monitor is single-client per reflector, so a second socket would
/// fight the first for the feed.
async fn spawn_eligible_monitors(
    config: &Tier2Config,
    pool: &sqlx::PgPool,
    monitors: &mut JoinSet<()>,
    task_reflectors: &mut HashMap<Id, String>,
) {
    let since = Utc::now()
        - chrono::Duration::seconds(
            i64::try_from(config.activity_threshold_secs).unwrap_or(i64::MAX),
        );
    let limit = i64::try_from(config.max_concurrent_monitors).unwrap_or(i64::MAX);

    let reflectors = match db::reflectors::get_tier2_eligible(pool, since, limit).await {
        Ok(rows) => rows,
        Err(e) => {
            tracing::warn!(error = %e, "tier2: failed to query eligible reflectors");
            return;
        }
    };

    // Reflectors that already have a live monitor task. Owned strings, so the
    // set does not borrow `task_reflectors` — which `spawn` mutates below.
    let monitored: std::collections::HashSet<String> = task_reflectors.values().cloned().collect();

    for row in &reflectors {
        // Skip if already monitored.
        if monitored.contains(&row.callsign) {
            continue;
        }

        // Respect the concurrency cap.
        if task_reflectors.len() >= config.max_concurrent_monitors {
            break;
        }

        // Parse the IP address from the database.
        let Some(ip_str) = &row.ip_address else {
            tracing::debug!(
                reflector = %row.callsign,
                "tier2: skipping reflector with no IP address"
            );
            continue;
        };

        let ip: IpAddr = match ip_str.parse() {
            Ok(addr) => addr,
            Err(e) => {
                tracing::debug!(
                    reflector = %row.callsign,
                    ip = %ip_str,
                    error = %e,
                    "tier2: skipping reflector with unparseable IP"
                );
                continue;
            }
        };

        // Spawn an independent monitor task and remember which reflector it
        // belongs to so its slot can be freed when it ends.
        let reflector = row.callsign.clone();
        let handle = monitors.spawn(monitor_loop(reflector.clone(), ip, pool.clone()));
        let _prev = task_reflectors.insert(handle.id(), reflector);
    }
}

/// Per-reflector monitor task: connect, then receive and dispatch messages
/// until the reflector becomes unresponsive.
///
/// [`XlxMonitor::recv`] applies its own 30-second timeout and returns `None`
/// on timeout or socket error; the first `None` ends the loop. The task then
/// returns, the `XlxMonitor` is dropped (sending a best-effort `"bye"`), and
/// the orchestrator's next refresh re-spawns the reflector if it is still
/// eligible.
async fn monitor_loop(reflector: String, ip: IpAddr, pool: sqlx::PgPool) {
    let monitor = match XlxMonitor::connect(ip, reflector.clone()).await {
        Ok(mon) => {
            tracing::info!(
                reflector = %reflector,
                peer = %mon.peer(),
                "tier2 monitor connected"
            );
            mon
        }
        Err(e) => {
            tracing::warn!(
                reflector = %reflector,
                ip = %ip,
                error = %e,
                "tier2: failed to connect monitor"
            );
            return;
        }
    };

    while let Some(msg) = monitor.recv().await {
        handle_message(&reflector, &msg, &pool).await;
    }

    tracing::info!(reflector = %reflector, "tier2 monitor unresponsive, stopping");
}

/// Dispatches a parsed monitor message to the appropriate handler.
async fn handle_message(reflector: &str, msg: &MonitorMessage, pool: &sqlx::PgPool) {
    match msg {
        MonitorMessage::Reflector(info) => {
            tracing::info!(
                reflector = %reflector,
                reported_name = %info.reflector.trim(),
                module_count = info.modules.len(),
                "tier2: reflector info received"
            );
        }
        MonitorMessage::Nodes(nodes) => {
            handle_nodes_update(reflector, nodes, pool).await;
        }
        MonitorMessage::Stations(stations) => {
            handle_stations_update(reflector, stations, pool).await;
        }
        MonitorMessage::OnAir(callsign) => {
            // TODO: trigger point for Tier 3 auto-promotion. When a station
            // goes on-air, the orchestrator could signal the Tier 3 manager to
            // establish a full D-STAR connection for voice capture.
            tracing::info!(
                reflector = %reflector,
                callsign = %callsign.trim(),
                "tier2: station on-air"
            );
        }
        MonitorMessage::OffAir(callsign) => {
            tracing::info!(
                reflector = %reflector,
                callsign = %callsign.trim(),
                "tier2: station off-air"
            );
        }
        MonitorMessage::Unknown(raw) => {
            tracing::debug!(
                reflector = %reflector,
                raw_json = %raw,
                "tier2: unrecognized monitor message"
            );
        }
    }
}

/// Processes a nodes update: clears stale entries and upserts the fresh
/// snapshot, all in one transaction so a reader never sees an empty list.
async fn handle_nodes_update(reflector: &str, nodes: &[protocol::NodeInfo], pool: &sqlx::PgPool) {
    tracing::debug!(
        reflector = %reflector,
        node_count = nodes.len(),
        "tier2: nodes update"
    );

    // Clear-then-reinsert inside a transaction: on commit the swap is atomic,
    // so a concurrent reader sees either the old snapshot or the new one,
    // never the empty gap between the DELETE and the re-INSERTs. Any error
    // returns early, dropping `tx`, which rolls the whole update back.
    let mut tx = match pool.begin().await {
        Ok(tx) => tx,
        Err(e) => {
            tracing::warn!(
                reflector = %reflector,
                error = %e,
                "tier2: failed to begin nodes transaction"
            );
            return;
        }
    };

    if let Err(e) = db::connected_nodes::clear_for_reflector(&mut *tx, reflector).await {
        tracing::warn!(
            reflector = %reflector,
            error = %e,
            "tier2: failed to clear stale nodes"
        );
        return;
    }

    let now = Utc::now();
    for node in nodes {
        // Extract the module letter from the linkedto field.
        let module = if node.linkedto.is_empty() {
            None
        } else {
            Some(node.linkedto.as_str())
        };

        if let Err(e) =
            db::connected_nodes::upsert_node(&mut *tx, reflector, &node.callsign, module, now).await
        {
            tracing::warn!(
                reflector = %reflector,
                node = %node.callsign,
                error = %e,
                "tier2: failed to upsert connected node"
            );
            return;
        }
    }

    if let Err(e) = tx.commit().await {
        tracing::warn!(
            reflector = %reflector,
            error = %e,
            "tier2: failed to commit nodes update"
        );
    }
}

/// Processes a stations update: inserts each station as an activity observation.
async fn handle_stations_update(
    reflector: &str,
    stations: &[protocol::StationInfo],
    pool: &sqlx::PgPool,
) {
    tracing::debug!(
        reflector = %reflector,
        station_count = stations.len(),
        "tier2: stations update"
    );

    let now = Utc::now();
    for station in stations {
        let module = if station.module.is_empty() {
            None
        } else {
            Some(station.module.as_str())
        };

        if let Err(e) = db::activity::insert_observation(
            pool,
            reflector,
            module,
            station.callsign.trim(),
            "xlx_monitor",
            now,
        )
        .await
        {
            tracing::warn!(
                reflector = %reflector,
                station = %station.callsign,
                error = %e,
                "tier2: failed to insert station observation"
            );
        }
    }
}
