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
//! [Radio] <--MMDVM BT/USB--> [DstarGateway] <--user code--> [Reflector UDP]
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
//! The [`DstarGateway`] owns an [`::mmdvm::AsyncModem`] via an
//! [`MmdvmSession`]. The [`mmdvm`] crate's async shell handles MMDVM
//! framing, periodic `GetStatus` polling, and TX-buffer slot gating
//! in a spawned task; the gateway consumes the [`::mmdvm::Event`]
//! stream, translates it into [`DstarEvent`]s, and forwards TX frames
//! through the handle's `send_dstar_*` methods.
//!
//! Create a gateway with [`DstarGateway::start`], which enters MMDVM
//! mode and initializes D-STAR, and tear it down with
//! [`DstarGateway::stop`], which exits MMDVM mode and returns the
//! [`Radio`] for other use.
//!
//! # Example
//!
//! ```no_run
//! use kenwood_thd75::types::DstarCallsign;
//! use kenwood_thd75::{DstarGateway, DstarGatewayConfig, Radio};
//! use kenwood_thd75::transport::SerialTransport;
//!
//! # async fn example() -> Result<(), Box<dyn std::error::Error>> {
//! let transport = SerialTransport::open("/dev/cu.usbmodem1234")?;
//! let radio = Radio::new(transport);
//!
//! let config = DstarGatewayConfig::new(DstarCallsign::new("N0CALL")?);
//! let mut gw = DstarGateway::start(radio, config).await.map_err(|(_, e)| e)?;
//!
//! for _ in 0..10 {
//!     let Some(event) = gw.next_event().await? else {
//!         // Quiet poll interval. Do other work, then continue polling.
//!         continue;
//!     };
//!     match event {
//!         kenwood_thd75::DstarEvent::VoiceStart(header) => {
//!             println!("TX from {} to {}", header.my_call, header.ur_call);
//!             // Forward header to reflector...
//!         }
//!         kenwood_thd75::DstarEvent::VoiceData(frame) => {
//!             let _ = frame; // Forward AMBE + slow data to reflector...
//!         }
//!         kenwood_thd75::DstarEvent::VoiceEnd => {
//!             // Send EOT to reflector...
//!         }
//!         kenwood_thd75::DstarEvent::TextMessage(text) => {
//!             println!("Slow data message bytes: {:?}", text.as_bytes());
//!         }
//!         kenwood_thd75::DstarEvent::StationHeard(entry) => {
//!             println!("Heard callsign bytes: {:?}", entry.callsign.as_bytes());
//!         }
//!         _ => {}
//!     }
//! }
//!
//! let radio = gw
//!     .stop()
//!     .await?
//!     .restore()
//!     .await
//!     .map_err(|(_desynced, e)| e)?;
//! # Ok(())
//! # }
//! ```

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use ::mmdvm::{AsyncModem, Event};
use dstar_gateway_core::{
    Callsign, DstarHeader, Module, ReflectorCallsign, SlowDataTextCollector, SlowDataTextMessage,
    Suffix, TypeError, VoiceFrame, WireTextError,
};
use mmdvm_core::{MMDVM_SET_CONFIG, ModemMode, ModemStatus};

use crate::error::Error;
use crate::radio::mmdvm_session::{MmdvmRadioRestore, MmdvmSession};
use crate::radio::{DesyncedRadio, Radio};
use crate::transport::{MmdvmTransportAdapter, Transport};
use crate::types::dstar::UrCallAction;
use crate::types::{DstarCallsign, DstarSuffix, PacketDataRate};

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

/// Configuration for a [`DstarGateway`] session.
///
/// Created with [`DstarGatewayConfig::new`] which provides sensible
/// defaults for a D-STAR gateway station. Its public identity fields use
/// validated types, so an invalid callsign or suffix cannot reach the wire.
/// All fields are public for customisation before passing to
/// [`DstarGateway::start`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DstarGatewayConfig {
    /// Validated MY callsign, space-padded when encoded on the wire.
    pub callsign: DstarCallsign,
    /// Validated MY suffix, space-padded when encoded on the wire.
    /// Default: empty.
    pub suffix: DstarSuffix,
    /// Packet data rate for MMDVM mode. Default: 9600 bps (GMSK, the
    /// standard D-STAR data rate).
    pub data_rate: PacketDataRate,
    /// Maximum last-heard entries to keep. Oldest entries are evicted
    /// when this limit is reached. Default: 100.
    pub max_last_heard: usize,
}

impl DstarGatewayConfig {
    /// Create a new configuration with sensible defaults.
    ///
    /// - Suffix: empty (encoded as four spaces)
    /// - Baud: 9600 bps (GMSK, standard for D-STAR voice)
    /// - Max last-heard: 100 entries
    #[must_use]
    pub fn new(callsign: DstarCallsign) -> Self {
        Self {
            callsign,
            suffix: DstarSuffix::default(),
            data_rate: PacketDataRate::Bps9600,
            max_last_heard: DEFAULT_MAX_LAST_HEARD,
        }
    }
}

/// A reflector destination that fits a D-STAR status header.
///
/// A status header places the reflector name in the first seven RPT bytes and
/// its module in the eighth. [`ReflectorCallsign`] alone permits eight-byte
/// names, so this gateway-specific type validates the narrower wire invariant
/// before any transmission begins.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DstarStatusReflector {
    name: ReflectorCallsign,
    module: Module,
}

impl DstarStatusReflector {
    /// Maximum reflector-name width in a status header.
    pub const MAX_NAME_LEN: usize = 7;

    /// Validate a reflector name and module for status-header transmission.
    ///
    /// # Errors
    ///
    /// Returns [`DstarStatusReflectorError::InvalidValue`] when `name` is not
    /// a recognized reflector callsign or `module` is not an uppercase ASCII
    /// letter. Returns [`DstarStatusReflectorError::NameTooLong`] when the
    /// validated name occupies the module byte.
    pub fn new(name: &str, module: char) -> Result<Self, DstarStatusReflectorError> {
        let name = ReflectorCallsign::try_from_str(name)?;
        let module = Module::try_from_char(module)?;
        let name_len = name
            .callsign()
            .as_bytes()
            .iter()
            .rposition(|byte| *byte != b' ')
            .map_or(0, |position| position + 1);
        if name_len > Self::MAX_NAME_LEN {
            return Err(DstarStatusReflectorError::NameTooLong {
                len: name_len,
                max: Self::MAX_NAME_LEN,
            });
        }
        Ok(Self { name, module })
    }

    /// Validated reflector callsign.
    #[must_use]
    pub const fn name(&self) -> &ReflectorCallsign {
        &self.name
    }

    /// Validated reflector module.
    #[must_use]
    pub const fn module(&self) -> Module {
        self.module
    }

    /// Encode the reflector name and module as one eight-byte RPT field.
    #[must_use]
    const fn to_wire_bytes(self) -> [u8; 8] {
        let [
            first,
            second,
            third,
            fourth,
            fifth,
            sixth,
            seventh,
            _padding,
        ] = *self.name.callsign().as_bytes();
        [
            first,
            second,
            third,
            fourth,
            fifth,
            sixth,
            seventh,
            self.module.as_byte(),
        ]
    }
}

/// Why a reflector cannot be represented in a D-STAR status header.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum DstarStatusReflectorError {
    /// The reflector callsign or module is invalid.
    #[error(transparent)]
    InvalidValue(#[from] TypeError),
    /// The name consumes the eighth byte reserved for the module.
    #[error("reflector name is {len} bytes; status headers allow at most {max}")]
    NameTooLong {
        /// Supplied reflector-name length.
        len: usize,
        /// Maximum status-header reflector-name length.
        max: usize,
    },
}

// ---------------------------------------------------------------------------
// Reconnection backoff
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Last heard
// ---------------------------------------------------------------------------

/// Exact callsign bytes observed in an inbound D-STAR header.
///
/// Unlike [`DstarCallsign`], which validates text before configuration and
/// transmission, this receive-boundary type preserves all eight wire bytes.
/// Use [`Self::text`] when semantic text is required and [`Self::as_bytes`]
/// when inspecting a malformed or unidentified field.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct ObservedDstarCallsign(Callsign);

