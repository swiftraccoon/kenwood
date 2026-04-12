//! Rdio API upload queue processor.
//!
//! Polls the `streams` table for completed voice recordings with
//! `upload_status = 'pending'` and uploads them to an `SDRTrunk`-compatible
//! Rdio Scanner API server using the `POST /api/call-upload`
//! multipart/form-data protocol.
//!
//! The protocol wire format — field names, the `User-Agent: sdrtrunk`
//! header, the `"Call imported successfully."` success marker — lives in
//! [`rdio`]. This module is the orchestrator: it drains the database
//! queue, maps `StreamRow` fields onto the Rdio field set, calls
//! [`rdio::upload_stream`] for each row, and transitions the row through
//! the upload lifecycle.
//!
//! # Lifecycle transitions
//!
//! For every `pending` row returned by [`crate::db::uploads::get_pending`]
//! the processor:
//!
//! - on success — calls [`crate::db::uploads::mark_uploaded`].
//! - on failure with attempts below `max_retries` — calls
//!   [`crate::db::uploads::increment_attempts`] to bump the counter and
//!   leave the row in `pending` so the next tick retries.
//! - on failure with attempts at or above `max_retries` — calls
//!   [`crate::db::uploads::mark_failed`] to transition to the terminal
//!   `failed` state.
//!
//! # System id scheme
//!
//! Rdio Scanner expects every `system` to be numeric, but D-STAR reflector
//! names are alphabetic with a numeric suffix (`REF030`, `DCS030`, ...).
//! To disambiguate reflectors that share a numeric suffix across protocol
//! families we prefix the numeric suffix with a protocol tag:
//!
//! | Protocol | Base | Example | Resulting id |
//! |---|---|---|---|
//! | `dplus` (REF) | `10_000` | `REF030` | `10030` |
//! | `dextra` (XLX) | `20_000` | `XLX030` | `20030` |
//! | `dcs` (DCS) | `30_000` | `DCS030` | `30030` |
//! | `dextra` (XRF) | `40_000` | `XRF030` | `40030` |
//!
//! Pi-Star labels XLX and XRF reflectors identically as `dextra`; we
//! distinguish them by the 3-letter prefix on the reflector callsign.
//!
//! # Talkgroup scheme
//!
//! Rdio Scanner talkgroups are numeric; D-STAR modules are letters A-Z.
//! We map `A → 1`, `B → 2`, ..., `Z → 26`. Unknown module letters fall
//! back to `0`.

pub(crate) mod rdio;

use std::time::Duration;

use chrono::{DateTime, Utc};
use sqlx::PgPool;

use crate::config::RdioConfig;
use crate::db::streams::StreamRow;
use crate::db::uploads;
use rdio::{UploadError, UploadFields};

/// Maximum number of pending rows to drain per tick.
///
/// Tuned small so that a transient API outage cannot lock a large batch of
/// rows in-flight for the duration of the HTTP timeout.
const BATCH_SIZE: i64 = 10;

/// Runs the upload queue processor loop.
///
/// Polls the database for pending streams every
/// `config.retry_interval_secs` and uploads each to the Rdio Scanner
/// endpoint. Runs until cancelled.
///
/// # Errors
///
/// Returns an error only if building the shared `reqwest::Client` fails —
/// subsequent iteration errors (SQL, HTTP, multipart) are logged at `warn`
/// level and the loop keeps running so a brief outage does not freeze the
/// whole processor.
pub(crate) async fn run(
    config: RdioConfig,
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    tracing::info!(
        endpoint = %config.endpoint,
        retry_interval_secs = config.retry_interval_secs,
        max_retries = config.max_retries,
        "upload queue processor starting"
    );

    // Shared HTTP client. reqwest pools connections per-client, so a
    // single instance keeps keep-alive connections warm across ticks.
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(60))
        .build()
        .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> { Box::new(e) })?;

    let mut interval = tokio::time::interval(Duration::from_secs(config.retry_interval_secs));
    // If the loop stalls (e.g. slow DB), do not burst-tick on resume —
    // skip missed ticks instead of trying to catch up.
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        let _instant = interval.tick().await;
        process_pending(&client, &pool, &config).await;
    }
}

