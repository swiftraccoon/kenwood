//! MMDVM (Multi-Mode Digital Voice Modem) serial protocol codec.
//!
//! This module implements the binary framing protocol used by MMDVM-compatible
//! modems for D-STAR digital voice communication. The codec is pure logic with
//! no I/O or async dependencies --- it encodes and decodes byte frames suitable
//! for transmission over any serial transport.
//!
//! # Frame format (MMDVM Specification 20150922)
//!
//! Every MMDVM frame has the structure:
//!
//! ```text
//! [0xE0] [length] [command] [payload...]
//! ```
//!
//! - Byte 0: Start marker, always `0xE0`.
//! - Byte 1: Total frame length (includes start, length, command, and payload).
//! - Byte 2: Command or response type identifier.
//! - Bytes 3+: Variable-length payload (may be empty).
//!
//! # Submodules
//!
//! - [`frame`] --- Frame-level encode/decode, builders, and response parsing.
//! - [`dstar`] --- D-STAR radio header (41 bytes with CRC-CCITT) codec.
//! - [`slow_data`] --- D-STAR slow data decoder for extracting text messages
//!   from voice frame payloads.

pub mod dstar;
pub mod frame;
pub mod slow_data;

// Re-export key types for convenience.
pub use dstar::DStarHeader;
pub use frame::{
    MmdvmConfig, MmdvmError, MmdvmFrame, MmdvmResponse, ModemMode, ModemState, ModemStatus,
    NakReason,
};
pub use slow_data::SlowDataDecoder;
