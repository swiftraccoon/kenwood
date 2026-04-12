//! ircDDB last-heard HTML scraper.
//!
//! Scrapes `https://status.ircddb.net/cgi-bin/ircddb-log?30 0` — an HTML page
//! listing recent D-STAR activity across the global ircDDB network. Each table
//! row represents one heard transmission, including the operator callsign,
//! repeater routing fields, and timestamp.
//!
//! Unlike the Pi-Star and XLX API fetchers which primarily discover
//! *reflectors*, the ircDDB scraper discovers *activity*: which callsigns are
//! transmitting through which reflectors. This activity data drives Tier 2
//! monitoring decisions — reflectors with recent ircDDB activity are prioritised
//! for live monitoring.
//!
//! **Poll interval:** every 60 seconds (default). The ircDDB last-heard page
//! refreshes frequently, and activity data is time-sensitive — stale
//! observations lose value quickly for prioritisation.
//!
//! # HTML Structure
//!
//! The page contains an HTML `<table>` where each `<tr>` after the header row
//! has columns:
//!
//! | Index | Column | Example |
//! |-------|--------|---------|
//! | 0 | Date/time (UTC) | `2024-01-15 14:30:00` |
//! | 1 | Callsign | `W1AW` |
//! | 2 | ID (suffix) | `D75` |
//! | 3 | Rptr1 | `W1AW  B` |
//! | 4 | Rptr2 | `REF001 B` |
//! | 5 | `UrCall` | `CQCQCQ` |
//! | 6 | Dest Rptr | `REF001 B` |
//! | 7 | TX-Message | `Hello` |
//!
//! The Rptr2 and Dest Rptr fields contain the reflector callsign (first 6-7
//! characters) and module letter. We extract the reflector callsign from the
//! Dest Rptr field (column 6) which indicates the intended destination.

use chrono::Utc;
use scraper::{Html, Selector};

use super::error::FetchError;
use crate::db;

/// ircDDB last-heard page URL.
///
/// The `30 0` suffix requests the last 30 minutes of activity starting from
/// offset 0.
const IRCDDB_URL: &str = "https://status.ircddb.net/cgi-bin/ircddb-log?30%200";

/// Minimum number of columns expected in each data row.
///
/// Rows with fewer columns are skipped — they are likely header rows, separator
/// rows, or malformed entries.
const MIN_COLUMNS: usize = 7;

/// A parsed activity observation extracted from one HTML table row.
///
/// All HTML parsing happens synchronously (no `.await`) to avoid holding
/// non-`Send` scraper types across await points. The observations are collected
/// into a `Vec` first, then written to the database in a second pass.
struct Observation {
    /// Operator callsign from column 1.
    callsign: String,
    /// Reflector callsign extracted from the Dest Rptr field (column 6).
    reflector: String,
    /// Module letter (A-Z) from the Dest Rptr field, if present.
    module: Option<String>,
    /// Protocol inferred from the reflector callsign prefix.
    protocol: &'static str,
}

/// Fetches the ircDDB last-heard page and inserts activity observations.
///
/// Returns the number of observations successfully inserted. Rows that cannot
/// be parsed (missing columns, empty callsign, unrecognisable reflector) are
/// skipped with a debug log rather than failing the entire scrape.
///
/// # Errors
///
/// - [`FetchError::Http`] if the HTTP request fails.
/// - [`FetchError::Html`] if the page contains no recognisable `<table>`.
/// - [`FetchError::Database`] if a database insert fails.
///
/// # HTML parsing notes
///
/// The scraper is written against the expected standard `<table>/<tr>/<td>`
/// structure. If the ircDDB site changes its layout, this function will return
/// 0 observations (not an error) and log a warning — the data simply becomes
/// stale until the scraper is updated.
pub(crate) async fn fetch_and_store(
    client: &reqwest::Client,
    pool: &sqlx::PgPool,
) -> Result<usize, FetchError> {
    let body = client.get(IRCDDB_URL).send().await?.text().await?;

    // Phase 1: parse HTML synchronously — scraper types are not Send, so all
    // DOM traversal must complete before the first await point after this block.
    let observations = parse_observations(&body)?;

    // Phase 2: write parsed observations to the database.
    let now = Utc::now();
    let mut count = 0usize;

    for obs in &observations {
        // The activity_log table has a foreign key to reflectors.callsign, so
        // we upsert a minimal reflector entry first to satisfy the constraint.
        db::reflectors::upsert(pool, &obs.reflector, obs.protocol, None, None, None).await?;

        db::activity::insert_observation(
            pool,
            &obs.reflector,
            obs.module.as_deref(),
            &obs.callsign,
            "ircddb",
            now,
        )
        .await?;

        count += 1;
    }

    if count == 0 {
        tracing::warn!("ircddb: scraped 0 activity observations — page layout may have changed");
    } else {
        tracing::info!(count, "ircddb: inserted activity observations");
    }

    Ok(count)
}

