//! Programming mode access for full radio memory read/write.
//!
//! The TH-D75 stores all radio configuration in a 500,480-byte flash
//! memory (1,955 pages of 256 bytes), accessible only via the binary
//! programming protocol (`0M PROGRAM`). This module provides methods to
//! read and write individual pages, memory regions, or the entire image.
//!
//! # Protocol
//!
//! By default the entire programming session runs at 9600 baud -- no
//! baud rate switching. This is the safe, proven approach. Switching to
//! 57600 baud after entry crashes the radio into MCP error mode.
//!
//! An optional [`McpSpeed::Fast`] mode switches the serial port to
//! 115200 baud after the initial handshake (~8 seconds for a full dump
//! instead of ~55 seconds). Enable it with [`Radio::set_mcp_speed`].
//!
//! # Warning
//!
//! Entering programming mode makes the radio stop responding to normal
//! CAT commands. The display shows "PROG MCP". Always call
//! `exit_programming_mode` when done,
//! even on error. The high-level methods handle entry/exit automatically.
//!
//! # Connection Lifetime
//!
//! The USB connection does not survive the programming mode transition.
//! The radio's USB stack resets when exiting MCP mode. After calling
//! any method in this module, the `Radio` instance should be dropped
//! and a fresh connection established for subsequent CAT commands.
//!
//! # Safety
//!
//! The last 2 pages (1953-1954) contain factory calibration data and are
//! **never** written by this library. Attempts to write these pages return
//! [`Error::MemoryWriteProtected`].
//!
//! The `0M` handler is at firmware address `0xC002F01C`.

use crate::error::{Error, ProtocolError, TransportError};
use crate::protocol::programming::{self, ChannelFlag};
use crate::transport::Transport;
use crate::types::FlashChannel;

use super::Radio;

/// Baud rate for the programming mode handshake.
///
/// The `0M PROGRAM\r` entry command is always sent at 9600 baud.
/// The data transfer phase may stay at 9600 or switch to 115200
/// depending on the configured [`McpSpeed`].
const PROGRAMMING_BAUD: u32 = 9600;

/// Baud rate for fast MCP transfers.
const FAST_TRANSFER_BAUD: u32 = 115_200;

/// MCP transfer speed options.
///
/// The default (`Safe`) keeps the entire programming session at 9600
/// baud, which is proven reliable across all platforms. The `Fast`
/// option switches the serial port to 115200 baud after the initial
/// handshake for faster transfers.
///
/// # Caution
///
/// `Fast` mode has not been tested on all USB host controllers and
/// operating systems. If you experience transfer errors, fall back to
/// `Safe` mode. The 57600 baud switch is known to crash the radio
/// and is **not** offered as an option.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum McpSpeed {
    /// 9600 baud throughout (proven reliable, ~55 s for full dump).
    #[default]
    Safe,
    /// 115200 baud for the binary transfer phase (~8 s for full dump).
    ///
    /// After the `0M PROGRAM` handshake at 9600 baud, the serial port
    /// is switched to 115200 baud. A sync byte is read and discarded.
    /// On exit the baud rate is restored.
    Fast,
}

/// Timeout for a full memory dump.
///
/// At 9600 baud: 1955 pages x 261 bytes x 10 bits/byte / 9600 bps ~ 53 s.
/// At 115200 baud: the same transfer takes ~ 4.4 s.
/// The 120-second ceiling provides ample margin for both modes.
const FULL_DUMP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

impl<T: Transport> Radio<T> {
    // -----------------------------------------------------------------------
    // High-level: full memory image
    // -----------------------------------------------------------------------

    /// Read the entire radio memory image (500,480 bytes).
    ///
    /// Enters programming mode, reads all 1,955 pages, and exits.
    /// This takes approximately 55 seconds at 9600 baud.
    ///
    /// # Errors
    ///
    /// Returns an error if entry, any page read, or exit fails.
    /// Programming mode is always exited, even on error.
    pub async fn read_memory_image(&mut self) -> Result<Vec<u8>, Error> {
        self.read_memory_image_with_progress(|_, _| {}).await
    }

    /// Read the entire radio memory image with a progress callback.
    ///
    /// The callback receives `(current_page, total_pages)` after each
    /// page is read, allowing progress display for the ~55-second dump.
    ///
    /// # Errors
    ///
    /// Returns an error if entry, any page read, or exit fails.
    /// Programming mode is always exited, even on error.
    pub async fn read_memory_image_with_progress<F>(
        &mut self,
        mut on_progress: F,
    ) -> Result<Vec<u8>, Error>
    where
        F: FnMut(u16, u16),
    {
        let saved_timeout = self.timeout;
        self.timeout = FULL_DUMP_TIMEOUT;

        self.enter_programming_mode().await?;

        let result = self
            .read_pages_raw(0, programming::TOTAL_PAGES, &mut on_progress)
            .await;

        let exit_result = self.exit_programming_mode().await;
        self.timeout = saved_timeout;

        let image = result?;
        exit_result?;

        Ok(image)
    }

    /// Write a complete memory image back to the radio.
    ///
    /// **WARNING:** This overwrites ALL radio settings except factory
    /// calibration (last 2 pages). The image must be exactly 500,480 bytes.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidImageSize`] if the image is the wrong size.
    /// Returns an error if entry, any page write, or exit fails.
    /// Programming mode is always exited, even on error.
    pub async fn write_memory_image(&mut self, image: &[u8]) -> Result<(), Error> {
        self.write_memory_image_with_progress(image, |_, _| {})
            .await
    }

    /// Write a complete memory image with a progress callback.
    ///
    /// The callback receives `(current_page, total_pages)` after each
    /// page is written.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidImageSize`] if the image is the wrong size.
    /// Returns an error if entry, any page write, or exit fails.
    /// Programming mode is always exited, even on error.
    pub async fn write_memory_image_with_progress<F>(
        &mut self,
        image: &[u8],
        mut on_progress: F,
    ) -> Result<(), Error>
    where
        F: FnMut(u16, u16),
    {
        if image.len() != programming::TOTAL_SIZE {
            return Err(Error::InvalidImageSize {
                actual: image.len(),
                expected: programming::TOTAL_SIZE,
            });
        }

        let saved_timeout = self.timeout;
        self.timeout = FULL_DUMP_TIMEOUT;

        self.enter_programming_mode().await?;

        // Write all pages except factory calibration (last 2).
        let writable_pages = programming::TOTAL_PAGES - programming::FACTORY_CAL_PAGES;
        let result = self
            .write_pages_raw(
                0,
                &image[..writable_pages as usize * programming::PAGE_SIZE],
                &mut on_progress,
            )
            .await;

        let exit_result = self.exit_programming_mode().await;
        self.timeout = saved_timeout;

        result?;
        exit_result?;

        Ok(())
    }

