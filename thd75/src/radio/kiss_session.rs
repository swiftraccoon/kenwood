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
//! # use kenwood_thd75::types::TncBaud;
//! # async fn example() -> Result<(), kenwood_thd75::error::Error> {
//! let transport = SerialTransport::open("/dev/cu.usbmodem1234", 115_200)?;
//! let radio = Radio::connect(transport).await?;
//!
//! // Enter KISS mode (consumes the Radio).
//! let mut kiss = radio.enter_kiss(TncBaud::Bps1200).await.map_err(|(_, e)| e)?;
//!
//! // Send and receive KISS frames.
//! use kiss_tnc::KissFrame;
//! let frame = KissFrame::data(vec![/* AX.25 */]);
//! kiss.send_frame(&frame).await?;
//!
//! // Exit KISS mode (returns the Radio).
//! let radio = kiss.exit().await.map_err(|(_session, e)| e)?;
//! # Ok(())
//! # }
//! ```

use std::time::Duration;

use kiss_tnc::{FEND, KissCommand, KissFrame, KissPort, decode_kiss_frame, encode_kiss_frame};

use crate::error::{Error, ProtocolError, TransportError};
use crate::protocol::{Codec, Command, Response};
use crate::transport::Transport;
use crate::types::{TncBaud, TncMode};

