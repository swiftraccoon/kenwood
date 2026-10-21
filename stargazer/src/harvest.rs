// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later

//! Bounded collection of published recordings with local matching metadata.
//!
//! Listing names identify `.dvrec`, `.mp3` and `.txt` artifacts. Matching uses
//! normalized station labels, hexadecimal stream identifiers and a 120-second
//! start-time tolerance. A match selects candidate counterparts; byte identity,
//! audio quality, retention and service coverage require separate verification.
//!
//! Robots policy follows [RFC 9309](https://www.rfc-editor.org/rfc/rfc9309.html).
//! Requests follow at most five redirects and reject oversized response bodies.
//! Downloads run sequentially, separated by at least two seconds, with a required
//! operator identity and a three-consecutive-failure stop. Unreachable robots
//! policy prevents listing/download requests. Existing artifacts are preserved.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read as _, Write as _};
use std::num::NonZeroUsize;
use std::path::Path;
use std::str::FromStr;
use std::time::Duration;

use chrono::{DateTime, NaiveDate, Utc};
use dstar_gateway_core::{Module, RoutingCallsign, StreamId};
use serde::{Deserialize, Serialize};

use crate::config::{ReflectorName, TargetKey};
use crate::http::HttpResponse;

/// Identify the tool, version, purpose and responsible operator.
fn build_user_agent(operator: RoutingCallsign) -> String {
    let mut ua = concat!(
        "stargazer-harvest/",
        env!("CARGO_PKG_VERSION"),
        " (amateur radio research; pairing our own reflector captures"
    )
    .to_string();
    ua.push_str("; operator ");
    ua.push_str(&operator.to_string());
    ua.push(')');
    ua
}

/// Evaluate the selected agent groups, longest matching rule and allow ties.
///
/// Rules follow RFC 9309 §§2.2.1-2.2.3. Wildcards match zero or more bytes;
/// only a final dollar anchors the end. Unknown records preserve their group.
#[cfg(test)]
fn robots_disallows(robots: &str, path: &str) -> bool {
    RobotsRules::new(robots).disallows(path)
}

#[derive(Debug)]
struct RobotsRule {
    pattern: Vec<u8>,
    exact: bool,
    allow: bool,
    priority: usize,
}

#[derive(Debug, Default)]
struct RobotsRules(Vec<RobotsRule>);
impl RobotsRules {
    fn new(body: &str) -> Self {
        let records = || {
            body.lines()
                .filter_map(|line| line.split('#').next()?.trim().split_once(':'))
                .map(|(key, value)| (key.trim(), value.trim()))
        };
        let specific = records().any(|(key, value)| {
            key.eq_ignore_ascii_case("user-agent")
                && value.eq_ignore_ascii_case("stargazer-harvest")
        });
        let mut applies = false;
        let mut in_rules = false;
        let mut rules = Vec::new();
        for (key, value) in records() {
            if key.eq_ignore_ascii_case("user-agent") {
                if in_rules {
                    applies = false;
                    in_rules = false;
                }
                if value.eq_ignore_ascii_case("stargazer-harvest") || (!specific && value == "*") {
                    applies = true;
                }
            } else if key.eq_ignore_ascii_case("allow") || key.eq_ignore_ascii_case("disallow") {
                in_rules = true;
                if applies && !value.is_empty() {
                    let exact = value.ends_with('$');
                    let raw = value.strip_suffix('$').unwrap_or(value);
                    let pattern = normalize_robots_path(raw, true);
                    let priority = pattern.len() + usize::from(exact);
                    rules.push(RobotsRule {
                        pattern,
                        exact,
                        priority,
                        allow: key.eq_ignore_ascii_case("allow"),
                    });
                }
            }
        }
        Self(rules)
    }

    fn disallows(&self, path: &str) -> bool {
        let path = normalize_robots_path(path, false);
        let best = self
            .0
            .iter()
            .filter(|rule| wildcard_matches(&rule.pattern, &path, rule.exact))
            .max_by_key(|rule| (rule.priority, rule.allow));
        best.is_some_and(|rule| !rule.allow)
    }
}

fn normalize_robots_path(value: &str, pattern: bool) -> Vec<u8> {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut normalized = Vec::with_capacity(value.len());
    let mut bytes = value.as_bytes().iter().copied().peekable();
    while let Some(byte) = bytes.next() {
        if byte == b'%' {
            let mut following = bytes.clone();
            if let (Some(high), Some(low)) = (following.next(), following.next())
                && let (Some(high), Some(low)) =
                    (char::from(high).to_digit(16), char::from(low).to_digit(16))
                && let Ok(decoded) = u8::try_from(high * 16 + low)
            {
                bytes = following;
                if decoded.is_ascii_alphanumeric() || b"-._~".contains(&decoded) {
                    normalized.push(decoded);
                } else {
                    normalized.push(b'%');
                    normalized.push(*HEX.get(usize::from(decoded >> 4)).unwrap_or(&b'0'));
                    normalized.push(*HEX.get(usize::from(decoded & 15)).unwrap_or(&b'0'));
                }
                continue;
            }
        }
        if !byte.is_ascii() || byte == b'$' || (!pattern && byte == b'*') {
            normalized.push(b'%');
            normalized.push(*HEX.get(usize::from(byte >> 4)).unwrap_or(&b'0'));
            normalized.push(*HEX.get(usize::from(byte & 15)).unwrap_or(&b'0'));
        } else {
            normalized.push(byte);
        }
    }
    normalized
}

