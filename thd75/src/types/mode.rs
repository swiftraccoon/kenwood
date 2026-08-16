//! Operating mode, power level, shift direction, and step size types.

use std::fmt;

use crate::error::ValidationError;

/// Operating mode as returned by the `MD` (mode) CAT command.
///
/// The `MD` response has ten defined wire values (0-9). This complete read
/// domain does not imply that every value can be selected with an `MD` write
/// in every band or current radio state.
///
/// Channel records use the same values through WFM, represented by the
/// narrower [`ChannelMode`] type because channel storage does not accept
/// CW-R.
///
/// # Receiver and front-panel availability
///
/// Not all modes are available on both bands:
///
/// - **Band A** supports **FM**, **NFM**, **DV**, and **DR**. Band A is the amateur
///   TX/RX band (144/220/430 MHz). Its receiver chain (VCO/PLL IC800,
///   IF IC IC900) is a double super heterodyne with 1st IF at 57.15 MHz
///   and 2nd IF at 450 kHz. It has no third IF stage, so AM/SSB/CW
///   demodulation is not possible in hardware (service manual §2.1.3).
/// - **Band B** supports all modes: FM, DV, AM, LSB, USB, CW, NFM, DR,
///   WFM, and CW-R. Band B's receiver chain (VCO/PLL IC700, IF IC
///   IC1002) adds a third mixer (IC1001) producing a 3rd IF at 10.8 kHz,
///   which feeds into the CODEC (IC2011) for AM/SSB/CW demodulation.
///   This triple super heterodyne architecture is what enables the
///   wideband mode support (service manual §2.1.3.2).
///
/// CAT write availability is narrower and state-dependent. Callers must use
/// immediate readback rather than infer a state change from the write reply.
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
/// WFM is `MD` mode 8. It is the FM broadcast radio mode used on Band B for
/// the 76-108 MHz range.
/// The radio's display shows "WFM" in this mode.
///
/// # CW-R (CW Reverse)
///
/// CW-R is `MD` mode 9. It uses LSB detection for CW reception instead of the
/// default USB detection used by standard CW mode.
///
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OperatingMode {
    /// FM modulation (index 0). Available on both Band A and Band B.
    Fm = 0,
    /// D-STAR digital voice (index 1). Available on both Band A and Band B.
    Dv = 1,
    /// AM modulation (index 2). Band B only: Band A lacks the 3rd IF
    /// stage (10.8 kHz via IC1001) required for AM envelope detection.
    Am = 2,
    /// Lower sideband (index 3). Band B only: requires the 3rd IF at
    /// 10.8 kHz (via 3rd mixer IC1001 and 460.8 kHz local oscillation).
    Lsb = 3,
    /// Upper sideband (index 4). Band B only: requires the 3rd IF at
    /// 10.8 kHz (via 3rd mixer IC1001 and 460.8 kHz local oscillation).
    Usb = 4,
    /// CW / Morse code (index 5). Band B only: requires the 3rd IF at
    /// 10.8 kHz (via 3rd mixer IC1001 and 460.8 kHz local oscillation).
    Cw = 5,
    /// Narrow FM modulation (index 6). Available on both Band A and Band B.
    Nfm = 6,
    /// D-STAR repeater mode (index 7). Available on both Band A and Band B.
    Dr = 7,
    /// Wide FM (index 8). Band B only: FM broadcast reception mode for the
    /// 76-108 MHz range.
    Wfm = 8,
    /// CW Reverse (index 9). Band B only: uses LSB detection for CW reception
    /// instead of the default USB.
    CwReverse = 9,
}

impl OperatingMode {
    /// Number of valid mode values (0-9).
    pub const COUNT: u8 = 10;

    /// Every operating mode, in `MD` wire-value order.
    pub const ALL: [Self; Self::COUNT as usize] = [
        Self::Fm,
        Self::Dv,
        Self::Am,
        Self::Lsb,
        Self::Usb,
        Self::Cw,
        Self::Nfm,
        Self::Dr,
        Self::Wfm,
        Self::CwReverse,
    ];

