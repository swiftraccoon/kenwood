//! Operating mode, power level, shift direction, and step size types.

use std::fmt;

use crate::error::ValidationError;

/// Operating mode as returned by the `MD` (mode) CAT command.
///
/// The TH-D75 supports 8 modes (0-7) via the `MD` command per the
/// KI4LAX CAT command reference. This encoding matches the flash
/// memory encoding (0-7).
///
/// Note: the `FO`/`ME` commands use a **different** mode encoding
/// (0=FM, 1=DV, 2=NFM, 3=AM) stored as a raw `u8` in [`ChannelMemory`].
/// This enum is only used for the `MD` command.
///
/// # Band restrictions (per Kenwood Operating Tips §5.9)
///
/// Not all modes are available on both bands:
///
/// - **Band A** supports only **FM** and **DV**. Band A is the amateur
///   TX/RX band (144/220/430 MHz) and its hardware path does not include
///   the DSP demodulator needed for SSB/CW/AM.
/// - **Band B** supports all modes: FM, DV, AM, LSB, USB, CW, NFM, and
///   DR. Band B has an independent receiver chain with DSP and IF filter
///   enabling wideband demodulation.
/// - **DR** (D-STAR repeater mode) is only available on **Band A**.
///   Attempting to set DR on Band B via `MD` will be rejected by the
///   firmware with a `?` error.
///
/// Attempting to set an unsupported mode on a band via the `MD` command
/// will result in the radio returning a `?` error response.
///
/// # WFM (Wide FM) note
///
/// WFM is NOT an `MD` mode — it is the FM broadcast radio mode accessed
/// via the `FR` (FM Radio) command at 76–108 MHz on Band B. The radio's
/// display shows "WFM" in FM Radio mode, but `MD` does not return a WFM
/// value. Per the Kenwood Operating Tips §5.9, WFM appears in Band B's
/// demodulation mode table for the FM Radio frequency range only.
///
/// [`ChannelMemory`]: crate::types::ChannelMemory
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Mode {
    /// FM modulation (index 0). Available on both Band A and Band B.
    Fm = 0,
    /// D-STAR digital voice (index 1). Available on both Band A and Band B.
    Dv = 1,
    /// AM modulation (index 2). Band B only — Band A hardware lacks the
    /// DSP demodulator required for AM reception.
    Am = 2,
    /// Lower sideband (index 3). Band B only — requires DSP demodulator
    /// not present in Band A's receiver chain.
    Lsb = 3,
    /// Upper sideband (index 4). Band B only — requires DSP demodulator
    /// not present in Band A's receiver chain.
    Usb = 4,
    /// CW / Morse code (index 5). Band B only — requires DSP demodulator
    /// not present in Band A's receiver chain.
    Cw = 5,
    /// Narrow FM modulation (index 6). Band B only — Band A supports
    /// only standard FM deviation.
    Nfm = 6,
    /// D-STAR repeater mode (index 7). Band A only — DR requires the
    /// CTRL/PTT band for gateway access and callsign routing.
    Dr = 7,
}

impl fmt::Display for Mode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Fm => f.write_str("FM"),
            Self::Dv => f.write_str("DV"),
            Self::Am => f.write_str("AM"),
            Self::Lsb => f.write_str("LSB"),
            Self::Usb => f.write_str("USB"),
            Self::Cw => f.write_str("CW"),
            Self::Nfm => f.write_str("NFM"),
            Self::Dr => f.write_str("DR"),
        }
    }
}

impl TryFrom<u8> for Mode {
    type Error = ValidationError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Fm),
            1 => Ok(Self::Dv),
            2 => Ok(Self::Am),
            3 => Ok(Self::Lsb),
            4 => Ok(Self::Usb),
            5 => Ok(Self::Cw),
            6 => Ok(Self::Nfm),
            7 => Ok(Self::Dr),
            _ => Err(ValidationError::ModeOutOfRange(value)),
        }
    }
}

