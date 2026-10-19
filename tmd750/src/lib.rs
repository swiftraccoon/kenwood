#![doc = include_str!("../README.md")]

// The extractor and proptest are dev-dependencies of the integration tests;
// the library's own unit-test target sees them too and must name them.
#[cfg(test)]
use mcp_d75_extract as _;
#[cfg(test)]
use proptest as _;

pub mod error;
pub mod file;
pub mod memory;
pub mod nmea;
pub mod protocol;
pub mod radio;
pub mod transport;
pub mod types;

pub use error::{Error, FileError, McpError, ProtocolError, SchemaError, ValidationError};
pub use file::{FileLayout, RadioConfig, parse_d750};
pub use memory::{ChannelAccess, MemoryImage, PatchPlanner, PatchSet, TerminalGatewayRoute};
pub use radio::backup::{McpBackupOutcome, McpBackupReport, McpBackupStage};
pub use radio::menu::{
    MenuAssignment, MenuFieldSnapshot, MenuUpdateError, MenuUpdatePlan, ScopedMenuField,
};
pub use radio::probe::{
    McpGatewayOffProbeReport, McpProbeExit, McpProbeOutcome, McpProbeReport, McpProbeSegment,
    McpProbeStage,
};
pub use radio::programming::{McpCompareExchangeReport, PageReplacement};
pub use radio::readiness::{
    ControlHost, ControlStage, ObservationReport, ReadinessReport, SettleReport, SystemControlHost,
    await_continuous_enumeration, await_settled, observe_control, verify_readiness,
};
pub use radio::terminal::lifecycle::{
    LifecycleError, RestorationError, RestorationReport, RestorationState, TerminalLifecycle,
    TerminalRecovery, TerminalStartup,
};
pub use radio::terminal::session::{
    TerminalJournal, TerminalPhase, TerminalProgramError, TerminalProgramReport, TerminalRequest,
    program_terminal,
};
pub use radio::terminal::transition::{
    ModemHost, ModemOpenFailure, ProvenModem, TransitionError, TransitionReport, acquire_modem,
};
pub use radio::terminal::{TerminalPlan, TerminalPlanError, TerminalTarget};
pub use radio::text_field_update::{
    SessionGuards, TextFieldUpdateSessionError, TextFieldUpdateSessionOutcome,
    TextFieldUpdateSessionReport, TextFieldUpdateSessionStage, TextFieldUpdateWriteDisposition,
};
pub use radio::{Identity, Progress, Radio, WRITE_TARGETS, WriteTarget};
pub use types::{
    Address, Band, DvGatewayMode, FirmwareIdentity, IMAGE_LENGTH, OperatingMode, Page,
    PhysicalChannel, RadioModel, RadioType, Region, SelectableMode, SlotIndex, StoredChannel,
    StoredChannelEntry,
};
