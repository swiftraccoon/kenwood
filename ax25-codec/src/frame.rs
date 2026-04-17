//! AX.25 frame encode/decode.
//!
//! This module hosts the [`Ax25Packet`] UI-frame type, the byte-level
//! [`parse_ax25`]/[`build_ax25`] codec, and the FCS helper [`ax25_fcs`].
//! All APIs are `no_std`-compatible.

use alloc::string::String;
use alloc::vec::Vec;

use crate::address::{Ax25Address, Callsign, Ssid};
use crate::control::{Ax25Control, CommandResponse};
use crate::error::Ax25Error;
use crate::pid::Ax25Pid;

/// Maximum number of digipeater addresses in an AX.25 v2.2 frame (§2.2.13).
pub const MAX_DIGIPEATERS: usize = 8;

/// Compute the AX.25 Frame Check Sequence (CRC-16-CCITT, polynomial
/// `0x1021`, initial value `0xFFFF`, reflected, `xorout = 0xFFFF`) over
/// a byte slice.
///
/// KISS frames do not carry the FCS — the TNC computes and strips it —
/// but this function is provided for callers working with raw AX.25
/// over a transport that does expect the FCS (e.g. a software modem,
/// SDR, or packet capture tool). The byte order on the wire is
/// little-endian: emit `(crc & 0xFF)` then `(crc >> 8)`.
#[must_use]
pub fn ax25_fcs(bytes: &[u8]) -> u16 {
    let mut crc: u16 = 0xFFFF;
    for &b in bytes {
        let mut b = b;
        for _ in 0..8 {
            let carry = (crc & 0x0001) != (u16::from(b) & 0x0001);
            crc >>= 1;
            b >>= 1;
            if carry {
                crc ^= 0x8408;
            }
        }
    }
    !crc
}

/// A parsed AX.25 UI (Unnumbered Information) frame.
///
/// APRS uses UI frames exclusively. The control field is `0x03` and
/// the protocol ID is `0xF0` (no layer 3) for standard APRS packets.
///
/// To inspect the AX.25 v2.2 Command/Response bits encoded in the C
/// bits of the source and destination SSID bytes, use
/// [`Self::command_response`] (re-derived on demand from the wire bytes
/// preserved by the parser).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ax25Packet {
    /// Source station address.
    pub source: Ax25Address,
    /// Destination address (often an APRS "tocall" like `APxxxx`).
    pub destination: Ax25Address,
    /// Digipeater path (0-8 via addresses).
    pub digipeaters: Vec<Ax25Address>,
    /// Control field (0x03 for UI frames).
    pub control: u8,
    /// Protocol identifier (0xF0 = no layer 3, standard for APRS).
    pub protocol: u8,
    /// Information field (the APRS payload).
    pub info: Vec<u8>,
}

impl Ax25Packet {
    /// Decode the raw [`Self::control`] byte into a typed [`Ax25Control`]
    /// value.
    #[must_use]
    pub const fn control_typed(&self) -> Ax25Control {
        Ax25Control::from_byte(self.control)
    }

    /// Decode the raw [`Self::protocol`] byte into a typed [`Ax25Pid`]
    /// value.
    #[must_use]
    pub const fn pid(&self) -> Ax25Pid {
        Ax25Pid::from_byte(self.protocol)
    }

    /// `true` if this is a UI frame (APRS standard).
    #[must_use]
    pub const fn is_ui(&self) -> bool {
        self.control_typed().is_ui()
    }

    /// AX.25 v2.2 Command/Response classification of this frame, derived
    /// from the C-bits captured at parse time on the source and
    /// destination addresses (per AX.25 v2.2 §4.3.1.2).
    ///
    /// - `dest C-bit = 1, source C-bit = 0` → Command (APRS frames)
    /// - `dest C-bit = 0, source C-bit = 1` → Response
    /// - both equal → Legacy (AX.25 v2.0 or unknown)
    #[must_use]
    pub const fn command_response(&self) -> CommandResponse {
        match (self.destination.c_bit, self.source.c_bit) {
            (true, false) => CommandResponse::Command,
            (false, true) => CommandResponse::Response,
            _ => CommandResponse::Legacy,
        }
    }
}

