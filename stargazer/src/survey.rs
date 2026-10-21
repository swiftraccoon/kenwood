// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later

//! Bounded HTTP activity polling and an append-only survey archive.
//!
//! Responses are published before parsing. Event identity is committed only
//! after its JSON line is flushed. Poll starts are separated by at least
//! 30 seconds, including cancelled or failed attempts. The feed is an observed
//! window, so the archive and ranking describe received rows rather than a
//! complete network census.

use std::collections::{HashMap, HashSet};
use std::fs::{File, OpenOptions};
use std::io::{self, BufRead as _, Read as _, Seek as _, Write as _};
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::{DateTime, Utc};
use dstar_gateway_core::RoutingCallsign;
use serde::{Deserialize, Serialize};

use crate::http::HttpResponse;
use crate::writer::{WriteError, WriteOperation};

/// The DPLUSMON query endpoint (unfiltered: all gateways).
const FEED_URL: &str = "https://nj6n.com/dplusmon/query-dplusdb.php?mycall=&gateway=";

/// Minimum number of seconds between poll starts, an application service policy.
pub const MIN_INTERVAL_SECS: u64 = 30;

/// Positive resource limits for network responses, archived lines and identities.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SurveyLimits {
    max_seen: NonZeroUsize,
    response_bytes: NonZeroUsize,
    line_bytes: NonZeroUsize,
}

impl SurveyLimits {
    /// Bound retained identities; default response and JSON-line bounds are 1 MiB.
    #[must_use]
    pub const fn new(max_seen: NonZeroUsize) -> Self {
        Self {
            max_seen,
            response_bytes: const { NonZeroUsize::new(1_048_576).unwrap() },
            line_bytes: const { NonZeroUsize::new(1_048_576).unwrap() },
        }
    }

    /// Set the maximum bytes accepted in one complete HTTP response.
    #[must_use]
    pub const fn with_response_bytes(mut self, maximum: NonZeroUsize) -> Self {
        self.response_bytes = maximum;
        self
    }

    /// Set the maximum JSON bytes per archive line, excluding its newline.
    #[must_use]
    pub const fn with_line_bytes(mut self, maximum: NonZeroUsize) -> Self {
        self.line_bytes = maximum;
        self
    }

    /// Return the maximum distinct archived identities retained in memory.
    #[must_use]
    pub const fn max_seen(self) -> NonZeroUsize {
        self.max_seen
    }

    /// Return the maximum response body size.
    #[must_use]
    pub const fn response_bytes(self) -> NonZeroUsize {
        self.response_bytes
    }

    /// Return the maximum JSON bytes per line.
    #[must_use]
    pub const fn line_bytes(self) -> NonZeroUsize {
        self.line_bytes
    }
}

/// Event identity independent of poll time, with unambiguous field boundaries.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ActivityKey {
    timestamp: DateTime<Utc>,
    gateway: String,
    mycall: String,
    rpt1: String,
}

/// One transmission event parsed from the feed.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ActivityEvent {
    /// Transmission timestamp (UTC) as reported by the feed.
    pub ts: DateTime<Utc>,
    /// Reporting gateway (a reflector name or a gateway callsign).
    pub gateway: String,
    /// Transmitting station as displayed (may carry a note suffix).
    pub mycall: String,
    /// UR field.
    pub urcall: String,
    /// Reflector the transmission used, if any (e.g. `"REF030"`).
    pub reflector: Option<String>,
    /// Reflector module, if any.
    pub module: Option<char>,
    /// RPT1 as displayed.
    pub rpt1: String,
    /// RPT2 as displayed.
    pub rpt2: String,
    /// When the poll first observed this event.
    pub polled_at: DateTime<Utc>,
}

impl ActivityEvent {
    /// Stable dedupe key across polls.
    #[must_use]
    pub fn key(&self) -> ActivityKey {
        ActivityKey::from(self)
    }
}

impl From<&ActivityEvent> for ActivityKey {
    fn from(event: &ActivityEvent) -> Self {
        Self {
            timestamp: event.ts,
            gateway: event.gateway.clone(),
            mycall: event.mycall.clone(),
            rpt1: event.rpt1.clone(),
        }
    }
}

/// Provenance record for one poll of the feed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PollRecord {
    /// When the poll ran.
    pub polled_at: DateTime<Utc>,
    /// HTTP status, if a response arrived.
    pub http_status: Option<u16>,
    /// Response body size in bytes.
    pub bytes: usize,
    /// Rows the parser recognized.
    pub rows: usize,
    /// Rows the parser skipped (unexpected cell count).
    pub parse_skips: usize,
    /// Events not seen in any earlier poll.
    pub new_events: usize,
    /// Oldest row timestamp in this window.
    pub window_oldest: Option<DateTime<Utc>>,
    /// Newest row timestamp in this window.
    pub window_newest: Option<DateTime<Utc>>,
    /// True when the window may have overflowed since the previous
    /// poll (no overlap: every row is newer than the previous poll's
    /// newest row); rows may have been missed.
    pub gap_risk: bool,
    /// Fetch/parse error, if the poll failed.
    error: Option<String>,
    /// Live typed fetch/status failure; historical JSON retains the error text.
    #[serde(skip)]
    failure: Option<std::sync::Arc<PollFailure>>,
}

impl PollRecord {
    /// Return the persisted failure description, if any.
    #[must_use]
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// Return the typed failure for a poll performed by this process.
    #[must_use]
    pub fn failure(&self) -> Option<&PollFailure> {
        self.failure.as_deref()
    }
}

