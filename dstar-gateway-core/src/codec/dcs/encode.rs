//! `DCS` packet encoders.
//!
//! Every encoder writes into a caller-supplied `&mut [u8]` and returns
//! the number of bytes written. Hot-path encoding is allocation-free.

use crate::error::EncodeError;
use crate::header::DStarHeader;
use crate::types::{Callsign, Module, StreamId};
use crate::voice::VoiceFrame;

use super::consts::{
    CONNECT_ACK_TAG, CONNECT_NAK_TAG, CONNECT_REPLY_LEN, LINK_HTML_DONGLE, LINK_HTML_HOTSPOT,
    LINK_HTML_REPEATER, LINK_HTML_STARNET, LINK_LEN, POLL_LEN, UNLINK_LEN, VOICE_EOT_MARKER,
    VOICE_LEN, VOICE_MAGIC,
};
use super::packet::GatewayType;

/// Encode a 519-byte LINK request.
///
/// Layout per `ircDDBGateway/Common/ConnectData.cpp:337-363`
/// (`getDCSData CT_LINK1`):
/// - `[0..7]`: first 7 chars of the client repeater callsign
/// - `[7]`: space padding (from `memset(data, ' ', 8)`)
/// - `[8]`: client module letter (8th char of the repeater)
/// - `[9]`: reflector module letter
/// - `[10]`: `0x00`
/// - `[11..18]`: first 7 chars of the reflector callsign
/// - `[18]`: space padding (from `memset(data + 11, ' ', 8)`)
/// - `[19..519]`: 500-byte HTML banner identifying the gateway type,
///   zero-padded. The receiving reflector logs this banner but does
///   not parse it — any short ASCII payload that fits in 500 bytes
///   is valid.
///
/// # Errors
///
/// Returns [`EncodeError::BufferTooSmall`] if `out.len() < 519`.
///
/// # See also
///
/// `ircDDBGateway/Common/ConnectData.cpp:337-363` (`getDCSData`
/// `CT_LINK1` branch) for the reference encoder this function
/// mirrors.
pub fn encode_connect_link(
    out: &mut [u8],
    callsign: &Callsign,
    client_module: Module,
    reflector_module: Module,
    reflector_callsign: &Callsign,
    gateway_type: GatewayType,
) -> Result<usize, EncodeError> {
    if out.len() < LINK_LEN {
        return Err(EncodeError::BufferTooSmall {
            need: LINK_LEN,
            have: out.len(),
        });
    }
    // Zero-initialize the full 519-byte region so the HTML tail is
    // zero-padded per the reference (`memset(data + 19U, 0x00U, 500U)`).
    if let Some(region) = out.get_mut(..LINK_LEN) {
        for b in region {
            *b = 0x00;
        }
    }
    write_connect_prefix(
        out,
        *callsign,
        client_module.as_byte(),
        reflector_module.as_byte(),
    );
    // Reflector callsign at [11..19]. Reference does
    //   memset(data + 11, ' ', 8)
    //   for i in 0..min(reflector.Len(), 7)
    //       data[i + 11] = reflector.GetChar(i)
    // so byte [18] stays as a space from the memset — we write
    // only the first 7 chars.
    if let Some(region) = out.get_mut(11..19) {
        region.fill(b' ');
    }
    let rc = reflector_callsign.as_bytes();
    if let Some(dst) = out.get_mut(11..18)
        && let Some(src) = rc.get(..7)
    {
        dst.copy_from_slice(src);
    }
    // Copy the HTML banner at [19..]. Any trailing bytes are left as
    // zeros from the memset above.
    let html = html_for(gateway_type);
    let copy_len = html.len().min(LINK_LEN - 19);
    if let Some(dst) = out.get_mut(19..19 + copy_len)
        && let Some(src) = html.get(..copy_len)
    {
        dst.copy_from_slice(src);
    }
    Ok(LINK_LEN)
}

