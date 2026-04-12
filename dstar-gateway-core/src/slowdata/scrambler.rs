//! Slow data XOR scrambler.
//!
//! Reference: `ircDDBGateway/Common/DStarDefines.h:111-113`
//! (`SCRAMBLER_BYTE1 = 0x70U`, `SCRAMBLER_BYTE2 = 0x4FU`,
//! `SCRAMBLER_BYTE3 = 0x93U`).

const SCRAMBLER_KEY: [u8; 3] = [0x70, 0x4F, 0x93];

/// Descramble 3 bytes of slow data using the D-STAR XOR key.
#[must_use]
pub const fn descramble(bytes: [u8; 3]) -> [u8; 3] {
    [
        bytes[0] ^ SCRAMBLER_KEY[0],
        bytes[1] ^ SCRAMBLER_KEY[1],
        bytes[2] ^ SCRAMBLER_KEY[2],
    ]
}

/// Scramble 3 bytes of slow data using the D-STAR XOR key.
///
/// `scramble(descramble(b)) == b` for any input, since XOR is symmetric.
#[must_use]
pub const fn scramble(bytes: [u8; 3]) -> [u8; 3] {
    descramble(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn descramble_then_scramble_roundtrips() {
        // Exhaustive over the first 4096 possible inputs is too much.
        // Pick a sampling of byte patterns.
        for a in [0u8, 1, 0x42, 0x7F, 0x80, 0xFE, 0xFF] {
            for b in [0u8, 1, 0x42, 0x7F, 0x80, 0xFE, 0xFF] {
                for c in [0u8, 1, 0x42, 0x7F, 0x80, 0xFE, 0xFF] {
                    let input = [a, b, c];
                    assert_eq!(
                        scramble(descramble(input)),
                        input,
                        "roundtrip failed for {input:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn descramble_zero_returns_key() {
        assert_eq!(descramble([0u8; 3]), [0x70, 0x4F, 0x93]);
    }

    #[test]
    fn scramble_zero_returns_key() {
        assert_eq!(scramble([0u8; 3]), [0x70, 0x4F, 0x93]);
    }

    #[test]
    fn scramble_key_returns_zero() {
        assert_eq!(scramble([0x70, 0x4F, 0x93]), [0; 3]);
    }
}
