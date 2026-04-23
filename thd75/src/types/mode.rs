//! Operating mode, power level, shift direction, and step size types.

use std::fmt;

use crate::error::ValidationError;

/// Operating mode as returned by the `MD` (mode) CAT command.
///
/// The TH-D75 supports 10 modes (0-9) via the `MD` command. Modes 0-7
/// are confirmed by firmware RE and the KI4LAX CAT command reference.
/// Modes 8 (WFM) and 9 (CW-R) are confirmed by the ARFC-D75
/// decompilation.
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
///   TX/RX band (144/220/430 MHz). Its receiver chain (VCO/PLL IC800,
///   IF IC IC900) is a double super heterodyne with 1st IF at 57.15 MHz
///   and 2nd IF at 450 kHz — it has no third IF stage, so AM/SSB/CW
///   demodulation is not possible in hardware (service manual §2.1.3).
/// - **Band B** supports all modes: FM, DV, AM, LSB, USB, CW, NFM, DR,
///   WFM, and CW-R. Band B's receiver chain (VCO/PLL IC700, IF IC
///   IC1002) adds a third mixer (IC1001) producing a 3rd IF at 10.8 kHz,
///   which feeds into the CODEC (IC2011) for AM/SSB/CW demodulation.
///   This triple super heterodyne architecture is what enables the
///   wideband mode support (service manual §2.1.3.2).
/// - **DR** (D-STAR repeater mode) is only available on **Band A**.
///   Attempting to set DR on Band B via `MD` will be rejected by the
///   firmware with a `?` error.
///
/// Attempting to set an unsupported mode on a band via the `MD` command
/// will result in the radio returning a `?` error response.
///
/// # Mode cycling on the radio (per User Manual Chapter 5)
///
/// Pressing `[MODE]` cycles through available modes:
/// - Band A: FM/NFM -> DR (DV) -> (back to FM/NFM)
/// - Band B: FM/NFM -> DR (DV) -> AM -> LSB -> USB -> CW -> (back to FM/NFM)
///
/// Switching between DV and DR requires the Digital Function Menu, not
/// `[MODE]`. Switching between FM and NFM requires Menu No. 103
/// (FM Narrow), not `[MODE]`.
///
/// # WFM (Wide FM)
///
/// WFM is `MD` mode 8, confirmed by ARFC-D75 decompilation. It is the
/// FM broadcast radio mode used on Band B for the 76-108 MHz range.
/// The radio's display shows "WFM" in this mode.
///
/// # CW-R (CW Reverse)
///
/// CW-R is `MD` mode 9, confirmed by ARFC-D75 decompilation. It uses
/// LSB detection for CW reception instead of the default USB detection
/// used by standard CW mode.
///
/// [`ChannelMemory`]: crate::types::ChannelMemory
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Mode {
    /// FM modulation (index 0). Available on both Band A and Band B.
    Fm = 0,
    /// D-STAR digital voice (index 1). Available on both Band A and Band B.
    Dv = 1,
    /// AM modulation (index 2). Band B only — Band A lacks the 3rd IF
    /// stage (10.8 kHz via IC1001) required for AM envelope detection.
    Am = 2,
    /// Lower sideband (index 3). Band B only — requires the 3rd IF at
    /// 10.8 kHz (via 3rd mixer IC1001 and 460.8 kHz local oscillation).
    Lsb = 3,
    /// Upper sideband (index 4). Band B only — requires the 3rd IF at
    /// 10.8 kHz (via 3rd mixer IC1001 and 460.8 kHz local oscillation).
    Usb = 4,
    /// CW / Morse code (index 5). Band B only — requires the 3rd IF at
    /// 10.8 kHz (via 3rd mixer IC1001 and 460.8 kHz local oscillation).
    Cw = 5,
    /// Narrow FM modulation (index 6). Band B only — Band A supports
    /// only standard FM deviation.
    Nfm = 6,
    /// D-STAR repeater mode (index 7). Band A only — DR requires the
    /// CTRL/PTT band for gateway access and callsign routing.
    Dr = 7,
    /// Wide FM (index 8). Band B only — FM broadcast reception mode
    /// for the 76-108 MHz range. Confirmed by ARFC-D75 decompilation.
    Wfm = 8,
    /// CW Reverse (index 9). Band B only — uses LSB detection for CW
    /// reception instead of the default USB. Confirmed by ARFC-D75
    /// decompilation.
    CwReverse = 9,
}

