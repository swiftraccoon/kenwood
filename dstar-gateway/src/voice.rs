//! D-STAR voice frame types and constants.
//!
//! Each voice frame carries 9 bytes of AMBE-encoded audio and 3 bytes
//! of slow data. Frames are transmitted at 50 Hz (one every 20 ms)
//! per the JARL D-STAR specification. A superframe is 21 frames
//! (frame 0 is a sync frame, frames 1-20 carry slow data) for a
//! total of 420 ms per superframe.
//!
//! # References
//!
//! - JARL D-STAR specification
//! - `ircDDBGateway/Common/AMBEData.cpp`
//! - `xlxd/src/cdextraprotocol.cpp:EncodeDvPacket`

/// AMBE silence frame (9 bytes) — used in EOT packets.
///
/// From `g4klx/MMDVMHost` `DStarDefines.h`.
pub const AMBE_SILENCE: [u8; 9] = [0x9E, 0x8D, 0x32, 0x88, 0x26, 0x1A, 0x3F, 0x61, 0xE8];

/// D-STAR sync bytes (3 bytes) — slow data filler for sync frames.
pub const DSTAR_SYNC_BYTES: [u8; 3] = [0x55, 0x55, 0x55];

/// A D-STAR voice data frame (9 bytes AMBE + 3 bytes slow data).
///
/// 21 frames form one superframe. Frame 0 carries the sync pattern,
/// frames 1-20 carry slow data. At 20 ms per frame, one superframe
/// is 420 ms of audio.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VoiceFrame {
    /// AMBE 3600x2450 codec voice data (9 bytes).
    pub ambe: [u8; 9],
    /// Slow data payload (3 bytes).
    pub slow_data: [u8; 3],
}

impl VoiceFrame {
    /// Create a silence frame (used for EOT and padding).
    #[must_use]
    pub const fn silence() -> Self {
        Self {
            ambe: AMBE_SILENCE,
            slow_data: DSTAR_SYNC_BYTES,
        }
    }
}