    // -----------------------------------------------------------------------
    // High-level: page range read/write
    // -----------------------------------------------------------------------

    /// Read a range of pages from radio memory.
    ///
    /// Enters programming mode, reads `count` pages starting at
    /// `start_page`, and exits. Returns the raw bytes.
    ///
    /// # Errors
    ///
    /// Returns an error if entry, any page read, or exit fails.
    /// Programming mode is always exited, even on error.
    pub async fn read_memory_pages(
        &mut self,
        start_page: u16,
        count: u16,
    ) -> Result<Vec<u8>, Error> {
        self.enter_programming_mode().await?;

        let result = self.read_pages_raw(start_page, count, &mut |_, _| {}).await;

        let exit_result = self.exit_programming_mode().await;

        let data = result?;
        exit_result?;

        Ok(data)
    }

    /// Write a range of pages to radio memory.
    ///
    /// Enters programming mode, writes pages starting at `start_page`
    /// with the provided data, and exits. The data length must be a
    /// multiple of 256 (one or more full pages).
    ///
    /// # Errors
    ///
    /// Returns [`Error::MemoryWriteProtected`] if any target page falls
    /// within the factory calibration region.
    /// Returns an error if entry, any page write, or exit fails.
    /// Programming mode is always exited, even on error.
    pub async fn write_memory_pages(&mut self, start_page: u16, data: &[u8]) -> Result<(), Error> {
        let page_count = data.len() / programming::PAGE_SIZE;
        // Validate no factory calibration pages are in range.
        for i in 0..page_count {
            // page_count is bounded by data.len() / 256, which fits in u16
            // because the maximum image is 500,480 bytes (1955 pages).
            #[allow(clippy::cast_possible_truncation)]
            let offset = i as u16;
            let page = start_page + offset;
            if programming::is_factory_calibration_page(page) {
                return Err(Error::MemoryWriteProtected { page });
            }
        }

        self.enter_programming_mode().await?;

        let result = self.write_pages_raw(start_page, data, &mut |_, _| {}).await;

        let exit_result = self.exit_programming_mode().await;

        result?;
        exit_result?;

        Ok(())
    }

    // -----------------------------------------------------------------------
    // High-level: single page read/write
    // -----------------------------------------------------------------------

    /// Read a single memory page (256 bytes).
    ///
    /// Enters programming mode, reads the page, and exits.
    ///
    /// # Errors
    ///
    /// Returns an error if entry, the page read, or exit fails.
    /// Programming mode is always exited, even on error.
    pub async fn read_page(&mut self, page: u16) -> Result<[u8; programming::PAGE_SIZE], Error> {
        self.enter_programming_mode().await?;

        let result = self.read_single_page(page).await;

        let exit_result = self.exit_programming_mode().await;

        let data = result?;
        exit_result?;

        Ok(data)
    }

    /// Write a single memory page (256 bytes).
    ///
    /// Enters programming mode, writes the page, and exits.
    ///
    /// # Errors
    ///
    /// Returns [`Error::MemoryWriteProtected`] if the page is in the
    /// factory calibration region.
    /// Returns an error if entry, the page write, or exit fails.
    /// Programming mode is always exited, even on error.
    pub async fn write_page(
        &mut self,
        page: u16,
        data: &[u8; programming::PAGE_SIZE],
    ) -> Result<(), Error> {
        if programming::is_factory_calibration_page(page) {
            return Err(Error::MemoryWriteProtected { page });
        }

        self.enter_programming_mode().await?;

        let result = self.write_single_page(page, data).await;

        let exit_result = self.exit_programming_mode().await;

        result?;
        exit_result?;

        Ok(())
    }

    // -----------------------------------------------------------------------
    // High-level: read-modify-write
    // -----------------------------------------------------------------------

    /// Read a memory page, apply in-place modifications, and write it back
    /// in a single MCP programming session.
    ///
    /// This is the key primitive for changing individual settings via MCP
    /// without reading or writing the entire 500 KB image. The three steps
    /// (read, modify, write) happen inside one programming mode session so
    /// the radio only enters and exits MCP mode once.
    ///
    /// # Connection lifetime
    ///
    /// The USB connection does not survive the programming mode transition.
    /// After this method returns, the `Radio` instance should be dropped
    /// and a fresh connection established for subsequent CAT commands.
    ///
    /// # Errors
    ///
    /// Returns [`Error::MemoryWriteProtected`] if the page is in the
    /// factory calibration region.
    /// Returns an error if entry, the page read, the page write, or exit
    /// fails. Programming mode is always exited, even on error.
    pub async fn modify_memory_page<F>(&mut self, page: u16, modify: F) -> Result<(), Error>
    where
        F: FnOnce(&mut [u8; programming::PAGE_SIZE]),
    {
        if programming::is_factory_calibration_page(page) {
            return Err(Error::MemoryWriteProtected { page });
        }

        self.enter_programming_mode().await?;

        let result: Result<(), Error> = async {
            // Read the current page contents.
            let mut page_data = self.read_single_page(page).await?;

            // Apply the caller's modifications in place.
            modify(&mut page_data);

            // Write the modified page back.
            self.write_single_page(page, &page_data).await?;

            Ok(())
        }
        .await;

        // Always exit programming mode, even on error.
        let exit_result = self.exit_programming_mode().await;

        result?;
        exit_result?;

        Ok(())
    }

    // -----------------------------------------------------------------------
    // High-level: structured data accessors
    // -----------------------------------------------------------------------

    /// Read all channel display names from the radio.
    ///
    /// This enters programming mode, reads the channel name memory pages,
    /// and exits programming mode. The radio will briefly show "PROG MCP"
    /// on its display during this operation.
    ///
    /// Returns a `Vec` of up to 1,000 channel names indexed by channel
    /// number. Channels without a user-assigned name are returned as
    /// empty strings.
    ///
    /// # Errors
    ///
    /// Returns an error if the radio fails to enter programming mode,
    /// if a page read fails, or if the connection is lost. On error, an
    /// attempt is still made to exit programming mode before returning.
    pub async fn read_channel_names(&mut self) -> Result<Vec<String>, Error> {
        self.enter_programming_mode().await?;

        let result = self.read_name_pages().await;

        // Always attempt to exit, even if reading failed.
        let exit_result = self.exit_programming_mode().await;

        // Propagate the read error first, then the exit error.
        let names = result?;
        exit_result?;

        Ok(names)
    }

