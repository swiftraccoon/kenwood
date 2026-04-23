#![expect(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "Proptest `prop_assert!` / closure bodies cannot use `?` to unwrap `Result` \
              or `Option`, so `.unwrap()` on known-valid constructor outputs and direct \
              `buf[..n]` slicing on fixed-size decoded byte arrays are structurally \
              required. `clippy::unwrap_used` fires on those unwraps; \
              `clippy::indexing_slicing` fires on the slice expressions. Both are safe \
              because the proptest strategies generate inputs that are guaranteed valid \
              by construction, and any failure would correctly panic the test."
)]
//! Property tests for `DPlus` codec round-trips.
//!
//! Two flavours:
//! 1. Round-trip properties — encode a valid input, decode it, assert
//!    the decoded value matches the original. Exercises every encoder
//!    against its matching decoder.
//! 2. Never-panic properties — throw random bytes at the decoders and
//!    `parse_auth_response` to prove they cannot panic on any input.
//!
//! Together these give ~27,000 generated cases per full run.

// Integration tests are separate compilation units and re-evaluate
// workspace deps. Suppress `unused_crate_dependencies` for deps that
// are only used transitively or not at all by this test file.
use static_assertions as _;
use thiserror as _;
use tracing as _;
use trybuild as _;

use dstar_gateway_core::codec::dplus::{
    ClientPacket, Link2Result, ServerPacket, decode_client_to_server, decode_server_to_client,
    encode_link1, encode_link2, encode_link2_reply, encode_poll, encode_unlink, encode_voice_data,
    encode_voice_eot, encode_voice_header,
};
use dstar_gateway_core::validator::NullSink;
use dstar_gateway_core::{Callsign, DStarHeader, StreamId, Suffix, VoiceFrame};
use proptest::prelude::*;

prop_compose! {
    fn any_callsign()(s in "[A-Z0-9]{1,8}") -> Callsign {
        // Strategy regex guarantees valid callsign characters.
        Callsign::try_from_str(&s).unwrap()
    }
}

prop_compose! {
    fn any_stream_id()(n in 1u16..=u16::MAX) -> StreamId {
        // Strategy range starts at 1, always non-zero.
        StreamId::new(n).unwrap()
    }
}

prop_compose! {
    fn any_voice_frame()(
        ambe in any::<[u8; 9]>(),
        slow in any::<[u8; 3]>(),
    ) -> VoiceFrame {
        VoiceFrame { ambe, slow_data: slow }
    }
}

