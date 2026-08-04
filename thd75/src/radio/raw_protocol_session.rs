//! Exclusive raw-protocol ownership of a TH-D75 transport.
//!
//! [`Radio`] owns the CAT codec and its response-correlation state. Raw bytes
//! must never be exchanged through that same live value: doing so could make a
//! later CAT command consume an unrelated response. [`RawProtocolSession`]
//! prevents that state aliasing by consuming the `Radio` and retaining only its
//! transport.
//!
//! Returning to CAT is deliberately not a cast. [`RawProtocolSession::restore_cat`]
//! closes and reopens the selected transport, creates entirely fresh host-side
//! state, and requires the exact `ID TH-D75\r` response before returning a new
//! `Radio`.

use std::io;

use crate::error::{Error, ProtocolError, TransportError};
use crate::transport::Transport;

use super::{McpPhase, Radio};

/// A transport whose bytes are intentionally outside the typed CAT protocol.
///
/// Constructing this session consumes [`Radio`], so raw traffic cannot share
/// its codec, timeout recovery, MCP phase, or response-correlation state. The
/// session itself implements [`Transport`] for integration with protocol
/// probes that deliberately operate on bytes.
pub struct RawProtocolSession<T: Transport> {
    transport: T,
}

impl<T: Transport> std::fmt::Debug for RawProtocolSession<T> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RawProtocolSession")
            .finish_non_exhaustive()
    }
}

impl<T: Transport> Radio<T> {
    /// Consume this CAT controller and transfer its transport to a raw session.
    ///
    /// No bytes are written. Entry requires a pristine CAT boundary: no
    /// interrupted MCP or strict-GM operation, no timed-out CAT command, and no
    /// buffered codec bytes. All CAT caches and subscriptions are discarded;
    /// [`RawProtocolSession::restore_cat`] creates fresh host-side state.
    ///
    /// # Errors
    ///
    /// Returns the original `Radio` with the error when its current CAT state
    /// is not a safe raw-protocol boundary.
    #[expect(
        clippy::result_large_err,
        reason = "entry errors must preserve the caller's selected transport inside Radio"
    )]
    pub fn into_raw_protocol_session(self) -> Result<RawProtocolSession<T>, (Self, Error)> {
        if let Err(error) = self.require_cat_ready() {
            return Err((self, error));
        }
        if self.mcp_phase != McpPhase::Inactive {
            return Err((self, Error::McpInterrupted));
        }
        if self.desynced {
            return Err((
                self,
                unclean_cat_boundary("a synchronized CAT stream", b"stream is desynchronized"),
            ));
        }
        if !self.codec.is_empty() {
            return Err((
                self,
                unclean_cat_boundary("an empty CAT codec", b"codec contains buffered bytes"),
            ));
        }

        tracing::info!("transferring radio transport to exclusive raw-protocol session");
        Ok(RawProtocolSession {
            transport: self.transport,
        })
    }
}

impl<T: Transport> RawProtocolSession<T> {
    /// Close the transport and consume this raw session.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Transport`] when the transport cannot close.
    pub async fn disconnect(mut self) -> Result<(), Error> {
        self.transport.close().await.map_err(Error::Transport)
    }

    /// Reopen the transport, prove exact TH-D75 CAT identity, and return a
    /// fresh [`Radio`].
    ///
    /// Recovery always attempts a physical close before `reopen`; it never
    /// reuses the old CAT codec, firmware cache, mode cache, subscriptions, or
    /// streaming-state cache. A failed close is diagnostic because the link
    /// may already be gone; `reopen` is still attempted. The exact
    /// `ID TH-D75\r` response is required before CAT capability is returned.
    ///
    /// # Errors
    ///
    /// Returns this session alongside a transport or protocol error, retaining
    /// `T` so the caller can retry recovery or disconnect it.
    pub async fn restore_cat(mut self) -> Result<Radio<T>, (Self, Error)> {
        if let Err(error) = self.transport.close().await {
            tracing::debug!(%error, "close before raw-protocol recovery failed");
        }
        if let Err(error) = self.transport.reopen().await {
            return Err((self, Error::Transport(error)));
        }

        let mut radio = Radio::from_transport(self.transport);
        match radio.prove_reopened_thd75_identity().await {
            Ok(()) => Ok(radio),
            Err(error) => Err((Self::from_failed_radio(radio), error)),
        }
    }

    fn from_failed_radio(radio: Radio<T>) -> Self {
        Self {
            transport: radio.transport,
        }
    }
}