fn wildcard_matches(pattern: &[u8], path: &[u8], exact: bool) -> bool {
    let mut pattern_offset = 0;
    let mut path_offset = 0;
    let mut star = None;
    let mut retry = 0;
    while path_offset < path.len() {
        if pattern_offset == pattern.len() && !exact {
            return true;
        }
        if pattern.get(pattern_offset) == Some(&b'*') {
            star = Some(pattern_offset);
            pattern_offset += 1;
            retry = path_offset;
        } else if pattern.get(pattern_offset) == path.get(path_offset) {
            pattern_offset += 1;
            path_offset += 1;
        } else if let Some(star) = star {
            retry += 1;
            path_offset = retry;
            pattern_offset = star + 1;
        } else {
            return false;
        }
    }
    while pattern.get(pattern_offset) == Some(&b'*') {
        pattern_offset += 1;
    }
    pattern_offset == pattern.len()
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
    stem: String,
    /// Reflector system name (e.g. `"REF030"`).
    system: String,
    /// Module letter.
    module: char,
    /// Transmission start per the publisher's clock (UTC).
    started_at: DateTime<Utc>,
    /// MYCALL as published (trimmed, unsanitized).
    mycall: String,
    /// Checked nonzero transmission identifier.
    stream_id: StreamId,
    /// Extensions available for this stem.
    extensions: BTreeSet<String>,
}

impl PublishedTx {
    /// Borrow the published filename stem.
    #[must_use]
    pub fn stem(&self) -> &str {
        &self.stem
    }
    /// Borrow the observed reflector name.
    #[must_use]
    pub fn system(&self) -> &str {
        &self.system
    }
    /// Return the observed uppercase module letter.
    #[must_use]
    pub const fn module(&self) -> char {
        self.module
    }
    /// Return the publisher's UTC start timestamp.
    #[must_use]
    pub const fn started_at(&self) -> DateTime<Utc> {
        self.started_at
    }
    /// Borrow the publisher's station label.
    #[must_use]
    pub fn mycall(&self) -> &str {
        &self.mycall
    }
    /// Return the checked nonzero transmission identifier.
    #[must_use]
    pub const fn stream_id(&self) -> StreamId {
        self.stream_id
    }
    /// Iterate available extension strings in lexical order.
    pub fn extensions(&self) -> std::collections::btree_set::Iter<'_, String> {
        self.extensions.iter()
    }
}

/// Pairing-relevant metadata of one local recording.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalRecording {
    /// Filename stem (no extension).
    stem: String,
    /// Callsign in filename form (sanitized, uppercase).
    callsign: String,
    /// Checked nonzero transmission identifier.
    stream_id: StreamId,
    /// Transmission start per the local clock (UTC).
    started_at: DateTime<Utc>,
    /// Sequence gaps observed during capture.
    gaps: u64,
}

impl LocalRecording {
    /// Borrow the local artifact stem.
    #[must_use]
    pub fn stem(&self) -> &str {
        &self.stem
    }
    /// Borrow the normalized station label used for matching.
    #[must_use]
    pub fn callsign(&self) -> &str {
        &self.callsign
    }
    /// Return the checked nonzero transmission identifier.
    #[must_use]
    pub const fn stream_id(&self) -> StreamId {
        self.stream_id
    }
    /// Return the local UTC start timestamp.
    #[must_use]
    pub const fn started_at(&self) -> DateTime<Utc> {
        self.started_at
    }
    /// Return the recorded sequence-gap count.
    #[must_use]
    pub const fn gaps(&self) -> u64 {
        self.gaps
    }
}

/// Local recordings loaded from a date directory.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
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
    /// Reference audio for a locally captured transmission.
    PairedAudio,
    /// Publisher's metadata sidecar for a paired transmission.
    PairedSidecar,
    /// Packet log to repair a gapped local capture.
    GapFillDvrec,
    /// Transmission missed entirely: take everything published.
    Salvage,
}

/// One planned download.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchItem {
    /// Published filename (stem + extension).
    file_name: String,
    /// Why this file is wanted.
    reason: FetchReason,
    /// Stem of the matched local recording, when paired.
    local_stem: Option<String>,
}

impl FetchItem {
    /// Borrow the validated single-component published filename.
    #[must_use]
    pub fn file_name(&self) -> &str {
        &self.file_name
    }
    /// Return why this file is selected.
    #[must_use]
    pub const fn reason(&self) -> FetchReason {
        self.reason
    }
    /// Borrow the local counterpart's stem, when paired.
    #[must_use]
    pub fn local_stem(&self) -> Option<&str> {
        self.local_stem.as_deref()
    }
}

/// Download plan plus pairing coverage for one date directory.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
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
    ) && !name.ends_with([' ', '.'])
        && name.len() <= 255
        && name
            .chars()
            .all(|character| !character.is_control() && !"<>:\"/\\|?*".contains(character))
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
            stream_id: StreamId::new(u16::from_str_radix(stream_raw, 16).ok()?)?,
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

