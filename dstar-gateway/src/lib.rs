//! D-STAR reflector gateway client library.
//!
//! Provides async UDP clients for the three standard D-STAR reflector
//! protocols: `DExtra` (XRF), `DCS`, and `DPlus` (REF). This library handles
//! the network side of a D-STAR gateway — connecting to reflectors,
//! relaying voice frames, and managing keepalives.
//!
//! # Architecture
//!
//! The gateway sits between an MMDVM modem (radio) and a reflector:
//!
//! ```text
//! [Radio] <--MMDVM--> [your app] <--dstar-gateway--> [Reflector UDP]
//! ```
//!
//! This crate provides the right side. Your application provides the
//! left side (e.g. via `kenwood-thd75`'s `DStarGateway`).
//!
//! # Protocols
//!
//! - **`DExtra`** (XRF reflectors, UDP port 30001): simplest protocol.
//!   Connect/disconnect/poll packets + DSVT voice framing.
//! - **`DCS`** (DCS reflectors, UDP port 30051): 519-byte connect with
//!   HTML client ID, 100-byte voice packets embedding full headers.
//! - **`DPlus`** (REF reflectors, UDP port 20001): requires TCP auth
//!   to `auth.dstargateway.org` before linking. DSVT voice framing.
//!
//! # Reference implementations
//!
//! Protocol formats derived from:
//! - `g4klx/ircDDBGateway` (GPL-2.0) — canonical gateway client
//! - `LX3JL/xlxd` (GPL-2.0) — canonical reflector server
//! - `g4klx/MMDVMHost` (GPL-2.0) — MMDVM modem host
//!
//! # Example
//!
//! Canonical connect → send voice → disconnect flow against a DCS
//! reflector, using the unified [`ReflectorClient`] façade:
//!
//! ```no_run
//! use dstar_gateway::{
//!     Callsign, DStarHeader, Module, Protocol, ReflectorClient,
//!     ReflectorClientParams, StreamId, Suffix, VoiceFrame,
//! };
//! use std::time::Duration;
//!
//! # async fn example() -> Result<(), dstar_gateway::Error> {
//! let params = ReflectorClientParams {
//!     callsign: Callsign::try_from_str("W1AW")?,
//!     local_module: Module::try_from_char('B')?,
//!     reflector_callsign: Callsign::try_from_str("DCS001")?,
//!     reflector_module: Module::try_from_char('C')?,
//!     remote: "1.2.3.4:30051".parse().unwrap(),
//!     protocol: Protocol::Dcs,
//! };
//! let mut client = ReflectorClient::new(params).await?;
//! client.connect_and_wait(Duration::from_secs(5)).await?;
//!
//! let header = DStarHeader {
//!     flag1: 0,
//!     flag2: 0,
//!     flag3: 0,
//!     rpt2: Callsign::try_from_str("DCS001 G")?,
//!     rpt1: Callsign::try_from_str("DCS001 C")?,
//!     ur_call: Callsign::try_from_str("CQCQCQ")?,
//!     my_call: Callsign::try_from_str("W1AW")?,
//!     my_suffix: Suffix::EMPTY,
//! };
//! let stream_id = StreamId::new(0x1234).unwrap();
//!
//! client.send_header(&header, stream_id).await?;
//! let frame = VoiceFrame {
//!     ambe: [0; 9],
//!     slow_data: [0; 3],
//! };
//! for seq in 0..5 {
//!     client.send_voice(stream_id, seq, &frame).await?;
//! }
//! client.send_eot(stream_id, 5).await?;
//! client.disconnect().await?;
//! # Ok(())
//! # }
//! ```

pub mod error;
pub mod header;
pub mod hosts;
pub mod protocol;
pub mod types;
pub mod voice;

pub use error::Error;
pub use header::DStarHeader;
pub use hosts::{HostEntry, HostFile};
pub use protocol::dplus::{DPlusHost, HostList, parse_auth_response};
pub use protocol::{Protocol, ReflectorClient, ReflectorClientParams, ReflectorEvent};
pub use types::{Callsign, Module, StreamId, Suffix, TypeError};
pub use voice::{AMBE_SILENCE, DSTAR_SYNC_BYTES, VoiceFrame};
