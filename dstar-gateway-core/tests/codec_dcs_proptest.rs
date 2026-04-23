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
//! Property tests for `DCS` codec round-trips.
//!
//! Two flavours:
//! 1. Round-trip properties — encode a valid input, decode it, assert
//!    the decoded value matches the original. Exercises every encoder
//!    against its matching decoder.
//! 2. Never-panic properties — throw random bytes at the decoders to
//!    prove they cannot panic on any input.

// Integration tests are separate compilation units and re-evaluate
// workspace deps. Suppress `unused_crate_dependencies` for deps that
// are only used transitively or not at all by this test file.
use static_assertions as _;
use thiserror as _;
use tracing as _;
use trybuild as _;

use dstar_gateway_core::codec::dcs::{
    ClientPacket, GatewayType, ServerPacket, decode_client_to_server, decode_server_to_client,
    encode_connect_ack, encode_connect_link, encode_connect_nak, encode_connect_unlink,
    encode_poll_reply, encode_poll_request, encode_voice,
};
use dstar_gateway_core::validator::NullSink;
use dstar_gateway_core::{Callsign, DStarHeader, Module, StreamId, Suffix, VoiceFrame};
use proptest::prelude::*;

prop_compose! {
    fn any_callsign()(s in "[A-Z0-9]{1,8}") -> Callsign {
        // Strategy regex guarantees valid callsign characters.
        Callsign::try_from_str(&s).unwrap()
    }
}

prop_compose! {
    /// Connect-packet callsigns — restricted to 1..=7 chars so byte 7
    /// is always a space. The connect-packet wire format places the
    /// module letter at byte [8], and the decoder reads bytes [0..8]
    /// as the callsign, so an 8-char callsign would put a non-space
    /// at byte 7 that wouldn't round-trip through the separate
    /// `client_module` field. This constraint matches how real D-STAR
    /// radios emit connect packets: the station callsign is at most
    /// 7 chars and the module letter is the 8th char.
    fn any_connect_callsign()(s in "[A-Z0-9]{1,7}") -> Callsign {
        // Strategy regex guarantees valid callsign characters.
        Callsign::try_from_str(&s).unwrap()
    }
}

