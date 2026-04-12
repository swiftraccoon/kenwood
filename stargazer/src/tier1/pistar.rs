//! Pi-Star JSON host file fetcher.
//!
//! Fetches the canonical D-STAR reflector host list from
//! `http://www.pistar.uk/downloads/DStar_Hosts.json` and upserts each
//! reflector into the `reflectors` Postgres table.
//!
//! The Pi-Star host file is the most comprehensive public registry of D-STAR
//! reflectors, maintained by the Pi-Star project. It maps reflector callsigns
//! to IP addresses and categorises them by protocol type (REF/XRF/DCS).
//!
//! **Poll interval:** daily (default 86 400 s). The host file changes rarely —
//! new reflectors appear at most a few times per month.

use serde::Deserialize;

use super::error::FetchError;
use crate::db;

/// Pi-Star host file download URL.
const PISTAR_URL: &str = "http://www.pistar.uk/downloads/DStar_Hosts.json";

/// Top-level JSON envelope returned by the Pi-Star host endpoint.
///
/// The response contains a single `"reflectors"` array with one object per
/// reflector.
#[derive(Debug, Deserialize)]
struct PiStarResponse {
    /// Array of reflector entries.
    reflectors: Vec<PiStarReflector>,
}

/// A single reflector entry from the Pi-Star host file.
///
/// Each entry provides the reflector's callsign (name), protocol type, and
/// IPv4 address. There is no dashboard URL or country information in this
/// data source — those fields are left as `None` during upsert.
#[derive(Debug, Deserialize)]
struct PiStarReflector {
    /// Reflector callsign, e.g. `"REF001"`, `"XRF320"`, `"DCS001"`.
    name: String,

    /// Protocol type as labelled by Pi-Star: `"REF"`, `"XRF"`, or `"DCS"`.
    reflector_type: String,

    /// IPv4 address of the reflector.
    ipv4: String,
}

/// Maps Pi-Star's protocol type labels to the internal protocol identifiers
/// used in the `reflectors` table.
///
/// - `"REF"` → `"dplus"` (REF reflectors use the `DPlus` protocol)
/// - `"XRF"` → `"dextra"` (XRF reflectors use the `DExtra` protocol)
/// - `"DCS"` → `"dcs"` (DCS reflectors use the DCS protocol)
///
/// Returns `None` for unrecognised types so the caller can skip them.
fn map_protocol(reflector_type: &str) -> Option<&'static str> {
    match reflector_type {
        "REF" => Some("dplus"),
        "XRF" => Some("dextra"),
        "DCS" => Some("dcs"),
        _ => None,
    }
}

/// Fetches the Pi-Star host file and upserts all reflectors into Postgres.
///
/// Returns the number of reflectors successfully upserted. Reflectors with
/// unrecognised protocol types are skipped with a debug log.
///
/// # Errors
///
/// - [`FetchError::Http`] if the HTTP request fails or returns a non-2xx status.
/// - [`FetchError::Database`] if any database upsert fails. Note: a database
///   error on one row aborts the entire batch (fail-fast) — this is acceptable
///   because the next poll cycle will retry the full set.
pub(crate) async fn fetch_and_store(
    client: &reqwest::Client,
    pool: &sqlx::PgPool,
) -> Result<usize, FetchError> {
    // Fetch and deserialize the JSON host file.
    let response: PiStarResponse = client.get(PISTAR_URL).send().await?.json().await?;

    let mut count = 0usize;
    for entry in &response.reflectors {
        let Some(protocol) = map_protocol(&entry.reflector_type) else {
            tracing::debug!(
                name = %entry.name,
                reflector_type = %entry.reflector_type,
                "skipping reflector with unrecognised protocol type"
            );
            continue;
        };

        // Pi-Star provides IP and protocol but no dashboard URL or country.
        db::reflectors::upsert(
            pool,
            &entry.name,
            protocol,
            Some(entry.ipv4.as_str()),
            None, // dashboard_url — not available from Pi-Star
            None, // country — not available from Pi-Star
        )
        .await?;

        count += 1;
    }

    tracing::info!(count, "pi-star: upserted reflectors");
    Ok(count)
}
