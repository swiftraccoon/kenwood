//! DPRS CRC — CRC-CCITT over the DPRS payload bytes.
//!
//! Reference: `ircDDBGateway/Common/APRSCollector.cpp:371-394`
//! (`CAPRSCollector::calcCRC`). The algorithm is:
//!
//! - initial value `0xFFFF`
//! - reflected polynomial `0x8408`
//! - final output `~accumulator & 0xFFFF` (bitwise NOT)
//!
//! This is identical to the D-STAR radio-header CRC-CCITT used in
//! [`crate::header::crc_ccitt`], so this module simply delegates.

use crate::header::crc_ccitt;

/// Compute the DPRS CRC-CCITT over the given bytes.
///
/// Matches `CAPRSCollector::calcCRC` from ircDDBGateway: reflected
/// polynomial `0x8408`, initial value `0xFFFF`, final `~accumulator`.
/// Equivalent to [`crate::header::crc_ccitt`].
///
/// # Example
/// ```
/// # use dstar_gateway_core::dprs::compute_crc;
/// // The empty-bytes CRC of CCITT with 0xFFFF init + final NOT is
/// // the canonical 0x0000 (init ^ 0xFFFF).
/// assert_eq!(compute_crc(b""), 0x0000);
/// ```
#[must_use]
pub fn compute_crc(bytes: &[u8]) -> u16 {
    crc_ccitt(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::header::crc_ccitt;

    #[test]
    fn empty_input_matches_header_crc() {
        assert_eq!(compute_crc(b""), crc_ccitt(b""));
    }

    #[test]
    fn short_input_matches_header_crc() {
        assert_eq!(compute_crc(b"TEST"), crc_ccitt(b"TEST"));
    }

    #[test]
    fn dprs_body_matches_header_crc() {
        // A representative DPRS payload — everything after the
        // `$$CRC<4hex>,` prefix up to but not including the final
        // line terminator. The exact input doesn't matter; the point
        // is that the CRC algorithm matches the header CRC.
        let body = b"W1AW    *>APDPRS,DSTAR*:!4041.27N/07229.28W>Test";
        assert_eq!(compute_crc(body), crc_ccitt(body));
    }

    #[test]
    fn empty_crc_is_zero() {
        // CCITT with init 0xFFFF and final `~acc` → ~0xFFFF = 0x0000
        // for the empty input (no bytes to process).
        assert_eq!(compute_crc(b""), 0x0000);
    }

    #[test]
    fn single_byte_crc_is_nonzero() {
        // Sanity: a non-empty input must produce a non-zero CRC
        // (otherwise the placeholder bug would still pass).
        assert_ne!(compute_crc(b"A"), 0);
    }

    #[test]
    fn known_dprs_body_regression() {
        // Regression vector: a fixed DPRS body must have a stable
        // CRC value. Computed by running the reference algorithm
        // (init 0xFFFF, reflected poly 0x8408, final ~acc) and
        // cross-checking with [`crc_ccitt`]. The body is the
        // contents that would follow `$$CRC<4hex>,` in a live
        // sentence — no line terminator, no prefix.
        let body: &[u8] = b"W1AW    *>APDPRS,DSTAR*:!4041.27N/07229.28W>Test";
        let observed = compute_crc(body);
        // Delegate sanity: this must equal `crc_ccitt(body)`.
        assert_eq!(observed, crc_ccitt(body));
        // Locked-in vector to prevent silent regression to a
        // placeholder-zero implementation. This is the value the
        // reference algorithm produces — any future edit that
        // changes it needs a documented reason.
        assert_eq!(observed, 0xB8D9);
    }

    #[test]
    fn single_byte_a_has_known_crc() {
        // Second locked-in vector: the CRC of the single byte `A`
        // is `0xA3F5` under CRC-CCITT init 0xFFFF, reflected poly
        // 0x8408, final `~acc`.
        assert_eq!(compute_crc(b"A"), 0xA3F5);
    }

    #[test]
    fn different_inputs_produce_different_crcs() {
        // Two inputs differing by one byte must produce different
        // CRCs — this would fail under the placeholder implementation
        // that returned 0 regardless of input.
        let a = compute_crc(b"HELLO");
        let b = compute_crc(b"HELLP");
        assert_ne!(a, b);
    }
}
