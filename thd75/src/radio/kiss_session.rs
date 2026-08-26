//! KISS TNC session management for the TH-D75.
//!
//! When the radio enters KISS mode (via `TN 2,x`), the serial port switches
//! from ASCII CAT commands to binary KISS framing. CAT commands cannot be
//! used until KISS mode is exited. The [`KissSession`] type enforces this
//! at the type level: creating one consumes the [`Radio`], and exiting
//! returns it.
//!
//! # Example
//!
//! ```rust,no_run
//! # use kenwood_thd75::radio::Radio;
//! # use kenwood_thd75::transport::SerialTransport;
//! # use kenwood_thd75::types::TncDataBand;
//! # async fn example() -> Result<(), kenwood_thd75::error::Error> {
//! let transport = SerialTransport::open("/dev/cu.usbmodem1234")?;
//! let radio = Radio::new(transport);
//!
//! // Enter KISS mode (consumes the Radio).
//! let mut kiss = radio.enter_kiss(TncDataBand::A).await.map_err(|(_, e)| e)?;
//!
//! // Send and receive KISS frames.
//! use kiss_tnc::KissFrame;
//! let frame = KissFrame::data(vec![/* AX.25 */]);
//! kiss.send_frame(&frame).await?;
//!
//! // Exit KISS mode; the desynchronized radio must be restored (or
//! // taken explicitly unproven) before ordinary CAT commands.
//! let radio = kiss
//!     .exit()
//!     .await
//!     .map_err(|(_session, e)| e)?
//!     .restore()
//!     .await
//!     .map_err(|(_desynced, e)| e)?;
//! # Ok(())
//! # }
//! ```

use std::time::Duration;

use kiss_tnc::{
    FEND, KissCommand, KissError, KissFrame, KissPort, decode_kiss_frame, encode_kiss_frame,
};

use crate::error::{Error, ProtocolError, TransportError};
use crate::protocol::{Command, Response};
use crate::transport::Transport;
use crate::types::{
    KissDuplex, KissPersistence, KissSlotTime, KissTxDelay, KissTxTail, PacketDataRate,
    TncDataBand, TncMode,
};

use super::{BinaryProtocolProof, DesyncedRadio, Radio, cat_restore_state::CatRestoreState};

/// Default timeout for KISS receive operations (10 seconds).
const KISS_RECEIVE_TIMEOUT: Duration = Duration::from_secs(10);

/// A KISS TNC session that owns the radio transport.
///
/// While this session is active, the serial port speaks KISS binary framing
/// instead of ASCII CAT commands. The [`Radio`] is consumed on entry and
/// returned on [`exit`](Self::exit).
///
/// # KISS commands supported by TH-D75
///
/// | Command | Code | Range | Default |
/// |---------|------|-------|---------|
/// | Data Frame | `0x00` | AX.25 payload | n/a |
/// | TX Delay | `0x01` | 0-1200 ms (10 ms steps) | Menu 508 |
/// | Persistence | `0x02` | 0-255 | 128 |
/// | Slot Time | `0x03` | 0-2500 ms (10 ms steps) | 100 ms |
/// | TX Tail | `0x04` | 0-2550 ms (10 ms steps) | 30 ms |
/// | Full Duplex | `0x05` | 0=half, nonzero=full | 0 |
/// | Set Hardware | `0x06` | 0/0x23=1200, 0x05/0x26=9600 | Menu 505 |
/// | Return | `0xFF` | n/a | n/a |
pub struct KissSession<T: Transport> {
    /// The underlying transport (serial or Bluetooth).
    pub(crate) transport: T,
    /// CAT state retained for a consistent return from binary KISS framing.
    cat_restore: CatRestoreState,
    /// Timeout for receive operations.
    receive_timeout: Duration,
    /// Internal buffer for accumulating KISS bytes from the transport.
    read_buf: Vec<u8>,
}

impl<T: Transport> std::fmt::Debug for KissSession<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KissSession")
            .field("receive_timeout", &self.receive_timeout)
            .field("read_buf_len", &self.read_buf.len())
            .finish_non_exhaustive()
    }
}

