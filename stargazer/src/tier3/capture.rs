// StreamCapture and CaptureManager are consumed by the AsyncSession-based
// Tier 3 orchestrator, which is scheduled for Task 13 (connection pool
// management). Until then the types are built and unit-tested in place.
// The `cfg_attr(not(test), ...)` gate avoids an unfulfilled expectation
// in the test build, where the unit tests exercise every item.
#![cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "capture state is wired into the session pool in the follow-up orchestrator task"
    )
)]

//! Voice-stream capture state manager.
//!
//! A D-STAR voice transmission on the wire is a sequence of protocol events:
//!
//! ```text
//! VoiceStart(header) -> VoiceFrame x N -> VoiceEnd
//! ```
//!
//! Each transmission carries two parallel data streams inside the voice frames:
//!
//! - **AMBE voice data** (9 bytes/frame, 50 frames/sec): the compressed
//!   speech audio that will be decoded to PCM and then to MP3.
//!
//! - **Slow data** (3 bytes/frame, 50 frames/sec): a low-bandwidth side
//!   channel carrying things like text messages, DPRS position reports,
//!   and squelch codes. Each logical slow-data block spans several frames
//!   and is reassembled by [`SlowDataAssembler`]. Frame 0 of every 21-frame
//!   superframe is a sync frame (slow-data content `0x555555`), not a data
//!   fragment — the assembler handles the descrambling internally.
//!
//! This module provides two types that encapsulate the per-stream and
//! cross-stream bookkeeping:
//!
//! - [`StreamCapture`] — one in-progress capture: header metadata, all
//!   accumulated AMBE frames, and the slow-data assembler with its
//!   recovered text/DPRS fields.
//!
//! - [`CaptureManager`] — maps stream IDs to active captures, so that
//!   voice frames from multiple concurrent transmissions (different
//!   modules, different reflectors) can be dispatched to the correct
//!   capture without cross-contamination.
//!
//! ## Slow-data assembly details
//!
//! [`dstar_gateway_core::SlowDataAssembler::push`] takes one 3-byte fragment
//! at a time and returns `Some(SlowDataBlock)` when a block completes. For
//! [`SlowDataBlock::Text`], the `.text` field holds the 20-character status
//! message. For [`SlowDataBlock::Gps`], the payload is a raw NMEA-style
//! DPRS sentence that we feed into [`dstar_gateway_core::parse_dprs`] to
//! extract latitude and longitude. All other block kinds (header retx,
//! fast data, squelch, unknown) are ignored for persistence purposes —
//! they aren't part of the streams table schema.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use dstar_gateway::{DStarHeader, StreamId, VoiceFrame};
use dstar_gateway_core::{SlowDataAssembler, SlowDataBlock, parse_dprs};

/// State for one in-progress voice-stream capture.
///
/// Created when a D-STAR `VoiceStart` event arrives (via
/// [`Self::new`], which extracts header metadata). Frames are added via
/// [`Self::push_frame`] as `VoiceFrame` events arrive. When the stream ends
/// (EOT or timeout), the accumulated state is consumed by the Tier 3
/// orchestrator to produce an MP3 and persist a database row.
#[derive(Debug)]
pub(crate) struct StreamCapture {
    /// Reflector callsign this stream was captured from (e.g. `"REF030"`).
    pub(crate) reflector: String,

    /// Module letter (A-Z) the stream was received on.
    pub(crate) module: String,

    /// Protocol name: `"dplus"`, `"dextra"`, or `"dcs"`.
    pub(crate) protocol: String,

    /// D-STAR stream ID (non-zero u16 on wire).
    pub(crate) stream_id: u16,

    /// Operator callsign (`my_call` from the header), ASCII-trimmed.
    pub(crate) callsign: String,

    /// Operator suffix (`my_suffix` from the header), if non-empty after trim.
    pub(crate) suffix: Option<String>,

    /// Destination callsign (`ur_call` from the header), if non-empty after trim.
    pub(crate) ur_call: Option<String>,

    /// AMBE voice frames accumulated in receive order.
    ///
    /// Each frame is 9 bytes; 160 PCM samples will be produced per frame
    /// by `mbelib-rs`. The order is load-bearing: the AMBE decoder uses
    /// inter-frame delta prediction.
    pub(crate) ambe_frames: Vec<[u8; 9]>,

    /// Stateful slow-data reassembler.
    ///
    /// Fed one 3-byte fragment per voice frame via
    /// [`SlowDataAssembler::push`]. Emits [`SlowDataBlock`] values when
    /// complete blocks arrive; we consume those immediately to update
    /// `dstar_text` and `dprs_lat`/`dprs_lon`.
    slow_data: SlowDataAssembler,