/// A network failure retained alongside the poll's serializable description.
#[derive(Debug, thiserror::Error)]
pub enum PollFailure {
    /// Request, response-size or allocation failure.
    #[error("fetch: {0}")]
    Fetch(#[source] crate::FetchError),
    /// A complete response had a non-success status.
    #[error("HTTP status {0}")]
    HttpStatus(reqwest::StatusCode),
}

/// Strip HTML tags and decode the entities the feed uses.
fn strip_tags(cell: &str) -> String {
    let mut out = String::with_capacity(cell.len());
    let mut in_tag = false;
    for c in cell.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            c if !in_tag => out.push(c),
            _ => {}
        }
    }
    out.replace("&nbsp;", " ")
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .trim()
        .to_string()
}

/// Split one `<tr>` chunk into its `<td>` cell texts.
fn row_cells(row: &str) -> Vec<String> {
    let mut cells = Vec::new();
    let mut rest = row;
    while let Some(start) = rest.find("<td") {
        let Some(open_end) = rest.get(start..).and_then(|s| s.find('>')) else {
            break;
        };
        let content_start = start + open_end + 1;
        let Some(len) = rest.get(content_start..).and_then(|s| s.find("</td>")) else {
            break;
        };
        if let Some(cell) = rest.get(content_start..content_start + len) {
            cells.push(strip_tags(cell));
        }
        rest = rest.get(content_start + len + 5..).unwrap_or("");
    }
    cells
}

/// Parse the feed's `"YYYY-MM-DD HH:MM:SS UTC"` timestamps.
fn parse_feed_ts(s: &str) -> Option<DateTime<Utc>> {
    let trimmed = s.trim().trim_end_matches(" UTC");
    chrono::NaiveDateTime::parse_from_str(trimmed, "%Y-%m-%d %H:%M:%S")
        .ok()
        .map(|naive| naive.and_utc())
}

/// Split a `"REF030 C"` reflector cell into name and module.
fn split_reflector(cell: &str) -> (Option<String>, Option<char>) {
    let t = cell.trim();
    if t.is_empty() {
        return (None, None);
    }
    let mut parts = t.split_whitespace();
    let name = parts.next().map(str::to_string);
    let module = parts.next().and_then(|m| {
        let mut chars = m.chars();
        match (chars.next(), chars.next()) {
            (Some(c), None) => Some(c),
            _ => None,
        }
    });
    (name, module)
}

/// Result of parsing one feed response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedFeed {
    /// Recognized events, newest first (feed order).
    pub events: Vec<ActivityEvent>,
    /// Rows skipped for unexpected shape.
    pub parse_skips: usize,
}

impl ParsedFeed {
    /// Parse the recognized DPLUSMON table grammar into events and skip counts.
    #[must_use]
    pub fn from_html(html: &str, polled_at: DateTime<Utc>) -> Self {
        let mut events = Vec::new();
        let mut parse_skips = 0usize;
        for row in html.split("<tr>").skip(1) {
            let cells = row_cells(row);
            if cells.is_empty() {
                continue; // header row uses <th>, not <td>
            }
            if cells.len() != 7 {
                parse_skips += 1;
                continue;
            }
            let Some(ts) = parse_feed_ts(cells.first().map_or("", String::as_str)) else {
                parse_skips += 1;
                continue;
            };
            let cell = |i: usize| cells.get(i).cloned().unwrap_or_default();
            let (reflector, module) = split_reflector(&cell(4));
            events.push(ActivityEvent {
                ts,
                gateway: cell(1),
                mycall: cell(2),
                urcall: cell(3),
                reflector,
                module,
                rpt1: cell(5),
                rpt2: cell(6),
                polled_at,
            });
        }
        Self {
            events,
            parse_skips,
        }
    }
}

/// On-disk layout for the survey archive.
#[derive(Debug, Clone)]
pub struct Archive {
    base: PathBuf,
}

impl Archive {
    /// Root the archive at `<base>/dplusmon`.
    #[must_use]
    pub fn new(base: &Path) -> Self {
        Self {
            base: base.join("dplusmon"),
        }
    }

    /// Path of the append-only event log.
    #[must_use]
    pub fn activity_path(&self) -> PathBuf {
        self.base.join("activity.jsonl")
    }

    /// Path of the append-only poll provenance log.
    #[must_use]
    pub fn polls_path(&self) -> PathBuf {
        self.base.join("polls.jsonl")
    }

    /// Path for one raw response, date-partitioned by poll time.
    #[must_use]
    pub fn raw_path(&self, polled_at: DateTime<Utc>) -> PathBuf {
        self.base
            .join("raw")
            .join(polled_at.format("%Y-%m-%d").to_string())
            .join(format!("{}.html", polled_at.format("%Y%m%dT%H%M%SZ")))
    }

    /// Publish response bytes before parsing, without replacing an earlier file.
    ///
    /// # Errors
    ///
    /// Returns publication errors or [`ArchiveError::NameExhausted`] after 1000
    /// occupied variants. File bytes are flushed before publication; containing
    /// directories are synced on Unix. Other platforms omit directory syncing.
    pub fn store_raw(
        &self,
        polled_at: DateTime<Utc>,
        body: &[u8],
    ) -> Result<PathBuf, ArchiveError> {
        let nominal = self.raw_path(polled_at);
        crate::writer::create_directories(
            nominal.parent().unwrap_or_else(|| Path::new(".")),
            &mut |_, _| Ok(()),
        )?;
        for variant in 0..1000 {
            let path = if variant == 0 {
                nominal.clone()
            } else {
                nominal.with_file_name(format!(
                    "{}-{variant}.html",
                    polled_at.format("%Y%m%dT%H%M%SZ")
                ))
            };
            match crate::writer::publish_bytes(&path, body, &mut |_, _| Ok(())) {
                Ok(()) => return Ok(path),
                Err(WriteError::Io {
                    source,
                    operation: WriteOperation::CreateTemporary | WriteOperation::Publish,
                    ..
                }) if source.kind() == io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(ArchiveError::Publication(error)),
            }
        }
        Err(ArchiveError::NameExhausted(nominal))
    }

