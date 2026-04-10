//! Integrated D-STAR gateway client for the TH-D75.
//!
//! Manages the MMDVM session, tracks voice transmissions, decodes
//! slow data messages, and maintains a last-heard list. This is the
//! building block for D-STAR reflector clients --- it handles the
//! radio side of the gateway while the user provides the network side.
//!
//! # Architecture
//!
//! The TH-D75 in Reflector Terminal Mode acts as an MMDVM modem.
//! This client manages that modem interface:
//!
//! ```text
//! [Radio] <--MMDVM BT/USB--> [DStarGateway] <--user code--> [Reflector UDP]
//! ```
//!
//! The gateway does NOT implement reflector protocols (DExtra/DCS/DPlus)
//! --- those are separate concerns. This client provides:
//! - Voice frame relay (radio to user, user to radio)
//! - D-STAR header management
//! - Slow data text message decode/encode
//! - Last heard tracking
//! - Connection lifecycle
//!
//! # Design
//!
//! The [`DStarGateway`] owns an [`MmdvmSession`] and therefore the radio
//! transport. Create it with [`DStarGateway::start`], which enters MMDVM
//! mode and initializes D-STAR, and tear it down with
//! [`DStarGateway::stop`], which exits MMDVM mode and returns the
//! [`Radio`] for other use. This is the same ownership pattern used by
//! [`AprsClient`](crate::kiss::aprs_client::AprsClient) and
//! [`MmdvmSession`].
//!
//! The main loop calls [`DStarGateway::next_event`] repeatedly. Each
//! call performs one cycle of I/O: receive a pending MMDVM frame (with a
//! short timeout), parse it, update state, and return a typed
//! [`DStarEvent`].
//!
//! # Example
//!
//! ```no_run
//! use kenwood_thd75::{Radio, DStarGateway, DStarGatewayConfig};
//! use kenwood_thd75::transport::SerialTransport;
//!
//! # async fn example() -> Result<(), Box<dyn std::error::Error>> {
//! let transport = SerialTransport::open("/dev/cu.usbmodem1234", 115_200)?;
//! let radio = Radio::connect(transport).await?;
//!
//! let config = DStarGatewayConfig::new("N0CALL");
//! let mut gw = DStarGateway::start(radio, config).await.map_err(|(_, e)| e)?;
//!
//! while let Some(event) = gw.next_event().await? {
//!     match event {
//!         kenwood_thd75::DStarEvent::VoiceStart(header) => {
//!             println!("TX from {} to {}", header.my_call, header.ur_call);
//!             // Forward header to reflector...
//!         }
//!         kenwood_thd75::DStarEvent::VoiceData(frame) => {
//!             let _ = frame; // Forward AMBE + slow data to reflector...
//!         }
//!         kenwood_thd75::DStarEvent::VoiceEnd => {
//!             // Send EOT to reflector...
//!         }
//!         kenwood_thd75::DStarEvent::TextMessage(text) => {
//!             println!("Slow data message: {text}");
//!         }
//!         kenwood_thd75::DStarEvent::StationHeard(entry) => {
//!             println!("Heard: {}", entry.callsign);
//!         }
//!         _ => {}
//!     }
//! }
//!
//! let _radio = gw.stop().await?;
//! # Ok(())
//! # }
//! ```

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use crate::error::Error;
use crate::mmdvm::ModemStatus;
use crate::mmdvm::dstar::DStarHeader;
use crate::mmdvm::frame::MmdvmResponse;
use crate::mmdvm::slow_data::{SlowDataDecoder, SlowDataEncoder};
use crate::radio::Radio;
use crate::radio::mmdvm_session::MmdvmSession;
use crate::transport::Transport;
use crate::types::TncBaud;
use crate::types::dstar::UrCallAction;

/// Default receive timeout for `next_event` polling (500 ms).
///
/// Short enough to keep the event loop responsive, long enough to
/// avoid busy-spinning on a quiet channel.
const EVENT_POLL_TIMEOUT: Duration = Duration::from_millis(500);

/// Default maximum entries in the last-heard list.
const DEFAULT_MAX_LAST_HEARD: usize = 100;

/// Default initial reconnection delay.
const DEFAULT_RECONNECT_INITIAL: Duration = Duration::from_secs(1);

/// Default maximum reconnection delay.
const DEFAULT_RECONNECT_MAX: Duration = Duration::from_secs(30);

/// D-STAR voice frame interval (20 ms per AMBE frame).
const VOICE_FRAME_INTERVAL: Duration = Duration::from_millis(20);

/// Number of frames to burst-send on initial TX (prebuffer).
///
/// Allows the radio's MMDVM buffer to fill before audio starts playing,
/// preventing initial audio gaps.
const TX_PREBUFFER_FRAMES: usize = 5;

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/// Configuration for a [`DStarGateway`] session.
///
/// Created with [`DStarGatewayConfig::new`] which provides sensible
/// defaults for a D-STAR gateway station. All fields are public for
/// customisation before passing to [`DStarGateway::start`].
#[derive(Debug, Clone)]
pub struct DStarGatewayConfig {
    /// My callsign (up to 8 characters, space-padded internally).
    pub callsign: String,
    /// My suffix (up to 4 characters, space-padded internally).
    /// Default: four spaces.
    pub suffix: String,
    /// TNC baud rate for MMDVM mode. Default: 9600 bps (GMSK, the
    /// standard D-STAR data rate).
    pub baud: TncBaud,
    /// Maximum last-heard entries to keep. Oldest entries are evicted
    /// when this limit is reached. Default: 100.
    pub max_last_heard: usize,
}

impl DStarGatewayConfig {
    /// Create a new configuration with sensible defaults.
    ///
    /// - Suffix: four spaces (no suffix)
    /// - Baud: 9600 bps (GMSK, standard for D-STAR voice)
    /// - Max last-heard: 100 entries
    #[must_use]
    pub fn new(callsign: &str) -> Self {
        Self {
            callsign: callsign.to_owned(),
            suffix: "    ".to_owned(),
            baud: TncBaud::Bps9600,
            max_last_heard: DEFAULT_MAX_LAST_HEARD,
        }
    }
}

