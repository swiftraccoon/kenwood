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

use dmr_rewind_core::{Callsign, TransportSequence};
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
    #[serde(skip_serializing_if = "Option::is_none")]
    callsign_wire_hex: Option<String>,
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
    #[error("{operation:?} {path}: {source}")]
    Io {
        /// Operation that failed.
        operation: WriteOperation,
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

/// Filesystem stage whose failure is retained by [`WriteError::Io`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteOperation {
    /// Create one missing output directory.
    CreateDirectory,
    /// Synchronize a created directory and its parent entry on Unix.
    SyncHierarchy,
    /// Atomically reserve a complete raw/JSON stem.
    ReserveName,
    /// Create the raw temporary file without replacing any file.
    CreateRaw,
    /// Write raw bytes.
    WriteRaw,
    /// Synchronize raw bytes.
    SyncRaw,
    /// Link the raw temporary file to its final name without replacement.
    InstallRaw,
    /// Remove the raw temporary link after installation.
    RemoveRawTemporary,
    /// Synchronize the raw directory entry on Unix.
    SyncRawDirectory,
    /// Create the JSON temporary file without replacement.
    CreateMetadata,
    /// Write JSON bytes.
    WriteMetadata,
    /// Synchronize JSON bytes.
    SyncMetadata,
    /// Link JSON to its final commit-marker name without replacement.
    InstallMetadata,
    /// Remove the JSON temporary link after installation.
    RemoveMetadataTemporary,
    /// Synchronize the JSON directory entry on Unix.
    SyncMetadataDirectory,
    /// Remove the transaction's reservation file.
    ReleaseReservation,
}

/// Concurrent, non-overwriting Pulsar recording writer.
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
    /// Returns [`WriteError`] for serialization or a named filesystem stage.
    /// Independent writers/processes reserve an entire stem with `create_new`.
    /// Final installation uses hard links, which fail rather than replacing an
    /// existing destination. The filesystem must support same-directory hard links.
    ///
    /// This blocking operation allocates the complete raw and JSON byte buffers.
    /// On Unix, newly created directories and their parent entries are synced
    /// before installation; each final file entry is synced before proceeding.
    /// Other platforms synchronize file contents but make no directory-durability
    /// promise. An error after JSON installation may leave a visible complete
    /// pair, including a directory-sync failure. Readers check for the JSON
    /// marker; callers must not retry by overwriting that stem. A crash can leave
    /// `.reserve`, `.tmp` or raw-only files, which later writes skip.
    pub fn write(&self, recording: &CompletedRecording) -> Result<PathBuf, WriteError> {
        self.write_with(recording, &mut |_, _| Ok(()))
    }

    fn write_with(
        &self,
        recording: &CompletedRecording,
        observe: &mut impl FnMut(WriteOperation, &Path) -> std::io::Result<()>,
    ) -> Result<PathBuf, WriteError> {
        let directory = self
            .base
            .join(sanitize_component(&recording.origin.master))
            .join(destination_component(recording))
            .join(recording.started_at.format("%Y-%m-%d").to_string());
        create_directories(&directory, observe)?;

        let reservation = reserve_stem(&directory, recording, observe)?;
        let stem = &reservation.stem;
        let ambe_filename = format!("{stem}.ambe");
        let ambe_path = directory.join(&ambe_filename);
        let json_path = directory.join(format!("{stem}.json"));
        let result = (|| {
            write_atomic(
                &ambe_path,
                &container_bytes(&recording.audio_packets),
                FileKind::Raw,
                observe,
            )?;

            let metadata =
                serde_json::to_vec_pretty(&RecordingDoc::for_file(recording, Some(ambe_filename)))?;
            write_atomic(&json_path, &metadata, FileKind::Metadata, observe)?;
            Ok(())
        })();
        let release = reservation.release(observe);
        result.and(release)?;
        Ok(json_path)
    }
}

impl RecordingDoc {
    /// Project immutable capture state to the stable `pulsar-recording/1` JSON view.
    ///
    /// The standalone view omits a raw filename; [`Writer::write`] adds the
    /// reserved raw filename while committing the recording.
    #[must_use]
    pub fn new(recording: &CompletedRecording) -> Self {
        Self::for_file(recording, None)
    }

