//! TH-D75-specific APRS integration.
//!
//! Generic packet-radio protocols live in their own workspace crates:
//! - [`kiss_tnc`]: KISS TNC wire framing.
//! - [`ax25_codec`]: AX.25 frame codec.
//! - [`aprs`]: APRS parser, digipeater, `SmartBeaconing`, messaging, station list.
//! - [`aprs_is`]: APRS-IS TCP client.
//!
//! This module contains only the D75-specific glue: [`client::AprsClient`]
//! owning a [`Radio`](crate::Radio) and [`KissSession`](crate::KissSession);
//! [`stored_settings_bridge`] for stored-radio ↔ runtime `SmartBeaconingConfig` conversion;
//! and D75-specific helpers like [`ax25_ui_frame`], [`ax25_to_kiss_wire`],
//! [`parse_digipeater_path`], and [`default_digipeater_path`].
//!
//! # TH-D75 KISS TNC specifications (per Operating Tips §2.7.2, User Manual Chapter 15)
//!
//! - TX buffer: 4 KB, RX buffer: 4 KB.
//! - Speeds: 1200 bps (AFSK) and 9600 bps (GMSK).
//! - The built-in TNC does NOT support Command mode or Converse mode;
//!   it enters KISS mode directly.
//! - The data band frequency defaults to Band A; changeable via Menu No. 506.
//! - USB or Bluetooth interface is selectable via Menu No. 983.
//! - To exit KISS mode: send KISS command `C0,FF,C0` (192,255,192).
//!   To re-enter KISS mode from PC: send CAT command `TN 2,0` (Band A)
//!   or `TN 2,1` (Band B).
//!
//! # References
//!
//! - KISS protocol: <http://www.ka9q.net/papers/kiss.html>
//! - AX.25 v2.2: <http://www.ax25.net/AX25.2.2-Jul%2098-2.pdf>
//! - APRS spec: <http://www.aprs.org/doc/APRS101.PDF>
//! - TH-D75 User Manual, Chapter 15: Built-In KISS TNC

pub mod client;
pub mod stored_settings_bridge;

use aprs::AprsError;
use ax25_codec::{
    Ax25Address, Ax25Packet, Ax25Pid, CommandResponse, DigipeaterPath, RouteEntry, build_ax25,
};
use kiss_tnc::{KissFrame, encode_kiss_frame};

// ---------------------------------------------------------------------------
// Construction helpers for the foreign `Ax25Packet` type
// ---------------------------------------------------------------------------

/// Build a minimal APRS UI frame with the given source, destination, path,
/// and info field. Control = 0x03, PID = 0xF0.
///
/// This is a free function because [`Ax25Packet`] belongs to the
/// [`ax25_codec`] crate and cannot receive an inherent implementation here.
#[must_use]
pub const fn ax25_ui_frame(
    source: Ax25Address,
    destination: Ax25Address,
    path: DigipeaterPath,
    info: Vec<u8>,
) -> Ax25Packet {
    Ax25Packet::unnumbered_information(
        source,
        destination,
        path,
        CommandResponse::Command,
        false,
        Ax25Pid::NoLayer3,
        info,
    )
}

/// Encode an [`Ax25Packet`] as a KISS-framed data frame ready for the
/// wire. Equivalent to wrapping [`build_ax25`] in [`encode_kiss_frame`]
/// with `port = 0` and `command = Data`.
///
/// This is a free function for the same ownership reason as
/// [`ax25_ui_frame`].
#[must_use]
pub fn ax25_to_kiss_wire(packet: &Ax25Packet) -> Vec<u8> {
    let ax25_bytes = build_ax25(packet);
    encode_kiss_frame(&KissFrame::data(ax25_bytes))
}

// ---------------------------------------------------------------------------
// APRS digipeater-path helpers
// ---------------------------------------------------------------------------

/// Default APRS digipeater path: WIDE1-1,WIDE2-1.
const DEFAULT_DIGIPEATER_PATH: &str = "WIDE1-1,WIDE2-1";