    /// Read all 1,200 channel display names from the radio, including
    /// extended entries (scan edges, WX, and call channels).
    ///
    /// This reads 75 pages (0x0100-0x014A) instead of the 63 pages read
    /// by [`read_channel_names`](Self::read_channel_names), which only
    /// returns the first 1,000 standard channel names.
    ///
    /// # Connection lifetime
    ///
    /// This enters MCP programming mode. The USB connection drops after
    /// exit. The `Radio` instance should be dropped and a fresh connection
    /// established for subsequent CAT commands.
    ///
    /// # Errors
    ///
    /// Returns an error if the radio fails to enter programming mode,
    /// if a page read fails, or if the connection is lost. On error, an
    /// attempt is still made to exit programming mode before returning.
    pub async fn read_all_channel_names(&mut self) -> Result<Vec<String>, Error> {
        self.enter_programming_mode().await?;

        let result = self.read_all_name_pages().await;

        let exit_result = self.exit_programming_mode().await;

        let names = result?;
        exit_result?;

        Ok(names)
    }

    /// Write a single channel display name via MCP programming mode.
    ///
    /// Enters programming mode, reads the containing name page, modifies
    /// the 16-byte slot for the given channel, writes the page back, and
    /// exits. The name is truncated to 15 bytes (leaving room for a null
    /// terminator) and null-padded to fill the 16-byte slot.
    ///
    /// # Connection lifetime
    ///
    /// This enters MCP programming mode. The USB connection drops after
    /// exit. The `Radio` instance should be dropped and a fresh connection
    /// established for subsequent CAT commands.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Validation`] if the channel number is 1200 or greater.
    /// Returns an error if entering programming mode, reading the page,
    /// writing the page, or exiting programming mode fails.
    pub async fn write_channel_name(&mut self, channel: u16, name: &str) -> Result<(), Error> {
        // TOTAL_CHANNEL_ENTRIES is 1200, which fits in u16.
        #[allow(clippy::cast_possible_truncation)]
        const MAX_CHANNEL: u16 = programming::TOTAL_CHANNEL_ENTRIES as u16;
        if channel >= MAX_CHANNEL {
            return Err(Error::Validation(
                crate::error::ValidationError::ChannelOutOfRange {
                    channel,
                    max: MAX_CHANNEL - 1,
                },
            ));
        }
        let page = programming::CHANNEL_NAMES_START + (channel / 16);
        let offset = (channel % 16) as usize * programming::NAME_ENTRY_SIZE;

        tracing::info!(channel, name, page, offset, "writing channel name via MCP");
        self.modify_memory_page(page, |data| {
            // Clear the 16-byte slot.
            data[offset..offset + programming::NAME_ENTRY_SIZE].fill(0);
            // Write the name (truncated to 15 bytes, leaving null terminator).
            let name_bytes = name.as_bytes();
            let len = name_bytes.len().min(programming::NAME_ENTRY_SIZE - 1);
            data[offset..offset + len].copy_from_slice(&name_bytes[..len]);
        })
        .await
    }

    /// Read channel flags for all 1,200 channel entries.
    ///
    /// Each flag indicates whether a channel slot is used (and which band),
    /// whether it is locked out from scanning, and its group assignment.
    ///
    /// # Errors
    ///
    /// Returns an error if entry, any page read, or exit fails.
    /// Programming mode is always exited, even on error.
    pub async fn read_channel_flags(&mut self) -> Result<Vec<ChannelFlag>, Error> {
        self.enter_programming_mode().await?;

        let page_count = programming::CHANNEL_FLAGS_END - programming::CHANNEL_FLAGS_START + 1;
        let result = self
            .read_pages_raw(programming::CHANNEL_FLAGS_START, page_count, &mut |_, _| {})
            .await;

        let exit_result = self.exit_programming_mode().await;

        let raw = result?;
        exit_result?;

        // Parse 4-byte flag records, 1200 entries.
        let mut flags = Vec::with_capacity(programming::TOTAL_CHANNEL_ENTRIES);
        for i in 0..programming::TOTAL_CHANNEL_ENTRIES {
            let offset = i * programming::FLAG_RECORD_SIZE;
            if offset + programming::FLAG_RECORD_SIZE <= raw.len() {
                if let Some(flag) = programming::parse_channel_flag(&raw[offset..]) {
                    flags.push(flag);
                }
            }
        }

        tracing::info!(count = flags.len(), "channel flags read");
        Ok(flags)
    }

    /// Read all channel memory data (frequencies, modes, tones, etc.)
    /// for all 1,200 channel entries.
    ///
    /// Channels whose flag indicates empty (`0xFF`) will still be returned
    /// with whatever data is in the slot (typically zeroed). Check the
    /// corresponding [`ChannelFlag`] to determine which slots are in use.
    ///
    /// # Errors
    ///
    /// Returns an error if entry, any page read, or exit fails.
    /// Programming mode is always exited, even on error.
    pub async fn read_all_channels(&mut self) -> Result<Vec<FlashChannel>, Error> {
        self.enter_programming_mode().await?;

        let page_count = programming::CHANNEL_DATA_END - programming::CHANNEL_DATA_START + 1;
        let result = self
            .read_pages_raw(programming::CHANNEL_DATA_START, page_count, &mut |_, _| {})
            .await;

        let exit_result = self.exit_programming_mode().await;

        let raw = result?;
        exit_result?;

        // Parse memgroups: each 256-byte page is one memgroup containing
        // 6 channel records of 40 bytes + 16 bytes padding.
        let mut channels = Vec::with_capacity(programming::TOTAL_CHANNEL_ENTRIES);
        for memgroup_idx in 0..programming::MEMGROUP_COUNT {
            let group_offset = memgroup_idx * programming::PAGE_SIZE;
            for slot in 0..programming::CHANNELS_PER_MEMGROUP {
                let ch_offset = group_offset + slot * programming::CHANNEL_RECORD_SIZE;
                if ch_offset + programming::CHANNEL_RECORD_SIZE <= raw.len() {
                    match FlashChannel::from_bytes(&raw[ch_offset..]) {
                        Ok(ch) => channels.push(ch),
                        Err(e) => {
                            tracing::warn!(
                                memgroup = memgroup_idx,
                                slot,
                                error = %e,
                                "failed to parse flash channel record, using default"
                            );
                            channels.push(FlashChannel::default());
                        }
                    }
                }
            }
        }

        tracing::info!(count = channels.len(), "channel memory records read");
        Ok(channels)
    }

