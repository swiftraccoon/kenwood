//! TH-D75 lifecycle ownership around the shared D-STAR modem runtime.
//!
//! [`DstarGateway`] retains CAT state and model entry/exit policy. Framing,
//! initialization, event processing, voice and slow data live in
//! [`mmdvm::dstar`]. Voice operations delegate to that runtime while keeping
//! its transport paired with this owner's restoration state. Stop this owner
//! to reclaim the radio with the correct lifecycle evidence.

use std::marker::PhantomData;
use std::time::Duration;

use dstar_gateway_core::{DstarHeader, VoiceFrame};
use kenwood_transport::{StreamAdapter, Transport, TransportError};
use mmdvm::AsyncModem;
use mmdvm::core::ModemStatus;
use mmdvm::dstar::{DstarError, DstarEvent, DstarModem, DstarModemConfig, DstarStatusReflector};

use crate::Error;
use crate::radio::mmdvm_session::{
    MmdvmRadioRestore, MmdvmSession, PersistentMmdvm, TransientMmdvm,
};
use crate::radio::{DesyncedRadio, Radio};
use crate::types::TncDataBand;

/// A shared D-STAR modem plus the TH-D75 state needed to stop it safely.
///
/// Transient owners enter and exit through the selected TNC data band.
/// Persistent owners require an already-proved binary link and never send
/// the transient ASCII exit. Stopping host processing alone does not prove
/// that CAT is ready.
///
/// ```rust,no_run
/// use kenwood_thd75::{DstarGateway, Radio};
/// use kenwood_thd75::transport::SerialTransport;
/// use kenwood_thd75::types::TncDataBand;
/// use mmdvm::dstar::DstarModemConfig;
///
/// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
/// let config = DstarModemConfig::new("N0CALL")?;
/// let radio = Radio::new(SerialTransport::open("/dev/tty-example")?);
/// let mut gateway = DstarGateway::start(radio, TncDataBand::B, config)
///     .await.map_err(|(_, error)| error)?;
/// let _event = gateway.next_event().await?;
/// let radio = gateway.stop().await?.restore().await
///     .map_err(|(_, error)| error)?;
/// # drop(radio);
/// # Ok(())
/// # }
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
    /// The shared configuration contains no radio-band policy. This owner
    /// performs `TN 3,x` and retains the same band for `TN 0,x` cleanup.
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
        let session = match radio.enter_mmdvm(data_band).await {
            Ok(session) => session,
            Err((radio, error)) => return Err((Some(radio), error)),
        };
        match Self::build_from_session(session, config).await {
            Ok(gateway) => Ok(gateway),
            Err((restore, modem, init_error)) => match restore.exit_and_rebuild(modem).await {
                Ok(desynced) => Err((Some(desynced.into_radio_unproven()), init_error)),
                Err(exit_error) => Err((None, double_fault_error(&init_error, &exit_error))),
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
    /// No successful CAT restoration is implied.
    pub async fn stop(self) -> Result<DesyncedRadio<T>, Error> {
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
        let session = match radio.into_mmdvm_session() {
            Ok(session) => session,
            Err((radio, error)) => return Err((Some(radio), error)),
        };
        match Self::build_from_session(session, config).await {
            Ok(gateway) => Ok(gateway),
            Err((restore, modem, init_error)) => {
                match restore.shutdown_and_rebuild_binary(modem).await {
                    Ok(radio) => Err((Some(radio), init_error)),
                    Err(reclaim_error) => Err((
                        None,
                        binary_reclaim_fault_error(&init_error, &reclaim_error),
                    )),
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
    /// Fails if either pump cannot be reclaimed cleanly. A failed pump cannot
    /// manufacture a CAT-ready or binary-proved replacement radio.
    pub async fn stop(self) -> Result<Radio<T>, Error> {
        self.restore
            .shutdown_and_rebuild_binary(self.modem.into_modem())
            .await
    }
}

impl<T: Transport + Unpin + 'static, Lifecycle> DstarGateway<T, Lifecycle> {
    /// Inspect shared modem configuration and receive state.
    ///
    /// The runtime must remain paired with this owner's radio restore state.
    /// Inspection therefore cannot provide a mutable runtime reference:
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
    /// No separate mutable-owner accessor exists:
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

    /// Receive a shared D-STAR event without releasing model lifecycle ownership.
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
    /// The two-second timeout is between received events, not an absolute
    /// operation deadline; see [`DstarModem::poll_status`].
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
    ) -> Result<Self, (MmdvmRadioRestore<T>, AsyncModem<StreamAdapter<T>>, Error)> {
        let (modem, restore) = session.into_parts();
        match DstarModem::initialize(modem, config).await {
            Ok(modem) => Ok(Self {
                modem,
                restore,
                lifecycle: PhantomData,
            }),
            Err((modem, error)) => Err((restore, modem, error.into())),
        }
    }
}

/// Preserve both failures when transient initialization and rollback fail.
fn double_fault_error(init: &Error, exit: &Error) -> Error {
    Error::Transport(TransportError::Disconnected(std::io::Error::other(
        format!(
            "radio unrecoverable: D-STAR init failed ({init}) and MMDVM exit failed ({exit}); reconnect from scratch"
        ),
    )))
}

/// Preserve both failures without carrying binary proof across a new connection.
fn binary_reclaim_fault_error(init: &Error, reclaim: &Error) -> Error {
    Error::Transport(TransportError::Disconnected(std::io::Error::other(
        format!(
            "radio unrecoverable: D-STAR init failed ({init}) and the persistent MMDVM link could not be reclaimed ({reclaim}); reconnect and diagnose from scratch"
        ),
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::radio::{BinaryProtocolProof, CatState};
    use kenwood_transport::MockTransport;
    use mmdvm_core::MMDVM_SET_CONFIG;
    use std::time::Duration;

    type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

    fn prepared_mock() -> MockTransport {
        let mut mock = MockTransport::new();
        mock.expect_any_write();
        mock.pend_when_empty();
        mock
    }

    fn binary_radio(mock: MockTransport) -> Radio<MockTransport> {
        let mut radio = Radio::new(mock);
        radio.cat_state = CatState::BinaryProven(BinaryProtocolProof::Mmdvm { data_band: None });
        radio
    }

    fn initialization_replies(mock: &mut MockTransport) {
        mock.queue_read_delayed(&[0xE0, 4, 0x70, 0x02], 20);
        mock.queue_read_delayed(&[0xE0, 4, 0x70, 0x03], 150);
    }

    #[tokio::test]
    async fn persistent_start_conversion_failure_returns_the_intact_radio() -> TestResult {
        let radio = Radio::new(MockTransport::new());
        let Err((Some(radio), error)) =
            DstarGateway::start_gateway_mode(radio, DstarModemConfig::new("N0CALL")?).await
        else {
            return Err("unproved CAT radio was not returned after conversion refusal".into());
        };
        assert!(matches!(error, Error::BinaryModeNotProven));
        assert_eq!(radio.cat_state, CatState::Ready);
        assert!(radio.transport.writes().is_empty());
        radio.transport.assert_complete();
        Ok(())
    }

    #[tokio::test]
    async fn persistent_init_nak_returns_a_retryable_binary_radio() -> TestResult {
        let mut mock = prepared_mock();
        mock.queue_read_delayed(&[0xE0, 5, 0x7F, MMDVM_SET_CONFIG, 4], 20);
        let Err((Some(radio), error)) =
            DstarGateway::start_gateway_mode(binary_radio(mock), DstarModemConfig::new("N0CALL")?)
                .await
        else {
            return Err("persistent init NAK did not return its binary radio".into());
        };
        assert!(matches!(error, Error::Dstar(_)));
        assert_eq!(
            radio.cat_state,
            CatState::BinaryProven(BinaryProtocolProof::Mmdvm { data_band: None })
        );
        let session = radio.into_mmdvm_session().map_err(|(_, error)| error)?;
        let radio = session.shutdown().await?;
        assert!(
            radio
                .transport
                .writes()
                .iter()
                .all(|write| write.first() == Some(&0xE0))
        );
        radio.transport.assert_complete();
        Ok(())
    }

    #[tokio::test]
    async fn stop_uses_the_entry_lifecycles_distinct_exit_paths() -> TestResult {
        let mut mock = prepared_mock();
        mock.queue_read(b"TN 3,1\r");
        initialization_replies(&mut mock);
        let transient = DstarGateway::start(
            Radio::new(mock),
            TncDataBand::B,
            DstarModemConfig::new("N0CALL")?,
        )
        .await
        .map_err(|(_, error)| error)?;
        let transient = transient.stop().await?.into_radio_unproven();
        assert_eq!(transient.cat_state, CatState::RecoveryRequired);
        assert!(
            transient
                .transport
                .writes()
                .iter()
                .any(|write| write == b"TN 0,1\r"),
            "transient MMDVM must exit using its entry band"
        );
        let mut mock = prepared_mock();
        initialization_replies(&mut mock);
        let persistent =
            DstarGateway::start_gateway_mode(binary_radio(mock), DstarModemConfig::new("N0CALL")?)
                .await
                .map_err(|(_, error)| error)?;
        let persistent = persistent.stop().await?;
        assert_eq!(
            persistent.cat_state,
            CatState::BinaryProven(BinaryProtocolProof::Mmdvm { data_band: None })
        );
        assert!(
            persistent
                .transport
                .writes()
                .iter()
                .all(|write| write.first() == Some(&0xE0)),
            "persistent MMDVM must never receive a transient ASCII exit"
        );
        persistent.transport.assert_complete();
        Ok(())
    }

    #[test]
    fn double_fault_error_carries_both_causes() {
        let init = Error::Timeout(Duration::from_secs(2));
        let exit = Error::CommandRejected {
            mnemonic: "0M".to_owned(),
        };
        for error in [
            double_fault_error(&init, &exit),
            binary_reclaim_fault_error(&init, &exit),
        ] {
            let mut messages = Vec::new();
            let mut source: Option<&dyn std::error::Error> = Some(&error);
            while let Some(error) = source {
                messages.push(error.to_string());
                source = error.source();
            }
            let chain = messages.join(" | ");
            assert!(
                chain.contains("timed out") && chain.contains("rejected the 0M command"),
                "both failures must remain visible: {chain}"
            );
        }
    }
}
