//! AX.25 control-field classification per v2.2 §4.3.

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
/// Only UI is commonly seen in practice, but we parse the full family.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Ax25Control {
    /// Information frame (I).
    Information {
        /// Numbered send sequence (N(S)).
        ns: u8,
        /// Numbered receive sequence (N(R)).
        nr: u8,
        /// Poll/final bit.
        pf: bool,
    },
    /// Supervisory frame (S) with sub-kind.
    Supervisory {
        /// Supervisory sub-kind (RR / RNR / REJ / SREJ).
        kind: SupervisoryKind,
        /// Numbered receive sequence (N(R)).
        nr: u8,
        /// Poll/final bit.
        pf: bool,
    },
    /// Unnumbered frame (U) with sub-kind.
    Unnumbered {
        /// Unnumbered sub-kind (UI / SABM / DISC / DM / UA / FRMR / XID / TEST).
        kind: UnnumberedKind,
        /// Poll/final bit.
        pf: bool,
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
                ns: (b >> 1) & 0x07,
                nr: (b >> 5) & 0x07,
                pf: (b & 0x10) != 0,
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
                nr: (b >> 5) & 0x07,
                pf: (b & 0x10) != 0,
            };
        }
        // Otherwise Unnumbered (bits 0-1 = 11)
        let pf = (b & 0x10) != 0;
        let kind_bits = b & 0xEF; // mask off P/F bit
        let kind = match kind_bits {
            0x03 => UnnumberedKind::UnnumberedInformation,
            0x2F => UnnumberedKind::SetAsyncBalancedMode,
            0x43 => UnnumberedKind::Disconnect,
            0x0F => UnnumberedKind::DisconnectedMode,
            0x63 => UnnumberedKind::UnnumberedAcknowledge,
            0x87 => UnnumberedKind::FrameReject,
            0xAF => UnnumberedKind::ExchangeIdentification,
            0xE3 => UnnumberedKind::Test,
            other => UnnumberedKind::Other(other),
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
}

/// Supervisory (S) frame sub-kinds (AX.25 v2.2 §4.3.2).
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum UnnumberedKind {
    /// Unnumbered Information (UI), used by APRS.
    UnnumberedInformation,
    /// Set Asynchronous Balanced Mode (SABM).
    SetAsyncBalancedMode,
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
    Other(u8),
}

#[cfg(test)]
mod tests {
    use super::{Ax25Control, SupervisoryKind, UnnumberedKind};

    /// Golden decode table for every U-frame kind per AX.25 v2.2
    /// §4.3.3, with the P/F bit both ways. The kind bytes live in a
    /// hand-maintained match: a DISC/UA transposition (0x43 ↔ 0x63,
    /// visually adjacent) would pass the packet round-trip suite,
    /// because `Ax25Packet` keeps the control byte raw and nothing
    /// else inspects the typed decode.
    #[test]
    fn unnumbered_kind_table_matches_spec() {
        let cases: [(u8, UnnumberedKind); 8] = [
            (0x03, UnnumberedKind::UnnumberedInformation),
            (0x2F, UnnumberedKind::SetAsyncBalancedMode),
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
                        pf: pf_bit != 0,
                    },
                    "U-frame control byte {byte:#04x}"
                );
            }
        }
    }

    /// Golden decode table for the four S-frame kinds per §4.3.2,
    /// across representative N(R) values and both P/F states.
    #[test]
    fn supervisory_kind_table_matches_spec() {
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
                            nr,
                            pf: pf_bit != 0,
                        },
                        "S-frame control byte {byte:#04x}"
                    );
                }
            }
        }
    }

    /// I-frame N(S)/N(R)/P-F extraction per §4.3.1: N(S) in bits 1-3,
    /// P/F in bit 4, N(R) in bits 5-7.
    #[test]
    fn information_fields_extract_per_spec() {
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
                Ax25Control::Information { ns, nr, pf },
                "I-frame control byte {byte:#04x}"
            );
        }
    }

    /// UI classification must mask the P/F bit: 0x13 is UI exactly
    /// like 0x03 (real RF traffic carries both), while an I-frame
    /// byte is never UI.
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

    /// SABME (0x6F) deliberately stays unclassified: modulo-128
    /// extended mode is out of this crate's scope, so the byte must
    /// fall through to `Other` rather than gain a variant by
    /// accident.
    #[test]
    fn sabme_stays_unclassified_other() {
        assert_eq!(
            Ax25Control::from_byte(0x6F),
            Ax25Control::Unnumbered {
                kind: UnnumberedKind::Other(0x6F),
                pf: false,
            },
            "SABME must remain Other while modulo-128 is out of scope"
        );
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
/// - both equal → legacy v2.0 / unknown, represented in
///   [`crate::Ax25Packet::command_or_response`] as `None`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CommandResponse {
    /// AX.25 v2.2 Command frame.
    Command,
    /// AX.25 v2.2 Response frame.
    Response,
}