    /// Canonical operator-facing mode name; also the [`fmt::Display`] form
    /// and the name accepted by this type's [`FromStr`](std::str::FromStr)
    /// implementation.
    ///
    /// # Examples
    ///
    /// ```
    /// use kenwood_thd75::types::OperatingMode;
    ///
    /// assert_eq!(OperatingMode::CwReverse.name(), "CW-R");
    /// assert_eq!("usb".parse::<OperatingMode>()?, OperatingMode::Usb);
    /// # Ok::<(), kenwood_thd75::error::ValidationError>(())
    /// ```
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Fm => "FM",
            Self::Dv => "DV",
            Self::Am => "AM",
            Self::Lsb => "LSB",
            Self::Usb => "USB",
            Self::Cw => "CW",
            Self::Nfm => "NFM",
            Self::Dr => "DR",
            Self::Wfm => "WFM",
            Self::CwReverse => "CW-R",
        }
    }
}

impl fmt::Display for OperatingMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

impl std::str::FromStr for OperatingMode {
    type Err = ValidationError;

    /// Parse the canonical mode name (the [`fmt::Display`] form), ASCII
    /// case-insensitively.
    fn from_str(text: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .into_iter()
            .find(|mode| text.eq_ignore_ascii_case(mode.name()))
            .ok_or_else(|| ValidationError::InvalidTextValue {
                name: "operating mode",
                value: text.to_owned(),
                detail: "must be one of FM, DV, AM, LSB, USB, CW, NFM, DR, WFM, CW-R",
                reason: "unrecognized mode name".to_owned(),
            })
    }
}

impl TryFrom<u8> for OperatingMode {
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
            _ => Err(ValidationError::SettingOutOfRange {
                name: "operating mode",
                value,
                detail: "must be 0-9: FM/DV/AM/LSB/USB/CW/NFM/DR/WFM/CW-R",
            }),
        }
    }
}

impl From<OperatingMode> for u8 {
    fn from(mode: OperatingMode) -> Self {
        mode as Self
    }
}

/// Transmit power level.
///
/// Maps to the power field in the `PC`, `FO`, and `ME` commands.
/// The four wire values are Hi (0), Mid (1), Lo (2), and EL (3).
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
    /// High power, 5 W (index 0).
    High = 0,
    /// Medium power, 2 W (index 1).
    Medium = 1,
    /// Low power, 0.5 W (index 2).
    Low = 2,
    /// Extra-low power, 50 mW (index 3).
    ExtraLow = 3,
}

impl PowerLevel {
    /// Number of valid power level values (0-3).
    pub const COUNT: u8 = 4;

    /// Every power level, in `PC` wire-value order (highest power first).
    pub const ALL: [Self; Self::COUNT as usize] =
        [Self::High, Self::Medium, Self::Low, Self::ExtraLow];

    /// Nominal transmit power in milliwatts (User Manual Chapter 28 values
    /// at external DC 13.8 V or battery 7.4 V).
    #[must_use]
    pub const fn as_milliwatts(self) -> u32 {
        match self {
            Self::High => 5_000,
            Self::Medium => 2_000,
            Self::Low => 500,
            Self::ExtraLow => 50,
        }
    }
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
            _ => Err(ValidationError::SettingOutOfRange {
                name: "power level",
                value,
                detail: "must be 0-3: High/Medium/Low/ExtraLow",
            }),
        }
    }
}

impl From<PowerLevel> for u8 {
    fn from(level: PowerLevel) -> Self {
        level as Self
    }
}

/// Repeater shift stored by FO/ME and MCP/SD-card channel records.
///
/// Split operation is a separate flag. Value 3 is the TH-D75's dedicated
/// minus 7.6 MHz shift used in the applicable regional band plan.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ShiftDirection {
    /// Transmit and receive on the same frequency (`0`).
    Simplex = 0,
    /// Transmit above the receive frequency by the configured offset (`1`).
    Plus = 1,
    /// Transmit below the receive frequency by the configured offset (`2`).
    Minus = 2,
    /// Transmit 7.6 MHz below the receive frequency (`3`).
    Minus7Point6MHz = 3,
}

impl ShiftDirection {
    /// Number of channel-shift values.
    pub const COUNT: u8 = 4;

    /// Every shift direction, in wire-value order.
    pub const ALL: [Self; Self::COUNT as usize] = [
        Self::Simplex,
        Self::Plus,
        Self::Minus,
        Self::Minus7Point6MHz,
    ];
}

impl TryFrom<u8> for ShiftDirection {
    type Error = ValidationError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Simplex),
            1 => Ok(Self::Plus),
            2 => Ok(Self::Minus),
            3 => Ok(Self::Minus7Point6MHz),
            _ => Err(ValidationError::SettingOutOfRange {
                name: "shift direction",
                value,
                detail: "must be 0-3",
            }),
        }
    }
}