const fn ref030_header(my_call: Callsign) -> DStarHeader {
    DStarHeader {
        flag1: 0,
        flag2: 0,
        flag3: 0,
        rpt2: Callsign::from_wire_bytes(*b"REF030 G"),
        rpt1: Callsign::from_wire_bytes(*b"REF030 C"),
        ur_call: Callsign::from_wire_bytes(*b"CQCQCQ  "),
        my_call,
        my_suffix: Suffix::EMPTY,
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(1000))]

    // ─── Fixed-byte encoders round-trip ──────────────────────
    #[test]
    fn link1_client_roundtrips(_ in 0u8..1) {
        let mut buf = [0u8; 16];
        let n = encode_link1(&mut buf).unwrap();
        let pkt = decode_client_to_server(&buf[..n], &mut NullSink).unwrap();
        prop_assert!(matches!(pkt, ClientPacket::Link1));
    }

    #[test]
    fn unlink_client_roundtrips(_ in 0u8..1) {
        let mut buf = [0u8; 16];
        let n = encode_unlink(&mut buf).unwrap();
        let pkt = decode_client_to_server(&buf[..n], &mut NullSink).unwrap();
        prop_assert!(matches!(pkt, ClientPacket::Unlink));
    }

    #[test]
    fn poll_client_roundtrips(_ in 0u8..1) {
        let mut buf = [0u8; 16];
        let n = encode_poll(&mut buf).unwrap();
        let pkt = decode_client_to_server(&buf[..n], &mut NullSink).unwrap();
        prop_assert!(matches!(pkt, ClientPacket::Poll));
    }

    #[test]
    fn poll_echo_server_roundtrips(_ in 0u8..1) {
        let mut buf = [0u8; 16];
        let n = encode_poll(&mut buf).unwrap();
        let pkt = decode_server_to_client(&buf[..n], &mut NullSink).unwrap();
        prop_assert!(matches!(pkt, ServerPacket::PollEcho));
    }

    // ─── Link2 with variable callsign ────────────────────────
    #[test]
    fn link2_client_roundtrips(cs in any_callsign()) {
        let mut buf = [0u8; 32];
        let n = encode_link2(&mut buf, &cs).unwrap();
        let pkt = decode_client_to_server(&buf[..n], &mut NullSink).unwrap();
        match pkt {
            ClientPacket::Link2 { callsign } => prop_assert_eq!(callsign, cs),
            _ => prop_assert!(false, "expected Link2"),
        }
    }

    #[test]
    fn link2_reply_accept_server_roundtrips(_ in 0u8..1) {
        let mut buf = [0u8; 16];
        let n = encode_link2_reply(&mut buf, Link2Result::Accept).unwrap();
        let pkt = decode_server_to_client(&buf[..n], &mut NullSink).unwrap();
        match pkt {
            ServerPacket::Link2Reply { result } => {
                prop_assert_eq!(result, Link2Result::Accept);
            }
            _ => prop_assert!(false, "expected Link2Reply"),
        }
    }

    #[test]
    fn link2_reply_busy_server_roundtrips(_ in 0u8..1) {
        let mut buf = [0u8; 16];
        let n = encode_link2_reply(&mut buf, Link2Result::Busy).unwrap();
        let pkt = decode_server_to_client(&buf[..n], &mut NullSink).unwrap();
        match pkt {
            ServerPacket::Link2Reply { result } => {
                prop_assert_eq!(result, Link2Result::Busy);
            }
            _ => prop_assert!(false, "expected Link2Reply"),
        }
    }

    // ─── Voice frame round-trips ─────────────────────────────
    #[test]
    fn voice_data_server_roundtrips(
        sid in any_stream_id(),
        seq in 0u8..21,
        frame in any_voice_frame(),
    ) {
        let mut buf = [0u8; 64];
        let n = encode_voice_data(&mut buf, sid, seq, &frame).unwrap();
        let pkt = decode_server_to_client(&buf[..n], &mut NullSink).unwrap();
        match pkt {
            ServerPacket::VoiceData { stream_id, seq: s, frame: f } => {
                prop_assert_eq!(stream_id, sid);
                prop_assert_eq!(s, seq);
                prop_assert_eq!(f.ambe, frame.ambe);
                prop_assert_eq!(f.slow_data, frame.slow_data);
            }
            _ => prop_assert!(false, "expected VoiceData"),
        }
    }

    #[test]
    fn voice_eot_server_roundtrips(sid in any_stream_id(), seq in 0u8..21) {
        let mut buf = [0u8; 64];
        let n = encode_voice_eot(&mut buf, sid, seq).unwrap();
        let pkt = decode_server_to_client(&buf[..n], &mut NullSink).unwrap();
        match pkt {
            ServerPacket::VoiceEot { stream_id, seq: s } => {
                prop_assert_eq!(stream_id, sid);
                prop_assert_eq!(s & 0x3F, seq, "low bits preserve seq");
                prop_assert!(s & 0x40 != 0, "EOT bit set");
            }
            _ => prop_assert!(false, "expected VoiceEot"),
        }
    }

    #[test]
    fn voice_header_server_roundtrips(sid in any_stream_id(), my_call in any_callsign()) {
        let header = ref030_header(my_call);
        let mut buf = [0u8; 64];
        let n = encode_voice_header(&mut buf, sid, &header).unwrap();
        let pkt = decode_server_to_client(&buf[..n], &mut NullSink).unwrap();
        match pkt {
            ServerPacket::VoiceHeader { stream_id, header: decoded } => {
                prop_assert_eq!(stream_id, sid);
                prop_assert_eq!(decoded.my_call, my_call);
            }
            _ => prop_assert!(false, "expected VoiceHeader"),
        }
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(5000))]

    #[test]
    fn decode_server_to_client_never_panics(
        data in proptest::collection::vec(any::<u8>(), 0..2048),
    ) {
        // `Result<ServerPacket, DPlusError>` has no interesting
        // destructor side-effects, but the workspace forbids
        // `let _ = ...` on drop types. Bind-and-inspect instead.
        let result = decode_server_to_client(&data, &mut NullSink);
        prop_assert!(result.is_ok() || result.is_err());
    }

    #[test]
    fn decode_client_to_server_never_panics(
        data in proptest::collection::vec(any::<u8>(), 0..2048),
    ) {
        let result = decode_client_to_server(&data, &mut NullSink);
        prop_assert!(result.is_ok() || result.is_err());
    }

    #[test]
    fn parse_auth_response_never_panics(
        data in proptest::collection::vec(any::<u8>(), 0..2048),
    ) {
        let mut sink = NullSink;
        let result = dstar_gateway_core::codec::dplus::parse_auth_response(&data, &mut sink);
        prop_assert!(result.is_ok() || result.is_err());
    }
}