impl<T: Transport> Radio<T> {
    /// Send the single CAT command that transitions this link into KISS mode.
    ///
    /// A successful return means the radio has already changed protocols; the
    /// caller must promptly consume this radio through
    /// [`Self::into_kiss_session`]. A correlated `N` or `?` response leaves
    /// the CAT boundary ready and returns the semantic error without requiring
    /// packet-mode recovery.
    ///
    /// # Errors
    ///
    /// Returns [`Error::NotAvailableInCurrentMode`] for an aligned `TN` `N`
    /// response. Timeout, transport, malformed-boundary, and cancellation
    /// failures leave CAT recovery required.
    pub async fn transition_to_kiss(&mut self, data_band: TncDataBand) -> Result<(), Error> {
        tracing::info!(?data_band, "transitioning to KISS mode");
        let response = self
            .execute(Command::SetTncMode {
                mode: TncMode::Kiss,
                data_band,
            })
            .await?;
        match response {
            Response::TncMode {
                mode: TncMode::Kiss,
                data_band: response_data_band,
            } if response_data_band == data_band => {
                // The exact TN echo is the CAT-side proof that the transport
                // may now be consumed by the typed binary session.
                self.cat_state =
                    super::CatState::BinaryProven(BinaryProtocolProof::Kiss(data_band));
                Ok(())
            }
            other => {
                self.cat_state = super::CatState::RecoveryRequired;
                Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                    expected: format!("TncMode {{ mode: Kiss, data_band: {data_band:?} }}"),
                    actual: format!("{other:?}").into_bytes(),
                }))
            }
        }
    }

    /// Consume a radio whose exact KISS transition has already completed.
    ///
    /// This performs no I/O. The protocol-specific proof stored on this exact
    /// radio is consumed with it, so a KISS transition cannot authorize a
    /// different radio or an MMDVM-proved link.
    ///
    /// # Errors
    ///
    /// Returns the intact radio when its live boundary is not binary-proved or
    /// another strict protocol still owns the transport.
    #[expect(
        clippy::result_large_err,
        reason = "the ownership-preserving failure must return the intact Radio so callers can recover it"
    )]
    pub fn into_kiss_session(self) -> Result<KissSession<T>, (Self, Error)> {
        let super::CatState::BinaryProven(BinaryProtocolProof::Kiss(data_band)) = self.cat_state
        else {
            return Err((self, Error::BinaryModeNotProven));
        };
        tracing::debug!(?data_band, "consuming proved KISS transition");
        let (transport, cat_restore) =
            self.into_binary_mode_parts(BinaryProtocolProof::Kiss(data_band))?;
        Ok(KissSession {
            transport,
            cat_restore,
            receive_timeout: KISS_RECEIVE_TIMEOUT,
            read_buf: Vec::with_capacity(512),
        })
    }

    /// Enter KISS mode, consuming this [`Radio`] and returning a [`KissSession`].
    ///
    /// Sends the `TN 2,x` CAT command to switch the TNC to KISS mode at the
    /// specified TNC data band. After this call, the serial port speaks KISS
    /// binary framing. Use [`KissSession::exit`] to return to CAT mode.
    ///
    /// # Errors
    ///
    /// On failure, returns the [`Radio`] alongside the error. If the transition
    /// write may have reached the radio, that handle rejects ordinary CAT until
    /// [`Radio::restore_cat_after_mode_exit`] proves the framing boundary.
    pub async fn enter_kiss(
        mut self,
        data_band: TncDataBand,
    ) -> Result<KissSession<T>, (Self, Error)> {
        match self.transition_to_kiss(data_band).await {
            Ok(()) => {}
            Err(e) => {
                if matches!(
                    e,
                    Error::Timeout(_) | Error::Transport(_) | Error::Protocol(_)
                ) {
                    self.cat_state = super::CatState::RecoveryRequired;
                }
                return Err((self, e));
            }
        }
        self.into_kiss_session()
    }
}

impl<T: Transport> KissSession<T> {
    /// Set the timeout for [`receive_frame`](Self::receive_frame) operations.
    ///
    /// Defaults to 10 seconds. Set higher for quiet channels.
    pub const fn set_receive_timeout(&mut self, duration: Duration) {
        self.receive_timeout = duration;
    }

    /// Write pre-encoded KISS wire bytes directly to the transport.
    ///
    /// Use this when you already have a fully KISS-encoded frame (e.g.,
    /// from [`build_aprs_message`](::aprs::build_aprs_message) or
    /// [`AprsMessenger::next_frame_to_send`](::aprs::AprsMessenger::next_frame_to_send)).
    /// Unlike [`send_frame`](Self::send_frame) and
    /// [`send_data`](Self::send_data), this does **not** perform any
    /// additional encoding.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Transport`] if the write fails.
    pub async fn send_wire(&mut self, wire: &[u8]) -> Result<(), Error> {
        tracing::debug!(wire_len = wire.len(), "KISS TX (raw wire)");
        self.transport.write(wire).await.map_err(Error::Transport)
    }

    /// Send a KISS frame to the TNC.
    ///
    /// The frame is KISS-encoded (with FEND delimiters and byte stuffing)
    /// before transmission.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Transport`] if the write fails.
    pub async fn send_frame(&mut self, frame: &KissFrame) -> Result<(), Error> {
        let wire = encode_kiss_frame(frame);
        tracing::debug!(
            command = ?frame.command,
            data_len = frame.data.len(),
            wire_len = wire.len(),
            "KISS TX"
        );
        self.transport.write(&wire).await.map_err(Error::Transport)
    }

