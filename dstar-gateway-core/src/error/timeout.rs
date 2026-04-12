//! `TimeoutError` typed deadlines.

use std::net::SocketAddr;
use std::time::Duration;

use crate::types::StreamId;

/// Typed deadline-exceeded errors.
///
/// Each variant carries enough context to write a useful log line
/// and to drive recovery logic.
#[derive(Debug, Clone, thiserror::Error)]
#[non_exhaustive]
pub enum TimeoutError {
    /// Reflector did not acknowledge LINK1/LINK2/DCS connect within deadline.
    #[error("connect timeout: deadline {deadline:?}, elapsed {elapsed:?}, last state {last_state}")]
    Connect {
        /// Configured deadline.
        deadline: Duration,
        /// Wall time elapsed before tripping.
        elapsed: Duration,
        /// Last state name observed before the trip (string tag;
        /// see [`crate::session::client::ClientStateKind`] for the typed form).
        last_state: &'static str,
    },

    /// Reflector stopped responding to keepalives.
    #[error("keepalive inactivity: deadline {deadline:?}, elapsed {elapsed:?}, peer {peer}")]
    KeepaliveInactivity {
        /// Configured deadline.
        deadline: Duration,
        /// Wall time elapsed.
        elapsed: Duration,
        /// Affected peer.
        peer: SocketAddr,
    },

    /// Disconnect was requested but the reflector never `ACKed` the unlink.
    #[error("disconnect ack timeout: deadline {deadline:?}, elapsed {elapsed:?}")]
    Disconnect {
        /// Configured deadline.
        deadline: Duration,
        /// Wall time elapsed.
        elapsed: Duration,
    },

    /// Voice stream stopped mid-transmission with no EOT for >2s.
    #[error("voice inactivity on stream {stream_id}, elapsed {elapsed:?}")]
    VoiceInactivity {
        /// Affected stream.
        stream_id: StreamId,
        /// Time since last frame on this stream.
        elapsed: Duration,
    },

    /// `DPlus` auth TCP connect took too long.
    #[error("auth connect timeout: deadline {deadline:?}, elapsed {elapsed:?}")]
    AuthConnect {
        /// Configured deadline.
        deadline: Duration,
        /// Wall time elapsed.
        elapsed: Duration,
    },

    /// `DPlus` auth read stalled waiting for the host list.
    #[error(
        "auth read stalled: deadline {deadline:?}, elapsed {elapsed:?}, {bytes_so_far} bytes received"
    )]
    AuthRead {
        /// Configured deadline.
        deadline: Duration,
        /// Wall time elapsed.
        elapsed: Duration,
        /// Bytes received before the stall.
        bytes_so_far: usize,
    },
}

#[cfg(test)]
mod tests {
    use std::net::{IpAddr, Ipv4Addr};

    use super::*;

    const CAFE: StreamId = {
        // SAFETY: Option::unwrap is const since 1.83; 0xCAFE != 0 so this
        // is a compile-time assertion, never a runtime panic.
        StreamId::new(0xCAFE).unwrap()
    };

    #[test]
    fn timeout_connect_display_includes_state() {
        let err = TimeoutError::Connect {
            deadline: Duration::from_secs(5),
            elapsed: Duration::from_secs(5),
            last_state: "Connecting",
        };
        let s = err.to_string();
        assert!(s.contains("Connecting"));
    }

    #[test]
    fn timeout_keepalive_includes_peer() {
        let peer = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 20001);
        let err = TimeoutError::KeepaliveInactivity {
            deadline: Duration::from_secs(30),
            elapsed: Duration::from_secs(31),
            peer,
        };
        let s = err.to_string();
        assert!(s.contains("127.0.0.1:20001"));
    }

    #[test]
    fn timeout_voice_inactivity_includes_stream_id() {
        let err = TimeoutError::VoiceInactivity {
            stream_id: CAFE,
            elapsed: Duration::from_secs(2),
        };
        let s = err.to_string();
        assert!(s.contains("0xCAFE"));
    }
}
