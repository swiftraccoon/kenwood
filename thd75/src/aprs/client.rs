//! Integrated APRS client for the TH-D75.
//!
//! Combines KISS session management, position beaconing ([`SmartBeaconing`]),
//! reliable messaging (ack/retry via [`AprsMessenger`]), station tracking
//! ([`StationList`]), and optional digipeater forwarding
//! ([`DigipeaterConfig`]) into a single, easy-to-use async interface.
//!
//! # Design
//!
//! The [`AprsClient`] owns a [`KissSession`] and therefore the radio
//! transport. Create it with [`AprsClient::start`], which enters KISS
//! mode, and tear it down with [`AprsClient::stop`], which exits KISS
//! mode and returns the [`Radio`] for other use. This is the same
//! ownership pattern used by [`KissSession`] and
//! [`MmdvmSession`](crate::radio::mmdvm_session::MmdvmSession).
//!
//! The main loop calls [`AprsClient::next_event`] repeatedly. Each call
//! performs one cycle of I/O: send pending retries and beacons, receive
//! an incoming packet (with a short timeout), parse it, update the
//! station list, auto-ack if configured, and return a typed
//! [`AprsEvent`].
//!
//! # Example
//!
//! ```no_run
//! use kenwood_thd75::{Radio, AprsClient, AprsClientConfig};
//! use kenwood_thd75::transport::SerialTransport;
//!
//! # async fn example() -> Result<(), Box<dyn std::error::Error>> {
//! let transport = SerialTransport::open("/dev/cu.usbmodem1234", 115_200)?;
//! let radio = Radio::connect(transport).await?;
//!
//! let config = AprsClientConfig::new("N0CALL", 7);
//! let mut client = AprsClient::start(radio, config).await.map_err(|(_, e)| e)?;
//!
//! // Send a message
//! client.send_message("KQ4NIT", "Hello!").await?;
//!
//! // Beacon position
//! client.beacon_position(35.25, -97.75, "On the road").await?;
//!
//! // Process incoming packets (call in a loop)
//! while let Some(event) = client.next_event().await? {
//!     match event {
//!         kenwood_thd75::AprsEvent::StationHeard(entry) => {
//!             println!("Heard: {}", entry.callsign);
//!         }
//!         kenwood_thd75::AprsEvent::MessageReceived(msg) => {
//!             println!("Msg: {}", msg.text);
//!         }
//!         kenwood_thd75::AprsEvent::MessageDelivered(id) => {
//!             println!("Delivered: {id}");
//!         }
//!         kenwood_thd75::AprsEvent::MessageExpired(id) => {
//!             println!("Failed: {id}");
//!         }
//!         _ => {}
//!     }
//! }
//!
//! // Clean shutdown — exits KISS mode, returns Radio for other use
//! let _radio = client.stop().await.map_err(|(_client, e)| e)?;
//! # Ok(())
//! # }
//! ```

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use aprs::{
    AprsData, AprsMessage, AprsMessenger, AprsPosition, AprsWeather, DigiAction, DigipeaterConfig,
    SmartBeaconing, SmartBeaconingConfig, StationEntry, StationList, build_aprs_mice,
    build_aprs_object, build_aprs_position_compressed, build_aprs_position_report,
    build_aprs_status, build_query_response_position, classify_ack_rej, parse_aprs_data_full,
};
use ax25_codec::{Ax25Address, Ax25Packet, CommandResponse, RouteEntry, build_ax25, parse_ax25};
use kiss_tnc::{KissCommand, KissFrame, encode_kiss_frame};

use crate::aprs::ax25_to_kiss_wire;
use crate::error::Error;
use crate::radio::Radio;
use crate::radio::kiss_session::KissSession;
use crate::transport::Transport;
use crate::types::TncBaud;

/// Default receive timeout for `next_event` polling (500 ms).
///
/// Short enough to keep the event loop responsive for retries and
/// beacons, long enough to avoid busy-spinning on a quiet channel.
const EVENT_POLL_TIMEOUT: Duration = Duration::from_millis(500);

/// Configuration for an [`AprsClient`] session.
///
/// Created with [`AprsClientConfig::new`] which provides sensible
/// defaults for a mobile station. All fields are public for
/// customisation before passing to [`AprsClient::start`]. Marked
/// `#[non_exhaustive]` so future optional fields can be added without
/// breaking the API.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct AprsClientConfig {
    /// Station callsign (e.g., `"N0CALL"`).
    pub callsign: String,
    /// SSID (0-15). Common values: 7 = handheld, 9 = mobile, 15 = generic.
    pub ssid: u8,
    /// APRS primary symbol table character. Default: `'/'`.
    pub symbol_table: char,
    /// APRS symbol code character. Default: `'>'` (car).
    pub symbol_code: char,
    /// TNC data speed. Default: 1200 bps (AFSK).
    pub baud: TncBaud,
    /// Default comment appended to position beacons.
    pub beacon_comment: String,
    /// `SmartBeaconing` algorithm configuration.
    pub smart_beaconing: SmartBeaconingConfig,
    /// Optional digipeater configuration. When set, incoming packets
    /// are evaluated for relay according to the digipeater rules.
    pub digipeater: Option<DigipeaterConfig>,
    /// Maximum number of stations to track. Default: 500.
    pub max_stations: usize,
    /// Seconds before a station entry expires. Default: 3600 (1 hour).
    pub station_timeout_secs: u64,
    /// Automatically acknowledge incoming messages addressed to us.
    /// Default: `true`.
    pub auto_ack: bool,
    /// Digipeater path for outgoing packets.
    ///
    /// Default: `WIDE1-1,WIDE2-1` (standard 2-hop path). Use an empty
    /// vector for direct transmission with no digipeating. Parse from
    /// a string with [`crate::aprs::parse_digipeater_path`].
    pub digipeater_path: Vec<RouteEntry>,
    /// Automatically respond to `?APRSP` position queries addressed to us.
    ///
    /// When set and an incoming message contains `?APRSP`, the client
    /// sends a position beacon in response. Requires
    /// [`auto_query_position`](Self::auto_query_position) to be set.
    ///
    /// Default: `true`.
    pub auto_query_response: bool,
    /// Cached position for auto query responses, as `(lat, lon)`.
    ///
    /// When `None`, query responses are not sent even if
    /// `auto_query_response` is `true`. Update via
    /// [`AprsClient::set_query_response_position`].
    pub auto_query_position: Option<(f64, f64)>,
}

impl AprsClientConfig {
    /// Create a new configuration with sensible defaults for a mobile station.
    ///
    /// - Symbol: car (`/>`)
    /// - Baud: 1200 bps (standard APRS AFSK)
    /// - `SmartBeaconing`: TH-D75 defaults (Menu 540-547)
    /// - Max stations: 500, timeout: 1 hour
    /// - Auto-ack: on
    #[must_use]
    pub fn new(callsign: &str, ssid: u8) -> Self {
        Self {
            callsign: callsign.to_owned(),
            ssid,
            symbol_table: '/',
            symbol_code: '>',
            baud: TncBaud::Bps1200,
            beacon_comment: String::new(),
            smart_beaconing: SmartBeaconingConfig::default(),
            digipeater: None,
            max_stations: 500,
            station_timeout_secs: 3600,
            auto_ack: true,
            digipeater_path: crate::aprs::default_digipeater_path(),
            auto_query_response: true,
            auto_query_position: None,
        }
    }

    /// Build the [`Ax25Address`] for this station.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Validation`] if the callsign or SSID is not a
    /// valid AX.25 address — config fields are freely settable, so
    /// validation happens at use ([`AprsClient::start`]), not at
    /// construction.
    fn my_address(&self) -> Result<Ax25Address, Error> {
        Ax25Address::new(&self.callsign, self.ssid).map_err(|e| {
            tracing::warn!(callsign = %self.callsign, ssid = self.ssid, error = %e, "invalid APRS station address");
            Error::Validation(crate::error::ValidationError::AprsWireOutOfRange {
                field: "APRS station address",
                detail: "callsign must be 1-6 chars A-Z/0-9 and SSID 0-15",
            })
        })
    }

    /// Start building a configuration with the fluent builder.
    ///
    /// Example:
    ///
    /// ```no_run
    /// use kenwood_thd75::AprsClientConfig;
    /// let config = AprsClientConfig::builder("N0CALL", 9)
    ///     .symbol('/', '>')
    ///     .beacon_comment("mobile")
    ///     .auto_ack(true)
    ///     .build()
    ///     .expect("valid callsign and symbol");
    /// ```
    #[must_use]
    pub fn builder(callsign: &str, ssid: u8) -> AprsClientConfigBuilder {
        AprsClientConfigBuilder::new(callsign, ssid)
    }
}

/// Fluent builder for [`AprsClientConfig`].
///
/// Validates callsign / SSID / symbol at [`Self::build`] time and
/// returns a descriptive [`crate::error::ValidationError`] on bad input.
#[derive(Debug, Clone)]
pub struct AprsClientConfigBuilder {
    callsign: String,
    ssid: u8,
    symbol_table: char,
    symbol_code: char,
    baud: TncBaud,
    beacon_comment: String,
    smart_beaconing: SmartBeaconingConfig,
    digipeater: Option<DigipeaterConfig>,
    max_stations: usize,
    station_timeout_secs: u64,
    auto_ack: bool,
    digipeater_path: Vec<RouteEntry>,
    auto_query_response: bool,
    auto_query_position: Option<(f64, f64)>,
}

impl AprsClientConfigBuilder {
    /// Create a new builder with sensible defaults for a mobile station.
    #[must_use]
    pub fn new(callsign: &str, ssid: u8) -> Self {
        Self {
            callsign: callsign.to_owned(),
            ssid,
            symbol_table: '/',
            symbol_code: '>',
            baud: TncBaud::Bps1200,
            beacon_comment: String::new(),
            smart_beaconing: SmartBeaconingConfig::default(),
            digipeater: None,
            max_stations: 500,
            station_timeout_secs: 3600,
            auto_ack: true,
            digipeater_path: crate::aprs::default_digipeater_path(),
            auto_query_response: true,
            auto_query_position: None,
        }
    }

    /// Set both symbol table and code in one call.
    #[must_use]
    pub const fn symbol(mut self, table: char, code: char) -> Self {
        self.symbol_table = table;
        self.symbol_code = code;
        self
    }

    /// Override the TNC data speed (default 1200 bps).
    #[must_use]
    pub const fn baud(mut self, baud: TncBaud) -> Self {
        self.baud = baud;
        self
    }