impl LoadedLocal {
    /// Load pairing metadata from JSON sidecars within `maximum` bytes each.
    ///
    /// # Errors
    ///
    /// Returns directory traversal errors. Individual unreadable, oversized or
    /// malformed sidecars and invalid stream identifiers increment
    /// [`LoadedLocal::skipped`]. No sidecar body grows beyond `maximum` bytes.
    pub fn from_directory(date_dir: &Path, maximum: NonZeroUsize) -> std::io::Result<Self> {
        let mut out = Self::default();
        let entries = match std::fs::read_dir(date_dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(out),
            Err(error) => return Err(error),
        };
        for entry in entries {
            let path = entry?.path();
            if path.extension() != Some("json".as_ref()) {
                continue;
            }
            let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
                out.skipped += 1;
                continue;
            };
            let doc = read_sidecar(&path, maximum)
                .ok()
                .and_then(|bytes| serde_json::from_slice::<SidecarDoc>(&bytes).ok());
            let Some(doc) = doc else {
                out.skipped += 1;
                continue;
            };
            let Some(stream_id) = parse_stream_id(&doc.stream_id) else {
                out.skipped += 1;
                continue;
            };
            out.recordings.push(LocalRecording {
                stem: stem.to_string(),
                callsign: doc.header.map_or_else(
                    || "UNKNOWN".to_string(),
                    |h| crate::writer::sanitize_callsign(&h.my_callsign),
                ),
                stream_id,
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
}

fn parse_stream_id(text: &str) -> Option<StreamId> {
    if text.len() != 4 || !text.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    StreamId::new(u16::from_str_radix(text, 16).ok()?)
}

fn read_sidecar(path: &Path, maximum: NonZeroUsize) -> std::io::Result<Vec<u8>> {
    let mut file = std::fs::File::open(path)?;
    let mut body = Vec::new();
    let mut chunk = [0; 8192];
    loop {
        let count = file.read(&mut chunk)?;
        if count == 0 {
            return Ok(body);
        }
        if body
            .len()
            .checked_add(count)
            .is_none_or(|size| size > maximum.get())
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "sidecar exceeds selected byte limit",
            ));
        }
        body.try_reserve(count).map_err(std::io::Error::other)?;
        body.extend_from_slice(chunk.get(..count).unwrap_or_default());
    }
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
    module: Module,
    existing: &BTreeSet<String>,
) -> HarvestPlan {
    let mut result = HarvestPlan::default();

    let mut locals_by_key: BTreeMap<(&str, u16), Vec<usize>> = BTreeMap::new();
    for (i, rec) in local.iter().enumerate() {
        locals_by_key
            .entry((rec.callsign.as_str(), rec.stream_id.get()))
            .or_default()
            .push(i);
    }

    let mut claimed = vec![false; local.len()];
    let mut pairs: Vec<(&PublishedTx, Option<&LocalRecording>)> = Vec::new();
    for tx in published.iter().filter(|t| t.module == module.as_char()) {
        let key = (
            crate::writer::sanitize_callsign(&tx.mycall),
            tx.stream_id.get(),
        );
        let best = locals_by_key
            .get(&(key.0.as_str(), key.1))
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

/// Harvest failures.
#[derive(Debug, thiserror::Error)]
pub enum HarvestError {
    /// No usable dashboard base URL.
    #[error("no dashboard base URL for {0}; supply a base URL override")]
    BaseUrl(TargetKey),
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
    /// A future pacing deadline cannot be represented on this platform.
    #[error("download pacing deadline exceeds this platform's Instant range")]
    PacingDeadline,
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
    /// Wanted files excluded by a matching robots rule.
    #[serde(default)]
    pub blocked_by_robots: usize,
    /// True when `limit` cut the plan short.
    pub truncated: bool,
    /// True when this run only planned (nothing written).
    pub dry_run: bool,
    /// Why the run could not complete: a listing fetch/HTTP failure,
    /// a robots.txt disallow, or a consecutive-failure stop.
    error: Option<String>,
    #[serde(skip)]
    live_failure: Option<std::sync::Arc<HarvestFailure>>,
}

impl RunRecord {
    fn new(target: &TargetKey, date: NaiveDate, listing: &reqwest::Url, dry_run: bool) -> Self {
        Self {
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
            blocked_by_robots: 0,
            truncated: false,
            dry_run,
            error: None,
            live_failure: None,
        }
    }

    /// Recorded stopping reason, including historical provenance read from JSON.
    #[must_use]
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }
    /// Typed most recent failure from this live run, absent from deserialized provenance.
    #[must_use]
    pub fn failure(&self) -> Option<&HarvestFailure> {
        self.live_failure.as_deref()
    }
}

/// HTTP request phase retained by a live harvest failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FetchStage {
    /// Robots policy lookup before any listing or recording request.
    Robots,
    /// Daily listing request.
    Listing,
    /// One selected recording artifact.
    Download,
}

