//! D-STAR reflector protocol implementations.
//!
//! Three protocols are supported:
//!
//! - [`dextra`] — `DExtra` (XRF reflectors, UDP port 30001)
//! - [`dcs`] — `DCS` (DCS reflectors, UDP port 30051)
//! - [`dplus`] — `DPlus` (REF reflectors, UDP port 20001 + TCP auth)
//!
//! Each protocol provides:
//! - Packet builders (`build_connect`, `build_voice`, etc.)
//! - Packet parser (`parse_packet`)
//! - An async [`ReflectorClient`] that manages the UDP connection,
//!   keepalives, and voice frame relay.
//!
//! Protocol formats verified against `g4klx/ircDDBGateway` (GPL-2.0)
//! and `LX3JL/xlxd` (GPL-2.0).

pub mod dcs;
pub mod dextra;
pub mod dplus;

/// Format the first `MAX_HEX_HEAD_BYTES` of a packet as a lowercase
/// hex string with no separators. Used by the protocol clients to
/// emit `trace`-level packet dumps without blowing up the log file
/// on long voice payloads.
pub(crate) fn format_hex_head(bytes: &[u8]) -> String {
    const MAX_HEX_HEAD_BYTES: usize = 32;
    let take = bytes.len().min(MAX_HEX_HEAD_BYTES);
    let mut out = String::with_capacity(take * 2);
    for b in &bytes[..take] {
        use std::fmt::Write as _;
        let _ = write!(&mut out, "{b:02x}");
    }
    out
}

#[cfg(test)]
mod reference_vectors;

use std::time::Duration;

use crate::error::Error;
use crate::header::DStarHeader;
use crate::types::{Callsign, Module, StreamId};
use crate::voice::VoiceFrame;

/// D-STAR reflector protocol selector.
///
/// Used by [`ReflectorClientParams`] to choose which underlying
/// protocol client to construct.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Protocol {
    /// `DExtra` (XRF/XLX reflectors, UDP port 30001).
    DExtra,
    /// `DPlus` (REF reflectors, UDP port 20001 + TCP auth).
    DPlus,
    /// `DCS` (DCS reflectors, UDP port 30051).
    Dcs,
}

impl Protocol {
    /// Identify the protocol from a reflector callsign prefix.
    ///
    /// Examines the first three characters (case-insensitive):
    ///
    /// - `"XRF"` or `"XLX"` → [`Protocol::DExtra`]
    /// - `"REF"` → [`Protocol::DPlus`]
    /// - `"DCS"` → [`Protocol::Dcs`]
    ///
    /// Returns `None` for any other prefix or if the input is
    /// shorter than 3 ASCII characters.
    ///
    /// # Examples
    ///
    /// ```
    /// use dstar_gateway::Protocol;
    ///
    /// assert_eq!(Protocol::from_reflector_prefix("REF030"), Some(Protocol::DPlus));
    /// assert_eq!(Protocol::from_reflector_prefix("xlx307"), Some(Protocol::DExtra));
    /// assert_eq!(Protocol::from_reflector_prefix("foo"), None);
    /// ```
    #[must_use]
    pub fn from_reflector_prefix(name: &str) -> Option<Self> {
        let bytes = name.as_bytes();
        if bytes.len() < 3 {
            return None;
        }
        let prefix: [u8; 3] = [
            bytes[0].to_ascii_uppercase(),
            bytes[1].to_ascii_uppercase(),
            bytes[2].to_ascii_uppercase(),
        ];
        match &prefix {
            b"XRF" | b"XLX" => Some(Self::DExtra),
            b"REF" => Some(Self::DPlus),
            b"DCS" => Some(Self::Dcs),
            _ => None,
        }
    }

    /// Whether this protocol requires calling [`ReflectorClient::authenticate`]
    /// before [`ReflectorClient::connect`] or [`ReflectorClient::connect_and_wait`].
    ///
    /// Only [`Protocol::DPlus`] requires authentication (TCP call to an
    /// auth server). `DExtra` and `DCS` do not.
    #[must_use]
    pub const fn requires_authentication(self) -> bool {
        matches!(self, Self::DPlus)
    }
}

/// Parameters for constructing a [`ReflectorClient`].
///
/// All protocols share the same parameter set. Fields that are
/// unused by a particular protocol (for example `reflector_callsign`
/// for `DPlus`) are still required for API uniformity.
#[derive(Debug, Clone)]
pub struct ReflectorClientParams {
    /// Originating station callsign.
    pub callsign: Callsign,
    /// Module letter on the originating station.
    pub local_module: Module,
    /// Reflector's own callsign (e.g. `"REF030"`, `"XLX307"`, `"DCS001"`).
    ///
    /// `DPlus` does not use this field; it is still required for API
    /// uniformity.
    pub reflector_callsign: Callsign,
    /// Module letter to link to on the reflector.
    pub reflector_module: Module,
    /// Reflector socket address.
    pub remote: std::net::SocketAddr,
    /// Protocol selector.
    pub protocol: Protocol,
}

