//! The MCP session: region reads, verified page writes, exit, recovery.

use super::{Progress, Radio};
use crate::error::{Error, McpError, ProtocolError};
use crate::protocol::mcp::{
    ACK, BAUD, ENTER, ENTER_RESPONSE, EXIT, FILL, HEADER_LEN, Header, PagePatch, WRITE,
    read_request, regions, write_request,
};
use crate::transport::Transport;
use crate::types::{IMAGE_LENGTH, Page, Region};

/// Pages the radio may have changed and pages confirmed by read-back.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct McpJournal {
    /// Pages a write was started for.
    pub possibly_written: Vec<Page>,
    /// Pages read back equal to the intended bytes.
    pub verified: Vec<Page>,
}

/// Outcome of a verified write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpWriteReport {
    /// Pages confirmed by read-back.
    pub verified_pages: Vec<Page>,
    /// Pages written but not confirmed (empty on success).
    pub possibly_written_pages: Vec<Page>,
}

/// Outcome of [`Radio::recover`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoveryReport {
    /// Journaled pages whose current bytes carry the intended patch.
    pub applied: Vec<Page>,
    /// Journaled pages that do not.
    pub pending: Vec<Page>,
}

/// Bytes read so far, with the regions they cover.
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

    /// The whole buffer (unread bytes are zero).
    #[must_use]
    pub fn raw(&self) -> &[u8] {
        &self.bytes
    }

    /// Regions read so far.
    #[must_use]
    pub fn covered(&self) -> &[Region] {
        &self.covered
    }

    /// Convert into a full image once every `required` region was read.
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
    /// # Errors
    ///
    /// Propagates identity failures; returns [`ProtocolError::EntryReply`]
    /// when the radio does not answer the expected entry line.
    pub async fn enter_mcp(&mut self) -> Result<McpSession<'_, T>, Error> {
        if self.identity().is_none() {
            let _proven = self.identify().await?;
        }
        tracing::info!("entering programming mode at {BAUD} baud");
        self.set_baud(BAUD)?;
        self.write_all(ENTER).await?;
        let reply = self.read_line("programming mode entry").await?;
        if reply != ENTER_RESPONSE {
            return Err(ProtocolError::EntryReply {
                expected: String::from_utf8_lossy(ENTER_RESPONSE).into_owned(),
                reply: String::from_utf8_lossy(&reply).into_owned(),
            }
            .into());
        }
        Ok(McpSession {
            radio: self,
            journal: McpJournal::default(),
        })
    }

    /// After an interrupted write, re-enter MCP, read the journaled pages,
    /// and report which already carry their intended patch. Never writes.
    ///
    /// # Errors
    ///
    /// Propagates entry, read, and exit failures.
    pub async fn recover(
        &mut self,
        journal: &McpJournal,
        intended: &[PagePatch],
    ) -> Result<RecoveryReport, Error> {
        let mut session = self.enter_mcp().await?;
        let mut report = RecoveryReport {
            applied: Vec::new(),
            pending: Vec::new(),
        };
        for page in &journal.possibly_written {
            let current = session.read_page(*page).await?;
            let applied = intended
                .iter()
                .filter(|patch| patch.page == *page)
                .all(|patch| patch.is_applied(&current));
            if applied {
                report.applied.push(*page);
            } else {
                report.pending.push(*page);
            }
        }
        session.exit().await?;
        Ok(report)
    }
}

/// An active programming session; holds the radio until [`McpSession::exit`].
#[derive(Debug)]
pub struct McpSession<'a, T: Transport> {
    radio: &'a mut Radio<T>,
    journal: McpJournal,
}