    /// Latest slow-data text message decoded from this stream, if any.
    pub(crate) dstar_text: Option<String>,

    /// DPRS latitude in decimal degrees, if a DPRS sentence was decoded.
    pub(crate) dprs_lat: Option<f64>,

    /// DPRS longitude in decimal degrees, if a DPRS sentence was decoded.
    pub(crate) dprs_lon: Option<f64>,

    /// When the voice header arrived (UTC wall-clock time).
    pub(crate) started_at: DateTime<Utc>,
}

impl StreamCapture {
    /// Creates a new capture from the D-STAR voice header.
    ///
    /// Extracts `my_call`, `my_suffix`, and `ur_call` from the header,
    /// trimming ASCII whitespace and dropping empty strings. The header's
    /// RPT1/RPT2 fields are not persisted — they are routing metadata and
    /// are redundant with the `reflector`/`module` arguments.
    pub(crate) fn new(
        reflector: String,
        module: String,
        protocol: String,
        stream_id: StreamId,
        header: &DStarHeader,
    ) -> Self {
        // Callsign and suffix are fixed-width, space-padded on the wire.
        // Trim trailing whitespace for persistence; drop to None if empty.
        let my_call_raw = header.my_call.as_str();
        let callsign = my_call_raw.trim_end().to_string();

        let my_suffix_raw = header.my_suffix.as_str();
        let suffix_trimmed = my_suffix_raw.trim_end().to_string();
        let suffix = if suffix_trimmed.is_empty() {
            None
        } else {
            Some(suffix_trimmed)
        };

        let ur_call_raw = header.ur_call.as_str();
        let ur_trimmed = ur_call_raw.trim_end().to_string();
        let ur_call = if ur_trimmed.is_empty() {
            None
        } else {
            Some(ur_trimmed)
        };

        Self {
            reflector,
            module,
            protocol,
            stream_id: stream_id.get(),
            callsign,
            suffix,
            ur_call,
            ambe_frames: Vec::new(),
            slow_data: SlowDataAssembler::new(),
            dstar_text: None,
            dprs_lat: None,
            dprs_lon: None,
            started_at: Utc::now(),
        }
    }

    /// Appends one voice frame to the capture.
    ///
    /// The 9-byte AMBE payload is stored verbatim for later mbelib-rs
    /// decoding. The 3-byte slow-data fragment is fed to the assembler,
    /// which may emit a complete [`SlowDataBlock`] after enough fragments
    /// arrive. Text and GPS blocks are retained (last-write-wins if the
    /// transmission contains multiple); other block kinds are ignored.
    pub(crate) fn push_frame(&mut self, frame: &VoiceFrame) {
        // Store the AMBE voice payload in receive order.
        self.ambe_frames.push(frame.ambe);

        // Feed the slow-data fragment and process any emitted block.
        if let Some(block) = self.slow_data.push(frame.slow_data) {
            self.consume_slow_data(block);
        }
    }

    /// Processes a single assembled slow-data block, updating capture fields.
    ///
    /// - `Text`: store the trimmed text message as `dstar_text`.
    /// - `Gps`: parse the DPRS sentence and store lat/lon if valid; DPRS
    ///   parse failures are non-fatal (real-world slow data often contains
    ///   malformed GPS payloads from non-GPS-equipped radios).
    /// - All other variants (`HeaderRetx`, `FastData`, `Squelch`,
    ///   `Unknown`): ignored — not part of the streams table.
    fn consume_slow_data(&mut self, block: SlowDataBlock) {
        match block {
            SlowDataBlock::Text(text) => {
                // The assembler already trimmed trailing spaces/nulls.
                // Discard if completely empty after trim.
                if !text.text.is_empty() {
                    self.dstar_text = Some(text.text);
                }
            }
            SlowDataBlock::Gps(sentence) => {
                // Non-DPRS GPS slow-data is common (raw NMEA passthrough
                // from GPS-capable radios). `parse_dprs` only succeeds on
                // `$$CRC`-prefixed DPRS sentences; other GPS payloads are
                // silently dropped.
                if let Ok(report) = parse_dprs(&sentence) {
                    self.dprs_lat = Some(report.latitude.degrees());
                    self.dprs_lon = Some(report.longitude.degrees());
                }
            }
            // Header retx, fast data, squelch, unknown — none are part
            // of the streams table schema. `SlowDataBlock` is marked
            // `#[non_exhaustive]`; the wildcard also catches any future
            // block kinds added upstream.
            _ => {}
        }
    }

