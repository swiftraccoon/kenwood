//! `SDRTrunk`-compatible Rdio API multipart upload client.
//!
//! This module implements the wire-level protocol used by the open-source
//! `SDRTrunk` radio-recording tool to push completed call audio into a
//! [Rdio Scanner](https://github.com/chuot/rdio-scanner) ingest server. The
//! user's `sdrtrunk-rdio-api` Python service speaks the same protocol;
//! stargazer impersonates `SDRTrunk` so that existing tooling does not need
//! to grow a stargazer-specific code path.
//!
//! # Endpoint
//!
//! The caller configures a single base URL (default
//! `http://rdio-api:8080/api/call-upload`). Every completed voice stream is
//! `POST`ed to this URL as `multipart/form-data`.
//!
//! # Required request headers
//!
//! | Header | Value | Why |
//! |---|---|---|
//! | `User-Agent` | `sdrtrunk` | Some Rdio deployments gate ingest on the UA string — `sdrtrunk-rdio-api` accepts the literal `"sdrtrunk"` token. |
//!
//! The `Content-Type: multipart/form-data; boundary=...` header is set
//! automatically by [`reqwest::multipart::Form`]; we do not build it by hand.
//!
//! # Form fields
//!
//! The server accepts the same field set that `SDRTrunk` emits when
//! forwarding a trunked call. Text fields are UTF-8 strings; the `audio`
//! part is a binary MP3 blob with an explicit filename and `audio/mpeg`
//! content-type.
//!
//! | Field | Type | Example | Purpose |
//! |---|---|---|---|
//! | `key` | text | `stargazer-key` | API key configured via `rdio.api_key`. |
//! | `system` | text | `10030` | Numeric system id — see [`crate::upload::compute_system_id`]. |
//! | `systemLabel` | text | `REF030 (DPlus)` | Human-readable system name. |
//! | `talkgroup` | text | `3` | Numeric talkgroup id (`A` → `1`, `B` → `2`, ...). |
//! | `talkgroupLabel` | text | `Module C` | Human-readable talkgroup name. |
//! | `talkgroupGroup` | text | `D-STAR` | Fixed group label. |
//! | `source` | text | `W1AW` | Operator callsign. The user confirmed their Rdio fork accepts alphanumeric callsigns in this field (upstream expects a numeric radio id). |
//! | `talkerAlias` | text | `W1AW / D75` | Callsign + optional suffix. |
//! | `frequency` | text | `0` | Always zero — reflectors have no RF carrier. |
//! | `dateTime` | text | `1760289000` | Unix seconds at start of transmission. |
//! | `audio` | file | `audio/mpeg` MP3 bytes | The recorded call. Filename follows [`crate::upload::make_audio_name`]. |
//! | `patches` | text | `[]` | Literal empty-array JSON — no patches. |
//! | `talkgroupTag` | text | D-STAR text message, or `""` | Free-form 20-char slow-data message, passed through verbatim. |
//!
//! # Success signalling
//!
//! Rdio Scanner returns HTTP 200 with the literal string
//! `"Call imported successfully."` embedded in the response body when ingest
//! succeeds. This client reads the full body and checks for that substring
//! via `str::contains`. Any other outcome — non-2xx status, missing marker,
//! network error — becomes an [`UploadError`] variant so the caller can
//! decide whether to retry.

use reqwest::Client;
use reqwest::multipart::{Form, Part};

/// Marker substring that the Rdio Scanner API includes in a successful
/// response body. Verified against the upstream source at
/// <https://github.com/chuot/rdio-scanner>.
const SUCCESS_MARKER: &str = "Call imported successfully.";

/// MIME type for the uploaded MP3 blob.
const AUDIO_MIME: &str = "audio/mpeg";

/// `User-Agent` header value. Must be the literal `"sdrtrunk"` so that
/// `sdrtrunk-rdio-api` routes the request through its ingest pipeline.
const USER_AGENT: &str = "sdrtrunk";

