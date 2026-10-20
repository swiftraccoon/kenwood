//! TH-D75 lifecycle ownership around the shared D-STAR modem runtime.
//!
//! [`DstarGateway`] retains CAT state and model entry/exit policy. Framing,
//! initialization, event processing, voice and slow data live in
//! [`mmdvm::dstar`]. Voice operations delegate to that runtime while keeping
//! its transport paired with the CAT state needed to restore the radio. Call
//! `stop` to reclaim the radio; a transient gateway then requires
//! [`DesyncedRadio::restore`] before any further CAT command.
//!
//! The Kenwood configuration layout is unverified. Both startup methods return
//! [`mmdvm::ConfigurationError::UnsupportedProfile`] before radio I/O.

use std::marker::PhantomData;
use std::time::Duration;

use dstar_gateway_core::{DstarHeader, VoiceFrame};
use kenwood_transport::{StreamAdapter, Transport};
use mmdvm::core::ModemStatus;
use mmdvm::dstar::{DstarError, DstarEvent, DstarModem, DstarModemConfig, DstarStatusReflector};
use mmdvm::{AsyncModem, ConfigurationError, ConfigurationProfile, DstarConfiguration};

use crate::Error;
use crate::radio::mmdvm_session::{
    MmdvmRadioRestore, MmdvmSession, MmdvmShutdown, PersistentMmdvm, TransientMmdvm,
};
use crate::radio::{DesyncedRadio, Radio};
use crate::types::TncDataBand;

/// A shared D-STAR modem plus the TH-D75 state needed to stop it safely.
///
/// A transient gateway enters and exits through the selected TNC data band.
/// A persistent gateway requires a connection that already carries binary-mode
/// proof and never sends the transient ASCII exit.
///
/// ```
/// use kenwood_thd75::DstarGateway;
/// use kenwood_transport::MockTransport;
/// use mmdvm::{ConfigurationError, ConfigurationProfile};
///
/// assert!(matches!(DstarGateway::<MockTransport>::wire_configuration(),
///     Err(ConfigurationError::UnsupportedProfile {
///         profile: ConfigurationProfile::KenwoodTerminal
///     })));
/// ```
pub struct DstarGateway<T: Transport + Unpin + 'static, Lifecycle = TransientMmdvm> {
    modem: DstarModem<StreamAdapter<T>>,
    restore: MmdvmRadioRestore<T>,
    lifecycle: PhantomData<fn() -> Lifecycle>,
}

impl<T: Transport + Unpin + 'static, Lifecycle> std::fmt::Debug for DstarGateway<T, Lifecycle> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DstarGateway")
            .field("modem", &self.modem)
            .finish_non_exhaustive()
    }
}

impl<T: Transport + Unpin + 'static> DstarGateway<T> {
    /// Enter transient MMDVM on `data_band`, then initialize D-STAR.
    ///
    /// The shared configuration carries no radio band. `data_band` selects the
    /// band sent in `TN 3,x` and is retained for the matching `TN 0,x` exit.
    ///
    /// # Errors
    ///
    /// Returns the original radio on entry failure. Initialization failure
    /// attempts transient exit and returns an unproven radio if cleanup
    /// succeeds. Failed rollback returns no radio and preserves both causes.
    pub async fn start(
        radio: Radio<T>,
        data_band: TncDataBand,
        config: DstarModemConfig,
    ) -> Result<Self, (Option<Radio<T>>, Error)> {
        let wire_config = match Self::wire_configuration() {
            Ok(config) => config,
            Err(error) => return Err((Some(radio), error.into())),
        };
        let session = match radio.enter_mmdvm(data_band).await {
            Ok(session) => session,
            Err((radio, error)) => return Err((Some(radio), error)),
        };
        match Self::build_from_session(session, config, wire_config).await {
            Ok(gateway) => Ok(gateway),
            Err((restore, modem, init_error)) => match restore.exit_and_rebuild(modem).await {
                Ok(completed) => {
                    let (radio, summary) = completed.into_parts();
                    Err((
                        Some(radio.into_radio_unproven()),
                        Error::DstarStartup {
                            initialization: Box::new(init_error),
                            cleanup: None,
                            shutdown: Some(summary),
                        },
                    ))
                }
                Err(exit_error) => Err((None, startup_cleanup_error(init_error, exit_error))),
            },
        }
    }

    /// Stop transient MMDVM and return the radio with CAT recovery required.
    ///
    /// Call [`DesyncedRadio::restore`] before ordinary CAT commands.
    ///
    /// # Errors
    ///
    /// Fails on modem shutdown, transport recovery, or the matching TNC exit.
    pub async fn stop(self) -> Result<MmdvmShutdown<DesyncedRadio<T>>, Error> {
        self.restore.exit_and_rebuild(self.modem.into_modem()).await
    }
}