/// Parse a digipeater path string like `"WIDE1-1,WIDE2-2"` into addresses.
///
/// Accepts at most eight comma-separated entries. Whitespace around the path
/// and each entry is ignored. Each nonempty entry must use the canonical
/// display form accepted by [`Ax25Address::from_canonical_str`]:
///
/// - `CALLSIGN` is 1-6 uppercase ASCII letters or digits.
/// - SSID zero has no suffix.
/// - A nonzero SSID is written as `-1` through `-15`, without a sign or
///   leading zero.
///
/// Thus `WIDE1`, `WIDE1-1`, and `WIDE2-15` are accepted, while `wide1-1`,
/// `WIDE1-0`, `WIDE1-01`, and `WIDE1-+1` are rejected. An empty or
/// whitespace-only string returns an empty path (direct transmission with no
/// digipeating).
///
/// # Errors
///
/// Returns [`AprsError::InvalidPath`] containing the original input if an
/// entry is empty or noncanonical, or if the path contains more than eight
/// entries.
///
/// # Examples
///
/// ```
/// use kenwood_thd75::aprs::parse_digipeater_path;
/// let path = parse_digipeater_path("WIDE1-1,WIDE2-2").expect("static input is valid");
/// assert_eq!(path.len(), 2);
/// let first = path.first().expect("two-entry path has a first entry");
/// assert_eq!(first.address.callsign, "WIDE1");
/// assert_eq!(first.address.ssid, 1);
/// ```
pub fn parse_digipeater_path(s: &str) -> Result<DigipeaterPath, AprsError> {
    let trimmed = s.trim();
    if trimmed.is_empty() {
        return Ok(DigipeaterPath::empty());
    }
    let mut result = DigipeaterPath::empty();
    for entry in trimmed.split(',') {
        let entry = entry.trim();
        if entry.is_empty() {
            return Err(AprsError::InvalidPath(s.to_owned()));
        }
        let address = Ax25Address::from_canonical_str(entry)
            .map_err(|_| AprsError::InvalidPath(s.to_owned()))?;
        let route = RouteEntry::from_address(address);
        result
            .try_insert(result.len(), route)
            .map_err(|_| AprsError::InvalidPath(s.to_owned()))?;
    }
    Ok(result)
}

