// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later

//! Pure, I/O-free capture core: assembles session voice events into
//! completed per-transmission recordings.
//!
//! Sans-io discipline: nothing here reads the clock or touches the
//! filesystem; the session shell passes `now` into every event
//! method, and the writer consumes the completed values.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::net::SocketAddr;
use std::num::{NonZeroU64, NonZeroUsize};

use chrono::{DateTime, Utc};
use dstar_gateway_core::dprs::DprsReport;
use dstar_gateway_core::slowdata::{SlowDataAssembler, SlowDataBlock, SlowDataTextCollector};
use dstar_gateway_core::{DstarHeader, StreamId, VoiceFrame};

use crate::config::{Target, TargetKey};

/// D-STAR voice seq values cycle 0..=20 (one 21-frame superframe).
pub(crate) const SEQ_MODULUS: u16 = 21;

/// D-STAR voice frames carried per second of codec time.
const FRAMES_PER_SECOND: usize = 50;

/// Validated operator policy for the largest retained stream prefix.
///
/// This is a storage policy, not a claim about a protocol maximum. The
/// configured number of seconds is converted to the exact number of 20 ms
/// D-STAR voice frames that may be retained in memory and written to disk.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CaptureDurationLimit {
    seconds: NonZeroU64,
    frames: NonZeroUsize,
}

impl CaptureDurationLimit {
    /// Validate an operator-supplied duration in seconds.
    ///
    /// # Errors
    ///
    /// Returns [`CaptureLimitError::Zero`] for zero or
    /// [`CaptureLimitError::TooLarge`] when retained frames exceed this
    /// platform's address space or worst-case inferred codec time exceeds u64.
    pub fn try_from_seconds(seconds: u64) -> Result<Self, CaptureLimitError> {
        let seconds = NonZeroU64::new(seconds).ok_or(CaptureLimitError::Zero)?;
        let seconds_as_usize =
            usize::try_from(seconds.get()).map_err(|_| CaptureLimitError::TooLarge { seconds })?;
        let frames = seconds_as_usize
            .checked_mul(FRAMES_PER_SECOND)
            .and_then(NonZeroUsize::new)
            .ok_or(CaptureLimitError::TooLarge { seconds })?;
        if seconds
            .get()
            .checked_mul(50)
            .and_then(|count| count.checked_sub(1))
            .and_then(|count| count.checked_mul(20))
            .and_then(|count| count.checked_add(1))
            .is_none()
        {
            return Err(CaptureLimitError::TooLarge { seconds });
        }
        Ok(Self { seconds, frames })
    }

    /// Configured duration in seconds.
    #[must_use]
    pub const fn as_seconds(self) -> u64 {
        self.seconds.get()
    }

    /// Exact maximum number of retained voice frames.
    #[must_use]
    pub const fn as_frames(self) -> usize {
        self.frames.get()
    }
}

/// Invalid capture-duration policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum CaptureLimitError {
    /// A zero-second limit cannot retain a recording.
    #[error("capture duration must be greater than zero seconds")]
    Zero,
    /// Retained frames or inferred codec time cannot be represented.
    #[error("capture duration {seconds} seconds exceeds retained-frame or codec-time capacity")]
    TooLarge {
        /// Rejected operator-supplied duration.
        seconds: NonZeroU64,
    },
}

/// Validated maximum number of simultaneous stream states retained for one
/// reflector session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ConcurrentCaptureLimit(NonZeroUsize);

impl ConcurrentCaptureLimit {
    /// Validate an operator-supplied simultaneous-capture count.
    ///
    /// # Errors
    ///
    /// Returns [`ConcurrentCaptureLimitError::Zero`] for zero or
    /// [`ConcurrentCaptureLimitError::TooLarge`] when the count cannot be
    /// represented on this platform.
    pub fn try_from_count(count: u64) -> Result<Self, ConcurrentCaptureLimitError> {
        let count = NonZeroU64::new(count).ok_or(ConcurrentCaptureLimitError::Zero)?;
        let count = usize::try_from(count.get())
            .map_err(|_| ConcurrentCaptureLimitError::TooLarge { count })?;
        let count = NonZeroUsize::new(count).ok_or(ConcurrentCaptureLimitError::Zero)?;
        Ok(Self(count))
    }

    /// Maximum number of simultaneous retained or suppressed streams.
    #[must_use]
    pub const fn get(self) -> usize {
        self.0.get()
    }
}

impl TryFrom<u64> for CaptureDurationLimit {
    type Error = CaptureLimitError;
    fn try_from(value: u64) -> Result<Self, Self::Error> {
        Self::try_from_seconds(value)
    }
}
impl From<CaptureDurationLimit> for u64 {
    fn from(value: CaptureDurationLimit) -> Self {
        value.as_seconds()
    }
}
impl TryFrom<u64> for ConcurrentCaptureLimit {
    type Error = ConcurrentCaptureLimitError;
    fn try_from(value: u64) -> Result<Self, Self::Error> {
        Self::try_from_count(value)
    }
}
impl From<ConcurrentCaptureLimit> for usize {
    fn from(value: ConcurrentCaptureLimit) -> Self {
        value.get()
    }
}