impl From<Mode> for u8 {
    fn from(mode: Mode) -> Self {
        mode as Self
    }
}

/// Transmit power level.
///
/// Maps to the power field in the `PC`, `FO`, and `ME` commands.
/// The D75 firmware RE confirms 4 levels: Hi (0), Mid (1), Lo (2), EL (3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PowerLevel {
    /// High power (index 0).
    High = 0,
    /// Medium power (index 1).
    Medium = 1,
    /// Low power (index 2).
    Low = 2,
    /// Extra-low power (index 3). D75-specific; not present on the TH-D74.
    ExtraLow = 3,
}

impl fmt::Display for PowerLevel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::High => f.write_str("High"),
            Self::Medium => f.write_str("Medium"),
            Self::Low => f.write_str("Low"),
            Self::ExtraLow => f.write_str("EL"),
        }
    }
}

impl TryFrom<u8> for PowerLevel {
    type Error = ValidationError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::High),
            1 => Ok(Self::Medium),
            2 => Ok(Self::Low),
            3 => Ok(Self::ExtraLow),
            _ => Err(ValidationError::PowerLevelOutOfRange(value)),
        }
    }
}

impl From<PowerLevel> for u8 {
    fn from(level: PowerLevel) -> Self {
        level as Self
    }
}

/// Repeater shift direction, stored as a raw firmware value.
///
/// Maps to the shift field (4-bit, low nibble of byte 0x08) in the `FO`
/// and `ME` commands. Known values: 0 = Simplex, 1 = Up, 2 = Down,
/// 3 = Split. Values 4-15 are used by VFO mode for extended shift
/// configurations whose meaning is not yet fully documented.
///
/// Accepts any value in the 4-bit range 0-15 to avoid parse failures
/// when reading VFO state from the radio.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ShiftDirection(u8);

impl ShiftDirection {
    /// Simplex, no shift (value 0).
    pub const SIMPLEX: Self = Self(0);
    /// Positive shift (value 1).
    pub const UP: Self = Self(1);
    /// Negative shift (value 2).
    pub const DOWN: Self = Self(2);
    /// Split: separate TX frequency (value 3).
    pub const SPLIT: Self = Self(3);

    /// Creates a new `ShiftDirection` from a raw 4-bit value.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::ShiftOutOfRange`] if `value > 15`.
    pub const fn new(value: u8) -> Result<Self, ValidationError> {
        if value <= 15 {
            Ok(Self(value))
        } else {
            Err(ValidationError::ShiftOutOfRange(value))
        }
    }

    /// Returns the raw firmware value.
    #[must_use]
    pub const fn as_u8(self) -> u8 {
        self.0
    }

    /// Returns `true` if this is a well-known shift direction (0-3).
    #[must_use]
    pub const fn is_known(self) -> bool {
        self.0 <= 3
    }
}

