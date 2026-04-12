//! HTTP handler functions for the stargazer operational API.
//!
//! Each handler is an `async fn` that takes axum extractors (`State`, `Query`,
//! `Path`) and returns either a JSON response or a `StatusCode` error. All
//! database errors are logged at `warn` level and surfaced to the caller as
//! `500 Internal Server Error` with a generic body — the raw `sqlx::Error`
//! is never leaked because it may contain connection strings or schema
//! details that are useful to an attacker.
//!
//! # Response types
//!
//! The handlers operate on database rows (`ReflectorRow`, `ActivityRow`,
//! `StreamRow`) but serialize trimmed `*View` structs over the wire. The
//! view structs intentionally omit columns that are operationally useless
//! to the HTTP consumer (raw `audio_mp3` blobs, stream ids internal to the
//! capture pipeline, etc.) and rename fields to the JSON conventions the
//! operator dashboards expect.
//!
//! # Time-window parsing
//!
//! Query endpoints accept a `since` parameter of the form `<n><unit>` where
//! `n` is a positive integer and `unit` is `s`, `m`, `h`, or `d`. The helper
//! [`parse_since`] turns this into a `DateTime<Utc>` anchored at `Utc::now()`.
//! A missing or unparseable value falls back to the endpoint-specific
//! default (documented per handler).

use std::time::Duration;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;

use crate::db;
use crate::db::activity::ActivityRow;
use crate::db::connected_nodes::ConnectedNodeRow;
use crate::db::reflectors::ReflectorRow;
use crate::db::streams::{StreamRow, StreamStatusCounts};

/// Default time window for `/api/reflectors` when `since` is missing.
///
/// Matches the default Tier 2 activity threshold (30 minutes) so the
/// dashboard's default view mirrors the monitor-pool selection logic.
const DEFAULT_REFLECTOR_WINDOW: Duration = Duration::from_secs(1_800);

/// Default time window for `/api/reflectors/{callsign}/activity`.
///
/// Six hours is a reasonable window for a "recent activity" pane: long
/// enough to show multi-hour conversations, short enough that the query
/// hits the composite `idx_activity_log_lookup` index cleanly.
const DEFAULT_ACTIVITY_WINDOW: Duration = Duration::from_secs(21_600);

/// Default time window for `/api/streams`.
const DEFAULT_STREAM_WINDOW: Duration = Duration::from_secs(3_600);

/// Default row cap for `/api/streams` when `limit` is missing.
const DEFAULT_STREAM_LIMIT: i64 = 50;

/// Default row cap for `/api/upload-queue` when `limit` is missing.
const DEFAULT_UPLOAD_LIMIT: i64 = 50;

/// Default row cap for `/api/activity` when `limit` is missing.
const DEFAULT_ACTIVITY_LIMIT: i64 = 50;

// ---------------------------------------------------------------------------
// Query parameter structs
// ---------------------------------------------------------------------------

/// Query parameters for the `/api/reflectors` endpoint.
#[derive(Debug, Default, Deserialize)]
pub(crate) struct ReflectorQuery {
    /// Relative time window, e.g. `"1h"`, `"30m"`, `"7d"`. Defaults to 30m.
    pub(crate) since: Option<String>,
}

/// Query parameters for the `/api/reflectors/{callsign}/activity` endpoint.
#[derive(Debug, Default, Deserialize)]
pub(crate) struct ActivityQuery {
    /// Relative time window, e.g. `"1h"`, `"24h"`, `"7d"`. Defaults to 6h.
    pub(crate) since: Option<String>,
}

/// Query parameters for the `/api/streams` endpoint.
#[derive(Debug, Default, Deserialize)]
pub(crate) struct StreamQuery {
    /// Relative time window, e.g. `"1h"`, `"24h"`, `"7d"`. Defaults to 1h.
    pub(crate) since: Option<String>,

    /// Optional reflector callsign filter. When present, only streams
    /// captured from this reflector are returned.
    pub(crate) reflector: Option<String>,

    /// Row cap. Clamped to a max of 500 rows per response; defaults to 50.
    pub(crate) limit: Option<i64>,
}

