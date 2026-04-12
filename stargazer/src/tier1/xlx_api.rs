//! XLX API XML reflector list fetcher.
//!
//! Fetches the live XLX reflector registry from
//! `http://xlxapi.rlx.lu/api.php?do=GetReflectorList` and upserts each
//! reflector into the `reflectors` Postgres table.
//!
//! The XLX API is the authoritative source for XLX-family reflectors. Unlike
//! the Pi-Star host file (which only provides IP addresses), this feed includes
//! dashboard URLs, uptime, country, and last-contact timestamps — making it the
//! richest Tier 1 data source.
//!
//! All XLX reflectors expose a UDP JSON monitor on port 10001, so this fetcher
//! sets `tier2_available = true` for every reflector it discovers, enabling
//! automatic Tier 2 promotion.
//!
//! **Poll interval:** every 10 minutes (default 600 s). The XLX network is
//! dynamic — reflectors come and go, and the API rate limit is generous enough
//! for this cadence.

use serde::Deserialize;

use super::error::FetchError;
use crate::db;

/// XLX API endpoint URL.
const XLX_API_URL: &str = "http://xlxapi.rlx.lu/api.php?do=GetReflectorList";

/// Top-level XML envelope: `<XLXAPI>`.
///
/// The XLX API wraps everything in an `<answer>` element inside the root.
#[derive(Debug, Deserialize)]
#[serde(rename = "XLXAPI")]
struct XlxApiResponse {
    /// The `<answer>` wrapper element.
    answer: XlxAnswer,
}

/// The `<answer>` element containing the reflector list.
#[derive(Debug, Deserialize)]
struct XlxAnswer {
    /// The `<reflectorlist>` element containing individual `<reflector>` entries.
    reflectorlist: XlxReflectorList,
}

/// The `<reflectorlist>` element — a wrapper around the reflector array.
///
/// Each child `<reflector>` element maps to one [`XlxReflector`] entry.
#[derive(Debug, Deserialize)]
struct XlxReflectorList {
    /// Individual reflector entries. Uses `#[serde(default)]` so an empty
    /// list deserializes to an empty `Vec` rather than failing.
    #[serde(default)]
    reflector: Vec<XlxReflector>,
}

/// A single `<reflector>` entry from the XLX API.
///
/// Only the fields needed for the reflector registry are deserialized; the
/// rest (uptime, lastcontact, comment) are ignored via `#[serde(default)]` on
/// the parent container and field-level defaults here.
#[derive(Debug, Deserialize)]
struct XlxReflector {
    /// Reflector callsign, e.g. `"XLX000"`, `"XLX320"`.
    name: String,

    /// Reflector IP address (the `<lastip>` element).
    #[serde(default)]
    lastip: String,

    /// URL of the reflector's web dashboard, if available.
    #[serde(default)]
    dashboardurl: Option<String>,

    /// Country or region string (e.g. `"USA - Florida"`).
    #[serde(default)]
    country: Option<String>,
}

/// Fetches the XLX reflector list and upserts all entries into Postgres.
///
/// Returns the number of reflectors successfully upserted. After upserting
/// each reflector, also sets `tier2_available = true` since all XLX reflectors
/// support the UDP JSON monitor protocol on port 10001.
///
/// # Errors
///
/// - [`FetchError::Http`] if the HTTP request fails.
/// - [`FetchError::Xml`] if the XML response cannot be deserialized.
/// - [`FetchError::Database`] if any database operation fails.
pub(crate) async fn fetch_and_store(
    client: &reqwest::Client,
    pool: &sqlx::PgPool,
) -> Result<usize, FetchError> {
    // Fetch raw XML text, then deserialize with quick-xml's serde support.
    let body = client.get(XLX_API_URL).send().await?.text().await?;
    let response: XlxApiResponse = quick_xml::de::from_str(&body)?;

    let mut count = 0usize;
    for entry in &response.answer.reflectorlist.reflector {
        // Determine IP: use the lastip field if non-empty.
        let ip = if entry.lastip.is_empty() {
            None
        } else {
            Some(entry.lastip.as_str())
        };

        // XLX reflectors use the DExtra protocol as their primary link protocol.
        db::reflectors::upsert(
            pool,
            &entry.name,
            "dextra",
            ip,
            entry.dashboardurl.as_deref(),
            entry.country.as_deref(),
        )
        .await?;

        // All XLX reflectors expose the UDP JSON monitor on port 10001.
        db::reflectors::set_tier2_available(pool, &entry.name, true).await?;

        count += 1;
    }

    tracing::info!(count, "xlx-api: upserted reflectors");
    Ok(count)
}
