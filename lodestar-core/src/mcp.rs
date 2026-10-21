// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

//! MCP (Memory Control Program) protocol primitives.
//!
//! Pure-logic byte encoders/decoders for the TH-D75's binary MCP
//! programming protocol. The orchestration (timing, transport, ACK
//! exchange, reconnect after exit) happens on the Swift side; this
//! module only produces and parses bytes.
//!
//! The schema selects CAT model `TH-D75` and the exact firmware identities in
//! [`MCP_D75_SCHEMA_FIRMWARE_IDENTITIES`] for USB CDC or Bluetooth SPP.
//!
//! # Protocol summary
//!
//! - **Entry:** send `\r0M PROGRAM\r` at 9600 baud. The leading carriage
//!   return terminates stale CAT bytes; the radio replies `0M\r`.
//! - **Read:** `R` + 2-byte BE page + `0x00 0x00` (5 bytes).
//!   Radio replies with a full `W` write frame (261 bytes) plus a
//!   trailing `ACK` (`0x06`).
//! - **Write:** `W` + 2-byte BE page + `0x00 0x00` + 256 data bytes
//!   (261 bytes total). Radio replies with a single `ACK` byte.
//! - **Exit:** single byte `E`. The radio replies with a single `ACK`
//!   byte, then drops the USB/BT connection; the caller must reconnect.
//!
//! # Reflector Terminal Mode
//!
//! Reflector Terminal Mode is a paired setting: Menu 985 selects the
//! physical PC interface and Menu 650 selects the gateway mode. The typed
//! schema returned by [`reflector_terminal_settings`] keeps both offsets and
//! stored values on the Rust side so Swift never duplicates firmware-specific
//! constants.

use std::sync::Arc;

use thiserror::Error;

/// Command to enter programming mode. Send at 9600 baud.
///
/// The leading carriage return terminates any stale, unterminated CAT
/// bytes before the firmware sees `0M PROGRAM`.
pub const ENTER_PROGRAMMING_CMD: &[u8] = b"\r0M PROGRAM\r";

/// Expected radio reply to [`ENTER_PROGRAMMING_CMD`].
pub const ENTER_RESPONSE: &[u8] = b"0M\r";

/// Single-byte acknowledgement used by the page and exit handshakes.
pub const ACK: u8 = 0x06;

/// Single-byte command to exit programming mode. Radio replies with [`ACK`].
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
/// the selected PC interface speaks MMDVM binary framing instead of CAT ASCII.
pub const GATEWAY_MODE_OFFSET: u16 = 0x1CA0;

/// MCP byte offset of the DV Gateway interface setting (Menu 985).
pub const GATEWAY_INTERFACE_OFFSET: u16 = 0x1093;

/// Gateway mode value: Off.
pub const GATEWAY_MODE_OFF: u8 = 0;
/// Gateway mode value: Reflector Terminal.
pub const GATEWAY_MODE_REFLECTOR_TERMINAL: u8 = 1;
/// Gateway mode value: Access Point.
pub const GATEWAY_MODE_ACCESS_POINT: u8 = 2;

/// Exact CAT model qualified for the bundled MCP-D75 schema.
pub const MCP_D75_SCHEMA_MODEL: &str = "TH-D75";

/// Exact CAT firmware identities qualified for the bundled MCP-D75 schema.
pub const MCP_D75_SCHEMA_FIRMWARE_IDENTITIES: &[&str] = &["1.03", "1.03.000", "1.03.AZM"];

/// Physical host interface selected by Menu 985.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum PcOutputInterface {
    /// USB CDC data interface.
    Usb,
    /// Bluetooth Classic SPP interface.
    Bluetooth,
}

impl PcOutputInterface {
    const fn stored_value(self) -> u8 {
        match self {
            Self::Usb => 0,
            Self::Bluetooth => 1,
        }
    }
}

/// Exact model and firmware identities accepted for MCP-D75 schema access.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct McpD75SchemaTarget {
    /// Exact CAT `ID` model string.
    model: String,
    /// Exact CAT `FV` strings whose memory layout has been qualified.
    firmware_identities: Vec<String>,
}

impl McpD75SchemaTarget {
    /// Exact CAT model required for the schema.
    #[must_use]
    pub fn model(&self) -> &str {
        &self.model
    }
    /// Accepted exact firmware strings.
    #[must_use]
    pub fn firmware_identities(&self) -> &[String] {
        &self.firmware_identities
    }
}

