//! Query functions for the stream upload queue.
//!
//! The upload queue is not a separate table; it operates on the `streams` table
//! using the `upload_status`, `upload_attempts`, and `last_upload_error` columns
//! to track the lifecycle of each stream's upload to the Rdio API.
//!
//! # Upload Lifecycle
//!
//! ```text
//! pending ──► uploaded    (success: mark_uploaded)
//!    │
//!    ├──► pending         (transient failure: increment_attempts)
//!    │
//!    └──► failed          (permanent failure or max retries: mark_failed)
//! ```
//!
//! The upload processor polls [`get_pending`] on a timer, attempts each upload,
//! and calls the appropriate status-transition function based on the result.
//! Only streams with non-null `audio_mp3` are returned by [`get_pending`],
//! ensuring that in-progress streams (still recording) are not prematurely
//! queued.

use chrono::Utc;
use sqlx::PgPool;

use super::streams::StreamRow;

/// Returns streams awaiting upload, ordered by creation time.
///
/// Filters for `upload_status = 'pending'` AND `audio_mp3 IS NOT NULL` (the
/// stream must have finished recording and MP3 encoding). Results are capped
/// at `limit` rows and ordered oldest-first so that the upload processor
/// works through the backlog in FIFO order.
///
/// # Errors
///
/// Returns `sqlx::Error` on query failure.
pub(crate) async fn get_pending(pool: &PgPool, limit: i64) -> Result<Vec<StreamRow>, sqlx::Error> {
    // Only select streams that have completed MP3 encoding (audio_mp3 IS NOT
    // NULL) and are still in pending state. Ordered by created_at ASC for FIFO.
    sqlx::query_as::<_, StreamRow>(
        "SELECT id, reflector, module, protocol, stream_id, callsign,
                suffix, ur_call, dstar_text, dprs_lat, dprs_lon,
                started_at, ended_at, frame_count, audio_mp3,
                upload_status, upload_attempts, last_upload_error,
                uploaded_at, created_at
         FROM streams
         WHERE upload_status = 'pending' AND audio_mp3 IS NOT NULL
         ORDER BY created_at ASC
         LIMIT $1",
    )
    .bind(limit)
    .fetch_all(pool)
    .await
}

/// Marks a stream as successfully uploaded.
///
/// Sets `upload_status = 'uploaded'` and records the current timestamp in
/// `uploaded_at`. Called by the upload processor after a successful Rdio API
/// response.
///
/// # Errors
///
/// Returns `sqlx::Error` on query failure.
pub(crate) async fn mark_uploaded(pool: &PgPool, id: i64) -> Result<(), sqlx::Error> {
    // Transition to terminal 'uploaded' state with timestamp.
    let _result = sqlx::query(
        "UPDATE streams
         SET upload_status = 'uploaded', uploaded_at = $1
         WHERE id = $2",
    )
    .bind(Utc::now())
    .bind(id)
    .execute(pool)
    .await?;
    Ok(())
}

/// Marks a stream as permanently failed.
///
/// Sets `upload_status = 'failed'` and records the error message. Called when
/// the upload processor decides not to retry (e.g., max attempts exceeded or
/// a non-retryable HTTP status).
///
/// # Errors
///
/// Returns `sqlx::Error` on query failure.
pub(crate) async fn mark_failed(pool: &PgPool, id: i64, error: &str) -> Result<(), sqlx::Error> {
    // Transition to terminal 'failed' state with error message.
    let _result = sqlx::query(
        "UPDATE streams
         SET upload_status = 'failed', last_upload_error = $1
         WHERE id = $2",
    )
    .bind(error)
    .bind(id)
    .execute(pool)
    .await?;
    Ok(())
}

/// Increments the attempt counter and records the error for a retryable failure.
///
/// The stream stays in `upload_status = 'pending'` so it will be picked up
/// again on the next poll cycle. The caller is responsible for checking
/// `upload_attempts` against the configured maximum and calling [`mark_failed`]
/// when retries are exhausted.
///
/// # Errors
///
/// Returns `sqlx::Error` on query failure.
pub(crate) async fn increment_attempts(
    pool: &PgPool,
    id: i64,
    error: &str,
) -> Result<(), sqlx::Error> {
    // Bump attempt counter and record the latest error, but leave status as
    // 'pending' so the upload processor retries on the next cycle.
    let _result = sqlx::query(
        "UPDATE streams
         SET upload_attempts = upload_attempts + 1, last_upload_error = $1
         WHERE id = $2",
    )
    .bind(error)
    .bind(id)
    .execute(pool)
    .await?;
    Ok(())
}