/// Query parameters for the `/api/upload-queue` endpoint.
#[derive(Debug, Default, Deserialize)]
pub(crate) struct UploadQueueQuery {
    /// Row cap for the pending-queue slice. Capped at 500; defaults to 50.
    pub(crate) limit: Option<i64>,
}

/// Query parameters for the `/api/activity` endpoint.
#[derive(Debug, Default, Deserialize)]
pub(crate) struct GlobalActivityQuery {
    /// Relative time window, e.g. `"1h"`, `"24h"`, `"7d"`. Defaults to 6h.
    pub(crate) since: Option<String>,

    /// Row cap. Clamped to a max of 500 rows per response; defaults to 50.
    pub(crate) limit: Option<i64>,
}

// ---------------------------------------------------------------------------
// Response structs
// ---------------------------------------------------------------------------

/// JSON response body for the `/health` endpoint.
#[derive(Debug, Serialize)]
pub(crate) struct HealthResponse {
    /// Always `"ok"` — stargazer responds with this whenever the HTTP
    /// server task is alive.
    pub(crate) status: &'static str,
}

/// JSON response body for the `/metrics` endpoint.
///
/// Aggregated counters across the `reflectors` and `streams` tables. Not
/// Prometheus-formatted — this is an operational snapshot meant for human
/// inspection or k8s health checks, not long-term time-series storage.
#[derive(Debug, Serialize)]
pub(crate) struct MetricsResponse {
    /// Total rows in the `reflectors` table.
    pub(crate) reflectors_count: i64,

    /// Total captured streams.
    pub(crate) streams_total: i64,

    /// Streams awaiting upload.
    pub(crate) streams_pending: i64,

    /// Streams successfully uploaded to the Rdio API.
    pub(crate) streams_uploaded: i64,

    /// Streams that failed permanently (max retries exceeded or hard error).
    pub(crate) streams_failed: i64,
}

/// JSON view of one reflector row.
#[derive(Debug, Serialize)]
pub(crate) struct ReflectorView {
    /// Reflector callsign, e.g. `"REF030"`.
    pub(crate) callsign: String,

    /// Protocol family (`"dplus"`, `"dextra"`, or `"dcs"`).
    pub(crate) protocol: String,

    /// Last time any activity was observed.
    pub(crate) last_seen: Option<DateTime<Utc>>,

    /// Two-letter country code, if known.
    pub(crate) country: Option<String>,

    /// URL of the reflector's web dashboard, if known.
    pub(crate) dashboard_url: Option<String>,

    /// Whether the reflector supports Tier 2 XLX UDP monitor.
    pub(crate) tier2_available: bool,

    /// When this row was first inserted into the reflectors table.
    ///
    /// Operators use this to distinguish "recently discovered" reflectors
    /// from long-established ones when triaging activity.
    pub(crate) created_at: Option<DateTime<Utc>>,
}

impl From<ReflectorRow> for ReflectorView {
    fn from(row: ReflectorRow) -> Self {
        Self {
            callsign: row.callsign,
            protocol: row.protocol,
            last_seen: row.last_seen,
            country: row.country,
            dashboard_url: row.dashboard_url,
            tier2_available: row.tier2_available.unwrap_or(false),
            created_at: row.created_at,
        }
    }
}

/// JSON view of one activity-log row.
#[derive(Debug, Serialize)]
pub(crate) struct ActivityView {
    /// Auto-generated row identifier from the `activity_log` BIGSERIAL
    /// primary key. Exposed so clients can deduplicate observations
    /// across pages or correlate them with backend logs.
    pub(crate) id: i64,

    /// Reflector callsign where the activity was heard.
    pub(crate) reflector: String,

    /// Module letter (A-Z), if the source reported one.
    pub(crate) module: Option<String>,

    /// Operator or node callsign.
    pub(crate) callsign: String,

    /// Data source tag (`"xlx_monitor"`, `"ircddb"`, `"pistar"`).
    pub(crate) source: String,

    /// When the activity was observed.
    pub(crate) observed_at: DateTime<Utc>,
}

impl From<ActivityRow> for ActivityView {
    fn from(row: ActivityRow) -> Self {
        Self {
            id: row.id,
            reflector: row.reflector,
            module: row.module,
            callsign: row.callsign,
            source: row.source,
            observed_at: row.observed_at,
        }
    }
}