    /// Set the default beacon comment.
    #[must_use]
    pub fn beacon_comment(mut self, s: &str) -> Self {
        s.clone_into(&mut self.beacon_comment);
        self
    }

    /// Replace the `SmartBeaconing` config.
    #[must_use]
    pub const fn smart_beaconing(mut self, sb: SmartBeaconingConfig) -> Self {
        self.smart_beaconing = sb;
        self
    }

    /// Attach a digipeater configuration.
    #[must_use]
    pub fn digipeater(mut self, cfg: DigipeaterConfig) -> Self {
        self.digipeater = Some(cfg);
        self
    }

    /// Maximum number of stations tracked in the station list.
    #[must_use]
    pub const fn max_stations(mut self, n: usize) -> Self {
        self.max_stations = n;
        self
    }

    /// Station entry expiry in seconds.
    #[must_use]
    pub const fn station_timeout_secs(mut self, s: u64) -> Self {
        self.station_timeout_secs = s;
        self
    }

    /// Whether to auto-ack incoming messages addressed to us.
    #[must_use]
    pub const fn auto_ack(mut self, on: bool) -> Self {
        self.auto_ack = on;
        self
    }

    /// Replace the outgoing digipeater path.
    #[must_use]
    pub fn digipeater_path(mut self, path: Vec<RouteEntry>) -> Self {
        self.digipeater_path = path;
        self
    }

    /// Whether to auto-respond to `?APRSP` position queries.
    #[must_use]
    pub const fn auto_query_response(mut self, on: bool) -> Self {
        self.auto_query_response = on;
        self
    }

    /// Cache a position for auto query responses.
    #[must_use]
    pub const fn auto_query_position(mut self, lat: f64, lon: f64) -> Self {
        self.auto_query_position = Some((lat, lon));
        self
    }

    /// Validate the accumulated fields and build the config.
    ///
    /// # Errors
    ///
    /// Returns [`crate::error::ValidationError::AprsWireOutOfRange`] if the callsign
    /// fails validation, the SSID is out of range, or the symbol table
    /// byte is outside the APRS-defined set (`/`, `\\`, 0-9, A-Z).
    pub fn build(self) -> Result<AprsClientConfig, crate::error::ValidationError> {
        // Callsign + SSID validation. `Ax25Address::new` returns
        // `Ax25Error` from the `ax25-codec` crate; map to this crate's
        // `ValidationError` at the boundary so callers keep the stable
        // error type.
        let _check = Ax25Address::new(&self.callsign, self.ssid).map_err(|_| {
            crate::error::ValidationError::AprsWireOutOfRange {
                field: "callsign",
                detail: "callsign must be 1-6 A-Z/0-9 and SSID 0-15",
            }
        })?;
        // Validate symbol table character. The aprs crate's
        // `SymbolTable::from_byte` returns `AprsError`; map to
        // `ValidationError` at the thd75 API boundary so callers keep the
        // stable error type.
        let _ = aprs::SymbolTable::from_byte(self.symbol_table as u8).map_err(|_| {
            crate::error::ValidationError::AprsWireOutOfRange {
                field: "APRS symbol table",
                detail: "must be '/', '\\\\', 0-9, or A-Z",
            }
        })?;
        // Validate symbol code (printable ASCII per APRS 1.0.1).
        let code_byte = self.symbol_code as u8;
        if !(0x21..=0x7E).contains(&code_byte) {
            return Err(crate::error::ValidationError::AprsWireOutOfRange {
                field: "APRS symbol code",
                detail: "must be printable ASCII (0x21-0x7E)",
            });
        }

        Ok(AprsClientConfig {
            callsign: self.callsign,
            ssid: self.ssid,
            symbol_table: self.symbol_table,
            symbol_code: self.symbol_code,
            baud: self.baud,
            beacon_comment: self.beacon_comment,
            smart_beaconing: self.smart_beaconing,
            digipeater: self.digipeater,
            max_stations: self.max_stations,
            station_timeout_secs: self.station_timeout_secs,
            auto_ack: self.auto_ack,
            digipeater_path: self.digipeater_path,
            auto_query_response: self.auto_query_response,
            auto_query_position: self.auto_query_position,
        })
    }
}

// ---------------------------------------------------------------------------
// AprsEvent
// ---------------------------------------------------------------------------

/// An event produced by [`AprsClient::next_event`].
///
/// Each variant represents a distinct category of APRS activity. The
/// client translates raw KISS/AX.25/APRS packets into these typed
/// events so callers never need to parse wire data.
#[derive(Debug, Clone)]
pub enum AprsEvent {
    /// A new or updated station was heard. Contains the station's
    /// current state after applying the received packet.
    StationHeard(StationEntry),
    /// An APRS message addressed to us was received.
    MessageReceived(AprsMessage),
    /// A previously sent message was acknowledged by the remote station.
    MessageDelivered(String),
    /// A previously sent message was rejected by the remote station.
    MessageRejected(String),
    /// A previously sent message expired after exhausting all retries.
    MessageExpired(String),
    /// A position report was received from another station.
    PositionReceived {
        /// Source callsign.
        source: String,
        /// Decoded position data.
        position: AprsPosition,
    },
    /// A weather report was received from another station.
    WeatherReceived {
        /// Source callsign.
        source: String,
        /// Decoded weather data.
        weather: AprsWeather,
    },
    /// A packet was digipeated (relayed) by our station.
    PacketDigipeated {
        /// Original source callsign.
        source: String,
    },
    /// An automatic response to a `?APRSP` position query was sent.
    QueryResponded {
        /// The callsign that sent the query.
        to: String,
    },
    /// A raw AX.25 packet that does not match any specific event type.
    RawPacket(Ax25Packet),
}

// ---------------------------------------------------------------------------
// AprsClient
// ---------------------------------------------------------------------------

/// Complete APRS client for the TH-D75.
///
/// Combines KISS session management, position beaconing
/// ([`SmartBeaconing`]), reliable messaging (ack/retry), station
/// tracking, and optional digipeater forwarding into a single,
/// easy-to-use async interface.
///
/// See the [module-level documentation](self) for a full usage example.
pub struct AprsClient<T: Transport> {
    session: KissSession<T>,
    config: AprsClientConfig,
    /// The station's AX.25 address, validated once at [`Self::start`].
    my_addr: Ax25Address,
    messenger: AprsMessenger,
    stations: StationList,
    beaconing: SmartBeaconing,
    /// Events produced but not yet returned to the caller.
    ///
    /// Used when a single call to [`Self::next_event`] generates more than
    /// one event (e.g. several retry timers expired at once). Drained at
    /// the top of each `next_event` before any new I/O is performed.
    pending_events: VecDeque<AprsEvent>,
    /// The raw AX.25 frame received during the current [`Self::next_event`]
    /// call, if any.
    ///
    /// Set to `Some` whenever a cycle receives a frame off the air (before
    /// digipeater and typed-event dispatch) and cleared at the top of the
    /// next cycle. An `IGate` consumer takes this with
    /// [`Self::take_last_rf_packet`] to gate *every* heard packet to
    /// APRS-IS, not just the ones that fall through to
    /// [`AprsEvent::RawPacket`].
    last_rf_packet: Option<Ax25Packet>,
}

impl<T: Transport> std::fmt::Debug for AprsClient<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AprsClient")
            .field("config", &self.config)
            .field("stations_count", &self.stations.len())
            .field("pending_messages", &self.messenger.pending_count())
            .finish_non_exhaustive()
    }
}

impl<T: Transport> AprsClient<T> {
    /// Start the APRS client, entering KISS mode on the radio.
    ///
    /// Consumes the [`Radio`] and returns an [`AprsClient`] that owns
    /// the transport. Call [`stop`](Self::stop) to exit KISS mode and
    /// reclaim the `Radio`.
    ///
    /// # Errors
    ///
    /// On failure, returns the [`Radio`] alongside the error so the
    /// caller can continue using CAT mode.
    pub async fn start(
        radio: Radio<T>,
        config: AprsClientConfig,
    ) -> Result<Self, (Radio<T>, Error)> {
        // Validate the station address BEFORE touching the radio —
        // config fields are freely settable, so this is the gate that
        // keeps an invalid callsign/SSID from reaching the air (and
        // it must be an error, never a panic).
        let my_addr = match config.my_address() {
            Ok(addr) => addr,
            Err(e) => return Err((radio, e)),
        };

        let mut session = match radio.enter_kiss(config.baud).await {
            Ok(s) => s,
            Err((radio, e)) => return Err((radio, e)),
        };
        session.set_receive_timeout(EVENT_POLL_TIMEOUT);

        let messenger = AprsMessenger::new(my_addr.clone(), config.digipeater_path.clone());
        let stations = StationList::new(
            config.max_stations,
            Duration::from_secs(config.station_timeout_secs),
        );
        let beaconing = SmartBeaconing::new(config.smart_beaconing.clone());

        Ok(Self {
            session,
            config,
            my_addr,
            messenger,
            stations,
            beaconing,
            pending_events: VecDeque::new(),
            last_rf_packet: None,
        })
    }

    /// Stop the APRS client, exiting KISS mode and returning the [`Radio`].
    ///
    /// # Errors
    ///
    /// Returns the client back together with the error if the KISS
    /// exit command fails, so the transport survives for a retry.
    pub async fn stop(self) -> Result<Radio<T>, (Box<Self>, Error)> {
        let Self {
            session,
            config,
            my_addr,
            messenger,
            stations,
            beaconing,
            pending_events,
            last_rf_packet,
        } = self;
        match session.exit().await {
            Ok(radio) => Ok(radio),
            Err((session, e)) => Err((
                Box::new(Self {
                    session,
                    config,
                    my_addr,
                    messenger,
                    stations,
                    beaconing,
                    pending_events,
                    last_rf_packet,
                }),
                e,
            )),
        }
    }

