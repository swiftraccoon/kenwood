//! Header flag fields from `ircDDBGateway/Common/DStarDefines.h`.

/// An independently combinable bit in the first header byte.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HeaderFlag {
    /// Data-mode bit, `0x80`.
    Data,
    /// Repeater-routing bit, `0x40`.
    Repeater,
    /// Interrupted-transmission bit, `0x20`.
    Interrupted,
    /// Control-signaling bit, `0x10`.
    ControlSignal,
    /// Urgent bit, `0x08`.
    Urgent,
}

impl HeaderFlag {
    const fn mask(self) -> u8 {
        match self {
            Self::Data => 0x80,
            Self::Repeater => 0x40,
            Self::Interrupted => 0x20,
            Self::ControlSignal => 0x10,
            Self::Urgent => 0x08,
        }
    }
}

/// Mutually exclusive repeater-control code in the low three flag bits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RepeaterControl {
    /// Zero control code.
    Normal,
    /// Relay unavailable, `1`.
    RelayUnavailable,
    /// No response, `2`.
    NoResponse,
    /// Acknowledgment, `3`.
    Acknowledge,
    /// Resend requested, `4`.
    ResendRequested,
    /// Code `5`, unnamed in the reference constants and preserved on receive.
    Unassigned,
    /// Automatic reply, `6`.
    AutoReply,
    /// Repeater control, `7`.
    Control,
}

impl RepeaterControl {
    const fn bits(self) -> u8 {
        match self {
            Self::Normal => 0,
            Self::RelayUnavailable => 1,
            Self::NoResponse => 2,
            Self::Acknowledge => 3,
            Self::ResendRequested => 4,
            Self::Unassigned => 5,
            Self::AutoReply => 6,
            Self::Control => 7,
        }
    }
}

/// Three lossless header bytes with typed flag and control operations.
///
/// All byte combinations are representable. Reserved bytes and the unnamed
/// control code survive receive, relay and encoding without normalization.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
pub struct HeaderFlags([u8; 3]);

impl HeaderFlags {
    /// Three zero flag bytes.
    pub const NONE: Self = Self([0; 3]);

    /// Preserve all three wire bytes, including reserved data.
    #[must_use]
    pub const fn from_bytes(bytes: [u8; 3]) -> Self {
        Self(bytes)
    }

    /// Complete flag bytes in their wire order.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 3] {
        &self.0
    }

    /// Whether the selected independent flag bit is set.
    #[must_use]
    pub const fn contains(self, flag: HeaderFlag) -> bool {
        let [control, _, _] = self.0;
        control & flag.mask() != 0
    }

    /// Set or clear one flag while preserving control and reserved bits.
    #[must_use]
    pub const fn with(self, flag: HeaderFlag, enabled: bool) -> Self {
        let [control, second, third] = self.0;
        let control = if enabled {
            control | flag.mask()
        } else {
            control & !flag.mask()
        };
        Self([control, second, third])
    }

    /// Repeater-control code carried by the low three bits.
    #[must_use]
    pub const fn control(self) -> RepeaterControl {
        let [control, _, _] = self.0;
        match control & 0x07 {
            0 => RepeaterControl::Normal,
            1 => RepeaterControl::RelayUnavailable,
            2 => RepeaterControl::NoResponse,
            3 => RepeaterControl::Acknowledge,
            4 => RepeaterControl::ResendRequested,
            5 => RepeaterControl::Unassigned,
            6 => RepeaterControl::AutoReply,
            _ => RepeaterControl::Control,
        }
    }

    /// Replace only the mutually exclusive repeater-control field.
    #[must_use]
    pub const fn with_control(self, code: RepeaterControl) -> Self {
        let [control, second, third] = self.0;
        Self([(control & !0x07) | code.bits(), second, third])
    }
}

impl From<[u8; 3]> for HeaderFlags {
    fn from(value: [u8; 3]) -> Self {
        Self::from_bytes(value)
    }
}

impl From<HeaderFlags> for [u8; 3] {
    fn from(value: HeaderFlags) -> Self {
        value.0
    }
}

impl AsRef<[u8; 3]> for HeaderFlags {
    fn as_ref(&self) -> &[u8; 3] {
        self.as_bytes()
    }
}
