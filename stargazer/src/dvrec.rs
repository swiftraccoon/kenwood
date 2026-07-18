// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later

//! Import reflector-published `dvrec` packet logs as recordings.
//!
//! The publishing daemon logs every packet of a transmission as one
//! timestamped text line; the voice lines carry the 9 AMBE bytes and
//! 3 slow-data bytes as hex, and a header line carries the full
//! D-STAR routing header. A capture from our own client session of
//! the same stream is byte-identical (verified against a live
//! transmission frame for frame), so a transmission we never
//! captured — recorder offline, target disabled, or a reflector we
//! do not link — can be reconstructed from its published dvrec into
//! the same `.ambe`/`.wav`/`.json` recording layout the live
//! recorder writes, and from there paired with its published
//! reference MP3 like any other recording.
//!
//! Line shape (fields colon-separated after the timestamp):
//!
//! ```text
//! #DVREC 20260711-175407.594 dplus
//! <ts>>c2g:20:0002:01:<sid-le>:<seq>:hdr:<flags>:<RPT2>:<RPT1>:<UR>:<MY>/<SUFX>:<pfcs>
//! <ts>>c2g:20:0002:01:<sid-le>:<seq>:vd :<18 hex ambe>:<6 hex slow>#<descrambled> |ascii|
//! ```
//!
//! The stream id in each line is byte-swapped relative to the id the
//! publisher uses in filenames. A `vde` line marks end-of-stream;
//! its payload is not treated as a voice frame.

use std::path::Path;

use chrono::{DateTime, Utc};
use dstar_gateway_core::{Callsign, DStarHeader, Module, StreamId, Suffix};

use crate::capture::{CompletedRecording, EndReason, FrameRecord, StreamOrigin};
use crate::writer::Writer;

/// Failures parsing a dvrec file.
#[derive(Debug, thiserror::Error)]
pub enum DvrecError {
    /// No header line — the D-STAR routing fields are unknown.
    #[error("no hdr line in dvrec")]
    MissingHeader,
    /// No voice frames.
    #[error("no voice frames in dvrec")]
    NoFrames,
    /// The stream id was zero or unparseable.
    #[error("bad stream id")]
    BadStreamId,
}

/// A parsed dvrec transmission.
#[derive(Debug)]
pub struct ParsedDvrec {
    /// Routing header from the `hdr` line.
    pub header: DStarHeader,
    /// Stream id (filename byte order).
    pub stream_id: StreamId,
    /// Voice frames in log order.
    pub frames: Vec<FrameRecord>,
    /// Missing frames implied by seq discontinuities (mod 21).
    pub gaps: u64,
    /// First line's timestamp.
    pub started_at: DateTime<Utc>,
    /// Last line's timestamp (a `vde` line when present).
    pub ended_at: DateTime<Utc>,
    /// True when a `vde` end-of-stream line was seen.
    pub saw_eot: bool,
    /// Lines that did not parse as any known record.
    pub skipped_lines: usize,
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
fn wire_field<const N: usize>(s: &str) -> [u8; N] {
    let mut out = [b' '; N];
    for (slot, byte) in out.iter_mut().zip(s.bytes()) {
        *slot = byte;
    }
    out
}

/// One tokenized log line.
struct LogLine<'a> {
    ts: DateTime<Utc>,
    sid_filename_order: u16,
    seq: u8,
    record: &'a str,
    payload: Vec<&'a str>,
}

fn tokenize(line: &str) -> Option<LogLine<'_>> {
    let (ts_str, rest) = line.split_once('>')?;
    let ts = parse_ts(ts_str)?;
    let fields: Vec<&str> = rest.split(':').collect();
    // [c2g, len, .., .., sid, seq, record, payload...]
    let sid = u16::from_str_radix(fields.get(4)?, 16).ok()?;
    let seq = u8::from_str_radix(fields.get(5)?, 16).ok()?;
    let record = fields.get(6)?.trim();
    Some(LogLine {
        ts,
        sid_filename_order: sid.swap_bytes(),
        seq,
        record,
        payload: fields.get(7..).unwrap_or(&[]).to_vec(),
    })
}

fn header_from_payload(payload: &[&str]) -> Option<DStarHeader> {
    let flags = hex_bytes::<3>(payload.first()?)?;
    let (my, suffix) = payload.get(4)?.split_once('/')?;
    Some(DStarHeader {
        flag1: flags[0],
        flag2: flags[1],
        flag3: flags[2],
        rpt2: Callsign::from_wire_bytes(wire_field::<8>(payload.get(1)?)),
        rpt1: Callsign::from_wire_bytes(wire_field::<8>(payload.get(2)?)),
        ur_call: Callsign::from_wire_bytes(wire_field::<8>(payload.get(3)?)),
        my_call: Callsign::from_wire_bytes(wire_field::<8>(my)),
        my_suffix: Suffix::from_wire_bytes(wire_field::<4>(suffix)),
    })
}

