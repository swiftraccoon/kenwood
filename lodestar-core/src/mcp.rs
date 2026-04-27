// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later
//
// MCP programming-protocol primitives ported from
// `thd75/src/protocol/programming.rs` (same crate, GPL-2.0-or-later).

//! MCP (Memory Control Program) protocol primitives.
//!
//! Pure-logic byte encoders/decoders for the TH-D75's binary MCP
//! programming protocol. The orchestration (timing, transport, ACK
//! exchange, reconnect after exit) happens on the Swift side — this
//! module only produces and parses bytes.
//!
//! # Protocol summary
//!
//! - **Entry:** send `0M PROGRAM\r` at 9600 baud. Radio replies `0M\r`.
//! - **Read:** `R` + 2-byte BE page + `0x00 0x00` (5 bytes).
//!   Radio replies with a full `W` write frame (261 bytes) plus a
//!   trailing `ACK` (`0x06`).
//! - **Write:** `W` + 2-byte BE page + `0x00 0x00` + 256 data bytes
//!   (261 bytes total). Radio replies with a single `ACK` byte.
//! - **Exit:** single byte `E`. The radio drops the USB/BT connection
//!   after this; the caller must reconnect.
//!
//! # Reflector Terminal Mode
//!
//! Menu 650 (DV Gateway) lives at MCP offset
//! [`GATEWAY_MODE_OFFSET`] (`0x1CA0`). Setting this byte to
//! `1` puts the radio into Reflector Terminal Mode after the next
//! reboot (the radio reboots on `E` / transport close).

use thiserror::Error;

/// Command to enter programming mode. Send at 9600 baud.
pub const ENTER_PROGRAMMING_CMD: &[u8] = b"0M PROGRAM\r";

/// Expected radio reply to [`ENTER_PROGRAMMING_CMD`].
pub const ENTER_RESPONSE: &[u8] = b"0M\r";

/// Single-byte acknowledgement the radio sends after each page read/write.
pub const ACK: u8 = 0x06;

/// Single-byte command to exit programming mode. Radio drops the connection.
pub const EXIT_CMD: u8 = b'E';

/// Bytes per MCP page.
pub const PAGE_SIZE: usize = 256;

/// Size of a full `W` write/read response frame (marker + 4-byte addr + page).
pub const W_FRAME_SIZE: usize = 5 + PAGE_SIZE;

/// Largest page index safe to overwrite. Pages above this sit in the
/// factory calibration region and must never be touched.
pub const MAX_WRITABLE_PAGE: u16 = 1952; // 0x07A0

/// MCP byte offset of the DV Gateway mode setting (Menu 650).
///
/// Values: `0` = Off, `1` = Reflector Terminal, `2` = Access Point.
/// Setting this to `1` and exiting programming mode puts the radio
/// into Reflector Terminal mode on the next reboot, at which point
/// the BT SPP speaks MMDVM binary framing instead of CAT ASCII.
pub const GATEWAY_MODE_OFFSET: u16 = 0x1CA0;

/// Gateway mode value: Off.
pub const GATEWAY_MODE_OFF: u8 = 0;
/// Gateway mode value: Reflector Terminal.
pub const GATEWAY_MODE_REFLECTOR_TERMINAL: u8 = 1;
/// Gateway mode value: Access Point.
pub const GATEWAY_MODE_ACCESS_POINT: u8 = 2;