impl TryFrom<u8> for ShiftDirection {
    type Error = ValidationError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<ShiftDirection> for u8 {
    fn from(dir: ShiftDirection) -> Self {
        dir.0
    }
}

/// Frequency step size for tuning.
///
/// Maps to the step field in the `FO` and `ME` commands.
/// The variant name encodes the step in Hz (e.g. `Hz5000` = 5.0 kHz).
///
/// # KI4LAX TABLE C reference
///
/// The hex index-to-step-size mapping (TABLE C in the KI4LAX CAT command
/// reference) is as follows:
///
/// | Index (hex) | Step size |
/// |-------------|-----------|
/// | 0x0 | 5.0 kHz |
/// | 0x1 | 6.25 kHz |
/// | 0x2 | 8.33 kHz |
/// | 0x3 | 9.0 kHz |
/// | 0x4 | 10.0 kHz |
/// | 0x5 | 12.5 kHz |
/// | 0x6 | 15.0 kHz |
/// | 0x7 | 20.0 kHz |
/// | 0x8 | 25.0 kHz |
/// | 0x9 | 30.0 kHz |
/// | 0xA | 50.0 kHz |
/// | 0xB | 100.0 kHz |
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StepSize {
    /// 5.000 kHz step (index 0).
    Hz5000 = 0,
    /// 6.250 kHz step (index 1).
    Hz6250 = 1,
    /// 8.330 kHz step (index 2).
    Hz8330 = 2,
    /// 9.000 kHz step (index 3).
    Hz9000 = 3,
    /// 10.000 kHz step (index 4).
    Hz10000 = 4,
    /// 12.500 kHz step (index 5).
    Hz12500 = 5,
    /// 15.000 kHz step (index 6).
    Hz15000 = 6,
    /// 20.000 kHz step (index 7).
    Hz20000 = 7,
    /// 25.000 kHz step (index 8).
    Hz25000 = 8,
    /// 30.000 kHz step (index 9).
    Hz30000 = 9,
    /// 50.000 kHz step (index 10).
    Hz50000 = 10,
    /// 100.000 kHz step (index 11).
    Hz100000 = 11,
}

impl TryFrom<u8> for StepSize {
    type Error = ValidationError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Hz5000),
            1 => Ok(Self::Hz6250),
            2 => Ok(Self::Hz8330),
            3 => Ok(Self::Hz9000),
            4 => Ok(Self::Hz10000),
            5 => Ok(Self::Hz12500),
            6 => Ok(Self::Hz15000),
            7 => Ok(Self::Hz20000),
            8 => Ok(Self::Hz25000),
            9 => Ok(Self::Hz30000),
            10 => Ok(Self::Hz50000),
            11 => Ok(Self::Hz100000),
            _ => Err(ValidationError::StepSizeOutOfRange(value)),
        }
    }
}

impl From<StepSize> for u8 {
    fn from(step: StepSize) -> Self {
        step as Self
    }
}

/// Operating mode as stored in the flash memory image.
///
/// This enum represents the mode encoding used in the MCP programming
/// memory (channel data byte 0x09 bits \[6:4\]). It differs from [`Mode`]
/// which represents the CAT wire format.
///
/// # Flash encoding
///
/// | Value | Mode |
/// |-------|------|
/// | 0 | FM |
/// | 1 | DV (D-STAR digital voice) |
/// | 2 | AM |
/// | 3 | LSB (lower sideband) |
/// | 4 | USB (upper sideband) |
/// | 5 | CW (Morse code) |
/// | 6 | NFM (narrow FM) |
/// | 7 | DR (D-STAR repeater) |
///
/// # CAT encoding (for comparison)
///
/// The CAT protocol (`FO`/`ME` commands) uses a different mapping:
/// 0=FM, 1=DV, 2=NFM, 3=AM. The memory image encoding adds LSB, USB,
/// CW, and DR modes that are not available via CAT.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MemoryMode {
    /// FM modulation (flash value 0).
    Fm = 0,
    /// D-STAR digital voice (flash value 1).
    Dv = 1,
    /// AM modulation (flash value 2).
    Am = 2,
    /// Lower sideband (flash value 3).
    Lsb = 3,
    /// Upper sideband (flash value 4).
    Usb = 4,
    /// CW / Morse code (flash value 5).
    Cw = 5,
    /// Narrow FM modulation (flash value 6).
    Nfm = 6,
    /// D-STAR repeater mode (flash value 7).
    Dr = 7,
}

impl fmt::Display for MemoryMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Fm => f.write_str("FM"),
            Self::Dv => f.write_str("DV"),
            Self::Am => f.write_str("AM"),
            Self::Lsb => f.write_str("LSB"),
            Self::Usb => f.write_str("USB"),
            Self::Cw => f.write_str("CW"),
            Self::Nfm => f.write_str("NFM"),
            Self::Dr => f.write_str("DR"),
        }
    }
}