impl<T: Transport + Unpin + 'static> DstarGateway<T, PersistentMmdvm> {
    /// Initialize D-STAR on this already-proved persistent MMDVM link.
    ///
    /// No CAT entry, TNC band selection, or CAT mode-switch command is sent.
    ///
    /// # Errors
    ///
    /// Returns the original radio if binary conversion is refused. Failed
    /// initialization returns a still-proved binary radio only after clean
    /// modem and transport-pump recovery. Failed recovery returns no radio
    /// and retains both failures; a new connection needs new protocol proof.
    pub async fn start_gateway_mode(
        radio: Radio<T>,
        config: DstarModemConfig,
    ) -> Result<Self, (Option<Radio<T>>, Error)> {
        let wire_config = match Self::wire_configuration() {
            Ok(config) => config,
            Err(error) => return Err((Some(radio), error.into())),
        };
        let session = match radio.into_mmdvm_session() {
            Ok(session) => session,
            Err((radio, error)) => return Err((Some(radio), error)),
        };
        match Self::build_from_session(session, config, wire_config).await {
            Ok(gateway) => Ok(gateway),
            Err((restore, modem, init_error)) => {
                match restore.shutdown_and_rebuild_binary(modem).await {
                    Ok(completed) => {
                        let (radio, summary) = completed.into_parts();
                        Err((
                            Some(radio),
                            Error::DstarStartup {
                                initialization: Box::new(init_error),
                                cleanup: None,
                                shutdown: Some(summary),
                            },
                        ))
                    }
                    Err(reclaim_error) => {
                        Err((None, startup_cleanup_error(init_error, reclaim_error)))
                    }
                }
            }
        }
    }

    /// Stop host processing while preserving persistent binary-mode proof.
    ///
    /// No transient exit, reopen, or CAT command is sent. The returned radio
    /// may start another persistent session on this same connection.
    ///
    /// # Errors
    ///
    /// Fails if either pump cannot be reclaimed cleanly. On failure no `Radio`
    /// is returned and the connection must be reopened and re-proved.
    pub async fn stop(self) -> Result<MmdvmShutdown<Radio<T>>, Error> {
        self.restore
            .shutdown_and_rebuild_binary(self.modem.into_modem())
            .await
    }
}