    /// Returns the number of voice frames captured so far.
    pub(crate) const fn frame_count(&self) -> usize {
        self.ambe_frames.len()
    }
}

/// Manages multiple concurrent voice-stream captures.
///
/// A single Tier 3 connection can multiplex voice streams across modules
/// (e.g. a reflector with modules A, B, C each carrying an independent
/// transmission at the same time). The `CaptureManager` maps each wire
/// stream ID to its in-progress [`StreamCapture`] so that frames are
/// routed correctly.
///
/// # Lifecycle
///
/// 1. [`Self::start`] — called on `VoiceStart`: registers a new capture
///    keyed by the stream ID from the header.
/// 2. [`Self::push_frame`] — called on `VoiceFrame`: appends the frame to
///    the matching capture (no-op if the stream ID is unknown, which can
///    happen if we missed the header).
/// 3. [`Self::end`] — called on `VoiceEnd`: removes and returns the
///    capture for finalization (MP3 encode + database insert).
#[derive(Debug, Default)]
pub(crate) struct CaptureManager {
    /// Active captures keyed by the wire stream ID.
    active: HashMap<u16, StreamCapture>,
}

impl CaptureManager {
    /// Creates an empty manager with no active captures.
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Registers a new capture. Replaces any existing entry for the same
    /// stream ID (a fresh header always supersedes any partial state).
    pub(crate) fn start(&mut self, capture: StreamCapture) {
        let _existing = self.active.insert(capture.stream_id, capture);
    }

    /// Appends a voice frame to the capture matching `stream_id`.
    ///
    /// No-op (logs a debug message) if no capture is registered for this
    /// stream ID. This can happen if we joined the wire mid-stream and
    /// missed the header, or if a stale frame arrives after we already
    /// processed the `VoiceEnd`.
    pub(crate) fn push_frame(&mut self, stream_id: u16, frame: &VoiceFrame) {
        if let Some(capture) = self.active.get_mut(&stream_id) {
            capture.push_frame(frame);
        } else {
            tracing::debug!(stream_id, "voice frame for unknown stream — dropping");
        }
    }

    /// Finalizes and removes the capture matching `stream_id`.
    ///
    /// Returns the captured state for downstream processing (MP3 encode,
    /// database insert). Returns `None` if no capture was registered for
    /// this stream ID.
    pub(crate) fn end(&mut self, stream_id: u16) -> Option<StreamCapture> {
        self.active.remove(&stream_id)
    }