/// Drains one batch of pending uploads.
///
/// Queries up to [`BATCH_SIZE`] pending rows, uploads each, and records the
/// outcome in the database. All errors are logged at `warn` level and
/// swallowed — a failure on one row never blocks the others.
async fn process_pending(client: &reqwest::Client, pool: &PgPool, config: &RdioConfig) {
    let rows = match uploads::get_pending(pool, BATCH_SIZE).await {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(error = %e, "upload: failed to query pending streams");
            return;
        }
    };

    if rows.is_empty() {
        tracing::debug!("upload: no pending streams");
        return;
    }

    tracing::debug!(count = rows.len(), "upload: processing pending streams");

    for row in rows {
        process_one(client, pool, config, row).await;
    }
}

/// Uploads a single stream and records the outcome.
///
/// Errors from either the HTTP upload or the status-transition query are
/// logged but never propagated — the caller iterates over a batch and we
/// do not want one bad row to abort the rest.
async fn process_one(client: &reqwest::Client, pool: &PgPool, config: &RdioConfig, row: StreamRow) {
    // Guard: `get_pending` already filters on `audio_mp3 IS NOT NULL`, but
    // the column is Option<Vec<u8>> at the type level. If somehow the row
    // is incomplete we log and mark it failed so it does not spin forever.
    let Some(audio_mp3) = row.audio_mp3.clone() else {
        tracing::warn!(
            id = row.id,
            "upload: pending row has no audio_mp3 — marking failed"
        );
        if let Err(e) = uploads::mark_failed(pool, row.id, "no audio_mp3").await {
            tracing::warn!(id = row.id, error = %e, "upload: failed to mark row failed");
        }
        return;
    };

    // Compute all the Rdio fields from the StreamRow.
    let system_id = compute_system_id(&row.reflector, &row.protocol);
    let protocol_label = protocol_label(&row.protocol);
    let system_label = format!("{} ({})", row.reflector, protocol_label);
    let talkgroup = module_to_talkgroup(Some(row.module.as_str()));
    let talkgroup_label = format!("Module {}", row.module);
    let talker_alias = make_talker_alias(&row.callsign, row.suffix.as_deref());
    let audio_name = make_audio_name(row.started_at, &row.reflector, &row.module, &row.callsign);
    let date_time = row.started_at.timestamp();

    let fields = UploadFields {
        api_key: &config.api_key,
        system: &system_id,
        system_label: &system_label,
        talkgroup: &talkgroup,
        talkgroup_label: &talkgroup_label,
        source: &row.callsign,
        talker_alias: &talker_alias,
        talkgroup_tag: row.dstar_text.as_deref(),
        date_time,
        audio_name: &audio_name,
        audio_mp3,
    };

    match rdio::upload_stream(client, &config.endpoint, fields).await {
        Ok(()) => {
            tracing::info!(
                id = row.id,
                reflector = %row.reflector,
                module = %row.module,
                callsign = %row.callsign,
                "upload: succeeded"
            );
            if let Err(e) = uploads::mark_uploaded(pool, row.id).await {
                tracing::warn!(id = row.id, error = %e, "upload: failed to mark row uploaded");
            }
        }
        Err(e) => {
            handle_upload_failure(pool, config, &row, &e).await;
        }
    }
}

