// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later

//! Recording writer: metadata document construction and atomic
//! three-file persistence.

use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::capture::{CompletedRecording, DprsSeen, FrameRecord};

/// What happened to the WAV for a recording.
#[derive(Debug, Clone)]
pub struct AudioOutcome {
    /// Whether a WAV file was written.
    pub wav: bool,
    /// Frames synthesized by gap concealment during decode.
    pub concealed_frames: u64,
    /// Error message if WAV writing failed (WAV is re-derivable, so
    /// the recording is still committed).
    pub error: Option<String>,
}

/// Top-level metadata JSON document (schema `stargazer-recording/1`).
#[derive(Debug, Serialize)]
pub struct RecordingDoc {
    schema: &'static str,
    writer: String,
    reflector: String,
    module: String,
    protocol: &'static str,
    host: String,
    peer: String,
    stream_id: String,
    started_at: String,
    ended_at: String,
    duration_s: f64,
    end_reason: &'static str,
    header: Option<HeaderDoc>,
    frames: FramesDoc,
    fec: FecDoc,
    slow_data: SlowDataDoc,
    audio: AudioDoc,
}

#[derive(Debug, Serialize)]
struct HeaderDoc {
    my_callsign: String,
    my_suffix: Option<String>,
    ur_callsign: Option<String>,
    rpt1: String,
    rpt2: String,
    flags: [u8; 3],
    raw_hex: String,
    diagnostics: Vec<String>,
}

#[derive(Debug, Serialize)]
struct FramesDoc {
    received: u64,
    expected: u64,
    gaps: u64,
}

#[derive(Debug, Serialize)]
struct FecDoc {
    corrected_bits: u64,
    frames_with_errors: u64,
    erasure_frames: u64,
    tone_frames: u64,
}

#[derive(Debug, Serialize)]
struct SlowDataDoc {
    text: Option<String>,
    /// Raw 20 message bytes as hex — present only when the message
    /// is not clean printable ASCII (e.g. JIS X 0201 half-width
    /// katakana from Japanese radios), where `text` is lossy.
    #[serde(skip_serializing_if = "Option::is_none")]
    text_hex: Option<String>,
    dprs: Vec<DprsDoc>,
}

#[derive(Debug, Serialize)]
struct DprsDoc {
    callsign: String,
    lat: f64,
    lon: f64,
    symbol: char,
    comment: Option<String>,
    at_frame: usize,
}

