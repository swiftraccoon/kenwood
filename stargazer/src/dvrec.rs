// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later

//! Import reflector-published `dvrec` packet logs as recordings.
//!
//! Recognized log lines retain nine AMBE bytes and three slow-data bytes
//! per received frame. Import produces the same versioned archive layout
//! as live capture, with absent live endpoint/peer metadata.
//!
//! Line shape (fields colon-separated after the timestamp):
//!
//! ```text
//! #DVREC 20260711-175407.594 dplus
//! <ts>>c2g:20:0002:01:<sid-le>:<seq>:hdr:<flags>:<RPT2>:<RPT1>:<UR>:<MY>/<SUFX>:<pfcs>
//! <ts>>c2g:20:0002:01:<sid-le>:<seq>:vd :<18 hex ambe>:<6 hex slow>#<descrambled> |ascii|
//! ```
//!
//! The parser swaps the two per-line stream-id bytes before filename matching.
//! A recognized `vde` line marks end-of-stream; its payload is not a voice frame.

use std::path::Path;

use chrono::{DateTime, Utc};
use dstar_gateway_core::{Callsign, DstarHeader, HeaderFlags, StreamId, Suffix};

use crate::capture::{
    CaptureDurationLimit, CompletedRecording, EndReason, FrameRecord, StreamOrigin,
};
use crate::config::TargetKey;
use crate::writer::Writer;

/// Failures parsing a dvrec file.
#[derive(Debug, thiserror::Error)]
pub enum DvrecError {
    /// No header line, so the D-STAR routing fields are unknown.
    #[error("no hdr line in dvrec")]
    MissingHeader,
    /// No voice frames.
    #[error("no voice frames in dvrec")]
    NoFrames,
    /// The stream id was zero or unparseable.
    #[error("bad stream id")]
    BadStreamId,
    /// A single log contains more than one nonzero stream identity.
    #[error("dvrec stream id changed from {expected} to {actual}")]
    MixedStream {
        /// Identity from the first recognized line.
        expected: StreamId,
        /// Conflicting later identity.
        actual: StreamId,
    },
    /// A routing field exceeds its fixed wire width.
    #[error("dvrec {field:?} has {actual} bytes; width is {maximum}")]
    FieldWidth {
        /// Header field that cannot be represented losslessly.
        field: HeaderField,
        /// Supplied byte count.
        actual: usize,
        /// Fixed wire width.
        maximum: usize,
    },
    /// A recognized header has malformed hex or missing fields.
    #[error("malformed dvrec routing header")]
    Header,
    /// Voice frames exceed the caller's retained-prefix policy.
    #[error("dvrec exceeds the configured capture frame limit")]
    FrameLimit,
    /// Expected codec frames cannot be represented.
    #[error("dvrec codec frame count overflows")]
    FrameCount,
}

/// Fixed-width routing field in a dvrec header.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HeaderField {
    /// Repeater 2, eight bytes.
    Rpt2,
    /// Repeater 1, eight bytes.
    Rpt1,
    /// Destination, eight bytes.
    Ur,
    /// Source, eight bytes.
    My,
    /// Source suffix, four bytes.
    Suffix,
}

/// A parsed dvrec transmission.
#[derive(Debug)]
pub struct ParsedDvrec {
    /// Routing header from the `hdr` line.
    header: DstarHeader,
    /// Stream id (filename byte order).
    stream_id: StreamId,
    /// Voice frames in log order.
    frames: Vec<FrameRecord>,
    /// Missing frames implied by seq discontinuities (mod 21).
    gaps: u64,
    /// First line's timestamp.
    started_at: DateTime<Utc>,
    /// Last line's timestamp (a `vde` line when present).
    ended_at: DateTime<Utc>,
    /// True when a `vde` end-of-stream line was seen.
    saw_eot: bool,
    /// Lines that did not parse as any known record.
    skipped_lines: usize,
}

