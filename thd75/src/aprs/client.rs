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
//! mode and returns the [`Radio`]. Before issuing another CAT operation,
//! call [`Radio::restore_cat_after_mode_exit`] to prove that no binary KISS
//! residue remains. This is the same ownership pattern used by [`KissSession`] and
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
//! use kenwood_thd75::{
//!     AprsClient, AprsClientConfig, Ax25Address, Latitude, Longitude,
//!     PositionReportText, Radio,
//! };
//! use kenwood_thd75::transport::SerialTransport;
//!
//! # async fn example() -> Result<(), Box<dyn std::error::Error>> {
//! let transport = SerialTransport::open("/dev/cu.usbmodem1234")?;
//! let radio = Radio::new(transport);
//!
//! let station = Ax25Address::new("N0CALL", 7)?;
//! let config = AprsClientConfig::new(station)?;
//! let mut client = AprsClient::start(radio, config).await.map_err(|(_, e)| e)?;
//!
//! // Send a message
//! let addressee = kenwood_thd75::MessageAddressee::new("KQ4NIT")?;
//! let text = kenwood_thd75::MessageText::new("Hello!")?;
//! client.send_message(&addressee, &text).await?;
//!
//! // Beacon position
//! let position_text = PositionReportText::new("On the road")?;
//! client
//!     .beacon_position(
//!         Latitude::new(35.25)?,
//!         Longitude::new(-97.75)?,
//!         &position_text,
//!     )
//!     .await?;
//!
//! // Process incoming packets. None is one quiet poll, not end-of-stream.
//! loop {
//!     if let Some(event) = client.next_event().await? {
//!       match event {
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
//!       }
//!     }
//! }
//!
//! // Clean shutdown: exit KISS, then prove ordinary CAT framing.
//! let mut radio = client.stop().await.map_err(|(_client, e)| e)?;
//! radio.restore_cat_after_mode_exit().await?;
//! # Ok(())
//! # }
//! ```

use std::collections::{HashMap, HashSet, VecDeque};
use std::num::NonZeroUsize;
use std::time::{Duration, Instant};

use aprs::{
    AprsData, AprsMessage, AprsMessenger, AprsPosition, AprsPositionlessWeatherReport,
    AprsReportTimestamp, AprsStatusTimestamp, AprsSymbol, CompressedPositionText, Course,
    DigiAction, DigipeaterConfig, Heading, Latitude, Longitude, MessageAddressee, MessageId,
    MessageKind, MessageText, MiceSpeed, MiceStatusText, ObjectName, PositionReportText,
    SmartBeaconing, SmartBeaconingConfig, Speed, StationEntry, StationList, StatusText,
    TimestampedStatusText, build_aprs_mice, build_aprs_object, build_aprs_position_compressed,
    build_aprs_position_report, build_aprs_status, build_aprs_timestamped_status,
    build_query_response_position, classify_ack_rej, parse_aprs_data_full, parse_aprs_message,
};
use aprs_is::{
    AprsIsLine, AprsIsPathElement, AprsIsUplinkLine, IGateFormatError, Passcode,
    igate_format_packet_for_is,
};
use ax25_codec::{
    Ax25Address, Ax25Packet, Ax25Pid, CommandResponse, DigipeaterPath, build_ax25, parse_ax25,
};
use kiss_tnc::{KissCommand, KissFrame, encode_kiss_frame};

use crate::aprs::ax25_to_kiss_wire;
use crate::error::Error;
use crate::radio::Radio;
use crate::radio::kiss_session::KissSession;
use crate::transport::Transport;
use crate::types::PacketDataRate;

/// Default receive timeout for `next_event` polling (500 ms).
///
/// Short enough to keep the event loop responsive for retries and
/// beacons, long enough to avoid busy-spinning on a quiet channel.
const EVENT_POLL_TIMEOUT: Duration = Duration::from_millis(500);

/// Maximum number of bytes in an AX.25 information field.
///
/// APRS builders elsewhere in the workspace use the same 256-octet boundary;
/// Internet-to-RF wrapping must apply it to the *completed* third-party
/// payload, not merely to the original APRS-IS information bytes.
const MAX_AX25_INFORMATION_BYTES: usize = 256;

/// Bounded, time-aware identity history used by `IGate` policy.
///
/// APRS-IS deliberately leaves several eligibility windows to the sysop.
/// Keeping those observations separate from [`StationList`] matters because
/// the station list stores only successfully parsed APRS payloads and indexes
/// by base callsign, while `IGate` policy applies to every valid AX.25 source
/// and distinguishes SSIDs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum IdentityHistoryOverflow {
    /// Forget the oldest identity. Appropriate for a positive prerequisite,
    /// where forgetting can only reject a transmission.
    EvictOldest,
    /// Conservatively match every identity for one complete history window.
    /// Appropriate for a blocking predicate, where eviction would otherwise
    /// make a transmission eligible.
    MatchAllUntilExpiry,
}

#[derive(Debug)]
struct RecentIdentities {
    last_heard: HashMap<String, Instant>,
    max_entries: NonZeroUsize,
    max_age: Duration,
    overflow: IdentityHistoryOverflow,
    match_all_since: Option<Instant>,
}

impl RecentIdentities {
    fn new(
        max_entries: NonZeroUsize,
        max_age: Duration,
        overflow: IdentityHistoryOverflow,
    ) -> Self {
        Self {
            last_heard: HashMap::new(),
            max_entries,
            max_age,
            overflow,
            match_all_since: None,
        }
    }

    fn record(&mut self, identity: &str, now: Instant) {
        self.purge_expired(now);
        let identity = identity.to_ascii_uppercase();
        if let Some(previous) = self.last_heard.get_mut(&identity) {
            *previous = now;
            return;
        }

        if self.last_heard.len() >= self.max_entries.get() {
            match self.overflow {
                IdentityHistoryOverflow::EvictOldest => {
                    if let Some(oldest) = self
                        .last_heard
                        .iter()
                        .min_by_key(|(_, heard)| *heard)
                        .map(|(identity, _)| identity.clone())
                    {
                        let _removed = self.last_heard.remove(&oldest);
                    }
                }
                IdentityHistoryOverflow::MatchAllUntilExpiry => {
                    self.match_all_since = Some(now);
                    return;
                }
            }
        }
        let _previous = self.last_heard.insert(identity, now);
    }

    fn contains_at(&self, identity: &str, now: Instant) -> bool {
        if self
            .match_all_since
            .is_some_and(|since| now.saturating_duration_since(since) < self.max_age)
        {
            return true;
        }
        self.last_heard
            .get(&identity.to_ascii_uppercase())
            .is_some_and(|heard| now.saturating_duration_since(*heard) < self.max_age)
    }

    fn purge_expired(&mut self, now: Instant) {
        let max_age = self.max_age;
        self.last_heard
            .retain(|_, heard| now.saturating_duration_since(*heard) < max_age);
        if self
            .match_all_since
            .is_some_and(|since| now.saturating_duration_since(since) >= max_age)
        {
            self.match_all_since = None;
        }
    }
}

/// Explicit RF locality rule for Internet-to-RF `IGate` eligibility.
///
/// AX.25 route entries carry a has-been-repeated bit. The maximum below is
/// compared with the number of repeated entries, allowing a sysop to define
/// local coverage by digipeater hops as required by APRS-IS. A value of zero
/// accepts only packets heard directly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IGateRfLocality {
    maximum_repeated_hops: u8,
}

impl IGateRfLocality {
    /// Direct RF reception only.
    pub const DIRECT: Self = Self {
        maximum_repeated_hops: 0,
    };

    /// Construct a locality rule allowing at most `maximum_repeated_hops`.
    ///
    /// # Errors
    ///
    /// Returns [`crate::error::ValidationError::IGateLocalityOutOfRange`] for
    /// values above the eight-entry AX.25 digipeater-path maximum.
    pub const fn new(maximum_repeated_hops: u8) -> Result<Self, crate::error::ValidationError> {
        if maximum_repeated_hops > 8 {
            return Err(crate::error::ValidationError::IGateLocalityOutOfRange {
                maximum: maximum_repeated_hops,
            });
        }
        Ok(Self {
            maximum_repeated_hops,
        })
    }

    /// Maximum accepted number of repeated AX.25 path entries.
    #[must_use]
    pub const fn maximum_repeated_hops(self) -> u8 {
        self.maximum_repeated_hops
    }

    fn includes(self, packet: &Ax25Packet) -> bool {
        packet
            .digipeaters
            .iter()
            .filter(|entry| entry.has_repeated)
            .count()
            <= usize::from(self.maximum_repeated_hops)
    }
}

/// Deliberate operator policy for APRS-IS to RF gating.
///
/// APRS-IS defines three independent recency tests but intentionally leaves
/// their exact periods to the sysop. This type requires all three values so a
/// host cannot begin unattended RF transmission under an implicit library
/// default.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IGateToRfConfig {
    receiver_locality: IGateRfLocality,
    receiver_rf_max_age: Duration,
    sender_rf_quiet_period: Duration,
    receiver_internet_quiet_period: Duration,
}

impl IGateToRfConfig {
    /// Construct an explicit Internet-to-RF eligibility policy.
    ///
    /// `receiver_locality` defines local range by repeated digipeater hops.
    /// `receiver_rf_max_age` is how recently the message addressee must have
    /// been heard within that range. `sender_rf_quiet_period` and
    /// `receiver_internet_quiet_period` suppress redundant Internet-to-RF
    /// traffic after those respective observations.
    ///
    /// # Errors
    ///
    /// Returns [`crate::error::ValidationError::IGatePeriodOutOfRange`] if any
    /// period is zero, or if `receiver_rf_max_age` exceeds the APRS-IS maximum
    /// recommendation of one hour.
    pub fn new(
        receiver_locality: IGateRfLocality,
        receiver_rf_max_age: Duration,
        sender_rf_quiet_period: Duration,
        receiver_internet_quiet_period: Duration,
    ) -> Result<Self, crate::error::ValidationError> {
        if receiver_rf_max_age.is_zero() || receiver_rf_max_age > Duration::from_secs(60 * 60) {
            return Err(crate::error::ValidationError::IGatePeriodOutOfRange {
                field: "receiver RF maximum age",
                value: receiver_rf_max_age,
                detail: "must be greater than zero and no more than 1 hour",
            });
        }
        if sender_rf_quiet_period.is_zero() {
            return Err(crate::error::ValidationError::IGatePeriodOutOfRange {
                field: "sender RF quiet",
                value: sender_rf_quiet_period,
                detail: "must be greater than zero",
            });
        }
        if receiver_internet_quiet_period.is_zero() {
            return Err(crate::error::ValidationError::IGatePeriodOutOfRange {
                field: "receiver Internet quiet",
                value: receiver_internet_quiet_period,
                detail: "must be greater than zero",
            });
        }
        Ok(Self {
            receiver_locality,
            receiver_rf_max_age,
            sender_rf_quiet_period,
            receiver_internet_quiet_period,
        })
    }

    /// RF path locality used when deciding whether a receiver is in range.
    #[must_use]
    pub const fn receiver_locality(self) -> IGateRfLocality {
        self.receiver_locality
    }

    /// Maximum age of a local RF observation of the message receiver.
    #[must_use]
    pub const fn receiver_rf_max_age(self) -> Duration {
        self.receiver_rf_max_age
    }

    /// Period for suppressing a sender recently heard directly on RF.
    #[must_use]
    pub const fn sender_rf_quiet_period(self) -> Duration {
        self.sender_rf_quiet_period
    }

    /// Period for suppressing a receiver recently heard via the Internet.
    #[must_use]
    pub const fn receiver_internet_quiet_period(self) -> Duration {
        self.receiver_internet_quiet_period
    }
}

#[derive(Debug)]
struct PendingAssociatedPosition {
    receivers: HashSet<String>,
    requested_at: Instant,
}

#[derive(Debug)]
struct IGateToRfState {
    receiver_locality: IGateRfLocality,
    rf_heard: RecentIdentities,
    direct_rf_heard: RecentIdentities,
    internet_heard: RecentIdentities,
    pending_positions: HashMap<String, PendingAssociatedPosition>,
    max_pending_positions: NonZeroUsize,
}

impl IGateToRfState {
    fn new(config: IGateToRfConfig, max_entries: NonZeroUsize) -> Self {
        Self {
            receiver_locality: config.receiver_locality(),
            rf_heard: RecentIdentities::new(
                max_entries,
                config.receiver_rf_max_age(),
                IdentityHistoryOverflow::EvictOldest,
            ),
            direct_rf_heard: RecentIdentities::new(
                max_entries,
                config.sender_rf_quiet_period(),
                IdentityHistoryOverflow::MatchAllUntilExpiry,
            ),
            internet_heard: RecentIdentities::new(
                max_entries,
                config.receiver_internet_quiet_period(),
                IdentityHistoryOverflow::MatchAllUntilExpiry,
            ),
            pending_positions: HashMap::new(),
            max_pending_positions: max_entries,
        }
    }