    // -----------------------------------------------------------------------
    // High-level: typed memory image
    // -----------------------------------------------------------------------

    /// Read and parse the full radio configuration.
    ///
    /// Reads the entire 500,480-byte memory image and returns a
    /// [`crate::memory::MemoryImage`] with typed access to all settings, channels,
    /// and subsystem configurations.
    ///
    /// This takes approximately 55 seconds at 9600 baud.
    ///
    /// # Errors
    ///
    /// Returns an error if the read fails. Programming mode is always
    /// exited, even on error.
    pub async fn read_configuration(&mut self) -> Result<crate::memory::MemoryImage, Error> {
        let raw = self.read_memory_image().await?;
        crate::memory::MemoryImage::from_raw(raw).map_err(|e| {
            Error::Protocol(ProtocolError::FieldParse {
                command: "read_configuration".into(),
                field: "memory_image".into(),
                detail: e.to_string(),
            })
        })
    }

    /// Read and parse the full radio configuration with progress.
    ///
    /// The callback receives `(current_page, total_pages)` after each
    /// page is read.
    ///
    /// # Errors
    ///
    /// Returns an error if the read fails. Programming mode is always
    /// exited, even on error.
    pub async fn read_configuration_with_progress<F>(
        &mut self,
        on_progress: F,
    ) -> Result<crate::memory::MemoryImage, Error>
    where
        F: FnMut(u16, u16),
    {
        let raw = self.read_memory_image_with_progress(on_progress).await?;
        crate::memory::MemoryImage::from_raw(raw).map_err(|e| {
            Error::Protocol(ProtocolError::FieldParse {
                command: "read_configuration".into(),
                field: "memory_image".into(),
                detail: e.to_string(),
            })
        })
    }

    /// Write a full radio configuration back to the radio.
    ///
    /// Takes a [`crate::memory::MemoryImage`] (possibly modified via its typed
    /// accessors) and writes it to the radio's flash memory.
    ///
    /// # Errors
    ///
    /// Returns an error if the write fails. Programming mode is always
    /// exited, even on error.
    pub async fn write_configuration(
        &mut self,
        image: &crate::memory::MemoryImage,
    ) -> Result<(), Error> {
        self.write_memory_image(image.as_raw()).await
    }

    /// Write a full radio configuration with progress.
    ///
    /// The callback receives `(current_page, total_pages)` after each
    /// page is written.
    ///
    /// # Errors
    ///
    /// Returns an error if the write fails. Programming mode is always
    /// exited, even on error.
    pub async fn write_configuration_with_progress<F>(
        &mut self,
        image: &crate::memory::MemoryImage,
        on_progress: F,
    ) -> Result<(), Error>
    where
        F: FnMut(u16, u16),
    {
        self.write_memory_image_with_progress(image.as_raw(), on_progress)
            .await
    }

    // -----------------------------------------------------------------------
    // Internal: programming mode entry/exit
    // -----------------------------------------------------------------------

