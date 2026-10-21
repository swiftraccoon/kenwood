// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later

//! Pure, I/O-free DMR call assembly.
//!
//! One `BrandMeister` Open Terminal connection carries at most one simultaneous
//! call. The capture manager therefore keeps one optional call, tolerates media
//! arriving before its superheader, and finalizes a prior call if a different
//! superheader replaces it.

use std::fmt;
use std::net::SocketAddr;
use std::num::{NonZeroU64, NonZeroUsize};
use std::time::Duration;

use chrono::{DateTime, Utc};
use dmr_rewind_core::{Callsign, DmrAudioSubtype, PacketFlags, TransportSequence, WireDmrId};

/// Number of AMBE+2 codewords in one 60 ms DMR audio packet.
pub const AMBE_FRAMES_PER_PACKET: u64 = 3;
/// Number of codec bytes in one Open Terminal DMR audio packet.
pub const AMBE_BYTES_PER_PACKET: u64 = 27;

/// Maximum number of repeatable event records retained for one call.
///
/// This is an operator storage policy, not a claim about a protocol maximum.
/// The budget is shared by audio packets, voice-header records, and embedded
/// link-control records. The single superheader and terminator fields do not
/// grow with call duration and are not charged against this limit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CaptureRecordLimit(NonZeroUsize);

impl CaptureRecordLimit {
    /// Validate an operator-supplied per-call record limit.
    ///
    /// # Errors
    ///
    /// Returns [`CaptureRecordLimitError::Zero`] for zero or
    /// [`CaptureRecordLimitError::TooLarge`] when the count cannot be
    /// represented on this platform.
    pub fn new(count: u64) -> Result<Self, CaptureRecordLimitError> {
        let count = NonZeroU64::new(count).ok_or(CaptureRecordLimitError::Zero)?;
        let count = usize::try_from(count.get())
            .map_err(|_| CaptureRecordLimitError::TooLarge { count })?;
        let count = NonZeroUsize::new(count).ok_or(CaptureRecordLimitError::Zero)?;
        Ok(Self(count))
    }

    /// Maximum retained repeatable event records per call.
    #[must_use]
    pub const fn get(self) -> usize {
        self.0.get()
    }
}

impl TryFrom<u64> for CaptureRecordLimit {
    type Error = CaptureRecordLimitError;
    fn try_from(value: u64) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<CaptureRecordLimit> for usize {
    fn from(value: CaptureRecordLimit) -> Self {
        value.get()
    }
}

impl fmt::Display for CaptureRecordLimit {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.get(), formatter)
    }
}

impl fmt::Binary for CaptureRecordLimit {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Binary::fmt(&self.get(), formatter)
    }
}

impl fmt::Octal for CaptureRecordLimit {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Octal::fmt(&self.get(), formatter)
    }
}

impl fmt::LowerHex for CaptureRecordLimit {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::LowerHex::fmt(&self.get(), formatter)
    }
}

impl fmt::UpperHex for CaptureRecordLimit {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::UpperHex::fmt(&self.get(), formatter)
    }
}

/// UTC metadata and an independently supplied monotonic elapsed reading.
///
/// Use one monotonic origin for the lifetime of a capture manager. Decreasing
/// elapsed readings are clamped to its preceding activity reading. UTC clock
/// adjustments change metadata only and never determine inactivity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CaptureTime {
    wall: DateTime<Utc>,
    elapsed: Duration,
}

impl CaptureTime {
    /// Pair a UTC timestamp with elapsed time since a caller-owned clock origin.
    #[must_use]
    pub const fn new(wall: DateTime<Utc>, elapsed: Duration) -> Self {
        Self { wall, elapsed }
    }
    /// Return the UTC metadata timestamp.
    #[must_use]
    pub const fn wall(self) -> DateTime<Utc> {
        self.wall
    }
    /// Return the monotonic elapsed reading.
    #[must_use]
    pub const fn elapsed(self) -> Duration {
        self.elapsed
    }
}

/// Invalid per-call capture record policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum CaptureRecordLimitError {
    /// A zero-record limit cannot retain a recording.
    #[error("capture record count must be greater than zero")]
    Zero,
    /// The count cannot be represented on this platform.
    #[error("capture record count {count} exceeds this platform's addressable capacity")]
    TooLarge {
        /// Rejected operator-supplied count.
        count: NonZeroU64,
    },
}

/// A live call exceeded its configured record-retention budget.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum CaptureError {
    /// One more record arrived after the call reached its configured limit.
    #[error(
        "capture record limit {limit} exceeded after {retained_records} retained records; partial call was discarded"
    )]
    RecordLimitExceeded {
        /// Configured maximum retained records for one call.
        limit: usize,
        /// Records retained before the rejected record arrived.
        retained_records: usize,
    },
}

/// Kind of DMR voice session described by a superheader.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionType {
    /// Group call to a talkgroup.
    Group,
    /// Private call to an individual DMR ID.
    Private,
}

impl SessionType {
    /// Lowercase value used in metadata.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Group => "group",
            Self::Private => "private",
        }
    }
}

/// DMR identity from an Open Terminal superheader.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DmrIdentity {
    /// Numeric DMR ID.
    pub id: WireDmrId,
    /// Exact padded bytes supplied by the master, or no callsign for full LC.
    pub callsign: Option<Callsign>,
}

/// Call identity and routing information from an Open Terminal superheader.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SuperHeader {
    /// Group or private voice.
    pub session_type: SessionType,
    /// Transmitting station.
    pub source: DmrIdentity,
    /// Talkgroup or private destination.
    pub destination: DmrIdentity,
}

