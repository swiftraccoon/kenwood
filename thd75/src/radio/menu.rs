//! Schema-driven MCP menu writes.
//!
//! This module bridges [`PatchSet`] to the radio's
//! sparse page read/modify/write primitive. Callers can plan many field
//! changes first, then apply every touched page during one MCP programming
//! session without downloading or uploading a complete memory image.

use crate::error::Error;
use crate::memory::{
    MCP_D75_SCHEMA_FIRMWARE, MCP_D75_SCHEMA_FIRMWARE_IDENTITIES, MCP_D75_SCHEMA_MODEL, PatchSet,
    is_supported_mcp_d75_schema_target,
};
use crate::protocol::programming::WritableMcpPage;
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
    /// Patch sets built by [`PatchPlanner`](crate::memory::PatchPlanner) are
    /// validated at plan time and can never address the factory-calibration
    /// region; the radio layer still independently re-checks every requested
    /// page before any I/O.
    ///
    /// # Errors
    ///
    /// Returns [`Error::McpUnsupportedSchemaTarget`] before entering MCP if
    /// the connected radio does not report the exact model and one of the
    /// CAT firmware identities qualified for the generated offsets.
    /// Returns [`Error::McpWriteProtected`] before I/O if the page set
    /// touches the factory-calibration region. Other errors report MCP entry,
    /// page read, verified write, exit, or reconnect failures. If a write or
    /// its verification fails partway through the batch, pages written
    /// earlier in the same session remain changed on the radio.
    pub async fn apply_menu_patches_via_mcp(
        &mut self,
        patches: &PatchSet,
    ) -> Result<Vec<WritableMcpPage>, Error> {
        let pages: Vec<WritableMcpPage> = patches.pages().collect();
        if pages.is_empty() {
            return Ok(Vec::new());
        }

        let identity = self.identify().await?;
        let firmware = self.get_firmware_version().await?;
        if !is_supported_mcp_d75_schema_target(identity.model, &firmware) {
            return Err(Error::McpUnsupportedSchemaTarget {
                expected_model: MCP_D75_SCHEMA_MODEL,
                expected_firmware: MCP_D75_SCHEMA_FIRMWARE,
                accepted_firmware_identities: MCP_D75_SCHEMA_FIRMWARE_IDENTITIES,
                actual_model: identity.model,
                actual_firmware: firmware,
            });
        }

        self.modify_memory_pages(&pages, |page, data| {
            if let Some(page_patch) = patches.page(page) {
                page_patch.apply_to_page(data);
            }
        })
        .await
    }
}