/// An event received from a reflector.
///
/// Produced by the protocol client's `poll` method. Each variant
/// represents a distinct category of reflector activity.
#[derive(Debug, Clone)]
pub enum ReflectorEvent {
    /// Connection to the reflector was accepted.
    Connected,
    /// Connection to the reflector was rejected.
    Rejected,
    /// Disconnected from the reflector.
    Disconnected,
    /// Keepalive echo received (reflector is alive).
    PollEcho,
    /// Incoming voice stream started (header received).
    VoiceStart {
        /// D-STAR radio header with routing information.
        header: DStarHeader,
        /// Stream identifier for correlating voice frames.
        ///
        /// Validated non-zero at parse time — packets carrying
        /// `stream_id == 0` are malformed per the D-STAR spec and are
        /// dropped by the protocol parsers before reaching this event.
        stream_id: StreamId,
    },
    /// Incoming voice data frame.
    VoiceData {
        /// Stream identifier.
        ///
        /// Validated non-zero at parse time — packets carrying
        /// `stream_id == 0` are dropped by the protocol parsers.
        stream_id: StreamId,
        /// Frame sequence number (0-20 cycle).
        seq: u8,
        /// Voice frame (AMBE + slow data).
        frame: VoiceFrame,
    },
    /// End of incoming voice transmission.
    VoiceEnd {
        /// Stream identifier of the ended stream.
        ///
        /// Validated non-zero at parse time — packets carrying
        /// `stream_id == 0` are dropped by the protocol parsers.
        stream_id: StreamId,
    },
}

/// Connection state of a reflector protocol.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionState {
    /// Not connected to any reflector.
    Disconnected,
    /// Connect request sent, waiting for acknowledgement.
    Connecting,
    /// Connected and operational.
    Connected,
    /// Disconnect request sent, waiting for acknowledgement.
    Disconnecting,
}

/// Unified reflector client wrapping `DExtra`, `DPlus`, or `DCS`.
///
/// Each protocol has slightly different APIs, particularly `DCS`
/// which embeds the full header in every voice frame. The per-protocol
/// clients now own their own cached TX header internally, so the
/// unified enum presents a uniform API.
#[derive(Debug)]
pub enum ReflectorClient {
    /// `DExtra`/XRF/XLX protocol.
    DExtra(dextra::DExtraClient),
    /// `DPlus`/REF protocol.
    DPlus(dplus::DPlusClient),
    /// `DCS` protocol.
    Dcs(dcs::DcsClient),
}

impl ReflectorClient {
    /// Create a client for the protocol selected by `params.protocol`.
    ///
    /// All three protocols take the same [`ReflectorClientParams`]
    /// struct. Fields that a particular protocol does not use (for
    /// example `reflector_callsign` for `DPlus`) are still required
    /// for API uniformity.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Io`] if the UDP socket cannot be bound.
    pub async fn new(params: ReflectorClientParams) -> Result<Self, Error> {
        let ReflectorClientParams {
            callsign,
            local_module,
            reflector_callsign,
            reflector_module,
            remote,
            protocol,
        } = params;
        match protocol {
            Protocol::DExtra => Ok(Self::DExtra(
                dextra::DExtraClient::new(callsign, local_module, reflector_module, remote).await?,
            )),
            Protocol::DPlus => Ok(Self::DPlus(
                dplus::DPlusClient::new(callsign, local_module, remote).await?,
            )),
            Protocol::Dcs => Ok(Self::Dcs(
                dcs::DcsClient::new(
                    callsign,
                    local_module,
                    reflector_callsign,
                    reflector_module,
                    remote,
                )
                .await?,
            )),
        }
    }

    /// Authenticate with the protocol's auth server if required.
    ///
    /// For `DPlus` (REF reflectors), performs TCP authentication to
    /// `auth.dstargateway.org`. This is a no-op for `DExtra` and `DCS`
    /// — neither protocol uses an auth server, so the matching arms
    /// return `Ok(())` without performing any I/O.
    ///
    /// This method is safe to call unconditionally on every client:
    /// the no-op paths do nothing. Callers that want to skip the call
    /// entirely for non-`DPlus` protocols can gate it on
    /// [`Protocol::requires_authentication`]:
    ///
    /// ```no_run
    /// # use dstar_gateway::{Protocol, ReflectorClient, ReflectorClientParams,
    /// #     Callsign, Module};
    /// # async fn example(mut client: ReflectorClient, protocol: Protocol)
    /// #     -> Result<(), dstar_gateway::Error> {
    /// if protocol.requires_authentication() {
    ///     client.authenticate().await?;
    /// }
    /// client.connect().await?;
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// # Errors
    ///
    /// Returns [`Error::Io`] if the TCP auth connection fails, or
    /// [`Error::AuthResponseInvalid`] if the auth server returns a
    /// malformed response. The `DExtra` and `DCS` arms never fail.
    pub async fn authenticate(&mut self) -> Result<(), Error> {
        match self {
            Self::DExtra(_) | Self::Dcs(_) => Ok(()),
            Self::DPlus(c) => c.authenticate().await,
        }
    }