fn frame_from_payload(payload: &[&str], seq: u8) -> Option<FrameRecord> {
    let ambe = hex_bytes::<9>(payload.first()?)?;
    let slow_hex = payload.get(1)?.split('#').next()?;
    let slow_data = hex_bytes::<3>(slow_hex)?;
    Some(FrameRecord {
        seq,
        ambe,
        slow_data,
    })
}

/// Parse one dvrec text file.
///
/// # Errors
///
/// [`DvrecError`] when the log is missing its header line, carries no
/// voice frames, or its stream id is invalid.
pub fn parse_dvrec(text: &str) -> Result<ParsedDvrec, DvrecError> {
    let mut header = None;
    let mut stream_id_raw = None;
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
        if stream_id_raw.is_none() {
            stream_id_raw = Some(log.sid_filename_order);
        }
        match log.record {
            "hdr" => {
                if header.is_none() {
                    match header_from_payload(&log.payload) {
                        Some(h) => header = Some(h),
                        None => skipped_lines += 1,
                    }
                }
            }
            "vd" => match frame_from_payload(&log.payload, log.seq) {
                None => skipped_lines += 1,
                Some(frame) => {
                    // Shared gap accounting: guards the untrusted seq
                    // byte (the 0x40 EOT flag can ride it) and treats a
                    // duplicate as zero gaps. The prior local
                    // `(seq - prev - 1).rem_euclid(21)` form yielded 20
                    // for a repeat and ran unguarded on wild bytes.
                    if let Some(prev) = prev_seq {
                        gaps += crate::capture::seq_gap(prev, log.seq);
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
    let stream_id = stream_id_raw
        .and_then(StreamId::new)
        .ok_or(DvrecError::BadStreamId)?;
    let started_at = started_at.ok_or(DvrecError::NoFrames)?;
    let ended_at = ended_at.ok_or(DvrecError::NoFrames)?;
    Ok(ParsedDvrec {
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

/// Import outcome for one tree walk.
#[derive(Debug, Default)]
pub struct ImportSummary {
    /// Recordings written.
    pub imported: usize,
    /// Transmissions that already had a local recording.
    pub skipped_existing: usize,
    /// Voiceless logs (kerchunks: header + end marker, zero voice
    /// frames) — a normal on-air shape, nothing to reconstruct.
    pub skipped_voiceless: usize,
    /// Dvrec files that could not be parsed or written.
    pub failed: usize,
}

/// Publish-vs-capture start-time skew treated as the same
/// transmission when a stream id repeats within a day.
const MATCH_TOLERANCE_SECS: i64 = 120;

/// Import every published dvrec that has no locally captured twin.
///
/// Walks `<recordings>/<SYS>-<M>/<date>/published/*.dvrec` and
/// reconstructs a recording per unmatched transmission (twins are
/// matched on stream id within a start-time window). Existing
/// captures — including gapped ones — are never overwritten.
///
/// # Errors
///
/// I/O errors walking the tree. Per-file parse/write failures are
/// counted in [`ImportSummary::failed`], not fatal.
pub fn import_tree(recordings: &Path, writer: &Writer) -> std::io::Result<ImportSummary> {
    let mut summary = ImportSummary::default();
    for target_entry in std::fs::read_dir(recordings)? {
        let target_dir = target_entry?.path();
        let Some(name) = target_dir.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        let Some((system, module_char)) = crate::harvest::split_target(name) else {
            continue;
        };
        let Ok(module) = Module::try_from_char(module_char) else {
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
            let local = crate::harvest::load_local_recordings(&date_dir)?;
            for file_entry in std::fs::read_dir(&published)? {
                let path = file_entry?.path();
                if path.extension() != Some("dvrec".as_ref()) {
                    continue;
                }
                import_one(
                    &path,
                    &system,
                    module,
                    &local.recordings,
                    writer,
                    &mut summary,
                );
            }
        }
    }
    Ok(summary)
}

fn import_one(
    path: &Path,
    system: &str,
    module: Module,
    local: &[crate::harvest::LocalRecording],
    writer: &Writer,
    summary: &mut ImportSummary,
) {
    let Ok(text) = std::fs::read_to_string(path) else {
        summary.failed += 1;
        return;
    };
    let parsed = match parse_dvrec(&text) {
        Ok(p) => p,
        Err(DvrecError::NoFrames) => {
            // Kerchunk: a header and end marker with no voice at all.
            // Normal on-air behavior, not a failure — and it must not
            // fail the import step (the nightly pipeline aborts on a
            // failing exit before ever reaching alignment).
            tracing::debug!(path = %path.display(), "voiceless dvrec (kerchunk) — skipped");
            summary.skipped_voiceless += 1;
            return;
        }
        Err(e) => {
            tracing::warn!(path = %path.display(), error = %e, "dvrec parse failed");
            summary.failed += 1;
            return;
        }
    };
    let sid_hex = format!("{:04X}", parsed.stream_id.get());
    let twin = local.iter().any(|rec| {
        rec.stream_id == sid_hex
            && (rec.started_at - parsed.started_at).num_seconds().abs() <= MATCH_TOLERANCE_SECS
    });
    if twin {
        summary.skipped_existing += 1;
        return;
    }
    let recording = CompletedRecording {
        origin: StreamOrigin {
            reflector: system.to_string(),
            module,
            protocol: "dplus",
            host: format!("{}.dstargateway.org", system.to_ascii_lowercase()),
            port: 20001,
            peer: std::net::SocketAddr::new(
                std::net::IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED),
                0,
            ),
        },
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

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    /// Real REF030 dvrec structure. The header line, first voice
    /// line, and vde end-marker shape are verified against live
    /// captures (a census over 97 published dvrecs parsed every line:
    /// only hdr/vd/vde records exist). The vde payload is always the
    /// AMBE silence constant with its seq carrying an 0x40 end flag —
    /// correctly not a voice frame. Later vd lines here carry real
    /// AMBE payloads with abridged slow-data, and one seq gap is
    /// contrived at 02.
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
        let parsed = parse_dvrec(FIXTURE)?;
        assert_eq!(parsed.header.rpt2.as_str(), "REF030 C");
        assert_eq!(parsed.header.rpt1.as_str(), "MM3TWA D");
        assert_eq!(parsed.header.ur_call.as_str(), "CQCQCQ");
        assert_eq!(parsed.header.my_call.as_str(), "MM3TWA");
        assert_eq!(parsed.header.my_suffix.as_str(), "AMBE");
        assert_eq!(
            [
                parsed.header.flag1,
                parsed.header.flag2,
                parsed.header.flag3
            ],
            [0, 0, 0]
        );
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
        // e.g. a corrupted byte) must both contribute ZERO gaps — the
        // divergent local formula previously reported 20 for a repeat
        // and ran unguarded on wild bytes. Seqs here: 0,1,1,42,2 — the
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
        let parsed = parse_dvrec(dup_fixture)?;
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
    fn rejects_headerless_and_frameless_logs() {
        let no_hdr = "#DVREC x dplus\n\
            20260711-175407.890>c2g:20:0002:01:58e6:00:vd :ac6b06c1f0c029975c:552d16#0 |x|\n";
        assert!(matches!(
            parse_dvrec(no_hdr),
            Err(DvrecError::MissingHeader)
        ));

        let no_frames = "#DVREC x dplus\n\
            20260711-175407.594>c2g:20:0002:01:58e6:80:hdr:000000:REF030 C:MM3TWA D:CQCQCQ  :MM3TWA  /AMBE:000b\n";
        assert!(matches!(parse_dvrec(no_frames), Err(DvrecError::NoFrames)));
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

        let summary = import_tree(&recordings, &writer)?;
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
        let summary = import_tree(&recordings, &writer)?;
        assert_eq!(summary.imported, 0, "{summary:?}");
        assert_eq!(summary.skipped_existing, 1);

        // A voiceless kerchunk log is a skip, never a failure — a
        // failing import exit would abort the nightly pipeline
        // before alignment.
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
        let summary = import_tree(&recordings, &writer)?;
        assert_eq!(summary.failed, 0, "{summary:?}");
        assert_eq!(summary.skipped_voiceless, 1, "{summary:?}");
        Ok(())
    }

    #[test]
    fn tolerates_garbage_lines_and_counts_them() -> TestResult {
        let mut text = FIXTURE.to_string();
        text.push_str("not a record line\n");
        text.push_str("20260711-175410.000>c2g:20:0002:01:58e6:05:vd :SHORTHEX:552d16#0 |x|\n");
        let parsed = parse_dvrec(&text)?;
        assert_eq!(parsed.frames.len(), 3);
        assert_eq!(parsed.skipped_lines, 2);
        Ok(())
    }
}