impl Mode {
    /// Number of valid mode values (0-9).
    pub const COUNT: u8 = 10;
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
            Self::Wfm => f.write_str("WFM"),
            Self::CwReverse => f.write_str("CW-R"),
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
            8 => Ok(Self::Wfm),
            9 => Ok(Self::CwReverse),
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
///
/// Per User Manual Chapter 5 and Chapter 28: power output with external
/// DC 13.8 V or battery 7.4 V:
///
/// | Level | Output | Current (DC IN) | Current (Batt) |
/// |-------|--------|-----------------|----------------|
/// | High | 5 W | 1.4 A | 2.0 A |
/// | Medium | 2 W | 0.9 A | 1.3 A |
/// | Low | 0.5 W | 0.6 A | 0.8 A |
/// | EL | 0.05 W | 0.4 A | 0.5 A |
///
/// Power settings can be programmed independently for Band A and Band B.
/// The optional KBP-9 alkaline battery case supports Low power only.
/// Power level cannot be changed while transmitting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PowerLevel {
    /// High power — 5 W (index 0).
    High = 0,
    /// Medium power — 2 W (index 1).
    Medium = 1,
    /// Low power — 0.5 W (index 2).
    Low = 2,
    /// Extra-low power — 50 mW (index 3). D75-specific; not present on the TH-D74.
    ExtraLow = 3,
}

impl PowerLevel {
    /// Number of valid power level values (0-3).
    pub const COUNT: u8 = 4;
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
/// Per User Manual Chapter 12: each band can have a separate step size.
/// Step size can only be changed in VFO mode and not while in FM
/// broadcast mode. Band-specific restrictions:
///
/// - 8.33 kHz is selectable only in the 118 MHz (airband) range.
/// - 9.0 kHz is selectable only in the LF/MF (AM broadcast) range.
///
/// Default step sizes per band (TH-D75A): 144 MHz = 5 kHz, 220 MHz =
/// 20 kHz, 430 MHz = 25 kHz. TH-D75E defaults: 144 MHz = 12.5 kHz,
/// 430 MHz = 25 kHz.
///
/// Changing step size may correct the displayed frequency. For example,
/// if 144.995 MHz is shown with 5 kHz steps, switching to 12.5 kHz
/// steps changes it to 144.9875 MHz.
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

impl StepSize {
    /// Number of valid step size values (0-11).
    pub const COUNT: u8 = 12;

    /// Returns the step size in Hz.
    #[must_use]
    pub const fn as_hz(self) -> u32 {
        match self {
            Self::Hz5000 => 5_000,
            Self::Hz6250 => 6_250,
            Self::Hz8330 => 8_330,
            Self::Hz9000 => 9_000,
            Self::Hz10000 => 10_000,
            Self::Hz12500 => 12_500,
            Self::Hz15000 => 15_000,
            Self::Hz20000 => 20_000,
            Self::Hz25000 => 25_000,
            Self::Hz30000 => 30_000,
            Self::Hz50000 => 50_000,
            Self::Hz100000 => 100_000,
        }
    }

    /// Returns the step size as a kHz display string (e.g. `"5.0"`, `"6.25"`).
    #[must_use]
    pub const fn as_khz_str(self) -> &'static str {
        match self {
            Self::Hz5000 => "5.0",
            Self::Hz6250 => "6.25",
            Self::Hz8330 => "8.33",
            Self::Hz9000 => "9.0",
            Self::Hz10000 => "10.0",
            Self::Hz12500 => "12.5",
            Self::Hz15000 => "15.0",
            Self::Hz20000 => "20.0",
            Self::Hz25000 => "25.0",
            Self::Hz30000 => "30.0",
            Self::Hz50000 => "50.0",
            Self::Hz100000 => "100.0",
        }
    }
}

impl fmt::Display for StepSize {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} kHz", self.as_khz_str())
    }
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

