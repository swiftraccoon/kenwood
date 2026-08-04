//! AX.25 frame encode/decode.
//!
//! This module hosts the [`Ax25Packet`] frame type, the byte-level
//! [`parse_ax25`]/[`build_ax25`] codec, and the FCS helper [`ax25_fcs`].
//! All APIs are `no_std`-compatible.

use alloc::string::String;
use alloc::vec::Vec;

use crate::address::{Ax25Address, Callsign, RouteEntry, Ssid};
use crate::control::{Ax25Control, CommandResponse, UnnumberedKind};
use crate::error::Ax25Error;
use crate::path::{DigipeaterPath, MAX_DIGIPEATERS};
use crate::pid::Ax25Pid;

/// Compute the AX.25 Frame Check Sequence (CRC-16-CCITT, polynomial
/// `0x1021`, initial value `0xFFFF`, reflected, `xorout = 0xFFFF`) over
/// a byte slice.
///
/// KISS frames do not carry the FCS (the TNC computes and strips it),
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
    pub digipeaters: DigipeaterPath,
    /// AX.25 v2.2 Command/Response classification:
    /// - `(dest_c=1, src_c=0)` → `Command` (APRS convention)
    /// - `(dest_c=0, src_c=1)` → `Response`
    /// - both equal → the corresponding lossless pre-v2.0 variant
    pub command_or_response: CommandResponse,
    control: Ax25Control,
    protocol: Option<Ax25Pid>,
    information: Vec<u8>,
}

impl Ax25Packet {
    /// Construct a packet from typed AX.25 fields.
    ///
    /// This is the general constructor for modulo-8 I, S, and U frames. It
    /// rejects structural combinations that AX.25 cannot put on the wire:
    /// only I and UI frames carry a PID; S frames never carry information;
    /// and only UI, FRMR, XID, and TEST U frames carry information.
    /// Unknown U-frame modifiers preserve trailing bytes losslessly because
    /// a future assignment may define an information field.
    ///
    /// # Errors
    ///
    /// Returns [`Ax25Error::MissingProtocolIdentifier`],
    /// [`Ax25Error::UnexpectedProtocolIdentifier`], or
    /// [`Ax25Error::UnexpectedInformationField`] when the fields do not
    /// match the control-field type.
    pub fn try_new(
        source: Ax25Address,
        destination: Ax25Address,
        digipeaters: DigipeaterPath,
        command_or_response: CommandResponse,
        control: Ax25Control,
        protocol: Option<Ax25Pid>,
        information: Vec<u8>,
    ) -> Result<Self, Ax25Error> {
        validate_frame_fields(control, protocol, &information)?;
        Ok(Self {
            source,
            destination,
            digipeaters,
            command_or_response,
            control,
            protocol,
            information,
        })
    }

    /// Construct an Unnumbered Information frame.
    ///
    /// UI is the connectionless frame used by APRS. Unlike the general
    /// constructor, this common valid shape is infallible.
    #[must_use]
    pub const fn unnumbered_information(
        source: Ax25Address,
        destination: Ax25Address,
        digipeaters: DigipeaterPath,
        command_or_response: CommandResponse,
        poll_final: bool,
        protocol: Ax25Pid,
        information: Vec<u8>,
    ) -> Self {
        Self {
            source,
            destination,
            digipeaters,
            command_or_response,
            control: Ax25Control::Unnumbered {
                kind: UnnumberedKind::UnnumberedInformation,
                pf: poll_final,
            },
            protocol: Some(protocol),
            information,
        }
    }

    /// Return the typed modulo-8 control field.
    #[must_use]
    pub const fn control(&self) -> Ax25Control {
        self.control
    }

    /// Return the encoded control byte.
    #[must_use]
    pub const fn control_byte(&self) -> u8 {
        self.control.as_byte()
    }

    /// Return the protocol identifier carried by an I or UI frame.
    ///
    /// Supervisory and non-UI unnumbered frames have no PID field.
    #[must_use]
    pub const fn protocol_identifier(&self) -> Option<Ax25Pid> {
        self.protocol
    }