impl ParsedDvrec {
    /// Received routing header.
    #[must_use]
    pub const fn header(&self) -> &DstarHeader {
        &self.header
    }
    /// Nonzero transmission identity in filename byte order.
    #[must_use]
    pub const fn stream_id(&self) -> StreamId {
        self.stream_id
    }
    /// Retained raw frames in log order.
    #[must_use]
    pub fn frames(&self) -> &[FrameRecord] {
        &self.frames
    }
    /// Inferred missing codec frames.
    #[must_use]
    pub const fn gaps(&self) -> u64 {
        self.gaps
    }
    /// First recognized line time.
    #[must_use]
    pub const fn started_at(&self) -> DateTime<Utc> {
        self.started_at
    }
    /// Last recognized line time, including an end marker.
    #[must_use]
    pub const fn ended_at(&self) -> DateTime<Utc> {
        self.ended_at
    }
    /// Whether a `vde` line was present.
    #[must_use]
    pub const fn saw_eot(&self) -> bool {
        self.saw_eot
    }
    /// Lines skipped because their record shape was unrecognized.
    #[must_use]
    pub const fn skipped_lines(&self) -> usize {
        self.skipped_lines
    }
}

fn parse_ts(s: &str) -> Option<DateTime<Utc>> {
    chrono::NaiveDateTime::parse_from_str(s, "%Y%m%d-%H%M%S%.3f")
        .ok()
        .map(|n| n.and_utc())
}

fn hex_bytes<const N: usize>(s: &str) -> Option<[u8; N]> {
    if s.len() != N * 2 || !s.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let mut out = [0u8; N];
    for (i, slot) in out.iter_mut().enumerate() {
        *slot = u8::from_str_radix(s.get(i * 2..i * 2 + 2)?, 16).ok()?;
    }
    Some(out)
}

/// Fixed-width wire field from a text field (space-padded to `N`).
fn wire_field<const N: usize>(s: &str, field: HeaderField) -> Result<[u8; N], DvrecError> {
    if s.len() > N {
        return Err(DvrecError::FieldWidth {
            field,
            actual: s.len(),
            maximum: N,
        });
    }
    let mut out = [b' '; N];
    for (slot, byte) in out.iter_mut().zip(s.bytes()) {
        *slot = byte;
    }
    Ok(out)
}

/// One tokenized log line.
struct LogLine<'a> {
    ts: DateTime<Utc>,
    sid_filename_order: u16,
    seq: u8,
    record: &'a str,
    payload: &'a str,
}

fn tokenize(line: &str) -> Option<LogLine<'_>> {
    let (ts_str, rest) = line.split_once('>')?;
    let ts = parse_ts(ts_str)?;
    let mut parts = rest.splitn(8, ':');
    let fields: [&str; 8] = std::array::from_fn(|_| parts.next().unwrap_or_default());
    // [c2g, len, .., .., sid, seq, record, payload...]
    let sid = u16::from_str_radix(fields.get(4)?, 16).ok()?;
    let seq = u8::from_str_radix(fields.get(5)?, 16).ok()?;
    let record = fields.get(6)?.trim();
    Some(LogLine {
        ts,
        sid_filename_order: sid.swap_bytes(),
        seq,
        record,
        payload: fields.get(7)?,
    })
}

fn header_from_payload(payload: &str) -> Result<DstarHeader, DvrecError> {
    let mut parts = payload.splitn(6, ':');
    let payload: [&str; 6] = std::array::from_fn(|_| parts.next().unwrap_or_default());
    let flags =
        hex_bytes::<3>(payload.first().ok_or(DvrecError::Header)?).ok_or(DvrecError::Header)?;
    let (my, suffix) = payload
        .get(4)
        .ok_or(DvrecError::Header)?
        .split_once('/')
        .ok_or(DvrecError::Header)?;
    Ok(DstarHeader {
        flags: HeaderFlags::from_bytes(flags),
        rpt2: Callsign::from_wire_bytes(wire_field::<8>(
            payload.get(1).ok_or(DvrecError::Header)?,
            HeaderField::Rpt2,
        )?),
        rpt1: Callsign::from_wire_bytes(wire_field::<8>(
            payload.get(2).ok_or(DvrecError::Header)?,
            HeaderField::Rpt1,
        )?),
        ur_call: Callsign::from_wire_bytes(wire_field::<8>(
            payload.get(3).ok_or(DvrecError::Header)?,
            HeaderField::Ur,
        )?),
        my_call: Callsign::from_wire_bytes(wire_field::<8>(my, HeaderField::My)?),
        my_suffix: Suffix::from_wire_bytes(wire_field::<4>(suffix, HeaderField::Suffix)?),
    })
}

