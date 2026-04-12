//! `DPlus` wire-format constants.
//!
//! Every magic byte and every duration here is annotated with the
//! exact line in the GPL reference implementation it was derived from.

use std::time::Duration;

/// Default UDP port for `DPlus` (REF) reflectors.
///
/// Reference: `ircDDBGateway/Common/DStarDefines.h:115` (`DPLUS_PORT = 20001U`).
/// Reference: `xlxd/src/main.h:93` (`#define DPLUS_PORT 20001`).
pub const DEFAULT_PORT: u16 = 20001;

/// `DPlus` keepalive interval.
///
/// Reference: `ircDDBGateway/Common/DPlusHandler.cpp:57`
/// (`m_pollTimer(1000U, 1U)` = 1 second).
/// Reference: `xlxd/src/main.h:94` (`DPLUS_KEEPALIVE_PERIOD = 1`).
///
/// **The legacy `dstar-gateway` crate ships 5s — that is the bug
/// the audit found.** This rewrite uses 1s to match both references.
pub const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(1);

/// `DPlus` outgoing keepalive inactivity timeout.
///
/// Reference: `ircDDBGateway/Common/DPlusHandler.cpp:58`
/// (`m_pollInactivityTimer(1000U, 30U)` = 30 seconds for outgoing
/// reflector links).
pub const KEEPALIVE_INACTIVITY_TIMEOUT: Duration = Duration::from_secs(30);

/// `DPlus` voice inactivity timeout — synthesize `VoiceEnd` after this.
///
/// Reference: `ircDDBGateway/Common/DStarDefines.h:122`
/// (`NETWORK_TIMEOUT = 2U`).
pub const VOICE_INACTIVITY_TIMEOUT: Duration = Duration::from_secs(2);

/// `DPlus` disconnect ACK timeout — give up waiting for unlink reply.
pub const DISCONNECT_TIMEOUT: Duration = Duration::from_secs(2);

/// Number of times the initial connect packet is retransmitted.
pub const CONNECT_RETX: u8 = 2;

/// Number of times the voice header is retransmitted.
///
/// Reference: `ircDDBGateway/Common/DPlusProtocolHandler.cpp:64-68`
/// (the `for (i = 0; i < 5; i++)` loop in `writeHeader`).
pub const HEADER_RETX: u8 = 5;

/// Number of times the unlink packet is retransmitted.
///
/// Reference: `ircDDBGateway/Common/DPlusHandler.cpp:481-482` sends
/// unlink twice; we send three times for an extra margin on lossy links.
pub const DISCONNECT_RETX: u8 = 3;

/// Inter-copy delay for retransmission bursts.
pub const RETX_DELAY: Duration = Duration::from_millis(50);

/// LINK1 connect packet (5 bytes).
///
/// Reference: `ircDDBGateway/Common/ConnectData.cpp:441-447`
/// (`getDPlusData CT_LINK1`).
/// Reference: `xlxd/src/cdplusprotocol.cpp:430`
/// (`IsValidConnectPacket`).
pub const LINK1_BYTES: [u8; 5] = [0x05, 0x00, 0x18, 0x00, 0x01];

/// UNLINK packet (5 bytes).
///
/// Reference: `ircDDBGateway/Common/ConnectData.cpp:475-481`
/// (`getDPlusData CT_UNLINK`).
/// Reference: `xlxd/src/cdplusprotocol.cpp:447-451`
/// (`IsValidDisconnectPacket`).
pub const UNLINK_BYTES: [u8; 5] = [0x05, 0x00, 0x18, 0x00, 0x00];

/// Keepalive poll packet (3 bytes).
///
/// Reference: `xlxd/src/cdplusprotocol.cpp:529-533`
/// (`EncodeKeepAlivePacket`).
pub const POLL_BYTES: [u8; 3] = [0x03, 0x60, 0x00];

/// LINK1 ACK echo — server replies with the same bytes as LINK1.
pub const LINK1_ACK_BYTES: [u8; 5] = LINK1_BYTES;

/// UNLINK ACK echo — server replies with the same bytes as UNLINK.
pub const UNLINK_ACK_BYTES: [u8; 5] = UNLINK_BYTES;

