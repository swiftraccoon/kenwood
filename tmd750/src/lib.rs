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
pub use memory::{ChannelAccess, MemoryImage, PatchPlanner, PatchSet, TerminalGatewayRoute};
pub use radio::backup::{McpBackupOutcome, McpBackupReport, McpBackupStage};
pub use radio::menu::{
    MenuAssignment, MenuFieldSnapshot, MenuUpdateError, MenuUpdatePlan, ScopedMenuField,
};
pub use radio::my1_callsign_update::{
    My1CallsignUpdateSessionError, My1CallsignUpdateSessionOutcome, My1CallsignUpdateSessionReport,
    My1CallsignUpdateSessionStage, My1CallsignUpdateWriteDisposition,
};
pub use radio::pm_name_trial::{
    PmNameTrialSessionError, PmNameTrialSessionOutcome, PmNameTrialSessionReport,
    PmNameTrialSessionStage, PmNameTrialWriteDisposition,
};
pub use radio::pm1_name_update::{
    Pm1NameUpdateSessionError, Pm1NameUpdateSessionOutcome, Pm1NameUpdateSessionReport,
    Pm1NameUpdateSessionStage, Pm1NameUpdateWriteDisposition,
};
pub use radio::programming::{McpCompareExchangeReport, PageReplacement};
pub use radio::qualification::{
    McpGatewayOffProbeReport, McpProbeExit, McpProbeOutcome, McpProbeReport, McpProbeSegment,
    McpProbeStage,
};
pub use radio::readiness::{
    ControlHost, ControlStage, ObservationReport, ReadinessReport, SystemControlHost,
    observe_control, verify_readiness,
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
pub use radio::terminal_exit_trial::{
    TerminalExitTrialSessionError, TerminalExitTrialSessionOutcome, TerminalExitTrialSessionReport,
    TerminalExitTrialSessionStage, TerminalExitTrialWriteDisposition,
};
pub use radio::{Identity, Progress, Radio};
pub use types::{
    Address, Band, DvGatewayMode, FirmwareIdentity, IMAGE_LENGTH, OperatingMode, Page,
    PhysicalChannel, RadioModel, RadioType, Region, SelectableMode, SlotIndex, StoredChannel,
    StoredChannelEntry,
};
