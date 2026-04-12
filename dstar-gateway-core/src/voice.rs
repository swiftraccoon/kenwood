//! D-STAR voice frame types and constants.
//!
//! Each voice frame carries 9 bytes of AMBE-encoded audio and 3
//! bytes of slow data. Frames are transmitted at 50 Hz (one every
//! 20 ms) per the JARL D-STAR specification. A superframe is 21
//! frames (frame 0 is sync, frames 1-20 carry slow data) for a
//! total of 420 ms per superframe.
//!
//! See `g4klx/MMDVMHost/DStarDefines.h:44` for `NULL_AMBE_DATA_BYTES`
//! (the AMBE silence pattern).

/// AMBE silence frame (9 bytes) — used in EOT packets.
///
/// Reference: `g4klx/MMDVMHost/DStarDefines.h:44` (`NULL_AMBE_DATA_BYTES`).
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ambe_silence_is_nine_bytes() {
        assert_eq!(AMBE_SILENCE.len(), 9);
    }

    #[test]
    fn ambe_silence_matches_mmdvmhost() {
        // Reference: g4klx/MMDVMHost/DStarDefines.h:44
        assert_eq!(
            AMBE_SILENCE,
            [0x9E, 0x8D, 0x32, 0x88, 0x26, 0x1A, 0x3F, 0x61, 0xE8]
        );
    }

    #[test]
    fn dstar_sync_bytes_are_0x555555() {
        assert_eq!(DSTAR_SYNC_BYTES, [0x55, 0x55, 0x55]);
    }

    #[test]
    fn voice_frame_silence_is_ambe_silence_plus_sync() {
        let frame = VoiceFrame::silence();
        assert_eq!(frame.ambe, AMBE_SILENCE);
        assert_eq!(frame.slow_data, DSTAR_SYNC_BYTES);
    }
}