// ---------------------------------------------------------------------------
// Reconnection backoff
// ---------------------------------------------------------------------------

/// Exponential backoff policy for reflector reconnection.
///
/// Provides a state machine that tracks reconnection attempts and
/// computes the next delay using exponential backoff with a configurable
/// ceiling.
///
/// # Usage
///
/// ```
/// use kenwood_thd75::mmdvm::ReconnectPolicy;
///
/// let mut policy = ReconnectPolicy::default();
/// // After first failure:
/// let delay = policy.next_delay();
/// // ... wait `delay`, then retry ...
/// // On success:
/// policy.reset();
/// ```
#[derive(Debug, Clone)]
pub struct ReconnectPolicy {
    /// Initial delay before the first retry.
    pub initial_delay: Duration,
    /// Maximum delay between retries.
    pub max_delay: Duration,
    /// Current delay (doubles after each failure).
    current_delay: Duration,
    /// Number of consecutive failures.
    attempts: u32,
}

impl ReconnectPolicy {
    /// Create a new policy with custom initial and max delays.
    #[must_use]
    pub const fn new(initial_delay: Duration, max_delay: Duration) -> Self {
        Self {
            initial_delay,
            max_delay,
            current_delay: initial_delay,
            attempts: 0,
        }
    }

    /// Get the next delay and advance the backoff state.
    ///
    /// The delay doubles with each call, up to `max_delay`.
    #[must_use]
    pub fn next_delay(&mut self) -> Duration {
        let delay = self.current_delay;
        self.attempts = self.attempts.saturating_add(1);
        self.current_delay = (self.current_delay * 2).min(self.max_delay);
        delay
    }

    /// Reset the backoff state after a successful connection.
    pub const fn reset(&mut self) {
        self.current_delay = self.initial_delay;
        self.attempts = 0;
    }

    /// Number of consecutive reconnection attempts.
    #[must_use]
    pub const fn attempts(&self) -> u32 {
        self.attempts
    }
}

impl Default for ReconnectPolicy {
    fn default() -> Self {
        Self::new(DEFAULT_RECONNECT_INITIAL, DEFAULT_RECONNECT_MAX)
    }
}

// ---------------------------------------------------------------------------
// Voice frame
// ---------------------------------------------------------------------------

/// A D-STAR voice data frame (9 bytes AMBE + 3 bytes slow data).
///
/// Each D-STAR voice frame carries 20 ms of AMBE-encoded audio and
/// 3 bytes of slow data used for text messages, GPS, or other auxiliary
/// information.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DStarVoiceFrame {
    /// AMBE codec voice data (9 bytes).
    pub ambe: [u8; 9],
    /// Slow data payload (3 bytes).
    pub slow_data: [u8; 3],
}

// ---------------------------------------------------------------------------
// Last heard
// ---------------------------------------------------------------------------

/// Entry in the last-heard list.
///
/// Tracks the most recent transmission heard from each unique callsign.
/// Updated each time a D-STAR header is received from the radio.
#[derive(Debug, Clone)]
pub struct LastHeardEntry {
    /// Origin callsign (MY field), trimmed of trailing spaces.
    pub callsign: String,
    /// Origin suffix (MY suffix field), trimmed of trailing spaces.
    pub suffix: String,
    /// Destination callsign (UR field), trimmed of trailing spaces.
    pub destination: String,
    /// Repeater 1 callsign, trimmed of trailing spaces.
    pub repeater1: String,
    /// Repeater 2 callsign, trimmed of trailing spaces.
    pub repeater2: String,
    /// When this station was last heard.
    pub timestamp: Instant,
}

// ---------------------------------------------------------------------------
// Event enum
// ---------------------------------------------------------------------------

/// An event produced by [`DStarGateway::next_event`].
///
/// Each variant represents a distinct category of D-STAR gateway
/// activity. The gateway translates raw MMDVM responses into these
/// typed events so callers never need to parse wire data.
#[derive(Debug)]
pub enum DStarEvent {
    /// A voice transmission started (header received from radio).
    VoiceStart(DStarHeader),
    /// A voice data frame received from the radio.
    VoiceData(DStarVoiceFrame),
    /// Voice transmission ended cleanly (EOT received).
    VoiceEnd,
    /// Voice transmission lost (no clean EOT, signal lost).
    VoiceLost,
    /// A slow data text message was decoded from the voice stream.
    TextMessage(String),
    /// GPS/DPRS data was decoded from the voice stream slow data.
    ///
    /// The bytes are the raw DPRS position encoding from type 3 slow
    /// data blocks (not NMEA text).
    GpsData(Vec<u8>),
    /// A station was heard (added or updated in the last-heard list).
    StationHeard(LastHeardEntry),
    /// A URCALL command was detected in the voice header.
    ///
    /// The gateway parsed the UR field and identified a special command
    /// (echo, unlink, info, link). The caller should handle the command
    /// (e.g. connect/disconnect reflector, start echo recording).
    UrCallCommand(UrCallAction),
    /// Modem status update received.
    StatusUpdate(ModemStatus),
}

// ---------------------------------------------------------------------------
// Gateway struct
// ---------------------------------------------------------------------------

