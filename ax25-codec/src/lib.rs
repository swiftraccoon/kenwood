//! AX.25 v2.2 frame encode/decode codec.
//!
//! Pure, `no_std`-compatible codec for AX.25 modulo-8 Information,
//! Supervisory, and Unnumbered frames. Consumers parse raw byte slices into
//! [`Ax25Packet`] via [`parse_ax25`] and produce wire bytes via
//! [`build_ax25`].
//!
//! # Scope
//!
//! - Frame header parsing: source, destination, up to 8 digipeaters
//!   (each a [`RouteEntry`] carrying the AX.25 H-bit).
//! - [`Ax25Control`] decoding (Information / Supervisory / Unnumbered).
//! - [`Ax25Pid`] one- and two-octet PID field decoding, including the `0xFF`
//!   escape form and unassigned one-octet values.
//! - FCS (frame check sequence) calculation via [`ax25_fcs`].
//! - Command/Response classification per AX.25 v2.2 §6.1.2.
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
mod path;
mod pid;

pub use address::{Ax25Address, Callsign, RouteEntry, Ssid};
pub use control::{
    Ax25Control, Ax25SequenceNumber, CommandResponse, SupervisoryKind, UnknownUnnumberedKind,
    UnnumberedKind,
};
pub use error::Ax25Error;
pub use frame::{Ax25Packet, ax25_fcs, build_ax25, parse_ax25};
pub use path::{DigipeaterPath, MAX_DIGIPEATERS};
pub use pid::{Ax25Pid, UnknownPid};