impl TryFrom<u8> for MemoryMode {
    type Error = ValidationError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Fm),
            1 => Ok(Self::Dv),
            2 => Ok(Self::Am),
            3 => Ok(Self::Lsb),
            4 => Ok(Self::Usb),
            5 => Ok(Self::Cw),
            6 => Ok(Self::Nfm),
            7 => Ok(Self::Dr),
            _ => Err(ValidationError::MemoryModeOutOfRange(value)),
        }
    }
}

impl From<MemoryMode> for u8 {
    fn from(mode: MemoryMode) -> Self {
        mode as Self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ValidationError;

    // --- Mode ---

    #[test]
    fn mode_valid_range() {
        for i in 0u8..8 {
            assert!(Mode::try_from(i).is_ok(), "Mode({i}) should be valid");
        }
    }

    #[test]
    fn mode_invalid() {
        assert!(Mode::try_from(8).is_err());
        assert!(Mode::try_from(255).is_err());
    }

    #[test]
    fn mode_round_trip() {
        for i in 0u8..8 {
            let val = Mode::try_from(i).unwrap();
            assert_eq!(u8::from(val), i);
        }
    }

    #[test]
    fn mode_error_variant() {
        let err = Mode::try_from(8).unwrap_err();
        assert!(matches!(err, ValidationError::ModeOutOfRange(8)));
    }

    #[test]
    fn mode_display() {
        assert_eq!(Mode::Fm.to_string(), "FM");
        assert_eq!(Mode::Dv.to_string(), "DV");
        assert_eq!(Mode::Am.to_string(), "AM");
        assert_eq!(Mode::Lsb.to_string(), "LSB");
        assert_eq!(Mode::Usb.to_string(), "USB");
        assert_eq!(Mode::Cw.to_string(), "CW");
        assert_eq!(Mode::Nfm.to_string(), "NFM");
        assert_eq!(Mode::Dr.to_string(), "DR");
    }

    // --- PowerLevel ---

    #[test]
    fn power_level_valid_range() {
        for i in 0u8..4 {
            assert!(
                PowerLevel::try_from(i).is_ok(),
                "PowerLevel({i}) should be valid"
            );
        }
    }

    #[test]
    fn power_level_invalid() {
        assert!(PowerLevel::try_from(4).is_err());
        assert!(PowerLevel::try_from(255).is_err());
    }

    #[test]
    fn power_level_round_trip() {
        for i in 0u8..4 {
            let val = PowerLevel::try_from(i).unwrap();
            assert_eq!(u8::from(val), i);
        }
    }

    #[test]
    fn power_level_error_variant() {
        let err = PowerLevel::try_from(4).unwrap_err();
        assert!(matches!(err, ValidationError::PowerLevelOutOfRange(4)));
    }

    #[test]
    fn power_level_display() {
        assert_eq!(PowerLevel::High.to_string(), "High");
        assert_eq!(PowerLevel::Medium.to_string(), "Medium");
        assert_eq!(PowerLevel::Low.to_string(), "Low");
        assert_eq!(PowerLevel::ExtraLow.to_string(), "EL");
    }

    // --- ShiftDirection ---

    #[test]
    fn shift_direction_valid_range() {
        // All 4-bit values (0-15) are valid.
        for i in 0u8..=15 {
            assert!(
                ShiftDirection::try_from(i).is_ok(),
                "ShiftDirection({i}) should be valid"
            );
        }
    }

    #[test]
    fn shift_direction_invalid() {
        assert!(ShiftDirection::try_from(16).is_err());
        assert!(ShiftDirection::try_from(255).is_err());
    }

    #[test]
    fn shift_direction_round_trip() {
        for i in 0u8..=15 {
            let val = ShiftDirection::try_from(i).unwrap();
            assert_eq!(u8::from(val), i);
        }
    }

    #[test]
    fn shift_direction_known_constants() {
        assert_eq!(ShiftDirection::SIMPLEX.as_u8(), 0);
        assert_eq!(ShiftDirection::UP.as_u8(), 1);
        assert_eq!(ShiftDirection::DOWN.as_u8(), 2);
        assert_eq!(ShiftDirection::SPLIT.as_u8(), 3);
        assert!(ShiftDirection::SIMPLEX.is_known());
        assert!(ShiftDirection::SPLIT.is_known());
    }

    #[test]
    fn shift_direction_extended_vfo_values() {
        // Values 4-15 are valid but not "known" named shift modes.
        let ext = ShiftDirection::new(8).unwrap();
        assert_eq!(ext.as_u8(), 8);
        assert!(!ext.is_known());
    }

    #[test]
    fn shift_direction_error_variant() {
        let err = ShiftDirection::try_from(16).unwrap_err();
        assert!(matches!(err, ValidationError::ShiftOutOfRange(16)));
    }

    // --- StepSize ---

    #[test]
    fn step_size_valid_range() {
        for i in 0u8..12 {
            assert!(
                StepSize::try_from(i).is_ok(),
                "StepSize({i}) should be valid"
            );
        }
    }

    #[test]
    fn step_size_invalid() {
        assert!(StepSize::try_from(12).is_err());
        assert!(StepSize::try_from(255).is_err());
    }

    #[test]
    fn step_size_round_trip() {
        for i in 0u8..12 {
            let val = StepSize::try_from(i).unwrap();
            assert_eq!(u8::from(val), i);
        }
    }

    #[test]
    fn step_size_error_variant() {
        let err = StepSize::try_from(12).unwrap_err();
        assert!(matches!(err, ValidationError::StepSizeOutOfRange(12)));
    }

    // --- MemoryMode ---

    #[test]
    fn memory_mode_valid_range() {
        for i in 0u8..8 {
            assert!(
                MemoryMode::try_from(i).is_ok(),
                "MemoryMode({i}) should be valid"
            );
        }
    }

    #[test]
    fn memory_mode_invalid() {
        assert!(MemoryMode::try_from(8).is_err());
        assert!(MemoryMode::try_from(255).is_err());
    }

    #[test]
    fn memory_mode_round_trip() {
        for i in 0u8..8 {
            let val = MemoryMode::try_from(i).unwrap();
            assert_eq!(u8::from(val), i);
        }
    }

    #[test]
    fn memory_mode_error_variant() {
        let err = MemoryMode::try_from(8).unwrap_err();
        assert!(matches!(err, ValidationError::MemoryModeOutOfRange(8)));
    }

    #[test]
    fn memory_mode_display() {
        assert_eq!(MemoryMode::Fm.to_string(), "FM");
        assert_eq!(MemoryMode::Dv.to_string(), "DV");
        assert_eq!(MemoryMode::Am.to_string(), "AM");
        assert_eq!(MemoryMode::Lsb.to_string(), "LSB");
        assert_eq!(MemoryMode::Usb.to_string(), "USB");
        assert_eq!(MemoryMode::Cw.to_string(), "CW");
        assert_eq!(MemoryMode::Nfm.to_string(), "NFM");
        assert_eq!(MemoryMode::Dr.to_string(), "DR");
    }

    #[test]
    fn cat_mode_matches_flash_encoding() {
        // CAT MD and flash memory use the same encoding for all 8 modes (0-7).
        assert_eq!(u8::from(Mode::Fm), u8::from(MemoryMode::Fm));
        assert_eq!(u8::from(Mode::Dv), u8::from(MemoryMode::Dv));
        assert_eq!(u8::from(Mode::Am), u8::from(MemoryMode::Am));
        assert_eq!(u8::from(Mode::Lsb), u8::from(MemoryMode::Lsb));
        assert_eq!(u8::from(Mode::Usb), u8::from(MemoryMode::Usb));
        assert_eq!(u8::from(Mode::Cw), u8::from(MemoryMode::Cw));
        assert_eq!(u8::from(Mode::Nfm), u8::from(MemoryMode::Nfm));
        assert_eq!(u8::from(Mode::Dr), u8::from(MemoryMode::Dr));
    }
}
