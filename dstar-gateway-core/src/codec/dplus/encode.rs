//! `DPlus` packet encoders.
//!
//! Every encoder writes into a caller-supplied `&mut [u8]` and returns
//! the number of bytes written. Hot-path encoding is allocation-free.

use crate::error::EncodeError;
use crate::header::DStarHeader;
use crate::types::{Callsign, StreamId};
use crate::voice::{AMBE_SILENCE, VoiceFrame};

use super::consts::{
    DSVT_MAGIC, DSVT_TYPE, DV_CLIENT_ID, LINK1_ACK_BYTES, LINK1_BYTES, LINK2_ACCEPT_TAG,
    LINK2_BUSY_TAG, LINK2_HEADER, LINK2_REPLY_PREFIX, POLL_BYTES, POLL_ECHO_BYTES,
    UNLINK_ACK_BYTES, UNLINK_BYTES, VOICE_DATA_PREFIX, VOICE_EOT_PREFIX, VOICE_EOT_TRAILER,
    VOICE_HEADER_PREFIX,
};
use super::packet::Link2Result;

/// Encode a LINK1 connect request (5 bytes).
///
/// # Errors
///
/// Returns [`EncodeError::BufferTooSmall`] if `out.len() < 5`.
///
/// # See also
///
/// `ircDDBGateway/Common/ConnectData.cpp:445-473` (`getDPlusData`
/// `CT_LINK1` branch) for the reference encoder this function mirrors.
pub fn encode_link1(out: &mut [u8]) -> Result<usize, EncodeError> {
    write_fixed(out, &LINK1_BYTES)
}

/// Encode an UNLINK request (5 bytes).
///
/// # Errors
///
/// Returns [`EncodeError::BufferTooSmall`] if `out.len() < 5`.
///
/// # See also
///
/// `ircDDBGateway/Common/ConnectData.cpp:445-473` (`getDPlusData`
/// `CT_UNLINK` branch).
pub fn encode_unlink(out: &mut [u8]) -> Result<usize, EncodeError> {
    write_fixed(out, &UNLINK_BYTES)
}

/// Encode a 3-byte keepalive poll.
///
/// # Errors
///
/// Returns [`EncodeError::BufferTooSmall`] if `out.len() < 3`.
///
/// # See also
///
/// `ircDDBGateway/Common/PollData.cpp:145-160` (`getDPlusData`).
pub fn encode_poll(out: &mut [u8]) -> Result<usize, EncodeError> {
    write_fixed(out, &POLL_BYTES)
}

/// Encode the server's LINK1 ACK echo (5 bytes — same shape as LINK1).
///
/// # Errors
///
/// Returns [`EncodeError::BufferTooSmall`] if `out.len() < 5`.
///
/// # See also
///
/// `xlxd/src/cdplusprotocol.cpp:510-540` (`EncodeConnectAckPacket`).
pub fn encode_link1_ack(out: &mut [u8]) -> Result<usize, EncodeError> {
    write_fixed(out, &LINK1_ACK_BYTES)
}

/// Encode the server's UNLINK ACK echo (5 bytes — same shape as UNLINK).
///
/// # Errors
///
/// Returns [`EncodeError::BufferTooSmall`] if `out.len() < 5`.
///
/// # See also
///
/// `xlxd/src/cdplusprotocol.cpp:510-540` (`EncodeDisconnectAckPacket`).
pub fn encode_unlink_ack(out: &mut [u8]) -> Result<usize, EncodeError> {
    write_fixed(out, &UNLINK_ACK_BYTES)
}

/// Encode the server's poll echo (3 bytes).
///
/// # Errors
///
/// Returns [`EncodeError::BufferTooSmall`] if `out.len() < 3`.
///
/// # See also
///
/// `xlxd/src/cdplusprotocol.cpp` — the reference encoder reuses the
/// client poll bytes verbatim.
pub fn encode_poll_echo(out: &mut [u8]) -> Result<usize, EncodeError> {
    write_fixed(out, &POLL_ECHO_BYTES)
}

