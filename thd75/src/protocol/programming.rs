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
//! - Exit: `E`
//!
//! # Safety
//!
//! Entering programming mode makes the radio stop responding to normal
//! CAT commands. Always exit programming mode when done.
//!
//! The `0M` handler is at firmware address `0xC002F01C`.

use crate::error::ProtocolError;

/// Entry command to enter programming mode (ASCII).
pub const ENTER_PROGRAMMING: &[u8] = b"0M PROGRAM\r";

/// Expected response when entering programming mode (ASCII).
pub const ENTER_RESPONSE: &[u8] = b"0M\r";

/// ACK byte sent after receiving a data block.
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

/// First page of D-STAR repeater list and callsign list.
pub const DSTAR_RPT_START: u16 = 0x02A1;

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

/// Total channel entries including extended channels (scan edges, WX, call).
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

/// Number of memgroups (200 memgroups, 192 used for 1152 channels + 8 spare).
pub const MEMGROUP_COUNT: usize = 192;

// ---------------------------------------------------------------------------
// Channel flag constants
// ---------------------------------------------------------------------------

/// Size of one channel flag record in bytes.
pub const FLAG_RECORD_SIZE: usize = 4;

/// Flag `used` value indicating an empty/unused channel slot.
pub const FLAG_EMPTY: u8 = 0xFF;
/// Flag `used` value indicating a VHF channel (freq < 150 MHz).
pub const FLAG_VHF: u8 = 0x00;
/// Flag `used` value indicating a 220 MHz channel (150-400 MHz).
pub const FLAG_220: u8 = 0x01;
/// Flag `used` value indicating a UHF channel (freq >= 400 MHz).
pub const FLAG_UHF: u8 = 0x02;

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
pub const fn build_read_command(page: u16) -> [u8; 5] {
    let addr = page.to_be_bytes();
    [b'R', addr[0], addr[1], 0x00, 0x00]
}

/// Build a binary write command for a given page address with 256-byte data.
///
/// Format: `W` + 2-byte big-endian page + `0x00 0x00` + 256-byte data = 261 bytes.
///
/// The radio responds with a single ACK byte (`0x06`) on success.
#[must_use]
pub fn build_write_command(page: u16, data: &[u8; PAGE_SIZE]) -> Vec<u8> {
    let addr = page.to_be_bytes();
    let mut cmd = Vec::with_capacity(W_RESPONSE_SIZE);
    cmd.extend_from_slice(&[b'W', addr[0], addr[1], 0x00, 0x00]);
    cmd.extend_from_slice(data);
    cmd
}

/// Returns `true` if the given page is within the factory calibration region
/// that must never be overwritten.
#[must_use]
pub const fn is_factory_calibration_page(page: u16) -> bool {
    page > MAX_WRITABLE_PAGE
}

/// Parse a write response from the radio.
///
/// Format: `W` + 4-byte address + 256-byte data = 261 bytes total.
/// Bytes 1-2 are the page address (big-endian), bytes 3-4 are the
/// offset (always zero).
///
/// Returns `(page_address, data_slice)` on success.
///
/// # Errors
///
/// - [`ProtocolError::WriteResponseTooShort`] if the buffer is shorter
///   than [`W_RESPONSE_SIZE`].
/// - [`ProtocolError::WriteResponseBadMarker`] if the first byte is not
///   `'W'`.
pub fn parse_write_response(buf: &[u8]) -> Result<(u16, &[u8]), ProtocolError> {
    // W response layout: `W` marker + 4-byte address + PAGE_SIZE bytes.
    let actual = buf.len();
    let &[marker, page_hi, page_lo, _off_hi, _off_lo, ..] = buf else {
        return Err(ProtocolError::WriteResponseTooShort {
            actual,
            expected: W_RESPONSE_SIZE,
        });
    };
    if marker != b'W' {
        return Err(ProtocolError::WriteResponseBadMarker { got: marker });
    }
    let page = u16::from_be_bytes([page_hi, page_lo]);
    let data = buf
        .get(5..5 + PAGE_SIZE)
        .ok_or(ProtocolError::WriteResponseTooShort {
            actual,
            expected: W_RESPONSE_SIZE,
        })?;
    Ok((page, data))
}

/// Extract a channel name from a 16-byte entry.
///
/// Names are null-terminated ASCII/UTF-8 within a fixed 16-byte field.
/// Returns the name as a trimmed string, stopping at the first null byte.
#[must_use]
pub fn extract_name(entry: &[u8]) -> String {
    let end = entry
        .iter()
        .position(|&b| b == 0)
        .unwrap_or(entry.len())
        .min(NAME_ENTRY_SIZE);
    entry
        .get(..end)
        .map(String::from_utf8_lossy)
        .map(|s| s.trim().to_string())
        .unwrap_or_default()
}

