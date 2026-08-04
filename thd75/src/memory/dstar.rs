//! Typed access to the D-STAR settings region of the memory image.
//!
//! The D-STAR settings occupy three regions:
//!
//! - **System settings** at byte offset `0x03F0` (~16 bytes): active
//!   D-STAR channel information.
//! - **MY callsign list** (`dv.MyCallsignDvGatewayList`) at byte
//!   offset `0x1CA8`: six 12-byte records, each an 8-byte space-padded
//!   callsign plus a 4-byte NUL-padded memo, with the active-entry
//!   selector (`dv.MyCallsignSelectDvGateway`) at `0x1CA1`.
//! - **Direct-callsign list** at page `0x0250` (byte offset `0x25000`):
//!   300 opaque 64-byte records through `0x29AFF`.
//! - **Repeater list** at page `0x02A0` (byte offset `0x2A000`): 1,500
//!   80-byte records, packed three per 256-byte page. The final 16 bytes of
//!   every page are padding.
//!
//! # Offset confidence
//!
//! The MY callsign list offsets come from the MCP-D75 field registry
//! ([`MCP_D75_MENU_FIELDS`](super::MCP_D75_MENU_FIELDS)) and are
//! confirmed by a retained physical-radio image. The verified callsign path
//! uses `dv.MyCallsignDvGatewayList`; unrelated APRS status-text storage is
//! not interpreted as D-STAR data.
//!
//! The repeater-list page geometry and the name, area, callsign, and
//! frequency fields are confirmed against the retained physical-radio image.
//! The remaining 24 bytes in each repeater record are exposed only as opaque
//! metadata because their semantics have not been verified.

use crate::error::ValidationError;
use crate::protocol::programming;
use crate::types::{DstarCallsign, Frequency};

/// Byte offset of the D-STAR channel info within the system settings region.
const DSTAR_CHANNEL_INFO_OFFSET: usize = 0x03F0;

/// Size of the D-STAR channel info field.
const DSTAR_CHANNEL_INFO_SIZE: usize = 16;

/// Byte offset of the D-STAR direct-callsign table.
const DSTAR_CALLSIGN_OFFSET: usize =
    programming::DSTAR_CALLSIGN_START as usize * programming::PAGE_SIZE;

/// Byte offset of the D-STAR repeater table.
const DSTAR_RPT_OFFSET: usize = programming::DSTAR_RPT_START as usize * programming::PAGE_SIZE;

/// End of the D-STAR region (the start of Bluetooth data).
const DSTAR_END_OFFSET: usize = programming::BT_START as usize * programming::PAGE_SIZE;

/// Size of a single D-STAR repeater-list record.
const REPEATER_RECORD_SIZE: usize = 80;

/// Size of one opaque direct-callsign-list record.
const CALLSIGN_LIST_RECORD_SIZE: usize = 64;

/// Number of repeater records packed into one MCP page.
const REPEATER_RECORDS_PER_PAGE: usize = 3;

/// Number of trailing padding bytes after three repeater records.
#[cfg(test)]
const REPEATER_PAGE_PADDING: usize =
    programming::PAGE_SIZE - REPEATER_RECORD_SIZE * REPEATER_RECORDS_PER_PAGE;

/// Byte offset of the active MY-callsign selector
/// (`dv.MyCallsignSelectDvGateway`, 1 byte, 0-5).
const DV_MY_CALLSIGN_SELECT_OFFSET: usize = 0x1CA1;

/// Byte offset of the MY callsign list (`dv.MyCallsignDvGatewayList`).
///
/// Six records of [`DV_MY_CALLSIGN_STRIDE`] bytes each: an 8-byte
/// space-padded callsign (`MyCallsignDvGateway`) followed by a 4-byte
/// NUL-padded memo (`MemoDvGateway`).
const DV_MY_CALLSIGN_LIST_OFFSET: usize = 0x1CA8;

/// Stride between MY-callsign records (callsign + memo).
const DV_MY_CALLSIGN_STRIDE: usize = 12;

/// Length of the callsign portion of a MY-callsign record.
const DV_MY_CALLSIGN_LEN: usize = 8;

/// Length of the memo portion of a MY-callsign record.
const DV_MY_CALLSIGN_MEMO_LEN: usize = 4;

// ---------------------------------------------------------------------------
// Repeater record field offsets confirmed by the physical-radio fixture.
// ---------------------------------------------------------------------------

/// Offset within a repeater record for the name field (16 bytes).
const RPT_NAME_OFFSET: usize = 0x00;

/// Offset within a repeater record for the area/sub-name field (16 bytes).
const RPT_AREA_OFFSET: usize = 0x10;

/// Offset within a repeater record for the RPT1 callsign (8 bytes).
const RPT_RPT1_OFFSET: usize = 0x20;

/// Offset within a repeater record for the RPT2/gateway callsign (8 bytes).
const RPT_RPT2_OFFSET: usize = 0x28;

/// Offset within a repeater record for the frequency (4 bytes, uint32 LE, Hz).
const RPT_FREQ_OFFSET: usize = 0x30;

/// Offset within a repeater record for the TX offset (4 bytes, uint32 LE, Hz).
const RPT_TX_OFFSET_OFFSET: usize = 0x34;

/// Offset of the unverified metadata tail within a repeater record.
const RPT_METADATA_OFFSET: usize = 0x38;

/// Size of the unverified metadata tail within a repeater record.
const RPT_METADATA_SIZE: usize = REPEATER_RECORD_SIZE - RPT_METADATA_OFFSET;

// ---------------------------------------------------------------------------
// Public boundary types
// ---------------------------------------------------------------------------

/// An invalid or unavailable value in the D-STAR region of an MCP image.
///
/// D-STAR memory access is deliberately strict. Missing bytes, invalid
/// selectors, malformed padding, and invalid callsigns are reported instead
/// of being converted into empty or default values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum DstarReadError {
    /// The image did not contain a complete required byte range.
    MissingRange {
        /// Name of the field or region being read.
        field: &'static str,
        /// Absolute MCP byte offset.
        offset: usize,
        /// Required byte count.
        len: usize,
    },
    /// The stored active MY-callsign selector was outside `0..=5`.
    InvalidMyCallsignSelector {
        /// Absolute MCP byte offset.
        offset: usize,
        /// Invalid stored byte.
        value: u8,
    },
    /// A fixed-width D-STAR callsign field was invalid.
    InvalidCallsign {
        /// Name of the callsign field.
        field: &'static str,
        /// Absolute MCP byte offset of the field.
        offset: usize,
        /// Exact stored bytes.
        bytes: [u8; DstarCallsign::WIRE_LEN],
    },
    /// A required repeater callsign was empty.
    EmptyRequiredCallsign {
        /// Name of the callsign field.
        field: &'static str,
        /// Absolute MCP byte offset of the field.
        offset: usize,
    },
    /// A NUL-padded memo contained non-NUL data after its terminator.
    InvalidMemoPadding {
        /// Absolute MCP byte offset of the unexpected byte.
        offset: usize,
        /// Unexpected stored byte.
        value: u8,
    },
    /// A MY-callsign memo contained invalid UTF-8.
    InvalidMemoUtf8 {
        /// Absolute MCP byte offset of the memo field.
        offset: usize,
        /// Number of valid bytes before the decoding failure.
        valid_up_to: usize,
        /// Exact stored field bytes.
        bytes: [u8; DV_MY_CALLSIGN_MEMO_LEN],
    },
    /// An occupied repeater record contained a frequency sentinel.
    InvalidRepeaterFrequency {
        /// Name of the frequency field.
        field: &'static str,
        /// Repeater-list slot.
        index: DstarRepeaterIndex,
        /// Absolute MCP byte offset of the field.
        offset: usize,
        /// Invalid decoded frequency in Hz.
        value: u32,
    },
}

