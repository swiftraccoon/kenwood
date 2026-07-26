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
//! The radio's USB stack resets when exiting MCP mode. Normal high-level
//! operations wait for re-enumeration, reopen the selected device, and prove
//! CAT identity before returning. Methods explicitly documented as detached
//! intentionally skip that reconnect because the caller owns recovery.
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

use super::{McpPhase, Radio};

/// Baud rate for the programming mode handshake.
///
/// The `0M PROGRAM\r` entry command is always sent at 9600 baud.
/// The data transfer phase may stay at 9600 or switch to 115200
/// depending on the configured [`McpSpeed`].
const PROGRAMMING_BAUD: u32 = 9600;

/// Baud rate for fast MCP transfers.
const FAST_TRANSFER_BAUD: u32 = 115_200;

/// Additional settle time before reconnecting after a programming-mode
/// exit. The radio drops and re-enumerates USB when it leaves MCP mode;
/// together with the 2-second mode-switch wait in
/// `exit_programming_mode`, this totals the ~5 seconds the hardware
/// needs before the port answers again.
const MCP_EXIT_SETTLE: std::time::Duration = std::time::Duration::from_secs(3);

/// Maximum time to wait for the radio's one-byte acknowledgement of the
/// raw MCP exit command.
///
/// Kenwood's official client uses a synchronous one-byte read with a
/// one-second timeout and accepts only ACK (`0x06`).
const MCP_EXIT_ACK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(1);