/// Encode a 19-byte UNLINK request.
///
/// Layout per `ircDDBGateway/Common/ConnectData.cpp:366-372`
/// (`getDCSData CT_UNLINK`):
/// - `[0..7]`: first 7 chars of the client callsign
/// - `[7]`: space padding
/// - `[8]`: client module letter (8th char of the repeater)
/// - `[9]`: `0x20` (space — the unlink marker)
/// - `[10]`: `0x00`
/// - `[11..18]`: first 7 chars of the reflector callsign
/// - `[18]`: space padding
///
/// # Errors
///
/// Returns [`EncodeError::BufferTooSmall`] if `out.len() < 19`.
///
/// # See also
///
/// `ircDDBGateway/Common/ConnectData.cpp:366-372` (`getDCSData`
/// `CT_UNLINK` branch).
pub fn encode_connect_unlink(
    out: &mut [u8],
    callsign: &Callsign,
    client_module: Module,
    reflector_callsign: &Callsign,
) -> Result<usize, EncodeError> {
    if out.len() < UNLINK_LEN {
        return Err(EncodeError::BufferTooSmall {
            need: UNLINK_LEN,
            have: out.len(),
        });
    }
    write_connect_prefix(out, *callsign, client_module.as_byte(), b' ');
    // Reflector callsign at [11..19] — first 7 chars + space pad
    // matching the reference `memset + loop i<7`.
    if let Some(region) = out.get_mut(11..19) {
        region.fill(b' ');
    }
    let rc = reflector_callsign.as_bytes();
    if let Some(dst) = out.get_mut(11..18)
        && let Some(src) = rc.get(..7)
    {
        dst.copy_from_slice(src);
    }
    Ok(UNLINK_LEN)
}

/// Encode a 14-byte connect ACK reply.
///
/// Layout per `ircDDBGateway/Common/ConnectData.cpp:374-380`
/// (`getDCSData CT_ACK`):
/// - `[0..7]`: first 7 chars of the echoed callsign
/// - `[7]`: space padding
/// - `[8]`: 8th char of the echoed callsign (the repeater/client
///   module letter)
/// - `[9]`: reflector module letter
/// - `[10..13]`: `b"ACK"`
/// - `[13]`: `0x00` NUL terminator
///
/// # Errors
///
/// Returns [`EncodeError::BufferTooSmall`] if `out.len() < 14`.
///
/// # See also
///
/// `ircDDBGateway/Common/ConnectData.cpp:374-380` (`getDCSData`
/// `CT_ACK` branch).
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
/// Same shape as [`encode_connect_ack`] but with `b"NAK"` at `[10..13]`.
///
/// # Errors
///
/// Returns [`EncodeError::BufferTooSmall`] if `out.len() < 14`.
///
/// # See also
///
/// `ircDDBGateway/Common/ConnectData.cpp:382-388` (`getDCSData`
/// `CT_NAK` branch).
pub fn encode_connect_nak(
    out: &mut [u8],
    callsign: &Callsign,
    reflector_module: Module,
) -> Result<usize, EncodeError> {
    write_connect_reply(out, *callsign, reflector_module, CONNECT_NAK_TAG)?;
    Ok(CONNECT_REPLY_LEN)
}

/// Encode a 17-byte poll (keepalive) request.
///
/// Layout per `ircDDBGateway/Common/PollData.cpp:170-186`
/// (`getDCSData` direction `DIR_OUTGOING`):
/// - `[0..8]`: space-padded client callsign
/// - `[8]`: `0x00`
/// - `[9..17]`: space-padded reflector callsign
///
/// # Errors
///
/// Returns [`EncodeError::BufferTooSmall`] if `out.len() < 17`.
///
/// # See also
///
/// `ircDDBGateway/Common/PollData.cpp:170-186` (`getDCSData`)
/// for the reference keepalive encoder.
pub fn encode_poll_request(
    out: &mut [u8],
    callsign: &Callsign,
    reflector_callsign: &Callsign,
) -> Result<usize, EncodeError> {
    write_poll(out, *callsign, *reflector_callsign)?;
    Ok(POLL_LEN)
}

