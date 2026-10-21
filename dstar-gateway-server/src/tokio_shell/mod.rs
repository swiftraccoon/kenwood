//! Tokio-facing shell: the per-protocol RX endpoint and fan-out glue.
//!
//! [`ProtocolEndpoint::handle_inbound`] is the sans-io entry point
//! used by unit tests; [`ProtocolEndpoint::run`] owns the real
//! `UdpSocket` pump and drives bounded recipient forwarding.
//! Cross-protocol re-encoding lives in [`transcode`].

pub use endpoint::{EndpointOutcome, ProtocolEndpoint, ShellError, VoiceRoute};
pub use transcode::{CrossProtocolEvent, TranscodeError, VoiceEvent, transcode_voice};

pub mod endpoint;
mod fanout;
pub mod transcode;
