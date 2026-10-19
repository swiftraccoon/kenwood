//! Channel names written in an MCP session.

use std::io;

use super::{McpCompareExchangeReport, McpSession, PageReplacement};
use crate::error::Error;
use crate::memory::{ChannelNamePlan, MemoryImage};
use crate::radio::Progress;
use kenwood_transport::Transport;

impl<T: Transport> McpSession<'_, T> {
    /// Read the plan's name pages, then compare-exchange the ones whose names
    /// change.
    ///
    /// Each touched page is read once to build its expected bytes;
    /// [`Self::compare_exchange_pages`] then reads it again before its write
    /// and verifies it after, with `before_write` and `progress` as its
    /// callbacks. A plan that changes nothing writes nothing. No exit, retry
    /// or CAT is performed.
    ///
    /// # Errors
    ///
    /// Returns session-state errors and [`Error::UnsupportedWriteTarget`]
    /// before any page traffic, and the [`Self::read_page`] and
    /// [`Self::compare_exchange_pages`] errors.
    pub async fn write_channel_names(
        &mut self,
        plan: &ChannelNamePlan,
        before_write: impl FnMut(&PageReplacement) -> io::Result<()>,
        progress: impl FnMut(Progress),
    ) -> Result<McpCompareExchangeReport, Error> {
        self.radio.require_mcp_ready()?;
        self.require_write_target()?;
        let mut image = MemoryImage::blank().into_bytes();
        for page in plan.pages() {
            let data = self.read_page(page).await?;
            for (destination, source) in image.iter_mut().skip(page.address().as_usize()).zip(data)
            {
                *destination = source;
            }
        }
        let replacements = plan.page_replacements(&MemoryImage::from_bytes(image)?)?;
        self.compare_exchange_pages(&replacements, before_write, progress)
            .await
    }
}
