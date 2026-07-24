// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later

//! Atomic raw AMBE+2 and metadata persistence.
//!
//! Pulsar deliberately does not decode DMR AMBE+2. Each call is committed as
//! a versioned `.ambe` transport archive followed by a `.json` metadata
//! document. JSON is written last and therefore serves as the commit marker.

use std::fmt::Write as _;
use std::fs;
use std::fs::OpenOptions;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::capture::{
    AudioPacket, CompletedRecording, DmrIdentity, EmbeddedLcRecord, SessionType, TerminatorRecord,
    VoiceHeaderRecord,
};

/// Eight-byte magic at the beginning of every Pulsar AMBE+2 archive.
pub const AMBE_MAGIC: &[u8; 8] = b"PLSRAMBE";
/// Pulsar AMBE+2 container format version.
pub const AMBE_VERSION: u16 = 1;
/// Bytes per audio record in container version 1.
pub const AMBE_RECORD_LEN: u16 = 34;

/// Top-level metadata JSON document (`pulsar-recording/1`).
#[derive(Debug, Serialize)]
pub struct RecordingDoc {
    schema: &'static str,
    writer: String,
    origin: OriginDoc,
    call: Option<CallDoc>,
    started_at: String,
    ended_at: String,
    wall_duration_s: f64,
    end_reason: &'static str,
    audio: AudioDoc,
    voice_headers: Vec<AncillaryDoc>,
    terminator: Option<AncillaryDoc>,
    embedded_lc: Vec<AncillaryDoc>,
    raw: RawDoc,
}

#[derive(Debug, Serialize)]
struct OriginDoc {
    master: String,
    host: String,
    port: u16,
    peer: String,
    dmr_id: u32,
}

#[derive(Debug, Serialize)]
struct CallDoc {
    session_type: &'static str,
    source: IdentityDoc,
    destination: IdentityDoc,
}

#[derive(Debug, Serialize)]
struct IdentityDoc {
    id: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    callsign: Option<String>,
}

#[derive(Debug, Serialize)]
struct AudioDoc {
    packets: u64,
    ambe_frames: u64,
    bytes: u64,
    codec_duration_s: f64,
    first_transport_seq: Option<u32>,
    last_transport_seq: Option<u32>,
}

#[derive(Debug, Serialize)]
struct AncillaryDoc {
    transport_seq: u32,
    flags: u16,
    subtype: u8,
    #[serde(skip_serializing_if = "Option::is_none")]
    data_hex: Option<String>,
}

#[derive(Debug, Serialize)]
struct RawDoc {
    format: &'static str,
    version: u16,
    record_len: u16,
    #[serde(skip_serializing_if = "Option::is_none")]
    filename: Option<String>,
}

/// Recording persistence failure.
#[derive(Debug, thiserror::Error)]
pub enum WriteError {
    /// Filesystem error annotated with its operation and path.
    #[error("{context} {path}: {source}")]
    Io {
        /// Operation that failed.
        context: &'static str,
        /// Path involved.
        path: PathBuf,
        /// Underlying filesystem error.
        #[source]
        source: std::io::Error,
    },
    /// Metadata serialization failed.
    #[error("serialize metadata: {0}")]
    Serialize(#[from] serde_json::Error),
    /// No unused suffix was available for an unlikely filename collision.
    #[error("no free filename for stem {0} after 999 attempts")]
    NameExhausted(String),
}

/// Atomic Pulsar recording writer.
#[derive(Debug, Clone)]
pub struct Writer {
    base: PathBuf,
}

impl Writer {
    /// Create a writer rooted at the configured recordings directory.
    #[must_use]
    pub const fn new(base: PathBuf) -> Self {
        Self { base }
    }

