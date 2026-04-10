//! MMDVM (Multi-Mode Digital Voice Modem) session management for the TH-D75.
//!
//! When the radio enters MMDVM mode (via `TN 3,x`), the serial port switches
//! from ASCII CAT commands to binary MMDVM framing. CAT commands cannot be
//! used until MMDVM mode is exited. The [`MmdvmSession`] type enforces this
//! at the type level: creating one consumes the [`Radio`], and exiting
//! returns it.
//!
//! The MMDVM protocol is used for D-STAR digital voice gateway operation,
//! where the radio acts as an MMDVM-compatible modem. Once in MMDVM mode,
//! the host can send and receive D-STAR headers, voice data, and modem
//! control commands using the binary MMDVM framing protocol.
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
//! let mut mmdvm = radio.enter_mmdvm(TncBaud::Bps1200).await.map_err(|(_, e)| e)?;
//!
//! // Initialize D-STAR modem.
//! let status = mmdvm.init_dstar().await?;
//! println!("Modem status: {:?}", status);
//!
//! // Exit MMDVM mode (returns the Radio).
//! let radio = mmdvm.exit().await?;
//! # Ok(())
//! # }
//! ```

use std::time::Duration;

use crate::error::{Error, ProtocolError, TransportError};
use crate::mmdvm::{
    self, DStarHeader, MmdvmConfig, MmdvmFrame, MmdvmResponse, ModemMode, ModemStatus,
};
use crate::protocol::{Codec, Command, Response};
use crate::transport::Transport;
use crate::types::{TncBaud, TncMode};

use super::Radio;

/// Default timeout for MMDVM receive operations (10 seconds).
const MMDVM_RECEIVE_TIMEOUT: Duration = Duration::from_secs(10);

/// Default TX delay for MMDVM configuration (in 10 ms units).
const DEFAULT_TX_DELAY: u8 = 10;

/// Default RX audio level for MMDVM configuration.
const DEFAULT_RX_LEVEL: u8 = 128;

/// Default TX audio level for MMDVM configuration.
const DEFAULT_TX_LEVEL: u8 = 128;

/// An MMDVM session that owns the radio transport.
///
/// While this session is active, the serial port speaks MMDVM binary framing
/// instead of ASCII CAT commands. The [`Radio`] is consumed on entry and
/// returned on [`exit`](Self::exit).
pub struct MmdvmSession<T: Transport> {
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
    /// Timeout for receive operations.
    receive_timeout: Duration,
    /// Internal buffer for accumulating MMDVM bytes from the transport.
    rx_buffer: Vec<u8>,
}

impl<T: Transport> std::fmt::Debug for MmdvmSession<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MmdvmSession")
            .field("receive_timeout", &self.receive_timeout)
            .field("rx_buffer_len", &self.rx_buffer.len())
            .finish_non_exhaustive()
    }
}

impl<T: Transport> Radio<T> {
    /// Wrap this [`Radio`] as an [`MmdvmSession`] without sending any commands.
    ///
    /// Use this when the radio is already in MMDVM mode (e.g. after
    /// enabling DV Gateway / Reflector Terminal Mode via MCP write to
    /// offset `0x1CA0`). The transport is assumed to already speak
    /// MMDVM binary framing.
    #[must_use]
    pub fn into_mmdvm_session(self) -> MmdvmSession<T> {
        tracing::info!("wrapping transport as MMDVM session (radio already in gateway mode)");
        MmdvmSession {
            transport: self.transport,
            codec: self.codec,
            notifications: self.notifications,
            timeout: self.timeout,
            mode_a: self.mode_a,
            mode_b: self.mode_b,
            mcp_speed: self.mcp_speed,
            receive_timeout: MMDVM_RECEIVE_TIMEOUT,
            rx_buffer: Vec::with_capacity(512),
        }
    }

    /// Enter MMDVM mode, consuming this [`Radio`] and returning an [`MmdvmSession`].
    ///
    /// Sends the `TN 3,x` CAT command to switch the TNC to MMDVM mode at the
    /// specified baud rate. After this call, the serial port speaks MMDVM
    /// binary framing. The radio enters DV Gateway mode and communicates
    /// using MMDVM framing. Use [`MmdvmSession::exit`] to return to CAT mode.
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