/// Typed stopping or most recent file failure from a live run.
#[derive(Debug, thiserror::Error)]
pub enum HarvestFailure {
    /// HTTP, redirect, deadline, response-size or allocation failure.
    #[error("{stage:?}: {source}")]
    Fetch {
        /// Request phase that failed.
        stage: FetchStage,
        /// Original HTTP/body error.
        #[source]
        source: crate::FetchError,
    },
    /// A response status prevents proceeding in this phase.
    #[error("{stage:?}: HTTP {status}")]
    HttpStatus {
        /// Request phase that failed.
        stage: FetchStage,
        /// Received status.
        status: reqwest::StatusCode,
    },
    /// The applicable robots rules exclude the daily listing.
    #[error("robots.txt disallows the streams listing")]
    RobotsDisallowed,
    /// The run stopped after three consecutive download failures.
    #[error("stopped after three consecutive download failures: {0}")]
    DownloadBreaker(#[source] std::sync::Arc<Self>),
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

/// An HTTP(S) dashboard base without credentials, query or fragment.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct DashboardUrl(reqwest::Url);

/// Invalid dashboard URL syntax or unsupported URL components.
#[derive(Debug, thiserror::Error)]
pub enum DashboardUrlError {
    /// URI parsing failed.
    #[error("dashboard URL: {0}")]
    Parse(#[source] <reqwest::Url as FromStr>::Err),
    /// The scheme is neither HTTP nor HTTPS, or the authority is absent.
    #[error("dashboard URL requires an HTTP(S) authority")]
    Authority,
    /// User information, query or fragment is present.
    #[error("dashboard base cannot include credentials, a query or a fragment")]
    Components,
}
impl DashboardUrl {
    /// Derive this application's default `REF`-plus-digits dashboard address.
    ///
    /// Returns `None` for other naming schemes. A derived address is a routing
    /// convention; availability and published content require an HTTP response.
    #[must_use]
    pub fn for_reflector(reflector: &ReflectorName) -> Option<Self> {
        let system = reflector.as_str();
        let digits = system.strip_prefix("REF")?;
        if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
            return None;
        }
        let url = reqwest::Url::parse(&format!(
            "https://{}.dstargateway.org:8443/",
            system.to_ascii_lowercase()
        ))
        .ok()?;
        Self::new(url).ok()
    }

    /// Check an owned parsed URL without copying its storage.
    ///
    /// # Errors
    ///
    /// Returns [`DashboardUrlError`] for unsupported schemes or base components.
    pub fn new(value: reqwest::Url) -> Result<Self, DashboardUrlError> {
        if !matches!(value.scheme(), "http" | "https")
            || !value.has_host()
            || value.cannot_be_a_base()
        {
            return Err(DashboardUrlError::Authority);
        }
        if !value.username().is_empty()
            || value.password().is_some()
            || value.query().is_some()
            || value.fragment().is_some()
        {
            return Err(DashboardUrlError::Components);
        }
        Ok(Self(value))
    }
    /// Borrow the checked base URL.
    #[must_use]
    pub const fn as_url(&self) -> &reqwest::Url {
        &self.0
    }
    /// Take the parsed base URL without copying.
    #[must_use]
    pub fn into_url(self) -> reqwest::Url {
        self.0
    }

    /// Construct the date-partitioned listing URL below this checked base.
    #[must_use]
    pub fn listing_url(&self, date: NaiveDate) -> reqwest::Url {
        self.streams_url(date, "")
    }

    /// Construct the URL for one parsed and planned published artifact.
    #[must_use]
    pub fn file_url(&self, date: NaiveDate, item: &FetchItem) -> reqwest::Url {
        self.streams_url(date, &item.file_name)
    }

    fn streams_url(&self, date: NaiveDate, last: &str) -> reqwest::Url {
        let mut url = self.0.clone();
        match url.path_segments_mut() {
            Ok(mut segments) => {
                let _segments = segments
                    .pop_if_empty()
                    .push("streams")
                    .push(&date.format("%Y").to_string())
                    .push(&date.format("%m").to_string())
                    .push(&date.format("%d").to_string())
                    .push(last);
            }
            Err(()) => {
                unreachable!("DashboardUrl construction requires a hierarchical HTTP(S) URL")
            }
        }
        url
    }
}
impl FromStr for DashboardUrl {
    type Err = DashboardUrlError;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::new(value.parse().map_err(DashboardUrlError::Parse)?)
    }
}
impl TryFrom<reqwest::Url> for DashboardUrl {
    type Error = DashboardUrlError;
    fn try_from(value: reqwest::Url) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}
impl From<DashboardUrl> for reqwest::Url {
    fn from(value: DashboardUrl) -> Self {
        value.into_url()
    }
}
impl AsRef<reqwest::Url> for DashboardUrl {
    fn as_ref(&self) -> &reqwest::Url {
        self.as_url()
    }
}
impl std::fmt::Display for DashboardUrl {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&self.0, formatter)
    }
}

/// At least two seconds between successive completed downloads and requests.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FetchGap(Duration);
/// Invalid or unrepresentable download pacing interval.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum FetchGapError {
    /// The requested interval is below the mandatory two-second policy.
    #[error("download gap {0:?} is below two seconds")]
    TooShort(Duration),
    /// Adding the interval to the current monotonic clock overflows.
    #[error("download gap exceeds the platform's Instant range")]
    TooLarge,
}
impl FetchGap {
    /// Enforce the application's two-second floor and monotonic representation.
    ///
    /// # Errors
    ///
    /// Returns [`FetchGapError`] before a harvester is constructed.
    pub fn new(value: Duration) -> Result<Self, FetchGapError> {
        if value < POLITE_GAP {
            return Err(FetchGapError::TooShort(value));
        }
        if std::time::Instant::now().checked_add(value).is_none() {
            return Err(FetchGapError::TooLarge);
        }
        Ok(Self(value))
    }
    /// Minimum interval between downloads.
    #[must_use]
    pub const fn duration(self) -> Duration {
        self.0
    }
}
impl Default for FetchGap {
    fn default() -> Self {
        Self(POLITE_GAP)
    }
}
impl TryFrom<Duration> for FetchGap {
    type Error = FetchGapError;
    fn try_from(value: Duration) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}
