//! Tier 1: discovery and sweep.
//!
//! Lightweight polling of public data sources to build a continuously updated
//! picture of which D-STAR reflectors exist and which are currently active.
//!
//! Three data sources are polled on independent intervals:
//!
//! - **Pi-Star hosts** (`DStar_Hosts.json`) — canonical list of reflector
//!   addresses, polled daily.
//! - **XLX API** (`xlxapi.rlx.lu`) — XML feed of XLX reflector status and
//!   connected nodes, polled every 10 minutes.
//! - **ircDDB last-heard** (`status.ircddb.net`) — HTML page of recent D-STAR
//!   activity across the network, scraped every 60 seconds.
//!
//! Discovered reflectors and activity observations are written to the
//! `reflectors` and `activity_log` `PostgreSQL` tables, which drive Tier 2
//! monitoring decisions.
//!
//! # Error handling
//!
//! Individual fetch failures are logged at `warn` level and retried on the next
//! interval tick. A single source going down does not affect the other two —
//! `tokio::select!` fires whichever timer expires next, regardless of prior
//! failures.

mod error;
mod ircddb;
mod pistar;
mod xlx_api;

use std::time::Duration;

use crate::config::Tier1Config;

/// Runs the Tier 1 discovery sweep loop.
///
/// Spawns three concurrent polling timers — one per data source — and runs
/// until the task is cancelled. Each timer fires independently at the interval
/// specified in `config`. When a timer fires, the corresponding fetcher runs
/// to completion; errors are logged but never propagated, so a transient
/// failure in one source does not block the others.
///
/// A shared `reqwest::Client` is used across all fetchers to benefit from
/// connection pooling and keep-alive.
///
/// # Errors
///
/// Returns an error only if all three fetchers encounter a non-retryable
/// condition simultaneously (currently unreachable — the function runs
/// indefinitely until cancelled).
pub(crate) async fn run(
    config: Tier1Config,
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    tracing::info!(
        pistar_interval_secs = config.pistar,
        xlx_api_interval_secs = config.xlx_api,
        ircddb_interval_secs = config.ircddb,
        "tier1 discovery sweep starting"
    );

    // Shared HTTP client with reasonable timeouts for public API polling.
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .user_agent("stargazer/0.1 (D-STAR observatory)")
        .build()
        .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> { Box::new(e) })?;

    // Independent interval timers for each data source. `tick()` fires
    // immediately on the first call, so each source is polled once at startup
    // before settling into the configured cadence.
    let mut pistar_interval = tokio::time::interval(Duration::from_secs(config.pistar));
    let mut xlx_interval = tokio::time::interval(Duration::from_secs(config.xlx_api));
    let mut ircddb_interval = tokio::time::interval(Duration::from_secs(config.ircddb));

    loop {
        tokio::select! {
            _ = pistar_interval.tick() => {
                match pistar::fetch_and_store(&client, &pool).await {
                    Ok(count) => {
                        tracing::debug!(count, "pi-star fetch completed");
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "pi-star fetch failed");
                    }
                }
            }
            _ = xlx_interval.tick() => {
                match xlx_api::fetch_and_store(&client, &pool).await {
                    Ok(count) => {
                        tracing::debug!(count, "xlx-api fetch completed");
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "xlx-api fetch failed");
                    }
                }
            }
            _ = ircddb_interval.tick() => {
                match ircddb::fetch_and_store(&client, &pool).await {
                    Ok(count) => {
                        tracing::debug!(count, "ircddb scrape completed");
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "ircddb scrape failed");
                    }
                }
            }
        }
    }
}
