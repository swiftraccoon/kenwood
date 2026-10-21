//! AX.25 modulo-8 control-field classification per v2.2 §4.3.

use crate::error::Ax25Error;

/// State of the AX.25 poll/final bit.
///
/// Its interpretation as poll or final depends on the frame's
/// [`CommandResponse`] classification. See the
/// [control and PID examples](crate#control-fields-and-protocol-identifiers).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum PollFinal {
    /// The poll/final bit is clear.
    #[default]
    Clear,
    /// The poll/final bit is set.
    Set,
}

impl PollFinal {
    /// Return whether the poll/final bit is set.
    #[must_use]
    pub const fn is_set(self) -> bool {
        matches!(self, Self::Set)
    }

    const fn from_control_byte(value: u8) -> Self {
        if value & 0x10 == 0 {
            Self::Clear
        } else {
            Self::Set
        }
    }
}

impl From<bool> for PollFinal {
    fn from(value: bool) -> Self {
        if value { Self::Set } else { Self::Clear }
    }
}

impl From<PollFinal> for bool {
    fn from(value: PollFinal) -> Self {
        value.is_set()
    }
}

/// A send or receive sequence number in AX.25 modulo-8 operation.
///
/// Keeping this range in the type prevents callers from constructing a
/// control field whose high bits would overlap another field when encoded.
/// See the [control example](crate#control-fields-and-protocol-identifiers).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Ax25SequenceNumber(u8);

impl Ax25SequenceNumber {
    /// Lowest modulo-8 sequence number.
    pub const MIN: Self = Self(0);
    /// Highest modulo-8 sequence number.
    pub const MAX: Self = Self(7);

    /// Validate a modulo-8 sequence number.
    ///
    /// # Errors
    ///
    /// Returns [`Ax25Error::InvalidSequenceNumber`] when `value` exceeds 7.
    pub const fn new(value: u8) -> Result<Self, Ax25Error> {
        if value <= Self::MAX.0 {
            Ok(Self(value))
        } else {
            Err(Ax25Error::InvalidSequenceNumber(value))
        }
    }

    /// Return the numeric sequence number.
    #[must_use]
    pub const fn get(self) -> u8 {
        self.0
    }
}

impl TryFrom<u8> for Ax25SequenceNumber {
    type Error = Ax25Error;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<Ax25SequenceNumber> for u8 {
    fn from(value: Ax25SequenceNumber) -> Self {
        value.get()
    }
}

/// A canonical, currently unassigned U-frame modifier pattern.
///
/// The stored byte has its P/F bit cleared. Known U-frame kinds have named
/// [`UnnumberedKind`] variants and cannot be smuggled through this type.
/// See the [control example](crate#control-fields-and-protocol-identifiers).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct UnknownUnnumberedKind(u8);

impl UnknownUnnumberedKind {
    /// Validate an unknown U-frame modifier pattern.
    ///
    /// # Errors
    ///
    /// Returns [`Ax25Error::InvalidUnknownUnnumberedKind`] if `value` is not
    /// a one-byte U-frame pattern, contains the P/F bit, or names a known
    /// AX.25 v2.2 U-frame kind.
    pub const fn new(value: u8) -> Result<Self, Ax25Error> {
        if value & 0x03 != 0x03 || value & 0x10 != 0 || is_known_unnumbered_kind(value) {
            Err(Ax25Error::InvalidUnknownUnnumberedKind(value))
        } else {
            Ok(Self(value))
        }
    }

    /// Return the raw modifier pattern with its P/F bit cleared.
    #[must_use]
    pub const fn as_byte(self) -> u8 {
        self.0
    }

