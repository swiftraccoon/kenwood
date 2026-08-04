//! Binary programming protocol for MCP (Memory Control Program) access.
//!
//! The TH-D75 supports a binary programming protocol entered via
//! `0M PROGRAM`. This provides access to data not available through
//! standard CAT commands, including channel display names.
//!
//! # Protocol
//!
//! - Entry: `0M PROGRAM\r` -> `0M\r`
//! - Read: `R` + 2-byte page + `0x00 0x00` -> `W` + 4-byte address + 256-byte data (261 bytes)
//! - ACK: `0x06`
//! - Exit: `E` -> `0x06`, followed by the radio's USB reset
//!
//! # Safety
//!
//! Entering programming mode makes the radio stop responding to normal
//! CAT commands. Always exit programming mode when done.

use crate::error::{Error, ProtocolError, ValidationError};
use crate::types::{ChannelDisplayName, StoredChannelFlag};

/// Command to enter MCP programming mode (ASCII).
///
/// The leading carriage return terminates any stale, unterminated
/// command sitting in the radio's CAT line buffer, for example an
/// MMDVM-detection probe (`E0 03 00`) sent just before, so it cannot
/// be prepended to `0M PROGRAM` and corrupt the handshake. This mirrors
/// the `\r`-prefixed preamble `Radio::connect_with_tnc_exit` uses; the radio
/// treats the empty leading line as a no-op.
pub const ENTER_PROGRAMMING: &[u8] = b"\r0M PROGRAM\r";

/// Expected response when entering programming mode (ASCII).
pub const ENTER_RESPONSE: &[u8] = b"0M\r";

/// ACK byte exchanged after page transfers and returned for an accepted exit.
pub const ACK: u8 = 0x06;

/// Exit byte to leave programming mode.
pub const EXIT: u8 = b'E';

// ---------------------------------------------------------------------------
// Memory geometry
// ---------------------------------------------------------------------------

/// Size of data payload in each page (256 bytes).
pub const PAGE_SIZE: usize = 256;

/// Total number of pages in the radio memory (0x0000-0x07A2).
pub const TOTAL_PAGES: u16 = 1955;

/// Total radio memory in bytes (1955 * 256).
pub const TOTAL_SIZE: usize = 500_480;

/// Number of factory calibration pages at the end that must never be written.
pub const FACTORY_CAL_PAGES: u16 = 2;

/// Last page that may be safely written (inclusive).
pub const MAX_WRITABLE_PAGE: u16 = TOTAL_PAGES - FACTORY_CAL_PAGES - 1; // 0x07A0 = 1952

/// A physical page in the TH-D75 MCP memory image.
///
/// This type cannot represent an address at or beyond [`TOTAL_PAGES`].
/// Requiring it at protocol serialization boundaries prevents an invalid raw
/// page number from reaching the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct McpPage(u16);

impl McpPage {
    /// First physical MCP page.
    pub const MIN: u16 = 0;

    /// Last physical MCP page.
    pub const MAX: u16 = TOTAL_PAGES - 1;

    /// Validate a raw MCP page address.
    ///
    /// # Errors
    ///
    /// Returns [`Error::McpPageOutOfRange`] when `page` lies beyond the
    /// physical memory image.
    pub const fn new(page: u16) -> Result<Self, Error> {
        if page >= TOTAL_PAGES {
            return Err(Error::McpPageOutOfRange {
                page,
                total_pages: TOTAL_PAGES,
            });
        }
        Ok(Self(page))
    }

    /// Return the validated raw MCP page address.
    #[must_use]
    pub const fn as_raw(self) -> u16 {
        self.0
    }
}

impl TryFrom<u16> for McpPage {
    type Error = Error;

    fn try_from(page: u16) -> Result<Self, Self::Error> {
        Self::new(page)
    }
}

impl From<McpPage> for u16 {
    fn from(page: McpPage) -> Self {
        page.as_raw()
    }
}

impl std::fmt::UpperHex for McpPage {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::UpperHex::fmt(&self.as_raw(), formatter)
    }
}

/// A physical MCP page that is safe for ordinary configuration writes.
///
/// This type cannot represent either an address beyond the TH-D75 memory
/// image or either of the two factory-calibration pages. Public write-frame
/// construction requires this type so protected addresses cannot be
/// serialized accidentally.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct WritableMcpPage(McpPage);