    fn remember_associated_position(&mut self, sender: &str, receiver: &str, now: Instant) {
        let sender = sender.to_ascii_uppercase();
        let receiver = receiver.to_ascii_uppercase();
        let pending =
            self.pending_positions
                .entry(sender)
                .or_insert_with(|| PendingAssociatedPosition {
                    receivers: HashSet::new(),
                    requested_at: now,
                });
        let _already_present = pending.receivers.insert(receiver);
        pending.requested_at = now;

        if self.pending_positions.len() > self.max_pending_positions.get()
            && let Some(oldest) = self
                .pending_positions
                .iter()
                .min_by_key(|(_, pending)| pending.requested_at)
                .map(|(sender, _)| sender.clone())
        {
            let _removed = self.pending_positions.remove(&oldest);
        }
    }

    fn take_associated_receivers(&mut self, sender: &str) -> Option<HashSet<String>> {
        self.pending_positions
            .remove(&sender.to_ascii_uppercase())
            .map(|pending| pending.receivers)
    }

    fn purge_expired(&mut self, now: Instant) {
        self.rf_heard.purge_expired(now);
        self.direct_rf_heard.purge_expired(now);
        self.internet_heard.purge_expired(now);
    }
}

#[derive(Debug)]
enum IGateToRfCandidate {
    Message { sender: String, receiver: String },
    AssociatedPosition,
}

/// Configuration for an [`AprsClient`] session.
///
/// Created with [`AprsClientConfig::new`] or the fluent
/// [`AprsClientConfig::builder`]. Every field whose wire representation has
/// semantic constraints is stored as a validated type, and fields are private
/// so callers cannot invalidate a configuration after construction.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct AprsClientConfig {
    source: Ax25Address,
    symbol: AprsSymbol,
    /// Packet data rate. Default: 1200 bps (AFSK).
    data_rate: PacketDataRate,
    /// Default comment appended to position beacons.
    beacon_comment: PositionReportText,
    /// `SmartBeaconing` algorithm configuration.
    smart_beaconing: SmartBeaconingConfig,
    /// Optional digipeater configuration. When set, incoming packets
    /// are evaluated for relay according to the digipeater rules.
    digipeater: Option<DigipeaterConfig>,
    /// Shared station-list and `IGate` identity-history capacity. Default: 500.
    max_stations: NonZeroUsize,
    /// Time before a station entry expires. Default: 1 hour.
    station_timeout: Duration,
    /// Explicit policy enabling APRS-IS to RF transmission.
    ///
    /// `None` is fail-closed: RF-to-IS remains available, but Internet
    /// packets are never transmitted on RF.
    igate_to_rf: Option<IGateToRfConfig>,
    /// Automatically acknowledge incoming messages addressed to us.
    /// Default: `true`.
    auto_ack: bool,
    /// Digipeater path for outgoing packets.
    ///
    /// Default: `WIDE1-1,WIDE2-1` (standard 2-hop path). Use an empty
    /// path for direct transmission with no digipeating. Parse from
    /// a string with [`crate::aprs::parse_digipeater_path`].
    digipeater_path: DigipeaterPath,
    /// Automatically respond to `?APRSP` position queries addressed to us.
    ///
    /// When set and an incoming message contains `?APRSP`, the client
    /// sends a position beacon in response. Requires
    /// [`auto_query_position`](Self::auto_query_position) to be set.
    ///
    /// Default: `true`.
    auto_query_response: bool,
    /// Cached position for auto query responses, as `(lat, lon)`.
    ///
    /// When `None`, query responses are not sent even if
    /// `auto_query_response` is `true`. Update via
    /// [`AprsClient::set_query_response_position`].
    auto_query_position: Option<(Latitude, Longitude)>,
}

impl AprsClientConfig {
    /// Create a configuration for a validated AX.25 station address.
    ///
    /// - Symbol: car (`/>`)
    /// - Baud: 1200 bps (standard APRS AFSK)
    /// - `SmartBeaconing`: TH-D75A V1.03 defaults (Menu 530-535), normalized
    ///   from the default mi/h setting to km/h
    /// - Station and `IGate` history capacity: 500; station timeout: 1 hour
    /// - Auto-ack: on
    /// - APRS-IS to RF gating: disabled until an [`IGateToRfConfig`] is
    ///   supplied
    ///
    /// # Errors
    ///
    /// Returns [`crate::error::ValidationError`] if the library's standard
    /// outgoing path fails its own validation. The static path is checked
    /// through the public parser rather than silently dropping bad entries.
    pub fn new(source: Ax25Address) -> Result<Self, crate::error::ValidationError> {
        Ok(Self::builder(source)?.build())
    }

    /// Parse a caller-provided callsign and SSID, then create the default
    /// configuration.
    ///
    /// # Errors
    ///
    /// Returns [`crate::error::ValidationError`] if either address component
    /// is invalid or if the standard path cannot be constructed.
    pub fn try_new(callsign: &str, ssid: u8) -> Result<Self, crate::error::ValidationError> {
        Self::new(validate_station_address(callsign, ssid)?)
    }

    /// Start building a configuration with the fluent builder.
    ///
    /// Example:
    ///
    /// ```no_run
    /// use kenwood_thd75::{
    ///     AprsClientConfig, AprsSymbol, Ax25Address, PositionReportText,
    /// };
    /// let station = Ax25Address::new("N0CALL", 9)?;
    /// let config = AprsClientConfig::builder(station)?
    ///     .symbol(AprsSymbol::CAR)
    ///     .beacon_comment(PositionReportText::new("mobile")?)
    ///     .auto_ack(true)
    ///     .build();
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    ///
    /// # Errors
    ///
    /// Returns [`crate::error::ValidationError`] if the standard outgoing
    /// digipeater path cannot be constructed.
    pub fn builder(
        source: Ax25Address,
    ) -> Result<AprsClientConfigBuilder, crate::error::ValidationError> {
        AprsClientConfigBuilder::new(source)
    }

    /// Parse a callsign and SSID, then start the fluent builder.
    ///
    /// # Errors
    ///
    /// Returns [`crate::error::ValidationError`] if the station address or
    /// standard outgoing path is invalid.
    pub fn try_builder(
        callsign: &str,
        ssid: u8,
    ) -> Result<AprsClientConfigBuilder, crate::error::ValidationError> {
        Self::builder(validate_station_address(callsign, ssid)?)
    }

    /// Return this station's validated AX.25 source address.
    #[must_use]
    pub const fn source(&self) -> &Ax25Address {
        &self.source
    }

    /// Return the configured APRS symbol.
    #[must_use]
    pub const fn symbol(&self) -> AprsSymbol {
        self.symbol
    }

    /// Return the configured packet data rate.
    #[must_use]
    pub const fn data_rate(&self) -> PacketDataRate {
        self.data_rate
    }

    /// Return the default position-beacon comment.
    #[must_use]
    pub const fn beacon_comment(&self) -> &PositionReportText {
        &self.beacon_comment
    }

    /// Return the `SmartBeaconing` configuration.
    #[must_use]
    pub const fn smart_beaconing(&self) -> &SmartBeaconingConfig {
        &self.smart_beaconing
    }

    /// Return the optional digipeater configuration.
    #[must_use]
    pub const fn digipeater(&self) -> Option<&DigipeaterConfig> {
        self.digipeater.as_ref()
    }

    /// Return the shared station-list and `IGate` history capacity.
    #[must_use]
    pub const fn max_stations(&self) -> NonZeroUsize {
        self.max_stations
    }

    /// Return the station retention time.
    #[must_use]
    pub const fn station_timeout(&self) -> Duration {
        self.station_timeout
    }

    /// Return the explicit APRS-IS to RF policy, if enabled.
    #[must_use]
    pub const fn igate_to_rf(&self) -> Option<IGateToRfConfig> {
        self.igate_to_rf
    }

    /// Return whether incoming addressed messages are acknowledged.
    #[must_use]
    pub const fn auto_ack(&self) -> bool {
        self.auto_ack
    }

    /// Return the outgoing digipeater path.
    #[must_use]
    pub const fn digipeater_path(&self) -> &DigipeaterPath {
        &self.digipeater_path
    }

    /// Return whether directed position queries are answered automatically.
    #[must_use]
    pub const fn auto_query_response(&self) -> bool {
        self.auto_query_response
    }

    /// Return the cached position used for directed query responses.
    #[must_use]
    pub const fn auto_query_position(&self) -> Option<(Latitude, Longitude)> {
        self.auto_query_position
    }
}

/// Fluent builder for [`AprsClientConfig`].
///
/// Every constrained field is validated before it enters this builder, so
/// [`Self::build`] is infallible.
#[derive(Debug, Clone)]
pub struct AprsClientConfigBuilder {
    source: Ax25Address,
    symbol: AprsSymbol,
    data_rate: PacketDataRate,
    beacon_comment: PositionReportText,
    smart_beaconing: SmartBeaconingConfig,
    digipeater: Option<DigipeaterConfig>,
    max_stations: NonZeroUsize,
    station_timeout: Duration,
    igate_to_rf: Option<IGateToRfConfig>,
    auto_ack: bool,
    digipeater_path: DigipeaterPath,
    auto_query_response: bool,
    auto_query_position: Option<(Latitude, Longitude)>,
}

impl AprsClientConfigBuilder {
    /// Create a builder for a validated station address.
    ///
    /// # Errors
    ///
    /// Returns [`crate::error::ValidationError`] if the standard outgoing
    /// path cannot be constructed.
    pub fn new(source: Ax25Address) -> Result<Self, crate::error::ValidationError> {
        let digipeater_path = crate::aprs::default_digipeater_path().map_err(|_| {
            crate::error::ValidationError::AprsWireOutOfRange {
                field: "default APRS digipeater path",
                detail: "the library default must contain only valid AX.25 route entries",
            }
        })?;
        Ok(Self {
            source,
            symbol: AprsSymbol::CAR,
            data_rate: PacketDataRate::Bps1200,
            beacon_comment: PositionReportText::default(),
            smart_beaconing: SmartBeaconingConfig::default(),
            digipeater: None,
            max_stations: NonZeroUsize::new(500).ok_or(
                crate::error::ValidationError::AprsWireOutOfRange {
                    field: "maximum APRS stations",
                    detail: "the library default must be nonzero",
                },
            )?,
            station_timeout: Duration::from_secs(3600),
            igate_to_rf: None,
            auto_ack: true,
            digipeater_path,
            auto_query_response: true,
            auto_query_position: None,
        })
    }

    /// Set the APRS symbol.
    #[must_use]
    pub const fn symbol(mut self, symbol: AprsSymbol) -> Self {
        self.symbol = symbol;
        self
    }

    /// Override the packet data rate (default 1200 bps).
    #[must_use]
    pub const fn data_rate(mut self, data_rate: PacketDataRate) -> Self {
        self.data_rate = data_rate;
        self
    }