/// Maximum time for either half of the ACK exchange that completes an
/// MCP page read.
const MCP_PAGE_ACK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(1);

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
    /// Combine an MCP operation with the cleanup that followed it.
    ///
    /// When CAT restoration was not proved, cleanup takes safety precedence
    /// and both errors are retained. A cleanup anomaly after an independently
    /// successful CAT reconnect remains visible, but does not mislabel the
    /// radio as stranded in MCP mode.
    fn finish_mcp_operation<R>(
        &self,
        operation: Result<R, Error>,
        cleanup: Result<(), Error>,
    ) -> Result<R, Error> {
        let cleanup = cleanup.map_err(|cleanup_error| {
            if self.mcp_phase != McpPhase::Inactive
                && !Self::contains_unproved_cleanup(&cleanup_error)
            {
                Error::McpCleanupNotProved {
                    cleanup: Box::new(cleanup_error),
                }
            } else {
                cleanup_error
            }
        });

        match (operation, cleanup) {
            (Ok(value), Ok(())) => Ok(value),
            (Err(operation_error), Ok(())) => Err(operation_error),
            (Err(operation_error), Err(cleanup_error)) => {
                Err(Error::McpOperationAndCleanupFailed {
                    operation: Box::new(operation_error),
                    cleanup: Box::new(cleanup_error),
                })
            }
            (Ok(_), Err(cleanup_error)) => Err(cleanup_error),
        }
    }

    /// Whether an error already carries terminal guidance for an unproved
    /// CAT restoration, directly or on the cleanup side of a combined error.
    fn contains_unproved_cleanup(error: &Error) -> bool {
        match error {
            Error::McpCleanupNotProved { .. } => true,
            Error::McpOperationAndCleanupFailed { cleanup, .. } => {
                Self::contains_unproved_cleanup(cleanup)
            }
            _ => false,
        }
    }

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
        self.begin_full_image_operation()?;

        if let Err(error) = self.enter_programming_mode().await {
            self.restore_mcp_timeout();
            return Err(error);
        }

        let result = self
            .read_pages_raw(0, programming::TOTAL_PAGES, &mut on_progress)
            .await;

        let exit_result = self.exit_programming_mode().await;
        self.restore_mcp_timeout();

        self.finish_mcp_operation(result, exit_result)
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

        self.begin_full_image_operation()?;

        // Struct-held so an interrupted session can restore it (see
        // `recover_from_interrupted_mcp`).
        if let Err(error) = self.enter_programming_mode().await {
            self.restore_mcp_timeout();
            return Err(error);
        }

        // Write all pages except factory calibration (last 2).
        let writable_pages = programming::TOTAL_PAGES - programming::FACTORY_CAL_PAGES;
        let writable_bytes = writable_pages as usize * programming::PAGE_SIZE;
        // Length is validated at the top of this function (image.len() == TOTAL_SIZE),
        // and TOTAL_SIZE > writable_bytes, so `.get()` always yields `Some`, but we
        // propagate via `?` anyway to avoid any possibility of a panic.
        let writable_slice = image.get(..writable_bytes).ok_or(Error::InvalidImageSize {
            actual: image.len(),
            expected: writable_bytes,
        })?;
        let result = self
            .write_pages_raw(0, writable_slice, &mut on_progress)
            .await;

        let exit_result = self.exit_programming_mode().await;
        self.restore_mcp_timeout();

        self.finish_mcp_operation(result, exit_result)
    }

    /// Validate that no unresolved MCP session exists, then raise the
    /// timeout for a full-image transfer without losing an earlier saved
    /// value left by a cancelled pre-entry future.
    fn begin_full_image_operation(&mut self) -> Result<(), Error> {
        if self.mcp_phase != McpPhase::Inactive {
            return Err(Error::McpInterrupted);
        }
        if self.mcp_saved_timeout.is_none() {
            self.mcp_saved_timeout = Some(self.timeout);
        }
        self.timeout = FULL_DUMP_TIMEOUT;
        Ok(())
    }

    /// Restore the CAT timeout saved by a full-image operation.
    const fn restore_mcp_timeout(&mut self) {
        if let Some(saved) = self.mcp_saved_timeout.take() {
            self.timeout = saved;
        }
    }

    /// Validate a complete contiguous page range before any MCP I/O.
    ///
    /// A zero-length range is a no-op. For a non-empty range, both the
    /// first and last page must lie inside the physical image, and the
    /// last-page calculation must not overflow `u16`.
    fn validate_mcp_page_range(start_page: u16, count: u16) -> Result<(), Error> {
        if count == 0 {
            return Ok(());
        }
        if start_page >= programming::TOTAL_PAGES {
            return Err(Error::McpPageOutOfRange {
                page: start_page,
                total_pages: programming::TOTAL_PAGES,
            });
        }

        let last_page = count
            .checked_sub(1)
            .and_then(|offset| start_page.checked_add(offset));
        if !matches!(last_page, Some(page) if page < programming::TOTAL_PAGES) {
            return Err(Error::McpPageOutOfRange {
                // `start_page` is valid, so this is the first page beyond
                // the physical image regardless of how far the request runs.
                page: programming::TOTAL_PAGES,
                total_pages: programming::TOTAL_PAGES,
            });
        }
        Ok(())
    }

    // -----------------------------------------------------------------------
    // High-level: page range read/write
    // -----------------------------------------------------------------------

    /// Read a range of pages from radio memory.
    ///
    /// Enters programming mode, reads `count` pages starting at
    /// `start_page`, and exits. Returns the raw bytes. A zero-page request
    /// is a no-op and does not enter programming mode.
    ///
    /// # Errors
    ///
    /// Returns [`Error::McpPageOutOfRange`] before any I/O if the complete
    /// requested range is not inside the radio's memory image. Returns an
    /// error if entry, any page read, or exit fails.
    /// Programming mode is always exited, even on error.
    pub async fn read_memory_pages(
        &mut self,
        start_page: u16,
        count: u16,
    ) -> Result<Vec<u8>, Error> {
        Self::validate_mcp_page_range(start_page, count)?;
        if count == 0 {
            return Ok(Vec::new());
        }

        self.enter_programming_mode().await?;

        let result = self.read_pages_raw(start_page, count, &mut |_, _| {}).await;

        let exit_result = self.exit_programming_mode().await;

        self.finish_mcp_operation(result, exit_result)
    }

    /// Read a sparse set of memory pages in one programming session.
    ///
    /// `pages` may be unordered and contain duplicates. Each distinct page is
    /// read exactly once, in ascending page-number order. The returned vector
    /// contains `(page_number, page_data)` pairs in that same order.
    ///
    /// An empty page list is a no-op and does not enter programming mode.
    ///
    /// # Errors
    ///
    /// Returns [`Error::McpPageOutOfRange`] before any I/O if any requested
    /// page is outside the radio's memory image. Returns an error if entry,
    /// any page read, or exit fails. Programming mode is always exited after a
    /// successful entry, even when a page read fails.
    pub async fn read_sparse_memory_pages(
        &mut self,
        pages: &[u16],
    ) -> Result<Vec<(u16, [u8; programming::PAGE_SIZE])>, Error> {
        self.read_sparse_memory_pages_with_progress(pages, |_, _| {})
            .await
    }

    /// Read a sparse set of memory pages with a progress callback.
    ///
    /// The callback receives `(pages_read, total_unique_pages)` after each
    /// successful page read. Pages are validated before any I/O, then sorted
    /// and deduplicated before the single programming session begins.
    ///
    /// An empty page list is a no-op and does not invoke the callback.
    ///
    /// # Errors
    ///
    /// Returns [`Error::McpPageOutOfRange`] before any I/O if any requested
    /// page is outside the radio's memory image. Returns an error if entry,
    /// any page read, or exit fails. Programming mode is always exited after a
    /// successful entry, even when a page read fails.
    pub async fn read_sparse_memory_pages_with_progress<F>(
        &mut self,
        pages: &[u16],
        mut on_progress: F,
    ) -> Result<Vec<(u16, [u8; programming::PAGE_SIZE])>, Error>
    where
        F: FnMut(u16, u16),
    {
        // Validate the complete request before entering MCP mode. Read-only
        // access to the final factory-calibration pages is permitted; only
        // page numbers beyond the physical image are invalid.
        for &page in pages {
            if page >= programming::TOTAL_PAGES {
                return Err(Error::McpPageOutOfRange {
                    page,
                    total_pages: programming::TOTAL_PAGES,
                });
            }
        }

        let mut pages = pages.to_vec();
        pages.sort_unstable();
        pages.dedup();

        if pages.is_empty() {
            return Ok(Vec::new());
        }

        // Every distinct page is less than TOTAL_PAGES, so the number of
        // distinct pages is also at most TOTAL_PAGES and fits in u16.
        #[expect(
            clippy::cast_possible_truncation,
            reason = "The validated and deduplicated page list can contain at most TOTAL_PAGES \
                      (1955) entries, which fits in u16."
        )]
        let total = pages.len() as u16;

        self.enter_programming_mode().await?;

        let result: Result<Vec<(u16, [u8; programming::PAGE_SIZE])>, Error> = async {
            let mut page_data = Vec::with_capacity(pages.len());
            for (completed, page) in (1u16..=total).zip(pages) {
                let data = self.read_single_page(page).await?;
                page_data.push((page, data));
                on_progress(completed, total);
            }
            Ok(page_data)
        }
        .await;

        // Always exit programming mode after a successful entry, including a
        // page-read failure.
        let exit_result = self.exit_programming_mode().await;

        self.finish_mcp_operation(result, exit_result)
    }

    /// Write a range of pages to radio memory.
    ///
    /// Enters programming mode, writes pages starting at `start_page`
    /// with the provided data, and exits. The data length must be a
    /// multiple of 256. Empty data is a no-op and does not enter
    /// programming mode.
    ///
    /// # Errors
    ///
    /// Returns [`Error::MemoryWriteProtected`] if any target page falls
    /// within the factory calibration region.
    /// Returns [`Error::InvalidImageSize`] before any I/O if `data` is not
    /// page-aligned, and [`Error::McpPageOutOfRange`] if the complete target
    /// range is not inside the radio's memory image.
    /// Returns an error if entry, any page write, or exit fails.
    /// Programming mode is always exited, even on error.
    pub async fn write_memory_pages(&mut self, start_page: u16, data: &[u8]) -> Result<(), Error> {
        if data.is_empty() {
            return Ok(());
        }
        if !data.len().is_multiple_of(programming::PAGE_SIZE) {
            return Err(Error::InvalidImageSize {
                actual: data.len(),
                expected: data.len().next_multiple_of(programming::PAGE_SIZE),
            });
        }
        let page_count = u16::try_from(data.len() / programming::PAGE_SIZE).map_err(|_| {
            Error::InvalidImageSize {
                actual: data.len(),
                expected: programming::PAGE_SIZE * usize::from(u16::MAX),
            }
        })?;
        Self::validate_mcp_page_range(start_page, page_count)?;

        // The complete range is now known to be in bounds, so checked
        // arithmetic above guarantees this last-page calculation.
        let last_page = start_page + (page_count - 1);
        if last_page > programming::MAX_WRITABLE_PAGE {
            let first_protected = start_page.max(programming::MAX_WRITABLE_PAGE + 1);
            return Err(Error::MemoryWriteProtected {
                page: first_protected,
            });
        }

        self.enter_programming_mode().await?;

        let result = self.write_pages_raw(start_page, data, &mut |_, _| {}).await;

        let exit_result = self.exit_programming_mode().await;

        self.finish_mcp_operation(result, exit_result)
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
    /// Returns [`Error::McpPageOutOfRange`] before any I/O if `page` is
    /// outside the radio's memory image. Returns an error if entry, the page
    /// read, or exit fails.
    /// Programming mode is always exited, even on error.
    pub async fn read_page(&mut self, page: u16) -> Result<[u8; programming::PAGE_SIZE], Error> {
        Self::validate_mcp_page_range(page, 1)?;
        self.enter_programming_mode().await?;

        let result = self.read_single_page(page).await;

        let exit_result = self.exit_programming_mode().await;

        self.finish_mcp_operation(result, exit_result)
    }

    /// Write a single memory page (256 bytes).
    ///
    /// Enters programming mode, writes the page, and exits.
    ///
    /// # Errors
    ///
    /// Returns [`Error::MemoryWriteProtected`] if the page is in the
    /// factory calibration region.
    /// Returns [`Error::McpPageOutOfRange`] if `page` is outside the radio's
    /// memory image.
    /// Returns an error if entry, the page write, or exit fails.
    /// Programming mode is always exited, even on error.
    pub async fn write_page(
        &mut self,
        page: u16,
        data: &[u8; programming::PAGE_SIZE],
    ) -> Result<(), Error> {
        Self::validate_mcp_page_range(page, 1)?;
        if programming::is_factory_calibration_page(page) {
            return Err(Error::MemoryWriteProtected { page });
        }

        self.enter_programming_mode().await?;

        let result = self.write_single_page(page, data).await;

        let exit_result = self.exit_programming_mode().await;

        self.finish_mcp_operation(result, exit_result)
    }

    /// Write a single memory page without read-back verification.
    ///
    /// Identical to [`write_page`](Self::write_page) but skips the
    /// verify step, halving the wire traffic. Prefer this only for
    /// bulk flows that verify separately at the end; the verified
    /// default catches flash writes the radio acknowledged but did
    /// not land.
    ///
    /// # Errors
    ///
    /// Returns [`Error::MemoryWriteProtected`] if the page is in the
    /// factory calibration region.
    /// Returns [`Error::McpPageOutOfRange`] if `page` is outside the radio's
    /// memory image.
    /// Returns an error if entry, the page write, or exit fails.
    /// Programming mode is always exited, even on error.
    pub async fn write_page_unverified(
        &mut self,
        page: u16,
        data: &[u8; programming::PAGE_SIZE],
    ) -> Result<(), Error> {
        Self::validate_mcp_page_range(page, 1)?;
        if programming::is_factory_calibration_page(page) {
            return Err(Error::MemoryWriteProtected { page });
        }

        self.enter_programming_mode().await?;

        let result = self.write_single_page_unverified(page, data).await;

        let exit_result = self.exit_programming_mode().await;

        self.finish_mcp_operation(result, exit_result)
    }

    // -----------------------------------------------------------------------
    // High-level: read-modify-write
    // -----------------------------------------------------------------------

    /// Read, modify, and selectively write a sparse set of memory pages in
    /// one MCP programming session.
    ///
    /// `pages` may be unordered and may contain duplicates. Each distinct
    /// page is read exactly once before `modify` is called for any page. The
    /// callback then receives each page in ascending order and can apply byte,
    /// string, or masked bit-field patches in place. Only pages whose contents
    /// actually changed are written, and every write is verified by read-back.
    /// The returned page numbers are the pages that were written.
    ///
    /// An empty page list is a no-op: programming mode is not entered and the
    /// callback is not called.
    ///
    /// # Connection lifetime
    ///
    /// The connection drops during the programming-mode transition; the exit
    /// path waits out the radio's reset and reconnects, so this method returns
    /// with the radio answering CAT commands again.
    ///
    /// # Errors
    ///
    /// Returns [`Error::MemoryWriteProtected`] before any I/O if any requested
    /// page is in the factory calibration region. Returns an error if entry,
    /// any page read, a changed-page write or verification, or exit fails.
    /// Programming mode is always exited after a successful entry, even when
    /// a read, write, or verification fails. A failed read cannot change the
    /// radio, but if a write or its verification fails partway through the
    /// batch, pages written earlier in the same session remain changed; the
    /// error identifies only the failing page.
    pub async fn modify_memory_pages<F>(
        &mut self,
        pages: &[u16],
        mut modify: F,
    ) -> Result<Vec<u16>, Error>
    where
        F: FnMut(u16, &mut [u8; programming::PAGE_SIZE]),
    {
        let pages: std::collections::BTreeSet<u16> = pages.iter().copied().collect();

        // Validate the complete request before entering MCP mode. This also
        // prevents a mixed settings/calibration request from partially
        // applying its safe pages before the protected page is discovered.
        for &page in &pages {
            if programming::is_factory_calibration_page(page) {
                return Err(Error::MemoryWriteProtected { page });
            }
        }

        if pages.is_empty() {
            return Ok(Vec::new());
        }

        self.enter_programming_mode().await?;

        let result: Result<Vec<u16>, Error> = async {
            // Read every requested page before running the patch callback or
            // writing anything. A failed read therefore cannot leave a
            // partially patched set of pages on the radio.
            let mut page_data = Vec::with_capacity(pages.len());
            for page in pages {
                let original = self.read_single_page(page).await?;
                page_data.push((page, original, original));
            }

            for (page, _, modified) in &mut page_data {
                modify(*page, modified);
            }

            let mut changed_pages = Vec::new();
            for (page, original, modified) in &page_data {
                if original != modified {
                    self.write_single_page(*page, modified).await?;
                    changed_pages.push(*page);
                }
            }

            Ok(changed_pages)
        }
        .await;

        // Always exit programming mode after a successful entry, including
        // read and verified-write failure paths.
        let exit_result = self.exit_programming_mode().await;

        self.finish_mcp_operation(result, exit_result)
    }

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
    /// The connection drops during the programming-mode transition; the
    /// exit path waits out the radio's reset and reconnects, so this
    /// method returns with the radio answering CAT commands again.
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

        self.finish_mcp_operation(result, exit_result)
    }

    /// Read-modify-write one memory page, then exit programming mode
    /// WITHOUT reconnecting.
    ///
    /// For writes whose purpose is to reboot the radio out of CAT mode
    /// (e.g. enabling DV Gateway / Reflector Terminal Mode, where the
    /// radio comes back speaking the MMDVM binary protocol): the normal
    /// post-exit reconnect would race that reboot. Over Bluetooth the
    /// link can reopen in the pre-reboot window and then wedge
    /// mid-command as the radio's stack dies. The write is still
    /// verified by read-back inside the session; the connection is
    /// deliberately left dead afterwards and the caller owns recovery
    /// (typically by reconnecting from a fresh process once the radio
    /// finishes rebooting).
    ///
    /// # Errors
    ///
    /// Returns [`Error::MemoryWriteProtected`] if the page is in the
    /// factory calibration region.
    /// Returns an error if entry, the page read, the page write, or the
    /// exit acknowledgement fails. Exit is always attempted after a
    /// successful entry. A successful page operation uses the detached exit
    /// expected by the caller. Any page-operation error instead uses the
    /// normal reconnect-and-identify exit path, because stale page bytes or
    /// ACKs must not be mistaken for proof of a detached exit. An unproved
    /// exit leaves CAT poisoned for explicit recovery.
    pub async fn modify_memory_page_detached<F>(
        &mut self,
        page: u16,
        modify: F,
    ) -> Result<(), Error>
    where
        F: FnOnce(&mut [u8; programming::PAGE_SIZE]),
    {
        if programming::is_factory_calibration_page(page) {
            return Err(Error::MemoryWriteProtected { page });
        }

        self.enter_programming_mode().await?;

        let result: Result<(), Error> = async {
            let mut page_data = self.read_single_page(page).await?;
            modify(&mut page_data);
            self.write_single_page(page, &page_data).await?;
            Ok(())
        }
        .await;

        let exit_result = if result.is_ok() {
            // The entire read/write/readback exchange is aligned and the
            // caller expects this setting to reboot into a non-CAT mode.
            self.exit_programming_mode_detached().await
        } else {
            // A partial W frame or delayed page/write ACK could otherwise
            // be consumed as the detached E acknowledgement. Require an
            // independent reconnect and CAT identity proof after any
            // operation failure.
            self.exit_programming_mode().await
        };

        self.finish_mcp_operation(result, exit_result)
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

        self.finish_mcp_operation(result, exit_result)
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
    /// This enters MCP programming mode. Exit resets the USB connection; the
    /// method waits for re-enumeration, reopens it, and proves CAT identity
    /// before returning.
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

        self.finish_mcp_operation(result, exit_result)
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
    /// This enters MCP programming mode. Exit resets the USB connection; the
    /// method waits for re-enumeration, reopens it, and proves CAT identity
    /// before returning.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Validation`] if the channel number is 1200 or greater.
    /// Returns an error if entering programming mode, reading the page,
    /// writing the page, or exiting programming mode fails.
    pub async fn write_channel_name(&mut self, channel: u16, name: &str) -> Result<(), Error> {
        // TOTAL_CHANNEL_ENTRIES is 1200, which fits in u16.
        #[expect(
            clippy::cast_possible_truncation,
            reason = "TOTAL_CHANNEL_ENTRIES is a `const usize = 1200` per the D75 MCP memory \
                      layout (1200 channel flag entries at MCP offset 0x2000). 1200 fits \
                      in u16::MAX = 65535, so the compile-time const cast is lossless."
        )]
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
            // Clear the 16-byte slot and write the name (truncated to 15 bytes,
            // leaving null terminator). `offset..offset + NAME_ENTRY_SIZE` is
            // bounded by the page size the closure caller passes; if either slice
            // is out of range we silently drop the write. `modify_memory_page`
            // validates `data.len() == PAGE_SIZE` up-stream.
            let Some(slot) = data.get_mut(offset..offset + programming::NAME_ENTRY_SIZE) else {
                return;
            };
            slot.fill(0);
            let name_bytes = name.as_bytes();
            let len = name_bytes.len().min(programming::NAME_ENTRY_SIZE - 1);
            if let (Some(dst), Some(src)) = (slot.get_mut(..len), name_bytes.get(..len)) {
                dst.copy_from_slice(src);
            }
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

        let raw = self.finish_mcp_operation(result, exit_result)?;

        // Parse 4-byte flag records, 1200 entries. A record that fails
        // to parse must error rather than be skipped: skipping shifts
        // every subsequent index, silently associating flags with the
        // wrong channels.
        let mut flags = Vec::with_capacity(programming::TOTAL_CHANNEL_ENTRIES);
        for i in 0..programming::TOTAL_CHANNEL_ENTRIES {
            let offset = i * programming::FLAG_RECORD_SIZE;
            let flag = raw
                .get(offset..offset + programming::FLAG_RECORD_SIZE)
                .and_then(programming::parse_channel_flag)
                .ok_or_else(|| {
                    Error::Protocol(ProtocolError::FieldParse {
                        command: "MCP channel flags".to_owned(),
                        field: format!("flag record {i}"),
                        detail: "record missing or unparseable".to_owned(),
                    })
                })?;
            flags.push(flag);
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

        let raw = self.finish_mcp_operation(result, exit_result)?;

        // Parse memgroups: each 256-byte page is one memgroup containing
        // 6 channel records of 40 bytes + 16 bytes padding.
        let mut channels = Vec::with_capacity(programming::TOTAL_CHANNEL_ENTRIES);
        for memgroup_idx in 0..programming::MEMGROUP_COUNT {
            let group_offset = memgroup_idx * programming::PAGE_SIZE;
            for slot in 0..programming::CHANNELS_PER_MEMGROUP {
                let ch_offset = group_offset + slot * programming::CHANNEL_RECORD_SIZE;
                if let Some(record) =
                    raw.get(ch_offset..ch_offset + programming::CHANNEL_RECORD_SIZE)
                {
                    match FlashChannel::from_bytes(record) {
                        Ok(ch) => channels.push(ch),
                        // A corrupt record is a real fault in the dump;
                        // substituting a fabricated default would
                        // misrepresent radio state to the caller.
                        Err(e) => {
                            return Err(Error::Protocol(ProtocolError::FieldParse {
                                command: "MCP channel data".to_owned(),
                                field: format!(
                                    "channel {}",
                                    memgroup_idx * programming::CHANNELS_PER_MEMGROUP + slot
                                ),
                                detail: e.to_string(),
                            }));
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
    /// Switches to 9600 baud and sends the `0M PROGRAM` entry command.
    /// The command is carriage-return prefixed so a stale partial
    /// command in the radio's input buffer cannot corrupt the handshake.
    /// The radio responds with `0M\r` and enters MCP mode. The session
    /// stays at 9600 baud for all subsequent R/W/ACK exchanges.
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
        // An incomplete strict GM exchange may still have response bytes in
        // flight. Reject before draining input, changing baud, mutating MCP
        // state, or sending the programming-mode entry command.
        self.require_unpoisoned_gm_stream()?;

        // A previous cancelled session must be recovered before any
        // further serial traffic. In particular, starting a fresh entry
        // must not erase the fact that its exit byte may already have
        // been sent.
        if self.mcp_phase != McpPhase::Inactive {
            return Err(Error::McpInterrupted);
        }
        if let Some(exit_error) = self.mcp_pending_exit_error.take() {
            // A previous exit anomaly can remain after CAT was proved if
            // its caller cancelled during optional state restoration.
            // Surface it before associating it with a new MCP session.
            return Err(exit_error);
        }

        tracing::info!("entering programming mode at 9600 baud");

        // Queued AI pushes / NMEA sentences would land ahead of the
        // radio's `0M\r` acknowledgement and blow the small entry
        // window, so drain them first.
        self.drain_stale_input().await;

        // Switching the host baud is synchronous and cannot have put the
        // radio in MCP mode if it fails.
        self.transport
            .set_baud_rate(PROGRAMMING_BAUD)
            .map_err(Error::Transport)?;

        // Mark the session active BEFORE any wire traffic: if this
        // future is cancelled from here on, the radio may be in (or
        // entering) PROG MCP mode and CAT must refuse until recovery.
        self.mcp_phase = McpPhase::Active;

        let entry: Result<(), Error> = async {
            self.transport
                .write(programming::ENTER_PROGRAMMING)
                .await
                .map_err(Error::Transport)?;

            // 10ms delay after write.
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;

            // Read response -- expect "0M\r" (3 bytes).
            let mut buf = [0u8; 64];
            let mut received = Vec::new();

            match tokio::time::timeout(self.timeout, async {
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
                    if let Some(chunk) = buf.get(..n) {
                        received.extend_from_slice(chunk);
                    }
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
            {
                Ok(inner) => inner,
                Err(_elapsed) => Err(Error::Timeout(self.timeout)),
            }?;

            // If Fast mode is requested, switch to 115200 baud for the data
            // transfer phase.
            if self.mcp_speed == McpSpeed::Fast {
                self.enter_fast_programming_transfer().await?;
            } else {
                tracing::info!("programming mode entered, staying at {PROGRAMMING_BAUD} baud");
            }

            Ok(())
        }
        .await;

        match entry {
            Ok(()) => Ok(()),
            Err(entry_error) => {
                // Any error after the raw entry write began is ambiguous:
                // the radio may have accepted MCP even when the write or
                // acknowledgement failed. Preserve a failed cleanup proof
                // alongside the entry error instead of hiding it.
                let cleanup = self.exit_programming_mode().await;
                self.finish_mcp_operation::<()>(Err(entry_error), cleanup)
            }
        }
    }

    /// Switch an acknowledged MCP session to the optional fast transfer baud.
    async fn enter_fast_programming_transfer(&mut self) -> Result<(), Error> {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        self.transport
            .set_baud_rate(FAST_TRANSFER_BAUD)
            .map_err(Error::Transport)?;

        // The sync byte proves that the radio also switched baud rates.
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
                Ok(())
            }
            Ok(Ok(_)) => {
                tracing::error!("fast mode sync read returned 0 bytes; baud mismatch likely");
                Err(Error::Protocol(ProtocolError::MalformedFrame(
                    b"fast mode sync byte not received".to_vec(),
                )))
            }
            Ok(Err(error)) => {
                tracing::error!("fast mode sync read failed: {error}");
                Err(Error::Transport(error))
            }
            Err(_) => {
                tracing::error!("fast mode sync byte timed out; radio may not have switched baud");
                Err(Error::Timeout(std::time::Duration::from_secs(2)))
            }
        }
    }

    /// Exit programming mode (`E` command) and reconnect.
    ///
    /// Sends the exit byte, waits out the radio's reset, and brings the
    /// link back so the caller gets a radio that answers CAT commands.
    ///
    /// # Errors
    ///
    /// Returns an error if the exit byte cannot be written, the radio
    /// does not acknowledge it, or the post-exit reconnect fails. Even
    /// when the acknowledgement is missing or wrong, the reset settle
    /// and CAT reconnect proof are still attempted. A successful CAT
    /// proof clears the programming-session poison but does not hide the
    /// original acknowledgement error.
    async fn exit_programming_mode(&mut self) -> Result<(), Error> {
        if let Err(error) = self.send_programming_exit().await {
            // Store this before the next await. If reset settling or
            // reconnect is cancelled, recovery can still surface the
            // original exit anomaly.
            if self.mcp_pending_exit_error.is_none() {
                self.mcp_pending_exit_error = Some(error);
            }
        }
        let reconnect_result = self.settle_and_reconnect_after_programming_exit().await;
        let exit_result = self.take_pending_mcp_exit_result();
        self.finish_mcp_operation(exit_result, reconnect_result)
    }

    /// Exit programming mode (`E` command) WITHOUT reconnecting.
    ///
    /// For writes whose purpose is to reboot the radio out of CAT mode
    /// (e.g. enabling a gateway / terminal mode): reconnecting here
    /// would race the reboot; the link can come back up in the
    /// pre-reboot window and then die mid-command. The connection is
    /// deliberately left dead; the caller owns recovery.
    ///
    /// # Errors
    ///
    /// Returns an error if the exit byte cannot be written or the radio
    /// does not answer with exactly one ACK byte. An unconfirmed exit
    /// leaves the session poisoned; the caller must recover it. The exit
    /// byte is never resent.
    async fn exit_programming_mode_detached(&mut self) -> Result<(), Error> {
        tracing::info!("exiting programming mode");

        self.send_programming_exit().await?;
        self.settle_after_programming_exit().await;

        // Detached mode deliberately does not prove CAT because the
        // caller expects the link to disappear. The exact ACK is its
        // terminal proof that MCP accepted the one exit byte.
        self.mcp_phase = McpPhase::Inactive;
        Ok(())
    }

    /// Send exactly one raw MCP exit byte and require its one-byte ACK.
    ///
    /// The phase flag is set before awaiting the write, because a cancelled
    /// or failed write may still have delivered the byte. It is cleared only
    /// after CAT reconnect is proved (or a detached exit receives its ACK).
    /// Any write, read, EOF, timeout, or wrong-byte error leaves the session
    /// poisoned and `desynced` so normal CAT cannot proceed.
    async fn send_programming_exit(&mut self) -> Result<(), Error> {
        if self.mcp_phase == McpPhase::ExitSent {
            return Err(Error::McpExitAlreadySent);
        }

        // Set this before polling the transport write. From this point on,
        // recovery must conservatively assume that E reached the radio.
        self.mcp_phase = McpPhase::ExitSent;
        self.desynced = true;
        tokio::time::timeout(
            MCP_EXIT_ACK_TIMEOUT,
            self.transport.write(&[programming::EXIT]),
        )
        .await
        .map_err(|_| Error::Timeout(MCP_EXIT_ACK_TIMEOUT))?
        .map_err(Error::Transport)?;

        let mut ack = [0u8; 1];
        let read_result = tokio::time::timeout(MCP_EXIT_ACK_TIMEOUT, self.transport.read(&mut ack))
            .await
            .map_err(|_| Error::Timeout(MCP_EXIT_ACK_TIMEOUT))?;

        let count = read_result.map_err(Error::Transport)?;
        if count == 0 {
            return Err(Error::Transport(TransportError::Disconnected(
                std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "connection closed waiting for MCP exit ACK",
                ),
            )));
        }
        if ack[0] != programming::ACK {
            return Err(Error::McpExitNotAcknowledged { got: ack[0] });
        }

        Ok(())
    }

    /// Take a retained MCP exit anomaly, or report a successful exit.
    fn take_pending_mcp_exit_result(&mut self) -> Result<(), Error> {
        self.mcp_pending_exit_error.take().map_or(Ok(()), Err)
    }

    /// Wait out the radio's MCP reset and prove the reopened link speaks CAT.
    async fn settle_and_reconnect_after_programming_exit(&mut self) -> Result<(), Error> {
        // Even an unconfirmed exit may have reached the radio. Wait out
        // the reset and try to prove CAT operation instead of abandoning
        // cleanup solely because the ACK was lost.
        self.settle_after_programming_exit().await;

        // The radio resets its USB stack when leaving MCP mode.
        // Combined with the mode-switch wait above, this totals the
        // ~5 seconds the hardware needs before the port answers again.
        tokio::time::sleep(MCP_EXIT_SETTLE).await;

        // Bring the link back so every ordinary MCP operation returns a
        // radio that answers CAT commands.
        self.reconnect_after_mcp_exit().await
    }

    /// Wait for the accepted exit to take effect and restore the host baud.
    async fn settle_after_programming_exit(&mut self) {
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
    }

    /// Reconnect after an exit without permanently clearing the MCP poison
    /// until CAT identity has been proved.
    ///
    /// `Radio::reconnect` must issue CAT commands, so this guard
    /// temporarily permits them. If the future is cancelled or reconnect
    /// fails, `Drop` restores the poisoned state.
    async fn reconnect_after_mcp_exit(&mut self) -> Result<(), Error> {
        struct ReconnectGuard<'a, T: Transport> {
            radio: &'a mut Radio<T>,
            recovery_phase: McpPhase,
            cat_proved: bool,
            restore_finished: bool,
        }

        impl<T: Transport> Drop for ReconnectGuard<'_, T> {
            fn drop(&mut self) {
                if !self.cat_proved {
                    self.radio.mcp_phase = self.recovery_phase;
                    self.radio.desynced = true;
                } else if !self.restore_finished {
                    // CAT identity was proved, so MCP must stay clear. An
                    // optional restore command may nevertheless have been
                    // cancelled with a late response still in flight.
                    self.radio.desynced = true;
                }
            }
        }

        // The guard exclusively borrows the radio, so no other CAT
        // operation can observe this temporary permission. Cancellation
        // drops the guard and restores the poison.
        let recovery_phase = self.mcp_phase;
        self.mcp_phase = McpPhase::Inactive;
        let mut guard = ReconnectGuard {
            radio: self,
            recovery_phase,
            cat_proved: false,
            restore_finished: false,
        };
        guard.radio.reopen_and_identify().await?;

        // This assignment is synchronous immediately after `identify`
        // succeeds. Cancellation or failure while restoring optional
        // cached state must not re-poison an independently proved CAT link.
        guard.cat_proved = true;
        let result = guard.radio.restore_state_after_reconnect().await;
        guard.restore_finished = true;
        result
    }

    /// Recover after an MCP programming session's future was cancelled
    /// mid-transfer (e.g. by a caller-side `tokio::time::timeout`).
    ///
    /// Sends the MCP exit byte at most once, restores the saved CAT timeout,
    /// and reconnects to prove normal CAT operation. If an earlier future
    /// was cancelled after the exit phase began, recovery only settles and
    /// reconnects; it never retransmits `E`. CAT commands refuse with
    /// [`Error::McpInterrupted`] until CAT reconnect/identity is proved. A
    /// no-op if no session was interrupted and no retained exit anomaly
    /// remains to be reported.
    ///
    /// # Errors
    ///
    /// Returns the original exit-ACK anomaly even if reconnect/ID
    /// independently proves CAT recovery. If CAT recovery is not proved,
    /// the MCP poison remains set and the error instructs the caller to
    /// fully power-cycle the radio.
    pub async fn recover_from_interrupted_mcp(&mut self) -> Result<(), Error> {
        if self.mcp_phase == McpPhase::Inactive {
            // A full-image future can be cancelled while draining stale
            // input, before any MCP wire traffic makes the phase active.
            // Its struct-held timeout still needs explicit restoration.
            self.restore_mcp_timeout();
            // CAT may instead already have been proved after a failed MCP
            // exit, with cancellation landing during cached-state restore.
            // Preserve and surface that exit anomaly without re-poisoning.
            return self.take_pending_mcp_exit_result();
        }
        tracing::warn!("recovering from interrupted MCP session");
        let recovery_result = if self.mcp_phase == McpPhase::ExitSent {
            tracing::warn!(
                "MCP exit was already attempted; recovering without retransmitting the exit byte"
            );
            let reconnect_result = self.settle_and_reconnect_after_programming_exit().await;
            let exit_result = self.take_pending_mcp_exit_result();
            self.finish_mcp_operation(exit_result, reconnect_result)
        } else {
            self.exit_programming_mode().await
        };
        self.restore_mcp_timeout();
        recovery_result
    }

    // -----------------------------------------------------------------------
    // Internal: raw page I/O (caller must hold programming mode)
    // -----------------------------------------------------------------------

    /// Read a contiguous range of pages while already in programming mode.
    ///
    /// Returns a `Vec<u8>` containing `count * 256` bytes.
    ///
    /// A complete response for the wrong page is acknowledged and retried
    /// once. Partial, timed-out, or otherwise ambiguous responses are never
    /// retried because the radio's ACK state cannot then be proved.
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
            // `read_single_page` verifies the echoed page address and
            // retries once only after fully completing the ACK handshake
            // for a wrong-page response.
            let data = self.read_single_page(page).await?;
            image.extend_from_slice(&data);
            on_progress(i + 1, count);
        }

        Ok(image)
    }

    /// Write a contiguous range of pages while already in programming mode.
    ///
    /// `data.len()` must be a multiple of 256.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidImageSize`] if `data.len()` is not a multiple
    /// of [`programming::PAGE_SIZE`] or would exceed `u16::MAX` pages.
    async fn write_pages_raw<F>(
        &mut self,
        start_page: u16,
        data: &[u8],
        on_progress: &mut F,
    ) -> Result<(), Error>
    where
        F: FnMut(u16, u16),
    {
        // Validate up front: `data.len()` must be a whole number of pages and
        // fit in `u16::MAX` pages (the MCP address space limit).
        if !data.len().is_multiple_of(programming::PAGE_SIZE) {
            return Err(Error::InvalidImageSize {
                actual: data.len(),
                expected: data.len().next_multiple_of(programming::PAGE_SIZE),
            });
        }
        let page_count = data.len() / programming::PAGE_SIZE;
        let page_count_u16 = u16::try_from(page_count).map_err(|_| Error::InvalidImageSize {
            actual: data.len(),
            expected: programming::PAGE_SIZE * usize::from(u16::MAX),
        })?;

        // `chunks_exact` guarantees each chunk is exactly `PAGE_SIZE` bytes, so the
        // conversion to `&[u8; PAGE_SIZE]` is effectively infallible; `map_err`
        // converts the impossible error into an `InvalidImageSize` for type
        // purposes rather than using `.expect()`.
        for (i, chunk) in (0u16..page_count_u16).zip(data.chunks_exact(programming::PAGE_SIZE)) {
            let page = start_page + i;
            let page_data: &[u8; programming::PAGE_SIZE] =
                chunk.try_into().map_err(|_| Error::InvalidImageSize {
                    actual: chunk.len(),
                    expected: programming::PAGE_SIZE,
                })?;
            self.write_single_page(page, page_data).await?;
            on_progress(i + 1, page_count_u16);
        }

        Ok(())
    }

    /// Read a single 256-byte page (caller must be in programming mode).
    /// Read one page, verifying the radio's echoed page address, with at
    /// most one retry after a fully acknowledged address mismatch.
    ///
    /// A partial or timed-out response is ambiguous: the radio may still be
    /// sending the `W` frame or waiting for its host ACK. Retrying in that
    /// state could misframe the next command, so those errors go directly to
    /// programming-mode cleanup.
    async fn read_single_page(&mut self, page: u16) -> Result<[u8; programming::PAGE_SIZE], Error> {
        match self.read_single_page_attempt(page).await {
            Ok(data) => Ok(data),
            Err(e @ Error::McpPageMismatch { .. }) => {
                // `read_single_page_attempt` returns a mismatch only after
                // ACKing the complete W frame and consuming the radio's ACK,
                // so exactly one retry is safe.
                tracing::warn!(page, error = %e, "acknowledged wrong-page response; retrying once");
                self.read_single_page_attempt(page).await
            }
            Err(e) => Err(e),
        }
    }

    /// One un-retried page read exchange (R command → W response → ACK).
    async fn read_single_page_attempt(
        &mut self,
        page: u16,
    ) -> Result<[u8; programming::PAGE_SIZE], Error> {
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
                if let Some(chunk) = buf.get(..n) {
                    received.extend_from_slice(chunk);
                }
            }
            Ok(())
        })
        .await
        .map_err(|_| Error::Timeout(self.timeout))?;
        result?;

        // Parse: W(1) + addr(4) + data(256).
        let (answered_page, data) =
            programming::parse_write_response(&received).map_err(Error::Protocol)?;

        // Copy into a fixed-size array.
        let mut page_data = [0u8; programming::PAGE_SIZE];
        page_data.copy_from_slice(data);

        // Every fully parsed W frame must complete its ACK handshake before
        // any retry, next page, or exit command is safe.
        self.acknowledge_page_read(answered_page).await?;

        // The echoed address is the only integrity check the MCP protocol
        // offers. This error is deliberately emitted only after the complete
        // wrong-page frame has been acknowledged, making one retry safe.
        if answered_page != page {
            return Err(Error::McpPageMismatch {
                requested: page,
                answered: answered_page,
            });
        }

        Ok(page_data)
    }

    /// Complete the host-ACK/radio-ACK handshake for one parsed `W` frame.
    async fn acknowledge_page_read(&mut self, answered_page: u16) -> Result<(), Error> {
        tokio::time::timeout(
            MCP_PAGE_ACK_TIMEOUT,
            self.transport.write(&[programming::ACK]),
        )
        .await
        .map_err(|_| Error::Timeout(MCP_PAGE_ACK_TIMEOUT))?
        .map_err(Error::Transport)?;
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;

        let mut ack_buf = [0u8; 1];
        let count = tokio::time::timeout(MCP_PAGE_ACK_TIMEOUT, self.transport.read(&mut ack_buf))
            .await
            .map_err(|_| Error::Timeout(MCP_PAGE_ACK_TIMEOUT))?
            .map_err(Error::Transport)?;
        if count == 0 {
            return Err(Error::Transport(TransportError::Disconnected(
                std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "connection closed waiting for MCP page-read ACK",
                ),
            )));
        }
        if ack_buf[0] != programming::ACK {
            return Err(Error::McpPageReadNotAcknowledged {
                page: answered_page,
                got: ack_buf[0],
            });
        }

        Ok(())
    }

    /// Write a single 256-byte page (caller must be in programming mode).
    /// Write one page and verify it by read-back.
    ///
    /// The radio's ACK only confirms receipt of the frame, not that
    /// the bytes landed in flash. Reading the page back catches a
    /// failed write before any cached image is trusted.
    async fn write_single_page(
        &mut self,
        page: u16,
        data: &[u8; programming::PAGE_SIZE],
    ) -> Result<(), Error> {
        self.write_single_page_unverified(page, data).await?;
        let readback = self.read_single_page(page).await?;
        if let Some((offset, (&expected, &actual))) = data
            .iter()
            .zip(readback.iter())
            .enumerate()
            .find(|(_, (e, a))| e != a)
        {
            return Err(Error::McpVerifyMismatch {
                page,
                offset,
                expected,
                actual,
            });
        }
        Ok(())
    }

    async fn write_single_page_unverified(
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
                if let Some(slot) = data.get(start..start + programming::NAME_ENTRY_SIZE) {
                    names.push(programming::extract_name(slot));
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
                if let Some(slot) = data.get(start..start + programming::NAME_ENTRY_SIZE) {
                    names.push(programming::extract_name(slot));
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
    use crate::error::{Error, ProtocolError, TransportError};
    use crate::protocol::programming;
    use crate::protocol::{Command, Response};
    use crate::radio::{McpPhase, Radio};
    use crate::transport::{MockTransport, Transport};
    use crate::types::Band;

    type TestResult = Result<(), Box<dyn std::error::Error>>;
    type BoxErr = Box<dyn std::error::Error>;

    #[derive(Debug)]
    struct HangingWriteTransport;

    impl Transport for HangingWriteTransport {
        async fn write(&mut self, _data: &[u8]) -> Result<(), TransportError> {
            std::future::pending().await
        }

        async fn read(&mut self, _buf: &mut [u8]) -> Result<usize, TransportError> {
            std::future::pending().await
        }

        async fn close(&mut self) -> Result<(), TransportError> {
            Ok(())
        }
    }

    /// Set a single byte at `offset` in a mutable slice, returning an error if out of range.
    fn set_byte(image: &mut [u8], offset: usize, value: u8) -> Result<(), BoxErr> {
        let img_len = image.len();
        *image
            .get_mut(offset)
            .ok_or_else(|| format!("set_byte: offset {offset} out of range (len={img_len})"))? =
            value;
        Ok(())
    }

    /// Copy `data` into `image` starting at `offset`.
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

    /// Convert a 256-byte `Vec<u8>` into a fixed-size array, returning an error on length mismatch.
    fn into_page_array(data: Vec<u8>) -> Result<[u8; 256], BoxErr> {
        let len = data.len();
        data.try_into()
            .map_err(|_| format!("expected 256-byte page, got {len}").into())
    }

    /// Build a mock 261-byte W response with the given page address and
    /// a 256-byte data payload. Returns an error if the payload length is wrong.
    fn build_w_response(page: u16, data: &[u8]) -> Result<Vec<u8>, BoxErr> {
        if data.len() != 256 {
            return Err(format!("W response payload must be 256 bytes, got {}", data.len()).into());
        }
        let addr = page.to_be_bytes();
        // W + 2-byte page + 0x00 0x00 + 256 data = 261 bytes.
        let [addr_hi, addr_lo] = addr;
        let mut resp = vec![b'W', addr_hi, addr_lo, 0x00, 0x00];
        resp.extend_from_slice(data);
        Ok(resp)
    }

    /// Build a 256-byte page payload with the given names in 16-byte slots.
    fn build_name_page(names: &[&str]) -> Result<Vec<u8>, BoxErr> {
        let mut data = vec![0u8; 256];
        for (i, name) in names.iter().enumerate().take(16) {
            let start = i * 16;
            let bytes = name.as_bytes();
            write_slice(&mut data, start, bytes)?;
        }
        Ok(data)
    }

    #[tokio::test]
    async fn mcp_entry_rejects_a_poisoned_gm_stream_before_io() -> TestResult {
        let mut radio = Radio::connect(MockTransport::new()).await?;
        radio.gm_poisoned = true;

        let result = radio.read_memory_pages(0, 1).await;
        assert!(matches!(result, Err(Error::MemoryReadStreamPoisoned)));
        assert_eq!(
            radio.mcp_phase,
            McpPhase::Inactive,
            "rejected MCP entry must not mutate programming state"
        );
        radio.transport.assert_complete();
        Ok(())
    }

    #[tokio::test(start_paused = true)]
    async fn read_channel_names_full_sequence() -> TestResult {
        // Mock the full programming mode sequence at 9600 baud throughout:
        // enter -> 63 page R/W/ACK loops -> exit.
        let mut mock = MockTransport::new();

        // Enter programming mode (no baud switch, no sync byte).
        mock.expect(b"\r0M PROGRAM\r", b"0M\r");

        // First page (256): has real names in slots 0-3.
        let first_page_data = build_name_page(&["ForestCityPD", "RPT1", "", "NOAA WX"])?;
        let read_cmd = programming::build_read_command(256);
        mock.expect(&read_cmd, &build_w_response(256, &first_page_data)?);

        // ACK exchange after first page, then remaining 62 pages.
        for page_offset in 1..programming::NAME_PAGE_COUNT {
            mock.expect(&[programming::ACK], &[programming::ACK]);

            let page = programming::NAME_START_PAGE + page_offset;
            let cmd = programming::build_read_command(page);
            let empty = vec![0u8; 256];
            mock.expect(&cmd, &build_w_response(page, &empty)?);
        }

        // Final ACK after last page.
        mock.expect(&[programming::ACK], &[programming::ACK]);

        // Exit programming mode, then the exit path reconnects.
        mock.expect(b"E", &[programming::ACK]);
        mock.expect_reopen(Ok(()));
        mock.expect(b"ID\r", b"ID TH-D75\r");

        let mut radio = Radio::connect(mock).await?;
        let names = radio.read_channel_names().await?;

        // 16 names per page * 63 pages = 1008, truncated to 1000.
        assert_eq!(names.len(), 1000);
        assert_eq!(names.first().ok_or("names[0] missing")?, "ForestCityPD");
        assert_eq!(names.get(1).ok_or("names[1] missing")?, "RPT1");
        assert_eq!(names.get(2).ok_or("names[2] missing")?, "");
        assert_eq!(names.get(3).ok_or("names[3] missing")?, "NOAA WX");
        for name in names.get(4..16).ok_or("names[4..16] missing")? {
            assert!(name.is_empty(), "expected empty name, got {name:?}");
        }
        Ok(())
    }

    #[tokio::test(start_paused = true)]
    async fn page_read_rejects_mismatched_address_and_retries() -> TestResult {
        let mut mock = MockTransport::new();
        mock.expect(b"\r0M PROGRAM\r", b"0M\r");

        let page: u16 = 0x0020;
        let cmd = programming::build_read_command(page);
        // The radio answers with a DIFFERENT page: a duplicate response
        // from an earlier retried read. Accepting it would store the
        // wrong page's bytes and shift the rest of a dump by one page.
        mock.expect(&cmd, &build_w_response(0x0021, &[0x11u8; 256])?);
        // A complete wrong-page W still requires the normal ACK exchange
        // before the retry command is safe.
        mock.expect(&[programming::ACK], &[programming::ACK]);
        // The retry re-requests and gets the right page.
        mock.expect(&cmd, &build_w_response(page, &[0x22u8; 256])?);
        mock.expect(&[programming::ACK], &[programming::ACK]);
        mock.expect(b"E", &[programming::ACK]);
        mock.expect_reopen(Ok(()));
        mock.expect(b"ID\r", b"ID TH-D75\r");

        let mut radio = Radio::connect(mock).await?;
        let data = radio.read_page(page).await?;
        assert_eq!(
            *data.first().ok_or("data[0] missing")?,
            0x22,
            "mismatched page must be rejected, not stored"
        );
        radio.transport.assert_complete();
        Ok(())
    }

    #[tokio::test(start_paused = true)]
    async fn persistent_page_mismatch_errors_and_still_exits() -> TestResult {
        let mut mock = MockTransport::new();
        mock.expect(b"\r0M PROGRAM\r", b"0M\r");

        let page: u16 = 0x0020;
        let cmd = programming::build_read_command(page);
        // Both the read and its retry answer with the wrong page.
        mock.expect(&cmd, &build_w_response(0x0021, &[0x11u8; 256])?);
        mock.expect(&[programming::ACK], &[programming::ACK]);
        mock.expect(&cmd, &build_w_response(0x0021, &[0x11u8; 256])?);
        mock.expect(&[programming::ACK], &[programming::ACK]);
        // Exit must still be attempted even though the read failed,
        // and the exit path reconnects.
        mock.expect(b"E", &[programming::ACK]);
        mock.expect_reopen(Ok(()));
        mock.expect(b"ID\r", b"ID TH-D75\r");

        let mut radio = Radio::connect(mock).await?;
        let result = radio.read_page(page).await;
        assert!(
            matches!(
                result,
                Err(Error::McpPageMismatch {
                    requested: 0x0020,
                    answered: 0x0021,
                })
            ),
            "persistent mismatch must surface: {result:?}"
        );
        radio.transport.assert_complete();
        Ok(())
    }

    #[tokio::test(start_paused = true)]
    async fn partial_page_timeout_is_not_retried_before_cleanup() -> TestResult {
        let mut mock = MockTransport::new();
        mock.expect(b"\r0M PROGRAM\r", b"0M\r");

        let page: u16 = 0x0020;
        let cmd = programming::build_read_command(page);
        let full_response = build_w_response(page, &[0x11u8; programming::PAGE_SIZE])?;
        let partial = full_response
            .get(..32)
            .ok_or("test W response unexpectedly shorter than 32 bytes")?;
        mock.expect_partial_then_hang(&cmd, partial);

        // No retry and no host ACK are scripted. The ambiguous response
        // must fail directly into the normal MCP exit path.
        mock.expect(b"E", &[programming::ACK]);
        mock.expect_reopen(Ok(()));
        mock.expect(b"ID\r", b"ID TH-D75\r");

        let mut radio = Radio::connect(mock).await?;
        let page_timeout = std::time::Duration::from_millis(50);
        radio.set_timeout(page_timeout);
        let result = radio.read_page(page).await;
        assert!(
            matches!(result, Err(Error::Timeout(timeout)) if timeout == page_timeout),
            "partial frame must time out without retrying: {result:?}"
        );
        radio.transport.assert_complete();
        Ok(())
    }

    #[tokio::test(start_paused = true)]
    async fn wrong_page_is_not_retried_without_completed_ack_handshake() -> TestResult {
        let mut mock = MockTransport::new();
        mock.expect(b"\r0M PROGRAM\r", b"0M\r");

        let requested_page: u16 = 0x0020;
        let answered_page: u16 = 0x0021;
        let cmd = programming::build_read_command(requested_page);
        mock.expect(
            &cmd,
            &build_w_response(answered_page, &[0x11u8; programming::PAGE_SIZE])?,
        );
        mock.expect(&[programming::ACK], &[0x15]);

        // A bad trailing ACK makes the exchange unsafe to retry. Cleanup is
        // the only remaining scripted operation.
        mock.expect(b"E", &[programming::ACK]);
        mock.expect_reopen(Ok(()));
        mock.expect(b"ID\r", b"ID TH-D75\r");

        let mut radio = Radio::connect(mock).await?;
        let result = radio.read_page(requested_page).await;
        assert!(
            matches!(
                result,
                Err(Error::McpPageReadNotAcknowledged {
                    page: 0x0021,
                    got: 0x15,
                })
            ),
            "wrong-page retry must require the radio's trailing ACK: {result:?}"
        );
        radio.transport.assert_complete();
        Ok(())
    }

    #[tokio::test(start_paused = true)]
    async fn interrupted_mcp_poisons_cat_until_recovered() -> TestResult {
        let mut mock = MockTransport::new();
        mock.expect(b"\r0M PROGRAM\r", b"0M\r");
        // The first page read never completes, so the caller's timeout
        // cancels the whole dump future mid-transfer.
        let cmd = programming::build_read_command(0);
        mock.expect_hang(&cmd);

        let mut radio = Radio::connect(mock).await?;
        let cancelled = tokio::time::timeout(
            std::time::Duration::from_millis(100),
            radio.read_memory_image(),
        )
        .await;
        assert!(cancelled.is_err(), "dump must be cancelled by the timeout");

        // The radio may still be in PROG MCP, so CAT must refuse rather
        // than talk binary-mode garbage.
        let refused = radio.execute(Command::GetMode { band: Band::A }).await;
        assert!(
            matches!(refused, Err(Error::McpInterrupted)),
            "CAT after a cancelled MCP session must refuse: {refused:?}"
        );

        // Recovery sends the exit byte, reconnects, and restores
        // normal operation.
        radio.transport.expect(b"E", &[programming::ACK]);
        radio.transport.expect_reopen(Ok(()));
        radio.transport.expect(b"ID\r", b"ID TH-D75\r");
        radio.recover_from_interrupted_mcp().await?;

        radio.transport.expect(b"MD 0\r", b"MD 0,0\r");
        let response = radio.execute(Command::GetMode { band: Band::A }).await?;
        assert!(matches!(response, Response::Mode { .. }));
        radio.transport.assert_complete();
        Ok(())
    }

    #[tokio::test(start_paused = true)]
    async fn cancelled_pre_entry_full_dump_recovery_restores_original_timeout() -> TestResult {
        let mut mock = MockTransport::new();
        // Keep the pre-entry stale-input drain pending long enough for
        // the caller to cancel before any MCP wire traffic is sent.
        mock.queue_read_delayed(b"stale\r", 100);

        let mut radio = Radio::connect(mock).await?;
        let original_timeout = std::time::Duration::from_secs(7);
        radio.set_timeout(original_timeout);

        let first_cancel = tokio::time::timeout(
            std::time::Duration::from_millis(1),
            radio.read_memory_image(),
        )
        .await;
        assert!(first_cancel.is_err(), "full dump was not cancelled");
        assert_eq!(
            radio.mcp_phase,
            McpPhase::Inactive,
            "pre-entry cancellation must not claim MCP wire traffic occurred"
        );
        assert_eq!(radio.timeout, super::FULL_DUMP_TIMEOUT);
        assert_eq!(radio.mcp_saved_timeout, Some(original_timeout));

        // A second pre-entry attempt must retain the first saved timeout
        // rather than replacing it with FULL_DUMP_TIMEOUT.
        let second_cancel = tokio::time::timeout(
            std::time::Duration::from_millis(1),
            radio.read_memory_image(),
        )
        .await;
        assert!(second_cancel.is_err(), "second full dump was not cancelled");
        assert_eq!(radio.mcp_saved_timeout, Some(original_timeout));

        // Recovery is meaningful even though the phase never became active:
        // it restores host-side state without touching the wire.
        radio.recover_from_interrupted_mcp().await?;
        assert_eq!(radio.timeout, original_timeout);
        assert_eq!(radio.mcp_saved_timeout, None);
        radio.transport.assert_complete();
        Ok(())
    }

    #[tokio::test(start_paused = true)]
    async fn poisoned_full_image_methods_do_not_mutate_saved_timeout() -> TestResult {
        let original_timeout = std::time::Duration::from_secs(7);

        let mut read_radio = Radio::connect(MockTransport::new()).await?;
        read_radio.timeout = super::FULL_DUMP_TIMEOUT;
        read_radio.mcp_saved_timeout = Some(original_timeout);
        read_radio.mcp_phase = McpPhase::Active;
        let read_result = read_radio.read_memory_image().await;
        assert!(
            matches!(read_result, Err(Error::McpInterrupted)),
            "poisoned full read must refuse before mutation: {read_result:?}"
        );
        assert_eq!(read_radio.timeout, super::FULL_DUMP_TIMEOUT);
        assert_eq!(read_radio.mcp_saved_timeout, Some(original_timeout));

        let mut write_radio = Radio::connect(MockTransport::new()).await?;
        write_radio.timeout = super::FULL_DUMP_TIMEOUT;
        write_radio.mcp_saved_timeout = Some(original_timeout);
        write_radio.mcp_phase = McpPhase::ExitSent;
        let image = vec![0; programming::TOTAL_SIZE];
        let write_result = write_radio.write_memory_image(&image).await;
        assert!(
            matches!(write_result, Err(Error::McpInterrupted)),
            "poisoned full write must refuse before mutation: {write_result:?}"
        );
        assert_eq!(write_radio.timeout, super::FULL_DUMP_TIMEOUT);
        assert_eq!(write_radio.mcp_saved_timeout, Some(original_timeout));
        Ok(())
    }

    #[tokio::test(start_paused = true)]
    async fn wrong_exit_ack_still_reconnects_but_surfaces_typed_error() -> TestResult {
        let mut mock = MockTransport::new();
        mock.expect(b"E", &[0x15]);
        mock.expect_reopen(Ok(()));
        mock.expect(b"ID\r", b"ID TH-D75\r");
        mock.expect(b"MD 0\r", b"MD 0,0\r");

        let mut radio = Radio::connect(mock).await?;
        radio.mcp_phase = McpPhase::Active;

        let result = radio.exit_programming_mode().await;
        assert!(
            matches!(result, Err(Error::McpExitNotAcknowledged { got: 0x15 })),
            "wrong exit byte must remain visible after successful CAT proof: {result:?}"
        );
        assert!(
            radio.mcp_phase == McpPhase::Inactive,
            "successful reconnect and ID must clear MCP poison"
        );
        assert!(
            !radio.desynced,
            "successful reconnect and ID must clear binary desynchronization"
        );

        let response = radio.execute(Command::GetMode { band: Band::A }).await?;
        assert!(matches!(response, Response::Mode { .. }));
        radio.transport.assert_complete();
        Ok(())
    }

    #[tokio::test(start_paused = true)]
    async fn cached_state_restore_failure_after_identify_does_not_repoison_mcp() -> TestResult {
        let mut mock = MockTransport::new();
        mock.expect(b"E", &[programming::ACK]);
        mock.expect_reopen(Ok(()));
        mock.expect(b"ID\r", b"ID TH-D75\r");
        mock.expect(b"AI 1\r", b"?\r");
        mock.expect(b"MD 0\r", b"MD 0,0\r");

        let mut radio = Radio::connect(mock).await?;
        radio.mcp_phase = McpPhase::Active;
        radio.auto_info_enabled = true;

        let result = radio.exit_programming_mode().await;
        assert!(
            matches!(result, Err(Error::RadioError)),
            "cached-state restore failure must remain an ordinary error: {result:?}"
        );
        assert_eq!(
            radio.mcp_phase,
            McpPhase::Inactive,
            "successful identify must clear MCP poison before cached-state restoration"
        );

        let response = radio.execute(Command::GetMode { band: Band::A }).await?;
        assert!(matches!(response, Response::Mode { .. }));
        radio.transport.assert_complete();
        Ok(())
    }

    #[tokio::test(start_paused = true)]
    async fn cancelled_cached_state_restore_after_identify_does_not_repoison_mcp() -> TestResult {
        let mut mock = MockTransport::new();
        mock.expect_reopen(Ok(()));
        mock.expect(b"ID\r", b"ID TH-D75\r");
        mock.expect_hang(b"AI 1\r");

        let mut radio = Radio::connect(mock).await?;
        radio.mcp_phase = McpPhase::ExitSent;
        radio.auto_info_enabled = true;

        let cancelled = tokio::time::timeout(
            std::time::Duration::from_millis(100),
            radio.reconnect_after_mcp_exit(),
        )
        .await;
        assert!(
            cancelled.is_err(),
            "cached-state restoration was not cancelled"
        );
        assert_eq!(
            radio.mcp_phase,
            McpPhase::Inactive,
            "cancellation after successful identify must not restore MCP poison"
        );
        assert!(
            radio.desynced,
            "cancelled cached-state command must drain possible late input before the next CAT command"
        );
        radio.transport.assert_complete();
        Ok(())
    }

    #[tokio::test(start_paused = true)]
    async fn exit_write_is_bounded_and_retains_exit_sent_phase() -> TestResult {
        let mut radio = Radio::connect(HangingWriteTransport).await?;
        radio.mcp_phase = McpPhase::Active;

        let result = radio.send_programming_exit().await;
        assert!(
            matches!(
                result,
                Err(Error::Timeout(timeout)) if timeout == super::MCP_EXIT_ACK_TIMEOUT
            ),
            "wedged raw exit write must time out explicitly: {result:?}"
        );
        assert_eq!(
            radio.mcp_phase,
            McpPhase::ExitSent,
            "write timeout must retain the no-second-E recovery phase"
        );
        assert!(
            radio.desynced,
            "write timeout must retain unknown wire state"
        );
        Ok(())
    }

    #[tokio::test(start_paused = true)]
    async fn cancelled_settle_preserves_exit_anomaly_for_recovery() -> TestResult {
        let mut mock = MockTransport::new();
        mock.expect(b"E", &[0x15]);

        let mut radio = Radio::connect(mock).await?;
        radio.mcp_phase = McpPhase::Active;

        // The wrong ACK is obtained immediately; cancellation then lands
        // during the reset-settle delay that follows it.
        let cancelled = tokio::time::timeout(
            std::time::Duration::from_millis(100),
            radio.exit_programming_mode(),
        )
        .await;
        assert!(cancelled.is_err(), "exit settle was not cancelled");
        assert_eq!(radio.mcp_phase, McpPhase::ExitSent);
        assert!(
            matches!(
                &radio.mcp_pending_exit_error,
                Some(Error::McpExitNotAcknowledged { got: 0x15 })
            ),
            "exit anomaly was not retained across cancellation"
        );

        // Recovery must not send E again, and must still report the
        // original ACK anomaly after independently proving CAT.
        radio.transport.expect_reopen(Ok(()));
        radio.transport.expect(b"ID\r", b"ID TH-D75\r");
        let recovery = radio.recover_from_interrupted_mcp().await;
        assert!(
            matches!(recovery, Err(Error::McpExitNotAcknowledged { got: 0x15 })),
            "recovery lost the retained exit anomaly: {recovery:?}"
        );
        assert_eq!(radio.mcp_phase, McpPhase::Inactive);
        assert!(radio.mcp_pending_exit_error.is_none());
        radio.transport.assert_complete();
        Ok(())
    }

    #[tokio::test(start_paused = true)]
    async fn detached_exit_without_ack_remains_poisoned() -> TestResult {
        let mut mock = MockTransport::new();
        mock.expect_hang(b"E");

        let mut radio = Radio::connect(mock).await?;
        radio.mcp_phase = McpPhase::Active;

        let result = radio.exit_programming_mode_detached().await;
        assert!(
            matches!(
                result,
                Err(Error::Timeout(timeout)) if timeout == super::MCP_EXIT_ACK_TIMEOUT
            ),
            "missing detached-exit ACK must time out explicitly: {result:?}"
        );
        assert!(
            radio.mcp_phase != McpPhase::Inactive,
            "an unconfirmed detached exit must keep CAT poisoned"
        );
        assert!(
            radio.desynced,
            "an unconfirmed detached exit must retain unknown wire state"
        );

        let refused = radio.execute(Command::GetMode { band: Band::A }).await;
        assert!(
            matches!(refused, Err(Error::McpInterrupted)),
            "CAT must remain blocked after an unconfirmed detached exit: {refused:?}"
        );
        radio.transport.assert_complete();
        Ok(())
    }

    #[tokio::test(start_paused = true)]
    async fn cancelled_exit_recovery_never_sends_a_second_exit_byte() -> TestResult {
        let mut mock = MockTransport::new();
        // The write succeeds, but waiting for its ACK never completes.
        // Cancellation therefore lands after E may have reached the radio.
        mock.expect_hang(b"E");

        let mut radio = Radio::connect(mock).await?;
        radio.mcp_phase = McpPhase::Active;

        let cancelled = tokio::time::timeout(
            std::time::Duration::from_millis(100),
            radio.exit_programming_mode(),
        )
        .await;
        assert!(cancelled.is_err(), "exit future was not cancelled");
        assert!(
            radio.mcp_phase == McpPhase::ExitSent,
            "cancellation after the E write must retain the exit-sent phase"
        );

        // There is deliberately no second E exchange in the strict mock.
        // Recovery may only wait out reset and prove CAT by reopening.
        radio.transport.expect_reopen(Ok(()));
        radio.transport.expect(b"ID\r", b"ID TH-D75\r");
        radio.recover_from_interrupted_mcp().await?;

        assert!(
            radio.mcp_phase == McpPhase::Inactive,
            "CAT proof must clear both MCP phase flags"
        );
        radio.transport.assert_complete();
        Ok(())
    }

    #[tokio::test(start_paused = true)]
    async fn sparse_read_reports_operation_and_unproved_reconnect_failures() -> TestResult {
        let mut mock = MockTransport::new();
        mock.expect(b"\r0M PROGRAM\r", b"0M\r");

        let page = 0x0010;
        let read = programming::build_read_command(page);
        // The short frame is followed by MockTransport's WouldBlock error,
        // producing a transfer failure without a timeout retry.
        mock.expect(&read, b"W");
        mock.expect(b"E", &[programming::ACK]);
        mock.expect_reopen(Err(TransportError::ReopenUnsupported));

        let mut radio = Radio::connect(mock).await?;
        let result = radio.read_sparse_memory_pages(&[page]).await;

        match result {
            Err(Error::McpOperationAndCleanupFailed { operation, cleanup }) => {
                assert!(
                    matches!(&*operation, Error::Transport(_)),
                    "the original page-read failure was not retained: {operation:?}"
                );
                assert!(
                    matches!(&*cleanup, Error::McpCleanupNotProved { .. }),
                    "the unproved reconnect failure was not prioritized: {cleanup:?}"
                );
            }
            other => {
                return Err(
                    format!("expected combined operation/cleanup failure, got {other:?}").into(),
                );
            }
        }
        assert!(
            radio.mcp_phase == McpPhase::ExitSent,
            "failed CAT proof must leave MCP recovery state poisoned"
        );
        radio.transport.assert_complete();
        Ok(())
    }

    #[tokio::test(start_paused = true)]
    async fn operation_and_exit_ack_failures_are_both_retained_after_cat_proof() -> TestResult {
        let mut mock = MockTransport::new();
        mock.expect(b"\r0M PROGRAM\r", b"0M\r");

        let page = 0x0010;
        let read = programming::build_read_command(page);
        mock.expect(&read, b"W");
        mock.expect(b"E", &[0x15]);
        mock.expect_reopen(Ok(()));
        mock.expect(b"ID\r", b"ID TH-D75\r");

        let mut radio = Radio::connect(mock).await?;
        let result = radio.read_sparse_memory_pages(&[page]).await;
        match result {
            Err(Error::McpOperationAndCleanupFailed { operation, cleanup }) => {
                assert!(
                    matches!(&*operation, Error::Transport(_)),
                    "original page-read failure was not retained: {operation:?}"
                );
                assert!(
                    matches!(&*cleanup, Error::McpExitNotAcknowledged { got: 0x15 }),
                    "exit-ACK anomaly was not retained: {cleanup:?}"
                );
            }
            other => {
                return Err(format!("expected both MCP failures, got {other:?}").into());
            }
        }
        assert_eq!(
            radio.mcp_phase,
            McpPhase::Inactive,
            "successful CAT proof must still clear MCP poison"
        );
        radio.transport.assert_complete();
        Ok(())
    }

    #[tokio::test(start_paused = true)]
    async fn failed_entry_retains_failed_exit_and_reconnect_cleanup() -> TestResult {
        let mut mock = MockTransport::new();
        mock.expect(b"\r0M PROGRAM\r", b"not-an-mcp-entry-acknowledgement");
        mock.expect(b"E", &[0x15]);
        mock.expect_reopen(Err(TransportError::ReopenUnsupported));

        let mut radio = Radio::connect(mock).await?;
        let result = radio.read_page(0).await;

        match result {
            Err(Error::McpOperationAndCleanupFailed { operation, cleanup }) => {
                assert!(
                    matches!(&*operation, Error::Protocol(_)),
                    "the failed entry was not retained: {operation:?}"
                );
                match *cleanup {
                    Error::McpOperationAndCleanupFailed {
                        operation: exit,
                        cleanup: reconnect,
                    } => {
                        assert!(
                            matches!(&*exit, Error::McpExitNotAcknowledged { got: 0x15 }),
                            "the failed exit acknowledgement was not retained: {exit:?}"
                        );
                        match *reconnect {
                            Error::McpCleanupNotProved { cleanup: source } => {
                                assert!(
                                    matches!(&*source, Error::Transport(_)),
                                    "the reconnect proof failure was not retained: {source:?}"
                                );
                            }
                            other => {
                                return Err(format!(
                                    "unproved reconnect lacked power-cycle guidance: {other:?}"
                                )
                                .into());
                            }
                        }
                    }
                    other => {
                        return Err(
                            format!("expected failed exit plus reconnect, got {other:?}").into(),
                        );
                    }
                }
            }
            other => {
                return Err(format!("expected failed entry plus cleanup, got {other:?}").into());
            }
        }
        assert!(
            radio.mcp_phase == McpPhase::ExitSent,
            "failed entry cleanup must keep CAT blocked"
        );
        radio.transport.assert_complete();
        Ok(())
    }

    #[tokio::test(start_paused = true)]
    async fn entry_drains_stale_noise_before_handshake() -> TestResult {
        let mut mock = MockTransport::new();
        // Stale AI/NMEA noise queued on the line from before the MCP
        // session: more than the entry parser's tolerance window.
        mock.queue_read(b"$GPGGA,123519,4807.038,N,01131.000,E,1,08,0.9*47\r\n");
        mock.expect(b"\r0M PROGRAM\r", b"0M\r");

        let page: u16 = 0x0020;
        let cmd = programming::build_read_command(page);
        mock.expect(&cmd, &build_w_response(page, &[0xAAu8; 256])?);
        mock.expect(&[programming::ACK], &[programming::ACK]);
        mock.expect(b"E", &[programming::ACK]);
        mock.expect_reopen(Ok(()));
        mock.expect(b"ID\r", b"ID TH-D75\r");

        let mut radio = Radio::connect(mock).await?;
        let data = radio.read_page(page).await?;
        assert_eq!(*data.first().ok_or("data[0] missing")?, 0xAA);
        Ok(())
    }

    #[tokio::test(start_paused = true)]
    async fn read_all_channels_rejects_corrupt_record() -> TestResult {
        let mut mock = MockTransport::new();
        mock.expect(b"\r0M PROGRAM\r", b"0M\r");

        let page_count = programming::CHANNEL_DATA_END - programming::CHANNEL_DATA_START + 1;
        for offset in 0..page_count {
            let page = programming::CHANNEL_DATA_START + offset;
            let mut page_data = vec![0u8; 256];
            if offset == 0 {
                // Corrupt the first channel record: byte 0x0A bits 1:0
                // = 3 is an invalid duplex value.
                set_byte(&mut page_data, 0x0A, 0x03)?;
            }
            let cmd = programming::build_read_command(page);
            mock.expect(&cmd, &build_w_response(page, &page_data)?);
            mock.expect(&[programming::ACK], &[programming::ACK]);
        }
        mock.expect(b"E", &[programming::ACK]);
        mock.expect_reopen(Ok(()));
        mock.expect(b"ID\r", b"ID TH-D75\r");

        let mut radio = Radio::connect(mock).await?;
        let result = radio.read_all_channels().await;
        assert!(
            matches!(result, Err(Error::Protocol(_))),
            "a corrupt channel record must error, not become a fabricated default: {result:?}"
        );
        Ok(())
    }

    #[tokio::test(start_paused = true)]
    async fn read_single_page_round_trip() -> TestResult {
        let mut mock = MockTransport::new();

        // Enter.
        mock.expect(b"\r0M PROGRAM\r", b"0M\r");

        // Read page 0x0020.
        let page: u16 = 0x0020;
        let mut page_data = vec![0xABu8; 256];
        set_byte(&mut page_data, 0, 0x00)?; // VHF flag
        let cmd = programming::build_read_command(page);
        mock.expect(&cmd, &build_w_response(page, &page_data)?);

        // ACK exchange.
        mock.expect(&[programming::ACK], &[programming::ACK]);

        // Exit, then the exit path reconnects.
        mock.expect(b"E", &[programming::ACK]);
        mock.expect_reopen(Ok(()));
        mock.expect(b"ID\r", b"ID TH-D75\r");

        let mut radio = Radio::connect(mock).await?;
        let result = radio.read_page(page).await?;
        assert_eq!(*result.first().ok_or("result[0] missing")?, 0x00);
        assert_eq!(*result.get(1).ok_or("result[1] missing")?, 0xAB);
        Ok(())
    }

    #[tokio::test(start_paused = true)]
    async fn sparse_page_read_sorts_deduplicates_and_reports_progress() -> TestResult {
        let mut mock = MockTransport::new();
        mock.expect(b"\r0M PROGRAM\r", b"0M\r");

        let low_page = 0x0010;
        let high_page = programming::TOTAL_PAGES - 1;
        let low_data = [0x11; programming::PAGE_SIZE];
        let high_data = [0x22; programming::PAGE_SIZE];

        // Input is deliberately unordered and duplicated. The strict mock
        // permits exactly one read and ACK exchange per distinct page, in
        // ascending order. The final factory-calibration page is readable.
        let low_read = programming::build_read_command(low_page);
        mock.expect(&low_read, &build_w_response(low_page, &low_data)?);
        mock.expect(&[programming::ACK], &[programming::ACK]);

        let high_read = programming::build_read_command(high_page);
        mock.expect(&high_read, &build_w_response(high_page, &high_data)?);
        mock.expect(&[programming::ACK], &[programming::ACK]);

        mock.expect(b"E", &[programming::ACK]);
        mock.expect_reopen(Ok(()));
        mock.expect(b"ID\r", b"ID TH-D75\r");

        let mut radio = Radio::connect(mock).await?;
        let mut progress = Vec::new();
        let pages = radio
            .read_sparse_memory_pages_with_progress(
                &[high_page, low_page, high_page],
                |completed, total| progress.push((completed, total)),
            )
            .await?;

        assert_eq!(pages, vec![(low_page, low_data), (high_page, high_data)]);
        assert_eq!(progress, vec![(1, 2), (2, 2)]);
        radio.transport.assert_complete();
        Ok(())
    }

    #[tokio::test]
    async fn sparse_page_read_rejects_out_of_range_page_before_io() -> TestResult {
        let mock = MockTransport::new();
        let mut radio = Radio::connect(mock).await?;
        let mut callback_called = false;

        let result = radio
            .read_sparse_memory_pages_with_progress(
                &[programming::TOTAL_PAGES - 1, programming::TOTAL_PAGES, 0],
                |_, _| callback_called = true,
            )
            .await;

        assert!(
            matches!(
                result,
                Err(Error::McpPageOutOfRange {
                    page,
                    total_pages
                }) if page == programming::TOTAL_PAGES
                    && total_pages == programming::TOTAL_PAGES
            ),
            "out-of-range request should return the invalid page: {result:?}"
        );
        assert!(
            !callback_called,
            "validation failure must not invoke the callback"
        );
        radio.transport.assert_complete();
        Ok(())
    }

    #[tokio::test]
    async fn sparse_page_read_empty_request_is_noop() -> TestResult {
        let mock = MockTransport::new();
        let mut radio = Radio::connect(mock).await?;
        let mut callback_called = false;

        let pages = radio
            .read_sparse_memory_pages_with_progress(&[], |_, _| callback_called = true)
            .await?;

        assert!(pages.is_empty());
        assert!(
            !callback_called,
            "empty request must not invoke the callback"
        );
        radio.transport.assert_complete();
        Ok(())
    }

    #[tokio::test(start_paused = true)]
    async fn sparse_page_read_exits_after_read_failure() -> TestResult {
        let mut mock = MockTransport::new();
        mock.expect(b"\r0M PROGRAM\r", b"0M\r");

        let page = 0x0010;
        let read = programming::build_read_command(page);
        // A short response is followed by MockTransport's WouldBlock error,
        // which fails the page read without triggering the timeout retry.
        mock.expect(&read, b"W");

        mock.expect(b"E", &[programming::ACK]);
        mock.expect_reopen(Ok(()));
        mock.expect(b"ID\r", b"ID TH-D75\r");

        let mut radio = Radio::connect(mock).await?;
        let result = radio.read_sparse_memory_pages(&[page]).await;

        assert!(
            matches!(result, Err(Error::Transport(_))),
            "short page response should surface as a transport error: {result:?}"
        );
        radio.transport.assert_complete();
        Ok(())
    }

    #[tokio::test(start_paused = true)]
    async fn write_single_page_round_trip() -> TestResult {
        let mut mock = MockTransport::new();

        // Enter.
        mock.expect(b"\r0M PROGRAM\r", b"0M\r");

        // Write page 0x0100.
        let page: u16 = 0x0100;
        let page_data = [0xCDu8; 256];
        let write_cmd = programming::build_write_command(page, &page_data);
        mock.expect(&write_cmd, &[programming::ACK]);

        // Verify read-back returns what was written.
        let read_cmd = programming::build_read_command(page);
        mock.expect(&read_cmd, &build_w_response(page, &page_data)?);
        mock.expect(&[programming::ACK], &[programming::ACK]);

        // Exit, then the exit path reconnects.
        mock.expect(b"E", &[programming::ACK]);
        mock.expect_reopen(Ok(()));
        mock.expect(b"ID\r", b"ID TH-D75\r");

        let mut radio = Radio::connect(mock).await?;
        radio.write_page(page, &page_data).await?;
        Ok(())
    }

    #[tokio::test(start_paused = true)]
    async fn modify_memory_pages_reads_all_and_writes_only_changed_pages() -> TestResult {
        let mut mock = MockTransport::new();
        mock.expect(b"\r0M PROGRAM\r", b"0M\r");

        let low_page = 0x0010;
        let high_page = 0x0152;
        let low_original = vec![0x11; programming::PAGE_SIZE];
        let high_original = vec![0x22; programming::PAGE_SIZE];

        // Input is deliberately unordered and duplicated. The implementation
        // reads each unique page once, in ascending order, before any write.
        let low_read = programming::build_read_command(low_page);
        mock.expect(&low_read, &build_w_response(low_page, &low_original)?);
        mock.expect(&[programming::ACK], &[programming::ACK]);

        let high_read = programming::build_read_command(high_page);
        mock.expect(&high_read, &build_w_response(high_page, &high_original)?);
        mock.expect(&[programming::ACK], &[programming::ACK]);

        // Only the low page is changed. The high page therefore has no write
        // exchange in the strict mock script.
        let mut low_modified = low_original.clone();
        set_byte(&mut low_modified, 0x34, 0xA5)?;
        let low_modified_array = into_page_array(low_modified.clone())?;
        let low_write = programming::build_write_command(low_page, &low_modified_array);
        mock.expect(&low_write, &[programming::ACK]);
        mock.expect(&low_read, &build_w_response(low_page, &low_modified)?);
        mock.expect(&[programming::ACK], &[programming::ACK]);

        mock.expect(b"E", &[programming::ACK]);
        mock.expect_reopen(Ok(()));
        mock.expect(b"ID\r", b"ID TH-D75\r");

        let mut radio = Radio::connect(mock).await?;
        let mut visited = Vec::new();
        let changed = radio
            .modify_memory_pages(&[high_page, low_page, high_page], |page, data| {
                visited.push(page);
                if page == low_page
                    && let Some(byte) = data.get_mut(0x34)
                {
                    *byte = 0xA5;
                }
            })
            .await?;

        assert_eq!(
            visited,
            vec![low_page, high_page],
            "callback should visit each distinct page in ascending order"
        );
        assert_eq!(
            changed,
            vec![low_page],
            "only the byte-different page should be written"
        );
        radio.transport.assert_complete();
        Ok(())
    }

    #[tokio::test]
    async fn modify_memory_pages_rejects_protected_page_before_io() -> TestResult {
        let mock = MockTransport::new();
        let mut radio = Radio::connect(mock).await?;
        let mut callback_called = false;

        let result = radio
            .modify_memory_pages(&[0x0010, 0x07A1], |_, _| callback_called = true)
            .await;

        assert!(
            matches!(result, Err(Error::MemoryWriteProtected { page: 0x07A1 })),
            "request should be rejected with the protected page number"
        );
        assert!(
            !callback_called,
            "callback must not run for a protected request"
        );
        radio.transport.assert_complete();
        Ok(())
    }

    #[tokio::test]
    async fn modify_memory_pages_empty_request_is_noop() -> TestResult {
        let mock = MockTransport::new();
        let mut radio = Radio::connect(mock).await?;
        let mut callback_called = false;

        let changed = radio
            .modify_memory_pages(&[], |_, _| callback_called = true)
            .await?;

        assert!(changed.is_empty(), "an empty request cannot write pages");
        assert!(
            !callback_called,
            "an empty request must not invoke the callback"
        );
        radio.transport.assert_complete();
        Ok(())
    }

    #[tokio::test(start_paused = true)]
    async fn modify_memory_pages_exits_after_read_failure() -> TestResult {
        let mut mock = MockTransport::new();
        mock.expect(b"\r0M PROGRAM\r", b"0M\r");

        let page = 0x0010;
        let read = programming::build_read_command(page);
        // A short response is followed by MockTransport's WouldBlock error,
        // which fails the page read without triggering the timeout retry.
        mock.expect(&read, b"W");

        mock.expect(b"E", &[programming::ACK]);
        mock.expect_reopen(Ok(()));
        mock.expect(b"ID\r", b"ID TH-D75\r");

        let mut radio = Radio::connect(mock).await?;
        let result = radio.modify_memory_pages(&[page], |_, _| {}).await;

        assert!(
            matches!(result, Err(Error::Transport(_))),
            "short page response should surface as a transport error: {result:?}"
        );
        radio.transport.assert_complete();
        Ok(())
    }

    #[tokio::test(start_paused = true)]
    async fn apply_menu_patches_writes_and_verifies_every_changed_page() -> TestResult {
        use crate::memory::{FieldValue, PatchPlanner, menu_field};

        let mut mock = MockTransport::new();
        mock.expect(b"ID\r", b"ID TH-D75\r");
        // Hardware captures contain both `1.03` and `1.03.000`; this
        // exercises the extended exact identity through the live gate.
        mock.expect(b"FV\r", b"FV 1.03.000\r");
        mock.expect(b"\r0M PROGRAM\r", b"0M\r");

        // radio.Beep (Bool at 0x1071, page 0x10) and gps.MyPositionSelect
        // (Byte at 0x11C0, page 0x11) both change, so the plan spans two
        // pages that must each be written and read-back verified within the
        // same programming session.
        let beep_page = 0x0010;
        let select_page = 0x0011;
        let beep_original = vec![0x00; programming::PAGE_SIZE];
        let select_original = vec![0x00; programming::PAGE_SIZE];

        let beep_read = programming::build_read_command(beep_page);
        mock.expect(&beep_read, &build_w_response(beep_page, &beep_original)?);
        mock.expect(&[programming::ACK], &[programming::ACK]);
        let select_read = programming::build_read_command(select_page);
        mock.expect(
            &select_read,
            &build_w_response(select_page, &select_original)?,
        );
        mock.expect(&[programming::ACK], &[programming::ACK]);

        let mut beep_modified = beep_original.clone();
        set_byte(&mut beep_modified, 0x71, 0x01)?;
        let beep_modified_array = into_page_array(beep_modified.clone())?;
        let beep_write = programming::build_write_command(beep_page, &beep_modified_array);
        mock.expect(&beep_write, &[programming::ACK]);
        mock.expect(&beep_read, &build_w_response(beep_page, &beep_modified)?);
        mock.expect(&[programming::ACK], &[programming::ACK]);

        let mut select_modified = select_original.clone();
        set_byte(&mut select_modified, 0xC0, 0x02)?;
        let select_modified_array = into_page_array(select_modified.clone())?;
        let select_write = programming::build_write_command(select_page, &select_modified_array);
        mock.expect(&select_write, &[programming::ACK]);
        mock.expect(
            &select_read,
            &build_w_response(select_page, &select_modified)?,
        );
        mock.expect(&[programming::ACK], &[programming::ACK]);

        mock.expect(b"E", &[programming::ACK]);
        mock.expect_reopen(Ok(()));
        mock.expect(b"ID\r", b"ID TH-D75\r");

        let mut radio = Radio::connect(mock).await?;
        let mut planner = PatchPlanner::new();
        let beep = menu_field("radio.Beep").ok_or("registry field radio.Beep missing")?;
        beep.plan_value(&mut planner, FieldValue::Bool(true))?;
        let select = menu_field("gps.MyPositionSelect")
            .ok_or("registry field gps.MyPositionSelect missing")?;
        select.plan_value(&mut planner, FieldValue::Unsigned(2))?;
        let patches = planner.finish()?;

        let changed = radio.apply_menu_patches(&patches).await?;
        assert_eq!(
            changed,
            vec![beep_page, select_page],
            "both changed pages must be written and verified in ascending order"
        );
        radio.transport.assert_complete();
        Ok(())
    }

    #[tokio::test]
    async fn apply_menu_patches_rejects_unqualified_firmware_before_mcp_io() -> TestResult {
        use crate::memory::{FieldValue, PatchPlanner, menu_field};

        let mut planner = PatchPlanner::new();
        let beep = menu_field("radio.Beep").ok_or("registry field radio.Beep missing")?;
        beep.plan_value(&mut planner, FieldValue::Bool(true))?;
        let patches = planner.finish()?;

        let mut mock = MockTransport::new();
        mock.expect(b"ID\r", b"ID TH-D75\r");
        mock.expect(b"FV\r", b"FV 1.04\r");

        let mut radio = Radio::connect(mock).await?;
        let result = radio.apply_menu_patches(&patches).await;
        let message = match &result {
            Err(error) => error.to_string(),
            Ok(changed) => {
                return Err(format!("unqualified firmware wrote pages: {changed:?}").into());
            }
        };
        assert!(
            message.contains("1.03.000") && message.contains("1.04"),
            "qualification error must list accepted and actual CAT identities: {message}"
        );
        assert!(
            matches!(
                result,
                Err(Error::UnsupportedMcpSchemaTarget {
                    expected_model: "TH-D75",
                    expected_firmware: "1.03",
                    ref actual_model,
                    ref actual_firmware,
                    ..
                }) if actual_model == "TH-D75" && actual_firmware == "1.04"
            ),
            "unqualified firmware must fail before MCP I/O: {result:?}"
        );
        radio.transport.assert_complete();
        Ok(())
    }

    #[tokio::test(start_paused = true)]
    async fn modify_memory_page_detached_skips_reconnect() -> TestResult {
        let mut mock = MockTransport::new();
        mock.expect(b"\r0M PROGRAM\r", b"0M\r");

        // Read-modify-write of one page (verified by read-back), as
        // the terminal-mode enable flow performs it.
        let page: u16 = 0x001C;
        let original = vec![0u8; 256];
        let mut modified = original.clone();
        set_byte(&mut modified, 0xA0, 0x01)?;
        let read_cmd = programming::build_read_command(page);
        mock.expect(&read_cmd, &build_w_response(page, &original)?);
        mock.expect(&[programming::ACK], &[programming::ACK]);
        let modified_array = into_page_array(modified.clone())?;
        let write_cmd = programming::build_write_command(page, &modified_array);
        mock.expect(&write_cmd, &[programming::ACK]);
        mock.expect(&read_cmd, &build_w_response(page, &modified)?);
        mock.expect(&[programming::ACK], &[programming::ACK]);

        // Exit byte only: NO reopen and NO identify are scripted, so
        // any reconnect attempt would fail the strict mock. The link
        // is deliberately left dead for the radio's reboot.
        mock.expect(b"E", &[programming::ACK]);

        let mut radio = Radio::connect(mock).await?;
        radio
            .modify_memory_page_detached(page, |data| {
                if let Some(b) = data.get_mut(0xA0) {
                    *b = 0x01;
                }
            })
            .await?;
        radio.transport.assert_complete();
        Ok(())
    }

    #[tokio::test(start_paused = true)]
    async fn detached_modify_rejects_nonzero_w_offset_before_patch_or_ack() -> TestResult {
        let mut mock = MockTransport::new();
        mock.expect(b"\r0M PROGRAM\r", b"0M\r");

        let page: u16 = 0x001C;
        let read_cmd = programming::build_read_command(page);
        let mut response = build_w_response(page, &[0x5A; programming::PAGE_SIZE])?;
        set_byte(&mut response, 3, 0x00)?;
        set_byte(&mut response, 4, 0x01)?;
        mock.expect(&read_cmd, &response);

        // The invalid W frame must not receive a host ACK or reach the patch
        // callback. The operation-error path requires normal CAT proof.
        mock.expect(b"E", &[programming::ACK]);
        mock.expect_reopen(Ok(()));
        mock.expect(b"ID\r", b"ID TH-D75\r");

        let mut radio = Radio::connect(mock).await?;
        let mut callback_called = false;
        let result = radio
            .modify_memory_page_detached(page, |_| callback_called = true)
            .await;
        assert!(
            matches!(
                result,
                Err(Error::Protocol(ProtocolError::WriteResponseNonzeroOffset {
                    got: 1
                }))
            ),
            "nonzero W offset was not rejected: {result:?}"
        );
        assert!(
            !callback_called,
            "invalid offset payload reached the patch callback"
        );
        assert_eq!(
            radio.mcp_phase,
            McpPhase::Inactive,
            "CAT identity proof should clear MCP after rejecting the frame"
        );
        radio.transport.assert_complete();
        Ok(())
    }

    #[tokio::test(start_paused = true)]
    async fn detached_operation_failure_does_not_trust_stale_ack_as_exit_proof() -> TestResult {
        let mut mock = MockTransport::new();
        mock.expect(b"\r0M PROGRAM\r", b"0M\r");

        let page: u16 = 0x001C;
        let read_cmd = programming::build_read_command(page);
        let full_response = build_w_response(page, &[0x5A; programming::PAGE_SIZE])?;
        let partial = full_response
            .get(..32)
            .ok_or("test W response unexpectedly shorter than 32 bytes")?;
        // The partial W times out. A delayed ACK then sits ahead of the
        // actual E response and can falsely satisfy a detached exit that
        // relies on one byte alone.
        mock.expect_partial_then_hang_with_late(&read_cmd, partial, &[programming::ACK]);
        mock.expect(b"E", &[programming::ACK]);
        mock.expect_reopen(Err(TransportError::ReopenUnsupported));

        let mut radio = Radio::connect(mock).await?;
        let page_timeout = std::time::Duration::from_millis(50);
        radio.set_timeout(page_timeout);
        let result = radio
            .modify_memory_page_detached(page, |data| {
                if let Some(byte) = data.get_mut(0xA0) {
                    *byte = 1;
                }
            })
            .await;

        match result {
            Err(Error::McpOperationAndCleanupFailed { operation, cleanup }) => {
                assert!(
                    matches!(&*operation, Error::Timeout(timeout) if *timeout == page_timeout),
                    "partial page failure was not retained: {operation:?}"
                );
                assert!(
                    matches!(&*cleanup, Error::McpCleanupNotProved { .. }),
                    "stale ACK incorrectly proved the detached exit: {cleanup:?}"
                );
            }
            other => {
                return Err(format!(
                    "expected page failure plus unproved CAT cleanup, got {other:?}"
                )
                .into());
            }
        }
        assert_eq!(
            radio.mcp_phase,
            McpPhase::ExitSent,
            "failed CAT identity proof must keep detached cleanup poisoned"
        );
        radio.transport.assert_complete();
        Ok(())
    }

    #[tokio::test(start_paused = true)]
    async fn write_page_verify_mismatch_is_typed() -> TestResult {
        let mut mock = MockTransport::new();
        mock.expect(b"\r0M PROGRAM\r", b"0M\r");

        let page: u16 = 0x0100;
        let page_data = [0xCDu8; 256];
        let write_cmd = programming::build_write_command(page, &page_data);
        mock.expect(&write_cmd, &[programming::ACK]);

        // Read-back returns one flipped byte at offset 7: the radio
        // ACKed the write but the byte did not land.
        let mut corrupted = page_data.to_vec();
        set_byte(&mut corrupted, 7, 0x00)?;
        let read_cmd = programming::build_read_command(page);
        mock.expect(&read_cmd, &build_w_response(page, &corrupted)?);
        mock.expect(&[programming::ACK], &[programming::ACK]);

        // Exit (with reconnect) still runs even though verify failed.
        mock.expect(b"E", &[programming::ACK]);
        mock.expect_reopen(Ok(()));
        mock.expect(b"ID\r", b"ID TH-D75\r");

        let mut radio = Radio::connect(mock).await?;
        let result = radio.write_page(page, &page_data).await;
        assert!(
            matches!(
                result,
                Err(Error::McpVerifyMismatch {
                    page: 0x0100,
                    offset: 7,
                    expected: 0xCD,
                    actual: 0x00,
                })
            ),
            "verify mismatch must surface with the differing byte: {result:?}"
        );
        Ok(())
    }

    #[tokio::test(start_paused = true)]
    async fn write_page_unverified_skips_readback() -> TestResult {
        let mut mock = MockTransport::new();
        mock.expect(b"\r0M PROGRAM\r", b"0M\r");

        let page: u16 = 0x0100;
        let page_data = [0xCDu8; 256];
        let write_cmd = programming::build_write_command(page, &page_data);
        mock.expect(&write_cmd, &[programming::ACK]);

        // No read-back scripted: the unverified variant must not read.
        mock.expect(b"E", &[programming::ACK]);
        mock.expect_reopen(Ok(()));
        mock.expect(b"ID\r", b"ID TH-D75\r");

        let mut radio = Radio::connect(mock).await?;
        radio.write_page_unverified(page, &page_data).await?;
        radio.transport.assert_complete();
        Ok(())
    }

    #[tokio::test]
    async fn write_factory_cal_page_rejected() -> TestResult {
        let mock = MockTransport::new();
        let mut radio = Radio::connect(mock).await?;

        let data = [0u8; 256];
        let result = radio.write_page(0x07A1, &data).await;
        let err = result
            .err()
            .ok_or("expected factory-cal write to fail but it succeeded")?;
        assert!(
            err.to_string().contains("protected"),
            "error should mention protected: {err}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn write_memory_image_wrong_size_rejected() -> TestResult {
        let mock = MockTransport::new();
        let mut radio = Radio::connect(mock).await?;

        let bad_image = vec![0u8; 1000]; // wrong size
        let result = radio.write_memory_image(&bad_image).await;
        let err = result
            .err()
            .ok_or("expected wrong-size write to fail but it succeeded")?;
        assert!(
            err.to_string().contains("invalid memory image size"),
            "error should mention size: {err}"
        );
        Ok(())
    }

    #[tokio::test(start_paused = true)]
    async fn read_memory_pages_small_range() -> TestResult {
        let mut mock = MockTransport::new();

        // Enter.
        mock.expect(b"\r0M PROGRAM\r", b"0M\r");

        // Read 2 pages starting at 0x0040.
        for i in 0..2u16 {
            let page = 0x0040 + i;
            #[expect(
                clippy::cast_possible_truncation,
                reason = "Test-only. `i` iterates 0..2, so the u16-to-u8 cast is trivially \
                          lossless (max value is 1)."
            )]
            let data = vec![i as u8; 256];
            let cmd = programming::build_read_command(page);
            mock.expect(&cmd, &build_w_response(page, &data)?);
            mock.expect(&[programming::ACK], &[programming::ACK]);
        }

        // Exit, then the exit path reconnects.
        mock.expect(b"E", &[programming::ACK]);
        mock.expect_reopen(Ok(()));
        mock.expect(b"ID\r", b"ID TH-D75\r");

        let mut radio = Radio::connect(mock).await?;
        let data = radio.read_memory_pages(0x0040, 2).await?;
        assert_eq!(data.len(), 512);
        // First page is all 0x00, second is all 0x01.
        assert!(
            data.get(..256)
                .ok_or("data[..256] missing")?
                .iter()
                .all(|&b| b == 0x00),
            "first page should be all 0x00"
        );
        assert!(
            data.get(256..)
                .ok_or("data[256..] missing")?
                .iter()
                .all(|&b| b == 0x01),
            "second page should be all 0x01"
        );
        Ok(())
    }

    #[tokio::test]
    async fn contiguous_reads_validate_complete_range_before_io() -> TestResult {
        let mock = MockTransport::new();
        let mut radio = Radio::connect(mock).await?;

        let empty = radio.read_memory_pages(u16::MAX, 0).await?;
        assert!(empty.is_empty(), "zero-page read must be a no-op");

        let crossing = radio
            .read_memory_pages(programming::TOTAL_PAGES - 1, 2)
            .await;
        assert!(
            matches!(
                crossing,
                Err(Error::McpPageOutOfRange {
                    page,
                    total_pages,
                }) if page == programming::TOTAL_PAGES
                    && total_pages == programming::TOTAL_PAGES
            ),
            "range crossing the image end must fail before I/O: {crossing:?}"
        );

        let overflowing = radio.read_memory_pages(1, u16::MAX).await;
        assert!(
            matches!(
                overflowing,
                Err(Error::McpPageOutOfRange {
                    page,
                    total_pages,
                }) if page == programming::TOTAL_PAGES
                    && total_pages == programming::TOTAL_PAGES
            ),
            "overflowing range must fail before I/O: {overflowing:?}"
        );

        let single = radio.read_page(u16::MAX).await;
        assert!(
            matches!(
                single,
                Err(Error::McpPageOutOfRange {
                    page: u16::MAX,
                    total_pages,
                }) if total_pages == programming::TOTAL_PAGES
            ),
            "out-of-range single page must fail before I/O: {single:?}"
        );
        radio.transport.assert_complete();
        Ok(())
    }

    #[tokio::test]
    async fn contiguous_writes_validate_shape_and_range_before_io() -> TestResult {
        let mock = MockTransport::new();
        let mut radio = Radio::connect(mock).await?;

        radio.write_memory_pages(u16::MAX, &[]).await?;

        let unaligned = vec![0; programming::PAGE_SIZE + 1];
        let alignment_result = radio.write_memory_pages(0, &unaligned).await;
        assert!(
            matches!(
                alignment_result,
                Err(Error::InvalidImageSize {
                    actual,
                    expected,
                }) if actual == programming::PAGE_SIZE + 1
                    && expected == programming::PAGE_SIZE * 2
            ),
            "unaligned data must fail before MCP entry: {alignment_result:?}"
        );

        let crossing = vec![0; programming::PAGE_SIZE * 2];
        let range_result = radio
            .write_memory_pages(programming::TOTAL_PAGES - 1, &crossing)
            .await;
        assert!(
            matches!(
                range_result,
                Err(Error::McpPageOutOfRange {
                    page,
                    total_pages,
                }) if page == programming::TOTAL_PAGES
                    && total_pages == programming::TOTAL_PAGES
            ),
            "write crossing the image end must fail before MCP entry: {range_result:?}"
        );
        radio.transport.assert_complete();
        Ok(())
    }

    #[tokio::test]
    async fn write_memory_pages_protected_range_rejected() -> TestResult {
        let mock = MockTransport::new();
        let mut radio = Radio::connect(mock).await?;

        // Try to write 3 pages starting at 0x07A0 -- page 0x07A1 is protected.
        let data = vec![0u8; 768]; // 3 pages
        let result = radio.write_memory_pages(0x07A0, &data).await;
        assert!(
            result.is_err(),
            "expected protected-range write to fail: {result:?}"
        );
        Ok(())
    }

    #[tokio::test(start_paused = true)]
    async fn read_channel_flags_sequence() -> TestResult {
        let mut mock = MockTransport::new();

        // Enter.
        mock.expect(b"\r0M PROGRAM\r", b"0M\r");

        // Channel flags span pages 0x0020 through 0x0032 (19 pages).
        let page_count = programming::CHANNEL_FLAGS_END - programming::CHANNEL_FLAGS_START + 1;
        for i in 0..page_count {
            let page = programming::CHANNEL_FLAGS_START + i;
            // Build page with flag records:
            // first 4 bytes = channel flag, rest = empty (0xFF).
            let mut data = vec![0xFF_u8; 256];
            if i == 0 {
                // Channel 0: VHF, not locked, group 0
                set_byte(&mut data, 0, 0x00)?; // used = VHF
                set_byte(&mut data, 1, 0x00)?; // not locked
                set_byte(&mut data, 2, 0x00)?; // group 0
                set_byte(&mut data, 3, 0xFF)?;
                // Channel 1: UHF, locked, group 5
                set_byte(&mut data, 4, 0x02)?; // used = UHF
                set_byte(&mut data, 5, 0x01)?; // locked
                set_byte(&mut data, 6, 0x05)?; // group 5
                set_byte(&mut data, 7, 0xFF)?;
            }
            let cmd = programming::build_read_command(page);
            mock.expect(&cmd, &build_w_response(page, &data)?);
            mock.expect(&[programming::ACK], &[programming::ACK]);
        }

        // Exit, then the exit path reconnects.
        mock.expect(b"E", &[programming::ACK]);
        mock.expect_reopen(Ok(()));
        mock.expect(b"ID\r", b"ID TH-D75\r");

        let mut radio = Radio::connect(mock).await?;
        let flags = radio.read_channel_flags().await?;

        // Should have 1200 flags.
        assert_eq!(flags.len(), programming::TOTAL_CHANNEL_ENTRIES);

        // Check the first two we programmed.
        let ch0 = flags.first().ok_or("channel 0 flag missing")?;
        assert!(!ch0.is_empty());
        assert_eq!(ch0.used, programming::FLAG_VHF);
        assert!(!ch0.lockout);
        assert_eq!(ch0.group, 0);

        let ch1 = flags.get(1).ok_or("channel 1 flag missing")?;
        assert!(!ch1.is_empty());
        assert_eq!(ch1.used, programming::FLAG_UHF);
        assert!(ch1.lockout);
        assert_eq!(ch1.group, 5);

        // The rest should be empty.
        let ch2 = flags.get(2).ok_or("channel 2 flag missing")?;
        assert!(ch2.is_empty());
        Ok(())
    }

    #[tokio::test(start_paused = true)]
    async fn progress_callback_invoked() -> TestResult {
        let mut mock = MockTransport::new();

        // Enter.
        mock.expect(b"\r0M PROGRAM\r", b"0M\r");

        // Read 3 pages.
        for i in 0..3u16 {
            let page = 0x0100 + i;
            let data = vec![0u8; 256];
            let cmd = programming::build_read_command(page);
            mock.expect(&cmd, &build_w_response(page, &data)?);
            mock.expect(&[programming::ACK], &[programming::ACK]);
        }

        // Exit, then the exit path reconnects.
        mock.expect(b"E", &[programming::ACK]);
        mock.expect_reopen(Ok(()));
        mock.expect(b"ID\r", b"ID TH-D75\r");

        let mut radio = Radio::connect(mock).await?;

        // Use read_memory_pages (which doesn't expose progress), but we
        // can test the internal progress via read_memory_image_with_progress
        // indirectly. For now, just verify read_memory_pages works with 3 pages.
        let data = radio.read_memory_pages(0x0100, 3).await?;
        assert_eq!(data.len(), 768);
        Ok(())
    }

    #[tokio::test(start_paused = true)]
    async fn modify_memory_page_read_modify_write() -> TestResult {
        let mut mock = MockTransport::new();

        // Page 0x0010 contains MCP offset 0x1000-0x10FF.
        let page: u16 = 0x0010;
        let byte_index: usize = 0x71; // offset 0x1071 within this page

        // Original page data: all zeros.
        let mut original_data = vec![0u8; 256];
        set_byte(&mut original_data, byte_index, 0x00)?; // beep off

        // Expected modified data: byte at 0x71 set to 1.
        let mut expected_data = original_data.clone();
        set_byte(&mut expected_data, byte_index, 0x01)?;

        // Enter programming mode.
        mock.expect(b"\r0M PROGRAM\r", b"0M\r");

        // Read page.
        let read_cmd = programming::build_read_command(page);
        mock.expect(&read_cmd, &build_w_response(page, &original_data)?);

        // ACK exchange after read.
        mock.expect(&[programming::ACK], &[programming::ACK]);

        // Write modified page.
        let expected_array = into_page_array(expected_data.clone())?;
        let write_cmd = programming::build_write_command(page, &expected_array);
        mock.expect(&write_cmd, &[programming::ACK]);

        // Verify read-back returns the modified page.
        mock.expect(&read_cmd, &build_w_response(page, &expected_array)?);
        mock.expect(&[programming::ACK], &[programming::ACK]);

        // Exit programming mode, then the exit path reconnects.
        mock.expect(b"E", &[programming::ACK]);
        mock.expect_reopen(Ok(()));
        mock.expect(b"ID\r", b"ID TH-D75\r");

        let mut radio = Radio::connect(mock).await?;
        radio
            .modify_memory_page(page, |data| {
                // The closure has mutable access to a 256-byte array; indexing at a fixed
                // compile-time-known byte is safe here. Keep it explicit via `.get_mut`.
                if let Some(b) = data.get_mut(byte_index) {
                    *b = 0x01;
                }
            })
            .await?;
        Ok(())
    }

    #[tokio::test]
    async fn modify_memory_page_factory_cal_rejected() -> TestResult {
        let mock = MockTransport::new();
        let mut radio = Radio::connect(mock).await?;

        let result = radio
            .modify_memory_page(0x07A1, |_data| {
                // Should never be called.
            })
            .await;
        let err = result
            .err()
            .ok_or("expected factory-cal modify to fail but it succeeded")?;
        assert!(
            err.to_string().contains("protected"),
            "error should mention protected: {err}"
        );
        Ok(())
    }

    #[tokio::test(start_paused = true)]
    async fn write_channel_name_round_trip() -> TestResult {
        let mut mock = MockTransport::new();

        // Channel 5 lives on page 0x0100 (5 / 16 = 0), offset = 5 * 16 = 80.
        let page: u16 = 0x0100;
        let offset = 5 * programming::NAME_ENTRY_SIZE;

        // Original page: all zeros (empty names).
        let original_data = vec![0u8; 256];

        // Expected: "TestCh" written at offset 80, null-padded.
        let mut expected_data = original_data.clone();
        let name = b"TestCh";
        write_slice(&mut expected_data, offset, name)?;

        // Enter programming mode.
        mock.expect(b"\r0M PROGRAM\r", b"0M\r");

        // Read page.
        let read_cmd = programming::build_read_command(page);
        mock.expect(&read_cmd, &build_w_response(page, &original_data)?);

        // ACK exchange after read.
        mock.expect(&[programming::ACK], &[programming::ACK]);

        // Write modified page.
        let expected_array = into_page_array(expected_data)?;
        let write_cmd = programming::build_write_command(page, &expected_array);
        mock.expect(&write_cmd, &[programming::ACK]);

        // Verify read-back returns the modified page.
        mock.expect(&read_cmd, &build_w_response(page, &expected_array)?);
        mock.expect(&[programming::ACK], &[programming::ACK]);

        // Exit programming mode, then the exit path reconnects.
        mock.expect(b"E", &[programming::ACK]);
        mock.expect_reopen(Ok(()));
        mock.expect(b"ID\r", b"ID TH-D75\r");

        let mut radio = Radio::connect(mock).await?;
        radio.write_channel_name(5, "TestCh").await?;
        Ok(())
    }

    #[tokio::test]
    async fn write_channel_name_out_of_range_rejected() -> TestResult {
        let mock = MockTransport::new();
        let mut radio = Radio::connect(mock).await?;

        let result = radio.write_channel_name(1200, "Bad").await;
        let err = result
            .err()
            .ok_or("expected out-of-range write to fail but it succeeded")?;
        assert!(
            err.to_string().contains("out of range"),
            "error should mention out of range: {err}"
        );
        Ok(())
    }

    #[tokio::test(start_paused = true)]
    async fn write_channel_name_truncates_long_name() -> TestResult {
        let mut mock = MockTransport::new();

        // Channel 0 on page 0x0100, offset 0.
        let page: u16 = 0x0100;
        let original_data = vec![0u8; 256];

        // A name longer than 15 bytes should be truncated to 15.
        let long_name = "ABCDEFGHIJKLMNOP"; // 16 chars
        let mut expected_data = original_data.clone();
        // Only first 15 bytes written (leaving null terminator).
        let truncated = long_name
            .as_bytes()
            .get(..15)
            .ok_or("long_name shorter than 15 bytes")?;
        write_slice(&mut expected_data, 0, truncated)?;

        mock.expect(b"\r0M PROGRAM\r", b"0M\r");
        let read_cmd = programming::build_read_command(page);
        mock.expect(&read_cmd, &build_w_response(page, &original_data)?);
        mock.expect(&[programming::ACK], &[programming::ACK]);
        let expected_array = into_page_array(expected_data)?;
        let write_cmd = programming::build_write_command(page, &expected_array);
        mock.expect(&write_cmd, &[programming::ACK]);
        mock.expect(&read_cmd, &build_w_response(page, &expected_array)?);
        mock.expect(&[programming::ACK], &[programming::ACK]);
        mock.expect(b"E", &[programming::ACK]);
        mock.expect_reopen(Ok(()));
        mock.expect(b"ID\r", b"ID TH-D75\r");

        let mut radio = Radio::connect(mock).await?;
        radio.write_channel_name(0, long_name).await?;
        Ok(())
    }

    #[tokio::test(start_paused = true)]
    async fn read_all_channel_names_returns_1200() -> TestResult {
        let mut mock = MockTransport::new();

        // Enter programming mode.
        mock.expect(b"\r0M PROGRAM\r", b"0M\r");

        // First page has some names.
        let first_page_data = build_name_page(&["AllCh0", "AllCh1"])?;
        let read_cmd = programming::build_read_command(programming::CHANNEL_NAMES_START);
        mock.expect(
            &read_cmd,
            &build_w_response(programming::CHANNEL_NAMES_START, &first_page_data)?,
        );

        // Remaining 74 pages are empty.
        for page_offset in 1..programming::NAME_ALL_PAGE_COUNT {
            mock.expect(&[programming::ACK], &[programming::ACK]);

            let page = programming::NAME_START_PAGE + page_offset;
            let cmd = programming::build_read_command(page);
            let empty = vec![0u8; 256];
            mock.expect(&cmd, &build_w_response(page, &empty)?);
        }

        // Final ACK after last page.
        mock.expect(&[programming::ACK], &[programming::ACK]);

        // Exit programming mode, then the exit path reconnects.
        mock.expect(b"E", &[programming::ACK]);
        mock.expect_reopen(Ok(()));
        mock.expect(b"ID\r", b"ID TH-D75\r");

        let mut radio = Radio::connect(mock).await?;
        let names = radio.read_all_channel_names().await?;

        // 16 names per page * 75 pages = 1200.
        assert_eq!(names.len(), 1200);
        assert_eq!(names.first().ok_or("names[0] missing")?, "AllCh0");
        assert_eq!(names.get(1).ok_or("names[1] missing")?, "AllCh1");
        for name in names.get(2..).ok_or("names[2..] missing")? {
            assert!(name.is_empty(), "expected empty name, got {name:?}");
        }
        Ok(())
    }
}
