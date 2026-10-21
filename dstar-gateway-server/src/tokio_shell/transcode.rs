//! Cross-protocol voice-frame transcoding.
//!
//! When `cross_protocol_forwarding` is enabled in `ReflectorConfig`,
//! a voice frame received on one protocol's endpoint may need to be
//! re-broadcast on a different protocol's endpoint. This module owns
//! the re-encoding step and the `CrossProtocolEvent` broadcast
//! envelope the `Reflector` uses to plumb events between endpoints.
//!
//! The transcoding is NOT loss-free across all protocol/frame type
//! combinations. Specifically:
//!
//! - `DPlus`/`DExtra` header frames encode the same 41-byte D-STAR
//!   header and can be copied directly after protocol-specific
//!   framing.
//! - `DCS` voice frames include the D-STAR header embedded in every
//!   packet; when transcoding `DCS` → `DPlus`/`DExtra`, the
//!   endpoint emits a synthetic header frame on the first `DCS`
//!   packet of a stream and data frames for subsequent packets.
//! - `DPlus`/`DExtra` → `DCS` caches stream-start headers without an
//!   output packet, then embeds the header in every 100-byte voice/end packet.
//!
//! The function is deliberately sans-io: it only touches the
//! caller-supplied scratch buffer and reads from the caller-supplied
//! `VoiceEvent` / cached header. The caller is responsible for
//! maintaining the `StreamCache` that holds cached headers across
//! voice-data packets.

use std::net::SocketAddr;

use dstar_gateway_core::EncodeError;
use dstar_gateway_core::codec::{dcs, dextra, dplus};
use dstar_gateway_core::header::DstarHeader;
use dstar_gateway_core::types::{FrameSequence, Module, ProtocolKind, StreamId};
use dstar_gateway_core::voice::VoiceFrame;

/// Broadcast envelope pushed onto the `Reflector`'s cross-protocol
/// voice bus when `cross_protocol_forwarding` is enabled.
///
/// Each inbound voice frame on any endpoint produces one
/// `CrossProtocolEvent`. Every other endpoint subscribes to the
/// broadcast and re-encodes the event via [`transcode_voice`] in
/// its own protocol before sending to its module members.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CrossProtocolEvent {
    /// Which protocol the source endpoint speaks.
    pub source_protocol: ProtocolKind,
    /// Which peer originated the frame (filtered out of the
    /// recipient list on all endpoints, same as within-protocol
    /// fan-out).
    pub source_peer: SocketAddr,
    /// Which module the originator is on.
    pub module: Module,
    /// The decoded voice event.
    pub event: VoiceEvent,
    /// The cached header for this stream, if any. Required for
    /// `DCS` transcoding but optional for `DPlus`/`DExtra`.
    pub cached_header: Option<DstarHeader>,
}

/// A decoded voice event ready for cross-protocol fan-out.
///
/// The endpoint's inbound path decodes a raw datagram and produces
/// one of these variants for each frame it wants to broadcast to
/// peers on other protocols. The same variant is re-encoded by
/// [`transcode_voice`] into each destination protocol's wire format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum VoiceEvent {
    /// Start of a stream. Requires the D-STAR header.
    StreamStart {
        /// The D-STAR header for this stream.
        header: DstarHeader,
        /// The stream id.
        stream_id: StreamId,
    },
    /// Middle-of-stream voice frame.
    Frame {
        /// The stream id.
        stream_id: StreamId,
        /// Frame sequence number.
        seq: FrameSequence,
        /// 9 bytes AMBE + 3 bytes slow data.
        frame: VoiceFrame,
    },
    /// End of stream.
    StreamEnd {
        /// The stream id.
        stream_id: StreamId,
        /// Final seq value.
        seq: FrameSequence,
    },
}