    /// Process pending I/O and return the next event.
    ///
    /// Each call performs one cycle:
    /// 1. Send any pending message retries via the [`AprsMessenger`].
    /// 2. Expire messages that have exhausted all retries.
    /// 3. Attempt to receive a KISS frame (short timeout).
    /// 4. If received: parse AX.25, parse APRS data, update station list.
    /// 5. If it is a message addressed to us and `auto_ack` is on, send ack.
    /// 6. If digipeater is configured, check whether we should relay.
    /// 7. Return the appropriate [`AprsEvent`].
    ///
    /// Returns `Ok(None)` when no activity occurs within the poll
    /// timeout. Callers should loop on this method.
    ///
    /// # Errors
    ///
    /// Returns an error on transport failures.
    pub async fn next_event(&mut self) -> Result<Option<AprsEvent>, Error> {
        // Clear the previous cycle's captured frame so a stale packet is
        // never gated against an event from a later cycle. It is set again
        // below only if this cycle actually receives a frame.
        self.last_rf_packet = None;

        // 0. Drain any events produced by a prior call.
        if let Some(ev) = self.pending_events.pop_front() {
            return Ok(Some(ev));
        }

        // Read the wall clock exactly once per iteration and thread it
        // through every stateful `aprs` call that needs a timestamp.
        // This keeps the sans-io `aprs` crate pure and guarantees that
        // all state-machine decisions within a single iteration observe
        // the same instant.
        let now = Instant::now();

        // 1. Send pending retries and enqueue expired message events.
        self.process_retries(now).await?;
        if let Some(ev) = self.pending_events.pop_front() {
            return Ok(Some(ev));
        }

        // 1b. Transmit any viscous-delayed digipeats whose hold time has
        // elapsed. This runs every cycle (even when idle) because a
        // deferred relay becomes due on a timer, not on a new receive.
        self.process_viscous_digipeater(now).await?;
        if let Some(ev) = self.pending_events.pop_front() {
            return Ok(Some(ev));
        }

        // 1c. Drop stations not heard within the configured timeout so
        // "heard recently" decisions (notably IGate RF-gating eligibility)
        // act on fresh data instead of stale entries that linger until
        // capacity eviction.
        self.stations.purge_expired(now);

        // 2. Try to receive a KISS data frame.
        let Some(packet) = self.recv_one_frame().await? else {
            return Ok(None);
        };

        // Capture the raw frame so an IGate can gate it regardless of how
        // it later classifies (typed event vs `RawPacket`).
        self.last_rf_packet = Some(packet.clone());

        // 3. Run digipeater logic before consuming the packet.
        if let Some(ev) = self.process_digipeater(&packet, now).await? {
            return Ok(Some(ev));
        }

        // 4. Parse APRS content and dispatch.
        self.handle_packet(packet, now).await
    }

    /// Phase 1: send any retry frames that are due and queue up
    /// `MessageExpired` events for any messages that exhausted their
    /// retry budget.
    async fn process_retries(&mut self, now: Instant) -> Result<(), Error> {
        // Peek → transmit → commit: if this future is cancelled during
        // the write, the attempt is not burned and the retry fires
        // again next cycle instead of silently vanishing.
        if let Some((id, frame)) = self.messenger.peek_frame_to_send(now) {
            self.session.send_wire(&frame).await?;
            self.messenger.commit_send(&id, now);
        }
        for id in self.messenger.cleanup_expired(now) {
            self.pending_events.push_back(AprsEvent::MessageExpired(id));
        }
        Ok(())
    }

    /// Phase 2: try to receive one KISS frame, decode it as AX.25, and
    /// return the parsed packet. Returns `Ok(None)` on timeout or
    /// `WouldBlock` (no data ready), and on non-data frames / parse
    /// failures. Real transport errors propagate as `Err`.
    async fn recv_one_frame(&mut self) -> Result<Option<Ax25Packet>, Error> {
        let frame = match self.session.receive_frame().await {
            Ok(f) => f,
            Err(Error::Timeout(_)) => return Ok(None),
            Err(Error::Transport(crate::error::TransportError::Read(io_err)))
                if matches!(
                    io_err.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                return Ok(None);
            }
            Err(e) => return Err(e),
        };
        if frame.command != KissCommand::Data {
            return Ok(None);
        }
        Ok(match parse_ax25(&frame.data) {
            Ok(packet) => Some(packet),
            Err(e) => {
                // A KISS frame that fails AX.25 decode is real
                // corruption on air — leave a trace so a degrading
                // link is distinguishable from an idle channel.
                tracing::debug!(error = ?e, len = frame.data.len(), "dropping undecodable AX.25 frame");
                None
            }
        })
    }

    /// Phase 3: if the digipeater is configured and would relay this
    /// packet, emit the relay frame and return a
    /// [`AprsEvent::PacketDigipeated`] event.
    async fn process_digipeater(
        &mut self,
        packet: &Ax25Packet,
        now: Instant,
    ) -> Result<Option<AprsEvent>, Error> {
        if let Some(digi_config) = self.config.digipeater.as_mut()
            && let DigiAction::Relay { modified_packet } = digi_config.process(packet, now)
        {
            let wire = ax25_to_kiss_wire(&modified_packet);
            self.session.send_wire(&wire).await?;
            return Ok(Some(AprsEvent::PacketDigipeated {
                source: packet.source.callsign.as_str().to_owned(),
            }));
        }
        Ok(None)
    }

    /// Phase 1b: transmit any viscous-delayed digipeats whose hold time
    /// has elapsed.
    ///
    /// When the digipeater runs with a non-zero viscous delay, a relay is
    /// not sent immediately — [`DigipeaterConfig::process`] returns
    /// [`DigiAction::Drop`] and stashes the modified frame internally. The
    /// frame is only released once the delay elapses (and no other station
    /// digipeated it first), via
    /// [`DigipeaterConfig::drain_ready_viscous`]. Without this drain the
    /// deferred relays would never be transmitted, so every viscous
    /// digipeat would be silently swallowed.
    ///
    /// Each released frame is sent and reported as a
    /// [`AprsEvent::PacketDigipeated`].
    async fn process_viscous_digipeater(&mut self, now: Instant) -> Result<(), Error> {
        // Drain first so the mutable borrow of `config` ends before the
        // send loop needs `&mut self.session`.
        let ready = match self.config.digipeater.as_mut() {
            Some(digi_config) => digi_config.drain_ready_viscous(now),
            None => return Ok(()),
        };
        for packet in ready {
            let wire = ax25_to_kiss_wire(&packet);
            let source = packet.source.callsign.as_str().to_owned();
            self.session.send_wire(&wire).await?;
            self.pending_events
                .push_back(AprsEvent::PacketDigipeated { source });
        }
        Ok(())
    }

    /// Take the raw AX.25 frame received during the most recent
    /// [`Self::next_event`] cycle, leaving `None` behind.
    ///
    /// Returns `Some` only when the cycle that produced the current event
    /// also received a frame off the air. An `IGate` uses this to forward
    /// **every** heard packet to APRS-IS — including ones that surfaced as
    /// typed events (`PositionReceived`, `StationHeard`, …) rather than
    /// [`AprsEvent::RawPacket`] — by pairing it with
    /// [`Self::format_for_is`].
    pub const fn take_last_rf_packet(&mut self) -> Option<Ax25Packet> {
        self.last_rf_packet.take()
    }

    /// Phase 4: parse the APRS info field, update the station list,
    /// and dispatch to the appropriate event variant.
    async fn handle_packet(
        &mut self,
        packet: Ax25Packet,
        now: Instant,
    ) -> Result<Option<AprsEvent>, Error> {
        let Ok(aprs_data) = parse_aprs_data_full(&packet.info, &packet.destination.callsign) else {
            return Ok(Some(AprsEvent::RawPacket(packet)));
        };

        let path: Vec<String> = packet.digipeaters.iter().map(ToString::to_string).collect();
        self.stations
            .update(&packet.source.callsign, &aprs_data, &path, now);

        if let AprsData::Message(ref msg) = aprs_data {
            // Check-then-mark, NOT check-and-mark: marking happens
            // only after the message was fully processed, so a
            // cancelled delivery (mid auto-ack send) does not eat the
            // message and dedup away its RF retries.
            if self
                .messenger
                .is_duplicate_incoming(&packet.source.callsign, msg, now)
            {
                return Ok(None);
            }
            let event = self.handle_incoming_message(msg, &packet.source).await?;
            self.messenger
                .mark_incoming_seen(&packet.source.callsign, msg, now);
            return Ok(event);
        }

        self.dispatch_event(packet, aprs_data)
    }

    /// Phase 4b: given the parsed APRS data and the source packet, pick
    /// the right `AprsEvent` variant.
    fn dispatch_event(
        &mut self,
        packet: Ax25Packet,
        aprs_data: AprsData,
    ) -> Result<Option<AprsEvent>, Error> {
        match aprs_data {
            AprsData::Position(pos) => {
                let source: String = packet.source.callsign.as_str().to_owned();
                let weather = pos.weather.clone();
                let entry = self.stations.get(&source).cloned();

                // Observation-order contract (matches every other arm in
                // this match): `StationHeard` is the *primary* event when
                // the station list has an entry — it carries the "we just
                // heard from this station" signal that callers use for UI
                // refresh and IGate timing. Any data-bearing sub-event
                // (weather, position) is queued for the *next* call to
                // [`AprsClient::next_event`] so the caller sees them in
                // arrival order: heard-then-data.
                //
                // Earlier code generations returned `WeatherReceived`
                // first and queued `StationHeard` — that violated the
                // sibling-arm convention and would *lose* the StationHeard
                // event entirely if the caller stopped polling between
                // the two.
                match (entry, weather) {
                    (Some(entry_ev), Some(wx)) => {
                        self.pending_events.push_back(AprsEvent::WeatherReceived {
                            source,
                            weather: wx,
                        });
                        Ok(Some(AprsEvent::StationHeard(entry_ev)))
                    }
                    (Some(entry_ev), None) => Ok(Some(AprsEvent::StationHeard(entry_ev))),
                    (None, Some(wx)) => Ok(Some(AprsEvent::WeatherReceived {
                        source,
                        weather: wx,
                    })),
                    (None, None) => Ok(Some(AprsEvent::PositionReceived {
                        source,
                        position: pos,
                    })),
                }
            }
            AprsData::Weather(wx) => Ok(Some(AprsEvent::WeatherReceived {
                source: packet.source.callsign.as_str().to_owned(),
                weather: wx,
            })),
            AprsData::Status(_)
            | AprsData::Object(_)
            | AprsData::Item(_)
            | AprsData::ThirdParty { .. }
            | AprsData::Grid(_)
            | AprsData::RawGps(_)
            | AprsData::StationCapabilities(_)
            | AprsData::AgreloDfJr(_)
            | AprsData::UserDefined { .. }
            | AprsData::InvalidOrTest(_)
            | AprsData::RawWeather { .. } => self
                .stations
                .get(&packet.source.callsign)
                .cloned()
                .map_or(Ok(Some(AprsEvent::RawPacket(packet))), |entry| {
                    Ok(Some(AprsEvent::StationHeard(entry)))
                }),
            AprsData::Message(_) => unreachable!("messages handled above"),
            AprsData::Telemetry(_) | AprsData::Query(_) => self
                .stations
                .get(&packet.source.callsign)
                .cloned()
                .map_or(Ok(Some(AprsEvent::RawPacket(packet))), |entry| {
                    Ok(Some(AprsEvent::StationHeard(entry)))
                }),
        }
    }

