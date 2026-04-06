//! APRS digipeater processing logic.
//!
//! Implements the three digipeater algorithms supported by the TH-D75
//! (per Operating Tips section 2.4):
//!
//! - **`UIdigipeat`**: Simple alias replacement. When a path entry matches
//!   a configured alias, replace it with our callsign and mark as used.
//! - **`UIflood`**: Decrement the hop count on a flooding alias (e.g., `CA3-3`).
//!   Drop when the count reaches zero.
//! - **`UItrace`**: Like `UIflood`, but also inserts our callsign into the
//!   path before the decremented hop entry.
//!
//! All functions are pure logic with no I/O or async dependencies.

use super::{Ax25Address, Ax25Packet};

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/// Digipeater configuration.
///
/// Controls which packets are relayed and how the digipeater path is modified.
#[derive(Debug, Clone)]
pub struct DigipeaterConfig {
    /// Our callsign (used for `UIdigipeat` and `UItrace` path insertion).
    pub callsign: Ax25Address,
    /// `UIdigipeat` aliases (e.g., `["WIDE1-1"]`). Relay if path contains
    /// this alias, replace with our callsign + completion flag.
    pub uidigipeat_aliases: Vec<String>,
    /// `UIflood` alias base (e.g., `"CA"`). Relay and decrement hop count.
    /// The SSID encodes the remaining hop count.
    pub uiflood_alias: Option<String>,
    /// `UItrace` alias base (e.g., `"WIDE"`). Relay, decrement hop count,
    /// and insert our callsign in the path.
    pub uitrace_alias: Option<String>,
}

// ---------------------------------------------------------------------------
// Result
// ---------------------------------------------------------------------------

/// Result of digipeater processing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DigiAction {
    /// Do not relay this packet.
    Drop,
    /// Relay with modified digipeater path.
    Relay {
        /// The packet with its path modified for retransmission.
        modified_packet: Ax25Packet,
    },
}

// ---------------------------------------------------------------------------
// Processing
// ---------------------------------------------------------------------------

impl DigipeaterConfig {
    /// Process an incoming AX.25 UI frame through digipeater logic.
    ///
    /// Examines the digipeater path for the first unused entry (an entry
    /// whose callsign hasn't been marked with `*`). If it matches a
    /// configured alias, the path is rewritten according to the matching
    /// algorithm and the packet is returned for relay.
    ///
    /// Returns [`DigiAction::Drop`] if no alias matches or the packet
    /// should not be relayed.
    #[must_use]
    pub fn process(&self, packet: &Ax25Packet) -> DigiAction {
        // Only process UI frames (control=0x03, PID=0xF0).
        if packet.control != 0x03 || packet.protocol != 0xF0 {
            return DigiAction::Drop;
        }

        // Find the first un-used digipeater entry.
        // In AX.25, the H-bit (has-been-repeated) is encoded by appending
        // '*' to the callsign in our representation.
        let Some(first_unused) = packet.digipeaters.iter().position(|d| !is_used_digi(d)) else {
            return DigiAction::Drop;
        };

        let digi = &packet.digipeaters[first_unused];

        // Try UIdigipeat aliases first.
        let digi_str = format!("{digi}");
        for alias in &self.uidigipeat_aliases {
            if digi_str.eq_ignore_ascii_case(alias) {
                return apply_uidigipeat(&self.callsign, packet, first_unused);
            }
        }

        // Try UIflood alias.
        if let Some(ref flood_alias) = self.uiflood_alias
            && digi.callsign.eq_ignore_ascii_case(flood_alias)
            && digi.ssid > 0
        {
            return apply_uiflood(packet, first_unused);
        }

        // Try UItrace alias.
        if let Some(ref trace_alias) = self.uitrace_alias
            && digi.callsign.eq_ignore_ascii_case(trace_alias)
            && digi.ssid > 0
        {
            return apply_uitrace(&self.callsign, packet, first_unused);
        }

        DigiAction::Drop
    }
}

/// `UIdigipeat`: replace the alias entry with our callsign, marked as used.
fn apply_uidigipeat(callsign: &Ax25Address, packet: &Ax25Packet, idx: usize) -> DigiAction {
    let mut modified = packet.clone();
    modified.digipeaters[idx] = mark_used(callsign);
    DigiAction::Relay {
        modified_packet: modified,
    }
}

/// `UIflood`: decrement the hop count. Mark as used when exhausted.
fn apply_uiflood(packet: &Ax25Packet, idx: usize) -> DigiAction {
    let digi = &packet.digipeaters[idx];
    let new_ssid = digi.ssid - 1;

    let mut modified = packet.clone();
    if new_ssid == 0 {
        modified.digipeaters[idx] = mark_used(&Ax25Address {
            callsign: digi.callsign.clone(),
            ssid: 0,
        });
    } else {
        modified.digipeaters[idx] = Ax25Address {
            callsign: digi.callsign.clone(),
            ssid: new_ssid,
        };
    }
    DigiAction::Relay {
        modified_packet: modified,
    }
}

