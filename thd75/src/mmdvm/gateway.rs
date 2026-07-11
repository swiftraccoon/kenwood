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

/// Default maximum entries in the last-heard list.
const DEFAULT_MAX_LAST_HEARD: usize = 100;

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

// The backoff policy is link-level machinery shared with the radio
// supervisor; it lives in the session module and stays re-exported
// here so existing `mmdvm::ReconnectPolicy` paths keep working.
pub use crate::session::ReconnectPolicy;

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
    /// Last observed TX state from the modem's status responses. Used
    /// to emit a `StatusUpdate` event only on rising / falling edges
    /// — keeps the event channel from being flooded with the modem's
    /// 4 Hz status stream while still surfacing the moment the radio
    /// keys (`tx() = true`) or stops transmitting (`tx() = false`).
    /// `None` until the first status response arrives.
    last_tx_active: Option<bool>,
    /// Previous status health bits (overflow/lockout, TX and CD bits
    /// masked out) for rising-edge warn logging.
    last_health_bits: Option<u8>,
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
    /// caller can continue using CAT mode. The `Radio` is `None` only
    /// when D-STAR init failed AND the MMDVM rollback also failed
    /// (e.g. the USB cable was pulled) — the transport is gone and
    /// the caller must reconnect from scratch.
    pub async fn start(
        radio: Radio<T>,
        config: DStarGatewayConfig,
    ) -> Result<Self, (Option<Radio<T>>, Error)> {
        let session = match radio.enter_mmdvm(config.baud).await {
            Ok(s) => s,
            Err((radio, e)) => return Err((Some(radio), e)),
        };

        match Self::build_from_session(session, config).await {
            Ok(gateway) => Ok(gateway),
            Err((restore, modem, init_err)) => {
                // Init failed; roll back MMDVM mode to recover the Radio.
                match restore.exit_and_rebuild(modem).await {
                    Ok(radio) => Err((Some(radio), init_err)),
                    Err(exit_err) => {
                        // Both init AND rollback failed (one USB
                        // unplug does it). No Radio can be returned —
                        // the transport is gone — but a long-running
                        // gateway app must get an error, not a
                        // process abort.
                        tracing::error!(
                            init_err = %init_err,
                            exit_err = %exit_err,
                            "MMDVM exit failed after D-STAR init failure; \
                             radio state is unrecoverable"
                        );
                        Err((None, double_fault_error(&init_err, &exit_err)))
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
            last_tx_active: None,
            last_health_bits: None,
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
        // accumulates faster than it's consumed and the mmdvm event
        // channel fills. The modem loop never blocks on a full
        // channel — it drops events instead — so a lazy drain here
        // costs real events (received voice frames included), not a
        // deadlock.
        //
        // Fix: loop internally past noise within the caller's time
        // budget so `Ok(None)` means "timed out with no meaningful
        // event" and nothing else, and the channel stays drained.
        let timeout = self.event_timeout;
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                return Ok(None);
            }
            let raw = match tokio::time::timeout(remaining, self.modem.next_event()).await {
                Ok(Some(raw)) => raw,
                Ok(None) => {
                    // The modem task exited and its event channel is
                    // fully drained. The terminal event (if any) was
                    // already consumed — this closed channel is the
                    // only remaining signal, and a dead modem must
                    // never read as quiet airtime.
                    return Err(Error::Transport(
                        crate::error::TransportError::Disconnected(std::io::Error::new(
                            std::io::ErrorKind::UnexpectedEof,
                            "MMDVM modem loop exited",
                        )),
                    ));
                }
                // Idle poll timeout — genuinely no event this cycle.
                Err(_elapsed) => return Ok(None),
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
        // Terminal events (transport gone, loop dying) become session
        // errors — a dead modem must never read as quiet airtime.
        if let Some(err) = terminal_event_error(&raw) {
            return Err(err);
        }
        match raw {
            Event::DStarHeaderRx { bytes } => {
                let header = DStarHeader::decode(&bytes);
                self.handle_voice_start(header);
                Ok(Some(DStarEvent::VoiceStart(header)))
            }
            Event::DStarDataRx { bytes } => {
                // The radio's MMDVM firmware delivers D-STAR voice
                // payloads in on-wire byte order — the same LSB-first
                // convention reflectors relay and mbelib-rs reads
                // natively (since 2026-07-04). A historical per-byte
                // bit reversal here was compensating for the decoder's
                // then-wrong MSB-first unpack; with the decoder fixed,
                // the bytes pass through untouched, matching the TX
                // path (which was always raw passthrough).
                let mut ambe = [0u8; 9];
                if let Some(src) = bytes.get(..9) {
                    ambe.copy_from_slice(src);
                }
                let mut slow_data = [0u8; 3];
                if let Some(src) = bytes.get(9..12) {
                    slow_data.copy_from_slice(src);
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
            // Queued TX frames were discarded because the modem
            // session is ending — the operator's last over was
            // truncated on air even though every send reported
            // success. The terminal event follows immediately;
            // this is the audit trail for what it took with it.
            Event::TxDropped { frames } => {
                tracing::warn!(
                    target: "kenwood_thd75::mmdvm::gateway",
                    frames,
                    "MMDVM session discarded queued TX frames; transmission truncated"
                );
                Ok(None)
            }
            // The radio sent a frame violating the MMDVM layout for
            // its command byte. Non-fatal, but a rising count means a
            // degrading link (or a firmware quirk worth capturing).
            Event::ProtocolViolation { command, detail } => {
                tracing::warn!(
                    target: "kenwood_thd75::mmdvm::gateway",
                    command = format!("0x{command:02X}"),
                    detail = %detail,
                    "MMDVM protocol violation from radio"
                );
                Ok(None)
            }
            // Status events are 4 Hz noise — but the TX flag inside
            // them is the single most useful diagnostic for the
            // network → radio voice path: did the radio actually key
            // the transmitter after we sent it a header + voice
            // frames? We swallow the steady stream as before, but
            // surface a `StatusUpdate` event whenever the TX flag
            // *changes* state. That keeps the channel from flooding
            // while still telling the operator (and any UI) the
            // exact moment the radio enters / leaves TX.
            Event::Status(status) => {
                log_noise_event(&Event::Status(status));
                // Health flags at warn on the RISING edge only — an
                // operator running at info/warn must see a degrading
                // modem before the audio breaks, without a 4 Hz flood.
                let health_now = status.flags.bits() & !0x01 & !0x40;
                let health_prev = self.last_health_bits.unwrap_or(0);
                let rising = health_now & !health_prev;
                if rising != 0 {
                    tracing::warn!(
                        adc_overflow = status.adc_overflow(),
                        rx_overflow = status.rx_overflow(),
                        tx_overflow = status.tx_overflow(),
                        dac_overflow = status.dac_overflow(),
                        lockout = status.lockout(),
                        "MMDVM modem health flag raised"
                    );
                }
                self.last_health_bits = Some(health_now);

                let tx_now = status.tx();
                let edge = self.last_tx_active != Some(tx_now);
                self.last_tx_active = Some(tx_now);
                if edge {
                    Ok(Some(DStarEvent::StatusUpdate(status)))
                } else {
                    Ok(None)
                }
            }
            // Everything else is non-fatal noise — init-handshake
            // artefacts, debug frames, unhandled commands, and
            // `#[non_exhaustive]` variants the mmdvm crate may add
            // in the future.
            other => {
                log_noise_event(&other);
                Ok(None)
            }
        }
    }

    /// Handle a received D-STAR EOT, emitting any queued text message
    /// and driving echo playback if the record phase was active.
    async fn on_eot(&mut self) -> Result<Option<DStarEvent>, Error> {
        // Reset ALL reception state and queue the decoded text BEFORE
        // any await: if the caller's future is cancelled mid-echo-
        // playback, the gateway must not be left claiming an RX that
        // already ended (and the text message must not be lost).
        let text_event = self.take_text_message();
        let was_echo = self.echo_active;
        self.echo_active = false;
        self.rx_active = false;
        self.rx_header = None;
        if let Some(text) = text_event {
            self.pending_events.push_back(DStarEvent::TextMessage(text));
        }
        if was_echo {
            self.play_echo().await?;
        }
        Ok(Some(DStarEvent::VoiceEnd))
    }

    /// Handle a received D-STAR header (internal).
    fn handle_voice_start(&mut self, header: DStarHeader) {
        self.rx_active = true;
        self.slow_data.reset();
        self.slow_data_frame_index = 0;
        self.rx_header = Some(header);

        // Parse URCALL for special commands. Lossy decode: a corrupted
        // header should show its garbled callsign (with replacement
        // chars) rather than silently becoming an empty string.
        let ur_str = String::from_utf8_lossy(header.ur_call.as_bytes());
        let action = UrCallAction::parse(&ur_str);
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
        // Raw passthrough: the radio accepts D-STAR voice payloads in
        // on-wire byte order, the same order reflectors relay. (The
        // "TX and RX are asymmetric" theory this comment used to
        // record was an artifact of the decoder's old MSB-first
        // unpack bug — TX passthrough always worked precisely BECAUSE
        // the radio is wire-order in both directions.)
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
            // Not a status: run it through the normal pipeline so
            // voice frames / EOT / terminal events that arrive while
            // polling are queued for next_event() instead of being
            // discarded (a discarded EOT would leave rx_active stuck
            // and lose the slow-data text).
            if let Some(dstar_event) = self.dispatch_event(evt).await? {
                self.pending_events.push_back(dstar_event);
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
/// `SetConfig` goes out as a raw write, so its ACK is awaited from
/// the event stream; `set_mode` correlates the modem's ACK/NAK into
/// its own return value, so its result is authoritative on its own.
/// `Version` and `Status` events delivered by the modem's startup
/// handshake are accepted silently.
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

    // Send SetMode — resolves with the modem's ACK/NAK directly, so
    // a rejection or a silent modem surfaces here as an error.
    modem
        .set_mode(ModemMode::DStar)
        .await
        .map_err(shell_err_to_thd75_err)?;

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
        if let Some(err) = terminal_event_error(&evt) {
            return Err(err);
        }
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

/// Combine a D-STAR init failure with a failed MMDVM rollback into
/// one reportable error carrying both causes — the double-fault path
/// of [`DStarGateway::start`], where no `Radio` can be returned.
fn double_fault_error(init_err: &Error, exit_err: &Error) -> Error {
    Error::Transport(crate::error::TransportError::Disconnected(
        std::io::Error::other(format!(
            "radio unrecoverable: D-STAR init failed ({init_err}) and \
             MMDVM exit failed ({exit_err}); reconnect from scratch"
        )),
    ))
}

/// Map a terminal [`mmdvm::Event`] — one that means the modem loop is
/// exiting — to the error the gateway surfaces to its caller.
///
/// Returns `None` for every non-terminal event, including
/// [`Event::TxDropped`]: the drop report always precedes the terminal
/// event, so it is logged where it is dispatched and the session error
/// comes from the event that follows.
fn terminal_event_error(event: &Event) -> Option<Error> {
    match event {
        Event::TransportClosed => Some(Error::Transport(
            crate::error::TransportError::Disconnected(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "MMDVM transport closed",
            )),
        )),
        Event::Fatal { message } => Some(Error::Transport(
            crate::error::TransportError::Disconnected(std::io::Error::other(format!(
                "MMDVM modem loop failed: {message}"
            ))),
        )),
        _ => None,
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
        mmdvm::ShellError::ResponseTimeout => {
            Error::Protocol(crate::error::ProtocolError::UnexpectedResponse {
                expected: "MMDVM ACK/NAK".to_owned(),
                actual: b"no response (timeout)".to_vec(),
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
    // Lossy: corrupted bytes surface as replacement characters in the
    // last-heard list — evidence of a bad header, not a blank entry.
    String::from_utf8_lossy(cs.as_bytes()).trim_end().to_owned()
}

/// Trim trailing spaces from a `Suffix` and return an owned `String`.
fn sfx_trim(sfx: dstar_gateway_core::Suffix) -> String {
    String::from_utf8_lossy(sfx.as_bytes())
        .trim_end()
        .to_owned()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::radio::Radio;
    use crate::transport::MockTransport;
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
    // Terminal-event mapping tests
    // -------------------------------------------------------------------

    #[test]
    fn fatal_event_maps_to_transport_error() -> Result<(), Box<dyn std::error::Error>> {
        let event = Event::Fatal {
            message: "transport write timed out".to_owned(),
        };
        let err = terminal_event_error(&event).ok_or("Fatal must be terminal")?;
        assert!(
            matches!(err, Error::Transport(_)),
            "expected a transport error, got {err:?}"
        );
        // The loop's failure message travels in the source chain
        // (Error → TransportError::Disconnected → io::Error).
        let mut source: Option<&dyn std::error::Error> = Some(&err);
        let mut found = false;
        while let Some(e) = source {
            if e.to_string().contains("transport write timed out") {
                found = true;
                break;
            }
            source = e.source();
        }
        assert!(
            found,
            "error chain must carry the modem loop's failure message: {err:?}"
        );
        Ok(())
    }

    #[test]
    fn transport_closed_maps_to_disconnected() -> Result<(), Box<dyn std::error::Error>> {
        let err = terminal_event_error(&Event::TransportClosed)
            .ok_or("TransportClosed must be terminal")?;
        assert!(
            matches!(err, Error::Transport(_)),
            "expected a transport error, got {err:?}"
        );
        Ok(())
    }

    /// Build a fully-started gateway over a scripted [`MockTransport`].
    ///
    /// The whole read script must be pre-queued (the gateway owns the
    /// transport once started): the D-STAR init ACKs are delivered
    /// with wire-latency delays so they arrive after the writes they
    /// answer, and `extra_reads` are appended for the test body.
    /// Must run inside a `tokio::task::LocalSet`.
    async fn started_gateway(
        extra_reads: &[(&[u8], u64)],
    ) -> Result<DStarGateway<MockTransport>, BoxTestErr> {
        let mut mock = MockTransport::new();
        // All writes accepted without wire assertions — the read
        // script below is the sequencing mechanism. (Responses are
        // pre-queued rather than attached to expectations because the
        // ACKs must be interleavable with the MMDVM pump's reads.)
        mock.expect_any_write();
        // The pump reads continuously — pend instead of erroring when
        // the script runs dry.
        mock.pend_when_empty();
        // enter_mmdvm's TN 3,1 response (Bps9600 default for D-STAR).
        mock.queue_read(b"TN 3,1\r");
        // ACK for SetConfig (0x02), then for SetMode (0x03) — the
        // SetMode ACK is delayed so it lands after set_mode's write
        // (its reply is correlated, not scanned from the event log).
        mock.queue_read_delayed(&[0xE0, 4, 0x70, 0x02], 20);
        mock.queue_read_delayed(&[0xE0, 4, 0x70, 0x03], 150);
        for (data, delay) in extra_reads {
            mock.queue_read_delayed(data, *delay);
        }

        let radio = Radio::connect(mock).await?;
        let config = DStarGatewayConfig::new("N0CALL");
        DStarGateway::start(radio, config)
            .await
            .map_err(|(_, e)| -> BoxTestErr { format!("gateway start failed: {e}").into() })
    }

    type BoxTestErr = Box<dyn std::error::Error>;

    #[tokio::test]
    async fn dead_modem_is_error_not_quiet_airtime() -> Result<(), BoxTestErr> {
        tokio::task::LocalSet::new()
            .run_until(async {
                // The transport EOFs shortly after startup (an empty
                // delayed read delivers Ok(0)): the modem loop exits.
                // The FIRST next_event surfaces the terminal event as
                // an error; every LATER call must also error — a dead
                // modem must never read as an idle timeout forever.
                let mut gateway = started_gateway(&[(&[], 300)]).await?;
                gateway.set_event_timeout(Duration::from_millis(200));

                let mut saw_error = false;
                for _ in 0..20 {
                    if gateway.next_event().await.is_err() {
                        saw_error = true;
                        break;
                    }
                }
                assert!(saw_error, "modem death must surface as an error");

                // And it must KEEP erroring — the channel is closed.
                let after = gateway.next_event().await;
                assert!(
                    after.is_err(),
                    "a dead modem must not read as quiet airtime: {after:?}"
                );
                Ok(())
            })
            .await
    }

    #[tokio::test]
    async fn poll_status_queues_voice_events_instead_of_discarding() -> Result<(), BoxTestErr> {
        tokio::task::LocalSet::new()
            .run_until(async {
                // A D-STAR header arrives while poll_status is
                // waiting for its status frame: the header must be
                // queued for next_event(), not thrown away.
                let header_frame = {
                    let mut f = vec![0xE0u8, 44, 0x10];
                    f.extend_from_slice(&[0x20u8; 41]);
                    f
                };
                let status_frame: &[u8] = &[0xE0, 15, 0x01, 1, 0, 0, 10, 0, 0, 0, 0, 0, 0, 0, 0];
                let mut gateway =
                    started_gateway(&[(header_frame.as_slice(), 300), (status_frame, 350)]).await?;
                gateway.set_event_timeout(Duration::from_millis(200));

                // Wait past the queued reads, then poll.
                tokio::time::sleep(Duration::from_millis(250)).await;
                let status = gateway.poll_status().await?;
                assert_eq!(status.dstar_space, 10);

                let event = gateway.next_event().await?;
                assert!(
                    matches!(event, Some(DStarEvent::VoiceStart(_))),
                    "the header drained during poll_status must surface: {event:?}"
                );
                Ok(())
            })
            .await
    }

    #[test]
    fn double_fault_error_carries_both_causes() {
        // When D-STAR init fails AND the MMDVM rollback also fails,
        // the process must not abort — the combined error carries
        // both causes for the operator.
        let init = Error::Timeout(Duration::from_secs(2));
        let exit = Error::RadioError;
        let err = double_fault_error(&init, &exit);
        let mut chain_text = String::new();
        let mut source: Option<&dyn std::error::Error> = Some(&err);
        while let Some(e) = source {
            chain_text.push_str(&e.to_string());
            source = e.source();
        }
        assert!(
            chain_text.contains("timed out") && chain_text.contains("error response"),
            "both causes must be reported: {chain_text}"
        );
    }

    #[test]
    fn non_terminal_events_map_to_none() {
        assert!(terminal_event_error(&Event::DStarEot).is_none());
        assert!(
            terminal_event_error(&Event::TxDropped { frames: 3 }).is_none(),
            "TxDropped is reported, not terminal — the terminal event follows it"
        );
        assert!(
            terminal_event_error(&Event::ProtocolViolation {
                command: 0x01,
                detail: String::new(),
            })
            .is_none()
        );
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