/// Coarse tuning step multiplier.
///
/// Discovered via ARFC-D75 decompilation. The ARFC application multiplies
/// the base step size by this factor before sending `UP`/`DW` commands,
/// enabling faster tuning in large frequency ranges. This is a
/// client-side feature — the radio itself has no coarse step command.
///
/// For example, with a 25.0 kHz base step and a `X10` multiplier, each
/// `UP`/`DW` press tunes 250.0 kHz.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CoarseStepMultiplier {
    /// 1x — no multiplication, same as normal step (index 0).
    X1 = 0,
    /// 2x multiplication (index 1).
    X2 = 1,
    /// 5x multiplication (index 2).
    X5 = 2,
    /// 10x multiplication (index 3).
    X10 = 3,
    /// 50x multiplication (index 4).
    X50 = 4,
    /// 100x multiplication (index 5).
    X100 = 5,
}

impl CoarseStepMultiplier {
    /// Number of valid coarse step multiplier values (0-5).
    pub const COUNT: u8 = 6;

    /// Returns the numeric multiplier value.
    #[must_use]
    pub const fn multiplier(self) -> u16 {
        match self {
            Self::X1 => 1,
            Self::X2 => 2,
            Self::X5 => 5,
            Self::X10 => 10,
            Self::X50 => 50,
            Self::X100 => 100,
        }
    }
}

impl fmt::Display for CoarseStepMultiplier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "x{}", self.multiplier())
    }
}

impl TryFrom<u8> for CoarseStepMultiplier {
    type Error = ValidationError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::X1),
            1 => Ok(Self::X2),
            2 => Ok(Self::X5),
            3 => Ok(Self::X10),
            4 => Ok(Self::X50),
            5 => Ok(Self::X100),
            _ => Err(ValidationError::SettingOutOfRange {
                name: "coarse step multiplier",
                value,
                detail: "must be 0-5",
            }),
        }
    }
}

