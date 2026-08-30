//! Kenwood TM-D750 memory programming: identity, MCP region transfers, and
//! manifest-driven menu fields per Programmable-Memory slot.
//!
//! The crate reuses the TH-D75 crate's transports through [`transport`] and
//! keeps every other layer TM-D750 shaped: address regions with 24-bit
//! headers, six Programmable-Memory slots as an address term, and a
//! 1,929,472-byte memory image.

// The extractor is a dev-dependency for the registry agreement test; the
// library's own unit-test target sees it too and must name it.
#[cfg(test)]
use mcp_d75_extract as _;

pub mod error;
pub mod file;
pub mod memory;
pub mod protocol;
pub mod radio;
pub mod transport;
pub mod types;

pub use error::{Error, FileError, McpError, ProtocolError, SchemaError, ValidationError};
pub use file::{FileLayout, RadioConfig, parse_d750};
pub use memory::{MemoryImage, PatchPlanner, PatchSet};
pub use radio::{Identity, Progress, Radio};
pub use types::{
    Address, FirmwareIdentity, IMAGE_LENGTH, MarketType, Page, RadioModel, Region, SlotIndex,
};
