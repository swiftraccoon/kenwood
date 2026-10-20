//! Explicit MCP ownership: sparse reads, guarded writes, exit, and journal inspection.
//!
//! # Choose a level
//!
//! [`Radio::backup_mcp_until_exit`] retains acknowledged standard configuration
//! pages even on failure. [`Radio::enter_mcp`] instead gives the caller a
//! borrowed [`McpSession`] for composing [`McpSession::read_page`],
//! [`McpSession::read_regions`], or sparse menu operations within one entry.
//! [`RegionImage`] retains read coverage; [`crate::memory::MemoryImage`] retains
//! only a full-sized byte buffer.
//!
//! For ordinary registered changes, prepare a
//! [`MenuUpdatePlan`](super::menu::MenuUpdatePlan). Persistent Gateway changes
//! use [`TerminalPlan`](super::terminal::TerminalPlan). The lower-level
//! [`McpSession::compare_exchange_pages`] writes caller-built
//! [`PageReplacement`]s under the same [`super::WRITE_TARGETS`] gate, fresh
//! comparison, journal callback and readback, without either plan's
//! operating-state guards.
//!
//! # Lifecycle and cleanup
//!
//! Entry interrupts normal radio operation. Keep the session and any write
//! journal until complete exchanges permit explicit [`McpSession::exit`].
//! A synchronized comparison failure can permit exit; an incomplete frame
//! cannot. Dropping a future or session performs no asynchronous protocol cleanup.
//!
//! E/ACK retires the original connection without proving CAT readiness. Close
//! and drop that transport before separately selecting, opening, and identifying
//! a fresh connection. Retain operation, exit, close, and fresh-verification
//! failures independently.
//!
//! The executable example below runs entirely on strict mocks. It demonstrates
//! these ownership boundaries, sparse coverage, and failure handling.

#![doc = concat!(
    "\n\n# Executable offline lifecycle\n\n",
    "Run the same source with `cargo run -p kenwood-tmd750 --example owned_lifecycle`.\n\n",
    "```rust\n",
    include_str!("../../examples/owned_lifecycle.rs"),
    "\n```\n"
)]

pub use compare_exchange::{McpCompareExchangeReport, PageReplacement};

use super::{Progress, Radio};
use crate::error::{AckStage, Error, McpError, ProtocolError, TimeoutOperation};
use crate::protocol::mcp::{
    ACK, BAUD, ENTER, ENTER_RESPONSE, EXIT, HEADER_LEN, Header, HeaderCommand,
};
use crate::types::{IMAGE_LENGTH, Page, Region};
use kenwood_transport::Transport;

mod channel_names;
mod compare_exchange;

/// Pages the radio may have changed and pages confirmed by read-back.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct McpJournal {
    /// Pages a write was started for.
    pub possibly_written: Vec<Page>,
    /// Pages read back equal to the intended bytes.
    pub verified: Vec<Page>,
}

/// A full-sized storage buffer plus the regions actually read into it.
///
/// Unread bytes are synthetic zeroes, not observations. Prefer [`Self::bytes`]
/// for coverage-checked inspection and retain [`Self::covered`] with exported
/// data. Conversion to [`crate::memory::MemoryImage`] loses this coverage map.
#[derive(Debug, Clone)]
pub struct RegionImage {
    bytes: Vec<u8>,
    covered: Vec<Region>,
}

impl Default for RegionImage {
    fn default() -> Self {
        Self::new()
    }
}

impl RegionImage {
    /// An image with nothing read.
    #[must_use]
    pub fn new() -> Self {
        Self {
            bytes: vec![0; IMAGE_LENGTH],
            covered: Vec::new(),
        }
    }

    /// Whether `region` lies inside a region that was read.
    #[must_use]
    pub fn covers(&self, region: Region) -> bool {
        self.covered
            .iter()
            .any(|covered| covered.contains_region(region))
    }

    /// The bytes of `region`, when it was read.
    #[must_use]
    pub fn bytes(&self, region: Region) -> Option<&[u8]> {
        if !self.covers(region) {
            return None;
        }
        let start = usize::try_from(region.start()).ok()?;
        let end = usize::try_from(region.end()).ok()?;
        self.bytes.get(start..end)
    }