/// Firmware-specific stored fields used to enter Reflector Terminal Mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Record)]
pub struct ReflectorTerminalSettings {
    /// MCP offset of Menu 985.
    interface_offset: u16,
    /// Stored Menu 985 value for the requested physical link.
    interface_value: u8,
    /// MCP offset of Menu 650.
    mode_offset: u16,
    /// Stored Menu 650 value for Reflector Terminal Mode.
    mode_value: u8,
}

impl ReflectorTerminalSettings {
    /// Byte offset of the interface setting in the MCP image.
    #[must_use]
    pub const fn interface_offset(self) -> u16 {
        self.interface_offset
    }
    /// Stored interface value selected by the checked enum.
    #[must_use]
    pub const fn interface_value(self) -> u8 {
        self.interface_value
    }
    /// Byte offset of the gateway mode setting in the MCP image.
    #[must_use]
    pub const fn mode_offset(self) -> u16 {
        self.mode_offset
    }
    /// Stored Reflector Terminal value.
    #[must_use]
    pub const fn mode_value(self) -> u8 {
        self.mode_value
    }
}

/// Return the exact MCP-D75 schema qualification contract.
#[must_use]
#[uniffi::export]
pub fn mcp_d75_schema_target() -> McpD75SchemaTarget {
    McpD75SchemaTarget {
        model: MCP_D75_SCHEMA_MODEL.to_owned(),
        firmware_identities: MCP_D75_SCHEMA_FIRMWARE_IDENTITIES
            .iter()
            .map(|identity| (*identity).to_owned())
            .collect(),
    }
}

/// Return the paired Menu 985 and Menu 650 values for one host interface.
#[must_use]
#[uniffi::export]
pub fn reflector_terminal_settings(interface: PcOutputInterface) -> ReflectorTerminalSettings {
    ReflectorTerminalSettings {
        interface_offset: GATEWAY_INTERFACE_OFFSET,
        interface_value: interface.stored_value(),
        mode_offset: GATEWAY_MODE_OFFSET,
        mode_value: GATEWAY_MODE_REFLECTOR_TERMINAL,
    }
}

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
    /// Response addressed a nonzero byte offset within the page.
    #[error("expected page-aligned W response, got offset {actual}")]
    NonZeroOffset {
        /// The big-endian offset encoded in header bytes 3-4.
        actual: u16,
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

/// Build the 12-byte `\r0M PROGRAM\r` command.
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

/// Parsed `W` response from the radio (either from a read command or
/// echoed back during the write handshake).
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Object)]
pub struct McpPage {
    page: u16,
    data: [u8; PAGE_SIZE],
}

#[uniffi::export]
impl McpPage {
    /// Construct an immutable page from exactly 256 bytes.
    ///
    /// Any 16-bit page address can be observed; [`Self::write_command`]
    /// separately rejects addresses above the writable region.
    ///
    /// # Errors
    ///
    /// Returns [`McpError::WrongPageSize`] for any other data length.
    #[uniffi::constructor]
    pub fn new(page: u16, data: Vec<u8>) -> Result<Arc<Self>, McpError> {
        let actual = u32::try_from(data.len()).unwrap_or(u32::MAX);
        let data = data.try_into().map_err(|_| McpError::WrongPageSize {
            expected: 256,
            got: actual,
        })?;
        Ok(Arc::new(Self { page, data }))
    }

    /// The observed page address.
    #[must_use]
    pub const fn page(&self) -> u16 {
        self.page
    }

    /// Copy the complete 256-byte contents for the foreign caller.
    #[must_use]
    pub fn data(&self) -> Vec<u8> {
        self.data.to_vec()
    }

    /// Copy this page with one byte changed; every `u8` offset is in range.
    #[must_use]
    pub fn with_byte(&self, offset: u8, value: u8) -> Arc<Self> {
        let mut data = self.data;
        let Some(slot) = data.get_mut(usize::from(offset)) else {
            unreachable!("u8 offset fits a 256-byte page");
        };
        *slot = value;
        Arc::new(Self {
            page: self.page,
            data,
        })
    }