    const fn from_parsed(value: u8) -> Self {
        Self(value)
    }
}

impl TryFrom<u8> for UnknownUnnumberedKind {
    type Error = Ax25Error;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<UnknownUnnumberedKind> for u8 {
    fn from(value: UnknownUnnumberedKind) -> Self {
        value.as_byte()
    }
}

const fn is_known_unnumbered_kind(value: u8) -> bool {
    matches!(
        value,
        0x03 | 0x0F | 0x2F | 0x43 | 0x63 | 0x6F | 0x87 | 0xAF | 0xE3
    )
}

/// AX.25 control-field frame-type family.
///
/// Per AX.25 v2.2 §4.2, the control byte identifies one of three frame
/// families:
/// - **Information (I)**: numbered data transfer frames
/// - **Supervisory (S)**: flow-control frames (RR, RNR, REJ, SREJ)
/// - **Unnumbered (U)**: link-setup, disconnection, and **UI** frames
///   used by APRS
///
/// The APRS protocol uses the `UI` subtype with control byte `0x03`.
/// See the
/// [control and PID examples](crate#control-fields-and-protocol-identifiers).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Ax25Control {
    /// Information frame (I).
    Information {
        /// Numbered send sequence (N(S)).
        ns: Ax25SequenceNumber,
        /// Numbered receive sequence (N(R)).
        nr: Ax25SequenceNumber,
        /// Poll/final bit.
        pf: PollFinal,
    },
    /// Supervisory frame (S) with sub-kind.
    Supervisory {
        /// Supervisory sub-kind (RR / RNR / REJ / SREJ).
        kind: SupervisoryKind,
        /// Numbered receive sequence (N(R)).
        nr: Ax25SequenceNumber,
        /// Poll/final bit.
        pf: PollFinal,
    },
    /// Unnumbered frame (U) with sub-kind.
    Unnumbered {
        /// Unnumbered sub-kind (UI / SABM / DISC / DM / UA / FRMR / XID / TEST).
        kind: UnnumberedKind,
        /// Poll/final bit.
        pf: PollFinal,
    },
}

impl Ax25Control {
    /// Parse a single control byte into an [`Ax25Control`] value.
    ///
    /// This covers modulo-8 control bytes. Modulo-128 extended control
    /// (2-byte) is not yet supported.
    #[must_use]
    pub const fn from_byte(b: u8) -> Self {
        // Bit 0 = 0 → Information frame
        if b & 0x01 == 0 {
            return Self::Information {
                ns: Ax25SequenceNumber((b >> 1) & 0x07),
                nr: Ax25SequenceNumber((b >> 5) & 0x07),
                pf: PollFinal::from_control_byte(b),
            };
        }
        // Bits 0-1 = 01 → Supervisory frame
        if b & 0x03 == 0x01 {
            let kind = match (b >> 2) & 0x03 {
                0 => SupervisoryKind::ReceiveReady,
                1 => SupervisoryKind::ReceiveNotReady,
                2 => SupervisoryKind::Reject,
                _ => SupervisoryKind::SelectiveReject,
            };
            return Self::Supervisory {
                kind,
                nr: Ax25SequenceNumber((b >> 5) & 0x07),
                pf: PollFinal::from_control_byte(b),
            };
        }
        // Otherwise Unnumbered (bits 0-1 = 11)
        let pf = PollFinal::from_control_byte(b);
        let kind_bits = b & 0xEF; // mask off P/F bit
        let kind = match kind_bits {
            0x03 => UnnumberedKind::UnnumberedInformation,
            0x2F => UnnumberedKind::SetAsyncBalancedMode,
            0x6F => UnnumberedKind::SetAsyncBalancedModeExtended,
            0x43 => UnnumberedKind::Disconnect,
            0x0F => UnnumberedKind::DisconnectedMode,
            0x63 => UnnumberedKind::UnnumberedAcknowledge,
            0x87 => UnnumberedKind::FrameReject,
            0xAF => UnnumberedKind::ExchangeIdentification,
            0xE3 => UnnumberedKind::Test,
            other => UnnumberedKind::Other(UnknownUnnumberedKind::from_parsed(other)),
        };
        Self::Unnumbered { kind, pf }
    }

    /// Returns `true` for the UI (Unnumbered Information) subtype used
    /// by APRS.
    #[must_use]
    pub const fn is_ui(self) -> bool {
        matches!(
            self,
            Self::Unnumbered {
                kind: UnnumberedKind::UnnumberedInformation,
                ..
            }
        )
    }

    /// Encode this modulo-8 control field as its wire byte.
    #[must_use]
    pub const fn as_byte(self) -> u8 {
        match self {
            Self::Information { ns, nr, pf } => {
                (ns.get() << 1) | (nr.get() << 5) | if pf.is_set() { 0x10 } else { 0 }
            }
            Self::Supervisory { kind, nr, pf } => {
                let kind_bits = match kind {
                    SupervisoryKind::ReceiveReady => 0x01,
                    SupervisoryKind::ReceiveNotReady => 0x05,
                    SupervisoryKind::Reject => 0x09,
                    SupervisoryKind::SelectiveReject => 0x0D,
                };
                kind_bits | (nr.get() << 5) | if pf.is_set() { 0x10 } else { 0 }
            }
            Self::Unnumbered { kind, pf } => kind.as_byte() | if pf.is_set() { 0x10 } else { 0 },
        }
    }
}

impl From<u8> for Ax25Control {
    fn from(value: u8) -> Self {
        Self::from_byte(value)
    }
}

impl From<Ax25Control> for u8 {
    fn from(value: Ax25Control) -> Self {
        value.as_byte()
    }
}

/// Supervisory (S) frame sub-kinds (AX.25 v2.2 §4.3.2).
///
/// See the [control example](crate#control-fields-and-protocol-identifiers).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SupervisoryKind {
    /// Receive Ready (RR).
    ReceiveReady,
    /// Receive Not Ready (RNR).
    ReceiveNotReady,
    /// Reject (REJ).
    Reject,
    /// Selective Reject (SREJ, AX.25 v2.2 addition).
    SelectiveReject,
}

