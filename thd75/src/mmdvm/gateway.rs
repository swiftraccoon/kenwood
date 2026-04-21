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
//! The [`DStarGateway`] owns an [`mmdvm::AsyncModem`] via an
//! [`MmdvmSession`]. The [`mmdvm`] crate's async shell handles MMDVM
//! framing, periodic `GetStatus` polling, and TX-buffer slot gating
//! in a spawned task; the gateway consumes the [`mmdvm::Event`]
//! stream, translates it into [`DStarEvent`]s, and forwards TX frames
//! through the handle's `send_dstar_*` methods.
//!
//! Create a gateway with [`DStarGateway::start`], which enters MMDVM
//! mode and initializes D-STAR, and tear it down with
//! [`DStarGateway::stop`], which exits MMDVM mode and returns the
//! [`Radio`] for other use.
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

use dstar_gateway_core::{DStarHeader, SlowDataTextCollector, VoiceFrame};
use mmdvm::{AsyncModem, Event};
use mmdvm_core::{MMDVM_SET_CONFIG, ModemMode, ModemStatus};

use crate::error::Error;
use crate::radio::Radio;
use crate::radio::mmdvm_session::{MmdvmRadioRestore, MmdvmSession};
use crate::transport::{MmdvmTransportAdapter, Transport};
use crate::types::TncBaud;
use crate::types::dstar::UrCallAction;

/// Default receive timeout for `next_event` polling (500 ms).
///
/// Gives the event loop a short ceiling so callers can drive other
/// work between polls on a quiet channel.
const EVENT_POLL_TIMEOUT: Duration = Duration::from_millis(500);

/// Reverses the bit order within a byte (MSB ↔ LSB).
///
/// The TH-D75's MMDVM firmware assembles serial bytes LSB-first — bit 0
/// of each delivered byte is the earliest-in-time bit that came off the
/// wire. Every other MMDVM-standard D-STAR tooling (mbelib, DSD,
/// `dstar-gateway-core`) expects the MSB-first convention where bit 7
/// is earliest. Bit-reversing each D-STAR-voice byte at the TH-D75
/// gateway boundary translates between the two conventions so
/// downstream decoders see standards-compliant bytes.
///
/// Discovered empirically (bit-reversal experiment on a captured
/// "chip hello" AMBE frame, 2026-04): applying this transform to
/// captured `DStarData`
/// payloads eliminated all spurious tone/erasure frames and dropped
/// the mean per-frame `b0` jump from 38 to 6.6, consistent with real
/// speech. The fix applies symmetrically on TX so what we hand to the
/// radio for air transmission matches what the DVSI expects.
#[inline]
const fn bit_reverse(byte: u8) -> u8 {
    let mut b = byte;
    b = (b & 0xAA) >> 1 | (b & 0x55) << 1;
    b = (b & 0xCC) >> 2 | (b & 0x33) << 2;
    b = (b & 0xF0) >> 4 | (b & 0x0F) << 4;
    b
}

/// Default maximum entries in the last-heard list.
const DEFAULT_MAX_LAST_HEARD: usize = 100;

/// Default initial reconnection delay.
const DEFAULT_RECONNECT_INITIAL: Duration = Duration::from_secs(1);

/// Default maximum reconnection delay.
const DEFAULT_RECONNECT_MAX: Duration = Duration::from_secs(30);

/// Timeout waiting for each ACK during the D-STAR init handshake.
const INIT_ACK_TIMEOUT: Duration = Duration::from_secs(2);

/// Default TX delay for MMDVM `SetConfig` (in 10 ms units).
const DEFAULT_TX_DELAY: u8 = 10;

/// Default RX audio level for MMDVM `SetConfig`.
const DEFAULT_RX_LEVEL: u8 = 128;

/// Default TX audio level for MMDVM `SetConfig`.
const DEFAULT_TX_LEVEL: u8 = 128;

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
    VoiceData(VoiceFrame),
    /// Voice transmission ended cleanly (EOT received).
    VoiceEnd,
    /// Voice transmission lost (no clean EOT, signal lost).
    VoiceLost,
    /// A slow data text message was decoded from the voice stream.
    TextMessage(String),
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
pub struct DStarGateway<T: Transport + Unpin + 'static> {
    /// The underlying MMDVM async modem.
    modem: AsyncModem<MmdvmTransportAdapter<T>>,
    /// Radio-state restore envelope used on [`Self::stop`].
    restore: MmdvmRadioRestore<T>,
    /// Gateway configuration.
    config: DStarGatewayConfig,
    /// Slow data decoder for the current RX stream.
    slow_data: SlowDataTextCollector,
    /// Frame counter for slow data decoding within a transmission.
    slow_data_frame_index: u8,
    /// Last-heard station list, newest first.
    last_heard: Vec<LastHeardEntry>,
    /// Whether a voice transmission is currently active (RX from radio).
    rx_active: bool,
    /// The D-STAR header for the currently active RX transmission.
    rx_header: Option<DStarHeader>,
    /// Buffered events to emit on the next `next_event` call.
    pending_events: VecDeque<DStarEvent>,
    /// Echo recording buffer (header + voice frames).
    echo_header: Option<DStarHeader>,
    /// Echo recorded voice frames.
    echo_frames: Vec<VoiceFrame>,
    /// Whether echo recording is active.
    echo_active: bool,
    /// Per-event poll timeout (configurable via [`Self::set_event_timeout`]).
    event_timeout: Duration,
}

