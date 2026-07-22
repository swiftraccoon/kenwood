// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later

//! Harvest reflector-published recordings that pair with ours.
//!
//! Reflectors running the current DPLUS daemon publish, per
//! transmission, a `.dvrec` packet log (every AMBE frame as hex), a
//! server-side decoded `.mp3`, and a `.txt` sidecar under
//! `https://<reflector>.dstargateway.org:8443/streams/<Y>/<M>/<D>/`
//! with directory listing enabled. The MP3 is decoded by a different
//! (higher-quality) vocoder implementation than ours, which makes it
//! a per-transmission reference signal for audio-quality work: our
//! `.ambe` capture of the same stream carries byte-identical frames,
//! so `(our frames, their audio)` forms a matched pair.
//!
//! The harvester walks a recordings directory as written by the
//! recorder, matches each published transmission to a local recording
//! by `(callsign, stream id)` with a timestamp tolerance, and
//! downloads the published files into `<date dir>/published/` under
//! their original names. Matched transmissions fetch the `.mp3` and
//! `.txt`; the bulkier `.dvrec` is fetched only where it adds frames
//! we lack (our capture has gaps, or we missed the transmission
//! entirely). Every download and every run appends a provenance
//! record to `<date dir>/published/harvest.jsonl`.
//!
//! Published retention is short (days), so harvesting is a same-day
//! affair. This is a volunteer-run service, not a mirror target, and
//! the client is engineered to be indistinguishable from a courteous
//! human visitor: robots.txt is honored before anything else is
//! fetched, downloads run strictly sequentially over one connection
//! with a multi-second pause, the User-Agent names the tool and the
//! responsible operator, re-runs never re-download, and a few
//! consecutive failures stop the run entirely rather than hammer a
//! struggling or purging server.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write as _;
use std::path::Path;
use std::time::Duration;

use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};

/// Identify ourselves honestly: tool, version, purpose, and (when
/// known) the responsible operator, so a server owner can reach a
/// human instead of reaching for a block.
fn build_user_agent(operator: Option<&str>) -> String {
    let mut ua = concat!(
        "stargazer-harvest/",
        env!("CARGO_PKG_VERSION"),
        " (amateur radio research; pairing our own reflector captures"
    )
    .to_string();
    if let Some(op) = operator {
        ua.push_str("; operator ");
        ua.push_str(op);
    }
    ua.push(')');
    ua
}

/// Decide whether a robots.txt body forbids us from `path`, checked
/// for both the wildcard agent and our own product token. We honor it
/// even though the files are operator-published links, because the
/// politest interpretation always wins.
fn robots_disallows(robots: &str, path: &str) -> bool {
    let mut applies = false; // current group names us (or everyone)
    let mut in_agent_run = false; // inside consecutive User-agent lines
    for raw in robots.lines() {
        let line = raw.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let key = key.trim().to_ascii_lowercase();
        let value = value.trim();
        match key.as_str() {
            "user-agent" => {
                if !in_agent_run {
                    applies = false; // a new group begins
                }
                in_agent_run = true;
                let agent = value.to_ascii_lowercase();
                if agent == "*" || agent.starts_with("stargazer") {
                    applies = true;
                }
            }
            "disallow" if applies => {
                in_agent_run = false;
                if !value.is_empty() && path.starts_with(value) {
                    return true;
                }
            }
            _ => in_agent_run = false,
        }
    }
    false
}

/// Pause between successive file downloads. Two seconds keeps a full
/// day's harvest around one small request every other second, far
/// below anything a rate limiter or intrusion sensor watches for.
pub const POLITE_GAP: Duration = Duration::from_secs(2);

/// Consecutive download failures after which a run stops touching the
/// server (remaining files retry on a later run).
const MAX_CONSECUTIVE_FAILURES: usize = 3;

/// Widest publish-vs-capture start-time skew treated as the same
/// transmission when `(callsign, stream id)` repeats within a day.
const MATCH_TOLERANCE_SECS: i64 = 120;

/// File extensions the daemon publishes per transmission.
const PUBLISHED_EXTENSIONS: [&str; 3] = ["dvrec", "mp3", "txt"];

/// One published transmission: the listing entries sharing a stem.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublishedTx {
    /// Filename stem exactly as published (no extension).
    pub stem: String,
    /// Reflector system name (e.g. `"REF030"`).
    pub system: String,
    /// Module letter.
    pub module: char,
    /// Transmission start per the publisher's clock (UTC).
    pub started_at: DateTime<Utc>,
    /// MYCALL as published (trimmed, unsanitized).
    pub mycall: String,
    /// Stream id, normalized to uppercase hex.
    pub stream_id: String,
    /// Extensions available for this stem.
    pub extensions: BTreeSet<String>,
}

/// Pairing-relevant metadata of one local recording.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalRecording {
    /// Filename stem (no extension).
    pub stem: String,
    /// Callsign in filename form (sanitized, uppercase).
    pub callsign: String,
    /// Stream id, uppercase hex.
    pub stream_id: String,
    /// Transmission start per our clock (UTC).
    pub started_at: DateTime<Utc>,
    /// Sequence gaps observed during capture.
    pub gaps: u64,
}

/// Local recordings loaded from a date directory.
#[derive(Debug, Default)]
pub struct LoadedLocal {
    /// Successfully parsed recordings.
    pub recordings: Vec<LocalRecording>,
    /// Sidecars that existed but could not be parsed.
    pub skipped: usize,
}

/// Why a published file is planned for download.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FetchReason {
    /// Reference audio for a transmission we captured.
    PairedAudio,
    /// Publisher's metadata sidecar for a paired transmission.
    PairedSidecar,
    /// Packet log to repair a gapped local capture.
    GapFillDvrec,
    /// Transmission we missed entirely: take everything published.
    Salvage,
}

/// One planned download.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchItem {
    /// Published filename (stem + extension).
    pub file_name: String,
    /// Why this file is wanted.
    pub reason: FetchReason,
    /// Stem of the matched local recording, when paired.
    pub local_stem: Option<String>,
}

/// Download plan plus pairing coverage for one date directory.
#[derive(Debug, Default)]
pub struct HarvestPlan {
    /// Files to download, deterministic order.
    pub items: Vec<FetchItem>,
    /// Published transmissions matched to a local recording.
    pub matched: usize,
    /// Published transmissions with no local counterpart.
    pub published_only: usize,
    /// Local recordings the publisher did not publish.
    pub local_only: usize,
    /// Wanted files skipped because they already exist locally.
    pub skipped_existing: usize,
}

