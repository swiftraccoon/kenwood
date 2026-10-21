//! D-STAR Position Reporting System (DPRS).
//!
//! This module parses and emits `$$CRC` envelopes carrying uncompressed,
//! hundredth-minute APRS positions with plain position comments. Compressed
//! positions, timestamps, weather extensions and Mic-E have separate grammars.
//! Source, coordinate, symbol and comment values validate before construction.
//! `FromStr` verifies the CRC including the canonical CR terminator; explicit
//! [`ChecksumPolicy::Ignore`] is available for checksum diagnostics.
//!
//! ```
//! use dstar_gateway_core::dprs::{
//!     DprsComment, DprsPosition, DprsReport, DprsSource, DprsSymbol,
//!     Latitude, Longitude, SymbolCode, SymbolTable,
//! };
//!
//! # fn example() -> Result<(), Box<dyn std::error::Error>> {
//! let report = DprsReport::new(
//!     DprsSource::new("W1AW")?,
//!     DprsPosition { latitude: Latitude::new(35.5)?, longitude: Longitude::new(-82.55)? },
//!     DprsSymbol::new(SymbolTable::Primary, SymbolCode::new('>')?),
//!     DprsComment::new("Asheville test")?,
//! );
//! let sentence = report.to_sentence(); // Includes its CR terminator.
//! let received: DprsReport = sentence.parse()?;
//! assert_eq!(received, report);
//! assert_eq!(received.symbol().code().as_char(), '>');
//! # Ok(())
//! # }
//! ```
//!
//! The envelope/CRC follows
//! [`ircDDBGateway`'s collector](https://github.com/g4klx/ircDDBGateway/blob/f5ab9f7c93c6f28147b84a8f9667f6cc4c027eaa/Common/APRSCollector.cpp),
//! including variable-width source identifiers and a terminating CR. Coordinate
//! layout, plain comments and symbol selectors follow chapters 6, 8 and 20 of
//! [APRS 1.0.1](https://www.aprs.org/doc/APRS101.PDF).

pub use comment::{DprsComment, MAX_COMMENT_BYTES};
pub use coordinates::{Latitude, Longitude};
pub use crc::compute_crc;
pub use error::DprsError;
pub use parser::MAX_SENTENCE_BYTES;
pub use report::{ChecksumPolicy, DprsPosition, DprsReport};
pub use source::{DprsSource, DprsSourceError};
pub use symbol::{DprsSymbol, SymbolCode, SymbolOverlay, SymbolTable};

mod comment;
mod coordinates;
mod crc;
mod encoder;
mod error;
mod parser;
mod report;
mod source;
mod symbol;