impl<T: Transport + Unpin + 'static> std::fmt::Debug for DStarGateway<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DStarGateway")
            .field("config", &self.config)
            .field("rx_active", &self.rx_active)
            .field("last_heard_count", &self.last_heard.len())
            .finish_non_exhaustive()
    }
}

impl<T: Transport + Unpin + 'static> DStarGateway<T> {
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
    /// Panics if MMDVM was entered and D-STAR init failed AND the
    /// subsequent MMDVM exit also failed. This indicates unrecoverable
    /// transport state; the caller's only option is to drop any
    /// remaining handles and reconnect to the radio from scratch.
    pub async fn start(
        radio: Radio<T>,
        config: DStarGatewayConfig,
    ) -> Result<Self, (Radio<T>, Error)> {
        let session = match radio.enter_mmdvm(config.baud).await {
            Ok(s) => s,
            Err((radio, e)) => return Err((radio, e)),
        };

        match Self::build_from_session(session, config).await {
            Ok(gateway) => Ok(gateway),
            Err((restore, modem, init_err)) => {
                // Init failed; roll back MMDVM mode to recover the Radio.
                match restore.exit_and_rebuild(modem).await {
                    Ok(radio) => Err((radio, init_err)),
                    Err(exit_err) => {
                        tracing::error!(
                            init_err = %init_err,
                            exit_err = %exit_err,
                            "MMDVM exit failed after D-STAR init failure; \
                             radio state is unrecoverable"
                        );
                        // We cannot return a valid Radio to the caller.
                        // The contract of this function requires a Radio
                        // in the error path; this is an unrecoverable
                        // state, so we mark it with `unreachable!` to
                        // satisfy the clippy `panic` lint while still
                        // failing loudly.
                        unreachable!(
                            "MMDVM exit failed after D-STAR init failure; \
                             transport is in an unrecoverable state: \
                             init_err={init_err}, exit_err={exit_err}"
                        );
                    }
                }
            }
        }
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
        let session = radio.into_mmdvm_session();
        Self::build_from_session(session, config)
            .await
            .map_err(|(_restore, _modem, err)| err)
    }

    /// Build a gateway from an already-prepared [`MmdvmSession`].
    ///
    /// Runs the D-STAR init handshake (`SetConfig` + `SetMode`) and,
    /// on success, returns the fully-initialised gateway. On failure,
    /// returns the `(restore, modem, error)` triple so the caller can
    /// clean up the MMDVM session before surfacing the error.
    async fn build_from_session(
        session: MmdvmSession<T>,
        config: DStarGatewayConfig,
    ) -> Result<
        Self,
        (
            MmdvmRadioRestore<T>,
            AsyncModem<MmdvmTransportAdapter<T>>,
            Error,
        ),
    > {
        let (mut modem, restore) = session.into_parts();

        if let Err(e) = init_dstar(&mut modem).await {
            return Err((restore, modem, e));
        }

        Ok(Self {
            modem,
            restore,
            config,
            slow_data: SlowDataTextCollector::new(),
            slow_data_frame_index: 0,
            last_heard: Vec::new(),
            rx_active: false,
            rx_header: None,
            pending_events: VecDeque::new(),
            echo_header: None,
            echo_frames: Vec::new(),
            echo_active: false,
            event_timeout: EVENT_POLL_TIMEOUT,
        })
    }

    /// Stop the gateway, exiting MMDVM mode and returning the [`Radio`].
    ///
    /// # Errors
    ///
    /// Returns an error if the MMDVM exit command fails.
    pub async fn stop(self) -> Result<Radio<T>, Error> {
        self.restore.exit_and_rebuild(self.modem).await
    }

    /// Process pending I/O and return the next event.
    ///
    /// Each call waits up to [`Self::set_event_timeout`] for a new MMDVM
    /// event from the modem loop, translates it into a [`DStarEvent`],
    /// and returns. Returns `Ok(None)` when no MMDVM event arrives
    /// within the timeout.
    ///
    /// # Errors
    ///
    /// Only returns errors if the underlying transport fails fatally.
    /// Malformed frames are swallowed by the [`mmdvm`] crate's RX loop
    /// as debug diagnostics — propagating a decode error would kill
    /// the whole session on a single malformed byte.
    pub async fn next_event(&mut self) -> Result<Option<DStarEvent>, Error> {
        // Drain buffered events first (e.g. UrCallCommand after VoiceStart).
        if let Some(evt) = self.pending_events.pop_front() {
            return Ok(Some(evt));
        }

        // Noise events (Status at 4 Hz, init-handshake Version/Ack/Nak,
        // Debug frames, etc.) are swallowed by `dispatch_event` and
        // surface as `Ok(None)`. Callers' typical drain loop is
        // `while let Ok(Some(e)) = gw.next_event().await { ... }`,
        // which would BREAK on the first noise event — leaving the
        // remaining noise in the mmdvm event channel. During an
        // active D-STAR voice stream the REPL spends most of its
        // time in the reflector-event branch of `dstar_poll_cycle`,
        // producing only ~one radio-drain pass per cycle; if that
        // pass swallows a single Status and then breaks, noise
        // accumulates faster than it's consumed. The channel fills
        // at cap 256 after ~128 s of voice, at which point
        // `ModemLoop::emit_event` blocks forever on `event_tx.send`,
        // wedging command processing and deadlocking the REPL on
        // `send_dstar_data`'s oneshot reply.
        //
        // Fix: loop internally past noise within the caller's time
        // budget so `Ok(None)` means "timed out with no meaningful
        // event" and nothing else.
        let timeout = self.event_timeout;
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                return Ok(None);
            }
            let Ok(Some(raw)) = tokio::time::timeout(remaining, self.modem.next_event()).await
            else {
                // Either a timeout (outer Err) or the task shut down cleanly
                // (inner None) — surface no event.
                return Ok(None);
            };
            if let Some(evt) = self.dispatch_event(raw).await? {
                return Ok(Some(evt));
            }
            // `dispatch_event` returned `Ok(None)` — noise event
            // consumed. Keep pulling from the mmdvm channel within
            // the same deadline so periodic Status frames don't
            // short-circuit the caller's drain loop.
        }
    }

    /// Dispatch a raw [`mmdvm::Event`] into a [`DStarEvent`].
    async fn dispatch_event(&mut self, raw: Event) -> Result<Option<DStarEvent>, Error> {
        match raw {
            Event::DStarHeaderRx { bytes } => {
                let header = DStarHeader::decode(&bytes);
                self.handle_voice_start(header);
                Ok(Some(DStarEvent::VoiceStart(header)))
            }
            Event::DStarDataRx { bytes } => {
                // The TH-D75 firmware hands us each byte bit-reversed.
                // See `bit_reverse` for the full rationale.
                let mut ambe = [0u8; 9];
                if let Some(src) = bytes.get(..9) {
                    for (dst, &b) in ambe.iter_mut().zip(src.iter()) {
                        *dst = bit_reverse(b);
                    }
                }
                let mut slow_data = [0u8; 3];
                if let Some(src) = bytes.get(9..12) {
                    for (dst, &b) in slow_data.iter_mut().zip(src.iter()) {
                        *dst = bit_reverse(b);
                    }
                }
                let frame = VoiceFrame { ambe, slow_data };
                self.handle_voice_data(frame);
                Ok(Some(DStarEvent::VoiceData(frame)))
            }
            Event::DStarEot => self.on_eot().await,
            Event::DStarLost => {
                self.rx_active = false;
                self.rx_header = None;
                Ok(Some(DStarEvent::VoiceLost))
            }
            Event::TransportClosed => Err(Error::Transport(
                crate::error::TransportError::Disconnected(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "MMDVM transport closed",
                )),
            )),
            // Everything else is non-fatal noise — status updates,
            // init-handshake artefacts, debug frames, unhandled
            // commands, and `#[non_exhaustive]` variants the mmdvm
            // crate may add in the future.
            other => {
                log_noise_event(&other);
                Ok(None)
            }
        }
    }

    /// Handle a received D-STAR EOT, emitting any queued text message
    /// and driving echo playback if the record phase was active.
    async fn on_eot(&mut self) -> Result<Option<DStarEvent>, Error> {
        let text_event = self.take_text_message();
        let was_echo = self.echo_active;
        if was_echo {
            self.echo_active = false;
            self.play_echo().await?;
        }
        self.rx_active = false;
        self.rx_header = None;
        if let Some(text) = text_event {
            self.pending_events.push_back(DStarEvent::TextMessage(text));
        }
        Ok(Some(DStarEvent::VoiceEnd))
    }

    /// Handle a received D-STAR header (internal).
    fn handle_voice_start(&mut self, header: DStarHeader) {
        self.rx_active = true;
        self.slow_data.reset();
        self.slow_data_frame_index = 0;
        self.rx_header = Some(header);

        // Parse URCALL for special commands.
        let ur_str = std::str::from_utf8(header.ur_call.as_bytes()).unwrap_or("");
        let action = UrCallAction::parse(ur_str);
        match &action {
            UrCallAction::Cq | UrCallAction::Callsign(_) => {}
            UrCallAction::Echo => {
                self.echo_active = true;
                self.echo_header = Some(header);
                self.echo_frames.clear();
            }
            _ => {
                self.pending_events
                    .push_back(DStarEvent::UrCallCommand(action));
            }
        }

        // Update last-heard list.
        let entry = LastHeardEntry {
            callsign: cs_trim(header.my_call),
            suffix: sfx_trim(header.my_suffix),
            destination: cs_trim(header.ur_call),
            repeater1: cs_trim(header.rpt1),
            repeater2: cs_trim(header.rpt2),
            timestamp: Instant::now(),
        };
        self.update_last_heard(entry);
    }

    /// Handle a received D-STAR voice frame (internal).
    fn handle_voice_data(&mut self, frame: VoiceFrame) {
        // Feed the slow data collector. Non-zero index so the
        // sync-frame codepath in the collector doesn't fire.
        let idx = (self.slow_data_frame_index % 20) + 1;
        self.slow_data.push(frame.slow_data, idx);
        self.slow_data_frame_index = self.slow_data_frame_index.wrapping_add(1);

        if self.echo_active {
            self.echo_frames.push(frame);
        }
    }

    /// Send a D-STAR voice header to the radio for transmission.
    ///
    /// Enqueues the header in the mmdvm TX queue, which is drained
    /// when the modem reports enough D-STAR FIFO space.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Transport`] if the modem loop has exited.
    pub async fn send_header(&mut self, header: &DStarHeader) -> Result<(), Error> {
        let encoded = header.encode();
        self.modem
            .send_dstar_header(encoded)
            .await
            .map_err(shell_err_to_thd75_err)
    }

    /// Send a D-STAR voice data frame to the radio for transmission.
    ///
    /// Enqueues the frame in the mmdvm TX queue. Pacing is handled
    /// inside the mmdvm modem loop — no host-side sleep is introduced
    /// here.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Transport`] if the modem loop has exited.
    pub async fn send_voice(&mut self, frame: &VoiceFrame) -> Result<(), Error> {
        // Do NOT bit-reverse on TX.
        //
        // Originally this path mirrored the RX bit-reversal for
        // symmetry, but the TH-D75's MMDVM firmware turns out to
        // handle TX and RX asymmetrically — the RX path delivers
        // bytes LSB-first (hence `dispatch_event`'s reversal to
        // restore MSB-first spec convention), but the TX path
        // accepts bytes in the on-wire MSB-first order directly.
        // User-confirmed regression: adding TX reversal broke
        // thd75-repl's D-STAR audio forwarding — the radio
        // received the D-STAR header (text message popped up on the
        // LCD) but couldn't decode any voice frames because the
        // byte layout no longer matched what the DVSI chip expected.
        // Reverting the TX path to raw passthrough restores
        // thd75-repl audio forwarding while keeping the RX fix that
        // made radio→sextant intelligible.
        let mut data = [0u8; 12];
        if let Some(dst) = data.get_mut(..9) {
            dst.copy_from_slice(&frame.ambe);
        }
        if let Some(dst) = data.get_mut(9..12) {
            dst.copy_from_slice(&frame.slow_data);
        }
        tracing::trace!(target: "mmdvm::hang_hunt", "gateway.send_voice: awaiting modem.send_dstar_data");
        let r = self
            .modem
            .send_dstar_data(data)
            .await
            .map_err(shell_err_to_thd75_err);
        tracing::trace!(target: "mmdvm::hang_hunt", "gateway.send_voice: modem.send_dstar_data returned");
        r
    }

    /// Send a voice frame to the radio without any host-side pacing.
    ///
    /// In the current architecture (mmdvm owns pacing via its
    /// buffer-gated `TxQueue` drain), this method and
    /// [`Self::send_voice`] are functionally equivalent; both simply
    /// enqueue the frame and let the modem loop drain when
    /// `dstar_space` allows. The alias is retained for back-compat
    /// with callers that historically preferred the unpaced variant.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Transport`] if the modem loop has exited.
    pub async fn send_voice_unpaced(&mut self, frame: &VoiceFrame) -> Result<(), Error> {
        self.send_voice(frame).await
    }

    /// Send end-of-transmission to the radio.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Transport`] if the modem loop has exited.
    pub async fn send_eot(&mut self) -> Result<(), Error> {
        self.modem
            .send_dstar_eot()
            .await
            .map_err(shell_err_to_thd75_err)
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
        use dstar_gateway_core::{Callsign, Suffix};

        let rpt_bytes = reflector.map_or(*b"DIRECT  ", |(name, module)| {
            let mut bytes = [b' '; 8];
            let name_bytes = name.as_bytes();
            let n = name_bytes.len().min(7);
            if let Some(dst) = bytes.get_mut(..n)
                && let Some(src) = name_bytes.get(..n)
            {
                dst.copy_from_slice(src);
            }
            if let Some(b) = bytes.get_mut(7) {
                *b = u8::try_from(u32::from(module)).unwrap_or(b'?');
            }
            bytes
        });

        let mut my_bytes = [b' '; 8];
        let cs = self.config.callsign.as_bytes();
        let n = cs.len().min(8);
        if let Some(dst) = my_bytes.get_mut(..n)
            && let Some(src) = cs.get(..n)
        {
            dst.copy_from_slice(src);
        }

        let mut suffix_bytes = [b' '; 4];
        let sfx = self.config.suffix.as_bytes();
        let s = sfx.len().min(4);
        if let Some(dst) = suffix_bytes.get_mut(..s)
            && let Some(src) = sfx.get(..s)
        {
            dst.copy_from_slice(src);
        }

        let header = DStarHeader {
            flag1: 0x00,
            flag2: 0x00,
            flag3: 0x00,
            rpt2: Callsign::from_wire_bytes(rpt_bytes),
            rpt1: Callsign::from_wire_bytes(rpt_bytes),
            ur_call: Callsign::from_wire_bytes(*b"CQCQCQ  "),
            my_call: Callsign::from_wire_bytes(my_bytes),
            my_suffix: Suffix::from_wire_bytes(suffix_bytes),
        };

        self.send_header(&header).await
    }

    /// Set the receive timeout for `next_event` polling.
    ///
    /// Lower values make the event loop more responsive but increase
    /// CPU usage. Use short timeouts (10-50ms) when actively relaying
    /// voice from a reflector.
    pub const fn set_event_timeout(&mut self, timeout: Duration) {
        self.event_timeout = timeout;
    }

    /// Current receive timeout for `next_event` polling.
    ///
    /// Mirrors [`Self::set_event_timeout`]. Callers that temporarily
    /// drop the timeout (e.g. during a tight event-drain loop) use
    /// this to save and restore the prior value.
    #[must_use]
    pub const fn event_timeout(&self) -> Duration {
        self.event_timeout
    }

    /// Get the last-heard list (newest first).
    #[must_use]
    pub fn last_heard(&self) -> &[LastHeardEntry] {
        &self.last_heard
    }

    /// Poll the modem status.
    ///
    /// Requests an immediate `GetStatus` and returns the next status
    /// event delivered by the modem loop. The mmdvm modem loop also
    /// polls status periodically (every 250 ms), so callers rarely
    /// need this.
    ///
    /// # Errors
    ///
    /// Returns an error if the status request fails or the modem loop
    /// exits before delivering a status event.
    pub async fn poll_status(&mut self) -> Result<ModemStatus, Error> {
        self.modem
            .request_status()
            .await
            .map_err(shell_err_to_thd75_err)?;

        // Drain until we see a Status event or the channel closes.
        loop {
            let evt =
                match tokio::time::timeout(Duration::from_secs(2), self.modem.next_event()).await {
                    Ok(Some(e)) => e,
                    Ok(None) => {
                        return Err(Error::Transport(
                            crate::error::TransportError::Disconnected(std::io::Error::new(
                                std::io::ErrorKind::UnexpectedEof,
                                "MMDVM modem loop exited before delivering status",
                            )),
                        ));
                    }
                    Err(_) => {
                        return Err(Error::Timeout(Duration::from_secs(2)));
                    }
                };
            if let Event::Status(status) = evt {
                return Ok(status);
            }
        }
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
        self.last_heard.retain(|e| e.callsign != entry.callsign);
        self.last_heard.insert(0, entry);
        if self.last_heard.len() > self.config.max_last_heard {
            self.last_heard.truncate(self.config.max_last_heard);
        }
    }

    /// Play back recorded echo frames to the radio.
    ///
    /// Builds a modified header (`RPT2` = callsign + G, `RPT1` = callsign
    /// + reflector module) and transmits all recorded frames.
    async fn play_echo(&mut self) -> Result<(), Error> {
        use dstar_gateway_core::{Callsign, Suffix};

        let Some(orig_header) = self.echo_header.take() else {
            return Ok(());
        };
        let frames = std::mem::take(&mut self.echo_frames);
        if frames.is_empty() {
            return Ok(());
        }

        let mut rpt2_bytes = [b' '; 8];
        let cs = self.config.callsign.as_bytes();
        let n = cs.len().min(7);
        if let Some(dst) = rpt2_bytes.get_mut(..n)
            && let Some(src) = cs.get(..n)
        {
            dst.copy_from_slice(src);
        }
        if let Some(b) = rpt2_bytes.get_mut(7) {
            *b = b'G';
        }

        let mut my_bytes = [b' '; 8];
        let m = cs.len().min(8);
        if let Some(dst) = my_bytes.get_mut(..m)
            && let Some(src) = cs.get(..m)
        {
            dst.copy_from_slice(src);
        }

        let mut suffix_bytes = [b' '; 4];
        let sfx = self.config.suffix.as_bytes();
        let s = sfx.len().min(4);
        if let Some(dst) = suffix_bytes.get_mut(..s)
            && let Some(src) = sfx.get(..s)
        {
            dst.copy_from_slice(src);
        }

        let echo_header = DStarHeader {
            flag1: orig_header.flag1,
            flag2: orig_header.flag2,
            flag3: orig_header.flag3,
            rpt2: Callsign::from_wire_bytes(rpt2_bytes),
            rpt1: orig_header.rpt1,
            ur_call: orig_header.my_call,
            my_call: Callsign::from_wire_bytes(my_bytes),
            my_suffix: Suffix::from_wire_bytes(suffix_bytes),
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
    fn take_text_message(&mut self) -> Option<String> {
        let bytes = self.slow_data.take_message()?;
        Some(String::from_utf8_lossy(&bytes).into_owned())
    }
}

/// Initialise the MMDVM modem for D-STAR: send `SetConfig` with
/// D-STAR-only flags, then `SetMode(DStar)`.
///
/// Consumes events until the corresponding ACK arrives for each
/// command. `Version` and `Status` events delivered by the modem's
/// startup handshake are accepted silently.
async fn init_dstar<T: Transport + Unpin + 'static>(
    modem: &mut AsyncModem<MmdvmTransportAdapter<T>>,
) -> Result<(), Error> {
    // Send SetConfig: D-STAR-only, default levels.
    let config_payload = vec![
        0x00, // invert
        0x01, // mode flags: D-STAR only
        DEFAULT_TX_DELAY,
        ModemMode::DStar.as_byte(),
        DEFAULT_RX_LEVEL,
        DEFAULT_TX_LEVEL,
    ];
    modem
        .send_raw(MMDVM_SET_CONFIG, config_payload)
        .await
        .map_err(shell_err_to_thd75_err)?;
    await_ack(modem, MMDVM_SET_CONFIG).await?;

    // Send SetMode.
    modem
        .set_mode(ModemMode::DStar)
        .await
        .map_err(shell_err_to_thd75_err)?;
    await_ack(modem, mmdvm_core::MMDVM_SET_MODE).await?;

    Ok(())
}