    /// Send the connect request.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Io`] if the send fails.
    pub async fn connect(&mut self) -> Result<(), Error> {
        match self {
            Self::DExtra(c) => c.connect().await,
            Self::DPlus(c) => c.connect().await,
            Self::Dcs(c) => c.connect().await,
        }
    }

    /// Connect to the reflector and wait for acknowledgement or timeout.
    ///
    /// Drives the state machine internally for the inner client. See
    /// [`DExtraClient::connect_and_wait`](dextra::DExtraClient::connect_and_wait),
    /// [`DPlusClient::connect_and_wait`](dplus::DPlusClient::connect_and_wait),
    /// and [`DcsClient::connect_and_wait`](dcs::DcsClient::connect_and_wait)
    /// for per-protocol details.
    ///
    /// # Errors
    ///
    /// See [`Error::ConnectTimeout`], [`Error::Rejected`], [`Error::Io`].
    pub async fn connect_and_wait(&mut self, timeout: Duration) -> Result<(), Error> {
        let proto = match self {
            Self::DExtra(_) => Protocol::DExtra,
            Self::DPlus(_) => Protocol::DPlus,
            Self::Dcs(_) => Protocol::Dcs,
        };
        tracing::info!(
            target: "dstar_gateway::reflector",
            protocol = ?proto,
            timeout_ms = u64::try_from(timeout.as_millis()).unwrap_or(u64::MAX),
            "ReflectorClient connect_and_wait"
        );
        let result = match self {
            Self::DExtra(c) => c.connect_and_wait(timeout).await,
            Self::DPlus(c) => c.connect_and_wait(timeout).await,
            Self::Dcs(c) => c.connect_and_wait(timeout).await,
        };
        match &result {
            Ok(()) => tracing::info!(
                target: "dstar_gateway::reflector",
                protocol = ?proto,
                "ReflectorClient connected"
            ),
            Err(e) => tracing::info!(
                target: "dstar_gateway::reflector",
                protocol = ?proto,
                error = %e,
                "ReflectorClient connect_and_wait failed"
            ),
        }
        result
    }

    /// Send the disconnect request.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Io`] if the send fails.
    pub async fn disconnect(&mut self) -> Result<(), Error> {
        match self {
            Self::DExtra(c) => c.disconnect().await,
            Self::DPlus(c) => c.disconnect().await,
            Self::Dcs(c) => c.disconnect().await,
        }
    }

    /// Poll for the next event (keepalives + receive).
    ///
    /// # Errors
    ///
    /// Returns [`Error::Io`] on socket failures.
    pub async fn poll(&mut self) -> Result<Option<ReflectorEvent>, Error> {
        match self {
            Self::DExtra(c) => c.poll().await,
            Self::DPlus(c) => c.poll().await,
            Self::Dcs(c) => c.poll().await,
        }
    }

    /// Send a voice header to start a new stream.
    ///
    /// For `DCS`, the header is cached internally by the inner client
    /// and reused for every subsequent [`send_voice`](Self::send_voice)
    /// and [`send_eot`](Self::send_eot) call.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Io`] if the send fails.
    pub async fn send_header(
        &mut self,
        header: &DStarHeader,
        stream_id: StreamId,
    ) -> Result<(), Error> {
        match self {
            Self::DExtra(c) => c.send_header(header, stream_id).await,
            Self::DPlus(c) => c.send_header(header, stream_id).await,
            Self::Dcs(c) => c.send_header(header, stream_id).await,
        }
    }

    /// Send a voice data frame.
    ///
    /// For `DCS`, requires a prior [`send_header`](Self::send_header)
    /// call to cache the header. Returns an error if no header is cached.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Io`] if the send fails, or [`Error::NoTxHeader`]
    /// if `DCS` is used without a prior [`send_header`](Self::send_header).
    pub async fn send_voice(
        &mut self,
        stream_id: StreamId,
        seq: u8,
        frame: &VoiceFrame,
    ) -> Result<(), Error> {
        match self {
            Self::DExtra(c) => c.send_voice(stream_id, seq, frame).await,
            Self::DPlus(c) => c.send_voice(stream_id, seq, frame).await,
            Self::Dcs(c) => c.send_voice(stream_id, seq, frame).await,
        }
    }

