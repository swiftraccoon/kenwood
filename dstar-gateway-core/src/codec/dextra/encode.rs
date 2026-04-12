//! `DExtra` packet encoders.
//!
//! Every encoder writes into a caller-supplied `&mut [u8]` and returns
//! the number of bytes written. Hot-path encoding is allocation-free.

use crate::error::EncodeError;
use crate::header::DStarHeader;
use crate::types::{Callsign, Module, StreamId};
use crate::voice::{AMBE_SILENCE, DSTAR_SYNC_BYTES, VoiceFrame};

use super::consts::{
    CONNECT_ACK_TAG, CONNECT_LEN, CONNECT_NAK_TAG, CONNECT_REPLY_LEN, DSVT_MAGIC, POLL_LEN,
    VOICE_DATA_LEN, VOICE_EOT_LEN, VOICE_HEADER_LEN,
};

/// Encode an 11-byte LINK connect request.
///
/// Layout per `ircDDBGateway/Common/ConnectData.cpp:278-296`
/// (`getDExtraData CT_LINK1`):
/// - `[0..7]`: first 7 chars of the callsign (from `data[0..7]`)
/// - `[7]`: space padding (from `memset(data, ' ', 8)`)
/// - `[8]`: 8th char of the callsign slot — the client module
///   letter per the ircDDBGateway convention
/// - `[9]`: reflector module letter
/// - `[10]`: `0x00`
///
/// # Errors
///
/// Returns [`EncodeError::BufferTooSmall`] if `out.len() < 11`.
///
/// # See also
///
/// `ircDDBGateway/Common/ConnectData.cpp:278-296` (`getDExtraData`
/// `CT_LINK1` branch) for the reference encoder this function
/// mirrors.
pub fn encode_connect_link(
    out: &mut [u8],
    callsign: &Callsign,
    reflector_module: Module,
    client_module: Module,
) -> Result<usize, EncodeError> {
    write_connect_common(out, *callsign, client_module, reflector_module.as_byte())?;
    Ok(CONNECT_LEN)
}

/// Encode an 11-byte UNLINK request.
///
/// Same shape as LINK but byte `[9]` is `b' '` (space) instead of the
/// reflector module.
///
/// # Errors
///
/// Returns [`EncodeError::BufferTooSmall`] if `out.len() < 11`.
///
/// # See also
///
/// `ircDDBGateway/Common/ConnectData.cpp:298-300` (`getDExtraData`
/// `CT_UNLINK` branch).
pub fn encode_unlink(
    out: &mut [u8],
    callsign: &Callsign,
    client_module: Module,
) -> Result<usize, EncodeError> {
    write_connect_common(out, *callsign, client_module, b' ')?;
    Ok(CONNECT_LEN)
}

/// Encode a 14-byte connect ACK reply.
///
/// Layout per `ircDDBGateway/Common/ConnectData.cpp:302-308`
/// (`getDExtraData CT_ACK`):
/// - `[0..7]`: first 7 chars of the echoed callsign
/// - `[7]`: space padding
/// - `[8]`: 8th char of the echoed callsign (the repeater/client
///   module letter from the original LINK request)
/// - `[9]`: reflector module letter
/// - `[10..13]`: `b"ACK"`
/// - `[13]`: `0x00` (NUL terminator)
///
/// # Errors
///
/// Returns [`EncodeError::BufferTooSmall`] if `out.len() < 14`.
///
/// # See also
///
/// `ircDDBGateway/Common/ConnectData.cpp:302-308` (`getDExtraData`
/// `CT_ACK` branch) for the reference encoder.
pub fn encode_connect_ack(
    out: &mut [u8],
    callsign: &Callsign,
    reflector_module: Module,
) -> Result<usize, EncodeError> {
    write_connect_reply(out, *callsign, reflector_module, CONNECT_ACK_TAG)?;
    Ok(CONNECT_REPLY_LEN)
}

