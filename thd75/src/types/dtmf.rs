//! DTMF (Dual-Tone Multi-Frequency) settings and memory types.
//!
//! DTMF is the tone signaling system used by touch-tone telephones and
//! amateur radio for dialing, auto-patching, and remote control. The
//! TH-D75 supports 10 DTMF memory channels (Menu No. 163) for storing
//! digit sequences, plus 10 dedicated `EchoLink` memory channels (Menu
//! No. 164), configurable encode speed, pause time, and TX hold behavior.
//!
//! Per User Manual Chapter 11:
//!
//! - **Manual dialing**: press `[PTT]` then press keypad keys to send
//!   DTMF tones in real time.
//! - **Automatic dialer**: store up to 16 digits per channel with an
//!   optional name (up to 16 characters). Transmit by pressing `[PTT]`,
//!   then `[ENT]`, selecting a channel, then `[ENT]` again.
//! - **DTMF Hold** (Menu No. 162): when enabled, the transmitter stays
//!   keyed for 2 seconds after each keypress without holding `[PTT]`.
//! - **DTMF Key Lock** (Menu No. 961): locks DTMF keys to prevent
//!   accidental transmission while PTT is held.
//! - **Encode speed** (Menu No. 160): 50 / 100 / 150 ms per digit.
//!   Some repeaters may not respond correctly at fast speed.
//! - **Pause time** (Menu No. 161): 100-2000 ms between digit groups.
//!
//! These types model DTMF settings from the TH-D75's menu system
//! (Chapter 11 of the user manual).

use crate::error::ValidationError;

use super::settings::DtmfToneDuration;

// ---------------------------------------------------------------------------
// DTMF memory slot
// ---------------------------------------------------------------------------

/// A DTMF memory slot.
///
/// The TH-D75 provides 10 DTMF memory slots (0-9), each storing a
/// name and a sequence of DTMF digits for the auto dialer function.
/// Valid DTMF digits are `0`-`9`, `A`-`D`, `*`, and `#`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct DtmfMemory {
    /// Slot index (0-9).
    pub slot: DtmfSlot,
    /// Memory name (up to 16 UTF-8 encoded bytes).
    pub name: DtmfName,
    /// DTMF digit sequence.
    pub digits: DtmfDigits,
}

/// DTMF memory slot index (0-9).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DtmfSlot(u8);

impl DtmfSlot {
    /// Maximum slot index.
    pub const MAX: u8 = 9;

    /// Total number of DTMF memory slots.
    pub const COUNT: usize = 10;

    /// Creates a new DTMF memory slot index.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::DtmfSlotOutOfRange`] if the index exceeds 9.
    pub const fn new(index: u8) -> Result<Self, ValidationError> {
        if index <= Self::MAX {
            Ok(Self(index))
        } else {
            Err(ValidationError::DtmfSlotOutOfRange { index })
        }
    }

    /// Returns the slot index.
    #[must_use]
    pub const fn as_raw(self) -> u8 {
        self.0
    }
}

/// DTMF memory name (up to 16 UTF-8 encoded bytes).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct DtmfName(String);

impl DtmfName {
    /// Maximum length of a DTMF memory name.
    pub const MAX_LEN: usize = 16;

    /// Creates a new DTMF memory name.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::DtmfNameTooLong`] if `text` exceeds sixteen
    /// UTF-8 encoded bytes.
    pub fn new(text: &str) -> Result<Self, ValidationError> {
        if text.len() <= Self::MAX_LEN {
            Ok(Self(text.to_owned()))
        } else {
            Err(ValidationError::DtmfNameTooLong { len: text.len() })
        }
    }

    /// Returns the name as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// DTMF digit sequence (valid characters: `0`-`9`, `A`-`D`, `*`, `#`).
///
/// The maximum length of a DTMF digit sequence on the TH-D75 is 16
/// characters.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct DtmfDigits(String);

impl DtmfDigits {
    /// Maximum length of a DTMF digit sequence.
    pub const MAX_LEN: usize = 16;

    /// Creates a new DTMF digit sequence after validating all characters.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::DtmfDigitsTooLong`] if the sequence exceeds
    /// sixteen encoded bytes, or [`ValidationError::InvalidDtmfDigit`] at the
    /// first character outside `0`-`9`, `A`-`D`, `*`, and `#`.
    pub fn new(digits: &str) -> Result<Self, ValidationError> {
        if digits.len() > Self::MAX_LEN {
            return Err(ValidationError::DtmfDigitsTooLong { len: digits.len() });
        }
        if let Some((offset, value)) = digits.char_indices().find(|(_, c)| !is_valid_dtmf(*c)) {
            return Err(ValidationError::InvalidDtmfDigit { offset, value });
        }
        Ok(Self(digits.to_owned()))
    }

    /// Returns the digit sequence as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Returns the number of digits in the sequence.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.0.len()
    }

    /// Returns `true` if the digit sequence is empty.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

// ---------------------------------------------------------------------------
// DTMF settings
// ---------------------------------------------------------------------------

/// DTMF encoder and dialer settings.
///
/// Controls the speed at which DTMF tones are generated, the pause
/// duration between digit groups, TX hold behavior, and whether DTMF
/// can be transmitted on a busy channel.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct DtmfSettings {
    /// DTMF tone encode speed.
    pub encode_speed: DtmfToneDuration,
    /// Pause time between DTMF digit groups.
    pub pause_time: DtmfPause,
    /// TX hold -- keep transmitter keyed between DTMF digit groups.
    pub tx_hold: bool,
    /// Allow DTMF transmission on a busy (occupied) channel.
    pub tx_on_busy: bool,
}