/// Where a call was received.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamOrigin {
    /// Configured master name.
    pub(crate) master: String,
    /// Configured master host.
    pub(crate) host: String,
    /// Configured master UDP port.
    pub(crate) port: std::num::NonZeroU16,
    /// Resolved peer address for this session.
    pub(crate) peer: SocketAddr,
    /// Seven-digit subscriber DMR ID used by this software terminal.
    pub(crate) dmr_id: dmr_rewind::SubscriberId,
}

impl StreamOrigin {
    /// Record a validated master configuration and the connection's resolved peer.
    #[must_use]
    pub fn new(master: &crate::config::MasterConfig, peer: SocketAddr) -> Self {
        Self {
            master: master.name().to_owned(),
            host: master.host().to_owned(),
            port: master.port(),
            peer,
            dmr_id: master.dmr_id(),
        }
    }
    /// Borrow the configured operator label.
    #[must_use]
    pub fn master(&self) -> &str {
        &self.master
    }
    /// Borrow the configured resolver host.
    #[must_use]
    pub fn host(&self) -> &str {
        &self.host
    }
    /// Return the configured UDP port.
    #[must_use]
    pub const fn port(&self) -> std::num::NonZeroU16 {
        self.port
    }
    /// Return the resolved peer.
    #[must_use]
    pub const fn peer(&self) -> SocketAddr {
        self.peer
    }
    /// Return the terminal subscriber ID.
    #[must_use]
    pub const fn dmr_id(&self) -> dmr_rewind::SubscriberId {
        self.dmr_id
    }
}

/// One received DMR audio packet, archived exactly as delivered by Open Terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioPacket {
    /// Open Terminal transport sequence number.
    pub transport_seq: TransportSequence,
    /// Open Terminal header flags.
    pub flags: PacketFlags,
    /// DMR packet subtype supplied by Open Terminal.
    pub subtype: DmrAudioSubtype,
    /// Three packed 9-byte AMBE+2 mode-33 codewords.
    pub ambe: [u8; 27],
}

/// A DMR voice-header record observed during a call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VoiceHeaderRecord {
    /// Open Terminal transport sequence number.
    pub transport_seq: TransportSequence,
    /// Open Terminal header flags.
    pub flags: PacketFlags,
    /// Raw 12-byte DMR voice-header payload.
    pub data: [u8; 12],
}

/// A DMR terminator with Full Link Control observed at the end of a call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TerminatorRecord {
    /// Open Terminal transport sequence number.
    pub transport_seq: TransportSequence,
    /// Open Terminal header flags.
    pub flags: PacketFlags,
    /// Raw 12-byte terminator Full Link Control, when supplied by the master.
    pub data: Option<[u8; 12]>,
}

/// An uninterpreted DMR embedded-data record observed during a call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmbeddedLcRecord {
    /// Open Terminal transport sequence number.
    pub transport_seq: TransportSequence,
    /// Open Terminal header flags.
    pub flags: PacketFlags,
    /// Exact embedded-data body bytes received from the master.
    data: Vec<u8>,
}

impl EmbeddedLcRecord {
    pub(crate) fn from_event(event: dmr_rewind::EmbeddedDataEvent) -> Self {
        Self {
            transport_seq: event.metadata().sequence(),
            flags: event.metadata().flags(),
            data: event.into_data(),
        }
    }
    /// Preserve an embedded body under the codec's outer payload bound.
    ///
    /// # Errors
    ///
    /// Returns [`dmr_rewind_core::CodecError::PayloadTooLong`] above the wire limit.
    pub fn new(
        transport_seq: TransportSequence,
        flags: PacketFlags,
        data: Vec<u8>,
    ) -> Result<Self, dmr_rewind_core::CodecError> {
        if data.len() > dmr_rewind_core::MAX_PAYLOAD_LEN {
            return Err(dmr_rewind_core::CodecError::PayloadTooLong {
                actual: data.len(),
                maximum: dmr_rewind_core::MAX_PAYLOAD_LEN,
            });
        }
        Ok(Self {
            transport_seq,
            flags,
            data,
        })
    }
    /// Borrow uninterpreted embedded bytes.
    #[must_use]
    pub fn data(&self) -> &[u8] {
        &self.data
    }
}

/// Why a captured call ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndReason {
    /// A DMR terminator was received.
    Terminator,
    /// No call traffic arrived during the configured hang interval.
    Inactivity,
    /// A different superheader replaced the call on the connection.
    Replaced,
    /// The Open Terminal session disconnected.
    Disconnect,
    /// Pulsar shut down while the call was open.
    Shutdown,
}

impl EndReason {
    /// Lowercase value used in metadata.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Terminator => "terminator",
            Self::Inactivity => "inactivity",
            Self::Replaced => "replaced",
            Self::Disconnect => "disconnect",
            Self::Shutdown => "shutdown",
        }
    }
}

/// External completion causes that cannot invent an observed terminator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FinishReason {
    /// No call traffic arrived during the configured hang interval.
    Inactivity,
    /// The terminal connection ended.
    Disconnect,
    /// Recorder shutdown was requested.
    Shutdown,
}

impl From<FinishReason> for EndReason {
    fn from(value: FinishReason) -> Self {
        match value {
            FinishReason::Inactivity => Self::Inactivity,
            FinishReason::Disconnect => Self::Disconnect,
            FinishReason::Shutdown => Self::Shutdown,
        }
    }
}