    /// Send an end-of-transmission.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Io`] if the send fails, or [`Error::NoTxHeader`]
    /// if `DCS` is used without a prior [`send_header`](Self::send_header).
    pub async fn send_eot(&mut self, stream_id: StreamId, seq: u8) -> Result<(), Error> {
        match self {
            Self::DExtra(c) => c.send_eot(stream_id, seq).await,
            Self::DPlus(c) => c.send_eot(stream_id, seq).await,
            Self::Dcs(c) => c.send_eot(stream_id, seq).await,
        }
    }

    /// Current connection state.
    #[must_use]
    pub const fn state(&self) -> ConnectionState {
        match self {
            Self::DExtra(c) => c.state(),
            Self::DPlus(c) => c.state(),
            Self::Dcs(c) => c.state(),
        }
    }

    /// Override the inner client's keepalive poll interval.
    ///
    /// Forwards to
    /// [`DExtraClient::set_poll_interval`](dextra::DExtraClient::set_poll_interval),
    /// [`DPlusClient::set_poll_interval`](dplus::DPlusClient::set_poll_interval),
    /// or [`DcsClient::set_poll_interval`](dcs::DcsClient::set_poll_interval)
    /// depending on the active variant. Use this to shorten keepalive
    /// cadence on NAT-traversing links with short connection-tracking timers.
    pub const fn set_poll_interval(&mut self, interval: Duration) {
        match self {
            Self::DExtra(c) => c.set_poll_interval(interval),
            Self::DPlus(c) => c.set_poll_interval(interval),
            Self::Dcs(c) => c.set_poll_interval(interval),
        }
    }

    /// Return the `DPlus` auth host list if this client is `DPlus` and
    /// [`ReflectorClient::authenticate`] has been called successfully.
    ///
    /// Returns `None` for `DExtra` and `DCS` clients (neither protocol
    /// uses an auth server).
    ///
    /// The list is populated by [`ReflectorClient::authenticate`] from
    /// the TCP response at `auth.dstargateway.org`.
    #[must_use]
    pub const fn auth_hosts(&self) -> Option<&dplus::HostList> {
        match self {
            Self::DPlus(c) => Some(c.auth_hosts()),
            Self::DExtra(_) | Self::Dcs(_) => None,
        }
    }
}

#[cfg(test)]
mod protocol_helper_tests {
    use super::Protocol;

    #[test]
    fn protocol_from_prefix_xrf_is_dextra() {
        assert_eq!(
            Protocol::from_reflector_prefix("XRF030"),
            Some(Protocol::DExtra)
        );
        assert_eq!(
            Protocol::from_reflector_prefix("XLX307"),
            Some(Protocol::DExtra)
        );
        assert_eq!(
            Protocol::from_reflector_prefix("REF030"),
            Some(Protocol::DPlus)
        );
        assert_eq!(
            Protocol::from_reflector_prefix("DCS001"),
            Some(Protocol::Dcs)
        );
    }

    #[test]
    fn protocol_from_prefix_case_insensitive() {
        assert_eq!(
            Protocol::from_reflector_prefix("xrf030"),
            Some(Protocol::DExtra)
        );
        assert_eq!(
            Protocol::from_reflector_prefix("xlx307"),
            Some(Protocol::DExtra)
        );
        assert_eq!(
            Protocol::from_reflector_prefix("ref030"),
            Some(Protocol::DPlus)
        );
        assert_eq!(
            Protocol::from_reflector_prefix("dcs001"),
            Some(Protocol::Dcs)
        );
        assert_eq!(
            Protocol::from_reflector_prefix("Ref030"),
            Some(Protocol::DPlus)
        );
    }

    #[test]
    fn protocol_from_prefix_unknown_returns_none() {
        assert_eq!(Protocol::from_reflector_prefix("ABC123"), None);
        assert_eq!(Protocol::from_reflector_prefix("FOO"), None);
        assert_eq!(Protocol::from_reflector_prefix("W1AW  "), None);
    }

    #[test]
    fn protocol_from_prefix_short_input_returns_none() {
        assert_eq!(Protocol::from_reflector_prefix(""), None);
        assert_eq!(Protocol::from_reflector_prefix("X"), None);
        assert_eq!(Protocol::from_reflector_prefix("XR"), None);
    }

    #[test]
    fn protocol_requires_authentication_only_dplus() {
        assert!(Protocol::DPlus.requires_authentication());
        assert!(!Protocol::DExtra.requires_authentication());
        assert!(!Protocol::Dcs.requires_authentication());
    }
}
