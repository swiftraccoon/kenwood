//! Validated, byte-preserving APRS-IS uplink lines.

/// Maximum APRS-IS line length on the wire, in bytes, including the
/// trailing `CRLF`.
///
/// Per <https://www.aprs-is.net/Connecting.aspx>, no line may exceed 512
/// bytes including its `CRLF` framing.
pub const MAX_IS_LINE_BYTES: usize = 512;

/// Why bytes cannot form one safe APRS-IS uplink line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum AprsIsUplinkLineError {
    /// Wire bytes did not end in exactly one `CRLF` terminator.
    #[error("APRS-IS uplink bytes must end with CRLF")]
    MissingCrlf,

    /// The line body contained a carriage return or line feed.
    ///
    /// Allowing either byte in the body would split one caller-supplied
    /// write into multiple APRS-IS lines.
    #[error("APRS-IS uplink body contains framing byte 0x{byte:02X} at byte offset {offset}")]
    EmbeddedNewline {
        /// Zero-based byte offset in the unframed line body.
        offset: usize,
        /// The rejected byte, either carriage return or line feed.
        byte: u8,
    },

    /// The framed line exceeded the APRS-IS 512-byte wire limit.
    #[error("APRS-IS uplink line is {actual} bytes; the maximum is {max}")]
    TooLong {
        /// Actual line length including `CRLF` framing.
        actual: usize,
        /// Protocol maximum, currently [`MAX_IS_LINE_BYTES`].
        max: usize,
    },
}

/// One validated APRS-IS line in its exact wire representation.
///
/// The stored bytes:
///
/// - end with exactly one `CRLF`,
/// - contain no carriage return or line feed in the body, and
/// - are no longer than [`MAX_IS_LINE_BYTES`].
///
/// Unlike a `String`, this type preserves all non-UTF-8 APRS information
/// bytes. This matters for Mic-E and other binary-compatible RF formats.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct AprsIsUplinkLine(Vec<u8>);

impl AprsIsUplinkLine {
    /// Build a wire line from unframed body bytes.
    ///
    /// This appends the one required `CRLF` terminator. The input must
    /// not already contain framing bytes.
    ///
    /// # Errors
    ///
    /// Returns [`AprsIsUplinkLineError::EmbeddedNewline`] when `body`
    /// contains carriage return or line feed, or
    /// [`AprsIsUplinkLineError::TooLong`] when the body plus `CRLF`
    /// exceeds [`MAX_IS_LINE_BYTES`].
    pub fn from_body_bytes(body: &[u8]) -> Result<Self, AprsIsUplinkLineError> {
        let wire_len = body
            .len()
            .checked_add(2)
            .ok_or(AprsIsUplinkLineError::TooLong {
                actual: usize::MAX,
                max: MAX_IS_LINE_BYTES,
            })?;
        if wire_len > MAX_IS_LINE_BYTES {
            return Err(AprsIsUplinkLineError::TooLong {
                actual: wire_len,
                max: MAX_IS_LINE_BYTES,
            });
        }
        validate_body(body)?;

        let mut wire = Vec::with_capacity(wire_len);
        wire.extend_from_slice(body);
        wire.extend_from_slice(b"\r\n");
        Ok(Self(wire))
    }

    /// Validate bytes that already carry their wire framing.
    ///
    /// The input must end with exactly one `CRLF`. Missing framing,
    /// LF-only framing, and any earlier carriage return or line feed are
    /// rejected rather than normalized.
    ///
    /// # Errors
    ///
    /// Returns [`AprsIsUplinkLineError::MissingCrlf`] when the exact
    /// terminator is absent, [`AprsIsUplinkLineError::EmbeddedNewline`]
    /// when the body contains another framing byte, or
    /// [`AprsIsUplinkLineError::TooLong`] when the input exceeds
    /// [`MAX_IS_LINE_BYTES`].
    pub fn from_wire_bytes(wire: &[u8]) -> Result<Self, AprsIsUplinkLineError> {
        if wire.len() > MAX_IS_LINE_BYTES {
            return Err(AprsIsUplinkLineError::TooLong {
                actual: wire.len(),
                max: MAX_IS_LINE_BYTES,
            });
        }
        let Some(body) = wire.strip_suffix(b"\r\n") else {
            return Err(AprsIsUplinkLineError::MissingCrlf);
        };
        validate_body(body)?;
        Ok(Self(wire.to_vec()))
    }

