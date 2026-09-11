//! Private, single-frame PM1 write exchange shared by the bounded drivers.

use super::Radio;
use crate::error::{Error, ProtocolError};
use crate::protocol::mcp::{ACK, write_request};
use crate::transport::Transport;
use crate::types::{Address, PAGE_SIZE, Page};

impl<T: Transport> Radio<T> {
    /// Complete the frame/ACK exchange after the caller's fixed-scope checks.
    ///
    /// Both callers validate identity, canonical page, the complete observed
    /// before-image, and durable intent before invoking this private helper.
    /// Any interrupted write or missing ACK keeps the handle uncertain.
    pub(super) async fn write_pm1_frame(
        &mut self,
        after: &[u8; PAGE_SIZE],
        stage: &'static str,
    ) -> Result<(), Error> {
        self.require_mcp_ready()?;
        let page = Page::new(Address::new(323_584)?, PAGE_SIZE)?;
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
