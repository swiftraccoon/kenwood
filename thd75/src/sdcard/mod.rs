//! SD card file format parsers for the TH-D75.
//!
//! The TH-D75 stores configuration data, logs, recordings, and captures
//! on a microSD card. These parsers allow reading and writing radio data
//! without entering MCP programming mode — just mount the SD card via
//! USB Mass Storage mode (Menu 980) or remove it physically.
//!
//! # File Types
//!
//! | Path | Format | Type |
//! |------|--------|------|
//! | `KENWOOD/TH-D75/SETTINGS/DATA/*.d75` | Binary | Full radio configuration |
//! | `KENWOOD/TH-D75/SETTINGS/RPT_LIST/*.tsv` | UTF-16LE TSV | D-STAR repeater list |
//! | `KENWOOD/TH-D75/SETTINGS/CALLSIGN_LIST/*.tsv` | UTF-16LE TSV | D-STAR callsign list |
//! | `KENWOOD/TH-D75/QSO_LOG/*.tsv` | TSV | QSO contact history |
//! | `KENWOOD/TH-D75/GPS_LOG/*.nme` | NMEA 0183 | GPS track logs |
//!
//! # Encoding
//!
//! All parsers accept `&[u8]` input — the caller decides how to read the
//! file (e.g., `std::fs::read`, memory-mapped, etc.).
//!
//! The repeater list and callsign list use UTF-16LE encoding with a BOM.
//! The QSO log and GPS log use plain ASCII/UTF-8 text.

pub mod callsign_list;
pub mod config;
pub mod gps_log;
pub mod qso_log;
pub mod repeater_list;

use std::fmt;

/// Errors that can occur when parsing SD card files.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SdCardError {
    /// The file is too small to contain the expected data.
    FileTooSmall {
        /// Minimum expected size in bytes.
        expected: usize,
        /// Actual size in bytes.
        actual: usize,
    },

    /// The .d75 file header contains an unrecognised model string.
    InvalidModelString {
        /// The model string found in the header.
        found: String,
    },

    /// A UTF-16LE encoded file is missing the byte order mark (BOM).
    MissingBom,

    /// A UTF-16LE file contains an odd number of bytes (invalid encoding).
    InvalidUtf16Length {
        /// The byte count, which must be even for UTF-16.
        len: usize,
    },

    /// A UTF-16 code unit sequence could not be decoded.
    Utf16Decode {
        /// Human-readable detail about the decode failure.
        detail: String,
    },

    /// A TSV row has an unexpected number of columns.
    ColumnCount {
        /// The 1-based line number in the file.
        line: usize,
        /// The expected number of columns.
        expected: usize,
        /// The actual number of columns.
        actual: usize,
    },

    /// A required field in a TSV row is empty or invalid.
    InvalidField {
        /// The 1-based line number in the file.
        line: usize,
        /// The column name or index.
        column: String,
        /// Human-readable detail about the problem.
        detail: String,
    },

    /// A channel entry in the .d75 binary could not be parsed.
    ChannelParse {
        /// The 0-based channel index.
        index: u16,
        /// Human-readable detail about the parse failure.
        detail: String,
    },
}

impl fmt::Display for SdCardError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::FileTooSmall { expected, actual } => {
                write!(
                    f,
                    "file too small: expected at least {expected} bytes, got {actual}"
                )
            }
            Self::InvalidModelString { found } => {
                write!(f, "invalid model string in .d75 header: {found:?}")
            }
            Self::MissingBom => write!(f, "UTF-16LE file missing byte order mark (BOM)"),
            Self::InvalidUtf16Length { len } => {
                write!(f, "UTF-16LE file has odd byte count ({len}), expected even")
            }
            Self::Utf16Decode { detail } => {
                write!(f, "UTF-16 decode error: {detail}")
            }
            Self::ColumnCount {
                line,
                expected,
                actual,
            } => {
                write!(f, "line {line}: expected {expected} columns, got {actual}")
            }
            Self::InvalidField {
                line,
                column,
                detail,
            } => {
                write!(f, "line {line}, column {column}: {detail}")
            }
            Self::ChannelParse { index, detail } => {
                write!(f, "channel {index}: {detail}")
            }
        }
    }
}

impl std::error::Error for SdCardError {}