/// Complete D-STAR gateway client for the TH-D75.
///
/// Manages the MMDVM session, tracks voice transmissions, decodes
/// slow data messages, and maintains a last-heard list. This is the
/// building block for D-STAR reflector clients --- it handles the
/// radio side of the gateway while the user provides the network side.
///
/// See the [module-level documentation](self) for architecture details
/// and a full usage example.
pub struct DStarGateway<T: Transport> {
    /// The underlying MMDVM session (owns the radio transport).
    session: MmdvmSession<T>,
    /// Gateway configuration.
    config: DStarGatewayConfig,
    /// Slow data decoder for the current RX stream.
    slow_data: SlowDataDecoder,
    /// Frame counter for slow data decoding within a transmission.
    slow_data_frame_index: u8,
    /// Last-heard station list, newest first.
    last_heard: Vec<LastHeardEntry>,
    /// Whether a voice transmission is currently active (RX from radio).
    rx_active: bool,
    /// The D-STAR header for the currently active RX transmission.
    rx_header: Option<DStarHeader>,
    /// Count of TX frames sent in the current outgoing transmission.
    tx_frame_count: usize,
    /// Timestamp of the last TX voice frame sent to the radio.
    last_tx_frame: Option<Instant>,
    /// Buffered events to emit on the next `next_event` call.
    pending_events: VecDeque<DStarEvent>,
    /// Echo recording buffer (header + voice frames).
    echo_header: Option<DStarHeader>,
    /// Echo recorded voice frames.
    echo_frames: Vec<DStarVoiceFrame>,
    /// Whether echo recording is active.
    echo_active: bool,
}

impl<T: Transport> std::fmt::Debug for DStarGateway<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DStarGateway")
            .field("config", &self.config)
            .field("rx_active", &self.rx_active)
            .field("last_heard_count", &self.last_heard.len())
            .finish_non_exhaustive()
    }
}

impl<T: Transport> DStarGateway<T> {
    /// Start the D-STAR gateway.
    ///
    /// Enters MMDVM mode on the radio, initializes the modem for D-STAR
    /// operation, and returns a ready-to-use gateway. Consumes the
    /// [`Radio`] --- call [`stop`](Self::stop) to exit and reclaim it.
    ///
    /// # Errors
    ///
    /// On failure, returns the [`Radio`] alongside the error so the
    /// caller can continue using CAT mode.
    ///
    /// # Panics
    ///
    /// Panics if MMDVM mode was entered but D-STAR init failed AND
    /// the subsequent MMDVM exit also failed (unrecoverable state).
    pub async fn start(
        radio: Radio<T>,
        config: DStarGatewayConfig,
    ) -> Result<Self, (Radio<T>, Error)> {
        let mut session = match radio.enter_mmdvm(config.baud).await {
            Ok(s) => s,
            Err((radio, e)) => return Err((radio, e)),
        };
        session.set_receive_timeout(EVENT_POLL_TIMEOUT);

        // Initialize the modem for D-STAR. This sends GetVersion,
        // SetConfig, and SetMode commands. If init fails, exit MMDVM
        // mode and return the radio so the caller can continue.
        let _status = match session.init_dstar().await {
            Ok(s) => s,
            Err(e) => {
                // Init failed — exit MMDVM mode to recover the radio.
                let radio = session
                    .exit()
                    .await
                    .map_err(|exit_err| {
                        tracing::error!("failed to exit MMDVM after init failure: {exit_err}");
                    })
                    .ok();
                // If we recovered the radio, return it with the error.
                // If not, we have no radio to return — this is unrecoverable.
                return Err((
                    radio.expect(
                        "MMDVM exit failed after init failure; \
                         transport is in an unrecoverable state",
                    ),
                    e,
                ));
            }
        };

        Ok(Self {
            session,
            config,
            slow_data: SlowDataDecoder::new(),
            slow_data_frame_index: 0,
            last_heard: Vec::new(),
            rx_active: false,
            rx_header: None,
            tx_frame_count: 0,
            last_tx_frame: None,
            pending_events: VecDeque::new(),
            echo_header: None,
            echo_frames: Vec::new(),
            echo_active: false,
        })
    }

    /// Start the D-STAR gateway on a radio already in MMDVM mode.
    ///
    /// Use this when the radio was put into DV Gateway / Reflector
    /// Terminal Mode via MCP write (offset `0x1CA0 = 1`). The transport
    /// already speaks MMDVM binary — no `TN` command is sent.
    ///
    /// # Errors
    ///
    /// Returns an error if the D-STAR initialization sequence fails.
    pub async fn start_gateway_mode(
        radio: Radio<T>,
        config: DStarGatewayConfig,
    ) -> Result<Self, Error> {
        let mut session = radio.into_mmdvm_session();
        session.set_receive_timeout(EVENT_POLL_TIMEOUT);

        let _status = session.init_dstar().await?;

        Ok(Self {
            session,
            config,
            slow_data: SlowDataDecoder::new(),
            slow_data_frame_index: 0,
            last_heard: Vec::new(),
            rx_active: false,
            rx_header: None,
            tx_frame_count: 0,
            last_tx_frame: None,
            pending_events: VecDeque::new(),
            echo_header: None,
            echo_frames: Vec::new(),
            echo_active: false,
        })
    }

    /// Stop the gateway, exiting MMDVM mode and returning the [`Radio`].
    ///
    /// # Errors
    ///
    /// Returns an error if the MMDVM exit command fails.
    pub async fn stop(self) -> Result<Radio<T>, Error> {
        self.session.exit().await
    }

