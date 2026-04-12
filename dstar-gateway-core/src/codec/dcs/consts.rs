//! `DCS` wire-format constants.
//!
//! Every magic byte and every duration here is annotated with the
//! exact line in the GPL reference implementation it was derived from.

use std::time::Duration;

/// Default UDP port for `DCS` reflectors.
///
/// Reference: `ircDDBGateway/Common/DStarDefines.h:117` (`DCS_PORT = 30051U`).
pub const DEFAULT_PORT: u16 = 30051;

/// `DCS` keepalive interval.
///
/// Reference: `ircDDBGateway/Common/DCSHandler.cpp:54`
/// (`m_pollTimer(1000U, 5U)` = 5 seconds).
pub const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(5);

/// `DCS` keepalive inactivity timeout.
///
/// Reference: `ircDDBGateway/Common/DCSHandler.cpp:55`
/// (`m_pollInactivityTimer(1000U, 60U)` = 60 seconds).
pub const KEEPALIVE_INACTIVITY_TIMEOUT: Duration = Duration::from_secs(60);

/// `DCS` voice inactivity timeout — synthesize `VoiceEnd` after this.
pub const VOICE_INACTIVITY_TIMEOUT: Duration = Duration::from_secs(2);

/// Disconnect ACK timeout — give up waiting for unlink reply.
pub const DISCONNECT_TIMEOUT: Duration = Duration::from_secs(2);

/// LINK packet length (519 bytes — includes 500-byte HTML template).
///
/// Reference: `ircDDBGateway/Common/ConnectData.cpp:364` (return 519U).
pub const LINK_LEN: usize = 519;

/// UNLINK packet length (19 bytes).
///
/// Reference: `ircDDBGateway/Common/ConnectData.cpp:372` (return 19U).
pub const UNLINK_LEN: usize = 19;

/// Connect reply (ACK/NAK) length (14 bytes).
///
/// Reference: `ircDDBGateway/Common/ConnectData.cpp:380,388` (return 14U).
pub const CONNECT_REPLY_LEN: usize = 14;

/// Poll packet length (17 bytes, both directions in this codec).
///
/// Reference: `ircDDBGateway/Common/PollData.cpp:186` (return 17U).
/// The 22-byte `DIR_INCOMING` variant from the reference is not used
/// here — both directions are symmetric 17-byte packets, which matches
/// xlxd's `IsValidKeepAlivePacket` accept list
/// (`xlxd/src/cdcsprotocol.cpp:411`).
pub const POLL_LEN: usize = 17;

/// Voice frame length (100 bytes).
///
/// Reference: `ircDDBGateway/Common/AMBEData.cpp:430` (return 100U).
pub const VOICE_LEN: usize = 100;

/// `"0001"` magic at offsets `[0..4]` of every `DCS` voice frame.
///
/// Reference: `ircDDBGateway/Common/AMBEData.cpp:398-401`.
pub const VOICE_MAGIC: [u8; 4] = *b"0001";

/// ACK tag at `[10..13]` of a 14-byte connect reply, followed by
/// `0x00` at byte `[13]`.
///
/// Reference: `ircDDBGateway/Common/ConnectData.cpp:374-380`
/// (`data[LONG_CALLSIGN_LENGTH + 2U] = 'A'`, i.e. `data[10]`).
pub const CONNECT_ACK_TAG: [u8; 3] = *b"ACK";

/// NAK tag at `[10..13]` of a 14-byte connect reply, followed by
/// `0x00` at byte `[13]`.
///
/// Reference: `ircDDBGateway/Common/ConnectData.cpp:382-388`.
pub const CONNECT_NAK_TAG: [u8; 3] = *b"NAK";

/// End-of-stream sentinel bytes at `[55..58]` of an EOT voice frame.
///
/// Reference: `ircDDBGateway/Common/AMBEData.cpp:410-414`.
pub const VOICE_EOT_MARKER: [u8; 3] = [0x55, 0x55, 0x55];

/// DCS HTML template for REPEATER gateway type (fills bytes `[19..519]`
/// of the LINK packet).
///
/// Reference: `ircDDBGateway/Common/ConnectData.cpp:344-358` shows the
/// reference populating a template via `wxString::Printf(HTML, ...)`.
/// We emit a static short banner instead of the full template — the
/// receiving reflector logs the HTML but does not parse it, so any
/// short identification string that fits in 500 bytes satisfies the
/// protocol. This mirrors `xlxd`, which accepts LINK packets regardless
/// of the HTML payload content.
pub const LINK_HTML_REPEATER: &[u8] =
    b"<table border=\"0\" width=\"95%\"><tr><td>REPEATER</td></tr></table>";

/// DCS HTML template banner for HOTSPOT gateway type.
pub const LINK_HTML_HOTSPOT: &[u8] =
    b"<table border=\"0\" width=\"95%\"><tr><td>HOTSPOT</td></tr></table>";

/// DCS HTML template banner for DONGLE gateway type.
pub const LINK_HTML_DONGLE: &[u8] =
    b"<table border=\"0\" width=\"95%\"><tr><td>DONGLE</td></tr></table>";

/// DCS HTML template banner for STARNET gateway type.
pub const LINK_HTML_STARNET: &[u8] =
    b"<table border=\"0\" width=\"95%\"><tr><td>STARNET</td></tr></table>";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_port_is_30051() {
        assert_eq!(DEFAULT_PORT, 30051);
    }

    #[test]
    fn voice_len_is_100_bytes() {
        assert_eq!(VOICE_LEN, 100);
    }

    #[test]
    fn link_len_is_519_bytes() {
        assert_eq!(LINK_LEN, 519);
    }

    #[test]
    fn unlink_len_is_19_bytes() {
        assert_eq!(UNLINK_LEN, 19);
    }

    #[test]
    fn connect_reply_len_is_14_bytes() {
        assert_eq!(CONNECT_REPLY_LEN, 14);
    }

    #[test]
    fn poll_len_is_17_bytes() {
        assert_eq!(POLL_LEN, 17);
    }

    #[test]
    fn keepalive_interval_five_seconds() {
        assert_eq!(KEEPALIVE_INTERVAL, Duration::from_secs(5));
    }

    #[test]
    fn keepalive_inactivity_sixty_seconds() {
        assert_eq!(KEEPALIVE_INACTIVITY_TIMEOUT, Duration::from_secs(60));
    }

    #[test]
    fn voice_magic_is_0001() {
        assert_eq!(&VOICE_MAGIC, b"0001");
    }

    #[test]
    fn voice_eot_marker_is_triple_0x55() {
        assert_eq!(VOICE_EOT_MARKER, [0x55, 0x55, 0x55]);
    }

    #[test]
    fn ack_tag_is_ack() {
        assert_eq!(&CONNECT_ACK_TAG, b"ACK");
    }

    #[test]
    fn nak_tag_is_nak() {
        assert_eq!(&CONNECT_NAK_TAG, b"NAK");
    }

    #[test]
    fn html_templates_fit_in_500_bytes() {
        assert!(LINK_HTML_REPEATER.len() <= 500);
        assert!(LINK_HTML_HOTSPOT.len() <= 500);
        assert!(LINK_HTML_DONGLE.len() <= 500);
        assert!(LINK_HTML_STARNET.len() <= 500);
    }
}
