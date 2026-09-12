//! Private single-frame writes for the two closed, bounded text targets.

use super::Radio;
use crate::error::{Error, ProtocolError};
use crate::protocol::mcp::{ACK, write_request};
use crate::types::{Address, PAGE_SIZE, Page};
use kenwood_transport::Transport;

/// No caller-supplied address can reach this experimental frame writer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum FixedTextTarget {
    Pm1,
    My1,
}

impl FixedTextTarget {
    pub(super) fn page(self) -> Result<Page, Error> {
        let address = match self {
            Self::Pm1 => 323_584,
            Self::My1 => 331_776,
        };
        Ok(Page::new(Address::new(address)?, PAGE_SIZE)?)
    }
}

impl<T: Transport> Radio<T> {
    /// Complete the frame/ACK exchange after the caller's fixed-scope checks.
    ///
    /// Each bounded caller validates identity, canonical page, the complete observed
    /// before-image, and durable intent before invoking this private helper.
    /// Any interrupted write or missing ACK keeps the handle uncertain.
    pub(super) async fn write_pm1_frame(
        &mut self,
        after: &[u8; PAGE_SIZE],
        stage: &'static str,
    ) -> Result<(), Error> {
        self.write_fixed_text_frame(FixedTextTarget::Pm1, after, stage)
            .await
    }

    /// Complete one fixed target's frame only after its driver checked the
    /// exact identity, immutable whole-page baseline, guards, and durable intent.
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