/// Decode a single AX.25 address from a 7-byte slice.
///
/// The H-bit (has-been-repeated) is extracted from bit 7 of the SSID
/// byte. For digipeater addresses, this indicates the hop has been used.
///
/// Per AX.25 v2.2 §3.2, callsign characters must be ASCII alphanumeric.
/// Any byte that decodes to a character outside `A-Z`, `a-z`, `0-9`, or
/// space (the pad character) is rejected.
///
/// # Errors
///
/// Returns [`Ax25Error::InvalidCallsignByte`] if any callsign byte
/// decodes to a non-alphanumeric character. Space is treated as padding
/// and stripped.
fn decode_ax25_address(bytes: &[u8]) -> Result<Ax25Address, Ax25Error> {
    let callsign_bytes = bytes.get(..6).ok_or(Ax25Error::PacketTooShort)?;
    let mut callsign = String::with_capacity(6);
    for &b in callsign_bytes {
        let ch = b >> 1;
        if ch == b' ' {
            // Pad; skip.
            continue;
        }
        if !ch.is_ascii_alphanumeric() {
            return Err(Ax25Error::InvalidCallsignByte(ch));
        }
        // AX.25 callsigns are stored uppercase; the parser tolerates
        // lowercase on the wire by uppercasing during construction.
        callsign.push(ch.to_ascii_uppercase() as char);
    }
    let ssid_byte = *bytes.get(6).ok_or(Ax25Error::PacketTooShort)?;
    let ssid_raw = (ssid_byte >> 1) & 0x0F;
    let repeated = ssid_byte & 0x80 != 0;
    // The "C bit" is bit 7 of the SSID byte for source/destination
    // addresses (parsed independently from the H-bit which only applies
    // to digipeater entries — but the wire bit is the same position;
    // higher layers know which interpretation applies based on the
    // address slot).
    let c_bit = ssid_byte & 0x80 != 0;
    let callsign = Callsign::new(&callsign).map_err(|_| Ax25Error::InvalidCallsignByte(0))?;
    let ssid = Ssid::new(ssid_raw).map_err(|_| Ax25Error::InvalidCallsignByte(ssid_raw))?;
    Ok(Ax25Address {
        callsign,
        ssid,
        repeated,
        c_bit,
    })
}

/// Encode an AX.25 address into 7 bytes.
///
/// `is_last` sets the address-extension bit on the final address.
/// The H-bit (has-been-repeated) is encoded into bit 7 of the SSID byte.
fn encode_ax25_address(addr: &Ax25Address, is_last: bool) -> [u8; 7] {
    let mut bytes = [0x40u8; 7]; // space << 1 = 0x40
    for (slot, &ch) in bytes
        .iter_mut()
        .take(6)
        .zip(addr.callsign.as_bytes().iter().take(6))
    {
        *slot = ch << 1;
    }
    let mut ssid_byte = 0x60 | ((addr.ssid.get() & 0x0F) << 1);
    if is_last {
        ssid_byte |= 0x01; // address-extension bit
    }
    if addr.repeated {
        ssid_byte |= 0x80; // H-bit (has-been-repeated)
    }
    bytes[6] = ssid_byte;
    bytes
}