impl From<FetchGap> for Duration {
    fn from(value: FetchGap) -> Self {
        value.duration()
    }
}

/// Validated options for one sequential [`Harvester`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HarvestOptions {
    /// Dashboard base URL override; default derives one per system
    /// via [`DashboardUrl::for_reflector`].
    base_url: Option<DashboardUrl>,
    /// Cap on downloads per run.
    limit: Option<NonZeroUsize>,
    /// Plan and report only: write nothing, download nothing.
    dry_run: bool,
    /// Pause between downloads (default [`POLITE_GAP`]).
    fetch_gap: FetchGap,
    /// Operator callsign advertised in the User-Agent so the server
    /// owner can identify and contact who is fetching.
    operator: RoutingCallsign,
    response_limit: NonZeroUsize,
    sidecar_limit: NonZeroUsize,
}

impl HarvestOptions {
    /// Select the operator identity; defaults are a two-second gap and 64 MiB per response.
    #[must_use]
    pub fn new(operator: RoutingCallsign) -> Self {
        Self {
            base_url: None,
            limit: None,
            dry_run: false,
            fetch_gap: FetchGap::default(),
            operator,
            response_limit: const { NonZeroUsize::new(67_108_864).unwrap() },
            sidecar_limit: const { NonZeroUsize::new(1_048_576).unwrap() },
        }
    }
    /// Override the derived dashboard address.
    #[must_use]
    pub fn with_base_url(mut self, value: DashboardUrl) -> Self {
        self.base_url = Some(value);
        self
    }
    /// Limit successful and failed download attempts per run.
    #[must_use]
    pub const fn with_limit(mut self, value: NonZeroUsize) -> Self {
        self.limit = Some(value);
        self
    }
    /// Plan without writing or downloading recording files.
    #[must_use]
    pub const fn with_dry_run(mut self, value: bool) -> Self {
        self.dry_run = value;
        self
    }
    /// Use a checked interval at least as long as the mandatory floor.
    #[must_use]
    pub const fn with_fetch_gap(mut self, value: FetchGap) -> Self {
        self.fetch_gap = value;
        self
    }
    /// Bound bytes retained for each listing or recording response.
    #[must_use]
    pub const fn with_response_limit(mut self, value: NonZeroUsize) -> Self {
        self.response_limit = value;
        self
    }
    /// Bound each local JSON sidecar before parsing; the default is 1 MiB.
    #[must_use]
    pub const fn with_sidecar_limit(mut self, value: NonZeroUsize) -> Self {
        self.sidecar_limit = value;
        self
    }
    /// Selected minimum delay between downloads.
    #[must_use]
    pub const fn fetch_gap(&self) -> FetchGap {
        self.fetch_gap
    }
    /// Borrow an explicit dashboard base, if selected.
    #[must_use]
    pub const fn base_url(&self) -> Option<&DashboardUrl> {
        self.base_url.as_ref()
    }
}

/// Downloads published recordings into `published/` subdirectories,
/// sequentially and politely, with per-file and per-run provenance.
#[derive(Debug)]
pub struct Harvester {
    client: reqwest::Client,
    base_url: Option<DashboardUrl>,
    limit: Option<usize>,
    dry_run: bool,
    fetch_gap: FetchGap,
    response_limit: NonZeroUsize,
    sidecar_limit: NonZeroUsize,
    next_download: Option<tokio::time::Instant>,
    pacing_overflow: bool,
}

struct DownloadPacing<'a> {
    next: &'a mut Option<tokio::time::Instant>,
    overflow: &'a mut bool,
    gap: Duration,
}
impl Drop for DownloadPacing<'_> {
    fn drop(&mut self) {
        *self.next = tokio::time::Instant::now().checked_add(self.gap);
        *self.overflow = self.next.is_none();
    }
}

impl Harvester {
    /// Build a harvester with a 30-second total deadline per HTTP request.
    ///
    /// # Errors
    ///
    /// Returns [`HarvestError::Client`] if the bounded HTTP client cannot be built.
    pub fn new(options: HarvestOptions) -> Result<Self, HarvestError> {
        let HarvestOptions {
            base_url,
            limit,
            dry_run,
            fetch_gap,
            operator,
            response_limit,
            sidecar_limit,
        } = options;
        let client = crate::http::client(&build_user_agent(operator), Duration::from_secs(30))
            .map_err(HarvestError::Client)?;
        Ok(Self {
            client,
            base_url,
            limit: limit.map(NonZeroUsize::get),
            dry_run,
            fetch_gap,
            response_limit,
            sidecar_limit,
            next_download: None,
            pacing_overflow: false,
        })
    }