impl DtmfSettings {
    /// Returns the documented TH-D75 factory DTMF settings.
    #[must_use]
    pub const fn factory_default() -> Self {
        Self {
            encode_speed: DtmfToneDuration::Ms100,
            pause_time: DtmfPause::Ms500,
            tx_hold: false,
            tx_on_busy: false,
        }
    }
}

/// DTMF pause time between digit groups.
///
/// When a DTMF sequence contains a pause marker, the transmitter
/// waits for the configured duration before continuing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DtmfPause {
    /// 100 ms pause.
    Ms100,
    /// 250 ms pause.
    Ms250,
    /// 500 ms pause.
    Ms500,
    /// 750 ms pause.
    Ms750,
    /// 1000 ms pause.
    Ms1000,
    /// 1500 ms pause.
    Ms1500,
    /// 2000 ms pause.
    Ms2000,
}

// ---------------------------------------------------------------------------
// Validation helper
// ---------------------------------------------------------------------------

/// Returns `true` if the character is a valid DTMF digit.
///
/// Valid DTMF digits are: `0`-`9`, `A`-`D`, `*`, and `#`.
#[must_use]
pub const fn is_valid_dtmf(c: char) -> bool {
    matches!(c, '0'..='9' | 'A'..='D' | '*' | '#')
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dtmf_slot_valid_range() {
        for i in 0u8..=9 {
            assert!(DtmfSlot::new(i).is_ok());
        }
    }

    #[test]
    fn dtmf_slot_invalid() {
        assert!(matches!(
            DtmfSlot::new(10),
            Err(ValidationError::DtmfSlotOutOfRange { index: 10 })
        ));
    }

    #[test]
    fn dtmf_name_valid() -> Result<(), Box<dyn std::error::Error>> {
        let name = DtmfName::new("AUTOPAT")?;
        assert_eq!(name.as_str(), "AUTOPAT");
        Ok(())
    }

    #[test]
    fn dtmf_name_max_length() -> Result<(), Box<dyn std::error::Error>> {
        let name = DtmfName::new("1234567890ABCDEF")?;
        assert_eq!(name.as_str().len(), 16);
        Ok(())
    }

    #[test]
    fn dtmf_name_too_long() {
        assert!(matches!(
            DtmfName::new("1234567890ABCDEFG"),
            Err(ValidationError::DtmfNameTooLong { len: 17 })
        ));
    }

    #[test]
    fn dtmf_digits_valid() -> Result<(), Box<dyn std::error::Error>> {
        let digits = DtmfDigits::new("123A*#BD")?;
        assert_eq!(digits.as_str(), "123A*#BD");
        assert_eq!(digits.len(), 8);
        assert!(!digits.is_empty());
        Ok(())
    }

    #[test]
    fn dtmf_digits_empty() -> Result<(), Box<dyn std::error::Error>> {
        let digits = DtmfDigits::new("")?;
        assert!(digits.is_empty());
        Ok(())
    }

    #[test]
    fn dtmf_digits_all_valid_chars() {
        assert!(DtmfDigits::new("0123456789ABCD*#").is_ok());
    }

    #[test]
    fn dtmf_digits_invalid_char() {
        assert!(matches!(
            DtmfDigits::new("123E"),
            Err(ValidationError::InvalidDtmfDigit {
                offset: 3,
                value: 'E'
            })
        ));
    }

    #[test]
    fn dtmf_digits_lowercase_rejected() {
        assert!(matches!(
            DtmfDigits::new("123a"),
            Err(ValidationError::InvalidDtmfDigit {
                offset: 3,
                value: 'a'
            })
        ));
    }

    #[test]
    fn dtmf_digits_too_long() {
        assert!(matches!(
            DtmfDigits::new("01234567890123456"),
            Err(ValidationError::DtmfDigitsTooLong { len: 17 })
        ));
    }

    #[test]
    fn dtmf_settings_factory_default() {
        let settings = DtmfSettings::factory_default();
        assert_eq!(settings.encode_speed, DtmfToneDuration::Ms100);
        assert_eq!(settings.pause_time, DtmfPause::Ms500);
        assert!(!settings.tx_hold);
        assert!(!settings.tx_on_busy);
    }

    #[test]
    fn dtmf_speed_covers_all_three_menu_values() -> Result<(), Box<dyn std::error::Error>> {
        for (raw, speed, milliseconds) in [
            (0, DtmfToneDuration::Ms50, 50),
            (1, DtmfToneDuration::Ms100, 100),
            (2, DtmfToneDuration::Ms150, 150),
        ] {
            assert_eq!(DtmfToneDuration::try_from(raw)?, speed);
            assert_eq!(u8::from(speed), raw);
            assert_eq!(speed.as_milliseconds(), milliseconds);
        }
        assert!(DtmfToneDuration::try_from(3).is_err());
        Ok(())
    }

    #[test]
    fn is_valid_dtmf_chars() {
        for c in '0'..='9' {
            assert!(is_valid_dtmf(c));
        }
        for c in 'A'..='D' {
            assert!(is_valid_dtmf(c));
        }
        assert!(is_valid_dtmf('*'));
        assert!(is_valid_dtmf('#'));
    }

    #[test]
    fn is_invalid_dtmf_chars() {
        assert!(!is_valid_dtmf('E'));
        assert!(!is_valid_dtmf('a'));
        assert!(!is_valid_dtmf(' '));
        assert!(!is_valid_dtmf('@'));
    }
}
