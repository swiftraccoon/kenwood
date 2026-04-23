//! TH-D75-specific APRS integration.
//!
//! Generic packet-radio protocols live in their own workspace crates:
//! - [`kiss_tnc`] — KISS TNC wire framing.
//! - [`ax25_codec`] — AX.25 frame codec.
//! - [`aprs`] — APRS parser, digipeater, `SmartBeaconing`, messaging, station list.
//! - [`aprs_is`] — APRS-IS TCP client.
//!
//! This module contains only the D75-specific glue: [`client::AprsClient`]
//! owning a [`Radio`](crate::Radio) and [`KissSession`](crate::KissSession);
//! [`mcp_bridge`] for MCP-memory ↔ runtime `SmartBeaconingConfig` conversion;
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
pub mod mcp_bridge;

use aprs::AprsError;
use ax25_codec::{Ax25Address, Ax25Packet, build_ax25};
use kiss_tnc::{KissFrame, encode_kiss_frame};

// ---------------------------------------------------------------------------
// Construction helpers for the foreign `Ax25Packet` type
// ---------------------------------------------------------------------------

/// Build a minimal APRS UI frame with the given source, destination, path,
/// and info field. Control = 0x03, PID = 0xF0.
///
/// This is the free-function form of the former `Ax25Packet::ui_frame`
/// inherent constructor; since [`Ax25Packet`] is a foreign type from the
/// [`ax25_codec`] crate, inherent impls on it must live there.
#[must_use]
pub const fn ax25_ui_frame(
    source: Ax25Address,
    destination: Ax25Address,
    path: Vec<Ax25Address>,
    info: Vec<u8>,
) -> Ax25Packet {
    Ax25Packet {
        source,
        destination,
        digipeaters: path,
        control: 0x03,
        protocol: 0xF0,
        info,
    }
}

/// Encode an [`Ax25Packet`] as a KISS-framed data frame ready for the
/// wire. Equivalent to wrapping [`build_ax25`] in [`encode_kiss_frame`]
/// with `port = 0` and `command = Data`.
///
/// This is the free-function form of the former `Ax25Packet::encode_kiss`
/// inherent method; see [`ax25_ui_frame`] for why.
#[must_use]
pub fn ax25_to_kiss_wire(packet: &Ax25Packet) -> Vec<u8> {
    let ax25_bytes = build_ax25(packet);
    encode_kiss_frame(&KissFrame::data(ax25_bytes))
}

// ---------------------------------------------------------------------------
// APRS digipeater-path helpers
// ---------------------------------------------------------------------------

/// Default APRS digipeater path: WIDE1-1, WIDE2-1.
const DEFAULT_DIGIPEATERS: &[(&str, u8)] = &[("WIDE1", 1), ("WIDE2", 1)];

/// Parse a digipeater path string like `"WIDE1-1,WIDE2-2"` into addresses.
///
/// Accepts comma-separated entries, each of the form `CALLSIGN[-SSID]`.
/// Whitespace around entries is trimmed. An empty string returns an empty
/// path (direct transmission with no digipeating).
///
/// # Errors
///
/// Returns [`AprsError::InvalidPath`] if any entry has an SSID that is
/// not a valid 0-15 integer, or if the callsign is empty or longer than
/// 6 characters.
///
/// # Examples
///
/// ```
/// use kenwood_thd75::aprs::parse_digipeater_path;
/// let path = parse_digipeater_path("WIDE1-1,WIDE2-2").unwrap();
/// assert_eq!(path.len(), 2);
/// assert_eq!(path[0].callsign, "WIDE1");
/// assert_eq!(path[0].ssid, 1);
/// ```
pub fn parse_digipeater_path(s: &str) -> Result<Vec<Ax25Address>, AprsError> {
    let trimmed = s.trim();
    if trimmed.is_empty() {
        return Ok(Vec::new());
    }
    let mut result = Vec::new();
    for entry in trimmed.split(',') {
        let entry = entry.trim();
        if entry.is_empty() {
            return Err(AprsError::InvalidPath(s.to_owned()));
        }
        let (callsign, ssid) = if let Some((call, ssid_str)) = entry.split_once('-') {
            let ssid: u8 = ssid_str
                .parse()
                .map_err(|_| AprsError::InvalidPath(s.to_owned()))?;
            if ssid > 15 {
                return Err(AprsError::InvalidPath(s.to_owned()));
            }
            (call, ssid)
        } else {
            (entry, 0)
        };
        if callsign.is_empty() || callsign.len() > 6 {
            return Err(AprsError::InvalidPath(s.to_owned()));
        }
        result.push(Ax25Address::new(callsign, ssid));
    }
    Ok(result)
}