macro_rules! numeric_format {
    ($value:ty, $getter:ident; $($format:ident),+ $(,)?) => {
        $(impl fmt::$format for $value {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result { fmt::$format::fmt(&self.$getter(), formatter) }
        })+
    };
}
numeric_format!(CaptureDurationLimit, as_seconds; Display, Binary, Octal, LowerHex, UpperHex, LowerExp, UpperExp);
numeric_format!(ConcurrentCaptureLimit, get; Display, Binary, Octal, LowerHex, UpperHex, LowerExp, UpperExp);

/// Invalid simultaneous-capture policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ConcurrentCaptureLimitError {
    /// A zero count would reject every transmission.
    #[error("concurrent capture count must be greater than zero")]
    Zero,
    /// The count cannot be represented on this platform.
    #[error("concurrent capture count {count} exceeds this platform's addressable capacity")]
    TooLarge {
        /// Rejected operator-supplied count.
        count: NonZeroU64,
    },
}

/// A new stream could not be admitted without exceeding the operator's
/// aggregate capture-state bound.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error(
    "stream {stream_id:04X} exceeds the configured maximum of {max_concurrent} concurrent captures"
)]
pub struct CaptureCapacityError {
    /// Stream identifier that was rejected.
    pub stream_id: u16,
    /// Configured simultaneous-capture limit.
    pub max_concurrent: usize,
}

/// Frames missing between consecutive received seq values.
///
/// The single source of truth for D-STAR gap accounting, shared by
/// the capture core, the audio decoder, the feature extractor, and
/// the dvrec importer so they can never diverge. Both the wire seq
/// and the previous seq are untrusted bytes: a corrupted frame (or
/// the 0x40 EOT flag riding the seq byte) can be out of the 0..=20
/// alphabet, and such a value must contribute no gaps rather than
/// underflow the modular distance. A duplicate seq is zero gaps.
#[must_use]
pub(crate) fn seq_gap(prev: u8, seq: u8) -> u64 {
    if u16::from(seq) >= SEQ_MODULUS || u16::from(prev) >= SEQ_MODULUS {
        return 0;
    }
    let distance = (u16::from(seq) + SEQ_MODULUS - u16::from(prev)) % SEQ_MODULUS;
    u64::from(distance.saturating_sub(1))
}

/// One received voice frame, archived exactly as it arrived.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FrameRecord {
    /// Raw sequence byte, including values outside the 0..=20 sequence alphabet.
    pub seq: u8,
    /// 9-byte AMBE voice payload.
    pub ambe: [u8; 9],
    /// 3-byte slow-data payload.
    pub slow_data: [u8; 3],
}

/// Why a recording ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EndReason {
    /// Real end-of-transmission packet.
    Eot,
    /// Synthesized after the protocol inactivity window.
    Inactivity,
    /// The session dropped while the stream was open.
    Disconnect,
    /// Stargazer was shut down while the stream was open.
    Shutdown,
    /// The operator's retention limit was reached; the archived prefix is
    /// complete, and the rest of the stream was deliberately discarded.
    CaptureLimit,
}

impl EndReason {
    /// Lowercase name used in metadata JSON.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Eot => "eot",
            Self::Inactivity => "inactivity",
            Self::Disconnect => "disconnect",
            Self::Shutdown => "shutdown",
            Self::CaptureLimit => "capture_limit",
        }
    }
}

/// Recording target and optional live connection metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamOrigin {
    target: TargetKey,
    endpoint: Option<Target>,
    peer: Option<SocketAddr>,
}

impl StreamOrigin {
    /// Stamp a live connection's checked target and resolved peer.
    #[must_use]
    pub fn new(target: Target, peer: SocketAddr) -> Self {
        Self {
            target: target.key(),
            endpoint: Some(target),
            peer: Some(peer),
        }
    }
    /// Identify an imported archive without claiming a live host or peer.
    #[must_use]
    pub const fn imported(target: TargetKey) -> Self {
        Self {
            target,
            endpoint: None,
            peer: None,
        }
    }
    /// Checked reflector/module path component.
    #[must_use]
    pub const fn target(&self) -> &TargetKey {
        &self.target
    }
    /// Live configured endpoint, absent for imported packet logs.
    #[must_use]
    pub const fn endpoint(&self) -> Option<&Target> {
        self.endpoint.as_ref()
    }
    /// Resolved peer, absent for imported packet logs.
    #[must_use]
    pub const fn peer(&self) -> Option<SocketAddr> {
        self.peer
    }
}