/// A finished call ready for persistent storage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompletedRecording {
    /// Connection on which this call arrived.
    pub(crate) origin: StreamOrigin,
    /// Superheader, if one arrived before the call ended.
    pub(crate) header: Option<SuperHeader>,
    /// Wall clock when the first event for this call arrived.
    pub(crate) started_at: DateTime<Utc>,
    /// Wall clock when the call was finalized.
    pub(crate) ended_at: DateTime<Utc>,
    /// Reason the call was finalized.
    pub(crate) end_reason: EndReason,
    /// Audio packets in arrival order.
    pub(crate) audio_packets: Vec<AudioPacket>,
    /// Voice-header records in arrival order.
    pub(crate) voice_headers: Vec<VoiceHeaderRecord>,
    /// Terminator Full Link Control, when a terminator ended the call.
    pub(crate) terminator: Option<TerminatorRecord>,
    /// Embedded link-control records in arrival order.
    pub(crate) embedded_lc: Vec<EmbeddedLcRecord>,
}

impl CompletedRecording {
    /// Borrow the connection identity.
    #[must_use]
    pub const fn origin(&self) -> &StreamOrigin {
        &self.origin
    }
    /// Borrow observed or enriched routing metadata.
    #[must_use]
    pub const fn header(&self) -> Option<&SuperHeader> {
        self.header.as_ref()
    }
    /// Return the first-event UTC timestamp.
    #[must_use]
    pub const fn started_at(&self) -> DateTime<Utc> {
        self.started_at
    }
    /// Return the completion UTC timestamp.
    #[must_use]
    pub const fn ended_at(&self) -> DateTime<Utc> {
        self.ended_at
    }
    /// Return the cause of completion.
    #[must_use]
    pub const fn end_reason(&self) -> EndReason {
        self.end_reason
    }
    /// Borrow audio packets in arrival order.
    #[must_use]
    pub fn audio_packets(&self) -> &[AudioPacket] {
        &self.audio_packets
    }
    /// Borrow received voice headers.
    #[must_use]
    pub fn voice_headers(&self) -> &[VoiceHeaderRecord] {
        &self.voice_headers
    }
    /// Borrow the observed terminator when it caused completion.
    #[must_use]
    pub const fn terminator(&self) -> Option<&TerminatorRecord> {
        self.terminator.as_ref()
    }
    /// Borrow uninterpreted embedded records.
    #[must_use]
    pub fn embedded_lc(&self) -> &[EmbeddedLcRecord] {
        &self.embedded_lc
    }
    /// Number of received 60 ms audio packets.
    #[must_use]
    pub fn packet_count(&self) -> u64 {
        u64::try_from(self.audio_packets.len()).unwrap_or(u64::MAX)
    }

    /// Number of packed 20 ms AMBE+2 codewords.
    #[must_use]
    pub fn ambe_frame_count(&self) -> u64 {
        self.packet_count().saturating_mul(AMBE_FRAMES_PER_PACKET)
    }

    /// Number of raw AMBE+2 payload bytes.
    #[must_use]
    pub fn audio_byte_count(&self) -> u64 {
        self.packet_count().saturating_mul(AMBE_BYTES_PER_PACKET)
    }

    /// Codec duration based on received audio packets.
    #[must_use]
    pub fn codec_duration_s(&self) -> f64 {
        Duration::from_millis(self.packet_count().saturating_mul(60)).as_secs_f64()
    }

    /// Nonnegative wall-clock duration between the first event and finalization.
    #[must_use]
    pub fn wall_duration_s(&self) -> f64 {
        self.ended_at
            .signed_duration_since(self.started_at)
            .to_std()
            .map_or(0.0, |duration| duration.as_secs_f64())
    }

    /// First audio packet's Open Terminal sequence number.
    #[must_use]
    pub fn first_transport_seq(&self) -> Option<TransportSequence> {
        self.audio_packets
            .first()
            .map(|packet| packet.transport_seq)
    }

    /// Last audio packet's Open Terminal sequence number.
    #[must_use]
    pub fn last_transport_seq(&self) -> Option<TransportSequence> {
        self.audio_packets.last().map(|packet| packet.transport_seq)
    }
}

#[derive(Debug)]
struct OpenCall {
    header: Option<SuperHeader>,
    started_at: DateTime<Utc>,
    last_activity_at: CaptureTime,
    audio_packets: Vec<AudioPacket>,
    voice_headers: Vec<VoiceHeaderRecord>,
    terminator: Option<TerminatorRecord>,
    embedded_lc: Vec<EmbeddedLcRecord>,
}

impl OpenCall {
    const fn new(now: CaptureTime) -> Self {
        Self {
            header: None,
            started_at: now.wall,
            last_activity_at: now,
            audio_packets: Vec::new(),
            voice_headers: Vec::new(),
            terminator: None,
            embedded_lc: Vec::new(),
        }
    }

    const fn retained_record_count(&self) -> usize {
        self.audio_packets
            .len()
            .saturating_add(self.voice_headers.len())
            .saturating_add(self.embedded_lc.len())
    }
}

/// Assembles events from one Open Terminal connection into completed calls.
#[derive(Debug)]
pub struct CaptureManager {
    origin: StreamOrigin,
    record_limit: CaptureRecordLimit,
    open: Option<OpenCall>,
}

impl CaptureManager {
    /// Create a capture manager for one live Open Terminal connection with an
    /// explicit per-call record-retention limit.
    #[must_use]
    pub const fn new(origin: StreamOrigin, record_limit: CaptureRecordLimit) -> Self {
        Self {
            origin,
            record_limit,
            open: None,
        }
    }

