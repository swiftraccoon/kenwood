//! AX.25 frame encode/decode.
//!
//! This module hosts the [`Ax25Packet`] frame type, the byte-level
//! [`parse_ax25`]/[`build_ax25`] codec, and the FCS helper [`ax25_fcs`].
//! All APIs are `no_std`-compatible.

use alloc::string::String;
use alloc::vec::Vec;

use crate::address::{Ax25Address, Callsign, RouteEntry, Ssid};
use crate::control::{Ax25Control, CommandResponse};
use crate::error::Ax25Error;
use crate::pid::Ax25Pid;

/// Maximum number of digipeater addresses in an AX.25 frame.
///
/// Matches AX.25 v2.0 / APRS deployment convention and Linux kernel
/// `AX25_MAX_DIGIS`. AX.25 v2.2 §3.12.5 reduced this to 2 but no APRS
/// network respects that limit.
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

/// A parsed AX.25 frame.
///
/// The control byte and protocol identifier are kept as raw `u8` for
/// flexibility (UI frames use `0x03` / `0xF0`); use [`Self::control_typed`]
/// and [`Self::pid`] for the decoded enums.
///
/// Command/Response classification (AX.25 v2.2 §6.1.2) lives at the
/// frame level rather than per-address, because the encoding rule is
/// joint over the destination and source SSID-byte C-bits. See
/// [`Self::command_or_response`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ax25Packet {
    /// Source station address.
    pub source: Ax25Address,
    /// Destination address (often an APRS "tocall" like `APxxxx`).
    pub destination: Ax25Address,
    /// Digipeater path (0..=[`MAX_DIGIPEATERS`] entries).
    pub digipeaters: Vec<RouteEntry>,
    /// AX.25 v2.2 Command/Response classification:
    /// - `(dest_c=1, src_c=0)` → `Some(Command)` (APRS convention)
    /// - `(dest_c=0, src_c=1)` → `Some(Response)`
    /// - both equal → `None` (legacy v2.0 / unknown)
    pub command_or_response: Option<CommandResponse>,
    /// Control field (`0x03` for UI frames).
    pub control: u8,
    /// Protocol identifier (`0xF0` = no layer 3, standard for APRS).
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
}

// ---------------------------------------------------------------------------
// Internal decode / encode
// ---------------------------------------------------------------------------

/// Decode a single AX.25 address slot from a 7-byte slice. Returns the
/// validated [`Ax25Address`] plus the raw wire bit 7 (interpretation
/// depends on slot — caller decides).
///
/// Rejects callsign bytes outside ASCII A-Z / 0-9 (after the AX.25
/// `<< 1` shift is undone) and rejects embedded spaces (only trailing
/// space padding is permitted per §3.12.2).
fn decode_address(bytes: [u8; 7]) -> Result<(Ax25Address, bool), Ax25Error> {
    let mut callsign = String::with_capacity(6);
    let mut in_pad = false;
    for &b in bytes.iter().take(6) {
        let ch = b >> 1;
        if ch == b' ' {
            in_pad = true;
            continue;
        }
        if in_pad {
            // Non-space byte after padding starts — malformed per §3.12.2.
            return Err(Ax25Error::InvalidCallsignByte(ch));
        }
        if !ch.is_ascii_alphanumeric() {
            return Err(Ax25Error::InvalidCallsignByte(ch));
        }
        callsign.push(ch.to_ascii_uppercase() as char);
    }
    let ssid_byte = *bytes.get(6).ok_or(Ax25Error::PacketTooShort)?;
    let ssid_raw = (ssid_byte >> 1) & 0x0F;
    let bit7 = ssid_byte & 0x80 != 0;
    let callsign = Callsign::new(&callsign).map_err(|_| Ax25Error::InvalidCallsignByte(0))?;
    let ssid = Ssid::new(ssid_raw).map_err(|_| Ax25Error::InvalidCallsignByte(ssid_raw))?;
    Ok((Ax25Address::from_parts(callsign, ssid), bit7))
}

