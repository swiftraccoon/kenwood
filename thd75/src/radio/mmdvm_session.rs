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
//! The session holds an [`::mmdvm::AsyncModem`] that owns the transport via a
//! [`MmdvmTransportAdapter`]. All MMDVM framing, periodic status polling,
//! TX-queue slot gating, and RX frame dispatch happen inside the
//! `AsyncModem`'s spawned task; the session itself is just a thin
//! lifecycle wrapper that also caches the [`Radio`]'s CAT-mode state for
//! restoration on exit.
//!
//! Higher-level D-STAR operation (slow-data decode, last-heard list,
//! URCALL parsing, echo recording, etc.) lives in
//! [`crate::dstar_gateway::DstarGateway`], which owns an [`MmdvmSession`] and
//! delegates raw frame I/O to it.
//!
//! # Example
//!
//! ```rust,no_run
//! # use kenwood_thd75::radio::Radio;
//! # use kenwood_thd75::transport::SerialTransport;
//! # use kenwood_thd75::types::PacketDataRate;
//! # async fn example() -> Result<(), kenwood_thd75::error::Error> {
//! let transport = SerialTransport::open("/dev/cu.usbmodem1234")?;
//! let radio = Radio::new(transport);
//!
//! // Enter MMDVM mode (consumes the Radio).
//! let session = radio.enter_mmdvm(PacketDataRate::Bps9600).await.map_err(|(_, e)| e)?;
//!
//! // ... use session.modem_mut() for raw MMDVM operations, or build a
//! // DstarGateway on top of it ...
//!
//! // Exit MMDVM mode; the desynchronized radio must be restored (or
//! // taken explicitly unproven) before ordinary CAT commands.
//! let radio = session
//!     .exit()
//!     .await?
//!     .restore()
//!     .await
//!     .map_err(|(_desynced, e)| e)?;
//! # Ok(())
//! # }
//! ```

use std::marker::PhantomData;
use std::time::Duration;

use ::mmdvm::AsyncModem;

use crate::error::{Error, ProtocolError};
use crate::protocol::{Command, Response};
use crate::transport::{MmdvmTransportAdapter, Transport};
use crate::types::{PacketDataRate, TncMode};

use super::{DesyncedRadio, Radio, cat_restore_state::CatRestoreState};

/// Wait time after the `TN 0,0` exit command before rebuilding the
/// `Radio`. Matches the pre-refactor delay so the TNC has time to
/// switch back to CAT mode.
const EXIT_SWITCH_DELAY: Duration = Duration::from_millis(100);

/// Gateway entered from ordinary CAT with the transient `TN 3,x` command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TransientMmdvm;

/// Gateway attached to persistent DV Gateway / Reflector Terminal Mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PersistentMmdvm;

/// An MMDVM session that owns the radio transport via an
/// [`::mmdvm::AsyncModem`].
///
/// While this session is active, the transport speaks the MMDVM binary
/// framing protocol and all I/O is funneled through the spawned
/// modem-loop task. Its lifecycle marker distinguishes a transient `TN`
/// session, whose [`MmdvmSession::exit`] returns to CAT, from persistent
/// Reflector Terminal Mode, whose
/// [`MmdvmSession::<T, PersistentMmdvm>::shutdown`] preserves binary mode.
///
/// The session is consumed on entry (via [`Radio::enter_mmdvm`]) and
/// returned on exit.
pub struct MmdvmSession<T: Transport + Unpin + 'static, Lifecycle = TransientMmdvm> {
    /// Async MMDVM modem driving the transport.
    modem: AsyncModem<MmdvmTransportAdapter<T>>,
    /// Radio state cached for restoration on exit.
    cat_restore: CatRestoreState,
    /// Compile-time record of how this MMDVM session was entered.
    lifecycle: PhantomData<fn() -> Lifecycle>,
}

