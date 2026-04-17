//! KISS TNC (Terminal Node Controller) wire protocol codec.
//!
//! This crate is a **runtime-agnostic, I/O-free** implementation of the
//! KISS TNC framing protocol described by Chepponis & Karn (1987). It
//! provides:
//!
//! - Constants for the KISS special bytes ([`FEND`], [`FESC`], [`TFEND`],
//!   [`TFESC`]) and the command type bytes ([`CMD_DATA`],
//!   [`CMD_TX_DELAY`], etc.).
//! - Typed [`KissCommand`] and [`KissPort`] wrappers over the raw
//!   nibble-encoded type byte.
//! - A decoded [`KissFrame`] type with convenience constructors.
//! - One-shot [`encode_kiss_frame`] / [`encode_kiss_frame_into`] and
//!   [`decode_kiss_frame`] helpers for a full frame in a single buffer.
//! - A streaming [`KissDecoder`] that accepts arbitrary byte chunks
//!   from a serial port and yields complete frames as they arrive.
//!
//! The crate is `#![no_std]`; the only allocator-backed types come from
//! [`alloc`], which callers must provide.
//!
//! # Example
//!
//! ```
//! use kiss_tnc::{CMD_DATA, FEND, KissDecoder};
//!
//! let mut decoder = KissDecoder::new();
//! let bytes = &[FEND, 0x00, 0x01, FEND];
//! decoder.push(bytes);
//! // In the real crate this returns the decoded frame; the scaffold
//! // stub returns an error, so we only demonstrate the API shape here.
//! let _ = decoder.next_frame();
//! let _ = (CMD_DATA, FEND);
//! ```
//!
//! # References
//!
//! - KISS protocol: <http://www.ka9q.net/papers/kiss.html>
//! - AX.25 v2.2: <http://www.ax25.net/AX25.2.2-Jul%2098-2.pdf>

#![no_std]

extern crate alloc;

mod command;
mod decoder;
mod error;
mod frame;

pub use command::{
    CMD_DATA, CMD_FULL_DUPLEX, CMD_PERSISTENCE, CMD_RETURN, CMD_SET_HARDWARE, CMD_SLOT_TIME,
    CMD_TX_DELAY, CMD_TX_TAIL, FEND, FESC, KissCommand, KissPort, TFEND, TFESC,
};
pub use decoder::KissDecoder;
pub use error::KissError;
pub use frame::{KissFrame, decode_kiss_frame, encode_kiss_frame, encode_kiss_frame_into};

// `proptest` is a dev-dependency used only in the integration test
// suites. Acknowledge it here to keep `-D unused-crate-dependencies`
// happy when the lib test crate compiles with dev-deps in scope.
#[cfg(test)]
use proptest as _;