/// A DPRS position report seen inside a stream's slow data.
#[derive(Debug, Clone)]
pub struct DprsSeen {
    /// The parsed report.
    pub report: DprsReport,
    /// Index into the recording's frames (arrival order) of the
    /// frame whose fragment completed the report.
    pub at_frame: usize,
}

/// A finished transmission, ready to be written to disk.
#[derive(Debug, Clone)]
pub struct CompletedRecording {
    /// Connection the stream arrived on.
    pub(crate) origin: StreamOrigin,
    /// D-STAR stream id.
    pub(crate) stream_id: StreamId,
    /// Decoded voice header, if one was received.
    pub(crate) header: Option<DstarHeader>,
    /// Stringified lenient-parse diagnostics from the header.
    pub(crate) header_diagnostics: Vec<String>,
    /// Wall clock when the stream opened.
    pub(crate) started_at: DateTime<Utc>,
    /// Wall clock when the stream was finalized.
    pub(crate) ended_at: DateTime<Utc>,
    /// Why the stream ended.
    pub(crate) end_reason: EndReason,
    /// Frames in arrival order.
    pub(crate) frames: Vec<FrameRecord>,
    /// Missing frames inferred from seq discontinuities.
    pub(crate) gaps: u64,
    /// Validated printable-ASCII D-STAR TX message, without trailing
    /// wire-space padding. Invalid textual bytes remain available through
    /// [`Self::text_bytes`] and never receive replacement characters.
    pub(crate) text: Option<String>,
    /// The TX message's raw 20 wire bytes, kept losslessly.
    pub(crate) text_bytes: Option<[u8; 20]>,
    /// DPRS position reports in arrival order.
    pub(crate) dprs: Vec<DprsSeen>,
}

impl CompletedRecording {
    /// Frames the transmission should have had: received + gaps.
    #[must_use]
    pub fn expected_frames(&self) -> u64 {
        u64::try_from(self.frames.len()).unwrap_or_else(|_| unreachable!("validated capture count"))
            + self.gaps
    }

    /// Codec-time duration: expected frames × 20 ms.
    #[must_use]
    #[expect(
        clippy::cast_precision_loss,
        reason = "codec duration is the nearest representable f64 projection of a checked u64 frame count"
    )]
    pub fn duration_s(&self) -> f64 {
        self.expected_frames() as f64 * 0.02
    }

    /// Recording target and live metadata, if available.
    #[must_use]
    pub const fn origin(&self) -> &StreamOrigin {
        &self.origin
    }
    /// Nonzero stream identifier.
    #[must_use]
    pub const fn stream_id(&self) -> StreamId {
        self.stream_id
    }
    /// Received header, if present.
    #[must_use]
    pub const fn header(&self) -> Option<&DstarHeader> {
        self.header.as_ref()
    }
    /// Header parser diagnostics in arrival order.
    #[must_use]
    pub fn header_diagnostics(&self) -> &[String] {
        &self.header_diagnostics
    }
    /// Caller-supplied stream opening time.
    #[must_use]
    pub const fn started_at(&self) -> DateTime<Utc> {
        self.started_at
    }
    /// Caller-supplied finalization time.
    #[must_use]
    pub const fn ended_at(&self) -> DateTime<Utc> {
        self.ended_at
    }
    /// Finalization trigger.
    #[must_use]
    pub const fn end_reason(&self) -> EndReason {
        self.end_reason
    }
    /// Losslessly retained received frames.
    #[must_use]
    pub fn frames(&self) -> &[FrameRecord] {
        &self.frames
    }
    /// Missing codec frames inferred from the sequence alphabet.
    #[must_use]
    pub const fn gaps(&self) -> u64 {
        self.gaps
    }
    /// Printable received TX text without trailing padding.
    #[must_use]
    pub fn text(&self) -> Option<&str> {
        self.text.as_deref()
    }
    /// Lossless received TX message bytes.
    #[must_use]
    pub const fn text_bytes(&self) -> Option<&[u8; 20]> {
        self.text_bytes.as_ref()
    }
    /// Parsed position reports and their frame indexes.
    #[must_use]
    pub fn dprs(&self) -> &[DprsSeen] {
        &self.dprs
    }
}

/// One in-progress stream.
#[derive(Debug)]
struct StreamCapture {
    header: Option<DstarHeader>,
    header_diagnostics: Vec<String>,
    started_at: DateTime<Utc>,
    frames: Vec<FrameRecord>,
    prev_seq: Option<u8>,
    gaps: u64,
    text_collector: SlowDataTextCollector,
    assembler: SlowDataAssembler,
    text: Option<String>,
    text_bytes: Option<[u8; 20]>,
    dprs: Vec<DprsSeen>,
}

impl StreamCapture {
    const fn new(started_at: DateTime<Utc>) -> Self {
        Self {
            header: None,
            header_diagnostics: Vec::new(),
            started_at,
            frames: Vec::new(),
            prev_seq: None,
            gaps: 0,
            text_collector: SlowDataTextCollector::new(),
            assembler: SlowDataAssembler::new(),
            text: None,
            text_bytes: None,
            dprs: Vec::new(),
        }
    }

