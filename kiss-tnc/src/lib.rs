#![doc = include_str!("../README.md")]
#![no_std]

extern crate alloc;

// The library test target includes the integration suites' dev-dependency.
#[cfg(test)]
use proptest as _;

pub use command::{
    CMD_DATA, CMD_FULL_DUPLEX, CMD_PERSISTENCE, CMD_RETURN, CMD_SET_HARDWARE, CMD_SLOT_TIME,
    CMD_TX_DELAY, CMD_TX_TAIL, FEND, FESC, KissCommand, KissPort, TFEND, TFESC,
};
pub use decoder::{DEFAULT_MAX_FRAME_LEN, KissDecoder};
pub use error::KissError;
pub use frame::KissFrame;

mod command;
mod decoder;
mod error;
mod frame;