    /// Encode this page as one 261-byte `W` request.
    ///
    /// # Errors
    ///
    /// Returns [`McpError::FactoryCalibrationPage`] above
    /// [`MAX_WRITABLE_PAGE`]. Encoding performs no I/O or protocol retry.
    pub fn write_command(&self) -> Result<Vec<u8>, McpError> {
        if self.page > MAX_WRITABLE_PAGE {
            return Err(McpError::FactoryCalibrationPage {
                page: self.page,
                max: MAX_WRITABLE_PAGE,
            });
        }
        let [high, low] = self.page.to_be_bytes();
        let mut command = Vec::with_capacity(W_FRAME_SIZE);
        command.extend_from_slice(&[b'W', high, low, 0, 0]);
        command.extend_from_slice(&self.data);
        Ok(command)
    }

    /// Parse the first 261 bytes of a page-aligned `W` response.
    ///
    /// Bytes after the page are ignored, including a separately handled ACK.
    ///
    /// # Errors
    ///
    /// Returns [`McpError::ResponseTooShort`] below 261 bytes,
    /// [`McpError::BadMarker`] without `W`, or [`McpError::NonZeroOffset`]
    /// when header bytes 3-4 contain a nonzero big-endian offset.
    #[uniffi::constructor(name = "parse")]
    #[expect(
        clippy::needless_pass_by_value,
        reason = "UniFFI constructors receive owned foreign byte vectors"
    )]
    pub fn parse(bytes: Vec<u8>) -> Result<Arc<Self>, McpError> {
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
        let offset_hi = bytes.get(3).copied().unwrap_or(0);
        let offset_lo = bytes.get(4).copied().unwrap_or(0);
        let offset = u16::from_be_bytes([offset_hi, offset_lo]);
        if offset != 0 {
            return Err(McpError::NonZeroOffset { actual: offset });
        }
        let data = bytes
            .get(5..5 + PAGE_SIZE)
            .ok_or_else(|| McpError::ResponseTooShort {
                got: u32::try_from(bytes.len()).unwrap_or(u32::MAX),
                expected: u32::try_from(W_FRAME_SIZE).unwrap_or(u32::MAX),
            })?
            .to_vec();
        Self::new(page, data)
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ACK, ENTER_PROGRAMMING_CMD, EXIT_CMD, GATEWAY_INTERFACE_OFFSET, GATEWAY_MODE_OFFSET,
        GATEWAY_MODE_REFLECTOR_TERMINAL, MAX_WRITABLE_PAGE, MCP_D75_SCHEMA_FIRMWARE_IDENTITIES,
        MCP_D75_SCHEMA_MODEL, McpError, McpPage, PAGE_SIZE, PcOutputInterface, W_FRAME_SIZE,
        build_enter_cmd, build_exit_cmd, build_read_page_cmd, byte_of, mcp_d75_schema_target,
        page_of, reflector_terminal_settings,
    };

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn enter_cmd_is_fixed_string() {
        assert_eq!(build_enter_cmd(), ENTER_PROGRAMMING_CMD);
        assert_eq!(build_enter_cmd(), b"\r0M PROGRAM\r");
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
        let result = McpPage::new(0, vec![0; 100]);
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
    fn write_cmd_rejects_factory_pages() -> TestResult {
        let page = McpPage::new(MAX_WRITABLE_PAGE + 1, vec![0; PAGE_SIZE])?;
        let result = page.write_command();
        assert!(
            matches!(result, Err(McpError::FactoryCalibrationPage { .. })),
            "got {result:?}"
        );
        Ok(())
    }

    #[test]
    fn write_cmd_happy_path() -> TestResult {
        let mut data = vec![0u8; PAGE_SIZE];
        *data.get_mut(0).ok_or("data[0] missing")? = 0xAB;
        *data.get_mut(255).ok_or("data[255] missing")? = 0xCD;
        let cmd = McpPage::new(0x1C, data)?.write_command()?;
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
        let cmd = McpPage::new(0x1C, data.clone())?.write_command()?;
        let parsed = McpPage::parse(cmd)?;
        assert_eq!(parsed.page, 0x1C);
        assert_eq!(parsed.data(), data);
        Ok(())
    }

    #[test]
    fn parse_w_frame_rejects_bad_marker() -> TestResult {
        let mut bytes = vec![0u8; W_FRAME_SIZE];
        *bytes.get_mut(0).ok_or("bytes[0] missing")? = b'X';
        let result = McpPage::parse(bytes);
        assert!(
            matches!(result, Err(McpError::BadMarker { actual: b'X' })),
            "got {result:?}"
        );
        Ok(())
    }

    #[test]
    fn parse_w_frame_rejects_short() {
        let result = McpPage::parse(vec![b'W', 0x00, 0x1C, 0x00, 0x00]);
        assert!(
            matches!(result, Err(McpError::ResponseTooShort { .. })),
            "got {result:?}"
        );
    }

    #[test]
    fn parse_w_frame_rejects_nonzero_offset() -> TestResult {
        let mut bytes = McpPage::new(0x1C, vec![0; PAGE_SIZE])?.write_command()?;
        *bytes.get_mut(4).ok_or("offset byte missing")? = 1;
        let result = McpPage::parse(bytes);
        assert_eq!(result, Err(McpError::NonZeroOffset { actual: 1 }));
        Ok(())
    }

    #[test]
    fn page_and_byte_of_gateway_offset() {
        assert_eq!(page_of(GATEWAY_MODE_OFFSET), 0x1C);
        assert_eq!(byte_of(GATEWAY_MODE_OFFSET), 0xA0);
    }

    #[test]
    fn terminal_schema_maps_both_physical_interfaces() {
        let usb = reflector_terminal_settings(PcOutputInterface::Usb);
        let bluetooth = reflector_terminal_settings(PcOutputInterface::Bluetooth);

        assert_eq!(usb.interface_offset, GATEWAY_INTERFACE_OFFSET);
        assert_eq!(usb.interface_value, 0);
        assert_eq!(bluetooth.interface_offset, GATEWAY_INTERFACE_OFFSET);
        assert_eq!(bluetooth.interface_value, 1);
        assert_eq!(usb.mode_offset, GATEWAY_MODE_OFFSET);
        assert_eq!(usb.mode_value, GATEWAY_MODE_REFLECTOR_TERMINAL);
        assert_eq!(bluetooth.mode_offset, GATEWAY_MODE_OFFSET);
        assert_eq!(bluetooth.mode_value, GATEWAY_MODE_REFLECTOR_TERMINAL);
    }

    #[test]
    fn schema_target_includes_azimuth_firmware_identity() {
        let target = mcp_d75_schema_target();
        assert_eq!(target.model, MCP_D75_SCHEMA_MODEL);
        assert_eq!(
            target.firmware_identities,
            MCP_D75_SCHEMA_FIRMWARE_IDENTITIES
        );
        assert!(
            target
                .firmware_identities
                .iter()
                .any(|identity| identity == "1.03.AZM")
        );
    }

    #[test]
    fn patch_page_byte_flips_offset() -> TestResult {
        let data = vec![0u8; PAGE_SIZE];
        let original = McpPage::new(0x1C, data)?;
        let patched = original.with_byte(0xA0, GATEWAY_MODE_REFLECTOR_TERMINAL);
        assert_eq!(
            patched.data().get(0xA0).copied(),
            Some(GATEWAY_MODE_REFLECTOR_TERMINAL)
        );
        assert_eq!(patched.data().get(0xA1).copied(), Some(0));
        assert_eq!(
            original.data().get(0xA0).copied(),
            Some(0),
            "immutable source page changed"
        );
        Ok(())
    }

    #[test]
    fn patch_page_byte_rejects_wrong_size() {
        let result = McpPage::new(0, vec![0; 10]);
        assert!(
            matches!(result, Err(McpError::WrongPageSize { .. })),
            "got {result:?}"
        );
    }

    #[test]
    fn page_parse_keeps_readonly_addresses_and_ignores_trailing_ack() -> TestResult {
        let mut bytes = McpPage::new(MAX_WRITABLE_PAGE, vec![0xA5; PAGE_SIZE])?.write_command()?;
        let address = u16::MAX.to_be_bytes();
        bytes
            .get_mut(1..3)
            .ok_or("address bytes")?
            .copy_from_slice(&address);
        bytes.extend_from_slice(&[ACK, b'X']);
        let page = McpPage::parse(bytes)?;
        assert_eq!(page.page(), u16::MAX);
        assert_eq!(page.data(), vec![0xA5; PAGE_SIZE]);
        assert!(
            matches!(
                page.write_command(),
                Err(McpError::FactoryCalibrationPage { .. })
            ),
            "observed calibration page became writable"
        );
        assert_eq!(page.with_byte(255, 1).data().last(), Some(&1));
        Ok(())
    }
}