/// Errors returned by [`transcode_voice`].
#[non_exhaustive]
#[derive(Debug, thiserror::Error)]
pub enum TranscodeError {
    /// Underlying encoder rejected the buffer.
    #[error("encode failed: {0}")]
    Encode(#[from] EncodeError),
    /// `DCS` frame/end encoding requires a cached header because every
    /// combined voice packet embeds it. Stream-start conversion writes nothing.
    #[error("cached header is required for DCS transcoding but was None")]
    MissingCachedHeader,
    /// The target protocol has no implemented voice encoder.
    #[error("unsupported transcode target: {0:?}")]
    UnsupportedProtocol(ProtocolKind),
}

/// Encode a voice event in the given target protocol's wire format.
///
/// For `DCS`, frame/end packets require `cached_header` because the wire packet
/// embeds it. A stream-start writes nothing and returns zero; the caller caches
/// that header for subsequent frames. `DPlus` and `DExtra` use `StreamStart`'s header;
/// their frame and end encoders ignore `cached_header`.
///
/// Returns the number of bytes written into `out`.
///
/// ```
/// use dstar_gateway_core::{FrameSequence, ProtocolKind, StreamId, VoiceFrame};
/// use dstar_gateway_server::{VoiceEvent, transcode_voice};
/// let event = VoiceEvent::Frame {
///     stream_id: StreamId::new(0x1234).ok_or("zero stream")?,
///     seq: FrameSequence::ZERO, frame: VoiceFrame::silence(),
/// };
/// let mut output = [0; 128];
/// assert_eq!(transcode_voice(ProtocolKind::DPlus, &event, None, &mut output)?, 29);
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
///
/// # Errors
///
/// - [`TranscodeError::Encode`] wrapping [`EncodeError::BufferTooSmall`]
///   if `out` is too small for the chosen target protocol's packet
///   size.
/// - [`TranscodeError::MissingCachedHeader`] for a `DCS` frame/end without a cached header.
/// - [`TranscodeError::UnsupportedProtocol`] for a target without an encoder.
pub fn transcode_voice(
    target: ProtocolKind,
    event: &VoiceEvent,
    cached_header: Option<&DstarHeader>,
    out: &mut [u8],
) -> Result<usize, TranscodeError> {
    match target {
        ProtocolKind::DExtra => transcode_dextra(event, out),
        ProtocolKind::DPlus => transcode_dplus(event, out),
        ProtocolKind::Dcs => transcode_dcs(event, cached_header, out),
        _ => Err(TranscodeError::UnsupportedProtocol(target)),
    }
}

fn transcode_dextra(event: &VoiceEvent, out: &mut [u8]) -> Result<usize, TranscodeError> {
    match event {
        VoiceEvent::StreamStart { header, stream_id } => {
            let n = dextra::encode_voice_header(out, *stream_id, header)?;
            Ok(n)
        }
        VoiceEvent::Frame {
            stream_id,
            seq,
            frame,
        } => {
            let n = dextra::encode_voice_data(out, *stream_id, *seq, frame)?;
            Ok(n)
        }
        VoiceEvent::StreamEnd { stream_id, seq } => {
            let n = dextra::encode_voice_eot(out, *stream_id, *seq)?;
            Ok(n)
        }
    }
}

fn transcode_dplus(event: &VoiceEvent, out: &mut [u8]) -> Result<usize, TranscodeError> {
    match event {
        VoiceEvent::StreamStart { header, stream_id } => {
            let n = dplus::encode_voice_header(out, *stream_id, header)?;
            Ok(n)
        }
        VoiceEvent::Frame {
            stream_id,
            seq,
            frame,
        } => {
            let n = dplus::encode_voice_data(out, *stream_id, *seq, frame)?;
            Ok(n)
        }
        VoiceEvent::StreamEnd { stream_id, seq } => {
            let n = dplus::encode_voice_eot(out, *stream_id, *seq)?;
            Ok(n)
        }
    }
}

fn transcode_dcs(
    event: &VoiceEvent,
    cached_header: Option<&DstarHeader>,
    out: &mut [u8],
) -> Result<usize, TranscodeError> {
    // xlxd/cdcsprotocol.cpp::HandleQueue caches headers and emits packets only
    // for voice/end frames. Header conversion cannot insert synthetic audio.
    match event {
        VoiceEvent::StreamStart { .. } => Ok(0),
        VoiceEvent::Frame {
            stream_id,
            seq,
            frame,
        } => {
            let header = cached_header.ok_or(TranscodeError::MissingCachedHeader)?;
            let n = dcs::encode_voice(out, header, *stream_id, *seq, frame, false)?;
            Ok(n)
        }
        VoiceEvent::StreamEnd { stream_id, seq } => {
            let header = cached_header.ok_or(TranscodeError::MissingCachedHeader)?;
            // The encoder sets the sequence byte's 0x40 end bit.
            let frame = VoiceFrame::silence();
            let n = dcs::encode_voice(out, header, *stream_id, *seq, &frame, true)?;
            Ok(n)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dstar_gateway_core::types::{Callsign, Suffix};

    const fn sid() -> StreamId {
        match StreamId::new(0xCAFE) {
            Some(s) => s,
            None => unreachable!(),
        }
    }

    fn test_header() -> DstarHeader {
        DstarHeader {
            flags: dstar_gateway_core::header::HeaderFlags::from_bytes([0; 3]),
            rpt2: Callsign::from_wire_bytes(*b"REF030 G"),
            rpt1: Callsign::from_wire_bytes(*b"REF030 C"),
            ur_call: Callsign::from_wire_bytes(*b"CQCQCQ  "),
            my_call: Callsign::from_wire_bytes(*b"W1AW    "),
            my_suffix: Suffix::EMPTY,
        }
    }

    fn test_frame() -> VoiceFrame {
        VoiceFrame {
            ambe: [0x11; 9],
            slow_data: [0x22; 3],
        }
    }

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn transcode_dextra_to_dplus_header_matches_reference_encoding() -> TestResult {
        let header = test_header();
        let event = VoiceEvent::StreamStart {
            header,
            stream_id: sid(),
        };
        let mut out = [0u8; 128];
        let n = transcode_voice(ProtocolKind::DPlus, &event, Some(&header), &mut out)?;
        assert_eq!(n, 58, "DPlus voice header is 58 bytes");
        // Sanity-check: the output matches the direct DPlus encoder
        // called with the same inputs.
        let mut reference = [0u8; 128];
        let m = dplus::encode_voice_header(&mut reference, sid(), &header)?;
        assert_eq!(m, n);
        assert_eq!(out.get(..n), reference.get(..m));
        Ok(())
    }

    #[test]
    fn transcode_dplus_to_dcs_requires_cached_header() {
        let event = VoiceEvent::Frame {
            stream_id: sid(),
            seq: const { FrameSequence::new(3).unwrap() },
            frame: test_frame(),
        };
        let mut out = [0u8; 128];
        let result = transcode_voice(ProtocolKind::Dcs, &event, None, &mut out);
        assert!(
            matches!(result, Err(TranscodeError::MissingCachedHeader)),
            "expected MissingCachedHeader, got {result:?}"
        );
    }

    #[test]
    fn transcode_dcs_to_dextra_frame_uses_same_ambe_bytes() -> TestResult {
        let frame = test_frame();
        let event = VoiceEvent::Frame {
            stream_id: sid(),
            seq: const { FrameSequence::new(5).unwrap() },
            frame,
        };
        let mut out = [0u8; 128];
        let n = transcode_voice(ProtocolKind::DExtra, &event, None, &mut out)?;
        assert_eq!(n, 27, "DExtra voice data is 27 bytes");
        // AMBE bytes live at [15..24] in DExtra voice data.
        assert_eq!(out.get(15..24), Some(frame.ambe.as_slice()));
        // Slow data at [24..27].
        assert_eq!(out.get(24..27), Some(frame.slow_data.as_slice()));
        // Seq at [14].
        assert_eq!(out.get(14), Some(&5));
        Ok(())
    }

    #[test]
    fn transcode_dplus_to_dextra_frame_preserves_seq_and_stream_id() -> TestResult {
        let frame = test_frame();
        let event = VoiceEvent::Frame {
            stream_id: sid(),
            seq: const { FrameSequence::new(7).unwrap() },
            frame,
        };
        let mut out = [0u8; 128];
        let n = transcode_voice(ProtocolKind::DExtra, &event, None, &mut out)?;
        assert_eq!(n, 27);
        // Stream id at [12..14] little-endian.
        assert_eq!(out.get(12), Some(&0xFE));
        assert_eq!(out.get(13), Some(&0xCA));
        assert_eq!(out.get(14), Some(&7));
        Ok(())
    }

    #[test]
    fn transcode_dextra_to_dcs_frame_embeds_cached_header() -> TestResult {
        let header = test_header();
        let frame = test_frame();
        let event = VoiceEvent::Frame {
            stream_id: sid(),
            seq: const { FrameSequence::new(4).unwrap() },
            frame,
        };
        let mut out = [0u8; 128];
        let n = transcode_voice(ProtocolKind::Dcs, &event, Some(&header), &mut out)?;
        assert_eq!(n, 100, "DCS voice is 100 bytes");
        // Magic at [0..4].
        assert_eq!(out.get(..4), Some(b"0001".as_slice()));
        // MY callsign at [31..39].
        assert_eq!(out.get(31..39), Some(header.my_call.as_bytes().as_slice()));
        // Stream id at [43..45] little-endian.
        assert_eq!(out.get(43), Some(&0xFE));
        assert_eq!(out.get(44), Some(&0xCA));
        // Seq at [45].
        assert_eq!(out.get(45), Some(&4));
        // AMBE at [46..55].
        assert_eq!(out.get(46..55), Some(frame.ambe.as_slice()));
        Ok(())
    }

    #[test]
    fn transcode_dextra_eot_has_0x40_bit_set() -> TestResult {
        let event = VoiceEvent::StreamEnd {
            stream_id: sid(),
            seq: const { FrameSequence::new(20).unwrap() },
        };
        let mut out = [0u8; 128];
        let n = transcode_voice(ProtocolKind::DExtra, &event, None, &mut out)?;
        assert_eq!(n, 27);
        assert_eq!(
            out.get(14).map(|b| b & 0x40),
            Some(0x40),
            "EOT bit set on seq byte"
        );
        Ok(())
    }

    #[test]
    fn transcode_buffer_too_small_is_error() {
        let event = VoiceEvent::Frame {
            stream_id: sid(),
            seq: const { FrameSequence::new(1).unwrap() },
            frame: test_frame(),
        };
        let mut out = [0u8; 4];
        let result = transcode_voice(ProtocolKind::DExtra, &event, None, &mut out);
        assert!(
            matches!(result, Err(TranscodeError::Encode(_))),
            "expected Encode error, got {result:?}"
        );
    }

    #[test]
    fn dcs_stream_start_writes_no_synthetic_voice_packet() -> TestResult {
        let mut output = [0xA5; 128];
        let event = VoiceEvent::StreamStart {
            header: test_header(),
            stream_id: sid(),
        };
        let length = transcode_voice(ProtocolKind::Dcs, &event, None, &mut output)?;
        assert_eq!(length, 0, "DCS has no separate header datagram");
        assert_eq!(
            output, [0xA5; 128],
            "header conversion must leave output untouched"
        );
        Ok(())
    }
}