    /// Send an APRS message to a station. Returns the message ID for tracking.
    ///
    /// The message is queued with the [`AprsMessenger`] for automatic
    /// retry until acknowledged, rejected, or expired.
    ///
    /// # Errors
    ///
    /// Returns an error if the initial transmission fails.
    pub async fn send_message(&mut self, addressee: &str, text: &str) -> Result<String, Error> {
        let now = Instant::now();
        let message_id = self.messenger.send_message(addressee, text, now);

        // Send the first frame immediately (peek → write → commit, so
        // a cancelled write leaves the first attempt unburned).
        if let Some((id, frame)) = self.messenger.peek_frame_to_send(now) {
            self.session.send_wire(&frame).await?;
            self.messenger.commit_send(&id, now);
        }

        Ok(message_id)
    }

    /// Beacon current position using uncompressed format, sampling the
    /// wall clock implicitly.
    ///
    /// Convenience wrapper around [`Self::beacon_position_at`]. Reads
    /// `Instant::now()` exactly once and threads the same value through
    /// the wire send and the `SmartBeaconing` timer update.
    ///
    /// Prefer [`Self::beacon_position_at`] from within larger state-
    /// machine iterations (e.g. [`Self::update_motion`]) so a single
    /// captured `now` flows through every clock-reading operation that
    /// shares the iteration's semantic timestamp.
    ///
    /// # Errors
    ///
    /// Returns an error if the transmission fails.
    pub async fn beacon_position(
        &mut self,
        lat: f64,
        lon: f64,
        comment: &str,
    ) -> Result<(), Error> {
        self.beacon_position_at(lat, lon, comment, Instant::now())
            .await
    }

    /// Beacon current position using uncompressed format, with an
    /// explicit clock sample.
    ///
    /// `now` is used to update the `SmartBeaconing` "last beacon" timer
    /// after the wire send completes. Callers participating in a larger
    /// state-machine iteration (notably [`Self::update_motion`]) should
    /// thread their captured `Instant::now()` here rather than letting
    /// this method re-sample, so that `should_beacon`, the wire send,
    /// and `beacon_sent` observe the same semantic timestamp.
    ///
    /// # Errors
    ///
    /// Returns an error if the transmission fails.
    pub async fn beacon_position_at(
        &mut self,
        lat: f64,
        lon: f64,
        comment: &str,
        now: Instant,
    ) -> Result<(), Error> {
        let source = self.my_addr.clone();
        let wire = build_aprs_position_report(
            &source,
            lat,
            lon,
            self.config.symbol_table,
            self.config.symbol_code,
            comment,
            &self.config.digipeater_path,
        );
        self.session.send_wire(&wire).await?;
        self.beaconing.beacon_sent(now);
        Ok(())
    }

    /// Beacon position using compressed format (smaller packet), sampling
    /// the wall clock implicitly.
    ///
    /// Uses base-91 encoding per APRS 1.0.1 Chapter 9. Produces smaller
    /// packets than [`Self::beacon_position`]. Convenience wrapper around
    /// [`Self::beacon_position_compressed_at`]; prefer the `_at` variant
    /// inside larger state-machine iterations to share a single clock
    /// sample with sibling clock-reading operations.
    ///
    /// # Errors
    ///
    /// Returns an error if the transmission fails.
    pub async fn beacon_position_compressed(
        &mut self,
        lat: f64,
        lon: f64,
        comment: &str,
    ) -> Result<(), Error> {
        self.beacon_position_compressed_at(lat, lon, comment, Instant::now())
            .await
    }

    /// Beacon position using compressed format, with an explicit clock
    /// sample.
    ///
    /// See [`Self::beacon_position_at`] for the rationale on threading
    /// `now` from a caller-managed iteration.
    ///
    /// # Errors
    ///
    /// Returns an error if the transmission fails.
    pub async fn beacon_position_compressed_at(
        &mut self,
        lat: f64,
        lon: f64,
        comment: &str,
        now: Instant,
    ) -> Result<(), Error> {
        let source = self.my_addr.clone();
        let wire = build_aprs_position_compressed(
            &source,
            lat,
            lon,
            self.config.symbol_table,
            self.config.symbol_code,
            comment,
            &self.config.digipeater_path,
        );
        self.session.send_wire(&wire).await?;
        self.beaconing.beacon_sent(now);
        Ok(())
    }

    /// Beacon current position using Mic-E encoding — the most compact
    /// APRS position format and the TH-D75's native one.
    ///
    /// Latitude is encoded in the AX.25 destination address; longitude,
    /// speed, and course are packed into the info field per APRS 1.0.1
    /// Chapter 10. Uses the configured symbol, the configured digipeater
    /// path, and the "Off Duty" Mic-E message code.
    ///
    /// `speed_knots` covers the Mic-E wire range 0–799; `course_deg`
    /// uses 0 for "unknown" per the spec. Updates the `SmartBeaconing`
    /// "last beacon" timer like the other beacon methods.
    ///
    /// # Errors
    ///
    /// Returns an error if the transmission fails.
    pub async fn beacon_position_mice(
        &mut self,
        lat: f64,
        lon: f64,
        speed_knots: u16,
        course_deg: u16,
        comment: &str,
    ) -> Result<(), Error> {
        let source = self.my_addr.clone();
        let wire = build_aprs_mice(
            &source,
            lat,
            lon,
            speed_knots,
            course_deg,
            self.config.symbol_table,
            self.config.symbol_code,
            comment,
            &self.config.digipeater_path,
        );
        self.session.send_wire(&wire).await?;
        self.beaconing.beacon_sent(Instant::now());
        Ok(())
    }

    /// Send a status report.
    ///
    /// # Errors
    ///
    /// Returns an error if the transmission fails.
    pub async fn send_status(&mut self, text: &str) -> Result<(), Error> {
        let source = self.my_addr.clone();
        let wire = build_aprs_status(&source, text, &self.config.digipeater_path);
        self.session.send_wire(&wire).await?;
        Ok(())
    }

    /// Set the cached position for auto query responses.
    ///
    /// When a station sends `?APRSP` and auto query response is enabled,
    /// the client replies with a position beacon using this position.
    pub const fn set_query_response_position(&mut self, lat: f64, lon: f64) {
        self.config.auto_query_position = Some((lat, lon));
    }

    /// Send an object report.
    ///
    /// # Errors
    ///
    /// Returns an error if the transmission fails.
    pub async fn send_object(
        &mut self,
        name: &str,
        live: bool,
        lat: f64,
        lon: f64,
        comment: &str,
    ) -> Result<(), Error> {
        let source = self.my_addr.clone();
        let wire = build_aprs_object(
            &source,
            name,
            live,
            lat,
            lon,
            self.config.symbol_table,
            self.config.symbol_code,
            comment,
            &self.config.digipeater_path,
        );
        self.session.send_wire(&wire).await?;
        Ok(())
    }