#[derive(Debug, Serialize)]
struct AudioDoc {
    wav: bool,
    concealed_frames: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

/// RFC 3339 with millisecond precision and a `Z` suffix.
fn rfc3339_millis(t: chrono::DateTime<chrono::Utc>) -> String {
    t.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

fn trimmed(s: &str) -> String {
    s.trim_end().to_string()
}

fn trimmed_opt(s: &str) -> Option<String> {
    let t = s.trim_end();
    if t.is_empty() {
        None
    } else {
        Some(t.to_string())
    }
}

fn hex_lower(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut s, b| {
            let _ = write!(s, "{b:02x}");
            s
        })
}

/// Build the metadata document for a completed recording.
///
/// FEC statistics are computed here from the archived frames via
/// [`mbelib_rs::frame_fec`] — a convenience index; the authoritative
/// numbers are always re-derivable from the `.ambe` container.
#[must_use]
pub fn build_doc(rec: &CompletedRecording, audio: &AudioOutcome) -> RecordingDoc {
    let mut fec = FecDoc {
        corrected_bits: 0,
        frames_with_errors: 0,
        erasure_frames: 0,
        tone_frames: 0,
    };
    for frame in &rec.frames {
        let f = mbelib_rs::frame_fec(&frame.ambe);
        fec.corrected_bits += u64::from(f.total_errors);
        if f.total_errors > 0 {
            fec.frames_with_errors += 1;
        }
        match f.kind {
            mbelib_rs::FrameKind::Erasure => fec.erasure_frames += 1,
            mbelib_rs::FrameKind::Tone => fec.tone_frames += 1,
            mbelib_rs::FrameKind::Voice => {}
        }
    }

    let header = rec.header.as_ref().map(|h| HeaderDoc {
        my_callsign: trimmed(&h.my_call.as_str()),
        my_suffix: trimmed_opt(&h.my_suffix.as_str()),
        ur_callsign: trimmed_opt(&h.ur_call.as_str()),
        rpt1: trimmed(&h.rpt1.as_str()),
        rpt2: trimmed(&h.rpt2.as_str()),
        flags: [h.flag1, h.flag2, h.flag3],
        raw_hex: hex_lower(&h.encode()),
        diagnostics: rec.header_diagnostics.clone(),
    });

    RecordingDoc {
        schema: "stargazer-recording/1",
        writer: format!("stargazer/{}", env!("CARGO_PKG_VERSION")),
        reflector: rec.origin.reflector.clone(),
        module: rec.origin.module.as_char().to_string(),
        protocol: rec.origin.protocol,
        host: format!("{}:{}", rec.origin.host, rec.origin.port),
        peer: rec.origin.peer.to_string(),
        stream_id: format!("{:04X}", rec.stream_id.get()),
        started_at: rfc3339_millis(rec.started_at),
        ended_at: rfc3339_millis(rec.ended_at),
        duration_s: rec.duration_s(),
        end_reason: rec.end_reason.as_str(),
        header,
        frames: FramesDoc {
            received: u64::try_from(rec.frames.len()).unwrap_or(u64::MAX),
            expected: rec.expected_frames(),
            gaps: rec.gaps,
        },
        fec,
        slow_data: SlowDataDoc {
            text: rec.text.clone(),
            text_hex: rec.text_bytes.and_then(|bytes| {
                let ascii = bytes.iter().all(|b| (0x20..=0x7E).contains(b));
                if ascii { None } else { Some(hex_lower(&bytes)) }
            }),
            dprs: rec.dprs.iter().map(dprs_doc).collect(),
        },
        audio: AudioDoc {
            wav: audio.wav,
            concealed_frames: audio.concealed_frames,
            error: audio.error.clone(),
        },
    }
}

fn dprs_doc(seen: &DprsSeen) -> DprsDoc {
    DprsDoc {
        callsign: trimmed(&seen.report.callsign.as_str()),
        lat: seen.report.latitude.degrees(),
        lon: seen.report.longitude.degrees(),
        symbol: seen.report.symbol,
        comment: seen.report.comment.clone(),
        at_frame: seen.at_frame,
    }
}

/// Container magic for the `.ambe` frame archive.
const AMBE_MAGIC: &[u8; 8] = b"STGZAMBE";
/// Container format version.
const AMBE_VERSION: u16 = 1;
/// Bytes per record: seq(1) + ambe(9) + `slow_data`(3).
const AMBE_RECORD_LEN: u16 = 13;

/// A recording write failure.
#[derive(Debug, thiserror::Error)]
pub enum WriteError {
    /// Filesystem error with the path it occurred on.
    #[error("{context} {path}: {source}")]
    Io {
        /// What was being attempted.
        context: &'static str,
        /// Path involved.
        path: PathBuf,
        /// Underlying error.
        #[source]
        source: std::io::Error,
    },
    /// The metadata document failed to serialize (unexpected).
    #[error("serialize metadata: {0}")]
    Serialize(#[from] serde_json::Error),
    /// No free filename variant was found (extremely unexpected).
    #[error("no free filename for stem {0} after 99 attempts")]
    NameExhausted(String),
}

fn io_err(context: &'static str, path: &Path) -> impl FnOnce(std::io::Error) -> WriteError {
    let path = path.to_path_buf();
    move |source| WriteError::Io {
        context,
        path,
        source,
    }
}

/// Persists completed recordings as `.ambe` + `.wav` + `.json`.
#[derive(Debug)]
pub struct Writer {
    base: PathBuf,
    write_wav: bool,
}

impl Writer {
    /// Create a writer rooted at `base` (the configured recordings dir).
    #[must_use]
    pub const fn new(base: PathBuf, write_wav: bool) -> Self {
        Self { base, write_wav }
    }