impl<T: Transport + Unpin + 'static, Lifecycle> DstarGateway<T, Lifecycle> {
    /// Check the Kenwood wire profile before changing any radio mode.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigurationError::UnsupportedProfile`] for the currently
    /// unverified [`ConfigurationProfile::KenwoodTerminal`] layout.
    pub const fn wire_configuration() -> Result<DstarConfiguration, ConfigurationError> {
        Err(ConfigurationError::UnsupportedProfile {
            profile: ConfigurationProfile::KenwoodTerminal,
        })
    }

    /// Inspect shared modem configuration and receive state.
    ///
    /// The runtime stays paired with the CAT state this gateway restores, so
    /// the borrow is shared and the runtime cannot be swapped out:
    ///
    /// ```compile_fail
    /// use kenwood_thd75::DstarGateway;
    /// use kenwood_transport::Transport;
    ///
    /// fn swap<T: Transport + Unpin + 'static, L>(
    ///     left: &mut DstarGateway<T, L>,
    ///     right: &mut DstarGateway<T, L>,
    /// ) {
    ///     std::mem::swap(left.modem(), right.modem());
    /// }
    /// ```
    ///
    /// There is no `modem_mut` accessor either:
    ///
    /// ```compile_fail
    /// use kenwood_thd75::DstarGateway;
    /// use kenwood_transport::Transport;
    ///
    /// fn swap<T: Transport + Unpin + 'static, L>(
    ///     left: &mut DstarGateway<T, L>,
    ///     right: &mut DstarGateway<T, L>,
    /// ) {
    ///     std::mem::swap(left.modem_mut(), right.modem_mut());
    /// }
    /// ```
    #[must_use]
    pub const fn modem(&self) -> &DstarModem<StreamAdapter<T>> {
        &self.modem
    }

    /// Receive one shared D-STAR event while retaining the radio and its
    /// restore state.
    ///
    /// `Ok(None)` is a quiet poll interval, not a closed modem. Event decoding,
    /// automatic echo, and pending-event ordering belong to [`DstarModem`].
    ///
    /// # Errors
    ///
    /// Returns the shared runtime's terminal modem or echo-submission error.
    ///
    /// # Cancellation
    ///
    /// Waiting for input is cancellation-safe. Automatic echo playback is
    /// not cancellation-atomic; see [`DstarModem::next_event`].
    pub async fn next_event(&mut self) -> Result<Option<DstarEvent>, DstarError> {
        self.modem.next_event().await
    }

    /// Submit a D-STAR header through the shared modem's FIFO-gated TX queue.
    ///
    /// # Errors
    ///
    /// Returns the shared modem's closed-session or full-queue error.
    ///
    /// # Cancellation
    ///
    /// A queued header may still transmit after cancellation; see
    /// [`DstarModem::send_header`].
    pub async fn send_header(&mut self, header: &DstarHeader) -> Result<(), DstarError> {
        self.modem.send_header(header).await
    }

    /// Submit a voice frame without adding model-side pacing or re-encoding.
    ///
    /// # Errors
    ///
    /// Returns the shared modem's closed-session or full-queue error.
    ///
    /// # Cancellation
    ///
    /// A queued frame may still transmit after cancellation; see
    /// [`DstarModem::send_voice`].
    pub async fn send_voice(&mut self, frame: &VoiceFrame) -> Result<(), DstarError> {
        self.modem.send_voice(frame).await
    }

    /// Submit end-of-transmission through the shared modem's TX queue.
    ///
    /// # Errors
    ///
    /// Returns the shared modem's closed-session or full-queue error.
    ///
    /// # Cancellation
    ///
    /// A queued marker may still transmit after cancellation; see
    /// [`DstarModem::send_eot`].
    pub async fn send_eot(&mut self) -> Result<(), DstarError> {
        self.modem.send_eot().await
    }

    /// Submit a shared-runtime status header using the validated station identity.
    ///
    /// # Errors
    ///
    /// Returns the shared modem's closed-session or full-queue error.
    ///
    /// # Cancellation
    ///
    /// A queued header may still transmit after cancellation; see
    /// [`DstarModem::send_status_header`].
    pub async fn send_status_header(
        &mut self,
        reflector: Option<DstarStatusReflector>,
    ) -> Result<(), DstarError> {
        self.modem.send_status_header(reflector).await
    }

    /// Configure the shared runtime's receive-event poll interval.
    ///
    /// Inspect the current interval with [`DstarModem::event_timeout`] through
    /// [`Self::modem`]. This changes no radio setting and performs no I/O.
    pub const fn set_event_timeout(&mut self, timeout: Duration) {
        self.modem.set_event_timeout(timeout);
    }

    /// Request status while retaining intervening shared-runtime voice events.
    ///
    /// One two-second absolute deadline covers request submission, waiting for
    /// status, and dispatch of unrelated events; see
    /// [`DstarModem::poll_status`].
    ///
    /// # Errors
    ///
    /// Returns the shared runtime's timeout, modem, or echo-submission error.
    ///
    /// # Cancellation
    ///
    /// The request may already be queued and automatic echo may have submitted
    /// TX work. Cancellation does not roll either operation back.
    pub async fn poll_status(&mut self) -> Result<ModemStatus, DstarError> {
        self.modem.poll_status().await
    }

    async fn build_from_session(
        session: MmdvmSession<T, Lifecycle>,
        config: DstarModemConfig,
        wire_config: DstarConfiguration,
    ) -> Result<Self, (MmdvmRadioRestore<T>, AsyncModem<StreamAdapter<T>>, Error)> {
        let (modem, restore) = session.into_parts();
        match DstarModem::initialize(modem, config, wire_config).await {
            Ok(modem) => Ok(Self {
                modem,
                restore,
                lifecycle: PhantomData,
            }),
            Err((modem, error)) => Err((restore, modem, error.into())),
        }
    }
}

