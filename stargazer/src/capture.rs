// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later

//! Pure, I/O-free capture core: assembles session voice events into
//! completed per-transmission recordings.
//!
//! Sans-io discipline: nothing here reads the clock or touches the
//! filesystem — the session shell passes `now` into every event
//! method, and the writer consumes the completed values.

use std::collections::HashMap;
use std::net::SocketAddr;

use chrono::{DateTime, Utc};
use dstar_gateway_core::dprs::{DprsReport, parse_dprs};
use dstar_gateway_core::slowdata::{SlowDataAssembler, SlowDataBlock, SlowDataTextCollector};
use dstar_gateway_core::{DStarHeader, Module, StreamId, VoiceFrame};

/// D-STAR voice seq values cycle 0..=20 (one 21-frame superframe).
const SEQ_MODULUS: u16 = 21;

/// One received voice frame, archived exactly as it arrived.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameRecord {
    /// D-STAR frame sequence number (0..=20).
    pub seq: u8,
    /// 9-byte AMBE voice payload.
    pub ambe: [u8; 9],
    /// 3-byte slow-data payload.
    pub slow_data: [u8; 3],
}

/// Why a recording ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndReason {
    /// Real end-of-transmission packet.
    Eot,
    /// Synthesized after the protocol inactivity window.
    Inactivity,
    /// The session dropped while the stream was open.
    Disconnect,
    /// Stargazer was shut down while the stream was open.
    Shutdown,
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
        }
    }
}

/// Where a stream was received: the configured target plus the
/// resolved peer address, stamped onto every recording.
#[derive(Debug, Clone)]
pub struct StreamOrigin {
    /// Reflector callsign (e.g. `"REF030"`).
    pub reflector: String,
    /// Reflector module linked to.
    pub module: Module,
    /// Protocol name (`"dplus"` / `"dextra"` / `"dcs"`).
    pub protocol: &'static str,
    /// Configured host string.
    pub host: String,
    /// Configured port.
    pub port: u16,
    /// Resolved peer socket address for this connection.
    pub peer: SocketAddr,
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
    pub origin: StreamOrigin,
    /// D-STAR stream id.
    pub stream_id: StreamId,
    /// Decoded voice header, if one was received.
    pub header: Option<DStarHeader>,
    /// Stringified lenient-parse diagnostics from the header.
    pub header_diagnostics: Vec<String>,
    /// Wall clock when the stream opened.
    pub started_at: DateTime<Utc>,
    /// Wall clock when the stream was finalized.
    pub ended_at: DateTime<Utc>,
    /// Why the stream ended.
    pub end_reason: EndReason,
    /// Frames in arrival order.
    pub frames: Vec<FrameRecord>,
    /// Missing frames inferred from seq discontinuities.
    pub gaps: u64,
    /// 20-character D-STAR TX message, trailing spaces trimmed
    /// (lossy UTF-8 view — Japanese radios commonly send JIS X 0201
    /// half-width katakana, which is not UTF-8).
    pub text: Option<String>,
    /// The TX message's raw 20 wire bytes, kept losslessly.
    pub text_bytes: Option<[u8; 20]>,
    /// DPRS position reports in arrival order.
    pub dprs: Vec<DprsSeen>,
}

impl CompletedRecording {
    /// Frames the transmission should have had: received + gaps.
    #[must_use]
    pub fn expected_frames(&self) -> u64 {
        u64::try_from(self.frames.len()).unwrap_or(u64::MAX) + self.gaps
    }

    /// Codec-time duration: expected frames × 20 ms.
    #[must_use]
    pub fn duration_s(&self) -> f64 {
        let frames = u32::try_from(self.expected_frames()).unwrap_or(u32::MAX);
        f64::from(frames) * 0.02
    }
}