/// Errors returned by [`upload_stream`].
///
/// Every variant is non-fatal at the process level — the upload loop
/// decides whether to retry or mark the row as permanently failed based on
/// the error kind and the per-row attempt counter.
#[derive(Debug, thiserror::Error)]
pub(crate) enum UploadError {
    /// The HTTP transport itself failed (DNS, TCP, TLS, read/write, or an
    /// error surfaced by reqwest while reading the response body).
    #[error("HTTP transport error: {0}")]
    Http(#[from] reqwest::Error),

    /// Constructing a [`reqwest::multipart::Part`] failed. In practice this
    /// only fires for invalid filename characters; we keep it as a distinct
    /// variant so the upload loop can log the offending field and move on.
    #[error("multipart form build error: {0}")]
    Build(String),

    /// The server returned HTTP 2xx but the body did not contain the
    /// `"Call imported successfully."` marker. This usually means the
    /// server silently rejected the upload (e.g. duplicate call id or
    /// malformed field) — retrying will not help.
    #[error("unexpected response body: {0}")]
    UnexpectedResponse(String),

    /// The server returned a non-2xx HTTP status. The body is included for
    /// operator diagnosis; very large bodies are truncated by the caller.
    #[error("server error: status={status} body={body}")]
    ServerError {
        /// HTTP status code (4xx or 5xx).
        status: u16,
        /// Response body (may be truncated).
        body: String,
    },
}

/// Parameters for a single Rdio Scanner upload.
///
/// Grouped into a struct because the wire format has a dozen distinct
/// fields and Clippy's `too_many_arguments` lint (denied in this crate)
/// would otherwise force an `#[expect]` attribute.
#[derive(Debug)]
pub(crate) struct UploadFields<'a> {
    /// API key sent in the `key` form field.
    pub(crate) api_key: &'a str,
    /// Numeric system id (see [`crate::upload::compute_system_id`]).
    pub(crate) system: &'a str,
    /// Human-readable system label, e.g. `"REF030 (DPlus)"`.
    pub(crate) system_label: &'a str,
    /// Numeric talkgroup id (`1`..`26`, from the module letter).
    pub(crate) talkgroup: &'a str,
    /// Human-readable talkgroup label, e.g. `"Module C"`.
    pub(crate) talkgroup_label: &'a str,
    /// Raw operator callsign.
    pub(crate) source: &'a str,
    /// Callsign + optional suffix, e.g. `"W1AW / D75"`.
    pub(crate) talker_alias: &'a str,
    /// Optional D-STAR slow-data text message (20 chars max).
    pub(crate) talkgroup_tag: Option<&'a str>,
    /// Unix seconds at the start of the transmission.
    pub(crate) date_time: i64,
    /// Filename for the `audio` file part (see
    /// [`crate::upload::make_audio_name`]).
    pub(crate) audio_name: &'a str,
    /// MP3 bytes.
    pub(crate) audio_mp3: Vec<u8>,
}