    /// The full-sized buffer, including synthetic zeroes for unread bytes.
    ///
    /// Its length is not a completeness test. Use [`Self::bytes`] to inspect a
    /// covered region; do not serialize this buffer as a complete radio backup
    /// or decode a field until its actual read coverage is established.
    #[must_use]
    pub fn raw(&self) -> &[u8] {
        &self.bytes
    }

    /// Regions read so far.
    #[must_use]
    pub fn covered(&self) -> &[Region] {
        &self.covered
    }

    /// Consume the buffer after checking only the caller's required regions.
    ///
    /// The coverage map is discarded and every unread byte remains a synthetic
    /// zero, so the returned image is full-sized regardless of what was read.
    /// An empty `required` list checks no coverage, even on a new image. Retain
    /// the coverage map separately if later readers need provenance; prefer
    /// [`Self::bytes`] or [`super::menu::MenuFieldSnapshot`] for sparse
    /// inspection.
    ///
    /// ```
    /// use kenwood_tmd750::{IMAGE_LENGTH, Region};
    /// use kenwood_tmd750::radio::RegionImage;
    ///
    /// let unread = RegionImage::new();
    /// let required = Region::new(8, 48)?;
    /// assert!(!unread.covers(required), "new storage has no read coverage");
    /// assert!(unread.bytes(required).is_none(), "unread bytes are not observations");
    /// // An empty requirement explicitly waives coverage, not just an error.
    /// let storage = unread.into_memory_image(&[])?;
    /// assert_eq!(storage.as_bytes().len(), IMAGE_LENGTH, "storage is full-sized");
    /// assert!(
    ///     storage.as_bytes().iter().all(|byte| *byte == 0),
    ///     "the unread buffer contains only synthetic zeroes"
    /// );
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    ///
    /// # Errors
    ///
    /// Returns [`McpError::RegionNotCovered`] naming the first missing region.
    pub fn into_memory_image(
        self,
        required: &[Region],
    ) -> Result<crate::memory::MemoryImage, Error> {
        for region in required {
            if !self.covers(*region) {
                return Err(McpError::RegionNotCovered {
                    start: region.start(),
                    end: region.end(),
                }
                .into());
            }
        }
        Ok(crate::memory::MemoryImage::from_bytes(self.bytes)?)
    }

    fn store(&mut self, page: Page, data: &[u8]) {
        let start = page.address().as_usize();
        if let Some(target) = self.bytes.get_mut(start..start + data.len()) {
            target.copy_from_slice(data);
        }
    }

    fn mark_covered(&mut self, region: Region) {
        self.covered.push(region);
    }
}

impl<T: Transport> Radio<T> {
    /// Enter programming mode after an identity proof.
    ///
    /// An interrupted entry exchange leaves the protocol state uncertain.
    /// Further CAT or MCP traffic is refused until the operator restores normal
    /// mode and establishes a fresh connection. The expected reply follows the
    /// official program; unexpected replies are never accepted automatically.
    ///
    /// # Errors
    ///
    /// Returns [`McpError::SessionActive`] during an existing session,
    /// [`McpError::RecoveryRequired`] after an incomplete exchange, or
    /// [`McpError::ConnectionRetired`] after an acknowledged exit. Otherwise
    /// propagates identity failures, and returns [`ProtocolError::EntryReply`]
    /// when the radio does not answer the expected entry line.
    pub async fn enter_mcp(&mut self) -> Result<McpSession<'_, T>, Error> {
        self.require_cat()?;
        if self.identity().is_none() {
            let _proven = self.identify().await?;
        }
        tracing::debug!("entering programming mode at {BAUD} baud");
        self.mark_mcp_uncertain();
        self.set_baud(BAUD)?;
        self.write_all(ENTER).await?;
        let reply = self.read_line(TimeoutOperation::ProgrammingEntry).await?;
        if reply != ENTER_RESPONSE {
            return Err(ProtocolError::EntryReply {
                expected: String::from_utf8_lossy(ENTER_RESPONSE).into_owned(),
                reply: String::from_utf8_lossy(&reply).into_owned(),
            }
            .into());
        }
        self.record_mcp_entry_reply(reply);
        self.mark_mcp_ready();
        self.mcp_session()
    }

    /// Borrow an existing MCP session at a fully completed exchange boundary.
    ///
    /// This sends no bytes and does not enter programming mode. It permits
    /// explicit cleanup after dropping a session that was idle. The returned
    /// journal starts empty; retain the previous journal before dropping its
    /// session if it contains writes that need recovery.
    ///
    /// # Errors
    ///
    /// Returns [`McpError::SessionNotActive`] outside programming mode,
    /// [`McpError::RecoveryRequired`] after an incomplete exchange, or
    /// [`McpError::ConnectionRetired`] after an acknowledged exit.
    pub fn mcp_session(&mut self) -> Result<McpSession<'_, T>, Error> {
        self.require_mcp_ready()?;
        let entry_reply = self
            .mcp_entry_reply()
            .ok_or(McpError::RecoveryRequired)?
            .to_vec();
        Ok(McpSession {
            radio: self,
            journal: McpJournal::default(),
            entry_reply,
        })
    }
}

