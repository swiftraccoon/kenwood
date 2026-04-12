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
//!   packet — when transcoding `DCS` → `DPlus`/`DExtra`, the
//!   endpoint emits a synthetic header frame on the first `DCS`
//!   packet of a stream and data frames for subsequent packets.
//! - `DPlus`/`DExtra` → `DCS` requires building a 100-byte DCS
//!   voice frame for every data packet, with the cached header
//!   embedded.
//!
//! The function is deliberately sans-io: it only touches the
//! caller-supplied scratch buffer and reads from the caller-supplied
//! `VoiceEvent` / cached header. The caller is responsible for
//! maintaining the `StreamCache` that holds cached headers across
//! voice-data packets.

use std::net::SocketAddr;

use dstar_gateway_core::EncodeError;
use dstar_gateway_core::codec::{dcs, dextra, dplus};
use dstar_gateway_core::header::DStarHeader;
use dstar_gateway_core::types::{Module, ProtocolKind, StreamId};
use dstar_gateway_core::voice::VoiceFrame;

/// Broadcast envelope pushed onto the `Reflector`'s cross-protocol
/// voice bus when `cross_protocol_forwarding` is enabled.
///
/// Each inbound voice frame on any endpoint produces one
/// `CrossProtocolEvent`. Every other endpoint subscribes to the
/// broadcast and re-encodes the event via [`transcode_voice`] in
/// its own protocol before sending to its module members.
#[derive(Debug, Clone)]
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
    pub cached_header: Option<DStarHeader>,
}

/// A decoded voice event ready for cross-protocol fan-out.
///
/// The endpoint's inbound path decodes a raw datagram and produces
/// one of these variants for each frame it wants to broadcast to
/// peers on other protocols. The same variant is re-encoded by
/// [`transcode_voice`] into each destination protocol's wire format.
#[derive(Debug, Clone)]
pub enum VoiceEvent {
    /// Start of a stream. Requires the D-STAR header.
    StreamStart {
        /// The D-STAR header for this stream.
        header: DStarHeader,
        /// The stream id.
        stream_id: StreamId,
    },
    /// Middle-of-stream voice frame.
    Frame {
        /// The stream id.
        stream_id: StreamId,
        /// Frame sequence number.
        seq: u8,
        /// 9 bytes AMBE + 3 bytes slow data.
        frame: VoiceFrame,
    },
    /// End of stream.
    StreamEnd {
        /// The stream id.
        stream_id: StreamId,
        /// Final seq value.
        seq: u8,
    },
}