        Ok(MmdvmSession {
            transport: self.transport,
            codec: self.codec,
            notifications: self.notifications,
            timeout: self.timeout,
            mode_a: self.mode_a,
            mode_b: self.mode_b,
            mcp_speed: self.mcp_speed,
            receive_timeout: MMDVM_RECEIVE_TIMEOUT,
            rx_buffer: Vec::with_capacity(512),
        })
    }
}

impl<T: Transport> MmdvmSession<T> {
    /// Set the timeout for [`receive_response`](Self::receive_response) operations.
    ///
    /// Defaults to 10 seconds.
    pub const fn set_receive_timeout(&mut self, duration: Duration) {
        self.receive_timeout = duration;
    }

    /// Exit MMDVM mode and return the [`Radio`].
    ///
    /// Sends a `TN 0,0` command to switch back to APRS/normal TNC mode,
    /// then rebuilds the Radio from saved state.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Transport`] if the write fails.
    pub async fn exit(mut self) -> Result<Radio<T>, Error> {
        tracing::info!("exiting MMDVM mode");

        // Send TN 0,0 to return to APRS mode (exits MMDVM framing).
        let tn_cmd = b"TN 0,0\r";
        self.transport
            .write(tn_cmd)
            .await
            .map_err(Error::Transport)?;

        // Small delay to let the TNC switch back to CAT mode.
        tokio::time::sleep(Duration::from_millis(100)).await;

        Ok(Radio {
            transport: self.transport,
            codec: self.codec,
            notifications: self.notifications,
            timeout: self.timeout,
            mode_a: self.mode_a,
            mode_b: self.mode_b,
            mcp_speed: self.mcp_speed,
            last_cmd_time: None,
        })
    }

    /// Send an MMDVM frame to the radio.
    ///
    /// The frame is encoded to MMDVM wire format (`[0xE0, length, command,
    /// payload...]`) before transmission.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Transport`] if the write fails.
    pub async fn send_frame(&mut self, frame: &MmdvmFrame) -> Result<(), Error> {
        let wire = mmdvm::frame::encode_frame(frame);
        tracing::debug!(
            command = frame.command,
            payload_len = frame.payload.len(),
            wire_len = wire.len(),
            "MMDVM TX"
        );
        self.transport.write(&wire).await.map_err(Error::Transport)
    }

    /// Receive an MMDVM response from the radio.
    ///
    /// Blocks until a complete MMDVM frame is received and parsed, or the
    /// receive timeout expires. Accumulates bytes from the transport and
    /// decodes frames using the MMDVM framing protocol.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Timeout`] if no complete frame arrives within the
    /// configured receive timeout.
    /// Returns [`Error::Transport`] if the read fails.
    /// Returns [`Error::Protocol`] if the frame cannot be parsed.
    pub async fn receive_response(&mut self) -> Result<MmdvmResponse, Error> {
        let timeout_dur = self.receive_timeout;
        tokio::time::timeout(timeout_dur, self.receive_response_inner())
            .await
            .map_err(|_| Error::Timeout(timeout_dur))?
    }

