//! DTMF (Dual-Tone Multi-Frequency) configuration and memory types.
//!
//! DTMF is the tone signaling system used by touch-tone telephones and
//! amateur radio for dialing, auto-patching, and remote control. The
//! TH-D75 supports 16 DTMF memory slots for storing digit sequences,
//! configurable encode speed, pause time, and TX hold behavior.
//!
//! These types model DTMF settings from the TH-D75's menu system
//! (Chapter 11 of the user manual). Derived from the capability gap
//! analysis features 128-132.

// ---------------------------------------------------------------------------
// DTMF memory slot
// ---------------------------------------------------------------------------

/// A DTMF memory slot.
///
/// The TH-D75 provides 16 DTMF memory slots (0-15), each storing a
/// name and a sequence of DTMF digits for the auto dialer function.
/// Valid DTMF digits are `0`-`9`, `A`-`D`, `*`, and `#`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct DtmfMemory {
    /// Slot index (0-15).
    pub slot: DtmfSlot,
    /// Memory name (up to 8 characters).
    pub name: DtmfName,
    /// DTMF digit sequence.
    pub digits: DtmfDigits,
}

/// DTMF memory slot index (0-15).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DtmfSlot(u8);

impl DtmfSlot {
    /// Maximum slot index.
    pub const MAX: u8 = 15;

    /// Total number of DTMF memory slots.
    pub const COUNT: usize = 16;

    /// Creates a new DTMF memory slot index.
    ///
    /// # Errors
    ///
    /// Returns `None` if the index exceeds 15.
    #[must_use]
    pub const fn new(index: u8) -> Option<Self> {
        if index <= Self::MAX {
            Some(Self(index))
        } else {
            None
        }
    }

    /// Returns the slot index.
    #[must_use]
    pub const fn index(self) -> u8 {
        self.0
    }
}

/// DTMF memory name (up to 8 characters).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct DtmfName(String);

impl DtmfName {
    /// Maximum length of a DTMF memory name.
    pub const MAX_LEN: usize = 8;

    /// Creates a new DTMF memory name.
    ///
    /// # Errors
    ///
    /// Returns `None` if the text exceeds 8 characters.
    #[must_use]
    pub fn new(text: &str) -> Option<Self> {
        if text.len() <= Self::MAX_LEN {
            Some(Self(text.to_owned()))
        } else {
            None
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
    /// Returns `None` if the sequence exceeds 16 characters or contains
    /// invalid DTMF digits.
    #[must_use]
    pub fn new(digits: &str) -> Option<Self> {
        if digits.len() <= Self::MAX_LEN && digits.chars().all(is_valid_dtmf) {
            Some(Self(digits.to_owned()))
        } else {
            None
        }
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
// DTMF configuration
// ---------------------------------------------------------------------------

/// DTMF encoder and dialer configuration.
///
/// Controls the speed at which DTMF tones are generated, the pause
/// duration between digit groups, TX hold behavior, and whether DTMF
/// can be transmitted on a busy channel.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct DtmfConfig {
    /// DTMF tone encode speed.
    pub encode_speed: DtmfSpeed,
    /// Pause time between DTMF digit groups.
    pub pause_time: DtmfPause,
    /// TX hold -- keep transmitter keyed between DTMF digit groups.
    pub tx_hold: bool,
    /// Allow DTMF transmission on a busy (occupied) channel.
    pub tx_on_busy: bool,
}

impl Default for DtmfConfig {
    fn default() -> Self {
        Self {
            encode_speed: DtmfSpeed::Slow,
            pause_time: DtmfPause::Ms500,
            tx_hold: false,
            tx_on_busy: false,
        }
    }
}

/// DTMF tone encode speed.
///
/// Controls how long each DTMF tone is transmitted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DtmfSpeed {
    /// Slow encode speed (100 ms per digit).
    Slow,
    /// Fast encode speed (50 ms per digit).
    Fast,
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
        for i in 0u8..=15 {
            assert!(DtmfSlot::new(i).is_some());
        }
    }

    #[test]
    fn dtmf_slot_invalid() {
        assert!(DtmfSlot::new(16).is_none());
    }

    #[test]
    fn dtmf_name_valid() {
        let name = DtmfName::new("AUTOPAT").unwrap();
        assert_eq!(name.as_str(), "AUTOPAT");
    }

    #[test]
    fn dtmf_name_max_length() {
        let name = DtmfName::new("12345678").unwrap();
        assert_eq!(name.as_str().len(), 8);
    }

    #[test]
    fn dtmf_name_too_long() {
        assert!(DtmfName::new("123456789").is_none());
    }

    #[test]
    fn dtmf_digits_valid() {
        let digits = DtmfDigits::new("123A*#BD").unwrap();
        assert_eq!(digits.as_str(), "123A*#BD");
        assert_eq!(digits.len(), 8);
        assert!(!digits.is_empty());
    }

    #[test]
    fn dtmf_digits_empty() {
        let digits = DtmfDigits::new("").unwrap();
        assert!(digits.is_empty());
    }

    #[test]
    fn dtmf_digits_all_valid_chars() {
        assert!(DtmfDigits::new("0123456789ABCD*#").is_some());
    }

    #[test]
    fn dtmf_digits_invalid_char() {
        assert!(DtmfDigits::new("123E").is_none());
    }

    #[test]
    fn dtmf_digits_lowercase_rejected() {
        assert!(DtmfDigits::new("123a").is_none());
    }

    #[test]
    fn dtmf_digits_too_long() {
        assert!(DtmfDigits::new("01234567890123456").is_none());
    }

    #[test]
    fn dtmf_config_default() {
        let cfg = DtmfConfig::default();
        assert_eq!(cfg.encode_speed, DtmfSpeed::Slow);
        assert_eq!(cfg.pause_time, DtmfPause::Ms500);
        assert!(!cfg.tx_hold);
        assert!(!cfg.tx_on_busy);
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