impl<T: Transport + Unpin + 'static, Lifecycle> std::fmt::Debug for MmdvmSession<T, Lifecycle> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MmdvmSession").finish_non_exhaustive()
    }
}

impl<T: Transport + Unpin + 'static> Radio<T> {
    /// Wrap this [`Radio`] as an [`MmdvmSession`] without sending any commands.
    ///
    /// Use this when the radio is already in MMDVM mode (e.g. after
    /// enabling DV Gateway / Reflector Terminal Mode via MCP write to
    /// offset `0x1CA0`). The transport must already have a successful MMDVM
    /// link diagnosis or a correlated owned MMDVM transition recorded on it.
    ///
    /// # Errors
    ///
    /// Returns the original radio and [`Error::BinaryModeNotProven`] if the
    /// link is still ordinary CAT, [`Error::MemoryReadStreamPoisoned`] if an
    /// incomplete GM exchange requires a reconnect, or [`Error::McpInterrupted`]
    /// if an MCP session is not fully recovered.
    #[expect(
        clippy::result_large_err,
        reason = "The ownership-preserving error matches enter_mmdvm: callers need the original \
                  Radio to reconnect or recover MCP without losing the selected transport"
    )]
    pub fn into_mmdvm_session(self) -> Result<MmdvmSession<T, PersistentMmdvm>, (Self, Error)> {
        let (transport, cat_restore) = self.into_binary_mode_parts()?;
        Ok(Self::mmdvm_session_with_lifecycle(transport, cat_restore))
    }

    fn mmdvm_session_with_lifecycle<Lifecycle>(
        transport: T,
        cat_restore: CatRestoreState,
    ) -> MmdvmSession<T, Lifecycle> {
        tracing::info!("wrapping transport as MMDVM session (radio already in gateway mode)");
        let adapter = MmdvmTransportAdapter::new(transport);
        let modem = AsyncModem::spawn(adapter);
        MmdvmSession {
            modem,
            cat_restore,
            lifecycle: PhantomData,
        }
    }

    /// Enter MMDVM mode, consuming this [`Radio`] and returning an [`MmdvmSession`].
    ///
    /// Sends the `TN 3,x` CAT command to switch the TNC to MMDVM mode at the
    /// specified packet data rate. After this call, the serial port speaks MMDVM
    /// binary framing. Use [`MmdvmSession::exit`] to return to CAT mode.
    ///
    /// # Errors
    ///
    /// On failure, returns the [`Radio`] alongside the error. If the transition
    /// write may have reached the radio, that handle rejects ordinary CAT until
    /// [`Radio::restore_cat_after_mode_exit`] proves the framing boundary.
    pub async fn enter_mmdvm(
        mut self,
        data_rate: PacketDataRate,
    ) -> Result<MmdvmSession<T>, (Self, Error)> {
        tracing::info!(?data_rate, "entering MMDVM mode");
        let response = match self
            .execute(Command::SetTncMode {
                mode: TncMode::Mmdvm,
                data_rate,
            })
            .await
        {
            Ok(r) => r,
            Err(e) => {
                if matches!(
                    e,
                    Error::Timeout(_) | Error::Transport(_) | Error::Protocol(_)
                ) {
                    self.cat_state = super::CatState::RecoveryRequired;
                }
                return Err((self, e));
            }
        };
        match response {
            Response::TncMode {
                mode: TncMode::Mmdvm,
                data_rate: response_data_rate,
            } if response_data_rate == data_rate => {
                // The exact TN echo is the CAT-side proof that the transport
                // may now be consumed by the typed binary session.
                self.cat_state = super::CatState::BinaryProven;
            }
            other => {
                self.cat_state = super::CatState::RecoveryRequired;
                return Err((
                    self,
                    Error::Protocol(ProtocolError::UnexpectedResponse {
                        expected: format!("TncMode {{ mode: Mmdvm, data_rate: {data_rate:?} }}"),
                        actual: format!("{other:?}").into_bytes(),
                    }),
                ));
            }
        }

        let (transport, cat_restore) = self.into_binary_mode_parts()?;
        Ok(Self::mmdvm_session_with_lifecycle::<TransientMmdvm>(
            transport,
            cat_restore,
        ))
    }
}