/// Parse a 4-byte channel flag record.
///
/// Format: `[used, lockout_byte, group, 0xFF]`.
///
/// - `used`: `0xFF` = empty, `0x00` = VHF, `0x01` = 220, `0x02` = UHF
/// - `lockout_byte` bit 0: `1` = locked out from scan
/// - `group`: bank/group assignment (0-29)
#[must_use]
pub fn parse_channel_flag(bytes: &[u8]) -> Option<ChannelFlag> {
    let &[used, lockout_byte, group, _pad, ..] = bytes else {
        return None;
    };
    Some(ChannelFlag {
        used,
        lockout: lockout_byte & 0x01 != 0,
        group,
    })
}

/// A single channel's flag data (4 bytes per channel at MCP offset 0x2000+).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChannelFlag {
    /// Band indicator: `0xFF` = empty, `0x00` = VHF, `0x01` = 220 MHz, `0x02` = UHF.
    pub used: u8,
    /// `true` if the channel is locked out from scanning.
    pub lockout: bool,
    /// Bank/group assignment (0-29, 30 groups).
    pub group: u8,
}

impl ChannelFlag {
    /// Returns `true` if this channel slot is empty/unused.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.used == FLAG_EMPTY
    }

    /// Serialize this flag back to a 4-byte record.
    #[must_use]
    pub const fn to_bytes(&self) -> [u8; FLAG_RECORD_SIZE] {
        [
            self.used,
            if self.lockout { 0x01 } else { 0x00 },
            self.group,
            0xFF,
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;
    type BoxErr = Box<dyn std::error::Error>;

    fn byte_at(bytes: &[u8], idx: usize) -> Result<u8, BoxErr> {
        bytes
            .get(idx)
            .copied()
            .ok_or_else(|| format!("byte_at: idx {idx} out of range (len={})", bytes.len()).into())
    }

    #[test]
    fn build_read_command_page_256() {
        let cmd = build_read_command(256);
        assert_eq!(cmd, [b'R', 0x01, 0x00, 0x00, 0x00]);
    }

    #[test]
    fn build_read_command_page_318() {
        // Channel 999 is on page 256 + (999/16) = 256 + 62 = 318
        let cmd = build_read_command(318);
        assert_eq!(cmd, [b'R', 0x01, 0x3E, 0x00, 0x00]);
    }

    #[test]
    fn build_read_command_page_zero() {
        let cmd = build_read_command(0);
        assert_eq!(cmd, [b'R', 0x00, 0x00, 0x00, 0x00]);
    }

    #[test]
    fn build_write_command_format() -> TestResult {
        let data = [0xAA; PAGE_SIZE];
        let cmd = build_write_command(0x0100, &data);
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
        let cmd = build_write_command(0, &data);
        assert_eq!(byte_at(&cmd, 1)?, 0x00);
        assert_eq!(byte_at(&cmd, 2)?, 0x00);
        Ok(())
    }

    #[test]
    fn factory_calibration_page_detection() {
        // Pages 0x07A1 and 0x07A2 are factory calibration
        assert!(!is_factory_calibration_page(0x07A0)); // last writable
        assert!(is_factory_calibration_page(0x07A1)); // factory cal
        assert!(is_factory_calibration_page(0x07A2)); // factory cal
        assert!(!is_factory_calibration_page(0x0000)); // system settings
        assert!(!is_factory_calibration_page(0x0100)); // channel names
    }

    #[test]
    fn parse_write_response_valid() -> TestResult {
        let mut resp = vec![b'W', 0x01, 0x00, 0x00, 0x00]; // W + 4-byte address
        resp.extend_from_slice(&[0x41; 256]); // 256 bytes of 'A'
        assert_eq!(resp.len(), 261);
        let (addr, data) = parse_write_response(&resp)?;
        assert_eq!(addr, 256);
        assert_eq!(data.len(), 256);
        assert!(data.iter().all(|&b| b == 0x41));
        Ok(())
    }

    #[test]
    fn parse_write_response_full_page() -> TestResult {
        let mut resp = vec![b'W', 0x01, 0x3E, 0x00, 0x00]; // page 318
        resp.extend_from_slice(&[0u8; 256]);
        assert_eq!(resp.len(), 261);
        let (addr, data) = parse_write_response(&resp)?;
        assert_eq!(addr, 318);
        assert_eq!(data.len(), 256);
        Ok(())
    }

    #[test]
    fn parse_write_response_invalid_marker() {
        let mut resp = vec![b'X', 0x01, 0x00, 0x00, 0x00];
        resp.extend_from_slice(&[0u8; 256]);
        let result = parse_write_response(&resp);
        assert!(
            matches!(
                result,
                Err(ProtocolError::WriteResponseBadMarker { got: b'X' })
            ),
            "expected WriteResponseBadMarker, got {result:?}"
        );
    }

    #[test]
    fn parse_write_response_empty() {
        let resp: Vec<u8> = vec![];
        let result = parse_write_response(&resp);
        assert!(
            matches!(result, Err(ProtocolError::WriteResponseTooShort { .. })),
            "expected WriteResponseTooShort, got {result:?}"
        );
    }

    #[test]
    fn parse_write_response_too_short() {
        let resp = vec![b'W', 0x01, 0x00, 0x00, 0x00, 0x41]; // only 6 bytes
        let result = parse_write_response(&resp);
        assert!(
            matches!(result, Err(ProtocolError::WriteResponseTooShort { .. })),
            "expected WriteResponseTooShort, got {result:?}"
        );
    }

    #[test]
    fn extract_name_null_terminated() -> TestResult {
        let mut entry = [0u8; 16];
        entry
            .get_mut(..4)
            .ok_or("entry[..4] missing")?
            .copy_from_slice(b"RPT1");
        assert_eq!(extract_name(&entry), "RPT1");
        Ok(())
    }

    #[test]
    fn extract_name_full_length() {
        let entry = *b"ForestCityPD\x00\x00\x00\x00";
        assert_eq!(extract_name(&entry), "ForestCityPD");
    }

    #[test]
    fn extract_name_empty() {
        let entry = [0u8; 16];
        assert_eq!(extract_name(&entry), "");
    }

    #[test]
    fn extract_name_max_16_chars() {
        let entry = *b"1234567890ABCDEF";
        assert_eq!(extract_name(&entry), "1234567890ABCDEF");
    }

    #[test]
    fn extract_name_trims_whitespace() -> TestResult {
        let mut entry = [0u8; 16];
        entry
            .get_mut(..6)
            .ok_or("entry[..6] missing")?
            .copy_from_slice(b"RPT1  ");
        assert_eq!(extract_name(&entry), "RPT1");
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
        assert_eq!(ENTER_PROGRAMMING, b"0M PROGRAM\r");
        assert_eq!(ENTER_RESPONSE, b"0M\r");
        assert_eq!(ACK, 0x06);
        assert_eq!(EXIT, b'E');
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
                      are known at compile time; that's exactly the point — we want a test \
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
            // APRS region before D-STAR
            assert!(APRS_START < DSTAR_RPT_START);
            // D-STAR before Bluetooth
            assert!(DSTAR_RPT_START < BT_START);
        }
    }

    #[test]
    fn channel_flag_parse_vhf() -> TestResult {
        let bytes = [FLAG_VHF, 0x00, 0x05, 0xFF];
        let flag = parse_channel_flag(&bytes).ok_or("parse_channel_flag returned None")?;
        assert!(!flag.is_empty());
        assert!(!flag.lockout);
        assert_eq!(flag.group, 5);
        assert_eq!(flag.used, FLAG_VHF);
        Ok(())
    }

    #[test]
    fn channel_flag_parse_empty() -> TestResult {
        let bytes = [FLAG_EMPTY, 0x00, 0x00, 0xFF];
        let flag = parse_channel_flag(&bytes).ok_or("parse_channel_flag returned None")?;
        assert!(flag.is_empty());
        Ok(())
    }

    #[test]
    fn channel_flag_parse_locked_out() -> TestResult {
        let bytes = [FLAG_UHF, 0x01, 0x0A, 0xFF];
        let flag = parse_channel_flag(&bytes).ok_or("parse_channel_flag returned None")?;
        assert!(!flag.is_empty());
        assert!(flag.lockout);
        assert_eq!(flag.group, 10);
        Ok(())
    }

    #[test]
    fn channel_flag_round_trip() -> TestResult {
        let flag = ChannelFlag {
            used: FLAG_220,
            lockout: true,
            group: 15,
        };
        let bytes = flag.to_bytes();
        let parsed = parse_channel_flag(&bytes).ok_or("parse_channel_flag returned None")?;
        assert_eq!(parsed, flag);
        Ok(())
    }

    #[test]
    fn channel_flag_too_short() {
        let bytes = [0xFF, 0x00, 0x00]; // only 3 bytes
        assert!(parse_channel_flag(&bytes).is_none());
    }
}
