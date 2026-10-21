//! Symbol selectors for uncompressed APRS position data.
//!
//! APRS 1.0.1 chapter 20 places the table selector between latitude and
//! longitude, and the symbol code immediately after longitude. The selector is
//! `/`, `\`, a numeric overlay, or an uppercase alphabetic overlay. Displaying
//! a particular glyph remains a receiving client's catalog operation.

use std::fmt;

use super::DprsError;

/// One numeric or uppercase alphabetic overlay for the alternate symbol table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SymbolOverlay(u8);

impl SymbolOverlay {
    /// Validate an uncompressed-position overlay character.
    ///
    /// # Errors
    ///
    /// Returns [`DprsError::InvalidSymbolOverlay`] outside `0..=9` or `A..=Z`.
    pub const fn new(value: char) -> Result<Self, DprsError> {
        if value.is_ascii_digit() || value.is_ascii_uppercase() {
            Ok(Self(value as u8))
        } else {
            Err(DprsError::InvalidSymbolOverlay { got: value })
        }
    }

    /// Validated overlay character.
    #[must_use]
    pub const fn as_char(self) -> char {
        self.0 as char
    }
}

impl TryFrom<char> for SymbolOverlay {
    type Error = DprsError;

    fn try_from(value: char) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<SymbolOverlay> for char {
    fn from(value: SymbolOverlay) -> Self {
        value.as_char()
    }
}

impl fmt::Display for SymbolOverlay {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.as_char(), formatter)
    }
}

/// Mutually exclusive APRS symbol table or alternate-table overlay selector.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SymbolTable {
    /// Primary table, encoded as `/`.
    Primary,
    /// Alternate table without an overlay, encoded as `\`.
    Alternate,
    /// Alternate table with a numeric or uppercase alphabetic overlay.
    Overlay(SymbolOverlay),
}

impl SymbolTable {
    /// Selector byte represented as an ASCII character.
    #[must_use]
    pub const fn as_char(self) -> char {
        match self {
            Self::Primary => '/',
            Self::Alternate => '\\',
            Self::Overlay(overlay) => overlay.as_char(),
        }
    }
}

impl TryFrom<char> for SymbolTable {
    type Error = DprsError;

    fn try_from(value: char) -> Result<Self, Self::Error> {
        match value {
            '/' => Ok(Self::Primary),
            '\\' => Ok(Self::Alternate),
            '0'..='9' | 'A'..='Z' => Ok(Self::Overlay(SymbolOverlay::new(value)?)),
            got => Err(DprsError::InvalidSymbolTable { got }),
        }
    }
}

impl From<SymbolTable> for char {
    fn from(value: SymbolTable) -> Self {
        value.as_char()
    }
}

impl fmt::Display for SymbolTable {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.as_char(), formatter)
    }
}

/// A printable APRS symbol code, from `!` through `~`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SymbolCode(u8);

impl SymbolCode {
    /// Validate the symbol's printable ASCII code.
    ///
    /// # Errors
    ///
    /// Returns [`DprsError::InvalidSymbolCode`] for space, controls or non-ASCII.
    pub const fn new(value: char) -> Result<Self, DprsError> {
        if value >= '!' && value <= '~' {
            Ok(Self(value as u8))
        } else {
            Err(DprsError::InvalidSymbolCode { got: value })
        }
    }

    /// Validated symbol code as an ASCII character.
    #[must_use]
    pub const fn as_char(self) -> char {
        self.0 as char
    }
}

impl TryFrom<char> for SymbolCode {
    type Error = DprsError;

    fn try_from(value: char) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<SymbolCode> for char {
    fn from(value: SymbolCode) -> Self {
        value.as_char()
    }
}

impl fmt::Display for SymbolCode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.as_char(), formatter)
    }
}

/// Independently validated table selector and code for an uncompressed position.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DprsSymbol {
    table: SymbolTable,
    code: SymbolCode,
}

impl DprsSymbol {
    /// Combine the table selector and code without changing either value.
    #[must_use]
    pub const fn new(table: SymbolTable, code: SymbolCode) -> Self {
        Self { table, code }
    }

    /// Table or overlay selector placed between the two coordinates.
    #[must_use]
    pub const fn table(self) -> SymbolTable {
        self.table
    }

    /// Symbol code placed immediately after longitude.
    #[must_use]
    pub const fn code(self) -> SymbolCode {
        self.code
    }
}
