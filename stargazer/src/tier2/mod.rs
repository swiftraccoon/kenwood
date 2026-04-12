//! Tier 2: XLX live monitoring via UDP JSON protocol.
//!
//! Maintains concurrent UDP connections to active XLX reflectors on port 10001,
//! receiving real-time push notifications about connected nodes, heard stations,
//! and on-air/off-air events.
//!
//! The monitor pool is activity-driven:
//!
//! - Reflectors detected as active by Tier 1 are connected (up to the
//!   configured maximum).
//! - Reflectors idle beyond the configured threshold are disconnected to free
//!   slots.
//! - Newly active reflectors are connected as Tier 1 detects them.
//!
//! Events are written to the `activity_log` and `connected_nodes` `PostgreSQL`
//! tables, and on-air events can trigger Tier 3 auto-promotion for voice
//! capture.

mod monitor;
mod protocol;

use std::collections::HashMap;
use std::net::IpAddr;
use std::time::Duration;

use chrono::Utc;

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
/// Manages a pool of UDP JSON monitor connections, connecting and disconnecting
/// based on Tier 1 activity data. Runs until cancelled.
///
/// # Startup behavior
///
/// On startup, queries the database for XLX reflectors with
/// `tier2_available = true` and recent activity (within `activity_threshold_secs`).
/// Connects to up to `max_concurrent_monitors` of the most recently active
/// reflectors.
///
/// # Main loop
///
/// The main loop uses `tokio::select!` to multiplex across:
///
/// 1. **Monitor recv**: each active monitor's `recv()` future is polled. When
///    a message arrives, it is dispatched by type:
///    - `Nodes`: upserts to `connected_nodes` table.
///    - `Stations`: inserts observations to `activity_log`.
///    - `OnAir`/`OffAir`: logged via tracing (potential Tier 3 trigger point).
///    - `Reflector`: logged once on connect, otherwise ignored.
///    - `Unknown`: logged at debug level for diagnostics.
///
/// 2. **Refresh timer**: every 60 seconds, re-queries the database for newly
///    eligible reflectors and connects any that are not already monitored.
///
/// # Error handling
///
/// Individual monitor failures (recv timeout, parse errors) are logged and
/// the monitor is removed from the pool. The orchestrator continues running
/// with the remaining monitors. Only a fatal error (e.g., database pool
/// closed) causes the function to return.
///
/// # Errors
///
/// Returns an error if a fatal, non-retryable failure occurs (e.g., the
/// database pool is closed or initial reflector query fails).
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

    let mut monitors: HashMap<String, XlxMonitor> = HashMap::new();
    let mut refresh_interval = tokio::time::interval(REFRESH_INTERVAL);

    // Initial connect pass: query eligible reflectors and connect monitors.
    connect_eligible_monitors(&config, &pool, &mut monitors).await;

    tracing::info!(
        active_monitors = monitors.len(),
        "tier2 initial monitor pool established"
    );

    // Main event loop: multiplex monitor recv and periodic refresh.
    loop {
        // If we have no active monitors, just wait for the refresh timer
        // to try connecting new ones.
        if monitors.is_empty() {
            let _tick = refresh_interval.tick().await;
            connect_eligible_monitors(&config, &pool, &mut monitors).await;
            continue;
        }

        // Poll all active monitors concurrently. We collect the callsigns
        // into a Vec first to avoid borrow conflicts with the HashMap.
        let callsigns: Vec<String> = monitors.keys().cloned().collect();

        tokio::select! {
            // Refresh timer: check for newly eligible reflectors.
            _ = refresh_interval.tick() => {
                connect_eligible_monitors(&config, &pool, &mut monitors).await;
            }

            // Monitor recv: process the first message from any monitor.
            // We use a helper that polls all monitors and returns the first
            // result along with the reflector callsign.
            result = poll_any_monitor(&callsigns, &monitors) => {
                let (callsign, message) = result;

                if let Some(msg) = message {
                    handle_message(&callsign, &msg, &pool).await;
                } else {
                    // Recv returned None — timeout or error. Remove the
                    // monitor so it can be reconnected on the next refresh.
                    tracing::info!(
                        reflector = %callsign,
                        "tier2 monitor unresponsive, removing from pool"
                    );
                    // Drop sends best-effort "bye".
                    let _removed = monitors.remove(&callsign);
                }
            }
        }
    }
}