/// Encode a 17-byte poll (keepalive) reply.
///
/// Identical to [`encode_poll_request`] — the server side of the poll
/// is byte-for-byte symmetric in this codec (matching xlxd's 17-byte
/// keepalive shape).
///
/// # Errors
///
/// Returns [`EncodeError::BufferTooSmall`] if `out.len() < 17`.
pub fn encode_poll_reply(
    out: &mut [u8],
    callsign: &Callsign,
    reflector_callsign: &Callsign,
) -> Result<usize, EncodeError> {
    write_poll(out, *callsign, *reflector_callsign)?;
    Ok(POLL_LEN)
}

/// Encode a 100-byte voice frame.
///
/// Layout per `ircDDBGateway/Common/AMBEData.cpp:391-431`
/// (`getDCSData`) combined with
/// `ircDDBGateway/Common/HeaderData.cpp:515-529` (`getDCSData`
/// embedding):
/// - `[0..4]`: `b"0001"` magic
/// - `[4..7]`: header flag bytes (flag1/flag2/flag3)
/// - `[7..15]`: RPT2 callsign (gateway)
/// - `[15..23]`: RPT1 callsign (access)
/// - `[23..31]`: YOUR callsign
/// - `[31..39]`: MY callsign
/// - `[39..43]`: MY suffix (4 bytes)
/// - `[43..45]`: stream id little-endian
/// - `[45]`: frame seq byte
/// - `[46..55]`: 9 AMBE bytes
/// - `[55..58]`: slow data (or `0x55 0x55 0x55` if `is_end`)
/// - `[58..61]`: 3-byte repeater sequence counter (always zero in this
///   codec — clients that care can layer counters on top)
/// - `[61]`: `0x01`
/// - `[62]`: `0x00`
/// - `[63]`: `0x21`
/// - `[64..84]`: 20-byte text field (zero-filled)
/// - `[84..100]`: 16 bytes of zero padding
///
/// # Errors
///
/// Returns [`EncodeError::BufferTooSmall`] if `out.len() < 100`.
///
/// # See also
///
/// `ircDDBGateway/Common/AMBEData.cpp:391-431` (`getDCSData`) and
/// `ircDDBGateway/Common/HeaderData.cpp:515-529` (`getDCSData`
/// embedding) for the reference encoders this function combines
/// into a single 100-byte DCS voice frame.
pub fn encode_voice(
    out: &mut [u8],
    header: &DStarHeader,
    stream_id: StreamId,
    seq: u8,
    frame: &VoiceFrame,
    is_end: bool,
) -> Result<usize, EncodeError> {
    if out.len() < VOICE_LEN {
        return Err(EncodeError::BufferTooSmall {
            need: VOICE_LEN,
            have: out.len(),
        });
    }
    // Zero-initialize the 100-byte region (AMBEData.cpp:396
    // `memset(data, 0x00U, 100U)`).
    if let Some(region) = out.get_mut(..VOICE_LEN) {
        for b in region {
            *b = 0x00;
        }
    }
    // Magic "0001" at [0..4].
    if let Some(dst) = out.get_mut(..4) {
        dst.copy_from_slice(&VOICE_MAGIC);
    }
    // Embedded header at [4..43] per HeaderData.cpp:520-528.
    if let Some(b) = out.get_mut(4) {
        *b = header.flag1;
    }
    if let Some(b) = out.get_mut(5) {
        *b = header.flag2;
    }
    if let Some(b) = out.get_mut(6) {
        *b = header.flag3;
    }
    if let Some(dst) = out.get_mut(7..15) {
        dst.copy_from_slice(header.rpt2.as_bytes());
    }
    if let Some(dst) = out.get_mut(15..23) {
        dst.copy_from_slice(header.rpt1.as_bytes());
    }
    if let Some(dst) = out.get_mut(23..31) {
        dst.copy_from_slice(header.ur_call.as_bytes());
    }
    if let Some(dst) = out.get_mut(31..39) {
        dst.copy_from_slice(header.my_call.as_bytes());
    }
    if let Some(dst) = out.get_mut(39..43) {
        dst.copy_from_slice(header.my_suffix.as_bytes());
    }
    // Stream id at [43..45] little-endian.
    let sid = stream_id.get().to_le_bytes();
    if let Some(b) = out.get_mut(43) {
        *b = sid[0];
    }
    if let Some(b) = out.get_mut(44) {
        *b = sid[1];
    }
    // Seq byte at [45]. The reference does NOT OR 0x40 for EOT in
    // DCS — the EOT marker is in bytes [55..58] instead. xlxd DOES
    // OR 0x40 (see cdcsprotocol.cpp:558), and the reference decoder
    // checks for it (AMBEData.cpp shows the same). We mirror xlxd and
    // set the bit when is_end is true, matching the protocol the
    // decoder expects.
    let seq_byte = if is_end { seq | 0x40 } else { seq };
    if let Some(b) = out.get_mut(45) {
        *b = seq_byte;
    }
    // AMBE at [46..55].
    if let Some(dst) = out.get_mut(46..55) {
        dst.copy_from_slice(&frame.ambe);
    }
    // Slow data at [55..58], or EOT sentinel if is_end.
    if is_end {
        if let Some(dst) = out.get_mut(55..58) {
            dst.copy_from_slice(&VOICE_EOT_MARKER);
        }
    } else if let Some(dst) = out.get_mut(55..58) {
        dst.copy_from_slice(&frame.slow_data);
    }
    // [58..61] rpt seq counter — zero for now.
    // [61] = 0x01, [62] = 0x00, [63] = 0x21 per AMBEData.cpp:420-423.
    if let Some(b) = out.get_mut(61) {
        *b = 0x01;
    }
    if let Some(b) = out.get_mut(62) {
        *b = 0x00;
    }
    if let Some(b) = out.get_mut(63) {
        *b = 0x21;
    }
    // [64..100] already zeroed above (text + trailing padding).
    Ok(VOICE_LEN)
}

