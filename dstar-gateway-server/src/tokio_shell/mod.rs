//! Tokio-facing shell: the per-protocol RX endpoint and fan-out glue.
//!
//! [`ProtocolEndpoint::handle_inbound`] is the sans-io entry point
//! used by unit tests; [`ProtocolEndpoint::run`] owns the real
//! `UdpSocket` pump and drives the fan-out engine in [`fanout`].
//! Cross-protocol re-encoding lives in [`transcode`].

pub mod endpoint;
pub mod fanout;
pub mod transcode;

pub use endpoint::{EndpointOutcome, ProtocolEndpoint, ShellError};
pub use fanout::{FanOutReport, fan_out_voice, fan_out_voice_at};
pub use transcode::{CrossProtocolEvent, TranscodeError, VoiceEvent, transcode_voice};