/// Queries the database for tier2-eligible reflectors and connects monitors
/// for any that are not already in the pool.
///
/// Respects the `max_concurrent_monitors` cap. Only connects to reflectors
/// that have a valid IP address, `tier2_available = true`, and recent activity.
async fn connect_eligible_monitors(
    config: &Tier2Config,
    pool: &sqlx::PgPool,
    monitors: &mut HashMap<String, XlxMonitor>,
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

    for row in &reflectors {
        // Skip if already monitored.
        if monitors.contains_key(&row.callsign) {
            continue;
        }

        // Respect the concurrency cap.
        if monitors.len() >= config.max_concurrent_monitors {
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

        // Attempt to connect the monitor.
        match XlxMonitor::connect(ip, row.callsign.clone()).await {
            Ok(mon) => {
                tracing::info!(
                    reflector = %row.callsign,
                    peer = %mon.peer(),
                    "tier2 monitor connected"
                );
                let _prev = monitors.insert(row.callsign.clone(), mon);
            }
            Err(e) => {
                tracing::warn!(
                    reflector = %row.callsign,
                    ip = %ip,
                    error = %e,
                    "tier2: failed to connect monitor"
                );
            }
        }
    }
}

/// Polls all monitors via round-robin and returns the first message received
/// along with the reflector callsign that produced it.
///
/// Each monitor is given a 500ms window to produce a message. If no monitor
/// has data in the quick-poll pass, falls back to a full blocking recv on
/// the first monitor (which uses the standard 30-second timeout).
///
/// With up to 100 monitors, the round-robin worst case is 50 seconds, but in
/// practice monitors with pending data return immediately.
async fn poll_any_monitor(
    callsigns: &[String],
    monitors: &HashMap<String, XlxMonitor>,
) -> (String, Option<MonitorMessage>) {
    // Round-robin poll with 500ms per-monitor timeout. Most monitors with
    // pending data return immediately; the timeout only fires for idle ones.
    let poll_timeout = Duration::from_millis(500);

    for callsign in callsigns {
        if let Some(monitor) = monitors.get(callsign)
            && let Ok(msg) = tokio::time::timeout(poll_timeout, monitor.recv()).await
        {
            return (callsign.clone(), msg);
        }
        // This monitor had no data within the poll window — try the next.
    }

    // All monitors timed out in the quick-poll pass. Do a full blocking recv
    // on the first monitor (uses the standard 30-second timeout) to avoid
    // busy-spinning when all monitors are idle.
    if let Some(callsign) = callsigns.first()
        && let Some(monitor) = monitors.get(callsign)
    {
        let msg = monitor.recv().await;
        return (callsign.clone(), msg);
    }

    // Unreachable when callsigns is non-empty (caller checks monitors.is_empty()),
    // but we must return something for exhaustiveness.
    callsigns
        .first()
        .map_or_else(|| (String::new(), None), |cs| (cs.clone(), None))
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

/// Processes a nodes update: clears stale entries and upserts the fresh snapshot.
async fn handle_nodes_update(reflector: &str, nodes: &[protocol::NodeInfo], pool: &sqlx::PgPool) {
    tracing::debug!(
        reflector = %reflector,
        node_count = nodes.len(),
        "tier2: nodes update"
    );

    // Clear stale nodes for this reflector, then upsert the fresh snapshot.
    // This simple delete-then-reinsert avoids diff logic.
    if let Err(e) = db::connected_nodes::clear_for_reflector(pool, reflector).await {
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
            db::connected_nodes::upsert_node(pool, reflector, &node.callsign, module, now).await
        {
            tracing::warn!(
                reflector = %reflector,
                node = %node.callsign,
                error = %e,
                "tier2: failed to upsert connected node"
            );
        }
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
