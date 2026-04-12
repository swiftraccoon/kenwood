//! D-STAR Position Reporting System (DPRS).
//!
//! DPRS is the APRS-equivalent for D-STAR. Position reports are
//! carried as a special slow-data text block whose payload starts
//! with `$$CRC` then APRS-style fields.
//!
//! Reference: `ircDDBGateway/Common/DPRSHandler.cpp:120-260` and
//! `ircDDBGateway/Common/APRSCollector.cpp:371-394` for the CRC
//! algorithm (CRC-CCITT with reflected polynomial `0x8408`,
//! initial value `0xFFFF`, final `~accumulator`).

mod coordinates;
mod crc;
mod encoder;
mod error;
mod parser;

pub use coordinates::{Latitude, Longitude};
pub use crc::compute_crc;
pub use encoder::encode_dprs;
pub use error::DprsError;
pub use parser::{DprsReport, parse_dprs};
