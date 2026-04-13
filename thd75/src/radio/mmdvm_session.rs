//! MMDVM session management for the TH-D75.
//!
//! When the radio enters MMDVM mode (via `TN 3,x`), the serial port switches
//! from ASCII CAT commands to binary MMDVM framing. CAT commands cannot be
//! used until MMDVM mode is exited. The [`MmdvmSession`] type enforces this
//! at the type level: creating one consumes the [`Radio`], and exiting
//! returns it.
//!
//! # Design notes
//!
//! The session holds an [`mmdvm::AsyncModem`] that owns the transport via a
//! [`MmdvmTransportAdapter`]. All MMDVM framing, periodic status polling,
//! TX-queue slot gating, and RX frame dispatch happen inside the
//! `AsyncModem`'s spawned task — the session itself is just a thin
//! lifecycle wrapper that also caches the [`Radio`]'s CAT-mode state for
//! restoration on exit.
//!
//! Higher-level D-STAR operation (slow-data decode, last-heard list,
//! URCALL parsing, echo recording, etc.) lives in
//! [`crate::mmdvm::DStarGateway`], which owns an [`MmdvmSession`] and
//! delegates raw frame I/O to it.
//!
//! # Example
//!
//! ```rust,no_run
//! # use kenwood_thd75::radio::Radio;
//! # use kenwood_thd75::transport::SerialTransport;
//! # use kenwood_thd75::types::TncBaud;
//! # async fn example() -> Result<(), kenwood_thd75::error::Error> {
//! let transport = SerialTransport::open("/dev/cu.usbmodem1234", 115_200)?;
//! let radio = Radio::connect(transport).await?;
//!
//! // Enter MMDVM mode (consumes the Radio).
//! let session = radio.enter_mmdvm(TncBaud::Bps9600).await.map_err(|(_, e)| e)?;
//!
//! // ... use session.modem_mut() for raw MMDVM operations, or build a
//! // DStarGateway on top of it ...
//!
//! // Exit MMDVM mode (returns the Radio).
//! let radio = session.exit().await?;
//! # Ok(())
//! # }
//! ```

use std::time::Duration;

use mmdvm::AsyncModem;

use crate::error::{Error, ProtocolError};
use crate::protocol::{Codec, Command, Response};
use crate::transport::{MmdvmTransportAdapter, Transport};
use crate::types::{TncBaud, TncMode};

use super::Radio;

/// Wait time after the `TN 0,0` exit command before rebuilding the
/// `Radio`. Matches the pre-refactor delay so the TNC has time to
/// switch back to CAT mode.
const EXIT_SWITCH_DELAY: Duration = Duration::from_millis(100);

/// Cached Radio state that persists across an MMDVM session so the
/// `Radio` can be rebuilt on [`MmdvmSession::exit`].
struct RadioState {
    codec: Codec,
    notifications: tokio::sync::broadcast::Sender<Response>,
    timeout: Duration,
    mode_a: Option<super::RadioMode>,
    mode_b: Option<super::RadioMode>,
    mcp_speed: super::programming::McpSpeed,
}

/// An MMDVM session that owns the radio transport via an
/// [`mmdvm::AsyncModem`].
///
/// While this session is active, the transport speaks the MMDVM binary
/// framing protocol and all I/O is funneled through the spawned
/// modem-loop task. CAT commands are unavailable until
/// [`MmdvmSession::exit`] is called.
///
/// The session is consumed on entry (via [`Radio::enter_mmdvm`]) and
/// returned on exit.
pub struct MmdvmSession<T: Transport + Unpin + 'static> {
    /// Async MMDVM modem driving the transport.
    modem: AsyncModem<MmdvmTransportAdapter<T>>,
    /// Radio state cached for restoration on exit.
    radio_state: RadioState,
}

impl<T: Transport + Unpin + 'static> std::fmt::Debug for MmdvmSession<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MmdvmSession").finish_non_exhaustive()
    }
}