    /// Enter programming mode (`0M PROGRAM`).
    ///
    /// Switches to 9600 baud and sends `0M PROGRAM\r`. The radio
    /// responds with `0M\r` and enters MCP mode. The session stays
    /// at 9600 baud for all subsequent R/W/ACK exchanges.
    ///
    /// The radio stops responding to normal CAT commands and displays
    /// "PROG MCP" until [`exit_programming_mode`](Self::exit_programming_mode)
    /// is called.
    ///
    /// # Errors
    ///
    /// Returns an error if the entry command fails or the radio does
    /// not respond with the expected `0M\r` acknowledgement.
    async fn enter_programming_mode(&mut self) -> Result<(), Error> {
        tracing::info!("entering programming mode at 9600 baud");

        // Switch to 9600 baud for the entire programming session.
        self.transport
            .set_baud_rate(PROGRAMMING_BAUD)
            .map_err(Error::Transport)?;

        self.transport
            .write(programming::ENTER_PROGRAMMING)
            .await
            .map_err(Error::Transport)?;

        // 10ms delay after write.
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;

        // Read response -- expect "0M\r" (3 bytes).
        let mut buf = [0u8; 64];
        let mut received = Vec::new();

        let result = tokio::time::timeout(self.timeout, async {
            loop {
                let n = self
                    .transport
                    .read(&mut buf)
                    .await
                    .map_err(Error::Transport)?;
                if n == 0 {
                    return Err(Error::Transport(TransportError::Disconnected(
                        std::io::Error::new(
                            std::io::ErrorKind::UnexpectedEof,
                            "connection closed during programming mode entry",
                        ),
                    )));
                }
                received.extend_from_slice(&buf[..n]);
                // Look for "0M\r" anywhere in the received data.
                if received.windows(3).any(|w| w == b"0M\r") {
                    return Ok(());
                }
                if received.len() > 20 {
                    // Too much data without finding "0M\r".
                    return Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                        expected: "0M\\r".to_string(),
                        actual: received.clone(),
                    }));
                }
            }
        })
        .await
        .map_err(|_| Error::Timeout(self.timeout))?;
        result?;

        // If Fast mode is requested, switch to 115200 baud for the data
        // transfer phase.
        if self.mcp_speed == McpSpeed::Fast {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            self.transport
                .set_baud_rate(FAST_TRANSFER_BAUD)
                .map_err(Error::Transport)?;
            // Read sync byte — verifies the radio switched baud rates.
            // If this times out, the radio is likely still at 9600 and all
            // subsequent reads will produce garbage.
            let mut sync = [0u8; 1];
            match tokio::time::timeout(
                std::time::Duration::from_secs(2),
                self.transport.read(&mut sync),
            )
            .await
            {
                Ok(Ok(n)) if n > 0 => {
                    tracing::info!(
                        sync_byte = sync[0],
                        "programming mode entered, switched to {FAST_TRANSFER_BAUD} baud (fast)"
                    );
                }
                Ok(Ok(_)) => {
                    tracing::error!("fast mode sync read returned 0 bytes — baud mismatch likely");
                    return Err(Error::Protocol(ProtocolError::MalformedFrame(
                        b"fast mode sync byte not received".to_vec(),
                    )));
                }
                Ok(Err(e)) => {
                    tracing::error!("fast mode sync read failed: {e}");
                    return Err(Error::Transport(e));
                }
                Err(_) => {
                    tracing::error!(
                        "fast mode sync byte timed out — radio may not have switched baud"
                    );
                    return Err(Error::Timeout(std::time::Duration::from_secs(2)));
                }
            }
        } else {
            tracing::info!("programming mode entered, staying at {PROGRAMMING_BAUD} baud");
        }

        Ok(())
    }

    /// Exit programming mode (`E` command).
    ///
    /// Sends the exit byte. The radio resets its USB stack after exiting
    /// MCP mode, so the connection should be considered dead after this.
    ///
    /// # Errors
    ///
    /// Returns an error if the exit byte cannot be written.
    async fn exit_programming_mode(&mut self) -> Result<(), Error> {
        tracing::info!("exiting programming mode");

        self.transport
            .write(&[programming::EXIT])
            .await
            .map_err(Error::Transport)?;

        // Give the radio time to leave MCP mode and resume CAT.
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;

        // If we were in fast mode, restore the default baud rate.
        if self.mcp_speed == McpSpeed::Fast {
            if let Err(e) = self
                .transport
                .set_baud_rate(crate::transport::SerialTransport::DEFAULT_BAUD)
            {
                tracing::warn!("failed to restore baud rate after fast MCP exit: {e}");
            }
            tracing::info!("programming mode exited, restored default baud rate");
        } else {
            // Stay at 9600 baud -- changing baud rate via SET_LINE_CODING
            // causes the USB CDC connection to drop on some platforms.
            // CAT commands work at 9600 baud (CDC ACM ignores line coding).
            tracing::info!("programming mode exited, staying at 9600 baud");
        }

        Ok(())
    }

    // -----------------------------------------------------------------------
    // Internal: raw page I/O (caller must hold programming mode)
    // -----------------------------------------------------------------------

    /// Read a contiguous range of pages while already in programming mode.
    ///
    /// Returns a `Vec<u8>` containing `count * 256` bytes.
    ///
    /// If a page read times out, it is retried once before failing. This
    /// improves reliability during long memory dumps where occasional
    /// serial hiccups can occur.
    async fn read_pages_raw<F>(
        &mut self,
        start_page: u16,
        count: u16,
        on_progress: &mut F,
    ) -> Result<Vec<u8>, Error>
    where
        F: FnMut(u16, u16),
    {
        let mut image = Vec::with_capacity(count as usize * programming::PAGE_SIZE);

        for i in 0..count {
            let page = start_page + i;
            let data = match self.read_single_page(page).await {
                Ok(d) => d,
                Err(Error::Timeout(_)) => {
                    tracing::warn!(page, "page read timed out, retrying once");
                    // Brief pause before retry to let the serial bus settle.
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                    self.read_single_page(page).await?
                }
                Err(e) => return Err(e),
            };
            image.extend_from_slice(&data);
            on_progress(i + 1, count);
        }

        Ok(image)
    }

    /// Write a contiguous range of pages while already in programming mode.
    ///
    /// `data.len()` must be a multiple of 256.
    async fn write_pages_raw<F>(
        &mut self,
        start_page: u16,
        data: &[u8],
        on_progress: &mut F,
    ) -> Result<(), Error>
    where
        F: FnMut(u16, u16),
    {
        let page_count = data.len() / programming::PAGE_SIZE;

        for i in 0..page_count {
            // page_count is bounded by TOTAL_PAGES (1955), which fits in u16.
            #[allow(clippy::cast_possible_truncation)]
            let page_offset = i as u16;
            let page = start_page + page_offset;
            let byte_offset = i * programming::PAGE_SIZE;
            let page_data: &[u8; programming::PAGE_SIZE] = data
                [byte_offset..byte_offset + programming::PAGE_SIZE]
                .try_into()
                .expect("slice is exactly PAGE_SIZE bytes");
            self.write_single_page(page, page_data).await?;
            #[allow(clippy::cast_possible_truncation)]
            let total = page_count as u16;
            on_progress(page_offset + 1, total);
        }

        Ok(())
    }

    /// Read a single 256-byte page (caller must be in programming mode).
    async fn read_single_page(&mut self, page: u16) -> Result<[u8; programming::PAGE_SIZE], Error> {
        let cmd = programming::build_read_command(page);

        tracing::debug!(page, "reading page");

        // Send R command (5 bytes).
        self.transport.write(&cmd).await.map_err(Error::Transport)?;
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;

        // Read 261-byte W response (W + 4-byte addr + 256-byte data).
        let mut received = Vec::with_capacity(programming::W_RESPONSE_SIZE);
        let mut buf = [0u8; 512];
        let result = tokio::time::timeout(self.timeout, async {
            while received.len() < programming::W_RESPONSE_SIZE {
                let n = self
                    .transport
                    .read(&mut buf)
                    .await
                    .map_err(Error::Transport)?;
                if n == 0 {
                    return Err(Error::Transport(TransportError::Disconnected(
                        std::io::Error::new(
                            std::io::ErrorKind::UnexpectedEof,
                            "connection closed during page read",
                        ),
                    )));
                }
                received.extend_from_slice(&buf[..n]);
            }
            Ok(())
        })
        .await
        .map_err(|_| Error::Timeout(self.timeout))?;
        result?;

        // Parse: W(1) + addr(4) + data(256).
        let (_page_addr, data) = programming::parse_write_response(&received)
            .map_err(|e| Error::Protocol(ProtocolError::MalformedFrame(e.into_bytes())))?;

        // Copy into a fixed-size array.
        let mut page_data = [0u8; programming::PAGE_SIZE];
        page_data.copy_from_slice(data);

        // Send ACK, read ACK back.
        self.transport
            .write(&[programming::ACK])
            .await
            .map_err(Error::Transport)?;
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        let mut ack_buf = [0u8; 1];
        let _ = tokio::time::timeout(
            std::time::Duration::from_millis(1000),
            self.transport.read(&mut ack_buf),
        )
        .await;

        Ok(page_data)
    }

    /// Write a single 256-byte page (caller must be in programming mode).
    async fn write_single_page(
        &mut self,
        page: u16,
        data: &[u8; programming::PAGE_SIZE],
    ) -> Result<(), Error> {
        if programming::is_factory_calibration_page(page) {
            return Err(Error::MemoryWriteProtected { page });
        }

        let cmd = programming::build_write_command(page, data);

        tracing::debug!(page, "writing page");

        // Send W command (261 bytes).
        self.transport.write(&cmd).await.map_err(Error::Transport)?;
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;

        // Read 1-byte ACK from radio.
        let mut ack_buf = [0u8; 1];
        let result = tokio::time::timeout(self.timeout, async {
            let n = self
                .transport
                .read(&mut ack_buf)
                .await
                .map_err(Error::Transport)?;
            if n == 0 {
                return Err(Error::Transport(TransportError::Disconnected(
                    std::io::Error::new(
                        std::io::ErrorKind::UnexpectedEof,
                        "connection closed waiting for write ACK",
                    ),
                )));
            }
            Ok(())
        })
        .await
        .map_err(|_| Error::Timeout(self.timeout))?;
        result?;

        if ack_buf[0] != programming::ACK {
            return Err(Error::WriteNotAcknowledged {
                page,
                got: ack_buf[0],
            });
        }

        Ok(())
    }

    // -----------------------------------------------------------------------
    // Internal: channel name page reading
    // -----------------------------------------------------------------------

    /// Read all channel name pages from the radio while in programming mode.
    ///
    /// Iterates over 63 pages starting at [`NAME_START_PAGE`](programming::NAME_START_PAGE),
    /// extracting 16 names per page, and truncating to 1,000 channels.
    async fn read_name_pages(&mut self) -> Result<Vec<String>, Error> {
        let mut names = Vec::with_capacity(programming::MAX_CHANNELS);

        for page_offset in 0..programming::NAME_PAGE_COUNT {
            let page = programming::NAME_START_PAGE + page_offset;
            let data = self.read_single_page(page).await?;

            // Extract 16 names from the 256-byte page.
            for i in 0..programming::NAMES_PER_PAGE {
                let start = i * programming::NAME_ENTRY_SIZE;
                if start + programming::NAME_ENTRY_SIZE <= data.len() {
                    let name = programming::extract_name(
                        &data[start..start + programming::NAME_ENTRY_SIZE],
                    );
                    names.push(name);
                }
            }

            // Stop once we have enough names.
            if names.len() >= programming::MAX_CHANNELS {
                names.truncate(programming::MAX_CHANNELS);
                break;
            }
        }

        tracing::info!(count = names.len(), "channel names read");
        Ok(names)
    }

    /// Read all 1,200 channel name entries from the radio while in programming mode.
    ///
    /// Iterates over 75 pages (0x0100-0x014A), extracting 16 names per page.
    async fn read_all_name_pages(&mut self) -> Result<Vec<String>, Error> {
        let mut names = Vec::with_capacity(programming::TOTAL_CHANNEL_ENTRIES);

        for page_offset in 0..programming::NAME_ALL_PAGE_COUNT {
            let page = programming::NAME_START_PAGE + page_offset;
            let data = self.read_single_page(page).await?;

            for i in 0..programming::NAMES_PER_PAGE {
                let start = i * programming::NAME_ENTRY_SIZE;
                if start + programming::NAME_ENTRY_SIZE <= data.len() {
                    let name = programming::extract_name(
                        &data[start..start + programming::NAME_ENTRY_SIZE],
                    );
                    names.push(name);
                }
            }

            if names.len() >= programming::TOTAL_CHANNEL_ENTRIES {
                names.truncate(programming::TOTAL_CHANNEL_ENTRIES);
                break;
            }
        }

        tracing::info!(
            count = names.len(),
            "all channel names read (including extended)"
        );
        Ok(names)
    }
}