impl From<ShiftDirection> for u8 {
    fn from(dir: ShiftDirection) -> Self {
        dir as Self
    }
}

impl fmt::Display for ShiftDirection {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Simplex => "Simplex",
            Self::Plus => "+",
            Self::Minus => "-",
            Self::Minus7Point6MHz => "-7.6 MHz",
        })
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
/// The CAT wire index-to-step-size mapping is:
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

    /// Every step size, in wire-value order (ascending step).
    pub const ALL: [Self; Self::COUNT as usize] = [
        Self::Hz5000,
        Self::Hz6250,
        Self::Hz8330,
        Self::Hz9000,
        Self::Hz10000,
        Self::Hz12500,
        Self::Hz15000,
        Self::Hz20000,
        Self::Hz25000,
        Self::Hz30000,
        Self::Hz50000,
        Self::Hz100000,
    ];

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
            _ => Err(ValidationError::SettingOutOfRange {
                name: "step size",
                value,
                detail: "must be 0-11",
            }),
        }
    }
}

impl From<StepSize> for u8 {
    fn from(step: StepSize) -> Self {
        step as Self
    }
}

/// Operating mode accepted by FO/ME and stored in channel memory.
///
/// The encoding is identical to [`OperatingMode`] for values 0 through 8. CW-R is an
/// `MD` operating mode but is not a valid stored-channel mode, so this type
/// makes that distinction explicit at API boundaries.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ChannelMode {
    /// FM modulation (`0`).
    Fm = 0,
    /// D-STAR digital voice (`1`).
    Dv = 1,
    /// AM modulation (`2`).
    Am = 2,
    /// Lower sideband (`3`).
    Lsb = 3,
    /// Upper sideband (`4`).
    Usb = 4,
    /// CW / Morse code (`5`).
    Cw = 5,
    /// Narrow FM modulation (`6`).
    Nfm = 6,
    /// D-STAR repeater mode (`7`).
    Dr = 7,
    /// Wide FM broadcast mode (`8`).
    Wfm = 8,
}

impl ChannelMode {
    /// Number of valid channel-mode values (0-8).
    pub const COUNT: u8 = 9;

    /// Every stored-channel mode, in wire-value order.
    pub const ALL: [Self; Self::COUNT as usize] = [
        Self::Fm,
        Self::Dv,
        Self::Am,
        Self::Lsb,
        Self::Usb,
        Self::Cw,
        Self::Nfm,
        Self::Dr,
        Self::Wfm,
    ];
}

impl fmt::Display for ChannelMode {
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
        }
    }
}

impl TryFrom<u8> for ChannelMode {
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
            _ => Err(ValidationError::SettingOutOfRange {
                name: "channel mode",
                value,
                detail: "must be 0-8: FM/DV/AM/LSB/USB/CW/NFM/DR/WFM",
            }),
        }
    }
}

impl From<ChannelMode> for u8 {
    fn from(mode: ChannelMode) -> Self {
        mode as Self
    }
}

impl From<ChannelMode> for OperatingMode {
    fn from(mode: ChannelMode) -> Self {
        match mode {
            ChannelMode::Fm => Self::Fm,
            ChannelMode::Dv => Self::Dv,
            ChannelMode::Am => Self::Am,
            ChannelMode::Lsb => Self::Lsb,
            ChannelMode::Usb => Self::Usb,
            ChannelMode::Cw => Self::Cw,
            ChannelMode::Nfm => Self::Nfm,
            ChannelMode::Dr => Self::Dr,
            ChannelMode::Wfm => Self::Wfm,
        }
    }
}

impl TryFrom<OperatingMode> for ChannelMode {
    type Error = ValidationError;