/// Parse an AX.25 packet from raw bytes (as received in a KISS data frame).
///
/// Handles the standard UI frame format used by APRS:
/// `destination(7) | source(7) | [digipeaters(7 each)] | control(1) | PID(1) | info(N)`
///
/// # Errors
///
/// Returns [`Ax25Error`] if the packet structure is invalid.
pub fn parse_ax25(data: &[u8]) -> Result<Ax25Packet, Ax25Error> {
    // Minimum: dest(7) + src(7) + control(1) + PID(1) = 16
    if data.len() < 16 {
        return Err(Ax25Error::PacketTooShort);
    }

    let dest_bytes = data.get(0..7).ok_or(Ax25Error::PacketTooShort)?;
    let src_bytes = data.get(7..14).ok_or(Ax25Error::PacketTooShort)?;
    let destination = decode_ax25_address(dest_bytes)?;
    let source = decode_ax25_address(src_bytes)?;

    // Find end of address field (bit 0 of last byte in each 7-byte address)
    let mut addr_end = 14;
    let mut digipeaters = Vec::new();

    // Check if source address has the extension bit set
    let source_ext_byte = *data.get(13).ok_or(Ax25Error::PacketTooShort)?;
    if source_ext_byte & 0x01 == 0 {
        // More addresses follow (digipeaters). AX.25 v2.2 §2.2.13 caps
        // this at 8 — reject packets claiming more to avoid unbounded
        // allocation from a malformed frame.
        loop {
            if digipeaters.len() >= MAX_DIGIPEATERS {
                return Err(Ax25Error::TooManyDigipeaters);
            }
            let digi_slice = data
                .get(addr_end..addr_end + 7)
                .ok_or(Ax25Error::InvalidAddressLength)?;
            let digi = decode_ax25_address(digi_slice)?;
            let last_byte = *digi_slice.get(6).ok_or(Ax25Error::InvalidAddressLength)?;
            let is_last = last_byte & 0x01 != 0;
            digipeaters.push(digi);
            addr_end += 7;
            if is_last {
                break;
            }
        }
    }

    // After addresses: control + PID
    if addr_end + 2 > data.len() {
        return Err(Ax25Error::MissingControlFields);
    }

    let control = *data.get(addr_end).ok_or(Ax25Error::MissingControlFields)?;
    let protocol = *data
        .get(addr_end + 1)
        .ok_or(Ax25Error::MissingControlFields)?;
    let info = data.get(addr_end + 2..).unwrap_or(&[]).to_vec();

    Ok(Ax25Packet {
        source,
        destination,
        digipeaters,
        control,
        protocol,
        info,
    })
}