    /// Harvest one `(target directory, date)`: fetch the listing,
    /// match against local recordings, download wanted files into
    /// `<date dir>/published/`, and append provenance. Listing and
    /// per-file fetch failures are recorded in the returned record,
    /// not raised; local I/O failures are fatal.
    ///
    /// Cancellation preserves download pacing and already-published files;
    /// completion counts and manifests can describe a partial run. Repeated runs
    /// share the checked minimum interval. Local filesystem calls are synchronous.
    ///
    /// # Errors
    ///
    /// [`HarvestError`] on invalid target names, missing base URL,
    /// or local filesystem failures.
    pub async fn harvest_dir(
        &mut self,
        recordings: &Path,
        target: &TargetKey,
        date: NaiveDate,
    ) -> Result<RunRecord, HarvestError> {
        if self.pacing_overflow {
            return Err(HarvestError::PacingDeadline);
        }
        let system = target.reflector().as_str();
        let module = target.module();
        let base = match &self.base_url {
            Some(url) => url.clone(),
            None => DashboardUrl::for_reflector(target.reflector())
                .ok_or_else(|| HarvestError::BaseUrl(target.clone()))?,
        };
        let date_dir = recordings
            .join(target.to_string())
            .join(date.format("%Y-%m-%d").to_string());
        let published_dir = date_dir.join("published");
        let listing = base.listing_url(date);

        let mut record = RunRecord::new(target, date, &listing, self.dry_run);

        let local = LoadedLocal::from_directory(&date_dir, self.sidecar_limit)?;
        record.local_recordings = local.recordings.len();
        record.local_skipped = local.skipped;

        let robots = match self.robots_rules(&base).await {
            Ok(rules) => rules,
            Err(error) => {
                record.error = Some(format!("robots.txt unreachable: {error}; nothing fetched"));
                record.live_failure = Some(std::sync::Arc::new(error));
                return Ok(record);
            }
        };
        if robots.disallows(listing.path()) {
            record.error =
                Some("robots.txt disallows the streams tree for us; nothing fetched".to_string());
            record.live_failure = Some(std::sync::Arc::new(HarvestFailure::RobotsDisallowed));
            return Ok(record);
        }

        match self.fetch(listing).await {
            Err(error) => {
                record.error = Some(error.to_string());
                record.live_failure = Some(std::sync::Arc::new(HarvestFailure::Fetch {
                    stage: FetchStage::Listing,
                    source: error,
                }));
            }
            Ok(HttpResponse { status, body }) => {
                record.http_status = Some(status.as_u16());
                match status {
                    // Nothing published for this date, which is normal.
                    reqwest::StatusCode::NOT_FOUND => {}
                    reqwest::StatusCode::OK => {
                        let mut published = parse_listing(&String::from_utf8_lossy(&body));
                        published.retain(|transmission| {
                            transmission.system.eq_ignore_ascii_case(system)
                        });
                        let existing = existing_files(&published_dir)?;
                        let mut plan = plan(&published, &local.recordings, module, &existing);
                        record.published_tx = plan.matched + plan.published_only;
                        record.matched = plan.matched;
                        record.published_only = plan.published_only;
                        record.local_only = plan.local_only;
                        record.skipped_existing = plan.skipped_existing;
                        let wanted = plan.items.len();
                        plan.items
                            .retain(|item| !robots.disallows(base.file_url(date, item).path()));
                        record.blocked_by_robots = wanted - plan.items.len();
                        record.planned = plan.items.len();
                        if !self.dry_run {
                            self.execute(&mut record, &plan, &base, date, &published_dir)
                                .await?;
                        }
                    }
                    other => {
                        record.error = Some(format!("http status {other}"));
                        record.live_failure =
                            Some(std::sync::Arc::new(HarvestFailure::HttpStatus {
                                stage: FetchStage::Listing,
                                status: other,
                            }));
                    }
                }
            }
        }

        if !self.dry_run
            && (record.planned > 0 || record.skipped_existing > 0 || published_dir.exists())
        {
            append_manifest(&published_dir, &ManifestRecord::Run(record.clone()))?;
        }
        Ok(record)
    }

    async fn robots_rules(&self, base: &DashboardUrl) -> Result<RobotsRules, HarvestFailure> {
        let mut robots_url = base.as_url().clone();
        robots_url.set_path("/robots.txt");
        match crate::http::fetch(
            &self.client,
            robots_url,
            const { NonZeroUsize::new(512_000).unwrap() },
        )
        .await
        {
            Ok(response) if response.status.is_success() => {
                Ok(RobotsRules::new(&String::from_utf8_lossy(&response.body)))
            }
            Ok(response) if response.status.is_client_error() => Ok(RobotsRules::default()),
            Ok(response) => Err(HarvestFailure::HttpStatus {
                stage: FetchStage::Robots,
                status: response.status,
            }),
            Err(error) => Err(HarvestFailure::Fetch {
                stage: FetchStage::Robots,
                source: error,
            }),
        }
    }

