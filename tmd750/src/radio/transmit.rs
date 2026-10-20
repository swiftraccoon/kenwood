//! Transmit-capable CAT commands: PTT, APRS beacon, TNC mode, VOX and power.
//!
//! [`Radio::transmit`] keys the PTT band (the second [`BandControl`] field) on
//! its current frequency, mode and power level, and the radio transmits until
//! [`Radio::receive`], its time-out timer or power loss ends it.
//! [`Radio::send_aprs_beacon`] transmits one APRS packet when the TNC is in
//! APRS mode. [`Radio::set_tnc_mode`] with [`SelectableTncMode::Kiss`] hands
//! the TNC to the interface Menu 984 selects, and [`Radio::set_vox`] lets
//! microphone audio key the transmitter. [`Radio::power_off`] switches the
//! radio off. The caller is responsible for the license conditions of every
//! transmission.
//!
//! Every method except [`Radio::receive`] runs the [`super::WRITE_TARGETS`]
//! gate before sending a byte. `receive` is also accepted after a CAT exchange
//! failed, so an unanswered `TX` never leaves the transmitter unreleasable.
//!
//! [`BandControl`]: crate::types::BandControl

use std::time::Duration;

use crate::error::{CommandName, Error, ProtocolError};
use crate::protocol::cat::{Command, Response};
use crate::types::{Band, SelectableTncMode, TncMode, TncState, VoxMode};
use kenwood_transport::Transport;

use super::{Radio, unexpected};

/// Time [`Radio::set_vox`] waits after its readback before returning.
///
/// On firmware 1.02 a `VX` write sent immediately after the readback of the
/// previous `VX` write was answered `N`, and one sent 89 ms after it was
/// accepted.
pub const VOX_SETTLE: Duration = Duration::from_millis(100);

impl<T: Transport> Radio<T> {
    /// Key the PTT band's transmitter (`TX`).
    ///
    /// Returns after the radio answers `TX 1`, about 30 ms after the command
    /// on firmware 1.02. The transmitter stays keyed until [`Self::receive`];
    /// dropping this `Radio` does not release it. A timeout or a malformed
    /// acknowledgement leaves the transmitter state unknown, and only
    /// [`Self::receive`] is accepted afterwards.
    ///
    /// # Errors
    ///
    /// Returns [`Error::UnsupportedWriteTarget`] before sending,
    /// [`crate::ProtocolError::NotAvailable`] when the radio refuses to
    /// transmit, and transport, timeout, parse or session-state errors.
    pub async fn transmit(&mut self) -> Result<(), Error> {
        self.require_write_target().await?;
        self.key().await
    }

    /// Release the transmitter (`RX`).
    ///
    /// Sends `RX` without the write-target gate, and also on a connection
    /// whose last CAT exchange failed; a correlated reply makes that
    /// connection ready again. A connection inside or uncertain about a
    /// programming session refuses it.
    ///
    /// # Errors
    ///
    /// Returns [`crate::McpError::RecoveryRequired`],
    /// [`crate::McpError::SessionActive`] or
    /// [`crate::McpError::ConnectionRetired`] before sending in those states,
    /// [`crate::ProtocolError::Rejected`] or
    /// [`crate::ProtocolError::NotAvailable`] for `?` or `N`, and transport,
    /// timeout or parse errors.
    pub async fn receive(&mut self) -> Result<(), Error> {
        match self.release_command().await? {
            Response::ReceiveAck => Ok(()),
            Response::Rejected => Err(ProtocolError::Rejected {
                command: CommandName::Rx,
            }
            .into()),
            Response::NotAvailable => Err(ProtocolError::NotAvailable {
                command: CommandName::Rx,
            }
            .into()),
            other => Err(unexpected(CommandName::Rx, &other)),
        }
    }

    /// Key the transmitter, await `stop`, then release it.
    ///
    /// `stop` is first polled after the radio acknowledges `TX`, so a future
    /// that is already complete still keys the transmitter once. When the
    /// radio refuses `TX`, `stop` is never polled and no `RX` is sent. When
    /// the `TX` exchange fails after the command may have reached the radio
    /// (a timeout, a transport failure or a malformed acknowledgement), `RX`
    /// is sent at once and the `TX` error returns after the radio
    /// acknowledges it. On firmware 1.02 the radio answers `RX` whether or not
    /// it is transmitting. Await this future to completion: dropping it while
    /// `stop` is pending leaves the transmitter keyed until [`Self::receive`].
    ///
    /// # Errors
    ///
    /// Returns [`Error::ReleaseUnconfirmed`] when the radio does not
    /// acknowledge an `RX` this method sent, the one error after which the
    /// transmitter may still be keyed; otherwise the errors of
    /// [`Self::transmit`], after which the transmitter is released or was
    /// never keyed.
    pub async fn transmit_until(
        &mut self,
        stop: impl Future<Output = ()> + Send,
    ) -> Result<(), Error> {
        self.require_write_target().await?;
        match self.key().await {
            Ok(()) => {}
            Err(error) if refused(&error) => return Err(error),
            Err(error) => return self.release(Some(error)).await,
        }
        stop.await;
        self.release(None).await
    }

    /// Key the transmitter, wait `duration`, then release it.
    ///
    /// [`Self::transmit_until`] with a `duration` sleep as the stop.
    ///
    /// # Errors
    ///
    /// Returns the errors of [`Self::transmit_until`].
    pub async fn transmit_for(&mut self, duration: Duration) -> Result<(), Error> {
        self.transmit_until(tokio::time::sleep(duration)).await
    }