impl std::fmt::Display for DstarReadError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingRange { field, offset, len } => write!(
                formatter,
                "{field} needs {len} byte(s) at MCP offset 0x{offset:05X}, but the range is missing"
            ),
            Self::InvalidMyCallsignSelector { offset, value } => write!(
                formatter,
                "D-STAR MY-callsign selector has invalid value {value} at MCP offset 0x{offset:04X} (expected 0-5)"
            ),
            Self::InvalidCallsign {
                field,
                offset,
                bytes,
            } => write!(
                formatter,
                "{field} contains invalid D-STAR callsign bytes {bytes:02X?} at MCP offset 0x{offset:05X}"
            ),
            Self::EmptyRequiredCallsign { field, offset } => write!(
                formatter,
                "{field} is empty at MCP offset 0x{offset:05X}, but the field is required for an occupied repeater record"
            ),
            Self::InvalidMemoPadding { offset, value } => write!(
                formatter,
                "D-STAR MY-callsign memo has non-NUL padding byte 0x{value:02X} at MCP offset 0x{offset:04X}"
            ),
            Self::InvalidMemoUtf8 {
                offset,
                valid_up_to,
                bytes,
            } => write!(
                formatter,
                "D-STAR MY-callsign memo bytes {bytes:02X?} are not UTF-8 at MCP offset 0x{offset:04X} (valid prefix length {valid_up_to})"
            ),
            Self::InvalidRepeaterFrequency {
                field,
                index,
                offset,
                value,
            } => write!(
                formatter,
                "D-STAR repeater slot {} {field} has invalid sentinel {value} Hz at MCP offset 0x{offset:05X}",
                index.as_raw()
            ),
        }
    }
}

impl std::error::Error for DstarReadError {}

/// A validated index into the six-entry D-STAR MY-callsign list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct DstarMyCallsignSlot(u8);

impl DstarMyCallsignSlot {
    /// Number of MY-callsign slots stored by the radio.
    pub const COUNT: u8 = 6;

    /// Creates a slot from its zero-based index.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::SettingOutOfRange`] unless `index` is in
    /// `0..=5`.
    pub const fn new(index: u8) -> Result<Self, ValidationError> {
        if index < Self::COUNT {
            Ok(Self(index))
        } else {
            Err(ValidationError::SettingOutOfRange {
                name: "D-STAR MY-callsign slot",
                value: index,
                detail: "must be 0-5",
            })
        }
    }

    /// Returns the zero-based slot index.
    #[must_use]
    pub const fn as_raw(self) -> u8 {
        self.0
    }
}

/// A validated index into the 300-entry D-STAR direct-callsign list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct DstarCallsignListIndex(u16);

impl DstarCallsignListIndex {
    /// Number of direct-callsign slots stored by the radio.
    pub const COUNT: u16 = 300;

    /// Creates a callsign-list index from its zero-based number.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::IntegerOutOfRange`] unless `index` is in
    /// `0..=299`.
    pub fn new(index: u16) -> Result<Self, ValidationError> {
        if index < Self::COUNT {
            Ok(Self(index))
        } else {
            Err(ValidationError::IntegerOutOfRange {
                name: "D-STAR direct-callsign-list index",
                value: i64::from(index),
                detail: "must be 0-299",
            })
        }
    }

    /// Returns the zero-based slot number.
    #[must_use]
    pub const fn as_raw(self) -> u16 {
        self.0
    }
}

/// A validated index into the 1,500-entry D-STAR repeater list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct DstarRepeaterIndex(u16);

impl DstarRepeaterIndex {
    /// Number of repeater-list slots stored by the radio.
    pub const COUNT: u16 = 1500;

    /// Creates a repeater index from its zero-based number.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::IntegerOutOfRange`] unless `index` is in
    /// `0..=1499`.
    pub fn new(index: u16) -> Result<Self, ValidationError> {
        if index < Self::COUNT {
            Ok(Self(index))
        } else {
            Err(ValidationError::IntegerOutOfRange {
                name: "D-STAR repeater-list index",
                value: i64::from(index),
                detail: "must be 0-1499",
            })
        }
    }

    /// Returns the zero-based slot number.
    #[must_use]
    pub const fn as_raw(self) -> u16 {
        self.0
    }
}

/// Opaque active-channel data whose internal fields remain unverified.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DstarChannelInfo<'a>(&'a [u8; DSTAR_CHANNEL_INFO_SIZE]);

impl DstarChannelInfo<'_> {
    /// Width of the opaque active-channel field.
    pub const SIZE: usize = DSTAR_CHANNEL_INFO_SIZE;

    /// Returns the exact bytes stored by the radio.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 16] {
        self.0
    }
}

/// Opaque bytes spanning the complete D-STAR repeater/callsign region.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DstarRepeaterCallsignRegion<'a>(&'a [u8]);

impl DstarRepeaterCallsignRegion<'_> {
    /// Returns the exact bytes stored by the radio.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8] {
        self.0
    }
}

/// Exact raw bytes for one D-STAR direct-callsign-list slot.
///
/// The record is deliberately opaque here. This accessor establishes the
/// table's proven address, count, and stride without assigning semantics to
/// its internal bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DstarCallsignListRecordBytes<'a>(&'a [u8; CALLSIGN_LIST_RECORD_SIZE]);

impl DstarCallsignListRecordBytes<'_> {
    /// Width of one direct-callsign-list slot.
    pub const SIZE: usize = CALLSIGN_LIST_RECORD_SIZE;

    /// Returns the exact bytes stored by the radio.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 64] {
        self.0
    }
}

/// Exact raw bytes for one page-packed D-STAR repeater slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DstarRepeaterRecordBytes<'a>(&'a [u8; REPEATER_RECORD_SIZE]);

impl DstarRepeaterRecordBytes<'_> {
    /// Width of one page-packed repeater slot.
    pub const SIZE: usize = REPEATER_RECORD_SIZE;

    /// Returns the exact bytes stored by the radio.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 80] {
        self.0
    }
}

/// An opaque 16-byte repeater name or area field.
///
/// The retained directory uses a legacy character encoding that is not
/// uniformly UTF-8. The bytes remain lossless; callers may request strict
/// UTF-8 decoding when they know a particular record uses UTF-8.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DstarRepeaterLabel([u8; 16]);

impl DstarRepeaterLabel {
    /// Width of a repeater name or area field.
    pub const SIZE: usize = 16;

    /// Returns the exact fixed-width field bytes.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; Self::SIZE] {
        &self.0
    }

    /// Decodes this label as strictly NUL-padded UTF-8.
    ///
    /// # Errors
    ///
    /// Returns [`DstarRepeaterLabelError`] for invalid UTF-8 or non-NUL data
    /// after the first terminator. No replacement characters or whitespace
    /// trimming are applied.
    pub fn decode_utf8(&self) -> Result<&str, DstarRepeaterLabelError> {
        let content_len = self.0.iter().position(|&byte| byte == 0).unwrap_or(16);
        let (content, padding) = self.0.split_at(content_len);
        if let Some((offset, &value)) = padding.iter().enumerate().find(|(_, byte)| **byte != 0) {
            return Err(DstarRepeaterLabelError::InvalidPadding {
                offset: content_len + offset,
                value,
            });
        }

        std::str::from_utf8(content).map_err(|error| DstarRepeaterLabelError::InvalidUtf8 {
            valid_up_to: error.valid_up_to(),
            bytes: self.0,
        })
    }
}

