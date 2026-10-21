#![doc = include_str!("../README.md")]
#![no_std]

extern crate alloc;

pub use address::{Ax25Address, Callsign, RouteEntry, Ssid};
pub use control::{
    Ax25Control, Ax25SequenceNumber, CommandResponse, PollFinal, SupervisoryKind,
    UnknownUnnumberedKind, UnnumberedKind,
};
pub use error::Ax25Error;
pub use frame::{Ax25Header, Ax25Packet, ax25_fcs};
pub use path::{DigipeaterPath, MAX_DIGIPEATERS};
pub use pid::{Ax25Pid, UnknownPid};

mod address;
mod control;
mod error;
mod frame;
mod path;
mod pid;