    /// Borrow the complete, CRLF-terminated wire bytes.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    /// Borrow the line body without its trailing `CRLF`.
    #[must_use]
    pub fn body_bytes(&self) -> &[u8] {
        self.0
            .strip_suffix(b"\r\n")
            .unwrap_or_else(|| unreachable!("AprsIsUplinkLine always stores CRLF framing"))
    }

    /// Consume the line and return its complete wire bytes.
    #[must_use]
    pub fn into_bytes(self) -> Vec<u8> {
        self.0
    }
}

impl AsRef<[u8]> for AprsIsUplinkLine {
    fn as_ref(&self) -> &[u8] {
        self.as_bytes()
    }
}

fn validate_body(body: &[u8]) -> Result<(), AprsIsUplinkLineError> {
    if let Some((offset, &byte)) = body
        .iter()
        .enumerate()
        .find(|(_, byte)| matches!(byte, b'\r' | b'\n'))
    {
        return Err(AprsIsUplinkLineError::EmbeddedNewline { offset, byte });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn body_constructor_preserves_non_utf8_bytes_and_adds_crlf() -> TestResult {
        let body = [
            b'N', b'0', b'C', b'A', b'L', b'L', b'>', b'A', b':', 0xC1, 0x82,
        ];
        let line = AprsIsUplinkLine::from_body_bytes(&body)?;

        let mut expected = body.to_vec();
        expected.extend_from_slice(b"\r\n");
        assert_eq!(line.body_bytes(), body);
        assert_eq!(line.as_bytes(), expected);
        Ok(())
    }

    #[test]
    fn wire_constructor_requires_exact_crlf() {
        for invalid in [
            b"N0CALL>APRS:test".as_slice(),
            b"N0CALL>APRS:test\n".as_slice(),
            b"N0CALL>APRS:test\r".as_slice(),
        ] {
            assert_eq!(
                AprsIsUplinkLine::from_wire_bytes(invalid),
                Err(AprsIsUplinkLineError::MissingCrlf)
            );
        }
    }

    #[test]
    fn embedded_framing_bytes_are_rejected_with_their_offset() {
        assert_eq!(
            AprsIsUplinkLine::from_body_bytes(b"good\r\nEVIL>X:forged"),
            Err(AprsIsUplinkLineError::EmbeddedNewline {
                offset: 4,
                byte: b'\r',
            })
        );
        assert_eq!(
            AprsIsUplinkLine::from_wire_bytes(b"good\r\n\r\n"),
            Err(AprsIsUplinkLineError::EmbeddedNewline {
                offset: 4,
                byte: b'\r',
            })
        );
    }

    #[test]
    fn length_limit_includes_crlf() -> TestResult {
        let maximum_body = vec![b'A'; MAX_IS_LINE_BYTES - 2];
        let maximum = AprsIsUplinkLine::from_body_bytes(&maximum_body)?;
        assert_eq!(maximum.as_bytes().len(), MAX_IS_LINE_BYTES);

        let oversized_body = vec![b'A'; MAX_IS_LINE_BYTES - 1];
        assert_eq!(
            AprsIsUplinkLine::from_body_bytes(&oversized_body),
            Err(AprsIsUplinkLineError::TooLong {
                actual: MAX_IS_LINE_BYTES + 1,
                max: MAX_IS_LINE_BYTES,
            })
        );
        Ok(())
    }
}