impl WritableMcpPage {
    /// First writable MCP page.
    pub const MIN: u16 = 0;

    /// Last writable MCP page, immediately before factory calibration.
    pub const MAX: u16 = MAX_WRITABLE_PAGE;

    /// Validate a raw MCP page address for writing.
    ///
    /// # Errors
    ///
    /// Returns [`Error::McpPageOutOfRange`] when `page` lies beyond the
    /// physical memory image. Returns [`Error::McpWriteProtected`] for
    /// either factory-calibration page.
    pub const fn new(page: u16) -> Result<Self, Error> {
        if page >= TOTAL_PAGES {
            return Err(Error::McpPageOutOfRange {
                page,
                total_pages: TOTAL_PAGES,
            });
        }
        Self::from_page(McpPage(page))
    }

    /// Validate a physical MCP page for ordinary configuration writes.
    ///
    /// # Errors
    ///
    /// Returns [`Error::McpWriteProtected`] for either
    /// factory-calibration page.
    pub const fn from_page(page: McpPage) -> Result<Self, Error> {
        if is_factory_calibration_page(page) {
            return Err(Error::McpWriteProtected { page });
        }
        Ok(Self(page))
    }

    /// Return the underlying physical MCP page.
    #[must_use]
    pub const fn page(self) -> McpPage {
        self.0
    }

    /// Return the validated raw MCP page address.
    #[must_use]
    pub const fn as_raw(self) -> u16 {
        self.page().as_raw()
    }
}

impl TryFrom<u16> for WritableMcpPage {
    type Error = Error;

    fn try_from(page: u16) -> Result<Self, Self::Error> {
        Self::new(page)
    }
}

impl TryFrom<McpPage> for WritableMcpPage {
    type Error = Error;

    fn try_from(page: McpPage) -> Result<Self, Self::Error> {
        Self::from_page(page)
    }
}

impl From<WritableMcpPage> for McpPage {
    fn from(page: WritableMcpPage) -> Self {
        page.page()
    }
}

impl From<WritableMcpPage> for u16 {
    fn from(page: WritableMcpPage) -> Self {
        page.as_raw()
    }
}

impl std::fmt::UpperHex for WritableMcpPage {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::UpperHex::fmt(&self.as_raw(), formatter)
    }
}

// ---------------------------------------------------------------------------
// Memory region page addresses
// ---------------------------------------------------------------------------

/// First page of system settings (radio state, global config).
pub const SETTINGS_START: u16 = 0x0000;
/// Last page of system settings (inclusive).
pub const SETTINGS_END: u16 = 0x001F;

/// First page of channel flags (1200 entries x 4 bytes = 4800 bytes).
pub const CHANNEL_FLAGS_START: u16 = 0x0020;
/// Last page of channel flags (inclusive).
pub const CHANNEL_FLAGS_END: u16 = 0x0032;

/// First page of channel memory data (192 memgroups x 256 bytes).
pub const CHANNEL_DATA_START: u16 = 0x0040;
/// Last page of channel memory data (inclusive).
pub const CHANNEL_DATA_END: u16 = 0x00FF;

/// First page of channel names (1200 entries x 16 bytes).
pub const CHANNEL_NAMES_START: u16 = 0x0100;
/// Last page of channel names (inclusive).
pub const CHANNEL_NAMES_END: u16 = 0x014A;

/// First page of group names (within the names array, indices 1152-1181).
pub const GROUP_NAMES_START: u16 = 0x0148;
/// Last page of group names (inclusive).
pub const GROUP_NAMES_END: u16 = 0x014A;

/// APRS message status header page.
pub const APRS_STATUS_PAGE: u16 = 0x0151;
/// First page of APRS messages and settings.
pub const APRS_START: u16 = 0x0152;

/// First page of the 300-record D-STAR direct-callsign table.
pub const DSTAR_CALLSIGN_START: u16 = 0x0250;
/// Last page occupied by the 300-record D-STAR direct-callsign table.
pub const DSTAR_CALLSIGN_END: u16 = 0x029A;
/// First page of the 1,500-record D-STAR repeater table.
pub const DSTAR_RPT_START: u16 = 0x02A0;

/// First page of Bluetooth device data and remaining config.
pub const BT_START: u16 = 0x04D1;

// ---------------------------------------------------------------------------
// Channel name constants
// ---------------------------------------------------------------------------