/// Internal helper: copy a fixed-byte literal into the output buffer.
fn write_fixed(out: &mut [u8], src: &[u8]) -> Result<usize, EncodeError> {
    if out.len() < src.len() {
        return Err(EncodeError::BufferTooSmall {
            need: src.len(),
            have: out.len(),
        });
    }
    if let Some(dst) = out.get_mut(..src.len()) {
        dst.copy_from_slice(src);
    }
    Ok(src.len())
}

/// Encode a LINK2 login packet (28 bytes).
///
/// Layout per `ircDDBGateway/Common/ConnectData.cpp:449-473`:
/// - bytes `[0..4]`: `[0x1C, 0xC0, 0x04, 0x00]`
/// - bytes `[4..20]`: callsign at `[4..]`, zero-padded to offset 20
/// - bytes `[20..28]`: `b"DV019999"`
///
/// # Errors
///
/// Returns [`EncodeError::BufferTooSmall`] if `out.len() < 28`.
///
/// # See also
///
/// `ircDDBGateway/Common/ConnectData.cpp:449-473` (`getDPlusData`
/// `CT_LINK2`) for the reference encoder this function mirrors.
pub fn encode_link2(out: &mut [u8], callsign: &Callsign) -> Result<usize, EncodeError> {
    const LEN: usize = 28;
    if out.len() < LEN {
        return Err(EncodeError::BufferTooSmall {
            need: LEN,
            have: out.len(),
        });
    }
    // Zero the entire 28-byte region first.
    if let Some(dst) = out.get_mut(..LEN) {
        for b in dst.iter_mut() {
            *b = 0;
        }
    }
    // 4-byte header.
    if let Some(dst) = out.get_mut(..LINK2_HEADER.len()) {
        dst.copy_from_slice(&LINK2_HEADER);
    }
    // Callsign at offset 4. The Callsign type guarantees 8 bytes
    // already space-padded — we want the trimmed length followed by
    // zeros up to offset 20, matching the reference encoder.
    let bytes = callsign.as_bytes();
    let trimmed_len = bytes.iter().rposition(|&b| b != b' ').map_or(0, |p| p + 1);
    if let Some(dst) = out.get_mut(4..4 + trimmed_len)
        && let Some(src) = bytes.get(..trimmed_len)
    {
        dst.copy_from_slice(src);
    }
    // DV019999 at offset 20.
    if let Some(dst) = out.get_mut(20..28) {
        dst.copy_from_slice(&DV_CLIENT_ID);
    }
    Ok(LEN)
}

/// Encode an 8-byte LINK2 reply.
///
/// Layout: 4-byte prefix `[0x08, 0xC0, 0x04, 0x00]` + 4-byte result tag.
/// - `Link2Result::Accept` → `b"OKRW"`
/// - `Link2Result::Busy` → `b"BUSY"`
/// - `Link2Result::Unknown { reply }` → the supplied 4-byte tag
///
/// # Errors
///
/// Returns [`EncodeError::BufferTooSmall`] if `out.len() < 8`.
///
/// # See also
///
/// `xlxd/src/cdplusprotocol.cpp:535-544` (`EncodeLoginAckPacket` /
/// `EncodeLoginNackPacket`) for the reference encoder.
pub fn encode_link2_reply(out: &mut [u8], result: Link2Result) -> Result<usize, EncodeError> {
    const LEN: usize = 8;
    if out.len() < LEN {
        return Err(EncodeError::BufferTooSmall {
            need: LEN,
            have: out.len(),
        });
    }
    if let Some(dst) = out.get_mut(..LINK2_REPLY_PREFIX.len()) {
        dst.copy_from_slice(&LINK2_REPLY_PREFIX);
    }
    let tag: [u8; 4] = match result {
        Link2Result::Accept => LINK2_ACCEPT_TAG,
        Link2Result::Busy => LINK2_BUSY_TAG,
        Link2Result::Unknown { reply } => reply,
    };
    if let Some(dst) = out.get_mut(4..8) {
        dst.copy_from_slice(&tag);
    }
    Ok(LEN)
}