    /// Attach a call superheader.
    ///
    /// A header attaches to an existing headerless call without losing late
    /// audio. An identical repeated header only refreshes activity. A different
    /// header finalizes the prior call with [`EndReason::Replaced`] and starts
    /// the newly announced call.
    pub fn on_super_header(
        &mut self,
        header: SuperHeader,
        now: CaptureTime,
    ) -> Option<CompletedRecording> {
        let Some(mut open) = self.open.take() else {
            let mut new_call = OpenCall::new(now);
            new_call.header = Some(header);
            self.open = Some(new_call);
            return None;
        };

        match open.header.as_mut() {
            None => {
                open.header = Some(header);
                open.last_activity_at = advance_time(open.last_activity_at, now);
                self.open = Some(open);
                None
            }
            Some(current) if same_call(current, &header) => {
                enrich_identity(&mut current.source, &header.source);
                enrich_identity(&mut current.destination, &header.destination);
                open.last_activity_at = advance_time(open.last_activity_at, now);
                self.open = Some(open);
                None
            }
            Some(_) => {
                let completed = self.complete(open, EndReason::Replaced, now.wall);
                let mut new_call = OpenCall::new(now);
                new_call.header = Some(header);
                self.open = Some(new_call);
                Some(completed)
            }
        }
    }

    /// Append one 60 ms DMR audio packet, opening a headerless call if needed.
    ///
    /// # Errors
    ///
    /// Returns [`CaptureError::RecordLimitExceeded`] and discards the entire
    /// partial call when this packet would exceed the configured limit.
    pub fn on_audio(&mut self, packet: AudioPacket, now: CaptureTime) -> Result<(), CaptureError> {
        let open = self.open_for_record(now)?;
        open.audio_packets.push(packet);
        Ok(())
    }

    /// Preserve a raw DMR voice-header record.
    ///
    /// # Errors
    ///
    /// Returns [`CaptureError::RecordLimitExceeded`] and discards the entire
    /// partial call when this record would exceed the configured limit.
    pub fn on_voice_header(
        &mut self,
        record: VoiceHeaderRecord,
        now: CaptureTime,
    ) -> Result<(), CaptureError> {
        let open = self.open_for_record(now)?;
        open.voice_headers.push(record);
        Ok(())
    }

    /// Preserve an uninterpreted embedded-data record.
    ///
    /// # Errors
    ///
    /// Returns [`CaptureError::RecordLimitExceeded`] and discards the entire
    /// partial call when this record would exceed the configured limit.
    pub fn on_embedded_lc(
        &mut self,
        record: EmbeddedLcRecord,
        now: CaptureTime,
    ) -> Result<(), CaptureError> {
        let open = self.open_for_record(now)?;
        open.embedded_lc.push(record);
        Ok(())
    }

    /// Enrich the current call from terminator Full Link Control.
    ///
    /// A terminator describes the call already in progress, not a new call.
    /// It may fill a missing header or missing
    /// IDs on a compatible header, but a conflicting identity is ignored and
    /// no call is opened or replaced.
    pub(crate) fn enrich_from_terminator(&mut self, header: SuperHeader) {
        let Some(open) = self.open.as_mut() else {
            return;
        };
        let Some(current) = open.header.as_mut() else {
            open.header = Some(header);
            return;
        };
        if same_call(current, &header) {
            enrich_identity(&mut current.source, &header.source);
            enrich_identity(&mut current.destination, &header.destination);
        }
    }

    /// Finalize the current call after a received DMR terminator.
    ///
    /// Returns `None` if no event has opened a call.
    pub fn on_terminator(
        &mut self,
        record: TerminatorRecord,
        now: CaptureTime,
    ) -> Option<CompletedRecording> {
        let open = self.open.as_mut()?;
        open.last_activity_at = now;
        open.terminator = Some(record);
        let open = self.open.take()?;
        Some(self.complete(open, EndReason::Terminator, now.wall))
    }

    /// Finalize the call only when its inactivity is at least `hang`.
    ///
    /// UTC clock changes do not affect expiry; only monotonic elapsed time is used.
    pub fn finish_if_inactive(
        &mut self,
        now: CaptureTime,
        hang: Duration,
    ) -> Option<CompletedRecording> {
        let inactive = self.open.as_ref().is_some_and(|open| {
            now.elapsed
                .checked_sub(open.last_activity_at.elapsed)
                .is_some_and(|elapsed| elapsed >= hang)
        });
        if inactive {
            self.finish(FinishReason::Inactivity, now)
        } else {
            None
        }
    }

    /// Finalize the current call with an externally determined reason.
    ///
    /// Session supervisors use this for disconnect and shutdown. Returns
    /// `None` if no event has opened a call.
    pub fn finish(&mut self, reason: FinishReason, now: CaptureTime) -> Option<CompletedRecording> {
        let open = self.open.take()?;
        Some(self.complete(open, reason.into(), now.wall))
    }

    /// Whether a call is currently open.
    #[must_use]
    pub const fn is_active(&self) -> bool {
        self.open.is_some()
    }

    /// Last activity time for the open call.
    #[must_use]
    pub fn last_activity_at(&self) -> Option<DateTime<Utc>> {
        self.open.as_ref().map(|open| open.last_activity_at.wall)
    }