    /// Persist the raw archive first and metadata JSON last.
    ///
    /// Files are grouped as
    /// `<base>/<master>/<destination>/<YYYY-MM-DD>/<stem>.*`. The returned
    /// path is the JSON commit marker.
    ///
    /// # Errors
    ///
    /// Returns [`WriteError`] if a directory cannot be created, a filename
    /// cannot be allocated, bytes cannot be serialized, or either file cannot
    /// be durably written and atomically renamed.
    pub fn write(&self, recording: &CompletedRecording) -> Result<PathBuf, WriteError> {
        let directory = self
            .base
            .join(sanitize_component(&recording.origin.master))
            .join(destination_component(recording))
            .join(recording.started_at.format("%Y-%m-%d").to_string());
        fs::create_dir_all(&directory).map_err(io_error("create directory", &directory))?;

        let stem = unique_stem(&directory, recording)?;
        let ambe_filename = format!("{stem}.ambe");
        let ambe_path = directory.join(&ambe_filename);
        write_atomic(&ambe_path, &container_bytes(&recording.audio_packets))?;

        let json_path = directory.join(format!("{stem}.json"));
        let metadata =
            serde_json::to_vec_pretty(&build_doc_for_file(recording, Some(ambe_filename)))?;
        write_atomic(&json_path, &metadata)?;
        Ok(json_path)
    }
}

/// Build the stable JSON metadata document for a completed recording.
#[must_use]
pub fn build_doc(recording: &CompletedRecording) -> RecordingDoc {
    build_doc_for_file(recording, None)
}

fn build_doc_for_file(
    recording: &CompletedRecording,
    raw_filename: Option<String>,
) -> RecordingDoc {
    let call = recording.header.as_ref().map(|header| CallDoc {
        session_type: header.session_type.as_str(),
        source: identity_doc(&header.source),
        destination: identity_doc(&header.destination),
    });

    RecordingDoc {
        schema: "pulsar-recording/1",
        writer: format!("pulsar/{}", env!("CARGO_PKG_VERSION")),
        origin: OriginDoc {
            master: recording.origin.master.clone(),
            host: recording.origin.host.clone(),
            port: recording.origin.port,
            peer: recording.origin.peer.to_string(),
            dmr_id: recording.origin.dmr_id,
        },
        call,
        started_at: rfc3339_millis(recording.started_at),
        ended_at: rfc3339_millis(recording.ended_at),
        wall_duration_s: recording.wall_duration_s(),
        end_reason: recording.end_reason.as_str(),
        audio: AudioDoc {
            packets: recording.packet_count(),
            ambe_frames: recording.ambe_frame_count(),
            bytes: recording.audio_byte_count(),
            codec_duration_s: recording.codec_duration_s(),
            first_transport_seq: recording.first_transport_seq(),
            last_transport_seq: recording.last_transport_seq(),
        },
        voice_headers: recording
            .voice_headers
            .iter()
            .map(voice_header_doc)
            .collect(),
        terminator: recording.terminator.as_ref().map(terminator_doc),
        embedded_lc: recording.embedded_lc.iter().map(embedded_lc_doc).collect(),
        raw: RawDoc {
            format: "PLSRAMBE",
            version: AMBE_VERSION,
            record_len: AMBE_RECORD_LEN,
            filename: raw_filename,
        },
    }
}

/// Encode version-1 Pulsar raw-container bytes.
#[must_use]
pub fn container_bytes(packets: &[AudioPacket]) -> Vec<u8> {
    let record_len = usize::from(AMBE_RECORD_LEN);
    let mut bytes =
        Vec::with_capacity(16usize.saturating_add(packets.len().saturating_mul(record_len)));
    bytes.extend_from_slice(AMBE_MAGIC);
    bytes.extend_from_slice(&AMBE_VERSION.to_le_bytes());
    bytes.extend_from_slice(&AMBE_RECORD_LEN.to_le_bytes());
    bytes.extend_from_slice(&0u32.to_le_bytes());
    for packet in packets {
        bytes.extend_from_slice(&packet.transport_seq.to_le_bytes());
        bytes.extend_from_slice(&packet.flags.to_le_bytes());
        bytes.push(packet.subtype);
        bytes.extend_from_slice(&packet.ambe);
    }
    bytes
}

fn identity_doc(identity: &DmrIdentity) -> IdentityDoc {
    let callsign = identity.callsign.trim();
    IdentityDoc {
        id: identity.id,
        callsign: if callsign.is_empty() {
            None
        } else {
            Some(callsign.to_string())
        },
    }
}

fn voice_header_doc(record: &VoiceHeaderRecord) -> AncillaryDoc {
    AncillaryDoc {
        transport_seq: record.transport_seq,
        flags: record.flags,
        subtype: record.subtype,
        data_hex: Some(hex_lower(&record.data)),
    }
}

fn terminator_doc(record: &TerminatorRecord) -> AncillaryDoc {
    AncillaryDoc {
        transport_seq: record.transport_seq,
        flags: record.flags,
        subtype: record.subtype,
        data_hex: record.data.as_ref().map(|data| hex_lower(data)),
    }
}

fn embedded_lc_doc(record: &EmbeddedLcRecord) -> AncillaryDoc {
    AncillaryDoc {
        transport_seq: record.transport_seq,
        flags: record.flags,
        subtype: record.subtype,
        data_hex: Some(hex_lower(&record.data)),
    }
}

fn rfc3339_millis(time: chrono::DateTime<chrono::Utc>) -> String {
    time.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

fn hex_lower(bytes: &[u8]) -> String {
    bytes.iter().fold(
        String::with_capacity(bytes.len() * 2),
        |mut output, byte| {
            let _result = write!(output, "{byte:02x}");
            output
        },
    )
}

fn destination_component(recording: &CompletedRecording) -> String {
    let Some(header) = &recording.header else {
        return "UNKNOWN".to_string();
    };
    match header.session_type {
        SessionType::Group => format!("GROUP-{}", header.destination.id),
        SessionType::Private => format!("PRIVATE-{}", header.destination.id),
    }
}

fn unique_stem(directory: &Path, recording: &CompletedRecording) -> Result<String, WriteError> {
    let timestamp = recording.started_at.format("%Y%m%dT%H%M%S%3fZ");
    let (source, destination) = recording.header.as_ref().map_or_else(
        || ("UNKNOWN".to_string(), "UNKNOWN".to_string()),
        |header| {
            let source_call = sanitize_component(&header.source.callsign);
            let source = if source_call == "UNKNOWN" {
                format!("ID{}", header.source.id)
            } else {
                format!("{source_call}-{}", header.source.id)
            };
            let destination = match header.session_type {
                SessionType::Group => format!("TG{}", header.destination.id),
                SessionType::Private => format!("ID{}", header.destination.id),
            };
            (source, destination)
        },
    );
    let sequence = recording
        .first_transport_seq()
        .map_or_else(|| "NOSEQ".to_string(), |value| format!("{value:08X}"));
    let base = format!("{timestamp}_{source}_{destination}_{sequence}");
    if stem_available(directory, &base) {
        return Ok(base);
    }
    for suffix in 1..=999u16 {
        let candidate = format!("{base}-{suffix}");
        if stem_available(directory, &candidate) {
            return Ok(candidate);
        }
    }
    Err(WriteError::NameExhausted(base))
}

fn stem_available(directory: &Path, stem: &str) -> bool {
    !directory.join(format!("{stem}.json")).exists()
        && !directory.join(format!("{stem}.json.tmp")).exists()
        && !directory.join(format!("{stem}.ambe")).exists()
        && !directory.join(format!("{stem}.ambe.tmp")).exists()
}

/// Map untrusted text to a safe, nonempty path component.
fn sanitize_component(raw: &str) -> String {
    let sanitized: String = raw
        .trim()
        .chars()
        .map(|character| match character {
            'A'..='Z' | '0'..='9' => character,
            'a'..='z' => character.to_ascii_uppercase(),
            _ => '-',
        })
        .collect();
    if sanitized.is_empty() || sanitized.chars().all(|character| character == '-') {
        "UNKNOWN".to_string()
    } else {
        sanitized
    }
}

fn io_error(context: &'static str, path: &Path) -> impl FnOnce(std::io::Error) -> WriteError {
    let path = path.to_path_buf();
    move |source| WriteError::Io {
        context,
        path,
        source,
    }
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), WriteError> {
    let mut temporary_name = path.as_os_str().to_owned();
    temporary_name.push(".tmp");
    let temporary_path = PathBuf::from(temporary_name);
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary_path)
        .map_err(io_error("create", &temporary_path))?;
    let staged = file
        .write_all(bytes)
        .map_err(io_error("write", &temporary_path))
        .and_then(|()| file.sync_all().map_err(io_error("sync", &temporary_path)));
    drop(file);
    if let Err(error) = staged {
        let _cleanup_result = fs::remove_file(&temporary_path);
        return Err(error);
    }
    if let Err(source) = fs::rename(&temporary_path, path) {
        let _cleanup_result = fs::remove_file(&temporary_path);
        return Err(io_error("rename", path)(source));
    }
    #[cfg(unix)]
    sync_parent_directory(path)?;
    Ok(())
}

#[cfg(unix)]
fn sync_parent_directory(path: &Path) -> Result<(), WriteError> {
    let directory = path.parent().ok_or_else(|| WriteError::Io {
        context: "locate parent directory for",
        path: path.to_path_buf(),
        source: std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "output path has no parent directory",
        ),
    })?;
    let handle = fs::File::open(directory).map_err(io_error("open directory", directory))?;
    handle
        .sync_all()
        .map_err(io_error("sync directory", directory))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capture::{
        AudioPacket, CompletedRecording, DmrIdentity, EmbeddedLcRecord, EndReason, StreamOrigin,
        SuperHeader, TerminatorRecord, VoiceHeaderRecord,
    };
    use chrono::{DateTime, Utc};
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn fixture() -> CompletedRecording {
        CompletedRecording {
            origin: StreamOrigin {
                master: "BM / US".to_string(),
                host: "3103.master.brandmeister.network".to_string(),
                port: 54_006,
                peer: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 54_006),
                dmr_id: 3_101_234,
            },
            header: Some(SuperHeader {
                session_type: SessionType::Group,
                source: DmrIdentity {
                    id: 310_123,
                    callsign: "w1aw".to_string(),
                },
                destination: DmrIdentity {
                    id: 91,
                    callsign: "World-Wide".to_string(),
                },
            }),
            started_at: DateTime::<Utc>::UNIX_EPOCH,
            ended_at: DateTime::<Utc>::UNIX_EPOCH + chrono::TimeDelta::milliseconds(125),
            end_reason: EndReason::Terminator,
            audio_packets: vec![
                AudioPacket {
                    transport_seq: 0x0102_0304,
                    flags: 0x0506,
                    subtype: 6,
                    ambe: [0xA8; 27],
                },
                AudioPacket {
                    transport_seq: 0x1112_1314,
                    flags: 0x1516,
                    subtype: 5,
                    ambe: [0xB8; 27],
                },
            ],
            voice_headers: vec![VoiceHeaderRecord {
                transport_seq: 8,
                flags: 9,
                subtype: 3,
                data: [0xAB; 12],
            }],
            terminator: Some(TerminatorRecord {
                transport_seq: 13,
                flags: 14,
                subtype: 5,
                data: Some([0xEF; 12]),
            }),
            embedded_lc: vec![EmbeddedLcRecord {
                transport_seq: 11,
                flags: 12,
                subtype: 4,
                data: [0xCD; 10],
            }],
        }
    }

    fn at<'a>(
        document: &'a serde_json::Value,
        pointer: &str,
    ) -> Result<&'a serde_json::Value, Box<dyn std::error::Error>> {
        document
            .pointer(pointer)
            .ok_or_else(|| format!("missing {pointer}").into())
    }

    fn le16_at(bytes: &[u8], offset: usize) -> Result<u16, Box<dyn std::error::Error>> {
        Ok(u16::from_le_bytes(
            bytes
                .get(offset..offset.saturating_add(2))
                .ok_or("short u16")?
                .try_into()?,
        ))
    }

    fn le32_at(bytes: &[u8], offset: usize) -> Result<u32, Box<dyn std::error::Error>> {
        Ok(u32::from_le_bytes(
            bytes
                .get(offset..offset.saturating_add(4))
                .ok_or("short u32")?
                .try_into()?,
        ))
    }

    #[test]
    fn metadata_contains_capture_ground_truth() -> TestResult {
        let document = serde_json::to_value(build_doc(&fixture()))?;
        assert_eq!(*at(&document, "/schema")?, "pulsar-recording/1");
        assert_eq!(*at(&document, "/origin/master")?, "BM / US");
        assert_eq!(
            *at(&document, "/origin/host")?,
            "3103.master.brandmeister.network"
        );
        assert_eq!(*at(&document, "/origin/port")?, 54_006);
        assert_eq!(*at(&document, "/origin/peer")?, "127.0.0.1:54006");
        assert_eq!(*at(&document, "/origin/dmr_id")?, 3_101_234);
        assert!(document.pointer("/origin/app_id").is_none());
        assert_eq!(*at(&document, "/call/session_type")?, "group");
        assert_eq!(*at(&document, "/call/source/id")?, 310_123);
        assert_eq!(*at(&document, "/call/source/callsign")?, "w1aw");
        assert_eq!(*at(&document, "/call/destination/id")?, 91);
        assert_eq!(*at(&document, "/started_at")?, "1970-01-01T00:00:00.000Z");
        assert_eq!(*at(&document, "/ended_at")?, "1970-01-01T00:00:00.125Z");
        assert_eq!(*at(&document, "/end_reason")?, "terminator");
        assert_eq!(*at(&document, "/audio/packets")?, 2);
        assert_eq!(*at(&document, "/audio/ambe_frames")?, 6);
        assert_eq!(*at(&document, "/audio/bytes")?, 54);
        assert_eq!(*at(&document, "/audio/first_transport_seq")?, 0x0102_0304);
        assert_eq!(*at(&document, "/audio/last_transport_seq")?, 0x1112_1314);
        assert_eq!(*at(&document, "/voice_headers/0/transport_seq")?, 8);
        assert_eq!(
            *at(&document, "/voice_headers/0/data_hex")?,
            "abababababababababababab"
        );
        assert_eq!(*at(&document, "/terminator/transport_seq")?, 13);
        assert_eq!(
            *at(&document, "/terminator/data_hex")?,
            "efefefefefefefefefefefef"
        );
        assert_eq!(
            *at(&document, "/embedded_lc/0/data_hex")?,
            "cdcdcdcdcdcdcdcdcdcd"
        );
        assert_eq!(*at(&document, "/raw/format")?, "PLSRAMBE");
        assert_eq!(*at(&document, "/raw/version")?, 1);
        assert_eq!(*at(&document, "/raw/record_len")?, 34);
        Ok(())
    }

    #[test]
    fn container_uses_specified_header_and_record_layout() -> TestResult {
        let bytes = container_bytes(&fixture().audio_packets);
        assert_eq!(bytes.len(), 16 + 2 * 34);
        assert_eq!(bytes.get(..8).ok_or("missing magic")?, b"PLSRAMBE");
        assert_eq!(le16_at(&bytes, 8)?, 1);
        assert_eq!(le16_at(&bytes, 10)?, 34);
        assert_eq!(le32_at(&bytes, 12)?, 0);
        assert_eq!(le32_at(&bytes, 16)?, 0x0102_0304);
        assert_eq!(le16_at(&bytes, 20)?, 0x0506);
        assert_eq!(bytes.get(22).copied(), Some(6));
        assert_eq!(bytes.get(23..50).ok_or("first AMBE payload")?, &[0xA8; 27]);
        assert_eq!(le32_at(&bytes, 50)?, 0x1112_1314);
        Ok(())
    }

    #[test]
    fn writer_groups_paths_and_commits_two_files() -> TestResult {
        let temporary = tempfile::tempdir()?;
        let writer = Writer::new(temporary.path().to_path_buf());
        let path = writer.write(&fixture())?;
        let expected = temporary
            .path()
            .join("BM---US")
            .join("GROUP-91")
            .join("1970-01-01");
        assert_eq!(path.parent(), Some(expected.as_path()));
        assert!(path.exists());
        assert!(path.with_extension("ambe").exists());
        assert!(!path.with_extension("wav").exists());
        let entries: Vec<PathBuf> = fs::read_dir(expected)?
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .collect();
        assert_eq!(entries.len(), 2, "{entries:?}");
        assert!(
            entries
                .iter()
                .all(|entry| entry.extension() != Some("tmp".as_ref())),
            "{entries:?}"
        );
        Ok(())
    }

    #[test]
    fn collision_gets_a_distinct_commit_marker() -> TestResult {
        let temporary = tempfile::tempdir()?;
        let writer = Writer::new(temporary.path().to_path_buf());
        let first = writer.write(&fixture())?;
        let second = writer.write(&fixture())?;
        assert_ne!(first, second);
        assert!(
            second
                .file_stem()
                .and_then(|stem| stem.to_str())
                .is_some_and(|stem| stem.ends_with("-1")),
            "got {second:?}"
        );
        assert!(second.with_extension("ambe").exists());
        Ok(())
    }

    #[test]
    fn orphan_raw_file_is_never_overwritten() -> TestResult {
        let temporary = tempfile::tempdir()?;
        let writer = Writer::new(temporary.path().to_path_buf());
        let orphan_json = writer.write(&fixture())?;
        let orphan_raw = orphan_json.with_extension("ambe");
        let original_raw = fs::read(&orphan_raw)?;
        fs::remove_file(&orphan_json)?;

        let committed = writer.write(&fixture())?;
        assert_ne!(committed.with_extension("ambe"), orphan_raw);
        assert!(
            committed
                .file_stem()
                .and_then(|stem| stem.to_str())
                .is_some_and(|stem| stem.ends_with("-1")),
            "got {committed:?}"
        );
        assert_eq!(fs::read(orphan_raw)?, original_raw);

        let document: serde_json::Value = serde_json::from_slice(&fs::read(&committed)?)?;
        let raw_path = committed.with_extension("ambe");
        let expected_filename = raw_path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or("raw filename")?;
        assert_eq!(*at(&document, "/raw/filename")?, expected_filename);
        Ok(())
    }

    #[test]
    fn headerless_call_uses_unknown_destination_and_null_call() -> TestResult {
        let temporary = tempfile::tempdir()?;
        let writer = Writer::new(temporary.path().to_path_buf());
        let mut recording = fixture();
        recording.header = None;
        recording.audio_packets.clear();
        recording.end_reason = EndReason::Inactivity;
        recording.terminator = None;
        let path = writer.write(&recording)?;
        assert!(
            path.components()
                .any(|component| component.as_os_str() == "UNKNOWN"),
            "got {path:?}"
        );
        let document: serde_json::Value = serde_json::from_slice(&fs::read(path)?)?;
        assert!(at(&document, "/call")?.is_null());
        assert!(at(&document, "/audio/first_transport_seq")?.is_null());
        assert_eq!(*at(&document, "/audio/packets")?, 0);
        assert!(at(&document, "/terminator")?.is_null());
        Ok(())
    }

    #[test]
    fn path_components_are_sanitized() {
        assert_eq!(sanitize_component("BM-US"), "BM-US");
        assert_eq!(sanitize_component("bm / us"), "BM---US");
        assert_eq!(sanitize_component("../"), "UNKNOWN");
        assert_eq!(sanitize_component(""), "UNKNOWN");
    }
}