impl<T: Transport + Unpin + 'static, Lifecycle> MmdvmSession<T, Lifecycle> {
    /// Mutable access to the underlying [`::mmdvm::AsyncModem`].
    ///
    /// Consumers that need low-level MMDVM control (custom status polls,
    /// mode changes, raw frame send) work with the handle directly.
    /// Higher-level D-STAR orchestration (headers, voice frames, EOT)
    /// is wrapped by [`crate::dstar_gateway::DstarGateway`].
    pub const fn modem_mut(&mut self) -> &mut AsyncModem<MmdvmTransportAdapter<T>> {
        &mut self.modem
    }

    /// Consume the session and return its [`::mmdvm::AsyncModem`].
    ///
    /// Used by [`crate::dstar_gateway::DstarGateway`] to keep long-lived ownership
    /// of the modem while tracking D-STAR-specific state separately.
    /// Returns the associated Radio restore state alongside the modem
    /// so the caller can rebuild the [`Radio`] after shutdown.
    pub(crate) fn into_parts(self) -> (AsyncModem<MmdvmTransportAdapter<T>>, MmdvmRadioRestore<T>) {
        (
            self.modem,
            MmdvmRadioRestore {
                cat_restore: self.cat_restore,
                _phantom: PhantomData,
            },
        )
    }
}

impl<T: Transport + Unpin + 'static> MmdvmSession<T> {
    /// Exit MMDVM mode and return the [`Radio`].
    ///
    /// Shuts down the [`::mmdvm::AsyncModem`], recovering the transport,
    /// sends `TN 0,0` on the raw transport to turn the radio's transient TNC
    /// mode off, then rebuilds the radio from saved state. Binary
    /// residue may remain, so the radio comes back wrapped in
    /// [`DesyncedRadio`]: call [`DesyncedRadio::restore`] before ordinary
    /// CAT commands. Unlike the KISS exit, a failure here cannot return
    /// the session: the modem shutdown has already consumed it.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Transport`] if the `TN 0,0` write fails, or
    /// translates [`::mmdvm::ShellError`] into [`Error::Transport`] /
    /// [`Error::Protocol`] as appropriate.
    pub async fn exit(self) -> Result<DesyncedRadio<T>, Error> {
        tracing::info!("exiting MMDVM mode");

        let (modem, restore) = self.into_parts();
        restore.exit_and_rebuild(modem).await
    }
}

impl<T: Transport + Unpin + 'static> MmdvmSession<T, PersistentMmdvm> {
    /// Stop this host session while preserving persistent MMDVM mode.
    ///
    /// The modem and transport pumps are shut down cleanly, but no transient
    /// `TN 0,0` exit is sent. The returned radio remains proved to speak MMDVM
    /// and can start another persistent session on the same link.
    ///
    /// # Errors
    ///
    /// Returns an error if the modem loop or transport pump cannot be
    /// reclaimed without invalidating the persistent binary-link proof.
    pub async fn shutdown(self) -> Result<Radio<T>, Error> {
        let (modem, restore) = self.into_parts();
        restore.shutdown_and_rebuild_binary(modem).await
    }
}

/// Radio restore state carried alongside the [`::mmdvm::AsyncModem`] during
/// MMDVM operation. Keeps the `Radio`'s CAT-mode codec, notifications,
/// timeouts, and VFO/memory cache alive so they can be restored on exit.
///
/// This type is crate-internal; it only escapes [`MmdvmSession::into_parts`]
/// so [`crate::dstar_gateway::DstarGateway`] can reconstruct the `Radio` after
/// `AsyncModem::shutdown`.
pub(crate) struct MmdvmRadioRestore<T: Transport + Unpin + 'static> {
    cat_restore: CatRestoreState,
    _phantom: PhantomData<fn() -> T>,
}