/// Encode a 58-byte voice header.
///
/// Layout per `ircDDBGateway/Common/HeaderData.cpp:637-684`
/// (`getDPlusData`):
/// - `[0]` 0x3A (length byte)
/// - `[1]` 0x80 (type byte)
/// - `[2..6]` "DSVT"
/// - `[6]` 0x10 (header type)
/// - `[7..10]` 0x00 0x00 0x00 (reserved)
/// - `[10]` 0x20 (config)
/// - `[11..14]` 0x00 0x01 0x02 (band1/2/3)
/// - `[14..16]` `stream_id` little-endian
/// - `[16]` 0x80 (header indicator)
/// - `[17..58]` `DStarHeader::encode_for_dsvt()` (flag bytes zeroed, 41 bytes)
///
/// # Errors
///
/// Returns [`EncodeError::BufferTooSmall`] if `out.len() < 58`.
///
/// # See also
///
/// `ircDDBGateway/Common/HeaderData.cpp:637-684` (`getDPlusData`)
/// for the reference encoder this function mirrors. The
/// `DStarHeader::encode_for_dsvt` helper mirrors the same file's
/// CRC logic.
pub fn encode_voice_header(
    out: &mut [u8],
    stream_id: StreamId,
    header: &DStarHeader,
) -> Result<usize, EncodeError> {
    const LEN: usize = 58;
    if out.len() < LEN {
        return Err(EncodeError::BufferTooSmall {
            need: LEN,
            have: out.len(),
        });
    }
    if let Some(b) = out.get_mut(0) {
        *b = VOICE_HEADER_PREFIX;
    }
    if let Some(b) = out.get_mut(1) {
        *b = DSVT_TYPE;
    }
    if let Some(dst) = out.get_mut(2..6) {
        dst.copy_from_slice(&DSVT_MAGIC);
    }
    if let Some(b) = out.get_mut(6) {
        *b = 0x10;
    }
    if let Some(b) = out.get_mut(7) {
        *b = 0x00;
    }
    if let Some(b) = out.get_mut(8) {
        *b = 0x00;
    }
    if let Some(b) = out.get_mut(9) {
        *b = 0x00;
    }
    if let Some(b) = out.get_mut(10) {
        *b = 0x20;
    }
    if let Some(b) = out.get_mut(11) {
        *b = 0x00;
    }
    if let Some(b) = out.get_mut(12) {
        *b = 0x01;
    }
    if let Some(b) = out.get_mut(13) {
        *b = 0x02;
    }
    let sid = stream_id.get().to_le_bytes();
    if let Some(b) = out.get_mut(14) {
        *b = sid[0];
    }
    if let Some(b) = out.get_mut(15) {
        *b = sid[1];
    }
    if let Some(b) = out.get_mut(16) {
        *b = 0x80;
    }
    let encoded = header.encode_for_dsvt();
    if let Some(dst) = out.get_mut(17..58) {
        dst.copy_from_slice(&encoded);
    }
    Ok(LEN)
}

/// Encode a 29-byte voice data packet.
///
/// Layout per `ircDDBGateway/Common/AMBEData.cpp:347-388`
/// (`getDPlusData` else branch):
/// - `[0]` 0x1D (length byte)
/// - `[1]` 0x80 (type byte)
/// - `[2..6]` "DSVT"
/// - `[6]` 0x20 (voice type)
/// - `[7..10]` 0x00 (reserved)
/// - `[10]` 0x20 (config)
/// - `[11..14]` 0x00 0x01 0x02 (bands)
/// - `[14..16]` `stream_id` LE
/// - `[16]` seq
/// - `[17..26]` 9 AMBE bytes
/// - `[26..29]` 3 slow data bytes
///
/// # Errors
///
/// Returns [`EncodeError::BufferTooSmall`] if `out.len() < 29`.
///
/// # See also
///
/// `ircDDBGateway/Common/AMBEData.cpp:347-388` (`getDPlusData`)
/// for the reference encoder, and `xlxd/src/cdplusprotocol.cpp`
/// for a mirror implementation.
pub fn encode_voice_data(
    out: &mut [u8],
    stream_id: StreamId,
    seq: u8,
    frame: &VoiceFrame,
) -> Result<usize, EncodeError> {
    const LEN: usize = 29;
    if out.len() < LEN {
        return Err(EncodeError::BufferTooSmall {
            need: LEN,
            have: out.len(),
        });
    }
    write_voice_prefix(out, VOICE_DATA_PREFIX, stream_id, seq);
    if let Some(dst) = out.get_mut(17..26) {
        dst.copy_from_slice(&frame.ambe);
    }
    if let Some(dst) = out.get_mut(26..29) {
        dst.copy_from_slice(&frame.slow_data);
    }
    Ok(LEN)
}

