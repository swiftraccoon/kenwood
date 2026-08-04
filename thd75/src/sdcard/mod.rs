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
//! | `KENWOOD/TH-D75/SETTING/DATA/*.d75` | Binary | Full radio configuration | Yes |
//! | `KENWOOD/TH-D75/SETTING/RPT_LIST/*.tsv` | UTF-16LE or Shift-JIS TSV | D-STAR repeater list | Yes |
//! | `KENWOOD/TH-D75/SETTING/CALLSIGN_LIST/*.tsv` | UTF-16LE or Shift-JIS TSV | D-STAR callsign list | Yes |
//! | `KENWOOD/TH-D75/QSO_LOG/*.csv` | TSV content | QSO contact history | Yes |
//! | `KENWOOD/TH-D75/GPS_LOG/*.nme` | NMEA 0183 | GPS track logs | Yes |
//! | `KENWOOD/TH-D75/REC/*.wav` | WAV 16kHz/16-bit/mono | TX/RX audio recordings | Yes |
//! | `KENWOOD/TH-D75/CAPTURE/*.bmp` | BMP 240x180/24-bit | Screen captures | Yes |
//!
//! # Encoding
//!
//! All parsers accept `&[u8]` input; the caller decides how to read the
//! file (e.g., `std::fs::read`, memory-mapped, etc.).
//!
//! MCP-D75 writes repeater catalogs and callsign lists as UTF-16LE with a BOM
//! or Shift-JIS without a BOM, depending on its selected radio/display mode.
//! QSO and GPS logs use their separately documented text encodings.

pub mod audio;
pub mod callsign_list;
pub mod capture;
pub mod config;
pub mod gps_log;
pub mod qso_log;
pub mod repeater_list;

pub use audio::AudioRecordingInfo;
pub use capture::ScreenCapture;

use std::fmt;

/// Text that can occupy one unquoted TSV field without changing table shape.
///
/// TH-D75 SD-card tables do not define an escaping or quoting convention, so
/// tabs and line terminators cannot be represented inside a field. NUL is also
/// rejected because Kenwood tooling treats it as a string terminator.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TsvField(String);

impl TsvField {
    /// Validate and store an SD-card TSV field.
    ///
    /// # Errors
    ///
    /// Returns [`TsvFieldError`] when `value` contains a tab, carriage return,
    /// line feed, or NUL character.
    pub fn new(value: &str) -> Result<Self, TsvFieldError> {
        Self::validate(value)?;
        Ok(Self(value.to_owned()))
    }

    /// Verify that text can occupy one unquoted TSV field.
    ///
    /// # Errors
    ///
    /// Returns [`TsvFieldError`] when `value` contains a tab, carriage return,
    /// line feed, or NUL character.
    pub fn validate(value: &str) -> Result<(), TsvFieldError> {
        if let Some((offset, character)) = value
            .char_indices()
            .find(|(_, character)| matches!(character, '\t' | '\r' | '\n' | '\0'))
        {
            return Err(TsvFieldError { offset, character });
        }
        Ok(())
    }

    /// Return the field exactly as supplied.
    #[must_use]
    pub const fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

impl fmt::Display for TsvField {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// A character cannot be represented inside an unquoted TH-D75 TSV field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TsvFieldError {
    /// UTF-8 byte offset of the invalid character.
    pub offset: usize,
    /// Invalid delimiter, terminator, or NUL character.
    pub character: char,
}

impl fmt::Display for TsvFieldError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "character {:?} at byte {} cannot appear in an unquoted TSV field",
            self.character, self.offset
        )
    }
}