/// Encode a single AX.25 address slot into 7 bytes. `bit7` is written
/// to wire bit 7 (interpretation depends on slot — caller decides).
/// `is_last` sets the address-extension bit on the final address.
fn encode_address(addr: &Ax25Address, bit7: bool, is_last: bool) -> [u8; 7] {
    let mut bytes = [0x40u8; 7]; // space << 1 = 0x40 — right-pads to 6 chars
    for (slot, &ch) in bytes
        .iter_mut()
        .take(6)
        .zip(addr.callsign.as_bytes().iter())
    {
        *slot = ch << 1;
    }
    let mut ssid_byte = 0x60 | ((addr.ssid.get() & 0x0F) << 1);
    if is_last {
        ssid_byte |= 0x01;
    }
    if bit7 {
        ssid_byte |= 0x80;
    }
    bytes[6] = ssid_byte;
    bytes
}

/// Parse an AX.25 packet from raw bytes (as received in a KISS data frame).
///
/// Handles the standard frame format:
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

    let dest_bytes: [u8; 7] = data
        .get(0..7)
        .and_then(|s| <[u8; 7]>::try_from(s).ok())
        .ok_or(Ax25Error::PacketTooShort)?;
    let src_bytes: [u8; 7] = data
        .get(7..14)
        .and_then(|s| <[u8; 7]>::try_from(s).ok())
        .ok_or(Ax25Error::PacketTooShort)?;

    let (destination, dest_c_bit) = decode_address(dest_bytes)?;
    let (source, src_c_bit) = decode_address(src_bytes)?;

    let command_or_response = match (dest_c_bit, src_c_bit) {
        (true, false) => Some(CommandResponse::Command),
        (false, true) => Some(CommandResponse::Response),
        _ => None,
    };

    // Find end of address field via the address-extension bit on the
    // last byte of each 7-byte address.
    let mut addr_end = 14;
    let mut digipeaters: Vec<RouteEntry> = Vec::new();

    let source_ext_byte = *data.get(13).ok_or(Ax25Error::PacketTooShort)?;
    if source_ext_byte & 0x01 == 0 {
        // More addresses follow (digipeaters). MAX_DIGIPEATERS cap
        // prevents unbounded allocation from a malformed frame.
        loop {
            if digipeaters.len() >= MAX_DIGIPEATERS {
                return Err(Ax25Error::TooManyDigipeaters);
            }
            let digi_slice: [u8; 7] = data
                .get(addr_end..addr_end + 7)
                .and_then(|s| <[u8; 7]>::try_from(s).ok())
                .ok_or(Ax25Error::InvalidAddressLength)?;
            let (address, has_repeated) = decode_address(digi_slice)?;
            let last_byte = *digi_slice.get(6).ok_or(Ax25Error::InvalidAddressLength)?;
            let is_last = last_byte & 0x01 != 0;
            digipeaters.push(RouteEntry {
                address,
                has_repeated,
            });
            addr_end += 7;
            if is_last {
                break;
            }
        }
    }

    // After addresses: control + PID
    let control = *data.get(addr_end).ok_or(Ax25Error::MissingControlFields)?;
    let protocol = *data
        .get(addr_end + 1)
        .ok_or(Ax25Error::MissingControlFields)?;
    let info = data.get(addr_end + 2..).unwrap_or(&[]).to_vec();

    Ok(Ax25Packet {
        source,
        destination,
        digipeaters,
        command_or_response,
        control,
        protocol,
        info,
    })
}

