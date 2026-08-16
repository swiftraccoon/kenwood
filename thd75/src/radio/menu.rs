//! Schema-driven MCP menu writes.
//!
//! This module bridges [`PatchSet`] to the radio's
//! sparse page read/modify/write primitive. Callers can plan many field
//! changes first, then apply every touched page during one MCP programming
//! session without downloading or uploading a complete memory image.

use crate::error::Error;
use crate::memory::menu_fields::MenuField;
use crate::memory::schema::{DecodedFieldValue, SchemaError};
use crate::memory::{
    MCP_D75_SCHEMA_FIRMWARE, MCP_D75_SCHEMA_FIRMWARE_IDENTITIES, MCP_D75_SCHEMA_MODEL, PatchSet,
    is_supported_mcp_d75_schema_target,
};
use crate::protocol::programming::{self, McpPage, WritableMcpPage};
use crate::transport::Transport;

use super::Radio;
use super::programming::McpPageExchange;

/// Sparse MCP snapshot covering a chosen set of menu fields.
///
/// Produced by [`Radio::read_menu_snapshot`]. Field values decode on demand
/// from the fetched pages, and the raw pages double as the expected bytes
/// for a stale-safe [`Radio::compare_exchange_menu_patches`] write.
#[derive(Debug)]
pub struct MenuFieldSnapshot {
    /// Zero-filled full-size image with the fetched pages copied in, so
    /// [`crate::memory::FieldDescriptor::read`] can address it directly.
    image: Vec<u8>,
    /// The fetched pages, ascending, exactly as read from the radio.
    pages: Vec<(McpPage, [u8; programming::PAGE_SIZE])>,
}

impl MenuFieldSnapshot {
    /// Build a snapshot from already-fetched pages.
    ///
    /// For callers that cache pages across operations (an FFI layer keeping
    /// its own snapshot protocol, or an offline re-decode after locally
    /// applying a patch) and need the same on-demand typed decoding that
    /// [`Radio::read_menu_snapshot`] returns.
    ///
    /// # Errors
    ///
    /// Returns [`Error::McpPageOutOfRange`] when a page index lies outside
    /// the radio's image; impossible for pages produced by this library's
    /// validated [`McpPage`] reads, but typed rather than assumed.
    pub fn from_pages(pages: Vec<(McpPage, [u8; programming::PAGE_SIZE])>) -> Result<Self, Error> {
        let mut image = vec![0_u8; programming::TOTAL_SIZE];
        for (page, data) in &pages {
            let slot = image
                .chunks_exact_mut(programming::PAGE_SIZE)
                .nth(usize::from(page.as_raw()))
                .ok_or(Error::McpPageOutOfRange {
                    page: page.as_raw(),
                    total_pages: programming::TOTAL_PAGES,
                })?;
            slot.copy_from_slice(data);
        }
        Ok(Self { image, pages })
    }

    /// Decode one menu field from the snapshot.
    ///
    /// The field's pages must have been part of the snapshot's read set;
    /// decoding a field outside it sees zero-filled bytes and typically
    /// fails its domain validation rather than returning invented data.
    ///
    /// # Errors
    ///
    /// Returns the schema decode error for malformed or out-of-domain
    /// stored bytes.
    pub fn value(&self, field: &MenuField) -> Result<DecodedFieldValue, SchemaError> {
        field.descriptor.read(&self.image)
    }

    /// The fetched pages, ascending, exactly as read from the radio.
    #[must_use]
    pub fn pages(&self) -> &[(McpPage, [u8; programming::PAGE_SIZE])] {
        &self.pages
    }