impl<T: Transport> Transport for RawProtocolSession<T> {
    async fn write(&mut self, data: &[u8]) -> Result<(), TransportError> {
        self.transport.write(data).await
    }

    async fn read(&mut self, buffer: &mut [u8]) -> Result<usize, TransportError> {
        let count = self.transport.read(buffer).await?;
        if count <= buffer.len() {
            Ok(count)
        } else {
            Err(TransportError::Read(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "transport reported {count} raw bytes for a {}-byte buffer",
                    buffer.len()
                ),
            )))
        }
    }

    async fn close(&mut self) -> Result<(), TransportError> {
        self.transport.close().await
    }

    async fn reopen(&mut self) -> Result<(), TransportError> {
        self.transport.reopen().await
    }
}

fn unclean_cat_boundary(expected: &str, actual: &[u8]) -> Error {
    Error::Protocol(ProtocolError::UnexpectedResponse {
        expected: expected.to_owned(),
        actual: actual.to_vec(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::MockTransport;
    use crate::types::RadioModel;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[tokio::test]
    async fn raw_io_exists_only_after_consuming_radio() -> TestResult {
        let mut transport = MockTransport::new();
        transport.expect(b"probe", b"raw reply");
        let radio = Radio::new(transport);
        let mut raw = radio
            .into_raw_protocol_session()
            .map_err(|(_, error)| error)?;

        raw.write(b"probe").await?;
        let mut buffer = [0_u8; 16];
        let count = raw.read(&mut buffer).await?;
        assert_eq!(
            buffer.get(..count),
            Some(b"raw reply".as_slice()),
            "raw session must forward the exact transport response"
        );
        Ok(())
    }

    #[tokio::test]
    async fn recovery_reopens_and_requires_exact_identity() -> TestResult {
        let mut transport = MockTransport::new();
        transport.expect_reopen(Ok(()));
        transport.expect(b"ID\r", b"ID TH-D75\r");
        transport.expect(b"ID\r", b"ID TH-D75\r");
        let radio = Radio::new(transport);
        let raw = radio
            .into_raw_protocol_session()
            .map_err(|(_, error)| error)?;

        let mut radio = raw.restore_cat().await.map_err(|(_, error)| error)?;
        assert_eq!(
            radio.identify().await?.model,
            RadioModel::ThD75,
            "restored CAT controller must identify the exact TH-D75 model"
        );
        assert!(
            radio.cached_firmware_version().is_none(),
            "raw recovery must not retain the pre-session firmware cache"
        );
        radio.transport.assert_complete();
        Ok(())
    }

    #[tokio::test]
    async fn failed_identity_proof_preserves_transport_for_retry() -> TestResult {
        let mut transport = MockTransport::new();
        transport.expect_reopen(Ok(()));
        transport.expect_reopen(Ok(()));
        transport.expect(b"ID\r", b"ID OTHER\r");
        transport.expect(b"ID\r", b"ID TH-D75\r");
        let radio = Radio::new(transport);
        let raw = radio
            .into_raw_protocol_session()
            .map_err(|(_, error)| error)?;

        let Err((raw, error)) = raw.restore_cat().await else {
            return Err("wrong identity unexpectedly restored CAT capability".into());
        };
        assert!(
            matches!(error, Error::Protocol(_)),
            "wrong identity must surface as a protocol error: {error:?}"
        );
        let radio = raw.restore_cat().await.map_err(|(_, error)| error)?;
        radio.transport.assert_complete();
        Ok(())
    }

    #[tokio::test]
    async fn entry_refuses_desynchronized_or_buffered_cat_state() -> TestResult {
        let mut desynchronized = Radio::new(MockTransport::new());
        desynchronized.desynced = true;
        let Err((_radio, error)) = desynchronized.into_raw_protocol_session() else {
            return Err("desynchronized CAT unexpectedly entered a raw session".into());
        };
        assert!(
            matches!(error, Error::Protocol(_)),
            "desynchronized CAT rejection must be precise: {error:?}"
        );

        let mut buffered = Radio::new(MockTransport::new());
        buffered.codec.feed(b"partial")?;
        let Err((_radio, error)) = buffered.into_raw_protocol_session() else {
            return Err("buffered CAT unexpectedly entered a raw session".into());
        };
        assert!(
            matches!(error, Error::Protocol(_)),
            "buffered CAT rejection must be precise: {error:?}"
        );
        Ok(())
    }
}