/// Build the default digipeater path as validated [`RouteEntry`] entries.
///
/// # Errors
///
/// Returns [`AprsError::InvalidPath`] if the library's static default ever
/// stops satisfying the same validation applied to caller-provided paths.
/// Keeping the check explicit prevents a malformed default from being
/// silently shortened.
pub fn default_digipeater_path() -> Result<DigipeaterPath, AprsError> {
    parse_digipeater_path(DEFAULT_DIGIPEATER_PATH)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use kiss_tnc::FEND;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn parse_digipeater_path_empty_is_ok() -> TestResult {
        assert_eq!(parse_digipeater_path("")?, DigipeaterPath::empty());
        assert_eq!(parse_digipeater_path("   ")?, DigipeaterPath::empty());
        Ok(())
    }

    #[test]
    fn parse_digipeater_path_single() -> TestResult {
        let path = parse_digipeater_path("WIDE1-1")?;
        assert_eq!(path.len(), 1);
        let first = path.first().ok_or("path[0] missing")?;
        assert_eq!(first.address.callsign, "WIDE1");
        assert_eq!(first.address.ssid, 1);
        Ok(())
    }

    #[test]
    fn parse_digipeater_path_multiple() -> TestResult {
        let path = parse_digipeater_path("WIDE1-1,WIDE2-2")?;
        assert_eq!(path.len(), 2);
        assert_eq!(
            path.first().ok_or("path[0] missing")?.address.callsign,
            "WIDE1",
        );
        let second = path.get(1).ok_or("path[1] missing")?;
        assert_eq!(second.address.callsign, "WIDE2");
        assert_eq!(second.address.ssid, 2);
        Ok(())
    }

    #[test]
    fn parse_digipeater_path_no_ssid() -> TestResult {
        let path = parse_digipeater_path("WIDE1")?;
        assert_eq!(path.len(), 1);
        assert_eq!(path.first().ok_or("path[0] missing")?.address.ssid, 0);
        Ok(())
    }

    #[test]
    fn parse_digipeater_path_accepts_canonical_boundaries_and_entry_whitespace() -> TestResult {
        let path = parse_digipeater_path("  A-1, ABC123-15, WIDE2  ")?;
        let addresses: Vec<_> = path.iter().map(|entry| entry.address.to_string()).collect();

        assert_eq!(addresses, ["A-1", "ABC123-15", "WIDE2"]);
        Ok(())
    }

    #[test]
    fn parse_digipeater_path_rejects_noncanonical_ssids() {
        for invalid in [
            "WIDE1-+1",
            "WIDE1-0",
            "WIDE1-00",
            "WIDE1-01",
            "WIDE1-001",
            "WIDE1-16",
            "WIDE1-99",
            "WIDE1-abc",
            "WIDE1-",
            "WIDE1--1",
            "WIDE1-1-2",
        ] {
            assert_eq!(
                parse_digipeater_path(invalid),
                Err(AprsError::InvalidPath(invalid.to_owned())),
                "accepted noncanonical path {invalid:?}",
            );
        }
    }

    #[test]
    fn parse_digipeater_path_rejects_noncanonical_callsigns() {
        for invalid in ["wide1-1", "WiDE1-1", "WIDE_1-1", "WIDE 1-1", "WIDE1-1*"] {
            assert_eq!(
                parse_digipeater_path(invalid),
                Err(AprsError::InvalidPath(invalid.to_owned())),
                "accepted noncanonical path {invalid:?}",
            );
        }
    }

    #[test]
    fn parse_digipeater_path_error_preserves_the_original_input() {
        let input = "  WIDE1-1, WIDE2-01  ";
        assert_eq!(
            parse_digipeater_path(input),
            Err(AprsError::InvalidPath(input.to_owned())),
        );
    }

    #[test]
    fn parse_digipeater_path_rejects_empty_entries() {
        for invalid in [",WIDE1-1", "WIDE1-1,", "WIDE1-1,,WIDE2-1"] {
            assert_eq!(
                parse_digipeater_path(invalid),
                Err(AprsError::InvalidPath(invalid.to_owned())),
            );
        }
    }

    #[test]
    fn parse_digipeater_path_rejects_long_callsign() {
        let input = "TOOLONG-1";
        assert_eq!(
            parse_digipeater_path(input),
            Err(AprsError::InvalidPath(input.to_owned())),
        );
    }

    #[test]
    fn parse_digipeater_path_accepts_eight_entries_and_rejects_the_ninth() -> TestResult {
        let eight = "WIDE1-1,WIDE1-2,WIDE1-3,WIDE1-4,WIDE1-5,WIDE1-6,WIDE1-7,WIDE1-8";
        let nine = "WIDE1-1,WIDE1-2,WIDE1-3,WIDE1-4,WIDE1-5,WIDE1-6,WIDE1-7,WIDE1-8,WIDE1-9";

        assert_eq!(parse_digipeater_path(eight)?.len(), 8);
        assert_eq!(
            parse_digipeater_path(nine),
            Err(AprsError::InvalidPath(nine.to_owned())),
        );
        Ok(())
    }

    #[test]
    fn default_path_is_wide1_wide2() -> TestResult {
        let path = default_digipeater_path()?;
        assert_eq!(path.len(), 2);
        let first = path.first().ok_or("path[0] missing")?;
        assert_eq!(first.address.callsign, "WIDE1");
        assert_eq!(first.address.ssid, 1);
        let second = path.get(1).ok_or("path[1] missing")?;
        assert_eq!(second.address.callsign, "WIDE2");
        assert_eq!(second.address.ssid, 1);
        Ok(())
    }

    #[test]
    fn ax25_ui_frame_sets_control_and_pid() -> TestResult {
        let packet = ax25_ui_frame(
            Ax25Address::new("N0CALL", 7)?,
            Ax25Address::new("APRS", 0)?,
            DigipeaterPath::empty(),
            b"!test".to_vec(),
        );
        assert_eq!(packet.control_byte(), 0x03);
        assert_eq!(packet.protocol_identifier(), Some(Ax25Pid::NoLayer3));
        assert_eq!(packet.source.callsign, "N0CALL");
        assert_eq!(packet.destination.callsign, "APRS");
        assert_eq!(packet.information(), b"!test");
        Ok(())
    }

    #[test]
    fn ax25_to_kiss_wire_produces_valid_kiss_frame() -> TestResult {
        let packet = ax25_ui_frame(
            Ax25Address::new("N0CALL", 7)?,
            Ax25Address::new("APRS", 0)?,
            DigipeaterPath::empty(),
            b"!test".to_vec(),
        );
        let wire = ax25_to_kiss_wire(&packet);
        // KISS frame starts and ends with FEND (0xC0).
        assert_eq!(wire.first(), Some(&FEND));
        assert_eq!(wire.last(), Some(&FEND));
        Ok(())
    }
}
