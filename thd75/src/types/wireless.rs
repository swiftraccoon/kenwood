//! Wireless remote control types.
//!
//! The TH-D75 supports wireless remote control of another TH-D75 (or
//! compatible transceiver) via DTMF signaling. A "control" radio sends
//! DTMF commands over air to a "target" radio, which decodes them and
//! executes the corresponding function. Access is protected by a
//! 4-digit DTMF password.
//!
//! These types model wireless control settings from Chapter 25 of the
//! TH-D75 user manual.

// ---------------------------------------------------------------------------
// Wireless control configuration
// ---------------------------------------------------------------------------

/// Wireless remote control configuration.
///
/// When enabled, the radio listens for incoming DTMF command sequences
/// and executes them if the correct password prefix is received.
/// The password is a 4-digit DTMF code (digits `0`-`9`, `A`-`D`,
/// `*`, `#`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct WirelessControlConfig {
    /// Enable wireless remote control reception.
    pub enabled: bool,
    /// 4-digit DTMF password for wireless control access.
    pub password: WirelessPassword,
}

/// Wireless control 4-digit DTMF password.
///
/// The password must be exactly 4 valid DTMF characters (`0`-`9`,
/// `A`-`D`, `*`, `#`).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct WirelessPassword(String);

impl WirelessPassword {
    /// Required password length (exactly 4 characters).
    pub const LEN: usize = 4;

    /// Creates a new wireless control password.
    ///
    /// # Errors
    ///
    /// Returns `None` if the password is not exactly 4 characters or
    /// contains invalid DTMF digits.
    #[must_use]
    pub fn new(password: &str) -> Option<Self> {
        if password.len() == Self::LEN
            && password.chars().all(super::dtmf::is_valid_dtmf)
        {
            Some(Self(password.to_owned()))
        } else {
            None
        }
    }

    /// Returns the password as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Default for WirelessPassword {
    fn default() -> Self {
        // Default password "0000" (all zeros).
        Self("0000".to_owned())
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wireless_config_default() {
        let cfg = WirelessControlConfig::default();
        assert!(!cfg.enabled);
        assert_eq!(cfg.password.as_str(), "0000");
    }

    #[test]
    fn wireless_password_valid() {
        let pwd = WirelessPassword::new("1234").unwrap();
        assert_eq!(pwd.as_str(), "1234");
    }

    #[test]
    fn wireless_password_dtmf_chars() {
        let pwd = WirelessPassword::new("A*#B").unwrap();
        assert_eq!(pwd.as_str(), "A*#B");
    }

    #[test]
    fn wireless_password_too_short() {
        assert!(WirelessPassword::new("123").is_none());
    }

    #[test]
    fn wireless_password_too_long() {
        assert!(WirelessPassword::new("12345").is_none());
    }

    #[test]
    fn wireless_password_invalid_chars() {
        assert!(WirelessPassword::new("12E4").is_none());
    }

    #[test]
    fn wireless_password_lowercase_rejected() {
        assert!(WirelessPassword::new("12a4").is_none());
    }
}