/// Retain the independent initialization and cleanup causes.
fn startup_cleanup_error(initialization: Error, cleanup: Error) -> Error {
    Error::DstarStartup {
        initialization: Box::new(initialization),
        cleanup: Some(Box::new(cleanup)),
        shutdown: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::radio::{BinaryProtocolProof, CatState};
    use kenwood_transport::MockTransport;
    use std::time::Duration;

    type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

    fn binary_radio(mock: MockTransport) -> Radio<MockTransport> {
        let mut radio = Radio::new(mock);
        radio.cat_state = CatState::BinaryProven(BinaryProtocolProof::Mmdvm { data_band: None });
        radio
    }

    #[tokio::test]
    async fn persistent_start_rejects_the_unverified_profile_before_conversion_or_io() -> TestResult
    {
        let radio = Radio::new(MockTransport::new());
        let Err((Some(radio), error)) =
            DstarGateway::start_gateway_mode(radio, DstarModemConfig::new("N0CALL")?).await
        else {
            return Err("unproved CAT radio was not returned after conversion refusal".into());
        };
        assert!(
            matches!(
                error,
                Error::MmdvmConfiguration(ConfigurationError::UnsupportedProfile {
                    profile: ConfigurationProfile::KenwoodTerminal
                })
            ),
            "{error:?}"
        );
        assert_eq!(radio.cat_state, CatState::Ready);
        assert!(radio.transport.writes().is_empty());
        radio.transport.assert_complete();
        Ok(())
    }

    #[tokio::test]
    async fn persistent_profile_refusal_retains_the_original_binary_proof() -> TestResult {
        let Err((Some(radio), error)) = DstarGateway::start_gateway_mode(
            binary_radio(MockTransport::new()),
            DstarModemConfig::new("N0CALL")?,
        )
        .await
        else {
            return Err("profile refusal did not return its binary radio".into());
        };
        assert!(
            matches!(
                error,
                Error::MmdvmConfiguration(ConfigurationError::UnsupportedProfile {
                    profile: ConfigurationProfile::KenwoodTerminal
                })
            ),
            "{error:?}"
        );
        assert_eq!(
            radio.cat_state,
            CatState::BinaryProven(BinaryProtocolProof::Mmdvm { data_band: None })
        );
        assert!(radio.transport.writes().is_empty());
        radio.transport.assert_complete();
        Ok(())
    }

    #[tokio::test]
    async fn transient_profile_refusal_sends_no_tnc_mode_command() -> TestResult {
        let Err((Some(radio), error)) = DstarGateway::start(
            Radio::new(MockTransport::new()),
            TncDataBand::B,
            DstarModemConfig::new("N0CALL")?,
        )
        .await
        else {
            return Err("unsupported transient profile did not preserve the radio".into());
        };
        assert!(
            matches!(
                error,
                Error::MmdvmConfiguration(ConfigurationError::UnsupportedProfile {
                    profile: ConfigurationProfile::KenwoodTerminal
                })
            ),
            "{error:?}"
        );
        assert_eq!(radio.cat_state, CatState::Ready);
        assert!(radio.transport.writes().is_empty());
        radio.transport.assert_complete();
        Ok(())
    }

    #[test]
    fn double_fault_error_carries_both_causes() {
        let init = Error::Timeout(Duration::from_secs(2));
        let exit = Error::CommandRejected {
            mnemonic: "0M".to_owned(),
        };
        let error = startup_cleanup_error(init, exit);
        assert!(
            matches!(error, Error::DstarStartup { initialization, cleanup: Some(cleanup), shutdown: None }
            if matches!(*initialization, Error::Timeout(_)) && matches!(*cleanup, Error::CommandRejected { ref mnemonic } if mnemonic == "0M")),
            "both typed causes must remain independently accessible"
        );
    }
}
