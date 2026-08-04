//! Validation for textual views of lossless D-STAR wire fields.

/// A byte in a D-STAR wire field cannot be represented as protocol text.
///
/// Receive-side types retain every byte exactly. This error is returned only
/// when a caller asks for a textual view of bytes that are not printable
/// ASCII.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("wire text byte {byte:#04x} at index {index} is not printable ASCII")]
pub struct WireTextError {
    /// Zero-based position of the invalid byte.
    pub index: usize,
    /// Exact byte found at `index`.
    pub byte: u8,
}

/// Validate printable ASCII and remove only trailing ASCII-space padding.
pub(crate) fn trimmed_printable_ascii(bytes: &[u8]) -> Result<&str, WireTextError> {
    for (index, byte) in bytes.iter().copied().enumerate() {
        if !(b' '..=b'~').contains(&byte) {
            return Err(WireTextError { index, byte });
        }
    }

    let end = bytes
        .iter()
        .rposition(|byte| *byte != b' ')
        .map_or(0, |position| position + 1);
    let Some(text_bytes) = bytes.get(..end) else {
        unreachable!("the computed padding boundary is within the validated slice");
    };
    std::str::from_utf8(text_bytes)
        .map_or_else(|_| unreachable!("printable ASCII is valid UTF-8"), Ok)
}

/// Render every byte as hex for compatibility-oriented diagnostic views.
pub(crate) fn diagnostic_wire_bytes(bytes: &[u8]) -> String {
    let hexadecimal = bytes
        .iter()
        .map(|byte| format!("{byte:02X}"))
        .collect::<Vec<_>>()
        .join(" ");
    format!("<invalid wire bytes: {hexadecimal}>")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn returns_text_without_trailing_padding() {
        assert_eq!(trimmed_printable_ascii(b"A B   "), Ok("A B"));
    }

    #[test]
    fn preserves_leading_and_interior_spaces() {
        assert_eq!(trimmed_printable_ascii(b" A B "), Ok(" A B"));
    }

    #[test]
    fn reports_the_exact_invalid_byte() {
        assert_eq!(
            trimmed_printable_ascii(&[b'A', 0x80, b'B']),
            Err(WireTextError {
                index: 1,
                byte: 0x80,
            })
        );
    }

    #[test]
    fn rejects_ascii_control_bytes() {
        assert_eq!(
            trimmed_printable_ascii(b"A\n"),
            Err(WireTextError {
                index: 1,
                byte: b'\n',
            })
        );
    }

    #[test]
    fn diagnostic_view_preserves_every_byte() {
        assert_eq!(
            diagnostic_wire_bytes(&[b'A', 0x80, b' ']),
            "<invalid wire bytes: 41 80 20>"
        );
    }
}