    /// Update speed and course for `SmartBeaconing`.
    ///
    /// If the `SmartBeaconing` algorithm determines a beacon is due (based
    /// on speed, course change, and elapsed time), a position report is
    /// transmitted and this method returns `Ok(true)`. Otherwise returns
    /// `Ok(false)`.
    ///
    /// # Clock discipline
    ///
    /// This method samples `Instant::now()` exactly once and threads the
    /// captured value through every clock-reading call it makes:
    /// [`SmartBeaconing::should_beacon`], [`Self::beacon_position_at`]
    /// (which forwards to [`SmartBeaconing::beacon_sent`]), and
    /// [`SmartBeaconing::beacon_sent_with`]. This guarantees the three
    /// state-machine decisions in one iteration observe the same
    /// semantic timestamp; an earlier code generation called
    /// [`Instant::now`] three times here and could record a
    /// `beacon_sent_with` slightly later than the `should_beacon` it was
    /// answering.
    ///
    /// # Errors
    ///
    /// Returns an error if the beacon transmission fails.
    pub async fn update_motion(
        &mut self,
        speed_kmh: f64,
        course_deg: f64,
        lat: f64,
        lon: f64,
    ) -> Result<bool, Error> {
        let now = Instant::now();
        if self.beaconing.should_beacon(speed_kmh, course_deg, now) {
            let comment = self.config.beacon_comment.clone();
            self.beacon_position_at(lat, lon, &comment, now).await?;
            self.beaconing.beacon_sent_with(speed_kmh, course_deg, now);
            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// Get the station list (read-only reference).
    #[must_use]
    pub const fn stations(&self) -> &StationList {
        &self.stations
    }

    /// Get the messenger state (pending message count, etc).
    #[must_use]
    pub const fn messenger(&self) -> &AprsMessenger {
        &self.messenger
    }

    /// Get the current configuration.
    #[must_use]
    pub const fn config(&self) -> &AprsClientConfig {
        &self.config
    }

    // -----------------------------------------------------------------------
    // IGate (Internet Gateway) methods
    // -----------------------------------------------------------------------

    /// Format a received RF packet for transmission to APRS-IS.
    ///
    /// Converts the AX.25 packet to APRS-IS text format:
    /// `SOURCE>DEST,PATH,qAR,MYCALL:data`
    ///
    /// The `qAR` construct identifies this as an RF-gated packet per
    /// the APRS-IS q-construct specification.
    #[must_use]
    pub fn format_for_is(&self, packet: &Ax25Packet) -> String {
        let mut path_parts: Vec<String> =
            packet.digipeaters.iter().map(ToString::to_string).collect();
        path_parts.push("qAR".to_owned());
        path_parts.push(format!("{}", self.my_addr));
        let path_str = path_parts.join(",");
        let data = String::from_utf8_lossy(&packet.info);
        format!(
            "{}>{},{path_str}:{data}\r\n",
            packet.source, packet.destination,
        )
    }

    /// Build the AX.25 wrap frame that this `IGate` would transmit for a
    /// given APRS-IS line, **without** sending it.
    ///
    /// Returns `None` if the line fails `IGate` gating rules
    /// ([`Self::should_gate_to_rf`]) or if its TNC2 form is malformed
    /// (missing `:`). Exposed at module-private scope as a pure helper
    /// so unit tests can assert wire-format properties (outer path,
    /// `,I` marker, etc.) without driving the mock transport.
    fn build_gate_from_is_packet(&self, is_packet: &str) -> Option<Ax25Packet> {
        if !self.should_gate_to_rf(is_packet) {
            return None;
        }

        // Parse `ORIGSRC>ORIGDEST,ORIGPATH:data`. The split at the first
        // `:` is correct for TNC2 format because the source/destination
        // and digi path are ASCII per spec and cannot contain a colon.
        let (header, data) = is_packet.split_once(':')?;

        // Build the inner third-party payload, appending the IGate's
        // own callsign and the `,I` marker per IGating.aspx. This is
        // what distinguishes an Internet→RF gated packet from one that
        // was already on RF; receivers look for `,I` to skip re-gating
        // and to know who introduced the packet.
        let igate_call = self.my_addr.clone();
        let third_party_payload = format!("}}{header},{igate_call},I:{data}");

        // Outer AX.25 frame:
        //   source = IGate callsign
        //   destination = TH-D75 tocall (matches this crate's other
        //     beacon-style transmissions)
        //   digipeaters = IGate's configured RF path so the gated
        //     packet propagates beyond the IGate's footprint
        let destination = Ax25Address::new("APK005", 0).ok()?;
        Some(Ax25Packet {
            source: igate_call,
            destination,
            digipeaters: self.config.digipeater_path.clone(),
            command_or_response: Some(CommandResponse::Command),
            control: 0x03,
            protocol: 0xF0,
            info: third_party_payload.into_bytes(),
        })
    }

    /// Parse an APRS-IS packet and transmit it on RF via KISS.
    ///
    /// Wraps the IS packet in a third-party header and broadcasts it on
    /// the configured RF digipeater path, following the canonical
    /// IGate-to-RF format documented at
    /// <http://www.aprs-is.net/IGating.aspx> and APRS 1.0.1 Chapter 17.
    ///
    /// # Wire shape
    ///
    /// Given an inbound IS line of the form:
    ///
    /// ```text
    /// ORIGSRC>ORIGDEST,ORIGPATH:data
    /// ```
    ///
    /// the function emits an outer AX.25 frame:
    ///
    /// ```text
    /// MYCALL>APK005,RF_PATH:}ORIGSRC>ORIGDEST,ORIGPATH,MYCALL,I:data
    /// ```
    ///
    /// where
    /// - `MYCALL` is the `IGate`'s configured callsign + SSID
    ///   (the station's validated address),
    /// - `APK005` is the TH-D75 tocall (matching this crate's beacon
    ///   destination), and
    /// - `RF_PATH` is the `IGate`'s configured digipeater path
    ///   (`self.config.digipeater_path`) so the gated packet reaches
    ///   beyond the `IGate`'s immediate footprint via existing RF
    ///   digipeaters.
    /// - The inner third-party payload appends `,MYCALL,I` to the
    ///   original IS-side path. `,I` is the spec-mandated marker
    ///   identifying the packet as crossing the Internet→RF boundary
    ///   at `MYCALL`. Receivers use this to detect `IGated` traffic and
    ///   avoid re-gating it.
    ///
    /// Pre-2026 versions used a hard-coded `[TCPIP-0]` outer path and
    /// did not append `,I`, which made the wire frame indistinguishable
    /// from an internet-originated packet that had never touched RF.
    ///
    /// # Filtering
    ///
    /// Only transmits if [`Self::should_gate_to_rf`] returns `true`.
    /// Returns `Ok(false)` if the packet was filtered out, `Ok(true)`
    /// if it was transmitted.
    ///
    /// # Errors
    ///
    /// Returns an error if the KISS transmission fails.
    pub async fn gate_from_is(&mut self, is_packet: &str) -> Result<bool, Error> {
        let Some(packet) = self.build_gate_from_is_packet(is_packet) else {
            return Ok(false);
        };
        let ax25_bytes = build_ax25(&packet);
        let wire = encode_kiss_frame(&KissFrame::data(ax25_bytes));
        self.session.send_wire(&wire).await?;
        Ok(true)
    }

    /// Check if a packet should be gated to APRS-IS.
    ///
    /// Applies standard `IGate` rules:
    /// - Don't gate packets from TCPIP/TCPXX sources
    /// - Don't gate third-party packets (`}` prefix)
    /// - Don't gate packets with NOGATE/RFONLY in path
    ///
    /// All callsign comparisons here are case-sensitive against the
    /// uppercase form because `ax25_codec::Callsign::new` already
    /// enforces uppercase ASCII alphanumeric at validation time — every
    /// `Callsign` in a parsed `Ax25Packet` is therefore already in
    /// canonical case, and a runtime `.to_uppercase()` would allocate
    /// without semantic effect.
    #[must_use]
    pub fn should_gate_to_is(packet: &Ax25Packet) -> bool {
        // Don't gate packets originating from the internet. `Callsign`
        // values are uppercase by validation invariant; direct PartialEq
        // against the spec-form literal is sufficient.
        let src = packet.source.callsign.as_str();
        if src == "TCPIP" || src == "TCPXX" {
            return false;
        }

        // Don't gate third-party packets (info starts with '}'). These
        // have already been gated once and re-gating creates loops.
        if packet.info.first() == Some(&b'}') {
            return false;
        }

        // Don't gate packets with NOGATE or RFONLY in the digipeater path.
        for digi in &packet.digipeaters {
            let call = digi.address.callsign.as_str();
            if call == "NOGATE" || call == "RFONLY" {
                return false;
            }
        }

        true
    }

    /// Check if an APRS-IS packet should be gated to RF.
    ///
    /// Applies standard `IGate` rules:
    /// - Only gate messages addressed to stations heard on RF recently
    /// - Don't gate general position reports to RF (would flood)
    /// - Don't gate packets containing TCPIP/TCPXX/NOGATE/RFONLY in path
    #[must_use]
    pub fn should_gate_to_rf(&self, is_line: &str) -> bool {
        let Some(line) = aprs_is::AprsIsLine::parse(is_line) else {
            return false;
        };

        // Skip packets marked as RF-only or no-gate (any path element).
        if line.has_no_gate_marker() {
            return false;
        }

        // Only gate APRS messages, not position reports, weather, etc.
        if !line.data.starts_with(':') {
            return false;
        }

        // Extract the addressee from the message (9-char padded field).
        if line.data.len() < 11 || line.data.as_bytes().get(10) != Some(&b':') {
            return false;
        }
        let addressee = line.data[1..10].trim();

        // Only gate if the addressee has been heard on RF recently.
        self.stations.get(addressee).is_some()
    }

    // -----------------------------------------------------------------------
    // Internal helpers
    // -----------------------------------------------------------------------

    /// Handle an incoming APRS message addressed to us.
    async fn handle_incoming_message(
        &mut self,
        msg: &AprsMessage,
        from: &Ax25Address,
    ) -> Result<Option<AprsEvent>, Error> {
        let my_call = self.config.callsign.to_uppercase();

        // Check if this message is addressed to us.
        if msg.addressee.to_uppercase() != my_call {
            // Not for us — treat as a station heard event.
            let entry = self.stations.get(&from.callsign).cloned();
            return Ok(entry.map(AprsEvent::StationHeard));
        }

        // Check if it is an ack/rej control frame for a pending message.
        // The messenger only honours it when `from` is the station the
        // message was addressed to — a message number is not a secret on
        // the air, so any other station's ack is ignored.
        if let Some((is_ack, id)) = classify_ack_rej(&msg.text) {
            let id_owned = id.to_owned();
            if self.messenger.process_incoming(&from.callsign, msg) {
                return Ok(Some(if is_ack {
                    AprsEvent::MessageDelivered(id_owned)
                } else {
                    AprsEvent::MessageRejected(id_owned)
                }));
            }
            // Control frame for an unknown message — ignore.
            return Ok(None);
        }

        // Regular message addressed to us — auto-ack if configured.
        if self.config.auto_ack
            && let Some(ref id) = msg.message_id
        {
            let ack_frame = self.messenger.build_ack(&from.callsign, id);
            self.session.send_wire(&ack_frame).await?;
        }

        // Handle directed position query (`?APRSP`).
        //
        // When enabled and a position is cached, respond with a position
        // beacon. The beacon goes to CQCQCQ (all stations), not just the
        // querying station — this is per APRS spec, which treats the
        // query as a request for a fresh beacon from the queried station.
        if self.config.auto_query_response
            && msg.text.trim() == "?APRSP"
            && let Some((lat, lon)) = self.config.auto_query_position
        {
            tracing::info!(from = %from.callsign, "responding to ?APRSP query");
            let source = self.my_addr.clone();
            let wire = build_query_response_position(
                &source,
                lat,
                lon,
                self.config.symbol_table,
                self.config.symbol_code,
                &self.config.beacon_comment,
                &self.config.digipeater_path,
            );
            self.session.send_wire(&wire).await?;
            return Ok(Some(AprsEvent::QueryResponded {
                to: from.callsign.as_str().to_owned(),
            }));
        }

        Ok(Some(AprsEvent::MessageReceived(msg.clone())))
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use aprs::{build_aprs_message as build_msg, build_aprs_position_report as build_pos};
    use kiss_tnc::FEND;

    use crate::aprs::default_digipeater_path;
    use crate::transport::MockTransport;
    use crate::types::TncBaud;

    type TestResult = Result<(), Box<dyn std::error::Error>>;
    type BoxErr = Box<dyn std::error::Error>;

    /// Build a mock Radio that expects the TN 2,x command for KISS entry.
    async fn mock_radio(baud: TncBaud) -> Result<Radio<MockTransport>, BoxErr> {
        let tn_cmd = format!("TN 2,{}\r", u8::from(baud));
        let tn_resp = format!("TN 2,{}\r", u8::from(baud));
        let mut mock = MockTransport::new();
        mock.expect(tn_cmd.as_bytes(), tn_resp.as_bytes());
        Ok(Radio::connect(mock).await?)
    }

    fn test_config() -> AprsClientConfig {
        AprsClientConfig::new("N0CALL", 7)
    }

    fn test_address() -> Ax25Address {
        Ax25Address::new("N0CALL", 7)
            .unwrap_or_else(|_| unreachable!("N0CALL-7 is statically valid"))
    }

    #[tokio::test]
    async fn start_enters_kiss_mode() -> TestResult {
        let radio = mock_radio(TncBaud::Bps1200).await?;
        let config = test_config();
        let client = AprsClient::start(radio, config).await.map_err(|(_, e)| e)?;
        assert_eq!(client.config().callsign, "N0CALL");
        assert_eq!(client.config().ssid, 7);
        assert_eq!(client.stations().len(), 0);
        assert_eq!(client.messenger().pending_count(), 0);
        Ok(())
    }

    #[tokio::test]
    async fn stop_exits_kiss_mode() -> TestResult {
        let radio = mock_radio(TncBaud::Bps1200).await?;
        let config = test_config();
        let mut client = AprsClient::start(radio, config).await.map_err(|(_, e)| e)?;

        // Queue the KISS exit frame expectation.
        client.session.transport.expect(&[FEND, 0xFF, FEND], &[]);

        let _radio = client.stop().await.map_err(|(_, e)| e)?;
        Ok(())
    }

    #[tokio::test]
    async fn start_with_invalid_callsign_errors_instead_of_panicking() -> TestResult {
        // Config fields are freely settable, so validation must happen
        // at use — an invalid callsign previously hit an `unreachable!`
        // deep inside start().
        let mock = MockTransport::new();
        let radio = Radio::connect(mock).await?;
        let config = AprsClientConfig::new("N0CALL/P", 7);
        let result = AprsClient::start(radio, config).await;
        assert!(
            matches!(result, Err((_, Error::Validation(_)))),
            "invalid callsign must be a validation error"
        );
        Ok(())
    }

    #[tokio::test]
    async fn start_with_invalid_ssid_errors_instead_of_panicking() -> TestResult {
        let mock = MockTransport::new();
        let radio = Radio::connect(mock).await?;
        let config = AprsClientConfig::new("N0CALL", 99);
        let result = AprsClient::start(radio, config).await;
        assert!(
            matches!(result, Err((_, Error::Validation(_)))),
            "SSID above 15 must be a validation error"
        );
        Ok(())
    }

    #[tokio::test]
    async fn send_message_queues_and_transmits() -> TestResult {
        let radio = mock_radio(TncBaud::Bps1200).await?;
        let config = test_config();
        let mut client = AprsClient::start(radio, config).await.map_err(|(_, e)| e)?;

        // The messenger builds a KISS-encoded wire frame internally.
        // send_message calls send_wire which writes it directly.
        let expected_wire = build_msg(
            &test_address(),
            "W1AW",
            "Hello",
            Some("1"),
            &default_digipeater_path(),
        );
        client.session.transport.expect(&expected_wire, &[]);

        let id = client.send_message("W1AW", "Hello").await?;
        assert_eq!(id, "1");
        assert_eq!(client.messenger().pending_count(), 1);
        Ok(())
    }

    #[tokio::test]
    async fn beacon_position_transmits() -> TestResult {
        let radio = mock_radio(TncBaud::Bps1200).await?;
        let config = test_config();
        let mut client = AprsClient::start(radio, config).await.map_err(|(_, e)| e)?;

        let expected = build_pos(
            &test_address(),
            35.25,
            -97.75,
            '/',
            '>',
            "mobile",
            &default_digipeater_path(),
        );
        client.session.transport.expect(&expected, &[]);

        client.beacon_position(35.25, -97.75, "mobile").await?;
        Ok(())
    }

    #[tokio::test]
    async fn beacon_position_compressed_transmits() -> TestResult {
        let radio = mock_radio(TncBaud::Bps1200).await?;
        let config = test_config();
        let mut client = AprsClient::start(radio, config).await.map_err(|(_, e)| e)?;

        let expected = build_aprs_position_compressed(
            &test_address(),
            35.25,
            -97.75,
            '/',
            '>',
            "compressed",
            &default_digipeater_path(),
        );
        client.session.transport.expect(&expected, &[]);

        client
            .beacon_position_compressed(35.25, -97.75, "compressed")
            .await?;
        Ok(())
    }

    #[tokio::test]
    async fn send_status_transmits() -> TestResult {
        let radio = mock_radio(TncBaud::Bps1200).await?;
        let config = test_config();
        let mut client = AprsClient::start(radio, config).await.map_err(|(_, e)| e)?;

        let expected = build_aprs_status(&test_address(), "On the air", &default_digipeater_path());
        client.session.transport.expect(&expected, &[]);

        client.send_status("On the air").await?;
        Ok(())
    }

    #[tokio::test]
    async fn send_object_transmits() -> TestResult {
        let radio = mock_radio(TncBaud::Bps1200).await?;
        let config = test_config();
        let mut client = AprsClient::start(radio, config).await.map_err(|(_, e)| e)?;

        let expected = build_aprs_object(
            &test_address(),
            "Marathon",
            true,
            35.0,
            -97.0,
            '/',
            '>',
            "5K run",
            &default_digipeater_path(),
        );
        client.session.transport.expect(&expected, &[]);

        client
            .send_object("Marathon", true, 35.0, -97.0, "5K run")
            .await?;
        Ok(())
    }

    #[test]
    fn config_builder_valid() -> TestResult {
        let cfg = AprsClientConfig::builder("N0CALL", 9)
            .symbol('/', '>')
            .beacon_comment("test")
            .auto_ack(false)
            .max_stations(100)
            .build()?;
        assert_eq!(cfg.callsign, "N0CALL");
        assert_eq!(cfg.ssid, 9);
        assert_eq!(cfg.symbol_table, '/');
        assert_eq!(cfg.symbol_code, '>');
        assert_eq!(cfg.beacon_comment, "test");
        assert!(!cfg.auto_ack);
        assert_eq!(cfg.max_stations, 100);
        Ok(())
    }

    #[test]
    fn config_builder_rejects_bad_callsign() {
        assert!(AprsClientConfig::builder("", 0).build().is_err());
        assert!(AprsClientConfig::builder("TOOLONG", 0).build().is_err());
    }

    #[test]
    fn config_builder_rejects_bad_ssid() {
        assert!(AprsClientConfig::builder("N0CALL", 16).build().is_err());
    }

    #[test]
    fn config_builder_rejects_bad_symbol_table() {
        assert!(
            AprsClientConfig::builder("N0CALL", 0)
                .symbol('!', '>')
                .build()
                .is_err()
        );
    }

    #[test]
    fn config_defaults() {
        let config = AprsClientConfig::new("W1AW", 0);
        assert_eq!(config.callsign, "W1AW");
        assert_eq!(config.ssid, 0);
        assert_eq!(config.symbol_table, '/');
        assert_eq!(config.symbol_code, '>');
        assert!(config.auto_ack);
        assert!(config.digipeater.is_none());
        assert_eq!(config.max_stations, 500);
        assert_eq!(config.station_timeout_secs, 3600);
    }

    #[test]
    fn config_my_address() -> TestResult {
        let config = AprsClientConfig::new("KQ4NIT", 9);
        let addr = config.my_address()?;
        assert_eq!(addr.callsign, "KQ4NIT");
        assert_eq!(addr.ssid, 9);
        Ok(())
    }

    #[test]
    fn aprs_event_debug_formatting() {
        let event = AprsEvent::MessageDelivered("42".to_owned());
        let debug = format!("{event:?}");
        assert!(debug.contains("MessageDelivered"));
        assert!(debug.contains("42"));
    }

    #[test]
    fn aprs_client_debug_formatting() {
        // Cannot construct AprsClient without async, but we can verify
        // the config formatting.
        let config = test_config();
        let debug = format!("{config:?}");
        assert!(debug.contains("N0CALL"));
    }

    // -----------------------------------------------------------------------
    // IGate tests
    // -----------------------------------------------------------------------

    fn make_test_packet(source: &str, dest: &str, digis: &[&str], info: &[u8]) -> Ax25Packet {
        // Parse each digi string as "CALL-SSID" or bare "CALL".
        let parse_digi = |s: &str| -> Option<RouteEntry> {
            let (call, ssid) = s
                .split_once('-')
                .map_or((s, 0u8), |(c, ssid_str)| (c, ssid_str.parse().unwrap_or(0)));
            RouteEntry::new(call, ssid).ok()
        };
        Ax25Packet {
            source: Ax25Address::new(source, 0)
                .unwrap_or_else(|_| unreachable!("test fixture source is valid")),
            destination: Ax25Address::new(dest, 0)
                .unwrap_or_else(|_| unreachable!("test fixture dest is valid")),
            digipeaters: digis.iter().filter_map(|d| parse_digi(d)).collect(),
            command_or_response: Some(CommandResponse::Command),
            control: 0x03,
            protocol: 0xF0,
            info: info.to_vec(),
        }
    }

    #[tokio::test]
    async fn format_for_is_basic() -> TestResult {
        let radio = mock_radio(TncBaud::Bps1200).await?;
        let config = test_config();
        let client = AprsClient::start(radio, config).await.map_err(|(_, e)| e)?;

        let packet = make_test_packet("W1AW", "APK005", &["WIDE1-1"], b"!4903.50N/07201.75W-");
        let is_line = client.format_for_is(&packet);

        assert!(is_line.starts_with("W1AW>APK005,WIDE1-1,qAR,N0CALL-7:"));
        assert!(is_line.ends_with("\r\n"));
        assert!(is_line.contains("!4903.50N/07201.75W-"));
        Ok(())
    }

    #[tokio::test]
    async fn format_for_is_no_digipeaters() -> TestResult {
        let radio = mock_radio(TncBaud::Bps1200).await?;
        let config = test_config();
        let client = AprsClient::start(radio, config).await.map_err(|(_, e)| e)?;

        let packet = make_test_packet("W1AW", "APK005", &[], b"!4903.50N/07201.75W-");
        let is_line = client.format_for_is(&packet);

        assert!(is_line.starts_with("W1AW>APK005,qAR,N0CALL-7:"));
        Ok(())
    }

    #[test]
    fn should_gate_to_is_normal_packet() {
        let packet = make_test_packet("W1AW", "APK005", &["WIDE1-1"], b"!4903.50N/07201.75W-");
        assert!(AprsClient::<MockTransport>::should_gate_to_is(&packet));
    }

    #[test]
    fn should_gate_to_is_blocks_tcpip_source() {
        let packet = make_test_packet("TCPIP", "APK005", &[], b"!4903.50N/07201.75W-");
        assert!(!AprsClient::<MockTransport>::should_gate_to_is(&packet));
    }

    #[test]
    fn should_gate_to_is_blocks_tcpxx_source() {
        let packet = make_test_packet("TCPXX", "APK005", &[], b"!4903.50N/07201.75W-");
        assert!(!AprsClient::<MockTransport>::should_gate_to_is(&packet));
    }

    #[test]
    fn should_gate_to_is_blocks_third_party() {
        let packet = make_test_packet("W1AW", "APK005", &[], b"}W2AW>APK005:!4903.50N/07201.75W-");
        assert!(!AprsClient::<MockTransport>::should_gate_to_is(&packet));
    }

    #[test]
    fn should_gate_to_is_blocks_nogate_in_path() {
        let packet = make_test_packet("W1AW", "APK005", &["NOGATE"], b"!4903.50N/07201.75W-");
        assert!(!AprsClient::<MockTransport>::should_gate_to_is(&packet));
    }

    #[test]
    fn should_gate_to_is_blocks_rfonly_in_path() {
        let packet = make_test_packet("W1AW", "APK005", &["RFONLY"], b"!4903.50N/07201.75W-");
        assert!(!AprsClient::<MockTransport>::should_gate_to_is(&packet));
    }

    #[tokio::test]
    async fn should_gate_to_rf_rejects_position_reports() -> TestResult {
        let radio = mock_radio(TncBaud::Bps1200).await?;
        let config = test_config();
        let client = AprsClient::start(radio, config).await.map_err(|(_, e)| e)?;

        // Position report (starts with '!') should not be gated to RF.
        let line = "W1AW>APK005,TCPIP:!4903.50N/07201.75W-Test\r\n";
        assert!(!client.should_gate_to_rf(line));
        Ok(())
    }

    #[tokio::test]
    async fn should_gate_to_rf_rejects_nogate_in_path() -> TestResult {
        let radio = mock_radio(TncBaud::Bps1200).await?;
        let config = test_config();
        let client = AprsClient::start(radio, config).await.map_err(|(_, e)| e)?;

        let line = "W1AW>APK005,NOGATE::N0CALL   :Hello{123\r\n";
        assert!(!client.should_gate_to_rf(line));
        Ok(())
    }

    #[tokio::test]
    async fn should_gate_to_rf_requires_heard_station() -> TestResult {
        let radio = mock_radio(TncBaud::Bps1200).await?;
        let config = test_config();
        let client = AprsClient::start(radio, config).await.map_err(|(_, e)| e)?;

        // Message to a station NOT in our station list.
        let line = "W1AW>APK005,TCPIP::UNKNOWN  :Hello{123\r\n";
        assert!(!client.should_gate_to_rf(line));
        Ok(())
    }

    #[tokio::test]
    async fn should_gate_to_rf_accepts_message_to_heard_station() -> TestResult {
        let radio = mock_radio(TncBaud::Bps1200).await?;
        let config = test_config();
        let mut client = AprsClient::start(radio, config).await.map_err(|(_, e)| e)?;

        // Simulate hearing a station on RF.
        client.stations.update(
            "KQ4NIT",
            &AprsData::Status(aprs::AprsStatus {
                text: "on air".to_owned(),
                ..aprs::AprsStatus::default()
            }),
            &[],
            Instant::now(),
        );

        // Message addressed to that station should be gated (no TCPIP
        // marker in the path since the spec forbids gating TCPIP-tagged
        // packets back to RF).
        let line = "W1AW>APK005,qAC,SRV::KQ4NIT   :Hello{123\r\n";
        assert!(client.should_gate_to_rf(line));
        Ok(())
    }

    #[tokio::test]
    async fn should_gate_to_rf_rejects_tcpip_marker() -> TestResult {
        let radio = mock_radio(TncBaud::Bps1200).await?;
        let config = test_config();
        let mut client = AprsClient::start(radio, config).await.map_err(|(_, e)| e)?;
        // Even with a heard addressee, TCPIP-marked packets must NOT
        // be gated back to RF (APRS-IS spec).
        client.stations.update(
            "KQ4NIT",
            &AprsData::Status(aprs::AprsStatus {
                text: "on air".to_owned(),
                ..aprs::AprsStatus::default()
            }),
            &[],
            Instant::now(),
        );
        let line = "W1AW>APK005,TCPIP::KQ4NIT   :Hello{123\r\n";
        assert!(!client.should_gate_to_rf(line));
        Ok(())
    }

    #[tokio::test]
    async fn gate_from_is_wraps_in_third_party_header() -> TestResult {
        let radio = mock_radio(TncBaud::Bps1200).await?;
        let config = test_config();
        let mut client = AprsClient::start(radio, config).await.map_err(|(_, e)| e)?;

        // Simulate hearing the addressee on RF so gating is allowed.
        client.stations.update(
            "KQ4NIT",
            &AprsData::Status(aprs::AprsStatus {
                text: "on air".to_owned(),
                ..aprs::AprsStatus::default()
            }),
            &[],
            Instant::now(),
        );

        // Expect the KISS frame output (we just need the mock to accept it).
        // The exact bytes depend on the third-party packet encoding.
        // We use a broad expectation: the mock will accept any write.
        client.session.transport.expect_any_write();

        let result = client
            .gate_from_is("W1AW>APK005,qAC,SRV::KQ4NIT   :Hello{123")
            .await?;
        assert!(result);
        Ok(())
    }

    #[tokio::test]
    async fn gate_from_is_packet_wire_shape() -> TestResult {
        // Regression guard for M5: the outer AX.25 frame must carry the
        // IGate's configured RF path (not the old hard-coded
        // `[TCPIP-0]`), and the inner third-party payload must end with
        // `,MYCALL,I:` per APRS 1.0.1 §17 + IGating.aspx. Verified
        // structurally by inspecting the built packet's fields rather
        // than the encoded KISS bytes — fewer brittle assertions, same
        // protocol coverage.
        let radio = mock_radio(TncBaud::Bps1200).await?;
        let config = test_config();
        let mut client = AprsClient::start(radio, config).await.map_err(|(_, e)| e)?;

        // Simulate hearing the addressee on RF so gating is allowed.
        client.stations.update(
            "KQ4NIT",
            &AprsData::Status(aprs::AprsStatus {
                text: "on air".to_owned(),
                ..aprs::AprsStatus::default()
            }),
            &[],
            Instant::now(),
        );

        let is_line = "W1AW>APK005,qAC,SRV::KQ4NIT   :Hello{123";
        let packet = client
            .build_gate_from_is_packet(is_line)
            .ok_or("gate packet not built — expected Some")?;

        // Outer source is the IGate's callsign + SSID.
        assert_eq!(packet.source.callsign.as_str(), "N0CALL");
        assert_eq!(packet.source.ssid.get(), 7);
        // Outer destination is the TH-D75 tocall, matching this crate's
        // other beacon transmissions.
        assert_eq!(packet.destination.callsign.as_str(), "APK005");
        // Outer path is the IGate's configured RF path (default
        // WIDE1-1,WIDE2-1 from AprsClientConfig::new). It is NOT the
        // bogus `[TCPIP-0]` singleton used by pre-fix code.
        assert!(
            !packet.digipeaters.is_empty(),
            "expected non-empty RF path, got empty"
        );
        for digi in &packet.digipeaters {
            assert_ne!(
                digi.address.callsign.as_str(),
                "TCPIP",
                "TCPIP must not appear in the outer RF path"
            );
        }
        // Inner third-party payload starts with `}`, ends with
        // `,MYCALL,I:data`. Decoding via lossy is safe — the payload is
        // ASCII by construction here.
        let info_str = String::from_utf8_lossy(&packet.info);
        assert!(
            info_str.starts_with('}'),
            "expected `}}` prefix: {info_str}"
        );
        let expected_marker = ",N0CALL-7,I:";
        assert!(
            info_str.contains(expected_marker),
            "expected `{expected_marker}` marker in info: {info_str}"
        );
        // Original payload data ("Hello{123") survives unchanged.
        assert!(
            info_str.ends_with("Hello{123"),
            "expected unchanged trailing data: {info_str}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn gate_from_is_filters_position_report() -> TestResult {
        let radio = mock_radio(TncBaud::Bps1200).await?;
        let config = test_config();
        let mut client = AprsClient::start(radio, config).await.map_err(|(_, e)| e)?;

        // Position report should not be gated to RF.
        let result = client
            .gate_from_is("W1AW>APK005,TCPIP:!4903.50N/07201.75W-Test")
            .await?;
        assert!(!result);
        Ok(())
    }

    // -----------------------------------------------------------------------
    // next_event dispatch tests
    // -----------------------------------------------------------------------

    /// Build a KISS-encoded data frame from a source callsign and APRS info.
    fn build_kiss_data_frame(source: &str, ssid: u8, info: &[u8]) -> Vec<u8> {
        let packet = Ax25Packet {
            source: Ax25Address::new(source, ssid)
                .unwrap_or_else(|_| unreachable!("test fixture source is valid")),
            destination: Ax25Address::new("APK005", 0)
                .unwrap_or_else(|_| unreachable!("APK005 is statically valid")),
            digipeaters: vec![],
            command_or_response: Some(CommandResponse::Command),
            control: 0x03,
            protocol: 0xF0,
            info: info.to_vec(),
        };
        let ax25_bytes = build_ax25(&packet);
        encode_kiss_frame(&KissFrame::data(ax25_bytes))
    }

    #[tokio::test]
    async fn next_event_position_received() -> TestResult {
        let radio = mock_radio(TncBaud::Bps1200).await?;
        let config = test_config();
        let mut client = AprsClient::start(radio, config).await.map_err(|(_, e)| e)?;

        // Uncompressed position: !DDMM.MMN/DDDMM.MMW>comment
        let info = b"!3515.00N/09745.00W>mobile";
        let wire = build_kiss_data_frame("W1AW", 0, info);
        client.session.transport.queue_read(&wire);

        let event = client
            .next_event()
            .await?
            .ok_or("next_event returned Ok(None), expected an event")?;
        match event {
            AprsEvent::StationHeard(entry) => {
                assert_eq!(entry.callsign, "W1AW");
            }
            AprsEvent::PositionReceived { source, .. } => {
                assert_eq!(source, "W1AW");
            }
            other => {
                return Err(
                    format!("expected StationHeard or PositionReceived, got {other:?}").into(),
                );
            }
        }
        Ok(())
    }

    #[tokio::test]
    async fn next_event_weather_received() -> TestResult {
        let radio = mock_radio(TncBaud::Bps1200).await?;
        let config = test_config();
        let mut client = AprsClient::start(radio, config).await.map_err(|(_, e)| e)?;

        // Position + weather report: !DDMM.MMN/DDDMM.MMW_DIR/SPDgGUSTt072
        let info = b"!3515.00N/09745.00W_090/010g015t072";
        let wire = build_kiss_data_frame("WX1STA", 0, info);
        client.session.transport.queue_read(&wire);

        // Observation-order contract (see `dispatch_event` for the
        // rationale): a position-with-weather packet emits two events
        // — `StationHeard` first ("we saw this station"), then
        // `WeatherReceived` ("here is the payload"). Earlier code
        // generations emitted these in the opposite order; the
        // current order matches every other arm of `dispatch_event`
        // and avoids losing `StationHeard` if the caller stops
        // polling between the two events.
        let first = client
            .next_event()
            .await?
            .ok_or("next_event returned Ok(None) for first event")?;
        let first_dump = format!("{first:?}");
        let AprsEvent::StationHeard(entry) = first else {
            return Err(format!("expected StationHeard first, got {first_dump}").into());
        };
        assert_eq!(entry.callsign, "WX1STA");

        let second = client
            .next_event()
            .await?
            .ok_or("next_event returned Ok(None) for second event")?;
        let second_dump = format!("{second:?}");
        let AprsEvent::WeatherReceived { source, weather } = second else {
            return Err(format!("expected WeatherReceived second, got {second_dump}").into());
        };
        assert_eq!(source, "WX1STA");
        assert_eq!(weather.wind_direction, Some(90));
        assert_eq!(weather.wind_speed, Some(10));
        assert_eq!(weather.wind_gust, Some(15));
        assert_eq!(weather.temperature, Some(72));
        Ok(())
    }

    #[tokio::test]
    async fn next_event_message_received() -> TestResult {
        let radio = mock_radio(TncBaud::Bps1200).await?;
        let mut config = test_config();
        config.auto_ack = false; // Disable auto-ack to simplify test
        let mut client = AprsClient::start(radio, config).await.map_err(|(_, e)| e)?;

        // APRS message: :ADDRESSEE:message text{id
        let info = b":N0CALL   :Hello from W1AW{42";
        let wire = build_kiss_data_frame("W1AW", 0, info);
        client.session.transport.queue_read(&wire);

        let event = client
            .next_event()
            .await?
            .ok_or("next_event returned Ok(None), expected a message event")?;
        let AprsEvent::MessageReceived(msg) = &event else {
            return Err(format!("expected MessageReceived, got {event:?}").into());
        };
        assert_eq!(msg.addressee, "N0CALL");
        assert!(msg.text.contains("Hello from W1AW"));
        Ok(())
    }

    #[tokio::test]
    async fn next_event_message_delivered() -> TestResult {
        let radio = mock_radio(TncBaud::Bps1200).await?;
        let config = test_config();
        let mut client = AprsClient::start(radio, config).await.map_err(|(_, e)| e)?;

        // First, send a message so we have a pending message with id "1"
        let expected_wire = build_msg(
            &test_address(),
            "W1AW",
            "Test",
            Some("1"),
            &default_digipeater_path(),
        );
        client.session.transport.expect(&expected_wire, &[]);
        let _id = client.send_message("W1AW", "Test").await?;

        // Now simulate receiving an ack for that message
        let info = b":N0CALL   :ack1";
        let wire = build_kiss_data_frame("W1AW", 0, info);
        client.session.transport.queue_read(&wire);

        let event = client
            .next_event()
            .await?
            .ok_or("next_event returned Ok(None), expected a delivery event")?;
        let AprsEvent::MessageDelivered(id) = &event else {
            return Err(format!("expected MessageDelivered, got {event:?}").into());
        };
        assert_eq!(id, "1");
        Ok(())
    }

    #[tokio::test]
    async fn next_event_message_rejected() -> TestResult {
        let radio = mock_radio(TncBaud::Bps1200).await?;
        let config = test_config();
        let mut client = AprsClient::start(radio, config).await.map_err(|(_, e)| e)?;

        // Send a message to have pending id "1"
        let expected_wire = build_msg(
            &test_address(),
            "W1AW",
            "Test",
            Some("1"),
            &default_digipeater_path(),
        );
        client.session.transport.expect(&expected_wire, &[]);
        let _id = client.send_message("W1AW", "Test").await?;

        // Simulate receiving a rejection
        let info = b":N0CALL   :rej1";
        let wire = build_kiss_data_frame("W1AW", 0, info);
        client.session.transport.queue_read(&wire);

        let event = client
            .next_event()
            .await?
            .ok_or("next_event returned Ok(None), expected a rejection event")?;
        let AprsEvent::MessageRejected(id) = &event else {
            return Err(format!("expected MessageRejected, got {event:?}").into());
        };
        assert_eq!(id, "1");
        Ok(())
    }

    #[tokio::test]
    async fn next_event_raw_packet_for_unknown_data() -> TestResult {
        let radio = mock_radio(TncBaud::Bps1200).await?;
        let config = test_config();
        let mut client = AprsClient::start(radio, config).await.map_err(|(_, e)| e)?;

        // Send some unparseable APRS data (random info bytes)
        let info = b"XUNKNOWN_DATA_TYPE";
        let wire = build_kiss_data_frame("W1AW", 0, info);
        client.session.transport.queue_read(&wire);

        let event = client
            .next_event()
            .await?
            .ok_or("next_event returned Ok(None), expected a raw packet event")?;
        let AprsEvent::RawPacket(pkt) = &event else {
            return Err(format!("expected RawPacket, got {event:?}").into());
        };
        assert_eq!(pkt.source.callsign, "W1AW");
        Ok(())
    }

    #[tokio::test]
    async fn typed_event_still_exposes_raw_frame_for_igate() -> TestResult {
        // Regression: an IGate must gate EVERY heard packet, but a standard
        // position report surfaces as a typed event (StationHeard /
        // PositionReceived), not RawPacket. `take_last_rf_packet` must still
        // hand back the underlying frame so the IGate can forward it.
        let radio = mock_radio(TncBaud::Bps1200).await?;
        let config = test_config();
        let mut client = AprsClient::start(radio, config).await.map_err(|(_, e)| e)?;

        let info = b"!3515.00N/09745.00W>mobile";
        let wire = build_kiss_data_frame("W1AW", 7, info);
        client.session.transport.queue_read(&wire);

        let event = client
            .next_event()
            .await?
            .ok_or("next_event returned Ok(None), expected a typed event")?;
        assert!(
            !matches!(event, AprsEvent::RawPacket(_)),
            "a plain position must classify as a typed event, got {event:?}"
        );

        // The raw frame for this cycle is available for gating, and taking
        // it leaves None behind.
        let pkt = client
            .take_last_rf_packet()
            .ok_or("take_last_rf_packet returned None after a received frame")?;
        assert_eq!(pkt.source.callsign, "W1AW");
        assert_eq!(pkt.source.ssid.get(), 7);
        assert!(
            client.take_last_rf_packet().is_none(),
            "take_last_rf_packet must consume the frame"
        );
        Ok(())
    }

    #[tokio::test]
    async fn idle_cycle_leaves_no_raw_frame_to_gate() -> TestResult {
        // An idle cycle (no frame received) must not leave a stale frame
        // that an IGate would re-gate against an unrelated event.
        let radio = mock_radio(TncBaud::Bps1200).await?;
        let config = test_config();
        let mut client = AprsClient::start(radio, config).await.map_err(|(_, e)| e)?;
        let _idle_event = client.next_event().await?;
        assert!(
            client.take_last_rf_packet().is_none(),
            "idle cycle must not expose a frame to gate"
        );
        Ok(())
    }

    #[tokio::test]
    async fn next_event_returns_none_when_idle() -> TestResult {
        // With no incoming frames the event loop should return Ok(None)
        // after the receive timeout, indicating the caller can sleep
        // before the next iteration. We don't use tokio::time::pause()
        // here because the underlying mock transport returns WouldBlock
        // immediately, which the session converts to a Timeout error,
        // which next_event maps to Ok(None) without ever sleeping.
        let radio = mock_radio(TncBaud::Bps1200).await?;
        let config = test_config();
        let mut client = AprsClient::start(radio, config).await.map_err(|(_, e)| e)?;
        let event = client.next_event().await?;
        assert!(event.is_none(), "expected Ok(None) on idle, got {event:?}");
        Ok(())
    }

    // -----------------------------------------------------------------------
    // update_motion tests
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn update_motion_first_call_triggers_beacon() -> TestResult {
        let radio = mock_radio(TncBaud::Bps1200).await?;
        let config = test_config();
        let mut client = AprsClient::start(radio, config).await.map_err(|(_, e)| e)?;

        // SmartBeaconing always triggers on first call.
        let expected = build_pos(
            &test_address(),
            35.25,
            -97.75,
            '/',
            '>',
            "",
            &default_digipeater_path(),
        );
        client.session.transport.expect(&expected, &[]);

        let beaconed = client.update_motion(50.0, 90.0, 35.25, -97.75).await?;
        assert!(beaconed);
        Ok(())
    }

    #[tokio::test]
    async fn update_motion_second_call_no_beacon() -> TestResult {
        let radio = mock_radio(TncBaud::Bps1200).await?;
        let config = test_config();
        let mut client = AprsClient::start(radio, config).await.map_err(|(_, e)| e)?;

        // First call beacons.
        let expected = build_pos(
            &test_address(),
            35.25,
            -97.75,
            '/',
            '>',
            "",
            &default_digipeater_path(),
        );
        client.session.transport.expect(&expected, &[]);
        let _ = client.update_motion(50.0, 90.0, 35.25, -97.75).await?;

        // Second call immediately after should NOT beacon.
        let beaconed = client.update_motion(50.0, 90.0, 35.25, -97.75).await?;
        assert!(!beaconed);
        Ok(())
    }

    #[tokio::test]
    async fn beacon_position_mice_sends_expected_wire_bytes() -> TestResult {
        let radio = mock_radio(TncBaud::Bps1200).await?;
        let config = test_config();
        let mut client = AprsClient::start(radio, config).await.map_err(|(_, e)| e)?;

        let expected = build_aprs_mice(
            &test_address(),
            35.30,
            -82.46,
            25,
            90,
            '/',
            '>',
            "mice hw validation",
            &default_digipeater_path(),
        );
        client.session.transport.expect(&expected, &[]);

        client
            .beacon_position_mice(35.30, -82.46, 25, 90, "mice hw validation")
            .await?;
        Ok(())
    }
}
