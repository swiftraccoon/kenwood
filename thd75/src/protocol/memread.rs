//! Memory-read command codec.
//!
//! The radio accepts a fixed 13-byte request whose offset and length fields are
//! hexadecimal, and answers by echoing the request's first ten bytes followed by
//! the data as uppercase hexadecimal. This module owns both encodings. It is
//! pure logic with no async and no I/O, per the crate's layering rules.
//!
//! # Firmware requirement
//!
//! Memory reads require firmware modified by the `thd75-fw` project. The
//! command reuses the `GM` mnemonic, which on stock firmware selects the GPS
//! operating mode, so a stock radio refuses the request. Callers should confirm
//! support before trusting a read rather than assuming it.
//!
//! Because one host library talks to both stock and modified radios, the reply
//! parser here declines anything not shaped like a memory-read reply, leaving
//! genuine GPS-mode replies to the GPS parser.

use crate::error::ProtocolError;
use crate::types::{MemoryReadOffset, MemoryReadTarget, ReadLen};

use super::Response;

/// The CAT mnemonic carrying the memory-read request.
///
/// `GM` is the normal-table slot the modified firmware repurposes. On stock
/// firmware this mnemonic reads and sets the GPS operating mode, so the two
/// reply shapes have to be told apart: this module's reply parser declines
/// anything not shaped like a memory-read reply, leaving a GPS-mode reply to
/// reach its own parser.
///
/// The factory service mnemonics (`0G`, `9R`, `9E`, `2V`) are deliberately NOT
/// used here. They are quarantined out of the shared CAT codec on purpose, and
/// `thd75/tests/protocol_service.rs` enforces that. Since one host library
/// talks to both stock and modified radios, parsing a service reply here could
/// misread a genuine service response from a stock radio.
pub const MEM_READ_MNEMONIC: &str = "GM";

/// Serializes a memory-read request to its wire form.
///
/// Produces 12 characters: the mnemonic, a space, six uppercase hexadecimal
/// offset digits, a comma, and two uppercase hexadecimal length digits. The
/// shared command serializer appends the carriage return, bringing the request
/// to the 13 bytes the radio requires.
#[must_use]
pub fn serialize_read(offset: MemoryReadOffset, len: ReadLen) -> String {
    format!(
        "{MEM_READ_MNEMONIC} {:06X},{:02X}",
        offset.as_u32(),
        len.as_wire()
    )
}

/// One planned memory-read request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReadChunk {
    /// Offset this request starts at.
    pub offset: MemoryReadOffset,
    /// Number of bytes this request asks for.
    pub len: ReadLen,
}

/// Splits a DDR byte range into requests the radio will accept.
///
/// Each returned chunk asks for at most 256 bytes, and the whole range is
/// checked against the 16 MiB DDR bound before any chunk is produced. Use
/// [`plan_read_for_target`] for a different qualified patch target.
///
/// # Errors
///
/// Returns [`ValidationError::MemoryParamOutOfRange`] if `total` is zero, or if
/// the range would touch a byte at or beyond the DDR bound.
///
/// [`ValidationError::MemoryParamOutOfRange`]: crate::error::ValidationError::MemoryParamOutOfRange
pub fn plan_read(
    start: MemoryReadOffset,
    total: u32,
) -> Result<Vec<ReadChunk>, crate::error::ValidationError> {
    plan_read_for_target(MemoryReadTarget::DdrV103, start, total)
}

