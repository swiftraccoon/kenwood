//! Complete-page optimistic comparison before any batch write.

use std::io;

use super::{McpSession, Progress};
use crate::error::{Error, McpError};
use crate::protocol::mcp::regions;
use crate::radio::menu::{MenuUpdateError, MenuUpdatePlan};
use crate::types::Page;
use kenwood_transport::Transport;

/// Immutable expected and desired bytes for one canonical writable page.
///
/// A canonical page is one complete unit in the existing writable-region walk,
/// including its short fragments. Construction proves shape and scope only;
/// it does not qualify a radio's firmware or authorize a settings write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageReplacement {
    page: Page,
    expected: Vec<u8>,
    replacement: Vec<u8>,
}

impl PageReplacement {
    /// Validate and own the complete expected and replacement page images.
    ///
    /// Equal images are admitted as a read-only comparison, not a forced write.
    ///
    /// # Errors
    ///
    /// Rejects protected ranges, noncanonical fragments, and either byte array
    /// whose length differs from the canonical page length.
    pub fn new(page: Page, expected: &[u8], replacement: &[u8]) -> Result<Self, McpError> {
        validate_page(page, expected.len(), replacement.len())?;
        Ok(Self {
            page,
            expected: expected.to_vec(),
            replacement: replacement.to_vec(),
        })
    }

    /// The exact canonical transfer unit.
    #[must_use]
    pub const fn page(&self) -> Page {
        self.page
    }

    /// Immutable complete before-image required by the fresh comparison.
    #[must_use]
    pub fn expected(&self) -> &[u8] {
        &self.expected
    }

    /// Immutable complete bytes to write and immediately verify.
    #[must_use]
    pub fn replacement(&self) -> &[u8] {
        &self.replacement
    }

    /// Whether fresh equality is sufficient without a write or durable intent.
    #[must_use]
    pub fn is_noop(&self) -> bool {
        self.expected == self.replacement
    }
}

/// Successful results for this invocation, not the session's earlier writes.
///
/// All vectors preserve the caller's order. This report establishes complete
/// preflight comparisons and immediate readback, not atomicity, durability
/// across exit/re-entry, or persistence across a power cycle.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct McpCompareExchangeReport {
    /// Every page whose fresh bytes matched its complete before-image.
    pub compared_pages: Vec<Page>,
    /// Changed pages acknowledged and immediately read back byte-for-byte.
    pub verified_pages: Vec<Page>,
    /// Equal expected/replacement pages compared without any W command.
    pub unchanged_pages: Vec<Page>,
}

fn validate_page(page: Page, expected_len: usize, replacement_len: usize) -> Result<(), McpError> {
    if !regions::is_writable_page(page) {
        return Err(McpError::PageNotWritable {
            address: page.address().as_u32(),
            len: u16::try_from(page.len()).unwrap_or(u16::MAX),
        });
    }
    if regions::writable_page_for(page.address()) != Some(page) {
        return Err(McpError::NonCanonicalPage {
            address: page.address().as_u32(),
            len: page.len(),
        });
    }
    if expected_len != page.len() || replacement_len != page.len() {
        return Err(McpError::ReplacementLength {
            address: page.address().as_u32(),
            page_len: page.len(),
            expected_len,
            replacement_len,
        });
    }
    Ok(())
}

fn validate_batch(replacements: &[PageReplacement]) -> Result<(), McpError> {
    for replacement in replacements {
        validate_page(
            replacement.page,
            replacement.expected.len(),
            replacement.replacement.len(),
        )?;
    }
    let mut pages: Vec<_> = replacements.iter().map(PageReplacement::page).collect();
    pages.sort_unstable_by_key(|page| page.address().as_u32());
    validate_disjoint(&pages)
}

fn validate_disjoint(pages: &[Page]) -> Result<(), McpError> {
    for pair in pages.windows(2) {
        if let [first, second] = pair {
            if first == second {
                return Err(McpError::DuplicateReplacement {
                    address: first.address().as_u32(),
                });
            }
            if second.address().as_u32() < first.end() {
                return Err(McpError::OverlappingReplacements {
                    first: first.address().as_u32(),
                    second: second.address().as_u32(),
                });
            }
        }
    }
    Ok(())
}