/// Internal helper: pick the HTML banner for a given `GatewayType`.
const fn html_for(gateway_type: GatewayType) -> &'static [u8] {
    match gateway_type {
        GatewayType::Repeater => LINK_HTML_REPEATER,
        GatewayType::Hotspot => LINK_HTML_HOTSPOT,
        GatewayType::Dongle => LINK_HTML_DONGLE,
        GatewayType::StarNet => LINK_HTML_STARNET,
    }
}

/// Internal helper: write the 11-byte LINK/UNLINK prefix shared by
/// `encode_connect_link` and `encode_connect_unlink`.
///
/// Mirrors `ircDDBGateway/Common/ConnectData.cpp:323-393`
/// (`getDCSData`) which uses the same layout as `DExtra` for the
/// first 11 bytes:
/// ```text
/// memset(data, ' ', 8)                       // out[0..8] = spaces
/// for i in 0..min(repeater.Len(), 7)         // out[0..7] = first 7 chars
///     data[i] = repeater.GetChar(i)
/// data[8] = repeater.GetChar(7)              // out[8] = client module
/// data[9] = reflector.GetChar(7) or 0x20     // out[9] = byte9
/// data[10] = 0x00
/// ```
fn write_connect_prefix(out: &mut [u8], callsign: Callsign, byte8: u8, byte9: u8) {
    // Fill the first 11 bytes with spaces so out[7] stays as the
    // memset pad slot regardless of what's written later.
    if let Some(region) = out.get_mut(..11) {
        region.fill(b' ');
    }
    // out[0..7] = first 7 chars of the client/repeater callsign.
    // The reference loop uses `i < 7`, so we never touch out[7].
    let cs = callsign.as_bytes();
    if let Some(dst) = out.get_mut(..7)
        && let Some(src) = cs.get(..7)
    {
        dst.copy_from_slice(src);
    }
    // out[8] = client module letter.
    if let Some(b) = out.get_mut(8) {
        *b = byte8;
    }
    // out[9] = reflector module letter (or 0x20 for UNLINK).
    if let Some(b) = out.get_mut(9) {
        *b = byte9;
    }
    // out[10] = 0x00.
    if let Some(b) = out.get_mut(10) {
        *b = 0x00;
    }
}

