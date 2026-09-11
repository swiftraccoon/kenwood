#![doc = include_str!("../README.md")]

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
pub use radio::backup::{McpBackupOutcome, McpBackupReport, McpBackupStage};
pub use radio::pm_name_trial::{
    PmNameTrialSessionError, PmNameTrialSessionOutcome, PmNameTrialSessionReport,
    PmNameTrialSessionStage, PmNameTrialWriteDisposition,
};
pub use radio::pm1_name_update::{
    Pm1NameUpdateSessionError, Pm1NameUpdateSessionOutcome, Pm1NameUpdateSessionReport,
    Pm1NameUpdateSessionStage, Pm1NameUpdateWriteDisposition,
};
pub use radio::qualification::{
    McpProbeExit, McpProbeOutcome, McpProbeReport, McpProbeSegment, McpProbeStage,
};
pub use radio::{Identity, Progress, Radio};
pub use types::{
    Address, Band, DvGatewayMode, FirmwareIdentity, IMAGE_LENGTH, OperatingMode, Page, RadioModel,
    RadioType, Region, SelectableMode, SlotIndex,
};
