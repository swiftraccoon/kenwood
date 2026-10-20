//! Text in the memory map's one-byte character form, NUL padded.
//!
//! The names and sub-names of the repeater and hotspot lists, their region,
//! country and group name tables, and the names and memos of the D-STAR
//! callsign list store text this way. A character from U+0020 to U+00FF other
//! than a control character is stored as its one code byte, and one from
//! U+0101 to U+01FF as `0x01` followed by the character's low byte: `ę`
//! (U+0119) is `01 19`. No other character has a stored form; U+0100's low
//! byte would read as padding. On firmware 1.02 every name of a repeater list
//! holding 1,455 records decoded in this form, including 35 with characters
//! above U+007F that match Kenwood's published repeater catalog.

use std::fmt;

#[cfg(test)]
#[path = "stored_text_tests.rs"]
mod tests;

/// The byte that introduces a character from U+0101 to U+01FF.
const LEAD_BYTE: u8 = 0x01;

/// Up to `N` bytes of stored text followed by NUL padding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct StoredText<const N: usize>([u8; N]);

impl<const N: usize> StoredText<N> {
    /// No text: `N` NUL bytes.
    pub const EMPTY: Self = Self([0; N]);

    /// Encode `text`.
    ///
    /// # Errors
    ///
    /// Returns [`StoredTextError::NotStorable`] for a control character,
    /// U+0100, a character above U+01FF, or text whose stored form exceeds
    /// `N` bytes.
    pub fn new(text: &str) -> Result<Self, StoredTextError> {
        let refused = || StoredTextError::NotStorable {
            text: text.to_owned(),
            capacity: N,
        };
        let mut stored = [0; N];
        let mut length = 0;
        for character in text.chars() {
            let (encoded, width) = encode(character).ok_or_else(refused)?;
            let end = length + width;
            stored
                .get_mut(length..end)
                .ok_or_else(refused)?
                .copy_from_slice(encoded.get(..width).ok_or_else(refused)?);
            length = end;
        }
        Ok(Self(stored))
    }

    /// Validate stored bytes.
    ///
    /// # Errors
    ///
    /// Returns [`StoredTextError::Malformed`] unless the bytes are the stored
    /// form of some text: a control byte, a `0x01` with no character byte
    /// after it, or a nonzero byte after the first NUL is refused.
    pub fn from_bytes(bytes: [u8; N]) -> Result<Self, StoredTextError> {
        match decode(&bytes) {
            Some(_) => Ok(Self(bytes)),
            None => Err(StoredTextError::Malformed {
                bytes: bytes.to_vec(),
            }),
        }
    }

    /// Bytes a record field already validated by [`Self::from_bytes`].
    pub(crate) const fn validated(bytes: [u8; N]) -> Self {
        Self(bytes)
    }

    /// The stored bytes.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; N] {
        &self.0
    }

    /// Whether the text is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.first().is_none_or(|&byte| byte == 0)
    }
}

impl<const N: usize> fmt::Display for StoredText<N> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&decode(&self.0).unwrap_or_default())
    }
}

impl<const N: usize> Default for StoredText<N> {
    fn default() -> Self {
        Self::EMPTY
    }
}

impl<const N: usize> AsRef<[u8]> for StoredText<N> {
    fn as_ref(&self) -> &[u8] {
        self.as_bytes()
    }
}

impl<const N: usize> std::str::FromStr for StoredText<N> {
    type Err = StoredTextError;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::new(value)
    }
}

impl<const N: usize> TryFrom<&str> for StoredText<N> {
    type Error = StoredTextError;
    fn try_from(value: &str) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl<const N: usize> TryFrom<[u8; N]> for StoredText<N> {
    type Error = StoredTextError;
    fn try_from(value: [u8; N]) -> Result<Self, Self::Error> {
        Self::from_bytes(value)
    }
}

impl<const N: usize> From<StoredText<N>> for [u8; N] {
    fn from(value: StoredText<N>) -> Self {
        value.0
    }
}

/// Text without a stored form, or stored bytes that are not one.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum StoredTextError {
    /// Text holding a character without a stored form, or whose stored form
    /// exceeds its field.
    #[error("{text:?} has no stored form of at most {capacity} bytes")]
    NotStorable {
        /// The rejected text.
        text: String,
        /// The field's size in bytes.
        capacity: usize,
    },
    /// Stored bytes that are not text followed by NUL padding.
    #[error("stored text {bytes:02X?} is not text followed by NUL padding")]
    Malformed {
        /// The stored bytes.
        bytes: Vec<u8>,
    },
}

/// The stored bytes of one character and how many of them are used.
fn encode(character: char) -> Option<([u8; 2], usize)> {
    let code = u32::from(character);
    let [low, high, ..] = code.to_le_bytes();
    if character.is_control() || code > 0x1FF || code == 0x100 {
        None
    } else if high == 0 {
        Some(([low, 0], 1))
    } else {
        Some(([LEAD_BYTE, low], 2))
    }
}

/// The text of a stored field, or `None` when the bytes are not a stored form.
fn decode(bytes: &[u8]) -> Option<String> {
    let mut text = String::new();
    let mut rest = bytes;
    while let Some((&byte, tail)) = rest.split_first() {
        if byte == 0 {
            return tail.iter().all(|&padding| padding == 0).then_some(text);
        }
        let character = if byte == LEAD_BYTE {
            let (&low, after) = tail.split_first()?;
            rest = after;
            if low == 0 {
                return None;
            }
            char::from_u32(0x100 | u32::from(low))?
        } else {
            rest = tail;
            char::from(byte)
        };
        if character.is_control() {
            return None;
        }
        text.push(character);
    }
    Some(text)
}