    /// Inner receive loop that accumulates bytes and decodes MMDVM frames.
    async fn receive_response_inner(&mut self) -> Result<MmdvmResponse, Error> {
        let mut tmp = [0u8; 1024];
        loop {
            // Try to decode a frame from the buffer first.
            match mmdvm::frame::decode_frame(&self.rx_buffer) {
                Ok(Some((frame, consumed))) => {
                    let _ = self.rx_buffer.drain(..consumed);
                    tracing::debug!(
                        command = frame.command,
                        payload_len = frame.payload.len(),
                        "MMDVM RX"
                    );
                    let response = mmdvm::frame::parse_response(&frame).map_err(|e| {
                        Error::Protocol(ProtocolError::FieldParse {
                            command: "MMDVM".to_owned(),
                            field: "frame".to_owned(),
                            detail: format!("{e}"),
                        })
                    })?;
                    return Ok(response);
                }
                Ok(None) => {
                    // Need more data.
                }
                Err(e) => {
                    tracing::warn!(?e, "discarding invalid MMDVM data, resetting buffer");
                    self.rx_buffer.clear();
                }
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
            self.rx_buffer.extend_from_slice(&tmp[..n]);
        }
    }

    /// Initialize the MMDVM modem for D-STAR operation.
    ///
    /// Performs the following sequence:
    /// 1. Send `GetVersion`, receive version response.
    /// 2. Send `SetConfig` with D-STAR enabled and default TX delay.
    /// 3. Wait for ACK.
    /// 4. Send `SetMode(DStar)`.
    /// 5. Wait for ACK.
    ///
    /// Returns the modem status after initialization.
    ///
    /// # Errors
    ///
    /// Returns an error if any step in the initialization sequence fails
    /// or if the modem responds with a NAK.
    pub async fn init_dstar(&mut self) -> Result<ModemStatus, Error> {
        tracing::info!("initializing MMDVM modem for D-STAR");

        // Step 1: Get version.
        let version_frame = MmdvmFrame {
            command: mmdvm::frame::CMD_GET_VERSION,
            payload: vec![],
        };
        self.send_frame(&version_frame).await?;
        let version_resp = self.receive_response().await?;
        match &version_resp {
            MmdvmResponse::Version {
                protocol,
                description,
            } => {
                tracing::info!(protocol, description, "MMDVM modem version");
            }
            other => {
                return Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                    expected: "Version".into(),
                    actual: format!("{other:?}").into_bytes(),
                }));
            }
        }

        // Step 2: Set config with D-STAR enabled.
        let config = MmdvmConfig {
            invert: 0x00,
            mode_flags: 0x01, // D-STAR only
            tx_delay: DEFAULT_TX_DELAY,
            state: ModemMode::DStar,
            rx_level: DEFAULT_RX_LEVEL,
            tx_level: DEFAULT_TX_LEVEL,
        };
        let config_frame = MmdvmFrame {
            command: mmdvm::frame::CMD_SET_CONFIG,
            payload: vec![
                config.invert,
                config.mode_flags,
                config.tx_delay,
                config.state as u8,
                config.rx_level,
                config.tx_level,
            ],
        };
        self.send_frame(&config_frame).await?;

        // Step 3: Wait for ACK.
        let ack_resp = self.receive_response().await?;
        match &ack_resp {
            MmdvmResponse::Ack { .. } => {
                tracing::debug!("SetConfig acknowledged");
            }
            MmdvmResponse::Nak { reason, .. } => {
                return Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                    expected: "Ack".into(),
                    actual: format!("NAK: {reason:?}").into_bytes(),
                }));
            }
            other => {
                return Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                    expected: "Ack".into(),
                    actual: format!("{other:?}").into_bytes(),
                }));
            }
        }

        // Step 4: Set mode to D-STAR.
        let mode_frame = MmdvmFrame {
            command: mmdvm::frame::CMD_SET_MODE,
            payload: vec![ModemMode::DStar as u8],
        };
        self.send_frame(&mode_frame).await?;

        // Step 5: Wait for ACK.
        let ack_resp = self.receive_response().await?;
        match &ack_resp {
            MmdvmResponse::Ack { .. } => {
                tracing::debug!("SetMode acknowledged");
            }
            MmdvmResponse::Nak { reason, .. } => {
                return Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                    expected: "Ack".into(),
                    actual: format!("NAK: {reason:?}").into_bytes(),
                }));
            }
            other => {
                return Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                    expected: "Ack".into(),
                    actual: format!("{other:?}").into_bytes(),
                }));
            }
        }

        // Get status to return.
        self.get_status().await
    }

    /// Send a D-STAR header to the radio for transmission.
    ///
    /// The header contains routing information (callsigns, repeater paths)
    /// and is sent before voice data frames.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Transport`] if the write fails.
    pub async fn send_dstar_header(&mut self, header: &DStarHeader) -> Result<(), Error> {
        tracing::debug!(
            my_call = %header.my_call.trim(),
            ur_call = %header.ur_call.trim(),
            "sending D-STAR header"
        );
        let encoded = header.encode();
        let frame = MmdvmFrame {
            command: mmdvm::frame::CMD_DSTAR_HEADER,
            payload: encoded.to_vec(),
        };
        self.send_frame(&frame).await
    }

    /// Send D-STAR voice data (12 bytes: 9 AMBE + 3 slow data).
    ///
    /// Each voice frame consists of 9 bytes of AMBE-encoded audio followed
    /// by 3 bytes of slow data (used for text messages, GPS, etc.).
    ///
    /// # Errors
    ///
    /// Returns [`Error::Transport`] if the write fails.
    pub async fn send_dstar_data(&mut self, data: &[u8; 12]) -> Result<(), Error> {
        let frame = MmdvmFrame {
            command: mmdvm::frame::CMD_DSTAR_DATA,
            payload: data.to_vec(),
        };
        self.send_frame(&frame).await
    }

    /// Send end-of-transmission marker.
    ///
    /// Signals the modem that the current D-STAR transmission is complete.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Transport`] if the write fails.
    pub async fn send_dstar_eot(&mut self) -> Result<(), Error> {
        tracing::debug!("sending D-STAR EOT");
        let frame = MmdvmFrame {
            command: mmdvm::frame::CMD_DSTAR_EOT,
            payload: vec![],
        };
        self.send_frame(&frame).await
    }

    /// Poll modem status.
    ///
    /// Sends a `GET_STATUS` command and returns the current modem status
    /// including enabled modes, operating state, TX status, and buffer
    /// availability.
    ///
    /// # Errors
    ///
    /// Returns an error if the status request fails or the response is
    /// not a valid status frame.
    pub async fn get_status(&mut self) -> Result<ModemStatus, Error> {
        let status_frame = MmdvmFrame {
            command: mmdvm::frame::CMD_GET_STATUS,
            payload: vec![],
        };
        self.send_frame(&status_frame).await?;
        let resp = self.receive_response().await?;
        match resp {
            MmdvmResponse::Status(status) => Ok(status),
            other => Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: "Status".into(),
                actual: format!("{other:?}").into_bytes(),
            })),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mmdvm::frame::{CMD_ACK, CMD_GET_STATUS, CMD_GET_VERSION, START_BYTE};
    use crate::transport::MockTransport;
    use crate::types::TncBaud;

    /// Helper: create a Radio with a mock that expects the TN 3,0 command.
    async fn mock_radio_for_mmdvm(baud: TncBaud) -> Radio<MockTransport> {
        let tn_cmd = format!("TN 3,{}\r", u8::from(baud));
        let tn_resp = format!("TN 3,{}\r", u8::from(baud));
        let mut mock = MockTransport::new();
        mock.expect(tn_cmd.as_bytes(), tn_resp.as_bytes());
        Radio::connect(mock).await.unwrap()
    }

    #[tokio::test]
    async fn enter_mmdvm_sends_tn_command() {
        let radio = mock_radio_for_mmdvm(TncBaud::Bps1200).await;
        let session = radio.enter_mmdvm(TncBaud::Bps1200).await.unwrap();
        assert!(format!("{session:?}").contains("MmdvmSession"));
    }

    #[tokio::test]
    async fn enter_mmdvm_9600_baud() {
        let radio = mock_radio_for_mmdvm(TncBaud::Bps9600).await;
        let _session = radio.enter_mmdvm(TncBaud::Bps9600).await.unwrap();
    }

    #[tokio::test]
    async fn send_frame_writes_mmdvm_encoded() {
        let radio = mock_radio_for_mmdvm(TncBaud::Bps1200).await;
        let mut session = radio.enter_mmdvm(TncBaud::Bps1200).await.unwrap();

        // Expect a GET_VERSION frame: [0xE0, 0x03, 0x00]
        session
            .transport
            .expect(&[START_BYTE, 3, CMD_GET_VERSION], &[]);

        let frame = MmdvmFrame {
            command: CMD_GET_VERSION,
            payload: vec![],
        };
        session.send_frame(&frame).await.unwrap();
    }

    #[tokio::test]
    async fn send_dstar_eot_sends_correct_frame() {
        let radio = mock_radio_for_mmdvm(TncBaud::Bps1200).await;
        let mut session = radio.enter_mmdvm(TncBaud::Bps1200).await.unwrap();

        // EOT frame: [0xE0, 0x03, 0x13]
        session
            .transport
            .expect(&[START_BYTE, 3, mmdvm::frame::CMD_DSTAR_EOT], &[]);

        session.send_dstar_eot().await.unwrap();
    }

    #[tokio::test]
    async fn send_dstar_data_sends_correct_frame() {
        let radio = mock_radio_for_mmdvm(TncBaud::Bps1200).await;
        let mut session = radio.enter_mmdvm(TncBaud::Bps1200).await.unwrap();

        let data: [u8; 12] = [0xAA; 12];
        // Data frame: [0xE0, 15, 0x11, 12 bytes of data]
        let mut expected = vec![START_BYTE, 15, mmdvm::frame::CMD_DSTAR_DATA];
        expected.extend_from_slice(&data);
        session.transport.expect(&expected, &[]);

        session.send_dstar_data(&data).await.unwrap();
    }

    #[tokio::test]
    async fn exit_sends_tn_and_restores_radio() {
        let radio = mock_radio_for_mmdvm(TncBaud::Bps1200).await;
        let mut session = radio.enter_mmdvm(TncBaud::Bps1200).await.unwrap();

        // Exit sends TN 0,0 to return to normal mode.
        session.transport.expect(b"TN 0,0\r", &[]);

        let _radio = session.exit().await.unwrap();
    }

    #[tokio::test]
    async fn set_receive_timeout() {
        let radio = mock_radio_for_mmdvm(TncBaud::Bps1200).await;
        let mut session = radio.enter_mmdvm(TncBaud::Bps1200).await.unwrap();
        session.set_receive_timeout(Duration::from_secs(30));
        assert_eq!(session.receive_timeout, Duration::from_secs(30));
    }

    #[tokio::test]
    async fn receive_response_parses_version() {
        let radio = mock_radio_for_mmdvm(TncBaud::Bps1200).await;
        let mut session = radio.enter_mmdvm(TncBaud::Bps1200).await.unwrap();

        // Queue: send GET_VERSION, receive version response.
        let mut resp = vec![START_BYTE, 9, CMD_GET_VERSION, 1];
        resp.extend_from_slice(b"MMDVM");
        session
            .transport
            .expect(&[START_BYTE, 3, CMD_GET_VERSION], &resp);

        // send_frame + receive_response round-trip.
        let frame = MmdvmFrame {
            command: CMD_GET_VERSION,
            payload: vec![],
        };
        session.send_frame(&frame).await.unwrap();
        let response = session.receive_response().await.unwrap();
        match response {
            MmdvmResponse::Version {
                protocol,
                description: desc,
            } => {
                assert_eq!(protocol, 1);
                assert_eq!(desc, "MMDVM");
            }
            other => panic!("expected Version, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn receive_response_parses_ack() {
        let radio = mock_radio_for_mmdvm(TncBaud::Bps1200).await;
        let mut session = radio.enter_mmdvm(TncBaud::Bps1200).await.unwrap();

        // Queue: send SET_CONFIG frame, receive ACK.
        let config_wire = vec![
            START_BYTE,
            9,
            mmdvm::frame::CMD_SET_CONFIG,
            0,
            1,
            10,
            1,
            128,
            128,
        ];
        let resp = vec![START_BYTE, 4, CMD_ACK, mmdvm::frame::CMD_SET_CONFIG];
        session.transport.expect(&config_wire, &resp);

        let frame = MmdvmFrame {
            command: mmdvm::frame::CMD_SET_CONFIG,
            payload: vec![0, 1, 10, 1, 128, 128],
        };
        session.send_frame(&frame).await.unwrap();
        let response = session.receive_response().await.unwrap();
        match response {
            MmdvmResponse::Ack { command } => {
                assert_eq!(command, mmdvm::frame::CMD_SET_CONFIG);
            }
            other => panic!("expected Ack, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn get_status_sends_and_parses() {
        let radio = mock_radio_for_mmdvm(TncBaud::Bps1200).await;
        let mut session = radio.enter_mmdvm(TncBaud::Bps1200).await.unwrap();

        // Expect GET_STATUS write: [0xE0, 0x03, 0x01]
        // Return status response: [0xE0, 0x07, 0x01, modes, state, tx, dstar_buf]
        let status_resp = vec![START_BYTE, 7, CMD_GET_STATUS, 0x01, 0x01, 0x00, 10];
        session
            .transport
            .expect(&[START_BYTE, 3, CMD_GET_STATUS], &status_resp);

        let status = session.get_status().await.unwrap();
        assert_eq!(status.enabled_modes, 0x01);
        assert_eq!(status.dstar_buffer, 10);
        assert!(!status.tx);
    }
}