    fn push_frame(&mut self, seq: u8, frame: &VoiceFrame) {
        if let Some(prev) = self.prev_seq {
            self.gaps += seq_gap(prev, seq);
        }
        self.prev_seq = Some(seq);
        self.frames.push(FrameRecord {
            seq,
            ambe: frame.ambe,
            slow_data: frame.slow_data,
        });

        // Text: the collector wants every frame index, and index 0 is
        // its documented reset signal for superframe sync.
        self.text_collector.push(frame.slow_data, seq);
        if self.text_bytes.is_none()
            && let Some(msg) = self.text_collector.take_message()
        {
            self.text_bytes = Some(msg.into_bytes());
            if let Ok(text) = msg.text()
                && !text.is_empty()
            {
                self.text = Some(text.to_owned());
            }
        }

        if let Some(SlowDataBlock::Gps(sentence_bytes)) = self.assembler.push(frame.slow_data, seq)
            && let Ok(sentence) = std::str::from_utf8(&sentence_bytes)
        {
            self.consume_gps_sentence(sentence);
        }
    }

    /// Parse a completed GPS slow-data sentence; keep DPRS reports,
    /// silently skip plain NMEA (common from GPS-equipped radios).
    fn consume_gps_sentence(&mut self, sentence: &str) {
        if let Ok(report) = sentence.parse::<DprsReport>() {
            self.dprs.push(DprsSeen {
                report,
                at_frame: self.frames.len().saturating_sub(1),
            });
        }
    }
}

/// Assembles voice events into [`CompletedRecording`]s, keyed by
/// stream id and tolerant of every arrival order the protocols
/// produce (frames before header, interleaved streams, missing EOT).
#[derive(Debug)]
pub struct CaptureManager {
    origin: StreamOrigin,
    limit: CaptureDurationLimit,
    concurrent_limit: ConcurrentCaptureLimit,
    open: HashMap<u16, StreamCapture>,
    limit_reached: HashSet<u16>,
}

impl CaptureManager {
    /// Create a manager for one connection's streams and retention policy.
    #[must_use]
    pub fn new(
        origin: StreamOrigin,
        limit: CaptureDurationLimit,
        concurrent_limit: ConcurrentCaptureLimit,
    ) -> Self {
        Self {
            origin,
            limit,
            concurrent_limit,
            open: HashMap::new(),
            limit_reached: HashSet::new(),
        }
    }

    /// Handle a `VoiceStart`: open a capture, or attach the header
    /// to a capture opened by frames that beat the header.
    ///
    /// # Errors
    ///
    /// Returns [`CaptureCapacityError`] when admitting a new stream would
    /// exceed the configured simultaneous-capture limit.
    pub fn on_voice_start(
        &mut self,
        stream_id: StreamId,
        header: DstarHeader,
        diagnostics: Vec<String>,
        now: DateTime<Utc>,
    ) -> Result<(), CaptureCapacityError> {
        let raw_id = stream_id.get();
        if self.limit_reached.contains(&raw_id) {
            return Ok(());
        }
        if !self.open.contains_key(&raw_id) {
            self.ensure_capacity(raw_id)?;
        }
        let entry = self
            .open
            .entry(raw_id)
            .or_insert_with(|| StreamCapture::new(now));
        // Attach without touching accumulated frames; keep the first
        // header if the core ever re-emits one.
        if entry.header.is_none() {
            entry.header = Some(header);
            entry.header_diagnostics = diagnostics;
        }
        Ok(())
    }

    /// Handle a `VoiceFrame`, opening a headerless capture if necessary.
    ///
    /// Returns a partial recording with [`EndReason::CaptureLimit`] when this
    /// is the first frame beyond the configured retained prefix. Later frames
    /// for that stream are rejected until its end event and return `None`.
    ///
    /// # Errors
    ///
    /// Returns [`CaptureCapacityError`] when a headerless new stream would
    /// exceed the configured simultaneous-capture limit.
    pub fn on_voice_frame(
        &mut self,
        stream_id: StreamId,
        seq: u8,
        frame: &VoiceFrame,
        now: DateTime<Utc>,
    ) -> Result<Option<CompletedRecording>, CaptureCapacityError> {
        let raw_id = stream_id.get();
        if self.limit_reached.contains(&raw_id) {
            return Ok(None);
        }
        if self
            .open
            .get(&raw_id)
            .is_some_and(|capture| capture.frames.len() >= self.limit.as_frames())
        {
            let Some(capture) = self.open.remove(&raw_id) else {
                return Ok(None);
            };
            let _newly_limited = self.limit_reached.insert(raw_id);
            return Ok(Some(self.complete(
                stream_id,
                capture,
                EndReason::CaptureLimit,
                now,
            )));
        }
        if !self.open.contains_key(&raw_id) {
            self.ensure_capacity(raw_id)?;
        }
        let entry = self
            .open
            .entry(raw_id)
            .or_insert_with(|| StreamCapture::new(now));
        entry.push_frame(seq, frame);
        Ok(None)
    }