impl<T: Transport + Unpin + 'static> std::fmt::Debug for MmdvmRadioRestore<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MmdvmRadioRestore").finish_non_exhaustive()
    }
}

impl<T: Transport + Unpin + 'static> MmdvmRadioRestore<T> {
    /// Shut down the MMDVM consumer and rebuild ownership of the same binary
    /// link without sending a mode-exit command.
    ///
    /// Used when persistent Reflector Terminal Mode remains active but a
    /// higher-level gateway initialization fails. Binary proof is retained
    /// only when both the modem loop and transport pump shut down cleanly. A
    /// pump failure may return a transport internally, but reopening it would
    /// establish a new link whose protocol has not been proved, so that handle
    /// is deliberately dropped instead of being mislabeled binary-safe.
    pub(crate) async fn shutdown_and_rebuild_binary(
        self,
        modem: AsyncModem<MmdvmTransportAdapter<T>>,
    ) -> Result<Radio<T>, Error> {
        let adapter = modem.shutdown().await.map_err(shell_err_to_thd75_err)?;
        let inner = adapter
            .shutdown_and_recover()
            .await
            .map_err(|recovery_error| {
                let (_transport, pump_error) = recovery_error.into_parts();
                Error::Transport(crate::error::TransportError::Disconnected(
                    std::io::Error::new(
                        pump_error.kind(),
                        format!(
                            "MMDVM transport could not be cleanly reclaimed without invalidating \
                         binary proof: {pump_error}"
                        ),
                    ),
                ))
            })?;
        Ok(self.cat_restore.rebuild_binary_proven(inner))
    }

    /// Shut down the modem, send `TN 0,0`, and rebuild the [`Radio`].
    ///
    /// The returned radio remains deliberately desynchronized until
    /// [`Radio::restore_cat_after_mode_exit`] succeeds.
    pub(crate) async fn exit_and_rebuild(
        self,
        modem: AsyncModem<MmdvmTransportAdapter<T>>,
    ) -> Result<DesyncedRadio<T>, Error> {
        // Shutdown returns the MmdvmTransportAdapter holding our T.
        let adapter = modem.shutdown().await.map_err(shell_err_to_thd75_err)?;

        // Pull the inner T out of the adapter.
        let mut inner = match adapter.shutdown_and_recover().await {
            Ok(inner) => inner,
            Err(recovery_error) => {
                let (transport, pump_error) = recovery_error.into_parts();
                let Some(mut inner) = transport else {
                    return Err(Error::Transport(
                        crate::error::TransportError::Disconnected(pump_error),
                    ));
                };
                tracing::warn!(
                    error = %pump_error,
                    "MMDVM pump failed; reopening recovered transport before CAT restoration"
                );
                if let Err(reopen_error) = inner.reopen().await {
                    return Err(Error::Transport(
                        crate::error::TransportError::Disconnected(std::io::Error::new(
                            pump_error.kind(),
                            format!(
                                "MMDVM pump failed: {pump_error}; recovered transport could not reopen: {reopen_error}"
                            ),
                        )),
                    ));
                }
                inner
            }
        };

        // Send TN 0,0 on the raw transport to turn the transient TNC mode
        // off. The adapter is dropped; we speak ASCII CAT on T directly now.
        inner.write(b"TN 0,0\r").await.map_err(Error::Transport)?;

        // Small delay to let the TNC switch back to CAT mode.
        tokio::time::sleep(EXIT_SWITCH_DELAY).await;

        Ok(DesyncedRadio::new(
            self.cat_restore.rebuild_desynchronized(inner),
        ))
    }
}