    /// Return the information bytes.
    ///
    /// Frames without an information field return an empty slice. Use
    /// [`Self::has_information_field`] when the distinction between an absent
    /// field and a present zero-length field matters.
    #[must_use]
    pub fn information(&self) -> &[u8] {
        &self.information
    }

    /// Return mutable information bytes when this frame type permits them.
    pub fn information_mut(&mut self) -> Option<&mut Vec<u8>> {
        self.has_information_field()
            .then_some(&mut self.information)
    }

    /// Return whether this frame type has an information field.
    #[must_use]
    pub const fn has_information_field(&self) -> bool {
        control_allows_information(self.control)
    }

    /// `true` if this is a UI frame (APRS standard).
    #[must_use]
    pub const fn is_ui(&self) -> bool {
        self.control.is_ui()
    }
}

const fn control_requires_protocol(control: Ax25Control) -> bool {
    matches!(
        control,
        Ax25Control::Information { .. }
            | Ax25Control::Unnumbered {
                kind: UnnumberedKind::UnnumberedInformation,
                ..
            }
    )
}

const fn control_allows_information(control: Ax25Control) -> bool {
    matches!(
        control,
        Ax25Control::Information { .. }
            | Ax25Control::Unnumbered {
                kind: UnnumberedKind::UnnumberedInformation
                    | UnnumberedKind::FrameReject
                    | UnnumberedKind::ExchangeIdentification
                    | UnnumberedKind::Test
                    | UnnumberedKind::Other(_),
                ..
            }
    )
}

