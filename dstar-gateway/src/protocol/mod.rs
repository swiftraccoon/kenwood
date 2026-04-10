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

use crate::header::DStarHeader;
use crate::voice::VoiceFrame;

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
        stream_id: u16,
    },
    /// Incoming voice data frame.
    VoiceData {
        /// Stream identifier.
        stream_id: u16,
        /// Frame sequence number (0-20 cycle).
        seq: u8,
        /// Voice frame (AMBE + slow data).
        frame: VoiceFrame,
    },
    /// End of incoming voice transmission.
    VoiceEnd {
        /// Stream identifier of the ended stream.
        stream_id: u16,
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

/// Unified reflector client wrapping either `DExtra` or `DPlus`.
#[derive(Debug)]
pub enum ReflectorClient {
    /// `DExtra`/XRF/XLX protocol.
    DExtra(dextra::DExtraClient),
    /// `DPlus`/REF protocol.
    DPlus(dplus::DPlusClient),
}

impl ReflectorClient {
    /// Create a client for the appropriate protocol based on reflector name prefix.
    ///
    /// # Errors
    ///
    /// Returns an I/O error if the UDP socket cannot be bound.
    pub async fn new(
        callsign: &str,
        module: char,
        remote: std::net::SocketAddr,
        prefix: &str,
    ) -> Result<Self, std::io::Error> {
        match prefix {
            "XRF" | "XLX" => Ok(Self::DExtra(
                dextra::DExtraClient::new(callsign, module, remote).await?,
            )),
            "REF" => Ok(Self::DPlus(
                dplus::DPlusClient::new(callsign, module, remote).await?,
            )),
            _ => Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("unsupported reflector prefix: {prefix}"),
            )),
        }
    }

    /// Authenticate with the protocol's auth server if required.
    ///
    /// For `DPlus` (REF reflectors), performs TCP authentication to
    /// `auth.dstargateway.org`. For `DExtra`, this is a no-op.
    ///
    /// # Errors
    ///
    /// Returns an I/O error if the auth connection fails.
    pub async fn authenticate(&self) -> Result<(), std::io::Error> {
        match self {
            Self::DExtra(_) => Ok(()),
            Self::DPlus(c) => c.authenticate().await,
        }
    }

    /// Send the connect request.
    ///
    /// # Errors
    ///
    /// Returns an I/O error if the send fails.
    pub async fn connect(&mut self) -> Result<(), std::io::Error> {
        match self {
            Self::DExtra(c) => c.connect().await,
            Self::DPlus(c) => c.connect().await,
        }
    }

    /// Send the disconnect request.
    ///
    /// # Errors
    ///
    /// Returns an I/O error if the send fails.
    pub async fn disconnect(&mut self) -> Result<(), std::io::Error> {
        match self {
            Self::DExtra(c) => c.disconnect().await,
            Self::DPlus(c) => c.disconnect().await,
        }
    }

    /// Poll for the next event (keepalives + receive).
    ///
    /// # Errors
    ///
    /// Returns an I/O error on socket failures.
    pub async fn poll(&mut self) -> Result<Option<ReflectorEvent>, std::io::Error> {
        match self {
            Self::DExtra(c) => c.poll().await,
            Self::DPlus(c) => c.poll().await,
        }
    }

    /// Send a voice header.
    ///
    /// # Errors
    ///
    /// Returns an I/O error if the send fails.
    pub async fn send_header(
        &self,
        header: &DStarHeader,
        stream_id: u16,
    ) -> Result<(), std::io::Error> {
        match self {
            Self::DExtra(c) => c.send_header(header, stream_id).await,
            Self::DPlus(c) => c.send_header(header, stream_id).await,
        }
    }

    /// Send a voice data frame.
    ///
    /// # Errors
    ///
    /// Returns an I/O error if the send fails.
    pub async fn send_voice(
        &self,
        stream_id: u16,
        seq: u8,
        frame: &VoiceFrame,
    ) -> Result<(), std::io::Error> {
        match self {
            Self::DExtra(c) => c.send_voice(stream_id, seq, frame).await,
            Self::DPlus(c) => c.send_voice(stream_id, seq, frame).await,
        }
    }

    /// Send an end-of-transmission.
    ///
    /// # Errors
    ///
    /// Returns an I/O error if the send fails.
    pub async fn send_eot(&self, stream_id: u16, seq: u8) -> Result<(), std::io::Error> {
        match self {
            Self::DExtra(c) => c.send_eot(stream_id, seq).await,
            Self::DPlus(c) => c.send_eot(stream_id, seq).await,
        }
    }

    /// Current connection state.
    #[must_use]
    pub const fn state(&self) -> ConnectionState {
        match self {
            Self::DExtra(c) => c.state(),
            Self::DPlus(c) => c.state(),
        }
    }
}