    fn try_from(mode: OperatingMode) -> Result<Self, Self::Error> {
        Self::try_from(u8::from(mode))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ValidationError;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    // --- OperatingMode ---

    #[test]
    fn operating_mode_valid_range() -> TestResult {
        for i in 0u8..OperatingMode::COUNT {
            let val = OperatingMode::try_from(i)?;
            assert_eq!(u8::from(val), i, "OperatingMode round-trip failed at {i}");
        }
        Ok(())
    }

    #[test]
    fn operating_mode_invalid() {
        assert!(OperatingMode::try_from(OperatingMode::COUNT).is_err());
        assert!(OperatingMode::try_from(255).is_err());
    }

    #[test]
    fn operating_mode_round_trip() -> TestResult {
        for i in 0u8..OperatingMode::COUNT {
            let val = OperatingMode::try_from(i)?;
            assert_eq!(u8::from(val), i);
        }
        Ok(())
    }

    #[test]
    fn operating_mode_error_variant() -> TestResult {
        let err = OperatingMode::try_from(OperatingMode::COUNT)
            .err()
            .ok_or("expected an operating-mode SettingOutOfRange error but got Ok")?;
        assert!(
            matches!(
                err,
                ValidationError::SettingOutOfRange {
                    name: "operating mode",
                    value: 10,
                    ..
                }
            ),
            "expected operating-mode SettingOutOfRange(10), got {err:?}"
        );
        Ok(())
    }

    #[test]
    fn operating_mode_display() {
        assert_eq!(OperatingMode::Fm.to_string(), "FM");
        assert_eq!(OperatingMode::Dv.to_string(), "DV");
        assert_eq!(OperatingMode::Am.to_string(), "AM");
        assert_eq!(OperatingMode::Lsb.to_string(), "LSB");
        assert_eq!(OperatingMode::Usb.to_string(), "USB");
        assert_eq!(OperatingMode::Cw.to_string(), "CW");
        assert_eq!(OperatingMode::Nfm.to_string(), "NFM");
        assert_eq!(OperatingMode::Dr.to_string(), "DR");
        assert_eq!(OperatingMode::Wfm.to_string(), "WFM");
        assert_eq!(OperatingMode::CwReverse.to_string(), "CW-R");
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
            .ok_or("expected a power-level SettingOutOfRange error but got Ok")?;
        assert!(
            matches!(
                err,
                ValidationError::SettingOutOfRange {
                    name: "power level",
                    value: 4,
                    ..
                }
            ),
            "expected power-level SettingOutOfRange(4), got {err:?}"
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
        for i in 0u8..ShiftDirection::COUNT {
            let val = ShiftDirection::try_from(i)?;
            assert_eq!(u8::from(val), i, "ShiftDirection round-trip failed at {i}");
        }
        Ok(())
    }

    #[test]
    fn shift_direction_invalid() {
        assert!(ShiftDirection::try_from(ShiftDirection::COUNT).is_err());
        assert!(ShiftDirection::try_from(255).is_err());
    }

    #[test]
    fn shift_direction_round_trip() -> TestResult {
        for i in 0u8..ShiftDirection::COUNT {
            let val = ShiftDirection::try_from(i)?;
            assert_eq!(u8::from(val), i);
        }
        Ok(())
    }

    #[test]
    fn shift_direction_values_and_display() {
        assert_eq!(u8::from(ShiftDirection::Simplex), 0);
        assert_eq!(u8::from(ShiftDirection::Plus), 1);
        assert_eq!(u8::from(ShiftDirection::Minus), 2);
        assert_eq!(u8::from(ShiftDirection::Minus7Point6MHz), 3);
        assert_eq!(ShiftDirection::Simplex.to_string(), "Simplex");
        assert_eq!(ShiftDirection::Plus.to_string(), "+");
        assert_eq!(ShiftDirection::Minus.to_string(), "-");
        assert_eq!(ShiftDirection::Minus7Point6MHz.to_string(), "-7.6 MHz");
    }

    #[test]
    fn shift_direction_error_variant() -> TestResult {
        let err = ShiftDirection::try_from(ShiftDirection::COUNT)
            .err()
            .ok_or("expected a shift-direction SettingOutOfRange error but got Ok")?;
        assert!(
            matches!(
                err,
                ValidationError::SettingOutOfRange {
                    name: "shift direction",
                    value: 4,
                    ..
                }
            ),
            "expected shift-direction SettingOutOfRange(4), got {err:?}"
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
            .ok_or("expected a step-size SettingOutOfRange error but got Ok")?;
        assert!(
            matches!(
                err,
                ValidationError::SettingOutOfRange {
                    name: "step size",
                    value: 12,
                    ..
                }
            ),
            "expected step-size SettingOutOfRange(12), got {err:?}"
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

    // --- ChannelMode ---

    #[test]
    fn channel_mode_valid_range() -> TestResult {
        for i in 0u8..ChannelMode::COUNT {
            let val = ChannelMode::try_from(i)?;
            assert_eq!(u8::from(val), i, "ChannelMode round-trip failed at {i}");
        }
        Ok(())
    }

    #[test]
    fn channel_mode_invalid() {
        assert!(ChannelMode::try_from(ChannelMode::COUNT).is_err());
        assert!(ChannelMode::try_from(255).is_err());
    }

    #[test]
    fn channel_mode_error_variant() -> TestResult {
        let err = ChannelMode::try_from(ChannelMode::COUNT)
            .err()
            .ok_or("expected a channel-mode SettingOutOfRange error but got Ok")?;
        assert!(
            matches!(
                err,
                ValidationError::SettingOutOfRange {
                    name: "channel mode",
                    value: 9,
                    ..
                }
            ),
            "expected channel-mode SettingOutOfRange(9), got {err:?}"
        );
        Ok(())
    }

    #[test]
    fn channel_mode_display() {
        assert_eq!(ChannelMode::Fm.to_string(), "FM");
        assert_eq!(ChannelMode::Dv.to_string(), "DV");
        assert_eq!(ChannelMode::Am.to_string(), "AM");
        assert_eq!(ChannelMode::Lsb.to_string(), "LSB");
        assert_eq!(ChannelMode::Usb.to_string(), "USB");
        assert_eq!(ChannelMode::Cw.to_string(), "CW");
        assert_eq!(ChannelMode::Nfm.to_string(), "NFM");
        assert_eq!(ChannelMode::Dr.to_string(), "DR");
        assert_eq!(ChannelMode::Wfm.to_string(), "WFM");
    }

    #[test]
    fn channel_mode_matches_md_encoding() -> TestResult {
        for raw in 0u8..ChannelMode::COUNT {
            let channel_mode = ChannelMode::try_from(raw)?;
            let md_mode = OperatingMode::try_from(raw)?;
            assert_eq!(OperatingMode::from(channel_mode), md_mode);
        }
        assert_eq!(u8::from(ChannelMode::Wfm), 8);
        assert!(ChannelMode::try_from(ChannelMode::COUNT).is_err());
        assert!(ChannelMode::try_from(OperatingMode::CwReverse).is_err());
        Ok(())
    }

    #[test]
    fn all_consts_list_every_wire_value_in_order() {
        for (raw, mode) in (0_u8..).zip(OperatingMode::ALL) {
            assert_eq!(u8::from(mode), raw);
        }
        for (raw, level) in (0_u8..).zip(PowerLevel::ALL) {
            assert_eq!(u8::from(level), raw);
        }
        for (raw, direction) in (0_u8..).zip(ShiftDirection::ALL) {
            assert_eq!(u8::from(direction), raw);
        }
        for (raw, step) in (0_u8..).zip(StepSize::ALL) {
            assert_eq!(u8::from(step), raw);
        }
        for (raw, mode) in (0_u8..).zip(ChannelMode::ALL) {
            assert_eq!(u8::from(mode), raw);
        }
        assert_eq!(OperatingMode::ALL.len(), usize::from(OperatingMode::COUNT));
        assert_eq!(PowerLevel::ALL.len(), usize::from(PowerLevel::COUNT));
        assert_eq!(
            ShiftDirection::ALL.len(),
            usize::from(ShiftDirection::COUNT)
        );
        assert_eq!(StepSize::ALL.len(), usize::from(StepSize::COUNT));
        assert_eq!(ChannelMode::ALL.len(), usize::from(ChannelMode::COUNT));
    }

    #[test]
    fn operating_mode_parses_canonical_names() -> TestResult {
        for mode in OperatingMode::ALL {
            let reparsed: OperatingMode = mode.to_string().parse()?;
            assert_eq!(reparsed, mode);
        }
        assert_eq!("fm".parse::<OperatingMode>()?, OperatingMode::Fm);
        assert_eq!("cw-r".parse::<OperatingMode>()?, OperatingMode::CwReverse);
        let unknown = "FMX".parse::<OperatingMode>();
        assert!(
            matches!(
                unknown,
                Err(crate::error::ValidationError::InvalidTextValue { .. })
            ),
            "unknown mode name must be rejected: {unknown:?}"
        );
        Ok(())
    }

    #[test]
    fn power_level_reports_nominal_milliwatts() {
        assert_eq!(PowerLevel::High.as_milliwatts(), 5_000);
        assert_eq!(PowerLevel::Medium.as_milliwatts(), 2_000);
        assert_eq!(PowerLevel::Low.as_milliwatts(), 500);
        assert_eq!(PowerLevel::ExtraLow.as_milliwatts(), 50);
    }
}