/// Build the default digipeater path as [`Ax25Address`] entries
/// (`WIDE1-1,WIDE2-1`).
#[must_use]
pub fn default_digipeater_path() -> Vec<Ax25Address> {
    DEFAULT_DIGIPEATERS
        .iter()
        .map(|(call, ssid)| Ax25Address::new(call, *ssid))
        .collect()
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
        assert_eq!(parse_digipeater_path("")?, Vec::<Ax25Address>::new());
        assert_eq!(parse_digipeater_path("   ")?, Vec::<Ax25Address>::new());
        Ok(())
    }

    #[test]
    fn parse_digipeater_path_single() -> TestResult {
        let path = parse_digipeater_path("WIDE1-1")?;
        assert_eq!(path.len(), 1);
        let first = path.first().ok_or("path[0] missing")?;
        assert_eq!(first.callsign, "WIDE1");
        assert_eq!(first.ssid, 1);
        Ok(())
    }

    #[test]
    fn parse_digipeater_path_multiple() -> TestResult {
        let path = parse_digipeater_path("WIDE1-1,WIDE2-2")?;
        assert_eq!(path.len(), 2);
        assert_eq!(path.first().ok_or("path[0] missing")?.callsign, "WIDE1");
        let second = path.get(1).ok_or("path[1] missing")?;
        assert_eq!(second.callsign, "WIDE2");
        assert_eq!(second.ssid, 2);
        Ok(())
    }

    #[test]
    fn parse_digipeater_path_no_ssid() -> TestResult {
        let path = parse_digipeater_path("WIDE1")?;
        assert_eq!(path.len(), 1);
        assert_eq!(path.first().ok_or("path[0] missing")?.ssid, 0);
        Ok(())
    }

    #[test]
    fn parse_digipeater_path_rejects_bad_ssid() {
        assert!(parse_digipeater_path("WIDE1-99").is_err());
        assert!(parse_digipeater_path("WIDE1-abc").is_err());
    }

    #[test]
    fn parse_digipeater_path_rejects_long_callsign() {
        assert!(parse_digipeater_path("TOOLONG-1").is_err());
    }

    #[test]
    fn default_path_is_wide1_wide2() -> TestResult {
        let path = default_digipeater_path();
        assert_eq!(path.len(), 2);
        let first = path.first().ok_or("path[0] missing")?;
        assert_eq!(first.callsign, "WIDE1");
        assert_eq!(first.ssid, 1);
        let second = path.get(1).ok_or("path[1] missing")?;
        assert_eq!(second.callsign, "WIDE2");
        assert_eq!(second.ssid, 1);
        Ok(())
    }

    #[test]
    fn ax25_ui_frame_sets_control_and_pid() {
        let packet = ax25_ui_frame(
            Ax25Address::new("N0CALL", 7),
            Ax25Address::new("APRS", 0),
            vec![],
            b"!test".to_vec(),
        );
        assert_eq!(packet.control, 0x03);
        assert_eq!(packet.protocol, 0xF0);
        assert_eq!(packet.source.callsign, "N0CALL");
        assert_eq!(packet.destination.callsign, "APRS");
        assert_eq!(&packet.info, b"!test");
    }

    #[test]
    fn ax25_to_kiss_wire_produces_valid_kiss_frame() {
        let packet = ax25_ui_frame(
            Ax25Address::new("N0CALL", 7),
            Ax25Address::new("APRS", 0),
            vec![],
            b"!test".to_vec(),
        );
        let wire = ax25_to_kiss_wire(&packet);
        // KISS frame starts and ends with FEND (0xC0).
        assert_eq!(wire.first(), Some(&FEND));
        assert_eq!(wire.last(), Some(&FEND));
    }
}
