//! Query functions for the `streams` table.
//!
//! The streams table stores captured D-STAR voice transmissions from Tier 3
//! deep connections. Each row represents one complete or in-progress voice
//! stream: the D-STAR header metadata, optional slow-data fields (text message,
//! DPRS position), the decoded MP3 audio blob, and upload lifecycle state.
//!
//! # Lifecycle
//!
//! 1. Tier 3 calls [`insert_stream`] when a voice header arrives, creating a
//!    row with `audio_mp3 = NULL` and `ended_at = NULL`.
//! 2. When the stream ends (EOT or timeout), Tier 3 calls [`update_audio`] to
//!    fill in the MP3 blob, final frame count, and end timestamp.
//! 3. The upload processor picks up rows with `upload_status = 'pending'` and
//!    non-null `audio_mp3` via the `uploads` module.

use chrono::{DateTime, Utc};
use sqlx::PgPool;

/// A pending stream's data, as the upload processor needs it to build one
/// Rdio API call.
///
/// This projection of the `streams` table includes the `audio_mp3` blob
/// (which the HTTP read paths deliberately omit — see [`StreamSummaryRow`])
/// and only the header fields that map onto Rdio form fields.
#[derive(Debug, sqlx::FromRow)]
pub(crate) struct PendingUploadRow {
    /// Database row id — the upload's primary key for status transitions.
    pub(crate) id: i64,

    /// Reflector callsign — drives the Rdio `system` id and labels.
    pub(crate) reflector: String,

    /// Module letter — drives the Rdio `talkgroup`.
    pub(crate) module: String,

    /// D-STAR protocol used: `"dplus"`, `"dextra"`, or `"dcs"`.
    pub(crate) protocol: String,

    /// Operator callsign from the D-STAR header `my_callsign` field.
    pub(crate) callsign: String,

    /// Operator suffix (4 bytes from `my_suffix`), if present.
    pub(crate) suffix: Option<String>,

    /// Slow-data text message decoded from the voice frames, mapped to the
    /// Rdio `talkgroupTag`.
    pub(crate) dstar_text: Option<String>,

    /// When the first voice frame was received — the Rdio `dateTime` and the
    /// audio filename stamp.
    pub(crate) started_at: DateTime<Utc>,

    /// Decoded MP3 audio blob. `get_pending` filters on `IS NOT NULL`, so in
    /// practice this is always `Some` for a row the processor receives.
    pub(crate) audio_mp3: Option<Vec<u8>>,

    /// Number of upload attempts made so far, for the retry/give-up decision.
    pub(crate) upload_attempts: Option<i32>,
}

/// A `streams` row without the `audio_mp3` blob.
///
/// The HTTP listing endpoints never serve audio: selecting `audio_mp3` would
/// pull the full MP3 of every listed stream out of Postgres only for the JSON
/// view to discard it. This projection is the row type for those read paths;
/// [`PendingUploadRow`] (with the blob) is reserved for the upload processor.
#[derive(Debug, sqlx::FromRow)]
pub(crate) struct StreamSummaryRow {
    /// Auto-generated row identifier.
    pub(crate) id: i64,
    /// Reflector callsign where this stream was captured.
    pub(crate) reflector: String,
    /// Module letter (A-Z) the stream was received on.
    pub(crate) module: String,
    /// D-STAR protocol used: `"dplus"`, `"dextra"`, or `"dcs"`.
    pub(crate) protocol: String,
    /// D-STAR stream ID (stored as i32 for Postgres `INTEGER` compatibility).
    pub(crate) stream_id: i32,
    /// Operator callsign from the D-STAR header `my_callsign` field.
    pub(crate) callsign: String,
    /// Operator suffix, if present.
    pub(crate) suffix: Option<String>,
    /// `UR` (destination) callsign from the D-STAR header.
    pub(crate) ur_call: Option<String>,
    /// Slow-data text message decoded from the voice frames.
    pub(crate) dstar_text: Option<String>,
    /// DPRS latitude, if a position report was embedded in the slow data.
    pub(crate) dprs_lat: Option<f64>,
    /// DPRS longitude, if a position report was embedded in the slow data.
    pub(crate) dprs_lon: Option<f64>,
    /// When the first voice frame was received.
    pub(crate) started_at: DateTime<Utc>,
    /// When the stream ended (EOT or timeout); `None` while in progress.
    pub(crate) ended_at: Option<DateTime<Utc>>,
    /// Number of voice frames captured.
    pub(crate) frame_count: Option<i32>,
    /// Upload lifecycle state: `"pending"`, `"uploaded"`, or `"failed"`.
    pub(crate) upload_status: Option<String>,
    /// Error message from the most recent failed upload attempt.
    pub(crate) last_upload_error: Option<String>,
    /// When the stream was successfully uploaded to the Rdio API.
    pub(crate) uploaded_at: Option<DateTime<Utc>>,
    /// When this row was first inserted.
    pub(crate) created_at: Option<DateTime<Utc>>,
}