/// Errors surfaced by the MCP primitives.
#[derive(Debug, Clone, Error, PartialEq, Eq, uniffi::Error)]
#[non_exhaustive]
pub enum McpError {
    /// Page number is in the factory calibration region and must not be written.
    #[error("page {page} is in factory calibration region (max writable is {max})")]
    FactoryCalibrationPage {
        /// The out-of-range page index.
        page: u16,
        /// The largest writable page index.
        max: u16,
    },
    /// Write data was not exactly 256 bytes.
    #[error("write data must be {expected} bytes, got {got}")]
    WrongPageSize {
        /// Required page size (always 256).
        expected: u32,
        /// Size actually supplied.
        got: u32,
    },
    /// Response frame was too short to parse.
    #[error("response too short: got {got} bytes, expected at least {expected}")]
    ResponseTooShort {
        /// Bytes received.
        got: u32,
        /// Minimum bytes expected.
        expected: u32,
    },
    /// First byte of a `W` response wasn't `'W'`.
    #[error("expected 'W' marker, got 0x{actual:02x}")]
    BadMarker {
        /// The byte that was there instead of `'W'`.
        actual: u8,
    },
    /// Offset into a page was out of range.
    #[error("byte offset {offset} out of range (page is {size} bytes)")]
    OffsetOutOfRange {
        /// Supplied offset.
        offset: u32,
        /// Page size in bytes.
        size: u32,
    },
}

/// Which page contains the given MCP byte offset.
#[must_use]
#[uniffi::export]
pub fn page_of(offset: u16) -> u16 {
    offset >> 8
}

/// Byte index within the page for the given MCP byte offset.
#[must_use]
#[uniffi::export]
pub fn byte_of(offset: u16) -> u8 {
    // `offset & 0xFF` is always in 0..=255 which fits in u8 losslessly.
    (offset & 0xFF) as u8
}

/// Build the 11-byte `0M PROGRAM\r` command.
#[must_use]
#[uniffi::export]
pub fn build_enter_cmd() -> Vec<u8> {
    ENTER_PROGRAMMING_CMD.to_vec()
}

/// Build the 1-byte `E` exit command.
#[must_use]
#[uniffi::export]
pub fn build_exit_cmd() -> Vec<u8> {
    vec![EXIT_CMD]
}

/// Build a 5-byte `R` read command for a single page.
///
/// Format: `R` + 2-byte BE page + `0x00 0x00`.
#[must_use]
#[uniffi::export]
pub fn build_read_page_cmd(page: u16) -> Vec<u8> {
    let addr = page.to_be_bytes();
    vec![b'R', addr[0], addr[1], 0x00, 0x00]
}

/// Build a 261-byte `W` write command for a single page.
///
/// # Errors
///
/// - [`McpError::FactoryCalibrationPage`] if `page > MAX_WRITABLE_PAGE`.
/// - [`McpError::WrongPageSize`] if `data.len() != PAGE_SIZE`.
#[expect(
    clippy::needless_pass_by_value,
    reason = "UniFFI FFI boundary — `sequence<u8>` in UDL maps to owned `Vec<u8>`."
)]
#[uniffi::export]
pub fn build_write_page_cmd(page: u16, data: Vec<u8>) -> Result<Vec<u8>, McpError> {
    if page > MAX_WRITABLE_PAGE {
        return Err(McpError::FactoryCalibrationPage {
            page,
            max: MAX_WRITABLE_PAGE,
        });
    }
    if data.len() != PAGE_SIZE {
        return Err(McpError::WrongPageSize {
            expected: PAGE_SIZE.try_into().unwrap_or(u32::MAX),
            got: u32::try_from(data.len()).unwrap_or(u32::MAX),
        });
    }
    let addr = page.to_be_bytes();
    let mut cmd = Vec::with_capacity(W_FRAME_SIZE);
    cmd.extend_from_slice(&[b'W', addr[0], addr[1], 0x00, 0x00]);
    cmd.extend_from_slice(&data);
    Ok(cmd)
}

/// Parsed `W` response from the radio (either from a read command or
/// echoed back during the write handshake).
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct McpPage {
    /// Page number this frame covers.
    pub page: u16,
    /// Full 256-byte page contents.
    pub data: Vec<u8>,
}