/// A strict text-decoding failure for an opaque repeater label.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum DstarRepeaterLabelError {
    /// A byte after the first NUL terminator was not NUL padding.
    InvalidPadding {
        /// Byte offset within the 16-byte label.
        offset: usize,
        /// Unexpected byte.
        value: u8,
    },
    /// The text before the NUL terminator was not valid UTF-8.
    InvalidUtf8 {
        /// Number of valid bytes before the decoding failure.
        valid_up_to: usize,
        /// Exact stored label bytes.
        bytes: [u8; 16],
    },
}

impl std::fmt::Display for DstarRepeaterLabelError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidPadding { offset, value } => write!(
                formatter,
                "repeater label has non-NUL padding byte 0x{value:02X} at field offset {offset}"
            ),
            Self::InvalidUtf8 { valid_up_to, bytes } => write!(
                formatter,
                "repeater label bytes {bytes:02X?} are not UTF-8 (valid prefix length {valid_up_to})"
            ),
        }
    }
}

impl std::error::Error for DstarRepeaterLabelError {}

/// A strictly decoded D-STAR MY-callsign memo.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct DstarMyCallsignMemo(String);

impl DstarMyCallsignMemo {
    /// Returns the memo exactly as decoded before its NUL padding.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// One decoded entry in the six-slot D-STAR MY-callsign list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DstarMyCallsignRecord {
    callsign: Option<DstarCallsign>,
    memo: Option<DstarMyCallsignMemo>,
}

impl DstarMyCallsignRecord {
    /// Returns the programmed callsign, or `None` for an empty slot.
    #[must_use]
    pub const fn callsign(&self) -> Option<&DstarCallsign> {
        self.callsign.as_ref()
    }

    /// Returns the programmed memo, or `None` for a NUL-filled memo field.
    #[must_use]
    pub const fn memo(&self) -> Option<&DstarMyCallsignMemo> {
        self.memo.as_ref()
    }
}

/// Opaque repeater metadata whose bit-level semantics remain unverified.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DstarRepeaterMetadata([u8; RPT_METADATA_SIZE]);

impl DstarRepeaterMetadata {
    /// Width of the unverified metadata tail.
    pub const SIZE: usize = RPT_METADATA_SIZE;

    /// Returns the exact metadata bytes stored by the radio.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; Self::SIZE] {
        &self.0
    }
}

/// Verified fields decoded from one occupied D-STAR repeater slot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DstarRepeaterRecord {
    index: DstarRepeaterIndex,
    name: DstarRepeaterLabel,
    area: DstarRepeaterLabel,
    callsign_rpt1: DstarCallsign,
    gateway_rpt2: Option<DstarCallsign>,
    frequency: Frequency,
    tx_offset: Frequency,
    metadata: DstarRepeaterMetadata,
}

impl DstarRepeaterRecord {
    /// Returns the repeater-list slot from which this record was decoded.
    #[must_use]
    pub const fn index(&self) -> DstarRepeaterIndex {
        self.index
    }

    /// Returns the opaque 16-byte repeater name.
    #[must_use]
    pub const fn name(&self) -> DstarRepeaterLabel {
        self.name
    }

    /// Returns the opaque 16-byte area or sub-name.
    #[must_use]
    pub const fn area(&self) -> DstarRepeaterLabel {
        self.area
    }

    /// Returns the required RPT1 access-repeater callsign.
    #[must_use]
    pub const fn callsign_rpt1(&self) -> &DstarCallsign {
        &self.callsign_rpt1
    }

    /// Returns the optional RPT2 gateway callsign.
    #[must_use]
    pub const fn gateway_rpt2(&self) -> Option<&DstarCallsign> {
        self.gateway_rpt2.as_ref()
    }

    /// Returns the stored operating frequency.
    #[must_use]
    pub const fn frequency(&self) -> Frequency {
        self.frequency
    }

    /// Returns the stored TX offset magnitude.
    #[must_use]
    pub const fn tx_offset(&self) -> Frequency {
        self.tx_offset
    }

    /// Returns the remaining bytes without assigning unverified semantics.
    #[must_use]
    pub const fn metadata(&self) -> DstarRepeaterMetadata {
        self.metadata
    }
}

// ---------------------------------------------------------------------------
// DstarAccess (read-only)
// ---------------------------------------------------------------------------

/// Read-only access to the D-STAR settings region.
///
/// Provides typed opaque byte views and verified field accessors for D-STAR
/// settings stored in the system settings area (channel info at `0x03F0`),
/// the MY callsign list at `0x1CA8`, the direct-callsign list at `0x25000`,
/// and the repeater list at `0x2A000`.
///
/// # Known sub-regions
///
/// | MCP Offset | Content |
/// |-----------|---------|
/// | `0x003F0` | D-STAR channel info (16 bytes) |
/// | `0x01CA1` | Active MY-callsign selector (1 byte, 0-5) |
/// | `0x01CA8` | MY callsign list (6 × 12-byte records: callsign + memo) |
/// | `0x25000` | Direct-callsign list (300 × 64-byte opaque records) |
/// | `0x29B00` | End of the 300-slot direct-callsign allocation |
/// | `0x2A000` | Repeater list (80-byte records, 3 per page) |
/// | `0x49400` | End of the 1,500-slot repeater-list allocation |
#[derive(Debug)]
pub struct DstarAccess<'a> {
    image: &'a [u8],
}

impl<'a> DstarAccess<'a> {
    /// Create a new D-STAR accessor borrowing the raw image.
    pub(crate) const fn new(image: &'a [u8]) -> Self {
        Self { image }
    }

