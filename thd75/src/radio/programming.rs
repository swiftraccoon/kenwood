//! Programming mode access for full radio memory read/write.
//!
//! The TH-D75 stores all radio configuration in a 500,480-byte flash
//! memory (1,955 pages of 256 bytes), accessible only via the binary
//! programming protocol (`0M PROGRAM`). This module provides methods to
//! read and write individual pages, memory regions, or the entire image.
//!
//! # Protocol
//!
//! The entire programming session runs at 9600 baud with no transfer-phase
//! baud-rate switch. This is the only hardware-qualified path. Switching to
//! 57600 baud after entry crashes the radio into MCP error mode, and faster
//! transfer modes are not exposed without equivalent qualification.
//!
//! # Warning
//!
//! Entering programming mode makes the radio stop responding to normal
//! CAT commands. The display shows "PROG MCP". An explicit session entered
//! with [`Radio::enter_mcp`] must be closed with [`McpSession::exit`]. If the
//! session or an exchange future is dropped, issue no further CAT command and
//! call [`Radio::recover_from_interrupted_mcp`]. Recovery sends the MCP exit
//! byte only from a proved quiescent frame boundary; an ambiguous partial
//! exchange is closed fail-safe and requires a radio power cycle. One-shot
//! high-level methods enforce the same boundary rule on ordinary errors.
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
//! [`Error::McpWriteProtected`].

use crate::error::{Error, ProtocolError, TransportError};
use crate::protocol::programming;
use crate::transport::Transport;
use crate::types::{
    ChannelDisplayName, RegularChannel, StoredChannelData, StoredChannelFlag, StoredChannelSlot,
};

use super::{McpPhase, McpWireBoundary, Radio};

pub use crate::protocol::programming::{McpPage, WritableMcpPage};

/// Baud rate for the programming mode handshake.
///
/// The `0M PROGRAM\r` entry command is always sent at 9600 baud.
/// The binary transfer phase also remains at this baud rate.
const PROGRAMMING_BAUD: u32 = 9600;

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

/// Flag-table pages corresponding exactly to the 1,152 data-backed slots.
const DATA_BACKED_CHANNEL_FLAG_PAGE_COUNT: u16 = 18;

/// Outcome of a conditional detached MCP page update.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DetachedMcpPageUpdate {
    /// Every requested page already held the requested bytes; no write
    /// occurred and CAT was restored and identified before returning.
    UnchangedCatReady,
    /// At least one verified write occurred and the radio is rebooting; the
    /// caller owns transport recovery and must not treat the current handle as
    /// CAT-ready.
    ChangedRadioRebooting,
}

/// Failure of a sparse detached MCP update, with page-write progress.
///
/// A page enters `possibly_written_pages` before its wire write is polled and
/// enters `verified_written_pages` only after immediate read-back succeeds.
/// This distinction lets a recovery path report partial multi-page outcomes
/// without pretending an acknowledged or interrupted write was atomic.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum DetachedMcpPageUpdateError {
    /// No page was supplied, so no CAT proof or programming transition ran.
    #[error("detached MCP update requires at least one writable page")]
    EmptyPageSet,
    /// MCP entry failed before any page exchange.
    #[error("entering MCP for detached page update failed: {source}")]
    Entry {
        /// Underlying entry failure.
        #[source]
        source: Box<Error>,
    },
    /// A page operation failed; cleanup succeeded.
    #[error(
        "detached MCP page update failed; possibly written pages: \
         {possibly_written_pages:?}; verified pages: {verified_written_pages:?}: {source}"
    )]
    Operation {
        /// Pages whose write may have started, in ascending order.
        possibly_written_pages: Vec<WritableMcpPage>,
        /// Pages whose write and immediate read-back both succeeded.
        verified_written_pages: Vec<WritableMcpPage>,
        /// Underlying page failure.
        #[source]
        source: Box<Error>,
    },
    /// Page operations succeeded, but the selected exit path failed.
    #[error(
        "detached MCP page update cleanup failed; possibly written pages: \
         {possibly_written_pages:?}; verified pages: {verified_written_pages:?}: {source}"
    )]
    Cleanup {
        /// Pages whose write may have started, in ascending order.
        possibly_written_pages: Vec<WritableMcpPage>,
        /// Pages whose write and immediate read-back both succeeded.
        verified_written_pages: Vec<WritableMcpPage>,
        /// Underlying cleanup failure.
        #[source]
        source: Box<Error>,
    },
    /// Both a page operation and cleanup failed.
    #[error(
        "detached MCP page update failed ({operation}); possibly written pages: \
         {possibly_written_pages:?}; verified pages: {verified_written_pages:?}; cleanup also \
         failed: {cleanup}"
    )]
    OperationAndCleanup {
        /// Pages whose write may have started, in ascending order.
        possibly_written_pages: Vec<WritableMcpPage>,
        /// Pages whose write and immediate read-back both succeeded.
        verified_written_pages: Vec<WritableMcpPage>,
        /// Underlying page failure.
        operation: Box<Error>,
        /// Underlying cleanup failure.
        #[source]
        cleanup: Box<Error>,
    },
}

impl DetachedMcpPageUpdateError {
    /// Pages whose wire write may have started, in ascending order.
    #[must_use]
    pub fn possibly_written_pages(&self) -> &[WritableMcpPage] {
        match self {
            Self::EmptyPageSet | Self::Entry { .. } => &[],
            Self::Operation {
                possibly_written_pages,
                ..
            }
            | Self::Cleanup {
                possibly_written_pages,
                ..
            }
            | Self::OperationAndCleanup {
                possibly_written_pages,
                ..
            } => possibly_written_pages,
        }
    }

    /// Pages whose write and immediate read-back both succeeded.
    #[must_use]
    pub fn verified_written_pages(&self) -> &[WritableMcpPage] {
        match self {
            Self::EmptyPageSet | Self::Entry { .. } => &[],
            Self::Operation {
                verified_written_pages,
                ..
            }
            | Self::Cleanup {
                verified_written_pages,
                ..
            }
            | Self::OperationAndCleanup {
                verified_written_pages,
                ..
            } => verified_written_pages,
        }
    }

    /// Whether this structured failure contains a lost physical link.
    pub(crate) fn is_link_lost(&self) -> bool {
        match self {
            Self::EmptyPageSet => false,
            Self::Entry { source }
            | Self::Operation { source, .. }
            | Self::Cleanup { source, .. } => source.is_link_lost(),
            Self::OperationAndCleanup {
                operation, cleanup, ..
            } => operation.is_link_lost() || cleanup.is_link_lost(),
        }
    }

    /// Whether this structured failure contains a poisoned protocol boundary.
    pub(crate) fn requires_recovery(&self) -> bool {
        match self {
            Self::EmptyPageSet => false,
            Self::Entry { source }
            | Self::Operation { source, .. }
            | Self::Cleanup { source, .. } => source.requires_recovery(),
            Self::OperationAndCleanup {
                operation, cleanup, ..
            } => operation.requires_recovery() || cleanup.requires_recovery(),
        }
    }
}

/// One page in an ordered, compare-and-exchange MCP transaction.
///
/// The transaction reads the live page and requires it to match `expected`
/// byte-for-byte before it can write `replacement`. Keeping both complete
/// pages in the request makes the optimistic-concurrency guard explicit and
/// gives callers the exact bytes needed for a later compensating transaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpPageExchange {
    page: WritableMcpPage,
    expected: [u8; programming::PAGE_SIZE],
    replacement: [u8; programming::PAGE_SIZE],
}

impl McpPageExchange {
    /// Construct one guarded page replacement.
    #[must_use]
    pub const fn new(
        page: WritableMcpPage,
        expected: [u8; programming::PAGE_SIZE],
        replacement: [u8; programming::PAGE_SIZE],
    ) -> Self {
        Self {
            page,
            expected,
            replacement,
        }
    }

    /// MCP page address.
    #[must_use]
    pub const fn page(&self) -> WritableMcpPage {
        self.page
    }

    /// Exact live bytes required before any page in the transaction is written.
    #[must_use]
    pub const fn expected(&self) -> &[u8; programming::PAGE_SIZE] {
        &self.expected
    }

    /// Bytes to write after every expected page has matched.
    #[must_use]
    pub const fn replacement(&self) -> &[u8; programming::PAGE_SIZE] {
        &self.replacement
    }
}

/// The in-session operation that stopped an MCP page exchange.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum McpPageExchangeOperationError {
    /// A page could not be read during the compare phase.
    #[error("could not read MCP page 0x{page:04X} for comparison: {source}")]
    Read {
        /// Page whose read failed.
        page: WritableMcpPage,
        /// Underlying MCP or transport failure.
        #[source]
        source: Box<Error>,
    },

    /// A live page differed from the caller's expected snapshot.
    #[error(
        "MCP compare mismatch on page 0x{page:04X} at offset 0x{offset:02X}: \
         expected 0x{expected:02X}, found 0x{actual:02X}"
    )]
    CompareMismatch {
        /// Page containing the first mismatch.
        page: WritableMcpPage,
        /// First differing byte offset within the page.
        offset: usize,
        /// Caller-supplied expected byte.
        expected: u8,
        /// Byte read from the radio.
        actual: u8,
    },

    /// A page write or its immediate read-back verification failed.
    #[error("verified write of MCP page 0x{page:04X} failed: {source}")]
    Write {
        /// Page whose write may have reached the radio.
        page: WritableMcpPage,
        /// Underlying write or read-back failure.
        #[source]
        source: Box<Error>,
    },
}

impl McpPageExchangeOperationError {
    /// Page on which the transaction stopped.
    #[must_use]
    pub const fn page(&self) -> WritableMcpPage {
        match self {
            Self::Read { page, .. }
            | Self::CompareMismatch { page, .. }
            | Self::Write { page, .. } => *page,
        }
    }

    fn is_link_lost(&self) -> bool {
        match self {
            Self::Read { source, .. } | Self::Write { source, .. } => source.is_link_lost(),
            Self::CompareMismatch { .. } => false,
        }
    }

    fn requires_recovery(&self) -> bool {
        match self {
            Self::Read { source, .. } | Self::Write { source, .. } => source.requires_recovery(),
            Self::CompareMismatch { .. } => false,
        }
    }
}

/// A rejected or failed ordered MCP page compare-and-exchange transaction.
///
/// For operation and cleanup failures, [`Self::possibly_written_pages`]
/// returns every page whose write was started, in caller order. The final page
/// may not have been verified (or even received by the radio), so a
/// compensating restore should conservatively compare and restore every page
/// in that list.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum McpPageExchangeError {
    /// The request named one page more than once.
    #[error("duplicate MCP compare-and-exchange page 0x{page:04X}")]
    DuplicatePage {
        /// Duplicated page address.
        page: WritableMcpPage,
    },

    /// Programming mode could not be entered.
    #[error("could not enter MCP mode for compare-and-exchange: {source}")]
    Entry {
        /// Entry failure, including any entry-path cleanup failure.
        #[source]
        source: Box<Error>,
    },

    /// The page operation failed, but MCP cleanup succeeded.
    #[error(
        "MCP compare-and-exchange failed after writes may have started for \
         {possibly_written_pages:?}: {operation}"
    )]
    Operation {
        /// Pages whose writes may have started, in caller order.
        possibly_written_pages: Vec<WritableMcpPage>,
        /// Read, comparison, write, or read-back failure.
        #[source]
        operation: Box<McpPageExchangeOperationError>,
    },

    /// Every page operation succeeded, but leaving MCP mode failed.
    #[error(
        "MCP compare-and-exchange cleanup failed after writes to \
         {possibly_written_pages:?}: {cleanup}"
    )]
    Cleanup {
        /// Successfully written and verified pages, in caller order.
        possibly_written_pages: Vec<WritableMcpPage>,
        /// MCP exit or CAT-restoration failure.
        #[source]
        cleanup: Box<Error>,
    },

    /// Both the page operation and MCP cleanup failed.
    #[error(
        "MCP compare-and-exchange failed after writes may have started for \
         {possibly_written_pages:?}: {operation}; cleanup also failed: {cleanup}"
    )]
    OperationAndCleanup {
        /// Pages whose writes may have started, in caller order.
        possibly_written_pages: Vec<WritableMcpPage>,
        /// Read, comparison, write, or read-back failure.
        operation: Box<McpPageExchangeOperationError>,
        /// MCP exit or CAT-restoration failure.
        #[source]
        cleanup: Box<Error>,
    },
}

impl McpPageExchangeError {
    /// Pages whose writes may have started, preserving caller order.
    ///
    /// The slice is empty for preflight, entry, read, and compare failures.
    #[must_use]
    pub fn possibly_written_pages(&self) -> &[WritableMcpPage] {
        match self {
            Self::Operation {
                possibly_written_pages,
                ..
            }
            | Self::Cleanup {
                possibly_written_pages,
                ..
            }
            | Self::OperationAndCleanup {
                possibly_written_pages,
                ..
            } => possibly_written_pages,
            Self::DuplicatePage { .. } | Self::Entry { .. } => &[],
        }
    }

    /// Whether this transaction failure contains a lost physical link.
    pub(crate) fn is_link_lost(&self) -> bool {
        match self {
            Self::DuplicatePage { .. } => false,
            Self::Entry { source } => source.is_link_lost(),
            Self::Operation { operation, .. } => operation.is_link_lost(),
            Self::Cleanup { cleanup, .. } => cleanup.is_link_lost(),
            Self::OperationAndCleanup {
                operation, cleanup, ..
            } => operation.is_link_lost() || cleanup.is_link_lost(),
        }
    }

    /// Whether this transaction failure contains a poisoned protocol boundary.
    pub(crate) fn requires_recovery(&self) -> bool {
        match self {
            Self::DuplicatePage { .. } => false,
            Self::Entry { source } => source.requires_recovery(),
            Self::Operation { operation, .. } => operation.requires_recovery(),
            Self::Cleanup { cleanup, .. } => cleanup.requires_recovery(),
            Self::OperationAndCleanup {
                operation, cleanup, ..
            } => operation.requires_recovery() || cleanup.requires_recovery(),
        }
    }
}

/// Exclusive access to an active MCP programming-mode connection.
///
/// The session borrows its [`Radio`] mutably, so ordinary CAT commands cannot
/// be issued until [`exit`](Self::exit) completes or the session is dropped.
/// Page reads accept any physical [`McpPage`], while writes require a
/// [`WritableMcpPage`] and therefore cannot target factory calibration.
///
/// Dropping the session does not perform I/O. It deliberately leaves the
/// radio's MCP state poisoned because an asynchronous exit cannot be made
/// reliable from `Drop`. Call
/// [`Radio::recover_from_interrupted_mcp`] before using CAT again.
#[must_use = "an active MCP session must be exited or explicitly recovered"]
pub struct McpSession<'a, T: Transport> {
    radio: &'a mut Radio<T>,
}

impl<T: Transport> std::fmt::Debug for McpSession<'_, T> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("McpSession").finish_non_exhaustive()
    }
}

impl<T: Transport> McpSession<'_, T> {
    /// Read one physical 256-byte page without leaving MCP mode.
    ///
    /// # Errors
    ///
    /// Returns an error if the binary page exchange or its ACK handshake
    /// fails. The session remains active and must still be exited or recovered.
    pub async fn read_page(
        &mut self,
        page: McpPage,
    ) -> Result<[u8; programming::PAGE_SIZE], Error> {
        self.radio.read_single_page(page).await
    }

    /// Write one configuration page and verify it by immediate read-back.
    ///
    /// # Errors
    ///
    /// Returns an error if the write, ACK, read-back, or byte comparison fails.
    /// Factory-calibration pages cannot be supplied to this method.
    pub async fn write_page(
        &mut self,
        page: WritableMcpPage,
        data: &[u8; programming::PAGE_SIZE],
    ) -> Result<(), Error> {
        self.radio.write_single_page(page, data).await
    }

    /// Write one configuration page without read-back verification.
    ///
    /// Prefer [`write_page`](Self::write_page). This lower-level operation is
    /// intended for flows that perform their own verification before exit.
    ///
    /// # Errors
    ///
    /// Returns an error if the write or ACK exchange fails.
    pub async fn write_page_unverified(
        &mut self,
        page: WritableMcpPage,
        data: &[u8; programming::PAGE_SIZE],
    ) -> Result<(), Error> {
        self.radio.write_single_page_unverified(page, data).await
    }

    /// Exit MCP mode, wait for re-enumeration, and prove CAT identity.
    ///
    /// # Errors
    ///
    /// Returns an error if the exit, reconnect, or CAT identity proof fails.
    /// A cancelled or failed exit retains the radio's poison state for
    /// [`Radio::recover_from_interrupted_mcp`].
    pub async fn exit(self) -> Result<(), Error> {
        self.radio.exit_programming_mode().await
    }
}

/// Timeout for a full memory dump.
///
/// At 9600 baud: 1955 pages x 261 bytes x 10 bits/byte / 9600 bps ~ 53 s.
/// The 120-second ceiling leaves ample margin for the qualified transfer.
const FULL_DUMP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