fn frame_from_payload(payload: &str, seq: u8) -> Option<FrameRecord> {
    let (ambe, slow) = payload.split_once(':')?;
    let ambe = hex_bytes::<9>(ambe)?;
    let slow_hex = slow.split('#').next()?;
    let slow_data = hex_bytes::<3>(slow_hex)?;
    Some(FrameRecord {
        seq,
        ambe,
        slow_data,
    })
}

impl ParsedDvrec {
    /// Parse one transmission within the caller's retained-frame bound.
    ///
    /// Malformed unknown lines increment [`Self::skipped_lines`]. Recognized
    /// routing fields and transmission identities must remain lossless and consistent.
    ///
    /// # Errors
    ///
    /// Returns [`DvrecError`] for a missing header/voice payload, invalid or mixed
    /// stream identities, oversized header fields or an exceeded frame bound.
    pub fn from_text(text: &str, limit: CaptureDurationLimit) -> Result<Self, DvrecError> {
        let mut header = None;
        let mut stream_id = None;
        let mut frames: Vec<FrameRecord> = Vec::new();
        let mut gaps = 0u64;
        let mut prev_seq: Option<u8> = None;
        let mut started_at = None;
        let mut ended_at = None;
        let mut saw_eot = false;
        let mut skipped_lines = 0usize;

        for raw in text.lines() {
            let line = raw.trim_end();
            if line.is_empty() || line.starts_with("#DVREC") {
                continue;
            }
            let Some(log) = tokenize(line) else {
                skipped_lines += 1;
                continue;
            };
            if started_at.is_none() {
                started_at = Some(log.ts);
            }
            ended_at = Some(log.ts);
            let actual = StreamId::new(log.sid_filename_order).ok_or(DvrecError::BadStreamId)?;
            if let Some(expected) = stream_id {
                if expected != actual {
                    return Err(DvrecError::MixedStream { expected, actual });
                }
            } else {
                stream_id = Some(actual);
            }
            match log.record {
                "hdr" => {
                    let parsed = header_from_payload(log.payload)?;
                    if header.is_none() {
                        header = Some(parsed);
                    }
                }
                "vd" => match frame_from_payload(log.payload, log.seq) {
                    None => skipped_lines += 1,
                    Some(frame) => {
                        if frames.len() >= limit.as_frames() {
                            return Err(DvrecError::FrameLimit);
                        }
                        if let Some(prev) = prev_seq {
                            gaps = gaps
                                .checked_add(crate::capture::seq_gap(prev, log.seq))
                                .ok_or(DvrecError::FrameCount)?;
                        }
                        prev_seq = Some(log.seq);
                        frames.push(frame);
                    }
                },
                "vde" => saw_eot = true,
                _ => skipped_lines += 1,
            }
        }

        let header = header.ok_or(DvrecError::MissingHeader)?;
        if frames.is_empty() {
            return Err(DvrecError::NoFrames);
        }
        let stream_id = stream_id.ok_or(DvrecError::BadStreamId)?;
        let started_at = started_at.ok_or(DvrecError::NoFrames)?;
        let ended_at = ended_at.ok_or(DvrecError::NoFrames)?;
        Ok(Self {
            header,
            stream_id,
            frames,
            gaps,
            started_at,
            ended_at,
            saw_eot,
            skipped_lines,
        })
    }
}

/// Import outcome for one tree walk.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ImportSummary {
    /// Recordings written.
    pub imported: usize,
    /// Transmissions that already had a local recording.
    pub skipped_existing: usize,
    /// Voiceless logs (kerchunks: header + end marker, zero voice
    /// frames): a normal on-air shape, nothing to reconstruct.
    pub skipped_voiceless: usize,
    /// Dvrec files that could not be parsed or written.
    pub failed: usize,
}

/// Publish-vs-capture start-time skew treated as the same
/// transmission when a stream id repeats within a day.
const MATCH_TOLERANCE_SECS: i64 = 120;

/// Explicit memory and retained-codec limits for each imported packet log.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ImportLimits {
    maximum_bytes: std::num::NonZeroUsize,
    capture: CaptureDurationLimit,
}
impl ImportLimits {
    /// Bound one log's bytes and retained voice frames before allocating/copying them.
    #[must_use]
    pub const fn new(maximum_bytes: std::num::NonZeroUsize, capture: CaptureDurationLimit) -> Self {
        Self {
            maximum_bytes,
            capture,
        }
    }
    /// Maximum input bytes for one log.
    #[must_use]
    pub const fn maximum_bytes(self) -> std::num::NonZeroUsize {
        self.maximum_bytes
    }
    /// Maximum received codec frames for one log.
    #[must_use]
    pub const fn capture(self) -> CaptureDurationLimit {
        self.capture
    }
}