    /// Gets the opaque D-STAR channel info (16 bytes at offset `0x03F0`).
    ///
    /// The internal fields have not been verified, so this method preserves
    /// the exact bytes without assigning meaning to them.
    ///
    /// # Errors
    ///
    /// Returns [`DstarReadError::MissingRange`] if the image is truncated.
    pub fn channel_info(&self) -> Result<DstarChannelInfo<'_>, DstarReadError> {
        self.fixed_array("D-STAR channel info", DSTAR_CHANNEL_INFO_OFFSET)
            .map(DstarChannelInfo)
    }

    /// Gets the opaque repeater/callsign-list region.
    ///
    /// This region spans from page `0x0250` to page `0x04D0` (before the
    /// Bluetooth data). It contains both the direct-callsign and repeater
    /// lists, including the currently unassigned gap between them.
    ///
    /// # Errors
    ///
    /// Returns [`DstarReadError::MissingRange`] if the image is truncated.
    pub fn repeater_callsign_region(
        &self,
    ) -> Result<DstarRepeaterCallsignRegion<'_>, DstarReadError> {
        self.image
            .get(DSTAR_CALLSIGN_OFFSET..DSTAR_END_OFFSET)
            .map(DstarRepeaterCallsignRegion)
            .ok_or(DstarReadError::MissingRange {
                field: "D-STAR repeater/callsign region",
                offset: DSTAR_CALLSIGN_OFFSET,
                len: DSTAR_END_OFFSET - DSTAR_CALLSIGN_OFFSET,
            })
    }

    /// Get the total size of the D-STAR repeater/callsign region in bytes.
    #[must_use]
    pub const fn region_size(&self) -> usize {
        DSTAR_END_OFFSET - DSTAR_CALLSIGN_OFFSET
    }

    /// Reads the exact 64 bytes in one direct-callsign-list slot.
    ///
    /// This view does not decode the record. It preserves every byte while
    /// exposing only the proven table address, count, and stride.
    ///
    /// # Errors
    ///
    /// Returns [`DstarReadError::MissingRange`] if the image is truncated.
    pub fn callsign_list_record_bytes(
        &self,
        index: DstarCallsignListIndex,
    ) -> Result<DstarCallsignListRecordBytes<'_>, DstarReadError> {
        let offset = callsign_list_record_offset(index);
        self.fixed_array("D-STAR direct-callsign record", offset)
            .map(DstarCallsignListRecordBytes)
    }

    /// Reads the exact 80 bytes in a page-packed repeater slot.
    ///
    /// This diagnostic view does not decode the slot. Use
    /// [`repeater_record`](Self::repeater_record) for verified fields.
    ///
    /// # Errors
    ///
    /// Returns [`DstarReadError::MissingRange`] if the image is truncated.
    pub fn repeater_record_bytes(
        &self,
        index: DstarRepeaterIndex,
    ) -> Result<DstarRepeaterRecordBytes<'_>, DstarReadError> {
        let offset = repeater_record_offset(index);
        self.fixed_array("D-STAR repeater record", offset)
            .map(DstarRepeaterRecordBytes)
    }

    // -----------------------------------------------------------------------
    // Typed D-STAR accessors
    // -----------------------------------------------------------------------

    /// Reads the active MY-callsign slot
    /// (`dv.MyCallsignSelectDvGateway`, 0-5).
    ///
    /// # Errors
    ///
    /// Returns [`DstarReadError::MissingRange`] if the selector byte is
    /// missing, or [`DstarReadError::InvalidMyCallsignSelector`] if the stored
    /// value is outside `0..=5`.
    pub fn my_callsign_select(&self) -> Result<DstarMyCallsignSlot, DstarReadError> {
        let value = self
            .image
            .get(DV_MY_CALLSIGN_SELECT_OFFSET)
            .copied()
            .ok_or(DstarReadError::MissingRange {
                field: "dv.MyCallsignSelectDvGateway",
                offset: DV_MY_CALLSIGN_SELECT_OFFSET,
                len: 1,
            })?;
        DstarMyCallsignSlot::new(value).map_err(|_| DstarReadError::InvalidMyCallsignSelector {
            offset: DV_MY_CALLSIGN_SELECT_OFFSET,
            value,
        })
    }

    /// Reads one entry in `dv.MyCallsignDvGatewayList`.
    ///
    /// The callsign is space-padded when programmed and either NUL-filled or
    /// space-filled when empty. The memo is NUL-padded UTF-8. Empty fields are
    /// represented by `None`; malformed fields return an error.
    ///
    /// # Errors
    ///
    /// Returns [`DstarReadError`] for missing bytes, invalid callsign bytes,
    /// malformed memo padding, or invalid memo UTF-8.
    pub fn my_callsign_record(
        &self,
        slot: DstarMyCallsignSlot,
    ) -> Result<DstarMyCallsignRecord, DstarReadError> {
        let record_offset =
            DV_MY_CALLSIGN_LIST_OFFSET + usize::from(slot.as_raw()) * DV_MY_CALLSIGN_STRIDE;
        let callsign_bytes = *self.fixed_array(
            "dv.MyCallsignDvGatewayList.MyCallsignDvGateway",
            record_offset,
        )?;
        let callsign = decode_optional_callsign(
            "dv.MyCallsignDvGatewayList.MyCallsignDvGateway",
            record_offset,
            callsign_bytes,
        )?;

        let memo_offset = record_offset + DV_MY_CALLSIGN_LEN;
        let memo_bytes = *self.fixed_array::<DV_MY_CALLSIGN_MEMO_LEN>(
            "dv.MyCallsignDvGatewayList.MemoDvGateway",
            memo_offset,
        )?;
        let memo = decode_memo(memo_offset, memo_bytes)?;

        Ok(DstarMyCallsignRecord { callsign, memo })
    }

    /// Reads the active D-STAR MY callsign.
    ///
    /// Resolves the selector at `0x1CA1` and reads that record from
    /// the MY callsign list at `0x1CA8` (`dv.MyCallsignDvGatewayList`,
    /// registry-verified and confirmed against a hardware dump).
    ///
    /// # Errors
    ///
    /// Returns [`DstarReadError`] if the selector or selected record is
    /// malformed. An unprogrammed selected record returns `Ok(None)`.
    pub fn my_callsign(&self) -> Result<Option<DstarCallsign>, DstarReadError> {
        let selected = self.my_callsign_select()?;
        Ok(self.my_callsign_record(selected)?.callsign)
    }

    /// Reads the verified fields of one D-STAR repeater-list slot.
    ///
    /// Returns `Ok(None)` for an erased record or the radio's initialized-empty
    /// pattern: all-NUL identity fields plus erased frequency fields. Other
    /// invalid storage is reported as an error.
    ///
    /// # Offset
    ///
    /// Repeater records start at `0x2A000`. Three 80-byte records occupy each
    /// 256-byte MCP page; the remaining 16 bytes are page padding.
    ///
    /// # Errors
    ///
    /// Returns [`DstarReadError`] for missing bytes, invalid callsigns, a
    /// missing required RPT1 callsign, or a frequency sentinel in an occupied
    /// record.
    pub fn repeater_record(
        &self,
        index: DstarRepeaterIndex,
    ) -> Result<Option<DstarRepeaterRecord>, DstarReadError> {
        let record_offset = repeater_record_offset(index);
        let bytes = self.repeater_record_bytes(index)?;
        let bytes = bytes.as_bytes();

        let erased = bytes.iter().all(|&byte| byte == 0xFF);
        let initialized_empty = bytes
            .as_slice()
            .get(..RPT_FREQ_OFFSET)
            .is_some_and(|identity| identity.iter().all(|&byte| byte == 0))
            && bytes
                .as_slice()
                .get(RPT_FREQ_OFFSET..RPT_METADATA_OFFSET)
                .is_some_and(|frequencies| frequencies.iter().all(|&byte| byte == 0xFF));
        if erased || initialized_empty {
            return Ok(None);
        }

        let name = DstarRepeaterLabel(
            *self.fixed_array("D-STAR repeater name", record_offset + RPT_NAME_OFFSET)?,
        );
        let area = DstarRepeaterLabel(
            *self.fixed_array("D-STAR repeater area", record_offset + RPT_AREA_OFFSET)?,
        );

        let rpt1_offset = record_offset + RPT_RPT1_OFFSET;
        let rpt1_bytes = *self.fixed_array("D-STAR repeater RPT1", rpt1_offset)?;
        let callsign_rpt1 =
            decode_optional_callsign("D-STAR repeater RPT1", rpt1_offset, rpt1_bytes)?.ok_or(
                DstarReadError::EmptyRequiredCallsign {
                    field: "D-STAR repeater RPT1",
                    offset: rpt1_offset,
                },
            )?;

        let rpt2_offset = record_offset + RPT_RPT2_OFFSET;
        let rpt2_bytes = *self.fixed_array("D-STAR repeater RPT2", rpt2_offset)?;
        let gateway_rpt2 =
            decode_optional_callsign("D-STAR repeater RPT2", rpt2_offset, rpt2_bytes)?;

        let frequency_offset = record_offset + RPT_FREQ_OFFSET;
        let frequency_bytes = *self.fixed_array("D-STAR repeater frequency", frequency_offset)?;
        let frequency_hz = u32::from_le_bytes(frequency_bytes);
        if frequency_hz == 0 || frequency_hz == u32::MAX {
            return Err(DstarReadError::InvalidRepeaterFrequency {
                field: "frequency",
                index,
                offset: frequency_offset,
                value: frequency_hz,
            });
        }

        let tx_offset_offset = record_offset + RPT_TX_OFFSET_OFFSET;
        let tx_offset_bytes = *self.fixed_array("D-STAR repeater TX offset", tx_offset_offset)?;
        let tx_offset_hz = u32::from_le_bytes(tx_offset_bytes);
        if tx_offset_hz == u32::MAX {
            return Err(DstarReadError::InvalidRepeaterFrequency {
                field: "TX offset",
                index,
                offset: tx_offset_offset,
                value: tx_offset_hz,
            });
        }

        let metadata = DstarRepeaterMetadata(*self.fixed_array(
            "D-STAR repeater metadata",
            record_offset + RPT_METADATA_OFFSET,
        )?);

        Ok(Some(DstarRepeaterRecord {
            index,
            name,
            area,
            callsign_rpt1,
            gateway_rpt2,
            frequency: Frequency::new(frequency_hz),
            tx_offset: Frequency::new(tx_offset_hz),
            metadata,
        }))
    }

    /// Counts strictly decoded occupied repeater entries.
    ///
    /// # Errors
    ///
    /// Returns the first [`DstarReadError`] encountered. A malformed occupied
    /// record is never counted as though it were absent.
    pub fn repeater_count(&self) -> Result<u16, DstarReadError> {
        let mut count: u16 = 0;
        for raw_index in 0..DstarRepeaterIndex::COUNT {
            let index = DstarRepeaterIndex(raw_index);
            if self.repeater_record(index)?.is_some() {
                count += 1;
            }
        }
        Ok(count)
    }

    fn fixed_array<const N: usize>(
        &self,
        field: &'static str,
        offset: usize,
    ) -> Result<&[u8; N], DstarReadError> {
        self.image
            .get(offset..offset + N)
            .and_then(|bytes| bytes.first_chunk::<N>())
            .ok_or(DstarReadError::MissingRange {
                field,
                offset,
                len: N,
            })
    }
}