    /// Download the planned files sequentially with the polite gap,
    /// appending a manifest record per attempt. Fetch/HTTP failures
    /// are recorded and counted; local I/O failures are fatal.
    async fn execute(
        &mut self,
        record: &mut RunRecord,
        plan: &HarvestPlan,
        base: &DashboardUrl,
        date: NaiveDate,
        published_dir: &Path,
    ) -> Result<(), HarvestError> {
        let take = self.limit.unwrap_or(usize::MAX);
        record.truncated = plan.items.len() > take;
        let mut consecutive_failures = 0usize;
        for item in plan.items.iter().take(take) {
            let earliest = self.next_download.unwrap_or_else(tokio::time::Instant::now);
            tokio::time::sleep_until(earliest).await;
            let url = base.file_url(date, item);
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
            let pacing = DownloadPacing {
                next: &mut self.next_download,
                overflow: &mut self.pacing_overflow,
                gap: self.fetch_gap.duration(),
            };
            let fetched = crate::http::fetch(&self.client, url, self.response_limit).await;
            drop(pacing);
            match fetched {
                Err(error) => {
                    file_record.error = Some(error.to_string());
                    record.live_failure = Some(std::sync::Arc::new(HarvestFailure::Fetch {
                        stage: FetchStage::Download,
                        source: error,
                    }));
                    record.failed += 1;
                    consecutive_failures += 1;
                }
                Ok(HttpResponse { status, body }) => {
                    file_record.http_status = Some(status.as_u16());
                    if status == reqwest::StatusCode::OK {
                        // Listing names are single path components by
                        // construction (parse_listing drops any href
                        // containing '/').
                        std::fs::create_dir_all(published_dir)?;
                        crate::writer::publish_bytes(
                            &published_dir.join(&item.file_name),
                            &body,
                            &mut |_, _| Ok(()),
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
                        record.live_failure =
                            Some(std::sync::Arc::new(HarvestFailure::HttpStatus {
                                stage: FetchStage::Download,
                                status,
                            }));
                        record.failed += 1;
                        consecutive_failures += 1;
                    }
                }
            }
            if self.pacing_overflow {
                return Err(HarvestError::PacingDeadline);
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
                if let Some(failure) = record.live_failure.take() {
                    record.live_failure = Some(std::sync::Arc::new(
                        HarvestFailure::DownloadBreaker(failure),
                    ));
                }
                break;
            }
        }
        Ok(())
    }

    async fn fetch(&self, url: reqwest::Url) -> Result<HttpResponse, crate::FetchError> {
        crate::http::fetch(&self.client, url, self.response_limit).await
    }
}

/// Names of nonzero-size files already in the destination directory.
fn existing_files(dir: &Path) -> std::io::Result<BTreeSet<String>> {
    let mut out = BTreeSet::new();
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(out),
        Err(error) => return Err(error),
    };
    for entry in entries {
        let entry = entry?;
        let meta = entry.metadata()?;
        if meta.is_file()
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

    /// Real rows sampled from the live REF030 listing:
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
            stream_id: parse_stream_id(stream_id).ok_or("stream identifier")?,
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
            stream_id: parse_stream_id(stream_id).ok_or("stream identifier")?,
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
            .find(|t| t.stream_id.get() == 0xe658)
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
            .find(|t| t.stream_id.get() == 0xe11d)
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
            .find(|t| t.stream_id.get() == 0x96da)
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
        assert!(is_safe_basename(
            "REF030-C--20260711-175407.594--É--e658.mp3"
        ));
        assert!(!is_safe_basename("name\n.mp3"));
    }

    #[test]
    fn existing_empty_artifacts_are_preserved_and_directory_errors_propagate() -> TestResult {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("empty.txt");
        std::fs::write(&path, [])?;
        assert!(existing_files(directory.path())?.contains("empty.txt"));
        assert!(existing_files(&path).is_err());
        assert!(
            LoadedLocal::from_directory(&path, const { NonZeroUsize::new(1024).unwrap() }).is_err()
        );
        Ok(())
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
        let plan = plan(&p, &l, Module::C, &BTreeSet::new());
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
        let plan = plan(&p, &l, Module::C, &BTreeSet::new());
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
        let plan = plan(&p, &[], Module::C, &BTreeSet::new());
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
        let plan = plan(&p, &l, Module::C, &BTreeSet::new());
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
        let plan = plan(&p, &[], Module::C, &BTreeSet::new());
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
        let plan = plan(&[], &l, Module::C, &BTreeSet::new());
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
        let plan = plan(&p, &l, Module::C, &BTreeSet::new());
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
        let plan = plan(&p, &l, Module::C, &BTreeSet::new());
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
        let plan = plan(&p, &l, Module::C, &existing);
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

        let loaded =
            LoadedLocal::from_directory(dir.path(), const { NonZeroUsize::new(4096).unwrap() })?;
        assert_eq!(loaded.skipped, 1, "garbage.json");
        assert_eq!(loaded.recordings.len(), 2);
        let rec = loaded
            .recordings
            .iter()
            .find(|r| r.stream_id.get() == 0xe658)
            .ok_or("E658")?;
        assert_eq!(rec.stem, "20260711T175407Z_MM3TWA_E658");
        assert_eq!(rec.callsign, "MM3TWA");
        assert_eq!(rec.gaps, 2);
        assert_eq!(rec.started_at.to_rfc3339(), "2026-07-11T17:54:07.608+00:00");
        let unknown = loaded
            .recordings
            .iter()
            .find(|r| r.stream_id.get() == 0x1234)
            .ok_or("1234")?;
        assert_eq!(unknown.callsign, "UNKNOWN", "headerless recording");
        Ok(())
    }