impl<T: Transport> McpSession<'_, T> {
    /// Compare every complete before-image, then write only changed pages.
    ///
    /// Session state, the unchanged schema-target gate, all page shapes, and
    /// duplicate/overlapping ranges are checked before page I/O. Every expected
    /// page, including no-ops, must then match a fresh acknowledged read before
    /// the first write. Comparison stops at the first mismatch; no rebase or
    /// merge is performed. This is a host-side optimistic check, not a
    /// firmware-atomic multi-page transaction or a lock against other actors.
    ///
    /// Immediately before each changed page, `before_write` must synchronize
    /// the caller's required raw evidence and durable intent, including both
    /// complete images. The callback is not called for no-ops. Its successful
    /// return precedes marking the session journal possibly written, a single
    /// complete W frame/ACK, and whole-page immediate readback. A new dispatch
    /// invalidates any earlier verification entry for that same page.
    ///
    /// Progress counts completed replacements (verified writes or compared
    /// no-ops), in input order, after the entire preflight has succeeded. Empty
    /// batches emit no progress or page traffic but still enforce the schema
    /// gate. The returned report contains only this batch's successful work;
    /// [`Self::journal`] retains the entire session's conservative write state.
    ///
    /// No rollback, retry, exit, fresh connection, or CAT query is performed.
    /// Complete comparison/callback failures leave an exit-safe boundary;
    /// incomplete exchanges block further protocol I/O. Await the future to
    /// completion; dropping it during an exchange can leave recovery required.
    /// Retain the journal before consuming the session with
    /// [`Self::exit`], then close/drop the original transport.
    ///
    /// # Errors
    ///
    /// Returns session-state, schema-target, or batch-validation errors before
    /// any page traffic. Comparison, callback, write, and readback failures are
    /// wrapped in [`McpError::Interrupted`] with the current session journal
    /// counts and original typed cause. A later failure may follow earlier
    /// successful writes; an error never establishes automatic restoration.
    pub async fn compare_exchange_pages(
        &mut self,
        replacements: &[PageReplacement],
        before_write: impl FnMut(&PageReplacement) -> io::Result<()>,
        progress: impl FnMut(Progress),
    ) -> Result<McpCompareExchangeReport, Error> {
        self.radio.require_mcp_ready()?;
        self.require_schema_target()?;
        self.compare_exchange_admitted(replacements, before_write, progress)
            .await
    }

    /// Apply one immutable, format-checked ordinary menu update on firmware 1.02.
    ///
    /// The plan is restricted to registered scalar fields with known value
    /// domains and an ordinary settings lifecycle. It contains complete source
    /// pages plus unchanged format, active-PM, and Gateway-Off guards. The
    /// session identity must equal the plan's complete identity before page I/O;
    /// every guarded page must still match before the first W. Only changed
    /// pages invoke `before_write` and require immediate whole-page readback.
    ///
    /// This separate software-layout policy does not widen the legacy raw-page
    /// schema gate. It is not hardware qualification of every menu setting,
    /// firmware-atomic execution, or permission to change Gateway mode, routing,
    /// or automatic-transmission settings. The caller owns capture durability,
    /// detached exit, transport release, fresh verification, and recovery.
    ///
    /// # Errors
    ///
    /// Returns identity disagreement before page traffic, or the same typed
    /// comparison, intent, transport, and readback failures as
    /// [`Self::compare_exchange_pages`]. Earlier writes remain in the session
    /// journal if a later page fails; no retry or automatic rollback occurs.
    pub async fn compare_exchange_menu_update(
        &mut self,
        plan: &MenuUpdatePlan,
        before_write: impl FnMut(&PageReplacement) -> io::Result<()>,
        progress: impl FnMut(Progress),
    ) -> Result<McpCompareExchangeReport, MenuUpdateError> {
        self.radio.require_mcp_ready()?;
        let identity = self
            .radio
            .identity()
            .ok_or(McpError::RecoveryRequired)
            .map_err(Error::from)?;
        plan.validate_identity(identity)?;
        Ok(self
            .compare_exchange_admitted(plan.replacements(), before_write, progress)
            .await?)
    }

    /// Share shape, comparison, dispatch, and journal semantics after admission.
    async fn compare_exchange_admitted(
        &mut self,
        replacements: &[PageReplacement],
        mut before_write: impl FnMut(&PageReplacement) -> io::Result<()>,
        mut progress: impl FnMut(Progress),
    ) -> Result<McpCompareExchangeReport, Error> {
        validate_batch(replacements)?;
        self.compare_exchange_validated(replacements, &mut before_write, &mut progress)
            .await
            .map_err(|source| {
                McpError::Interrupted {
                    operation: "page compare-and-exchange",
                    possibly_written: self.journal.possibly_written.len(),
                    verified: self.journal.verified.len(),
                    source: Box::new(source),
                }
                .into()
            })
    }

    async fn compare_exchange_validated(
        &mut self,
        replacements: &[PageReplacement],
        before_write: &mut impl FnMut(&PageReplacement) -> io::Result<()>,
        progress: &mut impl FnMut(Progress),
    ) -> Result<McpCompareExchangeReport, Error> {
        let mut report = McpCompareExchangeReport::default();
        for replacement in replacements {
            let current = self.read_page(replacement.page).await?;
            if let Some(offset) = replacement
                .expected
                .iter()
                .zip(&current)
                .position(|(expected, actual)| expected != actual)
            {
                return Err(McpError::CompareMismatch {
                    address: replacement.page.address().as_u32(),
                    offset,
                }
                .into());
            }
            report.compared_pages.push(replacement.page);
        }
        for (index, replacement) in replacements.iter().enumerate() {
            if replacement.is_noop() {
                report.unchanged_pages.push(replacement.page);
            } else {
                before_write(replacement).map_err(|source| McpError::DurableIntent {
                    address: replacement.page.address().as_u32(),
                    source,
                })?;
                self.write_data_verified(replacement.page, &replacement.replacement)
                    .await?;
                report.verified_pages.push(replacement.page);
            }
            progress(Progress {
                done: index + 1,
                total: replacements.len(),
            });
        }
        Ok(report)
    }
}

#[cfg(test)]
mod tests;