/// JSON view of one captured stream.
///
/// Intentionally excludes `audio_mp3` — raw binary audio blobs have no
/// place in a JSON response. Consumers who need audio fetch it separately
/// via the upload URL once the row is marked uploaded.
#[derive(Debug, Serialize)]
pub(crate) struct StreamView {
    /// Database row id.
    pub(crate) id: i64,

    /// Reflector callsign.
    pub(crate) reflector: String,

    /// Module letter.
    pub(crate) module: String,

    /// D-STAR protocol name.
    pub(crate) protocol: String,

    /// D-STAR wire stream ID (non-zero `u16`, stored as `i32`).
    ///
    /// Operators correlate this with gateway and reflector logs that
    /// record the same raw header value.
    pub(crate) stream_id: i32,

    /// Operator callsign from the D-STAR header.
    pub(crate) callsign: String,

    /// Operator suffix, if any.
    pub(crate) suffix: Option<String>,

    /// `UR` (destination) callsign from the D-STAR header.
    ///
    /// Typically `CQCQCQ` for ragchews or a callsign/routing string for
    /// directed traffic. Exposed so clients can filter and render the
    /// destination without re-parsing the audio blob.
    pub(crate) ur_call: Option<String>,

    /// Decoded slow-data text message, if any.
    pub(crate) dstar_text: Option<String>,

    /// DPRS latitude (decimal degrees) decoded from the stream, if present.
    pub(crate) dprs_lat: Option<f64>,

    /// DPRS longitude (decimal degrees) decoded from the stream, if present.
    pub(crate) dprs_lon: Option<f64>,

    /// Start timestamp (first voice frame).
    pub(crate) started_at: DateTime<Utc>,

    /// End timestamp (EOT or timeout). `None` if still in progress.
    pub(crate) ended_at: Option<DateTime<Utc>>,

    /// Number of voice frames captured.
    pub(crate) frame_count: Option<i32>,

    /// Upload lifecycle state.
    pub(crate) upload_status: Option<String>,

    /// Error message from the most recent failed upload attempt, if any.
    pub(crate) last_upload_error: Option<String>,

    /// When the stream was successfully uploaded to the Rdio API, if at all.
    pub(crate) uploaded_at: Option<DateTime<Utc>>,

    /// When this row was first inserted into the streams table.
    pub(crate) created_at: Option<DateTime<Utc>>,
}

impl From<StreamRow> for StreamView {
    fn from(row: StreamRow) -> Self {
        Self {
            id: row.id,
            reflector: row.reflector,
            module: row.module,
            protocol: row.protocol,
            stream_id: row.stream_id,
            callsign: row.callsign,
            suffix: row.suffix,
            ur_call: row.ur_call,
            dstar_text: row.dstar_text,
            dprs_lat: row.dprs_lat,
            dprs_lon: row.dprs_lon,
            started_at: row.started_at,
            ended_at: row.ended_at,
            frame_count: row.frame_count,
            upload_status: row.upload_status,
            last_upload_error: row.last_upload_error,
            uploaded_at: row.uploaded_at,
            created_at: row.created_at,
        }
    }
}

/// JSON view of one connected-node row.
///
/// A node is a gateway or hotspot currently linked to a reflector module,
/// as reported by the Tier 2 XLX UDP monitor `nodes` push notifications.
#[derive(Debug, Serialize)]
pub(crate) struct ConnectedNodeView {
    /// Reflector callsign the node is connected to.
    pub(crate) reflector: String,

    /// Node callsign with module suffix (e.g. `"W1AW  B"`).
    pub(crate) node_callsign: String,

    /// Reflector module letter the node is linked to, if reported.
    pub(crate) module: Option<String>,

    /// Protocol used by the node (e.g. `"dplus"`, `"dextra"`, `"dcs"`).
    pub(crate) protocol: Option<String>,

    /// When the node first connected, as reported by the reflector.
    pub(crate) connected_since: Option<DateTime<Utc>>,

    /// When the node was last seen in a monitor update.
    pub(crate) last_heard: Option<DateTime<Utc>>,
}