impl ObservedDstarCallsign {
    /// Return the exact eight bytes received from the radio.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 8] {
        self.0.as_bytes()
    }

    /// Return validated text without trailing padding.
    ///
    /// # Errors
    ///
    /// Returns [`WireTextError`] when an observed byte is not printable ASCII.
    pub fn text(&self) -> Result<&str, WireTextError> {
        self.0.text()
    }
}

impl From<Callsign> for ObservedDstarCallsign {
    fn from(callsign: Callsign) -> Self {
        Self(callsign)
    }
}

impl std::fmt::Display for ObservedDstarCallsign {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(formatter)
    }
}

impl std::fmt::Debug for ObservedDstarCallsign {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_tuple("ObservedDstarCallsign")
            .field(&self.0)
            .finish()
    }
}

/// Entry in the last-heard list.
///
/// Tracks the most recent transmission heard from each unique callsign.
/// Updated each time a D-STAR header is received from the radio.
#[derive(Debug, Clone)]
pub struct LastHeardEntry {
    /// Exact origin callsign (MY field), including its fixed-width padding.
    pub callsign: ObservedDstarCallsign,
    /// Exact origin suffix (MY suffix field), including its padding.
    pub suffix: Suffix,
    /// Exact destination callsign (UR field).
    pub destination: ObservedDstarCallsign,
    /// Exact repeater 1 callsign.
    pub repeater1: ObservedDstarCallsign,
    /// Exact repeater 2 callsign.
    pub repeater2: ObservedDstarCallsign,
    /// When this station was last heard.
    pub timestamp: Instant,
}

impl LastHeardEntry {
    /// How long ago this station was heard, as of `now`.
    ///
    /// The caller supplies the clock reading (typically
    /// `Instant::now()`), keeping display code testable. Saturates to
    /// zero if `now` precedes the entry's own timestamp, so render
    /// paths never panic on clock skew.
    #[must_use]
    pub fn age(&self, now: Instant) -> Duration {
        now.saturating_duration_since(self.timestamp)
    }
}

/// A D-STAR receive frame that violated the gateway stream state.
///
/// These violations are non-fatal: they are surfaced as typed events so a
/// long-running gateway can retain the exact offending data, report the
/// discontinuity, and continue waiting for the next valid header.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum DstarProtocolViolation {
    /// Voice data arrived without a preceding active D-STAR header.
    #[error("D-STAR voice data arrived without an active header")]
    VoiceDataWithoutHeader {
        /// The complete rejected voice frame.
        frame: VoiceFrame,
    },
    /// End-of-transmission arrived without a preceding active header.
    #[error("D-STAR end-of-transmission arrived without an active header")]
    EndOfTransmissionWithoutHeader,
    /// Signal-loss arrived without a preceding active header.
    #[error("D-STAR signal-loss arrived without an active header")]
    SignalLostWithoutHeader,
}

// ---------------------------------------------------------------------------
// Event enum
// ---------------------------------------------------------------------------

/// An event produced by [`DstarGateway::next_event`].
///
/// Each variant represents a distinct category of D-STAR gateway
/// activity. The gateway translates raw MMDVM responses into these
/// typed events so callers never need to parse wire data.
#[derive(Debug)]
pub enum DstarEvent {
    /// A voice transmission started (header received from radio).
    VoiceStart(DstarHeader),
    /// A voice data frame received from the radio.
    VoiceData(VoiceFrame),
    /// Voice transmission ended cleanly (EOT received).
    VoiceEnd,
    /// Voice transmission lost (no clean EOT, signal lost).
    VoiceLost,
    /// The modem's bounded event ring overwrote events before this
    /// consumer could receive them.
    EventsDropped {
        /// Exact number of missing raw modem events.
        count: u64,
    },
    /// A receive frame violated the D-STAR stream state.
    ProtocolViolation(DstarProtocolViolation),
    /// A slow data text message was decoded from the voice stream.
    TextMessage(SlowDataTextMessage),
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
    /// A non-terminal modem event that is not translated into a higher-level
    /// D-STAR event.
    ///
    /// This preserves diagnostic and not-yet-modeled data such as dropped TX
    /// frames, malformed modem responses, transparent/serial payloads, and
    /// future [`::mmdvm::Event`] variants. Callers can inspect the exact event
    /// instead of mistaking it for quiet airtime.
    ModemEvent(Event),
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
pub struct DstarGateway<T: Transport + Unpin + 'static> {
    /// The underlying MMDVM async modem.
    modem: AsyncModem<MmdvmTransportAdapter<T>>,
    /// Radio-state restore envelope used on [`Self::stop`].
    restore: MmdvmRadioRestore<T>,
    /// Gateway configuration.
    config: DstarGatewayConfig,
    /// Slow data decoder for the current RX stream.
    slow_data: SlowDataTextCollector,
    /// Frame counter for slow data decoding within a transmission.
    slow_data_frame_index: u8,
    /// Last-heard station list, newest first.
    last_heard: Vec<LastHeardEntry>,
    /// Whether a voice transmission is currently active (RX from radio).
    rx_active: bool,
    /// The D-STAR header for the currently active RX transmission.
    rx_header: Option<DstarHeader>,
    /// Buffered events to emit on the next `next_event` call.
    pending_events: VecDeque<DstarEvent>,
    /// Echo recording buffer (header + voice frames).
    echo_header: Option<DstarHeader>,
    /// Echo recorded voice frames.
    echo_frames: Vec<VoiceFrame>,
    /// Whether echo recording is active.
    echo_active: bool,
    /// Per-event poll timeout (configurable via [`Self::set_event_timeout`]).
    event_timeout: Duration,
    /// Last observed TX state from the modem's status responses. Used
    /// to emit a `StatusUpdate` event only on rising / falling edges,
    /// which keeps the event channel from being flooded with the modem's
    /// 4 Hz status stream while still surfacing the moment the radio
    /// keys (`tx() = true`) or stops transmitting (`tx() = false`).
    /// `None` until the first status response arrives.
    last_tx_active: Option<bool>,
    /// Previous status health bits (overflow/lockout, TX and CD bits
    /// masked out) for rising-edge warn logging.
    last_health_bits: Option<u8>,
}

impl<T: Transport + Unpin + 'static> std::fmt::Debug for DstarGateway<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DstarGateway")
            .field("config", &self.config)
            .field("rx_active", &self.rx_active)
            .field("last_heard_count", &self.last_heard.len())
            .finish_non_exhaustive()
    }
}