/// Parameters for inserting a new stream row.
///
/// Collected from the D-STAR voice header when a Tier 3 connection receives
/// a new transmission. Audio and end-time are filled in later via
/// [`update_audio`].
#[derive(Debug)]
pub(crate) struct NewStream<'a> {
    /// Reflector callsign.
    pub(crate) reflector: &'a str,
    /// Module letter.
    pub(crate) module: &'a str,
    /// Protocol name.
    pub(crate) protocol: &'a str,
    /// D-STAR stream ID.
    pub(crate) stream_id: i32,
    /// Operator callsign.
    pub(crate) callsign: &'a str,
    /// Operator suffix, if any.
    pub(crate) suffix: Option<&'a str>,
    /// UR callsign, if any.
    pub(crate) ur_call: Option<&'a str>,
    /// When the header was received.
    pub(crate) started_at: DateTime<Utc>,
}

/// Parameters for updating a stream after voice capture and MP3 encoding
/// complete.
///
/// Groups the audio blob, timing, and optional slow-data fields into a single
/// struct to stay within clippy's argument-count limit.
#[derive(Debug)]
pub(crate) struct AudioUpdate<'a> {
    /// Encoded MP3 audio bytes.
    pub(crate) audio_mp3: &'a [u8],
    /// When the stream ended (EOT or timeout).
    pub(crate) ended_at: DateTime<Utc>,
    /// Total number of voice frames captured.
    pub(crate) frame_count: i32,
    /// Slow-data text message decoded from voice frames.
    pub(crate) dstar_text: Option<&'a str>,
    /// DPRS latitude, if a position report was embedded.
    pub(crate) dprs_lat: Option<f64>,
    /// DPRS longitude, if a position report was embedded.
    pub(crate) dprs_lon: Option<f64>,
}

/// Inserts a new stream row and returns its auto-generated `id`.
///
/// Called by Tier 3 when a D-STAR voice header arrives. The row is created
/// with `audio_mp3 = NULL`, `ended_at = NULL`, and `upload_status = 'pending'`.
/// The returned `id` is used by subsequent [`update_audio`] calls.
///
/// # Errors
///
/// Returns `sqlx::Error` on connection or constraint failures.
pub(crate) async fn insert_stream(
    pool: &PgPool,
    stream: &NewStream<'_>,
) -> Result<i64, sqlx::Error> {
    // INSERT with RETURNING id to get the auto-generated BIGSERIAL value.
    let row: (i64,) = sqlx::query_as(
        "INSERT INTO streams (reflector, module, protocol, stream_id, callsign,
                              suffix, ur_call, started_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
         RETURNING id",
    )
    .bind(stream.reflector)
    .bind(stream.module)
    .bind(stream.protocol)
    .bind(stream.stream_id)
    .bind(stream.callsign)
    .bind(stream.suffix)
    .bind(stream.ur_call)
    .bind(stream.started_at)
    .fetch_one(pool)
    .await?;
    Ok(row.0)
}

/// Updates a stream with the encoded MP3 audio, end timestamp, and frame count.
///
/// Called by Tier 3 after the voice stream ends and MP3 encoding completes.
/// The [`AudioUpdate`] struct groups the audio blob, timing, and optional
/// slow-data fields decoded from the voice frames.
///
/// # Errors
///
/// Returns `sqlx::Error` on query failure.
pub(crate) async fn update_audio(
    pool: &PgPool,
    id: i64,
    update: &AudioUpdate<'_>,
) -> Result<(), sqlx::Error> {
    // UPDATE targeting a single row by primary key, filling in audio and
    // metadata fields that were NULL at insert time.
    let _result = sqlx::query(
        "UPDATE streams
         SET audio_mp3 = $1, ended_at = $2, frame_count = $3,
             dstar_text = $4, dprs_lat = $5, dprs_lon = $6
         WHERE id = $7",
    )
    .bind(update.audio_mp3)
    .bind(update.ended_at)
    .bind(update.frame_count)
    .bind(update.dstar_text)
    .bind(update.dprs_lat)
    .bind(update.dprs_lon)
    .bind(id)
    .execute(pool)
    .await?;
    Ok(())
}