    fn open_for_record(&mut self, now: CaptureTime) -> Result<&mut OpenCall, CaptureError> {
        let retained_records = self
            .open
            .as_ref()
            .map_or(0, OpenCall::retained_record_count);
        if retained_records >= self.record_limit.get() {
            self.open = None;
            return Err(CaptureError::RecordLimitExceeded {
                limit: self.record_limit.get(),
                retained_records,
            });
        }
        let open = self.open.get_or_insert_with(|| OpenCall::new(now));
        open.last_activity_at = advance_time(open.last_activity_at, now);
        Ok(open)
    }

    fn complete(
        &self,
        open: OpenCall,
        end_reason: EndReason,
        ended_at: DateTime<Utc>,
    ) -> CompletedRecording {
        CompletedRecording {
            origin: self.origin.clone(),
            header: open.header,
            started_at: open.started_at,
            ended_at,
            end_reason,
            audio_packets: open.audio_packets,
            voice_headers: open.voice_headers,
            terminator: open.terminator,
            embedded_lc: open.embedded_lc,
        }
    }
}

fn same_call(current: &SuperHeader, incoming: &SuperHeader) -> bool {
    current.session_type == incoming.session_type
        && ids_compatible(current.source.id, incoming.source.id)
        && ids_compatible(current.destination.id, incoming.destination.id)
}

const fn ids_compatible(current: WireDmrId, incoming: WireDmrId) -> bool {
    current.get() == 0 || incoming.get() == 0 || current.get() == incoming.get()
}

fn enrich_identity(current: &mut DmrIdentity, incoming: &DmrIdentity) {
    if current.id.get() == 0 && incoming.id.get() != 0 {
        current.id = incoming.id;
    }
    if current
        .callsign
        .is_none_or(|value| value.to_trimmed_lossy().is_empty())
        && incoming
            .callsign
            .is_some_and(|value| !value.to_trimmed_lossy().is_empty())
    {
        current.callsign = incoming.callsign;
    }
}