    fn append_line(
        path: &Path,
        value: &impl Serialize,
        maximum: NonZeroUsize,
    ) -> Result<(), ArchiveError> {
        let mut line = BoundedLine {
            bytes: Vec::new(),
            maximum,
            exceeded: false,
        };
        if let Err(source) = serde_json::to_writer(&mut line, value) {
            return Err(if line.exceeded {
                ArchiveError::LineTooLong {
                    path: path.to_owned(),
                    maximum,
                }
            } else {
                ArchiveError::Serialize {
                    path: path.to_owned(),
                    source,
                }
            });
        }
        append_bytes(path, &line.bytes, io::Write::write_all)
    }

    /// Append and flush one complete event, bounded by `maximum` JSON bytes.
    ///
    /// # Errors
    ///
    /// Returns serialization, lock or I/O failures. Failed writes are truncated
    /// back to the original length and synced. A rollback failure is returned
    /// with both errors; the next append rejects an unterminated tail.
    pub fn append_event(
        &self,
        event: &ActivityEvent,
        maximum: NonZeroUsize,
    ) -> Result<(), ArchiveError> {
        Self::append_line(&self.activity_path(), event, maximum)
    }

    /// Append one poll provenance record.
    ///
    /// # Errors
    ///
    /// Returns the same bounded, locked append failures as [`Self::append_event`].
    pub fn append_poll(
        &self,
        poll: &PollRecord,
        maximum: NonZeroUsize,
    ) -> Result<(), ArchiveError> {
        Self::append_line(&self.polls_path(), poll, maximum)
    }

    /// Stream an activity-log snapshot with at most `maximum` JSON bytes per line.
    ///
    /// # Errors
    ///
    /// Returns opening, locking or metadata errors. Each iterator item reports
    /// malformed, oversized or unterminated lines with its path and line number.
    /// A missing log produces an empty iterator. Appends after snapshot creation
    /// are excluded. The shared lock is held only while selecting the file length.
    pub fn events(&self, maximum: NonZeroUsize) -> Result<EventLines, ArchiveError> {
        let path = self.activity_path();
        let reader = match File::open(&path) {
            Ok(file) => {
                file.try_lock_shared()
                    .map_err(|source| ArchiveError::Lock {
                        path: path.clone(),
                        source,
                    })?;
                let length = file.metadata().map_err(archive_io(&path))?.len();
                file.unlock().map_err(archive_io(&path))?;
                Some(io::BufReader::new(file.take(length)))
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            Err(source) => return Err(ArchiveError::Io { path, source }),
        };
        Ok(EventLines {
            path,
            reader,
            maximum,
            line: 0,
            finished: false,
        })
    }
}

/// Archive operation failure; lock contention returns immediately.
#[derive(Debug, thiserror::Error)]
pub enum ArchiveError {
    /// Filesystem operation failed at the named path.
    #[error("archive {path}: {source}")]
    Io {
        /// Archive path.
        path: PathBuf,
        /// Underlying filesystem error.
        #[source]
        source: io::Error,
    },
    /// Another reader or writer holds an incompatible file lock.
    #[error("archive lock {path}: {source}")]
    Lock {
        /// Archive path.
        path: PathBuf,
        /// Lock acquisition error.
        #[source]
        source: std::fs::TryLockError,
    },
    /// A value could not be encoded within the selected line bound.
    #[error("serialize archive {path}: {source}")]
    Serialize {
        /// Archive path.
        path: PathBuf,
        /// Serializer or bounded-writer error.
        #[source]
        source: serde_json::Error,
    },
    /// Serialized JSON would exceed the selected line bound.
    #[error("archive {path} exceeds {maximum} JSON bytes per line")]
    LineTooLong {
        /// Archive path.
        path: PathBuf,
        /// Selected maximum.
        maximum: NonZeroUsize,
    },
    /// Existing data ends without the newline required by an append transaction.
    #[error("archive {0} has an unterminated final line")]
    Unterminated(PathBuf),
    /// Failed append could not be rolled back to its starting length.
    #[error("archive {path}: append failed ({write}); rollback failed ({rollback})")]
    Rollback {
        /// Archive path.
        path: PathBuf,
        /// Initial write or sync failure.
        #[source]
        write: io::Error,
        /// Truncate or rollback-sync failure.
        rollback: io::Error,
    },
    /// Raw snapshot publication failed.
    #[error(transparent)]
    Publication(#[from] WriteError),
    /// All 1000 raw-snapshot filename variants were occupied.
    #[error("raw snapshot names exhausted for {0}")]
    NameExhausted(PathBuf),
}

fn archive_io(path: &Path) -> impl FnOnce(io::Error) -> ArchiveError + '_ {
    |source| ArchiveError::Io {
        path: path.to_owned(),
        source,
    }
}

struct BoundedLine {
    bytes: Vec<u8>,
    maximum: NonZeroUsize,
    exceeded: bool,
}

impl io::Write for BoundedLine {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self
            .bytes
            .len()
            .checked_add(bytes.len())
            .is_none_or(|len| len > self.maximum.get())
        {
            self.exceeded = true;
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "JSON line exceeds selected byte limit",
            ));
        }
        self.bytes
            .try_reserve(bytes.len())
            .map_err(io::Error::other)?;
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn append_bytes(
    path: &Path,
    bytes: &[u8],
    mut write: impl FnMut(&mut File, &[u8]) -> io::Result<()>,
) -> Result<(), ArchiveError> {
    let directory = path.parent().unwrap_or_else(|| Path::new("."));
    crate::writer::create_directories(directory, &mut |_, _| Ok(()))?;
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .map_err(archive_io(path))?;
    file.try_lock().map_err(|source| ArchiveError::Lock {
        path: path.to_owned(),
        source,
    })?;
    #[cfg(unix)]
    crate::writer::sync_directory(directory, WriteOperation::SyncDirectory, &mut |_, _| Ok(()))?;
    let original = file.seek(io::SeekFrom::End(0)).map_err(archive_io(path))?;
    if original != 0 {
        let _position = file.seek(io::SeekFrom::End(-1)).map_err(archive_io(path))?;
        let mut tail = [0];
        file.read_exact(&mut tail).map_err(archive_io(path))?;
        if tail != [b'\n'] {
            return Err(ArchiveError::Unterminated(path.to_owned()));
        }
    }
    let result = write(&mut file, bytes)
        .and_then(|()| file.write_all(b"\n"))
        .and_then(|()| file.sync_all());
    if let Err(write) = result {
        if let Err(rollback) = file.set_len(original).and_then(|()| file.sync_all()) {
            return Err(ArchiveError::Rollback {
                path: path.to_owned(),
                write,
                rollback,
            });
        }
        return Err(ArchiveError::Io {
            path: path.to_owned(),
            source: write,
        });
    }
    Ok(())
}

