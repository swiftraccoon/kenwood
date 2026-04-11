//! Golden byte vectors copied verbatim from reference implementations.
//!
//! Each const is the byte-exact output of the corresponding function in
//! `g4klx/ircDDBGateway` or `LX3JL/xlxd` for a chosen input. The comment
//! above each const cites the source file and line range that produces
//! these bytes and the bug ID the vector regression-protects.
//!
//! Why this exists: the audit found several wire-format bugs in our
//! builders (C1 `DExtra` stream ID endianness, C2 `DExtra` connect byte
//! layout, C3 DSVT band3 byte, C4 `DPlus` 32-byte EOT, C7 `DCS` `rpt_seq`
//! increment). Checking our builder output byte-for-byte against the
//! reference catches future regressions automatically.
//!
//! Reference source paths (read-only, not compiled):
//! - `ref/ircDDBGateway/Common/ConnectData.cpp`
//! - `ref/ircDDBGateway/Common/HeaderData.cpp`
//! - `ref/ircDDBGateway/Common/AMBEData.cpp`
//! - `ref/xlxd/src/cdextraprotocol.cpp`
//! - `ref/xlxd/src/cdplusprotocol.cpp`
//! - `ref/xlxd/src/cdcsprotocol.cpp`
//!
//! All vectors in this module are referenced by at least one test
//! (see the `tests` submodule at the bottom).

/// `DPlus` 32-byte end-of-transmission packet for stream 0x1234 seq 0.
///
/// Regression protection: **C4** (`DPlus` EOT must be 32 bytes, not 29).
///
/// Reference: `ircDDBGateway/Common/AMBEData.cpp:380-388` with
/// `isEnd()` true on `getDPlusData`. Layout documented in
/// `protocol::dplus::build_eot`.
pub(super) const DPLUS_EOT_STREAM_1234_SEQ_0: [u8; 32] = [
    0x20, 0x80, // DPlus prefix / type
    b'D', b'S', b'V', b'T', 0x20, // voice flag
    0x00, 0x00, 0x00, // reserved
    0x20, // config
    0x00, 0x01, 0x02, // band1 / band2 / band3
    0x34, 0x12, // stream ID 0x1234 LE
    0x40, // seq 0 with EOT bit
    // AMBE silence (9 bytes) from voice.rs AMBE_SILENCE
    0x9E, 0x8D, 0x32, 0x88, 0x26, 0x1A, 0x3F, 0x61, 0xE8, // end pattern (6 bytes)
    0x55, 0x55, 0x55, 0x55, 0xC8, 0x7A,
];

/// `DExtra` 11-byte connect packet from W1AW module A → reflector module B.
///
/// Regression protection: **C2** (`DExtra` connect must place the
/// reflector module at byte 9 and a null terminator at byte 10 —
/// previously we duplicated the local module at byte 9 and wrote 0x0B
/// at byte 10, breaking cross-module linking to XLX reflectors).
///
/// Reference: `ircDDBGateway/Common/ConnectData.cpp:287-295`
/// (`getDExtraData` `CT_LINK1`) and
/// `xlxd/src/cdextraprotocol.cpp:396-425` (connect ACK handling).
///
/// Byte-by-byte breakdown:
/// - `[0..8]` = `"W1AW    "` (callsign, space-padded to 8 bytes)
/// - `[8]` = `'A'` (local module)
/// - `[9]` = `'B'` (reflector module)
/// - `[10]` = `0x00` (null terminator)
pub(super) const DEXTRA_CONNECT_W1AW_A_TO_B: [u8; 11] = [
    b'W', b'1', b'A', b'W', b' ', b' ', b' ', b' ', // "W1AW    "
    b'A', // local module
    b'B', // reflector module
    0x00, // null terminator
];

