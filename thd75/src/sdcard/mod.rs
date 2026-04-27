//! SD card file format parsers for the TH-D75.
//!
//! The TH-D75 stores configuration data, logs, recordings, and captures
//! on a microSD/microSDHC card (up to 32 GB, per Operating Tips §5.14).
//! These parsers allow reading and writing radio data without entering
//! MCP programming mode -- just mount the SD card via USB Mass Storage
//! mode (Menu No. 980) or remove it physically.
//!
//! Per User Manual Chapter 19:
//!
//! - Supported cards: microSD (2 GB) and microSDHC (4-32 GB).
//!   microSDXC is NOT supported.
//! - File system: FAT32. Maximum 255 files per folder.
//! - Format via Menu No. 830 (erases all data).
//! - Unmount before removal via Menu No. 820.
//! - Export config: Menu No. 800-803. Import: Menu No. 810-813.
//! - Mass Storage mode (Menu No. 980 set to `Mass Storage`): the radio
//!   appears as a removable disk on the PC. RX/TX and recording are
//!   disabled in this mode.
//!
//! Per User Manual Chapter 20 (Recording):
//!
//! - Recording format: WAV, 16-bit, 16 kHz, mono.
//! - Up to 2 GB per file (approximately 18 hours). Continues in a new
//!   file if exceeded.
//! - Recording band selectable: A or B (Menu No. 302).
//! - Recording starts/stops via Menu No. 301.
//!
//! Per User Manual Chapter 19 (QSO Log):
//!
//! - Menu No. 180 enables QSO history logging.
//! - Format: TSV (tab-separated values).
//! - Records: TX/RX, date, frequency, mode, position, power, S-meter,
//!   callsigns, messages, repeater control flags, and more.
//!
//! # File Types
//!
//! | Path | Format | Type | Parsed? |
//! |------|--------|------|---------|
//! | `KENWOOD/TH-D75/SETTINGS/DATA/*.d75` | Binary | Full radio configuration | Yes |
//! | `KENWOOD/TH-D75/SETTINGS/RPT_LIST/*.tsv` | UTF-16LE TSV | D-STAR repeater list | Yes |
//! | `KENWOOD/TH-D75/SETTINGS/CALLSIGN_LIST/*.tsv` | UTF-16LE TSV | D-STAR callsign list | Yes |
//! | `KENWOOD/TH-D75/QSO_LOG/*.tsv` | TSV | QSO contact history | Yes |
//! | `KENWOOD/TH-D75/GPS_LOG/*.nme` | NMEA 0183 | GPS track logs | Yes |
//! | `KENWOOD/TH-D75/AUDIO_REC/*.wav` | WAV 16kHz/16-bit/mono | TX/RX audio recordings | Yes |
//! | `KENWOOD/TH-D75/CAPTURE/*.bmp` | BMP 240x180/24-bit | Screen captures | Yes |
//!
//! # Encoding
//!
//! All parsers accept `&[u8]` input — the caller decides how to read the
//! file (e.g., `std::fs::read`, memory-mapped, etc.).
//!
//! The repeater list and callsign list use UTF-16LE encoding with a BOM.
//! The QSO log and GPS log use plain ASCII/UTF-8 text.

pub mod audio;
pub mod callsign_list;
pub mod capture;
pub mod config;
pub mod gps_log;
pub mod qso_log;
pub mod repeater_list;

pub use audio::AudioRecording;
pub use capture::ScreenCapture;

use std::fmt;

/// Errors that can occur when parsing SD card files.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
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

    /// A WAV file header is invalid or corrupt.
    InvalidWavHeader {
        /// Human-readable detail about the problem.
        detail: String,
    },

    /// A WAV file has a valid header but unexpected audio format
    /// (not matching TH-D75 spec: 16 kHz, 16-bit, mono).
    UnexpectedAudioFormat {
        /// The sample rate found in the file.
        sample_rate: u32,
        /// The bits per sample found in the file.
        bits_per_sample: u16,
        /// The channel count found in the file.
        channels: u16,
    },

    /// A BMP file header is invalid or corrupt.
    InvalidBmpHeader {
        /// Human-readable detail about the problem.
        detail: String,
    },

    /// A BMP file has a valid header but unexpected image format
    /// (not matching TH-D75 spec: 240x180, 24-bit).
    UnexpectedImageFormat {
        /// The image width found in the file.
        width: u32,
        /// The image height found in the file.
        height: u32,
        /// The bits per pixel found in the file.
        bits_per_pixel: u16,
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
            Self::InvalidWavHeader { detail } => {
                write!(f, "invalid WAV header: {detail}")
            }
            Self::UnexpectedAudioFormat {
                sample_rate,
                bits_per_sample,
                channels,
            } => {
                write!(
                    f,
                    "unexpected WAV format: {sample_rate} Hz, {bits_per_sample}-bit, \
                     {channels} ch (expected 16000 Hz, 16-bit, 1 ch)"
                )
            }
            Self::InvalidBmpHeader { detail } => {
                write!(f, "invalid BMP header: {detail}")
            }
            Self::UnexpectedImageFormat {
                width,
                height,
                bits_per_pixel,
            } => {
                write!(
                    f,
                    "unexpected BMP format: {width}x{height} @ {bits_per_pixel} bpp \
                     (expected 240x180 @ 24 bpp)"
                )
            }
        }
    }
}

impl std::error::Error for SdCardError {}

/// Read a little-endian `u16` from a byte slice at the given offset.
///
/// Returns `0` if the slice is too short — callers are expected to have
/// validated the buffer length against their wire-format constants.
pub(crate) fn read_u16_le(data: &[u8], offset: usize) -> u16 {
    data.get(offset..offset + 2)
        .and_then(|s| <[u8; 2]>::try_from(s).ok())
        .map_or(0, u16::from_le_bytes)
}

/// Read a little-endian `u32` from a byte slice at the given offset.
///
/// Returns `0` if the slice is too short — callers are expected to have
/// validated the buffer length against their wire-format constants.
pub(crate) fn read_u32_le(data: &[u8], offset: usize) -> u32 {
    data.get(offset..offset + 4)
        .and_then(|s| <[u8; 4]>::try_from(s).ok())
        .map_or(0, u32::from_le_bytes)
}