/// `UItrace`: like `UIflood` but also inserts our callsign before the hop entry.
fn apply_uitrace(callsign: &Ax25Address, packet: &Ax25Packet, idx: usize) -> DigiAction {
    // AX.25 supports at most 8 digipeater entries.
    if packet.digipeaters.len() >= 8 {
        return DigiAction::Drop;
    }

    let digi = &packet.digipeaters[idx];
    let new_ssid = digi.ssid - 1;

    let mut modified = packet.clone();

    // Insert our callsign (marked as used) before the current entry.
    modified.digipeaters.insert(idx, mark_used(callsign));

    // The original entry shifted to idx+1; update its hop count.
    let trace_idx = idx + 1;
    if new_ssid == 0 {
        modified.digipeaters[trace_idx] = mark_used(&Ax25Address {
            callsign: digi.callsign.clone(),
            ssid: 0,
        });
    } else {
        modified.digipeaters[trace_idx] = Ax25Address {
            callsign: digi.callsign.clone(),
            ssid: new_ssid,
        };
    }

    DigiAction::Relay {
        modified_packet: modified,
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Check if a digipeater entry has been used (has-been-repeated).
///
/// A callsign ending with `*` indicates the H-bit is set (the entry
/// has been consumed by a prior digipeater).
fn is_used_digi(addr: &Ax25Address) -> bool {
    addr.callsign.ends_with('*')
}

/// Create a copy of an address marked as "used" (H-bit set).
///
/// Appends `*` to the callsign if not already present.
fn mark_used(addr: &Ax25Address) -> Ax25Address {
    let callsign = if addr.callsign.ends_with('*') {
        addr.callsign.clone()
    } else {
        format!("{}*", addr.callsign)
    };
    Ax25Address {
        callsign,
        ssid: addr.ssid,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn make_addr(call: &str, ssid: u8) -> Ax25Address {
        Ax25Address {
            callsign: call.to_owned(),
            ssid,
        }
    }

    fn make_packet(digipeaters: Vec<Ax25Address>) -> Ax25Packet {
        Ax25Packet {
            source: make_addr("N0CALL", 7),
            destination: make_addr("APK005", 0),
            digipeaters,
            control: 0x03,
            protocol: 0xF0,
            info: b"!3518.00N/08414.00W-test".to_vec(),
        }
    }

    fn make_config() -> DigipeaterConfig {
        DigipeaterConfig {
            callsign: make_addr("MYDIGI", 0),
            uidigipeat_aliases: vec!["WIDE1-1".to_owned()],
            uiflood_alias: Some("CA".to_owned()),
            uitrace_alias: Some("WIDE".to_owned()),
        }
    }

    // ---- UIdigipeat tests ----

    #[test]
    fn uidigipeat_matches_alias() {
        let config = make_config();
        let packet = make_packet(vec![make_addr("WIDE1", 1), make_addr("WIDE2", 1)]);

        match config.process(&packet) {
            DigiAction::Relay { modified_packet } => {
                assert_eq!(modified_packet.digipeaters[0].callsign, "MYDIGI*");
                assert_eq!(modified_packet.digipeaters[0].ssid, 0);
                // Second entry unchanged.
                assert_eq!(modified_packet.digipeaters[1].callsign, "WIDE2");
                assert_eq!(modified_packet.digipeaters[1].ssid, 1);
            }
            DigiAction::Drop => panic!("expected Relay"),
        }
    }

    #[test]
    fn uidigipeat_skips_used_entries() {
        let config = make_config();
        let packet = make_packet(vec![make_addr("N1ABC*", 0), make_addr("WIDE1", 1)]);

        match config.process(&packet) {
            DigiAction::Relay { modified_packet } => {
                // First entry untouched (already used).
                assert_eq!(modified_packet.digipeaters[0].callsign, "N1ABC*");
                // Second entry replaced.
                assert_eq!(modified_packet.digipeaters[1].callsign, "MYDIGI*");
            }
            DigiAction::Drop => panic!("expected Relay"),
        }
    }

    #[test]
    fn uidigipeat_no_match_drops() {
        let config = make_config();
        let packet = make_packet(vec![make_addr("RELAY", 0)]);

        assert_eq!(config.process(&packet), DigiAction::Drop);
    }

    #[test]
    fn uidigipeat_all_used_drops() {
        let config = make_config();
        let packet = make_packet(vec![make_addr("WIDE1*", 1)]);

        assert_eq!(config.process(&packet), DigiAction::Drop);
    }

    // ---- UIflood tests ----

    #[test]
    fn uiflood_decrements_hop() {
        let config = make_config();
        let packet = make_packet(vec![make_addr("N1ABC*", 0), make_addr("CA", 3)]);

        match config.process(&packet) {
            DigiAction::Relay { modified_packet } => {
                assert_eq!(modified_packet.digipeaters[1].callsign, "CA");
                assert_eq!(modified_packet.digipeaters[1].ssid, 2);
            }
            DigiAction::Drop => panic!("expected Relay"),
        }
    }

    #[test]
    fn uiflood_last_hop_marks_used() {
        let config = make_config();
        let packet = make_packet(vec![make_addr("CA", 1)]);

        match config.process(&packet) {
            DigiAction::Relay { modified_packet } => {
                assert_eq!(modified_packet.digipeaters[0].callsign, "CA*");
                assert_eq!(modified_packet.digipeaters[0].ssid, 0);
            }
            DigiAction::Drop => panic!("expected Relay"),
        }
    }

    #[test]
    fn uiflood_zero_ssid_drops() {
        let config = make_config();
        let packet = make_packet(vec![make_addr("CA", 0)]);

        assert_eq!(config.process(&packet), DigiAction::Drop);
    }

    // ---- UItrace tests ----

    #[test]
    fn uitrace_inserts_callsign_and_decrements() {
        let config = make_config();
        let packet = make_packet(vec![make_addr("WIDE", 3)]);

        match config.process(&packet) {
            DigiAction::Relay { modified_packet } => {
                assert_eq!(modified_packet.digipeaters.len(), 2);
                // Our callsign inserted first, marked used.
                assert_eq!(modified_packet.digipeaters[0].callsign, "MYDIGI*");
                assert_eq!(modified_packet.digipeaters[0].ssid, 0);
                // Original entry with decremented hop.
                assert_eq!(modified_packet.digipeaters[1].callsign, "WIDE");
                assert_eq!(modified_packet.digipeaters[1].ssid, 2);
            }
            DigiAction::Drop => panic!("expected Relay"),
        }
    }

    #[test]
    fn uitrace_last_hop_marks_exhausted() {
        let config = make_config();
        let packet = make_packet(vec![make_addr("WIDE", 1)]);

        match config.process(&packet) {
            DigiAction::Relay { modified_packet } => {
                assert_eq!(modified_packet.digipeaters.len(), 2);
                assert_eq!(modified_packet.digipeaters[0].callsign, "MYDIGI*");
                assert_eq!(modified_packet.digipeaters[1].callsign, "WIDE*");
                assert_eq!(modified_packet.digipeaters[1].ssid, 0);
            }
            DigiAction::Drop => panic!("expected Relay"),
        }
    }

    #[test]
    fn uitrace_full_path_drops() {
        let config = make_config();
        // 8 digipeaters = maximum, can't insert another.
        let mut digis: Vec<Ax25Address> = (0..8).map(|i| make_addr("USED*", i)).collect();
        // Replace last one with an unused WIDE entry.
        digis[7] = make_addr("WIDE", 2);

        // But the first unused is at index 7, and there are already 8 entries.
        let packet = make_packet(digis);
        assert_eq!(config.process(&packet), DigiAction::Drop);
    }

    // ---- Edge cases ----

    #[test]
    fn non_ui_frame_drops() {
        let config = make_config();
        let mut packet = make_packet(vec![make_addr("WIDE1", 1)]);
        packet.control = 0x01; // Not a UI frame.

        assert_eq!(config.process(&packet), DigiAction::Drop);
    }

    #[test]
    fn empty_digipeater_path_drops() {
        let config = make_config();
        let packet = make_packet(vec![]);

        assert_eq!(config.process(&packet), DigiAction::Drop);
    }

    #[test]
    fn case_insensitive_alias_match() {
        let config = DigipeaterConfig {
            callsign: make_addr("MYDIGI", 0),
            uidigipeat_aliases: vec!["wide1-1".to_owned()],
            uiflood_alias: None,
            uitrace_alias: None,
        };
        let packet = make_packet(vec![make_addr("WIDE1", 1)]);

        match config.process(&packet) {
            DigiAction::Relay { .. } => {}
            DigiAction::Drop => panic!("expected case-insensitive match"),
        }
    }

    #[test]
    fn uitrace_priority_over_flood_when_both_configured() {
        // If both uiflood and uitrace are configured for different aliases,
        // the correct one should match.
        let config = DigipeaterConfig {
            callsign: make_addr("MYDIGI", 0),
            uidigipeat_aliases: vec![],
            uiflood_alias: Some("CA".to_owned()),
            uitrace_alias: Some("WIDE".to_owned()),
        };

        // UIflood packet.
        let flood_pkt = make_packet(vec![make_addr("CA", 2)]);
        match config.process(&flood_pkt) {
            DigiAction::Relay { modified_packet } => {
                // Should NOT insert callsign (flood, not trace).
                assert_eq!(modified_packet.digipeaters.len(), 1);
                assert_eq!(modified_packet.digipeaters[0].ssid, 1);
            }
            DigiAction::Drop => panic!("expected flood relay"),
        }

        // UItrace packet.
        let trace_pkt = make_packet(vec![make_addr("WIDE", 2)]);
        match config.process(&trace_pkt) {
            DigiAction::Relay { modified_packet } => {
                // Should insert callsign (trace).
                assert_eq!(modified_packet.digipeaters.len(), 2);
            }
            DigiAction::Drop => panic!("expected trace relay"),
        }
    }
}