/// Handles an upload failure by deciding whether to retry or mark the row
/// as permanently failed, based on the attempts-so-far counter.
async fn handle_upload_failure(
    pool: &PgPool,
    config: &RdioConfig,
    row: &StreamRow,
    error: &UploadError,
) {
    // attempts_so_far counts attempts BEFORE this one. The database will
    // be incremented by `increment_attempts` after the check.
    let attempts_so_far = row.upload_attempts.unwrap_or(0);
    let max = i32::try_from(config.max_retries).unwrap_or(i32::MAX);

    // The current attempt raised `error`, so `attempts_so_far + 1` total
    // attempts have now been made. If that reaches `max`, no more retries.
    let attempts_after = attempts_so_far.saturating_add(1);
    let error_msg = error.to_string();

    if attempts_after >= max {
        tracing::warn!(
            id = row.id,
            attempts = attempts_after,
            max_retries = config.max_retries,
            error = %error_msg,
            "upload: max retries exhausted — marking failed"
        );
        if let Err(e) = uploads::mark_failed(pool, row.id, &error_msg).await {
            tracing::warn!(id = row.id, error = %e, "upload: failed to mark row failed");
        }
    } else {
        tracing::warn!(
            id = row.id,
            attempts = attempts_after,
            max_retries = config.max_retries,
            error = %error_msg,
            "upload: failed — will retry"
        );
        if let Err(e) = uploads::increment_attempts(pool, row.id, &error_msg).await {
            tracing::warn!(id = row.id, error = %e, "upload: failed to increment attempts");
        }
    }
}

/// Returns the Rdio Scanner `system` id for a given D-STAR reflector.
///
/// See the module-level documentation for the base-offset table. Unknown
/// protocols or non-matching reflector-name patterns fall back to the raw
/// 3-digit suffix (or `"0"` if none).
///
/// # Examples
///
/// ```ignore
/// assert_eq!(compute_system_id("REF030", "dplus"), "10030");
/// assert_eq!(compute_system_id("DCS030", "dcs"),   "30030");
/// assert_eq!(compute_system_id("XLX030", "dextra"),"20030");
/// assert_eq!(compute_system_id("XRF030", "dextra"),"40030");
/// ```
fn compute_system_id(reflector: &str, protocol: &str) -> String {
    let suffix = numeric_suffix(reflector).unwrap_or(0);
    let base = match (protocol, reflector_prefix(reflector)) {
        ("dplus", _) => 10_000,
        ("dextra", Some("XRF")) => 40_000,
        ("dextra", _) => 20_000,
        ("dcs", _) => 30_000,
        _ => 0,
    };
    (base + u32::from(suffix)).to_string()
}

/// Returns the first three ASCII-uppercase letters of a reflector name, or
/// `None` if the prefix is shorter than three bytes or contains non-ASCII.
///
/// Used by [`compute_system_id`] to distinguish XLX from XRF within the
/// `dextra` protocol family.
fn reflector_prefix(reflector: &str) -> Option<&str> {
    let prefix = reflector.get(..3)?;
    if prefix.bytes().all(|b| b.is_ascii_uppercase()) {
        Some(prefix)
    } else {
        None
    }
}

/// Extracts the trailing numeric suffix of a reflector callsign.
///
/// `"REF030"` → `Some(30)`, `"XLX123"` → `Some(123)`, `"invalid"` → `None`.
/// Clamped to `u16` because all known D-STAR reflector registries use
/// 3-digit suffixes.
fn numeric_suffix(reflector: &str) -> Option<u16> {
    let tail: String = reflector
        .chars()
        .rev()
        .take_while(char::is_ascii_digit)
        .collect::<String>()
        .chars()
        .rev()
        .collect();
    tail.parse().ok()
}

/// Returns the human-readable protocol label used in `systemLabel`.
///
/// `"dplus"` → `"DPlus"`, `"dextra"` → `"DExtra"`, `"dcs"` → `"DCS"`.
/// Unknown protocols pass through as-is so the operator sees the raw
/// string rather than silently losing information.
fn protocol_label(protocol: &str) -> String {
    match protocol {
        "dplus" => "DPlus".to_owned(),
        "dextra" => "DExtra".to_owned(),
        "dcs" => "DCS".to_owned(),
        other => other.to_owned(),
    }
}