/// Splits a byte range for one qualified patched target.
///
/// Each returned chunk asks for at most 256 bytes, and the whole range is
/// checked against `target` before any chunk is produced.
///
/// # Errors
///
/// Returns [`ValidationError::MemoryParamOutOfRange`] if `total` is zero, or if
/// the range would touch a byte at or beyond the qualified target's bound.
///
/// [`ValidationError::MemoryParamOutOfRange`]: crate::error::ValidationError::MemoryParamOutOfRange
pub fn plan_read_for_target(
    target: MemoryReadTarget,
    start: MemoryReadOffset,
    total: u32,
) -> Result<Vec<ReadChunk>, crate::error::ValidationError> {
    use crate::error::ValidationError;

    if total == 0 {
        return Err(ValidationError::MemoryParamOutOfRange {
            name: "read range length",
            value: 0,
            detail: "must be at least 1 byte",
        });
    }

    // Widen to u64 so the sum cannot wrap regardless of inputs.
    let last = u64::from(start.as_u32()) + u64::from(total) - 1;
    if last >= u64::from(target.bound()) {
        let detail = match target {
            MemoryReadTarget::DdrV103 => "range must end below DDR bound 0x1000000",
            MemoryReadTarget::LowNorV103 => "range must end below low-NOR bound 0x200000",
        };
        return Err(ValidationError::MemoryParamOutOfRange {
            name: "read range end",
            value: total,
            detail,
        });
    }

    let mut chunks = Vec::new();
    let mut cursor = start.as_u32();
    let mut remaining = total;
    while remaining > 0 {
        let take = remaining.min(256);
        // `take` is 1..=256, so the u16 conversion always succeeds; the range
        // was validated above, so the offset always succeeds too. Both errors
        // are still propagated rather than unwrapped.
        let take_u16 = u16::try_from(take).map_err(|_| ValidationError::MemoryParamOutOfRange {
            name: "read length",
            value: take,
            detail: "must be 1-256",
        })?;
        chunks.push(ReadChunk {
            offset: MemoryReadOffset::new(cursor)?,
            len: ReadLen::new(take_u16)?,
        });
        cursor += take;
        remaining -= take;
    }
    Ok(chunks)
}

/// Encodes one nibble as an uppercase hexadecimal digit.
///
/// Mirrors the radio's own encoder, which adds `0x30` to a nibble below ten and
/// `0x37` otherwise, so `0xA` becomes `'A'`.
fn nibble_to_hex(nibble: u8) -> char {
    if nibble < 10 {
        char::from(b'0' + nibble)
    } else {
        char::from(b'A' + nibble - 10)
    }
}

/// Encodes bytes the way the radio encodes a reply: uppercase hexadecimal, two
/// characters per byte, high nibble first.
///
/// This is the inverse of the decoding this module performs on replies, and it
/// exists so tests and tools can build a byte-exact reply without duplicating
/// the encoding rule. Deliberately avoids `format!` per byte, which both
/// `clippy::format_collect` and `clippy::format_push_string` reject.
#[must_use]
pub fn encode_hex_upper(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len() * 2);
    for &byte in data {
        out.push(nibble_to_hex(byte >> 4));
        out.push(nibble_to_hex(byte & 0x0F));
    }
    out
}

/// Decodes one uppercase or lowercase hexadecimal digit.
const fn hex_digit(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        _ => None,
    }
}

/// Decodes one byte-exact memory-read reply during capability attestation.
///
/// Unlike the general parser, this requires the radio's observed uppercase
/// spelling, exact echoed offset, exact data length, and one final carriage
/// return. It is intentionally private to the radio layer's strict exchange.
pub(crate) fn parse_strict_read_reply(
    frame: &[u8],
    expected_offset: MemoryReadOffset,
    expected_len: ReadLen,
) -> Result<Vec<u8>, ProtocolError> {
    let field_err = |field: &str, detail: String| ProtocolError::FieldParse {
        command: MEM_READ_MNEMONIC.to_owned(),
        field: field.to_owned(),
        detail,
    };
    let prefix = format!("{MEM_READ_MNEMONIC} {expected_offset},");
    let data_len = usize::from(expected_len.as_u16()) * 2;
    let expected_frame_len = prefix.len() + data_len + 1;
    if frame.len() != expected_frame_len {
        return Err(field_err(
            "frame",
            format!(
                "expected {expected_frame_len} bytes including terminator, got {}",
                frame.len()
            ),
        ));
    }
    if !frame.starts_with(prefix.as_bytes()) {
        return Err(field_err(
            "offset",
            format!("expected exact prefix {prefix:?}"),
        ));
    }
    if frame.last().copied() != Some(b'\r') {
        return Err(field_err(
            "terminator",
            "expected one final carriage return".to_owned(),
        ));
    }

    let data_end = frame.len() - 1;
    let hex = frame
        .get(prefix.len()..data_end)
        .ok_or_else(|| field_err("data", "missing data field".to_owned()))?;
    let mut bytes = Vec::with_capacity(usize::from(expected_len.as_u16()));
    for pair in hex.chunks_exact(2) {
        let decode = |byte: u8| match byte {
            b'0'..=b'9' => Some(byte - b'0'),
            b'A'..=b'F' => Some(byte - b'A' + 10),
            _ => None,
        };
        let high = pair
            .first()
            .copied()
            .and_then(decode)
            .ok_or_else(|| field_err("data", "expected uppercase hexadecimal".to_owned()))?;
        let low = pair
            .get(1)
            .copied()
            .and_then(decode)
            .ok_or_else(|| field_err("data", "expected uppercase hexadecimal".to_owned()))?;
        bytes.push((high << 4) | low);
    }
    Ok(bytes)
}

