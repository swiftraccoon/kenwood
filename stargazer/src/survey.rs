// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later

//! DPLUS network activity survey via the NJ6N DPLUSMON feed.
//!
//! DPLUSMON (`nj6n.com/dplusmon`) republishes every transmission on
//! the DPLUS (REF reflector) network as a rolling ~30-row last-heard
//! table. Polling that one volunteer-run endpoint is the gentlest way
//! to measure per-reflector voice activity — no reflector is probed,
//! no client slot consumed. The feed is a sliding window, so history
//! evaporates unless archived: every fetch is stored verbatim, parsed
//! events append to a deduplicated JSONL log, and every poll writes a
//! provenance record (including a gap-risk flag when the window may
//! have overflowed between polls).

use std::collections::HashSet;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// The DPLUSMON query endpoint (unfiltered: all gateways).
const FEED_URL: &str = "https://nj6n.com/dplusmon/query-dplusdb.php?mycall=&gateway=";

/// Identify ourselves honestly: tool, version, operator contact.
const USER_AGENT: &str = concat!(
    "stargazer-survey/",
    env!("CARGO_PKG_VERSION"),
    " (amateur radio activity research)"
);

/// Minimum allowed poll interval — the feed's own web UI polls every
/// 15 s per viewer; we never go below twice that.
pub const MIN_INTERVAL_SECS: u64 = 30;

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
    /// When our poll first observed this event.
    pub polled_at: DateTime<Utc>,
}