/// Aggregated counts of streams grouped by `upload_status`.
///
/// Used by the HTTP API `/metrics` endpoint for operational visibility.
/// Any stream row whose `upload_status` is NULL or an unrecognized value is
/// bucketed into `unknown` so the total always sums back to the full row
/// count of the `streams` table.
#[derive(Debug, Default, serde::Serialize)]
pub(crate) struct StreamStatusCounts {
    /// Total rows in the `streams` table.
    pub(crate) total: i64,
    /// Rows with `upload_status = 'pending'`.
    pub(crate) pending: i64,
    /// Rows with `upload_status = 'uploaded'`.
    pub(crate) uploaded: i64,
    /// Rows with `upload_status = 'failed'`.
    pub(crate) failed: i64,
}

/// Returns aggregated upload-status counts for the `streams` table.
///
/// Runs a single `GROUP BY upload_status` query. It aggregates over every row
/// in `streams`; `idx_streams_upload` lets Postgres satisfy it with an index
/// scan rather than a heap scan, but the cost still grows with the table —
/// this is an operational snapshot, not an O(1) lookup.
///
/// # Errors
///
/// Returns `sqlx::Error` on query failure.
pub(crate) async fn count_by_status(pool: &PgPool) -> Result<StreamStatusCounts, sqlx::Error> {
    // Each row is (upload_status, COUNT(*)) — we fold into the typed
    // StreamStatusCounts struct so the HTTP layer can serialize directly.
    let rows: Vec<(Option<String>, i64)> =
        sqlx::query_as("SELECT upload_status, COUNT(*) FROM streams GROUP BY upload_status")
            .fetch_all(pool)
            .await?;
    let mut counts = StreamStatusCounts::default();
    for (status, n) in rows {
        counts.total = counts.total.saturating_add(n);
        match status.as_deref() {
            Some("pending") => counts.pending = n,
            Some("uploaded") => counts.uploaded = n,
            Some("failed") => counts.failed = n,
            _ => {}
        }
    }
    Ok(counts)
}

/// Queries streams with optional filters on reflector, time window, and limit.
///
/// Used by the HTTP API to serve the stream listing endpoint. When
/// `reflector_filter` is `Some`, only streams from that reflector are returned.
/// Results are ordered by `started_at DESC` (most recent first). The
/// `audio_mp3` blob is not selected — see [`StreamSummaryRow`].
///
/// # Errors
///
/// Returns `sqlx::Error` on query failure.
pub(crate) async fn query(
    pool: &PgPool,
    reflector_filter: Option<&str>,
    since: DateTime<Utc>,
    limit: i64,
) -> Result<Vec<StreamSummaryRow>, sqlx::Error> {
    // Two query paths: filtered by reflector, or all reflectors. Both are
    // time-bounded and row-limited, using idx_streams_lookup. audio_mp3 is
    // deliberately omitted from the projection.
    if let Some(reflector) = reflector_filter {
        sqlx::query_as::<_, StreamSummaryRow>(
            "SELECT id, reflector, module, protocol, stream_id, callsign,
                    suffix, ur_call, dstar_text, dprs_lat, dprs_lon,
                    started_at, ended_at, frame_count,
                    upload_status, last_upload_error, uploaded_at, created_at
             FROM streams
             WHERE reflector = $1 AND started_at >= $2
             ORDER BY started_at DESC
             LIMIT $3",
        )
        .bind(reflector)
        .bind(since)
        .bind(limit)
        .fetch_all(pool)
        .await
    } else {
        sqlx::query_as::<_, StreamSummaryRow>(
            "SELECT id, reflector, module, protocol, stream_id, callsign,
                    suffix, ur_call, dstar_text, dprs_lat, dprs_lon,
                    started_at, ended_at, frame_count,
                    upload_status, last_upload_error, uploaded_at, created_at
             FROM streams
             WHERE started_at >= $1
             ORDER BY started_at DESC
             LIMIT $2",
        )
        .bind(since)
        .bind(limit)
        .fetch_all(pool)
        .await
    }
}