/// Starting page address for channel name data.
pub const NAME_START_PAGE: u16 = CHANNEL_NAMES_START;

/// Number of pages containing channel name data (63 pages, channels 0-1007).
pub const NAME_PAGE_COUNT: u16 = 63;

/// Number of pages containing all channel name data including extended entries
/// (75 pages, channels 0-1199: scan edges, WX, call channels).
pub const NAME_ALL_PAGE_COUNT: u16 = CHANNEL_NAMES_END - CHANNEL_NAMES_START + 1;

/// Bytes per channel name entry.
pub const NAME_ENTRY_SIZE: usize = 16;

/// Channel name entries per 256-byte page (256 / 16 = 16).
pub const NAMES_PER_PAGE: usize = 16;

/// Maximum number of usable channel names (channels 0-999).
pub const MAX_CHANNELS: usize = 1000;

/// Total channel flag and name slots, including extended channels.
///
/// Only [`CHANNEL_DATA_RECORD_COUNT`] of these slots have a 40-byte channel-data
/// record. The remaining slots belong to the extended flag/name domain.
pub const TOTAL_CHANNEL_ENTRIES: usize = 1200;

// ---------------------------------------------------------------------------
// Channel data constants
// ---------------------------------------------------------------------------

/// Size of one channel memory record in bytes.
pub const CHANNEL_RECORD_SIZE: usize = 40;

/// Channels per memgroup (6 channels + 16 bytes padding = 256 bytes).
pub const CHANNELS_PER_MEMGROUP: usize = 6;

/// Padding bytes at the end of each memgroup.
pub const MEMGROUP_PADDING: usize = 16;

/// Number of memgroups physically present in the channel-data region.
pub const MEMGROUP_COUNT: usize = 192;

/// Number of 40-byte channel-data records stored by the radio.
///
/// This is deliberately distinct from [`TOTAL_CHANNEL_ENTRIES`], because the
/// flag and name tables contain 48 additional extended slots without matching
/// records in the channel-data region.
pub const CHANNEL_DATA_RECORD_COUNT: usize = MEMGROUP_COUNT * CHANNELS_PER_MEMGROUP;

// ---------------------------------------------------------------------------
// Channel flag constants
// ---------------------------------------------------------------------------

/// Size of one channel flag record in bytes.
pub const FLAG_RECORD_SIZE: usize = 4;

/// Byte-zero value indicating an empty/unused channel slot.
pub const FLAG_EMPTY: u8 = 0xFF;
/// Low three-bit band code identifying a VHF channel.
pub const FLAG_VHF: u8 = 0x00;
/// Low three-bit band code identifying a 220 MHz channel.
pub const FLAG_220: u8 = 0x01;
/// Low three-bit band code identifying a UHF channel.
pub const FLAG_UHF: u8 = 0x02;
/// Low three-bit band code identifying a 50 MHz channel.
pub const FLAG_50_MHZ: u8 = 0x05;

// ---------------------------------------------------------------------------
// Wire protocol sizes
// ---------------------------------------------------------------------------

/// Total size of a W response (1 opcode + 4 address + 256 data).
pub const W_RESPONSE_SIZE: usize = 261;

/// Size of the W response header (W + 2-byte block address + 2-byte data size).
pub const W_HEADER_SIZE: usize = 5;

/// Build a binary read command for a given page address.
///
/// Format: `R` + 2-byte big-endian page + `0x00 0x00` (5 bytes total).
#[must_use]
pub const fn build_read_command(page: McpPage) -> [u8; 5] {
    let addr = page.as_raw().to_be_bytes();
    [b'R', addr[0], addr[1], 0x00, 0x00]
}

/// Build a binary write command for a given page address with 256-byte data.
///
/// Format: `W` + 2-byte big-endian page + `0x00 0x00` + 256-byte data = 261 bytes.
///
/// A [`WritableMcpPage`] proves that the command cannot target an address
/// outside the physical image or either factory-calibration page.
///
/// The radio responds with a single ACK byte (`0x06`) on success.
#[must_use]
pub fn build_write_command(page: WritableMcpPage, data: &[u8; PAGE_SIZE]) -> Vec<u8> {
    let addr = page.as_raw().to_be_bytes();
    let mut cmd = Vec::with_capacity(W_RESPONSE_SIZE);
    cmd.extend_from_slice(&[b'W', addr[0], addr[1], 0x00, 0x00]);
    cmd.extend_from_slice(data);
    cmd
}