impl<T: Transport> Radio<T> {
    /// Enter MCP programming mode and borrow this radio exclusively.
    ///
    /// The returned [`McpSession`] is the only public surface for performing
    /// multiple page operations inside one programming-mode transition. Its
    /// mutable borrow makes interleaved CAT traffic fail to compile:
    ///
    /// ```compile_fail
    /// use kenwood_thd75::{Error, Radio, Transport};
    ///
    /// async fn interleave<T: Transport>(radio: &mut Radio<T>) -> Result<(), Error> {
    ///     let session = radio.enter_mcp().await?;
    ///     radio.identify().await?;
    ///     session.exit().await?;
    ///     Ok(())
    /// }
    /// ```
    ///
    /// Call [`McpSession::exit`] to leave MCP mode and prove that the reopened
    /// connection speaks CAT. If the session or its exit future is dropped,
    /// call [`Self::recover_from_interrupted_mcp`] before any CAT operation.
    ///
    /// # Errors
    ///
    /// Returns an error if CAT is not in a proved boundary, an earlier MCP
    /// session still needs recovery, or the radio rejects the MCP entry.
    pub async fn enter_mcp(&mut self) -> Result<McpSession<'_, T>, Error> {
        self.enter_programming_mode().await?;
        Ok(McpSession { radio: self })
    }

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
    /// Returns an error if entry, any page read, or cleanup fails. Cleanup
    /// sends the MCP exit byte only from a proved exchange boundary; otherwise
    /// it closes the transport and requires a radio power cycle.
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
    /// Returns an error if entry, any page read, or cleanup fails. Cleanup
    /// sends the MCP exit byte only from a proved exchange boundary; otherwise
    /// it closes the transport and requires a radio power cycle.
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
            .read_pages_raw(McpPage::new(0)?, programming::TOTAL_PAGES, &mut on_progress)
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
    /// Returns [`Error::McpInvalidImageSize`] if the image is the wrong size.
    /// Returns an error if entry, any page write, or cleanup fails. Cleanup
    /// sends the MCP exit byte only from a proved exchange boundary; otherwise
    /// it closes the transport and requires a radio power cycle.
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
    /// Returns [`Error::McpInvalidImageSize`] if the image is the wrong size.
    /// Returns an error if entry, any page write, or cleanup fails. Cleanup
    /// sends the MCP exit byte only from a proved exchange boundary; otherwise
    /// it closes the transport and requires a radio power cycle.
    pub async fn write_memory_image_with_progress<F>(
        &mut self,
        image: &[u8],
        mut on_progress: F,
    ) -> Result<(), Error>
    where
        F: FnMut(u16, u16),
    {
        if image.len() != programming::TOTAL_SIZE {
            return Err(Error::McpInvalidImageSize {
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
        let writable_slice = image
            .get(..writable_bytes)
            .ok_or(Error::McpInvalidImageSize {
                actual: image.len(),
                expected: writable_bytes,
            })?;
        let result = self
            .write_pages_raw(WritableMcpPage::new(0)?, writable_slice, &mut on_progress)
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
    /// error if entry, any page read, or cleanup fails. Cleanup sends the MCP
    /// exit byte only from a proved exchange boundary; otherwise it closes the
    /// transport and requires a radio power cycle.
    pub async fn read_memory_pages(
        &mut self,
        start_page: McpPage,
        count: u16,
    ) -> Result<Vec<u8>, Error> {
        Self::validate_mcp_page_range(start_page.as_raw(), count)?;
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
    /// Returns an error if entry, any page read, or cleanup fails. Cleanup
    /// sends the MCP exit byte only from a proved exchange boundary; otherwise
    /// it closes the transport and requires a radio power cycle.
    pub async fn read_sparse_memory_pages(
        &mut self,
        pages: &[McpPage],
    ) -> Result<Vec<(McpPage, [u8; programming::PAGE_SIZE])>, Error> {
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
    /// Returns an error if entry, any page read, or cleanup fails. Cleanup
    /// sends the MCP exit byte only from a proved exchange boundary; otherwise
    /// it closes the transport and requires a radio power cycle.
    pub async fn read_sparse_memory_pages_with_progress<F>(
        &mut self,
        pages: &[McpPage],
        mut on_progress: F,
    ) -> Result<Vec<(McpPage, [u8; programming::PAGE_SIZE])>, Error>
    where
        F: FnMut(u16, u16),
    {
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

        let result: Result<Vec<(McpPage, [u8; programming::PAGE_SIZE])>, Error> = async {
            let mut page_data = Vec::with_capacity(pages.len());
            for (completed, page) in (1u16..=total).zip(pages) {
                let data = self.read_single_page(page).await?;
                page_data.push((page, data));
                on_progress(completed, total);
            }
            Ok(page_data)
        }
        .await;

        // Attempt cleanup after a successful entry. The cleanup path sends E
        // only if the failed operation left a proved exchange boundary.
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
    /// Returns [`Error::McpWriteProtected`] if any target page falls
    /// within the factory calibration region.
    /// Returns [`Error::McpInvalidImageSize`] before any I/O if `data` is not
    /// page-aligned, and [`Error::McpPageOutOfRange`] if the complete target
    /// range is not inside the radio's memory image.
    /// Returns an error if entry, any page write, or cleanup fails. Cleanup
    /// sends the MCP exit byte only from a proved exchange boundary; otherwise
    /// it closes the transport and requires a radio power cycle.
    pub async fn write_memory_pages(
        &mut self,
        start_page: WritableMcpPage,
        data: &[u8],
    ) -> Result<(), Error> {
        if data.is_empty() {
            return Ok(());
        }
        if !data.len().is_multiple_of(programming::PAGE_SIZE) {
            return Err(Error::McpInvalidImageSize {
                actual: data.len(),
                expected: data.len().next_multiple_of(programming::PAGE_SIZE),
            });
        }
        let page_count = u16::try_from(data.len() / programming::PAGE_SIZE).map_err(|_| {
            Error::McpInvalidImageSize {
                actual: data.len(),
                expected: programming::PAGE_SIZE * usize::from(u16::MAX),
            }
        })?;
        Self::validate_mcp_page_range(start_page.as_raw(), page_count)?;

        // The complete range is now known to be in bounds, so checked
        // arithmetic above guarantees this last-page calculation.
        let last_page = start_page.as_raw() + (page_count - 1);
        if last_page > programming::MAX_WRITABLE_PAGE {
            let first_protected = start_page.as_raw().max(programming::MAX_WRITABLE_PAGE + 1);
            return Err(Error::McpWriteProtected {
                page: McpPage::new(first_protected)?,
            });
        }

        self.enter_programming_mode().await?;

        let result = self.write_pages_raw(start_page, data, &mut |_, _| {}).await;

        let exit_result = self.exit_programming_mode().await;

        self.finish_mcp_operation(result, exit_result)
    }

    /// Compare and conditionally replace sparse MCP pages in caller order.
    ///
    /// This is the guarded primitive for temporary multi-page state changes.
    /// Before entering programming mode, it rejects out-of-range, protected,
    /// and duplicate pages. Inside one MCP session it then reads **every**
    /// requested page, compares every live page byte-for-byte with its
    /// [`McpPageExchange::expected`] snapshot, and only then starts writing.
    /// Changed replacements are written and immediately read-back verified in
    /// exactly the order supplied by the caller; unchanged replacements are
    /// still compared but are not written. The returned page numbers are the
    /// changed pages successfully written, in caller order.
    ///
    /// An empty request is a no-op and does not enter programming mode.
    ///
    /// # Atomicity and restoration
    ///
    /// The compare phase is all-or-nothing, but the TH-D75 has no atomic
    /// multi-page write operation. A later write or verification can fail
    /// after earlier pages changed. On error,
    /// [`McpPageExchangeError::possibly_written_pages`] identifies, in caller
    /// order, every page whose write may have started. Its last page is not
    /// necessarily verified. Callers performing a temporary change should
    /// retain the request's complete expected pages and conservatively restore
    /// every page in that list.
    ///
    /// # Errors
    ///
    /// Returns [`McpPageExchangeError::DuplicatePage`] before any I/O. A
    /// live-byte mismatch is reported only after all requested pages have been
    /// read and causes zero writes. After a successful MCP entry, exit is
    /// always attempted, including read, mismatch, write, and verification
    /// failures; a simultaneous operation and cleanup failure retains both
    /// errors.
    pub async fn compare_exchange_memory_pages(
        &mut self,
        exchanges: &[McpPageExchange],
    ) -> Result<Vec<WritableMcpPage>, McpPageExchangeError> {
        let mut distinct_pages = std::collections::HashSet::with_capacity(exchanges.len());
        for exchange in exchanges {
            let page = exchange.page;
            if !distinct_pages.insert(page) {
                return Err(McpPageExchangeError::DuplicatePage { page });
            }
        }

        if exchanges.is_empty() {
            return Ok(Vec::new());
        }

        self.enter_programming_mode()
            .await
            .map_err(|source| McpPageExchangeError::Entry {
                source: Box::new(source),
            })?;

        let mut possibly_written_pages = Vec::with_capacity(exchanges.len());
        let operation: Result<Vec<WritableMcpPage>, McpPageExchangeOperationError> = async {
            // Complete every read before checking expectations. A stale page
            // anywhere in the request therefore gates every write.
            let mut live_pages = Vec::with_capacity(exchanges.len());
            for exchange in exchanges {
                let page = exchange.page;
                let live = self.read_single_page(page.page()).await.map_err(|source| {
                    McpPageExchangeOperationError::Read {
                        page,
                        source: Box::new(source),
                    }
                })?;
                live_pages.push(live);
            }

            for (exchange, live) in exchanges.iter().zip(&live_pages) {
                if let Some((offset, (&expected, &actual))) = exchange
                    .expected
                    .iter()
                    .zip(live.iter())
                    .enumerate()
                    .find(|(_, (expected, actual))| expected != actual)
                {
                    return Err(McpPageExchangeOperationError::CompareMismatch {
                        page: exchange.page,
                        offset,
                        expected,
                        actual,
                    });
                }
            }

            for exchange in exchanges {
                if exchange.expected == exchange.replacement {
                    continue;
                }

                // Record the page before polling the write. Any returned
                // failure can mean the W frame reached the radio even when
                // its ACK or verification did not complete.
                possibly_written_pages.push(exchange.page);
                self.write_single_page(exchange.page, &exchange.replacement)
                    .await
                    .map_err(|source| McpPageExchangeOperationError::Write {
                        page: exchange.page,
                        source: Box::new(source),
                    })?;
            }

            Ok(possibly_written_pages.clone())
        }
        .await;

        // Successful entry has one unconditional exit path, regardless of
        // which read, comparison, write, or read-back operation failed.
        let cleanup = self.exit_programming_mode().await;

        match (operation, cleanup) {
            (Ok(written_pages), Ok(())) => Ok(written_pages),
            (Err(operation), Ok(())) => Err(McpPageExchangeError::Operation {
                possibly_written_pages,
                operation: Box::new(operation),
            }),
            (Ok(_), Err(cleanup)) => Err(McpPageExchangeError::Cleanup {
                possibly_written_pages,
                cleanup: Box::new(cleanup),
            }),
            (Err(operation), Err(cleanup)) => Err(McpPageExchangeError::OperationAndCleanup {
                possibly_written_pages,
                operation: Box::new(operation),
                cleanup: Box::new(cleanup),
            }),
        }
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
    /// Returns an error if entry, the page read, or cleanup fails. Cleanup
    /// sends the MCP exit byte only from a proved exchange boundary; otherwise
    /// it closes the transport and requires a radio power cycle.
    pub async fn read_page(
        &mut self,
        page: McpPage,
    ) -> Result<[u8; programming::PAGE_SIZE], Error> {
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
    /// Returns an error if entry, the page write, or cleanup fails. Cleanup
    /// sends the MCP exit byte only from a proved exchange boundary; otherwise
    /// it closes the transport and requires a radio power cycle.
    pub async fn write_page(
        &mut self,
        page: WritableMcpPage,
        data: &[u8; programming::PAGE_SIZE],
    ) -> Result<(), Error> {
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
    /// Returns an error if entry, the page write, or cleanup fails. Cleanup
    /// sends the MCP exit byte only from a proved exchange boundary; otherwise
    /// it closes the transport and requires a radio power cycle.
    pub async fn write_page_unverified(
        &mut self,
        page: WritableMcpPage,
        data: &[u8; programming::PAGE_SIZE],
    ) -> Result<(), Error> {
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
    /// Returns an error if entry, any page read, a changed-page write or
    /// verification, or cleanup fails. Cleanup sends the MCP exit byte only
    /// from a proved exchange boundary; otherwise it closes the transport and
    /// requires a radio power cycle. A failed read cannot change the radio,
    /// but if a write or its verification fails partway through the batch,
    /// pages written earlier in the same session remain changed; the error
    /// identifies only the failing page.
    pub async fn modify_memory_pages<F>(
        &mut self,
        pages: &[WritableMcpPage],
        mut modify: F,
    ) -> Result<Vec<WritableMcpPage>, Error>
    where
        F: FnMut(WritableMcpPage, &mut [u8; programming::PAGE_SIZE]),
    {
        let pages: std::collections::BTreeSet<WritableMcpPage> = pages.iter().copied().collect();

        if pages.is_empty() {
            return Ok(Vec::new());
        }

        self.enter_programming_mode().await?;

        let result: Result<Vec<WritableMcpPage>, Error> = async {
            // Read every requested page before running the patch callback or
            // writing anything. A failed read therefore cannot leave a
            // partially patched set of pages on the radio.
            let mut page_data = Vec::with_capacity(pages.len());
            for page in pages {
                let original = self.read_single_page(page.page()).await?;
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

        // Attempt cleanup after a successful entry. The cleanup path sends E
        // only if the failed operation left a proved exchange boundary.
        let exit_result = self.exit_programming_mode().await;

        self.finish_mcp_operation(result, exit_result)
    }

    /// Selectively modify sparse pages and detach only when a write occurred.
    ///
    /// `pages` may be unordered and may contain duplicates. Each distinct
    /// page is read exactly once before `modify` is called for any page. The
    /// callback then receives each page in ascending order. Only pages whose
    /// contents actually changed are written, in ascending order, and every
    /// write is verified by immediate read-back.
    ///
    /// An empty page list is rejected before I/O because no operation ran that
    /// could truthfully return a CAT-ready connection proof.
    ///
    /// # Connection lifetime
    ///
    /// If at least one page changed, the detached exit path returns
    /// [`DetachedMcpPageUpdate::ChangedRadioRebooting`] and leaves transport
    /// recovery to the caller while the new settings take effect. If no page
    /// changed, the normal exit path reconnects, proves CAT identity, and
    /// returns [`DetachedMcpPageUpdate::UnchangedCatReady`].
    ///
    /// # Errors
    ///
    /// Returns [`DetachedMcpPageUpdateError::EmptyPageSet`] before I/O for an
    /// empty request, or a structured entry, page-I/O, verification, or cleanup
    /// failure. After an operation failure, cleanup exits and reconnects only
    /// when the binary stream is at a proved exchange boundary. An ambiguous
    /// partial command or handshake is closed without another protocol byte
    /// and requires a radio power cycle. The error retains pages whose writes
    /// may have started and the stricter subset whose read-back succeeded. A
    /// failed read cannot change the radio because all reads finish before any
    /// callback or write.
    pub async fn modify_memory_pages_detached_if_changed<F>(
        &mut self,
        pages: &[WritableMcpPage],
        mut modify: F,
    ) -> Result<DetachedMcpPageUpdate, DetachedMcpPageUpdateError>
    where
        F: FnMut(WritableMcpPage, &mut [u8; programming::PAGE_SIZE]),
    {
        let pages: std::collections::BTreeSet<WritableMcpPage> = pages.iter().copied().collect();

        if pages.is_empty() {
            return Err(DetachedMcpPageUpdateError::EmptyPageSet);
        }

        self.enter_programming_mode().await.map_err(|source| {
            DetachedMcpPageUpdateError::Entry {
                source: Box::new(source),
            }
        })?;

        let mut possibly_written_pages = Vec::with_capacity(pages.len());
        let mut verified_written_pages = Vec::with_capacity(pages.len());
        let result: Result<bool, Error> = async {
            // Complete every read before exposing bytes to the callback or
            // starting a write. A read failure therefore cannot leave a
            // partially changed set of pages on the radio.
            let mut page_data = Vec::with_capacity(pages.len());
            for page in pages {
                let original = self.read_single_page(page.page()).await?;
                page_data.push((page, original, original));
            }

            for (page, _, modified) in &mut page_data {
                modify(*page, modified);
            }

            let mut changed = false;
            for (page, original, modified) in &page_data {
                if original != modified {
                    // Record before polling the write. Cancellation or a
                    // missing ACK can mean some or all bytes reached the
                    // radio even though verification did not finish.
                    possibly_written_pages.push(*page);
                    self.write_single_page(*page, modified).await?;
                    verified_written_pages.push(*page);
                    changed = true;
                }
            }

            Ok(changed)
        }
        .await;

        let exit_result = if matches!(&result, Ok(true)) {
            self.exit_programming_mode_detached().await
        } else {
            self.exit_programming_mode().await
        };

        match (result, exit_result) {
            (Ok(changed), Ok(())) => Ok(if changed {
                DetachedMcpPageUpdate::ChangedRadioRebooting
            } else {
                DetachedMcpPageUpdate::UnchangedCatReady
            }),
            (Err(source), Ok(())) => Err(DetachedMcpPageUpdateError::Operation {
                possibly_written_pages,
                verified_written_pages,
                source: Box::new(source),
            }),
            (Ok(_), Err(source)) => Err(DetachedMcpPageUpdateError::Cleanup {
                possibly_written_pages,
                verified_written_pages,
                source: Box::new(source),
            }),
            (Err(operation), Err(cleanup)) => {
                Err(DetachedMcpPageUpdateError::OperationAndCleanup {
                    possibly_written_pages,
                    verified_written_pages,
                    operation: Box::new(operation),
                    cleanup: Box::new(cleanup),
                })
            }
        }
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
    /// Returns an error if entry, the page read, the page write, or cleanup
    /// fails. Cleanup sends the MCP exit byte only from a proved exchange
    /// boundary; otherwise it closes the transport and requires a radio power
    /// cycle.
    pub async fn modify_memory_page<F>(
        &mut self,
        page: WritableMcpPage,
        modify: F,
    ) -> Result<(), Error>
    where
        F: FnOnce(&mut [u8; programming::PAGE_SIZE]),
    {
        self.enter_programming_mode().await?;

        let result: Result<(), Error> = async {
            // Read the current page contents.
            let mut page_data = self.read_single_page(page.page()).await?;

            // Apply the caller's modifications in place.
            modify(&mut page_data);

            // Write the modified page back.
            self.write_single_page(page, &page_data).await?;

            Ok(())
        }
        .await;

        // Attempt cleanup after the operation. The cleanup path sends E only
        // if the failed operation left a proved exchange boundary.
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
    /// link can reopen in the pre-reboot window and then be interrupted
    /// mid-command when the radio resets its stack. The write is still
    /// verified by read-back inside the session; the connection is
    /// deliberately left dead afterwards and the caller owns recovery
    /// (typically by reconnecting from a fresh process once the radio
    /// finishes rebooting).
    ///
    /// # Errors
    ///
    /// Returns an error if entry, the page read, the page write, or the
    /// exit acknowledgement fails. Exit is always attempted after a
    /// successful entry. A successful page operation uses the detached exit
    /// expected by the caller. Any page-operation error instead uses the
    /// normal reconnect-and-identify exit path, because stale page bytes or
    /// ACKs must not be mistaken for proof of a detached exit. An unproved
    /// exit leaves CAT poisoned for explicit recovery.
    pub async fn modify_memory_page_detached<F>(
        &mut self,
        page: WritableMcpPage,
        modify: F,
    ) -> Result<(), Error>
    where
        F: FnOnce(&mut [u8; programming::PAGE_SIZE]),
    {
        self.enter_programming_mode().await?;

        let result: Result<(), Error> = async {
            let mut page_data = self.read_single_page(page.page()).await?;
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

    /// Selectively modify one page and detach only when a write occurred.
    ///
    /// The page is read once, patched in memory, and compared with its
    /// original contents. If unchanged, no page write is issued and the
    /// normal exit path restores CAT and returns
    /// [`DetachedMcpPageUpdate::UnchangedCatReady`]. If changed,
    /// the page is written with read-back verification and the detached exit
    /// path returns [`DetachedMcpPageUpdate::ChangedRadioRebooting`], leaving
    /// recovery to the caller while the setting takes effect.
    ///
    /// This is intended for persistent mode flags whose changed value causes
    /// a reboot, but whose already-correct value must not incur a redundant
    /// flash write or detached restart.
    ///
    /// # Errors
    ///
    /// Returns an error from programming entry, page I/O, verification, exit,
    /// or unchanged-state CAT restoration. After an operation failure, the normal
    /// reconnect-and-identify cleanup path is used rather than trusting a
    /// possibly stale detached-exit ACK.
    pub async fn modify_memory_page_detached_if_changed<F>(
        &mut self,
        page: WritableMcpPage,
        modify: F,
    ) -> Result<DetachedMcpPageUpdate, Error>
    where
        F: FnOnce(&mut [u8; programming::PAGE_SIZE]),
    {
        self.enter_programming_mode().await?;

        let result: Result<bool, Error> = async {
            let original = self.read_single_page(page.page()).await?;
            let mut modified = original;
            modify(&mut modified);
            if modified == original {
                Ok(false)
            } else {
                self.write_single_page(page, &modified).await?;
                Ok(true)
            }
        }
        .await;

        let exit_result = if matches!(&result, Ok(true)) {
            self.exit_programming_mode_detached().await
        } else {
            self.exit_programming_mode().await
        };

        self.finish_mcp_operation(result, exit_result)
            .map(|changed| {
                if changed {
                    DetachedMcpPageUpdate::ChangedRadioRebooting
                } else {
                    DetachedMcpPageUpdate::UnchangedCatReady
                }
            })
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
    /// Returns exactly 1,000 validated channel names indexed by regular
    /// channel number. Channels without a user-assigned name are returned as
    /// empty [`ChannelDisplayName`] values.
    ///
    /// # Connection lifetime
    ///
    /// Exiting MCP resets USB. This method waits for re-enumeration, reopens
    /// the transport, and proves CAT identity before returning.
    ///
    /// # Errors
    ///
    /// Returns an error if the radio fails to enter programming mode,
    /// if a page read fails, or if the connection is lost. On error, an
    /// attempt is still made to exit programming mode before returning.
    pub async fn read_channel_names(&mut self) -> Result<Vec<ChannelDisplayName>, Error> {
        self.enter_programming_mode().await?;

        let result = self.read_name_pages().await;

        // Always attempt to exit, even if reading failed.
        let exit_result = self.exit_programming_mode().await;

        self.finish_mcp_operation(result, exit_result)
    }

    /// Write a single channel display name via MCP programming mode.
    ///
    /// Enters programming mode, reads the containing name page, modifies
    /// the 16-byte slot for the given channel, writes the page back, and
    /// exits. A full-width 16-byte name occupies the complete slot; shorter
    /// names are NUL-padded.
    ///
    /// # Connection lifetime
    ///
    /// This enters MCP programming mode. Exit resets the USB connection; the
    /// method waits for re-enumeration, reopens it, and proves CAT identity
    /// before returning.
    ///
    /// # Errors
    ///
    /// Returns an error if entering programming mode, reading the page,
    /// writing and verifying the page, restoring CAT mode, or reconnecting
    /// fails.
    pub async fn write_channel_name(
        &mut self,
        channel: RegularChannel,
        name: &ChannelDisplayName,
    ) -> Result<(), Error> {
        let channel_number = channel.as_raw();
        let page = programming::CHANNEL_NAMES_START + (channel_number / 16);
        let page = WritableMcpPage::new(page)?;
        let offset = usize::from(channel_number % 16) * programming::NAME_ENTRY_SIZE;

        tracing::info!(channel = channel_number, name = %name, page = page.as_raw(), offset, "writing channel name via MCP");
        self.modify_memory_page(page, |data| {
            // `offset..offset + NAME_ENTRY_SIZE` is bounded by the page size the
            // closure caller passes. `modify_memory_page` validates
            // `data.len() == PAGE_SIZE` before invoking this closure.
            let Some(slot) = data.get_mut(offset..offset + programming::NAME_ENTRY_SIZE) else {
                return;
            };
            slot.copy_from_slice(&name.to_wire_bytes());
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
    /// Returns an error if entry, any page read, or cleanup fails. Cleanup
    /// sends the MCP exit byte only from a proved exchange boundary; otherwise
    /// it closes the transport and requires a radio power cycle.
    pub async fn read_channel_flags(&mut self) -> Result<Vec<StoredChannelFlag>, Error> {
        self.enter_programming_mode().await?;

        let page_count = programming::CHANNEL_FLAGS_END - programming::CHANNEL_FLAGS_START + 1;
        let result = self
            .read_pages_raw(
                McpPage::new(programming::CHANNEL_FLAGS_START)?,
                page_count,
                &mut |_, _| {},
            )
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
            let flag_bytes = raw
                .get(offset..offset + programming::FLAG_RECORD_SIZE)
                .ok_or_else(|| {
                    Error::Protocol(ProtocolError::FieldParse {
                        command: "MCP channel flags".to_owned(),
                        field: format!("flag record {i}"),
                        detail: "record is missing".to_owned(),
                    })
                })?;
            let flag = programming::parse_channel_flag(flag_bytes).map_err(|error| {
                Error::Protocol(ProtocolError::FieldParse {
                    command: "MCP channel flags".to_owned(),
                    field: format!("flag record {i}"),
                    detail: error.to_string(),
                })
            })?;
            flags.push(flag);
        }

        tracing::info!(count = flags.len(), "channel flags read");
        Ok(flags)
    }

    /// Read all 1,152 physical channel-data slots and their corresponding flags.
    ///
    /// The separate flag and name tables contain 1,200 slots. The final 48
    /// extended slots have no matching 40-byte record and are therefore not
    /// fabricated in this result. Each returned [`StoredChannelSlot`] carries
    /// its zero-based physical index, exact [`StoredChannelFlag`], and
    /// [`StoredChannelData`]. Flag-marked empty records remain uninterpreted
    /// 40-byte values, while programmed records are decoded and validated.
    /// Result index `n` is physical data-record slot `n`, and a successful read
    /// always returns exactly [`programming::CHANNEL_DATA_RECORD_COUNT`]
    /// elements.
    ///
    /// Both regions are read during one MCP session so the radio does not
    /// leave and re-enter programming mode between the flag and data snapshots.
    ///
    /// # Errors
    ///
    /// Returns an error if entry, any page read, or cleanup fails. Cleanup
    /// sends the MCP exit byte only from a proved exchange boundary; otherwise
    /// it closes the transport and requires a radio power cycle.
    pub async fn read_all_channels(&mut self) -> Result<Vec<StoredChannelSlot>, Error> {
        self.enter_programming_mode().await?;

        debug_assert_eq!(
            usize::from(DATA_BACKED_CHANNEL_FLAG_PAGE_COUNT) * programming::PAGE_SIZE,
            programming::CHANNEL_DATA_RECORD_COUNT * programming::FLAG_RECORD_SIZE,
            "the data-backed flag page count must cover exactly 1,152 flags"
        );

        let result = async {
            let flags = self
                .read_pages_raw(
                    McpPage::new(programming::CHANNEL_FLAGS_START)?,
                    DATA_BACKED_CHANNEL_FLAG_PAGE_COUNT,
                    &mut |_, _| {},
                )
                .await?;
            let data_page_count =
                programming::CHANNEL_DATA_END - programming::CHANNEL_DATA_START + 1;
            let data = self
                .read_pages_raw(
                    McpPage::new(programming::CHANNEL_DATA_START)?,
                    data_page_count,
                    &mut |_, _| {},
                )
                .await?;
            Ok((flags, data))
        }
        .await;

        let exit_result = self.exit_programming_mode().await;

        let (raw_flags, raw_data) = self.finish_mcp_operation(result, exit_result)?;

        // Parse memgroups: each 256-byte page is one memgroup containing
        // 6 channel records of 40 bytes + 16 bytes padding.
        let mut slots = Vec::with_capacity(programming::CHANNEL_DATA_RECORD_COUNT);
        for memgroup_idx in 0..programming::MEMGROUP_COUNT {
            let group_offset = memgroup_idx * programming::PAGE_SIZE;
            for group_slot in 0..programming::CHANNELS_PER_MEMGROUP {
                let data_offset = group_offset + group_slot * programming::CHANNEL_RECORD_SIZE;
                let physical_index = memgroup_idx * programming::CHANNELS_PER_MEMGROUP + group_slot;
                let flag_offset = physical_index * programming::FLAG_RECORD_SIZE;
                let flag_bytes = raw_flags
                    .get(flag_offset..flag_offset + programming::FLAG_RECORD_SIZE)
                    .ok_or_else(|| {
                        Error::Protocol(ProtocolError::FieldParse {
                            command: "MCP channel flags".to_owned(),
                            field: format!("flag record {physical_index}"),
                            detail: "record is missing".to_owned(),
                        })
                    })?;
                let flag = programming::parse_channel_flag(flag_bytes).map_err(|error| {
                    Error::Protocol(ProtocolError::FieldParse {
                        command: "MCP channel flags".to_owned(),
                        field: format!("flag record {physical_index}"),
                        detail: error.to_string(),
                    })
                })?;
                let record = raw_data
                    .get(data_offset..data_offset + programming::CHANNEL_RECORD_SIZE)
                    .ok_or_else(|| {
                        Error::Protocol(ProtocolError::FieldParse {
                            command: "MCP channel data".to_owned(),
                            field: format!("channel slot {physical_index}"),
                            detail: "record is missing".to_owned(),
                        })
                    })?;
                let data = StoredChannelData::from_bytes(record, flag).map_err(|error| {
                    // A corrupt record is a real fault in the dump;
                    // substituting a fabricated default would misrepresent
                    // radio state to the caller.
                    Error::Protocol(ProtocolError::FieldParse {
                        command: "MCP channel data".to_owned(),
                        field: format!("channel slot {physical_index}"),
                        detail: error.to_string(),
                    })
                })?;
                slots.push(StoredChannelSlot::new(physical_index, flag, data));
            }
        }

        debug_assert_eq!(
            slots.len(),
            programming::CHANNEL_DATA_RECORD_COUNT,
            "every requested channel slot must be returned"
        );

        tracing::info!(count = slots.len(), "channel memory slots read");
        Ok(slots)
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
    /// Returns an error if the read or MCP cleanup fails. An ambiguous binary
    /// exchange is closed without sending an exit byte and requires a radio
    /// power cycle.
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
    /// Returns an error if the read or MCP cleanup fails. An ambiguous binary
    /// exchange is closed without sending an exit byte and requires a radio
    /// power cycle.
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
    /// Returns an error if the write or MCP cleanup fails. An ambiguous binary
    /// exchange is closed without sending an exit byte and requires a radio
    /// power cycle.
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
    /// Returns an error if the write or MCP cleanup fails. An ambiguous binary
    /// exchange is closed without sending an exit byte and requires a radio
    /// power cycle.
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
        // Programming mode takes exclusive ownership of the byte stream and
        // changes its framing. An ambiguous CAT exchange must be recovered by
        // reconnecting before we drain input or send the entry command.
        self.require_cat_ready()?;

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
        self.drain_stale_input().await?;

        // Switching the host baud is synchronous and cannot have put the
        // radio in MCP mode if it fails.
        self.transport
            .set_baud_rate(PROGRAMMING_BAUD)
            .map_err(Error::Transport)?;

        // Mark the session active BEFORE any wire traffic: if this
        // future is cancelled from here on, the radio may be in (or
        // entering) PROG MCP mode and CAT must refuse until recovery.
        self.mcp_phase = McpPhase::Active;
        self.mcp_wire_boundary = McpWireBoundary::Ambiguous;

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

            tracing::info!("programming mode entered, staying at {PROGRAMMING_BAUD} baud");

            self.mcp_wire_boundary = McpWireBoundary::Quiescent;

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
        if self.mcp_wire_boundary == McpWireBoundary::Ambiguous {
            return self.close_ambiguous_mcp_boundary().await;
        }
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

        if self.mcp_wire_boundary == McpWireBoundary::Ambiguous {
            return self.close_ambiguous_mcp_boundary().await;
        }

        self.send_programming_exit().await?;
        self.settle_after_programming_exit().await;

        // Detached mode deliberately does not prove CAT because the
        // caller expects the link to disappear. The exact ACK is its
        // terminal proof that MCP accepted the one exit byte.
        self.mcp_phase = McpPhase::Inactive;
        self.mcp_wire_boundary = McpWireBoundary::Quiescent;
        Ok(())
    }

    /// Close without writing any protocol byte after a partial MCP exchange.
    async fn close_ambiguous_mcp_boundary(&mut self) -> Result<(), Error> {
        tracing::error!(
            "MCP exchange boundary is ambiguous; closing without sending the exit byte"
        );
        self.desynced = true;
        let _ = self.link_state_tx.send_replace(super::LinkState::Down);
        let boundary = Error::McpWireBoundaryUnproved;
        match self.transport.close().await {
            Ok(()) => Err(boundary),
            Err(close) => Err(Error::McpOperationAndCleanupFailed {
                operation: Box::new(boundary),
                cleanup: Box::new(Error::Transport(close)),
            }),
        }
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
        self.mcp_wire_boundary = McpWireBoundary::Ambiguous;
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

        self.mcp_wire_boundary = McpWireBoundary::Quiescent;

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

    /// Wait for the accepted exit to take effect.
    async fn settle_after_programming_exit(&self) {
        // Give the radio time to leave MCP mode and resume CAT.
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;

        // Stay at 9600 baud. Changing baud rate via SET_LINE_CODING causes the
        // USB CDC connection to drop on some platforms, while CAT commands
        // work at 9600 baud because CDC ACM ignores line coding.
        tracing::info!("programming mode exited, staying at {PROGRAMMING_BAUD} baud");
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
        guard.radio.mcp_wire_boundary = McpWireBoundary::Quiescent;
        let result = guard.radio.restore_state_after_reconnect().await;
        guard.restore_finished = true;
        result
    }

    /// Recover after an MCP programming session's future was cancelled
    /// mid-transfer (e.g. by a caller-side `tokio::time::timeout`).
    ///
    /// Sends the MCP exit byte only when the stream is at a proved quiescent
    /// exchange boundary, restores the saved CAT timeout, and reconnects to
    /// prove normal CAT operation. An ambiguous partial exchange is closed
    /// without sending any more bytes and requires a radio power cycle. If an
    /// earlier future was cancelled after the exit phase began, recovery only
    /// settles and reconnects; it never retransmits `E`. CAT commands refuse with
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
        start_page: McpPage,
        count: u16,
        on_progress: &mut F,
    ) -> Result<Vec<u8>, Error>
    where
        F: FnMut(u16, u16),
    {
        let mut image = Vec::with_capacity(count as usize * programming::PAGE_SIZE);

        for i in 0..count {
            let page = McpPage::new(start_page.as_raw() + i)?;
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
    /// Returns [`Error::McpInvalidImageSize`] if `data.len()` is not a multiple
    /// of [`programming::PAGE_SIZE`] or would exceed `u16::MAX` pages.
    async fn write_pages_raw<F>(
        &mut self,
        start_page: WritableMcpPage,
        data: &[u8],
        on_progress: &mut F,
    ) -> Result<(), Error>
    where
        F: FnMut(u16, u16),
    {
        // Validate up front: `data.len()` must be a whole number of pages and
        // fit in `u16::MAX` pages (the MCP address space limit).
        if !data.len().is_multiple_of(programming::PAGE_SIZE) {
            return Err(Error::McpInvalidImageSize {
                actual: data.len(),
                expected: data.len().next_multiple_of(programming::PAGE_SIZE),
            });
        }
        let page_count = data.len() / programming::PAGE_SIZE;
        let page_count_u16 = u16::try_from(page_count).map_err(|_| Error::McpInvalidImageSize {
            actual: data.len(),
            expected: programming::PAGE_SIZE * usize::from(u16::MAX),
        })?;

        // `chunks_exact` guarantees each chunk is exactly `PAGE_SIZE` bytes, so the
        // conversion to `&[u8; PAGE_SIZE]` is effectively infallible; `map_err`
        // converts the impossible error into an `McpInvalidImageSize` for type
        // purposes rather than using `.expect()`.
        for (i, chunk) in (0u16..page_count_u16).zip(data.chunks_exact(programming::PAGE_SIZE)) {
            let page = WritableMcpPage::new(start_page.as_raw() + i)?;
            let page_data: &[u8; programming::PAGE_SIZE] =
                chunk.try_into().map_err(|_| Error::McpInvalidImageSize {
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
    async fn read_single_page(
        &mut self,
        page: McpPage,
    ) -> Result<[u8; programming::PAGE_SIZE], Error> {
        match self.read_single_page_attempt(page).await {
            Ok(data) => Ok(data),
            Err(e @ Error::McpPageMismatch { .. }) => {
                // `read_single_page_attempt` returns a mismatch only after
                // ACKing the complete W frame and consuming the radio's ACK,
                // so exactly one retry is safe.
                tracing::warn!(page = page.as_raw(), error = %e, "acknowledged wrong-page response; retrying once");
                self.read_single_page_attempt(page).await
            }
            Err(e) => Err(e),
        }
    }

    /// One un-retried page read exchange (R command → W response → ACK).
    async fn read_single_page_attempt(
        &mut self,
        page: McpPage,
    ) -> Result<[u8; programming::PAGE_SIZE], Error> {
        let cmd = programming::build_read_command(page);

        tracing::debug!(page = page.as_raw(), "reading page");

        // Send R command (5 bytes).
        self.mcp_wire_boundary = McpWireBoundary::Ambiguous;
        self.transport.write(&cmd).await.map_err(Error::Transport)?;
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;

        // Read 261-byte W response (W + 4-byte addr + 256-byte data).
        let mut received = Vec::with_capacity(programming::W_RESPONSE_SIZE);
        let mut buf = [0u8; programming::W_RESPONSE_SIZE];
        let result = tokio::time::timeout(self.timeout, async {
            while received.len() < programming::W_RESPONSE_SIZE {
                let remaining = programming::W_RESPONSE_SIZE - received.len();
                let capacity = buf.len();
                let read_target = buf.get_mut(..remaining).ok_or_else(|| {
                    Error::Transport(TransportError::Read(std::io::Error::new(
                        std::io::ErrorKind::InvalidInput,
                        format!(
                            "MCP reader requested {remaining} bytes from a {capacity}-byte buffer"
                        ),
                    )))
                })?;
                let n = self
                    .transport
                    .read(&mut *read_target)
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
                let chunk = read_target.get(..n).ok_or_else(|| {
                    Error::Transport(TransportError::Read(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        format!("transport reported {n} bytes for a {remaining}-byte read buffer"),
                    )))
                })?;
                received.extend_from_slice(chunk);
            }
            Ok(())
        })
        .await
        .map_err(|_| Error::Timeout(self.timeout))?;
        result?;

        // Parse: W(1) + addr(4) + data(256).
        let (answered_page, data) =
            programming::parse_page_read_response(&received).map_err(Error::Protocol)?;
        let page_data = *data;

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
    async fn acknowledge_page_read(&mut self, answered_page: McpPage) -> Result<(), Error> {
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

        self.mcp_wire_boundary = McpWireBoundary::Quiescent;

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
        page: WritableMcpPage,
        data: &[u8; programming::PAGE_SIZE],
    ) -> Result<(), Error> {
        self.write_single_page_unverified(page, data).await?;
        let readback = self.read_single_page(page.page()).await?;
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
        page: WritableMcpPage,
        data: &[u8; programming::PAGE_SIZE],
    ) -> Result<(), Error> {
        let cmd = programming::build_write_command(page, data);

        tracing::debug!(page = page.as_raw(), "writing page");

        // Send W command (261 bytes).
        self.mcp_wire_boundary = McpWireBoundary::Ambiguous;
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
            return Err(Error::McpWriteNotAcknowledged {
                page,
                got: ack_buf[0],
            });
        }

        self.mcp_wire_boundary = McpWireBoundary::Quiescent;

        Ok(())
    }

    // -----------------------------------------------------------------------
    // Internal: channel name page reading
    // -----------------------------------------------------------------------

    /// Read all channel name pages from the radio while in programming mode.
    ///
    /// Iterates over 63 pages starting at [`NAME_START_PAGE`](programming::NAME_START_PAGE),
    /// extracting 16 names per page, and truncating to 1,000 channels.
    async fn read_name_pages(&mut self) -> Result<Vec<ChannelDisplayName>, Error> {
        let mut names = Vec::with_capacity(programming::MAX_CHANNELS);

        for page_offset in 0..programming::NAME_PAGE_COUNT {
            let page = McpPage::new(programming::NAME_START_PAGE + page_offset)?;
            let data = self.read_single_page(page).await?;

            // Extract 16 names from the 256-byte page.
            for i in 0..programming::NAMES_PER_PAGE {
                let start = i * programming::NAME_ENTRY_SIZE;
                let slot = data
                    .get(start..start + programming::NAME_ENTRY_SIZE)
                    .and_then(|bytes| bytes.try_into().ok())
                    .ok_or_else(|| {
                        Error::Protocol(ProtocolError::FieldParse {
                            command: "MCP channel-name page".into(),
                            field: format!("slot {i}"),
                            detail: "16-byte name entry was outside the 256-byte page".into(),
                        })
                    })?;
                names.push(programming::decode_channel_display_name(slot)?);
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
}

#[cfg(test)]
mod tests {
    use super::{
        DATA_BACKED_CHANNEL_FLAG_PAGE_COUNT, DetachedMcpPageUpdate, DetachedMcpPageUpdateError,
        McpPage, McpPageExchange, McpPageExchangeError, McpPageExchangeOperationError,
        WritableMcpPage,
    };
    use crate::error::{Error, ProtocolError, TransportError};
    use crate::protocol::programming;
    use crate::protocol::{Command, Response};
    use crate::radio::{CatState, LinkState, McpPhase, McpWireBoundary, Radio};
    use crate::transport::{MockTransport, Transport};
    use crate::types::{
        Band, ChannelDisplayName, Frequency, MemoryChannelBand, MemoryGroup, RegularChannel,
        StoredChannel,
    };

    type TestResult = Result<(), Box<dyn std::error::Error>>;
    type BoxErr = Box<dyn std::error::Error>;

    fn synthetic_stored_channel(receive_frequency: Frequency) -> StoredChannel {
        let mut wire = [0_u8; StoredChannel::BYTE_SIZE];
        wire[..4].copy_from_slice(&receive_frequency.to_le_bytes());
        StoredChannel::from_bytes(&wire).unwrap_or_else(|error| {
            unreachable!("fixed all-zero synthetic channel record must decode: {error}")
        })
    }

    #[tokio::test]
    async fn programming_entry_rejects_untrusted_cat_boundaries_before_io() -> TestResult {
        for cat_state in [CatState::RecoveryRequired, CatState::BinaryProven] {
            let mut radio = Radio::new(MockTransport::new());
            radio.cat_state = cat_state;

            let result = radio.enter_programming_mode().await;

            assert!(matches!(result, Err(Error::CatRecoveryRequired)));
            assert_eq!(radio.mcp_phase, McpPhase::Inactive);
        }
        Ok(())
    }

    #[tokio::test(start_paused = true)]
    async fn mcp_session_enters_reads_and_exits_with_cat_proof() -> TestResult {
        let page = McpPage::new(0x0010)?;
        let data = [0x5A; programming::PAGE_SIZE];
        let mut mock = MockTransport::new();
        mock.expect(programming::ENTER_PROGRAMMING, programming::ENTER_RESPONSE);
        let read = programming::build_read_command(page);
        mock.expect(&read, &build_w_response(page.as_raw(), &data)?);
        mock.expect(&[programming::ACK], &[programming::ACK]);
        mock.expect(&[programming::EXIT], &[programming::ACK]);
        mock.expect_reopen(Ok(()));
        mock.expect(b"ID\r", b"ID TH-D75\r");

        let mut radio = Radio::new(mock);
        let mut session = radio.enter_mcp().await?;
        assert_eq!(session.read_page(page).await?, data);
        session.exit().await?;

        assert_eq!(radio.mcp_phase, McpPhase::Inactive);
        radio.transport.assert_complete();
        Ok(())
    }

    #[tokio::test(start_paused = true)]
    async fn dropped_mcp_session_keeps_cat_poisoned_until_recovery() -> TestResult {
        let mut mock = MockTransport::new();
        mock.expect(programming::ENTER_PROGRAMMING, programming::ENTER_RESPONSE);

        let mut radio = Radio::new(mock);
        let session = radio.enter_mcp().await?;
        drop(session);

        assert_eq!(radio.mcp_phase, McpPhase::Active);
        let refused = radio.identify().await;
        assert!(
            matches!(refused, Err(Error::McpInterrupted)),
            "CAT must remain blocked after an MCP session is dropped: {refused:?}"
        );

        radio
            .transport
            .expect(&[programming::EXIT], &[programming::ACK]);
        radio.transport.expect_reopen(Ok(()));
        radio.transport.expect(b"ID\r", b"ID TH-D75\r");
        radio.recover_from_interrupted_mcp().await?;

        assert_eq!(radio.mcp_phase, McpPhase::Inactive);
        radio.transport.assert_complete();
        Ok(())
    }

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

    /// Script one contiguous MCP page-read sequence and its ACK exchanges.
    fn expect_read_pages(
        mock: &mut MockTransport,
        start_page: u16,
        pages: &[[u8; programming::PAGE_SIZE]],
    ) -> Result<(), BoxErr> {
        for (offset, data) in pages.iter().enumerate() {
            let offset = u16::try_from(offset)?;
            let page = start_page
                .checked_add(offset)
                .ok_or("mock MCP page range overflowed u16")?;
            let command = programming::build_read_command(McpPage::new(page)?);
            mock.expect(&command, &build_w_response(page, data)?);
            mock.expect(&[programming::ACK], &[programming::ACK]);
        }
        Ok(())
    }

    /// Build a 256-byte page payload with the given names in 16-byte slots.
    fn build_name_page(names: &[&str]) -> Result<Vec<u8>, BoxErr> {
        let mut data = vec![0u8; 256];
        for (i, name) in names.iter().enumerate().take(16) {
            let start = i * 16;
            let name = ChannelDisplayName::new(name)?;
            write_slice(&mut data, start, &name.to_wire_bytes())?;
        }
        Ok(data)
    }

    #[tokio::test]
    async fn mcp_entry_rejects_a_poisoned_gm_stream_before_io() -> TestResult {
        let mut radio = Radio::new(MockTransport::new());
        radio.gm_poisoned = true;

        let result = radio.read_memory_pages(McpPage::new(0)?, 1).await;
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
        let read_cmd = programming::build_read_command(McpPage::new(256)?);
        mock.expect(&read_cmd, &build_w_response(256, &first_page_data)?);

        // ACK exchange after first page, then remaining 62 pages.
        for page_offset in 1..programming::NAME_PAGE_COUNT {
            mock.expect(&[programming::ACK], &[programming::ACK]);

            let page = programming::NAME_START_PAGE + page_offset;
            let cmd = programming::build_read_command(McpPage::new(page)?);
            let empty = vec![0u8; 256];
            mock.expect(&cmd, &build_w_response(page, &empty)?);
        }

        // Final ACK after last page.
        mock.expect(&[programming::ACK], &[programming::ACK]);

        // Exit programming mode, then the exit path reconnects.
        mock.expect(b"E", &[programming::ACK]);
        mock.expect_reopen(Ok(()));
        mock.expect(b"ID\r", b"ID TH-D75\r");

        let mut radio = Radio::new(mock);
        let names = radio.read_channel_names().await?;

        // 16 names per page * 63 pages = 1008, truncated to 1000.
        assert_eq!(names.len(), 1000);
        assert_eq!(
            names.first().ok_or("names[0] missing")?.as_str(),
            "ForestCityPD"
        );
        assert_eq!(names.get(1).ok_or("names[1] missing")?.as_str(), "RPT1");
        assert!(names.get(2).ok_or("names[2] missing")?.is_empty());
        assert_eq!(names.get(3).ok_or("names[3] missing")?.as_str(), "NOAA WX");
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
        let cmd = programming::build_read_command(McpPage::new(page)?);
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

        let mut radio = Radio::new(mock);
        let data = radio.read_page(McpPage::new(page)?).await?;
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
        let cmd = programming::build_read_command(McpPage::new(page)?);
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

        let mut radio = Radio::new(mock);
        let result = radio.read_page(McpPage::new(page)?).await;
        assert!(
            matches!(
                result,
                Err(Error::McpPageMismatch {
                    requested,
                    answered,
                }) if requested.as_raw() == 0x0020 && answered.as_raw() == 0x0021
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
        let cmd = programming::build_read_command(McpPage::new(page)?);
        let full_response = build_w_response(page, &[0x11u8; programming::PAGE_SIZE])?;
        let partial = full_response
            .get(..32)
            .ok_or("test W response unexpectedly shorter than 32 bytes")?;
        mock.expect_partial_then_hang(&cmd, partial);

        // No retry, host ACK, raw exit, or reconnect is scripted. The partial
        // frame leaves the wire boundary ambiguous, so cleanup must close the
        // transport without transmitting another protocol byte.

        let mut radio = Radio::new(mock);
        let page_timeout = std::time::Duration::from_millis(50);
        radio.set_timeout(page_timeout);
        let result = radio.read_page(McpPage::new(page)?).await;
        assert!(
            matches!(
                &result,
                Err(Error::McpOperationAndCleanupFailed { operation, cleanup })
                    if matches!(operation.as_ref(), Error::Timeout(timeout) if *timeout == page_timeout)
                        && matches!(
                            cleanup.as_ref(),
                            Error::McpCleanupNotProved { cleanup }
                                if matches!(cleanup.as_ref(), Error::McpWireBoundaryUnproved)
                        )
            ),
            "partial frame must time out and refuse raw-E cleanup: {result:?}"
        );
        assert_eq!(radio.mcp_phase, McpPhase::Active);
        assert_eq!(radio.mcp_wire_boundary, McpWireBoundary::Ambiguous);
        assert_eq!(
            *radio.link_state().borrow(),
            LinkState::Down,
            "closing an ambiguous MCP stream must publish the lost link"
        );
        radio.transport.assert_complete();
        Ok(())
    }

    #[tokio::test(start_paused = true)]
    async fn page_read_does_not_discard_bytes_after_the_w_frame() -> TestResult {
        let mut mock = MockTransport::new();
        mock.expect(b"\r0M PROGRAM\r", b"0M\r");

        let page: u16 = 0x0020;
        let command = programming::build_read_command(McpPage::new(page)?);
        let mut response = build_w_response(page, &[0x11u8; programming::PAGE_SIZE])?;
        response.push(0x15);
        mock.expect(&command, &response);
        mock.expect(&[programming::ACK], &[programming::ACK]);

        let mut radio = Radio::new(mock);
        let result = radio.read_page(McpPage::new(page)?).await;
        assert!(
            matches!(
                &result,
                Err(Error::McpOperationAndCleanupFailed { operation, cleanup })
                    if matches!(
                        operation.as_ref(),
                        Error::McpPageReadNotAcknowledged { page, got: 0x15 }
                            if page.as_raw() == 0x0020
                    ) && matches!(
                        cleanup.as_ref(),
                        Error::McpCleanupNotProved { cleanup }
                            if matches!(cleanup.as_ref(), Error::McpWireBoundaryUnproved)
                    )
            ),
            "the trailing byte must reach the ACK parser and force fail-closed cleanup: {result:?}"
        );
        assert_eq!(radio.mcp_phase, McpPhase::Active);
        assert_eq!(radio.mcp_wire_boundary, McpWireBoundary::Ambiguous);
        radio.transport.assert_complete();
        Ok(())
    }

    #[tokio::test(start_paused = true)]
    async fn wrong_page_is_not_retried_without_completed_ack_handshake() -> TestResult {
        let mut mock = MockTransport::new();
        mock.expect(b"\r0M PROGRAM\r", b"0M\r");

        let requested_page: u16 = 0x0020;
        let answered_page: u16 = 0x0021;
        let cmd = programming::build_read_command(McpPage::new(requested_page)?);
        mock.expect(
            &cmd,
            &build_w_response(answered_page, &[0x11u8; programming::PAGE_SIZE])?,
        );
        mock.expect(&[programming::ACK], &[0x15]);

        // A bad trailing ACK makes the exchange unsafe to retry or exit. No
        // further protocol byte is scripted; cleanup must only close.

        let mut radio = Radio::new(mock);
        let result = radio.read_page(McpPage::new(requested_page)?).await;
        assert!(
            matches!(
                &result,
                Err(Error::McpOperationAndCleanupFailed { operation, cleanup })
                    if matches!(
                        operation.as_ref(),
                        Error::McpPageReadNotAcknowledged { page, got: 0x15 }
                            if page.as_raw() == 0x0021
                    ) && matches!(
                        cleanup.as_ref(),
                        Error::McpCleanupNotProved { cleanup }
                            if matches!(cleanup.as_ref(), Error::McpWireBoundaryUnproved)
                    )
            ),
            "wrong-page retry must require the radio's trailing ACK: {result:?}"
        );
        assert_eq!(radio.mcp_phase, McpPhase::Active);
        assert_eq!(radio.mcp_wire_boundary, McpWireBoundary::Ambiguous);
        radio.transport.assert_complete();
        Ok(())
    }

    #[tokio::test(start_paused = true)]
    async fn interrupted_mcp_poisons_cat_until_recovered() -> TestResult {
        let mut mock = MockTransport::new();
        mock.expect(b"\r0M PROGRAM\r", b"0M\r");
        // The first page read never completes, so the caller's timeout
        // cancels the whole dump future mid-transfer.
        let cmd = programming::build_read_command(McpPage::new(0)?);
        mock.expect_hang(&cmd);

        let mut radio = Radio::new(mock);
        let cancelled = tokio::time::timeout(
            std::time::Duration::from_millis(100),
            radio.read_memory_image(),
        )
        .await;
        assert!(cancelled.is_err(), "dump must be cancelled by the timeout");

        // The radio may still be in PROG MCP, so CAT must refuse rather
        // than talk binary-mode garbage.
        let refused = radio
            .execute(Command::GetOperatingMode { band: Band::A })
            .await;
        assert!(
            matches!(refused, Err(Error::McpInterrupted)),
            "CAT after a cancelled MCP session must refuse: {refused:?}"
        );

        // The cancelled read has no proved frame boundary. Recovery must
        // close without sending E or attempting CAT on the same transport.
        let recovery = radio.recover_from_interrupted_mcp().await;
        assert!(
            matches!(recovery, Err(Error::McpWireBoundaryUnproved)),
            "ambiguous recovery must require a power cycle: {recovery:?}"
        );
        assert_eq!(radio.mcp_phase, McpPhase::Active);
        assert_eq!(radio.mcp_wire_boundary, McpWireBoundary::Ambiguous);
        radio.transport.assert_complete();
        Ok(())
    }

    #[tokio::test(start_paused = true)]
    async fn cancelled_pre_entry_full_dump_recovery_restores_original_timeout() -> TestResult {
        let mut mock = MockTransport::new();
        // Keep the pre-entry stale-input drain pending long enough for
        // the caller to cancel before any MCP wire traffic is sent.
        mock.queue_read_delayed(b"stale\r", 100);

        let mut radio = Radio::new(mock);
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

        let mut read_radio = Radio::new(MockTransport::new());
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

        let mut write_radio = Radio::new(MockTransport::new());
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

        let mut radio = Radio::new(mock);
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

        let response = radio
            .execute(Command::GetOperatingMode { band: Band::A })
            .await?;
        assert!(matches!(response, Response::OperatingMode { .. }));
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

        let mut radio = Radio::new(mock);
        radio.mcp_phase = McpPhase::Active;
        radio.auto_info_enabled = true;

        let result = radio.exit_programming_mode().await;
        assert!(
            matches!(result, Err(Error::CommandRejected { .. })),
            "cached-state restore failure must remain an ordinary error: {result:?}"
        );
        assert_eq!(
            radio.mcp_phase,
            McpPhase::Inactive,
            "successful identify must clear MCP poison before cached-state restoration"
        );

        let response = radio
            .execute(Command::GetOperatingMode { band: Band::A })
            .await?;
        assert!(matches!(response, Response::OperatingMode { .. }));
        radio.transport.assert_complete();
        Ok(())
    }

    #[tokio::test(start_paused = true)]
    async fn cancelled_cached_state_restore_after_identify_does_not_repoison_mcp() -> TestResult {
        let mut mock = MockTransport::new();
        mock.expect_reopen(Ok(()));
        mock.expect(b"ID\r", b"ID TH-D75\r");
        mock.expect_hang(b"AI 1\r");

        let mut radio = Radio::new(mock);
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
        let mut radio = Radio::new(HangingWriteTransport);
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

        let mut radio = Radio::new(mock);
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

        let mut radio = Radio::new(mock);
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

        let refused = radio
            .execute(Command::GetOperatingMode { band: Band::A })
            .await;
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

        let mut radio = Radio::new(mock);
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
    async fn sparse_read_reports_operation_and_ambiguous_boundary_cleanup() -> TestResult {
        let mut mock = MockTransport::new();
        mock.expect(b"\r0M PROGRAM\r", b"0M\r");

        let page = McpPage::new(0x0010)?;
        let read = programming::build_read_command(page);
        // The short frame is followed by MockTransport's WouldBlock error,
        // producing a transfer failure without a timeout retry.
        mock.expect(&read, b"W");

        let mut radio = Radio::new(mock);
        let result = radio.read_sparse_memory_pages(&[page]).await;

        match result {
            Err(Error::McpOperationAndCleanupFailed { operation, cleanup }) => {
                assert!(
                    matches!(&*operation, Error::Transport(_)),
                    "the original page-read failure was not retained: {operation:?}"
                );
                assert!(
                    matches!(
                        &*cleanup,
                        Error::McpCleanupNotProved { cleanup }
                            if matches!(cleanup.as_ref(), Error::McpWireBoundaryUnproved)
                    ),
                    "the ambiguous boundary cleanup was not retained: {cleanup:?}"
                );
            }
            other => {
                return Err(
                    format!("expected combined operation/cleanup failure, got {other:?}").into(),
                );
            }
        }
        assert!(
            radio.mcp_phase == McpPhase::Active,
            "ambiguous read cleanup must not claim that E was sent"
        );
        assert_eq!(radio.mcp_wire_boundary, McpWireBoundary::Ambiguous);
        radio.transport.assert_complete();
        Ok(())
    }

    #[tokio::test(start_paused = true)]
    async fn operation_and_exit_ack_failures_are_both_retained_after_cat_proof() -> TestResult {
        let mut mock = MockTransport::new();
        mock.expect(b"\r0M PROGRAM\r", b"0M\r");

        let page = McpPage::new(0x0010)?;
        let read = programming::build_read_command(page);
        // Two complete, acknowledged wrong-page responses leave the wire at
        // a quiescent boundary while still producing an operation failure.
        // That is the condition under which sending E remains safe.
        let wrong_page = McpPage::new(page.as_raw() + 1)?;
        for _ in 0..2 {
            mock.expect(
                &read,
                &build_w_response(wrong_page.as_raw(), &[0xA5; programming::PAGE_SIZE])?,
            );
            mock.expect(&[programming::ACK], &[programming::ACK]);
        }
        mock.expect(b"E", &[0x15]);
        mock.expect_reopen(Ok(()));
        mock.expect(b"ID\r", b"ID TH-D75\r");

        let mut radio = Radio::new(mock);
        let result = radio.read_sparse_memory_pages(&[page]).await;
        match result {
            Err(Error::McpOperationAndCleanupFailed { operation, cleanup }) => {
                assert!(
                    matches!(
                        &*operation,
                        Error::McpPageMismatch { requested, answered }
                            if *requested == page && *answered == wrong_page
                    ),
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
    async fn failed_entry_closes_without_sending_an_unframed_exit() -> TestResult {
        let mut mock = MockTransport::new();
        mock.expect(b"\r0M PROGRAM\r", b"not-an-mcp-entry-acknowledgement");

        let mut radio = Radio::new(mock);
        let result = radio.read_page(McpPage::new(0)?).await;

        match result {
            Err(Error::McpOperationAndCleanupFailed { operation, cleanup }) => {
                assert!(
                    matches!(&*operation, Error::Protocol(_)),
                    "the failed entry was not retained: {operation:?}"
                );
                assert!(
                    matches!(
                        cleanup.as_ref(),
                        Error::McpCleanupNotProved { cleanup }
                            if matches!(cleanup.as_ref(), Error::McpWireBoundaryUnproved)
                    ),
                    "failed entry did not retain its ambiguous-boundary cleanup: {cleanup:?}"
                );
            }
            other => {
                return Err(format!("expected failed entry plus cleanup, got {other:?}").into());
            }
        }
        assert!(
            radio.mcp_phase == McpPhase::Active,
            "failed entry cleanup must not claim that E was sent"
        );
        assert_eq!(radio.mcp_wire_boundary, McpWireBoundary::Ambiguous);
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
        let cmd = programming::build_read_command(McpPage::new(page)?);
        mock.expect(&cmd, &build_w_response(page, &[0xAAu8; 256])?);
        mock.expect(&[programming::ACK], &[programming::ACK]);
        mock.expect(b"E", &[programming::ACK]);
        mock.expect_reopen(Ok(()));
        mock.expect(b"ID\r", b"ID TH-D75\r");

        let mut radio = Radio::new(mock);
        let data = radio.read_page(McpPage::new(page)?).await?;
        assert_eq!(*data.first().ok_or("data[0] missing")?, 0xAA);
        Ok(())
    }

    #[tokio::test(start_paused = true)]
    async fn read_all_channels_pairs_flags_and_preserves_empty_records() -> TestResult {
        let mut mock = MockTransport::new();
        mock.expect(b"\r0M PROGRAM\r", b"0M\r");

        let mut flag_pages =
            vec![[0xFF; programming::PAGE_SIZE]; usize::from(DATA_BACKED_CHANNEL_FLAG_PAGE_COUNT)];
        let first_flag_page = flag_pages.first_mut().ok_or("first flag page missing")?;
        let empty_first_flag = [0xFF, 0x7A, 0x1D, 0xA5];
        let programmed_second_flag = [0x08, 0x01, 0x05, 0xA5];
        write_slice(first_flag_page, 0, &empty_first_flag)?;
        write_slice(
            first_flag_page,
            programming::FLAG_RECORD_SIZE,
            &programmed_second_flag,
        )?;
        let empty_last_flag = [0xFF, 0x33, 0xAA, 0x55];
        let last_flag_page = flag_pages.last_mut().ok_or("last flag page missing")?;
        write_slice(
            last_flag_page,
            programming::PAGE_SIZE - programming::FLAG_RECORD_SIZE,
            &empty_last_flag,
        )?;
        expect_read_pages(&mut mock, programming::CHANNEL_FLAGS_START, &flag_pages)?;

        let data_page_count = programming::CHANNEL_DATA_END - programming::CHANNEL_DATA_START + 1;
        let mut data_pages = vec![[0xFF; programming::PAGE_SIZE]; usize::from(data_page_count)];
        let programmed = synthetic_stored_channel(Frequency::new(145_000_000));
        let first_data_page = data_pages.first_mut().ok_or("first data page missing")?;
        write_slice(
            first_data_page,
            programming::CHANNEL_RECORD_SIZE,
            &programmed.to_bytes(),
        )?;
        let empty_last_data = [0xA5; programming::CHANNEL_RECORD_SIZE];
        let last_data_page = data_pages.last_mut().ok_or("last data page missing")?;
        write_slice(
            last_data_page,
            (programming::CHANNELS_PER_MEMGROUP - 1) * programming::CHANNEL_RECORD_SIZE,
            &empty_last_data,
        )?;
        expect_read_pages(&mut mock, programming::CHANNEL_DATA_START, &data_pages)?;

        mock.expect(b"E", &[programming::ACK]);
        mock.expect_reopen(Ok(()));
        mock.expect(b"ID\r", b"ID TH-D75\r");

        let mut radio = Radio::new(mock);
        let slots = radio.read_all_channels().await?;
        assert_eq!(slots.len(), programming::CHANNEL_DATA_RECORD_COUNT);

        let first = slots.first().ok_or("first channel slot missing")?;
        assert_eq!(first.physical_index(), 0);
        assert_eq!(first.flag().to_wire_bytes(), empty_first_flag);
        assert!(!first.is_programmed());
        assert_eq!(
            first.data().to_bytes(),
            [0xFF; programming::CHANNEL_RECORD_SIZE],
            "the erased record must remain byte-for-byte intact"
        );

        let second = slots.get(1).ok_or("second channel slot missing")?;
        assert_eq!(second.physical_index(), 1);
        assert_eq!(second.flag().to_wire_bytes(), programmed_second_flag);
        assert!(second.is_programmed());
        assert_eq!(
            second
                .data()
                .programmed()
                .ok_or("programmed channel was not decoded")?
                .receive_frequency,
            Frequency::new(145_000_000)
        );

        let last = slots.last().ok_or("last channel slot missing")?;
        assert_eq!(
            last.physical_index(),
            programming::CHANNEL_DATA_RECORD_COUNT - 1
        );
        assert_eq!(last.flag().to_wire_bytes(), empty_last_flag);
        assert!(!last.is_programmed());
        assert_eq!(last.data().to_bytes(), empty_last_data);
        radio.transport.assert_complete();
        Ok(())
    }

    #[tokio::test(start_paused = true)]
    async fn read_all_channels_rejects_corrupt_record() -> TestResult {
        let mut mock = MockTransport::new();
        mock.expect(b"\r0M PROGRAM\r", b"0M\r");

        let mut flag_pages =
            vec![[0xFF; programming::PAGE_SIZE]; usize::from(DATA_BACKED_CHANNEL_FLAG_PAGE_COUNT)];
        write_slice(
            flag_pages.first_mut().ok_or("first flag page missing")?,
            0,
            &[0x00, 0x00, 0x00, 0xFF],
        )?;
        expect_read_pages(&mut mock, programming::CHANNEL_FLAGS_START, &flag_pages)?;

        let data_page_count = programming::CHANNEL_DATA_END - programming::CHANNEL_DATA_START + 1;
        let mut data_pages = vec![[0u8; programming::PAGE_SIZE]; usize::from(data_page_count)];
        // Corrupt the first programmed channel record: byte 0x0A's high nibble
        // must be one of the one-hot tone states 0, 1, 2, 4, or 8.
        set_byte(
            data_pages.first_mut().ok_or("first data page missing")?,
            0x0A,
            0xC0,
        )?;
        expect_read_pages(&mut mock, programming::CHANNEL_DATA_START, &data_pages)?;
        mock.expect(b"E", &[programming::ACK]);
        mock.expect_reopen(Ok(()));
        mock.expect(b"ID\r", b"ID TH-D75\r");

        let mut radio = Radio::new(mock);
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
        let cmd = programming::build_read_command(McpPage::new(page)?);
        mock.expect(&cmd, &build_w_response(page, &page_data)?);

        // ACK exchange.
        mock.expect(&[programming::ACK], &[programming::ACK]);

        // Exit, then the exit path reconnects.
        mock.expect(b"E", &[programming::ACK]);
        mock.expect_reopen(Ok(()));
        mock.expect(b"ID\r", b"ID TH-D75\r");

        let mut radio = Radio::new(mock);
        let result = radio.read_page(McpPage::new(page)?).await?;
        assert_eq!(*result.first().ok_or("result[0] missing")?, 0x00);
        assert_eq!(*result.get(1).ok_or("result[1] missing")?, 0xAB);
        Ok(())
    }

    #[tokio::test(start_paused = true)]
    async fn sparse_page_read_sorts_deduplicates_and_reports_progress() -> TestResult {
        let mut mock = MockTransport::new();
        mock.expect(b"\r0M PROGRAM\r", b"0M\r");

        let low_page = McpPage::new(0x0010)?;
        let high_page = McpPage::new(programming::TOTAL_PAGES - 1)?;
        let low_data = [0x11; programming::PAGE_SIZE];
        let high_data = [0x22; programming::PAGE_SIZE];

        // Input is deliberately unordered and duplicated. The strict mock
        // permits exactly one read and ACK exchange per distinct page, in
        // ascending order. The final factory-calibration page is readable.
        let low_read = programming::build_read_command(low_page);
        mock.expect(&low_read, &build_w_response(low_page.as_raw(), &low_data)?);
        mock.expect(&[programming::ACK], &[programming::ACK]);

        let high_read = programming::build_read_command(high_page);
        mock.expect(
            &high_read,
            &build_w_response(high_page.as_raw(), &high_data)?,
        );
        mock.expect(&[programming::ACK], &[programming::ACK]);

        mock.expect(b"E", &[programming::ACK]);
        mock.expect_reopen(Ok(()));
        mock.expect(b"ID\r", b"ID TH-D75\r");

        let mut radio = Radio::new(mock);
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
    async fn mcp_page_rejects_out_of_range_address_before_io() -> TestResult {
        let mock = MockTransport::new();
        let radio = Radio::new(mock);
        let result = McpPage::new(programming::TOTAL_PAGES);

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
        radio.transport.assert_complete();
        Ok(())
    }

    #[tokio::test]
    async fn sparse_page_read_empty_request_is_noop() -> TestResult {
        let mock = MockTransport::new();
        let mut radio = Radio::new(mock);
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
        let read = programming::build_read_command(McpPage::new(page)?);
        // A short response is followed by MockTransport's WouldBlock error,
        // which fails the page read without triggering the timeout retry.
        mock.expect(&read, b"W");

        let mut radio = Radio::new(mock);
        let result = radio.read_sparse_memory_pages(&[McpPage::new(page)?]).await;

        let Err(error) = result else {
            return Err("short page response unexpectedly succeeded".into());
        };
        assert!(
            error.to_string().contains("MCP wire boundary is ambiguous"),
            "short page response must refuse unsafe raw-E cleanup: {error}"
        );
        assert_eq!(radio.mcp_phase, McpPhase::Active);
        assert_eq!(radio.mcp_wire_boundary, McpWireBoundary::Ambiguous);
        radio.transport.assert_complete();
        Ok(())
    }

    #[tokio::test(start_paused = true)]
    async fn write_single_page_round_trip() -> TestResult {
        let mut mock = MockTransport::new();

        // Enter.
        mock.expect(b"\r0M PROGRAM\r", b"0M\r");

        // Write page 0x0100.
        let page = WritableMcpPage::new(0x0100)?;
        let page_data = [0xCDu8; 256];
        let write_cmd = programming::build_write_command(page, &page_data);
        mock.expect(&write_cmd, &[programming::ACK]);

        // Verify read-back returns what was written.
        let read_cmd = programming::build_read_command(page.page());
        mock.expect(&read_cmd, &build_w_response(page.as_raw(), &page_data)?);
        mock.expect(&[programming::ACK], &[programming::ACK]);

        // Exit, then the exit path reconnects.
        mock.expect(b"E", &[programming::ACK]);
        mock.expect_reopen(Ok(()));
        mock.expect(b"ID\r", b"ID TH-D75\r");

        let mut radio = Radio::new(mock);
        radio.write_page(page, &page_data).await?;
        Ok(())
    }

    #[tokio::test(start_paused = true)]
    async fn modify_memory_pages_reads_all_and_writes_only_changed_pages() -> TestResult {
        let mut mock = MockTransport::new();
        mock.expect(b"\r0M PROGRAM\r", b"0M\r");

        let low_page = WritableMcpPage::new(0x0010)?;
        let high_page = WritableMcpPage::new(0x0152)?;
        let low_original = vec![0x11; programming::PAGE_SIZE];
        let high_original = vec![0x22; programming::PAGE_SIZE];

        // Input is deliberately unordered and duplicated. The implementation
        // reads each unique page once, in ascending order, before any write.
        let low_read = programming::build_read_command(low_page.page());
        mock.expect(
            &low_read,
            &build_w_response(low_page.as_raw(), &low_original)?,
        );
        mock.expect(&[programming::ACK], &[programming::ACK]);

        let high_read = programming::build_read_command(high_page.page());
        mock.expect(
            &high_read,
            &build_w_response(high_page.as_raw(), &high_original)?,
        );
        mock.expect(&[programming::ACK], &[programming::ACK]);

        // Only the low page is changed. The high page therefore has no write
        // exchange in the strict mock script.
        let mut low_modified = low_original.clone();
        set_byte(&mut low_modified, 0x34, 0xA5)?;
        let low_modified_array = into_page_array(low_modified.clone())?;
        let low_write = programming::build_write_command(low_page, &low_modified_array);
        mock.expect(&low_write, &[programming::ACK]);
        mock.expect(
            &low_read,
            &build_w_response(low_page.as_raw(), &low_modified)?,
        );
        mock.expect(&[programming::ACK], &[programming::ACK]);

        mock.expect(b"E", &[programming::ACK]);
        mock.expect_reopen(Ok(()));
        mock.expect(b"ID\r", b"ID TH-D75\r");

        let mut radio = Radio::new(mock);
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

    #[tokio::test(start_paused = true)]
    async fn detached_multi_page_modify_writes_ascending_then_skips_reconnect() -> TestResult {
        let mut mock = MockTransport::new();
        mock.expect(b"\r0M PROGRAM\r", b"0M\r");

        let low_page = WritableMcpPage::new(0x0010)?;
        let high_page = WritableMcpPage::new(0x001C)?;
        let low_original = [0x11; programming::PAGE_SIZE];
        let high_original = [0x22; programming::PAGE_SIZE];

        // The caller supplies unordered, duplicate pages. Both complete reads
        // must occur once, in ascending order, before either write begins.
        for (page, original) in [(low_page, &low_original), (high_page, &high_original)] {
            let read = programming::build_read_command(page.page());
            mock.expect(&read, &build_w_response(page.as_raw(), original)?);
            mock.expect(&[programming::ACK], &[programming::ACK]);
        }

        let mut low_modified = low_original;
        let mut high_modified = high_original;
        set_byte(&mut low_modified, 0x71, 0xA1)?;
        set_byte(&mut high_modified, 0xA0, 0xB2)?;

        // Writes and their immediate read-back verification also retain the
        // distinct pages' ascending order.
        for (page, modified) in [(low_page, &low_modified), (high_page, &high_modified)] {
            let write = programming::build_write_command(page, modified);
            mock.expect(&write, &[programming::ACK]);
            let readback = programming::build_read_command(page.page());
            mock.expect(&readback, &build_w_response(page.as_raw(), modified)?);
            mock.expect(&[programming::ACK], &[programming::ACK]);
        }

        // A changed batch deliberately uses the detached exit: no reopen or
        // CAT identity exchange is present in this strict script.
        mock.expect(b"E", &[programming::ACK]);

        let mut radio = Radio::new(mock);
        let mut visited = Vec::new();
        let outcome = radio
            .modify_memory_pages_detached_if_changed(
                &[high_page, low_page, high_page],
                |page, data| {
                    visited.push(page);
                    if page == low_page {
                        if let Some(byte) = data.get_mut(0x71) {
                            *byte = 0xA1;
                        }
                    } else if page == high_page
                        && let Some(byte) = data.get_mut(0xA0)
                    {
                        *byte = 0xB2;
                    }
                },
            )
            .await?;

        assert_eq!(visited, vec![low_page, high_page]);
        assert_eq!(outcome, DetachedMcpPageUpdate::ChangedRadioRebooting);
        assert_eq!(radio.mcp_phase, McpPhase::Inactive);
        radio.transport.assert_complete();
        Ok(())
    }

    #[tokio::test(start_paused = true)]
    async fn detached_multi_page_modify_unchanged_restores_cat() -> TestResult {
        let mut mock = MockTransport::new();
        mock.expect(b"\r0M PROGRAM\r", b"0M\r");

        let page = WritableMcpPage::new(0x001C)?;
        let original = [0x5A; programming::PAGE_SIZE];
        let read = programming::build_read_command(page.page());
        mock.expect(&read, &build_w_response(page.as_raw(), &original)?);
        mock.expect(&[programming::ACK], &[programming::ACK]);

        // No write is scripted. An unchanged batch must take the normal exit
        // path and prove the reopened link speaks CAT.
        mock.expect(b"E", &[programming::ACK]);
        mock.expect_reopen(Ok(()));
        mock.expect(b"ID\r", b"ID TH-D75\r");

        let mut radio = Radio::new(mock);
        let mut visited = Vec::new();
        let outcome = radio
            .modify_memory_pages_detached_if_changed(&[page, page], |visited_page, _| {
                visited.push(visited_page);
            })
            .await?;

        assert_eq!(visited, vec![page]);
        assert_eq!(outcome, DetachedMcpPageUpdate::UnchangedCatReady);
        assert_eq!(radio.mcp_phase, McpPhase::Inactive);
        radio.transport.assert_complete();
        Ok(())
    }

    #[tokio::test]
    async fn detached_multi_page_empty_request_is_rejected_without_io() -> TestResult {
        let mock = MockTransport::new();
        let mut radio = Radio::new(mock);
        let mut callback_called = false;
        let result = radio
            .modify_memory_pages_detached_if_changed(&[], |_, _| callback_called = true)
            .await;

        assert!(
            matches!(result, Err(DetachedMcpPageUpdateError::EmptyPageSet)),
            "empty request must not claim that CAT was proved: {result:?}"
        );
        assert!(
            !callback_called,
            "empty request must not invoke its callback"
        );
        radio.transport.assert_complete();
        Ok(())
    }

    #[test]
    fn detached_update_wrapper_preserves_nested_recovery_and_link_classification() {
        let timed_out: Error = DetachedMcpPageUpdateError::Entry {
            source: Box::new(Error::Timeout(std::time::Duration::from_secs(1))),
        }
        .into();
        assert!(timed_out.is_link_lost());
        assert!(timed_out.requires_recovery());

        let ambiguous_cleanup: Error = DetachedMcpPageUpdateError::OperationAndCleanup {
            possibly_written_pages: Vec::new(),
            verified_written_pages: Vec::new(),
            operation: Box::new(Error::CommandRejected {
                mnemonic: "W".to_owned(),
            }),
            cleanup: Box::new(Error::McpWireBoundaryUnproved),
        }
        .into();
        assert!(!ambiguous_cleanup.is_link_lost());
        assert!(ambiguous_cleanup.requires_recovery());
    }

    #[tokio::test(start_paused = true)]
    async fn detached_multi_page_read_failure_writes_nothing_and_restores_cat() -> TestResult {
        let mut mock = MockTransport::new();
        mock.expect(b"\r0M PROGRAM\r", b"0M\r");

        let low_page = WritableMcpPage::new(0x0010)?;
        let high_page = WritableMcpPage::new(0x001C)?;
        let low_original = [0x11; programming::PAGE_SIZE];
        let low_read = programming::build_read_command(low_page.page());
        mock.expect(
            &low_read,
            &build_w_response(low_page.as_raw(), &low_original)?,
        );
        mock.expect(&[programming::ACK], &[programming::ACK]);

        // The second page is incomplete. Because the W frame boundary is not
        // proved, cleanup must close without sending raw E.
        let high_read = programming::build_read_command(high_page.page());
        mock.expect(&high_read, b"W");

        let mut radio = Radio::new(mock);
        let mut callback_called = false;
        let result = radio
            .modify_memory_pages_detached_if_changed(&[high_page, low_page], |_, _| {
                callback_called = true;
            })
            .await;

        let Err(error) = result else {
            return Err("short page response unexpectedly succeeded".into());
        };
        assert!(
            matches!(
                &error,
                DetachedMcpPageUpdateError::OperationAndCleanup { operation, .. }
                    if matches!(operation.as_ref(), Error::Transport(_))
            ),
            "short page response should retain its transport error: {error:?}"
        );
        assert!(
            error.possibly_written_pages().is_empty(),
            "a read failure cannot have started any write"
        );
        assert!(
            error.verified_written_pages().is_empty(),
            "a read failure cannot have verified any write"
        );
        assert!(
            !callback_called,
            "a failed read must prevent every callback and write"
        );
        assert_eq!(radio.mcp_phase, McpPhase::Active);
        assert_eq!(radio.mcp_wire_boundary, McpWireBoundary::Ambiguous);
        radio.transport.assert_complete();
        Ok(())
    }

    #[tokio::test(start_paused = true)]
    async fn detached_multi_page_partial_write_failure_restores_cat() -> TestResult {
        let mut mock = MockTransport::new();
        mock.expect(b"\r0M PROGRAM\r", b"0M\r");

        let low_page = WritableMcpPage::new(0x0010)?;
        let high_page = WritableMcpPage::new(0x001C)?;
        let low_original = [0x11; programming::PAGE_SIZE];
        let high_original = [0x22; programming::PAGE_SIZE];
        for (page, original) in [(low_page, &low_original), (high_page, &high_original)] {
            let read = programming::build_read_command(page.page());
            mock.expect(&read, &build_w_response(page.as_raw(), original)?);
            mock.expect(&[programming::ACK], &[programming::ACK]);
        }

        let mut low_modified = low_original;
        let mut high_modified = high_original;
        set_byte(&mut low_modified, 0x71, 0xA1)?;
        set_byte(&mut high_modified, 0xA0, 0xB2)?;

        // The first changed page is written and verified successfully.
        let low_write = programming::build_write_command(low_page, &low_modified);
        mock.expect(&low_write, &[programming::ACK]);
        let low_readback = programming::build_read_command(low_page.page());
        mock.expect(
            &low_readback,
            &build_w_response(low_page.as_raw(), &low_modified)?,
        );
        mock.expect(&[programming::ACK], &[programming::ACK]);

        // The second write reaches the transport but receives no ACK. The
        // operation must close without writing raw E because the W-frame
        // boundary is not proved.
        let high_write = programming::build_write_command(high_page, &high_modified);
        mock.expect(&high_write, &[]);

        let mut radio = Radio::new(mock);
        let result = radio
            .modify_memory_pages_detached_if_changed(&[high_page, low_page], |page, data| {
                if page == low_page {
                    if let Some(byte) = data.get_mut(0x71) {
                        *byte = 0xA1;
                    }
                } else if page == high_page
                    && let Some(byte) = data.get_mut(0xA0)
                {
                    *byte = 0xB2;
                }
            })
            .await;

        let Err(error) = result else {
            return Err("missing write ACK unexpectedly succeeded".into());
        };
        assert!(
            matches!(
                &error,
                DetachedMcpPageUpdateError::Operation { source, .. }
                    if matches!(source.as_ref(), Error::Transport(_))
            ) || matches!(
                &error,
                DetachedMcpPageUpdateError::OperationAndCleanup { operation, .. }
                    if matches!(operation.as_ref(), Error::Transport(_))
            ),
            "missing write ACK should retain its transport error: {error:?}"
        );
        assert_eq!(
            error.possibly_written_pages(),
            &[low_page, high_page],
            "both the completed and interrupted writes may have reached the radio"
        );
        assert_eq!(
            error.verified_written_pages(),
            &[low_page],
            "only the first page completed read-back verification"
        );
        assert_eq!(radio.mcp_phase, McpPhase::Active);
        assert_eq!(radio.mcp_wire_boundary, McpWireBoundary::Ambiguous);
        radio.transport.assert_complete();
        Ok(())
    }

    #[tokio::test(start_paused = true)]
    async fn detached_multi_page_second_verify_mismatch_reports_partial_progress() -> TestResult {
        let mut mock = MockTransport::new();
        mock.expect(b"\r0M PROGRAM\r", b"0M\r");

        let low_page = WritableMcpPage::new(0x0010)?;
        let high_page = WritableMcpPage::new(0x001C)?;
        let low_original = [0x11; programming::PAGE_SIZE];
        let high_original = [0x22; programming::PAGE_SIZE];
        for (page, original) in [(low_page, &low_original), (high_page, &high_original)] {
            let read = programming::build_read_command(page.page());
            mock.expect(&read, &build_w_response(page.as_raw(), original)?);
            mock.expect(&[programming::ACK], &[programming::ACK]);
        }

        let low_modified = [0xA1; programming::PAGE_SIZE];
        let high_modified = [0xB2; programming::PAGE_SIZE];
        let low_write = programming::build_write_command(low_page, &low_modified);
        mock.expect(&low_write, &[programming::ACK]);
        let low_read = programming::build_read_command(low_page.page());
        mock.expect(
            &low_read,
            &build_w_response(low_page.as_raw(), &low_modified)?,
        );
        mock.expect(&[programming::ACK], &[programming::ACK]);

        let high_write = programming::build_write_command(high_page, &high_modified);
        mock.expect(&high_write, &[programming::ACK]);
        let high_read = programming::build_read_command(high_page.page());
        let mut mismatched = high_modified;
        set_byte(&mut mismatched, 0xA0, 0xE3)?;
        mock.expect(
            &high_read,
            &build_w_response(high_page.as_raw(), &mismatched)?,
        );
        mock.expect(&[programming::ACK], &[programming::ACK]);

        // The read-back handshake completed, so the boundary is quiescent and
        // the normal error cleanup can safely exit and prove CAT identity.
        mock.expect(b"E", &[programming::ACK]);
        mock.expect_reopen(Ok(()));
        mock.expect(b"ID\r", b"ID TH-D75\r");

        let mut radio = Radio::new(mock);
        let result = radio
            .modify_memory_pages_detached_if_changed(&[high_page, low_page], |_, data| {
                let fill = if data.first().copied() == Some(0x11) {
                    0xA1
                } else {
                    0xB2
                };
                data.fill(fill);
            })
            .await;

        let Err(error) = result else {
            return Err("mismatched second-page read-back unexpectedly succeeded".into());
        };
        assert!(
            matches!(
                &error,
                DetachedMcpPageUpdateError::Operation { source, .. }
                    if matches!(
                        source.as_ref(),
                        Error::McpVerifyMismatch {
                            page,
                            offset: 0xA0,
                            expected: 0xB2,
                            actual: 0xE3,
                        } if *page == high_page
                    )
            ),
            "second-page verification mismatch lost its typed cause: {error:?}"
        );
        assert_eq!(error.possibly_written_pages(), &[low_page, high_page]);
        assert_eq!(error.verified_written_pages(), &[low_page]);
        assert_eq!(radio.mcp_phase, McpPhase::Inactive);
        assert_eq!(radio.mcp_wire_boundary, McpWireBoundary::Quiescent);
        radio.transport.assert_complete();
        Ok(())
    }

    #[tokio::test(start_paused = true)]
    async fn detached_multi_page_exit_failure_reports_all_verified_writes() -> TestResult {
        let mut mock = MockTransport::new();
        mock.expect(b"\r0M PROGRAM\r", b"0M\r");

        let low_page = WritableMcpPage::new(0x0010)?;
        let high_page = WritableMcpPage::new(0x001C)?;
        let low_original = [0x11; programming::PAGE_SIZE];
        let high_original = [0x22; programming::PAGE_SIZE];
        for (page, original) in [(low_page, &low_original), (high_page, &high_original)] {
            let read = programming::build_read_command(page.page());
            mock.expect(&read, &build_w_response(page.as_raw(), original)?);
            mock.expect(&[programming::ACK], &[programming::ACK]);
        }

        let low_modified = [0xA1; programming::PAGE_SIZE];
        let high_modified = [0xB2; programming::PAGE_SIZE];
        for (page, modified) in [(low_page, &low_modified), (high_page, &high_modified)] {
            let write = programming::build_write_command(page, modified);
            mock.expect(&write, &[programming::ACK]);
            let read = programming::build_read_command(page.page());
            mock.expect(&read, &build_w_response(page.as_raw(), modified)?);
            mock.expect(&[programming::ACK], &[programming::ACK]);
        }

        // Every page is verified before the detached exit receives its NAK.
        // No reconnect is attempted by this intentionally detached path.
        mock.expect(b"E", &[0x15]);

        let mut radio = Radio::new(mock);
        let result = radio
            .modify_memory_pages_detached_if_changed(&[high_page, low_page], |_, data| {
                let fill = if data.first().copied() == Some(0x11) {
                    0xA1
                } else {
                    0xB2
                };
                data.fill(fill);
            })
            .await;

        let Err(error) = result else {
            return Err("NAKed detached exit unexpectedly succeeded".into());
        };
        assert!(
            matches!(
                &error,
                DetachedMcpPageUpdateError::Cleanup { source, .. }
                    if matches!(
                        source.as_ref(),
                        Error::McpExitNotAcknowledged { got: 0x15 }
                    )
            ),
            "detached exit failure lost its typed cause: {error:?}"
        );
        assert_eq!(error.possibly_written_pages(), &[low_page, high_page]);
        assert_eq!(error.verified_written_pages(), &[low_page, high_page]);
        assert_eq!(radio.mcp_phase, McpPhase::ExitSent);
        assert_eq!(radio.mcp_wire_boundary, McpWireBoundary::Ambiguous);
        radio.transport.assert_complete();
        Ok(())
    }

    #[tokio::test]
    async fn writable_mcp_page_rejects_factory_calibration_before_io() -> TestResult {
        let mock = MockTransport::new();
        let radio = Radio::new(mock);
        let result = WritableMcpPage::new(0x07A1);

        assert!(
            matches!(result, Err(Error::McpWriteProtected { page }) if page.as_raw() == 0x07A1),
            "request should be rejected with the protected page number"
        );
        radio.transport.assert_complete();
        Ok(())
    }

    #[tokio::test]
    async fn modify_memory_pages_empty_request_is_noop() -> TestResult {
        let mock = MockTransport::new();
        let mut radio = Radio::new(mock);
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
    async fn modify_memory_pages_closes_after_ambiguous_read_failure() -> TestResult {
        let mut mock = MockTransport::new();
        mock.expect(b"\r0M PROGRAM\r", b"0M\r");

        let page = 0x0010;
        let read = programming::build_read_command(McpPage::new(page)?);
        // A short response is followed by MockTransport's WouldBlock error,
        // which fails the page read without triggering the timeout retry.
        mock.expect(&read, b"W");

        let mut radio = Radio::new(mock);
        let result = radio
            .modify_memory_pages(&[WritableMcpPage::new(page)?], |_, _| {})
            .await;

        assert!(
            matches!(
                &result,
                Err(Error::McpOperationAndCleanupFailed { operation, cleanup })
                    if matches!(operation.as_ref(), Error::Transport(_))
                        && matches!(
                            cleanup.as_ref(),
                            Error::McpCleanupNotProved { cleanup }
                                if matches!(cleanup.as_ref(), Error::McpWireBoundaryUnproved)
                        )
            ),
            "short page response must retain its error and refuse raw-E cleanup: {result:?}"
        );
        assert_eq!(radio.mcp_phase, McpPhase::Active);
        assert_eq!(radio.mcp_wire_boundary, McpWireBoundary::Ambiguous);
        radio.transport.assert_complete();
        Ok(())
    }

    #[tokio::test(start_paused = true)]
    async fn apply_menu_patches_via_mcp_writes_and_verifies_every_changed_page() -> TestResult {
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

        let beep_read = programming::build_read_command(McpPage::new(beep_page)?);
        mock.expect(&beep_read, &build_w_response(beep_page, &beep_original)?);
        mock.expect(&[programming::ACK], &[programming::ACK]);
        let select_read = programming::build_read_command(McpPage::new(select_page)?);
        mock.expect(
            &select_read,
            &build_w_response(select_page, &select_original)?,
        );
        mock.expect(&[programming::ACK], &[programming::ACK]);

        let mut beep_modified = beep_original.clone();
        set_byte(&mut beep_modified, 0x71, 0x01)?;
        let beep_modified_array = into_page_array(beep_modified.clone())?;
        let beep_write = programming::build_write_command(
            WritableMcpPage::new(beep_page)?,
            &beep_modified_array,
        );
        mock.expect(&beep_write, &[programming::ACK]);
        mock.expect(&beep_read, &build_w_response(beep_page, &beep_modified)?);
        mock.expect(&[programming::ACK], &[programming::ACK]);

        let mut select_modified = select_original.clone();
        set_byte(&mut select_modified, 0xC0, 0x02)?;
        let select_modified_array = into_page_array(select_modified.clone())?;
        let select_write = programming::build_write_command(
            WritableMcpPage::new(select_page)?,
            &select_modified_array,
        );
        mock.expect(&select_write, &[programming::ACK]);
        mock.expect(
            &select_read,
            &build_w_response(select_page, &select_modified)?,
        );
        mock.expect(&[programming::ACK], &[programming::ACK]);

        mock.expect(b"E", &[programming::ACK]);
        mock.expect_reopen(Ok(()));
        mock.expect(b"ID\r", b"ID TH-D75\r");

        let mut radio = Radio::new(mock);
        let mut planner = PatchPlanner::new();
        let beep = menu_field("radio.Beep").ok_or("registry field radio.Beep missing")?;
        beep.plan_value(&mut planner, FieldValue::Bool(true))?;
        let select = menu_field("gps.MyPositionSelect")
            .ok_or("registry field gps.MyPositionSelect missing")?;
        select.plan_value(&mut planner, FieldValue::Unsigned(2))?;
        let patches = planner.finish()?;

        let changed = radio.apply_menu_patches_via_mcp(&patches).await?;
        assert_eq!(
            changed,
            vec![
                WritableMcpPage::new(beep_page)?,
                WritableMcpPage::new(select_page)?,
            ],
            "both changed pages must be written and verified in ascending order"
        );
        radio.transport.assert_complete();
        Ok(())
    }

    #[tokio::test]
    async fn apply_menu_patches_via_mcp_rejects_unqualified_firmware_before_mcp_io() -> TestResult {
        use crate::memory::{FieldValue, PatchPlanner, menu_field};

        let mut planner = PatchPlanner::new();
        let beep = menu_field("radio.Beep").ok_or("registry field radio.Beep missing")?;
        beep.plan_value(&mut planner, FieldValue::Bool(true))?;
        let patches = planner.finish()?;

        let mut mock = MockTransport::new();
        mock.expect(b"ID\r", b"ID TH-D75\r");
        mock.expect(b"FV\r", b"FV 1.04\r");

        let mut radio = Radio::new(mock);
        let result = radio.apply_menu_patches_via_mcp(&patches).await;
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
                Err(Error::McpUnsupportedSchemaTarget {
                    expected_model: "TH-D75",
                    expected_firmware: "1.03",
                    ref actual_model,
                    ref actual_firmware,
                    ..
                }) if *actual_model == crate::types::RadioModel::ThD75
                    && actual_firmware.as_str() == "1.04"
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
        let page = WritableMcpPage::new(0x001C)?;
        let original = vec![0u8; 256];
        let mut modified = original.clone();
        set_byte(&mut modified, 0xA0, 0x01)?;
        let read_cmd = programming::build_read_command(page.page());
        mock.expect(&read_cmd, &build_w_response(page.as_raw(), &original)?);
        mock.expect(&[programming::ACK], &[programming::ACK]);
        let modified_array = into_page_array(modified.clone())?;
        let write_cmd = programming::build_write_command(page, &modified_array);
        mock.expect(&write_cmd, &[programming::ACK]);
        mock.expect(&read_cmd, &build_w_response(page.as_raw(), &modified)?);
        mock.expect(&[programming::ACK], &[programming::ACK]);

        // Exit byte only: NO reopen and NO identify are scripted, so
        // any reconnect attempt would fail the strict mock. The link
        // is deliberately left dead for the radio's reboot.
        mock.expect(b"E", &[programming::ACK]);

        let mut radio = Radio::new(mock);
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

        let page = WritableMcpPage::new(0x001C)?;
        let read_cmd = programming::build_read_command(page.page());
        let mut response = build_w_response(page.as_raw(), &[0x5A; programming::PAGE_SIZE])?;
        set_byte(&mut response, 3, 0x00)?;
        set_byte(&mut response, 4, 0x01)?;
        mock.expect(&read_cmd, &response);

        // The invalid W frame must not receive a host ACK, raw E, or reach the
        // patch callback. Its framing boundary cannot be proved after parse
        // failure, so cleanup closes the transport without another byte.

        let mut radio = Radio::new(mock);
        let mut callback_called = false;
        let result = radio
            .modify_memory_page_detached(page, |_| callback_called = true)
            .await;
        assert!(
            matches!(
                &result,
                Err(Error::McpOperationAndCleanupFailed { operation, cleanup })
                    if matches!(
                        operation.as_ref(),
                        Error::Protocol(ProtocolError::WriteResponseNonzeroOffset { got: 1 })
                    ) && matches!(
                        cleanup.as_ref(),
                        Error::McpCleanupNotProved { cleanup }
                            if matches!(cleanup.as_ref(), Error::McpWireBoundaryUnproved)
                    )
            ),
            "nonzero W offset was not rejected: {result:?}"
        );
        assert!(
            !callback_called,
            "invalid offset payload reached the patch callback"
        );
        assert_eq!(
            radio.mcp_phase,
            McpPhase::Active,
            "ambiguous parse failure must not claim that E was sent"
        );
        assert_eq!(radio.mcp_wire_boundary, McpWireBoundary::Ambiguous);
        radio.transport.assert_complete();
        Ok(())
    }

    #[tokio::test(start_paused = true)]
    async fn detached_operation_failure_does_not_trust_stale_ack_as_exit_proof() -> TestResult {
        let mut mock = MockTransport::new();
        mock.expect(b"\r0M PROGRAM\r", b"0M\r");

        let page = WritableMcpPage::new(0x001C)?;
        let read_cmd = programming::build_read_command(page.page());
        let full_response = build_w_response(page.as_raw(), &[0x5A; programming::PAGE_SIZE])?;
        let partial = full_response
            .get(..32)
            .ok_or("test W response unexpectedly shorter than 32 bytes")?;
        // The partial W times out. A delayed ACK could falsely satisfy a raw
        // exit if cleanup wrote E, so no E or reconnect is scripted.
        mock.expect_partial_then_hang_with_late(&read_cmd, partial, &[programming::ACK]);

        let mut radio = Radio::new(mock);
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
                    matches!(
                        &*cleanup,
                        Error::McpCleanupNotProved { cleanup }
                            if matches!(cleanup.as_ref(), Error::McpWireBoundaryUnproved)
                    ),
                    "ambiguous cleanup did not refuse the stale ACK: {cleanup:?}"
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
            McpPhase::Active,
            "ambiguous cleanup must not claim that E was sent"
        );
        assert_eq!(radio.mcp_wire_boundary, McpWireBoundary::Ambiguous);
        radio.transport.assert_complete();
        Ok(())
    }

    #[tokio::test(start_paused = true)]
    async fn page_exchange_preserves_caller_read_and_write_order() -> TestResult {
        let mut mock = MockTransport::new();
        mock.expect(b"\r0M PROGRAM\r", b"0M\r");

        // Deliberately neither ascending nor descending. The third exchange
        // is unchanged: it must still be read and compared in position, but
        // must not produce a write.
        let first_page = WritableMcpPage::new(0x0042)?;
        let second_page = WritableMcpPage::new(0x0040)?;
        let unchanged_page = WritableMcpPage::new(0x0041)?;
        let first_expected = [0x11; programming::PAGE_SIZE];
        let second_expected = [0x22; programming::PAGE_SIZE];
        let unchanged = [0x33; programming::PAGE_SIZE];
        let mut first_replacement = first_expected;
        let mut second_replacement = second_expected;
        set_byte(&mut first_replacement, 7, 0xA1)?;
        set_byte(&mut second_replacement, 9, 0xB2)?;

        for (page, data) in [
            (first_page, &first_expected),
            (second_page, &second_expected),
            (unchanged_page, &unchanged),
        ] {
            let read = programming::build_read_command(page.page());
            mock.expect(&read, &build_w_response(page.as_raw(), data)?);
            mock.expect(&[programming::ACK], &[programming::ACK]);
        }

        // The strict script proves writes retain caller order and that the
        // unchanged third page is skipped.
        for (page, replacement) in [
            (first_page, &first_replacement),
            (second_page, &second_replacement),
        ] {
            let write = programming::build_write_command(page, replacement);
            mock.expect(&write, &[programming::ACK]);
            let verify = programming::build_read_command(page.page());
            mock.expect(&verify, &build_w_response(page.as_raw(), replacement)?);
            mock.expect(&[programming::ACK], &[programming::ACK]);
        }

        mock.expect(b"E", &[programming::ACK]);
        mock.expect_reopen(Ok(()));
        mock.expect(b"ID\r", b"ID TH-D75\r");

        let exchanges = [
            McpPageExchange::new(first_page, first_expected, first_replacement),
            McpPageExchange::new(second_page, second_expected, second_replacement),
            McpPageExchange::new(unchanged_page, unchanged, unchanged),
        ];
        let mut radio = Radio::new(mock);
        let written = radio.compare_exchange_memory_pages(&exchanges).await?;
        assert_eq!(written, [first_page, second_page]);
        assert_eq!(radio.mcp_phase, McpPhase::Inactive);
        radio.transport.assert_complete();
        Ok(())
    }

    #[tokio::test(start_paused = true)]
    async fn page_exchange_reads_all_pages_then_mismatch_causes_zero_writes() -> TestResult {
        let mut mock = MockTransport::new();
        mock.expect(b"\r0M PROGRAM\r", b"0M\r");

        let pages = [
            WritableMcpPage::new(0x0010)?,
            WritableMcpPage::new(0x0012)?,
            WritableMcpPage::new(0x0011)?,
        ];
        let expected = [
            [0x10; programming::PAGE_SIZE],
            [0x20; programming::PAGE_SIZE],
            [0x30; programming::PAGE_SIZE],
        ];
        let mut live = expected;
        set_byte(&mut live[1], 0x5A, 0xFE)?;

        // Even though page index 1 mismatches, index 2 must be read before
        // comparison begins. No W command is scripted, so any write fails the
        // test before the exit exchange.
        for ((page, _), data) in pages.iter().zip(&expected).zip(&live) {
            let read = programming::build_read_command(page.page());
            mock.expect(&read, &build_w_response(page.as_raw(), data)?);
            mock.expect(&[programming::ACK], &[programming::ACK]);
        }
        mock.expect(b"E", &[programming::ACK]);
        mock.expect_reopen(Ok(()));
        mock.expect(b"ID\r", b"ID TH-D75\r");

        let exchanges = [
            McpPageExchange::new(pages[0], expected[0], [0xA0; programming::PAGE_SIZE]),
            McpPageExchange::new(pages[1], expected[1], [0xB0; programming::PAGE_SIZE]),
            McpPageExchange::new(pages[2], expected[2], [0xC0; programming::PAGE_SIZE]),
        ];
        let mut radio = Radio::new(mock);
        let error = match radio.compare_exchange_memory_pages(&exchanges).await {
            Err(error) => error,
            Ok(written) => {
                return Err(
                    format!("stale expected page unexpectedly wrote pages: {written:?}").into(),
                );
            }
        };
        assert!(
            matches!(
                &error,
                McpPageExchangeError::Operation { operation, .. }
                    if matches!(
                        operation.as_ref(),
                        McpPageExchangeOperationError::CompareMismatch {
                            page,
                            offset: 0x5A,
                            expected: 0x20,
                            actual: 0xFE,
                        } if page.as_raw() == 0x0012
                    )
            ),
            "first mismatch was not reported exactly: {error:?}"
        );
        assert!(error.possibly_written_pages().is_empty());
        assert_eq!(radio.mcp_phase, McpPhase::Inactive);
        radio.transport.assert_complete();
        Ok(())
    }

    #[tokio::test]
    async fn page_exchange_rejects_duplicate_pages_before_io() -> TestResult {
        let data = [0u8; programming::PAGE_SIZE];
        let page = WritableMcpPage::new(0x0010)?;

        let mut duplicate_radio = Radio::new(MockTransport::new());
        let duplicate = [
            McpPageExchange::new(page, data, data),
            McpPageExchange::new(page, data, data),
        ];
        let result = duplicate_radio
            .compare_exchange_memory_pages(&duplicate)
            .await;
        assert!(matches!(
            result,
            Err(McpPageExchangeError::DuplicatePage { page: duplicate })
                if duplicate == page
        ));
        duplicate_radio.transport.assert_complete();
        Ok(())
    }

    #[tokio::test(start_paused = true)]
    async fn page_exchange_write_failure_reports_ordered_possible_writes_and_exits() -> TestResult {
        let mut mock = MockTransport::new();
        mock.expect(b"\r0M PROGRAM\r", b"0M\r");

        let first_page = WritableMcpPage::new(0x0012)?;
        let second_page = WritableMcpPage::new(0x0010)?;
        let first_expected = [0x11; programming::PAGE_SIZE];
        let second_expected = [0x22; programming::PAGE_SIZE];
        let first_replacement = [0xA1; programming::PAGE_SIZE];
        let second_replacement = [0xB2; programming::PAGE_SIZE];

        for (page, data) in [
            (first_page, &first_expected),
            (second_page, &second_expected),
        ] {
            let read = programming::build_read_command(page.page());
            mock.expect(&read, &build_w_response(page.as_raw(), data)?);
            mock.expect(&[programming::ACK], &[programming::ACK]);
        }

        let first_write = programming::build_write_command(first_page, &first_replacement);
        mock.expect(&first_write, &[programming::ACK]);
        let first_verify = programming::build_read_command(first_page.page());
        mock.expect(
            &first_verify,
            &build_w_response(first_page.as_raw(), &first_replacement)?,
        );
        mock.expect(&[programming::ACK], &[programming::ACK]);

        let second_write = programming::build_write_command(second_page, &second_replacement);
        mock.expect(&second_write, &[0x15]);

        // A NAK does not prove the W exchange boundary. No raw exit or CAT
        // reconnect is scripted; cleanup must fail closed by closing.

        let exchanges = [
            McpPageExchange::new(first_page, first_expected, first_replacement),
            McpPageExchange::new(second_page, second_expected, second_replacement),
        ];
        let mut radio = Radio::new(mock);
        let error = match radio.compare_exchange_memory_pages(&exchanges).await {
            Err(error) => error,
            Ok(written) => {
                return Err(format!("NAKed exchange unexpectedly wrote pages: {written:?}").into());
            }
        };
        assert_eq!(
            error.possibly_written_pages(),
            [first_page, second_page],
            "the failing page must be included for conservative restoration"
        );
        assert!(
            matches!(
                &error,
                McpPageExchangeError::OperationAndCleanup {
                    operation,
                    cleanup,
                    ..
                }
                    if matches!(
                        operation.as_ref(),
                        McpPageExchangeOperationError::Write { page, source }
                            if *page == second_page
                                && matches!(
                                    source.as_ref(),
                                    Error::McpWriteNotAcknowledged { page, got: 0x15 }
                                        if *page == second_page
                                )
                    ) && matches!(
                        cleanup.as_ref(),
                        Error::McpWireBoundaryUnproved
                    )
            ),
            "write failure lost its page or typed cause: {error:?}"
        );
        assert_eq!(radio.mcp_phase, McpPhase::Active);
        assert_eq!(radio.mcp_wire_boundary, McpWireBoundary::Ambiguous);
        radio.transport.assert_complete();
        Ok(())
    }

    #[tokio::test(start_paused = true)]
    async fn page_exchange_readback_failure_reports_possible_write_and_exits() -> TestResult {
        let mut mock = MockTransport::new();
        mock.expect(b"\r0M PROGRAM\r", b"0M\r");

        let page = WritableMcpPage::new(0x0010)?;
        let expected = [0x11; programming::PAGE_SIZE];
        let replacement = [0x77; programming::PAGE_SIZE];
        let read = programming::build_read_command(page.page());
        mock.expect(&read, &build_w_response(page.as_raw(), &expected)?);
        mock.expect(&[programming::ACK], &[programming::ACK]);

        let write = programming::build_write_command(page, &replacement);
        mock.expect(&write, &[programming::ACK]);
        let mut bad_readback = replacement;
        set_byte(&mut bad_readback, 0x3C, 0x00)?;
        mock.expect(&read, &build_w_response(page.as_raw(), &bad_readback)?);
        mock.expect(&[programming::ACK], &[programming::ACK]);

        mock.expect(b"E", &[programming::ACK]);
        mock.expect_reopen(Ok(()));
        mock.expect(b"ID\r", b"ID TH-D75\r");

        let mut radio = Radio::new(mock);
        let exchange = [McpPageExchange::new(page, expected, replacement)];
        let error = match radio.compare_exchange_memory_pages(&exchange).await {
            Err(error) => error,
            Ok(written) => {
                return Err(
                    format!("corrupt readback unexpectedly accepted pages: {written:?}").into(),
                );
            }
        };
        assert_eq!(error.possibly_written_pages(), [page]);
        assert!(
            matches!(
                &error,
                McpPageExchangeError::Operation { operation, .. }
                    if matches!(
                        operation.as_ref(),
                        McpPageExchangeOperationError::Write { page: p, source }
                            if *p == page
                                && matches!(
                                    source.as_ref(),
                                    Error::McpVerifyMismatch {
                                        page: p,
                                        offset: 0x3C,
                                        expected: 0x77,
                                        actual: 0x00,
                                    } if *p == page
                                )
                    )
            ),
            "readback mismatch lost its typed cause: {error:?}"
        );
        assert_eq!(radio.mcp_phase, McpPhase::Inactive);
        radio.transport.assert_complete();
        Ok(())
    }

    #[tokio::test(start_paused = true)]
    async fn page_exchange_retains_compare_and_exit_failures() -> TestResult {
        let mut mock = MockTransport::new();
        mock.expect(b"\r0M PROGRAM\r", b"0M\r");

        let page = WritableMcpPage::new(0x0010)?;
        let expected = [0x11; programming::PAGE_SIZE];
        let actual = [0x22; programming::PAGE_SIZE];
        let read = programming::build_read_command(page.page());
        mock.expect(&read, &build_w_response(page.as_raw(), &actual)?);
        mock.expect(&[programming::ACK], &[programming::ACK]);

        // A wrong exit ACK is still followed by a successful CAT identity
        // proof. Both it and the comparison failure must remain visible.
        mock.expect(b"E", &[0x15]);
        mock.expect_reopen(Ok(()));
        mock.expect(b"ID\r", b"ID TH-D75\r");

        let exchange = [McpPageExchange::new(
            page,
            expected,
            [0x33; programming::PAGE_SIZE],
        )];
        let mut radio = Radio::new(mock);
        let error = match radio.compare_exchange_memory_pages(&exchange).await {
            Err(error) => error,
            Ok(written) => {
                return Err(format!(
                    "mismatched page and bad exit unexpectedly succeeded: {written:?}"
                )
                .into());
            }
        };
        assert!(error.possibly_written_pages().is_empty());
        assert!(
            matches!(
                &error,
                McpPageExchangeError::OperationAndCleanup {
                    operation,
                    cleanup,
                    ..
                } if matches!(
                    operation.as_ref(),
                    McpPageExchangeOperationError::CompareMismatch {
                        page,
                        offset: 0,
                        expected: 0x11,
                        actual: 0x22,
                    } if page.as_raw() == 0x0010
                ) && matches!(cleanup.as_ref(), Error::McpExitNotAcknowledged { got: 0x15 })
            ),
            "combined operation/cleanup context was not retained: {error:?}"
        );
        assert_eq!(radio.mcp_phase, McpPhase::Inactive);
        radio.transport.assert_complete();
        Ok(())
    }

    #[tokio::test(start_paused = true)]
    async fn write_page_verify_mismatch_is_typed() -> TestResult {
        let mut mock = MockTransport::new();
        mock.expect(b"\r0M PROGRAM\r", b"0M\r");

        let page = WritableMcpPage::new(0x0100)?;
        let page_data = [0xCDu8; 256];
        let write_cmd = programming::build_write_command(page, &page_data);
        mock.expect(&write_cmd, &[programming::ACK]);

        // Read-back returns one flipped byte at offset 7: the radio
        // ACKed the write but the byte did not land.
        let mut corrupted = page_data.to_vec();
        set_byte(&mut corrupted, 7, 0x00)?;
        let read_cmd = programming::build_read_command(page.page());
        mock.expect(&read_cmd, &build_w_response(page.as_raw(), &corrupted)?);
        mock.expect(&[programming::ACK], &[programming::ACK]);

        // Exit (with reconnect) still runs even though verify failed.
        mock.expect(b"E", &[programming::ACK]);
        mock.expect_reopen(Ok(()));
        mock.expect(b"ID\r", b"ID TH-D75\r");

        let mut radio = Radio::new(mock);
        let result = radio.write_page(page, &page_data).await;
        assert!(
            matches!(
                result,
                Err(Error::McpVerifyMismatch {
                    page,
                    offset: 7,
                    expected: 0xCD,
                    actual: 0x00,
                }) if page.as_raw() == 0x0100
            ),
            "verify mismatch must surface with the differing byte: {result:?}"
        );
        Ok(())
    }

    #[tokio::test(start_paused = true)]
    async fn write_page_unverified_skips_readback() -> TestResult {
        let mut mock = MockTransport::new();
        mock.expect(b"\r0M PROGRAM\r", b"0M\r");

        let page = WritableMcpPage::new(0x0100)?;
        let page_data = [0xCDu8; 256];
        let write_cmd = programming::build_write_command(page, &page_data);
        mock.expect(&write_cmd, &[programming::ACK]);

        // No read-back scripted: the unverified variant must not read.
        mock.expect(b"E", &[programming::ACK]);
        mock.expect_reopen(Ok(()));
        mock.expect(b"ID\r", b"ID TH-D75\r");

        let mut radio = Radio::new(mock);
        radio.write_page_unverified(page, &page_data).await?;
        radio.transport.assert_complete();
        Ok(())
    }

    #[tokio::test]
    async fn write_memory_image_wrong_size_rejected() -> TestResult {
        let mock = MockTransport::new();
        let mut radio = Radio::new(mock);

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
            let cmd = programming::build_read_command(McpPage::new(page)?);
            mock.expect(&cmd, &build_w_response(page, &data)?);
            mock.expect(&[programming::ACK], &[programming::ACK]);
        }

        // Exit, then the exit path reconnects.
        mock.expect(b"E", &[programming::ACK]);
        mock.expect_reopen(Ok(()));
        mock.expect(b"ID\r", b"ID TH-D75\r");

        let mut radio = Radio::new(mock);
        let data = radio.read_memory_pages(McpPage::new(0x0040)?, 2).await?;
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
        let mut radio = Radio::new(mock);

        let empty = radio.read_memory_pages(McpPage::new(0)?, 0).await?;
        assert!(empty.is_empty(), "zero-page read must be a no-op");

        let crossing = radio
            .read_memory_pages(McpPage::new(programming::TOTAL_PAGES - 1)?, 2)
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

        let overflowing = radio.read_memory_pages(McpPage::new(1)?, u16::MAX).await;
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

        let single = McpPage::new(u16::MAX);
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
        let mut radio = Radio::new(mock);

        radio
            .write_memory_pages(WritableMcpPage::new(0)?, &[])
            .await?;

        let unaligned = vec![0; programming::PAGE_SIZE + 1];
        let alignment_result = radio
            .write_memory_pages(WritableMcpPage::new(0)?, &unaligned)
            .await;
        assert!(
            matches!(
                alignment_result,
                Err(Error::McpInvalidImageSize {
                    actual,
                    expected,
                }) if actual == programming::PAGE_SIZE + 1
                    && expected == programming::PAGE_SIZE * 2
            ),
            "unaligned data must fail before MCP entry: {alignment_result:?}"
        );

        let crossing = vec![0; programming::PAGE_SIZE * 4];
        let range_result = radio
            .write_memory_pages(
                WritableMcpPage::new(programming::MAX_WRITABLE_PAGE)?,
                &crossing,
            )
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

        let single_result = WritableMcpPage::new(u16::MAX);
        assert!(
            matches!(
                single_result,
                Err(Error::McpPageOutOfRange {
                    page: u16::MAX,
                    total_pages,
                }) if total_pages == programming::TOTAL_PAGES
            ),
            "out-of-range single-page write must fail before MCP entry: {single_result:?}"
        );
        radio.transport.assert_complete();
        Ok(())
    }

    #[tokio::test]
    async fn write_memory_pages_protected_range_rejected() -> TestResult {
        let mock = MockTransport::new();
        let mut radio = Radio::new(mock);

        // Try to write 3 pages starting at 0x07A0 -- page 0x07A1 is protected.
        let data = vec![0u8; 768]; // 3 pages
        let result = radio
            .write_memory_pages(WritableMcpPage::new(0x07A0)?, &data)
            .await;
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
            let cmd = programming::build_read_command(McpPage::new(page)?);
            mock.expect(&cmd, &build_w_response(page, &data)?);
            mock.expect(&[programming::ACK], &[programming::ACK]);
        }

        // Exit, then the exit path reconnects.
        mock.expect(b"E", &[programming::ACK]);
        mock.expect_reopen(Ok(()));
        mock.expect(b"ID\r", b"ID TH-D75\r");

        let mut radio = Radio::new(mock);
        let flags = radio.read_channel_flags().await?;

        // Should have 1200 flags.
        assert_eq!(flags.len(), programming::TOTAL_CHANNEL_ENTRIES);

        // Check the first two we programmed.
        let ch0 = flags.first().ok_or("channel 0 flag missing")?;
        assert!(!ch0.is_empty());
        assert_eq!(ch0.band(), Some(MemoryChannelBand::Vhf));
        assert_eq!(ch0.scan_lockout(), Some(false));
        assert_eq!(ch0.group(), Some(MemoryGroup::new(0)?));

        let ch1 = flags.get(1).ok_or("channel 1 flag missing")?;
        assert!(!ch1.is_empty());
        assert_eq!(ch1.band(), Some(MemoryChannelBand::Uhf));
        assert_eq!(ch1.scan_lockout(), Some(true));
        assert_eq!(ch1.group(), Some(MemoryGroup::new(5)?));

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
            let cmd = programming::build_read_command(McpPage::new(page)?);
            mock.expect(&cmd, &build_w_response(page, &data)?);
            mock.expect(&[programming::ACK], &[programming::ACK]);
        }

        // Exit, then the exit path reconnects.
        mock.expect(b"E", &[programming::ACK]);
        mock.expect_reopen(Ok(()));
        mock.expect(b"ID\r", b"ID TH-D75\r");

        let mut radio = Radio::new(mock);

        // Use read_memory_pages (which doesn't expose progress), but we
        // can test the internal progress via read_memory_image_with_progress
        // indirectly. For now, just verify read_memory_pages works with 3 pages.
        let data = radio.read_memory_pages(McpPage::new(0x0100)?, 3).await?;
        assert_eq!(data.len(), 768);
        Ok(())
    }

    #[tokio::test(start_paused = true)]
    async fn modify_memory_page_read_modify_write() -> TestResult {
        let mut mock = MockTransport::new();

        // Page 0x0010 contains MCP offset 0x1000-0x10FF.
        let page = WritableMcpPage::new(0x0010)?;
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
        let read_cmd = programming::build_read_command(page.page());
        mock.expect(&read_cmd, &build_w_response(page.as_raw(), &original_data)?);

        // ACK exchange after read.
        mock.expect(&[programming::ACK], &[programming::ACK]);

        // Write modified page.
        let expected_array = into_page_array(expected_data.clone())?;
        let write_cmd = programming::build_write_command(page, &expected_array);
        mock.expect(&write_cmd, &[programming::ACK]);

        // Verify read-back returns the modified page.
        mock.expect(
            &read_cmd,
            &build_w_response(page.as_raw(), &expected_array)?,
        );
        mock.expect(&[programming::ACK], &[programming::ACK]);

        // Exit programming mode, then the exit path reconnects.
        mock.expect(b"E", &[programming::ACK]);
        mock.expect_reopen(Ok(()));
        mock.expect(b"ID\r", b"ID TH-D75\r");

        let mut radio = Radio::new(mock);
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
        let name = ChannelDisplayName::new("TestCh")?;
        write_slice(&mut expected_data, offset, name.as_str().as_bytes())?;

        // Enter programming mode.
        mock.expect(b"\r0M PROGRAM\r", b"0M\r");

        // Read page.
        let read_cmd = programming::build_read_command(McpPage::new(page)?);
        mock.expect(&read_cmd, &build_w_response(page, &original_data)?);

        // ACK exchange after read.
        mock.expect(&[programming::ACK], &[programming::ACK]);

        // Write modified page.
        let expected_array = into_page_array(expected_data)?;
        let write_cmd =
            programming::build_write_command(WritableMcpPage::new(page)?, &expected_array);
        mock.expect(&write_cmd, &[programming::ACK]);

        // Verify read-back returns the modified page.
        mock.expect(&read_cmd, &build_w_response(page, &expected_array)?);
        mock.expect(&[programming::ACK], &[programming::ACK]);

        // Exit programming mode, then the exit path reconnects.
        mock.expect(b"E", &[programming::ACK]);
        mock.expect_reopen(Ok(()));
        mock.expect(b"ID\r", b"ID TH-D75\r");

        let mut radio = Radio::new(mock);
        radio
            .write_channel_name(RegularChannel::new(5)?, &name)
            .await?;
        Ok(())
    }

    #[tokio::test]
    async fn write_channel_name_out_of_range_rejected() -> TestResult {
        let err = RegularChannel::new(1000)
            .err()
            .ok_or("expected out-of-range channel to fail validation")?;
        assert!(
            err.to_string().contains("out of range"),
            "error should mention out of range: {err}"
        );
        Ok(())
    }

    #[tokio::test(start_paused = true)]
    async fn write_channel_name_preserves_all_16_bytes() -> TestResult {
        let mut mock = MockTransport::new();

        // Channel 0 on page 0x0100, offset 0.
        let page: u16 = 0x0100;
        let original_data = vec![0u8; 256];

        let full_width_name = ChannelDisplayName::new("ABCDEFGHIJKLMNOP")?;
        let mut expected_data = original_data.clone();
        write_slice(&mut expected_data, 0, full_width_name.as_str().as_bytes())?;

        mock.expect(b"\r0M PROGRAM\r", b"0M\r");
        let read_cmd = programming::build_read_command(McpPage::new(page)?);
        mock.expect(&read_cmd, &build_w_response(page, &original_data)?);
        mock.expect(&[programming::ACK], &[programming::ACK]);
        let expected_array = into_page_array(expected_data)?;
        let write_cmd =
            programming::build_write_command(WritableMcpPage::new(page)?, &expected_array);
        mock.expect(&write_cmd, &[programming::ACK]);
        mock.expect(&read_cmd, &build_w_response(page, &expected_array)?);
        mock.expect(&[programming::ACK], &[programming::ACK]);
        mock.expect(b"E", &[programming::ACK]);
        mock.expect_reopen(Ok(()));
        mock.expect(b"ID\r", b"ID TH-D75\r");

        let mut radio = Radio::new(mock);
        radio
            .write_channel_name(RegularChannel::new(0)?, &full_width_name)
            .await?;
        Ok(())
    }
}
