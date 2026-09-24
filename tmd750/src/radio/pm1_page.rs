//! Private single-frame writes for the closed, bounded text targets: the PM1
//! name page, the PM Off MY1 page, and the channel name-table pages.

use super::Radio;
use crate::error::{Error, ProtocolError};
use crate::protocol::mcp::{ACK, write_request};
use crate::types::{
    Address, CHANNEL_NAME_SIZE, CHANNEL_NAMES_OFFSET, PAGE_SIZE, PHYSICAL_CHANNEL_COUNT, Page,
};
use kenwood_transport::Transport;

/// The pages this private frame writer can reach.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum FixedTextTarget {
    Pm1,
    My1,
    /// One complete page of the channel name table, admitted by
    /// [`Self::channel_names`].
    ChannelNames(Page),
}

impl FixedTextTarget {
    /// Admit one complete 256-byte page on the channel name-table grid, from
    /// the page holding channel 0 through the page holding the last physical
    /// channel; any other page is refused.
    pub(super) fn channel_names(page: Page) -> Option<Self> {
        let start = page.address().as_usize();
        let table_len = CHANNEL_NAME_SIZE * usize::from(PHYSICAL_CHANNEL_COUNT);
        let last_page_start = CHANNEL_NAMES_OFFSET + (table_len - 1) / PAGE_SIZE * PAGE_SIZE;
        let on_grid = start >= CHANNEL_NAMES_OFFSET
            && start <= last_page_start
            && (start - CHANNEL_NAMES_OFFSET).is_multiple_of(PAGE_SIZE);
        (on_grid && page.len() == PAGE_SIZE).then_some(Self::ChannelNames(page))
    }

    pub(super) fn page(self) -> Result<Page, Error> {
        let address = match self {
            Self::Pm1 => 323_584,
            Self::My1 => 331_776,
            Self::ChannelNames(page) => return Ok(page),
        };
        Ok(Page::new(Address::new(address)?, PAGE_SIZE)?)
    }
}

impl<T: Transport> Radio<T> {
    /// Complete the frame/ACK exchange after the caller's fixed-scope checks.
    ///
    /// Each caller validates identity, the canonical page, the complete observed
    /// before-image, and the recorded intent before invoking this private helper.
    /// An interrupted write or a missing ACK leaves the handle uncertain.
    pub(super) async fn write_pm1_frame(
        &mut self,
        after: &[u8; PAGE_SIZE],
        stage: &'static str,
    ) -> Result<(), Error> {
        self.write_fixed_text_frame(FixedTextTarget::Pm1, after, stage)
            .await
    }

    /// Send one fixed target's frame after its driver has checked the exact
    /// identity, whole-page baseline, guards, and recorded intent.
    pub(super) async fn write_fixed_text_frame(
        &mut self,
        target: FixedTextTarget,
        after: &[u8; PAGE_SIZE],
        stage: &'static str,
    ) -> Result<(), Error> {
        self.require_mcp_ready()?;
        let page = target.page()?;
        let mut frame = write_request(page).to_vec();
        frame.extend_from_slice(after);
        self.mark_mcp_uncertain();
        self.write_all(&frame).await?;
        let reply = self.read_exact(1, stage).await?;
        if reply.as_slice() != [ACK] {
            return Err(Error::Protocol(ProtocolError::MissingAck {
                stage,
                byte: reply.first().copied().unwrap_or_default(),
            }));
        }
        self.mark_mcp_ready();
        Ok(())
    }
}
