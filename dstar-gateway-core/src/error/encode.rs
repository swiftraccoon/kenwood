//! `EncodeError` for fixed-buffer encoders.

/// Errors raised by `encode_*` functions when the supplied output
/// buffer is too small for the packet they are asked to write.
#[derive(Debug, Clone, Copy, thiserror::Error, PartialEq, Eq)]
#[non_exhaustive]
pub enum EncodeError {
    /// Output buffer was smaller than the encoded packet length.
    #[error("output buffer too small: needed {need} bytes, have {have}")]
    BufferTooSmall {
        /// How many bytes the encoder needed to write.
        need: usize,
        /// How many bytes the buffer can hold.
        have: usize,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buffer_too_small_display() {
        let err = EncodeError::BufferTooSmall { need: 32, have: 16 };
        let s = err.to_string();
        assert!(s.contains("32"));
        assert!(s.contains("16"));
    }
}