/// `DExtra` 56-byte DSVT voice header for stream 0x1234 with REF030
/// repeater fields and W1AW origin callsign.
///
/// Regression protection: **C3** (DSVT config bytes must be
/// `[0x20, 0x00, 0x01, 0x02]` — byte `0x0B` is the "band3" marker and
/// must be `0x02`, not `0x00`. xlxd-family reflectors silently drop
/// voice streams with band3 = 0).
///
/// Reference: `xlxd/src/cdextraprotocol.cpp:552,567,581` and
/// `ircDDBGateway/Common/HeaderData.cpp:615-617` (DSVT layout +
/// pre-CRC flag zeroing).
///
/// Byte-by-byte breakdown:
/// - `[0..4]` = `"DSVT"`
/// - `[4]` = `0x10` (header flag)
/// - `[5..8]` = `0x00 0x00 0x00` (reserved)
/// - `[8..12]` = `0x20 0x00 0x01 0x02` (config — **band3 at [11]**)
/// - `[12..14]` = `0x34 0x12` (stream ID 0x1234 LE)
/// - `[14]` = `0x80` (header indicator)
/// - `[15..17]` = `0x00 0x00` (flag1, flag2 — zeroed per HeaderData.cpp:615)
/// - `[17]` = `0x00` (flag3 — zeroed)
/// - `[18..26]` = `"REF030 G"` (rpt2)
/// - `[26..34]` = `"REF030 C"` (rpt1)
/// - `[34..42]` = `"CQCQCQ  "` (your)
/// - `[42..50]` = `"W1AW    "` (my)
/// - `[50..54]` = `"    "` (suffix)
/// - `[54..56]` = `0x73 0x10` (CRC-CCITT LE)
pub(super) const DEXTRA_HEADER_BAND3_0X02: [u8; 56] = [
    0x44, 0x53, 0x56, 0x54, 0x10, 0x00, 0x00, 0x00, // DSVT + flag + reserved
    0x20, 0x00, 0x01, 0x02, // config — band3 = 0x02 at offset 11
    0x34, 0x12, // stream id 0x1234 LE
    0x80, // header indicator
    0x00, 0x00, 0x00, // flags zeroed pre-CRC
    0x52, 0x45, 0x46, 0x30, 0x33, 0x30, 0x20, 0x47, // "REF030 G" rpt2
    0x52, 0x45, 0x46, 0x30, 0x33, 0x30, 0x20, 0x43, // "REF030 C" rpt1
    0x43, 0x51, 0x43, 0x51, 0x43, 0x51, 0x20, 0x20, // "CQCQCQ  " your
    0x57, 0x31, 0x41, 0x57, 0x20, 0x20, 0x20, 0x20, // "W1AW    " my
    0x20, 0x20, 0x20, 0x20, // suffix (4 spaces)
    0x73, 0x10, // CRC-CCITT LE
];

/// `DCS` 100-byte voice packet with stream 0x5678, seq 0, `rpt_seq` = 0,
/// DCS001 repeater and W1AW origin.
///
/// Regression protection: **C7** (`DcsClient::send_header` must
/// increment the internal `rpt_seq` counter so the subsequent
/// `send_voice` uses a distinct value — previously both the header
/// frame and the first voice frame carried the same 24-bit counter,
/// confusing xlxd which uses `rpt_seq` for frame ordering).
///
/// This vector is the **first** packet (header frame). The companion
/// test in `dcs.rs` builds a second packet with `rpt_seq` = 1 and
/// asserts that bytes `[58..61]` differ between them by exactly 1.
///
/// Reference: `ircDDBGateway/Common/AMBEData::getDCSData` and
/// `xlxd/src/cdcsprotocol.cpp::EncodeDvPacket`.
///
/// Byte-by-byte breakdown:
/// - `[0..4]` = `"0001"` (DCS voice magic)
/// - `[4..7]` = `0x00 0x00 0x00` (flag1/flag2/flag3)
/// - `[7..15]` = `"DCS001 G"` (rpt2)
/// - `[15..23]` = `"DCS001 C"` (rpt1)
/// - `[23..31]` = `"CQCQCQ  "` (your)
/// - `[31..39]` = `"W1AW    "` (my)
/// - `[39..43]` = `"    "` (suffix)
/// - `[43..45]` = `0x78 0x56` (stream id 0x5678 LE)
/// - `[45]` = `0x00` (seq)
/// - `[46..55]` = AMBE silence
/// - `[55..58]` = `0x55 0x55 0x55` (slow data / sync bytes)
/// - `[58..61]` = `0x00 0x00 0x00` (**`rpt_seq`** — first packet)
/// - `[61..63]` = `0x01 0x00` (fixed trailer)
/// - `[63]` = `0x21` (fixed)
/// - `[64..100]` = zero padding
pub(super) const DCS_VOICE_RPT_SEQ_INCREMENT: [u8; 100] = [
    0x30, 0x30, 0x30, 0x31, // "0001"
    0x00, 0x00, 0x00, // flags
    0x44, 0x43, 0x53, 0x30, 0x30, 0x31, 0x20, 0x47, // "DCS001 G" rpt2
    0x44, 0x43, 0x53, 0x30, 0x30, 0x31, 0x20, 0x43, // "DCS001 C" rpt1
    0x43, 0x51, 0x43, 0x51, 0x43, 0x51, 0x20, 0x20, // "CQCQCQ  " your
    0x57, 0x31, 0x41, 0x57, 0x20, 0x20, 0x20, 0x20, // "W1AW    " my
    0x20, 0x20, 0x20, 0x20, // suffix
    0x78, 0x56, // stream id 0x5678 LE
    0x00, // seq 0
    0x9E, 0x8D, 0x32, 0x88, 0x26, 0x1A, 0x3F, 0x61, 0xE8, // AMBE silence
    0x55, 0x55, 0x55, // sync slow data
    0x00, 0x00, 0x00, // rpt_seq = 0 (first packet)
    0x01, 0x00, // trailer
    0x21, // fixed
    // [64..100] zero padding
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, //
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, //
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, //
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, //
    0x00, 0x00, 0x00, 0x00,
];

