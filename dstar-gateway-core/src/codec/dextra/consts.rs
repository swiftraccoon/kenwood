//! `DExtra` wire-format constants.
//!
//! Every magic byte and every duration here is annotated with the
//! exact line in the GPL reference implementation it was derived from.

use std::time::Duration;

/// Default UDP port for `DExtra` reflectors.
///
/// Reference: `ircDDBGateway/Common/DStarDefines.h:116` (`DEXTRA_PORT = 30001U`).
pub const DEFAULT_PORT: u16 = 30001;

/// `DExtra` keepalive interval.
///
/// Reference: `ircDDBGateway/Common/DExtraHandler.cpp:51`
/// (`m_pollTimer(1000U, 10U)` = 10 seconds).
pub const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(10);

/// `DExtra` keepalive inactivity timeout.
///
/// Reference: `ircDDBGateway/Common/DExtraHandler.cpp:52`
/// (`m_pollInactivityTimer(1000U, 60U)` = 60 seconds).
pub const KEEPALIVE_INACTIVITY_TIMEOUT: Duration = Duration::from_secs(60);

/// `DExtra` voice inactivity timeout — synthesize `VoiceEnd` after this.
pub const VOICE_INACTIVITY_TIMEOUT: Duration = Duration::from_secs(2);

/// `DExtra` disconnect ACK timeout — give up waiting for unlink reply.
pub const DISCONNECT_TIMEOUT: Duration = Duration::from_secs(2);

/// Number of times the connect packet is retransmitted.
pub const CONNECT_RETX: u8 = 2;

/// Number of times the voice header is retransmitted.
pub const HEADER_RETX: u8 = 5;

/// DSVT magic at offsets `[0..4]` of every `DExtra` voice packet.
///
/// **Note**: unlike `DPlus`, `DExtra` has NO 2-byte length prefix —
/// DSVT magic is at offset 0, not offset 2.
pub const DSVT_MAGIC: [u8; 4] = *b"DSVT";

/// Connect (LINK/UNLINK) packet length (11 bytes).
///
/// Reference: `ircDDBGateway/Common/ConnectData.cpp:283-300` (return 11U).
pub const CONNECT_LEN: usize = 11;

/// Connect reply length (14 bytes: 11 + 3 tag).
///
/// Reference: `ircDDBGateway/Common/ConnectData.cpp:302-316` (return 14U).
pub const CONNECT_REPLY_LEN: usize = 14;

/// Poll packet length (9 bytes).
///
/// Reference: `ircDDBGateway/Common/PollData.cpp:155-168` (return 9U).
pub const POLL_LEN: usize = 9;

/// Voice header length (56 bytes).
///
/// Reference: `ircDDBGateway/Common/HeaderData.cpp:590-635` (return 56U).
pub const VOICE_HEADER_LEN: usize = 56;

/// Voice data length (27 bytes).
///
/// Reference: `ircDDBGateway/Common/AMBEData.cpp:317-345` (return 15+12).
pub const VOICE_DATA_LEN: usize = 27;

/// Voice EOT length (27 bytes, same as data but seq has 0x40 bit).
pub const VOICE_EOT_LEN: usize = 27;

/// ACK tag at offsets `[10..13]` of a 14-byte connect reply,
/// followed by `0x00` at byte `[13]`.
///
/// Reference: `ircDDBGateway/Common/ConnectData.cpp:302-308`
/// (`data[LONG_CALLSIGN_LENGTH + 2U] = 'A';` — i.e. `data[10]`).
pub const CONNECT_ACK_TAG: [u8; 3] = *b"ACK";

/// NAK tag at offsets `[10..13]` of a 14-byte connect reply,
/// followed by `0x00` at byte `[13]`.
///
/// Reference: `ircDDBGateway/Common/ConnectData.cpp:310-316`.
pub const CONNECT_NAK_TAG: [u8; 3] = *b"NAK";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keepalive_interval_is_ten_seconds() {
        assert_eq!(KEEPALIVE_INTERVAL, Duration::from_secs(10));
    }

    #[test]
    fn default_port_is_30001() {
        assert_eq!(DEFAULT_PORT, 30001);
    }

    #[test]
    fn voice_header_is_56_bytes() {
        assert_eq!(VOICE_HEADER_LEN, 56);
    }

    #[test]
    fn voice_data_is_27_bytes() {
        assert_eq!(VOICE_DATA_LEN, 27);
    }

    #[test]
    fn ack_tag_is_ack() {
        assert_eq!(&CONNECT_ACK_TAG, b"ACK");
    }

    #[test]
    fn nak_tag_is_nak() {
        assert_eq!(&CONNECT_NAK_TAG, b"NAK");
    }
}
