#![doc = include_str!("../README.md")]
//! Async tokio shell for the `dstar-gateway-core` D-STAR reflector library.
//!
//! This crate provides the async API consumers will use: `AsyncSession<P>`
//! built on top of the sans-io typestate `Session<P, S>` in the core crate.
//!
//! # Architecture
//!
//! `dstar-gateway-core` is the **runtime-agnostic, I/O-free** sans-io core.
//! It contains the codecs and typestate session machines. This crate wraps
//! that core in a `tokio::net::UdpSocket`-backed driver loop, spawns it as
//! a task, and exposes an [`tokio_shell::AsyncSession`] handle with
//! `send_header` / `send_voice` / `send_eot` / `disconnect` methods.
//!
//! ```text
//! [your app] <--AsyncSession--> [SessionLoop task] <--UdpSocket--> [Reflector]
//!                                        |
//!                                   Session<P, Connected>
//!                                   (sans-io core)
//! ```
//!
//! # Feature flags
//!
//! - `blocking`: additionally compiles a blocking-shell variant
//!   under the `blocking_shell` module. Its caller-driven API uses
//!   `std::net::UdpSocket` and does not require running a tokio runtime;
//!   tokio remains an unconditional dependency of this async crate.
//! - `insecure-plaintext-xlx-directory`: pulls `reqwest` and exposes
//!   `insecure_plaintext_xlx_directory` for an explicit fetch from the XLX
//!   registry's HTTP-only endpoint. The response has no transport
//!   confidentiality, authenticity, or integrity. Disabled by default.
//!
//! # Core re-exports
//!
//! The core types are re-exported from `dstar-gateway-core` for
//! consumer convenience, so downstream crates don't need a separate
//! `dstar-gateway-core` dependency for the common types.

pub mod auth;
pub mod tokio_shell;

#[cfg(feature = "blocking")]
pub mod blocking_shell;

#[cfg(feature = "insecure-plaintext-xlx-directory")]
pub mod insecure_plaintext_xlx_directory;

// Re-export the core's leaf types so simple consumers need only this
// crate. The typestate session machinery (`Session`, `Driver`, the
// state and protocol markers) is deliberately NOT re-exported:
// driving the sans-io core directly requires an explicit
// `dstar-gateway-core` dependency.
pub use dstar_gateway_core::{
    AMBE_SILENCE, BandLetter, Callsign, DSTAR_SYNC_BYTES, DstarHeader, Error, HostEntry, HostFile,
    Module, ProtocolKind, ReflectorCallsign, StreamId, Suffix, TypeError, VoiceFrame,
};

#[cfg(test)]
use pcap_parser as _;
#[cfg(test)]
use tracing_subscriber as _;
#[cfg(test)]
use trybuild as _;