    /// Receive a KISS frame from the TNC.
    ///
    /// Blocks until a complete KISS frame is received or the receive timeout
    /// expires. Accumulates bytes from the transport and extracts frames
    /// delimited by FEND bytes.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Timeout`] if no complete frame arrives within the
    /// configured receive timeout.
    /// Returns [`Error::Transport`] if the read fails.
    /// Returns [`Error::Kiss`] when the peer sends a malformed KISS frame or
    /// an unterminated frame exceeds the bounded receive buffer.
    pub async fn receive_frame(&mut self) -> Result<KissFrame, Error> {
        let timeout_dur = self.receive_timeout;
        tokio::time::timeout(timeout_dur, self.receive_frame_inner())
            .await
            .map_err(|_| Error::Timeout(timeout_dur))?
    }

    /// Inner receive loop that accumulates bytes and extracts KISS frames.
    async fn receive_frame_inner(&mut self) -> Result<KissFrame, Error> {
        let mut tmp = [0u8; 1024];
        loop {
            // Try to extract a frame from the buffer first.
            if let Some(frame) = Self::try_extract_frame(&mut self.read_buf)? {
                return Ok(frame);
            }
            if self.read_buf.len() > Self::MAX_READ_BUF {
                // An unterminated frame that crossed the bound cannot have a
                // trustworthy suffix. Reset framing completely so the next
                // receive begins at a newly observed delimiter.
                self.read_buf.clear();
                return Err(KissError::FrameTooLong.into());
            }

            // Read more bytes from the transport.
            let n = self
                .transport
                .read(&mut tmp)
                .await
                .map_err(Error::Transport)?;
            if n == 0 {
                return Err(Error::Transport(TransportError::Disconnected(
                    std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "connection closed"),
                )));
            }
            let Some(chunk) = tmp.get(..n) else {
                return Err(Error::Transport(TransportError::Read(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "transport reported more KISS bytes than the supplied read buffer",
                ))));
            };
            let Some(next_len) = self.read_buf.len().checked_add(chunk.len()) else {
                self.read_buf.clear();
                return Err(KissError::FrameTooLong.into());
            };
            self.read_buf.reserve(next_len - self.read_buf.len());
            self.read_buf.extend_from_slice(chunk);
        }
    }

    /// Maximum RX buffer size (64 KB), mirroring the CAT codec's cap.
    const MAX_READ_BUF: usize = 64 * 1024;

    /// Current RX buffer length (test instrumentation).
    #[cfg(test)]
    pub(crate) const fn read_buf_len(&self) -> usize {
        self.read_buf.len()
    }

    /// Try to extract a complete KISS frame from the buffer.
    ///
    /// A frame starts with FEND and ends with FEND. If found, the frame bytes
    /// are removed from the buffer and decoded. Leading FENDs (inter-frame
    /// fill) are consumed. A malformed complete frame is removed and returned
    /// as its precise [`KissError`]; any independently framed bytes behind it
    /// remain buffered for the next call.
    fn try_extract_frame(buf: &mut Vec<u8>) -> Result<Option<KissFrame>, KissError> {
        // A frame can only start at a FEND. Discard inter-frame
        // noise (stray CAT/NMEA bytes, line garbage) up to the
        // first FEND (or everything, if no FEND exists), so one
        // stray byte can never block extraction forever.
        match buf.iter().position(|&b| b == FEND) {
            Some(0) => {}
            Some(start) => {
                tracing::warn!(discarded = start, "discarding pre-frame garbage bytes");
                drop(buf.drain(..start));
            }
            None => {
                if !buf.is_empty() {
                    tracing::debug!(discarded = buf.len(), "discarding FEND-free noise bytes");
                    buf.clear();
                }
                return Ok(None);
            }
        }

        // Skip leading duplicate FENDs (inter-frame fill).
        while matches!(buf.first(), Some(&FEND)) && matches!(buf.get(1), Some(&FEND)) {
            let _removed: u8 = buf.remove(0);
        }

        // Need at least FEND + type + FEND.
        if buf.len() < 3 || buf.first() != Some(&FEND) {
            return Ok(None);
        }

        // Find the closing FEND after the opening one.
        let Some(tail) = buf.get(1..) else {
            return Ok(None);
        };
        let Some(end_pos) = tail.iter().position(|&b| b == FEND) else {
            return Ok(None);
        };
        let frame_end = end_pos + 2; // Include the closing FEND.
        if frame_end > Self::MAX_READ_BUF {
            buf.clear();
            return Err(KissError::FrameTooLong);
        }

        let frame_bytes: Vec<u8> = buf.drain(..frame_end).collect();
        let frame = decode_kiss_frame(&frame_bytes)?;
        tracing::debug!(
            command = ?frame.command,
            data_len = frame.data.len(),
            "KISS RX"
        );
        Ok(Some(frame))
    }

    /// Set the TNC TX delay (KISS command `0x01`).
    ///
    /// The TH-D75 supports 0 through 1200 milliseconds in 10 millisecond
    /// steps. The default is configured via Menu No. 508.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Transport`] if the write fails.
    pub async fn set_tx_delay(&mut self, delay: KissTxDelay) -> Result<(), Error> {
        tracing::debug!(
            milliseconds = delay.as_milliseconds(),
            "setting KISS TX delay"
        );
        self.send_frame(&KissFrame {
            port: KissPort::TH_D75,
            command: KissCommand::TxDelay,
            data: vec![delay.to_wire_byte()],
        })
        .await
    }

    /// Set the CSMA persistence parameter (KISS command `0x02`).
    ///
    /// Range 0-255. The probability of transmitting when the channel is
    /// clear is `(persistence + 1) / 256`. Default: 128 (50%).
    ///
    /// # Errors
    ///
    /// Returns [`Error::Transport`] if the write fails.
    pub async fn set_persistence(&mut self, persistence: KissPersistence) -> Result<(), Error> {
        tracing::debug!(%persistence, "setting KISS persistence");
        self.send_frame(&KissFrame {
            port: KissPort::TH_D75,
            command: KissCommand::Persistence,
            data: vec![persistence.to_wire_byte()],
        })
        .await
    }

    /// Set the CSMA slot time (KISS command `0x03`).
    ///
    /// The TH-D75 supports 0 through 2500 milliseconds in 10 millisecond
    /// steps. The default is 100 milliseconds.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Transport`] if the write fails.
    pub async fn set_slot_time(&mut self, slot_time: KissSlotTime) -> Result<(), Error> {
        tracing::debug!(
            milliseconds = slot_time.as_milliseconds(),
            "setting KISS slot time"
        );
        self.send_frame(&KissFrame {
            port: KissPort::TH_D75,
            command: KissCommand::SlotTime,
            data: vec![slot_time.to_wire_byte()],
        })
        .await
    }

    /// Set the TX tail time (KISS command `0x04`).
    ///
    /// KISS represents 0 through 2550 milliseconds in 10 millisecond steps.
    /// The TH-D75 default is 30 milliseconds.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Transport`] if the write fails.
    pub async fn set_tx_tail(&mut self, tx_tail: KissTxTail) -> Result<(), Error> {
        tracing::debug!(
            milliseconds = tx_tail.as_milliseconds(),
            "setting KISS TX tail"
        );
        self.send_frame(&KissFrame {
            port: KissPort::TH_D75,
            command: KissCommand::TxTail,
            data: vec![tx_tail.to_wire_byte()],
        })
        .await
    }

    /// Set full or half duplex mode (KISS command `0x05`).
    ///
    /// # Errors
    ///
    /// Returns [`Error::Transport`] if the write fails.
    pub async fn set_duplex(&mut self, duplex: KissDuplex) -> Result<(), Error> {
        tracing::debug!(%duplex, "setting KISS duplex mode");
        self.send_frame(&KissFrame {
            port: KissPort::TH_D75,
            command: KissCommand::FullDuplex,
            data: vec![duplex.to_wire_byte()],
        })
        .await
    }

    /// Switch the packet data rate via KISS hardware command (`0x06`).
    ///
    /// The TH-D75 uses canonical value `0x00` for 1200 bps AFSK and `0x05`
    /// for 9600 bps GMSK. Received devices may also expose aliases `0x23`
    /// and `0x26`, but setters emit the canonical value.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Transport`] if the write fails.
    pub async fn set_hardware_data_rate(&mut self, data_rate: PacketDataRate) -> Result<(), Error> {
        let value = match data_rate {
            PacketDataRate::Bps1200 => 0x00,
            PacketDataRate::Bps9600 => 0x05,
        };
        tracing::debug!(%data_rate, value, "setting KISS hardware data rate");
        self.send_frame(&KissFrame {
            port: KissPort::TH_D75,
            command: KissCommand::SetHardware,
            data: vec![value],
        })
        .await
    }

    /// Send an AX.25 data frame via KISS.
    ///
    /// Wraps the raw AX.25 bytes in a KISS data frame (`CMD_DATA = 0x00`)
    /// and sends it.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Transport`] if the write fails.
    pub async fn send_data(&mut self, ax25_bytes: &[u8]) -> Result<(), Error> {
        self.send_frame(&KissFrame::data(ax25_bytes.to_vec())).await
    }

    /// Exit KISS mode by sending the `CMD_RETURN` (`0xFF`) frame.
    ///
    /// Binary bytes may remain buffered after the return frame, so the
    /// radio comes back wrapped in [`DesyncedRadio`]: call
    /// [`DesyncedRadio::restore`] to drain the residue and re-prove the
    /// CAT boundary before ordinary commands.
    ///
    /// # Errors
    ///
    /// Returns the session back together with the error if the exit
    /// write fails, so the already-owned transport survives for an exact
    /// retry without an unnecessary reconnect.
    pub async fn exit(mut self) -> Result<DesyncedRadio<T>, (Self, Error)> {
        tracing::info!("exiting KISS mode");
        if let Err(e) = self.send_frame(&KissFrame::return_command()).await {
            return Err((self, e));
        }

        // Small delay to let the TNC switch back to CAT mode.
        tokio::time::sleep(Duration::from_millis(100)).await;

        Ok(DesyncedRadio::new(
            self.cat_restore.rebuild_desynchronized(self.transport),
        ))
    }

    /// Reclaim the [`Radio`] after a failed [`exit`](Self::exit) without
    /// retrying the exit.
    ///
    /// The KISS Return frame may never have reached the TNC, so the
    /// radio may still be in KISS mode. The returned radio is marked
    /// recovery-required: ordinary CAT must be re-proved (see
    /// [`Radio::restore_cat_after_mode_exit`]) before further use.
    #[must_use]
    pub fn into_radio_recovery_required(self) -> Radio<T> {
        self.cat_restore.rebuild_desynchronized(self.transport)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::MockTransport;
    use crate::types::{
        KissDuplex, KissPersistence, KissSlotTime, KissTxDelay, KissTxTail, PacketDataRate,
        TncDataBand,
    };
    use kiss_tnc::FEND;

    type TestResult = Result<(), Box<dyn std::error::Error>>;
    type BoxErr = Box<dyn std::error::Error>;

    /// Helper: create a Radio with a mock that expects the TN 2,0 command.
    fn mock_radio_for_kiss(data_band: TncDataBand) -> Radio<MockTransport> {
        let tn_cmd = format!("TN 2,{}\r", u8::from(data_band));
        let tn_resp = format!("TN 2,{}\r", u8::from(data_band));
        let mut mock = MockTransport::new();
        mock.expect(tn_cmd.as_bytes(), tn_resp.as_bytes());
        Radio::new(mock)
    }

    /// Helper: create a Radio whose `TN` command receives a chosen response.
    fn mock_radio_for_kiss_response(
        data_band: TncDataBand,
        response: &[u8],
    ) -> Radio<MockTransport> {
        let tn_cmd = format!("TN 2,{}\r", u8::from(data_band));
        let mut mock = MockTransport::new();
        mock.expect(tn_cmd.as_bytes(), response);
        mock.pend_when_empty();
        let mut radio = Radio::new(mock);
        radio.set_timeout(Duration::from_millis(5));
        radio
    }

    #[tokio::test]
    async fn enter_kiss_sends_tn_command() -> TestResult {
        let radio = mock_radio_for_kiss(TncDataBand::A);
        let session = radio.enter_kiss(TncDataBand::A).await.map_err(|(_, e)| e)?;
        // Session created successfully means the TN command was sent and accepted.
        assert!(format!("{session:?}").contains("KissSession"));
        Ok(())
    }

    #[tokio::test]
    async fn enter_kiss_on_band_b() -> TestResult {
        let radio = mock_radio_for_kiss(TncDataBand::B);
        let _session = radio.enter_kiss(TncDataBand::B).await.map_err(|(_, e)| e)?;
        Ok(())
    }

    #[tokio::test]
    async fn aligned_not_available_retains_cat_without_packet_recovery() -> TestResult {
        let mut mock = MockTransport::new();
        mock.expect(b"TN 2,0\r", b"N\r");
        mock.expect(b"ID\r", b"ID TH-D75\r");
        let mut radio = Radio::new(mock);

        let Err(error) = radio.transition_to_kiss(TncDataBand::A).await else {
            return Err("TN=N must not produce an entered-mode proof".into());
        };

        assert!(matches!(
            error,
            Error::NotAvailableInCurrentMode { mnemonic } if mnemonic == "TN"
        ));
        assert_eq!(radio.cat_state, crate::radio::CatState::Ready);
        assert_eq!(
            radio.identify().await?.model,
            crate::types::RadioModel::ThD75
        );
        assert_eq!(
            radio.transport.writes(),
            &[b"TN 2,0\r".to_vec(), b"ID\r".to_vec()],
            "semantic refusal must not inject the packet-mode recovery preamble"
        );
        radio.transport.assert_complete();
        Ok(())
    }

    #[tokio::test]
    async fn mmdvm_proof_cannot_be_consumed_as_kiss() -> TestResult {
        let mut radio = Radio::new(MockTransport::new());
        radio.cat_state =
            crate::radio::CatState::BinaryProven(BinaryProtocolProof::Mmdvm { data_band: None });

        let Err((radio, error)) = radio.into_kiss_session() else {
            return Err("MMDVM proof unexpectedly authorized a KISS session".into());
        };

        assert!(matches!(error, Error::BinaryModeNotProven));
        assert_eq!(
            radio.cat_state,
            crate::radio::CatState::BinaryProven(BinaryProtocolProof::Mmdvm { data_band: None })
        );
        radio.transport.assert_complete();
        Ok(())
    }

    #[tokio::test]
    async fn enter_kiss_rejects_wrong_mode_echo() -> TestResult {
        let radio = mock_radio_for_kiss_response(TncDataBand::A, b"TN 0,0\r");
        let result = radio.enter_kiss(TncDataBand::A).await;
        let Err((mut radio, error)) = result else {
            return Err("wrong TNC mode echo must not create a KISS session".into());
        };
        assert!(matches!(error, Error::Timeout(_)));
        assert_eq!(radio.cat_state, crate::radio::CatState::RecoveryRequired);
        assert!(matches!(
            radio.identify().await,
            Err(Error::CatRecoveryRequired)
        ));
        Ok(())
    }

    #[tokio::test]
    async fn enter_kiss_rejects_wrong_data_band_echo() -> TestResult {
        let radio = mock_radio_for_kiss_response(TncDataBand::A, b"TN 2,1\r");
        let result = radio.enter_kiss(TncDataBand::A).await;
        let Err((mut radio, error)) = result else {
            return Err("wrong TNC data-band echo must not create a KISS session".into());
        };
        assert!(matches!(error, Error::Timeout(_)));
        assert_eq!(radio.cat_state, crate::radio::CatState::RecoveryRequired);
        assert!(matches!(
            radio.identify().await,
            Err(Error::CatRecoveryRequired)
        ));
        Ok(())
    }

    #[tokio::test]
    async fn send_frame_writes_kiss_encoded() -> TestResult {
        let radio = mock_radio_for_kiss(TncDataBand::A);
        let mut session = radio.enter_kiss(TncDataBand::A).await.map_err(|(_, e)| e)?;

        // The mock transport has no more exchanges queued, so sending
        // will fail. We add one to verify encoding.
        session
            .transport
            .expect(&[FEND, 0x00, 0xAA, 0xBB, FEND], &[]);

        let frame = KissFrame::data(vec![0xAA, 0xBB]);
        session.send_frame(&frame).await?;
        Ok(())
    }

    #[tokio::test]
    async fn send_data_wraps_in_kiss() -> TestResult {
        let radio = mock_radio_for_kiss(TncDataBand::A);
        let mut session = radio.enter_kiss(TncDataBand::A).await.map_err(|(_, e)| e)?;

        session
            .transport
            .expect(&[FEND, 0x00, 0x01, 0x02, FEND], &[]);

        session.send_data(&[0x01, 0x02]).await?;
        Ok(())
    }

    #[tokio::test]
    async fn set_tx_delay_sends_correct_frame() -> TestResult {
        let radio = mock_radio_for_kiss(TncDataBand::A);
        let mut session = radio.enter_kiss(TncDataBand::A).await.map_err(|(_, e)| e)?;

        // A 500 ms delay is encoded as 50 ten-millisecond units.
        session.transport.expect(&[FEND, 0x01, 50, FEND], &[]);

        session
            .set_tx_delay(KissTxDelay::from_milliseconds(500)?)
            .await?;
        Ok(())
    }

    #[tokio::test]
    async fn set_persistence_sends_correct_frame() -> TestResult {
        let radio = mock_radio_for_kiss(TncDataBand::A);
        let mut session = radio.enter_kiss(TncDataBand::A).await.map_err(|(_, e)| e)?;

        session.transport.expect(&[FEND, 0x02, 128, FEND], &[]);

        session.set_persistence(KissPersistence::new(128)).await?;
        Ok(())
    }

    #[tokio::test]
    async fn set_slot_time_sends_correct_frame() -> TestResult {
        let radio = mock_radio_for_kiss(TncDataBand::A);
        let mut session = radio.enter_kiss(TncDataBand::A).await.map_err(|(_, e)| e)?;

        session.transport.expect(&[FEND, 0x03, 10, FEND], &[]);

        session
            .set_slot_time(KissSlotTime::from_milliseconds(100)?)
            .await?;
        Ok(())
    }

    #[tokio::test]
    async fn set_tx_tail_sends_correct_frame() -> TestResult {
        let radio = mock_radio_for_kiss(TncDataBand::A);
        let mut session = radio.enter_kiss(TncDataBand::A).await.map_err(|(_, e)| e)?;

        session.transport.expect(&[FEND, 0x04, 3, FEND], &[]);

        session
            .set_tx_tail(KissTxTail::from_milliseconds(30)?)
            .await?;
        Ok(())
    }

    #[tokio::test]
    async fn set_duplex_sends_correct_frame() -> TestResult {
        let radio = mock_radio_for_kiss(TncDataBand::A);
        let mut session = radio.enter_kiss(TncDataBand::A).await.map_err(|(_, e)| e)?;

        session.transport.expect(&[FEND, 0x05, 1, FEND], &[]);

        session.set_duplex(KissDuplex::Full).await?;
        Ok(())
    }

    #[tokio::test]
    async fn set_hardware_data_rate_1200() -> TestResult {
        let radio = mock_radio_for_kiss(TncDataBand::A);
        let mut session = radio.enter_kiss(TncDataBand::A).await.map_err(|(_, e)| e)?;

        session.transport.expect(&[FEND, 0x06, 0x00, FEND], &[]);

        session
            .set_hardware_data_rate(PacketDataRate::Bps1200)
            .await?;
        Ok(())
    }

    #[tokio::test]
    async fn set_hardware_data_rate_9600() -> TestResult {
        let radio = mock_radio_for_kiss(TncDataBand::A);
        let mut session = radio.enter_kiss(TncDataBand::A).await.map_err(|(_, e)| e)?;

        session.transport.expect(&[FEND, 0x06, 0x05, FEND], &[]);

        session
            .set_hardware_data_rate(PacketDataRate::Bps9600)
            .await?;
        Ok(())
    }

    #[tokio::test]
    async fn exit_sends_return_and_restores_radio() -> TestResult {
        let mut radio = mock_radio_for_kiss(TncDataBand::A);
        radio.set_timeout(Duration::from_millis(731));
        radio.firmware_version = Some(crate::types::FirmwareIdentity::new("1.03.AZM")?);
        radio.tuning_mode_a = Some(crate::types::TuningMode::Memory);
        radio.auto_info_enabled = true;
        radio.gps_settings = Some(crate::types::GpsSettings::new(true, true));
        radio.gps_sentences = Some(crate::types::NmeaSentences::all());
        let mut session = radio.enter_kiss(TncDataBand::A).await.map_err(|(_, e)| e)?;

        // CMD_RETURN frame: C0 FF C0
        session.transport.expect(&[FEND, 0xFF, FEND], &[]);

        let radio = session
            .exit()
            .await
            .map_err(|(_, e)| e)?
            .into_radio_unproven();
        assert_eq!(radio.timeout, Duration::from_millis(731));
        assert_eq!(
            radio
                .firmware_version
                .as_ref()
                .map(crate::types::FirmwareIdentity::as_str),
            Some("1.03.AZM")
        );
        assert_eq!(radio.tuning_mode_a, Some(crate::types::TuningMode::Memory));
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
        Ok(())
    }

    #[tokio::test(start_paused = true)]
    async fn exit_then_restore_proves_cat_through_the_wrapper() -> TestResult {
        let radio = mock_radio_for_kiss(TncDataBand::B);
        let mut session = radio.enter_kiss(TncDataBand::B).await.map_err(|(_, e)| e)?;

        // CMD_RETURN, then the read-only CAT identity proof that `restore`
        // must drive. A bare TN query proves that restoration preserved the
        // selected Band B instead of writing the historical TN 0,0 fallback.
        session.transport.expect(&[FEND, 0xFF, FEND], &[]);
        session.transport.expect(b"ID\r", b"ID TH-D75\r");
        session.transport.expect(b"TN\r", b"TN 0,1\r");
        session.transport.pend_when_empty();

        let mut radio = session
            .exit()
            .await
            .map_err(|(_, e)| e)?
            .restore()
            .await
            .map_err(|(_, e)| e)?;
        let tnc = radio.get_tnc_mode().await?;
        assert!(!radio.desynced);
        assert!(!radio.cat_recovery_required());
        assert_eq!(tnc.data_band, TncDataBand::B);
        radio.transport.assert_complete();
        Ok(())
    }

    #[tokio::test]
    async fn try_extract_skips_leading_garbage() -> TestResult {
        // Stray bytes before framing starts (a late AI push, line
        // noise) must be discarded, not block extraction forever.
        let mut buf = b"TN 2,0\r".to_vec();
        buf.extend_from_slice(&[FEND, 0x00, 0xCC, FEND]);
        let frame = KissSession::<MockTransport>::try_extract_frame(&mut buf)
            .map_err(BoxErr::from)?
            .ok_or("frame behind leading garbage must extract")?;
        assert_eq!(frame.data, vec![0xCC]);
        assert!(buf.is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn try_extract_all_garbage_clears_buffer() {
        // No FEND anywhere: nothing can ever frame, and the noise must
        // not accumulate.
        let mut buf = b"pure ascii noise with no fend".to_vec();
        let frame = KissSession::<MockTransport>::try_extract_frame(&mut buf);
        assert!(matches!(frame, Ok(None)));
        assert!(buf.is_empty(), "FEND-free noise must be discarded");
    }

    #[tokio::test]
    async fn try_extract_reports_malformed_frame_and_preserves_following_frame() -> TestResult {
        // A malformed frame (invalid escape) followed by a valid one:
        // report the corruption to the caller and retain the independent
        // valid frame for the caller's next receive attempt.
        let mut buf = vec![FEND, 0x00, kiss_tnc::FESC, 0x00, FEND];
        buf.extend_from_slice(&[FEND, 0x00, 0xDD, FEND]);
        let error = KissSession::<MockTransport>::try_extract_frame(&mut buf);
        assert!(matches!(error, Err(KissError::InvalidEscapeSequence)));

        let frame = KissSession::<MockTransport>::try_extract_frame(&mut buf)?
            .ok_or("valid frame behind a malformed one must extract")?;
        assert_eq!(frame.data, vec![0xDD]);
        Ok(())
    }

    #[tokio::test]
    async fn receive_surfaces_malformed_frame() -> TestResult {
        let radio = mock_radio_for_kiss(TncDataBand::A);
        let mut session = radio.enter_kiss(TncDataBand::A).await.map_err(|(_, e)| e)?;

        let mut wire = vec![FEND, 0x00, kiss_tnc::FESC, 0x00, FEND];
        wire.extend_from_slice(&[FEND, 0x00, 0xEF, FEND]);
        session.transport.queue_read(&wire);

        let error = session.receive_frame().await;
        assert!(matches!(
            error,
            Err(Error::Kiss(KissError::InvalidEscapeSequence))
        ));
        let frame = session.receive_frame().await?;
        assert_eq!(frame.data, vec![0xEF]);
        Ok(())
    }

    #[tokio::test]
    async fn receive_resyncs_past_leading_garbage() -> TestResult {
        let radio = mock_radio_for_kiss(TncDataBand::A);
        let mut session = radio.enter_kiss(TncDataBand::A).await.map_err(|(_, e)| e)?;

        let mut wire = b"stray CAT response\r".to_vec();
        wire.extend_from_slice(&[FEND, 0x00, 0xEE, FEND]);
        session.transport.queue_read(&wire);

        let frame = session.receive_frame().await?;
        assert_eq!(frame.data, vec![0xEE]);
        Ok(())
    }

    #[tokio::test]
    async fn read_buffer_is_capped() -> TestResult {
        let radio = mock_radio_for_kiss(TncDataBand::A);
        let mut session = radio.enter_kiss(TncDataBand::A).await.map_err(|(_, e)| e)?;

        // An opening FEND followed by an endless unterminated payload
        // (a stuck TNC): the buffer must not grow without bound.
        session.transport.queue_read(&[FEND]);
        for _ in 0..80 {
            session.transport.queue_read(&[0x55u8; 1024]);
        }
        let result = session.receive_frame().await;
        assert!(
            matches!(result, Err(Error::Kiss(KissError::FrameTooLong))),
            "oversized unterminated frame must report FrameTooLong"
        );
        assert_eq!(session.read_buf_len(), 0);

        session.transport.queue_read(&[FEND, 0x00, 0xAC, FEND]);
        let frame = session.receive_frame().await?;
        assert_eq!(frame.data, vec![0xAC]);
        Ok(())
    }

    #[test]
    fn try_extract_rejects_complete_overlong_frame_and_clears_buffer() {
        let mut buf = vec![FEND, 0x00];
        buf.resize(KissSession::<MockTransport>::MAX_READ_BUF, 0x55);
        buf.push(FEND);

        let result = KissSession::<MockTransport>::try_extract_frame(&mut buf);
        assert!(matches!(result, Err(KissError::FrameTooLong)));
        assert!(buf.is_empty());
    }

    #[tokio::test]
    async fn exit_failure_returns_session_intact() -> TestResult {
        let radio = mock_radio_for_kiss(TncDataBand::A);
        let session = radio.enter_kiss(TncDataBand::A).await.map_err(|(_, e)| e)?;

        // No expected exchange for CMD_RETURN, so the write fails. The
        // session (and its transport) must come back for a retry
        // instead of being destroyed.
        let result = session.exit().await;
        let Err((session, _err)) = result else {
            return Err("exit with a failing write must return the session".into());
        };
        drop(session);
        Ok(())
    }

    #[tokio::test]
    async fn try_extract_frame_complete() -> TestResult {
        let mut buf = vec![FEND, 0x00, 0xAA, FEND];
        let frame = KissSession::<MockTransport>::try_extract_frame(&mut buf)
            .map_err(BoxErr::from)?
            .ok_or("try_extract_frame returned None for complete frame")?;
        assert_eq!(frame.command, KissCommand::Data);
        assert_eq!(frame.data, vec![0xAA]);
        assert!(buf.is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn try_extract_frame_incomplete() {
        let mut buf = vec![FEND, 0x00, 0xAA];
        let frame = KissSession::<MockTransport>::try_extract_frame(&mut buf);
        assert!(matches!(frame, Ok(None)));
        // Buffer should be unchanged.
        assert_eq!(buf.len(), 3);
    }

    #[tokio::test]
    async fn try_extract_frame_leading_fends() -> TestResult {
        let mut buf = vec![FEND, FEND, FEND, 0x00, 0xBB, FEND];
        let frame = KissSession::<MockTransport>::try_extract_frame(&mut buf)
            .map_err(BoxErr::from)?
            .ok_or("try_extract_frame returned None for frame with leading FENDs")?;
        assert_eq!(frame.command, KissCommand::Data);
        assert_eq!(frame.data, vec![0xBB]);
        Ok(())
    }

    #[tokio::test]
    async fn set_receive_timeout() -> TestResult {
        let radio = mock_radio_for_kiss(TncDataBand::A);
        let mut session = radio.enter_kiss(TncDataBand::A).await.map_err(|(_, e)| e)?;
        session.set_receive_timeout(Duration::from_secs(30));
        assert_eq!(session.receive_timeout, Duration::from_secs(30));
        Ok(())
    }
}