    fn for_file(recording: &CompletedRecording, raw_filename: Option<String>) -> Self {
        let call = recording.header.as_ref().map(|header| CallDoc {
            session_type: header.session_type.as_str(),
            source: identity_doc(&header.source),
            destination: identity_doc(&header.destination),
        });

        Self {
            schema: "pulsar-recording/1",
            writer: format!("pulsar/{}", env!("CARGO_PKG_VERSION")),
            origin: OriginDoc {
                master: recording.origin.master.clone(),
                host: recording.origin.host.clone(),
                port: recording.origin.port.get(),
                peer: recording.origin.peer.to_string(),
                dmr_id: recording.origin.dmr_id.get(),
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
                first_transport_seq: recording.first_transport_seq().map(TransportSequence::get),
                last_transport_seq: recording.last_transport_seq().map(TransportSequence::get),
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
}

impl From<&CompletedRecording> for RecordingDoc {
    fn from(value: &CompletedRecording) -> Self {
        Self::new(value)
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
        bytes.extend_from_slice(&packet.transport_seq.get().to_le_bytes());
        bytes.extend_from_slice(&packet.flags.bits().to_le_bytes());
        bytes.push(packet.subtype.get());
        bytes.extend_from_slice(&packet.ambe);
    }
    bytes
}

fn identity_doc(identity: &DmrIdentity) -> IdentityDoc {
    let callsign = identity
        .callsign
        .map(Callsign::to_trimmed_lossy)
        .filter(|value| !value.is_empty());
    IdentityDoc {
        id: identity.id.get(),
        callsign,
        callsign_wire_hex: identity.callsign.map(|value| hex_lower(&value.to_bytes())),
    }
}

fn voice_header_doc(record: &VoiceHeaderRecord) -> AncillaryDoc {
    AncillaryDoc {
        transport_seq: record.transport_seq.get(),
        flags: record.flags.bits(),
        subtype: 1,
        data_hex: Some(hex_lower(&record.data)),
    }
}

fn terminator_doc(record: &TerminatorRecord) -> AncillaryDoc {
    AncillaryDoc {
        transport_seq: record.transport_seq.get(),
        flags: record.flags.bits(),
        subtype: 2,
        data_hex: record.data.as_ref().map(|data| hex_lower(data)),
    }
}

fn embedded_lc_doc(record: &EmbeddedLcRecord) -> AncillaryDoc {
    AncillaryDoc {
        transport_seq: record.transport_seq.get(),
        flags: record.flags.bits(),
        subtype: 7,
        data_hex: Some(hex_lower(record.data())),
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

fn recording_stem(recording: &CompletedRecording) -> String {
    let timestamp = recording.started_at.format("%Y%m%dT%H%M%S%3fZ");
    let (source, destination) = recording.header.as_ref().map_or_else(
        || ("UNKNOWN".to_string(), "UNKNOWN".to_string()),
        |header| {
            let source_call = sanitize_component(
                &header
                    .source
                    .callsign
                    .map(Callsign::to_trimmed_lossy)
                    .unwrap_or_default(),
            );
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
    format!("{timestamp}_{source}_{destination}_{sequence}")
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

fn io_error(operation: WriteOperation, path: &Path) -> impl FnOnce(std::io::Error) -> WriteError {
    let path = path.to_path_buf();
    move |source| WriteError::Io {
        operation,
        path,
        source,
    }
}

#[derive(Debug, Clone, Copy)]
enum FileKind {
    Raw,
    Metadata,
}

fn write_atomic(
    path: &Path,
    bytes: &[u8],
    kind: FileKind,
    observe: &mut impl FnMut(WriteOperation, &Path) -> std::io::Result<()>,
) -> Result<(), WriteError> {
    let (create, write, sync, install, remove) = match kind {
        FileKind::Raw => (
            WriteOperation::CreateRaw,
            WriteOperation::WriteRaw,
            WriteOperation::SyncRaw,
            WriteOperation::InstallRaw,
            WriteOperation::RemoveRawTemporary,
        ),
        FileKind::Metadata => (
            WriteOperation::CreateMetadata,
            WriteOperation::WriteMetadata,
            WriteOperation::SyncMetadata,
            WriteOperation::InstallMetadata,
            WriteOperation::RemoveMetadataTemporary,
        ),
    };
    let mut temporary_name = path.as_os_str().to_owned();
    temporary_name.push(".tmp");
    let temporary_path = PathBuf::from(temporary_name);
    observe(create, &temporary_path).map_err(io_error(create, &temporary_path))?;
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary_path)
        .map_err(io_error(create, &temporary_path))?;
    let staged = (|| {
        observe(write, &temporary_path).map_err(io_error(write, &temporary_path))?;
        file.write_all(bytes)
            .map_err(io_error(write, &temporary_path))?;
        observe(sync, &temporary_path).map_err(io_error(sync, &temporary_path))?;
        file.sync_all().map_err(io_error(sync, &temporary_path))
    })();
    drop(file);
    if let Err(error) = staged {
        let _cleanup_result = fs::remove_file(&temporary_path);
        return Err(error);
    }
    let installed = observe(install, path).and_then(|()| fs::hard_link(&temporary_path, path));
    if let Err(source) = installed {
        let _cleanup_result = fs::remove_file(&temporary_path);
        return Err(io_error(install, path)(source));
    }
    observe(remove, &temporary_path).map_err(io_error(remove, &temporary_path))?;
    fs::remove_file(&temporary_path).map_err(io_error(remove, &temporary_path))?;
    #[cfg(unix)]
    {
        let operation = match kind {
            FileKind::Raw => WriteOperation::SyncRawDirectory,
            FileKind::Metadata => WriteOperation::SyncMetadataDirectory,
        };
        sync_directory(
            path.parent().unwrap_or_else(|| Path::new(".")),
            operation,
            observe,
        )?;
    }
    Ok(())
}

#[cfg(unix)]
fn sync_directory(
    directory: &Path,
    operation: WriteOperation,
    observe: &mut impl FnMut(WriteOperation, &Path) -> std::io::Result<()>,
) -> Result<(), WriteError> {
    let directory = if directory.as_os_str().is_empty() {
        Path::new(".")
    } else {
        directory
    };
    observe(operation, directory).map_err(io_error(operation, directory))?;
    fs::File::open(directory)
        .and_then(|handle| handle.sync_all())
        .map_err(io_error(operation, directory))
}

fn create_directories(
    directory: &Path,
    observe: &mut impl FnMut(WriteOperation, &Path) -> std::io::Result<()>,
) -> Result<(), WriteError> {
    let mut missing = Vec::new();
    let mut ancestor = directory;
    while !ancestor.as_os_str().is_empty() && !ancestor.is_dir() {
        missing.push(ancestor.to_path_buf());
        ancestor = ancestor.parent().unwrap_or_else(|| Path::new(""));
    }
    for path in missing.into_iter().rev() {
        observe(WriteOperation::CreateDirectory, &path)
            .map_err(io_error(WriteOperation::CreateDirectory, &path))?;
        match fs::create_dir(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists && path.is_dir() => {}
            Err(source) => return Err(io_error(WriteOperation::CreateDirectory, &path)(source)),
        }
    }
    #[cfg(unix)]
    for path in directory.ancestors() {
        sync_directory(path, WriteOperation::SyncHierarchy, observe)?;
    }
    Ok(())
}

struct Reservation {
    stem: String,
    path: PathBuf,
    handle: Option<fs::File>,
}

impl Reservation {
    fn release(
        mut self,
        observe: &mut impl FnMut(WriteOperation, &Path) -> std::io::Result<()>,
    ) -> Result<(), WriteError> {
        drop(self.handle.take());
        observe(WriteOperation::ReleaseReservation, &self.path)
            .map_err(io_error(WriteOperation::ReleaseReservation, &self.path))?;
        fs::remove_file(&self.path)
            .map_err(io_error(WriteOperation::ReleaseReservation, &self.path))
    }
}

fn reserve_stem(
    directory: &Path,
    recording: &CompletedRecording,
    observe: &mut impl FnMut(WriteOperation, &Path) -> std::io::Result<()>,
) -> Result<Reservation, WriteError> {
    let base = recording_stem(recording);
    for suffix in 0..=999_u16 {
        let stem = if suffix == 0 {
            base.clone()
        } else {
            format!("{base}-{suffix}")
        };
        let path = directory.join(format!("{stem}.reserve"));
        observe(WriteOperation::ReserveName, &path)
            .map_err(io_error(WriteOperation::ReserveName, &path))?;
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(handle) => {
                let reservation = Reservation {
                    stem,
                    path,
                    handle: Some(handle),
                };
                if stem_available(directory, &reservation.stem) {
                    return Ok(reservation);
                }
                reservation.release(observe)?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(source) => return Err(io_error(WriteOperation::ReserveName, &path)(source)),
        }
    }
    Err(WriteError::NameExhausted(base))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capture::{
        AudioPacket, CompletedRecording, DmrIdentity, EmbeddedLcRecord, EndReason, StreamOrigin,
        SuperHeader, TerminatorRecord, VoiceHeaderRecord,
    };
    use chrono::{DateTime, Utc};
    use dmr_rewind_core::{Callsign, WireDmrId};
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn fixture() -> Result<CompletedRecording, Box<dyn std::error::Error>> {
        Ok(CompletedRecording {
            origin: StreamOrigin {
                master: "BM / US".to_string(),
                host: "3103.master.brandmeister.network".to_string(),
                port: const { std::num::NonZeroU16::new(54_006).unwrap() },
                peer: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 54_006),
                dmr_id: dmr_rewind::SubscriberId::new(3_101_234)?,
            },
            header: Some(SuperHeader {
                session_type: SessionType::Group,
                source: DmrIdentity {
                    id: WireDmrId::new(310_123),
                    callsign: Some(Callsign::from_bytes(*b"w1aw      ")),
                },
                destination: DmrIdentity {
                    id: WireDmrId::new(91),
                    callsign: Some(Callsign::from_bytes(*b"World-Wide")),
                },
            }),
            started_at: DateTime::<Utc>::UNIX_EPOCH,
            ended_at: DateTime::<Utc>::UNIX_EPOCH + chrono::TimeDelta::milliseconds(125),
            end_reason: EndReason::Terminator,
            audio_packets: vec![
                AudioPacket {
                    transport_seq: 0x0102_0304.into(),
                    flags: dmr_rewind_core::PacketFlags::from_bits(0x0506),
                    subtype: dmr_rewind_core::DmrAudioSubtype::new(6)?,
                    ambe: [0xA8; 27],
                },
                AudioPacket {
                    transport_seq: 0x1112_1314.into(),
                    flags: dmr_rewind_core::PacketFlags::from_bits(0x1516),
                    subtype: dmr_rewind_core::DmrAudioSubtype::new(5)?,
                    ambe: [0xB8; 27],
                },
            ],
            voice_headers: vec![VoiceHeaderRecord {
                transport_seq: 8.into(),
                flags: dmr_rewind_core::PacketFlags::from_bits(9),
                data: [0xAB; 12],
            }],
            terminator: Some(TerminatorRecord {
                transport_seq: 13.into(),
                flags: dmr_rewind_core::PacketFlags::from_bits(14),
                data: Some([0xEF; 12]),
            }),
            embedded_lc: vec![EmbeddedLcRecord::new(
                11.into(),
                dmr_rewind_core::PacketFlags::from_bits(12),
                vec![0xCD; 11],
            )?],
        })
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
        let document = serde_json::to_value(RecordingDoc::new(&fixture()?))?;
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
            "cdcdcdcdcdcdcdcdcdcdcd"
        );
        assert_eq!(*at(&document, "/raw/format")?, "PLSRAMBE");
        assert_eq!(*at(&document, "/raw/version")?, 1);
        assert_eq!(*at(&document, "/raw/record_len")?, 34);
        Ok(())
    }

    #[test]
    fn container_uses_specified_header_and_record_layout() -> TestResult {
        let bytes = container_bytes(&fixture()?.audio_packets);
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
        let path = writer.write(&fixture()?)?;
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
        let first = writer.write(&fixture()?)?;
        let second = writer.write(&fixture()?)?;
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
    fn failed_write_explicitly_releases_its_reservation() -> TestResult {
        let temporary = tempfile::tempdir()?;
        let writer = Writer::new(temporary.path().to_path_buf());
        let mut directory = None;
        let result = writer.write_with(&fixture()?, &mut |operation, path| {
            if operation == WriteOperation::WriteRaw {
                directory = path.parent().map(Path::to_path_buf);
                return Err(std::io::Error::other("injected write failure"));
            }
            Ok(())
        });
        assert!(result.is_err());
        for entry in fs::read_dir(directory.ok_or("raw stage not reached")?)? {
            assert_ne!(entry?.path().extension(), Some("reserve".as_ref()));
        }
        Ok(())
    }

    #[test]
    fn orphan_raw_file_is_never_overwritten() -> TestResult {
        let temporary = tempfile::tempdir()?;
        let writer = Writer::new(temporary.path().to_path_buf());
        let orphan_json = writer.write(&fixture()?)?;
        let orphan_raw = orphan_json.with_extension("ambe");
        let original_raw = fs::read(&orphan_raw)?;
        fs::remove_file(&orphan_json)?;

        let committed = writer.write(&fixture()?)?;
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
        let mut recording = fixture()?;
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

    #[test]
    fn concurrent_writers_reserve_distinct_complete_pairs() -> TestResult {
        let temporary = tempfile::tempdir()?;
        let first_writer = Writer::new(temporary.path().to_path_buf());
        let second_writer = Writer::new(temporary.path().to_path_buf());
        let first = fixture()?;
        let mut second = fixture()?;
        second
            .audio_packets
            .first_mut()
            .ok_or("missing audio")?
            .ambe = [0x55; 27];
        let barrier = std::sync::Barrier::new(2);
        let observer = |operation, _: &Path| {
            if matches!(
                operation,
                WriteOperation::InstallRaw | WriteOperation::InstallMetadata
            ) {
                let _wait = barrier.wait();
            }
            Ok(())
        };
        let (left, right) = std::thread::scope(|scope| {
            let left = scope.spawn(|| {
                let mut observe = observer;
                first_writer.write_with(&first, &mut observe)
            });
            let right = scope.spawn(|| {
                let mut observe = observer;
                second_writer.write_with(&second, &mut observe)
            });
            (left.join(), right.join())
        });
        let left = left.map_err(|_| "first writer panicked")??;
        let right = right.map_err(|_| "second writer panicked")??;
        assert_ne!(left, right);
        assert_eq!(
            fs::read(left.with_extension("ambe"))?,
            container_bytes(&first.audio_packets)
        );
        assert_eq!(
            fs::read(right.with_extension("ambe"))?,
            container_bytes(&second.audio_packets)
        );
        for path in [&left, &right] {
            let document: serde_json::Value = serde_json::from_slice(&fs::read(path)?)?;
            assert_eq!(
                *at(&document, "/raw/filename")?,
                path.with_extension("ambe")
                    .file_name()
                    .and_then(|value| value.to_str())
                    .ok_or("raw filename")?
            );
        }
        Ok(())
    }

    #[test]
    fn filesystem_failures_report_stage_and_preserve_completion_boundary() -> TestResult {
        let operations = [
            WriteOperation::CreateDirectory,
            WriteOperation::ReserveName,
            WriteOperation::CreateRaw,
            WriteOperation::WriteRaw,
            WriteOperation::SyncRaw,
            WriteOperation::InstallRaw,
            WriteOperation::RemoveRawTemporary,
            WriteOperation::CreateMetadata,
            WriteOperation::WriteMetadata,
            WriteOperation::SyncMetadata,
            WriteOperation::InstallMetadata,
            WriteOperation::RemoveMetadataTemporary,
            WriteOperation::ReleaseReservation,
        ];
        for operation in operations {
            verify_failure(operation)?;
        }
        Ok(())
    }

    #[test]
    fn a_destination_created_after_reservation_is_never_replaced() -> TestResult {
        let temporary = tempfile::tempdir()?;
        let writer = Writer::new(temporary.path().to_path_buf());
        let mut occupied = None;
        let result = writer.write_with(&fixture()?, &mut |operation, path| {
            if operation == WriteOperation::InstallRaw {
                fs::write(path, b"preexisting recording")?;
                occupied = Some(path.to_path_buf());
            }
            Ok(())
        });
        let occupied = occupied.ok_or("installation was not attempted")?;
        assert_eq!(fs::read(&occupied)?, b"preexisting recording");
        assert!(!occupied.with_extension("json").exists());
        assert!(
            matches!(result, Err(WriteError::Io { operation: WriteOperation::InstallRaw, ref source, .. }) if source.kind() == std::io::ErrorKind::AlreadyExists),
            "{result:?}"
        );
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn directory_sync_failures_report_visible_json_and_complete_ancestor_order() -> TestResult {
        for operation in [
            WriteOperation::SyncHierarchy,
            WriteOperation::SyncRawDirectory,
            WriteOperation::SyncMetadataDirectory,
        ] {
            verify_failure(operation)?;
        }
        let temporary = tempfile::tempdir()?;
        let base = temporary.path().join("new").join("nested");
        let writer = Writer::new(base.clone());
        let mut seen = Vec::new();
        let _path = writer.write_with(&fixture()?, &mut |operation, path| {
            seen.push((operation, path.to_path_buf()));
            Ok(())
        })?;
        let first_file = seen
            .iter()
            .position(|(operation, _)| *operation == WriteOperation::CreateRaw)
            .ok_or("no raw create")?;
        for ancestor in base.ancestors() {
            assert!(
                seen.get(..first_file)
                    .ok_or("short operations")?
                    .contains(&(WriteOperation::SyncHierarchy, ancestor.to_path_buf())),
                "ancestor {ancestor:?} must be synced before file installation"
            );
        }
        let stages: Vec<_> = seen
            .iter()
            .map(|(operation, _)| *operation)
            .filter(|operation| {
                !matches!(
                    operation,
                    WriteOperation::CreateDirectory
                        | WriteOperation::SyncHierarchy
                        | WriteOperation::ReserveName
                )
            })
            .collect();
        assert_eq!(
            stages,
            [
                WriteOperation::CreateRaw,
                WriteOperation::WriteRaw,
                WriteOperation::SyncRaw,
                WriteOperation::InstallRaw,
                WriteOperation::RemoveRawTemporary,
                WriteOperation::SyncRawDirectory,
                WriteOperation::CreateMetadata,
                WriteOperation::WriteMetadata,
                WriteOperation::SyncMetadata,
                WriteOperation::InstallMetadata,
                WriteOperation::RemoveMetadataTemporary,
                WriteOperation::SyncMetadataDirectory,
                WriteOperation::ReleaseReservation
            ]
        );
        Ok(())
    }

    fn verify_failure(expected: WriteOperation) -> TestResult {
        let temporary = tempfile::tempdir()?;
        let writer = Writer::new(temporary.path().join("new"));
        let mut failed_path = None;
        let result = writer.write_with(&fixture()?, &mut |operation, path| {
            if operation == expected {
                failed_path = Some(path.to_path_buf());
                return Err(std::io::Error::other("injected failure"));
            }
            Ok(())
        });
        assert!(
            matches!(result, Err(WriteError::Io { operation, .. }) if operation == expected),
            "{expected:?}: {result:?}"
        );
        let failed = failed_path.ok_or("failure stage not reached")?;
        let directory = if expected == WriteOperation::SyncMetadataDirectory {
            failed
        } else {
            failed.parent().ok_or("no parent")?.to_path_buf()
        };
        let visible = directory
            .read_dir()
            .into_iter()
            .flatten()
            .filter_map(Result::ok)
            .any(|entry| {
                entry
                    .path()
                    .extension()
                    .is_some_and(|value| value == "json")
            });
        assert_eq!(
            visible,
            matches!(
                expected,
                WriteOperation::RemoveMetadataTemporary
                    | WriteOperation::SyncMetadataDirectory
                    | WriteOperation::ReleaseReservation
            ),
            "{expected:?}: JSON visibility"
        );
        let _retry = writer.write(&fixture()?)?;
        Ok(())
    }
}