/// Poll echo — server replies with the same bytes as POLL.
pub const POLL_ECHO_BYTES: [u8; 3] = POLL_BYTES;

/// LINK2 reply tag for accept (4 bytes at offsets `[4..8]` of an
/// 8-byte reply).
///
/// Reference: `ircDDBGateway/Common/ConnectData.cpp:251-259`
/// (`memcmp(data + 4, "OKRW", 4)`).
/// Reference: `xlxd/src/cdplusprotocol.cpp:535-539`
/// (`EncodeLoginAckPacket`).
pub const LINK2_ACCEPT_TAG: [u8; 4] = *b"OKRW";

/// LINK2 reply tag for busy (4 bytes at offsets `[4..8]` of an
/// 8-byte reply).
///
/// Reference: `xlxd/src/cdplusprotocol.cpp:541-545`
/// (`EncodeLoginNackPacket`).
pub const LINK2_BUSY_TAG: [u8; 4] = *b"BUSY";

/// LINK2 reply prefix (first 4 bytes of an 8-byte reply, before the tag).
pub const LINK2_REPLY_PREFIX: [u8; 4] = [0x08, 0xC0, 0x04, 0x00];

/// LINK2 packet header (4 bytes; full packet is 28 bytes with
/// callsign + `DV019999`).
///
/// Reference: `ircDDBGateway/Common/ConnectData.cpp:449-473`
/// (`getDPlusData CT_LINK2`).
pub const LINK2_HEADER: [u8; 4] = [0x1C, 0xC0, 0x04, 0x00];

/// `DV019999` 8-byte client identifier embedded in LINK2 at offsets
/// `[20..28]`.
pub const DV_CLIENT_ID: [u8; 8] = *b"DV019999";

/// DSVT magic at offsets `[2..6]` of every voice header / voice
/// data / EOT packet.
pub const DSVT_MAGIC: [u8; 4] = *b"DSVT";

/// Voice header DSVT length byte (offset 0 of a voice header packet).
pub const VOICE_HEADER_PREFIX: u8 = 0x3A;

/// Voice data DSVT length byte (offset 0 of a voice data packet).
pub const VOICE_DATA_PREFIX: u8 = 0x1D;

/// Voice EOT DSVT length byte (offset 0 of an EOT packet).
pub const VOICE_EOT_PREFIX: u8 = 0x20;

/// DSVT type byte (offset 1 of every DSVT-framed packet).
pub const DSVT_TYPE: u8 = 0x80;

/// EOT trailing pattern (last 6 bytes of an EOT packet).
///
/// Reference: `ircDDBGateway/Common/DStarDefines.h:34`
/// (`END_PATTERN_BYTES`).
pub const VOICE_EOT_TRAILER: [u8; 6] = [0x55, 0x55, 0x55, 0x55, 0xC8, 0x7A];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keepalive_interval_is_one_second() {
        assert_eq!(KEEPALIVE_INTERVAL, Duration::from_secs(1));
    }

    #[test]
    fn link1_bytes_are_known() {
        assert_eq!(LINK1_BYTES, [0x05, 0x00, 0x18, 0x00, 0x01]);
    }

    #[test]
    fn unlink_bytes_are_known() {
        assert_eq!(UNLINK_BYTES, [0x05, 0x00, 0x18, 0x00, 0x00]);
    }

    #[test]
    fn poll_bytes_are_known() {
        assert_eq!(POLL_BYTES, [0x03, 0x60, 0x00]);
    }

    #[test]
    fn link2_accept_is_okrw() {
        assert_eq!(&LINK2_ACCEPT_TAG, b"OKRW");
    }

    #[test]
    fn link2_busy_is_busy() {
        assert_eq!(&LINK2_BUSY_TAG, b"BUSY");
    }

    #[test]
    fn dsvt_magic_is_dsvt() {
        assert_eq!(&DSVT_MAGIC, b"DSVT");
    }

    #[test]
    fn voice_eot_trailer_is_known() {
        assert_eq!(VOICE_EOT_TRAILER, [0x55, 0x55, 0x55, 0x55, 0xC8, 0x7A]);
    }
}