/// Parse a `W` frame (261 bytes) into `(page, data)`.
///
/// # Errors
///
/// - [`McpError::ResponseTooShort`] if fewer than [`W_FRAME_SIZE`] bytes.
/// - [`McpError::BadMarker`] if the first byte isn't `'W'`.
#[expect(
    clippy::needless_pass_by_value,
    reason = "UniFFI FFI boundary — `sequence<u8>` in UDL maps to owned `Vec<u8>`."
)]
#[uniffi::export]
pub fn parse_w_frame(bytes: Vec<u8>) -> Result<McpPage, McpError> {
    if bytes.len() < W_FRAME_SIZE {
        return Err(McpError::ResponseTooShort {
            got: u32::try_from(bytes.len()).unwrap_or(u32::MAX),
            expected: u32::try_from(W_FRAME_SIZE).unwrap_or(u32::MAX),
        });
    }
    // Indices 0..5 are always present because we checked `.len() >= W_FRAME_SIZE`.
    let marker = bytes.first().copied().unwrap_or(0);
    if marker != b'W' {
        return Err(McpError::BadMarker { actual: marker });
    }
    // Bytes 1..=2 hold the big-endian page; bytes 3..=4 are a zero offset.
    let hi = bytes.get(1).copied().unwrap_or(0);
    let lo = bytes.get(2).copied().unwrap_or(0);
    let page = u16::from_be_bytes([hi, lo]);
    let data = bytes
        .get(5..5 + PAGE_SIZE)
        .ok_or_else(|| McpError::ResponseTooShort {
            got: u32::try_from(bytes.len()).unwrap_or(u32::MAX),
            expected: u32::try_from(W_FRAME_SIZE).unwrap_or(u32::MAX),
        })?
        .to_vec();
    Ok(McpPage { page, data })
}

