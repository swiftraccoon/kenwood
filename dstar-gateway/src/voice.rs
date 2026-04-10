//! D-STAR voice frame types.
//!
//! Each D-STAR voice frame carries 20 ms of AMBE-encoded audio (9 bytes)
//! and 3 bytes of slow data used for text messages or GPS position.

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
#[derive(Debug, Clone, PartialEq, Eq)]
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