/// Encode a 14-byte connect NAK reply.
///
/// Same as [`encode_connect_ack`] but the 3-byte tag is `b"NAK"`.
///
/// # Errors
///
/// Returns [`EncodeError::BufferTooSmall`] if `out.len() < 14`.
///
/// # See also
///
/// `ircDDBGateway/Common/ConnectData.cpp:310-316` (`getDExtraData`
/// `CT_NAK` branch).
pub fn encode_connect_nak(
    out: &mut [u8],
    callsign: &Callsign,
    reflector_module: Module,
) -> Result<usize, EncodeError> {
    write_connect_reply(out, *callsign, reflector_module, CONNECT_NAK_TAG)?;
    Ok(CONNECT_REPLY_LEN)
}

/// Encode a 9-byte keepalive poll.
///
/// Layout per `ircDDBGateway/Common/PollData.cpp:155-168`
/// (`getDExtraData`):
/// - `[0..8]`: 8-byte space-padded callsign
/// - `[8]`: `0x00`
///
/// # Errors
///
/// Returns [`EncodeError::BufferTooSmall`] if `out.len() < 9`.
///
/// # See also
///
/// `ircDDBGateway/Common/PollData.cpp:155-168` (`getDExtraData`)
/// for the reference keepalive encoder.
pub fn encode_poll(out: &mut [u8], callsign: &Callsign) -> Result<usize, EncodeError> {
    if out.len() < POLL_LEN {
        return Err(EncodeError::BufferTooSmall {
            need: POLL_LEN,
            have: out.len(),
        });
    }
    if let Some(dst) = out.get_mut(..8) {
        dst.copy_from_slice(callsign.as_bytes());
    }
    if let Some(b) = out.get_mut(8) {
        *b = 0x00;
    }
    Ok(POLL_LEN)
}

/// Encode a 9-byte poll echo (server-side reply to a client poll).
///
/// Same shape as [`encode_poll`]: callsign at `[0..8]` then `0x00`.
///
/// # Errors
///
/// Returns [`EncodeError::BufferTooSmall`] if `out.len() < 9`.
pub fn encode_poll_echo(out: &mut [u8], callsign: &Callsign) -> Result<usize, EncodeError> {
    encode_poll(out, callsign)
}

/// Encode a 56-byte voice header.
///
/// Layout per `ircDDBGateway/Common/HeaderData.cpp:590-635`
/// (`getDExtraData`):
/// - `[0..4]`: `b"DSVT"` (NOT preceded by a length prefix — unlike `DPlus`)
/// - `[4]`: `0x10` (header indicator)
/// - `[5..8]`: `0x00` (reserved)
/// - `[8]`: `0x20` (config)
/// - `[9..12]`: `0x00 0x01 0x02` (band1/band2/band3)
/// - `[12..14]`: `stream_id` little-endian
/// - `[14]`: `0x80` (header indicator)
/// - `[15..56]`: [`DStarHeader::encode_for_dsvt`] (41 bytes: 3 zero flag
///   bytes + RPT2 + RPT1 + YOUR + MY + `MY_SUFFIX` + CRC)
///
/// # Errors
///
/// Returns [`EncodeError::BufferTooSmall`] if `out.len() < 56`.
///
/// # See also
///
/// `ircDDBGateway/Common/HeaderData.cpp:590-635` (`getDExtraData`)
/// for the reference encoder. The 41-byte trailer is shared with
/// [`encode_voice_data`] below.
pub fn encode_voice_header(
    out: &mut [u8],
    stream_id: StreamId,
    header: &DStarHeader,
) -> Result<usize, EncodeError> {
    if out.len() < VOICE_HEADER_LEN {
        return Err(EncodeError::BufferTooSmall {
            need: VOICE_HEADER_LEN,
            have: out.len(),
        });
    }
    if let Some(dst) = out.get_mut(..4) {
        dst.copy_from_slice(&DSVT_MAGIC);
    }
    if let Some(b) = out.get_mut(4) {
        *b = 0x10;
    }
    if let Some(b) = out.get_mut(5) {
        *b = 0x00;
    }
    if let Some(b) = out.get_mut(6) {
        *b = 0x00;
    }
    if let Some(b) = out.get_mut(7) {
        *b = 0x00;
    }
    if let Some(b) = out.get_mut(8) {
        *b = 0x20;
    }
    if let Some(b) = out.get_mut(9) {
        *b = 0x00;
    }
    if let Some(b) = out.get_mut(10) {
        *b = 0x01;
    }
    if let Some(b) = out.get_mut(11) {
        *b = 0x02;
    }
    let sid = stream_id.get().to_le_bytes();
    if let Some(b) = out.get_mut(12) {
        *b = sid[0];
    }
    if let Some(b) = out.get_mut(13) {
        *b = sid[1];
    }
    if let Some(b) = out.get_mut(14) {
        *b = 0x80;
    }
    let encoded = header.encode_for_dsvt();
    if let Some(dst) = out.get_mut(15..56) {
        dst.copy_from_slice(&encoded);
    }
    Ok(VOICE_HEADER_LEN)
}