/// Translate an [`::mmdvm::ShellError`] into a thd75 [`Error`].
fn shell_err_to_thd75_err(err: ::mmdvm::ShellError) -> Error {
    match err {
        ::mmdvm::ShellError::SessionClosed => Error::Protocol(ProtocolError::UnexpectedResponse {
            expected: "MMDVM session active".into(),
            actual: b"session closed".to_vec(),
        }),
        ::mmdvm::ShellError::Core(e) => Error::Protocol(ProtocolError::FieldParse {
            command: "MMDVM".to_owned(),
            field: "frame".to_owned(),
            detail: format!("{e}"),
        }),
        ::mmdvm::ShellError::Io(e) => {
            Error::Transport(crate::error::TransportError::Disconnected(e))
        }
        ::mmdvm::ShellError::BufferFull { mode } => {
            Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: format!("MMDVM {mode:?} buffer ready"),
                actual: b"buffer full".to_vec(),
            })
        }
        ::mmdvm::ShellError::Nak { command, reason } => {
            Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: format!("MMDVM ACK for 0x{command:02X}"),
                actual: format!("NAK: {reason:?}").into_bytes(),
            })
        }
        // `::mmdvm::ShellError` is `#[non_exhaustive]`. Surface unknown
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
    use crate::types::PacketDataRate;

    type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

    /// Helper: create a Radio with a mock that expects the TN 3,x command.
    fn mock_radio_for_mmdvm(data_rate: PacketDataRate) -> Radio<MockTransport> {
        let tn_cmd = format!("TN 3,{}\r", u8::from(data_rate));
        let tn_resp = format!("TN 3,{}\r", u8::from(data_rate));
        let mut mock = MockTransport::new();
        mock.expect(tn_cmd.as_bytes(), tn_resp.as_bytes());
        mock.pend_when_empty();
        Radio::new(mock)
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn enter_mmdvm_sends_tn_command() -> TestResult {
        // Match clients such as thd75-tui, which own the radio in a spawned
        // task on a multi-thread runtime.
        tokio::spawn(async {
            let radio = mock_radio_for_mmdvm(PacketDataRate::Bps1200);
            let session = radio
                .enter_mmdvm(PacketDataRate::Bps1200)
                .await
                .map_err(|(_, e)| e)?;
            assert!(format!("{session:?}").contains("MmdvmSession"));
            TestResult::Ok(())
        })
        .await??;
        Ok(())
    }

    #[tokio::test]
    async fn enter_mmdvm_9600_bps() -> TestResult {
        tokio::task::LocalSet::new()
            .run_until(async {
                let radio = mock_radio_for_mmdvm(PacketDataRate::Bps9600);
                let _session = radio
                    .enter_mmdvm(PacketDataRate::Bps9600)
                    .await
                    .map_err(|(_, e)| e)?;
                Ok(())
            })
            .await
    }

    #[tokio::test(start_paused = true)]
    async fn ambiguous_mmdvm_entry_failure_blocks_cat_reuse() -> TestResult {
        let mut transport = MockTransport::new();
        transport.expect_hang(b"TN 3,1\r");
        let mut radio = Radio::new(transport);
        radio.set_timeout(Duration::from_millis(1));

        let Err((mut radio, error)) = radio.enter_mmdvm(PacketDataRate::Bps9600).await else {
            return Err("silent MMDVM transition unexpectedly created a session".into());
        };
        assert!(matches!(error, Error::Timeout(_)));
        assert_eq!(radio.cat_state, crate::radio::CatState::RecoveryRequired);
        assert!(matches!(
            radio.identify().await,
            Err(Error::CatRecoveryRequired)
        ));
        radio.transport.assert_complete();
        Ok(())
    }

    #[tokio::test]
    async fn transient_exit_returns_desynchronized_radio_with_preserved_cat_state() -> TestResult {
        tokio::task::LocalSet::new()
            .run_until(async {
                let mut transport = MockTransport::new();
                transport.expect(b"TN 3,1\r", b"TN 3,1\r");
                transport.expect_any_write();
                transport.expect(b"TN 0,0\r", b"");
                transport.pend_when_empty();
                let mut radio = Radio::new(transport);
                radio.set_timeout(Duration::from_millis(731));
                radio.firmware_version = Some(crate::types::FirmwareIdentity::new("1.03.AZM")?);
                radio.tuning_mode_b = Some(crate::types::TuningMode::Call);
                radio.auto_info_enabled = true;
                radio.gps_settings = Some(crate::types::GpsSettings::new(true, true));
                radio.gps_sentences = Some(crate::types::NmeaSentences::all());
                let session = radio
                    .enter_mmdvm(PacketDataRate::Bps9600)
                    .await
                    .map_err(|(_, error)| error)?;
                let radio = session.exit().await?.into_radio_unproven();

                assert_eq!(radio.timeout, Duration::from_millis(731));
                assert_eq!(
                    radio
                        .firmware_version
                        .as_ref()
                        .map(crate::types::FirmwareIdentity::as_str),
                    Some("1.03.AZM")
                );
                assert_eq!(radio.tuning_mode_b, Some(crate::types::TuningMode::Call));
                assert!(radio.auto_info_enabled);
                assert_eq!(
                    radio.gps_settings,
                    Some(crate::types::GpsSettings::new(true, true))
                );
                assert_eq!(
                    radio.gps_sentences,
                    Some(crate::types::NmeaSentences::all())
                );
                assert!(radio.desynced);
                assert!(radio.codec.is_empty());
                radio.transport.assert_complete();
                Ok(())
            })
            .await
    }

    #[tokio::test]
    async fn persistent_shutdown_preserves_binary_mode_without_transient_exit() -> TestResult {
        let mut transport = MockTransport::new();
        transport.expect_any_write();
        transport.pend_when_empty();
        let mut radio = Radio::new(transport);
        radio.cat_state = crate::radio::CatState::BinaryProven;

        let session = radio.into_mmdvm_session().map_err(|(_, error)| error)?;
        let radio = session.shutdown().await?;

        assert_eq!(radio.cat_state, crate::radio::CatState::BinaryProven);
        assert!(
            radio
                .transport
                .writes()
                .iter()
                .all(|write| write != b"TN 0,0\r"),
            "persistent session shutdown must not send the transient CAT exit"
        );
        radio.transport.assert_complete();
        Ok(())
    }

    #[tokio::test]
    async fn direct_mmdvm_conversion_rejects_a_poisoned_gm_stream() -> TestResult {
        let mut radio = Radio::new(MockTransport::new());
        radio.gm_poisoned = true;

        let Err((radio, error)) = radio.into_mmdvm_session() else {
            return Err("poisoned radio unexpectedly entered MMDVM mode".into());
        };
        assert!(matches!(error, Error::MemoryReadStreamPoisoned));
        radio.transport.assert_complete();
        Ok(())
    }

    #[tokio::test]
    async fn direct_mmdvm_conversion_rejects_an_unproved_cat_link() -> TestResult {
        let radio = Radio::new(MockTransport::new());

        let Err((radio, error)) = radio.into_mmdvm_session() else {
            return Err("unproved CAT link unexpectedly entered MMDVM mode".into());
        };
        assert!(matches!(error, Error::BinaryModeNotProven));
        radio.transport.assert_complete();
        Ok(())
    }

    #[tokio::test]
    async fn direct_mmdvm_conversion_rejects_active_mcp() -> TestResult {
        let mut radio = Radio::new(MockTransport::new());
        radio.mcp_phase = super::super::McpPhase::Active;

        let Err((radio, error)) = radio.into_mmdvm_session() else {
            return Err("MCP-active radio unexpectedly entered MMDVM mode".into());
        };
        assert!(matches!(error, Error::McpInterrupted));
        radio.transport.assert_complete();
        Ok(())
    }
}