/// Wait for an ACK for the given command byte, dropping Version /
/// Status events that arrive in the meantime.
async fn await_ack<T: Transport + Unpin + 'static>(
    modem: &mut AsyncModem<MmdvmTransportAdapter<T>>,
    expected_command: u8,
) -> Result<(), Error> {
    let deadline = tokio::time::Instant::now() + INIT_ACK_TIMEOUT;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return Err(Error::Timeout(INIT_ACK_TIMEOUT));
        }
        let Ok(maybe_evt) = tokio::time::timeout(remaining, modem.next_event()).await else {
            return Err(Error::Timeout(INIT_ACK_TIMEOUT));
        };
        let Some(evt) = maybe_evt else {
            return Err(Error::Transport(
                crate::error::TransportError::Disconnected(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "MMDVM modem loop exited during init",
                )),
            ));
        };
        match evt {
            Event::Ack { command } if command == expected_command => return Ok(()),
            Event::Nak { command, reason } if command == expected_command => {
                return Err(Error::Protocol(
                    crate::error::ProtocolError::UnexpectedResponse {
                        expected: format!("MMDVM ACK for 0x{expected_command:02X}"),
                        actual: format!("NAK: {reason:?}").into_bytes(),
                    },
                ));
            }
            Event::Version(_) | Event::Status(_) | Event::Ack { .. } | Event::Nak { .. } => {
                // Drop stray handshake events.
            }
            Event::Debug { level, text } => {
                tracing::trace!(level, ?text, "MMDVM debug during init");
            }
            Event::TransportClosed => {
                return Err(Error::Transport(
                    crate::error::TransportError::Disconnected(std::io::Error::new(
                        std::io::ErrorKind::UnexpectedEof,
                        "MMDVM transport closed during init",
                    )),
                ));
            }
            // Any protocol frames during init are unexpected but non-fatal.
            Event::DStarHeaderRx { .. }
            | Event::DStarDataRx { .. }
            | Event::DStarLost
            | Event::DStarEot
            | Event::SerialData(_)
            | Event::TransparentData(_)
            | Event::UnhandledResponse { .. } => {
                tracing::debug!("unexpected MMDVM event during init; ignoring");
            }
            // `mmdvm::Event` is marked `#[non_exhaustive]` — new
            // variants are added without a major version bump. Treat
            // unknown events as "keep waiting for the ACK".
            _ => {
                tracing::debug!("unrecognised MMDVM event during init; ignoring");
            }
        }
    }
}