/// Decode `%XX` percent-escapes; malformed escapes pass through.
fn percent_decode(s: &str) -> String {
    const fn hex_val(b: u8) -> Option<u8> {
        match b {
            b'0'..=b'9' => Some(b - b'0'),
            b'a'..=b'f' => Some(b - b'a' + 10),
            b'A'..=b'F' => Some(b - b'A' + 10),
            _ => None,
        }
    }
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0usize;
    while let Some(&b) = bytes.get(i) {
        if b == b'%'
            && let (Some(hi), Some(lo)) = (
                bytes.get(i + 1).copied().and_then(hex_val),
                bytes.get(i + 2).copied().and_then(hex_val),
            )
        {
            out.push((hi << 4) | lo);
            i += 3;
        } else {
            out.push(b);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// True when `name` is a single safe path component: no directory
/// separators, no parent-dir traversal, not absolute, non-empty.
///
/// A directory listing is untrusted input (the reflector server may be
/// hostile or compromised) and the name is later joined onto the
/// download directory and written, so a name carrying `/`, `\`, `..`,
/// or a leading separator must never reach the filesystem; otherwise
/// the HTTP body is written outside the download tree (an absolute
/// component makes `Path::join` drop the base entirely).
fn is_safe_basename(name: &str) -> bool {
    use std::path::Component;
    let mut components = Path::new(name).components();
    matches!(
        (components.next(), components.next()),
        (Some(Component::Normal(_)), None)
    ) && !name.contains('\\')
}

/// Parse one published filename into its transmission identity and
/// extension. Returns `None` for names that do not match the
/// `SYS-M--YYYYMMDD-HHMMSS.mmm--MYCALL--ssss.ext` shape.
fn parse_published_name(file_name: &str) -> Option<(PublishedTx, String)> {
    let (stem, ext) = file_name.rsplit_once('.')?;
    if !PUBLISHED_EXTENSIONS.contains(&ext) {
        return None;
    }
    let segments: Vec<&str> = stem.split("--").collect();
    if segments.len() < 4 {
        return None;
    }
    let (system, module_str) = segments.first()?.rsplit_once('-')?;
    let mut module_chars = module_str.chars();
    let module = match (module_chars.next(), module_chars.next()) {
        (Some(c), None) if c.is_ascii_uppercase() => c,
        _ => return None,
    };
    if system.is_empty() {
        return None;
    }
    let started_at = chrono::NaiveDateTime::parse_from_str(segments.get(1)?, "%Y%m%d-%H%M%S%.3f")
        .ok()?
        .and_utc();
    let stream_raw = segments.last()?;
    if stream_raw.len() != 4 || !stream_raw.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    // MYCALL may itself contain dashes; rejoin the middle segments.
    let mycall = segments.get(2..segments.len() - 1)?.join("--");
    if mycall.is_empty() {
        return None;
    }
    Some((
        PublishedTx {
            stem: stem.to_string(),
            system: system.to_string(),
            module,
            started_at,
            mycall,
            stream_id: stream_raw.to_ascii_uppercase(),
            extensions: BTreeSet::new(),
        },
        ext.to_string(),
    ))
}

/// Parse an autoindex listing page into published transmissions,
/// grouping the per-extension entries by stem. Order is deterministic
/// (start time, then stem).
#[must_use]
pub fn parse_listing(html: &str) -> Vec<PublishedTx> {
    let mut names: BTreeSet<String> = BTreeSet::new();
    let mut rest = html;
    while let Some(pos) = rest.find("href=\"") {
        rest = rest.get(pos + 6..).unwrap_or("");
        let Some(end) = rest.find('"') else { break };
        let href = rest.get(..end).unwrap_or("");
        // Published files are bare basenames; directory navigation
        // links (parent, sort columns) carry '/' or '?' and drop out.
        // The raw-href filter is NOT sufficient on its own: the value
        // we keep is percent-DECODED, so an href like `%2Ftmp%2Fx` has
        // no literal '/' yet decodes to an absolute path. Re-validate
        // the decoded name as a single safe path component before
        // trusting it as a basename downstream (it is later joined to
        // the download dir and written).
        if !href.contains('/') && !href.contains('?') {
            let decoded = percent_decode(href);
            if is_safe_basename(&decoded) {
                let _unused = names.insert(decoded);
            }
        }
        rest = rest.get(end..).unwrap_or("");
    }
    let mut by_stem: BTreeMap<String, PublishedTx> = BTreeMap::new();
    for name in &names {
        if let Some((tx, ext)) = parse_published_name(name) {
            let entry = by_stem.entry(tx.stem.clone()).or_insert(tx);
            let _unused = entry.extensions.insert(ext);
        }
    }
    let mut txs: Vec<PublishedTx> = by_stem.into_values().collect();
    txs.sort_by(|a, b| {
        a.started_at
            .cmp(&b.started_at)
            .then_with(|| a.stem.cmp(&b.stem))
    });
    txs
}

/// Load pairing metadata for every recording in a date directory (a
/// recording exists iff its `.json` exists).
///
/// # Errors
///
/// I/O errors reading the directory. Individual unparseable sidecars
/// are counted in [`LoadedLocal::skipped`], not fatal.
pub fn load_local_recordings(date_dir: &Path) -> std::io::Result<LoadedLocal> {
    let mut out = LoadedLocal::default();
    if !date_dir.exists() {
        // No local captures for this date, so everything published is
        // a salvage candidate.
        return Ok(out);
    }
    for entry in std::fs::read_dir(date_dir)? {
        let path = entry?.path();
        if path.extension() != Some("json".as_ref()) {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
            out.skipped += 1;
            continue;
        };
        let doc: Option<SidecarDoc> = std::fs::read_to_string(&path)
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok());
        let Some(doc) = doc else {
            out.skipped += 1;
            continue;
        };
        out.recordings.push(LocalRecording {
            stem: stem.to_string(),
            callsign: doc.header.map_or_else(
                || "UNKNOWN".to_string(),
                |h| crate::writer::sanitize_callsign(&h.my_callsign),
            ),
            stream_id: doc.stream_id.to_ascii_uppercase(),
            started_at: doc.started_at,
            gaps: doc.frames.map_or(0, |f| f.gaps),
        });
    }
    out.recordings.sort_by(|a, b| {
        a.started_at
            .cmp(&b.started_at)
            .then_with(|| a.stem.cmp(&b.stem))
    });
    Ok(out)
}

/// Pairing-relevant subset of the `stargazer-recording/1` sidecar.
#[derive(Debug, Deserialize)]
struct SidecarDoc {
    stream_id: String,
    started_at: DateTime<Utc>,
    #[serde(default)]
    frames: Option<SidecarFrames>,
    #[serde(default)]
    header: Option<SidecarHeader>,
}

#[derive(Debug, Deserialize)]
struct SidecarFrames {
    #[serde(default)]
    gaps: u64,
}

#[derive(Debug, Deserialize)]
struct SidecarHeader {
    my_callsign: String,
}

/// Match published transmissions against local recordings and decide
/// what to download. `existing` holds filenames already present in
/// the destination (for idempotent re-runs).
#[must_use]
pub fn plan(
    published: &[PublishedTx],
    local: &[LocalRecording],
    module: char,
    existing: &BTreeSet<String>,
) -> HarvestPlan {
    let mut result = HarvestPlan::default();

    let mut locals_by_key: BTreeMap<(&str, &str), Vec<usize>> = BTreeMap::new();
    for (i, rec) in local.iter().enumerate() {
        locals_by_key
            .entry((rec.callsign.as_str(), rec.stream_id.as_str()))
            .or_default()
            .push(i);
    }

    let mut claimed = vec![false; local.len()];
    let mut pairs: Vec<(&PublishedTx, Option<&LocalRecording>)> = Vec::new();
    for tx in published.iter().filter(|t| t.module == module) {
        let key = (
            crate::writer::sanitize_callsign(&tx.mycall),
            tx.stream_id.clone(),
        );
        let best = locals_by_key
            .get(&(key.0.as_str(), key.1.as_str()))
            .into_iter()
            .flatten()
            .filter(|&&i| claimed.get(i).is_some_and(|c| !*c))
            .filter_map(|&i| {
                let rec = local.get(i)?;
                let skew = (rec.started_at - tx.started_at).num_seconds().abs();
                (skew <= MATCH_TOLERANCE_SECS).then_some((i, rec, skew))
            })
            .min_by_key(|&(_, _, skew)| skew);
        if let Some((i, rec, _)) = best {
            if let Some(slot) = claimed.get_mut(i) {
                *slot = true;
            }
            result.matched += 1;
            pairs.push((tx, Some(rec)));
        } else {
            result.published_only += 1;
            pairs.push((tx, None));
        }
    }
    result.local_only = claimed.iter().filter(|&&c| !c).count();

    for (tx, matched_local) in pairs {
        let wanted: Vec<(&str, FetchReason)> = matched_local.map_or_else(
            || {
                tx.extensions
                    .iter()
                    .map(|e| (e.as_str(), FetchReason::Salvage))
                    .collect()
            },
            |rec| {
                let mut w = vec![
                    ("mp3", FetchReason::PairedAudio),
                    ("txt", FetchReason::PairedSidecar),
                ];
                if rec.gaps > 0 {
                    w.push(("dvrec", FetchReason::GapFillDvrec));
                }
                w
            },
        );
        for (ext, reason) in wanted {
            if !tx.extensions.contains(ext) {
                continue; // wanted but not published (partial upload)
            }
            let file_name = format!("{}.{ext}", tx.stem);
            if existing.contains(&file_name) {
                result.skipped_existing += 1;
                continue;
            }
            result.items.push(FetchItem {
                file_name,
                reason,
                local_stem: matched_local.map(|rec| rec.stem.clone()),
            });
        }
    }
    result
}

/// Derive the dashboard base URL for a reflector system name.
/// Only `REFnnn` systems live on the shared hosting scheme; other
/// systems need an explicit base URL.
#[must_use]
pub fn derived_base_url(system: &str) -> Option<reqwest::Url> {
    let digits = system.strip_prefix("REF")?;
    if digits.is_empty() || !digits.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    reqwest::Url::parse(&format!(
        "https://{}.dstargateway.org:8443/",
        system.to_ascii_lowercase()
    ))
    .ok()
}

/// Extend a base URL with `streams/<Y>/<M>/<D>` plus a final segment
/// (empty = trailing slash for the listing). A cannot-be-base URL is
/// returned unchanged; http(s) bases always extend.
fn streams_url(base: &reqwest::Url, date: NaiveDate, last: &str) -> reqwest::Url {
    let mut url = base.clone();
    if let Ok(mut segments) = url.path_segments_mut() {
        let _unused = segments
            .pop_if_empty()
            .push("streams")
            .push(&date.format("%Y").to_string())
            .push(&date.format("%m").to_string())
            .push(&date.format("%d").to_string())
            .push(last);
    }
    url
}

/// Listing URL for one date under a base URL.
#[must_use]
pub fn listing_url(base: &reqwest::Url, date: NaiveDate) -> reqwest::Url {
    streams_url(base, date, "")
}

/// Download URL for one published file under a base URL.
#[must_use]
pub fn file_url(base: &reqwest::Url, date: NaiveDate, file_name: &str) -> reqwest::Url {
    streams_url(base, date, file_name)
}

/// Split a recordings directory name (`"REF030-C"`) into its system
/// name and module letter. Returns `None` for names that are not
/// `<SYSTEM>-<MODULE>` (e.g. a stray `survey/` directory).
#[must_use]
pub fn split_target(name: &str) -> Option<(String, char)> {
    let (system, module_str) = name.rsplit_once('-')?;
    let mut chars = module_str.chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) if c.is_ascii_uppercase() && !system.is_empty() => {
            Some((system.to_string(), c))
        }
        _ => None,
    }
}