impl ActivityEvent {
    /// Stable dedupe key across polls.
    #[must_use]
    pub fn key(&self) -> String {
        format!(
            "{}|{}|{}|{}",
            self.ts.timestamp(),
            self.gateway,
            self.mycall,
            self.rpt1
        )
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
    /// newest row) — rows may have been missed.
    pub gap_risk: bool,
    /// Fetch/parse error, if the poll failed.
    pub error: Option<String>,
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
#[derive(Debug)]
pub struct ParsedFeed {
    /// Recognized events, newest first (feed order).
    pub events: Vec<ActivityEvent>,
    /// Rows skipped for unexpected shape.
    pub parse_skips: usize,
}

/// Parse the DPLUSMON HTML table into events.
#[must_use]
pub fn parse_feed(html: &str, polled_at: DateTime<Utc>) -> ParsedFeed {
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
    ParsedFeed {
        events,
        parse_skips,
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

    /// Store one raw response verbatim (archive-first: this happens
    /// before parsing so a parser bug can never lose data).
    ///
    /// # Errors
    ///
    /// I/O errors creating the date directory or writing the file.
    pub fn store_raw(&self, polled_at: DateTime<Utc>, body: &[u8]) -> std::io::Result<PathBuf> {
        let path = self.raw_path(polled_at);
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(&path, body)?;
        Ok(path)
    }

    /// Append one JSON line to a log file.
    ///
    /// # Errors
    ///
    /// I/O errors opening or writing the log.
    fn append_line(path: &Path, line: &str) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)?;
        writeln!(f, "{line}")
    }

    /// Append events to the activity log.
    ///
    /// # Errors
    ///
    /// Serialization or I/O errors.
    pub fn append_events(&self, events: &[ActivityEvent]) -> Result<(), SurveyError> {
        for ev in events {
            Self::append_line(&self.activity_path(), &serde_json::to_string(ev)?)?;
        }
        Ok(())
    }

    /// Append one poll provenance record.
    ///
    /// # Errors
    ///
    /// Serialization or I/O errors.
    pub fn append_poll(&self, poll: &PollRecord) -> Result<(), SurveyError> {
        Ok(Self::append_line(
            &self.polls_path(),
            &serde_json::to_string(poll)?,
        )?)
    }

    /// Load every previously archived event (used to seed dedupe on
    /// startup and by the report).
    ///
    /// # Errors
    ///
    /// I/O errors reading the log. Unparseable lines are skipped.
    pub fn load_events(&self) -> std::io::Result<Vec<ActivityEvent>> {
        let path = self.activity_path();
        if !path.exists() {
            return Ok(Vec::new());
        }
        let text = std::fs::read_to_string(path)?;
        Ok(text
            .lines()
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect())
    }
}

/// Survey failures.
#[derive(Debug, thiserror::Error)]
pub enum SurveyError {
    /// HTTP fetch failed.
    #[error("fetch: {0}")]
    Fetch(#[from] reqwest::Error),
    /// Archive I/O failed.
    #[error("archive io: {0}")]
    Io(#[from] std::io::Error),
    /// JSON encoding failed (unexpected).
    #[error("serialize: {0}")]
    Serialize(#[from] serde_json::Error),
}

/// Stateful poller: fetches the feed, archives raw + events + poll
/// provenance, and deduplicates across polls.
#[derive(Debug)]
pub struct Surveyor {
    client: reqwest::Client,
    archive: Archive,
    seen: HashSet<String>,
    last_newest: Option<DateTime<Utc>>,
}

impl Surveyor {
    /// Create a poller rooted at the archive base directory, seeding
    /// the dedupe set from any existing archive.
    ///
    /// # Errors
    ///
    /// I/O errors reading the existing archive or building the HTTP
    /// client.
    pub fn new(base: &Path) -> Result<Self, SurveyError> {
        let archive = Archive::new(base);
        let mut seen = HashSet::new();
        let mut last_newest = None;
        for ev in archive.load_events()? {
            if last_newest.is_none_or(|n| ev.ts > n) {
                last_newest = Some(ev.ts);
            }
            let _unused = seen.insert(ev.key());
        }
        let client = reqwest::Client::builder()
            .user_agent(USER_AGENT)
            .timeout(std::time::Duration::from_secs(15))
            .build()?;
        Ok(Self {
            client,
            archive,
            seen,
            last_newest,
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
    /// Archive I/O errors are fatal (the archive is the point).
    /// Fetch errors are NOT returned as `Err` — they are recorded in
    /// the poll log and reflected in the returned record, so the
    /// caller can back off without losing provenance.
    pub async fn poll_once(&mut self) -> Result<PollRecord, SurveyError> {
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
        };

        match self.fetch().await {
            Err(e) => {
                record.error = Some(e.to_string());
            }
            Ok((status, body)) => {
                record.http_status = Some(status);
                record.bytes = body.len();
                // Archive-first: raw bytes land before parsing.
                let _raw_path = self.archive.store_raw(polled_at, &body)?;
                if status == 200 {
                    let parsed = parse_feed(&String::from_utf8_lossy(&body), polled_at);
                    record.rows = parsed.events.len();
                    record.parse_skips = parsed.parse_skips;
                    record.window_oldest = parsed.events.iter().map(|e| e.ts).min();
                    record.window_newest = parsed.events.iter().map(|e| e.ts).max();
                    // Full window of unseen rows after a previous poll
                    // means the window may have rolled past us.
                    record.gap_risk = self.last_newest.is_some_and(|prev| {
                        !parsed.events.is_empty() && parsed.events.iter().all(|e| e.ts > prev)
                    });

                    let fresh: Vec<ActivityEvent> = parsed
                        .events
                        .into_iter()
                        .filter(|e| self.seen.insert(e.key()))
                        .collect();
                    record.new_events = fresh.len();
                    self.archive.append_events(&fresh)?;
                    if let Some(newest) = record.window_newest
                        && self.last_newest.is_none_or(|n| newest > n)
                    {
                        self.last_newest = Some(newest);
                    }
                } else {
                    record.error = Some(format!("http status {status}"));
                }
            }
        }

        self.archive.append_poll(&record)?;
        Ok(record)
    }

    async fn fetch(&self) -> Result<(u16, Vec<u8>), reqwest::Error> {
        let resp = self.client.get(FEED_URL).send().await?;
        let status = resp.status().as_u16();
        let body = resp.bytes().await?;
        Ok((status, body.to_vec()))
    }
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
struct Tally {
    transmissions: usize,
    callsigns: HashSet<String>,
    last_heard: DateTime<Utc>,
}

/// Rank reflector modules by observed transmissions since `since`.
#[must_use]
pub fn rank_activity(events: &[ActivityEvent], since: DateTime<Utc>) -> Vec<ReflectorActivity> {
    use std::collections::HashMap;
    let mut per: HashMap<(String, char), Tally> = HashMap::new();
    for ev in events {
        if ev.ts < since {
            continue;
        }
        let (Some(reflector), Some(module)) = (ev.reflector.clone(), ev.module) else {
            continue;
        };
        let callsign = ev
            .mycall
            .split_whitespace()
            .next()
            .unwrap_or_default()
            .to_string();
        let entry = per.entry((reflector, module)).or_insert_with(|| Tally {
            transmissions: 0,
            callsigns: HashSet::new(),
            last_heard: ev.ts,
        });
        entry.transmissions += 1;
        let _unused = entry.callsigns.insert(callsign);
        if ev.ts > entry.last_heard {
            entry.last_heard = ev.ts;
        }
    }
    let mut rows: Vec<ReflectorActivity> = per
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

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    /// Three real rows sampled from the live feed (2026-07-11):
    /// a reflector transmission, a local-gateway transmission with an
    /// empty Reflector cell, and a mycall with a note suffix.
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
        let parsed = parse_feed(FIXTURE, t0());
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
        let a = parse_feed(FIXTURE, t0());
        let b = parse_feed(
            FIXTURE,
            DateTime::<Utc>::UNIX_EPOCH + chrono::TimeDelta::seconds(60),
        );
        let ka: Vec<String> = a.events.iter().map(ActivityEvent::key).collect();
        let kb: Vec<String> = b.events.iter().map(ActivityEvent::key).collect();
        assert_eq!(ka, kb, "polled_at must not affect identity");
    }

    #[test]
    fn ranking_counts_and_sorts() -> TestResult {
        let parsed = parse_feed(FIXTURE, t0());
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
        let parsed = parse_feed(FIXTURE, t0());
        let future = DateTime::<Utc>::UNIX_EPOCH + chrono::TimeDelta::days(30_000);
        assert!(rank_activity(&parsed.events, future).is_empty());
    }

    #[test]
    fn archive_roundtrip_and_dedupe_seed() -> TestResult {
        let dir = tempfile::tempdir()?;
        let archive = Archive::new(dir.path());
        let parsed = parse_feed(FIXTURE, t0());
        archive.append_events(&parsed.events)?;
        let loaded = archive.load_events()?;
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
}
