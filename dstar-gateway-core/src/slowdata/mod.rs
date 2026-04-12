//! D-STAR slow data sub-codec.
//!
//! Slow data is the 3 bytes-per-voice-frame side channel that
//! carries text status messages, GPS NMEA passthrough, fast data,
//! and header retransmissions for late joiners. Per JARL spec.
//!
//! On the wire, slow data bytes are XOR-scrambled with a 3-byte
//! key (`0x70 0x4F 0x93`). The `scrambler` submodule handles
//! scramble/descramble; the [`scramble`] and [`descramble`] functions
//! are re-exported here. The `assembler` submodule accumulates
//! 3-byte fragments across consecutive frames into complete blocks
//! of type [`SlowDataBlockKind`]; use [`SlowDataAssembler`] to
//! drive it.
//!
//! Reference: `ircDDBGateway/Common/SlowDataEncoder.cpp`,
//! `ircDDBGateway/Common/DStarDefines.h:85-92, 111-113`.

mod assembler;
mod block;
mod error;
mod scrambler;

pub use assembler::SlowDataAssembler;
pub use block::{SlowDataBlock, SlowDataBlockKind, SlowDataText};
pub use error::SlowDataError;
pub use scrambler::{descramble, scramble};