/// Returns `true` if the given physical page is within the factory calibration
/// region that must never be overwritten.
#[must_use]
pub const fn is_factory_calibration_page(page: McpPage) -> bool {
    page.as_raw() > MAX_WRITABLE_PAGE
}

/// Parse a page-read response from the radio.
///
/// Format: `W` + 4-byte address + 256-byte data = 261 bytes total.
/// Bytes 1-2 are the page address (big-endian), bytes 3-4 are the
/// offset (always zero).
///
/// Returns the validated physical page and an exact-size reference to its
/// data on success.
///
/// # Errors
///
/// - [`ProtocolError::WriteResponseSize`] if the buffer is not exactly
///   [`W_RESPONSE_SIZE`] bytes.
/// - [`ProtocolError::WriteResponseBadMarker`] if the first byte is not
///   `'W'`.
/// - [`ProtocolError::WriteResponseNonzeroOffset`] if address bytes 3-4
///   contain an offset other than zero.
pub fn parse_page_read_response(buf: &[u8]) -> Result<(McpPage, &[u8; PAGE_SIZE]), ProtocolError> {
    // W response layout: `W` marker + 4-byte address + PAGE_SIZE bytes.
    let actual = buf.len();
    if actual != W_RESPONSE_SIZE {
        return Err(ProtocolError::WriteResponseSize {
            actual,
            expected: W_RESPONSE_SIZE,
        });
    }
    let &[marker, page_hi, page_lo, off_hi, off_lo, ..] = buf else {
        return Err(ProtocolError::WriteResponseSize {
            actual,
            expected: W_RESPONSE_SIZE,
        });
    };
    if marker != b'W' {
        return Err(ProtocolError::WriteResponseBadMarker { got: marker });
    }
    let offset = u16::from_be_bytes([off_hi, off_lo]);
    if offset != 0 {
        return Err(ProtocolError::WriteResponseNonzeroOffset { got: offset });
    }
    let raw_page = u16::from_be_bytes([page_hi, page_lo]);
    let page = McpPage::new(raw_page).map_err(|error| ProtocolError::FieldParse {
        command: "MCP W page read".into(),
        field: "page address".into(),
        detail: error.to_string(),
    })?;
    let data: &[u8; PAGE_SIZE] = buf
        .get(5..5 + PAGE_SIZE)
        .and_then(|data| data.try_into().ok())
        .ok_or(ProtocolError::WriteResponseSize {
            actual,
            expected: W_RESPONSE_SIZE,
        })?;
    Ok((page, data))
}

/// Decode one exact channel display-name entry from the MCP name table.
///
/// Full-width 16-byte names do not carry a terminator. Shorter names are
/// NUL-padded. Invalid bytes and nonzero data after the first NUL are errors.
///
/// # Errors
///
/// Returns [`ValidationError`] when the entry violates the channel display
/// name encoding.
pub fn decode_channel_display_name(
    entry: [u8; NAME_ENTRY_SIZE],
) -> Result<ChannelDisplayName, ValidationError> {
    ChannelDisplayName::try_from_wire(entry)
}