/// Encode a 27-byte voice data packet.
///
/// Layout per `ircDDBGateway/Common/AMBEData.cpp:317-345`
/// (`getDExtraData`):
/// - `[0..4]`: `b"DSVT"`
/// - `[4]`: `0x20` (voice type)
/// - `[5..8]`: `0x00` (reserved)
/// - `[8]`: `0x20` (config)
/// - `[9..12]`: `0x00 0x01 0x02` (bands)
/// - `[12..14]`: `stream_id` LE
/// - `[14]`: `seq`
/// - `[15..24]`: 9 AMBE bytes
/// - `[24..27]`: 3 slow data bytes
///
/// # Errors
///
/// Returns [`EncodeError::BufferTooSmall`] if `out.len() < 27`.
///
/// # See also
///
/// `ircDDBGateway/Common/AMBEData.cpp:317-345` (`getDExtraData`)
/// for the reference encoder this function mirrors.
pub fn encode_voice_data(
    out: &mut [u8],
    stream_id: StreamId,
    seq: u8,
    frame: &VoiceFrame,
) -> Result<usize, EncodeError> {
    if out.len() < VOICE_DATA_LEN {
        return Err(EncodeError::BufferTooSmall {
            need: VOICE_DATA_LEN,
            have: out.len(),
        });
    }
    write_voice_prefix(out, stream_id, seq);
    if let Some(dst) = out.get_mut(15..24) {
        dst.copy_from_slice(&frame.ambe);
    }
    if let Some(dst) = out.get_mut(24..27) {
        dst.copy_from_slice(&frame.slow_data);
    }
    Ok(VOICE_DATA_LEN)
}

/// Encode a 27-byte voice EOT packet.
///
/// Same shape as [`encode_voice_data`] but:
/// - `[14]`: `seq | 0x40` (EOT bit set)
/// - `[15..24]`: [`AMBE_SILENCE`]
/// - `[24..27]`: [`DSTAR_SYNC_BYTES`]
///
/// # Errors
///
/// Returns [`EncodeError::BufferTooSmall`] if `out.len() < 27`.
///
/// # See also
///
/// `ircDDBGateway/Common/AMBEData.cpp:317-345` — the
/// `getDExtraData` encoder produces the same 27-byte layout regardless
/// of `isEnd()`; the caller is expected to set `m_outSeq |= 0x40` and
/// fill `m_data` with silence + sync bytes before invoking it.
pub fn encode_voice_eot(
    out: &mut [u8],
    stream_id: StreamId,
    seq: u8,
) -> Result<usize, EncodeError> {
    if out.len() < VOICE_EOT_LEN {
        return Err(EncodeError::BufferTooSmall {
            need: VOICE_EOT_LEN,
            have: out.len(),
        });
    }
    write_voice_prefix(out, stream_id, seq | 0x40);
    if let Some(dst) = out.get_mut(15..24) {
        dst.copy_from_slice(&AMBE_SILENCE);
    }
    if let Some(dst) = out.get_mut(24..27) {
        dst.copy_from_slice(&DSTAR_SYNC_BYTES);
    }
    Ok(VOICE_EOT_LEN)
}

