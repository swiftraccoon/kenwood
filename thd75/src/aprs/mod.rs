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

use ax25_codec::{
    Ax25Address, Ax25Error, Ax25Header, Ax25Packet, Ax25Pid, CommandResponse, DigipeaterPath,
    PollFinal, RouteEntry,
};
use kiss_tnc::KissFrame;

pub mod client;
pub mod stored_settings_bridge;

/// A path entry failed canonical AX.25 parsing or exceeded path capacity.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid digipeater path entry {entry}: {source}")]
pub struct DigipeaterPathError {
    input: String,
    entry: usize,
    source: Ax25Error,
}

impl DigipeaterPathError {
    /// Borrow the complete original input, including whitespace.
    #[must_use]
    pub fn input(&self) -> &str {
        &self.input
    }

    /// Return the zero-based failing entry index.
    #[must_use]
    pub const fn entry(&self) -> usize {
        self.entry
    }

    /// Borrow the canonical address or path-capacity failure.
    #[must_use]
    pub const fn error(&self) -> &Ax25Error {
        &self.source
    }
}

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
        Ax25Header {
            source,
            destination,
            digipeaters: path,
            command_response: CommandResponse::Command,
        },
        PollFinal::Clear,
        Ax25Pid::NoLayer3,
        info,
    )
}

/// Encode an [`Ax25Packet`] as a KISS-framed data frame ready for the
/// wire. Wraps [`Ax25Packet::to_bytes`] in a port-zero KISS data frame,
/// encoded by [`KissFrame::to_bytes`].
///
/// This is a free function for the same ownership reason as
/// [`ax25_ui_frame`].
#[must_use]
pub fn ax25_to_kiss_wire(packet: &Ax25Packet) -> Vec<u8> {
    let ax25_bytes = packet.to_bytes();
    KissFrame::data(ax25_bytes).to_bytes()
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
/// Returns [`DigipeaterPathError`] containing the original input if an
/// entry is empty or noncanonical, or if the path contains more than eight
/// entries.
///
/// # Examples
///
/// ```
/// use kenwood_thd75::aprs::parse_digipeater_path;
/// # fn example() -> Result<(), Box<dyn std::error::Error>> {
/// let path = parse_digipeater_path("WIDE1-1,WIDE2-2")?;
/// assert_eq!(path.len(), 2);
/// let first = path.first().ok_or("the parsed path is empty")?;
/// assert_eq!(first.address.callsign, "WIDE1");
/// assert_eq!(first.address.ssid, 1);
/// # Ok(())
/// # }
/// ```
pub fn parse_digipeater_path(s: &str) -> Result<DigipeaterPath, DigipeaterPathError> {
    let trimmed = s.trim();
    if trimmed.is_empty() {
        return Ok(DigipeaterPath::empty());
    }
    let mut result = DigipeaterPath::empty();
    for (index, entry) in trimmed.split(',').enumerate() {
        let failure = |source| DigipeaterPathError {
            input: s.to_owned(),
            entry: index,
            source,
        };
        let address = Ax25Address::from_canonical_str(entry.trim()).map_err(failure)?;
        let route = RouteEntry::from_address(address);
        result.try_insert(result.len(), route).map_err(failure)?;
    }
    Ok(result)
}

/// Build the default digipeater path as validated [`RouteEntry`] entries.
///
/// # Errors
///
/// Returns [`DigipeaterPathError`] if the library's static default ever
/// stops satisfying the same validation applied to caller-provided paths.
/// Keeping the check explicit prevents a malformed default from being
/// silently shortened.
pub fn default_digipeater_path() -> Result<DigipeaterPath, DigipeaterPathError> {
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
            assert!(
                parse_digipeater_path(invalid).is_err(),
                "accepted noncanonical path {invalid:?}",
            );
        }
    }

    #[test]
    fn parse_digipeater_path_rejects_noncanonical_callsigns() {
        for invalid in ["wide1-1", "WiDE1-1", "WIDE_1-1", "WIDE 1-1", "WIDE1-1*"] {
            assert!(
                parse_digipeater_path(invalid).is_err(),
                "accepted noncanonical path {invalid:?}",
            );
        }
    }

    #[test]
    fn parse_digipeater_path_error_preserves_the_original_input() -> TestResult {
        let input = "  WIDE1-1, WIDE2-01  ";
        let error = parse_digipeater_path(input)
            .err()
            .ok_or("invalid SSID accepted")?;
        assert_eq!(error.input(), input);
        assert_eq!(error.entry(), 1);
        assert!(matches!(error.error(), Ax25Error::InvalidAddress(_)));
        assert!(std::error::Error::source(&error).is_some());
        Ok(())
    }

    #[test]
    fn parse_digipeater_path_rejects_empty_entries() {
        for invalid in [",WIDE1-1", "WIDE1-1,", "WIDE1-1,,WIDE2-1"] {
            assert!(parse_digipeater_path(invalid).is_err());
        }
    }

    #[test]
    fn parse_digipeater_path_rejects_long_callsign() {
        let input = "TOOLONG-1";
        assert!(parse_digipeater_path(input).is_err());
    }

    #[test]
    fn parse_digipeater_path_accepts_eight_entries_and_rejects_the_ninth() -> TestResult {
        let eight = "WIDE1-1,WIDE1-2,WIDE1-3,WIDE1-4,WIDE1-5,WIDE1-6,WIDE1-7,WIDE1-8";
        let nine = "WIDE1-1,WIDE1-2,WIDE1-3,WIDE1-4,WIDE1-5,WIDE1-6,WIDE1-7,WIDE1-8,WIDE1-9";

        assert_eq!(parse_digipeater_path(eight)?.len(), 8);
        let error = parse_digipeater_path(nine)
            .err()
            .ok_or("ninth hop accepted")?;
        assert_eq!(error.input(), nine);
        assert_eq!(error.entry(), 8);
        assert_eq!(error.error(), &Ax25Error::TooManyDigipeaters);
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
        assert_eq!(packet.header.source.callsign, "N0CALL");
        assert_eq!(packet.header.destination.callsign, "APRS");
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