/// Unnumbered (U) frame sub-kinds (AX.25 v2.2 §4.3.3).
///
/// See the [control example](crate#control-fields-and-protocol-identifiers).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum UnnumberedKind {
    /// Unnumbered Information (UI), used by APRS.
    UnnumberedInformation,
    /// Set Asynchronous Balanced Mode (SABM).
    SetAsyncBalancedMode,
    /// Set Asynchronous Balanced Mode Extended (SABME).
    SetAsyncBalancedModeExtended,
    /// Disconnect (DISC).
    Disconnect,
    /// Disconnected Mode (DM).
    DisconnectedMode,
    /// Unnumbered Acknowledge (UA).
    UnnumberedAcknowledge,
    /// Frame Reject (FRMR).
    FrameReject,
    /// Exchange Identification (XID).
    ExchangeIdentification,
    /// Test (TEST).
    Test,
    /// Any other pattern the parser does not classify.
    Other(UnknownUnnumberedKind),
}

impl UnnumberedKind {
    /// Return the U-frame modifier byte with its P/F bit cleared.
    #[must_use]
    pub const fn as_byte(self) -> u8 {
        match self {
            Self::UnnumberedInformation => 0x03,
            Self::SetAsyncBalancedMode => 0x2F,
            Self::SetAsyncBalancedModeExtended => 0x6F,
            Self::Disconnect => 0x43,
            Self::DisconnectedMode => 0x0F,
            Self::UnnumberedAcknowledge => 0x63,
            Self::FrameReject => 0x87,
            Self::ExchangeIdentification => 0xAF,
            Self::Test => 0xE3,
            Self::Other(kind) => kind.as_byte(),
        }
    }
}

#[cfg(test)]
mod tests {
    use alloc::boxed::Box;

    use super::{
        Ax25Control, Ax25SequenceNumber, PollFinal, SupervisoryKind, UnknownUnnumberedKind,
        UnnumberedKind, is_known_unnumbered_kind,
    };

    type TestResult = Result<(), Box<dyn core::error::Error>>;

    /// Every named U-frame modifier matches AX.25 v2.2 §4.3.3 with
    /// the poll/final bit clear and set.
    #[test]
    fn unnumbered_kind_table_matches_spec() {
        let cases: [(u8, UnnumberedKind); 9] = [
            (0x03, UnnumberedKind::UnnumberedInformation),
            (0x2F, UnnumberedKind::SetAsyncBalancedMode),
            (0x6F, UnnumberedKind::SetAsyncBalancedModeExtended),
            (0x43, UnnumberedKind::Disconnect),
            (0x0F, UnnumberedKind::DisconnectedMode),
            (0x63, UnnumberedKind::UnnumberedAcknowledge),
            (0x87, UnnumberedKind::FrameReject),
            (0xAF, UnnumberedKind::ExchangeIdentification),
            (0xE3, UnnumberedKind::Test),
        ];
        for (base, want) in cases {
            for pf_bit in [0x00u8, 0x10] {
                let byte = base | pf_bit;
                assert_eq!(
                    Ax25Control::from_byte(byte),
                    Ax25Control::Unnumbered {
                        kind: want,
                        pf: PollFinal::from(pf_bit != 0),
                    },
                    "U-frame control byte {byte:#04x}"
                );
            }
        }
    }

    /// Golden decode table for the four S-frame kinds per §4.3.2,
    /// across representative N(R) values and both P/F states.
    #[test]
    fn supervisory_kind_table_matches_spec() -> TestResult {
        let cases: [(u8, SupervisoryKind); 4] = [
            (0x01, SupervisoryKind::ReceiveReady),
            (0x05, SupervisoryKind::ReceiveNotReady),
            (0x09, SupervisoryKind::Reject),
            (0x0D, SupervisoryKind::SelectiveReject),
        ];
        for (base, want) in cases {
            for nr in [0u8, 5, 7] {
                for pf_bit in [0x00u8, 0x10] {
                    let byte = base | (nr << 5) | pf_bit;
                    assert_eq!(
                        Ax25Control::from_byte(byte),
                        Ax25Control::Supervisory {
                            kind: want,
                            nr: Ax25SequenceNumber::new(nr)?,
                            pf: PollFinal::from(pf_bit != 0),
                        },
                        "S-frame control byte {byte:#04x}"
                    );
                }
            }
        }
        Ok(())
    }