impl<T: Transport> McpSession<'_, T> {
    /// Pages written and verified so far.
    #[must_use]
    pub const fn journal(&self) -> &McpJournal {
        &self.journal
    }

    /// Read every page of `regions`, reporting progress per page.
    ///
    /// # Errors
    ///
    /// Returns header, ACK, transport, and timeout errors; nothing is written.
    pub async fn read_regions(
        &mut self,
        regions: &[Region],
        mut progress: impl FnMut(Progress),
    ) -> Result<RegionImage, Error> {
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

    /// Apply each patch to a freshly read page, write it, read it back, and
    /// compare. Refuses every patch outside the writable regions before any
    /// write. On failure the error carries the journal counts.
    ///
    /// # Errors
    ///
    /// Returns [`McpError::PageNotWritable`] up front, or
    /// [`McpError::Interrupted`] wrapping the failure that stopped the write.
    pub async fn write_pages_verified(
        &mut self,
        patches: &[PagePatch],
        mut progress: impl FnMut(Progress),
    ) -> Result<McpWriteReport, Error> {
        for patch in patches {
            if !regions::is_writable_page(patch.page) {
                return Err(McpError::PageNotWritable {
                    address: patch.page.address().as_u32(),
                    len: u16::try_from(patch.page.len()).unwrap_or(u16::MAX),
                }
                .into());
            }
        }
        let total = patches.len();
        for (index, patch) in patches.iter().enumerate() {
            if let Err(error) = self.write_page_verified(patch).await {
                return Err(McpError::Interrupted {
                    operation: "verified page write",
                    possibly_written: self.journal.possibly_written.len(),
                    verified: self.journal.verified.len(),
                    source: Box::new(error),
                }
                .into());
            }
            progress(Progress {
                done: index + 1,
                total,
            });
        }
        Ok(McpWriteReport {
            verified_pages: self.journal.verified.clone(),
            possibly_written_pages: self
                .journal
                .possibly_written
                .iter()
                .copied()
                .filter(|page| !self.journal.verified.contains(page))
                .collect(),
        })
    }

    /// Send `E`, expect the ACK, restore the CAT baud rate.
    ///
    /// # Errors
    ///
    /// Returns [`ProtocolError::MissingAck`], transport, and timeout errors.
    pub async fn exit(self) -> Result<(), Error> {
        let radio = self.radio;
        radio.write_all(&[EXIT]).await?;
        let cat_baud = radio.cat_baud();
        let result = expect_ack(radio, "MCP exit").await;
        radio.set_baud(cat_baud)?;
        result
    }

    async fn write_page_verified(&mut self, patch: &PagePatch) -> Result<(), Error> {
        let mut data = self.read_page(patch.page).await?;
        patch.apply(&mut data);
        self.journal.possibly_written.push(patch.page);
        let mut frame = write_request(patch.page).to_vec();
        frame.extend_from_slice(&data);
        self.radio.write_all(&frame).await?;
        expect_ack(self.radio, "MCP page write").await?;
        let read_back = self.read_page(patch.page).await?;
        if let Some(offset) = data
            .iter()
            .zip(read_back.iter())
            .position(|(written, read)| written != read)
        {
            return Err(McpError::VerifyMismatch {
                address: patch.page.address().as_u32(),
                offset,
            }
            .into());
        }
        self.journal.verified.push(patch.page);
        Ok(())
    }

    pub(crate) async fn read_page(&mut self, page: Page) -> Result<Vec<u8>, Error> {
        let request = read_request(page);
        self.radio.write_all(&request).await?;
        let reply = self.radio.read_exact(HEADER_LEN, "MCP read header").await?;
        let reply: [u8; HEADER_LEN] = reply.as_slice().try_into().map_err(|_| {
            Error::Protocol(ProtocolError::HeaderEcho {
                expected: request,
                actual: [0; HEADER_LEN],
            })
        })?;
        let header = Header::decode(&reply)?;
        if header.address != page.address() || header.len != page.len() {
            return Err(ProtocolError::HeaderEcho {
                expected: request,
                actual: reply,
            }
            .into());
        }
        let data = match header.command {
            WRITE => self.radio.read_exact(page.len(), "MCP page data").await?,
            FILL => {
                let fill = self.radio.read_exact(1, "MCP fill byte").await?;
                let byte = fill.first().copied().unwrap_or_default();
                vec![byte; page.len()]
            }
            other => return Err(ProtocolError::UnknownHeaderCommand { command: other }.into()),
        };
        self.radio.write_all(&[ACK]).await?;
        expect_ack(self.radio, "MCP page read").await?;
        Ok(data)
    }
}

async fn expect_ack<T: Transport>(radio: &mut Radio<T>, stage: &'static str) -> Result<(), Error> {
    let reply = radio.read_exact(1, stage).await?;
    match reply.first() {
        Some(&ACK) => Ok(()),
        Some(&byte) => Err(ProtocolError::MissingAck { stage, byte }.into()),
        None => Err(ProtocolError::MissingAck { stage, byte: 0 }.into()),
    }
}