/// Translate an [`mmdvm::ShellError`] into a thd75 [`Error`].
fn shell_err_to_thd75_err(err: mmdvm::ShellError) -> Error {
    match err {
        mmdvm::ShellError::SessionClosed => {
            Error::Transport(crate::error::TransportError::Disconnected(
                std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "MMDVM session closed"),
            ))
        }
        mmdvm::ShellError::Core(e) => Error::Protocol(crate::error::ProtocolError::FieldParse {
            command: "MMDVM".to_owned(),
            field: "frame".to_owned(),
            detail: format!("{e}"),
        }),
        mmdvm::ShellError::Io(e) => Error::Transport(crate::error::TransportError::Disconnected(e)),
        mmdvm::ShellError::BufferFull { mode } => {
            Error::Protocol(crate::error::ProtocolError::UnexpectedResponse {
                expected: format!("MMDVM {mode:?} buffer ready"),
                actual: b"buffer full".to_vec(),
            })
        }
        mmdvm::ShellError::Nak { command, reason } => {
            Error::Protocol(crate::error::ProtocolError::UnexpectedResponse {
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

/// Log a non-fatal MMDVM event (status update, init handshake
/// artefact, debug frame, etc.) at the appropriate tracing level so
/// consumers that dump trace output can see what's happening.
fn log_noise_event(event: &Event) {
    match event {
        Event::Status(status) => {
            // Buffer-slot gating happens inside mmdvm's TxQueue; no
            // consumer-side action needed. Log all status fields at
            // trace so operators can audit modem state over time —
            // particularly the `dstar_space` FIFO depth and the
            // overflow / lockout / CD bits that signal trouble.
            tracing::trace!(
                target: "kenwood_thd75::mmdvm::gateway",
                mode = ?status.mode,
                flags = format!("0x{:02X}", status.flags.bits()),
                tx = status.tx(),
                cd = status.cd(),
                lockout = status.lockout(),
                adc_overflow = status.adc_overflow(),
                rx_overflow = status.rx_overflow(),
                tx_overflow = status.tx_overflow(),
                dac_overflow = status.dac_overflow(),
                dstar_space = status.dstar_space,
                "MMDVM status"
            );
        }
        Event::Ack { command } => tracing::debug!(
            target: "kenwood_thd75::mmdvm::gateway",
            command = format!("0x{command:02X}"),
            "MMDVM ACK (ignored)"
        ),
        Event::Nak { command, reason } => tracing::debug!(
            target: "kenwood_thd75::mmdvm::gateway",
            command = format!("0x{command:02X}"),
            ?reason,
            "MMDVM NAK (ignored)"
        ),
        Event::Version(v) => tracing::debug!(
            target: "kenwood_thd75::mmdvm::gateway",
            protocol = v.protocol,
            description = %v.description,
            "MMDVM Version (ignored)"
        ),
        Event::Debug { level, text } => tracing::trace!(
            target: "kenwood_thd75::mmdvm::gateway",
            level = *level,
            text = %text,
            "MMDVM debug"
        ),
        Event::SerialData(data) => tracing::trace!(
            target: "kenwood_thd75::mmdvm::gateway",
            len = data.len(),
            "MMDVM serial data (ignored)"
        ),
        Event::TransparentData(data) => tracing::trace!(
            target: "kenwood_thd75::mmdvm::gateway",
            len = data.len(),
            "MMDVM transparent data (ignored)"
        ),
        Event::UnhandledResponse { command, payload } => tracing::debug!(
            target: "kenwood_thd75::mmdvm::gateway",
            command = format!("0x{command:02X}"),
            payload_len = payload.len(),
            "MMDVM unhandled response"
        ),
        // Handled variants should never reach this helper; unknown
        // future variants fall through silently.
        _ => tracing::trace!(
            target: "kenwood_thd75::mmdvm::gateway",
            "MMDVM unrecognised event"
        ),
    }
}

/// Trim trailing spaces from a `Callsign` and return an owned `String`.
fn cs_trim(cs: dstar_gateway_core::Callsign) -> String {
    std::str::from_utf8(cs.as_bytes())
        .unwrap_or("")
        .trim_end()
        .to_owned()
}

/// Trim trailing spaces from a `Suffix` and return an owned `String`.
fn sfx_trim(sfx: dstar_gateway_core::Suffix) -> String {
    std::str::from_utf8(sfx.as_bytes())
        .unwrap_or("")
        .trim_end()
        .to_owned()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::TncBaud;

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
        assert!(debug.contains("N0CALL"), "debug should mention callsign");
    }

    // -------------------------------------------------------------------
    // Voice frame tests
    // -------------------------------------------------------------------

    #[test]
    fn voice_frame_construction() {
        let frame = VoiceFrame {
            ambe: [1, 2, 3, 4, 5, 6, 7, 8, 9],
            slow_data: [0xA, 0xB, 0xC],
        };
        assert_eq!(frame.ambe[0], 1);
        assert_eq!(frame.slow_data[2], 0xC);
    }

    #[test]
    fn voice_frame_equality() {
        let a = VoiceFrame {
            ambe: [0; 9],
            slow_data: [0; 3],
        };
        let b = a;
        assert_eq!(a, b);
    }

    // -------------------------------------------------------------------
    // Bit-reversal tests (TH-D75 MMDVM byte-order quirk)
    // -------------------------------------------------------------------

    #[test]
    fn bit_reverse_identities() {
        // Known bit-reverse cases from the D75 serial-byte convention.
        assert_eq!(bit_reverse(0x00), 0x00);
        assert_eq!(bit_reverse(0xFF), 0xFF);
        assert_eq!(bit_reverse(0x80), 0x01);
        assert_eq!(bit_reverse(0x01), 0x80);
        assert_eq!(bit_reverse(0xAA), 0x55);
        assert_eq!(bit_reverse(0x55), 0xAA);
    }

    #[test]
    fn bit_reverse_is_involution() {
        // Applying bit-reversal twice must return the original byte —
        // guarantees RX reverse and TX reverse are mirror operations.
        for b in 0u8..=255 {
            assert_eq!(
                bit_reverse(bit_reverse(b)),
                b,
                "double-reverse should restore {b:#04x}"
            );
        }
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
        assert!(debug.contains("W1AW"), "debug should mention callsign");
    }

    // -------------------------------------------------------------------
    // Event enum tests
    // -------------------------------------------------------------------

    #[test]
    fn event_debug_formatting() {
        let event = DStarEvent::VoiceEnd;
        let debug = format!("{event:?}");
        assert!(debug.contains("VoiceEnd"), "debug should mention variant");
    }

    #[test]
    fn event_text_message_debug() {
        let event = DStarEvent::TextMessage("Hello D-STAR".to_owned());
        let debug = format!("{event:?}");
        assert!(debug.contains("Hello D-STAR"), "debug should mention text");
    }

    // -------------------------------------------------------------------
    // Reconnect policy tests
    // -------------------------------------------------------------------

    #[test]
    fn reconnect_policy_exponential_backoff() {
        let mut policy = ReconnectPolicy::default();
        let d1 = policy.next_delay();
        let d2 = policy.next_delay();
        assert_eq!(d1, DEFAULT_RECONNECT_INITIAL);
        assert_eq!(d2, DEFAULT_RECONNECT_INITIAL * 2);
    }

    #[test]
    fn reconnect_policy_caps_at_max() {
        let mut policy = ReconnectPolicy::new(Duration::from_secs(1), Duration::from_secs(4));
        for _ in 0..10 {
            let d = policy.next_delay();
            assert!(d <= Duration::from_secs(4), "delay capped at max");
        }
    }

    #[test]
    fn reconnect_policy_reset() {
        let mut policy = ReconnectPolicy::default();
        let _ = policy.next_delay();
        let _ = policy.next_delay();
        assert!(policy.attempts() > 0);
        policy.reset();
        assert_eq!(policy.attempts(), 0);
    }

    // Shell-err translation is unit-testable without a live modem.
    #[test]
    fn shell_err_session_closed_maps_to_transport_disconnected() {
        let err = shell_err_to_thd75_err(mmdvm::ShellError::SessionClosed);
        assert!(matches!(err, Error::Transport(_)));
    }

    #[test]
    fn shell_err_io_maps_to_transport_disconnected() {
        let err = shell_err_to_thd75_err(mmdvm::ShellError::Io(std::io::Error::from(
            std::io::ErrorKind::BrokenPipe,
        )));
        assert!(matches!(err, Error::Transport(_)));
    }
}