    /// Process pending I/O and return the next event.
    ///
    /// Each call performs one cycle:
    /// 1. Try to receive an MMDVM frame from the radio.
    /// 2. Parse the response into a typed event.
    /// 3. On `DStarHeader`: store header, set `rx_active`, add to
    ///    last-heard, reset slow data decoder, emit `VoiceStart` +
    ///    `StationHeard`.
    /// 4. On `DStarData`: feed slow data decoder, emit `VoiceData`.
    ///    If slow data has a complete message, return `TextMessage`
    ///    on the next call.
    /// 5. On `DStarEOT`: clear `rx_active`, emit `VoiceEnd`.
    /// 6. On `DStarLost`: clear `rx_active`, emit `VoiceLost`.
    /// 7. On `Status`: emit `StatusUpdate`.
    ///
    /// Returns `Ok(None)` when no activity occurs within the poll
    /// timeout. Callers should loop on this method.
    ///
    /// # Errors
    ///
    /// Returns an error on transport failures.
    pub async fn next_event(&mut self) -> Result<Option<DStarEvent>, Error> {
        // Drain buffered events first (e.g. UrCallCommand after VoiceStart).
        if let Some(evt) = self.pending_events.pop_front() {
            return Ok(Some(evt));
        }

        let response = match self.session.receive_response().await {
            Ok(r) => r,
            Err(Error::Timeout(_)) => return Ok(None),
            Err(e) => return Err(e),
        };

        match response {
            MmdvmResponse::DStarHeader(header) => {
                // Start of a new voice transmission.
                self.rx_active = true;
                self.slow_data.reset();
                self.slow_data_frame_index = 0;
                self.rx_header = Some(header.clone());

                // Parse URCALL for special commands.
                let action = UrCallAction::parse(&header.ur_call);
                match &action {
                    UrCallAction::Cq | UrCallAction::Callsign(_) => {}
                    UrCallAction::Echo => {
                        // Start echo recording.
                        self.echo_active = true;
                        self.echo_header = Some(header.clone());
                        self.echo_frames.clear();
                    }
                    _ => {
                        // Queue the command event after VoiceStart.
                        self.pending_events
                            .push_back(DStarEvent::UrCallCommand(action));
                    }
                }

                // Update last-heard list.
                let entry = LastHeardEntry {
                    callsign: header.my_call.trim().to_owned(),
                    suffix: header.my_suffix.trim().to_owned(),
                    destination: header.ur_call.trim().to_owned(),
                    repeater1: header.rpt1.trim().to_owned(),
                    repeater2: header.rpt2.trim().to_owned(),
                    timestamp: Instant::now(),
                };
                self.update_last_heard(entry);

                Ok(Some(DStarEvent::VoiceStart(header)))
            }

            MmdvmResponse::DStarData(data) => {
                // Split into AMBE (9 bytes) and slow data (3 bytes).
                let mut ambe = [0u8; 9];
                ambe.copy_from_slice(&data[..9]);
                let mut slow = [0u8; 3];
                slow.copy_from_slice(&data[9..12]);

                // Feed the slow data decoder.
                self.slow_data.add_frame(&slow, self.slow_data_frame_index);
                self.slow_data_frame_index = self.slow_data_frame_index.wrapping_add(1);

                let frame = DStarVoiceFrame {
                    ambe,
                    slow_data: slow,
                };

                // Record frames for echo playback.
                if self.echo_active {
                    self.echo_frames.push(frame.clone());
                }

                Ok(Some(DStarEvent::VoiceData(frame)))
            }

            MmdvmResponse::DStarEot => {
                // Check for decoded slow data before clearing state.
                let text_event = self.take_text_message();
                let gps_event = self.take_gps_data();

                // If echo mode was active, play back the recorded frames.
                let was_echo = self.echo_active;
                if was_echo {
                    self.echo_active = false;
                    self.play_echo().await?;
                }

                self.rx_active = false;
                self.rx_header = None;

                // Prioritize text message, then GPS data, then VoiceEnd.
                if let Some(text) = text_event {
                    return Ok(Some(DStarEvent::TextMessage(text)));
                }
                if let Some(gps) = gps_event {
                    return Ok(Some(DStarEvent::GpsData(gps)));
                }

                Ok(Some(DStarEvent::VoiceEnd))
            }

            MmdvmResponse::DStarLost => {
                self.rx_active = false;
                self.rx_header = None;
                Ok(Some(DStarEvent::VoiceLost))
            }

            MmdvmResponse::Status(status) => Ok(Some(DStarEvent::StatusUpdate(status))),

            // ACK/NAK/Version are not expected during normal operation;
            // ignore them gracefully.
            MmdvmResponse::Ack { .. }
            | MmdvmResponse::Nak { .. }
            | MmdvmResponse::Version { .. } => Ok(None),
        }
    }

    /// Send a D-STAR voice header to the radio for transmission.
    ///
    /// Use this to relay incoming reflector headers to the radio so they
    /// are transmitted over the air. Resets the TX pacing state for the
    /// new transmission.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Transport`] if the write fails.
    pub async fn send_header(&mut self, header: &DStarHeader) -> Result<(), Error> {
        self.tx_frame_count = 0;
        self.last_tx_frame = None;
        self.session.send_dstar_header(header).await
    }

    /// Send a D-STAR voice data frame to the radio for transmission.
    ///
    /// Use this to relay incoming reflector voice frames to the radio.
    /// The AMBE and slow data bytes are combined into the 12-byte format
    /// expected by the modem.
    ///
    /// Enforces 20 ms inter-frame pacing to match the D-STAR AMBE frame
    /// rate. The first 5 frames are sent immediately (burst) to fill the
    /// radio's MMDVM buffer; subsequent frames are paced at 20 ms
    /// intervals.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Transport`] if the write fails.
    pub async fn send_voice(&mut self, frame: &DStarVoiceFrame) -> Result<(), Error> {
        // Pace frames after the initial prebuffer burst.
        if self.tx_frame_count >= TX_PREBUFFER_FRAMES
            && let Some(last) = self.last_tx_frame
        {
            let elapsed = last.elapsed();
            if elapsed < VOICE_FRAME_INTERVAL {
                tokio::time::sleep(VOICE_FRAME_INTERVAL - elapsed).await;
            }
        }

        let mut data = [0u8; 12];
        data[..9].copy_from_slice(&frame.ambe);
        data[9..12].copy_from_slice(&frame.slow_data);
        self.session.send_dstar_data(&data).await?;

        self.tx_frame_count += 1;
        self.last_tx_frame = Some(Instant::now());
        Ok(())
    }