/// Harvest failures.
#[derive(Debug, thiserror::Error)]
pub enum HarvestError {
    /// No usable dashboard base URL.
    #[error("base url: {0}")]
    BaseUrl(String),
    /// Recordings directory name is not `<SYSTEM>-<MODULE>`.
    #[error("not a <SYSTEM>-<MODULE> directory name: {0:?}")]
    DirName(String),
    /// HTTP client construction failed.
    #[error("http client: {0}")]
    Client(#[source] reqwest::Error),
    /// Local filesystem failure (the archive is the point, so fatal).
    #[error("archive io: {0}")]
    Io(#[from] std::io::Error),
    /// Manifest serialization failed (unexpected).
    #[error("serialize: {0}")]
    Serialize(#[from] serde_json::Error),
    /// Atomic file write failed.
    #[error(transparent)]
    Write(#[from] crate::writer::WriteError),
}

/// One run's coverage summary, printed to the operator and appended
/// to the manifest.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunRecord {
    /// When the run started.
    pub run_at: DateTime<Utc>,
    /// Recordings directory name (`"REF030-C"`).
    pub target: String,
    /// Date harvested.
    pub date: NaiveDate,
    /// Listing URL used.
    pub listing_url: String,
    /// Listing HTTP status, if a response arrived.
    pub http_status: Option<u16>,
    /// Published transmissions for this module.
    pub published_tx: usize,
    /// Local recordings found for the date.
    pub local_recordings: usize,
    /// Local sidecars that existed but could not be read or parsed.
    pub local_skipped: usize,
    /// Published transmissions matched to a local recording.
    pub matched: usize,
    /// Published transmissions with no local counterpart.
    pub published_only: usize,
    /// Local recordings the publisher did not publish.
    pub local_only: usize,
    /// Files the plan wanted (excludes already-downloaded).
    pub planned: usize,
    /// Files downloaded this run.
    pub downloaded: usize,
    /// Downloads that failed (will retry next run).
    pub failed: usize,
    /// Wanted files already present from an earlier run.
    pub skipped_existing: usize,
    /// True when `limit` cut the plan short.
    pub truncated: bool,
    /// True when this run only planned (nothing written).
    pub dry_run: bool,
    /// Why the run could not complete: a listing fetch/HTTP failure,
    /// a robots.txt disallow, or a consecutive-failure stop.
    pub error: Option<String>,
}

/// One attempted download, appended to the manifest.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileRecord {
    /// When the download ran.
    pub fetched_at: DateTime<Utc>,
    /// Published filename.
    pub file_name: String,
    /// Full URL fetched.
    pub url: String,
    /// HTTP status, if a response arrived.
    pub http_status: Option<u16>,
    /// Body size written to disk.
    pub bytes: usize,
    /// Why this file was wanted.
    pub reason: FetchReason,
    /// Matched local recording stem, when paired.
    pub local_stem: Option<String>,
    /// Fetch/HTTP error, if the download failed.
    pub error: Option<String>,
}

/// Provenance record: one JSON line in `published/harvest.jsonl`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "record", rename_all = "kebab-case")]
pub enum ManifestRecord {
    /// A run summary.
    Run(RunRecord),
    /// An attempted download.
    File(FileRecord),
}

/// Options for building a [`Harvester`].
#[derive(Debug, Clone)]
pub struct HarvestOptions {
    /// Dashboard base URL override; default derives one per system
    /// via [`derived_base_url`].
    pub base_url: Option<String>,
    /// Cap on downloads per run.
    pub limit: Option<usize>,
    /// Plan and report only: write nothing, download nothing.
    pub dry_run: bool,
    /// Pause between downloads (default [`POLITE_GAP`]).
    pub fetch_gap: Duration,
    /// Operator callsign advertised in the User-Agent so the server
    /// owner can identify and contact who is fetching.
    pub operator: Option<String>,
}

impl Default for HarvestOptions {
    fn default() -> Self {
        Self {
            base_url: None,
            limit: None,
            dry_run: false,
            fetch_gap: POLITE_GAP,
            operator: None,
        }
    }
}

/// Downloads published recordings into `published/` subdirectories,
/// sequentially and politely, with per-file and per-run provenance.
#[derive(Debug)]
pub struct Harvester {
    client: reqwest::Client,
    base_url: Option<reqwest::Url>,
    limit: Option<usize>,
    dry_run: bool,
    fetch_gap: Duration,
}

impl Harvester {
    /// Build a harvester (validates the base URL override, if any).
    ///
    /// # Errors
    ///
    /// [`HarvestError::BaseUrl`] for an unparseable override;
    /// [`HarvestError::Client`] if the HTTP client cannot be built.
    pub fn new(options: HarvestOptions) -> Result<Self, HarvestError> {
        let HarvestOptions {
            base_url,
            limit,
            dry_run,
            fetch_gap,
            operator,
        } = options;
        let base_url = base_url
            .map(|raw| {
                reqwest::Url::parse(&raw).map_err(|e| HarvestError::BaseUrl(format!("{raw}: {e}")))
            })
            .transpose()?;
        let client = reqwest::Client::builder()
            .user_agent(build_user_agent(operator.as_deref()))
            .timeout(Duration::from_secs(30))
            .build()
            .map_err(HarvestError::Client)?;
        Ok(Self {
            client,
            base_url,
            limit,
            dry_run,
            fetch_gap,
        })
    }

    /// Harvest one `(target directory, date)`: fetch the listing,
    /// match against local recordings, download wanted files into
    /// `<date dir>/published/`, and append provenance. Listing and
    /// per-file fetch failures are recorded in the returned record,
    /// not raised; local I/O failures are fatal.
    ///
    /// # Errors
    ///
    /// [`HarvestError`] on invalid target names, missing base URL,
    /// or local filesystem failures.
    pub async fn harvest_dir(
        &self,
        recordings: &Path,
        target: &str,
        date: NaiveDate,
    ) -> Result<RunRecord, HarvestError> {
        let (system, module) =
            split_target(target).ok_or_else(|| HarvestError::DirName(target.to_string()))?;
        let base = match &self.base_url {
            Some(url) => url.clone(),
            None => derived_base_url(&system).ok_or_else(|| {
                HarvestError::BaseUrl(format!(
                    "no derivable dashboard URL for {system}; pass --base-url"
                ))
            })?,
        };
        let date_dir = recordings
            .join(target)
            .join(date.format("%Y-%m-%d").to_string());
        let published_dir = date_dir.join("published");
        let listing = listing_url(&base, date);

        let mut record = RunRecord {
            run_at: Utc::now(),
            target: target.to_string(),
            date,
            listing_url: listing.to_string(),
            http_status: None,
            published_tx: 0,
            local_recordings: 0,
            local_skipped: 0,
            matched: 0,
            published_only: 0,
            local_only: 0,
            planned: 0,
            downloaded: 0,
            failed: 0,
            skipped_existing: 0,
            truncated: false,
            dry_run: self.dry_run,
            error: None,
        };

        let local = load_local_recordings(&date_dir)?;
        record.local_recordings = local.recordings.len();
        record.local_skipped = local.skipped;

        // The politest interpretation wins: ask robots.txt first, and
        // if the streams tree is off-limits for us, walk away without
        // touching the listing.
        let mut robots_url = base.clone();
        robots_url.set_path("/robots.txt");
        let robots_blocked = match self.fetch(robots_url).await {
            Ok((200, body)) => robots_disallows(&String::from_utf8_lossy(&body), listing.path()),
            // Absent or unreadable robots: the operator-published
            // links themselves govern.
            _ => false,
        };
        if robots_blocked {
            record.error =
                Some("robots.txt disallows the streams tree for us; nothing fetched".to_string());
            return Ok(record);
        }

        match self.fetch(listing).await {
            Err(e) => record.error = Some(e.to_string()),
            Ok((status, body)) => {
                record.http_status = Some(status);
                match status {
                    // Nothing published for this date, which is normal.
                    404 => {}
                    200 => {
                        let published = parse_listing(&String::from_utf8_lossy(&body));
                        let existing = existing_files(&published_dir)?;
                        let plan = plan(&published, &local.recordings, module, &existing);
                        record.published_tx = plan.matched + plan.published_only;
                        record.matched = plan.matched;
                        record.published_only = plan.published_only;
                        record.local_only = plan.local_only;
                        record.skipped_existing = plan.skipped_existing;
                        record.planned = plan.items.len();
                        if !self.dry_run {
                            self.execute(&mut record, &plan, &base, date, &published_dir)
                                .await?;
                        }
                    }
                    other => record.error = Some(format!("http status {other}")),
                }
            }
        }

        // Append the run summary, but never create directory litter
        // for a date with nothing to do and nothing done before.
        if !self.dry_run
            && (record.planned > 0 || record.skipped_existing > 0 || published_dir.exists())
        {
            append_manifest(&published_dir, &ManifestRecord::Run(record.clone()))?;
        }
        Ok(record)
    }

    /// Download the planned files sequentially with the polite gap,
    /// appending a manifest record per attempt. Fetch/HTTP failures
    /// are recorded and counted; local I/O failures are fatal.
    async fn execute(
        &self,
        record: &mut RunRecord,
        plan: &HarvestPlan,
        base: &reqwest::Url,
        date: NaiveDate,
        published_dir: &Path,
    ) -> Result<(), HarvestError> {
        let take = self.limit.unwrap_or(usize::MAX);
        record.truncated = plan.items.len() > take;
        let mut consecutive_failures = 0usize;
        for item in plan.items.iter().take(take) {
            if !self.fetch_gap.is_zero() {
                tokio::time::sleep(self.fetch_gap).await;
            }
            let url = file_url(base, date, &item.file_name);
            let mut file_record = FileRecord {
                fetched_at: Utc::now(),
                file_name: item.file_name.clone(),
                url: url.to_string(),
                http_status: None,
                bytes: 0,
                reason: item.reason,
                local_stem: item.local_stem.clone(),
                error: None,
            };
            match self.fetch(url).await {
                Err(e) => {
                    file_record.error = Some(e.to_string());
                    record.failed += 1;
                    consecutive_failures += 1;
                }
                Ok((status, body)) => {
                    file_record.http_status = Some(status);
                    if status == 200 {
                        // Listing names are single path components by
                        // construction (parse_listing drops any href
                        // containing '/').
                        std::fs::create_dir_all(published_dir)?;
                        crate::writer::write_atomic(
                            &published_dir.join(&item.file_name),
                            &body,
                            false,
                        )?;
                        file_record.bytes = body.len();
                        record.downloaded += 1;
                        consecutive_failures = 0;
                        tracing::debug!(
                            file = %item.file_name,
                            bytes = body.len(),
                            "downloaded"
                        );
                    } else {
                        file_record.error = Some(format!("http status {status}"));
                        record.failed += 1;
                        consecutive_failures += 1;
                    }
                }
            }
            append_manifest(published_dir, &ManifestRecord::File(file_record))?;
            if consecutive_failures >= MAX_CONSECUTIVE_FAILURES {
                // A struggling or purging server should not receive
                // the rest of the plan; everything left retries on a
                // later run.
                record.error = Some(format!(
                    "stopped after {MAX_CONSECUTIVE_FAILURES} consecutive download failures. \
                     Leaving the server alone; remaining files retry next run"
                ));
                break;
            }
        }
        Ok(())
    }

    async fn fetch(&self, url: reqwest::Url) -> Result<(u16, Vec<u8>), reqwest::Error> {
        let resp = self.client.get(url).send().await?;
        let status = resp.status().as_u16();
        let body = resp.bytes().await?;
        Ok((status, body.to_vec()))
    }
}

/// Names of nonzero-size files already in the destination directory.
fn existing_files(dir: &Path) -> std::io::Result<BTreeSet<String>> {
    let mut out = BTreeSet::new();
    if !dir.exists() {
        return Ok(out);
    }
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let meta = entry.metadata()?;
        if meta.is_file()
            && meta.len() > 0
            && let Some(name) = entry.file_name().to_str()
        {
            let _unused = out.insert(name.to_string());
        }
    }
    Ok(out)
}

/// Append one provenance record to `<published>/harvest.jsonl`.
fn append_manifest(published_dir: &Path, record: &ManifestRecord) -> Result<(), HarvestError> {
    std::fs::create_dir_all(published_dir)?;
    let path = published_dir.join("harvest.jsonl");
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    writeln!(file, "{}", serde_json::to_string(record)?)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    /// Real rows sampled from the live REF030 listing (2026-07-11):
    /// Apache autoindex, every file linked twice (icon + name), plus
    /// the parent-directory row that must be ignored.
    const LISTING_FIXTURE: &str = concat!(
        "<html><body><h1>Index of /streams/2026/07/11</h1><table>",
        "<tr><td valign=\"top\"><a href=\"/streams/2026/07/\">",
        "<img src=\"/icons/back.gif\" alt=\"[PARENTDIR]\"/></a></td>",
        "<td><a href=\"/streams/2026/07/\">Parent Directory</a></td>",
        "<td>&nbsp;</td><td align=\"right\">  - </td></tr>",
        "<tr><td valign=\"top\"><a href=\"REF030-C--20260711-175407.594--MM3TWA--e658.txt\">",
        "<img src=\"/icons/text.gif\" alt=\"[TXT]\"/></a></td>",
        "<td><a href=\"REF030-C--20260711-175407.594--MM3TWA--e658.txt\">",
        "REF030-C--20260711-175407.594--MM3TWA--e658.txt</a></td>",
        "<td align=\"right\">2026-07-11 17:55  </td><td align=\"right\">377 </td></tr>",
        "<tr><td valign=\"top\"><a href=\"REF030-C--20260711-175407.594--MM3TWA--e658.mp3\">",
        "<img src=\"/icons/sound2.gif\" alt=\"[SND]\"/></a></td>",
        "<td><a href=\"REF030-C--20260711-175407.594--MM3TWA--e658.mp3\">",
        "REF030-C--20260711-175407.594--MM3TWA--e658.mp3</a></td>",
        "<td align=\"right\">2026-07-11 17:55  </td><td align=\"right\">311K</td></tr>",
        "<tr><td valign=\"top\"><a href=\"REF030-C--20260711-175407.594--MM3TWA--e658.dvrec\">",
        "<img src=\"/icons/unknown.gif\" alt=\"[   ]\"/></a></td>",
        "<td><a href=\"REF030-C--20260711-175407.594--MM3TWA--e658.dvrec\">",
        "REF030-C--20260711-175407.594--MM3TWA--e658.dvrec</a></td>",
        "<td align=\"right\">2026-07-11 17:55  </td><td align=\"right\">334K</td></tr>",
        "<tr><td valign=\"top\"><a href=\"REF030-C--20260711-182309.487--K8THH--e11d.txt\">",
        "<img src=\"/icons/text.gif\" alt=\"[TXT]\"/></a></td>",
        "<td><a href=\"REF030-C--20260711-182309.487--K8THH--e11d.txt\">",
        "REF030-C--20260711-182309.487--K8THH--e11d.txt</a></td>",
        "<td align=\"right\">2026-07-11 18:23  </td><td align=\"right\">345 </td></tr>",
        // Contrived: module-A row (filtered by module elsewhere) and a
        // MYCALL containing a space, percent-encoded by the server.
        "<tr><td></td><td><a href=\"REF030-A--20260711-120000.000--W1AW--0abc.mp3\">",
        "REF030-A--20260711-120000.000--W1AW--0abc.mp3</a></td></tr>",
        "<tr><td></td><td><a href=\"REF030-C--20260711-130000.250--KJ5CJS%20B--96da.mp3\">",
        "REF030-C--20260711-130000.250--KJ5CJS B--96da.mp3</a></td></tr>",
        "</table></body></html>"
    );

    fn ts(s: &str) -> Result<DateTime<Utc>, Box<dyn std::error::Error>> {
        Ok(DateTime::parse_from_rfc3339(s)?.with_timezone(&Utc))
    }

    fn published(
        stem: &str,
        module: char,
        started_at: &str,
        mycall: &str,
        stream_id: &str,
        extensions: &[&str],
    ) -> Result<PublishedTx, Box<dyn std::error::Error>> {
        Ok(PublishedTx {
            stem: stem.to_string(),
            system: "REF030".to_string(),
            module,
            started_at: ts(started_at)?,
            mycall: mycall.to_string(),
            stream_id: stream_id.to_string(),
            extensions: extensions.iter().map(ToString::to_string).collect(),
        })
    }

    fn local(
        stem: &str,
        callsign: &str,
        stream_id: &str,
        started_at: &str,
        gaps: u64,
    ) -> Result<LocalRecording, Box<dyn std::error::Error>> {
        Ok(LocalRecording {
            stem: stem.to_string(),
            callsign: callsign.to_string(),
            stream_id: stream_id.to_string(),
            started_at: ts(started_at)?,
            gaps,
        })
    }

    #[test]
    fn listing_parse_groups_extensions_by_stem() -> TestResult {
        let txs = parse_listing(LISTING_FIXTURE);
        assert_eq!(txs.len(), 4, "{txs:?}");

        let e658 = txs
            .iter()
            .find(|t| t.stream_id == "E658")
            .ok_or("E658 missing")?;
        assert_eq!(e658.stem, "REF030-C--20260711-175407.594--MM3TWA--e658");
        assert_eq!(e658.system, "REF030");
        assert_eq!(e658.module, 'C');
        assert_eq!(e658.mycall, "MM3TWA");
        assert_eq!(
            e658.started_at.to_rfc3339(),
            "2026-07-11T17:54:07.594+00:00"
        );
        let exts: Vec<&str> = e658.extensions.iter().map(String::as_str).collect();
        assert_eq!(exts, ["dvrec", "mp3", "txt"], "deduped and grouped");

        let partial = txs
            .iter()
            .find(|t| t.stream_id == "E11D")
            .ok_or("E11D missing")?;
        assert_eq!(
            partial
                .extensions
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            ["txt"],
            "partial publication survives"
        );
        Ok(())
    }

    #[test]
    fn listing_parse_decodes_percent_escapes() -> TestResult {
        let txs = parse_listing(LISTING_FIXTURE);
        let spaced = txs
            .iter()
            .find(|t| t.stream_id == "96DA")
            .ok_or("96DA missing")?;
        assert_eq!(spaced.mycall, "KJ5CJS B", "%20 decoded");
        assert_eq!(
            spaced.stem, "REF030-C--20260711-130000.250--KJ5CJS B--96da",
            "stem stored decoded"
        );
        Ok(())
    }

    #[test]
    fn listing_parse_is_ordered_by_start_time() {
        let stems: Vec<String> = parse_listing(LISTING_FIXTURE)
            .into_iter()
            .map(|t| t.stem)
            .collect();
        // Chronological, not lexical: the 12:00 module-A row precedes
        // the 17:54 row even though "REF030-A" sorts first lexically
        // only by accident; assert explicit chronology instead.
        assert!(
            stems.first().is_some_and(|s| s.contains("120000")),
            "{stems:?}"
        );
        assert!(
            stems.last().is_some_and(|s| s.contains("182309")),
            "{stems:?}"
        );
    }

    #[test]
    fn listing_rejects_percent_encoded_path_traversal() {
        // A hostile listing whose href has no LITERAL '/' but decodes
        // to an absolute path or a parent-dir escape. The raw-href
        // filter alone would let these through; the decoded-name guard
        // must drop them so nothing is ever written outside the tree.
        let hostile = concat!(
            "<a href=\"%2Ftmp%2Fpwned-C--20260711-175407.594--W1AW--e658.mp3\">x</a>",
            "<a href=\"%2E%2E%2F%2E%2E%2Fescape-C--20260711-175407.594--W1AW--e658.mp3\">y</a>",
            // A legitimate basename must still survive.
            "<a href=\"REF030-C--20260711-175407.594--W1AW--e658.mp3\">ok</a>",
        );
        let stems: Vec<String> = parse_listing(hostile).into_iter().map(|t| t.stem).collect();
        assert_eq!(stems.len(), 1, "only the safe basename survives: {stems:?}");
        assert!(
            stems.first().is_some_and(|s| s.starts_with("REF030-C")),
            "{stems:?}"
        );
    }

    #[test]
    fn is_safe_basename_guards_separators_and_traversal() {
        assert!(is_safe_basename(
            "REF030-C--20260711-175407.594--W1AW--e658.mp3"
        ));
        assert!(!is_safe_basename("/tmp/evil.txt"));
        assert!(!is_safe_basename("../escape.txt"));
        assert!(!is_safe_basename("a/b.txt"));
        assert!(!is_safe_basename("a\\b.txt"));
        assert!(!is_safe_basename(".."));
        assert!(!is_safe_basename(""));
    }

    #[test]
    fn published_name_rejects_malformed_variants() {
        // Missing module separator.
        assert!(parse_published_name("REF030--20260711-175407.594--X--abcd.mp3").is_none());
        // Unparseable datetime.
        assert!(parse_published_name("REF030-C--garbage--MM3TWA--e658.mp3").is_none());
        // Stream id not 4 hex digits.
        assert!(parse_published_name("REF030-C--20260711-175407.594--MM3TWA--zz.mp3").is_none());
        // Unknown extension.
        assert!(parse_published_name("REF030-C--20260711-175407.594--MM3TWA--e658.wav").is_none());
        // Too few segments.
        assert!(parse_published_name("REF030-C--20260711-175407.594--e658.mp3").is_none());
    }

    #[test]
    fn published_name_keeps_dashes_inside_mycall() -> TestResult {
        // Contrived: a MYCALL that itself contains "--" must not
        // break field splitting (join of the middle segments).
        let (tx, ext) = parse_published_name("REF030-C--20260711-175407.594--A--B--e658.mp3")
            .ok_or("should parse")?;
        assert_eq!(tx.mycall, "A--B");
        assert_eq!(ext, "mp3");
        Ok(())
    }

    #[test]
    fn plan_pairs_matched_transmissions_with_audio_and_sidecar() -> TestResult {
        let p = vec![published(
            "REF030-C--20260711-175407.594--MM3TWA--e658",
            'C',
            "2026-07-11T17:54:07.594Z",
            "MM3TWA",
            "E658",
            &["dvrec", "mp3", "txt"],
        )?];
        let l = vec![local(
            "20260711T175407Z_MM3TWA_E658",
            "MM3TWA",
            "E658",
            "2026-07-11T17:54:07.608Z",
            0,
        )?];
        let plan = plan(&p, &l, 'C', &BTreeSet::new());
        assert_eq!(plan.matched, 1);
        assert_eq!(plan.published_only, 0);
        assert_eq!(plan.local_only, 0);
        let names: Vec<(&str, FetchReason)> = plan
            .items
            .iter()
            .map(|i| (i.file_name.as_str(), i.reason))
            .collect();
        assert_eq!(
            names,
            [
                (
                    "REF030-C--20260711-175407.594--MM3TWA--e658.mp3",
                    FetchReason::PairedAudio
                ),
                (
                    "REF030-C--20260711-175407.594--MM3TWA--e658.txt",
                    FetchReason::PairedSidecar
                ),
            ],
            "clean capture wants audio + sidecar, not the dvrec"
        );
        assert!(
            plan.items
                .iter()
                .all(|i| i.local_stem.as_deref() == Some("20260711T175407Z_MM3TWA_E658")),
            "{:?}",
            plan.items
        );
        Ok(())
    }

    #[test]
    fn plan_fetches_dvrec_for_gapped_captures() -> TestResult {
        let p = vec![published(
            "REF030-C--20260711-175407.594--MM3TWA--e658",
            'C',
            "2026-07-11T17:54:07.594Z",
            "MM3TWA",
            "E658",
            &["dvrec", "mp3", "txt"],
        )?];
        let l = vec![local(
            "20260711T175407Z_MM3TWA_E658",
            "MM3TWA",
            "E658",
            "2026-07-11T17:54:07.608Z",
            3,
        )?];
        let plan = plan(&p, &l, 'C', &BTreeSet::new());
        assert!(
            plan.items.iter().any(|i| {
                i.reason == FetchReason::GapFillDvrec
                    && Path::new(&i.file_name).extension() == Some("dvrec".as_ref())
            }),
            "{:?}",
            plan.items
        );
        Ok(())
    }

    #[test]
    fn plan_salvages_unmatched_published_transmissions() -> TestResult {
        let p = vec![published(
            "REF030-C--20260711-180144.540--WB3JSW--9485",
            'C',
            "2026-07-11T18:01:44.540Z",
            "WB3JSW",
            "9485",
            &["dvrec", "mp3", "txt"],
        )?];
        let plan = plan(&p, &[], 'C', &BTreeSet::new());
        assert_eq!(plan.matched, 0);
        assert_eq!(plan.published_only, 1);
        assert_eq!(plan.items.len(), 3, "{:?}", plan.items);
        assert!(
            plan.items
                .iter()
                .all(|i| i.reason == FetchReason::Salvage && i.local_stem.is_none()),
            "{:?}",
            plan.items
        );
        Ok(())
    }

    #[test]
    fn plan_matches_sanitized_callsigns() -> TestResult {
        // Published "KJ5CJS B" (raw wire MYCALL) must pair with the
        // local filename form "KJ5CJS-B".
        let p = vec![published(
            "REF030-C--20260711-130000.250--KJ5CJS B--96da",
            'C',
            "2026-07-11T13:00:00.250Z",
            "KJ5CJS B",
            "96DA",
            &["mp3", "txt"],
        )?];
        let l = vec![local(
            "20260711T130000Z_KJ5CJS-B_96DA",
            "KJ5CJS-B",
            "96DA",
            "2026-07-11T13:00:00.300Z",
            0,
        )?];
        let plan = plan(&p, &l, 'C', &BTreeSet::new());
        assert_eq!(plan.matched, 1, "{plan:?}");
        Ok(())
    }

    #[test]
    fn plan_filters_other_modules() -> TestResult {
        let p = vec![published(
            "REF030-A--20260711-120000.000--W1AW--0abc",
            'A',
            "2026-07-11T12:00:00.000Z",
            "W1AW",
            "0ABC",
            &["mp3", "txt"],
        )?];
        let plan = plan(&p, &[], 'C', &BTreeSet::new());
        assert_eq!(plan.published_only, 0, "other module is out of scope");
        assert!(plan.items.is_empty());
        Ok(())
    }

    #[test]
    fn plan_counts_local_only_recordings() -> TestResult {
        let l = vec![local(
            "20260711T130000Z_G3ODP_83BD",
            "G3ODP",
            "83BD",
            "2026-07-11T13:00:00.000Z",
            0,
        )?];
        let plan = plan(&[], &l, 'C', &BTreeSet::new());
        assert_eq!(plan.local_only, 1);
        assert!(plan.items.is_empty());
        Ok(())
    }

    #[test]
    fn plan_disambiguates_repeated_key_by_time() -> TestResult {
        // Same station, same stream id twice in a day (contrived):
        // each published tx must pair with the nearest local start.
        let p = vec![
            published(
                "REF030-C--20260711-100000.000--MM3TWA--aaaa",
                'C',
                "2026-07-11T10:00:00.000Z",
                "MM3TWA",
                "AAAA",
                &["mp3", "txt"],
            )?,
            published(
                "REF030-C--20260711-170000.000--MM3TWA--aaaa",
                'C',
                "2026-07-11T17:00:00.000Z",
                "MM3TWA",
                "AAAA",
                &["mp3", "txt"],
            )?,
        ];
        let l = vec![
            local(
                "20260711T170001Z_MM3TWA_AAAA",
                "MM3TWA",
                "AAAA",
                "2026-07-11T17:00:01.000Z",
                0,
            )?,
            local(
                "20260711T100001Z_MM3TWA_AAAA",
                "MM3TWA",
                "AAAA",
                "2026-07-11T10:00:01.000Z",
                0,
            )?,
        ];
        let plan = plan(&p, &l, 'C', &BTreeSet::new());
        assert_eq!(plan.matched, 2);
        let morning = plan
            .items
            .iter()
            .find(|i| i.file_name.contains("100000"))
            .ok_or("morning item")?;
        assert_eq!(
            morning.local_stem.as_deref(),
            Some("20260711T100001Z_MM3TWA_AAAA"),
            "nearest-in-time pairing"
        );
        Ok(())
    }

    #[test]
    fn plan_rejects_matches_beyond_tolerance() -> TestResult {
        let p = vec![published(
            "REF030-C--20260711-100000.000--MM3TWA--aaaa",
            'C',
            "2026-07-11T10:00:00.000Z",
            "MM3TWA",
            "AAAA",
            &["mp3"],
        )?];
        let l = vec![local(
            "20260711T110000Z_MM3TWA_AAAA",
            "MM3TWA",
            "AAAA",
            "2026-07-11T11:00:00.000Z",
            0,
        )?];
        let plan = plan(&p, &l, 'C', &BTreeSet::new());
        assert_eq!(plan.matched, 0, "an hour apart is not the same tx");
        assert_eq!(plan.published_only, 1);
        assert_eq!(plan.local_only, 1);
        Ok(())
    }

    #[test]
    fn plan_skips_files_already_downloaded() -> TestResult {
        let p = vec![published(
            "REF030-C--20260711-175407.594--MM3TWA--e658",
            'C',
            "2026-07-11T17:54:07.594Z",
            "MM3TWA",
            "E658",
            &["mp3", "txt"],
        )?];
        let l = vec![local(
            "20260711T175407Z_MM3TWA_E658",
            "MM3TWA",
            "E658",
            "2026-07-11T17:54:07.608Z",
            0,
        )?];
        let existing =
            BTreeSet::from(["REF030-C--20260711-175407.594--MM3TWA--e658.mp3".to_string()]);
        let plan = plan(&p, &l, 'C', &existing);
        assert_eq!(plan.skipped_existing, 1);
        assert_eq!(plan.items.len(), 1, "{:?}", plan.items);
        assert!(
            plan.items
                .iter()
                .all(|i| Path::new(&i.file_name).extension() == Some("txt".as_ref())),
            "only the sidecar remains"
        );
        Ok(())
    }

    #[test]
    fn load_local_recordings_reads_sidecars() -> TestResult {
        let dir = tempfile::tempdir()?;
        // Field subset mirrors the real stargazer-recording/1 schema.
        std::fs::write(
            dir.path().join("20260711T175407Z_MM3TWA_E658.json"),
            r#"{
              "schema": "stargazer-recording/1",
              "stream_id": "E658",
              "started_at": "2026-07-11T17:54:07.608Z",
              "frames": { "received": 3975, "expected": 3975, "gaps": 2 },
              "header": { "my_callsign": "MM3TWA" }
            }"#,
        )?;
        std::fs::write(
            dir.path().join("20260711T180000Z_UNKNOWN_1234.json"),
            r#"{
              "schema": "stargazer-recording/1",
              "stream_id": "1234",
              "started_at": "2026-07-11T18:00:00.000Z",
              "frames": { "received": 10, "expected": 10, "gaps": 0 },
              "header": null
            }"#,
        )?;
        std::fs::write(dir.path().join("garbage.json"), "not json")?;
        std::fs::write(dir.path().join("note.txt"), "ignored")?;

