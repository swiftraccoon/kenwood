//! AX.25 v2.2 frame encode/decode codec.
//!
//! Pure, `no_std`-compatible codec for AX.25 Unnumbered Information (UI)
//! and connected-mode frames. Consumers parse raw byte slices into
//! [`Ax25Packet`] via [`parse_ax25`] and produce wire bytes via
//! [`build_ax25`].
//!
//! # Scope
//!
//! - Frame header parsing: source, destination, up to 8 digipeaters.
//! - [`Ax25Control`] decoding (Information / Supervisory / Unnumbered).
//! - [`Ax25Pid`] PID byte decoding (15 canonical values).
//! - FCS (frame check sequence) calculation via [`ax25_fcs`].
//! - Command/Response classification per AX.25 v2.2 §4.3.1.2.
//!
//! Non-goals: APRS parsing (see `aprs`), KISS framing (see `kiss-tnc`),
//! any I/O.
//!
//! # References
//!
//! - AX.25 v2.2: <http://www.ax25.net/AX25.2.2-Jul%2098-2.pdf>

#![no_std]

extern crate alloc;

mod address;
mod control;
mod error;
mod frame;
mod pid;

pub use address::{Ax25Address, Callsign, Ssid};
pub use control::{Ax25Control, CommandResponse, SupervisoryKind, UnnumberedKind};
pub use error::Ax25Error;
pub use frame::{Ax25Packet, MAX_DIGIPEATERS, ax25_fcs, build_ax25, parse_ax25};
pub use pid::Ax25Pid;

// `proptest` is a dev-dependency used only in the integration test
// suites. Acknowledge it here to keep `-D unused-crate-dependencies`
// happy when the lib test crate compiles with dev-deps in scope.
#[cfg(test)]
use proptest as _;