    /// Send a voice frame to the radio without TX pacing.
    ///
    /// Use this for reflector→radio relay where the radio's MMDVM
    /// buffer handles pacing internally. The prebuffer burst and
    /// 20 ms inter-frame timing are skipped.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Transport`] if the write fails.
    pub async fn send_voice_unpaced(&mut self, frame: &DStarVoiceFrame) -> Result<(), Error> {
        let mut data = [0u8; 12];
        data[..9].copy_from_slice(&frame.ambe);
        data[9..12].copy_from_slice(&frame.slow_data);
        self.session.send_dstar_data(&data).await
    }

    /// Send end-of-transmission to the radio.
    ///
    /// Signals the modem that the current D-STAR transmission from the
    /// reflector side is complete. Resets the TX pacing state.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Transport`] if the write fails.
    pub async fn send_eot(&mut self) -> Result<(), Error> {
        self.tx_frame_count = 0;
        self.last_tx_frame = None;
        self.session.send_dstar_eot().await
    }

    /// Send a status header to the radio indicating connection state.
    ///
    /// When connected to a reflector, sets RPT1/RPT2 to the reflector
    /// name + module and UR to CQCQCQ. When disconnected, sets
    /// RPT1/RPT2 to "DIRECT".
    ///
    /// This updates the radio's display to show the current gateway
    /// state, matching the behavior of `d75link` and `BlueDV`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Transport`] if the write fails.
    pub async fn send_status_header(
        &mut self,
        reflector: Option<(&str, char)>,
    ) -> Result<(), Error> {
        let (rpt1, rpt2) = if let Some((name, module)) = reflector {
            let rpt = format!("{name:<7}{module}");
            (rpt.clone(), rpt)
        } else {
            ("DIRECT  ".to_owned(), "DIRECT  ".to_owned())
        };

        let header = DStarHeader {
            flag1: 0x00,
            flag2: 0x00,
            flag3: 0x00,
            rpt2,
            rpt1,
            ur_call: "CQCQCQ  ".to_owned(),
            my_call: format!("{:<8}", self.config.callsign),
            my_suffix: format!("{:<4}", self.config.suffix),
        };

        self.session.send_dstar_header(&header).await
    }

    /// Set the receive timeout for `next_event` polling.
    ///
    /// Lower values make the event loop more responsive but increase
    /// CPU usage. Use short timeouts (10-50ms) when actively relaying
    /// voice from a reflector.
    pub const fn set_event_timeout(&mut self, timeout: Duration) {
        self.session.set_receive_timeout(timeout);
    }

    /// Encode a text message into slow data blocks.
    ///
    /// Returns the encoded 3-byte slow data payloads to interleave with
    /// AMBE silence frames when transmitting a text message. Each pair of
    /// returned arrays forms one 6-byte slow data block.
    #[must_use]
    pub fn encode_text_message(text: &str) -> Vec<[u8; 3]> {
        let encoder = SlowDataEncoder::new();
        encoder.encode_message(text)
    }

    /// Get the last-heard list (newest first).
    #[must_use]
    pub fn last_heard(&self) -> &[LastHeardEntry] {
        &self.last_heard
    }

    /// Poll the modem status.
    ///
    /// Sends a `GET_STATUS` command and returns the current modem status.
    ///
    /// # Errors
    ///
    /// Returns an error if the status request fails.
    pub async fn poll_status(&mut self) -> Result<ModemStatus, Error> {
        self.session.get_status().await
    }

    /// Check if a voice transmission is currently active (RX from radio).
    #[must_use]
    pub const fn is_receiving(&self) -> bool {
        self.rx_active
    }

    /// Get the current RX header, if a voice transmission is active.
    #[must_use]
    pub const fn current_header(&self) -> Option<&DStarHeader> {
        self.rx_header.as_ref()
    }

    /// Get the current configuration.
    #[must_use]
    pub const fn config(&self) -> &DStarGatewayConfig {
        &self.config
    }

    // -----------------------------------------------------------------------
    // Internal helpers
    // -----------------------------------------------------------------------

    /// Update the last-heard list with a new entry.
    ///
    /// If the callsign already exists, the existing entry is replaced.
    /// If the list exceeds the configured maximum, the oldest entry is
    /// removed.
    fn update_last_heard(&mut self, entry: LastHeardEntry) {
        // Remove existing entry for this callsign, if present.
        self.last_heard.retain(|e| e.callsign != entry.callsign);

        // Insert at the front (newest first).
        self.last_heard.insert(0, entry);

        // Enforce the maximum size.
        if self.last_heard.len() > self.config.max_last_heard {
            self.last_heard.truncate(self.config.max_last_heard);
        }
    }

    /// Play back recorded echo frames to the radio.
    ///
    /// Builds a modified header (`RPT2` = callsign + G, `RPT1` = callsign
    /// + reflector module) and transmits all recorded frames with 20 ms pacing.
    async fn play_echo(&mut self) -> Result<(), Error> {
        let Some(orig_header) = self.echo_header.take() else {
            return Ok(());
        };
        let frames = std::mem::take(&mut self.echo_frames);
        if frames.is_empty() {
            return Ok(());
        }

        // Build echo playback header: swap RPT fields to indicate
        // the echo came from the gateway.
        let echo_header = DStarHeader {
            flag1: orig_header.flag1,
            flag2: orig_header.flag2,
            flag3: orig_header.flag3,
            rpt2: format!("{:<7}G", self.config.callsign),
            rpt1: orig_header.rpt1.clone(),
            ur_call: orig_header.my_call.clone(),
            my_call: format!("{:<8}", self.config.callsign),
            my_suffix: format!("{:<4}", self.config.suffix),
        };

        self.send_header(&echo_header).await?;
        for frame in &frames {
            self.send_voice(frame).await?;
        }
        self.send_eot().await?;

        Ok(())
    }