/// Internal helper: write a 14-byte connect reply (ACK or NAK).
///
/// Layout per `ircDDBGateway/Common/ConnectData.cpp:374-393`
/// (`getDCSData CT_ACK`/`CT_NAK`):
/// - `[0..7]` first 7 chars of the echoed callsign
/// - `[7]` space padding (from `memset`)
/// - `[8]` 8th char of the echoed callsign — the repeater/client
///   module letter (`m_repeater.GetChar(7)`)
/// - `[9]` reflector module letter
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
    // Fill the first 14 bytes with spaces so out[7] is the pad
    // slot and any unwritten positions default to space.
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

/// Internal helper: write a 17-byte poll packet.
fn write_poll(
    out: &mut [u8],
    callsign: Callsign,
    reflector_callsign: Callsign,
) -> Result<(), EncodeError> {
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
    if let Some(dst) = out.get_mut(9..17) {
        dst.copy_from_slice(reflector_callsign.as_bytes());
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
            rpt2: Callsign::from_wire_bytes(*b"DCS001 G"),
            rpt1: Callsign::from_wire_bytes(*b"DCS001 C"),
            ur_call: Callsign::from_wire_bytes(*b"CQCQCQ  "),
            my_call: Callsign::from_wire_bytes(*b"W1AW    "),
            my_suffix: Suffix::EMPTY,
        }
    }

    // ─── LINK tests ──────────────────────────────────────────
    #[test]
    fn encode_connect_link_writes_519_bytes() -> TestResult {
        let mut buf = [0u8; 600];
        let n = encode_connect_link(
            &mut buf,
            &cs(*b"W1AW    "),
            Module::B,
            Module::C,
            &cs(*b"DCS001  "),
            GatewayType::Repeater,
        )?;
        assert_eq!(n, 519);
        // First 7 chars of client callsign at [0..7], pad at [7],
        // client module at [8], reflector module at [9], NUL at [10].
        assert_eq!(&buf[..7], b"W1AW   ", "client callsign chars 0..7");
        assert_eq!(buf[7], b' ', "pad slot");
        assert_eq!(buf[8], b'B', "client module at [8]");
        assert_eq!(buf[9], b'C', "reflector module at [9]");
        assert_eq!(buf[10], 0x00, "null at [10]");
        // Reflector callsign: first 7 chars at [11..18], space at [18].
        assert_eq!(&buf[11..18], b"DCS001 ", "reflector callsign chars 0..7");
        assert_eq!(buf[18], b' ', "reflector pad slot");
        // HTML at [19..]
        assert!(
            buf[19..].iter().any(|&b| b != 0),
            "HTML region not all zero"
        );
        Ok(())
    }

    #[test]
    fn encode_connect_link_rejects_small_buffer() -> TestResult {
        let mut buf = [0u8; 500];
        let Err(err) = encode_connect_link(
            &mut buf,
            &cs(*b"W1AW    "),
            Module::B,
            Module::C,
            &cs(*b"DCS001  "),
            GatewayType::Repeater,
        ) else {
            return Err("expected BufferTooSmall error".into());
        };
        assert_eq!(
            err,
            EncodeError::BufferTooSmall {
                need: 519,
                have: 500,
            }
        );
        Ok(())
    }

    #[test]
    fn encode_connect_link_hotspot_banner_differs_from_repeater() -> TestResult {
        let mut buf_rep = [0u8; 600];
        let mut buf_hot = [0u8; 600];
        let _n1 = encode_connect_link(
            &mut buf_rep,
            &cs(*b"W1AW    "),
            Module::B,
            Module::C,
            &cs(*b"DCS001  "),
            GatewayType::Repeater,
        )?;
        let _n2 = encode_connect_link(
            &mut buf_hot,
            &cs(*b"W1AW    "),
            Module::B,
            Module::C,
            &cs(*b"DCS001  "),
            GatewayType::Hotspot,
        )?;
        assert_ne!(
            &buf_rep[19..519],
            &buf_hot[19..519],
            "HTML region should differ by gateway type"
        );
        Ok(())
    }

    // ─── UNLINK tests ────────────────────────────────────────
    #[test]
    fn encode_connect_unlink_writes_19_bytes() -> TestResult {
        let mut buf = [0u8; 32];
        let n = encode_connect_unlink(&mut buf, &cs(*b"W1AW    "), Module::B, &cs(*b"DCS001  "))?;
        assert_eq!(n, 19);
        assert_eq!(&buf[..7], b"W1AW   ");
        assert_eq!(buf[7], b' ', "pad slot");
        assert_eq!(buf[8], b'B', "client module");
        assert_eq!(buf[9], b' ', "space (unlink marker)");
        assert_eq!(buf[10], 0x00);
        assert_eq!(&buf[11..18], b"DCS001 ", "reflector callsign chars 0..7");
        assert_eq!(buf[18], b' ', "reflector pad slot");
        Ok(())
    }

    #[test]
    fn encode_connect_unlink_rejects_small_buffer() -> TestResult {
        let mut buf = [0u8; 10];
        let Err(err) =
            encode_connect_unlink(&mut buf, &cs(*b"W1AW    "), Module::B, &cs(*b"DCS001  "))
        else {
            return Err("expected BufferTooSmall error".into());
        };
        assert_eq!(err, EncodeError::BufferTooSmall { need: 19, have: 10 });
        Ok(())
    }

    // ─── ACK/NAK tests ───────────────────────────────────────
    #[test]
    fn encode_connect_ack_writes_14_bytes() -> TestResult {
        let mut buf = [0u8; 32];
        // Test callsign "DCS001 C" so the 8th char is 'C' — the
        // client/repeater module letter echoed at byte [8].
        let n = encode_connect_ack(&mut buf, &cs(*b"DCS001 C"), Module::B)?;
        assert_eq!(n, 14);
        assert_eq!(&buf[..7], b"DCS001 ", "callsign chars 0..7");
        assert_eq!(buf[7], b' ', "pad slot");
        assert_eq!(buf[8], b'C', "echoed repeater module");
        assert_eq!(buf[9], b'B', "reflector module");
        assert_eq!(&buf[10..13], b"ACK");
        assert_eq!(buf[13], 0x00, "NUL terminator");
        Ok(())
    }

    #[test]
    fn encode_connect_nak_writes_14_bytes() -> TestResult {
        let mut buf = [0u8; 32];
        let n = encode_connect_nak(&mut buf, &cs(*b"DCS001 C"), Module::B)?;
        assert_eq!(n, 14);
        assert_eq!(&buf[..7], b"DCS001 ");
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
        let Err(err) = encode_connect_ack(&mut buf, &cs(*b"DCS001  "), Module::C) else {
            return Err("expected BufferTooSmall error".into());
        };
        assert_eq!(err, EncodeError::BufferTooSmall { need: 14, have: 13 });
        Ok(())
    }

    // ─── Poll tests ─────────────────────────────────────────
    #[test]
    fn encode_poll_request_writes_17_bytes() -> TestResult {
        let mut buf = [0u8; 32];
        let n = encode_poll_request(&mut buf, &cs(*b"W1AW    "), &cs(*b"DCS001  "))?;
        assert_eq!(n, 17);
        assert_eq!(&buf[..8], b"W1AW    ");
        assert_eq!(buf[8], 0x00);
        assert_eq!(&buf[9..17], b"DCS001  ");
        Ok(())
    }

    #[test]
    fn encode_poll_reply_matches_poll_request() -> TestResult {
        let mut req = [0u8; 17];
        let mut reply = [0u8; 17];
        let n1 = encode_poll_request(&mut req, &cs(*b"W1AW    "), &cs(*b"DCS001  "))?;
        let n2 = encode_poll_reply(&mut reply, &cs(*b"W1AW    "), &cs(*b"DCS001  "))?;
        assert_eq!(n1, n2);
        assert_eq!(req, reply);
        Ok(())
    }

    #[test]
    fn encode_poll_request_rejects_small_buffer() -> TestResult {
        let mut buf = [0u8; 16];
        let Err(err) = encode_poll_request(&mut buf, &cs(*b"W1AW    "), &cs(*b"DCS001  ")) else {
            return Err("expected BufferTooSmall error".into());
        };
        assert_eq!(err, EncodeError::BufferTooSmall { need: 17, have: 16 });
        Ok(())
    }

    // ─── Voice tests ────────────────────────────────────────
    #[test]
    fn encode_voice_writes_100_bytes() -> TestResult {
        let mut buf = [0u8; 128];
        let frame = VoiceFrame {
            ambe: [0x11; 9],
            slow_data: [0x22; 3],
        };
        let n = encode_voice(&mut buf, &test_header(), sid(0xCAFE), 5, &frame, false)?;
        assert_eq!(n, 100);
        assert_eq!(&buf[..4], b"0001", "magic");
        assert_eq!(buf[4], 0, "flag1");
        assert_eq!(buf[5], 0, "flag2");
        assert_eq!(buf[6], 0, "flag3");
        assert_eq!(&buf[7..15], b"DCS001 G", "rpt2");
        assert_eq!(&buf[15..23], b"DCS001 C", "rpt1");
        assert_eq!(&buf[23..31], b"CQCQCQ  ", "ur");
        assert_eq!(&buf[31..39], b"W1AW    ", "my");
        assert_eq!(&buf[39..43], b"    ", "suffix");
        assert_eq!(buf[43], 0xFE, "stream id LE low");
        assert_eq!(buf[44], 0xCA, "stream id LE high");
        assert_eq!(buf[45], 5, "seq");
        assert_eq!(&buf[46..55], &[0x11; 9], "AMBE");
        assert_eq!(&buf[55..58], &[0x22; 3], "slow data");
        assert_eq!(buf[61], 0x01);
        assert_eq!(buf[62], 0x00);
        assert_eq!(buf[63], 0x21);
        Ok(())
    }

    #[test]
    fn encode_voice_eot_sets_marker_and_seq_bit() -> TestResult {
        let mut buf = [0u8; 128];
        let frame = VoiceFrame {
            ambe: [0x11; 9],
            slow_data: [0x22; 3],
        };
        let n = encode_voice(&mut buf, &test_header(), sid(0xCAFE), 7, &frame, true)?;
        assert_eq!(n, 100);
        assert_eq!(buf[45] & 0x40, 0x40, "EOT bit set");
        assert_eq!(buf[45] & 0x3F, 7, "low bits preserve seq");
        assert_eq!(
            &buf[55..58],
            &VOICE_EOT_MARKER,
            "EOT marker replaces slow data"
        );
        Ok(())
    }

    #[test]
    fn encode_voice_rejects_small_buffer() -> TestResult {
        let mut buf = [0u8; 64];
        let frame = VoiceFrame {
            ambe: [0; 9],
            slow_data: [0; 3],
        };
        let Err(err) = encode_voice(&mut buf, &test_header(), sid(1), 0, &frame, false) else {
            return Err("expected BufferTooSmall error".into());
        };
        assert_eq!(
            err,
            EncodeError::BufferTooSmall {
                need: 100,
                have: 64,
            }
        );
        Ok(())
    }

    #[test]
    fn encode_voice_embeds_flag_bytes_verbatim() -> TestResult {
        let mut buf = [0u8; 128];
        let header = DStarHeader {
            flag1: 0xAA,
            flag2: 0xBB,
            flag3: 0xCC,
            ..test_header()
        };
        let frame = VoiceFrame {
            ambe: [0; 9],
            slow_data: [0; 3],
        };
        let _n = encode_voice(&mut buf, &header, sid(1), 0, &frame, false)?;
        assert_eq!(buf[4], 0xAA, "flag1 verbatim (not zeroed like DSVT)");
        assert_eq!(buf[5], 0xBB);
        assert_eq!(buf[6], 0xCC);
        Ok(())
    }
}