prop_compose! {
    fn any_module()(
        c in prop::sample::select(vec!['A','B','C','D','E','F','G','H'])
    ) -> Module {
        // Strategy restricts to uppercase A..H, always valid.
        Module::try_from_char(c).unwrap()
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

const fn dcs_header(my_call: Callsign) -> DStarHeader {
    DStarHeader {
        flag1: 0,
        flag2: 0,
        flag3: 0,
        rpt2: Callsign::from_wire_bytes(*b"DCS001 G"),
        rpt1: Callsign::from_wire_bytes(*b"DCS001 C"),
        ur_call: Callsign::from_wire_bytes(*b"CQCQCQ  "),
        my_call,
        my_suffix: Suffix::EMPTY,
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(500))]

    // ─── Connect / unlink / poll round-trips ────────────────
    #[test]
    fn link_client_roundtrips(
        cs in any_connect_callsign(),
        cm in any_module(),
        rm in any_module(),
    ) {
        let refl = Callsign::from_wire_bytes(*b"DCS001  ");
        let mut buf = [0u8; 600];
        let n = encode_connect_link(&mut buf, &cs, cm, rm, &refl, GatewayType::Repeater)
            .unwrap();
        let pkt = decode_client_to_server(&buf[..n], &mut NullSink).unwrap();
        match pkt {
            ClientPacket::Link {
                callsign,
                client_module,
                reflector_module,
                reflector_callsign,
                ..
            } => {
                prop_assert_eq!(callsign, cs);
                prop_assert_eq!(client_module, cm);
                prop_assert_eq!(reflector_module, rm);
                prop_assert_eq!(reflector_callsign, refl);
            }
            _ => prop_assert!(false, "expected Link"),
        }
    }

    #[test]
    fn unlink_client_roundtrips(cs in any_connect_callsign(), cm in any_module()) {
        let refl = Callsign::from_wire_bytes(*b"DCS001  ");
        let mut buf = [0u8; 32];
        let n = encode_connect_unlink(&mut buf, &cs, cm, &refl).unwrap();
        let pkt = decode_client_to_server(&buf[..n], &mut NullSink).unwrap();
        match pkt {
            ClientPacket::Unlink {
                callsign,
                client_module,
                reflector_callsign,
            } => {
                prop_assert_eq!(callsign, cs);
                prop_assert_eq!(client_module, cm);
                prop_assert_eq!(reflector_callsign, refl);
            }
            _ => prop_assert!(false, "expected Unlink"),
        }
    }

    #[test]
    fn poll_request_roundtrips(cs in any_callsign()) {
        let refl = Callsign::from_wire_bytes(*b"DCS001  ");
        let mut buf = [0u8; 32];
        let n = encode_poll_request(&mut buf, &cs, &refl).unwrap();
        let pkt = decode_client_to_server(&buf[..n], &mut NullSink).unwrap();
        match pkt {
            ClientPacket::Poll {
                callsign,
                reflector_callsign,
            } => {
                prop_assert_eq!(callsign, cs);
                prop_assert_eq!(reflector_callsign, refl);
            }
            _ => prop_assert!(false, "expected Poll"),
        }
    }

    #[test]
    fn poll_echo_server_roundtrips(cs in any_callsign()) {
        let refl = Callsign::from_wire_bytes(*b"DCS001  ");
        let mut buf = [0u8; 32];
        let n = encode_poll_reply(&mut buf, &cs, &refl).unwrap();
        let pkt = decode_server_to_client(&buf[..n], &mut NullSink).unwrap();
        match pkt {
            ServerPacket::PollEcho {
                callsign,
                reflector_callsign,
            } => {
                prop_assert_eq!(callsign, cs);
                prop_assert_eq!(reflector_callsign, refl);
            }
            _ => prop_assert!(false, "expected PollEcho"),
        }
    }

    // ─── Connect reply round-trips ──────────────────────────
    #[test]
    fn connect_ack_server_roundtrips(cs in any_connect_callsign(), m in any_module()) {
        let mut buf = [0u8; 32];
        let n = encode_connect_ack(&mut buf, &cs, m).unwrap();
        let pkt = decode_server_to_client(&buf[..n], &mut NullSink).unwrap();
        match pkt {
            ServerPacket::ConnectAck {
                callsign,
                reflector_module,
            } => {
                prop_assert_eq!(callsign, cs);
                prop_assert_eq!(reflector_module, m);
            }
            _ => prop_assert!(false, "expected ConnectAck"),
        }
    }

    #[test]
    fn connect_nak_server_roundtrips(cs in any_connect_callsign(), m in any_module()) {
        let mut buf = [0u8; 32];
        let n = encode_connect_nak(&mut buf, &cs, m).unwrap();
        let pkt = decode_server_to_client(&buf[..n], &mut NullSink).unwrap();
        match pkt {
            ServerPacket::ConnectNak {
                callsign,
                reflector_module,
            } => {
                prop_assert_eq!(callsign, cs);
                prop_assert_eq!(reflector_module, m);
            }
            _ => prop_assert!(false, "expected ConnectNak"),
        }
    }

    // ─── Voice round-trips ──────────────────────────────────
    #[test]
    fn voice_client_roundtrips(
        sid in any_stream_id(),
        seq in 0u8..21,
        frame in any_voice_frame(),
        my in any_callsign(),
    ) {
        let header = dcs_header(my);
        let mut buf = [0u8; 128];
        let n = encode_voice(&mut buf, &header, sid, seq, &frame, false).unwrap();
        let pkt = decode_client_to_server(&buf[..n], &mut NullSink).unwrap();
        match pkt {
            ClientPacket::Voice {
                stream_id,
                seq: s,
                frame: f,
                is_end,
                header: h,
            } => {
                prop_assert_eq!(stream_id, sid);
                prop_assert_eq!(s, seq);
                prop_assert_eq!(f.ambe, frame.ambe);
                prop_assert_eq!(f.slow_data, frame.slow_data);
                prop_assert!(!is_end);
                prop_assert_eq!(h.my_call, my);
            }
            _ => prop_assert!(false, "expected Voice"),
        }
    }

    #[test]
    fn voice_eot_client_roundtrips(
        sid in any_stream_id(),
        seq in 0u8..21,
        frame in any_voice_frame(),
        my in any_callsign(),
    ) {
        let header = dcs_header(my);
        let mut buf = [0u8; 128];
        let n = encode_voice(&mut buf, &header, sid, seq, &frame, true).unwrap();
        let pkt = decode_client_to_server(&buf[..n], &mut NullSink).unwrap();
        match pkt {
            ClientPacket::Voice { is_end, seq: s, .. } => {
                prop_assert!(is_end, "is_end should be true for EOT");
                prop_assert_eq!(s, seq);
            }
            _ => prop_assert!(false, "expected Voice"),
        }
    }

    #[test]
    fn voice_server_roundtrips(
        sid in any_stream_id(),
        seq in 0u8..21,
        frame in any_voice_frame(),
        my in any_callsign(),
    ) {
        let header = dcs_header(my);
        let mut buf = [0u8; 128];
        let n = encode_voice(&mut buf, &header, sid, seq, &frame, false).unwrap();
        let pkt = decode_server_to_client(&buf[..n], &mut NullSink).unwrap();
        match pkt {
            ServerPacket::Voice {
                stream_id,
                seq: s,
                frame: f,
                is_end,
                ..
            } => {
                prop_assert_eq!(stream_id, sid);
                prop_assert_eq!(s, seq);
                prop_assert_eq!(f.ambe, frame.ambe);
                prop_assert_eq!(f.slow_data, frame.slow_data);
                prop_assert!(!is_end);
            }
            _ => prop_assert!(false, "expected Voice"),
        }
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(5000))]

    #[test]
    fn decode_server_to_client_never_panics(
        data in proptest::collection::vec(any::<u8>(), 0..600),
    ) {
        let mut sink = NullSink;
        let result = decode_server_to_client(&data, &mut sink);
        prop_assert!(result.is_ok() || result.is_err());
    }

    #[test]
    fn decode_client_to_server_never_panics(
        data in proptest::collection::vec(any::<u8>(), 0..600),
    ) {
        let mut sink = NullSink;
        let result = decode_client_to_server(&data, &mut sink);
        prop_assert!(result.is_ok() || result.is_err());
    }
}