/// Uploads one completed voice stream to an `SDRTrunk`-compatible Rdio
/// Scanner endpoint.
///
/// Builds the multipart form from `fields`, POSTs it to `endpoint` with
/// `User-Agent: sdrtrunk`, and verifies the response body contains the
/// `"Call imported successfully."` marker. Any deviation (network error,
/// non-2xx status, missing marker) becomes an [`UploadError`].
///
/// # Errors
///
/// - [`UploadError::Http`] — transport/DNS/TLS/read failure.
/// - [`UploadError::Build`] — multipart part construction failed (invalid
///   filename).
/// - [`UploadError::ServerError`] — non-2xx HTTP status.
/// - [`UploadError::UnexpectedResponse`] — 2xx status but success marker
///   absent from the body.
pub(crate) async fn upload_stream(
    client: &Client,
    endpoint: &str,
    fields: UploadFields<'_>,
) -> Result<(), UploadError> {
    // The text fields map 1:1 onto the columns documented in the module
    // header. We build them first so the file part (which moves the MP3
    // blob) can be appended last.
    let talkgroup_tag = fields.talkgroup_tag.unwrap_or("");
    let mut form = Form::new()
        .text("key", fields.api_key.to_owned())
        .text("system", fields.system.to_owned())
        .text("systemLabel", fields.system_label.to_owned())
        .text("talkgroup", fields.talkgroup.to_owned())
        .text("talkgroupLabel", fields.talkgroup_label.to_owned())
        .text("talkgroupGroup", "D-STAR")
        .text("source", fields.source.to_owned())
        .text("talkerAlias", fields.talker_alias.to_owned())
        .text("frequency", "0")
        .text("dateTime", fields.date_time.to_string())
        .text("patches", "[]")
        .text("talkgroupTag", talkgroup_tag.to_owned());

    // `Part::bytes` never fails; only `mime_str` and `file_name` can fail
    // at part-build time, and only for non-UTF-8 MIME strings or filenames
    // with invalid header bytes. We surface both via UploadError::Build.
    let audio_part = Part::bytes(fields.audio_mp3)
        .file_name(fields.audio_name.to_owned())
        .mime_str(AUDIO_MIME)
        .map_err(|e| UploadError::Build(format!("audio part mime: {e}")))?;
    form = form.part("audio", audio_part);

    // POST with the sdrtrunk UA — see module-level docs for the rationale.
    let response = client
        .post(endpoint)
        .header(reqwest::header::USER_AGENT, USER_AGENT)
        .multipart(form)
        .send()
        .await?;

    let status = response.status();
    let body = response.text().await?;

    if !status.is_success() {
        // Truncate very large bodies (e.g. an HTML error page) to keep the
        // error log readable; 512 bytes is plenty to show the server's
        // human-readable failure message.
        let truncated = truncate_body(&body, 512);
        return Err(UploadError::ServerError {
            status: status.as_u16(),
            body: truncated,
        });
    }

    if body.contains(SUCCESS_MARKER) {
        Ok(())
    } else {
        Err(UploadError::UnexpectedResponse(truncate_body(&body, 512)))
    }
}

/// Returns `body` truncated to at most `max_chars` UTF-8 characters, with
/// an explicit `"... (truncated)"` suffix appended if truncation occurred.
///
/// Used when surfacing server response bodies in error variants — a
/// server that returns a 5 MB HTML error page should not flood the logs.
fn truncate_body(body: &str, max_chars: usize) -> String {
    let char_count = body.chars().count();
    if char_count <= max_chars {
        body.to_owned()
    } else {
        let prefix: String = body.chars().take(max_chars).collect();
        format!("{prefix}... (truncated, {char_count} chars total)")
    }
}

#[cfg(test)]
mod tests {
    use super::{SUCCESS_MARKER, truncate_body};

    #[test]
    fn truncate_body_short_returns_unchanged() {
        let body = "short body";
        let result = truncate_body(body, 512);
        assert_eq!(result, body, "body under the cap must be returned verbatim");
    }

    #[test]
    fn truncate_body_long_adds_truncation_marker() {
        let body = "a".repeat(1024);
        let result = truncate_body(&body, 16);
        assert!(
            result.starts_with(&"a".repeat(16)),
            "truncation must preserve the leading chars, got: {result}"
        );
        assert!(
            result.contains("truncated"),
            "truncated bodies must say so, got: {result}"
        );
        assert!(
            result.contains("1024"),
            "truncated bodies must include the original length, got: {result}"
        );
    }

    #[test]
    fn truncate_body_respects_utf8_char_boundaries() {
        // Four-byte UTF-8 codepoint ("🎶" = U+1F3B6) repeated enough times
        // that a naive byte-slice would split mid-codepoint.
        let body = "🎶".repeat(100);
        let result = truncate_body(&body, 10);
        // Must not panic; must still be valid UTF-8.
        assert!(
            result.starts_with(&"🎶".repeat(10)),
            "utf-8 truncation must keep whole codepoints, got: {result}"
        );
    }

    #[test]
    fn success_marker_is_the_sdrtrunk_literal() {
        // Documented value from <https://github.com/chuot/rdio-scanner>;
        // regressing this constant silently would break every upload.
        assert_eq!(SUCCESS_MARKER, "Call imported successfully.");
    }
}