    /// The fetched bytes for one page, when it was part of the read set.
    #[must_use]
    pub fn page(&self, page: McpPage) -> Option<&[u8; programming::PAGE_SIZE]> {
        self.pages
            .iter()
            .find(|(read, _)| *read == page)
            .map(|(_, data)| data)
    }
}

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

        self.verify_mcp_schema_target().await?;

        self.modify_memory_pages(&pages, |page, data| {
            if let Some(page_patch) = patches.page(page) {
                page_patch.apply_to_page(data);
            }
        })
        .await
    }

    /// Read a sparse, typed snapshot of the given menu fields.
    ///
    /// Computes the exact page span of every field (multi-byte fields can
    /// span pages), proves the MCP schema target, fetches only those pages
    /// in one programming session, and returns a [`MenuFieldSnapshot`] that
    /// decodes values on demand and doubles as the expected bytes for
    /// [`Radio::compare_exchange_menu_patches`].
    ///
    /// An empty field list returns an empty snapshot without any I/O.
    ///
    /// # Errors
    ///
    /// Returns [`Error::McpUnsupportedSchemaTarget`] before MCP entry for an
    /// unqualified radio, [`Error::Schema`] for a field whose span leaves
    /// the image, and MCP entry, page-read, exit, or reconnect errors from
    /// the sparse read.
    pub async fn read_menu_snapshot(
        &mut self,
        fields: &[&MenuField],
    ) -> Result<MenuFieldSnapshot, Error> {
        if fields.is_empty() {
            return Ok(MenuFieldSnapshot {
                image: vec![0; programming::TOTAL_SIZE],
                pages: Vec::new(),
            });
        }

        self.verify_mcp_schema_target().await?;

        let mut span: Vec<McpPage> = Vec::new();
        for field in fields {
            span.extend(field.descriptor.pages()?);
        }
        let pages = self.read_sparse_memory_pages(&span).await?;
        MenuFieldSnapshot::from_pages(pages)
    }

    /// Apply menu patches only if the radio's pages still match a snapshot.
    ///
    /// The stale-safe sibling of [`Radio::apply_menu_patches_via_mcp`]: each
    /// patched page's expected bytes come from `snapshot`, the patch is
    /// applied on top to form the replacement, and the whole batch goes
    /// through the compare-exchange primitive, which re-reads every page
    /// live and writes nothing on any mismatch.
    ///
    /// An empty patch set is a no-op and does not enter programming mode.
    /// Returns the pages that were written.
    ///
    /// # Errors
    ///
    /// Returns [`Error::McpUnsupportedSchemaTarget`] before MCP entry for an
    /// unqualified radio, [`Error::McpSnapshotPageMissing`] when a patched
    /// page was never read into the snapshot, and
    /// [`Error::McpPageExchange`] wrapping the compare-exchange outcome
    /// (including which pages may already have changed on a partial
    /// failure).
    pub async fn compare_exchange_menu_patches(
        &mut self,
        patches: &PatchSet,
        snapshot: &MenuFieldSnapshot,
    ) -> Result<Vec<WritableMcpPage>, Error> {
        let pages: Vec<WritableMcpPage> = patches.pages().collect();
        if pages.is_empty() {
            return Ok(Vec::new());
        }

        self.verify_mcp_schema_target().await?;

        let mut exchanges = Vec::with_capacity(pages.len());
        for page in pages {
            let expected = snapshot
                .page(page.page())
                .ok_or(Error::McpSnapshotPageMissing { page })?;
            let mut replacement = *expected;
            if let Some(page_patch) = patches.page(page) {
                page_patch.apply_to_page(&mut replacement);
            }
            exchanges.push(McpPageExchange::new(page, *expected, replacement));
        }
        let written = self.compare_exchange_memory_pages(&exchanges).await?;
        Ok(written)
    }

    /// Prove the connected radio is the exact MCP-D75 schema target.
    ///
    /// Runs the `ID` and `FV` identity exchanges and checks them against the
    /// generated schema's qualified model and firmware list. Registry-driven
    /// MCP operations run this internally; standalone callers can use it to
    /// fail fast before composing their own MCP work.
    ///
    /// # Errors
    ///
    /// Returns [`Error::McpUnsupportedSchemaTarget`] for any other model or
    /// firmware, and propagates identity-exchange failures.
    pub async fn verify_mcp_schema_target(&mut self) -> Result<(), Error> {
        let identity = self.identify().await?;
        let firmware = self.get_firmware_version().await?;
        if is_supported_mcp_d75_schema_target(identity.model, &firmware) {
            Ok(())
        } else {
            Err(Error::McpUnsupportedSchemaTarget {
                expected_model: MCP_D75_SCHEMA_MODEL,
                expected_firmware: MCP_D75_SCHEMA_FIRMWARE,
                accepted_firmware_identities: MCP_D75_SCHEMA_FIRMWARE_IDENTITIES,
                actual_model: identity.model,
                actual_firmware: firmware,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::MenuFieldSnapshot;
    use crate::error::Error;
    use crate::memory::PatchPlanner;
    use crate::memory::menu_fields::menu_field;
    use crate::memory::schema::{DecodedFieldValue, FieldValue};
    use crate::protocol::programming;
    use crate::radio::Radio;
    use crate::transport::MockTransport;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    /// Build one MCP `W` response frame for a full page.
    fn build_w_response(page: u16, data: &[u8; programming::PAGE_SIZE]) -> Vec<u8> {
        let [page_hi, page_lo] = page.to_be_bytes();
        let mut response = vec![b'W', page_hi, page_lo, 0, 0];
        response.extend_from_slice(data);
        response
    }

    #[tokio::test]
    async fn read_menu_snapshot_fetches_only_the_span_and_decodes() -> TestResult {
        let beep = menu_field("radio.Beep").ok_or("registry entry missing: radio.Beep")?;
        let volume =
            menu_field("radio.BeepVolume").ok_or("registry entry missing: radio.BeepVolume")?;

        let mut stored = [0_u8; programming::PAGE_SIZE];
        stored[0x71] = 1;
        stored[0x72] = 3;

        let mut mock = MockTransport::new();
        mock.expect(b"ID\r", b"ID TH-D75\r");
        mock.expect(b"FV\r", b"FV 1.03\r");
        mock.expect(programming::ENTER_PROGRAMMING, programming::ENTER_RESPONSE);
        let read = programming::build_read_command(programming::McpPage::new(0x10)?);
        mock.expect(&read, &build_w_response(0x10, &stored));
        mock.expect(&[programming::ACK], &[programming::ACK]);
        mock.expect(&[programming::EXIT], &[programming::ACK]);
        mock.expect_reopen(Ok(()));
        mock.expect(b"ID\r", b"ID TH-D75\r");
        let mut radio = Radio::new(mock);

        let snapshot = radio.read_menu_snapshot(&[beep, volume]).await?;
        assert_eq!(
            snapshot.pages().len(),
            1,
            "both fields share one page, read exactly once"
        );
        let value = snapshot.value(beep)?;
        assert!(
            matches!(value, DecodedFieldValue::Bool(true)),
            "beep must decode from the fetched page: {value:?}"
        );
        radio.transport.assert_complete();
        Ok(())
    }

    #[tokio::test]
    async fn read_menu_snapshot_is_a_no_op_for_no_fields() -> TestResult {
        let mut radio = Radio::new(MockTransport::new());
        let snapshot = radio.read_menu_snapshot(&[]).await?;
        assert!(snapshot.pages().is_empty());
        radio.transport.assert_complete();
        Ok(())
    }

    #[tokio::test]
    async fn compare_exchange_menu_patches_requires_snapshot_coverage() -> TestResult {
        let beep = menu_field("radio.Beep").ok_or("registry entry missing: radio.Beep")?;
        let mut planner = PatchPlanner::new();
        let _chained = planner.set(&beep.descriptor, FieldValue::Bool(false))?;
        let patches = planner.finish()?;
        let empty_snapshot = MenuFieldSnapshot::from_pages(Vec::new())?;

        let mut mock = MockTransport::new();
        mock.expect(b"ID\r", b"ID TH-D75\r");
        mock.expect(b"FV\r", b"FV 1.03\r");
        let mut radio = Radio::new(mock);

        let result = radio
            .compare_exchange_menu_patches(&patches, &empty_snapshot)
            .await;
        assert!(
            matches!(
                result,
                Err(Error::McpSnapshotPageMissing { page }) if page.as_raw() == 0x10
            ),
            "a patched page absent from the snapshot must be refused: {result:?}"
        );
        radio.transport.assert_complete();
        Ok(())
    }

    #[tokio::test]
    async fn compare_exchange_menu_patches_builds_exchanges_from_the_snapshot() -> TestResult {
        let beep = menu_field("radio.Beep").ok_or("registry entry missing: radio.Beep")?;
        let mut planner = PatchPlanner::new();
        let _chained = planner.set(&beep.descriptor, FieldValue::Bool(false))?;
        let patches = planner.finish()?;

        let mut stored = [0_u8; programming::PAGE_SIZE];
        stored[0x71] = 1;
        let mut replacement = stored;
        replacement[0x71] = 0;
        let snapshot =
            MenuFieldSnapshot::from_pages(vec![(programming::McpPage::new(0x10)?, stored)])?;

        let mut mock = MockTransport::new();
        mock.expect(b"ID\r", b"ID TH-D75\r");
        mock.expect(b"FV\r", b"FV 1.03\r");
        mock.expect(programming::ENTER_PROGRAMMING, programming::ENTER_RESPONSE);
        let read = programming::build_read_command(programming::McpPage::new(0x10)?);
        mock.expect(&read, &build_w_response(0x10, &stored));
        mock.expect(&[programming::ACK], &[programming::ACK]);
        let write = programming::build_write_command(
            programming::WritableMcpPage::new(0x10)?,
            &replacement,
        );
        mock.expect(&write, &[programming::ACK]);
        mock.expect(&read, &build_w_response(0x10, &replacement));
        mock.expect(&[programming::ACK], &[programming::ACK]);
        mock.expect(&[programming::EXIT], &[programming::ACK]);
        mock.expect_reopen(Ok(()));
        mock.expect(b"ID\r", b"ID TH-D75\r");
        let mut radio = Radio::new(mock);

        let written = radio
            .compare_exchange_menu_patches(&patches, &snapshot)
            .await?;
        assert_eq!(written, vec![programming::WritableMcpPage::new(0x10)?]);
        radio.transport.assert_complete();
        Ok(())
    }

    #[tokio::test]
    async fn verify_mcp_schema_target_accepts_the_qualified_radio() -> TestResult {
        let mut mock = MockTransport::new();
        mock.expect(b"ID\r", b"ID TH-D75\r");
        mock.expect(b"FV\r", b"FV 1.03\r");
        let mut radio = Radio::new(mock);
        radio.verify_mcp_schema_target().await?;
        radio.transport.assert_complete();
        Ok(())
    }

    #[tokio::test]
    async fn verify_mcp_schema_target_refuses_unqualified_firmware() -> TestResult {
        // Note: all three qualified identities (1.03, 1.03.000, 1.03.AZM)
        // share the stock memory layout and pass; a future vendor release
        // must be refused until its offsets are re-qualified.
        let mut mock = MockTransport::new();
        mock.expect(b"ID\r", b"ID TH-D75\r");
        mock.expect(b"FV\r", b"FV 1.04\r");
        let mut radio = Radio::new(mock);
        let result = radio.verify_mcp_schema_target().await;
        assert!(
            matches!(
                result,
                Err(Error::McpUnsupportedSchemaTarget { ref actual_firmware, .. })
                    if actual_firmware.as_str() == "1.04"
            ),
            "unqualified firmware must be refused before MCP entry: {result:?}"
        );
        radio.transport.assert_complete();
        Ok(())
    }
}