impl<T: Transport + Unpin + 'static> Radio<T> {
    /// Wrap this [`Radio`] as an [`MmdvmSession`] without sending any commands.
    ///
    /// Use this when the radio is already in MMDVM mode (e.g. after
    /// enabling DV Gateway / Reflector Terminal Mode via MCP write to
    /// offset `0x1CA0`). The transport is assumed to already speak
    /// MMDVM binary framing.
    #[must_use]
    pub fn into_mmdvm_session(self) -> MmdvmSession<T> {
        tracing::info!("wrapping transport as MMDVM session (radio already in gateway mode)");
        let adapter = MmdvmTransportAdapter::new(self.transport);
        let modem = AsyncModem::spawn(adapter);
        MmdvmSession {
            modem,
            radio_state: RadioState {
                codec: self.codec,
                notifications: self.notifications,
                timeout: self.timeout,
                mode_a: self.mode_a,
                mode_b: self.mode_b,
                mcp_speed: self.mcp_speed,
            },
        }
    }

    /// Enter MMDVM mode, consuming this [`Radio`] and returning an [`MmdvmSession`].
    ///
    /// Sends the `TN 3,x` CAT command to switch the TNC to MMDVM mode at the
    /// specified baud rate. After this call, the serial port speaks MMDVM
    /// binary framing. Use [`MmdvmSession::exit`] to return to CAT mode.
    ///
    /// # Errors
    ///
    /// On failure, returns the [`Radio`] alongside the error so the caller
    /// can continue using CAT mode. The radio is NOT consumed on error.
    pub async fn enter_mmdvm(mut self, baud: TncBaud) -> Result<MmdvmSession<T>, (Self, Error)> {
        tracing::info!(?baud, "entering MMDVM mode");
        let response = match self
            .execute(Command::SetTncMode {
                mode: TncMode::Mmdvm,
                setting: baud,
            })
            .await
        {
            Ok(r) => r,
            Err(e) => return Err((self, e)),
        };
        match response {
            Response::TncMode { .. } => {}
            other => {
                return Err((
                    self,
                    Error::Protocol(ProtocolError::UnexpectedResponse {
                        expected: "TncMode".into(),
                        actual: format!("{other:?}").into_bytes(),
                    }),
                ));
            }
        }

        Ok(self.into_mmdvm_session())
    }
}

impl<T: Transport + Unpin + 'static> MmdvmSession<T> {
    /// Mutable access to the underlying [`mmdvm::AsyncModem`].
    ///
    /// Consumers that need low-level MMDVM control (custom status polls,
    /// mode changes, raw frame send) work with the handle directly.
    /// Higher-level D-STAR orchestration (headers, voice frames, EOT)
    /// is wrapped by [`crate::mmdvm::DStarGateway`].
    pub const fn modem_mut(&mut self) -> &mut AsyncModem<MmdvmTransportAdapter<T>> {
        &mut self.modem
    }

    /// Consume the session and return its [`mmdvm::AsyncModem`].
    ///
    /// Used by [`crate::mmdvm::DStarGateway`] to keep long-lived ownership
    /// of the modem while tracking D-STAR-specific state separately.
    /// Returns the associated Radio restore state alongside the modem
    /// so the caller can rebuild the [`Radio`] after shutdown.
    pub(crate) fn into_parts(self) -> (AsyncModem<MmdvmTransportAdapter<T>>, MmdvmRadioRestore<T>) {
        (
            self.modem,
            MmdvmRadioRestore {
                state: self.radio_state,
                _phantom: std::marker::PhantomData,
            },
        )
    }

    /// Exit MMDVM mode and return the [`Radio`].
    ///
    /// Shuts down the [`mmdvm::AsyncModem`], recovering the transport,
    /// sends `TN 0,0` on the raw transport to return the radio's TNC to
    /// normal APRS mode, then rebuilds the `Radio` from saved state.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Transport`] if the `TN 0,0` write fails, or
    /// translates [`mmdvm::ShellError`] into [`Error::Transport`] /
    /// [`Error::Protocol`] as appropriate.
    pub async fn exit(self) -> Result<Radio<T>, Error> {
        tracing::info!("exiting MMDVM mode");

        let (modem, restore) = self.into_parts();
        restore.exit_and_rebuild(modem).await
    }
}

/// Radio restore state carried alongside the [`mmdvm::AsyncModem`] during
/// MMDVM operation. Keeps the `Radio`'s CAT-mode codec, notifications,
/// timeouts, and VFO/memory cache alive so they can be restored on exit.
///
/// This type is crate-internal — it only escapes [`MmdvmSession::into_parts`]
/// so [`crate::mmdvm::DStarGateway`] can reconstruct the `Radio` after
/// `AsyncModem::shutdown`.
pub(crate) struct MmdvmRadioRestore<T: Transport + Unpin + 'static> {
    state: RadioState,
    _phantom: std::marker::PhantomData<fn() -> T>,
}

impl<T: Transport + Unpin + 'static> std::fmt::Debug for MmdvmRadioRestore<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MmdvmRadioRestore").finish_non_exhaustive()
    }
}