    /// Take the decoded text message from the slow data decoder, if
    /// complete.
    fn take_text_message(&self) -> Option<String> {
        if self.slow_data.has_message() {
            self.slow_data.message().map(str::to_owned)
        } else {
            None
        }
    }

    /// Take the decoded GPS data from the slow data decoder, if present.
    fn take_gps_data(&self) -> Option<Vec<u8>> {
        if self.slow_data.has_gps_data() {
            self.slow_data.gps_data().map(<[u8]>::to_vec)
        } else {
            None
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mmdvm::frame::{
        CMD_ACK, CMD_DSTAR_DATA, CMD_DSTAR_EOT, CMD_DSTAR_HEADER, CMD_DSTAR_LOST, CMD_GET_STATUS,
        CMD_GET_VERSION, CMD_SET_CONFIG, CMD_SET_MODE, START_BYTE,
    };
    use crate::mmdvm::slow_data::SlowDataEncoder;
    use crate::transport::MockTransport;
    use crate::types::TncBaud;

    /// Build a mock Radio that expects the TN 3,x command for MMDVM entry.
    async fn mock_radio(baud: TncBaud) -> Radio<MockTransport> {
        let tn_cmd = format!("TN 3,{}\r", u8::from(baud));
        let tn_resp = format!("TN 3,{}\r", u8::from(baud));
        let mut mock = MockTransport::new();
        mock.expect(tn_cmd.as_bytes(), tn_resp.as_bytes());
        Radio::connect(mock).await.unwrap()
    }

    /// Queue the full `init_dstar` handshake responses on the mock session.
    ///
    /// The init sequence is: `GetVersion` -> Version, `SetConfig` -> ACK,
    /// `SetMode` -> ACK, `GetStatus` -> Status.
    fn queue_init_responses(session: &mut MmdvmSession<MockTransport>) {
        // Step 1: GetVersion request -> Version response
        let mut version_resp = vec![START_BYTE, 9, CMD_GET_VERSION, 1];
        version_resp.extend_from_slice(b"MMDVM");
        session
            .transport
            .expect(&[START_BYTE, 3, CMD_GET_VERSION], &version_resp);

        // Step 2: SetConfig request -> ACK response
        let config_wire = vec![START_BYTE, 9, CMD_SET_CONFIG, 0, 1, 10, 1, 128, 128];
        let ack_config = vec![START_BYTE, 4, CMD_ACK, CMD_SET_CONFIG];
        session.transport.expect(&config_wire, &ack_config);

        // Step 3: SetMode request -> ACK response
        let mode_wire = vec![START_BYTE, 4, CMD_SET_MODE, 1];
        let ack_mode = vec![START_BYTE, 4, CMD_ACK, CMD_SET_MODE];
        session.transport.expect(&mode_wire, &ack_mode);

        // Step 4: GetStatus request -> Status response
        let status_resp = vec![START_BYTE, 7, CMD_GET_STATUS, 0x01, 0x01, 0x00, 10];
        session
            .transport
            .expect(&[START_BYTE, 3, CMD_GET_STATUS], &status_resp);
    }

    /// Create a gateway with all init handshake mocked.
    async fn mock_gateway() -> DStarGateway<MockTransport> {
        let radio = mock_radio(TncBaud::Bps9600).await;

        // We need to manually enter MMDVM and queue init, since start()
        // does both. Build it by entering MMDVM first, then queuing
        // init responses, then calling init_dstar.
        let mut session = radio.enter_mmdvm(TncBaud::Bps9600).await.unwrap();
        session.set_receive_timeout(EVENT_POLL_TIMEOUT);
        queue_init_responses(&mut session);
        let _status = session.init_dstar().await.unwrap();

        let config = DStarGatewayConfig::new("N0CALL");
        DStarGateway {
            session,
            config,
            slow_data: SlowDataDecoder::new(),
            slow_data_frame_index: 0,
            last_heard: Vec::new(),
            rx_active: false,
            rx_header: None,
            tx_frame_count: 0,
            last_tx_frame: None,
            pending_events: VecDeque::new(),
            echo_header: None,
            echo_frames: Vec::new(),
            echo_active: false,
        }
    }

    /// Build a sample D-STAR header and return (header, encoded 41 bytes).
    fn sample_header() -> (DStarHeader, [u8; 41]) {
        let header = DStarHeader {
            flag1: 0x00,
            flag2: 0x00,
            flag3: 0x00,
            rpt2: "DIRECT  ".to_owned(),
            rpt1: "DIRECT  ".to_owned(),
            ur_call: "CQCQCQ  ".to_owned(),
            my_call: "W1AW    ".to_owned(),
            my_suffix: "    ".to_owned(),
        };
        let encoded = header.encode();
        (header, encoded)
    }

    fn test_config() -> DStarGatewayConfig {
        DStarGatewayConfig::new("N0CALL")
    }

    // -------------------------------------------------------------------
    // Configuration tests
    // -------------------------------------------------------------------

    #[test]
    fn config_defaults() {
        let config = DStarGatewayConfig::new("W1AW");
        assert_eq!(config.callsign, "W1AW");
        assert_eq!(config.suffix, "    ");
        assert_eq!(config.baud, TncBaud::Bps9600);
        assert_eq!(config.max_last_heard, 100);
    }

    #[test]
    fn config_debug_formatting() {
        let config = test_config();
        let debug = format!("{config:?}");
        assert!(debug.contains("N0CALL"));
    }

    // -------------------------------------------------------------------
    // Voice frame tests
    // -------------------------------------------------------------------

    #[test]
    fn voice_frame_construction() {
        let frame = DStarVoiceFrame {
            ambe: [1, 2, 3, 4, 5, 6, 7, 8, 9],
            slow_data: [0xA, 0xB, 0xC],
        };
        assert_eq!(frame.ambe[0], 1);
        assert_eq!(frame.slow_data[2], 0xC);
    }

    #[test]
    fn voice_frame_equality() {
        let a = DStarVoiceFrame {
            ambe: [0; 9],
            slow_data: [0; 3],
        };
        let b = a.clone();
        assert_eq!(a, b);
    }

    // -------------------------------------------------------------------
    // Last heard tests
    // -------------------------------------------------------------------

    #[test]
    fn last_heard_entry_debug() {
        let entry = LastHeardEntry {
            callsign: "W1AW".to_owned(),
            suffix: String::new(),
            destination: "CQCQCQ".to_owned(),
            repeater1: "DIRECT".to_owned(),
            repeater2: "DIRECT".to_owned(),
            timestamp: Instant::now(),
        };
        let debug = format!("{entry:?}");
        assert!(debug.contains("W1AW"));
    }

    // -------------------------------------------------------------------
    // Event enum tests
    // -------------------------------------------------------------------

    #[test]
    fn event_debug_formatting() {
        let event = DStarEvent::VoiceEnd;
        let debug = format!("{event:?}");
        assert!(debug.contains("VoiceEnd"));
    }

    #[test]
    fn event_text_message_debug() {
        let event = DStarEvent::TextMessage("Hello D-STAR".to_owned());
        let debug = format!("{event:?}");
        assert!(debug.contains("Hello D-STAR"));
    }

    // -------------------------------------------------------------------
    // Gateway tests
    // -------------------------------------------------------------------

    #[tokio::test]
    async fn gateway_debug_formatting() {
        let gw = mock_gateway().await;
        let debug = format!("{gw:?}");
        assert!(debug.contains("DStarGateway"));
        assert!(debug.contains("N0CALL"));
    }

    #[tokio::test]
    async fn gateway_initial_state() {
        let gw = mock_gateway().await;
        assert!(!gw.is_receiving());
        assert!(gw.current_header().is_none());
        assert!(gw.last_heard().is_empty());
        assert_eq!(gw.config().callsign, "N0CALL");
    }

    #[tokio::test]
    async fn gateway_receives_voice_start() {
        let mut gw = mock_gateway().await;

        // Queue a D-STAR header response.
        let (_header, encoded) = sample_header();
        let mut header_frame = vec![START_BYTE, 44, CMD_DSTAR_HEADER];
        header_frame.extend_from_slice(&encoded);
        // The receive call reads from the mock --- queue data without
        // a matching write (just provide response data).
        gw.session.transport.queue_read(&header_frame);

        let event = gw.next_event().await.unwrap().unwrap();
        match event {
            DStarEvent::VoiceStart(h) => {
                assert_eq!(h.my_call.trim(), "W1AW");
                assert_eq!(h.ur_call.trim(), "CQCQCQ");
            }
            other => panic!("expected VoiceStart, got {other:?}"),
        }

        assert!(gw.is_receiving());
        assert!(gw.current_header().is_some());
        assert_eq!(gw.last_heard().len(), 1);
        assert_eq!(gw.last_heard()[0].callsign, "W1AW");
    }

    #[tokio::test]
    async fn gateway_receives_voice_data() {
        let mut gw = mock_gateway().await;

        // Queue a D-STAR data response (12 bytes: 9 AMBE + 3 slow data).
        let mut data = [0u8; 12];
        data[..9].fill(0xAA); // AMBE
        data[9..12].fill(0xBB); // slow data
        let mut data_frame = vec![START_BYTE, 15, CMD_DSTAR_DATA];
        data_frame.extend_from_slice(&data);
        gw.session.transport.queue_read(&data_frame);

        let event = gw.next_event().await.unwrap().unwrap();
        match event {
            DStarEvent::VoiceData(frame) => {
                assert_eq!(frame.ambe, [0xAA; 9]);
                assert_eq!(frame.slow_data, [0xBB; 3]);
            }
            other => panic!("expected VoiceData, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn gateway_receives_voice_end() {
        let mut gw = mock_gateway().await;

        // Queue EOT.
        let eot_frame = vec![START_BYTE, 3, CMD_DSTAR_EOT];
        gw.session.transport.queue_read(&eot_frame);

        let event = gw.next_event().await.unwrap().unwrap();
        assert!(matches!(event, DStarEvent::VoiceEnd));
        assert!(!gw.is_receiving());
    }

    #[tokio::test]
    async fn gateway_receives_voice_lost() {
        let mut gw = mock_gateway().await;

        // Queue DStarLost.
        let lost_frame = vec![START_BYTE, 3, CMD_DSTAR_LOST];
        gw.session.transport.queue_read(&lost_frame);

        let event = gw.next_event().await.unwrap().unwrap();
        assert!(matches!(event, DStarEvent::VoiceLost));
        assert!(!gw.is_receiving());
    }

    #[tokio::test]
    async fn gateway_receives_status_update() {
        let mut gw = mock_gateway().await;

        // Queue a status response.
        let status_frame = vec![START_BYTE, 7, CMD_GET_STATUS, 0x01, 0x01, 0x00, 10];
        gw.session.transport.queue_read(&status_frame);

        let event = gw.next_event().await.unwrap().unwrap();
        match event {
            DStarEvent::StatusUpdate(status) => {
                assert_eq!(status.enabled_modes, 0x01);
                assert_eq!(status.dstar_buffer, 10);
            }
            other => panic!("expected StatusUpdate, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn gateway_send_header() {
        let mut gw = mock_gateway().await;

        let (header, encoded) = sample_header();
        // Expect the header write.
        let mut expected = vec![START_BYTE, 44, CMD_DSTAR_HEADER];
        expected.extend_from_slice(&encoded);
        gw.session.transport.expect(&expected, &[]);

        gw.send_header(&header).await.unwrap();
    }

    #[tokio::test]
    async fn gateway_send_voice() {
        let mut gw = mock_gateway().await;

        let frame = DStarVoiceFrame {
            ambe: [1, 2, 3, 4, 5, 6, 7, 8, 9],
            slow_data: [10, 11, 12],
        };

        // Expect the 12-byte data write.
        let mut expected = vec![START_BYTE, 15, CMD_DSTAR_DATA];
        expected.extend_from_slice(&[1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12]);
        gw.session.transport.expect(&expected, &[]);

        gw.send_voice(&frame).await.unwrap();
    }

    #[tokio::test]
    async fn gateway_send_eot() {
        let mut gw = mock_gateway().await;

        let expected = vec![START_BYTE, 3, CMD_DSTAR_EOT];
        gw.session.transport.expect(&expected, &[]);

        gw.send_eot().await.unwrap();
    }

    #[tokio::test]
    async fn gateway_stop_exits_mmdvm() {
        let mut gw = mock_gateway().await;

        // Exit sends TN 0,0 to return to normal mode.
        gw.session.transport.expect(b"TN 0,0\r", &[]);

        let _radio = gw.stop().await.unwrap();
    }

    // -------------------------------------------------------------------
    // Last heard management tests
    // -------------------------------------------------------------------

    #[tokio::test]
    async fn last_heard_deduplicates_by_callsign() {
        let mut gw = mock_gateway().await;

        // Hear the same station twice.
        for _ in 0..2 {
            let (_, encoded) = sample_header();
            let mut header_frame = vec![START_BYTE, 44, CMD_DSTAR_HEADER];
            header_frame.extend_from_slice(&encoded);
            gw.session.transport.queue_read(&header_frame);
            let _ = gw.next_event().await.unwrap();
        }

        // Should only have one entry.
        assert_eq!(gw.last_heard().len(), 1);
        assert_eq!(gw.last_heard()[0].callsign, "W1AW");
    }

    #[tokio::test]
    async fn last_heard_enforces_max_size() {
        let mut gw = mock_gateway().await;
        // Set a small max.
        gw.config.max_last_heard = 2;

        // Hear 3 different stations by using different headers.
        for call in ["AA1AA   ", "BB2BB   ", "CC3CC   "] {
            let header = DStarHeader {
                flag1: 0,
                flag2: 0,
                flag3: 0,
                rpt2: "DIRECT  ".to_owned(),
                rpt1: "DIRECT  ".to_owned(),
                ur_call: "CQCQCQ  ".to_owned(),
                my_call: call.to_owned(),
                my_suffix: "    ".to_owned(),
            };
            let encoded = header.encode();
            let mut frame = vec![START_BYTE, 44, CMD_DSTAR_HEADER];
            frame.extend_from_slice(&encoded);
            gw.session.transport.queue_read(&frame);
            let _ = gw.next_event().await.unwrap();
        }

        assert_eq!(gw.last_heard().len(), 2);
        // Newest first.
        assert_eq!(gw.last_heard()[0].callsign, "CC3CC");
        assert_eq!(gw.last_heard()[1].callsign, "BB2BB");
    }

    // -------------------------------------------------------------------
    // Slow data / text message tests
    // -------------------------------------------------------------------

    #[tokio::test]
    async fn gateway_decodes_text_message_at_eot() {
        let mut gw = mock_gateway().await;

        // First, receive a header.
        let (_, hdr_bytes) = sample_header();
        let mut header_frame = vec![START_BYTE, 44, CMD_DSTAR_HEADER];
        header_frame.extend_from_slice(&hdr_bytes);
        gw.session.transport.queue_read(&header_frame);
        let _ = gw.next_event().await.unwrap();

        // Encode a text message as slow data.
        let slow_enc = SlowDataEncoder::new();
        let payloads = slow_enc.encode_message("Hi!");
        assert_eq!(payloads.len(), 2);

        // Feed voice frames with the slow data.
        for payload in &payloads {
            let mut data = [0u8; 12];
            data[..9].fill(0x00); // silence AMBE
            data[9..12].copy_from_slice(payload);
            let mut frame = vec![START_BYTE, 15, CMD_DSTAR_DATA];
            frame.extend_from_slice(&data);
            gw.session.transport.queue_read(&frame);
            let _ = gw.next_event().await.unwrap();
        }

        // Now send EOT --- should emit TextMessage instead of VoiceEnd
        // because a complete message was decoded.
        let eot_frame = vec![START_BYTE, 3, CMD_DSTAR_EOT];
        gw.session.transport.queue_read(&eot_frame);

        let event = gw.next_event().await.unwrap().unwrap();
        match event {
            DStarEvent::TextMessage(text) => {
                assert_eq!(text, "Hi!");
            }
            other => panic!("expected TextMessage, got {other:?}"),
        }
    }

    // -------------------------------------------------------------------
    // Encode text message tests
    // -------------------------------------------------------------------

    #[test]
    fn encode_text_message_produces_blocks() {
        let payloads = DStarGateway::<MockTransport>::encode_text_message("Hello");
        // "Hello" = 5 chars => 1 block => 2 halves.
        assert_eq!(payloads.len(), 2);
    }

    #[test]
    fn encode_text_message_empty() {
        let payloads = DStarGateway::<MockTransport>::encode_text_message("");
        assert!(payloads.is_empty());
    }

    #[test]
    fn encode_text_message_multi_block() {
        let payloads = DStarGateway::<MockTransport>::encode_text_message("Hello World");
        // 11 chars => 3 blocks (5+5+1) => 6 halves.
        assert_eq!(payloads.len(), 6);
    }

    // -------------------------------------------------------------------
    // ACK/NAK passthrough test
    // -------------------------------------------------------------------

    #[tokio::test]
    async fn gateway_ignores_ack_nak() {
        let mut gw = mock_gateway().await;

        // Queue an ACK frame.
        let ack_frame = vec![START_BYTE, 4, CMD_ACK, 0x02];
        gw.session.transport.queue_read(&ack_frame);

        let event = gw.next_event().await.unwrap();
        assert!(event.is_none());
    }
}