/// Errors returned by [`transcode_voice`].
#[non_exhaustive]
#[derive(Debug, thiserror::Error)]
pub enum TranscodeError {
    /// Underlying encoder rejected the buffer.
    #[error("encode failed: {0}")]
    Encode(#[from] EncodeError),
    /// `DCS` encoding requires the header to be cached on every
    /// frame (including `Frame` and `StreamEnd` variants) because
    /// every `DCS` voice packet embeds the header. The caller must
    /// pass `Some` for every `DCS` transcode.
    #[error("cached header is required for DCS transcoding but was None")]
    MissingCachedHeader,
}

/// Encode a voice event in the given target protocol's wire format.
///
/// For `DCS`, `cached_header` must be `Some` on stream-start AND on
/// every subsequent frame (because each `DCS` voice packet embeds
/// the header). For `DPlus` and `DExtra`, `cached_header` is only
/// consulted on the header frame itself (and on `StreamEnd` where
/// the downstream protocol still needs the header for its own
/// framing on some encoders — currently unused, but kept for
/// symmetry).
///
/// Returns the number of bytes written into `out`.
///
/// # Errors
///
/// - [`TranscodeError::Encode`] wrapping [`EncodeError::BufferTooSmall`]
///   if `out` is too small for the chosen target protocol's packet
///   size.
/// - [`TranscodeError::MissingCachedHeader`] if `target == Dcs` and
///   `cached_header` is `None`.
pub fn transcode_voice(
    target: ProtocolKind,
    event: &VoiceEvent,
    cached_header: Option<&DStarHeader>,
    out: &mut [u8],
) -> Result<usize, TranscodeError> {
    // `ProtocolKind` is `#[non_exhaustive]`; cover every known
    // variant explicitly and fall through to `BufferTooSmall` with
    // a zero-byte write for any hypothetical future variant we
    // don't yet know how to encode.
    match target {
        ProtocolKind::DExtra => transcode_dextra(event, out),
        ProtocolKind::DPlus => transcode_dplus(event, out),
        ProtocolKind::Dcs => transcode_dcs(event, cached_header, out),
        _ => Err(TranscodeError::Encode(EncodeError::BufferTooSmall {
            need: 0,
            have: 0,
        })),
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
    cached_header: Option<&DStarHeader>,
    out: &mut [u8],
) -> Result<usize, TranscodeError> {
    // DCS is special — every voice packet embeds the header, so
    // `cached_header` is required for every `VoiceEvent` variant.
    let header = cached_header.ok_or(TranscodeError::MissingCachedHeader)?;
    match event {
        VoiceEvent::StreamStart { stream_id, .. } => {
            // Build a "first frame" packet. We use silence payload
            // because the header packet itself on `DCS` doesn't
            // carry an AMBE frame — the stream starts with a
            // regular voice frame. The caller that converts a
            // `StreamStart` into a DCS packet should really follow
            // it with a real frame; this branch exists so the
            // transcoder is total over the variant set.
            let frame = VoiceFrame::silence();
            let n = dcs::encode_voice(out, header, *stream_id, 0, &frame, false)?;
            Ok(n)
        }
        VoiceEvent::Frame {
            stream_id,
            seq,
            frame,
        } => {
            let n = dcs::encode_voice(out, header, *stream_id, *seq, frame, false)?;
            Ok(n)
        }
        VoiceEvent::StreamEnd { stream_id, seq } => {
            // DCS EOT is signaled in the slow_data bytes — the
            // encoder handles that when `is_end = true`. We pass a
            // silence frame because the EOT packet's AMBE payload
            // is conventionally silence.
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

    fn test_header() -> DStarHeader {
        DStarHeader {
            flag1: 0,
            flag2: 0,
            flag3: 0,
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
        assert_eq!(&out[..n], &reference[..m]);
        Ok(())
    }

    #[test]
    fn transcode_dplus_to_dcs_requires_cached_header() {
        let event = VoiceEvent::Frame {
            stream_id: sid(),
            seq: 3,
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
            seq: 5,
            frame,
        };
        let mut out = [0u8; 128];
        let n = transcode_voice(ProtocolKind::DExtra, &event, None, &mut out)?;
        assert_eq!(n, 27, "DExtra voice data is 27 bytes");
        // AMBE bytes live at [15..24] in DExtra voice data.
        assert_eq!(&out[15..24], &frame.ambe);
        // Slow data at [24..27].
        assert_eq!(&out[24..27], &frame.slow_data);
        // Seq at [14].
        assert_eq!(out[14], 5);
        Ok(())
    }

    #[test]
    fn transcode_dplus_to_dextra_frame_preserves_seq_and_stream_id() -> TestResult {
        let frame = test_frame();
        let event = VoiceEvent::Frame {
            stream_id: sid(),
            seq: 7,
            frame,
        };
        let mut out = [0u8; 128];
        let n = transcode_voice(ProtocolKind::DExtra, &event, None, &mut out)?;
        assert_eq!(n, 27);
        // Stream id at [12..14] little-endian.
        assert_eq!(out[12], 0xFE);
        assert_eq!(out[13], 0xCA);
        assert_eq!(out[14], 7);
        Ok(())
    }

    #[test]
    fn transcode_dextra_to_dcs_frame_embeds_cached_header() -> TestResult {
        let header = test_header();
        let frame = test_frame();
        let event = VoiceEvent::Frame {
            stream_id: sid(),
            seq: 4,
            frame,
        };
        let mut out = [0u8; 128];
        let n = transcode_voice(ProtocolKind::Dcs, &event, Some(&header), &mut out)?;
        assert_eq!(n, 100, "DCS voice is 100 bytes");
        // Magic at [0..4].
        assert_eq!(&out[..4], b"0001");
        // MY callsign at [31..39].
        assert_eq!(&out[31..39], header.my_call.as_bytes());
        // Stream id at [43..45] little-endian.
        assert_eq!(out[43], 0xFE);
        assert_eq!(out[44], 0xCA);
        // Seq at [45].
        assert_eq!(out[45], 4);
        // AMBE at [46..55].
        assert_eq!(&out[46..55], &frame.ambe);
        Ok(())
    }

    #[test]
    fn transcode_dextra_eot_has_0x40_bit_set() -> TestResult {
        let event = VoiceEvent::StreamEnd {
            stream_id: sid(),
            seq: 20,
        };
        let mut out = [0u8; 128];
        let n = transcode_voice(ProtocolKind::DExtra, &event, None, &mut out)?;
        assert_eq!(n, 27);
        assert_eq!(out[14] & 0x40, 0x40, "EOT bit set on seq byte");
        Ok(())
    }

    #[test]
    fn transcode_buffer_too_small_is_error() {
        let event = VoiceEvent::Frame {
            stream_id: sid(),
            seq: 1,
            frame: test_frame(),
        };
        let mut out = [0u8; 4];
        let result = transcode_voice(ProtocolKind::DExtra, &event, None, &mut out);
        assert!(
            matches!(result, Err(TranscodeError::Encode(_))),
            "expected Encode error, got {result:?}"
        );
    }
}