    /// Set the default beacon comment.
    #[must_use]
    pub fn beacon_comment(mut self, comment: PositionReportText) -> Self {
        self.beacon_comment = comment;
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

    /// Set the shared station-list and `IGate` identity-history capacity.
    ///
    /// On `IGate` blocker-history overflow, Internet-to-RF gating fails closed
    /// for the configured quiet period instead of evicting an unexpired
    /// blocker.
    #[must_use]
    pub const fn max_stations(mut self, n: NonZeroUsize) -> Self {
        self.max_stations = n;
        self
    }

    /// Set the station entry retention time.
    #[must_use]
    pub const fn station_timeout(mut self, timeout: Duration) -> Self {
        self.station_timeout = timeout;
        self
    }

    /// Enable APRS-IS to RF gating under an explicit operator policy.
    #[must_use]
    pub const fn igate_to_rf(mut self, config: IGateToRfConfig) -> Self {
        self.igate_to_rf = Some(config);
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
    pub fn digipeater_path(mut self, path: DigipeaterPath) -> Self {
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
    pub const fn auto_query_position(mut self, latitude: Latitude, longitude: Longitude) -> Self {
        self.auto_query_position = Some((latitude, longitude));
        self
    }

    /// Build the configuration. All constrained values are already typed.
    #[must_use]
    pub fn build(self) -> AprsClientConfig {
        AprsClientConfig {
            source: self.source,
            symbol: self.symbol,
            data_rate: self.data_rate,
            beacon_comment: self.beacon_comment,
            smart_beaconing: self.smart_beaconing,
            digipeater: self.digipeater,
            max_stations: self.max_stations,
            station_timeout: self.station_timeout,
            igate_to_rf: self.igate_to_rf,
            auto_ack: self.auto_ack,
            digipeater_path: self.digipeater_path,
            auto_query_response: self.auto_query_response,
            auto_query_position: self.auto_query_position,
        }
    }
}

fn validate_station_address(
    callsign: &str,
    ssid: u8,
) -> Result<Ax25Address, crate::error::ValidationError> {
    Ax25Address::new(callsign, ssid).map_err(|_| {
        crate::error::ValidationError::AprsWireOutOfRange {
            field: "APRS station address",
            detail: "callsign must be 1-6 ASCII A-Z/0-9 characters and SSID must be 0-15",
        }
    })
}

fn has_internet_path_marker(line: &AprsIsLine, repeated_required: bool) -> bool {
    line.path().iter().any(|element| {
        let AprsIsPathElement::Route(route) = element else {
            return false;
        };
        (!repeated_required || route.has_repeated())
            && (route.identity().base().eq_ignore_ascii_case("TCPIP")
                || route.identity().base().eq_ignore_ascii_case("TCPXX"))
    })
}

fn canonical_rf_header_identity(identity: &str) -> Option<String> {
    let uppercase = identity.to_ascii_uppercase();
    Ax25Address::from_canonical_str(&uppercase)
        .ok()
        .map(|address| address.to_string())
}

fn third_party_header_has_internet_marker(header: &str) -> bool {
    let Some((_, destination_and_path)) = header.split_once('>') else {
        return false;
    };
    let Some((_, path)) = destination_and_path.split_once(',') else {
        return false;
    };
    path.split(',').any(|element| {
        let identity = element.strip_suffix('*').unwrap_or(element);
        let base = identity.split_once('-').map_or(identity, |(base, _)| base);
        base.eq_ignore_ascii_case("TCPIP") || base.eq_ignore_ascii_case("TCPXX")
    })
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
    MessageDelivered(MessageId),
    /// A previously sent message was rejected by the remote station.
    MessageRejected(MessageId),
    /// A previously sent message expired after exhausting all retries.
    MessageExpired(MessageId),
    /// A position report was received from another station.
    PositionReceived {
        /// Source callsign.
        source: String,
        /// Decoded position data.
        position: AprsPosition,
    },
    /// A standalone positionless weather report was received from another
    /// station.
    WeatherReceived {
        /// Source callsign.
        source: String,
        /// Full report, including its mandatory UTC timestamp.
        report: AprsPositionlessWeatherReport,
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
    /// Stateful Internet-to-RF eligibility history. Absent unless the host
    /// deliberately enabled transmission with [`IGateToRfConfig`].
    igate_to_rf: Option<IGateToRfState>,
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
    /// On failure, returns the [`Radio`] alongside the error. The radio may
    /// require [`Radio::restore_cat_after_mode_exit`] before CAT commands are
    /// safe again because a failed KISS transition can leave binary bytes on
    /// the transport.
    pub async fn start(
        radio: Radio<T>,
        config: AprsClientConfig,
    ) -> Result<Self, (Radio<T>, Error)> {
        let my_addr = config.source.clone();

        let mut session = match radio.enter_kiss(config.data_rate).await {
            Ok(s) => s,
            Err((radio, e)) => return Err((radio, e)),
        };
        session.set_receive_timeout(EVENT_POLL_TIMEOUT);

        let messenger = AprsMessenger::new(my_addr.clone(), config.digipeater_path.clone());
        let stations = StationList::new(config.max_stations.get(), config.station_timeout);
        let igate_to_rf = config
            .igate_to_rf
            .map(|policy| IGateToRfState::new(policy, config.max_stations));
        let beaconing = SmartBeaconing::new(config.smart_beaconing.clone());

        Ok(Self {
            session,
            config,
            my_addr,
            messenger,
            stations,
            igate_to_rf,
            beaconing,
            pending_events: VecDeque::new(),
            last_rf_packet: None,
        })
    }

    /// Stop the APRS client, exiting KISS mode and returning the [`Radio`].
    ///
    /// The returned radio is deliberately desynchronized because unread
    /// binary frames may remain on the transport. Call
    /// [`Radio::restore_cat_after_mode_exit`] before using CAT again or
    /// reporting that CAT mode has been restored.
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
            igate_to_rf,
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
                    igate_to_rf,
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
        if let Some(igate) = &mut self.igate_to_rf {
            igate.purge_expired(now);
        }

        // 2. Try to receive a KISS data frame.
        let Some(packet) = self.recv_one_frame().await? else {
            return Ok(None);
        };

        // Capture the raw frame so an IGate can gate it regardless of how
        // it later classifies (typed event vs `RawPacket`).
        self.last_rf_packet = Some(packet.clone());
        self.record_rf_igate_observation(&packet, now);

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
                // corruption on air; leave a trace so a degrading
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
    /// not sent immediately: [`DigipeaterConfig::process`] returns
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
    /// **every** heard packet to APRS-IS, including ones that surfaced as
    /// typed events (`PositionReceived`, `StationHeard`, …) rather than
    /// [`AprsEvent::RawPacket`], by pairing it with
    /// [`Self::format_packet_for_aprs_is`].
    pub const fn take_last_rf_packet(&mut self) -> Option<Ax25Packet> {
        self.last_rf_packet.take()
    }

    fn record_rf_igate_observation(&mut self, packet: &Ax25Packet, now: Instant) {
        let Some(igate) = &mut self.igate_to_rf else {
            return;
        };
        let internet_gated_third_party = matches!(
            parse_aprs_data_full(packet.information(), &packet.destination.callsign),
            Ok(AprsData::ThirdParty { header, .. })
                if third_party_header_has_internet_marker(&header)
        );
        let source = packet.source.to_string();
        if igate.receiver_locality.includes(packet) {
            igate.rf_heard.record(&source, now);
        }
        if internet_gated_third_party {
            // APRS-IS defines an RF station forwarding a third-party
            // TCPIP/TCPXX packet as Internet-heard. That same packet is
            // explicitly excluded from the direct-RF sender test.
            igate.internet_heard.record(&source, now);
        } else {
            igate.direct_rf_heard.record(&source, now);
        }
    }

    /// Phase 4: parse the APRS info field, update the station list,
    /// and dispatch to the appropriate event variant.
    async fn handle_packet(
        &mut self,
        packet: Ax25Packet,
        now: Instant,
    ) -> Result<Option<AprsEvent>, Error> {
        let Ok(aprs_data) =
            parse_aprs_data_full(packet.information(), &packet.destination.callsign)
        else {
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
                let entry = self.stations.get(&source).cloned();

                // Observation-order contract (matches every other arm in
                // this match): `StationHeard` is the *primary* event when
                // the station list has an entry: it carries the "we just
                // heard from this station" signal that callers use for UI
                // refresh and IGate timing. Any data-bearing sub-event
                // (weather, position) is queued for the *next* call to
                // [`AprsClient::next_event`] so the caller sees them in
                // arrival order: heard-then-data. Embedded weather remains
                // part of the position, so this event never discards its
                // timestamp, coordinates, or comment.
                match entry {
                    Some(entry_ev) => {
                        self.pending_events.push_back(AprsEvent::PositionReceived {
                            source,
                            position: pos,
                        });
                        Ok(Some(AprsEvent::StationHeard(entry_ev)))
                    }
                    None => Ok(Some(AprsEvent::PositionReceived {
                        source,
                        position: pos,
                    })),
                }
            }
            AprsData::PositionlessWeather(report) => Ok(Some(AprsEvent::WeatherReceived {
                source: packet.source.callsign.as_str().to_owned(),
                report,
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
    pub async fn send_message(
        &mut self,
        addressee: &MessageAddressee,
        text: &MessageText,
    ) -> Result<MessageId, Error> {
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
        latitude: Latitude,
        longitude: Longitude,
        comment: &PositionReportText,
    ) -> Result<(), Error> {
        self.beacon_position_at(latitude, longitude, comment, Instant::now())
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
        latitude: Latitude,
        longitude: Longitude,
        comment: &PositionReportText,
        now: Instant,
    ) -> Result<(), Error> {
        let source = self.my_addr.clone();
        let wire = build_aprs_position_report(
            &source,
            latitude,
            longitude,
            self.config.symbol,
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
        latitude: Latitude,
        longitude: Longitude,
        comment: &CompressedPositionText,
    ) -> Result<(), Error> {
        self.beacon_position_compressed_at(latitude, longitude, comment, Instant::now())
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
        latitude: Latitude,
        longitude: Longitude,
        comment: &CompressedPositionText,
        now: Instant,
    ) -> Result<(), Error> {
        let source = self.my_addr.clone();
        let wire = build_aprs_position_compressed(
            &source,
            latitude,
            longitude,
            self.config.symbol,
            comment,
            &self.config.digipeater_path,
        );
        self.session.send_wire(&wire).await?;
        self.beaconing.beacon_sent(now);
        Ok(())
    }

    /// Beacon current position using Mic-E encoding, the most compact
    /// APRS position format and the TH-D75's native one.
    ///
    /// Latitude is encoded in the AX.25 destination address; longitude,
    /// speed, and course are packed into the info field per APRS 1.0.1
    /// Chapter 10. Uses the configured symbol, the configured digipeater
    /// path, and the "Off Duty" Mic-E message code.
    ///
    /// [`MiceSpeed`] enforces the Mic-E wire range 0-799 knots; [`Course`]
    /// uses 0 for "unknown" per the spec. Updates the `SmartBeaconing`
    /// "last beacon" timer like the other beacon methods.
    ///
    /// # Errors
    ///
    /// Returns an error if the transmission fails.
    pub async fn beacon_position_mice(
        &mut self,
        latitude: Latitude,
        longitude: Longitude,
        speed: MiceSpeed,
        course: Course,
        status_text: &MiceStatusText,
    ) -> Result<(), Error> {
        let source = self.my_addr.clone();
        let wire = build_aprs_mice(
            &source,
            latitude,
            longitude,
            speed,
            course,
            self.config.symbol,
            status_text,
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
    pub async fn send_status(&mut self, text: &StatusText) -> Result<(), Error> {
        let source = self.my_addr.clone();
        let wire = build_aprs_status(&source, text, &self.config.digipeater_path);
        self.session.send_wire(&wire).await?;
        Ok(())
    }

    /// Send a timestamped status report using the status-only DHM-UTC format.
    ///
    /// # Errors
    ///
    /// Returns an error if the transmission fails.
    pub async fn send_timestamped_status(
        &mut self,
        timestamp: AprsStatusTimestamp,
        text: &TimestampedStatusText,
    ) -> Result<(), Error> {
        let source = self.my_addr.clone();
        let wire =
            build_aprs_timestamped_status(&source, timestamp, text, &self.config.digipeater_path);
        self.session.send_wire(&wire).await?;
        Ok(())
    }

    /// Set the cached position for auto query responses.
    ///
    /// When a station sends `?APRSP` and auto query response is enabled,
    /// the client replies with a position beacon using this position.
    pub const fn set_query_response_position(&mut self, latitude: Latitude, longitude: Longitude) {
        self.config.auto_query_position = Some((latitude, longitude));
    }

    /// Send an object report.
    ///
    /// # Errors
    ///
    /// Returns an error if the transmission fails.
    pub async fn send_object(
        &mut self,
        name: &ObjectName,
        live: bool,
        timestamp: AprsReportTimestamp,
        latitude: Latitude,
        longitude: Longitude,
        comment: &PositionReportText,
    ) -> Result<(), Error> {
        let source = self.my_addr.clone();
        let wire = build_aprs_object(
            &source,
            name,
            live,
            timestamp,
            latitude,
            longitude,
            self.config.symbol,
            comment,
            &self.config.digipeater_path,
        );
        self.session.send_wire(&wire).await?;
        Ok(())
    }

    /// Update speed and course for `SmartBeaconing`.
    ///
    /// If the `SmartBeaconing` algorithm determines a beacon is due (based
    /// on speed, heading change, and elapsed time), a position report is
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
    /// semantic timestamp, so the recorded transmission time cannot drift
    /// beyond the decision that authorized it.
    ///
    /// # Errors
    ///
    /// Returns an error if the beacon transmission fails.
    pub async fn update_motion(
        &mut self,
        speed: Speed,
        heading: Heading,
        latitude: Latitude,
        longitude: Longitude,
    ) -> Result<bool, Error> {
        let now = Instant::now();
        if self.beaconing.should_beacon(speed, Some(heading), now) {
            let comment = self.config.beacon_comment.clone();
            self.beacon_position_at(latitude, longitude, &comment, now)
                .await?;
            self.beaconing.beacon_sent_with(speed, Some(heading), now);
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

    /// Enable APRS-IS to RF transmission under an explicit operator policy.
    ///
    /// Enabling or replacing the policy starts with empty observation history,
    /// so no Internet packet is eligible until the client has freshly observed
    /// the corresponding RF and Internet identities.
    pub fn configure_igate_to_rf(&mut self, config: IGateToRfConfig) {
        self.config.igate_to_rf = Some(config);
        self.igate_to_rf = Some(IGateToRfState::new(config, self.config.max_stations));
    }

    /// Disable APRS-IS to RF transmission and discard its observation state.
    pub fn disable_igate_to_rf(&mut self) {
        self.config.igate_to_rf = None;
        self.igate_to_rf = None;
    }

    /// Validate and format a received RF packet for APRS-IS.
    ///
    /// Converts the AX.25 packet to the byte-preserving APRS-IS wire form:
    /// `SOURCE>DEST,PATH,qAR,MYCALL:data`
    ///
    /// `login` selects `qAR` for a verified APRS-IS login and `qAO` for a
    /// receive-only login. The packet's information bytes are never decoded as
    /// UTF-8, so Mic-E and other binary-compatible payloads remain exact.
    ///
    /// # Errors
    ///
    /// Returns [`IGateFormatError`] when the packet is ineligible for gating,
    /// contains an embedded APRS-IS framing byte, or would exceed the
    /// 512-byte wire limit. Terminal TNC `CR`/`LF` framing is normalized to
    /// the single APRS-IS `CRLF` terminator.
    pub fn format_packet_for_aprs_is(
        &self,
        packet: &Ax25Packet,
        login: Passcode,
    ) -> Result<AprsIsUplinkLine, IGateFormatError> {
        igate_format_packet_for_is(packet, &self.my_addr, login)
    }

    fn evaluate_gate_from_is(
        &mut self,
        line: &AprsIsLine,
        now: Instant,
    ) -> Result<Option<IGateToRfCandidate>, Error> {
        let Some(igate) = &mut self.igate_to_rf else {
            return Ok(None);
        };

        // Only the exact repeated Internet transport markers count as an
        // Internet observation. A q construct alone does not satisfy the
        // APRS-IS definition of "heard via the Internet".
        if has_internet_path_marker(line, true) {
            igate.internet_heard.record(line.source().as_str(), now);
        }

        // APRS-IS accepts server-style identities such as `AE5PL-TS`, but
        // APRS 1.0.1 Chapter 17 requires source and destination callsigns with
        // numeric AX.25 SSIDs inside an RF third-party header.
        if canonical_rf_header_identity(line.source().as_str()).is_none()
            || canonical_rf_header_identity(line.destination().as_str()).is_none()
        {
            return Ok(None);
        }

        if line.information().first() == Some(&b':') {
            let message = parse_aprs_message(line.information())?;
            if !matches!(message.kind(), MessageKind::Direct | MessageKind::AckRej)
                || line.blocks_gating_to_rf()
            {
                return Ok(None);
            }

            let sender = line.source().as_str();
            let receiver = message.addressee.as_str();
            if !igate.rf_heard.contains_at(receiver, now)
                || igate.direct_rf_heard.contains_at(sender, now)
                || igate.internet_heard.contains_at(receiver, now)
            {
                return Ok(None);
            }
            return Ok(Some(IGateToRfCandidate::Message {
                sender: sender.to_ascii_uppercase(),
                receiver: receiver.to_ascii_uppercase(),
            }));
        }

        let is_position_type = matches!(
            line.information().first(),
            Some(b'!' | b'=' | b'/' | b'@' | b'`' | b'\'' | 0x1C | 0x1D)
        );
        if !is_position_type {
            return Ok(None);
        }
        if !matches!(
            parse_aprs_data_full(line.information(), line.destination().as_str())?,
            AprsData::Position(_)
        ) {
            return Ok(None);
        }

        // The primary rule says to pass the next position seen for a station
        // whose message was gated. Taking the association here makes "next"
        // exact even when that position carries an opt-out marker or later
        // fails another eligibility predicate.
        let sender = line.source().as_str();
        let Some(receivers) = igate.take_associated_receivers(sender) else {
            return Ok(None);
        };
        if line.blocks_gating_to_rf() || igate.direct_rf_heard.contains_at(sender, now) {
            return Ok(None);
        }
        if !receivers.iter().any(|receiver| {
            igate.rf_heard.contains_at(receiver, now)
                && !igate.internet_heard.contains_at(receiver, now)
        }) {
            return Ok(None);
        }
        Ok(Some(IGateToRfCandidate::AssociatedPosition))
    }

    fn wrap_gate_from_is_packet(&self, is_packet: &AprsIsLine) -> Result<Ax25Packet, Error> {
        // APRS-IS IGateDetails and APRS 1.0.1 §17 require a fresh RF-side
        // third-party header. The Internet path (including q constructs and
        // server identities) must be removed; q constructs and the historical
        // `I` construct must never appear on RF. `TCPIP,IGATECALL*` identifies
        // the third-party network and the receiving gateway while preventing
        // another IGate from feeding this packet back to APRS-IS.
        //
        // Source and destination come from the same typed line that policy
        // inspected, then are converted to the uppercase textual form required
        // when an APRS-IS identity is used on RF. The original unvalidated
        // input is never reparsed or copied, preventing parser/serializer
        // disagreement and CRLF leakage.
        let igate_call = self.my_addr.clone();
        let igate_text = igate_call.to_string();
        let source_text =
            canonical_rf_header_identity(is_packet.source().as_str()).unwrap_or_else(|| {
                unreachable!("policy accepted only RF-compatible source identities")
            });
        let destination_text = canonical_rf_header_identity(is_packet.destination().as_str())
            .unwrap_or_else(|| {
                unreachable!("policy accepted only RF-compatible destination identities")
            });
        let mut third_party_payload = Vec::new();
        third_party_payload.push(b'}');
        third_party_payload.extend_from_slice(source_text.as_bytes());
        third_party_payload.push(b'>');
        third_party_payload.extend_from_slice(destination_text.as_bytes());
        third_party_payload.extend_from_slice(b",TCPIP,");
        third_party_payload.extend_from_slice(igate_text.as_bytes());
        third_party_payload.extend_from_slice(b"*:");
        third_party_payload.extend_from_slice(is_packet.information());

        if third_party_payload.len() > MAX_AX25_INFORMATION_BYTES {
            return Err(Error::AprsThirdPartyInformationTooLong {
                actual: third_party_payload.len(),
                maximum: MAX_AX25_INFORMATION_BYTES,
            });
        }

        let destination = Ax25Address::new("APRS", 0)
            .unwrap_or_else(|_| unreachable!("APRS is a statically valid AX.25 address"));
        Ok(Ax25Packet::unnumbered_information(
            igate_call,
            destination,
            self.config.digipeater_path.clone(),
            CommandResponse::Command,
            false,
            Ax25Pid::NoLayer3,
            third_party_payload,
        ))
    }

    /// Build the AX.25 wrap frame that this `IGate` would transmit for one
    /// already parsed APRS-IS line, **without** sending it.
    ///
    /// Returns `Ok(None)` only when the typed packet fails `IGate` policy.
    /// Returns an error if the canonical third-party wrapper would exceed the
    /// AX.25 information-field limit. This stateful test helper records
    /// Internet observations and consumes a pending associated position in
    /// exactly the same way as [`Self::gate_from_is`].
    #[cfg(test)]
    fn build_gate_from_is_packet(
        &mut self,
        is_packet: &AprsIsLine,
        now: Instant,
    ) -> Result<Option<Ax25Packet>, Error> {
        if self.evaluate_gate_from_is(is_packet, now)?.is_none() {
            return Ok(None);
        }
        self.wrap_gate_from_is_packet(is_packet).map(Some)
    }

    /// Parse an APRS-IS packet and transmit it on RF via KISS.
    ///
    /// Wraps the IS packet in a third-party header and broadcasts it on
    /// the configured RF digipeater path, following the canonical
    /// IGate-to-RF format documented at
    /// <https://www.aprs-is.net/IGateDetails.aspx> and APRS 1.0.1 Chapter 17.
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
    /// MYCALL>APRS,RF_PATH:}ORIGSRC>ORIGDEST,TCPIP,MYCALL*:data
    /// ```
    ///
    /// where
    /// - `MYCALL` is the `IGate`'s configured callsign + SSID
    ///   (the station's validated address),
    /// - `APRS` is the generic destination required for third-party RF
    ///   packets, and
    /// - `RF_PATH` is the `IGate`'s configured digipeater path
    ///   (`self.config.digipeater_path`) so the gated packet reaches
    ///   beyond the `IGate`'s immediate footprint via existing RF
    ///   digipeaters.
    /// - The inner third-party payload discards the APRS-IS path and appends
    ///   the mandatory `TCPIP,MYCALL*` third-party network/gateway path.
    ///   APRS-IS q constructs, server identities, and the `I` construct are
    ///   never transmitted on RF. Mixed-case Internet identities are emitted
    ///   in the uppercase form required for use on RF.
    ///
    /// Earlier experimental versions put `TCPIP` in the outer AX.25 path or
    /// copied the APRS-IS path into the third-party header. Both forms violate
    /// the direction-specific `IGate` rules and can leak q constructs onto RF.
    ///
    /// # Filtering
    ///
    /// The input is parsed exactly once from its original bytes. Only
    /// transmits if [`Self::observe_and_evaluate_gate_from_is`] accepts that
    /// typed packet.
    /// Returns `Ok(false)` exclusively for a policy rejection and `Ok(true)`
    /// after transmission.
    ///
    /// # Errors
    ///
    /// Returns an error if the APRS-IS line is malformed, the completed
    /// third-party information field exceeds 256 bytes, or KISS transmission
    /// fails. Both UTF-8 strings and byte-native [`aprs_is::AprsIsPacket::raw`]
    /// buffers can be supplied without lossy conversion.
    ///
    /// `now` must be sampled once by the caller for the received APRS-IS event;
    /// all three recency predicates and associated-position state use that same
    /// instant. With no explicit [`IGateToRfConfig`], the method fails closed
    /// with `Ok(false)`.
    pub async fn gate_from_is(
        &mut self,
        is_packet: impl AsRef<[u8]>,
        now: Instant,
    ) -> Result<bool, Error> {
        let line = AprsIsLine::parse(is_packet)?;
        let Some(candidate) = self.evaluate_gate_from_is(&line, now)? else {
            return Ok(false);
        };
        let packet = self.wrap_gate_from_is_packet(&line)?;
        let ax25_bytes = build_ax25(&packet);
        let wire = encode_kiss_frame(&KissFrame::data(ax25_bytes));
        self.session.send_wire(&wire).await?;
        if let IGateToRfCandidate::Message { sender, receiver } = candidate
            && let Some(igate) = &mut self.igate_to_rf
        {
            igate.remember_associated_position(&sender, &receiver, now);
        }
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
    /// enforces uppercase ASCII alphanumeric at validation time; every
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
        if packet.information().first() == Some(&b'}') {
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

    /// Observe and evaluate a parsed APRS-IS packet without transmitting it.
    ///
    /// Applies all four APRS-IS recency and path predicates to directed
    /// messages, acknowledgements, rejections, and the next associated
    /// position after a message was actually gated. The observation and
    /// pending-position mutations are committed even when the packet is
    /// rejected, just as they are in [`Self::gate_from_is`].
    ///
    /// # Errors
    ///
    /// Returns [`Error::AprsPacket`] when a declared message or position has
    /// malformed APRS content.
    pub fn observe_and_evaluate_gate_from_is(
        &mut self,
        line: &AprsIsLine,
        now: Instant,
    ) -> Result<bool, Error> {
        self.evaluate_gate_from_is(line, now)
            .map(|candidate| candidate.is_some())
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
        let my_call = self.config.source.callsign.as_str();

        // Check if this message is addressed to us.
        if !msg.addressee.eq_ignore_ascii_case(my_call) {
            // Not for us; treat as a station heard event.
            let entry = self.stations.get(&from.callsign).cloned();
            return Ok(entry.map(AprsEvent::StationHeard));
        }

        // Check if it is an ack/rej control frame for a pending message.
        // The messenger only honours it when `from` is the station the
        // message was addressed to; a message number is not a secret on
        // the air, so any other station's ack is ignored.
        if let Some((is_ack, id)) = classify_ack_rej(&msg.text) {
            let Ok(id) = MessageId::new(id) else {
                return Ok(None);
            };
            if self.messenger.process_incoming(&from.callsign, msg) {
                return Ok(Some(if is_ack {
                    AprsEvent::MessageDelivered(id)
                } else {
                    AprsEvent::MessageRejected(id)
                }));
            }
            // Control frame for an unknown message; ignore.
            return Ok(None);
        }

        // Regular message addressed to us; auto-ack if configured.
        if self.config.auto_ack
            && let Some(ref id) = msg.message_id
            && let Ok(addressee) = MessageAddressee::new(from.callsign.as_str())
            && let Ok(message_id) = MessageId::new(id)
        {
            let ack_frame = self.messenger.build_ack(&addressee, &message_id);
            self.session.send_wire(&ack_frame).await?;
        }

        // Handle directed position query (`?APRSP`).
        //
        // When enabled and a position is cached, respond with a position
        // beacon. The beacon goes to CQCQCQ (all stations), not just the
        // querying station; this is per APRS spec, which treats the
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
                self.config.symbol,
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
    use aprs::{
        Fahrenheit, ThreeDigitWeatherValue, WindDirection, build_aprs_message as build_msg,
        build_aprs_position_report as build_pos,
    };
    use kiss_tnc::FEND;

    use crate::aprs::default_digipeater_path;
    use crate::transport::MockTransport;
    use crate::types::PacketDataRate;

    type TestResult = Result<(), Box<dyn std::error::Error>>;
    type BoxErr = Box<dyn std::error::Error>;

    /// Build a mock Radio that expects the TN 2,x command for KISS entry.
    fn mock_radio(data_rate: PacketDataRate) -> Radio<MockTransport> {
        let tn_cmd = format!("TN 2,{}\r", u8::from(data_rate));
        let tn_resp = format!("TN 2,{}\r", u8::from(data_rate));
        let mut mock = MockTransport::new();
        mock.expect(tn_cmd.as_bytes(), tn_resp.as_bytes());
        Radio::new(mock)
    }

    fn test_config() -> Result<AprsClientConfig, crate::error::ValidationError> {
        AprsClientConfig::try_new("N0CALL", 7)
    }

    fn test_igate_policy() -> Result<IGateToRfConfig, crate::error::ValidationError> {
        IGateToRfConfig::new(
            IGateRfLocality::DIRECT,
            Duration::from_secs(60),
            Duration::from_secs(60),
            Duration::from_secs(60),
        )
    }

    fn test_igate_config() -> Result<AprsClientConfig, crate::error::ValidationError> {
        Ok(AprsClientConfig::try_builder("N0CALL", 7)?
            .igate_to_rf(test_igate_policy()?)
            .build())
    }

    fn hear_directly_on_rf(
        client: &mut AprsClient<MockTransport>,
        identity: &str,
        now: Instant,
    ) -> TestResult {
        let igate = client
            .igate_to_rf
            .as_mut()
            .ok_or("test IGate policy is not configured")?;
        igate.rf_heard.record(identity, now);
        igate.direct_rf_heard.record(identity, now);
        Ok(())
    }

    fn hear_via_internet(
        client: &mut AprsClient<MockTransport>,
        identity: &str,
        now: Instant,
    ) -> TestResult {
        client
            .igate_to_rf
            .as_mut()
            .ok_or("test IGate policy is not configured")?
            .internet_heard
            .record(identity, now);
        Ok(())
    }

    fn test_address() -> Result<Ax25Address, ax25_codec::Ax25Error> {
        Ax25Address::new("N0CALL", 7)
    }

    fn position_text(value: &str) -> PositionReportText {
        PositionReportText::new(value)
            .unwrap_or_else(|_| unreachable!("test fixture position text is statically valid"))
    }

    fn compressed_text(value: &str) -> CompressedPositionText {
        CompressedPositionText::new(value).unwrap_or_else(|_| {
            unreachable!("test fixture compressed-position text is statically valid")
        })
    }

    fn mice_status_text(value: &str) -> MiceStatusText {
        MiceStatusText::new(value)
            .unwrap_or_else(|_| unreachable!("test fixture Mic-E status text is statically valid"))
    }

    #[test]
    fn client_uses_v103_smart_beaconing_defaults() -> TestResult {
        let config = test_config()?;
        let smart = config.smart_beaconing();
        assert!((smart.low_speed().as_kmh() - 8.046_72).abs() < f64::EPSILON);
        assert!((smart.high_speed().as_kmh() - 112.654_08).abs() < f64::EPSILON);
        assert_eq!(smart.slow_rate_secs(), 1800);
        assert_eq!(smart.fast_rate_secs(), 120);
        assert!((smart.turn_slope() - 41.842_944).abs() < f64::EPSILON);
        assert!((smart.turn_minimum().as_degrees() - 28.0).abs() < f64::EPSILON);
        assert_eq!(smart.turn_time_secs(), 60);
        Ok(())
    }

    #[tokio::test]
    async fn start_enters_kiss_mode() -> TestResult {
        let radio = mock_radio(PacketDataRate::Bps1200);
        let config = test_config()?;
        let client = AprsClient::start(radio, config).await.map_err(|(_, e)| e)?;
        assert_eq!(client.config().source().callsign, "N0CALL");
        assert_eq!(client.config().source().ssid, 7);
        assert_eq!(client.stations().len(), 0);
        assert_eq!(client.messenger().pending_count(), 0);
        Ok(())
    }

    #[tokio::test]
    async fn stop_exits_kiss_mode() -> TestResult {
        let radio = mock_radio(PacketDataRate::Bps1200);
        let config = test_config()?;
        let mut client = AprsClient::start(radio, config).await.map_err(|(_, e)| e)?;

        // Queue the KISS exit frame expectation.
        client.session.transport.expect(&[FEND, 0xFF, FEND], &[]);

        let _radio = client.stop().await.map_err(|(_, e)| e)?;
        Ok(())
    }

    #[test]
    fn config_rejects_invalid_callsign_at_construction() {
        assert!(AprsClientConfig::try_new("N0CALL/P", 7).is_err());
    }

    #[test]
    fn config_rejects_invalid_ssid_at_construction() {
        assert!(AprsClientConfig::try_new("N0CALL", 99).is_err());
    }

    #[tokio::test]
    async fn send_message_queues_and_transmits() -> TestResult {
        let radio = mock_radio(PacketDataRate::Bps1200);
        let config = test_config()?;
        let mut client = AprsClient::start(radio, config).await.map_err(|(_, e)| e)?;

        // The messenger builds a KISS-encoded wire frame internally.
        // send_message calls send_wire which writes it directly.
        let addressee = MessageAddressee::new("W1AW")?;
        let text = MessageText::new("Hello")?;
        let message_id = MessageId::new("1")?;
        let expected_wire = build_msg(
            &test_address()?,
            &addressee,
            &text,
            Some(&message_id),
            &default_digipeater_path()?,
        );
        client.session.transport.expect(&expected_wire, &[]);

        let id = client.send_message(&addressee, &text).await?;
        assert_eq!(id.as_str(), "1");
        assert_eq!(client.messenger().pending_count(), 1);
        Ok(())
    }

    #[tokio::test]
    async fn beacon_position_transmits() -> TestResult {
        let radio = mock_radio(PacketDataRate::Bps1200);
        let config = test_config()?;
        let mut client = AprsClient::start(radio, config).await.map_err(|(_, e)| e)?;

        let expected = build_pos(
            &test_address()?,
            Latitude::new(35.25)?,
            Longitude::new(-97.75)?,
            AprsSymbol::CAR,
            &position_text("mobile"),
            &default_digipeater_path()?,
        );
        client.session.transport.expect(&expected, &[]);

        client
            .beacon_position(
                Latitude::new(35.25)?,
                Longitude::new(-97.75)?,
                &position_text("mobile"),
            )
            .await?;
        Ok(())
    }

    #[tokio::test]
    async fn beacon_position_compressed_transmits() -> TestResult {
        let radio = mock_radio(PacketDataRate::Bps1200);
        let config = test_config()?;
        let mut client = AprsClient::start(radio, config).await.map_err(|(_, e)| e)?;

        let expected = build_aprs_position_compressed(
            &test_address()?,
            Latitude::new(35.25)?,
            Longitude::new(-97.75)?,
            AprsSymbol::CAR,
            &compressed_text("compressed"),
            &default_digipeater_path()?,
        );
        client.session.transport.expect(&expected, &[]);

        client
            .beacon_position_compressed(
                Latitude::new(35.25)?,
                Longitude::new(-97.75)?,
                &compressed_text("compressed"),
            )
            .await?;
        Ok(())
    }

    #[tokio::test]
    async fn send_status_transmits() -> TestResult {
        let radio = mock_radio(PacketDataRate::Bps1200);
        let config = test_config()?;
        let mut client = AprsClient::start(radio, config).await.map_err(|(_, e)| e)?;

        let text = StatusText::new("On the air")?;
        let expected = build_aprs_status(&test_address()?, &text, &default_digipeater_path()?);
        client.session.transport.expect(&expected, &[]);

        client.send_status(&text).await?;
        Ok(())
    }

    #[tokio::test]
    async fn send_timestamped_status_transmits() -> TestResult {
        let radio = mock_radio(PacketDataRate::Bps1200);
        let config = test_config()?;
        let mut client = AprsClient::start(radio, config).await.map_err(|(_, e)| e)?;

        let timestamp = AprsStatusTimestamp::day_hour_minute_utc(9, 23, 45)?;
        let text = TimestampedStatusText::new("On the air")?;
        let expected = build_aprs_timestamped_status(
            &test_address()?,
            timestamp,
            &text,
            &default_digipeater_path()?,
        );
        client.session.transport.expect(&expected, &[]);

        client.send_timestamped_status(timestamp, &text).await?;
        Ok(())
    }

    #[tokio::test]
    async fn send_object_transmits() -> TestResult {
        let radio = mock_radio(PacketDataRate::Bps1200);
        let config = test_config()?;
        let mut client = AprsClient::start(radio, config).await.map_err(|(_, e)| e)?;
        let timestamp = AprsReportTimestamp::day_hour_minute_utc(15, 14, 30)?;

        let name = ObjectName::new("Marathon")?;
        let expected = build_aprs_object(
            &test_address()?,
            &name,
            true,
            timestamp,
            Latitude::new(35.0)?,
            Longitude::new(-97.0)?,
            AprsSymbol::CAR,
            &position_text("5K run"),
            &default_digipeater_path()?,
        );
        client.session.transport.expect(&expected, &[]);

        client
            .send_object(
                &name,
                true,
                timestamp,
                Latitude::new(35.0)?,
                Longitude::new(-97.0)?,
                &position_text("5K run"),
            )
            .await?;
        Ok(())
    }

    #[test]
    fn config_builder_valid() -> TestResult {
        let cfg = AprsClientConfig::try_builder("N0CALL", 9)?
            .symbol(AprsSymbol::CAR)
            .beacon_comment(PositionReportText::new("test")?)
            .auto_ack(false)
            .max_stations(NonZeroUsize::new(100).ok_or("100 must be nonzero")?)
            .build();
        assert_eq!(cfg.source().callsign, "N0CALL");
        assert_eq!(cfg.source().ssid, 9);
        assert_eq!(cfg.symbol(), AprsSymbol::CAR);
        assert_eq!(cfg.beacon_comment().as_str(), "test");
        assert!(!cfg.auto_ack());
        assert_eq!(cfg.max_stations().get(), 100);
        Ok(())
    }

    #[test]
    fn config_builder_rejects_bad_callsign() {
        assert!(AprsClientConfig::try_builder("", 0).is_err());
        assert!(AprsClientConfig::try_builder("TOOLONG", 0).is_err());
    }

    #[test]
    fn config_builder_rejects_bad_ssid() {
        assert!(AprsClientConfig::try_builder("N0CALL", 16).is_err());
    }

    #[test]
    fn config_builder_rejects_bad_symbol_table() {
        assert!(AprsSymbol::from_chars('!', '>').is_err());
    }

    #[test]
    fn config_defaults() -> TestResult {
        let config = AprsClientConfig::try_new("W1AW", 0)?;
        assert_eq!(config.source().callsign, "W1AW");
        assert_eq!(config.source().ssid, 0);
        assert_eq!(config.symbol(), AprsSymbol::CAR);
        assert!(config.auto_ack());
        assert!(config.digipeater().is_none());
        assert_eq!(config.max_stations().get(), 500);
        assert_eq!(config.station_timeout(), Duration::from_secs(3600));
        assert_eq!(config.igate_to_rf(), None);
        Ok(())
    }

    #[test]
    fn igate_to_rf_config_requires_explicit_valid_periods() {
        let minute = Duration::from_secs(60);
        assert!(IGateRfLocality::new(8).is_ok());
        assert!(IGateRfLocality::new(9).is_err());
        assert!(IGateToRfConfig::new(IGateRfLocality::DIRECT, minute, minute, minute).is_ok());
        assert!(
            IGateToRfConfig::new(IGateRfLocality::DIRECT, Duration::ZERO, minute, minute).is_err()
        );
        assert!(
            IGateToRfConfig::new(IGateRfLocality::DIRECT, minute, Duration::ZERO, minute).is_err()
        );
        assert!(
            IGateToRfConfig::new(IGateRfLocality::DIRECT, minute, minute, Duration::ZERO).is_err()
        );
        assert!(
            IGateToRfConfig::new(
                IGateRfLocality::DIRECT,
                Duration::from_secs(3601),
                minute,
                minute,
            )
            .is_err(),
            "receiver RF period must not exceed the primary-source one-hour maximum"
        );
    }

    #[test]
    fn igate_history_overflow_is_fail_closed_for_blockers_until_exact_expiry() -> TestResult {
        let window = Duration::from_secs(10);
        let capacity = NonZeroUsize::new(2).ok_or("2 must be nonzero")?;
        let policy = IGateToRfConfig::new(IGateRfLocality::DIRECT, window, window, window)?;
        let mut state = IGateToRfState::new(policy, capacity);
        assert_eq!(
            state.rf_heard.overflow,
            IdentityHistoryOverflow::EvictOldest
        );
        assert_eq!(
            state.direct_rf_heard.overflow,
            IdentityHistoryOverflow::MatchAllUntilExpiry
        );
        assert_eq!(
            state.internet_heard.overflow,
            IdentityHistoryOverflow::MatchAllUntilExpiry
        );

        let start = Instant::now();
        let second = start + Duration::from_nanos(1);
        let overflowed = start + Duration::from_nanos(2);
        for history in [
            &mut state.rf_heard,
            &mut state.direct_rf_heard,
            &mut state.internet_heard,
        ] {
            history.record("FIRST", start);
            history.record("SECOND", second);
            history.record("THIRD", overflowed);
        }

        let boundary = overflowed + window;
        let just_before = boundary
            .checked_sub(Duration::from_nanos(1))
            .ok_or("test boundary must be representable")?;
        assert!(state.rf_heard.contains_at("THIRD", just_before));
        assert!(!state.rf_heard.contains_at("UNSEEN", just_before));
        assert!(state.direct_rf_heard.contains_at("UNSEEN", just_before));
        assert!(state.internet_heard.contains_at("UNSEEN", just_before));

        assert!(!state.rf_heard.contains_at("THIRD", boundary));
        assert!(!state.direct_rf_heard.contains_at("UNSEEN", boundary));
        assert!(!state.internet_heard.contains_at("UNSEEN", boundary));
        state.purge_expired(boundary);
        assert_eq!(state.direct_rf_heard.match_all_since, None);
        assert_eq!(state.internet_heard.match_all_since, None);
        Ok(())
    }

    #[test]
    fn config_preserves_typed_source_address() -> TestResult {
        let config = AprsClientConfig::try_new("KQ4NIT", 9)?;
        assert_eq!(config.source().callsign, "KQ4NIT");
        assert_eq!(config.source().ssid, 9);
        Ok(())
    }

    #[test]
    fn aprs_event_debug_formatting() -> TestResult {
        let event = AprsEvent::MessageDelivered(MessageId::new("42")?);
        let debug = format!("{event:?}");
        assert!(debug.contains("MessageDelivered"));
        assert!(debug.contains("42"));
        Ok(())
    }

    #[test]
    fn aprs_client_debug_formatting() -> TestResult {
        // Cannot construct AprsClient without async, but we can verify
        // the config formatting.
        let config = test_config()?;
        let debug = format!("{config:?}");
        assert!(debug.contains("N0CALL"));
        Ok(())
    }

    // -----------------------------------------------------------------------
    // IGate tests
    // -----------------------------------------------------------------------

    fn make_test_packet(
        source: &str,
        dest: &str,
        digis: &[&str],
        info: &[u8],
    ) -> Result<Ax25Packet, BoxErr> {
        let digipeaters = crate::aprs::parse_digipeater_path(&digis.join(","))?;
        Ok(Ax25Packet::unnumbered_information(
            Ax25Address::new(source, 0)?,
            Ax25Address::new(dest, 0)?,
            digipeaters,
            CommandResponse::Command,
            false,
            Ax25Pid::NoLayer3,
            info.to_vec(),
        ))
    }

    fn parse_is_fixture(line: &str) -> Result<AprsIsLine, BoxErr> {
        Ok(AprsIsLine::parse(line)?)
    }

    #[tokio::test]
    async fn format_packet_for_aprs_is_is_byte_exact() -> TestResult {
        let radio = mock_radio(PacketDataRate::Bps1200);
        let config = test_config()?;
        let client = AprsClient::start(radio, config).await.map_err(|(_, e)| e)?;

        let packet = make_test_packet("W1AW", "APK005", &["WIDE1-1"], b"!4903.50N/07201.75W-")?;
        let is_line = client.format_packet_for_aprs_is(&packet, Passcode::Verified(12_345))?;

        assert_eq!(
            is_line.as_bytes(),
            b"W1AW>APK005,WIDE1-1,qAR,N0CALL-7:!4903.50N/07201.75W-\r\n"
        );
        Ok(())
    }

    #[tokio::test]
    async fn format_packet_for_aprs_is_normalizes_terminal_tnc_framing() -> TestResult {
        let radio = mock_radio(PacketDataRate::Bps1200);
        let config = test_config()?;
        let client = AprsClient::start(radio, config).await.map_err(|(_, e)| e)?;

        let packet = make_test_packet("W1AW", "APK005", &["WIDE1-1"], b">QRV mobile\r")?;
        let normalized = client.format_packet_for_aprs_is(&packet, Passcode::Verified(12_345))?;
        assert_eq!(
            normalized.as_bytes(),
            b"W1AW>APK005,WIDE1-1,qAR,N0CALL-7:>QRV mobile\r\n"
        );

        let embedded = make_test_packet("W1AW", "APK005", &["WIDE1-1"], b">QRV\rmobile")?;
        assert!(matches!(
            client.format_packet_for_aprs_is(&embedded, Passcode::Verified(12_345)),
            Err(IGateFormatError::InvalidUplinkLine(
                aprs_is::AprsIsUplinkLineError::EmbeddedNewline { byte: b'\r', .. }
            ))
        ));
        Ok(())
    }

    #[tokio::test]
    async fn format_packet_for_aprs_is_preserves_non_utf8_and_login_state() -> TestResult {
        let radio = mock_radio(PacketDataRate::Bps1200);
        let config = test_config()?;
        let client = AprsClient::start(radio, config).await.map_err(|(_, e)| e)?;

        let information = [b'`', 0xC1, 0x82, b'X'];
        let packet = make_test_packet("W1AW", "APK005", &[], &information)?;
        let is_line = client.format_packet_for_aprs_is(&packet, Passcode::ReceiveOnly)?;

        let mut expected = b"W1AW>APK005,qAO,N0CALL-7:".to_vec();
        expected.extend_from_slice(&information);
        expected.extend_from_slice(b"\r\n");
        assert_eq!(is_line.as_bytes(), expected);
        Ok(())
    }

    #[test]
    fn should_gate_to_is_normal_packet() -> TestResult {
        let packet = make_test_packet("W1AW", "APK005", &["WIDE1-1"], b"!4903.50N/07201.75W-")?;
        assert!(AprsClient::<MockTransport>::should_gate_to_is(&packet));
        Ok(())
    }

    #[test]
    fn should_gate_to_is_blocks_tcpip_source() -> TestResult {
        let packet = make_test_packet("TCPIP", "APK005", &[], b"!4903.50N/07201.75W-")?;
        assert!(!AprsClient::<MockTransport>::should_gate_to_is(&packet));
        Ok(())
    }

    #[test]
    fn should_gate_to_is_blocks_tcpxx_source() -> TestResult {
        let packet = make_test_packet("TCPXX", "APK005", &[], b"!4903.50N/07201.75W-")?;
        assert!(!AprsClient::<MockTransport>::should_gate_to_is(&packet));
        Ok(())
    }

    #[test]
    fn should_gate_to_is_blocks_third_party() -> TestResult {
        let packet = make_test_packet("W1AW", "APK005", &[], b"}W2AW>APK005:!4903.50N/07201.75W-")?;
        assert!(!AprsClient::<MockTransport>::should_gate_to_is(&packet));
        Ok(())
    }

    #[test]
    fn should_gate_to_is_blocks_nogate_in_path() -> TestResult {
        let packet = make_test_packet("W1AW", "APK005", &["NOGATE"], b"!4903.50N/07201.75W-")?;
        assert!(!AprsClient::<MockTransport>::should_gate_to_is(&packet));
        Ok(())
    }

    #[test]
    fn should_gate_to_is_blocks_rfonly_in_path() -> TestResult {
        let packet = make_test_packet("W1AW", "APK005", &["RFONLY"], b"!4903.50N/07201.75W-")?;
        assert!(!AprsClient::<MockTransport>::should_gate_to_is(&packet));
        Ok(())
    }

    #[tokio::test]
    async fn observe_and_evaluate_gate_from_is_rejects_position_reports() -> TestResult {
        let radio = mock_radio(PacketDataRate::Bps1200);
        let config = test_igate_config()?;
        let mut client = AprsClient::start(radio, config).await.map_err(|(_, e)| e)?;

        // Position report (starts with '!') should not be gated to RF.
        let line = parse_is_fixture("W1AW>APK005,TCPIP:!4903.50N/07201.75W-Test\r\n")?;
        assert!(!client.observe_and_evaluate_gate_from_is(&line, Instant::now())?);
        Ok(())
    }

    #[tokio::test]
    async fn observe_and_evaluate_gate_from_is_does_not_treat_bulletins_as_direct_messages()
    -> TestResult {
        let radio = mock_radio(PacketDataRate::Bps1200);
        let config = test_igate_config()?;
        let mut client = AprsClient::start(radio, config).await.map_err(|(_, e)| e)?;
        let now = Instant::now();
        hear_directly_on_rf(&mut client, "NWS-WARN", now)?;

        let line = parse_is_fixture("WX1>APRS,TCPIP*,qAC,T2SERVER::NWS-WARN :AR_ASHLEY,{S9JbA")?;
        assert!(!client.observe_and_evaluate_gate_from_is(&line, now)?);
        Ok(())
    }

    #[tokio::test]
    async fn observe_and_evaluate_gate_from_is_rejects_nogate_in_path() -> TestResult {
        let radio = mock_radio(PacketDataRate::Bps1200);
        let config = test_igate_config()?;
        let mut client = AprsClient::start(radio, config).await.map_err(|(_, e)| e)?;

        let line = parse_is_fixture("W1AW>APK005,NOGATE::N0CALL   :Hello{123\r\n")?;
        assert!(!client.observe_and_evaluate_gate_from_is(&line, Instant::now())?);
        Ok(())
    }

    #[tokio::test]
    async fn observe_and_evaluate_gate_from_is_requires_heard_station() -> TestResult {
        let radio = mock_radio(PacketDataRate::Bps1200);
        let config = test_igate_config()?;
        let mut client = AprsClient::start(radio, config).await.map_err(|(_, e)| e)?;

        // Message to a station NOT in our station list.
        let line = parse_is_fixture("W1AW>APK005,TCPIP::UNKNOWN  :Hello{123\r\n")?;
        assert!(!client.observe_and_evaluate_gate_from_is(&line, Instant::now())?);
        Ok(())
    }

    #[tokio::test]
    async fn observe_and_evaluate_gate_from_is_accepts_message_to_heard_station() -> TestResult {
        let radio = mock_radio(PacketDataRate::Bps1200);
        let config = test_igate_config()?;
        let mut client = AprsClient::start(radio, config).await.map_err(|(_, e)| e)?;

        // Simulate hearing a station on RF.
        let now = Instant::now();
        hear_directly_on_rf(&mut client, "KQ4NIT", now)?;

        // Verified client-originated packets normally carry TCPIP* plus qAC.
        let line = parse_is_fixture("W1AW>APK005,TCPIP*,qAC,SRV::KQ4NIT   :Hello{123\r\n")?;
        assert!(client.observe_and_evaluate_gate_from_is(&line, now)?);
        Ok(())
    }

    #[tokio::test]
    async fn observe_and_evaluate_gate_from_is_rejects_unverified_and_opt_out_markers() -> TestResult
    {
        let radio = mock_radio(PacketDataRate::Bps1200);
        let config = test_igate_config()?;
        let mut client = AprsClient::start(radio, config).await.map_err(|(_, e)| e)?;
        let now = Instant::now();
        hear_directly_on_rf(&mut client, "KQ4NIT", now)?;
        for wire in [
            "KE4EVIL>APRS,TCPXX*,qAX,T2SERVER::KQ4NIT   :Hello{123\r\n",
            "W1AW>APK005,NOGATE-1*::KQ4NIT   :Hello{123\r\n",
            "W1AW>APK005,RFONLY-AA::KQ4NIT   :Hello{123\r\n",
        ] {
            let line = parse_is_fixture(wire)?;
            assert!(
                !client.observe_and_evaluate_gate_from_is(&line, now)?,
                "accepted {wire:?}"
            );
        }
        Ok(())
    }

    #[tokio::test]
    async fn observe_and_evaluate_gate_from_is_is_fail_closed_without_explicit_policy() -> TestResult
    {
        let radio = mock_radio(PacketDataRate::Bps1200);
        let mut client = AprsClient::start(radio, test_config()?)
            .await
            .map_err(|(_, error)| error)?;
        let line = parse_is_fixture("W1AW>APK005,TCPIP*,qAC,SRV::KQ4NIT   :Hello{123")?;
        assert!(!client.observe_and_evaluate_gate_from_is(&line, Instant::now())?);
        Ok(())
    }

    #[tokio::test]
    async fn receiver_locality_uses_explicit_repeated_hop_limit() -> TestResult {
        let minute = Duration::from_secs(60);
        let policy = IGateToRfConfig::new(IGateRfLocality::new(1)?, minute, minute, minute)?;
        let config = AprsClientConfig::try_builder("N0CALL", 7)?
            .igate_to_rf(policy)
            .build();
        let radio = mock_radio(PacketDataRate::Bps1200);
        let mut client = AprsClient::start(radio, config).await.map_err(|(_, e)| e)?;
        let now = Instant::now();
        let repeated_one = ax25_codec::RouteEntry::new("WIDE1", 1)?.marked_used();
        let repeated_two = ax25_codec::RouteEntry::new("WIDE2", 1)?.marked_used();
        let unused = ax25_codec::RouteEntry::new("WIDE3", 1)?;
        let beyond_local = Ax25Packet::unnumbered_information(
            Ax25Address::new("KQ4NIT", 9)?,
            Ax25Address::new("APK005", 0)?,
            DigipeaterPath::new(vec![repeated_one.clone(), repeated_two, unused.clone()])?,
            CommandResponse::Command,
            false,
            Ax25Pid::NoLayer3,
            b">two repeated hops".to_vec(),
        );
        client.record_rf_igate_observation(&beyond_local, now);
        let line = parse_is_fixture("W1AW>APK005,TCPIP*,qAC,SRV::KQ4NIT-9 :Hello{123")?;
        assert!(!client.observe_and_evaluate_gate_from_is(&line, now)?);
        let igate = client
            .igate_to_rf
            .as_ref()
            .ok_or("test IGate policy is not configured")?;
        assert!(igate.direct_rf_heard.contains_at("KQ4NIT-9", now));
        assert!(!igate.rf_heard.contains_at("KQ4NIT-9", now));

        let local = Ax25Packet::unnumbered_information(
            Ax25Address::new("KQ4NIT", 9)?,
            Ax25Address::new("APK005", 0)?,
            DigipeaterPath::new(vec![repeated_one, unused])?,
            CommandResponse::Command,
            false,
            Ax25Pid::NoLayer3,
            b">one repeated hop".to_vec(),
        );
        client.record_rf_igate_observation(&local, now + Duration::from_secs(1));
        assert!(client.observe_and_evaluate_gate_from_is(&line, now + Duration::from_secs(1))?);
        Ok(())
    }

    #[tokio::test]
    async fn rf_third_party_header_rejects_server_style_identities() -> TestResult {
        let radio = mock_radio(PacketDataRate::Bps1200);
        let mut client = AprsClient::start(radio, test_igate_config()?)
            .await
            .map_err(|(_, error)| error)?;
        let now = Instant::now();
        hear_directly_on_rf(&mut client, "KQ4NIT", now)?;

        for wire in [
            "AE5PL-TS>APK005,TCPIP*,qAC,SRV::KQ4NIT   :Hello{123",
            "W1AW>AE5PL-TS,TCPIP*,qAC,SRV::KQ4NIT   :Hello{123",
        ] {
            let line = parse_is_fixture(wire)?;
            assert!(
                !client.observe_and_evaluate_gate_from_is(&line, now)?,
                "server-style identity was admitted to an RF header: {wire}"
            );
        }

        let rf_compatible = parse_is_fixture("w1aw-7>apk005,TCPIP*,qAC,SRV::KQ4NIT   :Hello{123")?;
        assert!(client.observe_and_evaluate_gate_from_is(&rf_compatible, now)?);
        Ok(())
    }

    #[tokio::test]
    async fn sender_rf_quiet_period_expires_at_exact_boundary() -> TestResult {
        let period = Duration::from_secs(10);
        let policy = IGateToRfConfig::new(
            IGateRfLocality::DIRECT,
            Duration::from_secs(20),
            period,
            period,
        )?;
        let config = AprsClientConfig::try_builder("N0CALL", 7)?
            .igate_to_rf(policy)
            .build();
        let radio = mock_radio(PacketDataRate::Bps1200);
        let mut client = AprsClient::start(radio, config).await.map_err(|(_, e)| e)?;
        let observed = Instant::now();
        hear_directly_on_rf(&mut client, "KQ4NIT", observed)?;
        hear_directly_on_rf(&mut client, "W1AW", observed)?;
        let line = parse_is_fixture("W1AW>APK005,TCPIP*,qAC,SRV::KQ4NIT   :Hello{123")?;
        let boundary = observed + period;
        let just_before = boundary
            .checked_sub(Duration::from_nanos(1))
            .ok_or("test boundary must be representable")?;

        assert!(!client.observe_and_evaluate_gate_from_is(&line, just_before)?);
        assert!(client.observe_and_evaluate_gate_from_is(&line, boundary)?);
        Ok(())
    }

    #[tokio::test]
    async fn receiver_rf_max_age_expires_at_exact_boundary() -> TestResult {
        let period = Duration::from_secs(10);
        let policy = IGateToRfConfig::new(
            IGateRfLocality::DIRECT,
            period,
            Duration::from_secs(20),
            period,
        )?;
        let config = AprsClientConfig::try_builder("N0CALL", 7)?
            .igate_to_rf(policy)
            .build();
        let radio = mock_radio(PacketDataRate::Bps1200);
        let mut client = AprsClient::start(radio, config).await.map_err(|(_, e)| e)?;
        let observed = Instant::now();
        hear_directly_on_rf(&mut client, "KQ4NIT", observed)?;
        let line = parse_is_fixture("W1AW>APK005,TCPIP*,qAC,SRV::KQ4NIT   :Hello{123")?;
        let boundary = observed + period;
        let just_before = boundary
            .checked_sub(Duration::from_nanos(1))
            .ok_or("test boundary must be representable")?;

        assert!(client.observe_and_evaluate_gate_from_is(&line, just_before)?);
        assert!(!client.observe_and_evaluate_gate_from_is(&line, boundary)?);
        Ok(())
    }

    #[tokio::test]
    async fn receiver_internet_quiet_period_expires_at_exact_boundary() -> TestResult {
        let period = Duration::from_secs(10);
        let policy = IGateToRfConfig::new(
            IGateRfLocality::DIRECT,
            Duration::from_secs(20),
            period,
            period,
        )?;
        let config = AprsClientConfig::try_builder("N0CALL", 7)?
            .igate_to_rf(policy)
            .build();
        let radio = mock_radio(PacketDataRate::Bps1200);
        let mut client = AprsClient::start(radio, config).await.map_err(|(_, e)| e)?;
        let observed = Instant::now();
        hear_directly_on_rf(&mut client, "KQ4NIT-9", observed)?;
        hear_via_internet(&mut client, "KQ4NIT-9", observed)?;
        let line = parse_is_fixture("W1AW>APK005,TCPIP*,qAC,SRV::KQ4NIT-9 :Hello{123")?;
        let boundary = observed + period;
        let just_before = boundary
            .checked_sub(Duration::from_nanos(1))
            .ok_or("test boundary must be representable")?;

        assert!(!client.observe_and_evaluate_gate_from_is(&line, just_before)?);
        assert!(client.observe_and_evaluate_gate_from_is(&line, boundary)?);
        Ok(())
    }

    #[tokio::test]
    async fn packet_observations_update_full_identity_histories() -> TestResult {
        let radio = mock_radio(PacketDataRate::Bps1200);
        let mut client = AprsClient::start(radio, test_igate_config()?)
            .await
            .map_err(|(_, error)| error)?;
        let now = Instant::now();
        let direct = Ax25Packet::unnumbered_information(
            Ax25Address::new("KQ4NIT", 9)?,
            Ax25Address::new("APK005", 0)?,
            DigipeaterPath::empty(),
            CommandResponse::Command,
            false,
            Ax25Pid::NoLayer3,
            b">on air".to_vec(),
        );
        client.record_rf_igate_observation(&direct, now);
        drop(client.handle_packet(direct, now).await?);
        let to_full_identity = parse_is_fixture("W1AW>APK005,TCPIP*,qAC,SRV::KQ4NIT-9 :Hello{123")?;
        assert!(client.observe_and_evaluate_gate_from_is(&to_full_identity, now)?);

        let third_party = Ax25Packet::unnumbered_information(
            Ax25Address::new("IGATE", 5)?,
            Ax25Address::new("APRS", 0)?,
            DigipeaterPath::empty(),
            CommandResponse::Command,
            false,
            Ax25Pid::NoLayer3,
            b"}W1AW>APK005,TCPIP,N0CALL*:>from Internet".to_vec(),
        );
        client.record_rf_igate_observation(&third_party, now);
        drop(client.handle_packet(third_party, now).await?);
        let igate = client
            .igate_to_rf
            .as_ref()
            .ok_or("test IGate policy is not configured")?;
        assert!(igate.rf_heard.contains_at("IGATE-5", now));
        assert!(igate.internet_heard.contains_at("IGATE-5", now));
        assert!(
            !igate.direct_rf_heard.contains_at("IGATE-5", now),
            "Internet-gated third-party packets are excluded from direct RF hearing"
        );
        Ok(())
    }

    #[tokio::test]
    async fn repeated_tcpip_marker_records_source_as_internet_heard() -> TestResult {
        let radio = mock_radio(PacketDataRate::Bps1200);
        let mut client = AprsClient::start(radio, test_igate_config()?)
            .await
            .map_err(|(_, error)| error)?;
        let now = Instant::now();
        hear_directly_on_rf(&mut client, "KQ4NIT", now)?;
        let receiver_position =
            parse_is_fixture("KQ4NIT>APK005,TCPIP*,qAC,SRV:!4903.50N/07201.75W-Test")?;
        assert!(!client.observe_and_evaluate_gate_from_is(&receiver_position, now)?);

        let message = parse_is_fixture("W1AW>APK005,TCPIP*,qAC,SRV::KQ4NIT   :Hello{123")?;
        assert!(!client.observe_and_evaluate_gate_from_is(&message, now)?);
        Ok(())
    }

    #[tokio::test]
    async fn acknowledgements_use_the_same_stateful_eligibility_rules() -> TestResult {
        let radio = mock_radio(PacketDataRate::Bps1200);
        let mut client = AprsClient::start(radio, test_igate_config()?)
            .await
            .map_err(|(_, error)| error)?;
        let now = Instant::now();
        hear_directly_on_rf(&mut client, "KQ4NIT", now)?;
        let ack = parse_is_fixture("W1AW>APK005,TCPIP*,qAC,SRV::KQ4NIT   :ack123")?;
        assert!(client.observe_and_evaluate_gate_from_is(&ack, now)?);

        hear_via_internet(&mut client, "KQ4NIT", now)?;
        assert!(!client.observe_and_evaluate_gate_from_is(&ack, now)?);
        Ok(())
    }

    #[tokio::test]
    async fn gated_message_authorizes_exactly_the_next_associated_position() -> TestResult {
        let radio = mock_radio(PacketDataRate::Bps1200);
        let mut client = AprsClient::start(radio, test_igate_config()?)
            .await
            .map_err(|(_, error)| error)?;
        let now = Instant::now();
        hear_directly_on_rf(&mut client, "KQ4NIT", now)?;
        client.session.transport.expect_any_write();
        assert!(
            client
                .gate_from_is("W1AW>APK005,TCPIP*,qAC,SRV::KQ4NIT   :Hello{123", now,)
                .await?
        );

        let position = "W1AW>APK005,TCPIP*,qAC,SRV:!4903.50N/07201.75W-Test";
        client.session.transport.expect_any_write();
        assert!(
            client
                .gate_from_is(position, now + Duration::from_secs(1))
                .await?
        );
        assert!(
            !client
                .gate_from_is(position, now + Duration::from_secs(2))
                .await?,
            "only the first position after the gated message is associated"
        );
        Ok(())
    }

    #[tokio::test]
    async fn associated_position_preserves_path_and_q_construct_blocks() -> TestResult {
        for blocked_path in ["NOGATE-1*", "RFONLY-AA", "TCPXX*", "qAX", "qAZ"] {
            let radio = mock_radio(PacketDataRate::Bps1200);
            let mut client = AprsClient::start(radio, test_igate_config()?)
                .await
                .map_err(|(_, error)| error)?;
            let now = Instant::now();
            hear_directly_on_rf(&mut client, "KQ4NIT", now)?;
            client.session.transport.expect_any_write();
            assert!(
                client
                    .gate_from_is("W1AW>APK005,TCPIP*,qAC,SRV::KQ4NIT   :Hello{123", now,)
                    .await?
            );

            let position = format!("W1AW>APK005,{blocked_path}:!4903.50N/07201.75W-Test");
            assert!(
                !client
                    .gate_from_is(position, now + Duration::from_secs(1))
                    .await?,
                "associated position bypassed marker {blocked_path}"
            );
        }
        Ok(())
    }

    #[tokio::test]
    async fn gate_from_is_wraps_in_third_party_header() -> TestResult {
        let radio = mock_radio(PacketDataRate::Bps1200);
        let config = test_igate_config()?;
        let mut client = AprsClient::start(radio, config).await.map_err(|(_, e)| e)?;

        // Simulate hearing the addressee on RF so gating is allowed.
        let now = Instant::now();
        hear_directly_on_rf(&mut client, "KQ4NIT", now)?;

        // Expect the KISS frame output (we just need the mock to accept it).
        // The exact bytes depend on the third-party packet encoding.
        // We use a broad expectation: the mock will accept any write.
        client.session.transport.expect_any_write();

        let result = client
            .gate_from_is("W1AW>APK005,qAC,SRV::KQ4NIT   :Hello{123", now)
            .await?;
        assert!(result);
        Ok(())
    }

    #[tokio::test]
    async fn gate_from_is_packet_wire_shape() -> TestResult {
        // The outer AX.25 frame carries the IGate's configured RF path, while
        // the inner header replaces all APRS-IS routing metadata with the
        // mandatory `TCPIP,MYCALL*` third-party path. Verified
        // structurally by inspecting the built packet's fields rather
        // than the encoded KISS bytes: fewer brittle assertions, same
        // protocol coverage.
        let radio = mock_radio(PacketDataRate::Bps1200);
        let config = test_igate_config()?;
        let mut client = AprsClient::start(radio, config).await.map_err(|(_, e)| e)?;

        // Simulate hearing the addressee on RF so gating is allowed.
        let now = Instant::now();
        hear_directly_on_rf(&mut client, "KQ4NIT", now)?;

        let is_line = parse_is_fixture("w1aw>apk005,TCPIP*,qAC,SRV::KQ4NIT   :Hello{123")?;
        let packet = client
            .build_gate_from_is_packet(&is_line, now)?
            .ok_or("gate packet not built: expected Some")?;

        // Outer source is the IGate's callsign + SSID.
        assert_eq!(packet.source.callsign.as_str(), "N0CALL");
        assert_eq!(packet.source.ssid.get(), 7);
        // Third-party packets use the generic APRS destination.
        assert_eq!(packet.destination.callsign.as_str(), "APRS");
        // Outer path is the IGate's configured RF path (default
        // WIDE1-1,WIDE2-1 from AprsClientConfig::new), never an
        // Internet-origin marker such as TCPIP.
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
        assert_eq!(
            packet.information(),
            b"}W1AW>APK005,TCPIP,N0CALL-7*::KQ4NIT   :Hello{123"
        );
        assert!(
            !packet
                .information()
                .windows(3)
                .any(|window| window == b"qAC")
        );
        assert!(
            !packet
                .information()
                .windows(3)
                .any(|window| window == b"SRV")
        );
        Ok(())
    }

    #[tokio::test]
    async fn gate_from_is_filters_position_report() -> TestResult {
        let radio = mock_radio(PacketDataRate::Bps1200);
        let config = test_igate_config()?;
        let mut client = AprsClient::start(radio, config).await.map_err(|(_, e)| e)?;

        // Position report should not be gated to RF.
        let result = client
            .gate_from_is("W1AW>APK005,TCPIP:!4903.50N/07201.75W-Test", Instant::now())
            .await?;
        assert!(!result);
        Ok(())
    }

    #[tokio::test]
    async fn gate_from_is_reports_malformed_input_instead_of_policy_rejection() -> TestResult {
        let radio = mock_radio(PacketDataRate::Bps1200);
        let config = test_igate_config()?;
        let mut client = AprsClient::start(radio, config)
            .await
            .map_err(|(_, error)| error)?;

        for malformed in [
            b"W1AW>APK005,qAC,SRV::KQ4NIT   :hello\r\nEVIL>APRS:forged\r\n".as_slice(),
            "W1AW>APK005,qAC,SÉRV::KQ4NIT   :hello".as_bytes(),
            b"W1AW>APK005,qAC,SR\x01V::KQ4NIT   :hello".as_slice(),
            b"W1AW>APK005,WIDE1-ABC::KQ4NIT   :hello".as_slice(),
        ] {
            let result = client.gate_from_is(malformed, Instant::now()).await;
            assert!(
                matches!(result, Err(Error::AprsIsLine(_))),
                "malformed input was not surfaced as an error: {result:?}"
            );
        }

        let malformed_message = client
            .gate_from_is("W1AW>APK005,qAC,SRV::SHORT:message", Instant::now())
            .await;
        assert!(
            matches!(malformed_message, Err(Error::AprsPacket(_))),
            "malformed APRS message was not surfaced as an error: {malformed_message:?}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn gate_from_is_rejects_non_ascii_message_instead_of_lossy_conversion() -> TestResult {
        let radio = mock_radio(PacketDataRate::Bps1200);
        let config = test_igate_config()?;
        let mut client = AprsClient::start(radio, config)
            .await
            .map_err(|(_, error)| error)?;
        let now = Instant::now();
        hear_directly_on_rf(&mut client, "KQ4NIT", now)?;

        let original = b"W1AW>APK005,qAC,SRV::KQ4NIT   :\xC1\x82";
        let result = client.gate_from_is(original, now).await;
        assert!(
            matches!(result, Err(Error::AprsPacket(_))),
            "non-ASCII APRS message was not rejected exactly: {result:?}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn gate_from_is_enforces_completed_rf_information_boundary() -> TestResult {
        const INPUT_HEADER: &[u8] = b"W1AW>APK005,qAC,SRV";
        const RF_SOURCE_DESTINATION: &[u8] = b"W1AW>APK005";

        let radio = mock_radio(PacketDataRate::Bps1200);
        let config = test_igate_config()?;
        let mut client = AprsClient::start(radio, config)
            .await
            .map_err(|(_, error)| error)?;
        let now = Instant::now();
        hear_directly_on_rf(&mut client, "KQ4NIT", now)?;

        let wrapper_bytes =
            1 + RF_SOURCE_DESTINATION.len() + b",TCPIP,".len() + "N0CALL-7".len() + b"*:".len();
        let maximum_original_information = MAX_AX25_INFORMATION_BYTES - wrapper_bytes;
        let mut information = b":KQ4NIT   :".to_vec();
        information.resize(maximum_original_information, b'X');

        let mut exact_wire = INPUT_HEADER.to_vec();
        exact_wire.push(b':');
        exact_wire.extend_from_slice(&information);
        let exact = AprsIsLine::parse(&exact_wire)?;
        let packet = client
            .build_gate_from_is_packet(&exact, now)?
            .ok_or("eligible maximum-length packet was rejected by policy")?;
        assert_eq!(packet.information().len(), MAX_AX25_INFORMATION_BYTES);

        information.push(b'X');
        let mut oversized_wire = INPUT_HEADER.to_vec();
        oversized_wire.push(b':');
        oversized_wire.extend_from_slice(&information);
        let oversized = AprsIsLine::parse(&oversized_wire)?;
        assert!(matches!(
            client.build_gate_from_is_packet(&oversized, now),
            Err(Error::AprsThirdPartyInformationTooLong {
                actual: 257,
                maximum: MAX_AX25_INFORMATION_BYTES,
            })
        ));
        Ok(())
    }

    // -----------------------------------------------------------------------
    // next_event dispatch tests
    // -----------------------------------------------------------------------

    /// Build a KISS-encoded data frame from a source callsign and APRS info.
    fn build_kiss_data_frame(source: &str, ssid: u8, info: &[u8]) -> Vec<u8> {
        let packet = Ax25Packet::unnumbered_information(
            Ax25Address::new(source, ssid)
                .unwrap_or_else(|_| unreachable!("test fixture source is valid")),
            Ax25Address::new("APK005", 0)
                .unwrap_or_else(|_| unreachable!("APK005 is statically valid")),
            DigipeaterPath::empty(),
            CommandResponse::Command,
            false,
            Ax25Pid::NoLayer3,
            info.to_vec(),
        );
        let ax25_bytes = build_ax25(&packet);
        encode_kiss_frame(&KissFrame::data(ax25_bytes))
    }

    #[tokio::test]
    async fn next_event_position_received() -> TestResult {
        let radio = mock_radio(PacketDataRate::Bps1200);
        let config = test_config()?;
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
        let radio = mock_radio(PacketDataRate::Bps1200);
        let config = test_config()?;
        let mut client = AprsClient::start(radio, config).await.map_err(|(_, e)| e)?;

        // Position + weather report: !DDMM.MMN/DDDMM.MMW_DIR/SPDgGUSTt072
        let info = b"!3515.00N/09745.00W_090/010g015t072";
        let wire = build_kiss_data_frame("WX1STA", 0, info);
        client.session.transport.queue_read(&wire);

        // Observation-order contract (see `dispatch_event` for the
        // rationale): a position-with-weather packet emits two events:
        // `StationHeard` first ("we saw this station"), then the full
        // `PositionReceived` payload. Weather stays embedded in that
        // position so its coordinates, timestamp, and comment are not lost.
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
        let AprsEvent::PositionReceived { source, position } = second else {
            return Err(format!("expected PositionReceived second, got {second_dump}").into());
        };
        assert_eq!(source, "WX1STA");
        let weather = position.weather.ok_or("embedded weather missing")?;
        assert_eq!(
            weather.wind_direction().map(WindDirection::degrees),
            Some(90)
        );
        assert_eq!(
            weather.wind_speed().map(ThreeDigitWeatherValue::value),
            Some(10),
        );
        assert_eq!(
            weather.wind_gust().map(ThreeDigitWeatherValue::value),
            Some(15),
        );
        assert_eq!(weather.temperature().map(Fahrenheit::get), Some(72));
        Ok(())
    }

    #[tokio::test]
    async fn next_event_message_received() -> TestResult {
        let radio = mock_radio(PacketDataRate::Bps1200);
        let config = AprsClientConfig::try_builder("N0CALL", 7)?
            .auto_ack(false)
            .build();
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
        let radio = mock_radio(PacketDataRate::Bps1200);
        let config = test_config()?;
        let mut client = AprsClient::start(radio, config).await.map_err(|(_, e)| e)?;

        // First, send a message so we have a pending message with id "1"
        let addressee = MessageAddressee::new("W1AW")?;
        let text = MessageText::new("Test")?;
        let message_id = MessageId::new("1")?;
        let expected_wire = build_msg(
            &test_address()?,
            &addressee,
            &text,
            Some(&message_id),
            &default_digipeater_path()?,
        );
        client.session.transport.expect(&expected_wire, &[]);
        let _id = client.send_message(&addressee, &text).await?;

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
        assert_eq!(id.as_str(), "1");
        Ok(())
    }

    #[tokio::test]
    async fn next_event_message_rejected() -> TestResult {
        let radio = mock_radio(PacketDataRate::Bps1200);
        let config = test_config()?;
        let mut client = AprsClient::start(radio, config).await.map_err(|(_, e)| e)?;

        // Send a message to have pending id "1"
        let addressee = MessageAddressee::new("W1AW")?;
        let text = MessageText::new("Test")?;
        let message_id = MessageId::new("1")?;
        let expected_wire = build_msg(
            &test_address()?,
            &addressee,
            &text,
            Some(&message_id),
            &default_digipeater_path()?,
        );
        client.session.transport.expect(&expected_wire, &[]);
        let _id = client.send_message(&addressee, &text).await?;

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
        assert_eq!(id.as_str(), "1");
        Ok(())
    }

    #[tokio::test]
    async fn next_event_raw_packet_for_unknown_data() -> TestResult {
        let radio = mock_radio(PacketDataRate::Bps1200);
        let config = test_config()?;
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
        let radio = mock_radio(PacketDataRate::Bps1200);
        let config = test_config()?;
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
        let radio = mock_radio(PacketDataRate::Bps1200);
        let config = test_config()?;
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
        let radio = mock_radio(PacketDataRate::Bps1200);
        let config = test_config()?;
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
        let radio = mock_radio(PacketDataRate::Bps1200);
        let config = test_config()?;
        let mut client = AprsClient::start(radio, config).await.map_err(|(_, e)| e)?;

        // SmartBeaconing always triggers on first call.
        let expected = build_pos(
            &test_address()?,
            Latitude::new(35.25)?,
            Longitude::new(-97.75)?,
            AprsSymbol::CAR,
            &PositionReportText::default(),
            &default_digipeater_path()?,
        );
        client.session.transport.expect(&expected, &[]);

        let beaconed = client
            .update_motion(
                Speed::from_kmh(50.0)?,
                Heading::new(90.0)?,
                Latitude::new(35.25)?,
                Longitude::new(-97.75)?,
            )
            .await?;
        assert!(beaconed);
        Ok(())
    }

    #[tokio::test]
    async fn update_motion_second_call_no_beacon() -> TestResult {
        let radio = mock_radio(PacketDataRate::Bps1200);
        let config = test_config()?;
        let mut client = AprsClient::start(radio, config).await.map_err(|(_, e)| e)?;

        // First call beacons.
        let expected = build_pos(
            &test_address()?,
            Latitude::new(35.25)?,
            Longitude::new(-97.75)?,
            AprsSymbol::CAR,
            &PositionReportText::default(),
            &default_digipeater_path()?,
        );
        client.session.transport.expect(&expected, &[]);
        let _ = client
            .update_motion(
                Speed::from_kmh(50.0)?,
                Heading::new(90.0)?,
                Latitude::new(35.25)?,
                Longitude::new(-97.75)?,
            )
            .await?;

        // Second call immediately after should NOT beacon.
        let beaconed = client
            .update_motion(
                Speed::from_kmh(50.0)?,
                Heading::new(90.0)?,
                Latitude::new(35.25)?,
                Longitude::new(-97.75)?,
            )
            .await?;
        assert!(!beaconed);
        Ok(())
    }

    #[tokio::test]
    async fn beacon_position_mice_sends_expected_wire_bytes() -> TestResult {
        let radio = mock_radio(PacketDataRate::Bps1200);
        let config = test_config()?;
        let mut client = AprsClient::start(radio, config).await.map_err(|(_, e)| e)?;

        let expected = build_aprs_mice(
            &test_address()?,
            Latitude::new(35.30)?,
            Longitude::new(-82.46)?,
            MiceSpeed::new(25)?,
            Course::new(90)?,
            AprsSymbol::CAR,
            &mice_status_text("mice hw validation"),
            &default_digipeater_path()?,
        );
        client.session.transport.expect(&expected, &[]);

        client
            .beacon_position_mice(
                Latitude::new(35.30)?,
                Longitude::new(-82.46)?,
                MiceSpeed::new(25)?,
                Course::new(90)?,
                &mice_status_text("mice hw validation"),
            )
            .await?;
        Ok(())
    }
}
