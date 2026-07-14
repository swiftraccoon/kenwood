//! Schema-driven MCP menu writes.
//!
//! This module bridges [`PatchSet`] to the radio's
//! sparse page read/modify/write primitive. Callers can plan many field
//! changes first, then apply every touched page during one MCP programming
//! session without downloading or uploading a complete memory image.

use crate::error::Error;
use crate::memory::PatchSet;
use crate::transport::Transport;

use super::Radio;

impl<T: Transport> Radio<T> {
    /// Apply schema-generated menu patches in one MCP programming session.
    ///
    /// Every touched page is freshly read before any write. Patches own only
    /// their declared bits, so unrelated settings sharing a byte are
    /// preserved. Only pages whose resulting contents differ are written,
    /// and each write uses the normal MCP read-back verification.
    ///
    /// An empty patch set is a no-op and does not enter programming mode. The
    /// returned page numbers are the pages that were actually written.
    ///
    /// # Errors
    ///
    /// Returns [`Error::MemoryWriteProtected`] before I/O if the patch set
    /// touches the factory-calibration region. Other errors report MCP entry,
    /// page read, verified write, exit, or reconnect failures.
    pub async fn apply_menu_patches(&mut self, patches: &PatchSet) -> Result<Vec<u16>, Error> {
        let pages: Vec<u16> = patches.pages().collect();
        self.modify_memory_pages(&pages, |page, data| {
            if let Some(page_patch) = patches.page(page) {
                page_patch.apply_to_page(data);
            }
        })
        .await
    }
}