    /// I-frame N(S)/N(R)/P-F extraction per §4.3.1: N(S) in bits 1-3,
    /// P/F in bit 4, N(R) in bits 5-7.
    #[test]
    fn information_fields_extract_per_spec() -> TestResult {
        let cases: [(u8, u8, bool); 4] = [
            (0, 0, false), // byte 0x00
            (3, 6, true),  // byte 0xD6
            (7, 7, true),  // byte 0xFE
            (1, 4, false), // byte 0x82
        ];
        for (ns, nr, pf) in cases {
            let byte = (ns << 1) | (nr << 5) | if pf { 0x10 } else { 0x00 };
            assert_eq!(
                Ax25Control::from_byte(byte),
                Ax25Control::Information {
                    ns: Ax25SequenceNumber::new(ns)?,
                    nr: Ax25SequenceNumber::new(nr)?,
                    pf: PollFinal::from(pf),
                },
                "I-frame control byte {byte:#04x}"
            );
        }
        Ok(())
    }

    /// UI classification masks the poll/final bit: 0x13 and 0x03 are UI,
    /// while an I-frame byte is never UI.
    #[test]
    fn is_ui_masks_the_pf_bit() {
        assert!(Ax25Control::from_byte(0x03).is_ui(), "0x03 is plain UI");
        assert!(Ax25Control::from_byte(0x13).is_ui(), "0x13 is UI with P/F");
        assert!(
            !Ax25Control::from_byte(0x00).is_ui(),
            "0x00 is an I frame, not UI"
        );
        assert!(
            !Ax25Control::from_byte(0x01).is_ui(),
            "0x01 is an S frame, not UI"
        );
    }

    /// SABME remains a one-byte U frame even though it negotiates
    /// modulo-128 operation for subsequent I and S frames.
    #[test]
    fn sabme_is_classified_as_a_known_unnumbered_frame() {
        assert_eq!(
            Ax25Control::from_byte(0x6F),
            Ax25Control::Unnumbered {
                kind: UnnumberedKind::SetAsyncBalancedModeExtended,
                pf: PollFinal::Clear,
            },
            "SABME is a U frame, not a two-byte extended control field"
        );
    }

    #[test]
    fn every_control_byte_survives_typed_decode_and_encode() {
        for byte in 0..=u8::MAX {
            assert_eq!(
                u8::from(Ax25Control::from(byte)),
                byte,
                "control byte {byte:#04x}"
            );
        }
    }

    #[test]
    fn unknown_unnumbered_constructor_accepts_only_canonical_unassigned_patterns() {
        for byte in 0..=u8::MAX {
            let is_canonical_unknown =
                byte & 0x03 == 0x03 && byte & 0x10 == 0 && !is_known_unnumbered_kind(byte);
            assert_eq!(
                UnknownUnnumberedKind::try_from(byte).is_ok(),
                is_canonical_unknown,
                "unknown U-frame modifier {byte:#04x}",
            );
        }
    }

    #[test]
    fn sequence_conversions_preserve_the_validated_domain() -> TestResult {
        for value in 0..=u8::MAX {
            let converted = Ax25SequenceNumber::try_from(value);
            assert_eq!(
                converted,
                Ax25SequenceNumber::new(value),
                "sequence {value}"
            );
            if value <= Ax25SequenceNumber::MAX.get() {
                assert_eq!(u8::from(converted?), value);
            }
        }
        Ok(())
    }

    /// Exhaustive family classification over the full byte space:
    /// bit 0 = 0 → I, bits 1..0 = 01 → S, bits 1..0 = 11 → U.
    #[test]
    fn every_byte_classifies_into_the_spec_family() {
        for b in 0..=255u8 {
            let parsed = Ax25Control::from_byte(b);
            match (b & 0x01, b & 0x03) {
                (0, _) => assert!(
                    matches!(parsed, Ax25Control::Information { .. }),
                    "byte {b:#04x} must classify as I"
                ),
                (_, 0x01) => assert!(
                    matches!(parsed, Ax25Control::Supervisory { .. }),
                    "byte {b:#04x} must classify as S"
                ),
                _ => assert!(
                    matches!(parsed, Ax25Control::Unnumbered { .. }),
                    "byte {b:#04x} must classify as U"
                ),
            }
        }
    }
}

/// Command/Response classification of an AX.25 frame.
///
/// Per AX.25 v2.2 §6.1.2, the C-bit on the destination SSID byte and
/// the C-bit on the source SSID byte together encode whether a frame
/// is a command or a response:
///
/// - `(dest_c=1, src_c=0)` → [`Self::Command`] (APRS frames)
/// - `(dest_c=0, src_c=1)` → [`Self::Response`]
/// - both equal → a pre-v2.0 frame; the two legacy encodings remain distinct
///   so parse/build round trips preserve the address bits.
///
/// See the [packet example](crate#encoding-and-decoding-a-packet).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CommandResponse {
    /// AX.25 v2.2 Command frame.
    Command,
    /// AX.25 v2.2 Response frame.
    Response,
    /// Pre-v2.0 frame with both C bits clear.
    LegacyBothClear,
    /// Pre-v2.0 frame with both C bits set.
    LegacyBothSet,
}
