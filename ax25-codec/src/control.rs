//! AX.25 control-field classification per v2.2 §4.3.

/// AX.25 control-field frame-type family.
///
/// Per AX.25 v2.2 §4.2, the control byte identifies one of three frame
/// families:
/// - **Information (I)** — numbered data transfer frames
/// - **Supervisory (S)** — flow-control frames (RR, RNR, REJ, SREJ)
/// - **Unnumbered (U)** — link-setup, disconnection, and **UI** frames
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
    /// Unnumbered Information (UI) — used by APRS.
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

/// Command/Response classification of an AX.25 address pair.
///
/// Per AX.25 v2.2 §4.3.1.2, bit 7 of the destination SSID byte and bit 7
/// of the source SSID byte together encode whether the frame is a command
/// or response. APRS only sends commands, but we parse both for
/// completeness.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CommandResponse {
    /// AX.25 v2.2 Command frame (dest C-bit=1, source C-bit=0).
    Command,
    /// AX.25 v2.2 Response frame (dest C-bit=0, source C-bit=1).
    Response,
    /// Legacy AX.25 v2.0 or unknown (both C-bits equal).
    Legacy,
}