        let loaded = load_local_recordings(dir.path())?;
        assert_eq!(loaded.skipped, 1, "garbage.json");
        assert_eq!(loaded.recordings.len(), 2);
        let rec = loaded
            .recordings
            .iter()
            .find(|r| r.stream_id == "E658")
            .ok_or("E658")?;
        assert_eq!(rec.stem, "20260711T175407Z_MM3TWA_E658");
        assert_eq!(rec.callsign, "MM3TWA");
        assert_eq!(rec.gaps, 2);
        assert_eq!(rec.started_at.to_rfc3339(), "2026-07-11T17:54:07.608+00:00");
        let unknown = loaded
            .recordings
            .iter()
            .find(|r| r.stream_id == "1234")
            .ok_or("1234")?;
        assert_eq!(unknown.callsign, "UNKNOWN", "headerless recording");
        Ok(())
    }

    #[test]
    fn urls_derive_encode_and_partition_by_date() -> TestResult {
        let base = derived_base_url("REF030").ok_or("REF systems derive")?;
        assert_eq!(
            base.as_str(),
            "https://ref030.dstargateway.org:8443/",
            "lowercased shared-hosting scheme"
        );
        assert!(
            derived_base_url("XRF757").is_none(),
            "non-REF needs --base-url"
        );
        assert!(derived_base_url("REF999X").is_none(), "not a REFnnn name");

        let date = NaiveDate::from_ymd_opt(2026, 7, 11).ok_or("date")?;
        assert_eq!(
            listing_url(&base, date).as_str(),
            "https://ref030.dstargateway.org:8443/streams/2026/07/11/"
        );
        assert_eq!(
            file_url(
                &base,
                date,
                "REF030-C--20260711-130000.250--KJ5CJS B--96da.mp3"
            )
            .as_str(),
            "https://ref030.dstargateway.org:8443/streams/2026/07/11/REF030-C--20260711-130000.250--KJ5CJS%20B--96da.mp3",
            "space re-encoded on the way out"
        );
        Ok(())
    }

    #[test]
    fn default_pacing_is_gentle() {
        assert_eq!(
            HarvestOptions::default().fetch_gap,
            Duration::from_secs(2),
            "default pace stays well under any rate-limit radar"
        );
    }

    #[test]
    fn user_agent_identifies_tool_and_operator() {
        let anon = build_user_agent(None);
        assert!(anon.starts_with("stargazer-harvest/"), "{anon}");
        assert!(anon.contains("amateur radio research"), "{anon}");
        assert!(!anon.contains("operator"), "{anon}");

        let signed = build_user_agent(Some("KQ4NIT"));
        assert!(signed.contains("operator KQ4NIT"), "{signed}");
    }

    #[test]
    fn robots_rules_parse_and_match() {
        // No robots, empty rules, or unrelated prefixes: allowed.
        assert!(!robots_disallows("", "/streams/2026/07/11/"));
        assert!(!robots_disallows(
            "User-agent: *\nDisallow:",
            "/streams/2026/07/11/"
        ));
        assert!(!robots_disallows(
            "User-agent: *\nDisallow: /admin",
            "/streams/2026/07/11/"
        ));
        // Rules scoped to other agents do not apply to us.
        assert!(!robots_disallows(
            "User-agent: googlebot\nDisallow: /",
            "/streams/2026/07/11/"
        ));
        // Wildcard-agent prefixes match our path.
        assert!(robots_disallows(
            "User-agent: *\nDisallow: /streams/",
            "/streams/2026/07/11/"
        ));
        assert!(robots_disallows(
            "User-agent: *\nDisallow: /",
            "/streams/2026/07/11/"
        ));
        // Rules addressed to our own product token bind hardest.
        assert!(robots_disallows(
            "User-agent: stargazer-harvest\nDisallow: /",
            "/streams/2026/07/11/"
        ));
        // Case-insensitive keys, comments, and blank lines survive.
        assert!(robots_disallows(
            "# be nice\nUSER-AGENT: *   # everyone\n\ndisallow: /streams",
            "/streams/2026/07/11/"
        ));
        // A later agent block does not leak rules into the first.
        assert!(!robots_disallows(
            "User-agent: *\nDisallow:\n\nUser-agent: badbot\nDisallow: /",
            "/streams/2026/07/11/"
        ));
    }

    #[test]
    fn percent_decode_handles_escapes_and_passthrough() {
        assert_eq!(percent_decode("KJ5CJS%20B"), "KJ5CJS B");
        assert_eq!(percent_decode("plain"), "plain");
        assert_eq!(
            percent_decode("bad%2"),
            "bad%2",
            "truncated escape survives"
        );
        assert_eq!(
            percent_decode("100%zz"),
            "100%zz",
            "non-hex escape survives"
        );
    }
}