/// Encode a 32-byte voice EOT packet.
///
/// Layout per `ircDDBGateway/Common/AMBEData.cpp:380-388`
/// (`getDPlusData isEnd branch`):
/// - same as `encode_voice_data` for offsets `[0..17]` except `[0]` is 0x20
/// - `[16]` seq with 0x40 bit set (EOT marker)
/// - `[17..26]` `AMBE_SILENCE` (9 bytes)
/// - `[26..32]` `VOICE_EOT_TRAILER` `[0x55, 0x55, 0x55, 0x55, 0xC8, 0x7A]`
///
/// # Errors
///
/// Returns [`EncodeError::BufferTooSmall`] if `out.len() < 32`.
///
/// # See also
///
/// `ircDDBGateway/Common/AMBEData.cpp:380-388` (`getDPlusData`
/// isEnd branch) for the reference EOT encoder.
pub fn encode_voice_eot(
    out: &mut [u8],
    stream_id: StreamId,
    seq: u8,
) -> Result<usize, EncodeError> {
    const LEN: usize = 32;
    if out.len() < LEN {
        return Err(EncodeError::BufferTooSmall {
            need: LEN,
            have: out.len(),
        });
    }
    write_voice_prefix(out, VOICE_EOT_PREFIX, stream_id, seq | 0x40);
    if let Some(dst) = out.get_mut(17..26) {
        dst.copy_from_slice(&AMBE_SILENCE);
    }
    if let Some(dst) = out.get_mut(26..32) {
        dst.copy_from_slice(&VOICE_EOT_TRAILER);
    }
    Ok(LEN)
}