/// Parses a memory-read reply.
///
/// The reply echoes the request's first ten bytes, so the offset is recovered
/// from the echo. Callers should confirm it matches what they asked for, which
/// turns a mis-routed or stale reply into an error instead of silently wrong
/// data.
///
/// Returns `None` if the mnemonic is not the memory-read mnemonic, or if the
/// payload is not shaped like a memory-read reply. Declining on shape rather
/// than erroring is what lets the same mnemonic still carry its stock meaning:
/// this parser runs before the GPS parser, and a GPS-mode reply falls through
/// to it untouched.
pub(crate) fn parse_memread(
    mnemonic: &str,
    payload: &str,
) -> Option<Result<Response, ProtocolError>> {
    if mnemonic != MEM_READ_MNEMONIC || !looks_like_memread(payload) {
        return None;
    }
    Some(parse_memread_payload(payload))
}

/// Returns `true` if `payload` has the shape of a memory-read reply.
///
/// That is an offset of hexadecimal digits, a comma, and at least one data
/// character. A stock GPS-mode reply is a single decimal digit with no comma,
/// so the two are unambiguous.
///
/// The offset is allowed to be longer than six digits here so that an
/// out-of-range offset still reaches [`parse_memread_payload`] and produces a
/// precise error instead of silently falling through to another parser.
fn looks_like_memread(payload: &str) -> bool {
    let Some((offset, rest)) = payload.split_once(',') else {
        return false;
    };
    !offset.is_empty()
        && offset.len() <= 8
        && offset.bytes().all(|b| hex_digit(b).is_some())
        && !rest.trim().is_empty()
}

