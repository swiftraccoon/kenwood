//! `StateError` for runtime state-machine residuals.

use crate::session::client::ClientStateKind;
use crate::types::{ProtocolKind, StreamId};

/// Runtime state-machine errors that the typestate cannot prevent.
#[derive(Debug, Clone, thiserror::Error)]
#[non_exhaustive]
pub enum StateError {
    /// `send_header` for stream X while stream Y is still streaming.
    #[error("stream {already_active} is still streaming; cannot start {requested}")]
    StreamAlreadyActive {
        /// The stream id currently in flight.
        already_active: StreamId,
        /// The stream id the caller tried to start.
        requested: StreamId,
    },

    /// `send_voice` with seq > 20 (D-STAR seq is mod 21).
    #[error("voice sequence {got} out of range; D-STAR seq must be 0..21")]
    VoiceSeqOutOfRange {
        /// The rejected seq value.
        got: u8,
    },

    /// Receive callback fired for a stream id with no known session state.
    #[error("received frame for unknown stream id {stream_id}")]
    UnknownStreamId {
        /// The stream id with no matching session.
        stream_id: StreamId,
    },

    /// A method was called on a [`SessionCore`] while it was in the
    /// wrong [`ClientStateKind`] — e.g. `enqueue_connect` outside of
    /// `Configured`/`Authenticated`, or `attach_host_list` on a
    /// non-`DPlus` session.
    ///
    /// The typestate wrapper ([`crate::session::client::Session`]) prevents
    /// this at compile time; the variant exists so direct `SessionCore`
    /// users (tests + the protocol-erased fallback) get a useful
    /// runtime error instead of a panic.
    ///
    /// [`SessionCore`]: crate::session::client::SessionCore
    #[error("{operation} is not valid in {state:?} for protocol {protocol:?}")]
    WrongState {
        /// Which operation was attempted.
        operation: &'static str,
        /// The current runtime state.
        state: ClientStateKind,
        /// The runtime protocol discriminator.
        protocol: ProtocolKind,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    const SID_1111: StreamId = StreamId::new(0x1111).unwrap();
    const SID_2222: StreamId = StreamId::new(0x2222).unwrap();

    #[test]
    fn state_error_stream_already_active_display() {
        let err = StateError::StreamAlreadyActive {
            already_active: SID_1111,
            requested: SID_2222,
        };
        let s = err.to_string();
        assert!(s.contains("0x1111"));
        assert!(s.contains("0x2222"));
    }

    #[test]
    fn state_error_voice_seq_out_of_range_display() {
        let err = StateError::VoiceSeqOutOfRange { got: 25 };
        assert!(err.to_string().contains("25"));
    }

    #[test]
    fn state_error_wrong_state_display() {
        let err = StateError::WrongState {
            operation: "enqueue_connect",
            state: ClientStateKind::Connected,
            protocol: ProtocolKind::DPlus,
        };
        let s = err.to_string();
        assert!(s.contains("enqueue_connect"));
        assert!(s.contains("Connected"));
        assert!(s.contains("DPlus"));
    }
}