use super::Radio;

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
/// | TX Delay | `0x01` | 0-120 (10 ms units) | Menu 508 |
/// | Persistence | `0x02` | 0-255 | 128 |
/// | Slot Time | `0x03` | 0-250 (10 ms units) | 10 |
/// | TX Tail | `0x04` | 0-255 | 3 |
/// | Full Duplex | `0x05` | 0=half, nonzero=full | 0 |
/// | Set Hardware | `0x06` | 0/0x23=1200, 0x05/0x26=9600 | Menu 505 |
/// | Return | `0xFF` | n/a | n/a |
pub struct KissSession<T: Transport> {
    /// The underlying transport (serial or Bluetooth).
    pub(crate) transport: T,
    /// Codec retained from the Radio for later restoration.
    codec: Codec,
    /// Broadcast channel retained from the Radio for later restoration.
    notifications: tokio::sync::broadcast::Sender<Response>,
    /// Cached timeout from the Radio.
    timeout: Duration,
    /// Cached `mode_a` from the Radio.
    mode_a: Option<super::RadioMode>,
    /// Cached `mode_b` from the Radio.
    mode_b: Option<super::RadioMode>,
    /// MCP speed from the Radio.
    mcp_speed: super::programming::McpSpeed,
    /// Link-state channel retained from the Radio for later restoration.
    link_state_tx: tokio::sync::watch::Sender<super::LinkState>,
    /// Auto-info flag retained from the Radio for later restoration.
    auto_info_enabled: bool,
    /// GPS config retained from the Radio for later restoration.
    gps_config: Option<(bool, bool)>,
    /// GPS sentence flags retained from the Radio for later restoration.
    gps_sentences: Option<(bool, bool, bool, bool, bool, bool)>,
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
    /// Enter KISS mode, consuming this [`Radio`] and returning a [`KissSession`].
    ///
    /// Sends the `TN 2,x` CAT command to switch the TNC to KISS mode at the
    /// specified baud rate. After this call, the serial port speaks KISS
    /// binary framing. Use [`KissSession::exit`] to return to CAT mode.
    ///
    /// # Errors
    ///
    /// On failure, returns the [`Radio`] alongside the error so the caller
    /// can continue using CAT mode. The radio is NOT consumed on error.
    pub async fn enter_kiss(mut self, baud: TncBaud) -> Result<KissSession<T>, (Self, Error)> {
        tracing::info!(?baud, "entering KISS mode");
        let response = match self
            .execute(Command::SetTncMode {
                mode: TncMode::Kiss,
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

        Ok(KissSession {
            transport: self.transport,
            codec: self.codec,
            notifications: self.notifications,
            timeout: self.timeout,
            mode_a: self.mode_a,
            mode_b: self.mode_b,
            mcp_speed: self.mcp_speed,
            link_state_tx: self.link_state_tx,
            auto_info_enabled: self.auto_info_enabled,
            gps_config: self.gps_config,
            gps_sentences: self.gps_sentences,
            receive_timeout: KISS_RECEIVE_TIMEOUT,
            read_buf: Vec::with_capacity(512),
        })
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
            if let Some(frame) = Self::try_extract_frame(&mut self.read_buf) {
                return Ok(frame);
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
            if let Some(chunk) = tmp.get(..n) {
                self.read_buf.extend_from_slice(chunk);
            }
            // Cap the buffer: an opening FEND followed by an endless
            // unterminated payload (stuck TNC, wedged line) must not
            // grow memory without bound. Keep the newest bytes; the
            // oldest belong to a frame that never completed.
            if self.read_buf.len() > Self::MAX_READ_BUF {
                tracing::warn!(
                    len = self.read_buf.len(),
                    "KISS read buffer exceeded cap; truncating oldest bytes"
                );
                let excess = self.read_buf.len() - Self::MAX_READ_BUF;
                drop(self.read_buf.drain(..excess));
            }
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
    /// fill) are consumed.
    fn try_extract_frame(buf: &mut Vec<u8>) -> Option<KissFrame> {
        loop {
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
                    return None;
                }
            }

            // Skip leading duplicate FENDs (inter-frame fill).
            while matches!(buf.first(), Some(&FEND)) && matches!(buf.get(1), Some(&FEND)) {
                let _removed: u8 = buf.remove(0);
            }

            // Need at least FEND + type + FEND.
            if buf.len() < 3 || buf.first() != Some(&FEND) {
                return None;
            }

            // Find the closing FEND after the opening one.
            let end_pos = buf.get(1..)?.iter().position(|&b| b == FEND)?;
            let frame_end = end_pos + 2; // Include the closing FEND.

            let frame_bytes: Vec<u8> = buf.drain(..frame_end).collect();
            match decode_kiss_frame(&frame_bytes) {
                Ok(frame) => {
                    tracing::debug!(
                        command = ?frame.command,
                        data_len = frame.data.len(),
                        "KISS RX"
                    );
                    return Some(frame);
                }
                Err(e) => {
                    // A malformed frame must not stall a valid frame
                    // already buffered behind it, so keep extracting.
                    tracing::warn!(?e, "discarding malformed KISS frame");
                }
            }
        }
    }

    /// Set the TNC TX delay (KISS command `0x01`).
    ///
    /// The value is in units of 10 ms. The TH-D75 supports 0-120
    /// (0 ms to 1200 ms). The default is configured via Menu No. 508.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Transport`] if the write fails.
    pub async fn set_tx_delay(&mut self, tens_of_ms: u8) -> Result<(), Error> {
        tracing::debug!(tens_of_ms, "setting KISS TX delay");
        self.send_frame(&KissFrame {
            port: KissPort::TH_D75,
            command: KissCommand::TxDelay,
            data: vec![tens_of_ms],
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
    pub async fn set_persistence(&mut self, value: u8) -> Result<(), Error> {
        tracing::debug!(value, "setting KISS persistence");
        self.send_frame(&KissFrame {
            port: KissPort::TH_D75,
            command: KissCommand::Persistence,
            data: vec![value],
        })
        .await
    }

    /// Set the CSMA slot time (KISS command `0x03`).
    ///
    /// The value is in units of 10 ms. Range 0-250. Default: 10 (100 ms).
    ///
    /// # Errors
    ///
    /// Returns [`Error::Transport`] if the write fails.
    pub async fn set_slot_time(&mut self, tens_of_ms: u8) -> Result<(), Error> {
        tracing::debug!(tens_of_ms, "setting KISS slot time");
        self.send_frame(&KissFrame {
            port: KissPort::TH_D75,
            command: KissCommand::SlotTime,
            data: vec![tens_of_ms],
        })
        .await
    }

    /// Set the TX tail time (KISS command `0x04`).
    ///
    /// The value is in units of 10 ms. Range 0-255. Default: 3 (30 ms).
    ///
    /// # Errors
    ///
    /// Returns [`Error::Transport`] if the write fails.
    pub async fn set_tx_tail(&mut self, tens_of_ms: u8) -> Result<(), Error> {
        tracing::debug!(tens_of_ms, "setting KISS TX tail");
        self.send_frame(&KissFrame {
            port: KissPort::TH_D75,
            command: KissCommand::TxTail,
            data: vec![tens_of_ms],
        })
        .await
    }

    /// Set full or half duplex mode (KISS command `0x05`).
    ///
    /// `true` = full duplex, `false` = half duplex (default).
    ///
    /// # Errors
    ///
    /// Returns [`Error::Transport`] if the write fails.
    pub async fn set_full_duplex(&mut self, full_duplex: bool) -> Result<(), Error> {
        tracing::debug!(full_duplex, "setting KISS duplex mode");
        self.send_frame(&KissFrame {
            port: KissPort::TH_D75,
            command: KissCommand::FullDuplex,
            data: vec![u8::from(full_duplex)],
        })
        .await
    }

    /// Switch the TNC data speed via KISS hardware command (`0x06`).
    ///
    /// On the TH-D75, `true` = 1200 bps (AFSK), `false` = 9600 bps (GMSK).
    /// The hardware command values are: 0 or 0x23 for 1200, 0x05 or 0x26
    /// for 9600.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Transport`] if the write fails.
    pub async fn set_hardware_baud(&mut self, baud_1200: bool) -> Result<(), Error> {
        let value = if baud_1200 { 0x00 } else { 0x05 };
        tracing::debug!(baud_1200, value, "setting KISS hardware baud");
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
    /// Returns the [`Radio`] so CAT commands can be used again.
    ///
    /// # Errors
    ///
    /// Returns the session back together with the error if the exit
    /// write fails, so the transport survives for a retry: a full
    /// reopen is expensive (and fragile over Bluetooth RFCOMM).
    pub async fn exit(mut self) -> Result<Radio<T>, (Self, Error)> {
        tracing::info!("exiting KISS mode");
        if let Err(e) = self.send_frame(&KissFrame::return_command()).await {
            return Err((self, e));
        }

        // Small delay to let the TNC switch back to CAT mode.
        tokio::time::sleep(Duration::from_millis(100)).await;

        // Rebuild the Radio from saved state.
        Ok(Radio {
            transport: self.transport,
            codec: self.codec,
            notifications: self.notifications,
            timeout: self.timeout,
            mode_a: self.mode_a,
            mode_b: self.mode_b,
            mcp_speed: self.mcp_speed,
            last_cmd_time: None,
            // Binary TNC traffic may have left residue on the line, so
            // drain before the first CAT command.
            desynced: true,
            gm_poisoned: false,
            mcp_phase: super::McpPhase::Inactive,
            mcp_saved_timeout: None,
            mcp_pending_exit_error: None,
            link_state_tx: self.link_state_tx,
            auto_info_enabled: self.auto_info_enabled,
            gps_config: self.gps_config,
            gps_sentences: self.gps_sentences,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::MockTransport;
    use crate::types::TncBaud;
    use kiss_tnc::FEND;

    type TestResult = Result<(), Box<dyn std::error::Error>>;
    type BoxErr = Box<dyn std::error::Error>;

    /// Helper: create a Radio with a mock that expects the TN 2,0 command.
    async fn mock_radio_for_kiss(baud: TncBaud) -> Result<Radio<MockTransport>, BoxErr> {
        let tn_cmd = format!("TN 2,{}\r", u8::from(baud));
        let tn_resp = format!("TN 2,{}\r", u8::from(baud));
        let mut mock = MockTransport::new();
        mock.expect(tn_cmd.as_bytes(), tn_resp.as_bytes());
        Ok(Radio::connect(mock).await?)
    }

    #[tokio::test]
    async fn enter_kiss_sends_tn_command() -> TestResult {
        let radio = mock_radio_for_kiss(TncBaud::Bps1200).await?;
        let session = radio
            .enter_kiss(TncBaud::Bps1200)
            .await
            .map_err(|(_, e)| e)?;
        // Session created successfully means the TN command was sent and accepted.
        assert!(format!("{session:?}").contains("KissSession"));
        Ok(())
    }

    #[tokio::test]
    async fn enter_kiss_9600_baud() -> TestResult {
        let radio = mock_radio_for_kiss(TncBaud::Bps9600).await?;
        let _session = radio
            .enter_kiss(TncBaud::Bps9600)
            .await
            .map_err(|(_, e)| e)?;
        Ok(())
    }

    #[tokio::test]
    async fn send_frame_writes_kiss_encoded() -> TestResult {
        let radio = mock_radio_for_kiss(TncBaud::Bps1200).await?;
        let mut session = radio
            .enter_kiss(TncBaud::Bps1200)
            .await
            .map_err(|(_, e)| e)?;

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
        let radio = mock_radio_for_kiss(TncBaud::Bps1200).await?;
        let mut session = radio
            .enter_kiss(TncBaud::Bps1200)
            .await
            .map_err(|(_, e)| e)?;

        session
            .transport
            .expect(&[FEND, 0x00, 0x01, 0x02, FEND], &[]);

        session.send_data(&[0x01, 0x02]).await?;
        Ok(())
    }

    #[tokio::test]
    async fn set_tx_delay_sends_correct_frame() -> TestResult {
        let radio = mock_radio_for_kiss(TncBaud::Bps1200).await?;
        let mut session = radio
            .enter_kiss(TncBaud::Bps1200)
            .await
            .map_err(|(_, e)| e)?;

        // TX delay of 50 (500 ms)
        session.transport.expect(&[FEND, 0x01, 50, FEND], &[]);

        session.set_tx_delay(50).await?;
        Ok(())
    }

    #[tokio::test]
    async fn set_persistence_sends_correct_frame() -> TestResult {
        let radio = mock_radio_for_kiss(TncBaud::Bps1200).await?;
        let mut session = radio
            .enter_kiss(TncBaud::Bps1200)
            .await
            .map_err(|(_, e)| e)?;

        session.transport.expect(&[FEND, 0x02, 128, FEND], &[]);

        session.set_persistence(128).await?;
        Ok(())
    }

    #[tokio::test]
    async fn set_slot_time_sends_correct_frame() -> TestResult {
        let radio = mock_radio_for_kiss(TncBaud::Bps1200).await?;
        let mut session = radio
            .enter_kiss(TncBaud::Bps1200)
            .await
            .map_err(|(_, e)| e)?;

        session.transport.expect(&[FEND, 0x03, 10, FEND], &[]);

        session.set_slot_time(10).await?;
        Ok(())
    }

    #[tokio::test]
    async fn set_tx_tail_sends_correct_frame() -> TestResult {
        let radio = mock_radio_for_kiss(TncBaud::Bps1200).await?;
        let mut session = radio
            .enter_kiss(TncBaud::Bps1200)
            .await
            .map_err(|(_, e)| e)?;

        session.transport.expect(&[FEND, 0x04, 3, FEND], &[]);

        session.set_tx_tail(3).await?;
        Ok(())
    }

    #[tokio::test]
    async fn set_full_duplex_sends_correct_frame() -> TestResult {
        let radio = mock_radio_for_kiss(TncBaud::Bps1200).await?;
        let mut session = radio
            .enter_kiss(TncBaud::Bps1200)
            .await
            .map_err(|(_, e)| e)?;

        session.transport.expect(&[FEND, 0x05, 1, FEND], &[]);

        session.set_full_duplex(true).await?;
        Ok(())
    }

    #[tokio::test]
    async fn set_hardware_baud_1200() -> TestResult {
        let radio = mock_radio_for_kiss(TncBaud::Bps1200).await?;
        let mut session = radio
            .enter_kiss(TncBaud::Bps1200)
            .await
            .map_err(|(_, e)| e)?;

        session.transport.expect(&[FEND, 0x06, 0x00, FEND], &[]);

        session.set_hardware_baud(true).await?;
        Ok(())
    }

    #[tokio::test]
    async fn set_hardware_baud_9600() -> TestResult {
        let radio = mock_radio_for_kiss(TncBaud::Bps1200).await?;
        let mut session = radio
            .enter_kiss(TncBaud::Bps1200)
            .await
            .map_err(|(_, e)| e)?;

        session.transport.expect(&[FEND, 0x06, 0x05, FEND], &[]);

        session.set_hardware_baud(false).await?;
        Ok(())
    }

    #[tokio::test]
    async fn exit_sends_return_and_restores_radio() -> TestResult {
        let radio = mock_radio_for_kiss(TncBaud::Bps1200).await?;
        let mut session = radio
            .enter_kiss(TncBaud::Bps1200)
            .await
            .map_err(|(_, e)| e)?;

        // CMD_RETURN frame: C0 FF C0
        session.transport.expect(&[FEND, 0xFF, FEND], &[]);

        let _radio = session.exit().await.map_err(|(_, e)| e)?;
        Ok(())
    }

    #[tokio::test]
    async fn try_extract_skips_leading_garbage() -> TestResult {
        // Stray bytes before framing starts (a late AI push, line
        // noise) must be discarded, not block extraction forever.
        let mut buf = b"TN 2,0\r".to_vec();
        buf.extend_from_slice(&[FEND, 0x00, 0xCC, FEND]);
        let frame = KissSession::<MockTransport>::try_extract_frame(&mut buf)
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
        assert!(frame.is_none());
        assert!(buf.is_empty(), "FEND-free noise must be discarded");
    }

    #[tokio::test]
    async fn try_extract_recovers_past_malformed_frame() -> TestResult {
        // A malformed frame (invalid escape) followed by a valid one:
        // the valid frame must be returned by the SAME call, not
        // stall until more bytes arrive.
        let mut buf = vec![FEND, 0x00, kiss_tnc::FESC, 0x00, FEND];
        buf.extend_from_slice(&[FEND, 0x00, 0xDD, FEND]);
        let frame = KissSession::<MockTransport>::try_extract_frame(&mut buf)
            .ok_or("valid frame behind a malformed one must extract")?;
        assert_eq!(frame.data, vec![0xDD]);
        Ok(())
    }

    #[tokio::test]
    async fn receive_resyncs_past_leading_garbage() -> TestResult {
        let radio = mock_radio_for_kiss(TncBaud::Bps1200).await?;
        let mut session = radio
            .enter_kiss(TncBaud::Bps1200)
            .await
            .map_err(|(_, e)| e)?;

        let mut wire = b"stray CAT response\r".to_vec();
        wire.extend_from_slice(&[FEND, 0x00, 0xEE, FEND]);
        session.transport.queue_read(&wire);

        let frame = session.receive_frame().await?;
        assert_eq!(frame.data, vec![0xEE]);
        Ok(())
    }

    #[tokio::test]
    async fn read_buffer_is_capped() -> TestResult {
        let radio = mock_radio_for_kiss(TncBaud::Bps1200).await?;
        let mut session = radio
            .enter_kiss(TncBaud::Bps1200)
            .await
            .map_err(|(_, e)| e)?;

        // An opening FEND followed by an endless unterminated payload
        // (a stuck TNC): the buffer must not grow without bound.
        session.transport.queue_read(&[FEND]);
        for _ in 0..80 {
            session.transport.queue_read(&[0x55u8; 1024]);
        }
        let result = session.receive_frame().await;
        assert!(result.is_err(), "no complete frame must yield an error");
        assert!(
            session.read_buf_len() <= 64 * 1024,
            "read buffer exceeded cap: {}",
            session.read_buf_len()
        );
        Ok(())
    }

    #[tokio::test]
    async fn exit_failure_returns_session_intact() -> TestResult {
        let radio = mock_radio_for_kiss(TncBaud::Bps1200).await?;
        let session = radio
            .enter_kiss(TncBaud::Bps1200)
            .await
            .map_err(|(_, e)| e)?;

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
        assert!(frame.is_none());
        // Buffer should be unchanged.
        assert_eq!(buf.len(), 3);
    }

    #[tokio::test]
    async fn try_extract_frame_leading_fends() -> TestResult {
        let mut buf = vec![FEND, FEND, FEND, 0x00, 0xBB, FEND];
        let frame = KissSession::<MockTransport>::try_extract_frame(&mut buf)
            .ok_or("try_extract_frame returned None for frame with leading FENDs")?;
        assert_eq!(frame.command, KissCommand::Data);
        assert_eq!(frame.data, vec![0xBB]);
        Ok(())
    }

    #[tokio::test]
    async fn set_receive_timeout() -> TestResult {
        let radio = mock_radio_for_kiss(TncBaud::Bps1200).await?;
        let mut session = radio
            .enter_kiss(TncBaud::Bps1200)
            .await
            .map_err(|(_, e)| e)?;
        session.set_receive_timeout(Duration::from_secs(30));
        assert_eq!(session.receive_timeout, Duration::from_secs(30));
        Ok(())
    }
}