impl<T: Transport + Unpin + 'static> DstarGateway<T> {
    /// Start the D-STAR gateway.
    ///
    /// Enters MMDVM mode on the radio, initializes the modem for D-STAR
    /// operation, and returns a ready-to-use gateway. Consumes the
    /// [`Radio`] --- call [`stop`](Self::stop) to exit and reclaim it.
    ///
    /// # Errors
    ///
    /// On failure, returns the [`Radio`] alongside the error when its
    /// transport could be recovered. A returned radio may require
    /// [`Radio::restore_cat_after_mode_exit`] before CAT commands are safe
    /// because failed entry and rollback paths can leave binary bytes on the
    /// transport. The `Radio` is `None` only when D-STAR init failed AND the
    /// MMDVM rollback also failed (e.g. the USB cable was pulled); the
    /// transport is gone and the caller must reconnect from scratch.
    pub async fn start(
        radio: Radio<T>,
        config: DstarGatewayConfig,
    ) -> Result<Self, (Option<Radio<T>>, Error)> {
        let session = match radio.enter_mmdvm(config.data_rate).await {
            Ok(s) => s,
            Err((radio, e)) => return Err((Some(radio), e)),
        };

        match Self::build_from_session(session, config).await {
            Ok(gateway) => Ok(gateway),
            Err((restore, modem, init_err)) => {
                // Init failed; roll back MMDVM mode to recover the Radio.
                match restore.exit_and_rebuild(modem).await {
                    // The caller receives the init error; the radio
                    // still tracks its own recovery obligation.
                    Ok(desynced) => Err((Some(desynced.into_radio_unproven()), init_err)),
                    Err(exit_err) => {
                        // Both init AND rollback failed (one USB
                        // unplug does it). No Radio can be returned
                        // (the transport is gone), but a long-running
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
    /// already speaks MMDVM binary: no `TN` command is sent.
    ///
    /// # Errors
    ///
    /// On failure, returns the [`Radio`] alongside the error when the same
    /// binary link could be reclaimed cleanly. That radio remains proved to
    /// speak MMDVM and may be passed to this method again. The `Radio` is
    /// `None` only when modem shutdown or transport recovery also failed; the
    /// caller must reconnect and diagnose the replacement link from scratch.
    pub async fn start_gateway_mode(
        radio: Radio<T>,
        config: DstarGatewayConfig,
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
                    Err(reclaim_error) => {
                        tracing::error!(
                            init_error = %init_error,
                            reclaim_error = %reclaim_error,
                            "persistent MMDVM link reclaim failed after D-STAR init failure"
                        );
                        Err((
                            None,
                            binary_reclaim_fault_error(&init_error, &reclaim_error),
                        ))
                    }
                }
            }
        }
    }

    /// Build a gateway from an already-prepared [`MmdvmSession`].
    ///
    /// Runs the D-STAR init handshake (`SetConfig` + `SetMode`) and,
    /// on success, returns the fully-initialised gateway. On failure,
    /// returns the `(restore, modem, error)` triple so the caller can
    /// clean up the MMDVM session before surfacing the error.
    async fn build_from_session(
        session: MmdvmSession<T>,
        config: DstarGatewayConfig,
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

    /// Stop the gateway, exiting MMDVM mode.
    ///
    /// Unread MMDVM frames may remain on the transport, so the radio
    /// comes back wrapped in [`DesyncedRadio`]: call
    /// [`DesyncedRadio::restore`] before using CAT again or reporting
    /// that CAT mode has been restored.
    ///
    /// # Errors
    ///
    /// Returns an error if the MMDVM exit command fails.
    pub async fn stop(self) -> Result<DesyncedRadio<T>, Error> {
        self.restore.exit_and_rebuild(self.modem).await
    }

    /// Process pending I/O and return the next event.
    ///
    /// Each call waits up to [`Self::set_event_timeout`] for a new MMDVM
    /// event from the modem loop, translates it into a [`DstarEvent`],
    /// and returns. `Ok(None)` is a quiet poll interval, not end-of-stream;
    /// callers should do other work and poll again. Periodic status frames are
    /// consumed without being returned when their transmitter state has not
    /// changed.
    ///
    /// # Errors
    ///
    /// Only returns errors if the underlying transport fails fatally.
    /// Malformed frames are swallowed by the [`mmdvm`] crate's RX loop
    /// as debug diagnostics: propagating a decode error would kill
    /// the whole session on a single malformed byte.
    pub async fn next_event(&mut self) -> Result<Option<DstarEvent>, Error> {
        // Drain buffered events first (e.g. UrCallCommand after VoiceStart).
        if let Some(evt) = self.pending_events.pop_front() {
            return Ok(Some(evt));
        }

        // Steady Status events arrive at 4 Hz and are intentionally
        // coalesced by `dispatch_event`, surfacing as `Ok(None)` unless
        // the transmitter state changes. Callers' typical drain loop is
        // `while let Ok(Some(e)) = gw.next_event().await { ... }`,
        // which would BREAK on the first noise event, leaving the
        // remaining noise in the mmdvm event channel. During an
        // active D-STAR voice stream the REPL spends most of its
        // time in the reflector-event branch of `dstar_poll_cycle`,
        // producing only ~one radio-drain pass per cycle; if that
        // pass swallows a single Status and then breaks, noise
        // accumulates faster than it's consumed and the mmdvm event
        // ring wraps. The modem loop never blocks on a slow consumer;
        // it reports the exact overwrite count as EventsDropped, so a
        // lazy drain here creates explicit stream discontinuities.
        //
        // Loop internally past coalesced status within the caller's time
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
                    // already consumed; this closed channel is the
                    // only remaining signal, and a dead modem must
                    // never read as quiet airtime.
                    return Err(Error::Transport(
                        crate::error::TransportError::Disconnected(std::io::Error::new(
                            std::io::ErrorKind::UnexpectedEof,
                            "MMDVM modem loop exited",
                        )),
                    ));
                }
                // Idle poll timeout: genuinely no event this cycle.
                Err(_elapsed) => return Ok(None),
            };
            if let Some(evt) = self.dispatch_event(raw).await? {
                return Ok(Some(evt));
            }
            // `dispatch_event` returned `Ok(None)`: an unchanged periodic
            // status was consumed. Keep pulling from the MMDVM channel
            // within the same deadline so status polling cannot short-circuit
            // the caller's drain loop.
        }
    }

    /// Dispatch a raw [`::mmdvm::Event`] into a [`DstarEvent`].
    async fn dispatch_event(&mut self, raw: Event) -> Result<Option<DstarEvent>, Error> {
        // Terminal events (transport gone, loop dying) become session
        // errors; a dead modem must never read as quiet airtime.
        if let Some(err) = terminal_event_error(&raw) {
            return Err(err);
        }
        match raw {
            Event::DstarHeaderRx { bytes } => {
                let header = DstarHeader::decode(&bytes);
                let preempted = self.rx_active;
                let followup_start = self.pending_events.len();
                self.handle_voice_start(header);
                if preempted {
                    // A second header is an observed stream boundary, not
                    // permission to silently replace the active stream.
                    // `handle_voice_start` has already reset the old stream
                    // and queued the new stream's secondary events; insert
                    // its VoiceStart before those secondaries.
                    self.pending_events
                        .insert(followup_start, DstarEvent::VoiceStart(header));
                    Ok(Some(DstarEvent::VoiceLost))
                } else {
                    Ok(Some(DstarEvent::VoiceStart(header)))
                }
            }
            Event::DstarDataRx { bytes } => {
                // The radio's MMDVM firmware delivers D-STAR voice
                // payloads in on-wire byte order: the same LSB-first
                // convention reflectors relay and mbelib-rs reads
                // natively (since 2026-07-04). A historical per-byte
                // bit reversal here was compensating for the decoder's
                // then-wrong MSB-first unpack; with the decoder fixed,
                // the bytes pass through untouched, matching the TX
                // path (which was always raw passthrough).
                let [a0, a1, a2, a3, a4, a5, a6, a7, a8, s0, s1, s2] = bytes;
                let frame = VoiceFrame {
                    ambe: [a0, a1, a2, a3, a4, a5, a6, a7, a8],
                    slow_data: [s0, s1, s2],
                };
                if !self.rx_active {
                    return Ok(Some(DstarEvent::ProtocolViolation(
                        DstarProtocolViolation::VoiceDataWithoutHeader { frame },
                    )));
                }
                self.handle_voice_data(frame);
                Ok(Some(DstarEvent::VoiceData(frame)))
            }
            Event::DstarEot if self.rx_active => self.on_eot().await,
            Event::DstarEot => Ok(Some(DstarEvent::ProtocolViolation(
                DstarProtocolViolation::EndOfTransmissionWithoutHeader,
            ))),
            Event::DstarLost => {
                let interrupted = self.rx_active;
                self.reset_receive_state();
                if interrupted {
                    Ok(Some(DstarEvent::VoiceLost))
                } else {
                    Ok(Some(DstarEvent::ProtocolViolation(
                        DstarProtocolViolation::SignalLostWithoutHeader,
                    )))
                }
            }
            Event::EventsDropped { count } => {
                let interrupted = self.rx_active;
                tracing::warn!(
                    target: "kenwood_thd75::dstar_gateway::gateway",
                    count,
                    interrupted,
                    "MMDVM event stream discontinuity"
                );
                self.reset_receive_state();
                if interrupted {
                    self.pending_events.push_back(DstarEvent::VoiceLost);
                }
                Ok(Some(DstarEvent::EventsDropped { count }))
            }
            // Queued TX frames were discarded because the modem
            // session is ending; the operator's last over was
            // truncated on air even though every send reported
            // success. The terminal event follows immediately;
            // this is the audit trail for what it took with it.
            event @ Event::TxDropped { frames } => {
                tracing::warn!(
                    target: "kenwood_thd75::dstar_gateway::gateway",
                    frames,
                    "MMDVM session discarded queued TX frames; transmission truncated"
                );
                Ok(Some(DstarEvent::ModemEvent(event)))
            }
            // The radio sent a frame violating the MMDVM layout for
            // its command byte. Non-fatal, but a rising count means a
            // degrading link (or a firmware quirk worth capturing).
            Event::ProtocolViolation { command, detail } => {
                tracing::warn!(
                    target: "kenwood_thd75::dstar_gateway::gateway",
                    command = format!("0x{command:02X}"),
                    detail = %detail,
                    "MMDVM protocol violation from radio"
                );
                Ok(Some(DstarEvent::ModemEvent(Event::ProtocolViolation {
                    command,
                    detail,
                })))
            }
            // Status events are 4 Hz noise, but the TX flag inside
            // them is the single most useful diagnostic for the
            // network → radio voice path: did the radio actually key
            // the transmitter after we sent it a header + voice
            // frames? We swallow the steady stream as before, but
            // surface a `StatusUpdate` event whenever the TX flag
            // *changes* state. That keeps the channel from flooding
            // while still telling the operator (and any UI) the
            // exact moment the radio enters / leaves TX.
            Event::Status(status) => {
                let event = self.handle_status(status);
                Ok(event)
            }
            // Preserve everything else exactly. The raw enum is
            // `#[non_exhaustive]`, so this also carries future modem events
            // without converting them into an ambiguous `Ok(None)`.
            other => {
                log_noise_event(&other);
                Ok(Some(DstarEvent::ModemEvent(other)))
            }
        }
    }

    /// Observe one periodic modem status without flooding the public event
    /// stream, returning an event only when the transmitter changes state.
    fn handle_status(&mut self, status: ModemStatus) -> Option<DstarEvent> {
        log_noise_event(&Event::Status(status));

        // Health flags log at warn on their rising edge. This surfaces a
        // degrading modem without repeating the warning at the 4 Hz poll rate.
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
        let changed = self.last_tx_active != Some(tx_now);
        self.last_tx_active = Some(tx_now);
        changed.then_some(DstarEvent::StatusUpdate(status))
    }

    /// Handle a received D-STAR EOT, emitting any queued text message
    /// and driving echo playback if the record phase was active.
    async fn on_eot(&mut self) -> Result<Option<DstarEvent>, Error> {
        // Reset ALL reception state and queue the decoded text BEFORE
        // any await: if the caller's future is cancelled mid-echo-
        // playback, the gateway must not be left claiming an RX that
        // already ended (and the text message must not be lost).
        let text_event = self.take_text_message();
        let echo_recording = if self.echo_active {
            self.echo_header
                .take()
                .map(|header| (header, std::mem::take(&mut self.echo_frames)))
        } else {
            None
        };
        self.reset_receive_state();
        if let Some(text) = text_event {
            self.pending_events.push_back(DstarEvent::TextMessage(text));
        }
        if let Some((header, frames)) = echo_recording {
            self.play_echo(header, frames).await?;
        }
        Ok(Some(DstarEvent::VoiceEnd))
    }

    /// Handle a received D-STAR header (internal).
    fn handle_voice_start(&mut self, header: DstarHeader) {
        self.reset_receive_state();
        self.rx_active = true;
        self.rx_header = Some(header);

        // Update last-heard before command-specific handling. The
        // VoiceStart is returned directly by `dispatch_event`; this
        // queued StationHeard therefore follows it deterministically.
        let entry = LastHeardEntry {
            callsign: header.my_call.into(),
            suffix: header.my_suffix,
            destination: header.ur_call.into(),
            repeater1: header.rpt1.into(),
            repeater2: header.rpt2.into(),
            timestamp: Instant::now(),
        };
        self.update_last_heard(entry.clone());
        self.pending_events
            .push_back(DstarEvent::StationHeard(entry));

        // Classify special commands directly from the lossless receive
        // callsign. Opaque wire bytes remain intact in the destination
        // variant and can never be fabricated into a command by UTF-8
        // replacement, truncation, or padding.
        let action = UrCallAction::classify(header.ur_call);
        match &action {
            UrCallAction::Cq | UrCallAction::Callsign(_) => {}
            UrCallAction::Echo => {
                self.echo_active = true;
                self.echo_header = Some(header);
                self.echo_frames.clear();
            }
            _ => {
                self.pending_events
                    .push_back(DstarEvent::UrCallCommand(action));
            }
        }
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
    pub async fn send_header(&mut self, header: &DstarHeader) -> Result<(), Error> {
        let encoded = header.encode();
        self.modem
            .send_dstar_header(encoded)
            .await
            .map_err(shell_err_to_thd75_err)
    }

    /// Send a D-STAR voice data frame to the radio for transmission.
    ///
    /// Enqueues the frame in the mmdvm TX queue. Pacing is handled
    /// inside the mmdvm modem loop; no host-side sleep is introduced
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
        // unpack bug; TX passthrough always worked precisely BECAUSE
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
        reflector: Option<DstarStatusReflector>,
    ) -> Result<(), Error> {
        let rpt_bytes = reflector.map_or(*b"DIRECT  ", DstarStatusReflector::to_wire_bytes);

        let header = DstarHeader {
            flag1: 0x00,
            flag2: 0x00,
            flag3: 0x00,
            rpt2: Callsign::from_wire_bytes(rpt_bytes),
            rpt1: Callsign::from_wire_bytes(rpt_bytes),
            ur_call: Callsign::from_wire_bytes(*b"CQCQCQ  "),
            my_call: (&self.config.callsign).into(),
            my_suffix: (&self.config.suffix).into(),
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
            let insertion_index = self.pending_events.len();
            if let Some(dstar_event) = self.dispatch_event(evt).await? {
                // `dispatch_event` can queue secondary events (for
                // example StationHeard after VoiceStart). Preserve the
                // same primary-before-secondary ordering used by
                // `next_event`, while leaving older queued events first.
                self.pending_events.insert(insertion_index, dstar_event);
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
    pub const fn current_header(&self) -> Option<&DstarHeader> {
        self.rx_header.as_ref()
    }

    /// Get the current configuration.
    #[must_use]
    pub const fn config(&self) -> &DstarGatewayConfig {
        &self.config
    }

    // -----------------------------------------------------------------------
    // Internal helpers
    // -----------------------------------------------------------------------

    /// Clear every piece of state scoped to the current receive stream.
    ///
    /// Lost markers, event-ring discontinuities, and a preempting header all
    /// use the same reset so slow data or echo audio can never leak into the
    /// next transmission.
    fn reset_receive_state(&mut self) {
        self.rx_active = false;
        self.rx_header = None;
        self.slow_data.reset();
        self.slow_data_frame_index = 0;
        self.echo_active = false;
        self.echo_header = None;
        self.echo_frames.clear();
    }

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
    async fn play_echo(
        &mut self,
        orig_header: DstarHeader,
        frames: Vec<VoiceFrame>,
    ) -> Result<(), Error> {
        if frames.is_empty() {
            return Ok(());
        }

        // A D-STAR gateway route uses the final RPT2 byte as the fixed
        // gateway designator, independently of the MY callsign field.
        let mut rpt2_bytes = self.config.callsign.to_wire_bytes();
        if let Some(b) = rpt2_bytes.get_mut(7) {
            *b = b'G';
        }

        let echo_header = DstarHeader {
            flag1: orig_header.flag1,
            flag2: orig_header.flag2,
            flag3: orig_header.flag3,
            rpt2: Callsign::from_wire_bytes(rpt2_bytes),
            rpt1: orig_header.rpt1,
            ur_call: orig_header.my_call,
            my_call: (&self.config.callsign).into(),
            my_suffix: (&self.config.suffix).into(),
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
    fn take_text_message(&mut self) -> Option<SlowDataTextMessage> {
        self.slow_data.take_message()
    }
}

/// Initialise the MMDVM modem for D-STAR: send `SetConfig` with
/// D-STAR-only flags, then `SetMode(Dstar)`.
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
        ModemMode::Dstar.as_byte(),
        DEFAULT_RX_LEVEL,
        DEFAULT_TX_LEVEL,
    ];
    modem
        .send_raw(MMDVM_SET_CONFIG, config_payload)
        .await
        .map_err(shell_err_to_thd75_err)?;
    await_ack(modem, MMDVM_SET_CONFIG).await?;

    // Send SetMode. It resolves with the modem's ACK/NAK directly, so
    // a rejection or a silent modem surfaces here as an error.
    modem
        .set_mode(ModemMode::Dstar)
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
            Event::DstarHeaderRx { .. }
            | Event::DstarDataRx { .. }
            | Event::DstarLost
            | Event::DstarEot
            | Event::SerialData(_)
            | Event::TransparentData(_)
            | Event::UnhandledResponse { .. } => {
                tracing::debug!("unexpected MMDVM event during init; ignoring");
            }
            // `::mmdvm::Event` is marked `#[non_exhaustive]`, so new
            // variants are added without a major version bump. Treat
            // unknown events as "keep waiting for the ACK".
            _ => {
                tracing::debug!("unrecognised MMDVM event during init; ignoring");
            }
        }
    }
}

