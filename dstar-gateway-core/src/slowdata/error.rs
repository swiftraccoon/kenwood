//! Errors raised by the slow data assembler.

/// Slow data assembly errors.
#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq)]
#[non_exhaustive]
pub enum SlowDataError {
    /// Block length byte (low nibble of byte 0) is zero.
    #[error("slow data block has zero length")]
    ZeroLength,

    /// Block length exceeds the buffer capacity (max 18 bytes after the type byte).
    #[error("slow data block length {got} exceeds max 18")]
    LengthOverflow {
        /// Observed length value.
        got: usize,
    },

    /// Buffer for the assembler ran out of room — should not happen
    /// in well-formed input.
    #[error("slow data assembler internal buffer overflow")]
    BufferOverflow,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_length_display() {
        let err = SlowDataError::ZeroLength;
        assert!(err.to_string().contains("zero length"));
    }

    #[test]
    fn length_overflow_display() {
        let err = SlowDataError::LengthOverflow { got: 25 };
        assert!(err.to_string().contains("25"));
    }

    #[test]
    fn buffer_overflow_display() {
        let err = SlowDataError::BufferOverflow;
        assert_eq!(
            err.to_string(),
            "slow data assembler internal buffer overflow"
        );
    }
}