fn callsign_list_record_offset(index: DstarCallsignListIndex) -> usize {
    DSTAR_CALLSIGN_OFFSET + usize::from(index.as_raw()) * CALLSIGN_LIST_RECORD_SIZE
}

fn repeater_record_offset(index: DstarRepeaterIndex) -> usize {
    let index = usize::from(index.as_raw());
    let page = index / REPEATER_RECORDS_PER_PAGE;
    let slot_in_page = index % REPEATER_RECORDS_PER_PAGE;
    DSTAR_RPT_OFFSET + page * programming::PAGE_SIZE + slot_in_page * REPEATER_RECORD_SIZE
}

fn decode_optional_callsign(
    field: &'static str,
    offset: usize,
    bytes: [u8; DstarCallsign::WIRE_LEN],
) -> Result<Option<DstarCallsign>, DstarReadError> {
    let nul_filled = bytes.iter().all(|&byte| byte == 0);
    let space_filled = bytes.iter().all(|&byte| byte == b' ');
    if nul_filled || space_filled {
        return Ok(None);
    }

    DstarCallsign::try_from_wire_bytes(bytes)
        .map(Some)
        .map_err(|_| DstarReadError::InvalidCallsign {
            field,
            offset,
            bytes,
        })
}

fn decode_memo(
    offset: usize,
    bytes: [u8; DV_MY_CALLSIGN_MEMO_LEN],
) -> Result<Option<DstarMyCallsignMemo>, DstarReadError> {
    let content_len = bytes
        .iter()
        .position(|&byte| byte == 0)
        .unwrap_or(bytes.len());
    let (content, padding) = bytes.split_at(content_len);
    if let Some((padding_offset, &value)) = padding.iter().enumerate().find(|(_, byte)| **byte != 0)
    {
        return Err(DstarReadError::InvalidMemoPadding {
            offset: offset + content_len + padding_offset,
            value,
        });
    }
    if content_len == 0 {
        return Ok(None);
    }

    let content =
        std::str::from_utf8(content).map_err(|error| DstarReadError::InvalidMemoUtf8 {
            offset,
            valid_up_to: error.valid_up_to(),
            bytes,
        })?;
    Ok(Some(DstarMyCallsignMemo(content.to_owned())))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::programming::TOTAL_SIZE;

    type TestResult = Result<(), Box<dyn std::error::Error>>;
    type BoxErr = Box<dyn std::error::Error>;

    fn write_slice(image: &mut [u8], offset: usize, data: &[u8]) -> Result<(), BoxErr> {
        let end = offset + data.len();
        let img_len = image.len();
        image
            .get_mut(offset..end)
            .ok_or_else(|| {
                format!("write_slice: range {offset}..{end} out of bounds (len={img_len})")
            })?
            .copy_from_slice(data);
        Ok(())
    }

    fn make_dstar_image() -> Vec<u8> {
        vec![0u8; TOTAL_SIZE]
    }

    fn my_callsign_slot(index: u8) -> Result<DstarMyCallsignSlot, BoxErr> {
        DstarMyCallsignSlot::new(index).map_err(Into::into)
    }

    fn callsign_list_index(index: u16) -> Result<DstarCallsignListIndex, BoxErr> {
        DstarCallsignListIndex::new(index).map_err(Into::into)
    }

    fn repeater_index(index: u16) -> Result<DstarRepeaterIndex, BoxErr> {
        DstarRepeaterIndex::new(index).map_err(Into::into)
    }

    fn write_repeater(
        image: &mut [u8],
        index: DstarRepeaterIndex,
        name: &[u8],
        area: &[u8],
        rpt1: [u8; DstarCallsign::WIRE_LEN],
        rpt2: [u8; DstarCallsign::WIRE_LEN],
        frequency_hz: u32,
        tx_offset_hz: u32,
    ) -> Result<(), BoxErr> {
        let offset = repeater_record_offset(index);
        write_slice(image, offset, name)?;
        write_slice(image, offset + RPT_AREA_OFFSET, area)?;
        write_slice(image, offset + RPT_RPT1_OFFSET, &rpt1)?;
        write_slice(image, offset + RPT_RPT2_OFFSET, &rpt2)?;
        write_slice(image, offset + RPT_FREQ_OFFSET, &frequency_hz.to_le_bytes())?;
        write_slice(
            image,
            offset + RPT_TX_OFFSET_OFFSET,
            &tx_offset_hz.to_le_bytes(),
        )?;
        Ok(())
    }

    fn write_initialized_empty_repeater(
        image: &mut [u8],
        index: DstarRepeaterIndex,
    ) -> Result<(), BoxErr> {
        write_slice(
            image,
            repeater_record_offset(index) + RPT_FREQ_OFFSET,
            &[0xFF; RPT_METADATA_OFFSET - RPT_FREQ_OFFSET],
        )
    }

    fn initialize_empty_repeater_directory(image: &mut [u8]) -> Result<(), BoxErr> {
        for raw_index in 0..DstarRepeaterIndex::COUNT {
            write_initialized_empty_repeater(image, repeater_index(raw_index)?)?;
        }
        Ok(())
    }

    #[test]
    fn dstar_channel_info_accessible() -> TestResult {
        let mut image = make_dstar_image();
        // Write a known pattern at the D-STAR channel info offset.
        write_slice(
            &mut image,
            DSTAR_CHANNEL_INFO_OFFSET,
            &[0xDE, 0xAD, 0xBE, 0xEF],
        )?;

        let mi = crate::memory::MemoryImage::from_raw(image)?;
        let dstar = mi.dstar();
        let info = dstar.channel_info()?;
        assert_eq!(
            info.as_bytes().get(..4).ok_or("info too short")?,
            &[0xDE, 0xAD, 0xBE, 0xEF]
        );
        Ok(())
    }

    #[test]
    fn dstar_repeater_region_accessible() -> TestResult {
        let image = make_dstar_image();
        let mi = crate::memory::MemoryImage::from_raw(image)?;
        let dstar = mi.dstar();
        let region = dstar.repeater_callsign_region()?;
        assert!(!region.as_bytes().is_empty());
        assert_eq!(region.as_bytes().len(), dstar.region_size());
        Ok(())
    }

    #[test]
    fn dstar_truncated_regions_return_precise_errors() -> TestResult {
        let empty = DstarAccess::new(&[]);
        assert_eq!(
            empty.channel_info(),
            Err(DstarReadError::MissingRange {
                field: "D-STAR channel info",
                offset: DSTAR_CHANNEL_INFO_OFFSET,
                len: DstarChannelInfo::SIZE,
            })
        );
        assert_eq!(
            empty.my_callsign_select(),
            Err(DstarReadError::MissingRange {
                field: "dv.MyCallsignSelectDvGateway",
                offset: DV_MY_CALLSIGN_SELECT_OFFSET,
                len: 1,
            })
        );
        assert_eq!(
            empty.my_callsign_record(my_callsign_slot(0)?),
            Err(DstarReadError::MissingRange {
                field: "dv.MyCallsignDvGatewayList.MyCallsignDvGateway",
                offset: DV_MY_CALLSIGN_LIST_OFFSET,
                len: DstarCallsign::WIRE_LEN,
            })
        );
        assert_eq!(
            empty.repeater_callsign_region(),
            Err(DstarReadError::MissingRange {
                field: "D-STAR repeater/callsign region",
                offset: DSTAR_CALLSIGN_OFFSET,
                len: DSTAR_END_OFFSET - DSTAR_CALLSIGN_OFFSET,
            })
        );

        let callsign_index = callsign_list_index(0)?;
        assert_eq!(
            empty.callsign_list_record_bytes(callsign_index),
            Err(DstarReadError::MissingRange {
                field: "D-STAR direct-callsign record",
                offset: DSTAR_CALLSIGN_OFFSET,
                len: DstarCallsignListRecordBytes::SIZE,
            })
        );

        let index = repeater_index(0)?;
        assert_eq!(
            empty.repeater_record_bytes(index),
            Err(DstarReadError::MissingRange {
                field: "D-STAR repeater record",
                offset: DSTAR_RPT_OFFSET,
                len: DstarRepeaterRecordBytes::SIZE,
            })
        );
        assert_eq!(
            empty.repeater_record(index),
            Err(DstarReadError::MissingRange {
                field: "D-STAR repeater record",
                offset: DSTAR_RPT_OFFSET,
                len: DstarRepeaterRecordBytes::SIZE,
            })
        );
        Ok(())
    }

    #[test]
    fn dstar_repeater_records_cross_page_boundary_without_drift() -> TestResult {
        let mut image = make_dstar_image();
        let record_2 = repeater_index(2)?;
        let record_3 = repeater_index(3)?;
        let record_2_offset = repeater_record_offset(record_2);
        let record_3_offset = repeater_record_offset(record_3);
        write_slice(&mut image, record_2_offset, b"record two")?;
        write_slice(&mut image, record_3_offset, b"record three")?;

        let mi = crate::memory::MemoryImage::from_raw(image)?;
        let dstar = mi.dstar();

        assert_eq!(REPEATER_PAGE_PADDING, 16);
        assert_eq!(record_2_offset, DSTAR_RPT_OFFSET + 2 * REPEATER_RECORD_SIZE);
        assert_eq!(record_3_offset, DSTAR_RPT_OFFSET + programming::PAGE_SIZE);
        assert_eq!(
            record_3_offset - (record_2_offset + REPEATER_RECORD_SIZE),
            16
        );
        assert_eq!(
            dstar.repeater_record_bytes(record_2)?.as_bytes().get(..10),
            Some(b"record two".as_slice())
        );
        assert_eq!(
            dstar.repeater_record_bytes(record_3)?.as_bytes().get(..12),
            Some(b"record three".as_slice())
        );
        Ok(())
    }

    #[test]
    fn dstar_callsign_table_has_300_contiguous_64_byte_records() -> TestResult {
        let first = callsign_list_index(0)?;
        let last = callsign_list_index(DstarCallsignListIndex::COUNT - 1)?;
        let first_offset = callsign_list_record_offset(first);
        let last_offset = callsign_list_record_offset(last);

        assert_eq!(first_offset, 0x25000);
        assert_eq!(last_offset, 0x29AC0);
        assert_eq!(last_offset + CALLSIGN_LIST_RECORD_SIZE, 0x29B00);
        assert!(last_offset + CALLSIGN_LIST_RECORD_SIZE <= DSTAR_RPT_OFFSET);
        assert!(DstarCallsignListIndex::new(DstarCallsignListIndex::COUNT).is_err());

        let mut image = make_dstar_image();
        write_slice(&mut image, last_offset, &[0xA5; CALLSIGN_LIST_RECORD_SIZE])?;
        let memory = crate::memory::MemoryImage::from_raw(image)?;
        assert_eq!(
            memory.dstar().callsign_list_record_bytes(last)?.as_bytes(),
            &[0xA5; CALLSIGN_LIST_RECORD_SIZE]
        );
        Ok(())
    }

    #[test]
    fn dstar_final_repeater_record_fits_before_page_padding() -> TestResult {
        let final_index = repeater_index(DstarRepeaterIndex::COUNT - 1)?;
        let offset = repeater_record_offset(final_index);
        let repeater_allocation_end = DSTAR_RPT_OFFSET + 500 * programming::PAGE_SIZE;

        assert_eq!(
            offset,
            repeater_allocation_end - REPEATER_PAGE_PADDING - REPEATER_RECORD_SIZE
        );
        assert_eq!(offset + REPEATER_RECORD_SIZE, repeater_allocation_end - 16);
        assert!(DstarRepeaterIndex::new(DstarRepeaterIndex::COUNT).is_err());

        let mut image = make_dstar_image();
        write_repeater(
            &mut image,
            final_index,
            b"Final repeater",
            b"Last area",
            *b"N0CALL A",
            *b"N0CALL G",
            439_990_000,
            5_000_000,
        )?;
        let mi = crate::memory::MemoryImage::from_raw(image)?;
        assert_eq!(
            mi.dstar()
                .repeater_record_bytes(final_index)?
                .as_bytes()
                .len(),
            REPEATER_RECORD_SIZE
        );
        let record = mi
            .dstar()
            .repeater_record(final_index)?
            .ok_or("final repeater slot must be occupied")?;
        assert_eq!(record.index(), final_index);
        assert_eq!(record.name().decode_utf8()?, "Final repeater");
        assert_eq!(record.callsign_rpt1().as_str(), "N0CALL A");
        assert_eq!(record.frequency().as_hz(), 439_990_000);
        Ok(())
    }

    #[test]
    fn dstar_region_size_positive() -> TestResult {
        let image = make_dstar_image();
        let mi = crate::memory::MemoryImage::from_raw(image)?;
        let dstar = mi.dstar();
        // D-STAR region should be substantial (>100 KB).
        assert!(dstar.region_size() > 100_000);
        Ok(())
    }

    #[test]
    fn dstar_my_callsign() -> TestResult {
        let mut image = make_dstar_image();
        // Record 0 of dv.MyCallsignDvGatewayList (selector stays 0).
        write_slice(&mut image, DV_MY_CALLSIGN_LIST_OFFSET, b"N0CALL  ")?;

        let mi = crate::memory::MemoryImage::from_raw(image)?;
        let dstar = mi.dstar();
        assert_eq!(dstar.my_callsign_select()?.as_raw(), 0);
        assert_eq!(
            dstar.my_callsign()?.as_ref().map(DstarCallsign::as_str),
            Some("N0CALL")
        );
        Ok(())
    }

    #[test]
    fn dstar_my_callsign_follows_the_selector() -> TestResult {
        let mut image = make_dstar_image();
        // Record 0 and record 2 hold different callsigns; the selector
        // points at record 2.
        write_slice(&mut image, DV_MY_CALLSIGN_LIST_OFFSET, b"N0CALL  ")?;
        write_slice(
            &mut image,
            DV_MY_CALLSIGN_LIST_OFFSET + 2 * DV_MY_CALLSIGN_STRIDE,
            b"W1AW    ",
        )?;
        write_slice(&mut image, DV_MY_CALLSIGN_SELECT_OFFSET, &[2])?;

        let mi = crate::memory::MemoryImage::from_raw(image)?;
        let dstar = mi.dstar();
        assert_eq!(dstar.my_callsign_select()?.as_raw(), 2);
        assert_eq!(
            dstar.my_callsign()?.as_ref().map(DstarCallsign::as_str),
            Some("W1AW")
        );
        assert_eq!(
            dstar
                .my_callsign_record(my_callsign_slot(0)?)?
                .callsign()
                .map(DstarCallsign::as_str),
            Some("N0CALL")
        );
        Ok(())
    }

    #[test]
    fn dstar_my_callsign_memo_is_strict_and_preserves_spaces() -> TestResult {
        let mut image = make_dstar_image();
        write_slice(&mut image, DV_MY_CALLSIGN_LIST_OFFSET, b"N0CALL  A  \0")?;

        let mi = crate::memory::MemoryImage::from_raw(image)?;
        let dstar = mi.dstar();
        assert_eq!(
            dstar
                .my_callsign_record(my_callsign_slot(0)?)?
                .memo()
                .map(DstarMyCallsignMemo::as_str),
            Some("A  ")
        );
        Ok(())
    }

    #[test]
    fn dstar_my_callsign_rejects_invalid_bytes() -> TestResult {
        let mut image = make_dstar_image();
        write_slice(&mut image, DV_MY_CALLSIGN_LIST_OFFSET, b"W1\0AW   ")?;

        let mi = crate::memory::MemoryImage::from_raw(image)?;
        let dstar = mi.dstar();
        assert_eq!(
            dstar.my_callsign(),
            Err(DstarReadError::InvalidCallsign {
                field: "dv.MyCallsignDvGatewayList.MyCallsignDvGateway",
                offset: DV_MY_CALLSIGN_LIST_OFFSET,
                bytes: *b"W1\0AW   ",
            })
        );
        Ok(())
    }

    #[test]
    fn dstar_my_callsign_empty() -> TestResult {
        let image = make_dstar_image();
        let mi = crate::memory::MemoryImage::from_raw(image)?;
        let dstar = mi.dstar();
        assert_eq!(dstar.my_callsign()?, None);
        let record = dstar.my_callsign_record(my_callsign_slot(0)?)?;
        assert_eq!(record.callsign(), None);
        assert_eq!(record.memo(), None);

        let mut space_filled = make_dstar_image();
        write_slice(
            &mut space_filled,
            DV_MY_CALLSIGN_LIST_OFFSET,
            &[b' '; DstarCallsign::WIRE_LEN],
        )?;
        let mi = crate::memory::MemoryImage::from_raw(space_filled)?;
        assert_eq!(
            mi.dstar()
                .my_callsign_record(my_callsign_slot(0)?)?
                .callsign(),
            None
        );
        Ok(())
    }

    #[test]
    fn dstar_my_callsign_does_not_treat_mixed_padding_as_absent() -> TestResult {
        let mut image = make_dstar_image();
        let malformed = [0, b' ', 0, b' ', 0, b' ', 0, b' '];
        write_slice(&mut image, DV_MY_CALLSIGN_LIST_OFFSET, &malformed)?;

        let mi = crate::memory::MemoryImage::from_raw(image)?;
        assert_eq!(
            mi.dstar().my_callsign(),
            Err(DstarReadError::InvalidCallsign {
                field: "dv.MyCallsignDvGatewayList.MyCallsignDvGateway",
                offset: DV_MY_CALLSIGN_LIST_OFFSET,
                bytes: malformed,
            })
        );
        Ok(())
    }

    #[test]
    fn dstar_my_callsign_selector_rejects_out_of_range_storage() -> TestResult {
        let mut image = make_dstar_image();
        write_slice(&mut image, DV_MY_CALLSIGN_SELECT_OFFSET, &[6])?;
        let mi = crate::memory::MemoryImage::from_raw(image)?;
        assert_eq!(
            mi.dstar().my_callsign_select(),
            Err(DstarReadError::InvalidMyCallsignSelector {
                offset: DV_MY_CALLSIGN_SELECT_OFFSET,
                value: 6,
            })
        );
        assert!(DstarMyCallsignSlot::new(DstarMyCallsignSlot::COUNT).is_err());
        Ok(())
    }

    #[test]
    fn dstar_my_callsign_memo_rejects_invalid_padding_and_utf8() -> TestResult {
        let mut invalid_padding = make_dstar_image();
        write_slice(
            &mut invalid_padding,
            DV_MY_CALLSIGN_LIST_OFFSET + DV_MY_CALLSIGN_LEN,
            b"A\0B\0",
        )?;
        let image = crate::memory::MemoryImage::from_raw(invalid_padding)?;
        assert_eq!(
            image.dstar().my_callsign_record(my_callsign_slot(0)?),
            Err(DstarReadError::InvalidMemoPadding {
                offset: DV_MY_CALLSIGN_LIST_OFFSET + DV_MY_CALLSIGN_LEN + 2,
                value: b'B',
            })
        );

        let mut invalid_utf8 = make_dstar_image();
        write_slice(
            &mut invalid_utf8,
            DV_MY_CALLSIGN_LIST_OFFSET + DV_MY_CALLSIGN_LEN,
            &[0xFF, 0, 0, 0],
        )?;
        let image = crate::memory::MemoryImage::from_raw(invalid_utf8)?;
        assert_eq!(
            image.dstar().my_callsign_record(my_callsign_slot(0)?),
            Err(DstarReadError::InvalidMemoUtf8 {
                offset: DV_MY_CALLSIGN_LIST_OFFSET + DV_MY_CALLSIGN_LEN,
                valid_up_to: 0,
                bytes: [0xFF, 0, 0, 0],
            })
        );
        Ok(())
    }

    #[test]
    fn dstar_repeater_record_empty_storage_is_absent() -> TestResult {
        let mut initialized_empty = make_dstar_image();
        let index = repeater_index(0)?;
        write_initialized_empty_repeater(&mut initialized_empty, index)?;
        let mi = crate::memory::MemoryImage::from_raw(initialized_empty)?;
        assert_eq!(mi.dstar().repeater_record(index)?, None);

        let mut erased_image = make_dstar_image();
        let index = repeater_index(1)?;
        write_slice(
            &mut erased_image,
            repeater_record_offset(index),
            &[0xFF; REPEATER_RECORD_SIZE],
        )?;
        let mi = crate::memory::MemoryImage::from_raw(erased_image)?;
        assert_eq!(mi.dstar().repeater_record(index)?, None);
        Ok(())
    }

    #[test]
    fn dstar_repeater_record_does_not_hide_partial_empty_corruption() -> TestResult {
        let all_zero = make_dstar_image();
        let index = repeater_index(0)?;
        let mi = crate::memory::MemoryImage::from_raw(all_zero)?;
        assert_eq!(
            mi.dstar().repeater_record(index),
            Err(DstarReadError::EmptyRequiredCallsign {
                field: "D-STAR repeater RPT1",
                offset: DSTAR_RPT_OFFSET + RPT_RPT1_OFFSET,
            })
        );

        let mut partial_frequency_sentinel = make_dstar_image();
        write_slice(
            &mut partial_frequency_sentinel,
            DSTAR_RPT_OFFSET + RPT_FREQ_OFFSET,
            &u32::MAX.to_le_bytes(),
        )?;
        let mi = crate::memory::MemoryImage::from_raw(partial_frequency_sentinel)?;
        assert_eq!(
            mi.dstar().repeater_record(index),
            Err(DstarReadError::EmptyRequiredCallsign {
                field: "D-STAR repeater RPT1",
                offset: DSTAR_RPT_OFFSET + RPT_RPT1_OFFSET,
            })
        );
        Ok(())
    }

    #[test]
    fn dstar_repeater_record_decodes_only_verified_fields() -> TestResult {
        let mut image = make_dstar_image();
        let index = repeater_index(3)?;
        write_repeater(
            &mut image,
            index,
            b"Test Rptr",
            b"Test Area",
            *b"JR6YPR B",
            *b"JR6YPR G",
            439_010_000,
            5_000_000,
        )?;
        let metadata = [0xA5; RPT_METADATA_SIZE];
        write_slice(
            &mut image,
            repeater_record_offset(index) + RPT_METADATA_OFFSET,
            &metadata,
        )?;

        let mi = crate::memory::MemoryImage::from_raw(image)?;
        let record = mi
            .dstar()
            .repeater_record(index)?
            .ok_or("populated repeater missing")?;
        assert_eq!(record.index(), index);
        assert_eq!(record.name().decode_utf8()?, "Test Rptr");
        assert_eq!(record.area().decode_utf8()?, "Test Area");
        assert_eq!(record.callsign_rpt1().as_str(), "JR6YPR B");
        assert_eq!(
            record.gateway_rpt2().map(DstarCallsign::as_str),
            Some("JR6YPR G")
        );
        assert_eq!(record.frequency().as_hz(), 439_010_000);
        assert_eq!(record.tx_offset().as_hz(), 5_000_000);
        assert_eq!(record.metadata().as_bytes(), &metadata);
        Ok(())
    }

    #[test]
    fn dstar_repeater_optional_gateway_is_not_fabricated() -> TestResult {
        let mut image = make_dstar_image();
        let index = repeater_index(0)?;
        write_repeater(
            &mut image,
            index,
            b"Direct",
            b"Local",
            *b"N0CALL A",
            [0; DstarCallsign::WIRE_LEN],
            145_000_000,
            0,
        )?;

        let mi = crate::memory::MemoryImage::from_raw(image)?;
        let record = mi
            .dstar()
            .repeater_record(index)?
            .ok_or("populated repeater missing")?;
        assert_eq!(record.gateway_rpt2(), None);
        Ok(())
    }

    #[test]
    fn dstar_repeater_record_rejects_invalid_required_fields() -> TestResult {
        let mut invalid_callsign = make_dstar_image();
        let index = repeater_index(0)?;
        write_repeater(
            &mut invalid_callsign,
            index,
            b"Broken",
            b"Area",
            *b"N0\0CALL ",
            *b"N0CALL G",
            145_000_000,
            600_000,
        )?;
        let mi = crate::memory::MemoryImage::from_raw(invalid_callsign)?;
        assert_eq!(
            mi.dstar().repeater_record(index),
            Err(DstarReadError::InvalidCallsign {
                field: "D-STAR repeater RPT1",
                offset: DSTAR_RPT_OFFSET + RPT_RPT1_OFFSET,
                bytes: *b"N0\0CALL ",
            })
        );

        let mut invalid_frequency = make_dstar_image();
        write_repeater(
            &mut invalid_frequency,
            index,
            b"Broken",
            b"Area",
            *b"N0CALL A",
            *b"N0CALL G",
            u32::MAX,
            600_000,
        )?;
        let mi = crate::memory::MemoryImage::from_raw(invalid_frequency)?;
        assert_eq!(
            mi.dstar().repeater_record(index),
            Err(DstarReadError::InvalidRepeaterFrequency {
                field: "frequency",
                index,
                offset: DSTAR_RPT_OFFSET + RPT_FREQ_OFFSET,
                value: u32::MAX,
            })
        );

        let mut invalid_tx_offset = make_dstar_image();
        write_repeater(
            &mut invalid_tx_offset,
            index,
            b"Broken",
            b"Area",
            *b"N0CALL A",
            *b"N0CALL G",
            145_000_000,
            u32::MAX,
        )?;
        let mi = crate::memory::MemoryImage::from_raw(invalid_tx_offset)?;
        assert_eq!(
            mi.dstar().repeater_record(index),
            Err(DstarReadError::InvalidRepeaterFrequency {
                field: "TX offset",
                index,
                offset: DSTAR_RPT_OFFSET + RPT_TX_OFFSET_OFFSET,
                value: u32::MAX,
            })
        );
        Ok(())
    }

    #[test]
    fn dstar_repeater_labels_are_lossless_and_never_decode_lossily() -> TestResult {
        let valid = DstarRepeaterLabel(*b"  Name with gap\0");
        assert_eq!(valid.decode_utf8()?, "  Name with gap");

        let invalid_utf8 = DstarRepeaterLabel([
            b'L', b'i', 0xE8, b'g', b'e', 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        ]);
        assert_eq!(
            invalid_utf8.decode_utf8(),
            Err(DstarRepeaterLabelError::InvalidUtf8 {
                valid_up_to: 2,
                bytes: [
                    b'L', b'i', 0xE8, b'g', b'e', 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                ],
            })
        );
        assert_eq!(invalid_utf8.as_bytes().get(2), Some(&0xE8));

        let invalid_padding = DstarRepeaterLabel(*b"Name\0bad\0\0\0\0\0\0\0\0");
        assert_eq!(
            invalid_padding.decode_utf8(),
            Err(DstarRepeaterLabelError::InvalidPadding {
                offset: 5,
                value: b'b',
            })
        );
        Ok(())
    }

    #[test]
    fn dstar_repeater_count_is_strict() -> TestResult {
        let mut image = make_dstar_image();
        initialize_empty_repeater_directory(&mut image)?;
        for raw_index in [0, 2, 3] {
            write_repeater(
                &mut image,
                repeater_index(raw_index)?,
                b"Repeater",
                b"Area",
                *b"N0CALL A",
                *b"N0CALL G",
                145_000_000,
                600_000,
            )?;
        }

        let mi = crate::memory::MemoryImage::from_raw(image)?;
        assert_eq!(mi.dstar().repeater_count()?, 3);

        let mut malformed = make_dstar_image();
        write_repeater(
            &mut malformed,
            repeater_index(0)?,
            b"Repeater",
            b"Area",
            [0; DstarCallsign::WIRE_LEN],
            [0; DstarCallsign::WIRE_LEN],
            145_000_000,
            600_000,
        )?;
        let mi = crate::memory::MemoryImage::from_raw(malformed)?;
        assert!(matches!(
            mi.dstar().repeater_count(),
            Err(DstarReadError::EmptyRequiredCallsign { .. })
        ));
        Ok(())
    }
}