    /// Handle a `VoiceEnd`: finalize and return the recording.
    /// Returns `None` for unknown stream ids.
    pub fn on_voice_end(
        &mut self,
        stream_id: StreamId,
        end_reason: EndReason,
        now: DateTime<Utc>,
    ) -> Option<CompletedRecording> {
        let raw_id = stream_id.get();
        if self.limit_reached.remove(&raw_id) {
            return None;
        }
        let capture = self.open.remove(&raw_id)?;
        Some(self.complete(stream_id, capture, end_reason, now))
    }

    /// Finalize every open stream (session drop / shutdown).
    pub fn finalize_all(
        &mut self,
        end_reason: EndReason,
        now: DateTime<Utc>,
    ) -> Vec<CompletedRecording> {
        self.limit_reached.clear();
        let mut done: Vec<CompletedRecording> = Vec::with_capacity(self.open.len());
        let drained: Vec<(u16, StreamCapture)> = self.open.drain().collect();
        for (raw_id, capture) in drained {
            if let Some(stream_id) = StreamId::new(raw_id) {
                done.push(self.complete(stream_id, capture, end_reason, now));
            }
        }
        done
    }

    /// Number of currently open streams.
    #[must_use]
    pub fn active_count(&self) -> usize {
        self.open.len() + self.limit_reached.len()
    }

    fn ensure_capacity(&self, stream_id: u16) -> Result<(), CaptureCapacityError> {
        if self.active_count() >= self.concurrent_limit.get() {
            Err(CaptureCapacityError {
                stream_id,
                max_concurrent: self.concurrent_limit.get(),
            })
        } else {
            Ok(())
        }
    }

