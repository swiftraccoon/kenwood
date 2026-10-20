//! The user Mic-E type code tables: up to five extra radio models per
//! family, which the station list uses to name stations that send Mic-E
//! position reports.
//!
//! Three tables of five 16-byte entries sit at [`MicEFamily::address`], in the
//! page at `0x800` inside the standard backup schedule. An entry holds its
//! one-byte type code in byte 0 and the model name in bytes 1 to 15, both
//! zero-filled; an all-zero entry is unused. The library admits printable
//! ASCII in both fields.
//!
//! On firmware 1.02, type `K,2,1`, a radio with no entries held all three
//! tables zero. An entry written over the operation-panel USB read back in
//! place in a fresh session, changing only its own bytes, and clearing it
//! restored every byte of the standard backup. How the radio matches a
//! received report against an entry is not established.

use std::fmt;

use crate::memory::MemoryImage;
use crate::radio::programming::PageReplacement;
use crate::types::{Address, PAGE_SIZE, Page};

#[cfg(test)]
#[path = "mic_e_tests.rs"]
mod tests;

/// Entries in each table.
pub const MIC_E_TABLE_ENTRIES: usize = 5;
/// Bytes per entry.
pub const MIC_E_ENTRY_SIZE: usize = 16;
/// Bytes of a model name.
pub const MIC_E_MODEL_SIZE: usize = 15;

/// Address of the page holding the three tables.
const TABLES_PAGE: u32 = 0x800;

/// The radio family a Mic-E type code table extends.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum MicEFamily {
    /// Kenwood handheld codes.
    KenwoodHandheld,
    /// Kenwood mobile codes.
    KenwoodMobile,
    /// Yaesu codes.
    Yaesu,
}

impl MicEFamily {
    /// The families in table order.
    pub const ALL: [Self; 3] = [Self::KenwoodHandheld, Self::KenwoodMobile, Self::Yaesu];

    /// Image address of the family's first entry.
    #[must_use]
    pub const fn address(self) -> u32 {
        match self {
            Self::KenwoodHandheld => 0x800,
            Self::KenwoodMobile => 0x850,
            Self::Yaesu => 0x8A0,
        }
    }

    const fn position(self) -> usize {
        match self {
            Self::KenwoodHandheld => 0,
            Self::KenwoodMobile => 1,
            Self::Yaesu => 2,
        }
    }
}

impl fmt::Display for MicEFamily {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::KenwoodHandheld => "Kenwood handheld",
            Self::KenwoodMobile => "Kenwood mobile",
            Self::Yaesu => "Yaesu",
        })
    }
}

/// One table entry: a type code and the model name shown for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MicETypeCode {
    code: u8,
    model: [u8; MIC_E_MODEL_SIZE],
}

impl MicETypeCode {
    /// An entry for `code`, named `model`.
    ///
    /// # Errors
    ///
    /// Returns [`MicETypeCodeError::InvalidCode`] for a code outside `!` to
    /// `~` and [`MicETypeCodeError::InvalidModel`] for a model name longer
    /// than [`MIC_E_MODEL_SIZE`] bytes or holding a character outside space
    /// to `~`.
    pub fn new(code: char, model: &str) -> Result<Self, MicETypeCodeError> {
        let code = u8::try_from(code)
            .ok()
            .filter(|byte| (b'!'..=b'~').contains(byte))
            .ok_or(MicETypeCodeError::InvalidCode { code })?;
        if model.len() > MIC_E_MODEL_SIZE || !model.bytes().all(is_model_byte) {
            return Err(MicETypeCodeError::InvalidModel {
                model: model.to_owned(),
            });
        }
        let mut field = [0; MIC_E_MODEL_SIZE];
        for (destination, source) in field.iter_mut().zip(model.bytes()) {
            *destination = source;
        }
        Ok(Self { code, model: field })
    }

    /// Decode a stored entry; an all-zero entry is unused.
    ///
    /// # Errors
    ///
    /// Returns [`MicETypeCodeError::Malformed`] for a code byte outside `!` to
    /// `~`, a model byte outside space to `~`, or a nonzero byte after the
    /// model name's first zero.
    pub fn from_bytes(bytes: [u8; MIC_E_ENTRY_SIZE]) -> Result<Option<Self>, MicETypeCodeError> {
        if bytes == [0; MIC_E_ENTRY_SIZE] {
            return Ok(None);
        }
        let [code, model @ ..] = bytes;
        let length = model
            .iter()
            .position(|&byte| byte == 0)
            .unwrap_or(MIC_E_MODEL_SIZE);
        let (text, padding) = model.split_at(length);
        if !(b'!'..=b'~').contains(&code)
            || !text.iter().copied().all(is_model_byte)
            || padding.iter().any(|&byte| byte != 0)
        {
            return Err(MicETypeCodeError::Malformed { bytes });
        }
        Ok(Some(Self { code, model }))
    }

    /// The stored form.
    #[must_use]
    pub fn to_bytes(&self) -> [u8; MIC_E_ENTRY_SIZE] {
        let mut bytes = [0; MIC_E_ENTRY_SIZE];
        for (destination, source) in bytes
            .iter_mut()
            .zip(std::iter::once(self.code).chain(self.model))
        {
            *destination = source;
        }
        bytes
    }

    /// The type code.
    #[must_use]
    pub const fn code(&self) -> char {
        self.code as char
    }

    /// The model name.
    #[must_use]
    pub fn model(&self) -> &str {
        let length = self
            .model
            .iter()
            .position(|&byte| byte == 0)
            .unwrap_or(MIC_E_MODEL_SIZE);
        self.model
            .get(..length)
            .and_then(|text| std::str::from_utf8(text).ok())
            .unwrap_or_default()
    }
}