/// Helper: copy a 256-byte page, flip a single byte at `offset`, and
/// return the modified page ready to feed into [`build_write_page_cmd`].
///
/// # Errors
///
/// - [`McpError::WrongPageSize`] if `page_data` isn't exactly 256 bytes.
/// - [`McpError::OffsetOutOfRange`] is not reachable for `u8 offset` but
///   kept in the error enum for future expansion to `u16`-sized offsets.
#[uniffi::export]
pub fn patch_page_byte(page_data: Vec<u8>, offset: u8, value: u8) -> Result<Vec<u8>, McpError> {
    if page_data.len() != PAGE_SIZE {
        return Err(McpError::WrongPageSize {
            expected: u32::try_from(PAGE_SIZE).unwrap_or(u32::MAX),
            got: u32::try_from(page_data.len()).unwrap_or(u32::MAX),
        });
    }
    let mut out = page_data;
    // offset is a u8, so it always fits in 0..=255, which is < PAGE_SIZE.
    if let Some(slot) = out.get_mut(usize::from(offset)) {
        *slot = value;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::{
        ACK, ENTER_PROGRAMMING_CMD, EXIT_CMD, GATEWAY_MODE_OFFSET, GATEWAY_MODE_REFLECTOR_TERMINAL,
        MAX_WRITABLE_PAGE, McpError, PAGE_SIZE, W_FRAME_SIZE, build_enter_cmd, build_exit_cmd,
        build_read_page_cmd, build_write_page_cmd, byte_of, page_of, parse_w_frame,
        patch_page_byte,
    };

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn enter_cmd_is_fixed_string() {
        assert_eq!(build_enter_cmd(), ENTER_PROGRAMMING_CMD);
    }

    #[test]
    fn exit_cmd_is_single_byte() {
        assert_eq!(build_exit_cmd(), vec![EXIT_CMD]);
        assert_eq!(EXIT_CMD, b'E');
    }

    #[test]
    fn ack_is_0x06() {
        assert_eq!(ACK, 0x06);
    }

    #[test]
    fn read_cmd_format() {
        let cmd = build_read_page_cmd(0x001C);
        assert_eq!(cmd, vec![b'R', 0x00, 0x1C, 0x00, 0x00]);
    }

    #[test]
    fn read_cmd_big_endian() {
        let cmd = build_read_page_cmd(0x1234);
        assert_eq!(cmd, vec![b'R', 0x12, 0x34, 0x00, 0x00]);
    }

    #[test]
    fn write_cmd_rejects_short_data() {
        let result = build_write_page_cmd(0, vec![0; 100]);
        assert!(
            matches!(
                result,
                Err(McpError::WrongPageSize {
                    expected: 256,
                    got: 100
                })
            ),
            "got {result:?}"
        );
    }

    #[test]
    fn write_cmd_rejects_factory_pages() {
        let result = build_write_page_cmd(MAX_WRITABLE_PAGE + 1, vec![0; PAGE_SIZE]);
        assert!(
            matches!(result, Err(McpError::FactoryCalibrationPage { .. })),
            "got {result:?}"
        );
    }

    #[test]
    fn write_cmd_happy_path() -> TestResult {
        let mut data = vec![0u8; PAGE_SIZE];
        *data.get_mut(0).ok_or("data[0] missing")? = 0xAB;
        *data.get_mut(255).ok_or("data[255] missing")? = 0xCD;
        let cmd = build_write_page_cmd(0x1C, data)?;
        assert_eq!(cmd.len(), W_FRAME_SIZE);
        assert_eq!(cmd.first().copied(), Some(b'W'));
        assert_eq!(cmd.get(1).copied(), Some(0x00));
        assert_eq!(cmd.get(2).copied(), Some(0x1C));
        assert_eq!(cmd.get(3).copied(), Some(0x00));
        assert_eq!(cmd.get(4).copied(), Some(0x00));
        assert_eq!(cmd.get(5).copied(), Some(0xAB));
        assert_eq!(cmd.get(260).copied(), Some(0xCD));
        Ok(())
    }

    #[test]
    fn parse_w_frame_round_trip() -> TestResult {
        let mut data = vec![0u8; PAGE_SIZE];
        *data.get_mut(0xA0).ok_or("data[0xA0] missing")? = GATEWAY_MODE_REFLECTOR_TERMINAL;
        let cmd = build_write_page_cmd(0x1C, data.clone())?;
        let parsed = parse_w_frame(cmd)?;
        assert_eq!(parsed.page, 0x1C);
        assert_eq!(parsed.data, data);
        Ok(())
    }

    #[test]
    fn parse_w_frame_rejects_bad_marker() -> TestResult {
        let mut bytes = vec![0u8; W_FRAME_SIZE];
        *bytes.get_mut(0).ok_or("bytes[0] missing")? = b'X';
        let result = parse_w_frame(bytes);
        assert!(
            matches!(result, Err(McpError::BadMarker { actual: b'X' })),
            "got {result:?}"
        );
        Ok(())
    }

    #[test]
    fn parse_w_frame_rejects_short() {
        let result = parse_w_frame(vec![b'W', 0x00, 0x1C, 0x00, 0x00]);
        assert!(
            matches!(result, Err(McpError::ResponseTooShort { .. })),
            "got {result:?}"
        );
    }

    #[test]
    fn page_and_byte_of_gateway_offset() {
        assert_eq!(page_of(GATEWAY_MODE_OFFSET), 0x1C);
        assert_eq!(byte_of(GATEWAY_MODE_OFFSET), 0xA0);
    }

    #[test]
    fn patch_page_byte_flips_offset() -> TestResult {
        let data = vec![0u8; PAGE_SIZE];
        let patched = patch_page_byte(data, 0xA0, GATEWAY_MODE_REFLECTOR_TERMINAL)?;
        assert_eq!(
            patched.get(0xA0).copied(),
            Some(GATEWAY_MODE_REFLECTOR_TERMINAL)
        );
        assert_eq!(patched.get(0xA1).copied(), Some(0));
        Ok(())
    }

    #[test]
    fn patch_page_byte_rejects_wrong_size() {
        let result = patch_page_byte(vec![0; 10], 0, 1);
        assert!(
            matches!(result, Err(McpError::WrongPageSize { .. })),
            "got {result:?}"
        );
    }
}