    fn complete(
        &self,
        stream_id: StreamId,
        capture: StreamCapture,
        end_reason: EndReason,
        now: DateTime<Utc>,
    ) -> CompletedRecording {
        CompletedRecording {
            origin: self.origin.clone(),
            stream_id,
            header: capture.header,
            header_diagnostics: capture.header_diagnostics,
            started_at: capture.started_at,
            ended_at: now,
            end_reason,
            frames: capture.frames,
            gaps: capture.gaps,
            text: capture.text,
            text_bytes: capture.text_bytes,
            dprs: capture.dprs,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dstar_gateway_core::{Callsign, HeaderFlags, Suffix};

    type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

    fn origin() -> TestResult<StreamOrigin> {
        Ok(StreamOrigin::imported("REF030-C".parse()?))
    }

    fn header() -> DstarHeader {
        DstarHeader {
            flags: HeaderFlags::NONE,
            rpt2: Callsign::from_wire_bytes(*b"REF030 G"),
            rpt1: Callsign::from_wire_bytes(*b"REF030 C"),
            ur_call: Callsign::from_wire_bytes(*b"CQCQCQ  "),
            my_call: Callsign::from_wire_bytes(*b"W1AW    "),
            my_suffix: Suffix::from_wire_bytes(*b"D75 "),
        }
    }

    const fn sid(raw: u16) -> StreamId {
        match StreamId::new(raw) {
            Some(value) => value,
            None => unreachable!(),
        }
    }

    fn frame(seq: u8) -> VoiceFrame {
        VoiceFrame {
            ambe: [seq; 9],
            slow_data: [0x16, 0x29, 0xF5],
        }
    }

    fn limit(seconds: u64) -> TestResult<CaptureDurationLimit> {
        Ok(CaptureDurationLimit::try_from_seconds(seconds)?)
    }

    fn concurrent(count: u64) -> TestResult<ConcurrentCaptureLimit> {
        Ok(ConcurrentCaptureLimit::try_from_count(count)?)
    }

    fn manager() -> TestResult<CaptureManager> {
        Ok(CaptureManager::new(origin()?, limit(60)?, concurrent(4)?))
    }

    fn start(mgr: &mut CaptureManager, stream_id: StreamId) {
        assert!(
            mgr.on_voice_start(stream_id, header(), Vec::new(), t0())
                .is_ok(),
            "ordinary test traffic must remain below the concurrent capture limit"
        );
    }

    fn retain(mgr: &mut CaptureManager, stream_id: StreamId, seq: u8, frame: &VoiceFrame) {
        assert!(
            matches!(mgr.on_voice_frame(stream_id, seq, frame, t0()), Ok(None)),
            "ordinary test traffic must remain below the capture limit"
        );
    }

    fn t0() -> DateTime<Utc> {
        DateTime::<Utc>::UNIX_EPOCH
    }

    #[test]
    fn seq_gap_helper_covers_wrap_dup_and_wild() {
        assert_eq!(seq_gap(0, 1), 0, "adjacent = no gap");
        assert_eq!(seq_gap(1, 3), 1, "one missing");
        assert_eq!(seq_gap(19, 1), 2, "wrap: 20 and 0 missing between 19 and 1");
        assert_eq!(seq_gap(5, 5), 0, "duplicate = no gap (was 20 in dvrec)");
        assert_eq!(seq_gap(5, 200), 0, "out-of-alphabet seq guarded");
        assert_eq!(seq_gap(200, 5), 0, "out-of-alphabet prev guarded");
        assert_eq!(seq_gap(20, 0), 0, "20 -> 0 is adjacent across the wrap");
    }

    #[test]
    fn capture_duration_limit_is_exact_and_rejects_zero() -> TestResult {
        let one_second = limit(1)?;
        assert_eq!(one_second.as_seconds(), 1);
        assert_eq!(one_second.as_frames(), 50);
        assert_eq!(
            CaptureDurationLimit::try_from_seconds(0),
            Err(CaptureLimitError::Zero)
        );
        assert_eq!(
            ConcurrentCaptureLimit::try_from_count(0),
            Err(ConcurrentCaptureLimitError::Zero)
        );
        assert!(CaptureDurationLimit::try_from_seconds(u64::MAX / 50).is_err());
        let maximum = (u64::try_from(usize::MAX)? / 50).min(u64::MAX / 1000);
        assert_eq!(
            CaptureDurationLimit::try_from_seconds(maximum)?.as_seconds(),
            maximum
        );
        assert!(matches!(
            CaptureDurationLimit::try_from_seconds(maximum + 1),
            Err(CaptureLimitError::TooLarge { .. })
        ));
        Ok(())
    }

    #[test]
    fn exact_capture_limit_can_still_end_normally() -> TestResult {
        let mut mgr = CaptureManager::new(origin()?, limit(1)?, concurrent(1)?);
        start(&mut mgr, sid(1));
        for index in 0..50usize {
            let seq = u8::try_from(index % usize::from(SEQ_MODULUS))?;
            retain(&mut mgr, sid(1), seq, &frame(seq));
        }

        let recording = mgr.on_voice_end(sid(1), EndReason::Eot, t0());
        assert!(
            matches!(recording, Some(ref value) if value.frames.len() == 50 && value.end_reason == EndReason::Eot),
            "the configured frame count itself must not be treated as loss: {recording:?}"
        );
        Ok(())
    }

    #[test]
    fn first_excess_frame_finalizes_visible_prefix_and_suppresses_tail() -> TestResult {
        let mut mgr = CaptureManager::new(origin()?, limit(1)?, concurrent(1)?);
        start(&mut mgr, sid(1));
        for index in 0..50usize {
            let seq = u8::try_from(index % usize::from(SEQ_MODULUS))?;
            retain(&mut mgr, sid(1), seq, &frame(seq));
        }

        let overflow = mgr.on_voice_frame(sid(1), 8, &frame(8), t0());
        assert!(
            matches!(overflow, Ok(Some(ref value)) if value.frames.len() == 50 && value.end_reason == EndReason::CaptureLimit),
            "the first unretained frame must return an explicitly partial recording: {overflow:?}"
        );
        assert_eq!(
            mgr.active_count(),
            1,
            "the limited stream remains suppressed"
        );
        assert!(
            matches!(mgr.on_voice_frame(sid(1), 9, &frame(9), t0()), Ok(None)),
            "the tail must not start a second headerless recording"
        );
        start(&mut mgr, sid(1));
        assert!(
            mgr.on_voice_end(sid(1), EndReason::Eot, t0()).is_none(),
            "the real end only clears suppression; the partial recording was already emitted"
        );
        assert_eq!(mgr.active_count(), 0);
        start(&mut mgr, sid(1));
        assert_eq!(
            mgr.active_count(),
            1,
            "a stream id must be reusable after its normal end"
        );
        Ok(())
    }

    #[test]
    fn concurrent_limit_rejects_new_id_and_inactivity_frees_capacity() -> TestResult {
        let mut mgr = CaptureManager::new(origin()?, limit(60)?, concurrent(1)?);
        start(&mut mgr, sid(1));
        let rejected = mgr.on_voice_start(sid(2), header(), Vec::new(), t0());
        assert_eq!(
            rejected,
            Err(CaptureCapacityError {
                stream_id: 2,
                max_concurrent: 1,
            })
        );
        let headerless_rejected = mgr.on_voice_frame(sid(2), 0, &frame(0), t0());
        assert!(
            matches!(
                headerless_rejected,
                Err(CaptureCapacityError {
                    stream_id: 2,
                    max_concurrent: 1,
                })
            ),
            "frames that beat their header must obey the same aggregate bound"
        );
        assert_eq!(mgr.active_count(), 1);

        assert!(
            mgr.on_voice_end(sid(1), EndReason::Inactivity, t0())
                .is_some(),
            "inactivity must finalize the admitted stream"
        );
        start(&mut mgr, sid(2));
        assert_eq!(mgr.active_count(), 1);
        Ok(())
    }

    #[test]
    fn happy_path_assembles_frames_in_order() -> TestResult {
        let mut mgr = manager()?;
        start(&mut mgr, sid(0x04D2));
        for seq in 0..=4u8 {
            retain(&mut mgr, sid(0x04D2), seq, &frame(seq));
        }
        let rec = mgr
            .on_voice_end(sid(0x04D2), EndReason::Eot, t0())
            .ok_or("stream was open")?;
        assert_eq!(rec.frames.len(), 5);
        assert_eq!(rec.gaps, 0);
        assert_eq!(rec.expected_frames(), 5);
        assert!((rec.duration_s() - 0.1).abs() < 1e-9);
        assert_eq!(rec.end_reason, EndReason::Eot);
        assert!(rec.header.is_some());
        assert_eq!(mgr.active_count(), 0);
        Ok(())
    }

    #[test]
    fn seq_gap_is_counted_including_wrap() -> TestResult {
        let mut mgr = manager()?;
        start(&mut mgr, sid(1));
        // seq 18, 19, then jump to 1 (missing 20, 0) = gap 2
        for seq in [18u8, 19, 1] {
            retain(&mut mgr, sid(1), seq, &frame(seq));
        }
        let rec = mgr.on_voice_end(sid(1), EndReason::Eot, t0());
        assert!(
            matches!(rec, Some(ref r) if r.gaps == 2 && r.expected_frames() == 5),
            "got {rec:?}"
        );
        Ok(())
    }

    /// Raw sequence bytes outside the alphabet are retained without inferred gaps.
    #[test]
    fn out_of_alphabet_seq_does_not_panic_or_count_gaps() -> TestResult {
        let mut mgr = manager()?;
        start(&mut mgr, sid(1));
        for seq in [3u8, 200, 3, 0xFF, 4] {
            retain(&mut mgr, sid(1), seq, &frame(seq));
        }
        let rec = mgr.on_voice_end(sid(1), EndReason::Eot, t0());
        assert!(
            matches!(rec, Some(ref r) if r.frames.len() == 5 && r.gaps == 0),
            "wild seq must be archived but never drive gap math: {rec:?}"
        );
        Ok(())
    }

    #[test]
    fn duplicate_seq_is_not_a_gap() -> TestResult {
        let mut mgr = manager()?;
        start(&mut mgr, sid(1));
        for seq in [3u8, 3, 4] {
            retain(&mut mgr, sid(1), seq, &frame(seq));
        }
        let rec = mgr.on_voice_end(sid(1), EndReason::Eot, t0());
        assert!(matches!(rec, Some(ref r) if r.gaps == 0), "got {rec:?}");
        Ok(())
    }

    #[test]
    fn frames_before_header_open_a_capture_and_header_attaches() -> TestResult {
        let mut mgr = manager()?;
        retain(&mut mgr, sid(7), 0, &frame(0));
        retain(&mut mgr, sid(7), 1, &frame(1));
        assert_eq!(mgr.active_count(), 1);
        start(&mut mgr, sid(7));
        retain(&mut mgr, sid(7), 2, &frame(2));
        let rec = mgr.on_voice_end(sid(7), EndReason::Eot, t0());
        assert!(
            matches!(rec, Some(ref r) if r.frames.len() == 3 && r.header.is_some()),
            "header attach must not reset frames: {rec:?}"
        );
        Ok(())
    }

    #[test]
    fn never_headered_stream_completes_headerless() -> TestResult {
        let mut mgr = manager()?;
        retain(&mut mgr, sid(9), 0, &frame(0));
        let rec = mgr.on_voice_end(sid(9), EndReason::Inactivity, t0());
        assert!(
            matches!(rec, Some(ref r) if r.header.is_none()),
            "got {rec:?}"
        );
        Ok(())
    }

    #[test]
    fn interleaved_streams_stay_separate() -> TestResult {
        let mut mgr = manager()?;
        start(&mut mgr, sid(1));
        start(&mut mgr, sid(2));
        retain(&mut mgr, sid(1), 0, &frame(0));
        retain(&mut mgr, sid(2), 0, &frame(0));
        retain(&mut mgr, sid(1), 1, &frame(1));
        assert_eq!(mgr.active_count(), 2);
        let r1 = mgr.on_voice_end(sid(1), EndReason::Eot, t0());
        assert!(
            matches!(r1, Some(ref r) if r.frames.len() == 2),
            "got {r1:?}"
        );
        assert_eq!(mgr.active_count(), 1);
        Ok(())
    }

    #[test]
    fn finalize_all_flushes_open_streams_with_reason() -> TestResult {
        let mut mgr = manager()?;
        start(&mut mgr, sid(1));
        retain(&mut mgr, sid(1), 0, &frame(0));
        start(&mut mgr, sid(2));
        let recs = mgr.finalize_all(EndReason::Shutdown, t0());
        assert_eq!(recs.len(), 2);
        assert!(recs.iter().all(|r| r.end_reason == EndReason::Shutdown));
        assert_eq!(mgr.active_count(), 0);
        Ok(())
    }

    #[test]
    fn end_for_unknown_stream_is_none() -> TestResult {
        let mut mgr = manager()?;
        assert!(mgr.on_voice_end(sid(42), EndReason::Eot, t0()).is_none());
        Ok(())
    }

    #[test]
    fn text_message_is_collected_from_encoded_fragments() -> Result<(), Box<dyn std::error::Error>>
    {
        let mut mgr = manager()?;
        start(&mut mgr, sid(1));
        let message =
            dstar_gateway_core::slowdata::SlowDataTextMessage::try_from_text("Asheville NC")?;
        let fragments = message.encode();
        // Frame 0 is the sync frame; text fragments ride frames 1..
        retain(&mut mgr, sid(1), 0, &frame(0));
        for (i, frag) in fragments.iter().enumerate() {
            let seq = u8::try_from((i + 1) % 21)?;
            let vf = VoiceFrame {
                ambe: [0u8; 9],
                slow_data: *frag,
            };
            retain(&mut mgr, sid(1), seq, &vf);
        }
        let rec = mgr.on_voice_end(sid(1), EndReason::Eot, t0());
        assert!(
            matches!(rec, Some(ref r) if r.text.as_deref() == Some("Asheville NC")),
            "got {rec:?}"
        );
        assert!(
            matches!(rec, Some(ref r) if r.text_bytes == Some(*b"Asheville NC        ")),
            "raw message bytes must be kept losslessly: {rec:?}"
        );
        Ok(())
    }

    #[test]
    fn invalid_first_text_message_is_retained_losslessly() -> Result<(), Box<dyn std::error::Error>>
    {
        use dstar_gateway_core::slowdata::SlowDataTextMessage;

        let mut mgr = manager()?;
        start(&mut mgr, sid(1));
        retain(&mut mgr, sid(1), 0, &frame(0));

        let mut invalid_bytes = *b"FIRST MESSAGE       ";
        invalid_bytes[0] = 0xB6;
        let invalid = SlowDataTextMessage::from_wire_bytes(invalid_bytes);
        let valid = SlowDataTextMessage::try_from_text("later valid")?;
        for (index, fragment) in invalid
            .encode()
            .into_iter()
            .chain(valid.encode())
            .enumerate()
        {
            let seq = u8::try_from(index + 1)?;
            retain(
                &mut mgr,
                sid(1),
                seq,
                &VoiceFrame {
                    ambe: [0; 9],
                    slow_data: fragment,
                },
            );
        }

        let recording = mgr.on_voice_end(sid(1), EndReason::Eot, t0());
        assert!(
            matches!(
                recording,
                Some(ref value)
                    if value.text.is_none() && value.text_bytes == Some(invalid_bytes)
            ),
            "the later valid message replaced the first raw message: {recording:?}"
        );
        Ok(())
    }

    #[test]
    fn dprs_sentence_parses_into_report_with_frame_index() -> TestResult {
        use dstar_gateway_core::dprs::{
            DprsComment, DprsPosition, DprsSource, DprsSymbol, Latitude, Longitude, SymbolCode,
            SymbolTable,
        };
        let sentence = DprsReport::new(
            DprsSource::new("W1AW")?,
            DprsPosition {
                latitude: Latitude::new(35.5)?,
                longitude: Longitude::new(-82.55)?,
            },
            DprsSymbol::new(SymbolTable::Primary, SymbolCode::new('#')?),
            DprsComment::new("/Asheville test")?,
        )
        .to_sentence();
        let mut capture = StreamCapture::new(t0());
        capture.push_frame(0, &frame(0));
        capture.consume_gps_sentence(&sentence);
        assert_eq!(capture.dprs.len(), 1);
        let seen = capture.dprs.first().map(|d| d.at_frame);
        assert_eq!(seen, Some(0));
        let lat = capture.dprs.first().map(|d| d.report.latitude().degrees());
        assert!(lat.is_some_and(|l| (l - 35.5).abs() < 0.001), "got {lat:?}");
        capture.consume_gps_sentence(
            "$$CRC0000,W1AW*>APDPRS,DSTAR*:!3530.00N/08233.00W#/Asheville test",
        );
        assert_eq!(
            capture.dprs.len(),
            1,
            "invalid checksums must not enter metadata"
        );
        Ok(())
    }

    #[test]
    fn non_dprs_gps_is_ignored() {
        let mut capture = StreamCapture::new(t0());
        capture.push_frame(0, &frame(0));
        capture.consume_gps_sentence("$GPGGA,junk,fields*00");
        assert!(capture.dprs.is_empty());
    }
}