impl<T: Transport + Unpin + 'static> MmdvmRadioRestore<T> {
    /// Shut down the modem, send `TN 0,0`, and rebuild the [`Radio`].
    pub(crate) async fn exit_and_rebuild(
        self,
        modem: AsyncModem<MmdvmTransportAdapter<T>>,
    ) -> Result<Radio<T>, Error> {
        // Shutdown returns the MmdvmTransportAdapter holding our T.
        let adapter = modem.shutdown().await.map_err(shell_err_to_thd75_err)?;

        // Pull the inner T out of the adapter.
        let mut inner = adapter
            .into_inner()
            .await
            .map_err(|e| Error::Transport(crate::error::TransportError::Disconnected(e)))?;

        // Send TN 0,0 on the raw transport to switch the TNC back to
        // APRS mode. The adapter is dropped; we speak ASCII CAT on T
        // directly now.
        inner.write(b"TN 0,0\r").await.map_err(Error::Transport)?;

        // Small delay to let the TNC switch back to CAT mode.
        tokio::time::sleep(EXIT_SWITCH_DELAY).await;

        Ok(Radio {
            transport: inner,
            codec: self.state.codec,
            notifications: self.state.notifications,
            timeout: self.state.timeout,
            mode_a: self.state.mode_a,
            mode_b: self.state.mode_b,
            mcp_speed: self.state.mcp_speed,
            last_cmd_time: None,
        })
    }
}

/// Translate an [`mmdvm::ShellError`] into a thd75 [`Error`].
fn shell_err_to_thd75_err(err: mmdvm::ShellError) -> Error {
    match err {
        mmdvm::ShellError::SessionClosed => Error::Protocol(ProtocolError::UnexpectedResponse {
            expected: "MMDVM session active".into(),
            actual: b"session closed".to_vec(),
        }),
        mmdvm::ShellError::Core(e) => Error::Protocol(ProtocolError::FieldParse {
            command: "MMDVM".to_owned(),
            field: "frame".to_owned(),
            detail: format!("{e}"),
        }),
        mmdvm::ShellError::Io(e) => Error::Transport(crate::error::TransportError::Disconnected(e)),
        mmdvm::ShellError::BufferFull { mode } => {
            Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: format!("MMDVM {mode:?} buffer ready"),
                actual: b"buffer full".to_vec(),
            })
        }
        mmdvm::ShellError::Nak { command, reason } => {
            Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: format!("MMDVM ACK for 0x{command:02X}"),
                actual: format!("NAK: {reason:?}").into_bytes(),
            })
        }
        // `mmdvm::ShellError` is `#[non_exhaustive]`. Surface unknown
        // variants as a generic transport disconnection.
        _ => Error::Transport(crate::error::TransportError::Disconnected(
            std::io::Error::other("unknown MMDVM shell error"),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::MockTransport;
    use crate::types::TncBaud;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    /// Helper: create a Radio with a mock that expects the TN 3,x command.
    async fn mock_radio_for_mmdvm(baud: TncBaud) -> Result<Radio<MockTransport>, Error> {
        let tn_cmd = format!("TN 3,{}\r", u8::from(baud));
        let tn_resp = format!("TN 3,{}\r", u8::from(baud));
        let mut mock = MockTransport::new();
        mock.expect(tn_cmd.as_bytes(), tn_resp.as_bytes());
        Radio::connect(mock).await
    }

    #[tokio::test]
    async fn enter_mmdvm_sends_tn_command() -> TestResult {
        // `enter_mmdvm` constructs an `MmdvmTransportAdapter`, which
        // spawns its pump task via `tokio::task::spawn_local`. Run
        // inside a `LocalSet` so the spawn succeeds.
        tokio::task::LocalSet::new()
            .run_until(async {
                let radio = mock_radio_for_mmdvm(TncBaud::Bps1200).await?;
                let session = radio
                    .enter_mmdvm(TncBaud::Bps1200)
                    .await
                    .map_err(|(_, e)| e)?;
                assert!(format!("{session:?}").contains("MmdvmSession"));
                Ok(())
            })
            .await
    }

    #[tokio::test]
    async fn enter_mmdvm_9600_baud() -> TestResult {
        tokio::task::LocalSet::new()
            .run_until(async {
                let radio = mock_radio_for_mmdvm(TncBaud::Bps9600).await?;
                let _session = radio
                    .enter_mmdvm(TncBaud::Bps9600)
                    .await
                    .map_err(|(_, e)| e)?;
                Ok(())
            })
            .await
    }
}