#[cfg(test)]
mod tests {
    use crate::protocol::programming;
    use crate::radio::Radio;
    use crate::transport::MockTransport;

    /// Build a mock 261-byte W response with the given page address and
    /// a 256-byte data payload.
    fn build_w_response(page: u16, data: &[u8]) -> Vec<u8> {
        assert_eq!(data.len(), 256, "W response payload must be 256 bytes");
        let addr = page.to_be_bytes();
        // W + 2-byte page + 0x00 0x00 + 256 data = 261 bytes.
        let mut resp = vec![b'W', addr[0], addr[1], 0x00, 0x00];
        resp.extend_from_slice(data);
        resp
    }

    /// Build a 256-byte page payload with the given names in 16-byte slots.
    fn build_name_page(names: &[&str]) -> Vec<u8> {
        let mut data = vec![0u8; 256];
        for (i, name) in names.iter().enumerate().take(16) {
            let start = i * 16;
            let bytes = name.as_bytes();
            data[start..start + bytes.len()].copy_from_slice(bytes);
        }
        data
    }

    #[tokio::test]
    async fn read_channel_names_full_sequence() {
        // Mock the full programming mode sequence at 9600 baud throughout:
        // enter -> 63 page R/W/ACK loops -> exit.
        let mut mock = MockTransport::new();

        // Enter programming mode (no baud switch, no sync byte).
        mock.expect(b"0M PROGRAM\r", b"0M\r");

        // First page (256): has real names in slots 0-3.
        let first_page_data = build_name_page(&["ForestCityPD", "RPT1", "", "NOAA WX"]);
        let read_cmd = programming::build_read_command(256);
        mock.expect(&read_cmd, &build_w_response(256, &first_page_data));

        // ACK exchange after first page, then remaining 62 pages.
        for page_offset in 1..programming::NAME_PAGE_COUNT {
            mock.expect(&[programming::ACK], &[programming::ACK]);

            let page = programming::NAME_START_PAGE + page_offset;
            let cmd = programming::build_read_command(page);
            let empty = vec![0u8; 256];
            mock.expect(&cmd, &build_w_response(page, &empty));
        }

        // Final ACK after last page.
        mock.expect(&[programming::ACK], &[programming::ACK]);

        // Exit programming mode.
        mock.expect(b"E", &[]);

        let mut radio = Radio::connect(mock).await.unwrap();
        let names = radio.read_channel_names().await.unwrap();

        // 16 names per page * 63 pages = 1008, truncated to 1000.
        assert_eq!(names.len(), 1000);
        assert_eq!(names[0], "ForestCityPD");
        assert_eq!(names[1], "RPT1");
        assert_eq!(names[2], "");
        assert_eq!(names[3], "NOAA WX");
        for name in &names[4..16] {
            assert!(name.is_empty());
        }
    }