/// Import every published dvrec that has no locally captured twin.
///
/// Walks `<recordings>/<SYS>-<M>/<date>/published/*.dvrec` and
/// reconstructs a recording per unmatched transmission (twins are
/// matched on stream id within a start-time window). Existing
/// captures, including gapped ones, are never overwritten.
///
/// # Errors
///
/// I/O errors walking the tree. Per-file parse/write failures are
/// counted in [`ImportSummary::failed`], not fatal.
pub fn import_tree(
    recordings: &Path,
    writer: &Writer,
    limits: ImportLimits,
) -> std::io::Result<ImportSummary> {
    let mut summary = ImportSummary::default();
    for target_entry in std::fs::read_dir(recordings)? {
        let target_dir = target_entry?.path();
        let Some(name) = target_dir.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        let Ok(target) = name.parse::<TargetKey>() else {
            continue;
        };
        if !target_dir.is_dir() {
            continue;
        }
        for date_entry in std::fs::read_dir(&target_dir)? {
            let date_dir = date_entry?.path();
            let published = date_dir.join("published");
            if !published.is_dir() {
                continue;
            }
            let local =
                crate::harvest::LoadedLocal::from_directory(&date_dir, limits.maximum_bytes)?;
            for file_entry in std::fs::read_dir(&published)? {
                let path = file_entry?.path();
                if path.extension() != Some("dvrec".as_ref()) {
                    continue;
                }
                import_one(
                    &path,
                    &target,
                    &local.recordings,
                    writer,
                    limits,
                    &mut summary,
                );
            }
        }
    }
    Ok(summary)
}

fn import_one(
    path: &Path,
    target: &TargetKey,
    local: &[crate::harvest::LocalRecording],
    writer: &Writer,
    limits: ImportLimits,
    summary: &mut ImportSummary,
) {
    let Ok(text) = read_bounded(path, limits.maximum_bytes) else {
        summary.failed += 1;
        return;
    };
    let parsed = match ParsedDvrec::from_text(&text, limits.capture) {
        Ok(p) => p,
        Err(DvrecError::NoFrames) => {
            // Header/end-only logs contain no voice samples to reconstruct.
            tracing::debug!(path = %path.display(), "voiceless dvrec (kerchunk); skipped");
            summary.skipped_voiceless += 1;
            return;
        }
        Err(e) => {
            tracing::warn!(path = %path.display(), error = %e, "dvrec parse failed");
            summary.failed += 1;
            return;
        }
    };
    let file_sid = path
        .file_stem()
        .and_then(|name| name.to_str())
        .and_then(|stem| stem.rsplit_once("--"))
        .and_then(|(_, id)| u16::from_str_radix(id, 16).ok())
        .and_then(StreamId::new);
    if file_sid != Some(parsed.stream_id) {
        tracing::warn!(path = %path.display(), stream_id = %parsed.stream_id, "dvrec filename and line stream identities differ");
        summary.failed += 1;
        return;
    }
    let twin = local.iter().any(|rec| {
        rec.stream_id() == parsed.stream_id
            && (rec.started_at() - parsed.started_at).num_seconds().abs() <= MATCH_TOLERANCE_SECS
    });
    if twin {
        summary.skipped_existing += 1;
        return;
    }
    let recording = CompletedRecording {
        origin: StreamOrigin::imported(target.clone()),
        stream_id: parsed.stream_id,
        header: Some(parsed.header),
        header_diagnostics: vec![
            "reconstructed from the reflector-published dvrec packet log".to_string(),
        ],
        started_at: parsed.started_at,
        ended_at: parsed.ended_at,
        end_reason: if parsed.saw_eot {
            EndReason::Eot
        } else {
            EndReason::Inactivity
        },
        frames: parsed.frames,
        gaps: parsed.gaps,
        text: None,
        text_bytes: None,
        dprs: Vec::new(),
    };
    match writer.write(&recording) {
        Ok(json_path) => {
            tracing::info!(path = %json_path.display(), "imported from dvrec");
            summary.imported += 1;
        }
        Err(e) => {
            tracing::error!(path = %path.display(), error = %e, "import write failed");
            summary.failed += 1;
        }
    }
}