/// Parses the `OOOOOO,<hex>` portion of a memory-read reply.
fn parse_memread_payload(payload: &str) -> Result<Response, ProtocolError> {
    let field_err = |field: &str, detail: String| ProtocolError::FieldParse {
        command: MEM_READ_MNEMONIC.to_owned(),
        field: field.to_owned(),
        detail,
    };

    let (offset_str, hex) = payload
        .split_once(',')
        .ok_or_else(|| field_err("offset", "missing ',' separator".to_owned()))?;

    let raw = u32::from_str_radix(offset_str, 16)
        .map_err(|e| field_err("offset", format!("{offset_str:?}: {e}")))?;
    let offset = MemoryReadOffset::new(raw).map_err(|e| field_err("offset", format!("{e}")))?;

    // `Codec::next_frame` already strips the `\r` terminator, so trimming is
    // belt-and-braces. It earns its keep on this radio because GPS NMEA output
    // shares the serial line and stray whitespace is a realistic artifact; the
    // alternative is a confusing "odd hex length" error for a sound reply.
    let hex_bytes = hex.trim().as_bytes();
    if hex_bytes.is_empty() {
        return Err(field_err("data", "no data bytes".to_owned()));
    }
    if hex_bytes.len() % 2 != 0 {
        return Err(field_err(
            "data",
            format!("odd hex length {}", hex_bytes.len()),
        ));
    }

    let mut bytes = Vec::with_capacity(hex_bytes.len() / 2);
    for pair in hex_bytes.chunks_exact(2) {
        let hi = pair
            .first()
            .copied()
            .and_then(hex_digit)
            .ok_or_else(|| field_err("data", "invalid hex digit".to_owned()))?;
        let lo = pair
            .get(1)
            .copied()
            .and_then(hex_digit)
            .ok_or_else(|| field_err("data", "invalid hex digit".to_owned()))?;
        bytes.push((hi << 4) | lo);
    }
    Ok(Response::MemoryData { offset, bytes })
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn serialize_omits_the_terminator() -> TestResult {
        // The shared serializer appends '\r'; this function must not.
        let wire = serialize_read(MemoryReadOffset::new(0x10)?, ReadLen::new(2)?);
        assert_eq!(wire, "GM 000010,02");
        assert!(!wire.ends_with('\r'), "terminator is added by the caller");
        Ok(())
    }

    #[test]
    fn strict_reply_accepts_only_the_exact_radio_form() -> TestResult {
        let offset = MemoryReadOffset::new(0x10)?;
        let length = ReadLen::new(2)?;
        assert_eq!(
            parse_strict_read_reply(b"GM 000010,DEAD\r", offset, length)?,
            [0xDE, 0xAD]
        );
        for invalid in [
            b"GM 000010,dead\r".as_slice(),
            b"GM 000011,DEAD\r",
            b"GM 000010,DE\r",
            b"GM 000010,DEAD \r",
            b"GM 000010,DEAD\r\r",
        ] {
            let result = parse_strict_read_reply(invalid, offset, length);
            assert!(
                result.is_err(),
                "strict decoder accepted malformed reply {invalid:?}"
            );
        }
        Ok(())
    }

    #[test]
    fn plans_exact_multiple_of_256() -> TestResult {
        let chunks = plan_read(MemoryReadOffset::ZERO, 512)?;
        assert_eq!(chunks.len(), 2);
        let first = chunks.first().ok_or("missing first chunk")?;
        let second = chunks.get(1).ok_or("missing second chunk")?;
        assert_eq!(first.offset.as_u32(), 0);
        assert_eq!(first.len.as_u16(), 256);
        assert_eq!(second.offset.as_u32(), 256);
        assert_eq!(second.len.as_u16(), 256);
        Ok(())
    }

    #[test]
    fn plans_remainder_chunk() -> TestResult {
        let chunks = plan_read(MemoryReadOffset::ZERO, 300)?;
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks.get(1).ok_or("missing chunk")?.len.as_u16(), 44);
        Ok(())
    }

    #[test]
    fn plans_single_short_chunk() -> TestResult {
        let chunks = plan_read(MemoryReadOffset::new(0x17_D1BC)?, 64)?;
        assert_eq!(chunks.len(), 1);
        let only = chunks.first().ok_or("missing chunk")?;
        assert_eq!(only.offset.as_u32(), 0x17_D1BC);
        assert_eq!(only.len.as_u16(), 64);
        Ok(())
    }

    #[test]
    fn top_of_window_is_fully_readable() -> TestResult {
        // 0xFFFF00 + 256 - 1 == 0xFFFFFF, which is inside the bound.
        let chunks = plan_read(MemoryReadOffset::new(0xFF_FF00)?, 256)?;
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks.first().ok_or("missing chunk")?.len.as_u16(), 256);
        Ok(())
    }

    #[test]
    fn rejects_range_crossing_the_bound() -> TestResult {
        let result = plan_read(MemoryReadOffset::new(0xFF_FF00)?, 257);
        assert!(
            result.is_err(),
            "a range ending at 0x1000000 must be rejected, got {result:?}"
        );
        Ok(())
    }

    #[test]
    fn rejects_zero_total() {
        let result = plan_read(MemoryReadOffset::ZERO, 0);
        assert!(
            result.is_err(),
            "zero-byte read must be rejected, got {result:?}"
        );
    }

    #[test]
    fn low_nor_planner_rejects_the_unqualified_remainder_of_the_wire_window() -> TestResult {
        let result = plan_read_for_target(
            MemoryReadTarget::LowNorV103,
            MemoryReadOffset::new(0x001F_FF00)?,
            257,
        );
        assert!(
            result.is_err(),
            "a range crossing the 2 MiB low-NOR qualification must be rejected"
        );

        let chunks = plan_read_for_target(
            MemoryReadTarget::LowNorV103,
            MemoryReadOffset::new(0x001F_FF00)?,
            256,
        )?;
        assert_eq!(chunks.len(), 1);
        Ok(())
    }
}