/// An active programming session; holds the radio until [`McpSession::exit`].
///
/// Dropping the session does not send an exit command or restore CAT access.
/// Use [`Radio::mcp_session`] to resume an idle session. If an exchange fails or
/// is cancelled, no further traffic is allowed on this managed connection;
/// an exit byte could otherwise be interpreted as part of an incomplete frame.
#[derive(Debug)]
pub struct McpSession<'a, T: Transport> {
    radio: &'a mut Radio<T>,
    journal: McpJournal,
    entry_reply: Vec<u8>,
}

impl<T: Transport> McpSession<'_, T> {
    /// The exact verified programming-entry reply, excluding its terminator.
    #[must_use]
    pub fn entry_reply(&self) -> &[u8] {
        &self.entry_reply
    }

    /// Whether the last exchange completed and an explicit exit is safe.
    #[must_use]
    pub const fn is_ready(&self) -> bool {
        self.radio.mcp_ready()
    }

    /// Pages written and verified so far.
    #[must_use]
    pub const fn journal(&self) -> &McpJournal {
        &self.journal
    }

    /// Read every page of `regions`, reporting progress per page.
    ///
    /// # Errors
    ///
    /// Returns session-state, header, ACK, transport, and timeout errors.
    /// Read requests and protocol acknowledgments are sent, but no memory is
    /// programmed. An incomplete exchange requires operator recovery.
    pub async fn read_regions(
        &mut self,
        regions: &[Region],
        mut progress: impl FnMut(Progress),
    ) -> Result<RegionImage, Error> {
        self.radio.require_mcp_ready()?;
        let pages: Vec<Page> = regions.iter().flat_map(|region| region.pages()).collect();
        let total = pages.len();
        let mut image = RegionImage::new();
        for (index, page) in pages.into_iter().enumerate() {
            let data = self.read_page(page).await?;
            image.store(page, &data);
            progress(Progress {
                done: index + 1,
                total,
            });
        }
        for region in regions {
            image.mark_covered(*region);
        }
        Ok(image)
    }

    /// Stop at the exit ACK without depending on the old handle afterward.
    ///
    /// The caller must close and drop the transport. Its protocol state remains
    /// blocked so no CAT or second programming session can reuse this handle.
    /// This method performs no baud change, reconnect, retry, or CAT query.
    /// Retain any journal before consuming this session.
    ///
    /// # Errors
    ///
    /// Returns session-state, acknowledgment, transport, and timeout errors.
    /// An incomplete exchange prohibits exit; an uncertain exit is not retried.
    /// Await this future to completion rather than dropping it to cancel I/O.
    pub async fn exit(mut self) -> Result<(), Error> {
        self.acknowledge_exit().await
    }

    /// Refuse a session whose entry identity is outside [`super::WRITE_TARGETS`].
    fn require_write_target(&self) -> Result<(), Error> {
        let identity = self.radio.identity().ok_or(McpError::RecoveryRequired)?;
        if identity.is_write_target() {
            Ok(())
        } else {
            Err(Error::UnsupportedWriteTarget {
                actual: identity.clone(),
            })
        }
    }

    async fn acknowledge_exit(&mut self) -> Result<(), Error> {
        self.radio.require_mcp_ready()?;
        self.radio.mark_mcp_uncertain();
        self.radio.write_all(&[EXIT]).await?;
        expect_ack(self.radio, AckStage::Exit).await?;
        self.radio.mark_retired();
        Ok(())
    }

    /// The write and readback every compare-exchange page goes through.
    async fn write_data_verified(&mut self, page: Page, data: &[u8]) -> Result<(), Error> {
        self.journal.verified.retain(|previous| *previous != page);
        if !self.journal.possibly_written.contains(&page) {
            self.journal.possibly_written.push(page);
        }
        let mut frame = Header::write_request(page).to_vec();
        frame.extend_from_slice(data);
        self.radio.mark_mcp_uncertain();
        self.radio.write_all(&frame).await?;
        expect_ack(self.radio, AckStage::PageWrite).await?;
        self.radio.mark_mcp_ready();
        let read_back = self.read_page(page).await?;
        if let Some(offset) = data
            .iter()
            .zip(read_back.iter())
            .position(|(written, read)| written != read)
        {
            return Err(McpError::VerifyMismatch {
                address: page.address().as_u32(),
                offset,
            }
            .into());
        }
        self.journal.verified.push(page);
        Ok(())
    }

    /// Read one complete page and finish its host/radio acknowledgment exchange.
    ///
    /// Returns exactly [`Page::len`] bytes, expanding a uniform-fill response
    /// when present. The caller selects the page and retains its address and
    /// coverage; this method does not infer canonical configuration regions,
    /// construct a dense image, or alter the write journal. Use this boundary
    /// for incremental captures without allocating a whole-image buffer.
    ///
    /// Cancellation should be checked between completed calls. Dropping an
    /// in-flight read or receiving an incomplete exchange leaves the session
    /// uncertain and prohibits subsequent protocol I/O, including exit. A
    /// successful call restores MCP readiness and stays in programming mode.
    /// No automatic retry occurs.
    ///
    /// # Errors
    ///
    /// Rejects a session that is not MCP-ready before any page traffic. Returns
    /// transport, timeout, header-address/length, response-command, or
    /// acknowledgment errors while preserving the uncertain session state.
    /// A matching read-request echo returns
    /// [`ProtocolError::UnexpectedPageResponse`] before payload or ACK I/O;
    /// valid header framing does not make it a data response.
    pub async fn read_page(&mut self, page: Page) -> Result<Vec<u8>, Error> {
        self.radio.require_mcp_ready()?;
        let request = Header::read_request(page);
        self.radio.mark_mcp_uncertain();
        self.radio.write_all(&request).await?;
        let reply = self
            .radio
            .read_exact(HEADER_LEN, TimeoutOperation::McpReadHeader)
            .await?;
        let reply: [u8; HEADER_LEN] = reply.as_slice().try_into().map_err(|_| {
            Error::Protocol(ProtocolError::HeaderEcho {
                expected: request,
                actual: [0; HEADER_LEN],
            })
        })?;
        let header = Header::decode(&reply)?;
        if header.page() != page {
            return Err(ProtocolError::HeaderEcho {
                expected: request,
                actual: reply,
            }
            .into());
        }
        let data = match header.command() {
            HeaderCommand::Write => {
                self.radio
                    .read_exact(page.len(), TimeoutOperation::McpPageData)
                    .await?
            }
            HeaderCommand::Fill => {
                let fill = self
                    .radio
                    .read_exact(1, TimeoutOperation::McpFillByte)
                    .await?;
                let byte = fill.first().copied().unwrap_or_default();
                vec![byte; page.len()]
            }
            other @ HeaderCommand::Read => {
                return Err(ProtocolError::UnexpectedPageResponse { command: other }.into());
            }
        };
        self.radio.write_all(&[ACK]).await?;
        expect_ack(self.radio, AckStage::PageRead).await?;
        self.radio.mark_mcp_ready();
        Ok(data)
    }
}

async fn expect_ack<T: Transport>(radio: &mut Radio<T>, stage: AckStage) -> Result<(), Error> {
    let reply = radio
        .read_exact(1, TimeoutOperation::Acknowledgement(stage))
        .await?;
    match reply.first() {
        Some(&ACK) => Ok(()),
        Some(&byte) => Err(ProtocolError::MissingAck { stage, byte }.into()),
        None => Err(ProtocolError::MissingAck { stage, byte: 0 }.into()),
    }
}