impl std::error::Error for TsvFieldError {}

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

    /// A fixed-size SD-card file contains bytes beyond its complete format.
    UnexpectedFileSize {
        /// Kind of SD-card file being parsed.
        file_type: &'static str,
        /// Exact byte count required by the format.
        expected: usize,
        /// Byte count supplied by the caller.
        actual: usize,
    },

    /// A `.d75` body cannot form a canonical MCP memory image.
    InvalidMemoryImage {
        /// Human-readable detail from the memory-image validator.
        detail: String,
    },

    /// The `.d75` file header contains an unrecognised 16-byte model identifier.
    InvalidModelIdentifier {
        /// Exact identifier bytes found in the header, including any padding.
        found: [u8; 16],
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

    /// A file documented as ASCII/UTF-8 contains an invalid byte sequence.
    InvalidUtf8 {
        /// Kind of SD-card file being decoded.
        file_type: &'static str,
        /// Number of valid bytes before the decoding failure.
        valid_up_to: usize,
        /// Length of the invalid sequence when known, or `None` for truncated input.
        error_len: Option<usize>,
    },

    /// A file documented as 7-bit ASCII contains a byte with its high bit set.
    InvalidAscii {
        /// Kind of SD-card file being decoded.
        file_type: &'static str,
        /// Zero-based byte offset of the first non-ASCII byte.
        offset: usize,
        /// Exact byte found at `offset`.
        byte: u8,
    },

    /// A text file does not use any encoding supported for that file type.
    UnsupportedTextEncoding {
        /// Kind of SD-card file being decoded.
        file_type: &'static str,
        /// Encodings accepted by the parser, in user-facing form.
        expected: &'static str,
    },

    /// A structured text file's first row is not its exact format header.
    HeaderMismatch {
        /// Kind of SD-card file being decoded.
        file_type: &'static str,
        /// Exact header required by the parser.
        expected: String,
        /// Header found in the input, or an empty string when no row exists.
        actual: String,
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

    /// An SD-card table contains more records than the radio can store.
    EntryCount {
        /// Kind of SD-card table being decoded or encoded.
        file_type: &'static str,
        /// Maximum number of records supported by the radio.
        maximum: usize,
        /// Number of records supplied by the file or caller.
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
            Self::UnexpectedFileSize {
                file_type,
                expected,
                actual,
            } => write!(
                f,
                "unexpected {file_type} size: expected exactly {expected} bytes, got {actual}"
            ),
            Self::InvalidMemoryImage { detail } => {
                write!(f, "invalid .d75 memory image: {detail}")
            }
            Self::InvalidModelIdentifier { found } => {
                write!(f, "invalid model identifier in .d75 header: {found:02X?}")
            }
            Self::MissingBom => write!(f, "UTF-16LE file missing byte order mark (BOM)"),
            Self::InvalidUtf16Length { len } => {
                write!(f, "UTF-16LE file has odd byte count ({len}), expected even")
            }
            Self::Utf16Decode { detail } => {
                write!(f, "UTF-16 decode error: {detail}")
            }
            Self::InvalidUtf8 {
                file_type,
                valid_up_to,
                error_len,
            } => fmt_invalid_utf8(f, file_type, *valid_up_to, *error_len),
            Self::InvalidAscii {
                file_type,
                offset,
                byte,
            } => write!(
                f,
                "{file_type} contains non-ASCII byte 0x{byte:02X} at byte {offset}"
            ),
            Self::UnsupportedTextEncoding {
                file_type,
                expected,
            } => write!(
                f,
                "{file_type} uses an unsupported text encoding; expected {expected}"
            ),
            Self::HeaderMismatch {
                file_type,
                expected,
                actual,
            } => write!(
                f,
                "{file_type} header mismatch: expected {expected:?}, got {actual:?}"
            ),
            Self::ColumnCount {
                line,
                expected,
                actual,
            } => {
                write!(f, "line {line}: expected {expected} columns, got {actual}")
            }
            Self::EntryCount {
                file_type,
                maximum,
                actual,
            } => write!(
                f,
                "{file_type} contains {actual} entries, but the radio supports at most {maximum}"
            ),
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
            } => fmt_unexpected_audio(f, *sample_rate, *bits_per_sample, *channels),
            Self::InvalidBmpHeader { detail } => {
                write!(f, "invalid BMP header: {detail}")
            }
            Self::UnexpectedImageFormat {
                width,
                height,
                bits_per_pixel,
            } => fmt_unexpected_image(f, *width, *height, *bits_per_pixel),
        }
    }
}

fn fmt_invalid_utf8(
    formatter: &mut fmt::Formatter<'_>,
    file_type: &str,
    valid_up_to: usize,
    error_len: Option<usize>,
) -> fmt::Result {
    match error_len {
        Some(length) => write!(
            formatter,
            "{file_type} contains a {length}-byte invalid UTF-8 sequence at byte {valid_up_to}"
        ),
        None => write!(
            formatter,
            "{file_type} ends with a truncated UTF-8 sequence at byte {valid_up_to}"
        ),
    }
}