    /// Write one recording: `.ambe` (ground truth — failure aborts),
    /// then `.wav` (failure is recorded in the JSON, not fatal),
    /// then `.json` (the commit marker, fsynced). Returns the JSON path.
    ///
    /// # Errors
    ///
    /// [`WriteError`] if the directory, `.ambe`, or `.json` cannot be
    /// written.
    pub fn write(&self, rec: &CompletedRecording) -> Result<PathBuf, WriteError> {
        let dir = self
            .base
            .join(format!(
                "{}-{}",
                rec.origin.reflector,
                rec.origin.module.as_char()
            ))
            .join(rec.started_at.format("%Y-%m-%d").to_string());
        fs::create_dir_all(&dir).map_err(io_err("create dir", &dir))?;

        let stem = unique_stem(&dir, rec)?;

        let ambe_path = dir.join(format!("{stem}.ambe"));
        write_atomic(&ambe_path, &container_bytes(&rec.frames), false)?;

        let mut outcome = AudioOutcome {
            wav: false,
            concealed_frames: 0,
            error: None,
        };
        if self.write_wav {
            let decoded = crate::audio::decode_stream(&rec.frames);
            outcome.concealed_frames = decoded.concealed_frames;
            let wav_path = dir.join(format!("{stem}.wav"));
            match write_atomic(&wav_path, &crate::wav::wav_bytes(&decoded.pcm), false) {
                Ok(()) => outcome.wav = true,
                Err(e) => {
                    tracing::error!(path = %wav_path.display(), error = %e, "WAV write failed");
                    outcome.error = Some(e.to_string());
                }
            }
        }

        let doc = build_doc(rec, &outcome);
        let json_path = dir.join(format!("{stem}.json"));
        write_atomic(&json_path, &serde_json::to_vec_pretty(&doc)?, true)?;
        Ok(json_path)
    }
}

/// Serialize frames into the self-describing container format.
fn container_bytes(frames: &[FrameRecord]) -> Vec<u8> {
    let mut out = Vec::with_capacity(16 + frames.len() * usize::from(AMBE_RECORD_LEN));
    out.extend_from_slice(AMBE_MAGIC);
    out.extend_from_slice(&AMBE_VERSION.to_le_bytes());
    out.extend_from_slice(&AMBE_RECORD_LEN.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes()); // reserved
    for f in frames {
        out.push(f.seq);
        out.extend_from_slice(&f.ambe);
        out.extend_from_slice(&f.slow_data);
    }
    out
}

/// Map a wire callsign to a filename-safe `[A-Z0-9-]+` token.
pub(crate) fn sanitize_callsign(raw: &str) -> String {
    let cleaned: String = raw
        .trim_end()
        .chars()
        .map(|c| match c {
            'A'..='Z' | '0'..='9' => c,
            'a'..='z' => c.to_ascii_uppercase(),
            _ => '-',
        })
        .collect();
    if cleaned.chars().all(|c| c == '-') || cleaned.is_empty() {
        "UNKNOWN".to_string()
    } else {
        cleaned
    }
}

/// Compute the filename stem, appending `-1`, `-2`, … if the `.json`
/// already exists (same second + callsign + stream id).
fn unique_stem(dir: &Path, rec: &CompletedRecording) -> Result<String, WriteError> {
    let callsign = rec.header.as_ref().map_or_else(
        || "UNKNOWN".to_string(),
        |h| sanitize_callsign(&h.my_call.as_str()),
    );
    let base = format!(
        "{}_{}_{:04X}",
        rec.started_at.format("%Y%m%dT%H%M%SZ"),
        callsign,
        rec.stream_id.get()
    );
    if !dir.join(format!("{base}.json")).exists() {
        return Ok(base);
    }
    for i in 1..=99u32 {
        let candidate = format!("{base}-{i}");
        if !dir.join(format!("{candidate}.json")).exists() {
            return Ok(candidate);
        }
    }
    Err(WriteError::NameExhausted(base))
}

/// Write bytes to `<path>.tmp`, optionally fsync, then rename over
/// `path`. A crash leaves at most `.tmp` litter, never a torn file.
pub(crate) fn write_atomic(path: &Path, bytes: &[u8], fsync: bool) -> Result<(), WriteError> {
    let mut tmp_os = path.as_os_str().to_owned();
    tmp_os.push(".tmp");
    let tmp = PathBuf::from(tmp_os);
    {
        let mut f = fs::File::create(&tmp).map_err(io_err("create", &tmp))?;
        f.write_all(bytes).map_err(io_err("write", &tmp))?;
        if fsync {
            f.sync_all().map_err(io_err("fsync", &tmp))?;
        }
    }
    fs::rename(&tmp, path).map_err(io_err("rename", path))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capture::{CompletedRecording, EndReason, FrameRecord, StreamOrigin};
    use chrono::{DateTime, Utc};
    use dstar_gateway_core::{Callsign, DStarHeader, Module, StreamId, Suffix};
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn fixture() -> Option<CompletedRecording> {
        Some(CompletedRecording {
            origin: StreamOrigin {
                reflector: "REF030".to_string(),
                module: Module::C,
                protocol: "dplus",
                host: "ref030.example.org".to_string(),
                port: 20001,
                peer: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 20001),
            },
            stream_id: StreamId::new(0x04D2)?,
            header: Some(DStarHeader {
                flag1: 12,
                flag2: 0,
                flag3: 0,
                rpt2: Callsign::from_wire_bytes(*b"REF030 G"),
                rpt1: Callsign::from_wire_bytes(*b"REF030 C"),
                ur_call: Callsign::from_wire_bytes(*b"CQCQCQ  "),
                my_call: Callsign::from_wire_bytes(*b"W1AW    "),
                my_suffix: Suffix::from_wire_bytes(*b"D75 "),
            }),
            header_diagnostics: vec!["example diagnostic".to_string()],
            started_at: DateTime::<Utc>::UNIX_EPOCH,
            ended_at: DateTime::<Utc>::UNIX_EPOCH + chrono::TimeDelta::milliseconds(100),
            end_reason: EndReason::Eot,
            frames: vec![
                FrameRecord {
                    seq: 0,
                    ambe: [0u8; 9],
                    slow_data: [0u8; 3],
                },
                FrameRecord {
                    seq: 2,
                    ambe: [0u8; 9],
                    slow_data: [0u8; 3],
                },
            ],
            gaps: 1,
            text: Some("Asheville NC".to_string()),
            text_bytes: Some(*b"Asheville NC        "),
            dprs: Vec::new(),
        })
    }