fn advance_time(previous: CaptureTime, now: CaptureTime) -> CaptureTime {
    CaptureTime {
        wall: now.wall,
        elapsed: previous.elapsed.max(now.elapsed),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr};

    const TEST_RECORD_LIMIT: CaptureRecordLimit = match NonZeroUsize::new(100) {
        Some(limit) => CaptureRecordLimit(limit),
        None => unreachable!(),
    };

    fn t(milliseconds: i64) -> CaptureTime {
        CaptureTime::new(
            DateTime::<Utc>::UNIX_EPOCH + chrono::TimeDelta::milliseconds(milliseconds),
            Duration::from_millis(milliseconds.unsigned_abs()),
        )
    }

    fn origin() -> StreamOrigin {
        StreamOrigin {
            master: "BM-US".to_string(),
            host: "3103.master.brandmeister.network".to_string(),
            port: const { std::num::NonZeroU16::new(54_006).unwrap() },
            peer: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 54_006),
            dmr_id: const {
                match dmr_rewind::SubscriberId::new(3_101_234) {
                    Ok(value) => value,
                    Err(_) => unreachable!(),
                }
            },
        }
    }

    fn header(source: u32, destination: u32) -> SuperHeader {
        SuperHeader {
            session_type: SessionType::Group,
            source: DmrIdentity {
                id: WireDmrId::new(source),
                callsign: Some(Callsign::from_bytes(*b"W1AW      ")),
            },
            destination: DmrIdentity {
                id: WireDmrId::new(destination),
                callsign: Some(Callsign::from_bytes(*b"TG91      ")),
            },
        }
    }

    const fn audio(seq: u32) -> AudioPacket {
        AudioPacket {
            transport_seq: TransportSequence::new(seq),
            flags: PacketFlags::from_bits(0x1234),
            subtype: match DmrAudioSubtype::new(2) {
                Ok(value) => value,
                Err(_) => unreachable!(),
            },
            ambe: [0xA5; 27],
        }
    }

    const fn terminator(seq: u32) -> TerminatorRecord {
        TerminatorRecord {
            transport_seq: TransportSequence::new(seq),
            flags: PacketFlags::from_bits(0x4321),
            data: Some([0x5A; 12]),
        }
    }

    #[test]
    fn normal_call_preserves_packets_and_counts() {
        let mut capture = CaptureManager::new(origin(), TEST_RECORD_LIMIT);
        assert!(capture.on_super_header(header(310_123, 91), t(0)).is_none());
        assert_eq!(capture.on_audio(audio(10), t(20)), Ok(()));
        assert_eq!(capture.on_audio(audio(11), t(80)), Ok(()));
        let recording = capture.on_terminator(terminator(12), t(100));
        let Some(recording) = recording else {
            unreachable!("call was open");
        };
        assert_eq!(recording.end_reason, EndReason::Terminator);
        assert_eq!(recording.packet_count(), 2);
        assert_eq!(recording.ambe_frame_count(), 6);
        assert_eq!(recording.audio_byte_count(), 54);
        assert!((recording.codec_duration_s() - 0.12).abs() < f64::EPSILON);
        assert!((recording.wall_duration_s() - 0.1).abs() < f64::EPSILON);
        assert_eq!(
            recording.first_transport_seq(),
            Some(TransportSequence::new(10))
        );
        assert_eq!(
            recording.last_transport_seq(),
            Some(TransportSequence::new(11))
        );
        assert_eq!(recording.terminator, Some(terminator(12)));
        assert!(
            !capture.is_active(),
            "a normal terminator must leave no active call"
        );
    }

    #[test]
    fn audio_before_header_is_retained() {
        let mut capture = CaptureManager::new(origin(), TEST_RECORD_LIMIT);
        assert_eq!(capture.on_audio(audio(7), t(0)), Ok(()));
        assert!(
            capture
                .on_super_header(header(310_123, 91), t(20))
                .is_none()
        );
        let recording = capture.on_terminator(terminator(8), t(60));
        assert!(
            matches!(recording, Some(ref value) if value.audio_packets == [audio(7)] && value.header.is_some()),
            "got {recording:?}"
        );
    }

    #[test]
    fn headerless_call_is_valid() {
        let mut capture = CaptureManager::new(origin(), TEST_RECORD_LIMIT);
        assert_eq!(capture.on_audio(audio(7), t(0)), Ok(()));
        let recording = capture.finish(FinishReason::Disconnect, t(10));
        assert!(
            matches!(recording, Some(ref value) if value.header.is_none()),
            "got {recording:?}"
        );
    }

    #[test]
    fn identical_header_does_not_split_call() {
        let mut capture = CaptureManager::new(origin(), TEST_RECORD_LIMIT);
        let announced = header(310_123, 91);
        assert!(capture.on_super_header(announced.clone(), t(0)).is_none());
        assert_eq!(capture.on_audio(audio(1), t(20)), Ok(()));
        assert!(capture.on_super_header(announced, t(40)).is_none());
        assert_eq!(capture.on_audio(audio(2), t(60)), Ok(()));
        let recording = capture.on_terminator(terminator(3), t(80));
        assert!(
            matches!(recording, Some(ref value) if value.packet_count() == 2),
            "got {recording:?}"
        );
    }

    #[test]
    fn richer_callsigns_enrich_the_same_call_without_splitting() {
        let mut capture = CaptureManager::new(origin(), TEST_RECORD_LIMIT);
        let mut sparse = header(310_123, 91);
        sparse.source.callsign = None;
        sparse.destination.callsign = None;
        assert!(capture.on_super_header(sparse, t(0)).is_none());
        assert_eq!(capture.on_audio(audio(1), t(20)), Ok(()));
        assert!(
            capture
                .on_super_header(header(310_123, 91), t(40))
                .is_none()
        );
        assert_eq!(capture.on_audio(audio(2), t(60)), Ok(()));
        let recording = capture.on_terminator(terminator(3), t(80));
        assert!(
            matches!(
                recording,
                Some(ref value)
                    if value.packet_count() == 2
                        && value.header.as_ref().is_some_and(|item| {
                            item.source.callsign == Some(Callsign::from_bytes(*b"W1AW      "))
                                && item.destination.callsign == Some(Callsign::from_bytes(*b"TG91      "))
                        })
            ),
            "got {recording:?}"
        );
    }

    #[test]
    fn sparse_repeated_header_does_not_erase_callsigns() {
        let mut capture = CaptureManager::new(origin(), TEST_RECORD_LIMIT);
        assert!(capture.on_super_header(header(310_123, 91), t(0)).is_none());
        let mut sparse = header(310_123, 91);
        sparse.source.callsign = None;
        sparse.destination.callsign = None;
        assert!(capture.on_super_header(sparse, t(20)).is_none());
        let recording = capture.on_terminator(terminator(1), t(40));
        assert!(
            matches!(
                recording,
                Some(ref value)
                    if value.header.as_ref().is_some_and(|item| {
                        item.source.callsign == Some(Callsign::from_bytes(*b"W1AW      "))
                            && item.destination.callsign == Some(Callsign::from_bytes(*b"TG91      "))
                    })
            ),
            "got {recording:?}"
        );
    }

    #[test]
    fn zero_ids_are_missing_metadata_not_a_new_call() {
        let mut capture = CaptureManager::new(origin(), TEST_RECORD_LIMIT);
        assert!(capture.on_super_header(header(310_123, 91), t(0)).is_none());
        assert_eq!(capture.on_audio(audio(1), t(20)), Ok(()));
        let sparse = SuperHeader {
            session_type: SessionType::Group,
            source: DmrIdentity {
                id: WireDmrId::new(0),
                callsign: None,
            },
            destination: DmrIdentity {
                id: WireDmrId::new(0),
                callsign: None,
            },
        };
        assert!(capture.on_super_header(sparse, t(40)).is_none());
        let recording = capture.on_terminator(terminator(2), t(60));
        assert!(
            matches!(
                recording,
                Some(ref value)
                    if value.packet_count() == 1
                        && value.header.as_ref().is_some_and(|item| {
                            item.source.id.get() == 310_123
                                && item.destination.id.get() == 91
                        })
            ),
            "got {recording:?}"
        );
    }

    #[test]
    fn real_ids_enrich_an_initial_zero_id_superheader() {
        let mut capture = CaptureManager::new(origin(), TEST_RECORD_LIMIT);
        let sparse = SuperHeader {
            session_type: SessionType::Group,
            source: DmrIdentity {
                id: WireDmrId::new(0),
                callsign: Some(Callsign::from_bytes(*b"W1AW      ")),
            },
            destination: DmrIdentity {
                id: WireDmrId::new(0),
                callsign: None,
            },
        };
        assert!(capture.on_super_header(sparse, t(0)).is_none());
        assert_eq!(capture.on_audio(audio(1), t(20)), Ok(()));
        assert!(
            capture
                .on_super_header(header(310_123, 91), t(40))
                .is_none()
        );
        let recording = capture.on_terminator(terminator(2), t(60));
        assert!(
            matches!(
                recording,
                Some(ref value)
                    if value.packet_count() == 1
                        && value.header.as_ref().is_some_and(|item| {
                            item.source.id.get() == 310_123
                                && item.source.callsign == Some(Callsign::from_bytes(*b"W1AW      "))
                                && item.destination.id.get() == 91
                        })
            ),
            "got {recording:?}"
        );
    }

    #[test]
    fn different_header_replaces_and_opens_new_call() {
        let mut capture = CaptureManager::new(origin(), TEST_RECORD_LIMIT);
        assert!(capture.on_super_header(header(310_123, 91), t(0)).is_none());
        assert_eq!(capture.on_audio(audio(1), t(20)), Ok(()));
        let replaced = capture.on_super_header(header(310_999, 3100), t(40));
        assert!(
            matches!(replaced, Some(ref value) if value.end_reason == EndReason::Replaced && value.packet_count() == 1),
            "got {replaced:?}"
        );
        let current = capture.on_terminator(terminator(2), t(60));
        assert!(
            matches!(current, Some(ref value) if value.header.as_ref().is_some_and(|item| item.destination.id.get() == 3100)),
            "got {current:?}"
        );
    }

    #[test]
    fn terminator_identity_enriches_a_headerless_call() {
        let mut capture = CaptureManager::new(origin(), TEST_RECORD_LIMIT);
        assert_eq!(capture.on_audio(audio(1), t(0)), Ok(()));
        capture.enrich_from_terminator(header(310_123, 91));

        let recording = capture.on_terminator(terminator(2), t(60));
        assert!(
            matches!(
                recording,
                Some(ref value)
                    if value.header.as_ref().is_some_and(|item| {
                        item.source.id.get() == 310_123 && item.destination.id.get() == 91
                    })
                        && value.packet_count() == 1
            ),
            "got {recording:?}"
        );
    }

    #[test]
    fn conflicting_terminator_identity_does_not_replace_the_current_call() {
        let mut capture = CaptureManager::new(origin(), TEST_RECORD_LIMIT);
        assert!(capture.on_super_header(header(310_123, 91), t(0)).is_none());
        assert_eq!(capture.on_audio(audio(1), t(20)), Ok(()));
        capture.enrich_from_terminator(header(310_999, 3100));

        let recording = capture.on_terminator(terminator(2), t(60));
        assert!(
            matches!(
                recording,
                Some(ref value)
                    if value.end_reason == EndReason::Terminator
                        && value.packet_count() == 1
                        && value.header.as_ref().is_some_and(|item| {
                            item.source.id.get() == 310_123 && item.destination.id.get() == 91
                        })
            ),
            "got {recording:?}"
        );
        assert!(
            !capture.is_active(),
            "the mismatched terminator must finalize the original call"
        );
    }

    #[test]
    fn voice_header_and_embedded_lc_open_and_survive() {
        let mut capture = CaptureManager::new(origin(), TEST_RECORD_LIMIT);
        let voice = VoiceHeaderRecord {
            transport_seq: TransportSequence::new(4),
            flags: PacketFlags::from_bits(5),
            data: [7; 12],
        };
        let embedded = EmbeddedLcRecord {
            transport_seq: TransportSequence::new(8),
            flags: PacketFlags::from_bits(9),
            data: vec![11; 11],
        };
        assert_eq!(capture.on_voice_header(voice, t(0)), Ok(()));
        assert_eq!(capture.on_embedded_lc(embedded.clone(), t(20)), Ok(()));
        let ending = terminator(12);
        let recording = capture.on_terminator(ending, t(40));
        assert!(
            matches!(recording, Some(ref value) if value.voice_headers == [voice] && value.terminator == Some(ending) && value.embedded_lc == [embedded]),
            "got {recording:?}"
        );
    }

    #[test]
    fn shared_record_budget_accepts_the_exact_boundary() -> Result<(), CaptureRecordLimitError> {
        let limit = CaptureRecordLimit::new(3)?;
        let mut capture = CaptureManager::new(origin(), limit);
        let voice = VoiceHeaderRecord {
            transport_seq: TransportSequence::new(1),
            flags: PacketFlags::from_bits(2),
            data: [4; 12],
        };
        let embedded = EmbeddedLcRecord {
            transport_seq: TransportSequence::new(5),
            flags: PacketFlags::from_bits(6),
            data: vec![8; 10],
        };

        assert_eq!(capture.on_audio(audio(9), t(0)), Ok(()));
        assert_eq!(capture.on_voice_header(voice, t(20)), Ok(()));
        assert_eq!(capture.on_embedded_lc(embedded.clone(), t(40)), Ok(()));
        let recording = capture.on_terminator(terminator(10), t(60));

        assert!(
            matches!(
                recording,
                Some(ref value)
                    if value.audio_packets == [audio(9)]
                        && value.voice_headers == [voice]
                        && value.embedded_lc == [embedded]
                        && value.terminator == Some(terminator(10))
            ),
            "got {recording:?}"
        );
        Ok(())
    }

    #[test]
    fn next_record_over_the_shared_budget_discards_the_partial_call()
    -> Result<(), CaptureRecordLimitError> {
        let limit = CaptureRecordLimit::new(2)?;
        let mut capture = CaptureManager::new(origin(), limit);
        let embedded = EmbeddedLcRecord {
            transport_seq: TransportSequence::new(2),
            flags: PacketFlags::from_bits(3),
            data: vec![5; 10],
        };
        let voice = VoiceHeaderRecord {
            transport_seq: TransportSequence::new(6),
            flags: PacketFlags::from_bits(7),
            data: [9; 12],
        };

        assert_eq!(capture.on_audio(audio(1), t(0)), Ok(()));
        assert_eq!(capture.on_embedded_lc(embedded, t(20)), Ok(()));
        let overflow = capture.on_voice_header(voice, t(40));

        assert_eq!(
            overflow,
            Err(CaptureError::RecordLimitExceeded {
                limit: 2,
                retained_records: 2,
            })
        );
        assert!(
            !capture.is_active(),
            "overflow must discard rather than retain a truncated call"
        );
        assert!(
            capture.finish(FinishReason::Shutdown, t(60)).is_none(),
            "discarded partial content must not become a recording"
        );
        Ok(())
    }

    #[test]
    fn inactivity_obeys_last_event_and_clock_direction() {
        let mut capture = CaptureManager::new(origin(), TEST_RECORD_LIMIT);
        assert_eq!(capture.on_audio(audio(1), t(1_000)), Ok(()));
        assert!(
            capture
                .finish_if_inactive(t(999), Duration::from_secs(3))
                .is_none()
        );
        assert!(
            capture
                .finish_if_inactive(t(3_999), Duration::from_secs(3))
                .is_none()
        );
        let recording = capture.finish_if_inactive(t(4_000), Duration::from_secs(3));
        assert!(
            matches!(recording, Some(ref value) if value.end_reason == EndReason::Inactivity),
            "got {recording:?}"
        );
    }

    #[test]
    fn terminator_without_open_call_is_ignored() {
        let mut capture = CaptureManager::new(origin(), TEST_RECORD_LIMIT);
        assert!(capture.on_terminator(terminator(1), t(0)).is_none());
    }

    #[test]
    fn empty_wire_terminator_still_finalizes_the_call() {
        let mut capture = CaptureManager::new(origin(), TEST_RECORD_LIMIT);
        assert_eq!(capture.on_audio(audio(1), t(0)), Ok(()));
        let empty = TerminatorRecord {
            data: None,
            ..terminator(2)
        };
        let recording = capture.on_terminator(empty, t(60));
        assert!(
            matches!(
                recording,
                Some(ref value)
                    if value.end_reason == EndReason::Terminator
                        && value.terminator == Some(empty)
            ),
            "got {recording:?}"
        );
    }

    #[test]
    fn timeout_finish_has_no_terminator_record() {
        let mut capture = CaptureManager::new(origin(), TEST_RECORD_LIMIT);
        assert_eq!(capture.on_audio(audio(1), t(0)), Ok(()));
        let recording = capture.finish_if_inactive(t(3_000), Duration::from_secs(3));
        assert!(
            matches!(recording, Some(ref value) if value.end_reason == EndReason::Inactivity && value.terminator.is_none()),
            "got {recording:?}"
        );
    }

    #[test]
    fn utc_adjustments_never_change_monotonic_inactivity() -> Result<(), CaptureError> {
        for shift in [-86_400, 86_400] {
            let mut capture = CaptureManager::new(origin(), TEST_RECORD_LIMIT);
            let initial = CaptureTime::new(DateTime::<Utc>::UNIX_EPOCH, Duration::from_secs(10));
            capture.on_audio(audio(1), initial)?;
            let shifted = DateTime::<Utc>::UNIX_EPOCH + chrono::TimeDelta::seconds(shift);
            assert!(
                capture
                    .finish_if_inactive(
                        CaptureTime::new(shifted, Duration::from_secs(12)),
                        Duration::from_secs(3)
                    )
                    .is_none()
            );
            let completed = capture.finish_if_inactive(
                CaptureTime::new(shifted, Duration::from_secs(13)),
                Duration::from_secs(3),
            );
            assert!(
                matches!(completed, Some(ref call) if call.end_reason() == EndReason::Inactivity && call.terminator().is_none()),
                "{completed:?}"
            );
            if let Some(call) = completed {
                assert_eq!(
                    call.wall_duration_s().to_bits(),
                    (Duration::from_secs(shift.unsigned_abs()).as_secs_f64()
                        * f64::from(shift > 0))
                    .to_bits()
                );
            }
        }
        Ok(())
    }

    #[test]
    fn full_width_id_and_variable_embedded_body_are_preserved()
    -> Result<(), Box<dyn std::error::Error>> {
        for length in [0, 1, 9, 10, 11, dmr_rewind_core::MAX_PAYLOAD_LEN] {
            let record = EmbeddedLcRecord::new(
                u32::MAX.into(),
                PacketFlags::from_bits(u16::MAX),
                vec![0xA5; length],
            )?;
            let mut capture = CaptureManager::new(origin(), TEST_RECORD_LIMIT);
            let mut announced = header(u32::MAX, 0);
            announced.source.callsign = Some(Callsign::from_bytes([0xff; 10]));
            let _previous = capture.on_super_header(announced.clone(), t(0));
            capture.on_embedded_lc(record.clone(), t(1))?;
            let completed = capture
                .finish(FinishReason::Shutdown, t(2))
                .ok_or("missing capture")?;
            assert_eq!(completed.header(), Some(&announced));
            assert_eq!(completed.embedded_lc(), [record]);
            assert_eq!(
                completed
                    .embedded_lc()
                    .first()
                    .ok_or("missing embedded")?
                    .data()
                    .len(),
                length
            );
        }
        assert!(matches!(
            EmbeddedLcRecord::new(
                0.into(),
                PacketFlags::NONE,
                vec![0; dmr_rewind_core::MAX_PAYLOAD_LEN + 1]
            ),
            Err(dmr_rewind_core::CodecError::PayloadTooLong { .. })
        ));
        Ok(())
    }
}