/// Internal helper: write the shared 15-byte prefix common to voice
/// data and voice EOT packets.
fn write_voice_prefix(out: &mut [u8], stream_id: StreamId, seq: u8) {
    if let Some(dst) = out.get_mut(..4) {
        dst.copy_from_slice(&DSVT_MAGIC);
    }
    if let Some(b) = out.get_mut(4) {
        *b = 0x20;
    }
    if let Some(b) = out.get_mut(5) {
        *b = 0x00;
    }
    if let Some(b) = out.get_mut(6) {
        *b = 0x00;
    }
    if let Some(b) = out.get_mut(7) {
        *b = 0x00;
    }
    if let Some(b) = out.get_mut(8) {
        *b = 0x20;
    }
    if let Some(b) = out.get_mut(9) {
        *b = 0x00;
    }
    if let Some(b) = out.get_mut(10) {
        *b = 0x01;
    }
    if let Some(b) = out.get_mut(11) {
        *b = 0x02;
    }
    let sid = stream_id.get().to_le_bytes();
    if let Some(b) = out.get_mut(12) {
        *b = sid[0];
    }
    if let Some(b) = out.get_mut(13) {
        *b = sid[1];
    }
    if let Some(b) = out.get_mut(14) {
        *b = seq;
    }
}

/// Internal helper: write the 11-byte LINK/UNLINK skeleton, with a
/// caller-supplied byte at position `[9]` (reflector module for LINK,
/// space for UNLINK).
///
/// Mirrors `ircDDBGateway/Common/ConnectData.cpp:278-300`
/// (`getDExtraData`):
/// ```text
/// memset(data, ' ', 8)                          // out[0..8] = spaces
/// for i in 0..min(repeater.Len(), 7)            // out[0..7] = first 7 chars
///     data[i] = repeater.GetChar(i)
/// data[8] = repeater.GetChar(7)                 // out[8] = client module
/// data[9] = reflector.GetChar(7) or ' '          // out[9] = byte9
/// data[10] = 0x00
/// ```
fn write_connect_common(
    out: &mut [u8],
    callsign: Callsign,
    client_module: Module,
    byte9: u8,
) -> Result<(), EncodeError> {
    if out.len() < CONNECT_LEN {
        return Err(EncodeError::BufferTooSmall {
            need: CONNECT_LEN,
            have: out.len(),
        });
    }
    // Mirror the reference `memset(data, ' ', LONG_CALLSIGN_LENGTH)`
    // by filling the first 11 bytes with spaces before writing the
    // per-position overrides. This ensures out[7] ends up as ' '
    // (the pad slot between the first 7 chars of the repeater
    // callsign and the module letter at out[8]).
    if let Some(region) = out.get_mut(..CONNECT_LEN) {
        region.fill(b' ');
    }
    // out[0..7] = first 7 chars of the callsign. Loop `i < 7` in
    // the reference, so we intentionally stop at 7 — never touch
    // out[7].
    let cs = callsign.as_bytes();
    if let Some(dst) = out.get_mut(..7)
        && let Some(src) = cs.get(..7)
    {
        dst.copy_from_slice(src);
    }
    // out[8] = client module letter (= 8th char of repeater per
    // the reference convention).
    if let Some(b) = out.get_mut(8) {
        *b = client_module.as_byte();
    }
    // out[9] = reflector module letter for LINK, or space for
    // UNLINK (handed in by the caller).
    if let Some(b) = out.get_mut(9) {
        *b = byte9;
    }
    // out[10] = 0x00 NUL terminator.
    if let Some(b) = out.get_mut(10) {
        *b = 0x00;
    }
    Ok(())
}