/// A malformed or unreadable activity-log line.
#[derive(Debug, thiserror::Error)]
#[error("activity log {path}, line {line}: {reason}")]
pub struct EventReadError {
    path: PathBuf,
    line: u64,
    #[source]
    reason: EventReadReason,
}

impl EventReadError {
    /// Return the archive path.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
    /// Return the one-based line number.
    #[must_use]
    pub const fn line(&self) -> u64 {
        self.line
    }
    /// Return the structured line failure.
    #[must_use]
    pub const fn reason(&self) -> &EventReadReason {
        &self.reason
    }
}

/// Activity-log line failure category.
#[derive(Debug, thiserror::Error)]
pub enum EventReadReason {
    /// Filesystem read failed.
    #[error("read: {0}")]
    Io(#[source] io::Error),
    /// Reserving the bounded line buffer failed.
    #[error("allocation: {0}")]
    Allocation(#[source] std::collections::TryReserveError),
    /// JSON could not be decoded as an event.
    #[error("JSON: {0}")]
    Json(#[source] serde_json::Error),
    /// The line exceeded its selected JSON byte bound.
    #[error("line exceeds {maximum} JSON bytes")]
    TooLong {
        /// Selected maximum.
        maximum: NonZeroUsize,
    },
    /// The file snapshot ended before the committing newline.
    #[error("unterminated final line")]
    Unterminated,
}

/// A bounded, fallible activity-log snapshot iterator.
#[derive(Debug)]
pub struct EventLines {
    path: PathBuf,
    reader: Option<io::BufReader<io::Take<File>>>,
    maximum: NonZeroUsize,
    line: u64,
    finished: bool,
}

impl Iterator for EventLines {
    type Item = Result<ActivityEvent, EventReadError>;
    fn next(&mut self) -> Option<Self::Item> {
        if self.finished {
            return None;
        }
        let reader = self.reader.as_mut()?;
        let mut bytes = Vec::new();
        let result = loop {
            let available = match reader.fill_buf() {
                Ok(bytes) => bytes,
                Err(error) => {
                    self.finished = true;
                    break Err(EventReadReason::Io(error));
                }
            };
            if available.is_empty() {
                self.finished = true;
                if bytes.is_empty() {
                    return None;
                }
                break Err(EventReadReason::Unterminated);
            }
            let newline = available.iter().position(|byte| *byte == b'\n');
            let count = newline.unwrap_or(available.len());
            if bytes
                .len()
                .checked_add(count)
                .is_none_or(|len| len > self.maximum.get())
            {
                self.finished = true;
                break Err(EventReadReason::TooLong {
                    maximum: self.maximum,
                });
            }
            if let Err(error) = bytes.try_reserve(count) {
                self.finished = true;
                break Err(EventReadReason::Allocation(error));
            }
            bytes.extend_from_slice(available.get(..count).unwrap_or_default());
            reader.consume(count + usize::from(newline.is_some()));
            if newline.is_some() {
                break serde_json::from_slice(&bytes).map_err(EventReadReason::Json);
            }
        };
        self.line = self.line.saturating_add(1);
        Some(result.map_err(|reason| EventReadError {
            path: self.path.clone(),
            line: self.line,
            reason,
        }))
    }
}

impl std::iter::FusedIterator for EventLines {}

/// Survey failures.
#[derive(Debug, thiserror::Error)]
pub enum SurveyError {
    /// HTTP client configuration failed.
    #[error("HTTP client: {0}")]
    Client(#[source] reqwest::Error),
    /// The configured service endpoint could not be parsed.
    #[error("survey endpoint: {0}")]
    Endpoint(#[source] <reqwest::Url as std::str::FromStr>::Err),
    /// Archive publication or append failed.
    #[error(transparent)]
    Archive(#[from] ArchiveError),
    /// An existing activity-log line is invalid.
    #[error(transparent)]
    Event(#[from] EventReadError),
    /// The selected identity bound would be exceeded.
    #[error("survey identity limit reached ({maximum})")]
    SeenLimit {
        /// Selected maximum retained identities.
        maximum: NonZeroUsize,
    },
    /// A poll was attempted before the 30-second service interval elapsed.
    #[error("poll interval has {retry_after:?} remaining")]
    TooSoon {
        /// Time remaining before another poll may start.
        retry_after: Duration,
    },
    /// The next monotonic poll deadline cannot be represented.
    #[error("poll deadline cannot be represented")]
    Deadline,
    /// Reserving deduplication storage failed.
    #[error("identity allocation: {0}")]
    Allocation(#[source] std::collections::TryReserveError),
}

/// Stateful poller: fetches the feed, archives raw + events + poll
/// provenance, and deduplicates across polls.
#[derive(Debug)]
pub struct Surveyor {
    client: reqwest::Client,
    archive: Archive,
    seen: HashSet<ActivityKey>,
    last_newest: Option<DateTime<Utc>>,
    limits: SurveyLimits,
    next_poll: Option<tokio::time::Instant>,
    endpoint: reqwest::Url,
}

impl Surveyor {
    /// Create a poller rooted at the archive base directory, seeding
    /// the dedupe set from any existing archive.
    ///
    /// # Errors
    ///
    /// Returns client configuration, archive-line, allocation or identity-limit
    /// errors. Every archived event is read through a bounded line iterator.
    pub fn new(
        base: &Path,
        limits: SurveyLimits,
        operator: RoutingCallsign,
    ) -> Result<Self, SurveyError> {
        let archive = Archive::new(base);
        let mut seen = HashSet::new();
        let mut last_newest = None;
        for ev in archive.events(limits.line_bytes)? {
            let ev = ev?;
            if last_newest.is_none_or(|n| ev.ts > n) {
                last_newest = Some(ev.ts);
            }
            let key = ev.key();
            if !seen.contains(&key) {
                reserve_identity(&mut seen, limits.max_seen)?;
                let _inserted = seen.insert(key);
            }
        }
        let agent = format!(
            "stargazer-survey/{} (operator {operator})",
            env!("CARGO_PKG_VERSION")
        );
        let client =
            crate::http::client(&agent, Duration::from_secs(15)).map_err(SurveyError::Client)?;
        Ok(Self {
            client,
            archive,
            seen,
            last_newest,
            limits,
            next_poll: None,
            endpoint: FEED_URL.parse().map_err(SurveyError::Endpoint)?,
        })
    }

    /// Number of distinct events observed so far (all time).
    #[must_use]
    pub fn total_seen(&self) -> usize {
        self.seen.len()
    }

    /// Run one poll cycle: fetch, archive raw, parse, dedupe, append
    /// events, append provenance. Returns the poll record.
    ///
    /// # Errors
    ///
    /// Returns [`SurveyError::TooSoon`] within 30 seconds of a previous start,
    /// or archive/resource failures. Network failures are returned in the poll
    /// record, with a typed live source and persisted text. Cancellation retains
    /// the poll deadline and any events already flushed; cancelled downloads
    /// may lack a poll record. Local archive operations are synchronous.
    pub async fn poll_once(&mut self) -> Result<PollRecord, SurveyError> {
        self.reserve_poll(tokio::time::Instant::now())?;
        let polled_at = Utc::now();
        let mut record = PollRecord {
            polled_at,
            http_status: None,
            bytes: 0,
            rows: 0,
            parse_skips: 0,
            new_events: 0,
            window_oldest: None,
            window_newest: None,
            gap_risk: false,
            error: None,
            failure: None,
        };

        match self.fetch().await {
            Err(e) => {
                record.error = Some(e.to_string());
                record.failure = Some(std::sync::Arc::new(PollFailure::Fetch(e)));
            }
            Ok(HttpResponse { status, body }) => {
                record.http_status = Some(status.as_u16());
                record.bytes = body.len();
                // Archive-first: raw bytes land before parsing.
                let _raw_path = self.archive.store_raw(polled_at, &body)?;
                if status.is_success() {
                    let parsed = ParsedFeed::from_html(&String::from_utf8_lossy(&body), polled_at);
                    record.rows = parsed.events.len();
                    record.parse_skips = parsed.parse_skips;
                    record.window_oldest = parsed.events.iter().map(|e| e.ts).min();
                    record.window_newest = parsed.events.iter().map(|e| e.ts).max();
                    // Full window of unseen rows after a previous poll
                    // means the window may have rolled past us.
                    record.gap_risk = self.last_newest.is_some_and(|prev| {
                        !parsed.events.is_empty() && parsed.events.iter().all(|e| e.ts > prev)
                    });

                    record.new_events = self.commit_events(&parsed.events)?;
                    if let Some(newest) = record.window_newest
                        && self.last_newest.is_none_or(|n| newest > n)
                    {
                        self.last_newest = Some(newest);
                    }
                } else {
                    record.error = Some(format!("http status {status}"));
                    record.failure = Some(std::sync::Arc::new(PollFailure::HttpStatus(status)));
                }
            }
        }

        self.archive.append_poll(&record, self.limits.line_bytes)?;
        Ok(record)
    }

    fn reserve_poll(&mut self, now: tokio::time::Instant) -> Result<(), SurveyError> {
        if let Some(deadline) = self.next_poll
            && now < deadline
        {
            return Err(SurveyError::TooSoon {
                retry_after: deadline - now,
            });
        }
        self.next_poll = Some(
            now.checked_add(Duration::from_secs(MIN_INTERVAL_SECS))
                .ok_or(SurveyError::Deadline)?,
        );
        Ok(())
    }

    fn commit_events(&mut self, events: &[ActivityEvent]) -> Result<usize, SurveyError> {
        let mut committed = 0;
        for event in events {
            let key = event.key();
            if self.seen.contains(&key) {
                continue;
            }
            reserve_identity(&mut self.seen, self.limits.max_seen)?;
            self.archive.append_event(event, self.limits.line_bytes)?;
            let _inserted = self.seen.insert(key);
            self.last_newest = Some(
                self.last_newest
                    .map_or(event.ts, |previous| previous.max(event.ts)),
            );
            committed += 1;
        }
        Ok(committed)
    }

    async fn fetch(&self) -> Result<HttpResponse, crate::FetchError> {
        crate::http::fetch(
            &self.client,
            self.endpoint.clone(),
            self.limits.response_bytes,
        )
        .await
    }
}

fn reserve_identity(
    seen: &mut HashSet<ActivityKey>,
    maximum: NonZeroUsize,
) -> Result<(), SurveyError> {
    if seen.len() >= maximum.get() {
        return Err(SurveyError::SeenLimit { maximum });
    }
    seen.try_reserve(1).map_err(SurveyError::Allocation)
}

/// One row of the activity ranking.
#[derive(Debug, Clone, Serialize)]
pub struct ReflectorActivity {
    /// Reflector name (e.g. `"REF001"`).
    pub reflector: String,
    /// Module letter.
    pub module: char,
    /// Transmissions observed inside the window.
    pub transmissions: usize,
    /// Distinct transmitting callsigns (first token of `mycall`).
    pub distinct_callsigns: usize,
    /// Most recent transmission observed.
    pub last_heard: DateTime<Utc>,
}

/// Per-module accumulator used while ranking.
#[derive(Debug)]
struct Tally {
    transmissions: usize,
    callsigns: HashSet<String>,
    last_heard: DateTime<Utc>,
}

/// Incremental ranking accumulator; retains grouped counters and unique callsigns.
#[derive(Debug)]
pub struct ActivityRanking {
    since: DateTime<Utc>,
    per: HashMap<(String, char), Tally>,
}

impl ActivityRanking {
    /// Select the inclusive UTC beginning of the ranking window.
    #[must_use]
    pub fn new(since: DateTime<Utc>) -> Self {
        Self {
            since,
            per: HashMap::new(),
        }
    }

    /// Accumulate one event; rows lacking a reflector/module are excluded.
    pub fn observe(&mut self, event: &ActivityEvent) {
        if event.ts < self.since {
            return;
        }
        let (Some(reflector), Some(module)) = (event.reflector.as_ref(), event.module) else {
            return;
        };
        let callsign = event
            .mycall
            .split_whitespace()
            .next()
            .unwrap_or_default()
            .to_owned();
        let tally = self
            .per
            .entry((reflector.clone(), module))
            .or_insert_with(|| Tally {
                transmissions: 0,
                callsigns: HashSet::new(),
                last_heard: event.ts,
            });
        tally.transmissions = tally.transmissions.saturating_add(1);
        let _inserted = tally.callsigns.insert(callsign);
        tally.last_heard = tally.last_heard.max(event.ts);
    }

    /// Return descending transmission counts, breaking ties by name and module.
    ///
    /// A count saturates at `usize::MAX`; unique-call storage grows with the input.
    #[must_use]
    pub fn into_rows(self) -> Vec<ReflectorActivity> {
        let mut rows: Vec<_> = self
            .per
            .into_iter()
            .map(|((reflector, module), tally)| ReflectorActivity {
                reflector,
                module,
                transmissions: tally.transmissions,
                distinct_callsigns: tally.callsigns.len(),
                last_heard: tally.last_heard,
            })
            .collect();
        rows.sort_by(|a, b| {
            b.transmissions
                .cmp(&a.transmissions)
                .then_with(|| a.reflector.cmp(&b.reflector))
                .then_with(|| a.module.cmp(&b.module))
        });
        rows
    }
}

/// Rank reflector modules by observed transmissions since `since`.
#[must_use]
pub fn rank_activity(events: &[ActivityEvent], since: DateTime<Utc>) -> Vec<ReflectorActivity> {
    let mut ranking = ActivityRanking::new(since);
    for event in events {
        ranking.observe(event);
    }
    ranking.into_rows()
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    /// Rows covering reflector routing, a local gateway and a note suffix.
    const FIXTURE: &str = concat!(
        "<table><caption>dplus Last Heard</caption>",
        "<tr><th>Date / Time</th><th>Gateway</th><th>MyCall</th><th>UrCall</th>",
        "<th>Reflector</th><th>RPT1</th><th>RPT2</th></tr>",
        "<tr><td class=odd><center>2026-07-11 03:51:34 UTC</center></td>",
        "<td class=odd><a href=\"?gateway=REF030\">REF030</a></td>",
        "<td class=odd><a class='TS1' href=\"http://qrz.com/db/K4CEM\">K4CEM</a></td>",
        "<td class=odd>CQCQCQ</td><td class=odd>REF030 C</td>",
        "<td class=odd>K4CEM  C</td><td class=odd>REF030 C</td></tr>",
        "<tr><td class=even><center>2026-07-11 03:51:30 UTC</center></td>",
        "<td class=even><a href=\"?gateway=W4LCO\">W4LCO</a></td>",
        "<td class=even><a class='TS1' href=\"http://qrz.com/db/KQ4SCY\">KQ4SCY (ID52)</a></td>",
        "<td class=even>CQCQCQ</td><td class=even></td>",
        "<td class=even>W4LCO  C</td><td class=even>W4LCO  G</td></tr>",
        "<tr><td class=odd><center>2026-07-11 03:51:16 UTC</center></td>",
        "<td class=odd><a href=\"?gateway=REF004\">REF004</a></td>",
        "<td class=odd><a class='TS1' href=\"http://qrz.com/db/KB6MAT\">KB6MAT</a></td>",
        "<td class=odd>CQCQCQ</td><td class=odd>REF004 C</td>",
        "<td class=odd>KB6MAT B</td><td class=odd>REF004 C</td></tr>",
        "</table>"
    );

    fn t0() -> DateTime<Utc> {
        DateTime::<Utc>::UNIX_EPOCH
    }

    #[test]
    fn parses_real_feed_rows() -> TestResult {
        let parsed = ParsedFeed::from_html(FIXTURE, t0());
        assert_eq!(parsed.events.len(), 3);
        assert_eq!(parsed.parse_skips, 0);

        let first = parsed.events.first().ok_or("row 0")?;
        assert_eq!(first.gateway, "REF030");
        assert_eq!(first.mycall, "K4CEM");
        assert_eq!(first.reflector.as_deref(), Some("REF030"));
        assert_eq!(first.module, Some('C'));
        assert_eq!(first.ts.to_rfc3339(), "2026-07-11T03:51:34+00:00");

        let local = parsed.events.get(1).ok_or("row 1")?;
        assert_eq!(local.gateway, "W4LCO");
        assert_eq!(local.mycall, "KQ4SCY (ID52)");
        assert!(local.reflector.is_none());
        assert!(local.module.is_none());
        Ok(())
    }

    #[test]
    fn strip_tags_handles_nested_markup_and_entities() {
        assert_eq!(strip_tags("<center>2026</center>"), "2026");
        assert_eq!(strip_tags("<a href=\"x\">K4CEM</a>"), "K4CEM");
        assert_eq!(strip_tags("A&nbsp;B &amp; C"), "A B & C");
    }

    #[test]
    fn split_reflector_variants() {
        assert_eq!(
            split_reflector("REF030 C"),
            (Some("REF030".to_string()), Some('C'))
        );
        assert_eq!(split_reflector("  "), (None, None));
        assert_eq!(
            split_reflector("REF030"),
            (Some("REF030".to_string()), None)
        );
    }

    #[test]
    fn dedupe_key_is_stable_across_polls() {
        let a = ParsedFeed::from_html(FIXTURE, t0());
        let b = ParsedFeed::from_html(
            FIXTURE,
            DateTime::<Utc>::UNIX_EPOCH + chrono::TimeDelta::seconds(60),
        );
        let ka: Vec<ActivityKey> = a.events.iter().map(ActivityEvent::key).collect();
        let kb: Vec<ActivityKey> = b.events.iter().map(ActivityEvent::key).collect();
        assert_eq!(ka, kb, "polled_at must not affect identity");
    }

    #[test]
    fn ranking_counts_and_sorts() -> TestResult {
        let parsed = ParsedFeed::from_html(FIXTURE, t0());
        let ranked = rank_activity(&parsed.events, t0());
        // Two reflector rows (REF030 C, REF004 C); local gateway row excluded.
        assert_eq!(ranked.len(), 2);
        let top = ranked.first().ok_or("top")?;
        assert_eq!(top.transmissions, 1);
        assert_eq!(top.distinct_callsigns, 1);
        // Tie on count → alphabetical.
        assert_eq!(top.reflector, "REF004");
        Ok(())
    }

    #[test]
    fn ranking_respects_window() {
        let parsed = ParsedFeed::from_html(FIXTURE, t0());
        let future = DateTime::<Utc>::UNIX_EPOCH + chrono::TimeDelta::days(30_000);
        assert!(rank_activity(&parsed.events, future).is_empty());
    }

    #[test]
    fn archive_roundtrip_and_dedupe_seed() -> TestResult {
        let dir = tempfile::tempdir()?;
        let archive = Archive::new(dir.path());
        let parsed = ParsedFeed::from_html(FIXTURE, t0());
        let maximum = NonZeroUsize::new(4096).ok_or("maximum")?;
        for event in &parsed.events {
            archive.append_event(event, maximum)?;
        }
        let loaded = archive.events(maximum)?.collect::<Result<Vec<_>, _>>()?;
        assert_eq!(loaded.len(), 3);
        assert_eq!(loaded, parsed.events);
        Ok(())
    }

    #[test]
    fn raw_path_is_date_partitioned() {
        let archive = Archive::new(Path::new("survey"));
        let ts = parse_feed_ts("2026-07-11 03:51:34 UTC");
        assert!(ts.is_some());
        if let Some(ts) = ts {
            let p = archive.raw_path(ts);
            assert!(
                p.ends_with("dplusmon/raw/2026-07-11/20260711T035134Z.html"),
                "got {p:?}"
            );
        }
    }

    const fn limits() -> SurveyLimits {
        SurveyLimits::new(const { NonZeroUsize::new(100).unwrap() })
    }

    fn surveyor(base: &Path) -> Result<Surveyor, Box<dyn std::error::Error>> {
        Ok(Surveyor::new(
            base,
            limits(),
            RoutingCallsign::new("N0CALL")?,
        )?)
    }

    #[test]
    fn event_identity_preserves_boundaries_and_timestamp_precision() -> TestResult {
        let parsed = ParsedFeed::from_html(FIXTURE, t0());
        let mut left = parsed.events.first().ok_or("event")?.clone();
        let mut right = left.clone();
        left.gateway = "A|B".to_owned();
        left.mycall = "C".to_owned();
        right.gateway = "A".to_owned();
        right.mycall = "B|C".to_owned();
        assert_ne!(left.key(), right.key());
        right = left.clone();
        right.ts += chrono::TimeDelta::nanoseconds(1);
        assert_ne!(left.key(), right.key());
        Ok(())
    }

    #[test]
    fn failed_append_rolls_back_bytes_and_can_be_retried() -> TestResult {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("activity.jsonl");
        append_bytes(&path, b"first", io::Write::write_all)?;
        let before = std::fs::read(&path)?;
        let result = append_bytes(&path, b"second", |file, bytes| {
            file.write_all(bytes.get(..3).ok_or_else(|| io::Error::other("prefix"))?)?;
            Err(io::Error::other("injected write failure"))
        });
        assert!(matches!(result, Err(ArchiveError::Io { .. })), "{result:?}");
        assert_eq!(std::fs::read(&path)?, before);
        append_bytes(&path, b"second", io::Write::write_all)?;
        assert_eq!(std::fs::read(&path)?, b"first\nsecond\n");
        Ok(())
    }

    #[test]
    fn seen_state_changes_only_after_committed_append_and_deduplicates_batch() -> TestResult {
        let directory = tempfile::tempdir()?;
        let mut poller = surveyor(directory.path())?;
        let events = ParsedFeed::from_html(FIXTURE, t0()).events;
        let event = events.first().ok_or("event")?;
        std::fs::create_dir_all(poller.archive.activity_path())?;
        assert!(poller.commit_events(std::slice::from_ref(event)).is_err());
        assert_eq!(poller.total_seen(), 0);
        std::fs::remove_dir(poller.archive.activity_path())?;
        assert_eq!(poller.commit_events(&[event.clone(), event.clone()])?, 1);
        assert_eq!(poller.total_seen(), 1);
        assert_eq!(
            poller
                .archive
                .events(limits().line_bytes)?
                .collect::<Result<Vec<_>, _>>()?,
            vec![event.clone()]
        );
        Ok(())
    }

    #[test]
    fn archive_lines_report_json_length_tail_and_lock_failures() -> TestResult {
        let directory = tempfile::tempdir()?;
        let archive = Archive::new(directory.path());
        let event = ParsedFeed::from_html(FIXTURE, t0())
            .events
            .into_iter()
            .next()
            .ok_or("event")?;
        archive.append_event(&event, limits().line_bytes)?;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(archive.activity_path())?;
        file.try_lock()?;
        assert!(matches!(
            archive.append_event(&event, limits().line_bytes),
            Err(ArchiveError::Lock { .. })
        ));
        file.unlock()?;
        let mut contents = b"invalid JSON\n".to_vec();
        contents.extend_from_slice(&serde_json::to_vec(&event)?);
        contents.push(b'\n');
        std::fs::write(archive.activity_path(), &contents)?;
        let mut lines = archive.events(limits().line_bytes)?;
        let first = lines.next().ok_or("line one")?;
        assert!(
            matches!(
                first,
                Err(EventReadError {
                    line: 1,
                    reason: EventReadReason::Json(_),
                    ..
                })
            ),
            "{first:?}"
        );
        assert_eq!(lines.next().ok_or("line two")??, event);
        assert!(lines.next().is_none());
        std::fs::write(archive.activity_path(), serde_json::to_vec(&event)?)?;
        assert!(matches!(
            archive.events(limits().line_bytes)?.next(),
            Some(Err(EventReadError {
                reason: EventReadReason::Unterminated,
                ..
            }))
        ));
        assert!(matches!(
            archive.append_event(&event, limits().line_bytes),
            Err(ArchiveError::Unterminated(_))
        ));
        let tiny = NonZeroUsize::new(2).ok_or("tiny")?;
        assert!(matches!(
            archive.events(tiny)?.next(),
            Some(Err(EventReadError {
                reason: EventReadReason::TooLong { .. },
                ..
            }))
        ));
        assert!(matches!(
            archive.append_event(&event, tiny),
            Err(ArchiveError::LineTooLong { .. })
        ));
        Ok(())
    }

    #[test]
    fn raw_snapshots_never_replace_same_timestamp_and_readers_have_fixed_snapshot() -> TestResult {
        let directory = tempfile::tempdir()?;
        let archive = Archive::new(directory.path());
        let first = archive.store_raw(t0(), b"first")?;
        let second = archive.store_raw(t0(), b"second")?;
        assert_ne!(first, second);
        assert_eq!(std::fs::read(first)?, b"first");
        assert_eq!(std::fs::read(second)?, b"second");
        let events = ParsedFeed::from_html(FIXTURE, t0()).events;
        let first = events.first().ok_or("event")?;
        archive.append_event(first, limits().line_bytes)?;
        let snapshot = archive.events(limits().line_bytes)?;
        archive.append_event(events.get(1).ok_or("second event")?, limits().line_bytes)?;
        assert_eq!(
            snapshot.collect::<Result<Vec<_>, _>>()?,
            vec![first.clone()]
        );
        Ok(())
    }

    #[test]
    fn poll_floor_is_committed_before_network_work_and_identity_bound_is_exact() -> TestResult {
        let directory = tempfile::tempdir()?;
        let mut poller = surveyor(directory.path())?;
        let start = tokio::time::Instant::now();
        poller.reserve_poll(start)?;
        assert!(matches!(
            poller.reserve_poll(start + Duration::from_secs(29)),
            Err(SurveyError::TooSoon { .. })
        ));
        poller.reserve_poll(start + Duration::from_secs(30))?;
        poller.limits = SurveyLimits::new(NonZeroUsize::new(2).ok_or("limit")?);
        let events = ParsedFeed::from_html(FIXTURE, t0()).events;
        assert!(matches!(
            poller.commit_events(&events),
            Err(SurveyError::SeenLimit { .. })
        ));
        assert_eq!(poller.total_seen(), 2);
        assert_eq!(
            poller
                .archive
                .events(limits().line_bytes)?
                .collect::<Result<Vec<_>, _>>()?
                .len(),
            2
        );
        poller.limits = limits();
        assert_eq!(poller.commit_events(&events)?, 1);
        assert_eq!(poller.total_seen(), 3);
        Ok(())
    }
}
