//! Lossless domains for independently named REWIND wire words.

use std::fmt;
use std::ops::{BitAnd, BitAndAssign, BitOr, BitOrAssign, BitXor, BitXorAssign, Not};

// These domains accept every value of their wire width. The shared expansion
// keeps standard extraction and numeric formatting identical across widths.
macro_rules! wire_value {
    ($(#[$meta:meta])* $name:ident, $raw:ty) => {
        $(#[$meta])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        pub struct $name($raw);

        impl $name {
            /// Preserve the complete wire value, including unknown values and zero.
            #[must_use]
            pub const fn new(value: $raw) -> Self { Self(value) }

            /// Return the complete wire value.
            #[must_use]
            pub const fn get(self) -> $raw { self.0 }
        }

        impl From<$raw> for $name {
            fn from(value: $raw) -> Self { Self::new(value) }
        }

        impl From<$name> for $raw {
            fn from(value: $name) -> Self { value.get() }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { fmt::Display::fmt(&self.0, f) }
        }

        impl fmt::Binary for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { fmt::Binary::fmt(&self.0, f) }
        }

        impl fmt::Octal for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { fmt::Octal::fmt(&self.0, f) }
        }

        impl fmt::LowerHex for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { fmt::LowerHex::fmt(&self.0, f) }
        }

        impl fmt::UpperHex for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { fmt::UpperHex::fmt(&self.0, f) }
        }
    };
}

wire_value!(
    /// A remote application's four-byte identifier, distinct from a DMR subscriber ID.
    RemoteId, u32
);
wire_value!(
    /// A full-width DMR identity in a REWIND subscription, poll or super-header.
    ///
    /// All 32 bits, including zero, are valid wire data. This domain deliberately
    /// differs from the 24-bit full-link-control [`crate::DmrId`] and a shell's
    /// operator-facing subscriber allocation rules.
    WireDmrId, u32
);
wire_value!(
    /// A wrapping REWIND transport sequence word.
    ///
    /// Numeric formatting exposes its bits; chronological comparisons must use
    /// the protocol's wrapping sequence rules.
    TransportSequence, u32
);
wire_value!(
    /// An application service octet, preserving unrecognized service codes.
    ServiceCode, u8
);
wire_value!(
    /// A session-poll tree/type selector, preserving unmodeled selectors.
    PollKind, u32
);
wire_value!(
    /// A session-poll state word, preserving unmodeled states.
    PollState, u32
);
wire_value!(
    /// A full-link-control feature-set octet, preserving unmodeled feature codes.
    FeatureId, u8
);
wire_value!(
    /// A complete full-link-control octet: six opcode bits and both upper flags.
    ControlOctet, u8
);

impl ServiceCode {
    /// Self-service Open DMR Terminal, wire service `0x21`.
    pub const OPEN_TERMINAL: Self = Self(0x21);
}

impl ControlOctet {
    /// Classify the low six opcode bits while retaining the complete octet.
    #[must_use]
    pub const fn call_type(self) -> crate::FullLinkControlType {
        crate::FullLinkControlType::from_flco(self.0)
    }
}

macro_rules! wire_flags {
    ($(#[$meta:meta])* $name:ident, $raw:ty) => {
        wire_value!($(#[$meta])* $name, $raw);

        impl $name {
            /// No flags are set.
            pub const NONE: Self = Self(0);

            /// Preserve every flag bit, including unmodeled bits.
            #[must_use]
            pub const fn from_bits(bits: $raw) -> Self { Self(bits) }

            /// Return every flag bit.
            #[must_use]
            pub const fn bits(self) -> $raw { self.0 }

            /// Whether all bits in `other` are set.
            #[must_use]
            pub const fn contains(self, other: Self) -> bool { self.0 & other.0 == other.0 }
        }

        impl Default for $name {
            fn default() -> Self { Self::NONE }
        }

        impl BitOr for $name {
            type Output = Self;
            fn bitor(self, rhs: Self) -> Self { Self(self.0 | rhs.0) }
        }

        impl BitAnd for $name {
            type Output = Self;
            fn bitand(self, rhs: Self) -> Self { Self(self.0 & rhs.0) }
        }

        impl BitXor for $name {
            type Output = Self;
            fn bitxor(self, rhs: Self) -> Self { Self(self.0 ^ rhs.0) }
        }

        impl Not for $name {
            type Output = Self;
            fn not(self) -> Self { Self(!self.0) }
        }

        impl BitOrAssign for $name {
            fn bitor_assign(&mut self, rhs: Self) { self.0 |= rhs.0; }
        }

        impl BitAndAssign for $name {
            fn bitand_assign(&mut self, rhs: Self) { self.0 &= rhs.0; }
        }

        impl BitXorAssign for $name {
            fn bitxor_assign(&mut self, rhs: Self) { self.0 ^= rhs.0; }
        }
    };
}

wire_flags!(
    /// Session-poll flags, preserving every bit in their 32-bit wire word.
    PollFlags, u32
);
wire_flags!(
    /// Full-link-control service options, preserving every bit in their wire octet.
    ServiceOptions, u8
);