/// Combine a D-STAR init failure with a failed MMDVM rollback into
/// one reportable error carrying both causes: the double-fault path
/// of [`DstarGateway::start`], where no `Radio` can be returned.
fn double_fault_error(init_err: &Error, exit_err: &Error) -> Error {
    Error::Transport(crate::error::TransportError::Disconnected(
        std::io::Error::other(format!(
            "radio unrecoverable: D-STAR init failed ({init_err}) and \
             MMDVM exit failed ({exit_err}); reconnect from scratch"
        )),
    ))
}

/// Combine a persistent-mode D-STAR init failure with failure to reclaim the
/// still-binary transport. The old binary proof cannot be carried across a
/// reconnect, so no [`Radio`] is returned on this path.
fn binary_reclaim_fault_error(init_err: &Error, reclaim_err: &Error) -> Error {
    Error::Transport(crate::error::TransportError::Disconnected(
        std::io::Error::other(format!(
            "radio unrecoverable: D-STAR init failed ({init_err}) and the persistent MMDVM \
             link could not be reclaimed ({reclaim_err}); reconnect and diagnose from scratch"
        )),
    ))
}

/// Map a terminal [`::mmdvm::Event`], one that means the modem loop is
/// exiting, to the error the gateway surfaces to its caller.
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

/// Translate an [`::mmdvm::ShellError`] into a thd75 [`Error`].
fn shell_err_to_thd75_err(err: ::mmdvm::ShellError) -> Error {
    match err {
        ::mmdvm::ShellError::SessionClosed => {
            Error::Transport(crate::error::TransportError::Disconnected(
                std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "MMDVM session closed"),
            ))
        }
        ::mmdvm::ShellError::Core(e) => Error::Protocol(crate::error::ProtocolError::FieldParse {
            command: "MMDVM".to_owned(),
            field: "frame".to_owned(),
            detail: format!("{e}"),
        }),
        ::mmdvm::ShellError::Io(e) => {
            Error::Transport(crate::error::TransportError::Disconnected(e))
        }
        ::mmdvm::ShellError::BufferFull { mode } => {
            Error::Protocol(crate::error::ProtocolError::UnexpectedResponse {
                expected: format!("MMDVM {mode:?} buffer ready"),
                actual: b"buffer full".to_vec(),
            })
        }
        ::mmdvm::ShellError::Nak { command, reason } => {
            Error::Protocol(crate::error::ProtocolError::UnexpectedResponse {
                expected: format!("MMDVM ACK for 0x{command:02X}"),
                actual: format!("NAK: {reason:?}").into_bytes(),
            })
        }
        ::mmdvm::ShellError::ResponseTimeout => {
            Error::Protocol(crate::error::ProtocolError::UnexpectedResponse {
                expected: "MMDVM ACK/NAK".to_owned(),
                actual: b"no response (timeout)".to_vec(),
            })
        }
        // `::mmdvm::ShellError` is `#[non_exhaustive]`. Surface unknown
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
            // trace so operators can audit modem state over time,
            // particularly the `dstar_space` FIFO depth and the
            // overflow / lockout / CD bits that signal trouble.
            tracing::trace!(
                target: "kenwood_thd75::dstar_gateway::gateway",
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
            target: "kenwood_thd75::dstar_gateway::gateway",
            command = format!("0x{command:02X}"),
            "MMDVM ACK (ignored)"
        ),
        Event::Nak { command, reason } => tracing::debug!(
            target: "kenwood_thd75::dstar_gateway::gateway",
            command = format!("0x{command:02X}"),
            ?reason,
            "MMDVM NAK (ignored)"
        ),
        Event::Version(v) => tracing::debug!(
            target: "kenwood_thd75::dstar_gateway::gateway",
            protocol = v.protocol,
            description = %v.description,
            "MMDVM Version (ignored)"
        ),
        Event::Debug { level, text } => tracing::trace!(
            target: "kenwood_thd75::dstar_gateway::gateway",
            level = *level,
            text = %text,
            "MMDVM debug"
        ),
        Event::SerialData(data) => tracing::trace!(
            target: "kenwood_thd75::dstar_gateway::gateway",
            len = data.len(),
            "MMDVM serial data (ignored)"
        ),
        Event::TransparentData(data) => tracing::trace!(
            target: "kenwood_thd75::dstar_gateway::gateway",
            len = data.len(),
            "MMDVM transparent data (ignored)"
        ),
        Event::UnhandledResponse { command, payload } => tracing::debug!(
            target: "kenwood_thd75::dstar_gateway::gateway",
            command = format!("0x{command:02X}"),
            payload_len = payload.len(),
            "MMDVM unhandled response"
        ),
        // Handled variants should never reach this helper; unknown
        // future variants fall through silently.
        _ => tracing::trace!(
            target: "kenwood_thd75::dstar_gateway::gateway",
            "MMDVM unrecognised event"
        ),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::radio::Radio;
    use crate::transport::MockTransport;
    use crate::types::PacketDataRate;

    fn test_config() -> Result<DstarGatewayConfig, BoxTestErr> {
        Ok(DstarGatewayConfig::new(DstarCallsign::new("N0CALL")?))
    }

    fn test_header(my_call: &str, ur_call: [u8; 8]) -> Result<DstarHeader, BoxTestErr> {
        use dstar_gateway_core::{Callsign, Suffix};

        Ok(DstarHeader {
            flag1: 0,
            flag2: 0,
            flag3: 0,
            rpt2: Callsign::try_from_str("DIRECT")?,
            rpt1: Callsign::try_from_str("DIRECT")?,
            ur_call: Callsign::from_wire_bytes(ur_call),
            my_call: Callsign::try_from_str(my_call)?,
            my_suffix: Suffix::from_wire_bytes(*b"    "),
        })
    }

    // -------------------------------------------------------------------
    // Configuration tests
    // -------------------------------------------------------------------

    #[test]
    fn config_defaults() -> Result<(), BoxTestErr> {
        let config = DstarGatewayConfig::new(DstarCallsign::new("W1AW")?);
        assert_eq!(config.callsign.as_str(), "W1AW");
        assert_eq!(config.suffix, DstarSuffix::default());
        assert_eq!(config.data_rate, PacketDataRate::Bps9600);
        assert_eq!(config.max_last_heard, 100);
        Ok(())
    }

    #[test]
    fn config_debug_formatting() -> Result<(), BoxTestErr> {
        let config = test_config()?;
        let debug = format!("{config:?}");
        assert!(debug.contains("N0CALL"), "debug should mention callsign");
        Ok(())
    }

    #[test]
    fn config_identity_converts_to_exact_wire_widths() -> Result<(), BoxTestErr> {
        let mut config = DstarGatewayConfig::new(DstarCallsign::new("W1AW")?);
        config.suffix = DstarSuffix::new("/P")?;

        assert_eq!(config.callsign.to_wire_bytes(), *b"W1AW    ");
        assert_eq!(config.suffix.to_wire_bytes(), *b"/P  ");
        Ok(())
    }

    #[test]
    fn config_identity_rejects_values_that_would_need_truncation() {
        assert!(DstarCallsign::new("N0CALL123").is_err());
        assert!(DstarSuffix::new("ABCDE").is_err());
    }

    #[test]
    fn status_reflector_encodes_name_and_module_without_loss() -> Result<(), BoxTestErr> {
        let reflector = DstarStatusReflector::new("REF030", 'C')?;

        assert_eq!(reflector.name().to_string(), "REF030");
        assert_eq!(reflector.module(), Module::C);
        assert_eq!(reflector.to_wire_bytes(), *b"REF030 C");
        Ok(())
    }

    #[test]
    fn status_reflector_accepts_the_full_seven_byte_name_width() -> Result<(), BoxTestErr> {
        let reflector = DstarStatusReflector::new("REF1234", 'Z')?;

        assert_eq!(reflector.to_wire_bytes(), *b"REF1234Z");
        Ok(())
    }

    #[test]
    fn status_reflector_rejects_a_name_that_would_overwrite_its_module() -> Result<(), BoxTestErr> {
        let error = DstarStatusReflector::new("REF12345", 'C')
            .err()
            .ok_or("an eight-byte reflector name must be rejected")?;

        assert!(matches!(
            error,
            DstarStatusReflectorError::NameTooLong { len: 8, max: 7 }
        ));
        Ok(())
    }

    #[test]
    fn status_reflector_rejects_invalid_names_and_modules() {
        assert!(matches!(
            DstarStatusReflector::new("W1AW", 'C'),
            Err(DstarStatusReflectorError::InvalidValue(
                TypeError::InvalidReflectorCallsign { .. }
            ))
        ));
        assert!(matches!(
            DstarStatusReflector::new("REF030", 'c'),
            Err(DstarStatusReflectorError::InvalidValue(
                TypeError::InvalidModule { got: 'c' }
            ))
        ));
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
    fn last_heard_entry_debug() -> Result<(), Box<dyn std::error::Error>> {
        let entry = LastHeardEntry {
            callsign: Callsign::try_from_str("W1AW")?.into(),
            suffix: Suffix::EMPTY,
            destination: Callsign::try_from_str("CQCQCQ")?.into(),
            repeater1: Callsign::try_from_str("DIRECT")?.into(),
            repeater2: Callsign::try_from_str("DIRECT")?.into(),
            timestamp: Instant::now(),
        };
        let debug = format!("{entry:?}");
        assert!(debug.contains("W1AW"), "debug should mention callsign");
        Ok(())
    }

    #[test]
    fn last_heard_entry_age_is_duration_since_timestamp() -> Result<(), Box<dyn std::error::Error>>
    {
        let t0 = Instant::now();
        let entry = LastHeardEntry {
            callsign: Callsign::try_from_str("W1AW")?.into(),
            suffix: Suffix::EMPTY,
            destination: Callsign::try_from_str("CQCQCQ")?.into(),
            repeater1: Callsign::try_from_str("DIRECT")?.into(),
            repeater2: Callsign::try_from_str("DIRECT")?.into(),
            timestamp: t0,
        };
        assert_eq!(
            entry.age(t0 + Duration::from_secs(30)),
            Duration::from_secs(30)
        );
        // Clock skew between capture and render must not panic.
        assert_eq!(entry.age(t0), Duration::ZERO);
        Ok(())
    }

    // -------------------------------------------------------------------
    // Event enum tests
    // -------------------------------------------------------------------

    #[test]
    fn event_debug_formatting() {
        let event = DstarEvent::VoiceEnd;
        let debug = format!("{event:?}");
        assert!(debug.contains("VoiceEnd"), "debug should mention variant");
    }

    #[test]
    fn event_text_message_debug() {
        let event = DstarEvent::TextMessage(SlowDataTextMessage::from_wire_bytes(
            *b"Hello D-STAR        ",
        ));
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
    ) -> Result<DstarGateway<MockTransport>, BoxTestErr> {
        let mut mock = MockTransport::new();
        // All writes accepted without wire assertions; the read
        // script below is the sequencing mechanism. (Responses are
        // pre-queued rather than attached to expectations because the
        // ACKs must be interleavable with the MMDVM pump's reads.)
        mock.expect_any_write();
        // The pump reads continuously, so pend instead of erroring when
        // the script runs dry.
        mock.pend_when_empty();
        // enter_mmdvm's TN 3,1 response (Bps9600 default for D-STAR).
        mock.queue_read(b"TN 3,1\r");
        // ACK for SetConfig (0x02), then for SetMode (0x03). The
        // SetMode ACK is delayed so it lands after set_mode's write
        // (its reply is correlated, not scanned from the event log).
        mock.queue_read_delayed(&[0xE0, 4, 0x70, 0x02], 20);
        mock.queue_read_delayed(&[0xE0, 4, 0x70, 0x03], 150);
        for (data, delay) in extra_reads {
            mock.queue_read_delayed(data, *delay);
        }

        let radio = Radio::new(mock);
        let config = DstarGatewayConfig::new(DstarCallsign::new("N0CALL")?);
        DstarGateway::start(radio, config)
            .await
            .map_err(|(_, e)| -> BoxTestErr { format!("gateway start failed: {e}").into() })
    }

    type BoxTestErr = Box<dyn std::error::Error>;

    #[tokio::test]
    async fn persistent_start_conversion_failure_returns_the_intact_radio() -> Result<(), BoxTestErr>
    {
        let radio = Radio::new(MockTransport::new());
        let Err((Some(radio), error)) =
            DstarGateway::start_gateway_mode(radio, test_config()?).await
        else {
            return Err("unproved CAT radio was not returned after conversion refusal".into());
        };

        assert!(matches!(error, Error::BinaryModeNotProven));
        assert_eq!(radio.cat_state, crate::radio::CatState::Ready);
        radio.transport.assert_complete();
        Ok(())
    }

    #[tokio::test]
    async fn persistent_init_nak_returns_a_retryable_binary_radio() -> Result<(), BoxTestErr> {
        tokio::task::LocalSet::new()
            .run_until(async {
                let mut mock = MockTransport::new();
                mock.expect_any_write();
                mock.pend_when_empty();
                // Reject SetConfig after it reaches the modem loop. The
                // persistent-mode rollback must reclaim this exact binary
                // transport and must not issue the CAT-side `TN 0,0` exit.
                mock.queue_read_delayed(&[0xE0, 5, 0x7F, MMDVM_SET_CONFIG, 4], 20);

                let mut radio = Radio::new(mock);
                radio.cat_state = crate::radio::CatState::BinaryProven;
                let Err((Some(radio), error)) =
                    DstarGateway::start_gateway_mode(radio, test_config()?).await
                else {
                    return Err("persistent init NAK did not return its binary radio".into());
                };

                assert!(matches!(error, Error::Protocol(_)));
                assert_eq!(radio.cat_state, crate::radio::CatState::BinaryProven);

                // Typed conversion is the public safety proof that a caller
                // can retry D-STAR init without reconnecting or sending CAT.
                let session = radio.into_mmdvm_session().map_err(|(_, error)| error)?;
                let (modem, restore) = session.into_parts();
                let radio = restore.shutdown_and_rebuild_binary(modem).await?;
                radio.transport.assert_complete();
                Ok(())
            })
            .await
    }

    #[tokio::test]
    async fn config_zero_last_heard_limit_retains_no_entries() -> Result<(), BoxTestErr> {
        tokio::task::LocalSet::new()
            .run_until(async {
                use dstar_gateway_core::{Callsign, Suffix};

                let mut gateway = started_gateway(&[]).await?;
                gateway.config.max_last_heard = 0;
                gateway.handle_voice_start(DstarHeader {
                    flag1: 0,
                    flag2: 0,
                    flag3: 0,
                    rpt2: Callsign::try_from_str("DIRECT")?,
                    rpt1: Callsign::try_from_str("DIRECT")?,
                    ur_call: Callsign::try_from_str("CQCQCQ")?,
                    my_call: Callsign::try_from_str("W1AW")?,
                    my_suffix: Suffix::from_wire_bytes(*b"    "),
                });

                assert!(gateway.last_heard().is_empty());
                Ok(())
            })
            .await
    }

    #[tokio::test]
    async fn header_emits_voice_start_then_station_heard() -> Result<(), BoxTestErr> {
        tokio::task::LocalSet::new()
            .run_until(async {
                let mut gateway = started_gateway(&[]).await?;
                let header = test_header("W1AW", *b"CQCQCQ  ")?;

                let primary = gateway
                    .dispatch_event(Event::DstarHeaderRx {
                        bytes: header.encode(),
                    })
                    .await?;
                assert!(matches!(primary, Some(DstarEvent::VoiceStart(got)) if got == header));

                let followup = gateway.pending_events.pop_front();
                assert!(matches!(
                    followup,
                    Some(DstarEvent::StationHeard(LastHeardEntry {
                        callsign,
                        destination,
                        ..
                    })) if callsign.text() == Ok("W1AW") && destination.text() == Ok("CQCQCQ")
                ));
                assert_eq!(gateway.last_heard().len(), 1);
                assert_eq!(
                    gateway
                        .last_heard()
                        .first()
                        .unwrap_or_else(|| unreachable!("one station was just recorded"))
                        .callsign,
                    Callsign::try_from_str("W1AW")?.into()
                );
                Ok(())
            })
            .await
    }

    #[tokio::test]
    async fn station_heard_preserves_opaque_identity_fields() -> Result<(), BoxTestErr> {
        tokio::task::LocalSet::new()
            .run_until(async {
                let mut gateway = started_gateway(&[]).await?;
                let mut header = test_header("W1AW", *b"CQCQCQ  ")?;
                let my_call = [b'W', b'1', 0xFF, b'W', b' ', b' ', b' ', b' '];
                let my_suffix = [b'P', 0x00, b' ', b' '];
                header.my_call = Callsign::from_wire_bytes(my_call);
                header.my_suffix = Suffix::from_wire_bytes(my_suffix);

                gateway.handle_voice_start(header);
                let Some(DstarEvent::StationHeard(entry)) = gateway.pending_events.pop_front()
                else {
                    unreachable!("voice start must queue its exact last-heard entry");
                };
                assert_eq!(entry.callsign.as_bytes(), &my_call);
                assert_eq!(entry.suffix.as_bytes(), &my_suffix);
                assert!(entry.callsign.text().is_err());
                assert!(entry.suffix.text().is_err());
                let stored = gateway
                    .last_heard()
                    .first()
                    .unwrap_or_else(|| unreachable!("the heard station was stored"));
                assert_eq!(stored.callsign.as_bytes(), &my_call);
                assert_eq!(stored.suffix.as_bytes(), &my_suffix);
                Ok(())
            })
            .await
    }

    #[tokio::test]
    async fn text_event_preserves_invalid_slow_data_bytes() -> Result<(), BoxTestErr> {
        tokio::task::LocalSet::new()
            .run_until(async {
                let mut gateway = started_gateway(&[]).await?;
                gateway.rx_active = true;

                let invalid_message = [
                    b'A', 0xFF, b'C', b'D', b'E', b'F', b'G', b'H', b'I', b'J', b'K', b'L', b'M',
                    b'N', b'O', b'P', b'Q', b'R', b'S', b'T',
                ];
                let blocks = [
                    [0x40, b'A', 0xFF, b'C', b'D', b'E'],
                    [0x41, b'F', b'G', b'H', b'I', b'J'],
                    [0x42, b'K', b'L', b'M', b'N', b'O'],
                    [0x43, b'P', b'Q', b'R', b'S', b'T'],
                ];
                let mut frame_index = 1;
                for [kind, first, second, third, fourth, fifth] in blocks {
                    gateway.slow_data.push(
                        dstar_gateway_core::scramble([kind, first, second]),
                        frame_index,
                    );
                    frame_index += 1;
                    gateway.slow_data.push(
                        dstar_gateway_core::scramble([third, fourth, fifth]),
                        frame_index,
                    );
                    frame_index += 1;
                }

                assert!(matches!(
                    gateway.on_eot().await?,
                    Some(DstarEvent::VoiceEnd)
                ));
                let Some(DstarEvent::TextMessage(message)) = gateway.pending_events.pop_front()
                else {
                    unreachable!("a complete text message must follow voice end");
                };
                assert_eq!(message.as_bytes(), &invalid_message);
                assert_eq!(
                    message.text(),
                    Err(WireTextError {
                        index: 1,
                        byte: 0xFF,
                    })
                );
                Ok(())
            })
            .await
    }

    #[tokio::test]
    async fn header_link_command_uses_validated_reflector_types() -> Result<(), BoxTestErr> {
        tokio::task::LocalSet::new()
            .run_until(async {
                let mut gateway = started_gateway(&[]).await?;
                let header = test_header("W1AW", *b"REF001 A")?;

                gateway.handle_voice_start(header);
                assert!(matches!(
                    gateway.pending_events.pop_front(),
                    Some(DstarEvent::StationHeard(_))
                ));
                let Some(DstarEvent::UrCallCommand(UrCallAction::Link { reflector, module })) =
                    gateway.pending_events.pop_front()
                else {
                    unreachable!("valid reflector link must produce a typed command event");
                };
                assert_eq!(reflector, ReflectorCallsign::try_from_str("REF001")?);
                assert_eq!(module, Module::A);
                assert!(gateway.pending_events.is_empty());
                Ok(())
            })
            .await
    }

    #[tokio::test]
    async fn opaque_urcall_never_becomes_a_gateway_command() -> Result<(), BoxTestErr> {
        tokio::task::LocalSet::new()
            .run_until(async {
                let mut gateway = started_gateway(&[]).await?;
                let opaque = [b'R', b'E', b'F', 0xff, b'0', b'1', b' ', b'A'];
                let header = test_header("W1AW", opaque)?;

                gateway.handle_voice_start(header);
                assert!(matches!(
                    gateway.pending_events.pop_front(),
                    Some(DstarEvent::StationHeard(_))
                ));
                assert!(gateway.pending_events.is_empty());
                assert!(!gateway.echo_active);
                assert_eq!(
                    gateway
                        .current_header()
                        .unwrap_or_else(|| unreachable!("the received stream is active"))
                        .ur_call
                        .as_bytes(),
                    &opaque
                );
                Ok(())
            })
            .await
    }

    #[tokio::test]
    async fn second_header_explicitly_preempts_active_stream() -> Result<(), BoxTestErr> {
        tokio::task::LocalSet::new()
            .run_until(async {
                let mut gateway = started_gateway(&[]).await?;
                let first = test_header("W1AW", *b"       E")?;
                let second = test_header("K1ABC", *b"CQCQCQ  ")?;

                let first_event = gateway
                    .dispatch_event(Event::DstarHeaderRx {
                        bytes: first.encode(),
                    })
                    .await?;
                assert!(matches!(first_event, Some(DstarEvent::VoiceStart(_))));
                assert!(gateway.echo_active);
                gateway.echo_frames.push(VoiceFrame {
                    ambe: [0x11; 9],
                    slow_data: [0x22; 3],
                });
                gateway.pending_events.clear();

                let boundary = gateway
                    .dispatch_event(Event::DstarHeaderRx {
                        bytes: second.encode(),
                    })
                    .await?;
                assert!(matches!(boundary, Some(DstarEvent::VoiceLost)));
                assert!(matches!(
                    gateway.pending_events.pop_front(),
                    Some(DstarEvent::VoiceStart(got)) if got == second
                ));
                assert!(matches!(
                    gateway.pending_events.pop_front(),
                    Some(DstarEvent::StationHeard(entry)) if entry.callsign.text() == Ok("K1ABC")
                ));
                assert!(gateway.pending_events.is_empty());
                assert_eq!(gateway.current_header(), Some(&second));
                assert!(!gateway.echo_active);
                assert!(gateway.echo_header.is_none());
                assert!(gateway.echo_frames.is_empty());
                assert_eq!(gateway.slow_data_frame_index, 0);
                Ok(())
            })
            .await
    }

    #[tokio::test]
    async fn lost_clears_echo_and_slow_data_stream_state() -> Result<(), BoxTestErr> {
        tokio::task::LocalSet::new()
            .run_until(async {
                let mut gateway = started_gateway(&[]).await?;
                let header = test_header("W1AW", *b"       E")?;
                let _start = gateway
                    .dispatch_event(Event::DstarHeaderRx {
                        bytes: header.encode(),
                    })
                    .await?;
                gateway.pending_events.clear();
                let _data = gateway
                    .dispatch_event(Event::DstarDataRx { bytes: [0x55; 12] })
                    .await?;
                assert!(gateway.echo_active);
                assert_eq!(gateway.echo_frames.len(), 1);
                assert_eq!(gateway.slow_data_frame_index, 1);

                let lost = gateway.dispatch_event(Event::DstarLost).await?;
                assert!(matches!(lost, Some(DstarEvent::VoiceLost)));
                assert!(!gateway.is_receiving());
                assert!(gateway.current_header().is_none());
                assert!(!gateway.echo_active);
                assert!(gateway.echo_header.is_none());
                assert!(gateway.echo_frames.is_empty());
                assert_eq!(gateway.slow_data_frame_index, 0);
                Ok(())
            })
            .await
    }

    #[tokio::test]
    async fn orphan_data_and_eot_surface_nonfatal_typed_violations() -> Result<(), BoxTestErr> {
        tokio::task::LocalSet::new()
            .run_until(async {
                let mut gateway = started_gateway(&[]).await?;
                let bytes = [0xA5; 12];

                let data = gateway.dispatch_event(Event::DstarDataRx { bytes }).await?;
                assert!(matches!(
                    data,
                    Some(DstarEvent::ProtocolViolation(
                        DstarProtocolViolation::VoiceDataWithoutHeader { frame }
                    )) if frame.ambe == [0xA5; 9] && frame.slow_data == [0xA5; 3]
                ));
                assert!(!gateway.is_receiving());
                assert_eq!(gateway.slow_data_frame_index, 0);

                let eot = gateway.dispatch_event(Event::DstarEot).await?;
                assert!(matches!(
                    eot,
                    Some(DstarEvent::ProtocolViolation(
                        DstarProtocolViolation::EndOfTransmissionWithoutHeader
                    ))
                ));
                assert!(!gateway.is_receiving());

                let lost = gateway.dispatch_event(Event::DstarLost).await?;
                assert!(matches!(
                    lost,
                    Some(DstarEvent::ProtocolViolation(
                        DstarProtocolViolation::SignalLostWithoutHeader
                    ))
                ));
                assert!(!gateway.is_receiving());
                Ok(())
            })
            .await
    }

    #[tokio::test]
    async fn untranslated_modem_events_are_preserved_exactly() -> Result<(), BoxTestErr> {
        tokio::task::LocalSet::new()
            .run_until(async {
                let mut gateway = started_gateway(&[]).await?;

                let dropped = gateway
                    .dispatch_event(Event::TxDropped { frames: 7 })
                    .await?;
                assert!(matches!(
                    dropped,
                    Some(DstarEvent::ModemEvent(Event::TxDropped { frames: 7 }))
                ));

                let violation = gateway
                    .dispatch_event(Event::ProtocolViolation {
                        command: 0x31,
                        detail: "expected 12 payload bytes, got 11".to_owned(),
                    })
                    .await?;
                assert!(matches!(
                    violation,
                    Some(DstarEvent::ModemEvent(Event::ProtocolViolation {
                        command: 0x31,
                        detail,
                    })) if detail == "expected 12 payload bytes, got 11"
                ));

                let payload = vec![0x00, 0x7F, 0x80, 0xFF];
                let unhandled = gateway
                    .dispatch_event(Event::UnhandledResponse {
                        command: 0x19,
                        payload: payload.clone(),
                    })
                    .await?;
                assert!(matches!(
                    unhandled,
                    Some(DstarEvent::ModemEvent(Event::UnhandledResponse {
                        command: 0x19,
                        payload: actual,
                    })) if actual == payload
                ));
                Ok(())
            })
            .await
    }

    #[tokio::test]
    async fn raw_event_loss_resets_stream_and_queues_voice_lost() -> Result<(), BoxTestErr> {
        tokio::task::LocalSet::new()
            .run_until(async {
                let mut gateway = started_gateway(&[]).await?;
                let header = test_header("W1AW", *b"       E")?;
                let _start = gateway
                    .dispatch_event(Event::DstarHeaderRx {
                        bytes: header.encode(),
                    })
                    .await?;
                gateway.pending_events.clear();
                let _data = gateway
                    .dispatch_event(Event::DstarDataRx { bytes: [0x33; 12] })
                    .await?;

                let loss = gateway
                    .dispatch_event(Event::EventsDropped { count: 17 })
                    .await?;
                assert!(matches!(
                    loss,
                    Some(DstarEvent::EventsDropped { count: 17 })
                ));
                assert!(matches!(
                    gateway.pending_events.pop_front(),
                    Some(DstarEvent::VoiceLost)
                ));
                assert!(!gateway.is_receiving());
                assert!(gateway.current_header().is_none());
                assert!(!gateway.echo_active);
                assert!(gateway.echo_frames.is_empty());
                assert_eq!(gateway.slow_data_frame_index, 0);
                Ok(())
            })
            .await
    }

    #[tokio::test]
    async fn dead_modem_is_error_not_quiet_airtime() -> Result<(), BoxTestErr> {
        tokio::task::LocalSet::new()
            .run_until(async {
                // The transport EOFs shortly after startup (an empty
                // delayed read delivers Ok(0)): the modem loop exits.
                // The FIRST next_event surfaces the terminal event as
                // an error; every LATER call must also error, since a dead
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

                // And it must KEEP erroring; the channel is closed.
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

                let mut observed = Vec::new();
                for _ in 0..4 {
                    let Some(event) = gateway.next_event().await? else {
                        break;
                    };
                    let is_voice_start = matches!(event, DstarEvent::VoiceStart(_));
                    observed.push(event);
                    if is_voice_start {
                        break;
                    }
                }
                assert!(
                    observed
                        .iter()
                        .any(|event| matches!(event, DstarEvent::VoiceStart(_))),
                    "the header drained during poll_status must surface: {observed:?}"
                );
                Ok(())
            })
            .await
    }

    #[test]
    fn double_fault_error_carries_both_causes() {
        // When D-STAR init fails AND the MMDVM rollback also fails,
        // the process must not abort; the combined error carries
        // both causes for the operator.
        let init = Error::Timeout(Duration::from_secs(2));
        let exit = Error::CommandRejected {
            mnemonic: "0M".to_string(),
        };
        let err = double_fault_error(&init, &exit);
        let mut chain_text = String::new();
        let mut source: Option<&dyn std::error::Error> = Some(&err);
        while let Some(e) = source {
            if !chain_text.is_empty() {
                chain_text.push_str(" | ");
            }
            chain_text.push_str(&e.to_string());
            source = e.source();
        }
        assert!(
            chain_text.contains("timed out") && chain_text.contains("rejected the 0M command"),
            "both causes must be reported: {chain_text}"
        );

        let err = binary_reclaim_fault_error(&init, &exit);
        let mut chain_text = String::new();
        let mut source: Option<&dyn std::error::Error> = Some(&err);
        while let Some(error) = source {
            if !chain_text.is_empty() {
                chain_text.push_str(" | ");
            }
            chain_text.push_str(&error.to_string());
            source = error.source();
        }
        assert!(
            chain_text.contains("timed out")
                && chain_text.contains("rejected the 0M command")
                && chain_text.contains("diagnose from scratch"),
            "persistent reclaim fault must retain both causes and recovery: {chain_text}"
        );
    }

    #[test]
    fn non_terminal_events_map_to_none() {
        assert!(terminal_event_error(&Event::DstarEot).is_none());
        assert!(
            terminal_event_error(&Event::TxDropped { frames: 3 }).is_none(),
            "TxDropped is reported, not terminal; the terminal event follows it"
        );
        assert!(
            terminal_event_error(&Event::ProtocolViolation {
                command: 0x01,
                detail: String::new(),
            })
            .is_none()
        );
        assert!(
            terminal_event_error(&Event::EventsDropped { count: 9 }).is_none(),
            "event-ring lag is an explicit discontinuity, not a dead modem"
        );
    }

    // Shell-err translation is unit-testable without a live modem.
    #[test]
    fn shell_err_session_closed_maps_to_transport_disconnected() {
        let err = shell_err_to_thd75_err(::mmdvm::ShellError::SessionClosed);
        assert!(matches!(err, Error::Transport(_)));
    }

    #[test]
    fn shell_err_io_maps_to_transport_disconnected() {
        let err = shell_err_to_thd75_err(::mmdvm::ShellError::Io(std::io::Error::from(
            std::io::ErrorKind::BrokenPipe,
        )));
        assert!(matches!(err, Error::Transport(_)));
    }
}