/// Returns the Rdio Scanner `talkgroup` id for a D-STAR module letter.
///
/// `A` → `1`, `B` → `2`, ..., `Z` → `26`. Any other input (including
/// `None`, empty strings, lowercase letters, multi-byte strings) falls
/// back to `"0"` so the upload still goes through with a sentinel value
/// the server can filter on.
fn module_to_talkgroup(module: Option<&str>) -> String {
    let Some(m) = module else {
        return "0".to_owned();
    };
    let mut bytes = m.bytes();
    let first = bytes.next();
    let rest_empty = bytes.next().is_none();
    match (first, rest_empty) {
        (Some(c), true) if c.is_ascii_uppercase() => (c - b'A' + 1).to_string(),
        _ => "0".to_owned(),
    }
}

/// Builds the `talkerAlias` field from a callsign and optional suffix.
///
/// `("W1AW", Some("D75"))` → `"W1AW / D75"`, `("W1AW", None)` → `"W1AW"`.
/// Trims trailing whitespace from the suffix because D-STAR pads
/// suffixes to 4 bytes on the wire.
fn make_talker_alias(callsign: &str, suffix: Option<&str>) -> String {
    let trimmed = suffix.map_or("", str::trim);
    if trimmed.is_empty() {
        callsign.to_owned()
    } else {
        format!("{callsign} / {trimmed}")
    }
}

