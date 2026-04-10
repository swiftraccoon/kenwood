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

pub mod header;
pub mod hosts;
pub mod protocol;
pub mod voice;

pub use header::DStarHeader;
pub use hosts::{HostEntry, HostFile};
pub use protocol::{ReflectorClient, ReflectorEvent};
pub use voice::{AMBE_SILENCE, DSTAR_SYNC_BYTES, VoiceFrame};