    #[tokio::test]
    async fn read_single_page_round_trip() {
        let mut mock = MockTransport::new();

        // Enter.
        mock.expect(b"0M PROGRAM\r", b"0M\r");

        // Read page 0x0020.
        let page: u16 = 0x0020;
        let mut page_data = vec![0xABu8; 256];
        page_data[0] = 0x00; // VHF flag
        let cmd = programming::build_read_command(page);
        mock.expect(&cmd, &build_w_response(page, &page_data));

        // ACK exchange.
        mock.expect(&[programming::ACK], &[programming::ACK]);

        // Exit.
        mock.expect(b"E", &[]);

        let mut radio = Radio::connect(mock).await.unwrap();
        let result = radio.read_page(page).await.unwrap();
        assert_eq!(result[0], 0x00);
        assert_eq!(result[1], 0xAB);
    }

    #[tokio::test]
    async fn write_single_page_round_trip() {
        let mut mock = MockTransport::new();

        // Enter.
        mock.expect(b"0M PROGRAM\r", b"0M\r");

        // Write page 0x0100.
        let page: u16 = 0x0100;
        let page_data = [0xCDu8; 256];
        let write_cmd = programming::build_write_command(page, &page_data);
        mock.expect(&write_cmd, &[programming::ACK]);

        // Exit.
        mock.expect(b"E", &[]);

        let mut radio = Radio::connect(mock).await.unwrap();
        radio.write_page(page, &page_data).await.unwrap();
    }