/// Internal helper: write the 17-byte prefix common to voice data
/// and voice EOT packets.
fn write_voice_prefix(out: &mut [u8], prefix: u8, stream_id: StreamId, seq: u8) {
    if let Some(b) = out.get_mut(0) {
        *b = prefix;
    }
    if let Some(b) = out.get_mut(1) {
        *b = DSVT_TYPE;
    }
    if let Some(dst) = out.get_mut(2..6) {
        dst.copy_from_slice(&DSVT_MAGIC);
    }
    if let Some(b) = out.get_mut(6) {
        *b = 0x20;
    }
    if let Some(b) = out.get_mut(7) {
        *b = 0x00;
    }
    if let Some(b) = out.get_mut(8) {
        *b = 0x00;
    }
    if let Some(b) = out.get_mut(9) {
        *b = 0x00;
    }
    if let Some(b) = out.get_mut(10) {
        *b = 0x20;
    }
    if let Some(b) = out.get_mut(11) {
        *b = 0x00;
    }
    if let Some(b) = out.get_mut(12) {
        *b = 0x01;
    }
    if let Some(b) = out.get_mut(13) {
        *b = 0x02;
    }
    let sid = stream_id.get().to_le_bytes();
    if let Some(b) = out.get_mut(14) {
        *b = sid[0];
    }
    if let Some(b) = out.get_mut(15) {
        *b = sid[1];
    }
    if let Some(b) = out.get_mut(16) {
        *b = seq;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Suffix;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    const fn cs(bytes: [u8; 8]) -> Callsign {
        Callsign::from_wire_bytes(bytes)
    }

    #[expect(clippy::unwrap_used, reason = "compile-time validated: n != 0")]
    const fn sid(n: u16) -> StreamId {
        StreamId::new(n).unwrap()
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

    // Fixed-byte encoder tests
    #[test]
    fn encode_link1_writes_five_bytes() -> TestResult {
        let mut buf = [0u8; 16];
        let n = encode_link1(&mut buf)?;
        assert_eq!(n, 5);
        assert_eq!(&buf[..5], &[0x05, 0x00, 0x18, 0x00, 0x01]);
        Ok(())
    }

    #[test]
    fn encode_unlink_writes_five_bytes() -> TestResult {
        let mut buf = [0u8; 16];
        let n = encode_unlink(&mut buf)?;
        assert_eq!(n, 5);
        assert_eq!(&buf[..5], &[0x05, 0x00, 0x18, 0x00, 0x00]);
        Ok(())
    }

    #[test]
    fn encode_poll_writes_three_bytes() -> TestResult {
        let mut buf = [0u8; 16];
        let n = encode_poll(&mut buf)?;
        assert_eq!(n, 3);
        assert_eq!(&buf[..3], &[0x03, 0x60, 0x00]);
        Ok(())
    }

    #[test]
    fn encode_link1_ack_matches_link1() -> TestResult {
        let mut buf = [0u8; 16];
        let n = encode_link1_ack(&mut buf)?;
        assert_eq!(
            buf.get(..n).ok_or("n within buf")?,
            &[0x05, 0x00, 0x18, 0x00, 0x01]
        );
        Ok(())
    }

    #[test]
    fn encode_unlink_ack_matches_unlink() -> TestResult {
        let mut buf = [0u8; 16];
        let n = encode_unlink_ack(&mut buf)?;
        assert_eq!(
            buf.get(..n).ok_or("n within buf")?,
            &[0x05, 0x00, 0x18, 0x00, 0x00]
        );
        Ok(())
    }

    #[test]
    fn encode_poll_echo_matches_poll() -> TestResult {
        let mut buf = [0u8; 16];
        let n = encode_poll_echo(&mut buf)?;
        assert_eq!(buf.get(..n).ok_or("n within buf")?, &[0x03, 0x60, 0x00]);
        Ok(())
    }

    #[test]
    fn encode_link1_rejects_small_buffer() -> TestResult {
        let mut buf = [0u8; 4];
        let Err(err) = encode_link1(&mut buf) else {
            return Err("expected BufferTooSmall error".into());
        };
        assert_eq!(err, EncodeError::BufferTooSmall { need: 5, have: 4 });
        Ok(())
    }

    #[test]
    fn encode_poll_rejects_two_byte_buffer() -> TestResult {
        let mut buf = [0u8; 2];
        let Err(err) = encode_poll(&mut buf) else {
            return Err("expected BufferTooSmall error".into());
        };
        assert_eq!(err, EncodeError::BufferTooSmall { need: 3, have: 2 });
        Ok(())
    }

    // LINK2 tests
    #[test]
    fn encode_link2_w1aw_writes_28_bytes() -> TestResult {
        let mut buf = [0u8; 32];
        let n = encode_link2(&mut buf, &cs(*b"W1AW    "))?;
        assert_eq!(n, 28);
        assert_eq!(&buf[..4], &[0x1C, 0xC0, 0x04, 0x00], "header");
        assert_eq!(
            &buf[4..12],
            b"W1AW\0\0\0\0",
            "callsign followed by zeros to offset 12"
        );
        assert_eq!(
            &buf[12..20],
            &[0u8; 8],
            "zero-pad between callsign and DV id"
        );
        assert_eq!(&buf[20..28], b"DV019999", "client identifier");
        Ok(())
    }

    #[test]
    fn encode_link2_rejects_buffer_too_small() -> TestResult {
        let mut buf = [0u8; 16];
        let Err(err) = encode_link2(&mut buf, &cs(*b"W1AW    ")) else {
            return Err("expected BufferTooSmall error".into());
        };
        assert_eq!(err, EncodeError::BufferTooSmall { need: 28, have: 16 });
        Ok(())
    }

    // LINK2 reply tests
    #[test]
    fn encode_link2_reply_accept_writes_okrw() -> TestResult {
        let mut buf = [0u8; 16];
        let n = encode_link2_reply(&mut buf, Link2Result::Accept)?;
        assert_eq!(n, 8);
        assert_eq!(&buf[..4], &[0x08, 0xC0, 0x04, 0x00]);
        assert_eq!(&buf[4..8], b"OKRW");
        Ok(())
    }

    #[test]
    fn encode_link2_reply_busy_writes_busy() -> TestResult {
        let mut buf = [0u8; 16];
        let n = encode_link2_reply(&mut buf, Link2Result::Busy)?;
        assert_eq!(n, 8);
        assert_eq!(&buf[4..8], b"BUSY");
        Ok(())
    }

    #[test]
    fn encode_link2_reply_unknown_writes_custom_tag() -> TestResult {
        let mut buf = [0u8; 16];
        let n = encode_link2_reply(&mut buf, Link2Result::Unknown { reply: *b"FAIL" })?;
        assert_eq!(&buf[4..8], b"FAIL");
        assert_eq!(n, 8);
        Ok(())
    }

    // Voice header tests
    #[test]
    fn encode_voice_header_writes_58_bytes() -> TestResult {
        let mut buf = [0u8; 64];
        let n = encode_voice_header(&mut buf, sid(0xCAFE), &test_header())?;
        assert_eq!(n, 58);
        assert_eq!(buf[0], 0x3A, "DPlus prefix");
        assert_eq!(buf[1], 0x80, "DSVT type");
        assert_eq!(&buf[2..6], b"DSVT");
        assert_eq!(buf[6], 0x10, "header type");
        assert_eq!(buf[14], 0xFE, "stream id LE low byte");
        assert_eq!(buf[15], 0xCA, "stream id LE high byte");
        assert_eq!(buf[16], 0x80, "header indicator");
        assert_eq!(buf[17], 0, "flag1 zeroed by encode_for_dsvt");
        assert_eq!(buf[18], 0, "flag2 zeroed by encode_for_dsvt");
        assert_eq!(buf[19], 0, "flag3 zeroed by encode_for_dsvt");
        Ok(())
    }

    #[test]
    fn encode_voice_header_rejects_small_buffer() -> TestResult {
        let mut buf = [0u8; 32];
        let Err(err) = encode_voice_header(&mut buf, sid(0x1234), &test_header()) else {
            return Err("expected BufferTooSmall error".into());
        };
        assert_eq!(err, EncodeError::BufferTooSmall { need: 58, have: 32 });
        Ok(())
    }

    // Voice data tests
    #[test]
    fn encode_voice_data_writes_29_bytes() -> TestResult {
        let mut buf = [0u8; 64];
        let frame = VoiceFrame {
            ambe: [0x11; 9],
            slow_data: [0x22; 3],
        };
        let n = encode_voice_data(&mut buf, sid(0x1234), 5, &frame)?;
        assert_eq!(n, 29);
        assert_eq!(buf[0], 0x1D, "DPlus prefix");
        assert_eq!(buf[1], 0x80, "DSVT type");
        assert_eq!(&buf[2..6], b"DSVT");
        assert_eq!(buf[14], 0x34, "stream id LE low byte");
        assert_eq!(buf[15], 0x12, "stream id LE high byte");
        assert_eq!(buf[16], 5, "seq");
        assert_eq!(&buf[17..26], &[0x11; 9]);
        assert_eq!(&buf[26..29], &[0x22; 3]);
        Ok(())
    }

    // Voice EOT tests
    #[test]
    fn encode_voice_eot_writes_32_bytes() -> TestResult {
        let mut buf = [0u8; 64];
        let n = encode_voice_eot(&mut buf, sid(0x1234), 7)?;
        assert_eq!(n, 32);
        assert_eq!(buf[0], 0x20, "EOT prefix");
        assert_eq!(buf[1], 0x80, "DSVT type");
        assert_eq!(buf[16] & 0x40, 0x40, "EOT bit set");
        assert_eq!(buf[16] & 0x3F, 7, "low bits preserve seq");
        assert_eq!(&buf[17..26], &AMBE_SILENCE);
        assert_eq!(&buf[26..32], &[0x55, 0x55, 0x55, 0x55, 0xC8, 0x7A]);
        Ok(())
    }
}