fn fmt_unexpected_audio(
    formatter: &mut fmt::Formatter<'_>,
    sample_rate: u32,
    bits_per_sample: u16,
    channels: u16,
) -> fmt::Result {
    write!(
        formatter,
        "unexpected WAV format: {sample_rate} Hz, {bits_per_sample}-bit, \
         {channels} ch (expected 16000 Hz, 16-bit, 1 ch)"
    )
}

fn fmt_unexpected_image(
    formatter: &mut fmt::Formatter<'_>,
    width: u32,
    height: u32,
    bits_per_pixel: u16,
) -> fmt::Result {
    write!(
        formatter,
        "unexpected BMP format: {width}x{height} @ {bits_per_pixel} bpp \
         (expected 240x180 @ 24 bpp)"
    )
}

impl std::error::Error for SdCardError {}

/// Decode a file whose specified representation is ASCII or UTF-8.
pub(crate) fn decode_utf8<'a>(
    data: &'a [u8],
    file_type: &'static str,
) -> Result<&'a str, SdCardError> {
    std::str::from_utf8(data).map_err(|error| SdCardError::InvalidUtf8 {
        file_type,
        valid_up_to: error.valid_up_to(),
        error_len: error.error_len(),
    })
}

/// Decode a UTF-16LE byte sequence with a leading BOM.
pub(crate) fn decode_utf16le_bom(data: &[u8]) -> Result<String, SdCardError> {
    let Some((bom, payload)) = data.split_first_chunk::<2>() else {
        return Err(SdCardError::MissingBom);
    };
    if *bom != [0xFF, 0xFE] {
        return Err(SdCardError::MissingBom);
    }

    if !payload.len().is_multiple_of(2) {
        return Err(SdCardError::InvalidUtf16Length { len: payload.len() });
    }

    let mut code_units = Vec::with_capacity(payload.len() / 2);
    for pair in payload.chunks_exact(2) {
        let bytes: [u8; 2] = pair
            .try_into()
            .map_err(|_| SdCardError::InvalidUtf16Length { len: payload.len() })?;
        code_units.push(u16::from_le_bytes(bytes));
    }

    String::from_utf16(&code_units).map_err(|error| SdCardError::Utf16Decode {
        detail: error.to_string(),
    })
}

/// Encode text as UTF-16LE with a leading BOM.
pub(crate) fn encode_utf16le_bom(text: &str) -> Vec<u8> {
    let mut output = Vec::with_capacity(2 + text.len() * 2);
    output.extend_from_slice(&[0xFF, 0xFE]);
    for unit in text.encode_utf16() {
        output.extend_from_slice(&unit.to_le_bytes());
    }
    output
}

/// Read a little-endian `u16` from a byte slice at the given offset.
///
/// # Errors
///
/// Returns [`SdCardError::FileTooSmall`] if the two-byte field is not fully
/// present. Truncated binary metadata is never fabricated as zero.
pub(crate) fn read_u16_le(data: &[u8], offset: usize) -> Result<u16, SdCardError> {
    Ok(u16::from_le_bytes(read_array(data, offset)?))
}

/// Read a little-endian `u32` from a byte slice at the given offset.
///
/// # Errors
///
/// Returns [`SdCardError::FileTooSmall`] if the four-byte field is not fully
/// present. Truncated binary metadata is never fabricated as zero.
pub(crate) fn read_u32_le(data: &[u8], offset: usize) -> Result<u32, SdCardError> {
    Ok(u32::from_le_bytes(read_array(data, offset)?))
}

fn read_array<const N: usize>(data: &[u8], offset: usize) -> Result<[u8; N], SdCardError> {
    let end = offset.saturating_add(N);
    let bytes = data.get(offset..end).ok_or(SdCardError::FileTooSmall {
        expected: end,
        actual: data.len(),
    })?;
    <[u8; N]>::try_from(bytes).map_err(|_| SdCardError::FileTooSmall {
        expected: end,
        actual: data.len(),
    })
}