/// Build an AX.25 UI frame from an [`Ax25Packet`].
///
/// Returns the raw bytes suitable for encapsulation in a KISS data frame.
///
/// # Panics
///
/// Panics if the packet has more than [`MAX_DIGIPEATERS`] digipeater
/// addresses, which is the AX.25 v2.2 §2.2.13 cap. Use [`parse_ax25`] to
/// validate packets coming from untrusted sources before building them.
#[must_use]
pub fn build_ax25(packet: &Ax25Packet) -> Vec<u8> {
    assert!(
        packet.digipeaters.len() <= MAX_DIGIPEATERS,
        "AX.25 v2.2 §2.2.13: packet has {} digipeaters, max is {MAX_DIGIPEATERS}",
        packet.digipeaters.len(),
    );
    let no_digis = packet.digipeaters.is_empty();
    let total_len = 14 + packet.digipeaters.len() * 7 + 2 + packet.info.len();
    let mut out = Vec::with_capacity(total_len);

    // Destination (never last unless no source... but source always follows)
    out.extend_from_slice(&encode_ax25_address(&packet.destination, false));

    // Source
    out.extend_from_slice(&encode_ax25_address(&packet.source, no_digis));

    // Digipeaters
    let digi_count = packet.digipeaters.len();
    for (i, digi) in packet.digipeaters.iter().enumerate() {
        let is_last = i + 1 == digi_count;
        out.extend_from_slice(&encode_ax25_address(digi, is_last));
    }

    out.push(packet.control);
    out.push(packet.protocol);
    out.extend_from_slice(&packet.info);

    out
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use alloc::boxed::Box;
    use alloc::vec;

    use super::*;

    type TestResult = Result<(), Box<dyn core::error::Error>>;

    fn make_test_ax25_bytes() -> Vec<u8> {
        let mut frame = Vec::new();

        // Destination: "APRS  " (shifted left by 1, SSID 0, not last)
        for &ch in b"APRS  " {
            frame.push(ch << 1);
        }
        frame.push(0x60); // SSID 0, not last

        // Source: "N0CALL" (shifted left by 1, SSID 7, last address)
        for &ch in b"N0CALL" {
            frame.push(ch << 1);
        }
        frame.push(0x60 | (7 << 1) | 0x01); // SSID 7, last

        // Control + PID (UI frame, no layer 3)
        frame.push(0x03);
        frame.push(0xF0);

        // Info
        frame.extend_from_slice(b"!4903.50N/07201.75W-Test");

        frame
    }

    #[test]
    fn parse_ax25_basic() -> TestResult {
        let data = make_test_ax25_bytes();
        let packet = parse_ax25(&data)?;
        assert_eq!(packet.destination.callsign, "APRS");
        assert_eq!(packet.destination.ssid, 0);
        assert_eq!(packet.source.callsign, "N0CALL");
        assert_eq!(packet.source.ssid, 7);
        assert!(packet.digipeaters.is_empty());
        assert_eq!(packet.control, 0x03);
        assert_eq!(packet.protocol, 0xF0);
        assert_eq!(&packet.info, b"!4903.50N/07201.75W-Test");
        Ok(())
    }

    #[test]
    fn parse_ax25_with_digipeaters() -> TestResult {
        let mut frame = Vec::new();

        // Destination: "APRS  "
        for &ch in b"APRS  " {
            frame.push(ch << 1);
        }
        frame.push(0x60); // not last

        // Source: "W6DJY "
        for &ch in b"W6DJY " {
            frame.push(ch << 1);
        }
        frame.push(0x60 | (9 << 1)); // SSID 9, not last (has digis)

        // Digipeater 1: "WIDE1 "
        for &ch in b"WIDE1 " {
            frame.push(ch << 1);
        }
        frame.push(0x60 | (1 << 1)); // SSID 1, not last

        // Digipeater 2: "WIDE2 "
        for &ch in b"WIDE2 " {
            frame.push(ch << 1);
        }
        frame.push(0x60 | (1 << 1) | 0x01); // SSID 1, last address

        frame.push(0x03);
        frame.push(0xF0);
        frame.extend_from_slice(b"=test data");

        let packet = parse_ax25(&frame)?;
        assert_eq!(packet.source.callsign, "W6DJY");
        assert_eq!(packet.source.ssid, 9);
        assert_eq!(packet.digipeaters.len(), 2);
        let digi0 = packet.digipeaters.first().ok_or("missing digi 0")?;
        let digi1 = packet.digipeaters.get(1).ok_or("missing digi 1")?;
        assert_eq!(digi0.callsign, "WIDE1");
        assert_eq!(digi0.ssid, 1);
        assert_eq!(digi1.callsign, "WIDE2");
        assert_eq!(digi1.ssid, 1);
        Ok(())
    }

    #[test]
    fn ax25_roundtrip() -> TestResult {
        let original = Ax25Packet {
            source: Ax25Address::new("N0CALL", 7),
            destination: Ax25Address::new("APRS", 0),
            digipeaters: vec![Ax25Address::new("WIDE1", 1), Ax25Address::new("WIDE2", 1)],
            control: 0x03,
            protocol: 0xF0,
            info: b"!4903.50N/07201.75W-Test 73".to_vec(),
        };

        let bytes = build_ax25(&original);
        let parsed = parse_ax25(&bytes)?;

        assert_eq!(parsed.source.callsign, original.source.callsign);
        assert_eq!(parsed.source.ssid, original.source.ssid);
        assert_eq!(parsed.destination.callsign, original.destination.callsign);
        assert_eq!(parsed.destination.ssid, original.destination.ssid);
        assert_eq!(parsed.digipeaters.len(), original.digipeaters.len());
        for (a, b) in parsed.digipeaters.iter().zip(&original.digipeaters) {
            assert_eq!(a.callsign, b.callsign);
            assert_eq!(a.ssid, b.ssid);
        }
        assert_eq!(parsed.control, original.control);
        assert_eq!(parsed.protocol, original.protocol);
        assert_eq!(parsed.info, original.info);
        Ok(())
    }

    #[test]
    fn parse_ax25_too_short() {
        let r = parse_ax25(&[0; 10]);
        assert!(r.is_err(), "expected error for too-short input, got {r:?}");
    }

    #[test]
    fn parse_ax25_rejects_more_than_8_digipeaters() {
        // Build a packet claiming 9 digipeaters. Each address is 7 bytes,
        // and the source extension bit is 0, so the parser follows the
        // digi chain. We set the E-bit only on the 9th digi.
        let mut frame = Vec::new();
        for &ch in b"APRS  " {
            frame.push(ch << 1);
        }
        frame.push(0x60); // dest ssid 0, not last

        for &ch in b"N0CALL" {
            frame.push(ch << 1);
        }
        frame.push(0x60); // source ssid 0, not last → digis follow

        // 9 digipeater addresses, last one has E-bit.
        for i in 0..9 {
            for &ch in b"WIDE1 " {
                frame.push(ch << 1);
            }
            let ssid_byte = 0x60 | (1 << 1) | u8::from(i == 8);
            frame.push(ssid_byte);
        }
        frame.push(0x03);
        frame.push(0xF0);
        frame.extend_from_slice(b"!test");

        let result = parse_ax25(&frame);
        assert_eq!(result, Err(Ax25Error::TooManyDigipeaters));
    }

    #[test]
    fn ax25_fcs_known_value() {
        // Known test vector: CRC-CCITT of "123456789" in reflected form
        // equals 0x906E. (See CRC-16/X-25, matches AX.25 FCS.)
        assert_eq!(ax25_fcs(b"123456789"), 0x906E);
    }

    #[test]
    fn ax25_fcs_empty_matches_init_xor() {
        // Initial value 0xFFFF with xorout 0xFFFF → 0x0000 for empty.
        assert_eq!(ax25_fcs(&[]), 0x0000);
    }

    #[test]
    fn ax25_packet_command_response_classification() {
        // APRS frames have dest C-bit=1, src C-bit=0 → Command.
        let mut packet = Ax25Packet {
            source: Ax25Address::new("N0CALL", 7),
            destination: Ax25Address::new("APRS", 0),
            digipeaters: vec![],
            control: 0x03,
            protocol: 0xF0,
            info: b"!test".to_vec(),
        };
        packet.destination.c_bit = true;
        packet.source.c_bit = false;
        assert_eq!(packet.command_response(), CommandResponse::Command);

        packet.destination.c_bit = false;
        packet.source.c_bit = true;
        assert_eq!(packet.command_response(), CommandResponse::Response);

        packet.destination.c_bit = false;
        packet.source.c_bit = false;
        assert_eq!(packet.command_response(), CommandResponse::Legacy);
    }

    #[test]
    fn ax25_packet_typed_accessors() {
        let packet = Ax25Packet {
            source: Ax25Address::new("N0CALL", 7),
            destination: Ax25Address::new("APRS", 0),
            digipeaters: vec![],
            control: 0x03,
            protocol: 0xF0,
            info: b"!test".to_vec(),
        };
        assert!(packet.is_ui());
        assert_eq!(packet.pid(), Ax25Pid::NoLayer3);
    }

    #[test]
    fn decode_ax25_address_rejects_non_ascii() {
        // Byte 0x24 >> 1 = 0x12 is a control char, not alphanumeric.
        let bytes = [
            b'N' << 1,
            b'0' << 1,
            0x24,
            b'A' << 1,
            b'L' << 1,
            b'L' << 1,
            0x60,
        ];
        let err = decode_ax25_address(&bytes);
        assert!(
            matches!(err, Err(Ax25Error::InvalidCallsignByte(0x12))),
            "expected InvalidCallsignByte(0x12), got {err:?}"
        );
    }
}