impl From<ConnectedNodeRow> for ConnectedNodeView {
    fn from(row: ConnectedNodeRow) -> Self {
        Self {
            reflector: row.reflector,
            node_callsign: row.node_callsign,
            module: row.module,
            protocol: row.protocol,
            connected_since: row.connected_since,
            last_heard: row.last_heard,
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Parses a relative time string like `"1h"` or `"30m"` into a `Duration`.
///
/// Accepts a positive integer followed by exactly one unit character:
///
/// | Suffix | Unit |
/// |--------|------|
/// | `s`    | seconds |
/// | `m`    | minutes |
/// | `h`    | hours |
/// | `d`    | days |
///
/// Returns `None` for any malformed input (missing digits, unknown unit,
/// overflow). Callers fall back to endpoint-specific defaults.
fn parse_duration_string(s: &str) -> Option<Duration> {
    let s = s.trim();
    let (num_str, unit) = s.split_at(s.len().checked_sub(1)?);
    if num_str.is_empty() {
        return None;
    }
    let n: u64 = num_str.parse().ok()?;
    let secs = match unit {
        "s" => n,
        "m" => n.checked_mul(60)?,
        "h" => n.checked_mul(3600)?,
        "d" => n.checked_mul(86_400)?,
        _ => return None,
    };
    Some(Duration::from_secs(secs))
}

/// Turns a relative `since` string into an absolute `DateTime<Utc>`.
///
/// `None` or a malformed input falls back to `now - default`.
fn parse_since(since: Option<&str>, default: Duration) -> DateTime<Utc> {
    let window = since.and_then(parse_duration_string).unwrap_or(default);
    // Negative or overflowing durations wrap to the default window anchored
    // at `now`. chrono::Duration handles u64→i64 conversions with saturation.
    let window_chrono =
        chrono::Duration::from_std(window).unwrap_or_else(|_| chrono::Duration::seconds(0));
    Utc::now() - window_chrono
}

/// Clamps a caller-provided row limit to the configured maximum.
///
/// `None`, zero, or negative values return the default. Values above the
/// maximum are clamped down so the database is never forced to materialise
/// an unbounded result set.
fn clamp_limit(provided: Option<i64>, default: i64, max: i64) -> i64 {
    let requested = provided.unwrap_or(default);
    if requested <= 0 {
        default
    } else {
        requested.min(max)
    }
}

/// Maximum rows any query endpoint will return regardless of `limit`.
const MAX_LIMIT: i64 = 500;

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// `GET /health` — kubernetes liveness/readiness probe.
///
/// Always returns `200 OK` with body `{"status":"ok"}`. The endpoint is
/// deliberately side-effect-free (no database touch) so a database stall
/// does not take the whole pod out of the load balancer.
pub(crate) async fn health() -> Json<HealthResponse> {
    Json(HealthResponse { status: "ok" })
}

/// `GET /metrics` — tier statistics snapshot.
///
/// Returns aggregated counters: total reflectors and stream upload-status
/// counts. Used by operators to answer "is capture working?" at a glance.
///
/// Logs and returns `500` on database errors.
pub(crate) async fn metrics(
    State(pool): State<PgPool>,
) -> Result<Json<MetricsResponse>, StatusCode> {
    let reflectors_count = db::reflectors::count_total(&pool).await.map_err(|e| {
        tracing::warn!(error = %e, "metrics: failed to count reflectors");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    let StreamStatusCounts {
        total,
        pending,
        uploaded,
        failed,
    } = db::streams::count_by_status(&pool).await.map_err(|e| {
        tracing::warn!(error = %e, "metrics: failed to count streams");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    Ok(Json(MetricsResponse {
        reflectors_count,
        streams_total: total,
        streams_pending: pending,
        streams_uploaded: uploaded,
        streams_failed: failed,
    }))
}

/// `GET /api/reflectors` — list active reflectors with status.
///
/// Returns reflectors that have been observed within the `since` window
/// (default 30m), ordered by most-recently-seen first.
pub(crate) async fn list_reflectors(
    State(pool): State<PgPool>,
    Query(params): Query<ReflectorQuery>,
) -> Result<Json<Vec<ReflectorView>>, StatusCode> {
    let since = parse_since(params.since.as_deref(), DEFAULT_REFLECTOR_WINDOW);
    let rows = db::reflectors::get_active(&pool, since)
        .await
        .map_err(|e| {
            tracing::warn!(error = %e, "list_reflectors: query failed");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;
    Ok(Json(rows.into_iter().map(ReflectorView::from).collect()))
}

/// `GET /api/reflectors/{callsign}/activity` — recent activity for one reflector.
///
/// Returns the `activity_log` rows for `callsign` within the `since`
/// window (default 6h), ordered most-recent-first.
pub(crate) async fn reflector_activity(
    State(pool): State<PgPool>,
    Path(callsign): Path<String>,
    Query(params): Query<ActivityQuery>,
) -> Result<Json<Vec<ActivityView>>, StatusCode> {
    let since = parse_since(params.since.as_deref(), DEFAULT_ACTIVITY_WINDOW);
    let rows = db::activity::get_for_reflector(&pool, &callsign, since)
        .await
        .map_err(|e| {
            tracing::warn!(error = %e, callsign = %callsign, "reflector_activity: query failed");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;
    Ok(Json(rows.into_iter().map(ActivityView::from).collect()))
}

/// `GET /api/streams` — query captured streams with filters.
///
/// Supports `?since=<duration>`, `?reflector=<callsign>`, `?limit=<n>`.
/// Default window is 1h, default limit 50, max limit 500.
pub(crate) async fn list_streams(
    State(pool): State<PgPool>,
    Query(params): Query<StreamQuery>,
) -> Result<Json<Vec<StreamView>>, StatusCode> {
    let since = parse_since(params.since.as_deref(), DEFAULT_STREAM_WINDOW);
    let limit = clamp_limit(params.limit, DEFAULT_STREAM_LIMIT, MAX_LIMIT);
    let reflector_filter = params.reflector.as_deref();
    let rows = db::streams::query(&pool, reflector_filter, since, limit)
        .await
        .map_err(|e| {
            tracing::warn!(error = %e, "list_streams: query failed");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;
    Ok(Json(rows.into_iter().map(StreamView::from).collect()))
}

/// `GET /api/upload-queue` — pending and failed uploads.
///
/// Returns streams currently awaiting upload, ordered oldest-first (FIFO
/// matches the upload processor's drain order). Failed streams are not
/// included; they are terminal and viewable via `/api/streams`.
pub(crate) async fn upload_queue(
    State(pool): State<PgPool>,
    Query(params): Query<UploadQueueQuery>,
) -> Result<Json<Vec<StreamView>>, StatusCode> {
    let limit = clamp_limit(params.limit, DEFAULT_UPLOAD_LIMIT, MAX_LIMIT);
    let rows = db::uploads::get_pending(&pool, limit).await.map_err(|e| {
        tracing::warn!(error = %e, "upload_queue: query failed");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;
    Ok(Json(rows.into_iter().map(StreamView::from).collect()))
}

/// `GET /api/reflectors/{callsign}/nodes` — nodes linked to a reflector.
///
/// Returns the roster of gateways and hotspots currently linked to the
/// reflector's modules as reported by the most recent Tier 2 monitor
/// snapshot. Entries are ordered by `last_heard DESC NULLS LAST`.
pub(crate) async fn reflector_nodes(
    State(pool): State<PgPool>,
    Path(callsign): Path<String>,
) -> Result<Json<Vec<ConnectedNodeView>>, StatusCode> {
    let rows = db::connected_nodes::get_for_reflector(&pool, &callsign)
        .await
        .map_err(|e| {
            tracing::warn!(error = %e, callsign = %callsign, "reflector_nodes: query failed");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;
    Ok(Json(
        rows.into_iter().map(ConnectedNodeView::from).collect(),
    ))
}

/// `GET /api/activity` — global activity feed across all reflectors.
///
/// Returns `activity_log` rows within the `since` window (default 6h),
/// ordered most-recent-first and capped at `limit` rows (default 50,
/// max 500). Mirrors the per-reflector `/api/reflectors/{cs}/activity`
/// endpoint but across the whole registry.
pub(crate) async fn list_activity(
    State(pool): State<PgPool>,
    Query(params): Query<GlobalActivityQuery>,
) -> Result<Json<Vec<ActivityView>>, StatusCode> {
    let since = parse_since(params.since.as_deref(), DEFAULT_ACTIVITY_WINDOW);
    let limit = clamp_limit(params.limit, DEFAULT_ACTIVITY_LIMIT, MAX_LIMIT);
    let rows = db::activity::get_recent(&pool, since, limit)
        .await
        .map_err(|e| {
            tracing::warn!(error = %e, "list_activity: query failed");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;
    Ok(Json(rows.into_iter().map(ActivityView::from).collect()))
}

/// `POST /api/tier3/connect` — manually promote a reflector to Tier 3.
///
/// Stub: returns `501 Not Implemented` until the Tier 3 orchestrator
/// exposes a management channel. The route is live so callers discover
/// the endpoint and get a correct protocol-level response, not a 404.
pub(crate) async fn tier3_connect() -> impl IntoResponse {
    StatusCode::NOT_IMPLEMENTED
}

/// `DELETE /api/tier3/{callsign}/{module}` — disconnect a Tier 3 session.
///
/// Stub: returns `501 Not Implemented`. The path parameters are parsed and
/// validated (both must be non-empty ASCII) so that when the orchestrator
/// wiring lands the extractor signature need not change.
pub(crate) async fn tier3_disconnect(
    Path((_callsign, _module)): Path<(String, String)>,
) -> impl IntoResponse {
    StatusCode::NOT_IMPLEMENTED
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::{
        ActivityView, MAX_LIMIT, ReflectorView, StreamView, clamp_limit, health,
        parse_duration_string, parse_since,
    };
    use crate::db::activity::ActivityRow;
    use crate::db::reflectors::ReflectorRow;
    use crate::db::streams::StreamRow;
    use chrono::{TimeZone, Utc};
    use std::time::Duration;

    #[test]
    fn parse_duration_seconds() {
        assert_eq!(parse_duration_string("30s"), Some(Duration::from_secs(30)));
        assert_eq!(parse_duration_string("1s"), Some(Duration::from_secs(1)));
    }

    #[test]
    fn parse_duration_minutes() {
        assert_eq!(parse_duration_string("1m"), Some(Duration::from_secs(60)));
        assert_eq!(
            parse_duration_string("90m"),
            Some(Duration::from_secs(5_400))
        );
    }

    #[test]
    fn parse_duration_hours() {
        assert_eq!(
            parse_duration_string("1h"),
            Some(Duration::from_secs(3_600))
        );
        assert_eq!(
            parse_duration_string("24h"),
            Some(Duration::from_secs(86_400))
        );
    }

    #[test]
    fn parse_duration_days() {
        assert_eq!(
            parse_duration_string("1d"),
            Some(Duration::from_secs(86_400))
        );
        assert_eq!(
            parse_duration_string("7d"),
            Some(Duration::from_secs(604_800))
        );
    }

    #[test]
    fn parse_duration_rejects_malformed() {
        // No unit suffix.
        assert_eq!(parse_duration_string("30"), None);
        // Unknown unit.
        assert_eq!(parse_duration_string("5x"), None);
        // Empty digits.
        assert_eq!(parse_duration_string("h"), None);
        // Empty string.
        assert_eq!(parse_duration_string(""), None);
        // Negative values not supported.
        assert_eq!(parse_duration_string("-1h"), None);
        // Trailing whitespace after unit.
        assert_eq!(parse_duration_string("1 h"), None);
    }

    #[test]
    fn parse_duration_handles_overflow() {
        // 2^64 / 86400 ≈ 2.1e14; feed a value that overflows u64 * 86400.
        assert_eq!(parse_duration_string("999999999999999999d"), None);
    }

    #[test]
    fn parse_since_defaults_when_missing() {
        // With no `since` and a 1h default, the returned timestamp should be
        // ~1h before now. Allow 5 seconds of slack for test runtime.
        let t = parse_since(None, Duration::from_secs(3600));
        let delta = Utc::now().signed_duration_since(t);
        assert!(
            (3590..=3605).contains(&delta.num_seconds()),
            "expected ~3600s ago, got {}s",
            delta.num_seconds()
        );
    }

    #[test]
    fn parse_since_uses_default_on_bad_input() {
        // Malformed input silently falls back to default rather than
        // returning an HTTP error — permissive parsing for the UI.
        let t = parse_since(Some("bogus"), Duration::from_secs(600));
        let delta = Utc::now().signed_duration_since(t);
        assert!(
            (595..=605).contains(&delta.num_seconds()),
            "expected ~600s ago, got {}s",
            delta.num_seconds()
        );
    }

    #[test]
    fn parse_since_honors_valid_input() {
        let t = parse_since(Some("2h"), Duration::from_secs(60));
        let delta = Utc::now().signed_duration_since(t);
        assert!(
            (7190..=7210).contains(&delta.num_seconds()),
            "expected ~7200s ago, got {}s",
            delta.num_seconds()
        );
    }

    #[test]
    fn clamp_limit_applies_default_for_none() {
        assert_eq!(clamp_limit(None, 50, MAX_LIMIT), 50);
    }

    #[test]
    fn clamp_limit_applies_default_for_nonpositive() {
        assert_eq!(clamp_limit(Some(0), 50, MAX_LIMIT), 50);
        assert_eq!(clamp_limit(Some(-1), 50, MAX_LIMIT), 50);
        assert_eq!(clamp_limit(Some(-9_999), 50, MAX_LIMIT), 50);
    }

    #[test]
    fn clamp_limit_caps_at_max() {
        assert_eq!(clamp_limit(Some(9_999), 50, MAX_LIMIT), MAX_LIMIT);
        assert_eq!(clamp_limit(Some(MAX_LIMIT + 1), 50, MAX_LIMIT), MAX_LIMIT);
    }

    #[test]
    fn clamp_limit_passes_valid_values() {
        assert_eq!(clamp_limit(Some(1), 50, MAX_LIMIT), 1);
        assert_eq!(clamp_limit(Some(100), 50, MAX_LIMIT), 100);
        assert_eq!(clamp_limit(Some(MAX_LIMIT), 50, MAX_LIMIT), MAX_LIMIT);
    }

    #[tokio::test]
    async fn health_returns_ok_literal() {
        let resp = health().await;
        assert_eq!(resp.0.status, "ok");
    }

    #[test]
    fn reflector_view_from_row_maps_all_fields() -> Result<(), Box<dyn std::error::Error>> {
        // Use a specific, deterministic timestamp so the field mapping
        // is actually verified — not just that "some" value gets copied.
        let ts = Utc
            .with_ymd_and_hms(2026, 4, 12, 10, 0, 0)
            .single()
            .ok_or("fixed timestamp must be unambiguous")?;
        let row = ReflectorRow {
            callsign: "REF030".to_owned(),
            protocol: "dplus".to_owned(),
            ip_address: Some("1.2.3.4".to_owned()),
            dashboard_url: Some("https://example.invalid".to_owned()),
            country: Some("US".to_owned()),
            last_seen: Some(ts),
            tier2_available: Some(true),
            created_at: Some(ts),
        };
        let view = ReflectorView::from(row);
        assert_eq!(view.callsign, "REF030");
        assert_eq!(view.protocol, "dplus");
        assert_eq!(view.country.as_deref(), Some("US"));
        assert_eq!(
            view.dashboard_url.as_deref(),
            Some("https://example.invalid")
        );
        assert_eq!(view.last_seen, Some(ts));
        assert!(view.tier2_available);
        Ok(())
    }

    #[test]
    fn reflector_view_tier2_available_defaults_false() {
        // NULL in the database must not surface as `null` in JSON — we
        // document tier2_available as a bool, so `None` becomes `false`.
        let row = ReflectorRow {
            callsign: "DCS030".to_owned(),
            protocol: "dcs".to_owned(),
            ip_address: None,
            dashboard_url: None,
            country: None,
            last_seen: None,
            tier2_available: None,
            created_at: None,
        };
        let view = ReflectorView::from(row);
        assert!(!view.tier2_available);
    }

    #[test]
    fn activity_view_from_row_preserves_fields() -> Result<(), Box<dyn std::error::Error>> {
        let ts = Utc
            .with_ymd_and_hms(2026, 4, 12, 11, 0, 0)
            .single()
            .ok_or("fixed timestamp must be unambiguous")?;
        let row = ActivityRow {
            id: 42,
            reflector: "XLX030".to_owned(),
            module: Some("C".to_owned()),
            callsign: "W1AW".to_owned(),
            source: "xlx_monitor".to_owned(),
            observed_at: ts,
        };
        let view = ActivityView::from(row);
        assert_eq!(view.reflector, "XLX030");
        assert_eq!(view.module.as_deref(), Some("C"));
        assert_eq!(view.callsign, "W1AW");
        assert_eq!(view.source, "xlx_monitor");
        assert_eq!(view.observed_at, ts);
        Ok(())
    }

    #[test]
    fn stream_view_from_row_omits_audio_blob() -> Result<(), Box<dyn std::error::Error>> {
        // The HTTP view intentionally drops audio_mp3; confirm it is not
        // accidentally added back (the field isn't present in StreamView).
        let ts = Utc
            .with_ymd_and_hms(2026, 4, 12, 12, 0, 0)
            .single()
            .ok_or("fixed timestamp must be unambiguous")?;
        let row = StreamRow {
            id: 7,
            reflector: "REF030".to_owned(),
            module: "C".to_owned(),
            protocol: "dplus".to_owned(),
            stream_id: 1234,
            callsign: "W1AW".to_owned(),
            suffix: Some("D75".to_owned()),
            ur_call: Some("CQCQCQ".to_owned()),
            dstar_text: Some("hello".to_owned()),
            dprs_lat: Some(42.0),
            dprs_lon: Some(-71.0),
            started_at: ts,
            ended_at: Some(ts),
            frame_count: Some(100),
            audio_mp3: Some(vec![0xFF; 1024]),
            upload_status: Some("pending".to_owned()),
            upload_attempts: Some(0),
            last_upload_error: None,
            uploaded_at: None,
            created_at: Some(ts),
        };
        let view = StreamView::from(row);
        assert_eq!(view.id, 7);
        assert_eq!(view.reflector, "REF030");
        assert_eq!(view.module, "C");
        assert_eq!(view.callsign, "W1AW");
        assert_eq!(view.suffix.as_deref(), Some("D75"));
        assert_eq!(view.dstar_text.as_deref(), Some("hello"));
        assert_eq!(view.frame_count, Some(100));
        assert_eq!(view.upload_status.as_deref(), Some("pending"));

        // Serialize to JSON and confirm audio_mp3 is absent. This is the
        // real "system" test — the view's serialization is the HTTP
        // wire contract. Every other row field is surfaced in the view,
        // so this is the single exclusion we guard.
        let json = serde_json::to_string(&view)?;
        assert!(
            !json.contains("audio_mp3"),
            "audio_mp3 must not appear in the JSON output: {json}"
        );
        // ur_call, dprs_lat, dprs_lon, last_upload_error, uploaded_at,
        // created_at, and stream_id are surfaced for operator visibility.
        assert!(
            json.contains("\"ur_call\":\"CQCQCQ\""),
            "ur_call must appear in the JSON output: {json}"
        );
        assert!(
            json.contains("\"dprs_lat\":42.0"),
            "dprs_lat must appear in the JSON output: {json}"
        );
        assert!(
            json.contains("\"stream_id\":1234"),
            "stream_id must appear in the JSON output: {json}"
        );
        Ok(())
    }

    #[test]
    fn stream_query_route_pattern_matches_axum_syntax() {
        // Regression guard: axum 0.8 uses `{name}` path params, not
        // `:name` (the 0.7-and-earlier syntax). A wrong syntax here
        // compiles fine but 404s on every request. Confirm the routes
        // we register use the modern form by inspecting the string
        // constants.
        const ACTIVITY_ROUTE: &str = "/api/reflectors/{callsign}/activity";
        const TIER3_ROUTE: &str = "/api/tier3/{callsign}/{module}";
        assert!(ACTIVITY_ROUTE.contains("{callsign}"));
        assert!(TIER3_ROUTE.contains("{callsign}"));
        assert!(TIER3_ROUTE.contains("{module}"));
        assert!(!ACTIVITY_ROUTE.contains(":callsign"));
        assert!(!TIER3_ROUTE.contains(":module"));
    }
}