    /// JSON-pointer accessor that fails the test on a missing path
    /// (workspace lints ban `Value` bracket indexing — it may panic).
    fn at<'a>(
        doc: &'a serde_json::Value,
        path: &str,
    ) -> Result<&'a serde_json::Value, Box<dyn std::error::Error>> {
        doc.pointer(path)
            .ok_or_else(|| format!("missing {path}").into())
    }

    #[test]
    fn document_matches_schema() -> TestResult {
        let rec = fixture().ok_or("fixture")?;
        let audio = AudioOutcome {
            wav: true,
            concealed_frames: 1,
            error: None,
        };
        let doc = serde_json::to_value(build_doc(&rec, &audio))?;

        assert_eq!(*at(&doc, "/schema")?, "stargazer-recording/1");
        assert_eq!(*at(&doc, "/reflector")?, "REF030");
        assert_eq!(*at(&doc, "/module")?, "C");
        assert_eq!(*at(&doc, "/protocol")?, "dplus");
        assert_eq!(*at(&doc, "/host")?, "ref030.example.org:20001");
        assert_eq!(*at(&doc, "/stream_id")?, "04D2");
        assert_eq!(*at(&doc, "/started_at")?, "1970-01-01T00:00:00.000Z");
        assert_eq!(*at(&doc, "/end_reason")?, "eot");
        assert_eq!(*at(&doc, "/frames/received")?, 2);
        assert_eq!(*at(&doc, "/frames/expected")?, 3);
        assert_eq!(*at(&doc, "/frames/gaps")?, 1);
        let duration = at(&doc, "/duration_s")?.as_f64().ok_or("duration")?;
        assert!((duration - 0.06).abs() < 1e-9);
        assert_eq!(*at(&doc, "/header/my_callsign")?, "W1AW");
        assert_eq!(*at(&doc, "/header/my_suffix")?, "D75");
        assert_eq!(*at(&doc, "/header/ur_callsign")?, "CQCQCQ");
        assert_eq!(*at(&doc, "/header/rpt1")?, "REF030 C");
        assert_eq!(*at(&doc, "/header/flags/0")?, 12);
        let raw_hex = at(&doc, "/header/raw_hex")?.as_str().ok_or("raw_hex")?;
        assert_eq!(raw_hex.len(), 82);
        assert_eq!(*at(&doc, "/slow_data/text")?, "Asheville NC");
        // All-zero frames pass C0 as the zero codeword but the
        // descrambled C1 needs 2 corrections each (see mbelib-rs
        // frame_fec tests) — the fixture's two frames pin 4 bits.
        assert_eq!(*at(&doc, "/fec/corrected_bits")?, 4);
        assert_eq!(*at(&doc, "/fec/frames_with_errors")?, 2);
        assert_eq!(*at(&doc, "/fec/erasure_frames")?, 0);
        assert_eq!(*at(&doc, "/fec/tone_frames")?, 0);
        assert_eq!(*at(&doc, "/audio/wav")?, true);
        assert_eq!(*at(&doc, "/audio/concealed_frames")?, 1);
        assert!(
            doc.pointer("/audio/error").is_none(),
            "error omitted when None"
        );
        Ok(())
    }

    #[test]
    fn ascii_text_omits_hex_but_katakana_carries_it() -> TestResult {
        let audio = AudioOutcome {
            wav: false,
            concealed_frames: 0,
            error: None,
        };
        // ASCII message → no text_hex.
        let rec = fixture().ok_or("fixture")?;
        let doc = serde_json::to_value(build_doc(&rec, &audio))?;
        assert!(doc.pointer("/slow_data/text_hex").is_none());

        // JIS X 0201 half-width katakana (0xA1..=0xDF) is not UTF-8;
        // the raw bytes must survive as hex.
        let mut rec = fixture().ok_or("fixture")?;
        let mut msg = [b' '; 20];
        msg[0] = 0xC4; // ﾄ
        msg[1] = 0xB6; // ｶ
        msg[2] = b'7';
        msg[3] = b'3';
        rec.text = Some(String::from_utf8_lossy(&msg).trim_end().to_string());
        rec.text_bytes = Some(msg);
        let doc = serde_json::to_value(build_doc(&rec, &audio))?;
        let hex = doc
            .pointer("/slow_data/text_hex")
            .and_then(|v| v.as_str())
            .ok_or("text_hex missing")?;
        assert!(hex.starts_with("c4b63733"), "got {hex}");
        assert_eq!(hex.len(), 40);
        Ok(())
    }

    #[test]
    fn headerless_document_has_null_header() -> TestResult {
        let mut rec = fixture().ok_or("fixture")?;
        rec.header = None;
        let audio = AudioOutcome {
            wav: false,
            concealed_frames: 0,
            error: None,
        };
        let doc = serde_json::to_value(build_doc(&rec, &audio))?;
        assert!(at(&doc, "/header")?.is_null());
        assert_eq!(*at(&doc, "/audio/wav")?, false);
        Ok(())
    }

    // -- persistence tests --

    fn rec_with(frames: Vec<FrameRecord>) -> Option<CompletedRecording> {
        let mut rec = fixture()?;
        rec.frames = frames;
        rec.gaps = 0;
        Some(rec)
    }

    fn le16_at(bytes: &[u8], at: usize) -> Result<u16, Box<dyn std::error::Error>> {
        Ok(u16::from_le_bytes(
            bytes.get(at..at + 2).ok_or("short u16")?.try_into()?,
        ))
    }

    #[test]
    fn writes_three_files_with_spec_layout() -> TestResult {
        let dir = tempfile::tempdir()?;
        let writer = Writer::new(dir.path().to_path_buf(), true);
        let rec = fixture().ok_or("fixture")?;
        let json_path = writer.write(&rec)?;

        let expected_dir = dir.path().join("REF030-C").join("1970-01-01");
        assert_eq!(json_path.parent(), Some(expected_dir.as_path()));
        let stem = "19700101T000000Z_W1AW_04D2";
        assert!(expected_dir.join(format!("{stem}.ambe")).exists());
        assert!(expected_dir.join(format!("{stem}.wav")).exists());
        assert!(expected_dir.join(format!("{stem}.json")).exists());
        // No temp litter after success.
        let names: Vec<String> = fs::read_dir(&expected_dir)?
            .filter_map(Result::ok)
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect();
        assert!(
            names
                .iter()
                .all(|n| Path::new(n).extension() != Some("tmp".as_ref())),
            "{names:?}"
        );
        Ok(())
    }

    #[test]
    fn ambe_container_round_trips_records() -> TestResult {
        let dir = tempfile::tempdir()?;
        let writer = Writer::new(dir.path().to_path_buf(), false);
        let frames = vec![
            FrameRecord {
                seq: 5,
                ambe: [0xAA; 9],
                slow_data: [1, 2, 3],
            },
            FrameRecord {
                seq: 6,
                ambe: [0xBB; 9],
                slow_data: [4, 5, 6],
            },
        ];
        let rec = rec_with(frames).ok_or("fixture")?;
        let json_path = writer.write(&rec)?;
        let ambe_path = json_path.with_extension("ambe");
        let bytes = fs::read(ambe_path)?;
        assert_eq!(bytes.len(), 16 + 2 * 13);
        assert_eq!(bytes.get(..8).ok_or("magic")?, b"STGZAMBE");
        assert_eq!(le16_at(&bytes, 8)?, 1, "version");
        assert_eq!(le16_at(&bytes, 10)?, 13, "record_len");
        assert_eq!(bytes.get(16).copied(), Some(5), "first record seq");
        assert_eq!(bytes.get(17..26).ok_or("ambe1")?, &[0xAA; 9]);
        assert_eq!(bytes.get(26..29).ok_or("slow1")?, &[1, 2, 3]);
        assert_eq!(bytes.get(29).copied(), Some(6), "second record seq");
        Ok(())
    }

    #[test]
    fn no_wav_when_disabled_and_json_says_so() -> TestResult {
        let dir = tempfile::tempdir()?;
        let writer = Writer::new(dir.path().to_path_buf(), false);
        let rec = fixture().ok_or("fixture")?;
        let json_path = writer.write(&rec)?;
        assert!(!json_path.with_extension("wav").exists());
        let doc: serde_json::Value = serde_json::from_slice(&fs::read(&json_path)?)?;
        assert_eq!(*at(&doc, "/audio/wav")?, false);
        Ok(())
    }

    #[test]
    fn filename_collision_appends_suffix() -> TestResult {
        let dir = tempfile::tempdir()?;
        let writer = Writer::new(dir.path().to_path_buf(), false);
        let rec = fixture().ok_or("fixture")?;
        let first = writer.write(&rec)?;
        let second = writer.write(&rec)?;
        assert_ne!(first, second);
        assert!(
            second
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.contains("-1")),
            "got {second:?}"
        );
        Ok(())
    }

    #[test]
    fn headerless_recording_files_use_unknown_callsign() -> TestResult {
        let dir = tempfile::tempdir()?;
        let writer = Writer::new(dir.path().to_path_buf(), false);
        let mut rec = fixture().ok_or("fixture")?;
        rec.header = None;
        let json_path = writer.write(&rec)?;
        assert!(
            json_path
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.contains("_UNKNOWN_")),
            "got {json_path:?}"
        );
        Ok(())
    }

    #[test]
    fn callsign_is_sanitized_for_filenames() {
        assert_eq!(sanitize_callsign("W1AW"), "W1AW");
        assert_eq!(sanitize_callsign("w1aw/p"), "W1AW-P");
        assert_eq!(sanitize_callsign("   "), "UNKNOWN");
    }
}