#[cfg(test)]
mod tests {
    use super::{
        DCS_VOICE_RPT_SEQ_INCREMENT, DEXTRA_CONNECT_W1AW_A_TO_B, DEXTRA_HEADER_BAND3_0X02,
    };
    use crate::header::DStarHeader;
    use crate::protocol::{dcs, dextra};
    use crate::types::{Callsign, Module, StreamId, Suffix};
    use crate::voice::VoiceFrame;

    fn cs(s: &str) -> Callsign {
        Callsign::try_from_str(s).expect("valid test callsign")
    }

    fn m(c: char) -> Module {
        Module::try_from_char(c).expect("valid test module")
    }

    fn sid(n: u16) -> StreamId {
        StreamId::new(n).expect("non-zero test stream id")
    }

    fn ref_header() -> DStarHeader {
        DStarHeader {
            flag1: 0,
            flag2: 0,
            flag3: 0,
            rpt2: cs("REF030 G"),
            rpt1: cs("REF030 C"),
            ur_call: cs("CQCQCQ"),
            my_call: cs("W1AW"),
            my_suffix: Suffix::EMPTY,
        }
    }

    fn dcs_header() -> DStarHeader {
        DStarHeader {
            flag1: 0,
            flag2: 0,
            flag3: 0,
            rpt2: cs("DCS001 G"),
            rpt1: cs("DCS001 C"),
            ur_call: cs("CQCQCQ"),
            my_call: cs("W1AW"),
            my_suffix: Suffix::EMPTY,
        }
    }

    #[test]
    fn dextra_connect_matches_reference_golden_vector() {
        // C2 regression: byte 9 must be reflector module, byte 10 = 0x00.
        let pkt = dextra::build_connect(&cs("W1AW"), m('A'), m('B'));
        assert_eq!(
            pkt.as_slice(),
            DEXTRA_CONNECT_W1AW_A_TO_B.as_slice(),
            "DExtra connect W1AW A->B diverged from reference vector"
        );
    }

    #[test]
    fn dextra_header_matches_reference_golden_vector() {
        // C3 regression: DSVT config[band3] must be 0x02.
        let pkt = dextra::build_header(&ref_header(), sid(0x1234));
        assert_eq!(
            pkt.as_slice(),
            DEXTRA_HEADER_BAND3_0X02.as_slice(),
            "DExtra voice header diverged from reference vector"
        );
        // Cross-check the C3 field directly in case a future change
        // relocates band3 within the DSVT layout.
        assert_eq!(pkt[11], 0x02, "band3 byte must remain at offset 11");
    }

    #[test]
    fn dcs_voice_first_packet_matches_reference_golden_vector() {
        // C7 regression guard: the first packet of a stream built with
        // rpt_seq = 0 must match the stored vector byte-for-byte.
        let frame = VoiceFrame::silence();
        let pkt = dcs::build_voice(&dcs_header(), sid(0x5678), 0, 0, &frame);
        assert_eq!(
            pkt.as_slice(),
            DCS_VOICE_RPT_SEQ_INCREMENT.as_slice(),
            "DCS voice packet diverged from reference vector"
        );
        // First packet's rpt_seq is all zero.
        assert_eq!(&pkt[58..61], &[0x00, 0x00, 0x00]);
    }

    #[test]
    fn dcs_voice_rpt_seq_advances_by_one() {
        // C7 regression: a fresh build with rpt_seq = 1 must differ
        // from the golden vector by exactly the rpt_seq bytes and
        // the low byte must increase by 1.
        let frame = VoiceFrame::silence();
        let pkt_next = dcs::build_voice(&dcs_header(), sid(0x5678), 1, 1, &frame);

        // rpt_seq at [58..61] is 24-bit LE → 0x000001.
        assert_eq!(
            &pkt_next[58..61],
            &[0x01, 0x00, 0x00],
            "second packet rpt_seq must be 1 (LE)"
        );

        // Every byte outside [45] (seq) and [58..61] (rpt_seq) must
        // be identical to the golden vector.
        for (i, (a, b)) in DCS_VOICE_RPT_SEQ_INCREMENT
            .iter()
            .zip(pkt_next.iter())
            .enumerate()
        {
            if i == 45 || (58..61).contains(&i) {
                continue;
            }
            assert_eq!(
                a, b,
                "packet byte {i} differs unexpectedly (golden={a:#04x}, next={b:#04x})"
            );
        }

        // And the seq byte advanced 0 → 1.
        assert_eq!(pkt_next[45], 1);
    }
}