impl fmt::Display for MicETypeCode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{} {:?}", self.code(), self.model())
    }
}

const fn is_model_byte(byte: u8) -> bool {
    matches!(byte, b' '..=b'~')
}

/// The three user Mic-E type code tables.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MicETypeCodeTables {
    entries: [[Option<MicETypeCode>; MIC_E_TABLE_ENTRIES]; 3],
}

impl MicETypeCodeTables {
    /// Decode the three tables from `image`.
    ///
    /// # Errors
    ///
    /// Returns [`MicETypeCodeError::Entry`] naming the first entry whose
    /// bytes [`MicETypeCode::from_bytes`] refuses.
    pub fn read(image: &MemoryImage) -> Result<Self, MicETypeCodeError> {
        let mut tables = Self::default();
        for family in MicEFamily::ALL {
            for position in 0..MIC_E_TABLE_ENTRIES {
                let start = entry_address(family, position);
                let mut bytes = [0; MIC_E_ENTRY_SIZE];
                for (destination, source) in
                    bytes.iter_mut().zip(image.as_bytes().iter().skip(start))
                {
                    *destination = *source;
                }
                let entry =
                    MicETypeCode::from_bytes(bytes).map_err(|source| MicETypeCodeError::Entry {
                        family,
                        position,
                        source: Box::new(source),
                    })?;
                tables.set(family, position, entry)?;
            }
        }
        Ok(tables)
    }

    /// The five entries of `family`, unused ones `None`.
    #[must_use]
    pub fn table(&self, family: MicEFamily) -> &[Option<MicETypeCode>; MIC_E_TABLE_ENTRIES] {
        const UNUSED: [Option<MicETypeCode>; MIC_E_TABLE_ENTRIES] = [None; MIC_E_TABLE_ENTRIES];
        self.entries.get(family.position()).unwrap_or(&UNUSED)
    }

    /// The entry at zero-based `position` of `family`.
    #[must_use]
    pub fn get(&self, family: MicEFamily, position: usize) -> Option<&MicETypeCode> {
        self.table(family).get(position).and_then(Option::as_ref)
    }

    /// Store `entry` at zero-based `position` of `family`; `None` clears it.
    ///
    /// # Errors
    ///
    /// Returns [`MicETypeCodeError::PositionOutOfRange`] at or beyond
    /// [`MIC_E_TABLE_ENTRIES`].
    pub fn set(
        &mut self,
        family: MicEFamily,
        position: usize,
        entry: Option<MicETypeCode>,
    ) -> Result<(), MicETypeCodeError> {
        let slot = self
            .entries
            .get_mut(family.position())
            .and_then(|table| table.get_mut(position))
            .ok_or(MicETypeCodeError::PositionOutOfRange { position })?;
        *slot = entry;
        Ok(())
    }

    /// The write of the tables' page when its bytes change, keeping every
    /// byte outside the tables as `image` holds it.
    ///
    /// Apply it with [`crate::radio::McpSession::compare_exchange_pages`],
    /// which compares the page with a fresh read before writing.
    ///
    /// # Errors
    ///
    /// Returns the [`PageReplacement::new`] errors.
    pub fn page_replacements(
        &self,
        image: &MemoryImage,
    ) -> Result<Vec<PageReplacement>, crate::Error> {
        let page = Page::new(Address::new(TABLES_PAGE)?, PAGE_SIZE)?;
        let start = page.address().as_usize();
        let expected = image
            .as_bytes()
            .get(start..start + PAGE_SIZE)
            .unwrap_or_default();
        let mut replacement = expected.to_vec();
        for family in MicEFamily::ALL {
            for (position, entry) in self.table(family).iter().enumerate() {
                let stored = entry.map_or([0; MIC_E_ENTRY_SIZE], |entry| entry.to_bytes());
                let offset = entry_address(family, position) - start;
                for (destination, source) in replacement.iter_mut().skip(offset).zip(stored) {
                    *destination = source;
                }
            }
        }
        if replacement == expected {
            return Ok(Vec::new());
        }
        Ok(vec![PageReplacement::new(page, expected, &replacement)?])
    }
}

/// Image address of the entry at zero-based `position` of `family`.
const fn entry_address(family: MicEFamily, position: usize) -> usize {
    family.address() as usize + MIC_E_ENTRY_SIZE * position
}

/// A Mic-E type code entry or table the library refuses.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum MicETypeCodeError {
    /// A type code is not one printable ASCII character other than space.
    #[error("Mic-E type code {code:?} is not one printable ASCII character other than space")]
    InvalidCode {
        /// The rejected code.
        code: char,
    },
    /// A model name is longer than 15 bytes or holds a character outside
    /// printable ASCII.
    #[error(
        "model name {model:?} is longer than 15 bytes or holds a character outside printable ASCII"
    )]
    InvalidModel {
        /// The rejected name.
        model: String,
    },
    /// Stored entry bytes are not the zero-filled form of an entry.
    #[error("stored Mic-E type code entry {bytes:02X?} is not a zero-filled code and model name")]
    Malformed {
        /// The stored bytes.
        bytes: [u8; MIC_E_ENTRY_SIZE],
    },
    /// A table position is not below [`MIC_E_TABLE_ENTRIES`].
    #[error("Mic-E table position {position} is outside 0..5")]
    PositionOutOfRange {
        /// The rejected position.
        position: usize,
    },
    /// An entry of a stored table did not decode.
    #[error("{family} Mic-E entry {position}: {source}")]
    Entry {
        /// The table.
        family: MicEFamily,
        /// The zero-based position.
        position: usize,
        /// The entry's error.
        source: Box<Self>,
    },
}