/// One in-progress stream.
#[derive(Debug)]
struct StreamCapture {
    header: Option<DStarHeader>,
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
            let distance = (u16::from(seq) + SEQ_MODULUS - u16::from(prev)) % SEQ_MODULUS;
            self.gaps += u64::from(distance.saturating_sub(1));
        }
        self.prev_seq = Some(seq);
        self.frames.push(FrameRecord {
            seq,
            ambe: frame.ambe,
            slow_data: frame.slow_data,
        });

        // Text: the collector wants every frame index — index 0 is
        // its documented reset signal for superframe sync.
        self.text_collector.push(frame.slow_data, seq);
        if self.text.is_none()
            && let Some(msg) = self.text_collector.take_message()
        {
            let s = String::from_utf8_lossy(&msg).trim_end().to_string();
            if !s.is_empty() {
                self.text = Some(s);
                self.text_bytes = Some(msg);
            }
        }

        // GPS/DPRS: sync-frame bytes are not slow data — feeding them
        // would corrupt an in-progress block.
        if seq != 0
            && let Some(SlowDataBlock::Gps(sentence)) = self.assembler.push(frame.slow_data)
        {
            self.consume_gps_sentence(&sentence);
        }
    }

    /// Parse a completed GPS slow-data sentence; keep DPRS reports,
    /// silently skip plain NMEA (common from GPS-equipped radios).
    fn consume_gps_sentence(&mut self, sentence: &str) {
        if let Ok(report) = parse_dprs(sentence) {
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
    open: HashMap<u16, StreamCapture>,
}

impl CaptureManager {
    /// Create a manager for one connection's streams.
    #[must_use]
    pub fn new(origin: StreamOrigin) -> Self {
        Self {
            origin,
            open: HashMap::new(),
        }
    }

    /// Handle a `VoiceStart`: open a capture, or attach the header
    /// to a capture opened by frames that beat the header.
    pub fn on_voice_start(
        &mut self,
        stream_id: StreamId,
        header: DStarHeader,
        diagnostics: Vec<String>,
        now: DateTime<Utc>,
    ) {
        let entry = self
            .open
            .entry(stream_id.get())
            .or_insert_with(|| StreamCapture::new(now));
        // Attach without touching accumulated frames; keep the first
        // header if the core ever re-emits one.
        if entry.header.is_none() {
            entry.header = Some(header);
            entry.header_diagnostics = diagnostics;
        }
    }

    /// Handle a `VoiceFrame`: append to the stream's capture,
    /// opening a headerless capture for an unknown stream id.
    pub fn on_voice_frame(
        &mut self,
        stream_id: StreamId,
        seq: u8,
        frame: &VoiceFrame,
        now: DateTime<Utc>,
    ) {
        let entry = self
            .open
            .entry(stream_id.get())
            .or_insert_with(|| StreamCapture::new(now));
        entry.push_frame(seq, frame);
    }

    /// Handle a `VoiceEnd`: finalize and return the recording.
    /// Returns `None` for unknown stream ids.
    pub fn on_voice_end(
        &mut self,
        stream_id: StreamId,
        end_reason: EndReason,
        now: DateTime<Utc>,
    ) -> Option<CompletedRecording> {
        let capture = self.open.remove(&stream_id.get())?;
        Some(self.complete(stream_id, capture, end_reason, now))
    }

    /// Finalize every open stream (session drop / shutdown).
    pub fn finalize_all(
        &mut self,
        end_reason: EndReason,
        now: DateTime<Utc>,
    ) -> Vec<CompletedRecording> {
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
        self.open.len()
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
    use dstar_gateway_core::{Callsign, Suffix};
    use std::net::{IpAddr, Ipv4Addr};

    fn origin() -> StreamOrigin {
        StreamOrigin {
            reflector: "REF030".to_string(),
            module: Module::C,
            protocol: "dplus",
            host: "ref030.example.org".to_string(),
            port: 20001,
            peer: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 20001),
        }
    }

    fn header() -> DStarHeader {
        DStarHeader {
            flag1: 0,
            flag2: 0,
            flag3: 0,
            rpt2: Callsign::from_wire_bytes(*b"REF030 G"),
            rpt1: Callsign::from_wire_bytes(*b"REF030 C"),
            ur_call: Callsign::from_wire_bytes(*b"CQCQCQ  "),
            my_call: Callsign::from_wire_bytes(*b"W1AW    "),
            my_suffix: Suffix::from_wire_bytes(*b"D75 "),
        }
    }

    fn sid(raw: u16) -> StreamId {
        StreamId::new(raw).unwrap_or_else(|| unreachable!("test stream ids are nonzero"))
    }

    fn frame(seq: u8) -> VoiceFrame {
        VoiceFrame {
            ambe: [seq; 9],
            slow_data: [0x16, 0x29, 0xF5],
        }
    }

    fn t0() -> DateTime<Utc> {
        DateTime::<Utc>::UNIX_EPOCH
    }

    #[test]
    fn happy_path_assembles_frames_in_order() {
        let mut mgr = CaptureManager::new(origin());
        mgr.on_voice_start(sid(0x04D2), header(), Vec::new(), t0());
        for seq in 0..=4u8 {
            mgr.on_voice_frame(sid(0x04D2), seq, &frame(seq), t0());
        }
        let rec = mgr.on_voice_end(sid(0x04D2), EndReason::Eot, t0());
        let Some(rec) = rec else {
            unreachable!("stream was open");
        };
        assert_eq!(rec.frames.len(), 5);
        assert_eq!(rec.gaps, 0);
        assert_eq!(rec.expected_frames(), 5);
        assert!((rec.duration_s() - 0.1).abs() < 1e-9);
        assert_eq!(rec.end_reason, EndReason::Eot);
        assert!(rec.header.is_some());
        assert_eq!(mgr.active_count(), 0);
    }

    #[test]
    fn seq_gap_is_counted_including_wrap() {
        let mut mgr = CaptureManager::new(origin());
        mgr.on_voice_start(sid(1), header(), Vec::new(), t0());
        // seq 18, 19, then jump to 1 (missing 20, 0) = gap 2
        for seq in [18u8, 19, 1] {
            mgr.on_voice_frame(sid(1), seq, &frame(seq), t0());
        }
        let rec = mgr.on_voice_end(sid(1), EndReason::Eot, t0());
        assert!(
            matches!(rec, Some(ref r) if r.gaps == 2 && r.expected_frames() == 5),
            "got {rec:?}"
        );
    }

    #[test]
    fn duplicate_seq_is_not_a_gap() {
        let mut mgr = CaptureManager::new(origin());
        mgr.on_voice_start(sid(1), header(), Vec::new(), t0());
        for seq in [3u8, 3, 4] {
            mgr.on_voice_frame(sid(1), seq, &frame(seq), t0());
        }
        let rec = mgr.on_voice_end(sid(1), EndReason::Eot, t0());
        assert!(matches!(rec, Some(ref r) if r.gaps == 0), "got {rec:?}");
    }

    #[test]
    fn frames_before_header_open_a_capture_and_header_attaches() {
        let mut mgr = CaptureManager::new(origin());
        mgr.on_voice_frame(sid(7), 0, &frame(0), t0());
        mgr.on_voice_frame(sid(7), 1, &frame(1), t0());
        assert_eq!(mgr.active_count(), 1);
        mgr.on_voice_start(sid(7), header(), Vec::new(), t0());
        mgr.on_voice_frame(sid(7), 2, &frame(2), t0());
        let rec = mgr.on_voice_end(sid(7), EndReason::Eot, t0());
        assert!(
            matches!(rec, Some(ref r) if r.frames.len() == 3 && r.header.is_some()),
            "header attach must not reset frames: {rec:?}"
        );
    }

    #[test]
    fn never_headered_stream_completes_headerless() {
        let mut mgr = CaptureManager::new(origin());
        mgr.on_voice_frame(sid(9), 0, &frame(0), t0());
        let rec = mgr.on_voice_end(sid(9), EndReason::Inactivity, t0());
        assert!(
            matches!(rec, Some(ref r) if r.header.is_none()),
            "got {rec:?}"
        );
    }

    #[test]
    fn interleaved_streams_stay_separate() {
        let mut mgr = CaptureManager::new(origin());
        mgr.on_voice_start(sid(1), header(), Vec::new(), t0());
        mgr.on_voice_start(sid(2), header(), Vec::new(), t0());
        mgr.on_voice_frame(sid(1), 0, &frame(0), t0());
        mgr.on_voice_frame(sid(2), 0, &frame(0), t0());
        mgr.on_voice_frame(sid(1), 1, &frame(1), t0());
        assert_eq!(mgr.active_count(), 2);
        let r1 = mgr.on_voice_end(sid(1), EndReason::Eot, t0());
        assert!(
            matches!(r1, Some(ref r) if r.frames.len() == 2),
            "got {r1:?}"
        );
        assert_eq!(mgr.active_count(), 1);
    }

    #[test]
    fn finalize_all_flushes_open_streams_with_reason() {
        let mut mgr = CaptureManager::new(origin());
        mgr.on_voice_start(sid(1), header(), Vec::new(), t0());
        mgr.on_voice_frame(sid(1), 0, &frame(0), t0());
        mgr.on_voice_start(sid(2), header(), Vec::new(), t0());
        let recs = mgr.finalize_all(EndReason::Shutdown, t0());
        assert_eq!(recs.len(), 2);
        assert!(recs.iter().all(|r| r.end_reason == EndReason::Shutdown));
        assert_eq!(mgr.active_count(), 0);
    }

    #[test]
    fn end_for_unknown_stream_is_none() {
        let mut mgr = CaptureManager::new(origin());
        assert!(mgr.on_voice_end(sid(42), EndReason::Eot, t0()).is_none());
    }

    #[test]
    fn text_message_is_collected_from_encoded_fragments() {
        let mut mgr = CaptureManager::new(origin());
        mgr.on_voice_start(sid(1), header(), Vec::new(), t0());
        let fragments = dstar_gateway_core::slowdata::encode_text_message("Asheville NC");
        // Frame 0 is the sync frame; text fragments ride frames 1..
        mgr.on_voice_frame(sid(1), 0, &frame(0), t0());
        for (i, frag) in fragments.iter().enumerate() {
            let seq = u8::try_from((i + 1) % 21).unwrap_or(1);
            let vf = VoiceFrame {
                ambe: [0u8; 9],
                slow_data: *frag,
            };
            mgr.on_voice_frame(sid(1), seq, &vf, t0());
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
    }

    #[test]
    fn dprs_sentence_parses_into_report_with_frame_index() {
        // The parser accepts any 4-hex CRC field (it is parsed, not
        // validated) — same synthesized sentence the dprs module's
        // own tests use.
        let sentence = "$$CRC0000,W1AW    *>APDPRS,DSTAR*:!3530.00N/08233.00W#/Asheville test";
        let mut capture = StreamCapture::new(t0());
        capture.push_frame(0, &frame(0));
        capture.consume_gps_sentence(sentence);
        assert_eq!(capture.dprs.len(), 1);
        let seen = capture.dprs.first().map(|d| d.at_frame);
        assert_eq!(seen, Some(0));
        let lat = capture.dprs.first().map(|d| d.report.latitude.degrees());
        assert!(lat.is_some_and(|l| (l - 35.5).abs() < 0.001), "got {lat:?}");
    }

    #[test]
    fn non_dprs_gps_is_ignored() {
        let mut capture = StreamCapture::new(t0());
        capture.push_frame(0, &frame(0));
        capture.consume_gps_sentence("$GPGGA,junk,fields*00");
        assert!(capture.dprs.is_empty());
    }
}