/// Parses the HTML body into a vector of owned [`Observation`] values.
///
/// All DOM traversal happens here, synchronously, so that the non-`Send`
/// scraper types do not live across any `.await` boundaries.
fn parse_observations(body: &str) -> Result<Vec<Observation>, FetchError> {
    let document = Html::parse_document(body);

    // Build CSS selectors for the table structure.
    let table_sel =
        Selector::parse("table").map_err(|e| FetchError::Html(format!("bad selector: {e}")))?;
    let row_sel =
        Selector::parse("tr").map_err(|e| FetchError::Html(format!("bad selector: {e}")))?;
    let cell_sel =
        Selector::parse("td").map_err(|e| FetchError::Html(format!("bad selector: {e}")))?;

    // Find the first <table> on the page — the ircDDB log is the primary table.
    let table = document
        .select(&table_sel)
        .next()
        .ok_or_else(|| FetchError::Html("no <table> element found on page".to_owned()))?;

    let mut observations = Vec::new();

    for row in table.select(&row_sel) {
        let cells: Vec<String> = row
            .select(&cell_sel)
            .map(|td| td.text().collect::<String>().trim().to_owned())
            .collect();

        // Skip header rows and malformed rows with insufficient columns.
        if cells.len() < MIN_COLUMNS {
            continue;
        }

        // Column 1: operator callsign.
        let Some(callsign) = cells.get(1) else {
            continue;
        };
        if callsign.is_empty() {
            continue;
        }

        // Column 6: Dest Rptr — contains reflector callsign + module letter.
        // Format is typically "REF001 B" (callsign padded to 7 chars + space
        // + module).
        let Some(dest_rptr) = cells.get(6) else {
            continue;
        };
        let (reflector, module) = parse_rptr_field(dest_rptr);

        // Skip rows with no recognisable reflector destination.
        let Some(reflector) = reflector else {
            continue;
        };

        let protocol = infer_protocol(&reflector);

        observations.push(Observation {
            callsign: callsign.clone(),
            reflector,
            module,
            protocol,
        });
    }

    Ok(observations)
}

/// Parses an RPT field (e.g. `"REF001 B"`) into a reflector callsign and
/// optional module letter.
///
/// The field format is: up to 7 characters of callsign (possibly space-padded),
/// followed by a space and a single module letter (A-Z). Returns `(None, None)`
/// if the field is empty or does not contain a recognisable callsign.
fn parse_rptr_field(field: &str) -> (Option<String>, Option<String>) {
    let trimmed = field.trim();
    if trimmed.is_empty() {
        return (None, None);
    }

    // Try to split on the last space to separate callsign from module.
    if let Some(last_space) = trimmed.rfind(' ') {
        let callsign_part = trimmed.get(..last_space).unwrap_or("").trim();
        let module_part = trimmed.get(last_space + 1..).unwrap_or("").trim();

        // Module should be a single uppercase letter A-Z.
        let module = if module_part.len() == 1
            && module_part
                .as_bytes()
                .first()
                .is_some_and(u8::is_ascii_uppercase)
        {
            Some(module_part.to_owned())
        } else {
            None
        };

        if !callsign_part.is_empty() {
            return (Some(callsign_part.to_owned()), module);
        }
    }

    // No space found — treat the entire field as a callsign with no module.
    (Some(trimmed.to_owned()), None)
}

/// Infers the D-STAR protocol from a reflector callsign prefix.
///
/// - `REF` prefix → `"dplus"`
/// - `XRF` or `XLX` prefix → `"dextra"`
/// - `DCS` prefix → `"dcs"`
/// - Anything else → `"dextra"` (conservative default for unknown reflectors)
fn infer_protocol(callsign: &str) -> &'static str {
    if callsign.starts_with("REF") {
        "dplus"
    } else if callsign.starts_with("DCS") {
        "dcs"
    } else if callsign.starts_with("XRF") || callsign.starts_with("XLX") {
        "dextra"
    } else {
        // Unknown prefix — default to dextra as it is the most common
        // protocol for non-standard reflectors.
        "dextra"
    }
}
