// Portions of this file are derived from MMDVMHost by Jonathan Naylor
// G4KLX, Copyright (C) 2015-2026, licensed under GPL-2.0-or-later.
// See LICENSE for full attribution.

//! MMDVM modem operating mode.
//!
//! The ordinary protocol modes follow `MMDVM/Globals.h` and the corresponding
//! `MODE_*` constants in `MMDVMHost/Defines.h`. Host application-state
//! sentinels are separate from modem wire modes.

use std::fmt;

use crate::command::{
    MODE_DMR, MODE_DSTAR, MODE_FM, MODE_IDLE, MODE_NXDN, MODE_P25, MODE_POCSAG, MODE_YSF,
};
use crate::error::MmdvmError;

/// Modem operating mode (see `MODE_*` byte constants).
///
/// Decoding is lenient: unknown bytes are mapped to `ModemMode::Idle`
/// by [`ModemMode::from_byte`]. [`TryFrom<u8>`](Self::try_from) performs strict
/// conversion.
/// See the [status example](crate#parse-version-and-status-responses).
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ModemMode {
    /// No mode active.
    Idle,
    /// D-STAR digital voice.
    Dstar,
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
}

impl ModemMode {
    /// Wire byte for this mode.
    #[must_use]
    pub const fn as_byte(self) -> u8 {
        match self {
            Self::Idle => MODE_IDLE,
            Self::Dstar => MODE_DSTAR,
            Self::Dmr => MODE_DMR,
            Self::Ysf => MODE_YSF,
            Self::P25 => MODE_P25,
            Self::Nxdn => MODE_NXDN,
            Self::Pocsag => MODE_POCSAG,
            Self::Fm => MODE_FM,
        }
    }

    /// Parse a wire byte into a mode.
    ///
    /// Unknown bytes map to [`ModemMode::Idle`]. Use [`Self::try_from`] when an
    /// unrecognized wire value must be rejected rather than interpreted as idle.
    #[must_use]
    pub const fn from_byte(b: u8) -> Self {
        match b {
            MODE_DSTAR => Self::Dstar,
            MODE_DMR => Self::Dmr,
            MODE_YSF => Self::Ysf,
            MODE_P25 => Self::P25,
            MODE_NXDN => Self::Nxdn,
            MODE_POCSAG => Self::Pocsag,
            MODE_FM => Self::Fm,
            // MODE_IDLE and all other bytes collapse to Idle.
            _ => Self::Idle,
        }
    }
}

impl TryFrom<u8> for ModemMode {
    type Error = MmdvmError;

    fn try_from(mode: u8) -> Result<Self, Self::Error> {
        match mode {
            MODE_IDLE | MODE_DSTAR | MODE_DMR | MODE_YSF | MODE_P25 | MODE_NXDN | MODE_POCSAG
            | MODE_FM => Ok(Self::from_byte(mode)),
            _ => Err(MmdvmError::InvalidModemMode { mode }),
        }
    }
}

impl From<ModemMode> for u8 {
    fn from(mode: ModemMode) -> Self {
        mode.as_byte()
    }
}

impl fmt::Display for ModemMode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Idle => "idle",
            Self::Dstar => "D-STAR",
            Self::Dmr => "DMR",
            Self::Ysf => "YSF",
            Self::P25 => "P25",
            Self::Nxdn => "NXDN",
            Self::Pocsag => "POCSAG",
            Self::Fm => "FM",
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn strict_conversion_rejects_every_unassigned_wire_value() -> TestResult {
        for byte in 0..=u8::MAX {
            let lenient = ModemMode::from_byte(byte);
            let strict = ModemMode::try_from(byte);
            if [0, 1, 2, 3, 4, 5, 6, 10].contains(&byte) {
                assert_eq!(u8::from(strict?), byte);
            } else {
                assert_eq!(lenient, ModemMode::Idle);
                assert!(
                    matches!(strict, Err(MmdvmError::InvalidModemMode { mode }) if mode == byte),
                    "{strict:?}"
                );
            }
        }
        Ok(())
    }

    #[test]
    fn roundtrip_all_known_modes() {
        let all = [
            ModemMode::Idle,
            ModemMode::Dstar,
            ModemMode::Dmr,
            ModemMode::Ysf,
            ModemMode::P25,
            ModemMode::Nxdn,
            ModemMode::Pocsag,
            ModemMode::Fm,
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
        assert_eq!(ModemMode::Dstar.as_byte(), MODE_DSTAR);
        assert_eq!(ModemMode::Fm.as_byte(), MODE_FM);
    }
}