    /// Returns the number of captures currently in progress.
    ///
    /// Used for observability (metrics exported on the HTTP API) and for
    /// backpressure decisions in the orchestrator.
    pub(crate) fn active_count(&self) -> usize {
        self.active.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use dstar_gateway_core::{Callsign, Suffix};

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    /// Builds a minimal `DStarHeader` for tests.
    fn test_header() -> DStarHeader {
        DStarHeader {
            flag1: 0x00,
            flag2: 0x00,
            flag3: 0x00,
            rpt2: Callsign::from_wire_bytes(*b"REF030 G"),
            rpt1: Callsign::from_wire_bytes(*b"REF030 C"),
            ur_call: Callsign::from_wire_bytes(*b"CQCQCQ  "),
            my_call: Callsign::from_wire_bytes(*b"W1AW    "),
            my_suffix: Suffix::from_wire_bytes(*b"D75 "),
        }
    }

    fn stream_id(raw: u16) -> StreamId {
        // Tests only use non-zero IDs; fall back to 1 for safety.
        // `StreamId::new(1)` is const-safe and never returns None.
        const ONE: StreamId = match StreamId::new(1) {
            Some(s) => s,
            None => unreachable!(),
        };
        StreamId::new(raw).unwrap_or(ONE)
    }

    fn make_capture(raw_id: u16) -> StreamCapture {
        StreamCapture::new(
            "REF030".to_string(),
            "C".to_string(),
            "dplus".to_string(),
            stream_id(raw_id),
            &test_header(),
        )
    }

    #[test]
    fn new_extracts_callsign_and_trims_suffix() {
        let cap = make_capture(0x1234);
        assert_eq!(cap.callsign, "W1AW", "trailing spaces trimmed");
        assert_eq!(cap.suffix.as_deref(), Some("D75"), "suffix extracted");
        assert_eq!(cap.ur_call.as_deref(), Some("CQCQCQ"), "ur_call extracted");
        assert_eq!(cap.stream_id, 0x1234);
        assert_eq!(cap.reflector, "REF030");
        assert_eq!(cap.module, "C");
        assert_eq!(cap.protocol, "dplus");
        assert_eq!(cap.frame_count(), 0);
    }

    #[test]
    fn new_handles_empty_suffix() {
        let header = DStarHeader {
            my_suffix: Suffix::from_wire_bytes(*b"    "),
            ..test_header()
        };
        let cap = StreamCapture::new(
            "REF030".to_string(),
            "C".to_string(),
            "dplus".to_string(),
            stream_id(1),
            &header,
        );
        assert!(cap.suffix.is_none(), "all-space suffix becomes None");
    }

    #[test]
    fn push_frame_accumulates_ambe_and_increments_count() -> TestResult {
        let mut cap = make_capture(0x1111);
        let frame = VoiceFrame::silence();
        cap.push_frame(&frame);
        cap.push_frame(&frame);
        cap.push_frame(&frame);
        assert_eq!(cap.frame_count(), 3);
        assert_eq!(cap.ambe_frames.len(), 3);
        let first_frame = cap.ambe_frames.first().ok_or("no first frame")?;
        assert_eq!(*first_frame, frame.ambe);
        Ok(())
    }

    #[test]
    fn manager_new_is_empty() {
        let mgr = CaptureManager::new();
        assert_eq!(mgr.active_count(), 0);
    }

    #[test]
    fn manager_start_push_end_lifecycle() -> TestResult {
        let mut mgr = CaptureManager::new();

        // Start a capture.
        let capture = make_capture(0xAABB);
        mgr.start(capture);
        assert_eq!(mgr.active_count(), 1);

        // Push a few frames.
        let frame = VoiceFrame::silence();
        mgr.push_frame(0xAABB, &frame);
        mgr.push_frame(0xAABB, &frame);
        mgr.push_frame(0xAABB, &frame);

        // End the capture and verify frames accumulated.
        let finalized = mgr.end(0xAABB).ok_or("capture was removed during push")?;
        assert_eq!(finalized.frame_count(), 3);
        assert_eq!(finalized.stream_id, 0xAABB);
        assert_eq!(mgr.active_count(), 0, "manager empty after end");
        Ok(())
    }

    #[test]
    fn manager_push_to_unknown_stream_is_noop() {
        let mut mgr = CaptureManager::new();
        let frame = VoiceFrame::silence();
        // No active captures; this should just log and return.
        mgr.push_frame(0xDEAD, &frame);
        assert_eq!(mgr.active_count(), 0);
    }

    #[test]
    fn manager_end_on_unknown_stream_returns_none() {
        let mut mgr = CaptureManager::new();
        assert!(mgr.end(0xFFFE).is_none());
    }

    #[test]
    fn manager_handles_multiple_concurrent_streams() -> TestResult {
        let mut mgr = CaptureManager::new();
        mgr.start(make_capture(0x0001));
        mgr.start(make_capture(0x0002));
        mgr.start(make_capture(0x0003));
        assert_eq!(mgr.active_count(), 3);

        let frame = VoiceFrame::silence();
        mgr.push_frame(0x0001, &frame);
        mgr.push_frame(0x0002, &frame);
        mgr.push_frame(0x0002, &frame);
        mgr.push_frame(0x0003, &frame);
        mgr.push_frame(0x0003, &frame);
        mgr.push_frame(0x0003, &frame);

        let cap1 = mgr.end(0x0001).ok_or("cap1 missing")?;
        let cap2 = mgr.end(0x0002).ok_or("cap2 missing")?;
        let cap3 = mgr.end(0x0003).ok_or("cap3 missing")?;
        assert_eq!(cap1.frame_count(), 1);
        assert_eq!(cap2.frame_count(), 2);
        assert_eq!(cap3.frame_count(), 3);
        assert_eq!(mgr.active_count(), 0);
        Ok(())
    }

    #[test]
    fn manager_start_replaces_existing_stream() -> TestResult {
        let mut mgr = CaptureManager::new();
        let mut cap = make_capture(0x1000);
        cap.ambe_frames.push([1; 9]);
        mgr.start(cap);
        // A fresh header for the same stream ID should reset the capture.
        mgr.start(make_capture(0x1000));
        assert_eq!(mgr.active_count(), 1);
        // The new capture starts with zero frames.
        let cap = mgr
            .end(0x1000)
            .ok_or("capture registered via second start")?;
        assert_eq!(cap.frame_count(), 0);
        Ok(())
    }
}