fn read_bounded(path: &Path, maximum: std::num::NonZeroUsize) -> std::io::Result<String> {
    use std::io::Read as _;
    let mut file = std::fs::File::open(path)?;
    let mut bytes = Vec::new();
    let mut chunk = [0; 8192];
    loop {
        let count = file.read(&mut chunk)?;
        if count == 0 {
            break;
        }
        let length = bytes
            .len()
            .checked_add(count)
            .filter(|length| *length <= maximum.get())
            .ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "dvrec input byte limit exceeded",
                )
            })?;
        bytes
            .try_reserve(length - bytes.len())
            .map_err(std::io::Error::other)?;
        bytes.extend_from_slice(
            chunk
                .get(..count)
                .ok_or_else(|| std::io::Error::other("invalid read count"))?,
        );
    }
    String::from_utf8(bytes)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    /// Header, voice and end-marker grammar with one missing sequence value.
    const FIXTURE: &str = "\
#DVREC 20260711-175407.594 dplus
20260711-175407.594>c2g:20:0002:01:58e6:80:hdr:000000:REF030 C:MM3TWA D:CQCQCQ  :MM3TWA  /AMBE:000b
20260711-175407.890>c2g:20:0002:01:58e6:00:vd :ac6b06c1f0c029975c:552d16#256285 |%b.|
20260711-175407.910>c2g:20:0002:01:58e6:01:vd :2f210e626b3e6f0790:4d4d33#3d3d33 |M M3|
20260711-175407.950>c2g:20:0002:01:58e6:03:vd :7ac6883a2f11dba642:aaaaaa#dadada |...|
20260711-175409.000>c2g:20:0002:01:58e6:44:vde:9e8d3288261a3f61e8:555555#251ac6 |%..|
";

    #[test]
    fn parses_real_shaped_dvrec() -> TestResult {
        let parsed = ParsedDvrec::from_text(FIXTURE, CaptureDurationLimit::try_from_seconds(60)?)?;
        assert_eq!(parsed.header.rpt2.text()?, "REF030 C");
        assert_eq!(parsed.header.rpt1.text()?, "MM3TWA D");
        assert_eq!(parsed.header.ur_call.text()?, "CQCQCQ");
        assert_eq!(parsed.header.my_call.text()?, "MM3TWA");
        assert_eq!(parsed.header.my_suffix.text()?, "AMBE");
        assert_eq!(*parsed.header.flags.as_bytes(), [0, 0, 0]);
        assert_eq!(
            parsed.stream_id.get(),
            0xE658,
            "line order is byte-swapped vs filename order"
        );
        assert_eq!(parsed.frames.len(), 3, "vde is not a voice frame");
        let first = parsed.frames.first().ok_or("frame 0")?;
        assert_eq!(first.seq, 0);
        assert_eq!(
            first.ambe,
            [0xAC, 0x6B, 0x06, 0xC1, 0xF0, 0xC0, 0x29, 0x97, 0x5C]
        );
        assert_eq!(first.slow_data, [0x55, 0x2D, 0x16]);
        assert_eq!(
            parsed.frames.iter().map(|f| f.seq).collect::<Vec<_>>(),
            [0, 1, 3]
        );
        assert_eq!(parsed.gaps, 1, "seq 02 missing");
        assert!(parsed.saw_eot);
        assert_eq!(parsed.skipped_lines, 0);
        assert_eq!(
            parsed.started_at.to_rfc3339(),
            "2026-07-11T17:54:07.594+00:00"
        );
        assert_eq!(parsed.ended_at.to_rfc3339(), "2026-07-11T17:54:09+00:00");
        Ok(())
    }

    #[test]
    fn gap_accounting_matches_capture_core_on_dup_and_wild_seq() -> TestResult {
        // A duplicate seq (01, 01) and an out-of-alphabet seq (2A = 42,
        // e.g. a corrupted byte) must both contribute ZERO gaps; a
        // repeat is not a 20-frame gap and a wild byte must not be
        // counted. Seqs here: 0,1,1,42,2. The
        // only genuine discontinuity (1 -> 2 across the noise) is none,
        // since 2 follows 1 in the alphabet once the wild value is
        // ignored... but the wild value resets prev, so 42 -> 2 is also
        // guarded to 0. Net: 0 gaps.
        let dup_fixture = "\
#DVREC 20260711-175407.594 dplus
20260711-175407.594>c2g:20:0002:01:58e6:80:hdr:000000:REF030 C:MM3TWA D:CQCQCQ  :MM3TWA  /AMBE:000b
20260711-175407.890>c2g:20:0002:01:58e6:00:vd :ac6b06c1f0c029975c:552d16#256285 |%b.|
20260711-175407.910>c2g:20:0002:01:58e6:01:vd :2f210e626b3e6f0790:4d4d33#3d3d33 |M M3|
20260711-175407.930>c2g:20:0002:01:58e6:01:vd :2f210e626b3e6f0790:4d4d33#3d3d33 |M M3|
20260711-175407.940>c2g:20:0002:01:58e6:2a:vd :7ac6883a2f11dba642:aaaaaa#dadada |...|
20260711-175407.960>c2g:20:0002:01:58e6:02:vd :7ac6883a2f11dba642:aaaaaa#dadada |...|
20260711-175409.000>c2g:20:0002:01:58e6:44:vde:9e8d3288261a3f61e8:555555#251ac6 |%..|
";
        let parsed =
            ParsedDvrec::from_text(dup_fixture, CaptureDurationLimit::try_from_seconds(60)?)?;
        assert_eq!(
            parsed.frames.iter().map(|f| f.seq).collect::<Vec<_>>(),
            [0, 1, 1, 0x2A, 2]
        );
        assert_eq!(
            parsed.gaps, 0,
            "duplicate and out-of-alphabet seqs must not inflate gaps"
        );
        Ok(())
    }

    #[test]
    fn rejects_headerless_and_frameless_logs() -> TestResult {
        let limit = CaptureDurationLimit::try_from_seconds(60)?;
        let no_hdr = "#DVREC x dplus\n\
            20260711-175407.890>c2g:20:0002:01:58e6:00:vd :ac6b06c1f0c029975c:552d16#0 |x|\n";
        assert!(matches!(
            ParsedDvrec::from_text(no_hdr, limit),
            Err(DvrecError::MissingHeader)
        ));

        let no_frames = "#DVREC x dplus\n\
            20260711-175407.594>c2g:20:0002:01:58e6:80:hdr:000000:REF030 C:MM3TWA D:CQCQCQ  :MM3TWA  /AMBE:000b\n";
        assert!(matches!(
            ParsedDvrec::from_text(no_frames, limit),
            Err(DvrecError::NoFrames)
        ));
        Ok(())
    }

    #[test]
    fn import_tree_reconstructs_and_is_idempotent() -> TestResult {
        let dir = tempfile::tempdir()?;
        let recordings = dir.path().join("recordings");
        let published = recordings
            .join("REF030-C")
            .join("2026-07-11")
            .join("published");
        std::fs::create_dir_all(&published)?;
        std::fs::write(
            published.join("REF030-C--20260711-175407.594--MM3TWA--e658.dvrec"),
            FIXTURE,
        )?;
        let writer = Writer::new(recordings.clone(), true);
        let limits = ImportLimits::new(
            const { std::num::NonZeroUsize::new(1_048_576).unwrap() },
            CaptureDurationLimit::try_from_seconds(60)?,
        );

        let summary = import_tree(&recordings, &writer, limits)?;
        assert_eq!(summary.imported, 1, "{summary:?}");
        assert_eq!(summary.failed, 0);
        let date_dir = recordings.join("REF030-C").join("2026-07-11");
        let stem = "20260711T175407Z_MM3TWA_E658";
        for ext in ["ambe", "wav", "json"] {
            assert!(
                date_dir.join(format!("{stem}.{ext}")).exists(),
                "{stem}.{ext} missing"
            );
        }
        let doc: serde_json::Value =
            serde_json::from_slice(&std::fs::read(date_dir.join(format!("{stem}.json")))?)?;
        assert_eq!(
            doc.pointer("/header/my_callsign").and_then(|v| v.as_str()),
            Some("MM3TWA")
        );
        assert_eq!(
            doc.pointer("/frames/gaps")
                .and_then(serde_json::Value::as_u64),
            Some(1)
        );
        assert_eq!(
            doc.pointer("/end_reason").and_then(|v| v.as_str()),
            Some("eot")
        );

        // Second walk: the freshly written recording is the twin.
        let summary = import_tree(&recordings, &writer, limits)?;
        assert_eq!(summary.imported, 0, "{summary:?}");
        assert_eq!(summary.skipped_existing, 1);

        // A header and end marker without voice is counted separately.
        std::fs::write(
            recordings
                .join("REF030-C")
                .join("2026-07-11")
                .join("published")
                .join("REF030-C--20260711-190000.000--W1AW--aaaa.dvrec"),
            "#DVREC 20260711-190000.000 dplus\n\
             20260711-190000.000>c2g:20:0002:01:aaaa:80:hdr:000000:REF030 C:W1AW C:CQCQCQ  :W1AW    /    :0000\n\
             20260711-190000.400>c2g:20:0002:01:aaaa:44:vde:9e8d3288261a3f61e8:555555#251ac6 |%..|\n",
        )?;
        let summary = import_tree(&recordings, &writer, limits)?;
        assert_eq!(summary.failed, 0, "{summary:?}");
        assert_eq!(summary.skipped_voiceless, 1, "{summary:?}");
        Ok(())
    }

    #[test]
    fn tolerates_garbage_lines_and_counts_them() -> TestResult {
        let mut text = FIXTURE.to_string();
        text.push_str("not a record line\n");
        text.push_str("20260711-175410.000>c2g:20:0002:01:58e6:05:vd :SHORTHEX:552d16#0 |x|\n");
        let parsed = ParsedDvrec::from_text(&text, CaptureDurationLimit::try_from_seconds(60)?)?;
        assert_eq!(parsed.frames.len(), 3);
        assert_eq!(parsed.skipped_lines, 2);
        Ok(())
    }

    #[test]
    fn rejects_mixed_streams_and_oversized_header_fields() -> TestResult {
        let limit = CaptureDurationLimit::try_from_seconds(60)?;
        let mixed = FIXTURE.replacen("58e6:01:vd", "59e6:01:vd", 1);
        assert!(matches!(
            ParsedDvrec::from_text(&mixed, limit),
            Err(DvrecError::MixedStream { .. })
        ));
        let wide = FIXTURE.replace(":REF030 C:", ":REF030 CC:");
        assert!(matches!(
            ParsedDvrec::from_text(&wide, limit),
            Err(DvrecError::FieldWidth {
                field: HeaderField::Rpt2,
                maximum: 8,
                actual: 9
            })
        ));
        let suffix = FIXTURE.replace("/AMBE:", "/ABCDE:");
        assert!(matches!(
            ParsedDvrec::from_text(&suffix, limit),
            Err(DvrecError::FieldWidth {
                field: HeaderField::Suffix,
                ..
            })
        ));
        let many = [
            FIXTURE,
            &"20260711-175410.000>c2g:20:0002:01:58e6:05:vd :ac6b06c1f0c029975c:552d16#0 |x|\n"
                .repeat(50),
        ]
        .concat();
        assert!(matches!(
            ParsedDvrec::from_text(&many, CaptureDurationLimit::try_from_seconds(1)?),
            Err(DvrecError::FrameLimit)
        ));
        Ok(())
    }

    #[test]
    fn import_rejects_filename_identity_mismatch_and_input_overflow() -> TestResult {
        let directory = tempfile::tempdir()?;
        let published = directory.path().join("REF030-C/2026-07-11/published");
        std::fs::create_dir_all(&published)?;
        let path = published.join("REF030-C--20260711-175407.594--MM3TWA--e659.dvrec");
        std::fs::write(&path, FIXTURE)?;
        let writer = Writer::new(directory.path().to_path_buf(), false);
        let capture = CaptureDurationLimit::try_from_seconds(60)?;
        let limits = ImportLimits::new(
            const { std::num::NonZeroUsize::new(1_048_576).unwrap() },
            capture,
        );
        let summary = import_tree(directory.path(), &writer, limits)?;
        assert_eq!(summary.failed, 1);
        assert_eq!(summary.imported, 0);
        let limited =
            ImportLimits::new(const { std::num::NonZeroUsize::new(10).unwrap() }, capture);
        let summary = import_tree(directory.path(), &writer, limited)?;
        assert_eq!(summary.failed, 1);
        assert!(read_bounded(&path, limited.maximum_bytes()).is_err());
        Ok(())
    }
}