/// Builds the `audio` part's filename.
///
/// Format: `YYYYMMDD_HHMMSS_<reflector>_<module>_<callsign>.mp3`, derived
/// from the stream's `started_at` timestamp (UTC). Example:
///
/// ```text
/// 20260412_143000_REF030_C_W1AW.mp3
/// ```
///
/// The callsign is trimmed of trailing whitespace (D-STAR pads callsigns
/// to 8 bytes on the wire) so the filename is not littered with padding.
fn make_audio_name(
    started_at: DateTime<Utc>,
    reflector: &str,
    module: &str,
    callsign: &str,
) -> String {
    let stamp = started_at.format("%Y%m%d_%H%M%S");
    let callsign = callsign.trim();
    format!("{stamp}_{reflector}_{module}_{callsign}.mp3")
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;

    use super::{
        compute_system_id, make_audio_name, make_talker_alias, module_to_talkgroup, numeric_suffix,
        protocol_label, reflector_prefix,
    };

    #[test]
    fn compute_system_id_dplus_ref_prefix() {
        assert_eq!(compute_system_id("REF030", "dplus"), "10030");
        assert_eq!(compute_system_id("REF001", "dplus"), "10001");
        assert_eq!(compute_system_id("REF999", "dplus"), "10999");
    }

    #[test]
    fn compute_system_id_dextra_xlx_prefix() {
        assert_eq!(compute_system_id("XLX030", "dextra"), "20030");
        assert_eq!(compute_system_id("XLX001", "dextra"), "20001");
    }

    #[test]
    fn compute_system_id_dcs_prefix() {
        assert_eq!(compute_system_id("DCS030", "dcs"), "30030");
        assert_eq!(compute_system_id("DCS001", "dcs"), "30001");
    }

    #[test]
    fn compute_system_id_dextra_xrf_prefix() {
        assert_eq!(compute_system_id("XRF030", "dextra"), "40030");
        assert_eq!(compute_system_id("XRF123", "dextra"), "40123");
    }

    #[test]
    fn compute_system_id_unknown_protocol_uses_zero_base() {
        // Unknown protocols fall back to base 0 so the numeric suffix is
        // preserved and the upload still goes through.
        assert_eq!(compute_system_id("REF030", "unknown"), "30");
    }

    #[test]
    fn compute_system_id_missing_suffix_returns_base() {
        // No numeric suffix means 0 — base prefix only.
        assert_eq!(compute_system_id("REFXYZ", "dplus"), "10000");
    }

    #[test]
    fn module_to_talkgroup_covers_all_uppercase_letters() {
        // A through Z map to 1 through 26 inclusive.
        for (i, letter) in ('A'..='Z').enumerate() {
            let input = letter.to_string();
            let expected = (i + 1).to_string();
            assert_eq!(
                module_to_talkgroup(Some(input.as_str())),
                expected,
                "module {letter} must map to {expected}"
            );
        }
    }

    #[test]
    fn module_to_talkgroup_none_returns_zero() {
        assert_eq!(module_to_talkgroup(None), "0");
    }

    #[test]
    fn module_to_talkgroup_rejects_invalid_inputs() {
        // Empty string, lowercase, multi-char, and non-ASCII all sentinel.
        assert_eq!(module_to_talkgroup(Some("")), "0");
        assert_eq!(module_to_talkgroup(Some("a")), "0");
        assert_eq!(module_to_talkgroup(Some("AA")), "0");
        assert_eq!(module_to_talkgroup(Some("A ")), "0");
        assert_eq!(module_to_talkgroup(Some("1")), "0");
    }

    #[test]
    fn make_audio_name_formats_timestamp_correctly() -> Result<(), Box<dyn std::error::Error>> {
        let ts = chrono::Utc
            .with_ymd_and_hms(2026, 4, 12, 14, 30, 0)
            .single()
            .ok_or("fixed timestamp must be unambiguous")?;
        let name = make_audio_name(ts, "REF030", "C", "W1AW");
        assert_eq!(name, "20260412_143000_REF030_C_W1AW.mp3");
        Ok(())
    }

    #[test]
    fn make_audio_name_trims_callsign_padding() -> Result<(), Box<dyn std::error::Error>> {
        // D-STAR pads callsigns to 8 bytes with trailing spaces; those
        // must not end up in the filename.
        let ts = chrono::Utc
            .with_ymd_and_hms(2026, 1, 1, 0, 0, 0)
            .single()
            .ok_or("fixed timestamp must be unambiguous")?;
        let name = make_audio_name(ts, "DCS030", "B", "W1AW    ");
        assert_eq!(name, "20260101_000000_DCS030_B_W1AW.mp3");
        Ok(())
    }

    #[test]
    fn make_talker_alias_appends_suffix() {
        assert_eq!(make_talker_alias("W1AW", Some("D75")), "W1AW / D75");
    }

    #[test]
    fn make_talker_alias_trims_suffix() {
        // D-STAR suffixes are padded to 4 bytes; trailing whitespace must
        // not leak into the talker alias display.
        assert_eq!(make_talker_alias("W1AW", Some("D75 ")), "W1AW / D75");
    }

    #[test]
    fn make_talker_alias_skips_empty_suffix() {
        assert_eq!(make_talker_alias("W1AW", Some("    ")), "W1AW");
        assert_eq!(make_talker_alias("W1AW", Some("")), "W1AW");
        assert_eq!(make_talker_alias("W1AW", None), "W1AW");
    }

    #[test]
    fn protocol_label_known_protocols_capitalise() {
        assert_eq!(protocol_label("dplus"), "DPlus");
        assert_eq!(protocol_label("dextra"), "DExtra");
        assert_eq!(protocol_label("dcs"), "DCS");
    }

    #[test]
    fn protocol_label_unknown_passes_through() {
        assert_eq!(protocol_label("weird"), "weird");
    }

    #[test]
    fn numeric_suffix_extracts_trailing_digits() {
        assert_eq!(numeric_suffix("REF030"), Some(30));
        assert_eq!(numeric_suffix("DCS001"), Some(1));
        assert_eq!(numeric_suffix("XRF999"), Some(999));
    }

    #[test]
    fn numeric_suffix_handles_no_digits() {
        assert_eq!(numeric_suffix("REFXYZ"), None);
        assert_eq!(numeric_suffix(""), None);
    }

    #[test]
    fn reflector_prefix_returns_three_uppercase_letters() {
        assert_eq!(reflector_prefix("REF030"), Some("REF"));
        assert_eq!(reflector_prefix("XRF030"), Some("XRF"));
        assert_eq!(reflector_prefix("XLX030"), Some("XLX"));
        assert_eq!(reflector_prefix("DCS030"), Some("DCS"));
    }

    #[test]
    fn reflector_prefix_rejects_short_or_non_ascii() {
        assert_eq!(reflector_prefix("RE"), None);
        assert_eq!(reflector_prefix("ref030"), None);
    }
}