    #[test]
    fn pairing_metadata_rejects_invalid_stream_ids_and_bounds_sidecar_bytes() -> TestResult {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("capture.json");
        for stream in ["0000", "10000", "xyz!"] {
            let bytes = serde_json::to_vec(
                &serde_json::json!({ "stream_id": stream, "started_at": "2026-07-11T18:00:00Z" }),
            )?;
            std::fs::write(&path, bytes)?;
            let loaded = LoadedLocal::from_directory(
                directory.path(),
                const { NonZeroUsize::new(4096).unwrap() },
            )?;
            assert_eq!(loaded.skipped, 1, "{stream}");
            assert!(loaded.recordings.is_empty());
        }
        std::fs::write(&path, [b'x'; 33])?;
        let maximum = NonZeroUsize::new(32).ok_or("limit")?;
        assert!(read_sidecar(&path, maximum).is_err());
        let loaded = LoadedLocal::from_directory(directory.path(), maximum)?;
        assert_eq!(loaded.skipped, 1);
        assert!(loaded.recordings.is_empty());
        assert!(parse_published_name("REF030-C--20260711-180000.000--W1AW--0000.mp3").is_none());
        Ok(())
    }

    #[test]
    fn urls_derive_encode_and_partition_by_date() -> TestResult {
        let base = DashboardUrl::for_reflector(&ReflectorName::new("REF030")?)
            .ok_or("REF systems derive")?;
        assert_eq!(
            base.as_url().as_str(),
            "https://ref030.dstargateway.org:8443/",
            "lowercased shared-hosting scheme"
        );
        assert!(
            DashboardUrl::for_reflector(&ReflectorName::new("XRF757")?).is_none(),
            "non-REF needs --base-url"
        );
        assert!(
            DashboardUrl::for_reflector(&ReflectorName::new("REF999X")?).is_none(),
            "not a REFnnn name"
        );

        let date = NaiveDate::from_ymd_opt(2026, 7, 11).ok_or("date")?;
        assert_eq!(
            base.listing_url(date).as_str(),
            "https://ref030.dstargateway.org:8443/streams/2026/07/11/"
        );
        assert_eq!(
            base.file_url(
                date,
                &FetchItem {
                    file_name: "REF030-C--20260711-130000.250--KJ5CJS B--96da.mp3".to_owned(),
                    reason: FetchReason::Salvage,
                    local_stem: None
                }
            )
            .as_str(),
            "https://ref030.dstargateway.org:8443/streams/2026/07/11/REF030-C--20260711-130000.250--KJ5CJS%20B--96da.mp3",
            "space re-encoded on the way out"
        );
        Ok(())
    }

    #[test]
    fn default_pacing_and_direct_construction_enforce_the_floor() -> TestResult {
        assert_eq!(
            HarvestOptions::new(RoutingCallsign::new("W1AW")?)
                .fetch_gap()
                .duration(),
            Duration::from_secs(2),
            "the minimum interval is application policy"
        );
        assert!(matches!(
            FetchGap::new(Duration::ZERO),
            Err(FetchGapError::TooShort(_))
        ));
        assert!(FetchGap::new(Duration::from_millis(1999)).is_err());
        assert!(FetchGap::new(Duration::MAX).is_err());
        assert_eq!(
            FetchGap::new(Duration::from_secs(2))?.duration(),
            POLITE_GAP
        );
        for invalid in [
            "file:///tmp/recordings",
            "mailto:operator@example.org",
            "https://user@example.org/",
            "https://example.org/?query",
            "https://example.org/#fragment",
        ] {
            assert!(invalid.parse::<DashboardUrl>().is_err(), "{invalid}");
        }
        Ok(())
    }

    #[test]
    fn user_agent_identifies_tool_and_operator() -> TestResult {
        let signed = build_user_agent(RoutingCallsign::new("KQ4NIT")?);
        assert!(signed.starts_with("stargazer-harvest/"), "{signed}");
        assert!(signed.contains("amateur radio research"), "{signed}");
        assert!(signed.contains("operator KQ4NIT"), "{signed}");
        Ok(())
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

    #[test]
    fn robots_longest_allow_wildcard_anchor_encoding_and_agent_groups() {
        let rules = "User-agent: *\nDisallow: /streams/\nAllow: /streams/public/\nDisallow: /*.mp3$\nAllow: /streams/public/open.mp3$\n";
        assert!(robots_disallows(rules, "/streams/private/a.txt"));
        assert!(!robots_disallows(rules, "/streams/public/a.txt"));
        assert!(robots_disallows(rules, "/other/a.mp3"));
        assert!(!robots_disallows(rules, "/other/a.mp3.txt"));
        assert!(!robots_disallows(rules, "/streams/public/open.mp3"));
        assert!(!robots_disallows(
            "User-agent: *\nDisallow: /\nUser-agent: STARGAZER-HARVEST\nAllow: /",
            "/streams/"
        ));
        assert!(robots_disallows(
            "User-agent: stargazer-harvest\nDisallow: /streams/\nSitemap: /map\nAllow: /else/\nUser-agent: stargazer-harvest\nDisallow: /other/",
            "/other/"
        ));
        assert!(!robots_disallows(
            "User-agent: *\nDisallow: /x\nAllow: /x",
            "/x"
        ));
        assert!(robots_disallows(
            "User-agent: *\nDisallow: /caf%C3%A9/",
            "/café/a"
        ));
        assert!(robots_disallows(
            "User-agent: *\nDisallow: /foo/%62az",
            "/foo/baz"
        ));
        assert!(!robots_disallows(
            "User-agent: *\nDisallow: /foo%2Fbar",
            "/foo/bar"
        ));
        assert!(robots_disallows(
            "User-agent: *\nDisallow: /literal%2A$",
            "/literal*"
        ));
    }
}