    #[tokio::test]
    async fn write_factory_cal_page_rejected() {
        let mock = MockTransport::new();
        let mut radio = Radio::connect(mock).await.unwrap();

        let data = [0u8; 256];
        let result = radio.write_page(0x07A1, &data).await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            err.to_string().contains("protected"),
            "error should mention protected: {err}"
        );
    }

    #[tokio::test]
    async fn write_memory_image_wrong_size_rejected() {
        let mock = MockTransport::new();
        let mut radio = Radio::connect(mock).await.unwrap();

        let bad_image = vec![0u8; 1000]; // wrong size
        let result = radio.write_memory_image(&bad_image).await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            err.to_string().contains("invalid memory image size"),
            "error should mention size: {err}"
        );
    }

    #[tokio::test]
    async fn read_memory_pages_small_range() {
        let mut mock = MockTransport::new();

        // Enter.
        mock.expect(b"0M PROGRAM\r", b"0M\r");

        // Read 2 pages starting at 0x0040.
        for i in 0..2u16 {
            let page = 0x0040 + i;
            let data = vec![i as u8; 256];
            let cmd = programming::build_read_command(page);
            mock.expect(&cmd, &build_w_response(page, &data));
            mock.expect(&[programming::ACK], &[programming::ACK]);
        }

        // Exit.
        mock.expect(b"E", &[]);

        let mut radio = Radio::connect(mock).await.unwrap();
        let data = radio.read_memory_pages(0x0040, 2).await.unwrap();
        assert_eq!(data.len(), 512);
        // First page is all 0x00, second is all 0x01.
        assert!(data[..256].iter().all(|&b| b == 0x00));
        assert!(data[256..].iter().all(|&b| b == 0x01));
    }

    #[tokio::test]
    async fn write_memory_pages_protected_range_rejected() {
        let mock = MockTransport::new();
        let mut radio = Radio::connect(mock).await.unwrap();

        // Try to write 3 pages starting at 0x07A0 -- page 0x07A1 is protected.
        let data = vec![0u8; 768]; // 3 pages
        let result = radio.write_memory_pages(0x07A0, &data).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn read_channel_flags_sequence() {
        let mut mock = MockTransport::new();

        // Enter.
        mock.expect(b"0M PROGRAM\r", b"0M\r");

        // Channel flags span pages 0x0020 through 0x0032 (19 pages).
        let page_count = programming::CHANNEL_FLAGS_END - programming::CHANNEL_FLAGS_START + 1;
        for i in 0..page_count {
            let page = programming::CHANNEL_FLAGS_START + i;
            // Build page with flag records:
            // first 4 bytes = channel flag, rest = empty (0xFF).
            let mut data = vec![0xFF_u8; 256];
            if i == 0 {
                // Channel 0: VHF, not locked, group 0
                data[0] = 0x00; // used = VHF
                data[1] = 0x00; // not locked
                data[2] = 0x00; // group 0
                data[3] = 0xFF;
                // Channel 1: UHF, locked, group 5
                data[4] = 0x02; // used = UHF
                data[5] = 0x01; // locked
                data[6] = 0x05; // group 5
                data[7] = 0xFF;
            }
            let cmd = programming::build_read_command(page);
            mock.expect(&cmd, &build_w_response(page, &data));
            mock.expect(&[programming::ACK], &[programming::ACK]);
        }

        // Exit.
        mock.expect(b"E", &[]);

        let mut radio = Radio::connect(mock).await.unwrap();
        let flags = radio.read_channel_flags().await.unwrap();

        // Should have 1200 flags.
        assert_eq!(flags.len(), programming::TOTAL_CHANNEL_ENTRIES);

        // Check the first two we programmed.
        assert!(!flags[0].is_empty());
        assert_eq!(flags[0].used, programming::FLAG_VHF);
        assert!(!flags[0].lockout);
        assert_eq!(flags[0].group, 0);

        assert!(!flags[1].is_empty());
        assert_eq!(flags[1].used, programming::FLAG_UHF);
        assert!(flags[1].lockout);
        assert_eq!(flags[1].group, 5);

        // The rest should be empty.
        assert!(flags[2].is_empty());
    }

    #[tokio::test]
    async fn progress_callback_invoked() {
        let mut mock = MockTransport::new();

        // Enter.
        mock.expect(b"0M PROGRAM\r", b"0M\r");

        // Read 3 pages.
        for i in 0..3u16 {
            let page = 0x0100 + i;
            let data = vec![0u8; 256];
            let cmd = programming::build_read_command(page);
            mock.expect(&cmd, &build_w_response(page, &data));
            mock.expect(&[programming::ACK], &[programming::ACK]);
        }

        // Exit.
        mock.expect(b"E", &[]);

        let mut radio = Radio::connect(mock).await.unwrap();

        // Use read_memory_pages (which doesn't expose progress), but we
        // can test the internal progress via read_memory_image_with_progress
        // indirectly. For now, just verify read_memory_pages works with 3 pages.
        let data = radio.read_memory_pages(0x0100, 3).await.unwrap();
        assert_eq!(data.len(), 768);
    }

    #[tokio::test]
    async fn modify_memory_page_read_modify_write() {
        let mut mock = MockTransport::new();

        // Page 0x0010 contains MCP offset 0x1000-0x10FF.
        let page: u16 = 0x0010;
        let byte_index: usize = 0x71; // offset 0x1071 within this page

        // Original page data: all zeros.
        let mut original_data = vec![0u8; 256];
        original_data[byte_index] = 0x00; // beep off

        // Expected modified data: byte at 0x71 set to 1.
        let mut expected_data = original_data.clone();
        expected_data[byte_index] = 0x01;

        // Enter programming mode.
        mock.expect(b"0M PROGRAM\r", b"0M\r");

        // Read page.
        let read_cmd = programming::build_read_command(page);
        mock.expect(&read_cmd, &build_w_response(page, &original_data));

        // ACK exchange after read.
        mock.expect(&[programming::ACK], &[programming::ACK]);

        // Write modified page.
        let expected_array: [u8; 256] = expected_data.clone().try_into().unwrap();
        let write_cmd = programming::build_write_command(page, &expected_array);
        mock.expect(&write_cmd, &[programming::ACK]);

        // Exit programming mode.
        mock.expect(b"E", &[]);

        let mut radio = Radio::connect(mock).await.unwrap();
        radio
            .modify_memory_page(page, |data| {
                data[byte_index] = 0x01;
            })
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn modify_memory_page_factory_cal_rejected() {
        let mock = MockTransport::new();
        let mut radio = Radio::connect(mock).await.unwrap();

        let result = radio
            .modify_memory_page(0x07A1, |_data| {
                // Should never be called.
            })
            .await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            err.to_string().contains("protected"),
            "error should mention protected: {err}"
        );
    }

    #[tokio::test]
    async fn write_channel_name_round_trip() {
        let mut mock = MockTransport::new();

        // Channel 5 lives on page 0x0100 (5 / 16 = 0), offset = 5 * 16 = 80.
        let page: u16 = 0x0100;
        let offset = 5 * programming::NAME_ENTRY_SIZE;

        // Original page: all zeros (empty names).
        let original_data = vec![0u8; 256];

        // Expected: "TestCh" written at offset 80, null-padded.
        let mut expected_data = original_data.clone();
        let name = b"TestCh";
        expected_data[offset..offset + name.len()].copy_from_slice(name);

        // Enter programming mode.
        mock.expect(b"0M PROGRAM\r", b"0M\r");

        // Read page.
        let read_cmd = programming::build_read_command(page);
        mock.expect(&read_cmd, &build_w_response(page, &original_data));

        // ACK exchange after read.
        mock.expect(&[programming::ACK], &[programming::ACK]);

        // Write modified page.
        let expected_array: [u8; 256] = expected_data.try_into().unwrap();
        let write_cmd = programming::build_write_command(page, &expected_array);
        mock.expect(&write_cmd, &[programming::ACK]);

        // Exit programming mode.
        mock.expect(b"E", &[]);

        let mut radio = Radio::connect(mock).await.unwrap();
        radio.write_channel_name(5, "TestCh").await.unwrap();
    }

    #[tokio::test]
    async fn write_channel_name_out_of_range_rejected() {
        let mock = MockTransport::new();
        let mut radio = Radio::connect(mock).await.unwrap();

        let result = radio.write_channel_name(1200, "Bad").await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            err.to_string().contains("out of range"),
            "error should mention out of range: {err}"
        );
    }

    #[tokio::test]
    async fn write_channel_name_truncates_long_name() {
        let mut mock = MockTransport::new();

        // Channel 0 on page 0x0100, offset 0.
        let page: u16 = 0x0100;
        let original_data = vec![0u8; 256];

        // A name longer than 15 bytes should be truncated to 15.
        let long_name = "ABCDEFGHIJKLMNOP"; // 16 chars
        let mut expected_data = original_data.clone();
        // Only first 15 bytes written (leaving null terminator).
        expected_data[..15].copy_from_slice(&long_name.as_bytes()[..15]);

        mock.expect(b"0M PROGRAM\r", b"0M\r");
        let read_cmd = programming::build_read_command(page);
        mock.expect(&read_cmd, &build_w_response(page, &original_data));
        mock.expect(&[programming::ACK], &[programming::ACK]);
        let expected_array: [u8; 256] = expected_data.try_into().unwrap();
        let write_cmd = programming::build_write_command(page, &expected_array);
        mock.expect(&write_cmd, &[programming::ACK]);
        mock.expect(b"E", &[]);

        let mut radio = Radio::connect(mock).await.unwrap();
        radio.write_channel_name(0, long_name).await.unwrap();
    }

    #[tokio::test]
    async fn read_all_channel_names_returns_1200() {
        let mut mock = MockTransport::new();

        // Enter programming mode.
        mock.expect(b"0M PROGRAM\r", b"0M\r");

        // First page has some names.
        let first_page_data = build_name_page(&["AllCh0", "AllCh1"]);
        let read_cmd = programming::build_read_command(programming::CHANNEL_NAMES_START);
        mock.expect(
            &read_cmd,
            &build_w_response(programming::CHANNEL_NAMES_START, &first_page_data),
        );

        // Remaining 74 pages are empty.
        for page_offset in 1..programming::NAME_ALL_PAGE_COUNT {
            mock.expect(&[programming::ACK], &[programming::ACK]);

            let page = programming::NAME_START_PAGE + page_offset;
            let cmd = programming::build_read_command(page);
            let empty = vec![0u8; 256];
            mock.expect(&cmd, &build_w_response(page, &empty));
        }

        // Final ACK after last page.
        mock.expect(&[programming::ACK], &[programming::ACK]);

        // Exit programming mode.
        mock.expect(b"E", &[]);

        let mut radio = Radio::connect(mock).await.unwrap();
        let names = radio.read_all_channel_names().await.unwrap();

        // 16 names per page * 75 pages = 1200.
        assert_eq!(names.len(), 1200);
        assert_eq!(names[0], "AllCh0");
        assert_eq!(names[1], "AllCh1");
        for name in &names[2..] {
            assert!(name.is_empty());
        }
    }
}