impl From<CoarseStepMultiplier> for u8 {
    fn from(mult: CoarseStepMultiplier) -> Self {
        mult as Self
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

impl MemoryMode {
    /// Number of valid memory mode values (0-7).
    pub const COUNT: u8 = 8;
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

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    // --- Mode ---

    #[test]
    fn mode_valid_range() -> TestResult {
        for i in 0u8..Mode::COUNT {
            let val = Mode::try_from(i)?;
            assert_eq!(u8::from(val), i, "Mode round-trip failed at {i}");
        }
        Ok(())
    }

    #[test]
    fn mode_invalid() {
        assert!(Mode::try_from(Mode::COUNT).is_err());
        assert!(Mode::try_from(255).is_err());
    }

    #[test]
    fn mode_round_trip() -> TestResult {
        for i in 0u8..Mode::COUNT {
            let val = Mode::try_from(i)?;
            assert_eq!(u8::from(val), i);
        }
        Ok(())
    }

    #[test]
    fn mode_error_variant() -> TestResult {
        let err = Mode::try_from(Mode::COUNT)
            .err()
            .ok_or("expected ModeOutOfRange error but got Ok")?;
        assert!(
            matches!(err, ValidationError::ModeOutOfRange(10)),
            "expected ModeOutOfRange(10), got {err:?}"
        );
        Ok(())
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
        assert_eq!(Mode::Wfm.to_string(), "WFM");
        assert_eq!(Mode::CwReverse.to_string(), "CW-R");
    }

    // --- PowerLevel ---

    #[test]
    fn power_level_valid_range() -> TestResult {
        for i in 0u8..PowerLevel::COUNT {
            let val = PowerLevel::try_from(i)?;
            assert_eq!(u8::from(val), i, "PowerLevel round-trip failed at {i}");
        }
        Ok(())
    }

    #[test]
    fn power_level_invalid() {
        assert!(PowerLevel::try_from(PowerLevel::COUNT).is_err());
        assert!(PowerLevel::try_from(255).is_err());
    }

    #[test]
    fn power_level_round_trip() -> TestResult {
        for i in 0u8..PowerLevel::COUNT {
            let val = PowerLevel::try_from(i)?;
            assert_eq!(u8::from(val), i);
        }
        Ok(())
    }

    #[test]
    fn power_level_error_variant() -> TestResult {
        let err = PowerLevel::try_from(PowerLevel::COUNT)
            .err()
            .ok_or("expected PowerLevelOutOfRange error but got Ok")?;
        assert!(
            matches!(err, ValidationError::PowerLevelOutOfRange(4)),
            "expected PowerLevelOutOfRange(4), got {err:?}"
        );
        Ok(())
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
    fn shift_direction_valid_range() -> TestResult {
        // All 4-bit values (0-15) are valid.
        for i in 0u8..=15 {
            let val = ShiftDirection::try_from(i)?;
            assert_eq!(u8::from(val), i, "ShiftDirection round-trip failed at {i}");
        }
        Ok(())
    }

    #[test]
    fn shift_direction_invalid() {
        assert!(ShiftDirection::try_from(16).is_err());
        assert!(ShiftDirection::try_from(255).is_err());
    }

    #[test]
    fn shift_direction_round_trip() -> TestResult {
        for i in 0u8..=15 {
            let val = ShiftDirection::try_from(i)?;
            assert_eq!(u8::from(val), i);
        }
        Ok(())
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
    fn shift_direction_extended_vfo_values() -> TestResult {
        // Values 4-15 are valid but not "known" named shift modes.
        let ext = ShiftDirection::new(8)?;
        assert_eq!(ext.as_u8(), 8);
        assert!(!ext.is_known());
        Ok(())
    }

    #[test]
    fn shift_direction_error_variant() -> TestResult {
        let err = ShiftDirection::try_from(16)
            .err()
            .ok_or("expected ShiftOutOfRange error but got Ok")?;
        assert!(
            matches!(err, ValidationError::ShiftOutOfRange(16)),
            "expected ShiftOutOfRange(16), got {err:?}"
        );
        Ok(())
    }

    // --- StepSize ---

    #[test]
    fn step_size_valid_range() -> TestResult {
        for i in 0u8..StepSize::COUNT {
            let val = StepSize::try_from(i)?;
            assert_eq!(u8::from(val), i, "StepSize round-trip failed at {i}");
        }
        Ok(())
    }

    #[test]
    fn step_size_invalid() {
        assert!(StepSize::try_from(StepSize::COUNT).is_err());
        assert!(StepSize::try_from(255).is_err());
    }

    #[test]
    fn step_size_round_trip() -> TestResult {
        for i in 0u8..StepSize::COUNT {
            let val = StepSize::try_from(i)?;
            assert_eq!(u8::from(val), i);
        }
        Ok(())
    }

    #[test]
    fn step_size_error_variant() -> TestResult {
        let err = StepSize::try_from(StepSize::COUNT)
            .err()
            .ok_or("expected StepSizeOutOfRange error but got Ok")?;
        assert!(
            matches!(err, ValidationError::StepSizeOutOfRange(12)),
            "expected StepSizeOutOfRange(12), got {err:?}"
        );
        Ok(())
    }

    #[test]
    fn step_size_as_hz() {
        assert_eq!(StepSize::Hz5000.as_hz(), 5_000);
        assert_eq!(StepSize::Hz6250.as_hz(), 6_250);
        assert_eq!(StepSize::Hz8330.as_hz(), 8_330);
        assert_eq!(StepSize::Hz9000.as_hz(), 9_000);
        assert_eq!(StepSize::Hz10000.as_hz(), 10_000);
        assert_eq!(StepSize::Hz12500.as_hz(), 12_500);
        assert_eq!(StepSize::Hz15000.as_hz(), 15_000);
        assert_eq!(StepSize::Hz20000.as_hz(), 20_000);
        assert_eq!(StepSize::Hz25000.as_hz(), 25_000);
        assert_eq!(StepSize::Hz30000.as_hz(), 30_000);
        assert_eq!(StepSize::Hz50000.as_hz(), 50_000);
        assert_eq!(StepSize::Hz100000.as_hz(), 100_000);
    }

    #[test]
    fn step_size_as_khz_str() {
        assert_eq!(StepSize::Hz5000.as_khz_str(), "5.0");
        assert_eq!(StepSize::Hz6250.as_khz_str(), "6.25");
        assert_eq!(StepSize::Hz8330.as_khz_str(), "8.33");
        assert_eq!(StepSize::Hz9000.as_khz_str(), "9.0");
        assert_eq!(StepSize::Hz10000.as_khz_str(), "10.0");
        assert_eq!(StepSize::Hz12500.as_khz_str(), "12.5");
        assert_eq!(StepSize::Hz15000.as_khz_str(), "15.0");
        assert_eq!(StepSize::Hz20000.as_khz_str(), "20.0");
        assert_eq!(StepSize::Hz25000.as_khz_str(), "25.0");
        assert_eq!(StepSize::Hz30000.as_khz_str(), "30.0");
        assert_eq!(StepSize::Hz50000.as_khz_str(), "50.0");
        assert_eq!(StepSize::Hz100000.as_khz_str(), "100.0");
    }

    #[test]
    fn step_size_display() {
        assert_eq!(StepSize::Hz5000.to_string(), "5.0 kHz");
        assert_eq!(StepSize::Hz25000.to_string(), "25.0 kHz");
        assert_eq!(StepSize::Hz8330.to_string(), "8.33 kHz");
    }

    // --- MemoryMode ---

    #[test]
    fn memory_mode_valid_range() -> TestResult {
        for i in 0u8..MemoryMode::COUNT {
            let val = MemoryMode::try_from(i)?;
            assert_eq!(u8::from(val), i, "MemoryMode round-trip failed at {i}");
        }
        Ok(())
    }

    #[test]
    fn memory_mode_invalid() {
        assert!(MemoryMode::try_from(MemoryMode::COUNT).is_err());
        assert!(MemoryMode::try_from(255).is_err());
    }

    #[test]
    fn memory_mode_round_trip() -> TestResult {
        for i in 0u8..MemoryMode::COUNT {
            let val = MemoryMode::try_from(i)?;
            assert_eq!(u8::from(val), i);
        }
        Ok(())
    }

    #[test]
    fn memory_mode_error_variant() -> TestResult {
        let err = MemoryMode::try_from(MemoryMode::COUNT)
            .err()
            .ok_or("expected MemoryModeOutOfRange error but got Ok")?;
        assert!(
            matches!(err, ValidationError::MemoryModeOutOfRange(8)),
            "expected MemoryModeOutOfRange(8), got {err:?}"
        );
        Ok(())
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

    // --- CoarseStepMultiplier ---

    #[test]
    fn coarse_step_multiplier_valid_range() -> TestResult {
        for i in 0u8..CoarseStepMultiplier::COUNT {
            let val = CoarseStepMultiplier::try_from(i)?;
            assert_eq!(
                u8::from(val),
                i,
                "CoarseStepMultiplier round-trip failed at {i}"
            );
        }
        Ok(())
    }

    #[test]
    fn coarse_step_multiplier_invalid() {
        assert!(CoarseStepMultiplier::try_from(CoarseStepMultiplier::COUNT).is_err());
        assert!(CoarseStepMultiplier::try_from(255).is_err());
    }

    #[test]
    fn coarse_step_multiplier_round_trip() -> TestResult {
        for i in 0u8..CoarseStepMultiplier::COUNT {
            let val = CoarseStepMultiplier::try_from(i)?;
            assert_eq!(u8::from(val), i);
        }
        Ok(())
    }

    #[test]
    fn coarse_step_multiplier_values() {
        assert_eq!(CoarseStepMultiplier::X1.multiplier(), 1);
        assert_eq!(CoarseStepMultiplier::X2.multiplier(), 2);
        assert_eq!(CoarseStepMultiplier::X5.multiplier(), 5);
        assert_eq!(CoarseStepMultiplier::X10.multiplier(), 10);
        assert_eq!(CoarseStepMultiplier::X50.multiplier(), 50);
        assert_eq!(CoarseStepMultiplier::X100.multiplier(), 100);
    }

    #[test]
    fn coarse_step_multiplier_display() {
        assert_eq!(CoarseStepMultiplier::X1.to_string(), "x1");
        assert_eq!(CoarseStepMultiplier::X2.to_string(), "x2");
        assert_eq!(CoarseStepMultiplier::X5.to_string(), "x5");
        assert_eq!(CoarseStepMultiplier::X10.to_string(), "x10");
        assert_eq!(CoarseStepMultiplier::X50.to_string(), "x50");
        assert_eq!(CoarseStepMultiplier::X100.to_string(), "x100");
    }
}