/// Internal helper: write a 14-byte connect reply (ACK or NAK).
///
/// Layout per `ircDDBGateway/Common/ConnectData.cpp:302-316`
/// (`getDExtraData CT_ACK`/`CT_NAK`):
/// - `[0..7]` first 7 chars of the echoed callsign
/// - `[7]` space padding (from `memset`)
/// - `[8]` 8th char of the echoed callsign — the client's module
///   from the original LINK request (`m_repeater.GetChar(7)`)
/// - `[9]` reflector module letter (`m_reflector.GetChar(7)`)
/// - `[10..13]` `tag` (`b"ACK"` or `b"NAK"`)
/// - `[13]` `0x00` NUL terminator
fn write_connect_reply(
    out: &mut [u8],
    callsign: Callsign,
    reflector_module: Module,
    tag: [u8; 3],
) -> Result<(), EncodeError> {
    if out.len() < CONNECT_REPLY_LEN {
        return Err(EncodeError::BufferTooSmall {
            need: CONNECT_REPLY_LEN,
            have: out.len(),
        });
    }
    // Mirror `memset(data, ' ', 8)` + the rest of the trailing
    // positions. Filling with spaces ensures out[7] is a space —
    // and any bytes we don't explicitly overwrite stay as spaces
    // rather than leaking arbitrary prior contents.
    if let Some(region) = out.get_mut(..CONNECT_REPLY_LEN) {
        region.fill(b' ');
    }
    // out[0..7] = first 7 chars of the callsign.
    let cs = callsign.as_bytes();
    if let Some(dst) = out.get_mut(..7)
        && let Some(src) = cs.get(..7)
    {
        dst.copy_from_slice(src);
    }
    // out[8] = 8th char of the callsign (the repeater/client
    // module letter from the original LINK request we're echoing).
    if let Some(b) = out.get_mut(8) {
        *b = cs.get(7).copied().unwrap_or(b' ');
    }
    // out[9] = reflector module letter.
    if let Some(b) = out.get_mut(9) {
        *b = reflector_module.as_byte();
    }
    // out[10..13] = 3-byte tag.
    if let Some(dst) = out.get_mut(10..13) {
        dst.copy_from_slice(&tag);
    }
    // out[13] = 0x00 NUL terminator.
    if let Some(b) = out.get_mut(13) {
        *b = 0x00;
    }
    Ok(())
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

    const fn test_header() -> DStarHeader {
        DStarHeader {
            flag1: 0,
            flag2: 0,
            flag3: 0,
            rpt2: Callsign::from_wire_bytes(*b"XRF030 G"),
            rpt1: Callsign::from_wire_bytes(*b"XRF030 C"),
            ur_call: Callsign::from_wire_bytes(*b"CQCQCQ  "),
            my_call: Callsign::from_wire_bytes(*b"W1AW    "),
            my_suffix: Suffix::EMPTY,
        }
    }

    // ─── Connect (LINK) tests ──────────────────────────────────
    #[test]
    fn encode_connect_link_writes_11_bytes() -> TestResult {
        let mut buf = [0u8; 16];
        let n = encode_connect_link(&mut buf, &cs(*b"W1AW    "), Module::C, Module::B)?;
        assert_eq!(n, 11);
        // First 7 chars of the callsign at [0..7]. "W1AW" pads to
        // "W1AW   " — 4 chars + 3 spaces.
        assert_eq!(&buf[..7], b"W1AW   ", "callsign chars 0..7");
        // Position 7 stays as the space padding (from memset).
        assert_eq!(buf[7], b' ', "pad slot before module letter");
        // Position 8 holds the client module letter.
        assert_eq!(buf[8], b'B', "client module");
        // Position 9 holds the reflector module letter.
        assert_eq!(buf[9], b'C', "reflector module");
        assert_eq!(buf[10], 0x00, "trailing null");
        Ok(())
    }

    #[test]
    fn encode_connect_link_rejects_small_buffer() -> TestResult {
        let mut buf = [0u8; 10];
        let Err(err) = encode_connect_link(&mut buf, &cs(*b"W1AW    "), Module::C, Module::B)
        else {
            return Err("expected too small".into());
        };
        assert_eq!(err, EncodeError::BufferTooSmall { need: 11, have: 10 });
        Ok(())
    }

    // ─── Unlink tests ──────────────────────────────────────────
    #[test]
    fn encode_unlink_writes_11_bytes_with_space_at_9() -> TestResult {
        let mut buf = [0u8; 16];
        let n = encode_unlink(&mut buf, &cs(*b"W1AW    "), Module::B)?;
        assert_eq!(n, 11);
        assert_eq!(&buf[..7], b"W1AW   ", "callsign chars 0..7");
        assert_eq!(buf[7], b' ', "pad slot before module letter");
        assert_eq!(buf[8], b'B', "client module");
        assert_eq!(buf[9], b' ', "space where reflector module would be");
        assert_eq!(buf[10], 0x00);
        Ok(())
    }

    #[test]
    fn encode_unlink_rejects_small_buffer() -> TestResult {
        let mut buf = [0u8; 5];
        let Err(err) = encode_unlink(&mut buf, &cs(*b"W1AW    "), Module::B) else {
            return Err("expected too small".into());
        };
        assert_eq!(err, EncodeError::BufferTooSmall { need: 11, have: 5 });
        Ok(())
    }

    // ─── Connect ACK/NAK tests ─────────────────────────────────
    #[test]
    fn encode_connect_ack_writes_14_bytes() -> TestResult {
        let mut buf = [0u8; 16];
        // Test case: callsign "XRF030 C" so the 8th char of the
        // callsign is 'C' — the client/repeater module letter we
        // want echoed at byte [8]. The reflector module is 'B'.
        let n = encode_connect_ack(&mut buf, &cs(*b"XRF030 C"), Module::B)?;
        assert_eq!(n, 14);
        // First 7 chars of callsign: "XRF030 " (6 chars + trailing space).
        assert_eq!(&buf[..7], b"XRF030 ", "callsign chars 0..7");
        // Pad slot.
        assert_eq!(buf[7], b' ', "pad slot");
        // Repeater/client module letter (8th char of callsign).
        assert_eq!(buf[8], b'C', "echoed repeater module");
        // Reflector module letter.
        assert_eq!(buf[9], b'B', "reflector module");
        // Tag at [10..13], NUL at [13].
        assert_eq!(&buf[10..13], b"ACK");
        assert_eq!(buf[13], 0x00, "NUL terminator");
        Ok(())
    }

    #[test]
    fn encode_connect_nak_writes_14_bytes() -> TestResult {
        let mut buf = [0u8; 16];
        let n = encode_connect_nak(&mut buf, &cs(*b"XRF030 C"), Module::B)?;
        assert_eq!(n, 14);
        assert_eq!(&buf[..7], b"XRF030 ");
        assert_eq!(buf[7], b' ');
        assert_eq!(buf[8], b'C');
        assert_eq!(buf[9], b'B');
        assert_eq!(&buf[10..13], b"NAK");
        assert_eq!(buf[13], 0x00);
        Ok(())
    }

    #[test]
    fn encode_connect_ack_rejects_small_buffer() -> TestResult {
        let mut buf = [0u8; 13];
        let Err(err) = encode_connect_ack(&mut buf, &cs(*b"XRF030  "), Module::C) else {
            return Err("expected too small".into());
        };
        assert_eq!(err, EncodeError::BufferTooSmall { need: 14, have: 13 });
        Ok(())
    }

    // ─── Poll / poll echo tests ────────────────────────────────
    #[test]
    fn encode_poll_writes_9_bytes() -> TestResult {
        let mut buf = [0u8; 16];
        let n = encode_poll(&mut buf, &cs(*b"W1AW    "))?;
        assert_eq!(n, 9);
        assert_eq!(&buf[..8], b"W1AW    ");
        assert_eq!(buf[8], 0x00);
        Ok(())
    }

    #[test]
    fn encode_poll_echo_matches_poll() -> TestResult {
        let mut poll_buf = [0u8; 16];
        let mut echo_buf = [0u8; 16];
        let n1 = encode_poll(&mut poll_buf, &cs(*b"XRF030  "))?;
        let n2 = encode_poll_echo(&mut echo_buf, &cs(*b"XRF030  "))?;
        assert_eq!(n1, n2);
        assert_eq!(
            poll_buf.get(..n1).ok_or("n1 within poll_buf")?,
            echo_buf.get(..n2).ok_or("n2 within echo_buf")?,
        );
        Ok(())
    }

    #[test]
    fn encode_poll_rejects_small_buffer() -> TestResult {
        let mut buf = [0u8; 8];
        let Err(err) = encode_poll(&mut buf, &cs(*b"W1AW    ")) else {
            return Err("expected too small".into());
        };
        assert_eq!(err, EncodeError::BufferTooSmall { need: 9, have: 8 });
        Ok(())
    }

    // ─── Voice header tests ────────────────────────────────────
    #[test]
    fn encode_voice_header_writes_56_bytes() -> TestResult {
        let mut buf = [0u8; 64];
        let n = encode_voice_header(&mut buf, sid(0xCAFE), &test_header())?;
        assert_eq!(n, 56);
        assert_eq!(&buf[..4], b"DSVT", "magic at offset 0 (no DPlus prefix)");
        assert_eq!(buf[4], 0x10, "header type");
        assert_eq!(buf[5], 0x00);
        assert_eq!(buf[6], 0x00);
        assert_eq!(buf[7], 0x00);
        assert_eq!(buf[8], 0x20, "config");
        assert_eq!(buf[9], 0x00, "band1");
        assert_eq!(buf[10], 0x01, "band2");
        assert_eq!(buf[11], 0x02, "band3");
        assert_eq!(buf[12], 0xFE, "stream id LE low byte");
        assert_eq!(buf[13], 0xCA, "stream id LE high byte");
        assert_eq!(buf[14], 0x80, "header indicator");
        assert_eq!(buf[15], 0, "flag1 zeroed by encode_for_dsvt");
        assert_eq!(buf[16], 0, "flag2 zeroed");
        assert_eq!(buf[17], 0, "flag3 zeroed");
        Ok(())
    }

    #[test]
    fn encode_voice_header_rejects_small_buffer() -> TestResult {
        let mut buf = [0u8; 32];
        let Err(err) = encode_voice_header(&mut buf, sid(0x1234), &test_header()) else {
            return Err("expected too small".into());
        };
        assert_eq!(err, EncodeError::BufferTooSmall { need: 56, have: 32 });
        Ok(())
    }

    // ─── Voice data tests ──────────────────────────────────────
    #[test]
    fn encode_voice_data_writes_27_bytes() -> TestResult {
        let mut buf = [0u8; 64];
        let frame = VoiceFrame {
            ambe: [0x11; 9],
            slow_data: [0x22; 3],
        };
        let n = encode_voice_data(&mut buf, sid(0x1234), 5, &frame)?;
        assert_eq!(n, 27);
        assert_eq!(&buf[..4], b"DSVT");
        assert_eq!(buf[4], 0x20, "voice type");
        assert_eq!(buf[5], 0x00);
        assert_eq!(buf[6], 0x00);
        assert_eq!(buf[7], 0x00);
        assert_eq!(buf[8], 0x20, "config");
        assert_eq!(buf[9], 0x00);
        assert_eq!(buf[10], 0x01);
        assert_eq!(buf[11], 0x02);
        assert_eq!(buf[12], 0x34, "stream id LE low byte");
        assert_eq!(buf[13], 0x12, "stream id LE high byte");
        assert_eq!(buf[14], 5, "seq");
        assert_eq!(&buf[15..24], &[0x11; 9]);
        assert_eq!(&buf[24..27], &[0x22; 3]);
        Ok(())
    }

    #[test]
    fn encode_voice_data_rejects_small_buffer() -> TestResult {
        let mut buf = [0u8; 10];
        let frame = VoiceFrame {
            ambe: [0; 9],
            slow_data: [0; 3],
        };
        let Err(err) = encode_voice_data(&mut buf, sid(0x1234), 0, &frame) else {
            return Err("expected too small".into());
        };
        assert_eq!(err, EncodeError::BufferTooSmall { need: 27, have: 10 });
        Ok(())
    }

    // ─── Voice EOT tests ───────────────────────────────────────
    #[test]
    fn encode_voice_eot_writes_27_bytes() -> TestResult {
        let mut buf = [0u8; 64];
        let n = encode_voice_eot(&mut buf, sid(0x1234), 7)?;
        assert_eq!(n, 27);
        assert_eq!(&buf[..4], b"DSVT");
        assert_eq!(buf[4], 0x20);
        assert_eq!(buf[14] & 0x40, 0x40, "EOT bit set");
        assert_eq!(buf[14] & 0x3F, 7, "low bits preserve seq");
        assert_eq!(&buf[15..24], &AMBE_SILENCE);
        assert_eq!(&buf[24..27], &DSTAR_SYNC_BYTES);
        Ok(())
    }

    #[test]
    fn encode_voice_eot_rejects_small_buffer() -> TestResult {
        let mut buf = [0u8; 20];
        let Err(err) = encode_voice_eot(&mut buf, sid(0x1234), 0) else {
            return Err("expected too small".into());
        };
        assert_eq!(err, EncodeError::BufferTooSmall { need: 27, have: 20 });
        Ok(())
    }
}
