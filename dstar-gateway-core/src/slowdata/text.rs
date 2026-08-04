//! Lossless D-STAR slow-data text values.

use crate::types::WireTextError;
use crate::types::wire_text::trimmed_printable_ascii;

use super::text_collector::MAX_MESSAGE_LEN;

/// A string cannot be represented by a D-STAR slow-data text message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum SlowDataTextMessageError {
    /// The UTF-8 input occupies more bytes than the fixed-width wire field.
    #[error("slow-data text is {length} bytes; maximum is {maximum} bytes")]
    TooLong {
        /// Exact byte length supplied by the caller.
        length: usize,
        /// Maximum wire length accepted by the protocol.
        maximum: usize,
    },
    /// The input contains a byte outside printable ASCII.
    #[error(transparent)]
    InvalidText(#[from] WireTextError),
}

/// One complete fixed-width D-STAR slow-data text message.
///
/// The receive path stores all 20 wire bytes exactly, including padding and
/// malformed bytes. [`Self::text`] is a separate validated view, so callers
/// cannot mistake replacement characters for bytes actually sent over RF.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct SlowDataTextMessage([u8; MAX_MESSAGE_LEN]);

impl SlowDataTextMessage {
    /// Construct a transmit message from printable ASCII text.
    ///
    /// The input is preserved exactly and padded on the right with ASCII
    /// spaces to the 20-byte wire width. Empty text is a valid all-space
    /// message; callers that mean “send no message” should use `Option`.
    ///
    /// # Errors
    ///
    /// Returns [`SlowDataTextMessageError::TooLong`] when `text` occupies
    /// more than 20 bytes, or [`SlowDataTextMessageError::InvalidText`] at
    /// the first byte outside printable ASCII.
    pub fn try_from_text(text: &str) -> Result<Self, SlowDataTextMessageError> {
        let text_bytes = text.as_bytes();
        if text_bytes.len() > MAX_MESSAGE_LEN {
            return Err(SlowDataTextMessageError::TooLong {
                length: text_bytes.len(),
                maximum: MAX_MESSAGE_LEN,
            });
        }

        for (index, byte) in text_bytes.iter().copied().enumerate() {
            if !(b' '..=b'~').contains(&byte) {
                return Err(WireTextError { index, byte }.into());
            }
        }

        let mut bytes = [b' '; MAX_MESSAGE_LEN];
        let Some(destination) = bytes.get_mut(..text_bytes.len()) else {
            unreachable!("validated text length fits the fixed-width message");
        };
        destination.copy_from_slice(text_bytes);
        Ok(Self(bytes))
    }

    /// Preserve one complete message exactly as received from the wire.
    #[must_use]
    pub const fn from_wire_bytes(bytes: [u8; MAX_MESSAGE_LEN]) -> Self {
        Self(bytes)
    }

    /// Return the exact fixed-width wire bytes, including trailing padding.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; MAX_MESSAGE_LEN] {
        &self.0
    }

    /// Consume the value and return its exact fixed-width wire bytes.
    #[must_use]
    pub const fn into_bytes(self) -> [u8; MAX_MESSAGE_LEN] {
        self.0
    }

    /// Return validated text without trailing ASCII-space padding.
    ///
    /// Leading and interior spaces are meaningful and remain intact.
    ///
    /// # Errors
    ///
    /// Returns [`WireTextError`] at the first byte outside printable ASCII.
    pub fn text(&self) -> Result<&str, WireTextError> {
        trimmed_printable_ascii(&self.0)
    }
}

impl TryFrom<&str> for SlowDataTextMessage {
    type Error = SlowDataTextMessageError;

    fn try_from(text: &str) -> Result<Self, Self::Error> {
        Self::try_from_text(text)
    }
}

impl AsRef<[u8; MAX_MESSAGE_LEN]> for SlowDataTextMessage {
    fn as_ref(&self) -> &[u8; MAX_MESSAGE_LEN] {
        self.as_bytes()
    }
}

impl std::ops::Deref for SlowDataTextMessage {
    type Target = [u8; MAX_MESSAGE_LEN];

    fn deref(&self) -> &Self::Target {
        self.as_bytes()
    }
}

impl std::fmt::Debug for SlowDataTextMessage {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SlowDataTextMessage")
            .field("bytes", &self.0)
            .field("text", &self.text())
            .finish()
    }
}

/// One five-byte slow-data text block with exact receive bytes.
///
/// This is the block-level representation produced by
/// [`super::SlowDataAssembler`]. A complete radio message is represented by
/// [`SlowDataTextMessage`] instead.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlowDataText {
    bytes: Vec<u8>,
}

impl SlowDataText {
    /// Preserve one decoded block payload exactly.
    #[must_use]
    pub const fn from_wire_bytes(bytes: Vec<u8>) -> Self {
        Self { bytes }
    }

    /// Return the exact decoded block payload.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Return validated text without trailing ASCII-space padding.
    ///
    /// # Errors
    ///
    /// Returns [`WireTextError`] at the first byte outside printable ASCII.
    pub fn text(&self) -> Result<&str, WireTextError> {
        trimmed_printable_ascii(&self.bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn complete_message_preserves_bytes_and_validates_text() {
        let message = SlowDataTextMessage::from_wire_bytes(*b"CQ TEST             ");
        assert_eq!(message.as_bytes(), b"CQ TEST             ");
        assert_eq!(message.text(), Ok("CQ TEST"));
    }

    #[test]
    fn transmit_constructor_preserves_text_and_pads_to_wire_width() -> TestResult {
        let message = SlowDataTextMessage::try_from_text(" A B")?;
        assert_eq!(message.as_bytes(), b" A B                ");
        assert_eq!(message.text(), Ok(" A B"));
        Ok(())
    }

    #[test]
    fn transmit_constructor_represents_empty_text_as_all_spaces() -> TestResult {
        let message = SlowDataTextMessage::try_from_text("")?;
        assert_eq!(message.as_bytes(), b"                    ");
        assert_eq!(message.text(), Ok(""));
        Ok(())
    }

    #[test]
    fn transmit_constructor_rejects_oversize_input_without_truncation() {
        assert_eq!(
            SlowDataTextMessage::try_from_text("123456789012345678901"),
            Err(SlowDataTextMessageError::TooLong {
                length: 21,
                maximum: 20,
            })
        );
    }

    #[test]
    fn transmit_constructor_rejects_non_printable_input_at_exact_byte() {
        assert_eq!(
            SlowDataTextMessage::try_from_text("A\nB"),
            Err(SlowDataTextMessageError::InvalidText(WireTextError {
                index: 1,
                byte: b'\n',
            }))
        );
        assert_eq!(
            SlowDataTextMessage::try_from_text("Aé"),
            Err(SlowDataTextMessageError::InvalidText(WireTextError {
                index: 1,
                byte: 0xC3,
            }))
        );
    }

    #[test]
    fn complete_message_reports_invalid_byte_without_replacement() {
        let mut bytes = *b"CQ TEST             ";
        bytes[2] = 0xFF;
        let message = SlowDataTextMessage::from_wire_bytes(bytes);
        assert_eq!(message.as_bytes(), &bytes);
        assert_eq!(
            message.text(),
            Err(WireTextError {
                index: 2,
                byte: 0xFF,
            })
        );
    }

    #[test]
    fn block_text_preserves_control_byte_and_rejects_text_view() {
        let text = SlowDataText::from_wire_bytes(vec![b'A', 0, b'B']);
        assert_eq!(text.as_bytes(), &[b'A', 0, b'B']);
        assert_eq!(text.text(), Err(WireTextError { index: 1, byte: 0 }));
    }
}