/// Parse and validate an exact four-byte channel flag record.
///
/// The returned value retains all four bytes. Only the fields established by
/// physical radio images are interpreted: `0xFF` in byte zero means empty,
/// the low three bits of byte zero encode the band, bit zero of byte one is
/// scan lockout, and byte two is the memory group. Higher and trailing bits
/// are intentionally opaque and survive round trips unchanged.
///
/// # Errors
///
/// Returns [`ValidationError`] when the record width, programmed band code,
/// or programmed memory group is invalid.
pub fn parse_channel_flag(bytes: &[u8]) -> Result<StoredChannelFlag, ValidationError> {
    StoredChannelFlag::try_from(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{MemoryChannelBand, MemoryGroup};

    type TestResult = Result<(), Box<dyn std::error::Error>>;
    type BoxErr = Box<dyn std::error::Error>;

    fn byte_at(bytes: &[u8], idx: usize) -> Result<u8, BoxErr> {
        bytes
            .get(idx)
            .copied()
            .ok_or_else(|| format!("byte_at: idx {idx} out of range (len={})", bytes.len()).into())
    }

    #[test]
    fn build_read_command_page_256() -> TestResult {
        let cmd = build_read_command(McpPage::new(256)?);
        assert_eq!(cmd, [b'R', 0x01, 0x00, 0x00, 0x00]);
        Ok(())
    }

    #[test]
    fn build_read_command_page_318() -> TestResult {
        // Channel 999 is on page 256 + (999/16) = 256 + 62 = 318
        let cmd = build_read_command(McpPage::new(318)?);
        assert_eq!(cmd, [b'R', 0x01, 0x3E, 0x00, 0x00]);
        Ok(())
    }

    #[test]
    fn build_read_command_page_zero() -> TestResult {
        let cmd = build_read_command(McpPage::new(0)?);
        assert_eq!(cmd, [b'R', 0x00, 0x00, 0x00, 0x00]);
        Ok(())
    }

    #[test]
    fn mcp_page_boundaries_are_validated_before_serialization() -> TestResult {
        let first = McpPage::new(McpPage::MIN)?;
        let last = McpPage::new(McpPage::MAX)?;

        assert_eq!(build_read_command(first), [b'R', 0x00, 0x00, 0x00, 0x00]);
        assert_eq!(build_read_command(last), [b'R', 0x07, 0xA2, 0x00, 0x00]);
        assert_eq!(u16::from(last), TOTAL_PAGES - 1);

        for page in [TOTAL_PAGES, u16::MAX] {
            let result = McpPage::new(page);
            assert!(
                matches!(
                    result,
                    Err(Error::McpPageOutOfRange {
                        page: rejected,
                        total_pages: TOTAL_PAGES,
                    }) if rejected == page
                ),
                "out-of-range page 0x{page:04X} reached serialization: {result:?}"
            );
        }
        Ok(())
    }

    #[test]
    fn build_write_command_format() -> TestResult {
        let data = [0xAA; PAGE_SIZE];
        let cmd = build_write_command(WritableMcpPage::new(0x0100)?, &data);
        assert_eq!(cmd.len(), W_RESPONSE_SIZE);
        assert_eq!(byte_at(&cmd, 0)?, b'W');
        assert_eq!(byte_at(&cmd, 1)?, 0x01); // page high byte
        assert_eq!(byte_at(&cmd, 2)?, 0x00); // page low byte
        assert_eq!(byte_at(&cmd, 3)?, 0x00); // offset high
        assert_eq!(byte_at(&cmd, 4)?, 0x00); // offset low
        assert!(
            cmd.get(5..)
                .ok_or("cmd[5..] missing")?
                .iter()
                .all(|&b| b == 0xAA),
            "payload should be all 0xAA"
        );
        Ok(())
    }

    #[test]
    fn build_write_command_page_zero() -> TestResult {
        let data = [0u8; PAGE_SIZE];
        let cmd = build_write_command(WritableMcpPage::new(0)?, &data);
        assert_eq!(byte_at(&cmd, 1)?, 0x00);
        assert_eq!(byte_at(&cmd, 2)?, 0x00);
        Ok(())
    }

    #[test]
    fn writable_mcp_page_accepts_complete_safe_range() -> TestResult {
        let first = WritableMcpPage::new(WritableMcpPage::MIN)?;
        let last_page = McpPage::new(WritableMcpPage::MAX)?;
        let last = WritableMcpPage::try_from(last_page)?;

        assert_eq!(first.as_raw(), 0);
        assert_eq!(last.as_raw(), MAX_WRITABLE_PAGE);
        assert_eq!(McpPage::from(last), last_page);
        assert_eq!(u16::from(last), MAX_WRITABLE_PAGE);
        Ok(())
    }

    #[test]
    fn writable_mcp_page_rejects_factory_calibration_pages() -> TestResult {
        for page in (MAX_WRITABLE_PAGE + 1)..TOTAL_PAGES {
            let physical_page = McpPage::new(page)?;
            let result = WritableMcpPage::try_from(physical_page);
            assert!(
                matches!(result, Err(Error::McpWriteProtected { page: rejected }) if rejected.as_raw() == page),
                "factory-calibration page 0x{page:04X} was not rejected: {result:?}"
            );
        }
        Ok(())
    }

    #[test]
    fn writable_mcp_page_rejects_out_of_range_addresses() {
        for page in [TOTAL_PAGES, u16::MAX] {
            let result = WritableMcpPage::new(page);
            assert!(
                matches!(
                    result,
                    Err(Error::McpPageOutOfRange {
                        page: rejected,
                        total_pages: TOTAL_PAGES,
                    }) if rejected == page
                ),
                "out-of-range page 0x{page:04X} was not rejected: {result:?}"
            );
        }
    }

    #[test]
    fn factory_calibration_page_detection() {
        // Pages 0x07A1 and 0x07A2 are factory calibration
        assert!(!is_factory_calibration_page(McpPage(0x07A0))); // last writable
        assert!(is_factory_calibration_page(McpPage(0x07A1))); // factory cal
        assert!(is_factory_calibration_page(McpPage(0x07A2))); // factory cal
        assert!(!is_factory_calibration_page(McpPage(0x0000))); // system settings
        assert!(!is_factory_calibration_page(McpPage(0x0100))); // channel names
    }

    #[test]
    fn parse_page_read_response_valid() -> TestResult {
        let mut resp = vec![b'W', 0x01, 0x00, 0x00, 0x00]; // W + 4-byte address
        resp.extend_from_slice(&[0x41; 256]); // 256 bytes of 'A'
        assert_eq!(resp.len(), 261);
        let (page, data) = parse_page_read_response(&resp)?;
        assert_eq!(page.as_raw(), 256);
        assert_eq!(data.len(), 256);
        assert!(data.iter().all(|&b| b == 0x41));
        Ok(())
    }

    #[test]
    fn parse_page_read_response_full_page() -> TestResult {
        let mut resp = vec![b'W', 0x01, 0x3E, 0x00, 0x00]; // page 318
        resp.extend_from_slice(&[0u8; 256]);
        assert_eq!(resp.len(), 261);
        let (page, data) = parse_page_read_response(&resp)?;
        assert_eq!(page.as_raw(), 318);
        assert_eq!(data.len(), 256);
        Ok(())
    }

    #[test]
    fn parse_page_read_response_invalid_marker() {
        let mut resp = vec![b'X', 0x01, 0x00, 0x00, 0x00];
        resp.extend_from_slice(&[0u8; 256]);
        let result = parse_page_read_response(&resp);
        assert!(
            matches!(
                result,
                Err(ProtocolError::WriteResponseBadMarker { got: b'X' })
            ),
            "expected WriteResponseBadMarker, got {result:?}"
        );
    }

    #[test]
    fn parse_page_read_response_rejects_nonzero_offset() {
        let mut resp = vec![b'W', 0x01, 0x00, 0x01, 0x23];
        resp.extend_from_slice(&[0x41; 256]);
        let result = parse_page_read_response(&resp);
        assert!(
            matches!(
                result,
                Err(ProtocolError::WriteResponseNonzeroOffset { got: 0x0123 })
            ),
            "expected WriteResponseNonzeroOffset, got {result:?}"
        );
    }

    #[test]
    fn parse_page_read_response_empty() {
        let resp: Vec<u8> = vec![];
        let result = parse_page_read_response(&resp);
        assert!(
            matches!(result, Err(ProtocolError::WriteResponseSize { .. })),
            "expected WriteResponseSize, got {result:?}"
        );
    }

    #[test]
    fn parse_page_read_response_too_short() {
        let resp = vec![b'W', 0x01, 0x00, 0x00, 0x00, 0x41]; // only 6 bytes
        let result = parse_page_read_response(&resp);
        assert!(
            matches!(result, Err(ProtocolError::WriteResponseSize { .. })),
            "expected WriteResponseSize, got {result:?}"
        );
    }

    #[test]
    fn parse_page_read_response_rejects_trailing_bytes() {
        let mut resp = vec![b'W', 0x01, 0x00, 0x00, 0x00];
        resp.extend(std::iter::repeat_n(0x41, PAGE_SIZE + 1));
        let result = parse_page_read_response(&resp);
        assert!(
            matches!(
                result,
                Err(ProtocolError::WriteResponseSize {
                    actual,
                    expected: W_RESPONSE_SIZE,
                }) if actual == W_RESPONSE_SIZE + 1
            ),
            "expected exact-size rejection, got {result:?}"
        );
    }

    #[test]
    fn parse_page_read_response_rejects_invalid_page_echoes() {
        for page in [TOTAL_PAGES, u16::MAX] {
            let [page_hi, page_lo] = page.to_be_bytes();
            let mut response = vec![b'W', page_hi, page_lo, 0x00, 0x00];
            response.extend_from_slice(&[0u8; PAGE_SIZE]);

            let result = parse_page_read_response(&response);
            assert!(
                matches!(result, Err(ProtocolError::FieldParse { .. })),
                "invalid echoed page 0x{page:04X} was accepted: {result:?}"
            );
        }
    }

    #[test]
    fn decode_channel_display_name_null_terminated() -> TestResult {
        let mut entry = [0u8; 16];
        entry
            .get_mut(..4)
            .ok_or("entry[..4] missing")?
            .copy_from_slice(b"RPT1");
        assert_eq!(decode_channel_display_name(entry)?.as_str(), "RPT1");
        Ok(())
    }

    #[test]
    fn decode_channel_display_name_short_value() -> TestResult {
        let entry = *b"ForestCityPD\x00\x00\x00\x00";
        assert_eq!(decode_channel_display_name(entry)?.as_str(), "ForestCityPD");
        Ok(())
    }

    #[test]
    fn decode_channel_display_name_empty() -> TestResult {
        let entry = [0u8; 16];
        assert!(decode_channel_display_name(entry)?.is_empty());
        Ok(())
    }

    #[test]
    fn decode_channel_display_name_accepts_physical_full_width_value() -> TestResult {
        let entry = *b"WX  1 Greenville";
        assert_eq!(
            decode_channel_display_name(entry)?.as_str(),
            "WX  1 Greenville"
        );
        Ok(())
    }

    #[test]
    fn decode_channel_display_name_preserves_whitespace() -> TestResult {
        let mut entry = [0u8; 16];
        entry
            .get_mut(..6)
            .ok_or("entry[..6] missing")?
            .copy_from_slice(b"RPT1  ");
        assert_eq!(decode_channel_display_name(entry)?.as_str(), "RPT1  ");
        Ok(())
    }

    #[test]
    fn name_page_calculation() {
        /// Compute the page address for a given channel number.
        fn page_for(channel: u16) -> u16 {
            NAME_START_PAGE + channel / 16
        }
        // Channel 0 is on page 256, slot 0
        assert_eq!(page_for(0), 256);
        // Channel 15 is still on page 256, slot 15
        assert_eq!(page_for(15), 256);
        // Channel 16 is on page 257, slot 0
        assert_eq!(page_for(16), 257);
        // Channel 999 is on page 256 + 62 = 318
        assert_eq!(page_for(999), 318);
    }

    #[test]
    fn total_name_slots() {
        let total = NAME_PAGE_COUNT as usize * NAMES_PER_PAGE;
        assert_eq!(total, 1008);
        assert!(total >= MAX_CHANNELS);
    }

    #[test]
    fn constants_consistent() {
        assert_eq!(ENTER_PROGRAMMING, b"\r0M PROGRAM\r");
        assert_eq!(ENTER_RESPONSE, b"0M\r");
        assert_eq!(ACK, 0x06);
        assert_eq!(EXIT, b'E');
    }

    #[test]
    fn enter_programming_leads_with_cr_to_flush_stale_input() {
        // The leading CR terminates any unterminated fragment left in
        // the radio's CAT line buffer (e.g. an MMDVM-detection probe)
        // so it cannot be prepended to `0M PROGRAM` and corrupt entry.
        assert!(
            ENTER_PROGRAMMING.starts_with(b"\r"),
            "ENTER_PROGRAMMING must lead with a flushing CR: {ENTER_PROGRAMMING:?}"
        );
        assert!(
            ENTER_PROGRAMMING.ends_with(b"0M PROGRAM\r"),
            "ENTER_PROGRAMMING must still carry the 0M PROGRAM command"
        );
    }

    #[test]
    fn memory_geometry_consistent() {
        assert_eq!(TOTAL_SIZE, TOTAL_PAGES as usize * PAGE_SIZE);
        // These are compile-time truths but we assert them to catch
        // regressions if someone edits the constants.
        #[expect(
            clippy::assertions_on_constants,
            reason = "Deliberately asserting on `const` values. If someone edits these constants \
                      to violate the factory-calibration invariant (MAX_WRITABLE_PAGE < \
                      TOTAL_PAGES), this test must fail; compile-time-only assertions via \
                      `const { assert!(...) }` would be silenced by the same const-folding \
                      clippy is complaining about."
        )]
        {
            assert!(MAX_WRITABLE_PAGE < TOTAL_PAGES);
            assert_eq!(CHANNEL_DATA_RECORD_COUNT, 1_152);
            assert_eq!(TOTAL_CHANNEL_ENTRIES - CHANNEL_DATA_RECORD_COUNT, 48);
        }
        assert_eq!(FACTORY_CAL_PAGES, 2);
    }

    #[test]
    fn region_boundaries_non_overlapping() {
        // These are all compile-time truths verified at test time to
        // catch regressions if the constants are ever changed.
        #[expect(
            clippy::assertions_on_constants,
            reason = "Regression guard: if any region offset constant is edited to overlap with \
                      a neighbour, these asserts must fail. Clippy warns because the constants \
                      are known at compile time; that's exactly the point: we want a test \
                      failure if someone silently breaks the memory map."
        )]
        {
            // Settings end before flags start
            assert!(SETTINGS_END < CHANNEL_FLAGS_START);
            // Flags end before data starts
            assert!(CHANNEL_FLAGS_END < CHANNEL_DATA_START);
            // Data ends before names start
            assert!(CHANNEL_DATA_END < CHANNEL_NAMES_START);
            // Names end before APRS starts
            assert!(CHANNEL_NAMES_END < APRS_START);
            // APRS region before the D-STAR callsign table
            assert!(APRS_START < DSTAR_CALLSIGN_START);
            // The callsign allocation ends before the repeater table
            assert!(DSTAR_CALLSIGN_END < DSTAR_RPT_START);
            // D-STAR before Bluetooth
            assert!(DSTAR_RPT_START < BT_START);
        }
    }

    #[test]
    fn channel_flag_parse_vhf() -> TestResult {
        let bytes = [FLAG_VHF, 0x00, 0x05, 0xFF];
        let flag = parse_channel_flag(&bytes)?;
        assert!(!flag.is_empty());
        assert_eq!(flag.scan_lockout(), Some(false));
        assert_eq!(flag.group(), Some(MemoryGroup::new(5)?));
        assert_eq!(flag.band(), Some(MemoryChannelBand::Vhf));
        Ok(())
    }

    #[test]
    fn channel_flag_parse_empty() -> TestResult {
        let bytes = [FLAG_EMPTY, 0x00, 0x00, 0xFF];
        let flag = parse_channel_flag(&bytes)?;
        assert!(flag.is_empty());
        assert_eq!(flag.band(), None);
        assert_eq!(flag.group(), None);
        assert_eq!(flag.scan_lockout(), None);
        Ok(())
    }

    #[test]
    fn channel_flag_parse_locked_out() -> TestResult {
        let bytes = [FLAG_UHF, 0x01, 0x0A, 0xFF];
        let flag = parse_channel_flag(&bytes)?;
        assert!(!flag.is_empty());
        assert_eq!(flag.scan_lockout(), Some(true));
        assert_eq!(flag.group(), Some(MemoryGroup::new(10)?));
        assert_eq!(flag.band(), Some(MemoryChannelBand::Uhf));
        Ok(())
    }

    #[test]
    fn channel_flag_round_trip() -> TestResult {
        let flag =
            StoredChannelFlag::programmed(MemoryChannelBand::Band220, MemoryGroup::new(15)?, true);
        let bytes = flag.to_wire_bytes();
        let parsed = parse_channel_flag(&bytes)?;
        assert_eq!(parsed, flag);
        Ok(())
    }

    #[test]
    fn channel_flag_too_short() {
        let bytes = [0xFF, 0x00, 0x00]; // only 3 bytes
        assert!(matches!(
            parse_channel_flag(&bytes),
            Err(ValidationError::StoredChannelFlagLength { actual: 3 })
        ));
    }

    #[test]
    fn channel_flag_preserves_verified_opaque_bits() -> TestResult {
        let bytes = [0x08, 0xA0, 0x00, 0x00];
        let flag = parse_channel_flag(&bytes)?;
        assert_eq!(flag.band(), Some(MemoryChannelBand::Vhf));
        assert_eq!(flag.scan_lockout(), Some(false));
        assert_eq!(flag.group(), Some(MemoryGroup::new(0)?));
        assert_eq!(flag.to_wire_bytes(), bytes);
        Ok(())
    }
}
