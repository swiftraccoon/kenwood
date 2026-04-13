// Portions of this file are derived from MMDVMHost by Jonathan Naylor
// G4KLX, Copyright (C) 2015-2026, licensed under GPL-2.0-or-later.
// See LICENSE for full attribution.

//! MMDVM modem operating mode.
//!
//! Mirrors the `MODE_*` constants in `ref/MMDVMHost/Defines.h:31-44`.
//! The byte values are the same ones used in `SetMode` requests and
//! in the status response's mode field.

use crate::command::{
    MODE_CW, MODE_DMR, MODE_DSTAR, MODE_ERROR, MODE_FM, MODE_IDLE, MODE_LOCKOUT, MODE_NXDN,
    MODE_P25, MODE_POCSAG, MODE_QUIT, MODE_YSF,
};

/// Modem operating mode (see `MODE_*` byte constants).
///
/// Decoding is lenient: unknown bytes are mapped to `ModemMode::Idle`
/// by [`ModemMode::from_byte`], in the spirit of the rest of the codec.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ModemMode {
    /// No mode active.
    Idle,
    /// D-STAR digital voice.
    DStar,
    /// DMR Tier II digital voice.
    Dmr,
    /// Yaesu System Fusion (C4FM).
    Ysf,
    /// APCO Project 25.
    P25,
    /// NXDN digital voice.
    Nxdn,
    /// POCSAG paging.
    Pocsag,
    /// Analog FM.
    Fm,
    /// Continuous-wave (CW) ID.
    Cw,
    /// Lockout (carrier-sense blocked).
    Lockout,
    /// Error state.
    Error,
    /// Quit / shutdown.
    Quit,
}

impl ModemMode {
    /// Wire byte for this mode.
    #[must_use]
    pub const fn as_byte(self) -> u8 {
        match self {
            Self::Idle => MODE_IDLE,
            Self::DStar => MODE_DSTAR,
            Self::Dmr => MODE_DMR,
            Self::Ysf => MODE_YSF,
            Self::P25 => MODE_P25,
            Self::Nxdn => MODE_NXDN,
            Self::Pocsag => MODE_POCSAG,
            Self::Fm => MODE_FM,
            Self::Cw => MODE_CW,
            Self::Lockout => MODE_LOCKOUT,
            Self::Error => MODE_ERROR,
            Self::Quit => MODE_QUIT,
        }
    }

    /// Parse a wire byte into a mode.
    ///
    /// Unknown bytes are mapped to [`ModemMode::Idle`] to keep the
    /// sans-io core lenient — callers that need strict validation
    /// should compare the raw byte against the `MODE_*` constants
    /// themselves.
    #[must_use]
    pub const fn from_byte(b: u8) -> Self {
        match b {
            MODE_DSTAR => Self::DStar,
            MODE_DMR => Self::Dmr,
            MODE_YSF => Self::Ysf,
            MODE_P25 => Self::P25,
            MODE_NXDN => Self::Nxdn,
            MODE_POCSAG => Self::Pocsag,
            MODE_FM => Self::Fm,
            MODE_CW => Self::Cw,
            MODE_LOCKOUT => Self::Lockout,
            MODE_ERROR => Self::Error,
            MODE_QUIT => Self::Quit,
            // MODE_IDLE and all other bytes collapse to Idle.
            _ => Self::Idle,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_all_known_modes() {
        let all = [
            ModemMode::Idle,
            ModemMode::DStar,
            ModemMode::Dmr,
            ModemMode::Ysf,
            ModemMode::P25,
            ModemMode::Nxdn,
            ModemMode::Pocsag,
            ModemMode::Fm,
            ModemMode::Cw,
            ModemMode::Lockout,
            ModemMode::Error,
            ModemMode::Quit,
        ];
        for mode in all {
            let back = ModemMode::from_byte(mode.as_byte());
            assert_eq!(back, mode, "roundtrip failed for {mode:?}");
        }
    }

    #[test]
    fn unknown_byte_maps_to_idle() {
        assert_eq!(ModemMode::from_byte(0xFF), ModemMode::Idle);
        assert_eq!(ModemMode::from_byte(200), ModemMode::Idle);
        // Byte value 7 is unused (between POCSAG=6 and FM=10).
        assert_eq!(ModemMode::from_byte(7), ModemMode::Idle);
    }

    #[test]
    fn mode_byte_values_match_command_constants() {
        assert_eq!(ModemMode::DStar.as_byte(), MODE_DSTAR);
        assert_eq!(ModemMode::Fm.as_byte(), MODE_FM);
        assert_eq!(ModemMode::Quit.as_byte(), MODE_QUIT);
    }
}