const fn validate_frame_fields(
    control: Ax25Control,
    protocol: Option<Ax25Pid>,
    information: &[u8],
) -> Result<(), Ax25Error> {
    let control_byte = control.as_byte();
    match (control_requires_protocol(control), protocol) {
        (true, None) => {
            return Err(Ax25Error::MissingProtocolIdentifier {
                control: control_byte,
            });
        }
        (false, Some(_)) => {
            return Err(Ax25Error::UnexpectedProtocolIdentifier {
                control: control_byte,
            });
        }
        _ => {}
    }
    if !control_allows_information(control) && !information.is_empty() {
        return Err(Ax25Error::UnexpectedInformationField {
            control: control_byte,
            length: information.len(),
        });
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Internal decode / encode
// ---------------------------------------------------------------------------

/// Decode a single AX.25 address slot from a 7-byte slice. Returns the
/// validated [`Ax25Address`] plus the raw wire bit 7 (interpretation
/// depends on slot; caller decides).
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
            // Non-space byte after padding starts: malformed per §3.12.2.
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
/// to wire bit 7 (interpretation depends on slot; caller decides).
/// `is_last` sets the address-extension bit on the final address.
fn encode_address(addr: &Ax25Address, bit7: bool, is_last: bool) -> [u8; 7] {
    let mut bytes = [0x40u8; 7]; // space << 1 = 0x40, right-pads to 6 chars
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
/// Handles modulo-8 I, S, and U frames. A PID byte is consumed only for I
/// and UI frames. Information bytes are accepted only for I, UI, FRMR, XID,
/// TEST, and unknown U-frame modifier patterns.
///
/// # Errors
///
/// Returns [`Ax25Error`] if the packet structure is invalid.
pub fn parse_ax25(data: &[u8]) -> Result<Ax25Packet, Ax25Error> {
    // Minimum: dest(7) + src(7) + control(1) = 15. I and UI frames are
    // checked for their required PID after the control field is classified.
    if data.len() < 15 {
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
        (true, false) => CommandResponse::Command,
        (false, true) => CommandResponse::Response,
        (false, false) => CommandResponse::LegacyBothClear,
        (true, true) => CommandResponse::LegacyBothSet,
    };

    // Find end of address field via the address-extension bit on the
    // last byte of each 7-byte address.
    let mut addr_end = 14;
    let mut digipeaters = DigipeaterPath::empty();

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
            digipeaters.try_push(RouteEntry {
                address,
                has_repeated,
            })?;
            addr_end += 7;
            if is_last {
                break;
            }
        }
    }

    let control =
        Ax25Control::from_byte(*data.get(addr_end).ok_or(Ax25Error::MissingControlFields)?);
    let (protocol, information_start) = if control_requires_protocol(control) {
        let first_pid_octet =
            *data
                .get(addr_end + 1)
                .ok_or(Ax25Error::MissingProtocolIdentifier {
                    control: control.as_byte(),
                })?;
        let protocol = if first_pid_octet == 0xFF {
            let extension = *data
                .get(addr_end + 2)
                .ok_or(Ax25Error::MissingProtocolIdentifierExtension)?;
            Ax25Pid::escaped(extension)
        } else {
            Ax25Pid::from_byte(first_pid_octet)?
        };
        let information_start = addr_end + 1 + protocol.wire_len();
        (Some(protocol), information_start)
    } else {
        (None, addr_end + 1)
    };
    let information = data.get(information_start..).unwrap_or(&[]).to_vec();

    Ax25Packet::try_new(
        source,
        destination,
        digipeaters,
        command_or_response,
        control,
        protocol,
        information,
    )
}

/// Build an AX.25 frame from an [`Ax25Packet`].
///
/// Returns the raw bytes suitable for encapsulation in a KISS data frame.
///
#[must_use]
pub fn build_ax25(packet: &Ax25Packet) -> Vec<u8> {
    let (dest_c, src_c) = match packet.command_or_response {
        CommandResponse::Command => (true, false),
        CommandResponse::Response => (false, true),
        CommandResponse::LegacyBothClear => (false, false),
        CommandResponse::LegacyBothSet => (true, true),
    };
    let no_digis = packet.digipeaters.is_empty();
    let total_len = 14
        + packet.digipeaters.len() * 7
        + 1
        + packet.protocol.map_or(0, Ax25Pid::wire_len)
        + packet.information.len();
    let mut out = Vec::with_capacity(total_len);

    out.extend_from_slice(&encode_address(&packet.destination, dest_c, false));
    out.extend_from_slice(&encode_address(&packet.source, src_c, no_digis));

    let digi_count = packet.digipeaters.len();
    for (i, entry) in packet.digipeaters.iter().enumerate() {
        let is_last = i + 1 == digi_count;
        out.extend_from_slice(&encode_address(&entry.address, entry.has_repeated, is_last));
    }

    out.push(packet.control.as_byte());
    if let Some(protocol) = packet.protocol {
        out.push(protocol.as_byte());
        if let Some(extension) = protocol.extension_byte() {
            out.push(extension);
        }
    }
    out.extend_from_slice(&packet.information);

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

    fn make_ui_packet(
        source: Ax25Address,
        destination: Ax25Address,
        digipeaters: DigipeaterPath,
        command_or_response: CommandResponse,
        poll_final: bool,
        information: Vec<u8>,
    ) -> Ax25Packet {
        Ax25Packet::unnumbered_information(
            source,
            destination,
            digipeaters,
            command_or_response,
            poll_final,
            Ax25Pid::NoLayer3,
            information,
        )
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

    fn make_address_field_bytes() -> Vec<u8> {
        let mut frame = make_test_ax25_bytes();
        frame.truncate(14);
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
        assert_eq!(packet.control_byte(), 0x03);
        assert_eq!(packet.protocol_identifier(), Some(Ax25Pid::NoLayer3));
        assert_eq!(packet.command_or_response, CommandResponse::LegacyBothClear);
        assert_eq!(packet.information(), b"!4903.50N/07201.75W-Test");
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
    fn supervisory_frame_has_neither_pid_nor_information() -> TestResult {
        let mut wire = make_address_field_bytes();
        wire.push(0x01); // RR, N(R)=0, P/F clear.

        let packet = parse_ax25(&wire)?;
        assert_eq!(packet.control_byte(), 0x01);
        assert_eq!(packet.protocol_identifier(), None);
        assert!(!packet.has_information_field());
        assert_eq!(packet.information(), b"");
        assert_eq!(build_ax25(&packet), wire);
        Ok(())
    }

    #[test]
    fn information_frame_consumes_pid_before_information() -> TestResult {
        let mut wire = make_address_field_bytes();
        wire.extend_from_slice(&[0x00, 0xCC, 0x45, 0x00]);

        let packet = parse_ax25(&wire)?;
        assert!(matches!(packet.control(), Ax25Control::Information { .. }));
        assert_eq!(packet.protocol_identifier(), Some(Ax25Pid::Ip));
        assert_eq!(packet.information(), [0x45, 0x00]);
        assert_eq!(build_ax25(&packet), wire);
        Ok(())
    }

    #[test]
    fn escaped_pid_consumes_its_extension_before_information() -> TestResult {
        for control_byte in [0x00, 0x03] {
            let mut wire = make_address_field_bytes();
            wire.extend_from_slice(&[control_byte, 0xFF, 0xCC, 0x45, 0x00]);

            let packet = parse_ax25(&wire)?;
            assert_eq!(
                packet.protocol_identifier(),
                Some(Ax25Pid::Escape { extension: 0xCC }),
                "control {control_byte:#04x}",
            );
            assert_eq!(
                packet.information(),
                [0x45, 0x00],
                "escaped PID extension must not leak into information",
            );
            assert_eq!(build_ax25(&packet), wire);
        }
        Ok(())
    }

    #[test]
    fn escaped_pid_without_extension_is_rejected() {
        for control_byte in [0x00, 0x03] {
            let mut wire = make_address_field_bytes();
            wire.extend_from_slice(&[control_byte, 0xFF]);

            assert_eq!(
                parse_ax25(&wire),
                Err(Ax25Error::MissingProtocolIdentifierExtension),
                "control {control_byte:#04x}",
            );
        }
    }

    #[test]
    fn xid_information_starts_immediately_after_control() -> TestResult {
        let mut wire = make_address_field_bytes();
        wire.extend_from_slice(&[0xAF, 0x82, 0x80, 0x00, 0x00]);

        let packet = parse_ax25(&wire)?;
        assert_eq!(
            packet.control(),
            Ax25Control::Unnumbered {
                kind: UnnumberedKind::ExchangeIdentification,
                pf: false,
            }
        );
        assert_eq!(packet.protocol_identifier(), None);
        assert_eq!(packet.information(), [0x82, 0x80, 0x00, 0x00]);
        assert_eq!(build_ax25(&packet), wire);
        Ok(())
    }

    #[test]
    fn known_control_frame_rejects_forbidden_information() {
        let mut wire = make_address_field_bytes();
        wire.extend_from_slice(&[0x6F, 0xAA]); // SABME cannot carry information.

        assert_eq!(
            parse_ax25(&wire),
            Err(Ax25Error::UnexpectedInformationField {
                control: 0x6F,
                length: 1,
            })
        );
    }

    #[test]
    fn information_frames_without_required_pid_report_the_specific_error() {
        for control_byte in [0x00, 0x03] {
            let mut wire = make_address_field_bytes();
            wire.push(control_byte);
            assert_eq!(
                parse_ax25(&wire),
                Err(Ax25Error::MissingProtocolIdentifier {
                    control: control_byte,
                }),
                "control {control_byte:#04x}",
            );
        }
    }

    #[test]
    fn constructor_rejects_pid_and_information_mismatches() -> TestResult {
        let source = Ax25Address::new("N0CALL", 7)?;
        let destination = Ax25Address::new("APRS", 0)?;
        let rr = Ax25Control::from_byte(0x01);

        assert_eq!(
            Ax25Packet::try_new(
                source.clone(),
                destination.clone(),
                DigipeaterPath::empty(),
                CommandResponse::Response,
                rr,
                Some(Ax25Pid::NoLayer3),
                Vec::new(),
            ),
            Err(Ax25Error::UnexpectedProtocolIdentifier { control: 0x01 })
        );
        assert_eq!(
            Ax25Packet::try_new(
                source,
                destination,
                DigipeaterPath::empty(),
                CommandResponse::Response,
                rr,
                None,
                vec![0xAA],
            ),
            Err(Ax25Error::UnexpectedInformationField {
                control: 0x01,
                length: 1,
            })
        );
        Ok(())
    }

    #[test]
    fn constructor_enforces_the_complete_known_frame_field_matrix() -> TestResult {
        let source = Ax25Address::new("N0CALL", 7)?;
        let destination = Ax25Address::new("APRS", 0)?;

        // AX.25 v2.2 §3.5: only I, UI, FRMR, XID, and TEST may carry
        // information. Of those, only I and UI also require a PID (§3.4).
        for control_byte in [0x00, 0x03, 0x87, 0xAF, 0xE3] {
            let control = Ax25Control::from_byte(control_byte);
            let protocol = control_requires_protocol(control).then_some(Ax25Pid::NoLayer3);
            let mut packet = Ax25Packet::try_new(
                source.clone(),
                destination.clone(),
                DigipeaterPath::empty(),
                CommandResponse::LegacyBothClear,
                control,
                protocol,
                vec![0xAA],
            )?;
            assert!(
                packet.has_information_field(),
                "control {control_byte:#04x} must expose its information field",
            );
            assert!(
                packet.information_mut().is_some(),
                "control {control_byte:#04x} must permit information mutation",
            );
        }

        // All S frames and every other known U frame must end immediately
        // after the control byte.
        for control_byte in [
            0x01, 0x05, 0x09, 0x0D, // RR, RNR, REJ, SREJ
            0x2F, 0x6F, 0x43, 0x0F, 0x63, // SABM, SABME, DISC, DM, UA
        ] {
            let control = Ax25Control::from_byte(control_byte);
            let packet = Ax25Packet::try_new(
                source.clone(),
                destination.clone(),
                DigipeaterPath::empty(),
                CommandResponse::LegacyBothClear,
                control,
                None,
                Vec::new(),
            )?;
            assert!(
                !packet.has_information_field(),
                "control {control_byte:#04x} must not expose an information field",
            );
            assert_eq!(
                Ax25Packet::try_new(
                    source.clone(),
                    destination.clone(),
                    DigipeaterPath::empty(),
                    CommandResponse::LegacyBothClear,
                    control,
                    None,
                    vec![0xAA],
                ),
                Err(Ax25Error::UnexpectedInformationField {
                    control: control_byte,
                    length: 1,
                }),
                "control {control_byte:#04x} accepted forbidden information",
            );
            assert_eq!(
                Ax25Packet::try_new(
                    source.clone(),
                    destination.clone(),
                    DigipeaterPath::empty(),
                    CommandResponse::LegacyBothClear,
                    control,
                    Some(Ax25Pid::NoLayer3),
                    Vec::new(),
                ),
                Err(Ax25Error::UnexpectedProtocolIdentifier {
                    control: control_byte,
                }),
                "control {control_byte:#04x} accepted a forbidden PID",
            );
        }

        for control_byte in [0x00, 0x03] {
            assert_eq!(
                Ax25Packet::try_new(
                    source.clone(),
                    destination.clone(),
                    DigipeaterPath::empty(),
                    CommandResponse::LegacyBothClear,
                    Ax25Control::from_byte(control_byte),
                    None,
                    Vec::new(),
                ),
                Err(Ax25Error::MissingProtocolIdentifier {
                    control: control_byte,
                }),
                "control {control_byte:#04x} accepted a missing PID",
            );
        }
        Ok(())
    }

    #[test]
    fn unknown_unnumbered_trailing_bytes_roundtrip_without_pid_guessing() -> TestResult {
        for control_byte in [0x07, 0x17] {
            let mut wire = make_address_field_bytes();
            wire.extend_from_slice(&[control_byte, 0xF0, 0xAA]);

            let packet = parse_ax25(&wire)?;
            let Ax25Control::Unnumbered {
                kind: UnnumberedKind::Other(kind),
                pf,
            } = packet.control()
            else {
                return Err("fixture must classify as an unknown U frame".into());
            };
            assert_eq!(kind.as_byte(), 0x07);
            assert_eq!(pf, control_byte & 0x10 != 0);
            assert_eq!(packet.protocol_identifier(), None);
            assert_eq!(packet.information(), [0xF0, 0xAA]);
            assert_eq!(build_ax25(&packet), wire);
        }
        Ok(())
    }

    #[test]
    fn ax25_roundtrip() -> TestResult {
        let original = make_ui_packet(
            Ax25Address::new("N0CALL", 7)?,
            Ax25Address::new("APRS", 0)?,
            DigipeaterPath::new(vec![
                RouteEntry::new("WIDE1", 1)?,
                RouteEntry::new("WIDE2", 1)?,
            ])?,
            CommandResponse::Command,
            false,
            b"!4903.50N/07201.75W-Test 73".to_vec(),
        );

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

    /// Absolute wire pin for the encoder, hand-derived from AX.25
    /// v2.2 §3.12. Every other encode check in this crate round-trips
    /// through our own parser, which MASKS the reserved SSID bits the
    /// encoder writes (`(ssid_byte >> 1) & 0x0F`), so an encoder
    /// emitting `0x00`-base SSID bytes would pass the entire suite
    /// while producing frames other TNCs may reject. Layout per SSID
    /// byte: C/H in bit 7, reserved `0b11` in bits 6-5, SSID in bits
    /// 4-1, address-extension in bit 0 (last address only).
    #[test]
    fn build_matches_hand_derived_wire_bytes() -> TestResult {
        let destination = Ax25Address::from_parts(
            Callsign::new("APRS").map_err(|_| "dest callsign")?,
            Ssid::new(0).map_err(|_| "dest ssid")?,
        );
        let source = Ax25Address::from_parts(
            Callsign::new("N0CALL").map_err(|_| "src callsign")?,
            Ssid::new(7).map_err(|_| "src ssid")?,
        );
        let digi = RouteEntry {
            address: Ax25Address::from_parts(
                Callsign::new("WIDE1").map_err(|_| "digi callsign")?,
                Ssid::new(1).map_err(|_| "digi ssid")?,
            ),
            has_repeated: true,
        };
        let packet = make_ui_packet(
            source,
            destination,
            DigipeaterPath::new(vec![digi])?,
            CommandResponse::Command,
            false,
            b"hello".to_vec(),
        );
        let wire = build_ax25(&packet);
        let expected: [u8; 28] = [
            // "APRS  " shifted << 1; SSID byte 0xE0 = C-bit (command)
            // | 0x60 reserved | ssid 0 | ext 0.
            0x82, 0xA0, 0xA4, 0xA6, 0x40, 0x40, 0xE0,
            // "N0CALL" shifted; SSID byte 0x6E = C-bit clear | 0x60
            // | ssid 7 << 1 | ext 0.
            0x9C, 0x60, 0x86, 0x82, 0x98, 0x98, 0x6E,
            // "WIDE1 " shifted; SSID byte 0xE3 = H-bit (repeated)
            // | 0x60 | ssid 1 << 1 | ext 1 (last address).
            0xAE, 0x92, 0x88, 0x8A, 0x62, 0x40, 0xE3,
            // UI control, no-layer-3 PID, payload verbatim.
            0x03, 0xF0, b'h', b'e', b'l', b'l', b'o',
        ];
        assert_eq!(
            wire.as_slice(),
            expected.as_slice(),
            "encoder wire bytes must match the §3.12 hand derivation"
        );
        Ok(())
    }

    /// Wire-level idempotence for canonical inputs:
    /// `build(parse(bytes)) == bytes`. Decode tolerates lowercase
    /// wire callsigns as NORMALIZATION (uppercasing on the way in),
    /// so identity holds only for canonical frames, which is exactly
    /// what this pins: the parser/encoder pair must not silently
    /// rewrite any bit of an already-canonical frame.
    #[test]
    fn canonical_wire_bytes_survive_parse_then_build() -> TestResult {
        let canonical: [u8; 28] = [
            0x82, 0xA0, 0xA4, 0xA6, 0x40, 0x40, 0xE0, 0x9C, 0x60, 0x86, 0x82, 0x98, 0x98, 0x6E,
            0xAE, 0x92, 0x88, 0x8A, 0x62, 0x40, 0xE3, 0x03, 0xF0, b'h', b'e', b'l', b'l', b'o',
        ];
        let packet = parse_ax25(&canonical).map_err(|_| "canonical frame must parse")?;
        let rebuilt = build_ax25(&packet);
        assert_eq!(
            rebuilt.as_slice(),
            canonical.as_slice(),
            "parse→build must be the identity on canonical wire bytes"
        );
        Ok(())
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
        let packet = make_ui_packet(
            Ax25Address::new("N0CALL", 7)?,
            Ax25Address::new("APRS", 0)?,
            DigipeaterPath::empty(),
            CommandResponse::Command,
            false,
            b"!test".to_vec(),
        );
        assert!(packet.is_ui());
        assert_eq!(packet.protocol_identifier(), Some(Ax25Pid::NoLayer3));
        Ok(())
    }

    #[test]
    fn ax25_command_roundtrip_preserves_classification() -> TestResult {
        // Regression for Bug 1: command frames must survive build → parse.
        let original = make_ui_packet(
            Ax25Address::new("N0CALL", 7)?,
            Ax25Address::new("APRS", 0)?,
            DigipeaterPath::empty(),
            CommandResponse::Command,
            false,
            b"!".to_vec(),
        );
        let bytes = build_ax25(&original);
        let parsed = parse_ax25(&bytes)?;
        assert_eq!(parsed.command_or_response, CommandResponse::Command);
        Ok(())
    }

    #[test]
    fn ax25_response_roundtrip_preserves_classification() -> TestResult {
        let original = make_ui_packet(
            Ax25Address::new("N0CALL", 7)?,
            Ax25Address::new("APRS", 0)?,
            DigipeaterPath::empty(),
            CommandResponse::Response,
            false,
            b"!".to_vec(),
        );
        let bytes = build_ax25(&original);
        let parsed = parse_ax25(&bytes)?;
        assert_eq!(parsed.command_or_response, CommandResponse::Response);
        Ok(())
    }

    #[test]
    fn ax25_legacy_c_bit_forms_roundtrip_losslessly() -> TestResult {
        for legacy in [
            CommandResponse::LegacyBothClear,
            CommandResponse::LegacyBothSet,
        ] {
            let original = make_ui_packet(
                Ax25Address::new("N0CALL", 7)?,
                Ax25Address::new("APRS", 0)?,
                DigipeaterPath::empty(),
                legacy,
                false,
                b"!".to_vec(),
            );
            let bytes = build_ax25(&original);
            let parsed = parse_ax25(&bytes)?;
            assert_eq!(parsed.command_or_response, legacy);
            assert_eq!(build_ax25(&parsed), bytes);
        }
        Ok(())
    }

    #[test]
    fn ax25_repeated_digi_roundtrip_preserves_h_bit() -> TestResult {
        let mut digi = RouteEntry::new("WIDE1", 1)?;
        digi.has_repeated = true;
        let original = make_ui_packet(
            Ax25Address::new("N0CALL", 7)?,
            Ax25Address::new("APRS", 0)?,
            DigipeaterPath::new(vec![digi])?,
            CommandResponse::Command,
            false,
            b"!".to_vec(),
        );
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
        // Endpoints have no H-bit semantic, so never render `*`.
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

    fn arb_command_response() -> impl Strategy<Value = CommandResponse> {
        prop_oneof![
            Just(CommandResponse::Command),
            Just(CommandResponse::Response),
            Just(CommandResponse::LegacyBothClear),
            Just(CommandResponse::LegacyBothSet),
        ]
    }

    fn arb_pid() -> impl Strategy<Value = Ax25Pid> {
        prop_oneof![
            any::<u8>().prop_filter_map("complete one-octet PID", |byte| {
                Ax25Pid::from_byte(byte).ok()
            }),
            any::<u8>().prop_map(Ax25Pid::escaped),
        ]
    }

    fn arb_packet() -> impl Strategy<Value = Ax25Packet> {
        (
            arb_address(),
            arb_address(),
            proptest::collection::vec(arb_route_entry(), 0..=MAX_DIGIPEATERS)
                .prop_filter_map("valid digipeater path", |entries| {
                    DigipeaterPath::new(entries).ok()
                }),
            arb_command_response(),
            any::<u8>(),
            arb_pid(),
            proptest::collection::vec(any::<u8>(), 0..=256),
        )
            .prop_map(
                |(source, destination, digipeaters, command_or_response, byte, pid, info)| {
                    let control = Ax25Control::from_byte(byte);
                    let protocol = control_requires_protocol(control).then_some(pid);
                    let information = if control_allows_information(control) {
                        info
                    } else {
                        Vec::new()
                    };
                    Ax25Packet::try_new(
                        source,
                        destination,
                        digipeaters,
                        command_or_response,
                        control,
                        protocol,
                        information,
                    )
                    .unwrap_or_else(|_| unreachable!("strategy produces valid frame fields"))
                },
            )
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