    /// Transmit one APRS beacon (`BE`).
    ///
    /// The radio answers `BE` with the TNC in APRS mode and `N` with it off.
    /// The packet carries the APRS My Callsign and the selected position
    /// source, on the TNC data band's frequency.
    ///
    /// # Errors
    ///
    /// Returns [`Error::UnsupportedWriteTarget`] before sending,
    /// [`crate::ProtocolError::NotAvailable`] when the TNC is not in APRS
    /// mode, and transport, timeout, parse or session-state errors.
    pub async fn send_aprs_beacon(&mut self) -> Result<(), Error> {
        self.require_write_target().await?;
        self.query(Command::SendAprsBeacon, |response| {
            matches!(response, Response::AprsBeaconAck).then_some(())
        })
        .await
    }

    /// Select the TNC mode and its data band (`TN mode,band`) with echo and
    /// readback.
    ///
    /// [`SelectableTncMode::Aprs`] starts the radio's APRS application, which
    /// transmits beacons by itself under the `Auto` and `SmartBeaconing`
    /// [`crate::types::BeaconMethod`]s; set `Manual` first to transmit only
    /// on [`Self::send_aprs_beacon`]. [`SelectableTncMode::Kiss`] hands the TNC
    /// to the Menu 984 interface. On firmware 1.02 a `TN 2,band` sent over the
    /// operation-panel USB endpoint, with Menu 984 selecting the main-unit USB
    /// port, re-enumerated that endpoint before any reply, so the call returns
    /// a transport error, and the TNC read off on the next connection.
    ///
    /// # Errors
    ///
    /// Returns [`Error::UnsupportedWriteTarget`] before sending,
    /// [`crate::ProtocolError::EchoMismatch`] or
    /// [`crate::ProtocolError::ReadbackMismatch`], and transport, timeout,
    /// parse, rejection or session-state errors.
    pub async fn set_tnc_mode(
        &mut self,
        mode: SelectableTncMode,
        data_band: Band,
    ) -> Result<(), Error> {
        self.apply(
            Command::SetTncMode { mode, data_band },
            Command::GetTncMode,
            TncState {
                mode: TncMode::from(mode),
                data_band,
            },
            tnc_reply,
        )
        .await
    }

    /// Switch VOX (`VX 0|1`) with echo and readback, then wait
    /// [`VOX_SETTLE`].
    ///
    /// With VOX on, microphone audio above the VOX gain threshold keys the
    /// transmitter.
    ///
    /// # Errors
    ///
    /// Returns [`Error::UnsupportedWriteTarget`] before sending,
    /// [`crate::ProtocolError::EchoMismatch`] or
    /// [`crate::ProtocolError::ReadbackMismatch`], and transport, timeout,
    /// parse, rejection or session-state errors.
    pub async fn set_vox(&mut self, enabled: bool) -> Result<(), Error> {
        self.apply(
            Command::SetVox { enabled },
            Command::GetVox,
            VoxMode::from(enabled),
            |response| match response {
                Response::Vox(mode) => Some(*mode),
                _ => None,
            },
        )
        .await?;
        tokio::time::sleep(VOX_SETTLE).await;
        Ok(())
    }

    /// Switch the radio off (`PS 0`).
    ///
    /// Returns after a `PS 0` reply. This command has not been sent to a
    /// radio, so its reply and what follows on the connection are
    /// unverified.
    ///
    /// # Errors
    ///
    /// Returns [`Error::UnsupportedWriteTarget`] before sending and transport,
    /// timeout, parse, rejection or session-state errors.
    pub async fn power_off(&mut self) -> Result<(), Error> {
        self.require_write_target().await?;
        self.query(Command::PowerOff, |response| {
            matches!(response, Response::PowerStatus { on: false }).then_some(())
        })
        .await
    }

    /// Send `TX` and require `TX 1`, without the write-target gate.
    async fn key(&mut self) -> Result<(), Error> {
        self.query(Command::Transmit, |response| {
            matches!(response, Response::TransmitAck).then_some(())
        })
        .await
    }

    /// Send `RX` after a `TX` that may have keyed the transmitter, where
    /// `transmit` is the `TX` exchange's failure.
    async fn release(&mut self, transmit: Option<Error>) -> Result<(), Error> {
        match (self.receive().await, transmit) {
            (Ok(()), None) => Ok(()),
            (Ok(()), Some(transmit)) => Err(transmit),
            (Err(release), transmit) => Err(Error::ReleaseUnconfirmed {
                transmit: transmit.map(Box::new),
                release: Box::new(release),
            }),
        }
    }
}

/// Whether `error`, from the `TX` exchange, shows that `TX` was never sent
/// (a session-state refusal) or that the radio refused it with `?` or `N`.
const fn refused(error: &Error) -> bool {
    matches!(
        error,
        Error::Mcp(_)
            | Error::Protocol(ProtocolError::Rejected { .. } | ProtocolError::NotAvailable { .. })
    )
}

/// The TNC state a `TN` reply carries.
pub(super) const fn tnc_reply(response: &Response) -> Option<TncState> {
    match response {
        Response::TncMode { mode, data_band } => Some(TncState {
            mode: *mode,
            data_band: *data_band,
        }),
        _ => None,
    }
}
