//! Structured warnings emitted by the lenient parsers.
//!
//! This file holds the cross-protocol union of diagnostic variants.
//! Every protocol codec (`DPlus`, `DExtra`, DCS) contributes the variants
//! its parser can fire, but the type is shared so a single
//! `DiagnosticSink` can route diagnostics across all protocols without
//! branching on codec.

use std::net::SocketAddr;
use std::time::Duration;

use crate::types::{Callsign, ProtocolKind, StreamId};

/// One observable malformation detected by a lenient parser.
///
/// **Lenient parsing rule**: the codec must NEVER reject a packet
/// solely because of a recoverable malformation. Every malformation
/// becomes a `Diagnostic` and the packet still parses with whatever
/// content was extractable. Strict-mode is implemented in the
/// consumer's `DiagnosticSink`, never in the parser.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Diagnostic {
    // ─── Header (any DSVT-framed protocol) ──────────────────────
    /// Header CRC does not match recomputed CRC.
    HeaderCrcMismatch {
        /// Originating protocol.
        protocol: ProtocolKind,
        /// CRC computed from the wire bytes.
        computed: u16,
        /// CRC carried in bytes 39-40 of the header.
        on_wire: u16,
        /// MY callsign from the header (for log correlation).
        my_call: Callsign,
    },

    /// Header carries non-zero flag bytes (the DSVT-embedded form
    /// is supposed to zero them — see `HeaderData.cpp:665-667`).
    HeaderFlagsNonZero {
        /// Originating protocol.
        protocol: ProtocolKind,
        /// Observed flag1.
        flag1: u8,
        /// Observed flag2.
        flag2: u8,
        /// Observed flag3.
        flag3: u8,
    },

    /// Callsign field contains a byte outside ASCII printable range.
    CallsignNonPrintable {
        /// Originating protocol.
        protocol: ProtocolKind,
        /// Which callsign field (rpt2 / rpt1 / ur / my / suffix).
        field: CallsignField,
        /// Byte offset within the 8-byte (or 4-byte for suffix) field.
        offset_in_field: u8,
        /// The non-printable byte observed.
        byte: u8,
    },

    // ─── Voice / DSVT ───────────────────────────────────────────
    /// Stream id flipped mid-stream without an EOT in between.
    StreamIdSwitchWithoutEot {
        /// Originating protocol.
        protocol: ProtocolKind,
        /// Previous stream id (now abandoned).
        previous: StreamId,
        /// New stream id (just observed).
        new: StreamId,
        /// How long since the last frame on `previous`.
        elapsed_since_last_frame: Duration,
    },

    /// Voice data sequence number outside 0..21.
    VoiceSeqOutOfRange {
        /// Originating protocol.
        protocol: ProtocolKind,
        /// The stream the seq belongs to.
        stream_id: StreamId,
        /// The out-of-range seq value (0x40 bit stripped).
        got: u8,
    },

    /// Voice data with bit 0x40 set on a non-EOT-length packet, or
    /// EOT-length packet without bit 0x40 set.
    VoiceEotBitMismatch {
        /// Originating protocol.
        protocol: ProtocolKind,
        /// Total packet length (including `DPlus` prefix where applicable).
        packet_len: usize,
        /// Observed seq byte.
        seq_byte: u8,
    },

    /// EOT trailer pattern doesn't match `0x55 0x55 0x55 0x55 0xC8 0x7A`.
    /// `xlxd` writes a different trailer; reflectors talking to xlxd
    /// may emit either form.
    VoiceEotTrailerMismatch {
        /// Originating protocol.
        protocol: ProtocolKind,
        /// Last 6 bytes of the EOT packet as observed.
        observed: [u8; 6],
    },

    // ─── DPlus connect / state machine ──────────────────────────
    /// LINK1 ACK arrived after LINK2 was already sent.
    DuplicateLink1Ack {
        /// The peer that sent the duplicate.
        peer: SocketAddr,
    },

    /// 8-byte LINK2 reply at offsets `[4..8]` is neither OKRW nor BUSY.
    UnknownLink2Reply {
        /// The 4-byte tag (typically interpreted as ASCII).
        reply: [u8; 4],
    },

    // ─── DPlus auth ─────────────────────────────────────────────
    /// Auth response chunk had a record dropped per the lenient filter.
    AuthHostSkipped {
        /// Byte offset of the dropped record.
        offset: usize,
        /// Why it was dropped.
        reason: AuthHostSkipReason,
    },

    /// Auth chunk had trailing bytes that don't form a complete record.
    AuthChunkTrailingBytes {
        /// Byte offset where the trailing bytes started.
        offset: usize,
        /// How many trailing bytes there were.
        bytes: usize,
    },
}

/// Which callsign field a [`Diagnostic::CallsignNonPrintable`] refers to.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallsignField {
    /// RPT2 (gateway repeater).
    Rpt2,
    /// RPT1 (access repeater).
    Rpt1,
    /// YOUR callsign.
    Ur,
    /// MY callsign.
    My,
    /// MY suffix.
    MySuffix,
    /// `Authenticate` packet callsign field.
    Authenticate,
    /// LINK request callsign field.
    LinkRequest,
}

/// Reason an auth host record was skipped during chunk parsing.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthHostSkipReason {
    /// Record's active flag (high bit of byte 25) was clear.
    Inactive,
    /// Callsign field starts with `XRF` — the reference filters these
    /// out before caching.
    XrfPrefix,
    /// IP field is empty after trimming.
    EmptyIp,
    /// Callsign field is empty after trimming.
    EmptyCallsign,
    /// IP field couldn't be parsed as IPv4 ASCII.
    MalformedIp,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diagnostic_header_crc_mismatch_constructible() {
        let cs = Callsign::from_wire_bytes(*b"W1AW    ");
        let d = Diagnostic::HeaderCrcMismatch {
            protocol: ProtocolKind::DPlus,
            computed: 0x1073,
            on_wire: 0xFFFF,
            my_call: cs,
        };
        assert!(matches!(d, Diagnostic::HeaderCrcMismatch { .. }));
    }

    #[test]
    fn diagnostic_callsign_non_printable_carries_field() {
        let d = Diagnostic::CallsignNonPrintable {
            protocol: ProtocolKind::DPlus,
            field: CallsignField::My,
            offset_in_field: 0,
            byte: 0xC3,
        };
        assert!(
            matches!(
                d,
                Diagnostic::CallsignNonPrintable {
                    field: CallsignField::My,
                    byte: 0xC3,
                    ..
                }
            ),
            "expected CallsignNonPrintable {{ My, 0xC3 }}, got {d:?}"
        );
    }

    #[test]
    fn diagnostic_auth_host_skipped_inactive() {
        let d = Diagnostic::AuthHostSkipped {
            offset: 0,
            reason: AuthHostSkipReason::Inactive,
        };
        assert!(
            matches!(
                d,
                Diagnostic::AuthHostSkipped {
                    reason: AuthHostSkipReason::Inactive,
                    ..
                }
            ),
            "expected AuthHostSkipped {{ Inactive }}, got {d:?}"
        );
    }
}