/// Build an AX.25 frame from an [`Ax25Packet`].
///
/// Returns the raw bytes suitable for encapsulation in a KISS data frame.
///
/// # Panics
///
/// Panics if the packet has more than [`MAX_DIGIPEATERS`] digipeater
/// addresses. Use [`parse_ax25`] to validate packets coming from
/// untrusted sources before re-building them.
#[must_use]
pub fn build_ax25(packet: &Ax25Packet) -> Vec<u8> {
    assert!(
        packet.digipeaters.len() <= MAX_DIGIPEATERS,
        "AX.25 v2.0/APRS convention: packet has {} digipeaters, max is {MAX_DIGIPEATERS}",
        packet.digipeaters.len(),
    );
    let (dest_c, src_c) = match packet.command_or_response {
        Some(CommandResponse::Command) => (true, false),
        Some(CommandResponse::Response) => (false, true),
        None => (false, false),
    };
    let no_digis = packet.digipeaters.is_empty();
    let total_len = 14 + packet.digipeaters.len() * 7 + 2 + packet.info.len();
    let mut out = Vec::with_capacity(total_len);

    out.extend_from_slice(&encode_address(&packet.destination, dest_c, false));
    out.extend_from_slice(&encode_address(&packet.source, src_c, no_digis));

    let digi_count = packet.digipeaters.len();
    for (i, entry) in packet.digipeaters.iter().enumerate() {
        let is_last = i + 1 == digi_count;
        out.extend_from_slice(&encode_address(&entry.address, entry.has_repeated, is_last));
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

    use proptest::prelude::*;
    use proptest::test_runner::TestCaseError;

    use super::*;

    type TestResult = Result<(), Box<dyn core::error::Error>>;

    fn to_test_err<E: core::fmt::Debug>(e: E) -> TestCaseError {
        TestCaseError::fail(alloc::format!("{e:?}"))
    }

    fn make_test_ax25_bytes() -> Vec<u8> {
        let mut frame = Vec::new();
        for &ch in b"APRS  " {
            frame.push(ch << 1);
        }
        frame.push(0x60); // dest SSID 0, not last
        for &ch in b"N0CALL" {
            frame.push(ch << 1);
        }
        frame.push(0x60 | (7 << 1) | 0x01); // src SSID 7, last
        frame.push(0x03);
        frame.push(0xF0);
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
        assert_eq!(packet.command_or_response, None);
        assert_eq!(&packet.info, b"!4903.50N/07201.75W-Test");
        Ok(())
    }

    #[test]
    fn parse_ax25_with_digipeaters() -> TestResult {
        let mut frame = Vec::new();
        for &ch in b"APRS  " {
            frame.push(ch << 1);
        }
        frame.push(0x60);
        for &ch in b"W6DJY " {
            frame.push(ch << 1);
        }
        frame.push(0x60 | (9 << 1));
        for &ch in b"WIDE1 " {
            frame.push(ch << 1);
        }
        frame.push(0x60 | (1 << 1));
        for &ch in b"WIDE2 " {
            frame.push(ch << 1);
        }
        frame.push(0x60 | (1 << 1) | 0x01);
        frame.push(0x03);
        frame.push(0xF0);
        frame.extend_from_slice(b"=test data");

        let packet = parse_ax25(&frame)?;
        assert_eq!(packet.source.callsign, "W6DJY");
        assert_eq!(packet.source.ssid, 9);
        assert_eq!(packet.digipeaters.len(), 2);
        let digi0 = packet.digipeaters.first().ok_or("missing digi 0")?;
        let digi1 = packet.digipeaters.get(1).ok_or("missing digi 1")?;
        assert_eq!(digi0.address.callsign, "WIDE1");
        assert_eq!(digi0.address.ssid, 1);
        assert!(!digi0.has_repeated);
        assert_eq!(digi1.address.callsign, "WIDE2");
        assert_eq!(digi1.address.ssid, 1);
        Ok(())
    }

    #[test]
    fn ax25_roundtrip() -> TestResult {
        let original = Ax25Packet {
            source: Ax25Address::new("N0CALL", 7)?,
            destination: Ax25Address::new("APRS", 0)?,
            digipeaters: vec![RouteEntry::new("WIDE1", 1)?, RouteEntry::new("WIDE2", 1)?],
            command_or_response: Some(CommandResponse::Command),
            control: 0x03,
            protocol: 0xF0,
            info: b"!4903.50N/07201.75W-Test 73".to_vec(),
        };

        let bytes = build_ax25(&original);
        let parsed = parse_ax25(&bytes)?;

        assert_eq!(parsed, original);
        Ok(())
    }

    #[test]
    fn parse_ax25_too_short() {
        let r = parse_ax25(&[0; 10]);
        assert!(
            matches!(r, Err(Ax25Error::PacketTooShort)),
            "expected PacketTooShort, got {r:?}",
        );
    }

    #[test]
    fn parse_ax25_rejects_more_than_8_digipeaters() {
        let mut frame = Vec::new();
        for &ch in b"APRS  " {
            frame.push(ch << 1);
        }
        frame.push(0x60);
        for &ch in b"N0CALL" {
            frame.push(ch << 1);
        }
        frame.push(0x60);
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
        assert_eq!(parse_ax25(&frame), Err(Ax25Error::TooManyDigipeaters));
    }

    #[test]
    fn ax25_fcs_known_value() {
        assert_eq!(ax25_fcs(b"123456789"), 0x906E);
    }

    #[test]
    fn ax25_fcs_empty_matches_init_xor() {
        assert_eq!(ax25_fcs(&[]), 0x0000);
    }

    #[test]
    fn ax25_packet_typed_accessors() -> TestResult {
        let packet = Ax25Packet {
            source: Ax25Address::new("N0CALL", 7)?,
            destination: Ax25Address::new("APRS", 0)?,
            digipeaters: vec![],
            command_or_response: Some(CommandResponse::Command),
            control: 0x03,
            protocol: 0xF0,
            info: b"!test".to_vec(),
        };
        assert!(packet.is_ui());
        assert_eq!(packet.pid(), Ax25Pid::NoLayer3);
        Ok(())
    }

    #[test]
    fn ax25_command_roundtrip_preserves_classification() -> TestResult {
        // Regression for Bug 1: command frames must survive build → parse.
        let original = Ax25Packet {
            source: Ax25Address::new("N0CALL", 7)?,
            destination: Ax25Address::new("APRS", 0)?,
            digipeaters: vec![],
            command_or_response: Some(CommandResponse::Command),
            control: 0x03,
            protocol: 0xF0,
            info: b"!".to_vec(),
        };
        let bytes = build_ax25(&original);
        let parsed = parse_ax25(&bytes)?;
        assert_eq!(parsed.command_or_response, Some(CommandResponse::Command));
        Ok(())
    }

    #[test]
    fn ax25_response_roundtrip_preserves_classification() -> TestResult {
        let original = Ax25Packet {
            source: Ax25Address::new("N0CALL", 7)?,
            destination: Ax25Address::new("APRS", 0)?,
            digipeaters: vec![],
            command_or_response: Some(CommandResponse::Response),
            control: 0x03,
            protocol: 0xF0,
            info: b"!".to_vec(),
        };
        let bytes = build_ax25(&original);
        let parsed = parse_ax25(&bytes)?;
        assert_eq!(parsed.command_or_response, Some(CommandResponse::Response));
        Ok(())
    }

    #[test]
    fn ax25_legacy_roundtrips_as_none() -> TestResult {
        let original = Ax25Packet {
            source: Ax25Address::new("N0CALL", 7)?,
            destination: Ax25Address::new("APRS", 0)?,
            digipeaters: vec![],
            command_or_response: None,
            control: 0x03,
            protocol: 0xF0,
            info: b"!".to_vec(),
        };
        let bytes = build_ax25(&original);
        let parsed = parse_ax25(&bytes)?;
        assert_eq!(parsed.command_or_response, None);
        Ok(())
    }

    #[test]
    fn ax25_repeated_digi_roundtrip_preserves_h_bit() -> TestResult {
        let mut digi = RouteEntry::new("WIDE1", 1)?;
        digi.has_repeated = true;
        let original = Ax25Packet {
            source: Ax25Address::new("N0CALL", 7)?,
            destination: Ax25Address::new("APRS", 0)?,
            digipeaters: vec![digi],
            command_or_response: Some(CommandResponse::Command),
            control: 0x03,
            protocol: 0xF0,
            info: b"!".to_vec(),
        };
        let bytes = build_ax25(&original);
        let parsed = parse_ax25(&bytes)?;
        assert_eq!(parsed.digipeaters.len(), 1);
        let d0 = parsed.digipeaters.first().ok_or("missing digi 0")?;
        assert!(d0.has_repeated);
        Ok(())
    }

    #[test]
    fn decode_address_rejects_non_ascii() {
        // 0x24 >> 1 = 0x12 is a control char, not alphanumeric.
        let bytes = [
            b'N' << 1,
            b'0' << 1,
            0x24,
            b'A' << 1,
            b'L' << 1,
            b'L' << 1,
            0x60,
        ];
        let err = decode_address(bytes);
        assert!(
            matches!(err, Err(Ax25Error::InvalidCallsignByte(0x12))),
            "expected InvalidCallsignByte(0x12), got {err:?}",
        );
    }

    #[test]
    fn parse_ax25_rejects_embedded_space_in_callsign() {
        // Regression for Bug 3.
        let mut frame = Vec::new();
        for &c in b"A B   " {
            frame.push(c << 1);
        }
        frame.push(0x60);
        for &c in b"N0CALL" {
            frame.push(c << 1);
        }
        frame.push(0x60 | (7 << 1) | 0x01);
        frame.push(0x03);
        frame.push(0xF0);
        frame.push(b'!');

        let r = parse_ax25(&frame);
        assert!(
            matches!(r, Err(Ax25Error::InvalidCallsignByte(_))),
            "embedded space should be rejected, got {r:?}",
        );
    }

    // ---- Display ----

    #[test]
    fn endpoint_address_display_omits_asterisk() -> TestResult {
        // Endpoints have no H-bit semantic — never render `*`.
        let addr = Ax25Address::new("APRS", 0)?;
        assert_eq!(alloc::format!("{addr}"), "APRS");
        let addr = Ax25Address::new("N0CALL", 7)?;
        assert_eq!(alloc::format!("{addr}"), "N0CALL-7");
        Ok(())
    }

    #[test]
    fn route_entry_display_appends_asterisk_when_repeated() -> TestResult {
        let entry = RouteEntry::new("WIDE1", 1)?;
        assert_eq!(alloc::format!("{entry}"), "WIDE1-1");
        let used = entry.marked_used();
        assert_eq!(alloc::format!("{used}"), "WIDE1-1*");
        Ok(())
    }

    // ---- Proptest ----

    fn arb_callsign() -> impl Strategy<Value = Callsign> {
        "[A-Z0-9]{1,6}".prop_filter_map("Callsign::new", |s| Callsign::new(&s).ok())
    }

    fn arb_ssid() -> impl Strategy<Value = Ssid> {
        (0u8..=15).prop_filter_map("Ssid::new", |n| Ssid::new(n).ok())
    }

    fn arb_address() -> impl Strategy<Value = Ax25Address> {
        (arb_callsign(), arb_ssid()).prop_map(|(c, s)| Ax25Address::from_parts(c, s))
    }

    fn arb_route_entry() -> impl Strategy<Value = RouteEntry> {
        (arb_address(), any::<bool>()).prop_map(|(address, has_repeated)| RouteEntry {
            address,
            has_repeated,
        })
    }

    fn arb_command_response() -> impl Strategy<Value = Option<CommandResponse>> {
        proptest::option::of(prop_oneof![
            Just(CommandResponse::Command),
            Just(CommandResponse::Response),
        ])
    }

    fn arb_packet() -> impl Strategy<Value = Ax25Packet> {
        (
            arb_address(),
            arb_address(),
            proptest::collection::vec(arb_route_entry(), 0..=MAX_DIGIPEATERS),
            arb_command_response(),
            any::<u8>(),
            any::<u8>(),
            proptest::collection::vec(any::<u8>(), 0..=256),
        )
            .prop_map(|(s, d, digis, cr, ctl, pid, info)| Ax25Packet {
                source: s,
                destination: d,
                digipeaters: digis,
                command_or_response: cr,
                control: ctl,
                protocol: pid,
                info,
            })
    }

    proptest! {
        /// Any well-formed Ax25Packet roundtrips through build → parse.
        /// Would have caught Bug 1 (C-bit silently dropped on encode).
        #[test]
        fn ax25_packet_roundtrips(p in arb_packet()) {
            let bytes = build_ax25(&p);
            let parsed = parse_ax25(&bytes).map_err(to_test_err)?;
            prop_assert_eq!(parsed, p);
        }

        /// Parser never panics on arbitrary input. Would have caught
        /// Bug 3 (embedded-space stripping) via structural divergence.
        #[test]
        fn parse_ax25_never_panics(data in proptest::collection::vec(any::<u8>(), 0..=512)) {
            drop(parse_ax25(&data));
        }
    }
}
