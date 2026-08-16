//! Rust-owned qualified automation and stale-safe setting execution.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use ax25_codec::build_ax25;
use kenwood_thd75::error::Error as RadioError;
use kenwood_thd75::memory::{
    DecodedFieldValue, MCP_D75_MENU_FIELDS, MenuField, PatchSet, menu_field,
};
use kenwood_thd75::protocol::programming::PAGE_SIZE;
use kenwood_thd75::radio::Radio;
use kenwood_thd75::radio::automation::{
    AutomationAbi, AutomationSession, AutomationSnapshot, FrontPanelKey as RadioFrontPanelKey,
    GuardedKeyError, GuardedKeyOutcome,
};
use kenwood_thd75::radio::kiss_session::KissSession;
use kenwood_thd75::radio::menu::MenuFieldSnapshot;
use kenwood_thd75::radio::programming::{
    McpPage, McpPageExchange, McpPageExchangeError, McpPageExchangeOperationError, WritableMcpPage,
};
use kenwood_thd75::screen::{SCREEN_HEIGHT, SCREEN_WIDTH};
use kenwood_thd75::types::{
    KissDuplex, KissPersistence, KissSlotTime, KissTxDelay, KissTxTail, PacketDataRate,
};
use kiss_tnc::KissCommand;
use tokio::sync::{mpsc, oneshot};

use crate::aprs::{
    AprsActivityRecord, AprsActivityStore, AprsOperationalSnapshot, AprsSessionConfig,
    AprsSessionStatus,
};
use crate::catalog::{SettingChange, SettingValue, build_patch_plan, validate_changes};
use crate::if_dsp_radio::{
    EngageIfDspError, IF_CENTER_HZ, SavedIfDspRadioState, engage_if_dsp_radio,
    restore_if_dsp_radio, retune_if_dsp_radio,
};
use crate::transport::{ByteTransport, SwiftByteTransport};

/// Exact automation ABI record proved when a controller connects.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Record)]
pub struct AutomationAbiRecord {
    /// Firmware automation ABI version. V1.03.AZM reports `3`.
    pub version: u8,
    /// Firmware feature bitmap. V1.03.AZM reports `0x7F`.
    pub features: u8,
    /// Largest accepted front-panel key identifier.
    pub max_key: u8,
    /// Largest accepted input phase.
    pub max_phase: u8,
}

impl From<AutomationAbi> for AutomationAbiRecord {
    fn from(value: AutomationAbi) -> Self {
        Self {
            version: value.version,
            features: value.features,
            max_key: value.max_key,
            max_phase: value.max_phase,
        }
    }
}

/// One of the 25 exact front-panel dispatcher identifiers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum FrontPanelKey {
    /// Mode key.
    Mode,
    /// Menu key.
    Menu,
    /// A/B key.
    Ab,
    /// Function key.
    Function,
    /// Monitor key.
    Monitor,
    /// Direction pad up.
    Up,
    /// Direction pad down.
    Down,
    /// Direction pad left.
    Left,
    /// Direction pad right.
    Right,
    /// Enter key.
    Enter,
    /// Mark or keypad 0.
    Mark0,
    /// VFO or keypad 1.
    Vfo1,
    /// Memory recall or keypad 2.
    Mr2,
    /// Call or keypad 3.
    Call3,
    /// Message or keypad 4.
    Msg4,
    /// List or keypad 5.
    List5,
    /// Beacon or keypad 6.
    Beacon6,
    /// Reverse or keypad 7.
    Reverse7,
    /// Tone or keypad 8.
    Tone8,
    /// Front PF1 or keypad 9.
    Pf1_9,
    /// MHz or keypad star.
    MhzStar,
    /// Front PF2 or keypad hash.
    Pf2Hash,
    /// Microphone PF1.
    MicPf1,
    /// Microphone PF2.
    MicPf2,
    /// Microphone PF3.
    MicPf3,
}

impl From<FrontPanelKey> for RadioFrontPanelKey {
    fn from(value: FrontPanelKey) -> Self {
        match value {
            FrontPanelKey::Mode => Self::Mode,
            FrontPanelKey::Menu => Self::Menu,
            FrontPanelKey::Ab => Self::Ab,
            FrontPanelKey::Function => Self::Function,
            FrontPanelKey::Monitor => Self::Monitor,
            FrontPanelKey::Up => Self::Up,
            FrontPanelKey::Down => Self::Down,
            FrontPanelKey::Left => Self::Left,
            FrontPanelKey::Right => Self::Right,
            FrontPanelKey::Enter => Self::Enter,
            FrontPanelKey::Mark0 => Self::Mark0,
            FrontPanelKey::Vfo1 => Self::Vfo1,
            FrontPanelKey::Mr2 => Self::Mr2,
            FrontPanelKey::Call3 => Self::Call3,
            FrontPanelKey::Msg4 => Self::Msg4,
            FrontPanelKey::List5 => Self::List5,
            FrontPanelKey::Beacon6 => Self::Beacon6,
            FrontPanelKey::Reverse7 => Self::Reverse7,
            FrontPanelKey::Tone8 => Self::Tone8,
            FrontPanelKey::Pf1_9 => Self::Pf1_9,
            FrontPanelKey::MhzStar => Self::MhzStar,
            FrontPanelKey::Pf2Hash => Self::Pf2Hash,
            FrontPanelKey::MicPf1 => Self::MicPf1,
            FrontPanelKey::MicPf2 => Self::MicPf2,
            FrontPanelKey::MicPf3 => Self::MicPf3,
        }
    }
}

/// Stable authenticated LCD frame ready for display by Swift.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct RemoteScreenFrame {
    /// One-use host lease. A guarded tap must return this exact value.
    pub lease_id: u64,
    /// Screen width in pixels. Always 240.
    pub width: u32,
    /// Screen height in pixels. Always 180.
    pub height: u32,
    /// Bytes per row in `rgba8888`. Always 960.
    pub row_bytes: u32,
    /// Canonical top-down RGB565 little-endian pixels from the firmware.
    pub rgb565_le: Vec<u8>,
    /// Top-down RGBA8888 pixels for direct app rendering.
    pub rgba8888: Vec<u8>,
    /// Firmware framebuffer generation.
    pub generation: u32,
    /// Host-verified reflected IEEE CRC-32 of `rgb565_le`.
    pub crc32: u32,
    /// Exact cumulative automation command count.
    pub command_count: u32,
    /// Exact even firmware seqlock value.
    pub seqlock: u32,
}

/// Semantic outcome of one guarded tap.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum GuardedTapDisposition {
    /// The framebuffer matched and the press/release pair completed.
    Dispatched,
    /// The framebuffer changed, so firmware dispatched no input.
    ContextChanged,
    /// The complete press/release pair was authenticated after the host deadline.
    DispatchedAfterDeadline,
}

impl GuardedTapDisposition {
    const fn label(self) -> &'static str {
        match self {
            Self::Dispatched => "dispatched",
            Self::ContextChanged => "context changed; no input dispatched",
            Self::DispatchedAfterDeadline => "dispatched and released after deadline",
        }
    }
}

/// Guarded input outcome plus the required post-input screen capture.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct GuardedTapResult {
    /// Authenticated input disposition.
    pub disposition: GuardedTapDisposition,
    /// Fresh screen captured after the disposition was authenticated.
    pub screen: RemoteScreenFrame,
}

/// One typed value resolved from a live setting snapshot.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct SettingValueRecord {
    /// Stable catalog identifier.
    pub setting_id: String,
    /// Decoded live value.
    pub value: SettingValue,
}

/// Values read together during one sparse MCP session.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct SettingReadResult {
    /// Opaque optimistic-concurrency snapshot identifier.
    pub snapshot_id: u64,
    /// Typed values keyed by `setting_id`.
    pub values: Vec<SettingValueRecord>,
}

/// Result for one accepted change after verified page execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum SettingChangeOutcome {
    /// The approved value differed and its containing page was verified.
    Applied,
    /// The approved value already matched the reviewed value.
    AlreadyCurrent,
}

/// Per-change result from a completed setting batch.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct SettingChangeResult {
    /// Stable catalog identifier.
    pub setting_id: String,
    /// Whether execution changed the approved value.
    pub outcome: SettingChangeOutcome,
    /// Verified final typed value.
    pub value: SettingValue,
}

/// Completed stale-safe setting batch.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct SettingApplyReport {
    /// Reviewed snapshot consumed by this batch.
    pub previous_snapshot_id: u64,
    /// MCP pages actually written and read-back verified.
    pub pages_written: Vec<u16>,
    /// Result for every approved change in caller order.
    pub changes: Vec<SettingChangeResult>,
    /// Refreshed values and concurrency token after the verified batch.
    pub refreshed_values: SettingReadResult,
}

/// Radio-side ownership state for a live USB IF-DSP session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum IfDspRadioPhase {
    /// No radio state is reserved for IF capture.
    Inactive,
    /// Band B is in verified Single Band / USB / open-squelch / IF-output mode.
    Active,
    /// The original snapshot is still owned, but IF output is not verified.
    /// Only restoration is safe in this state.
    NeedsRestoration,
}

/// Verified radio facts returned at IF-DSP lifecycle boundaries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Record)]
pub struct IfDspRadioStatus {
    /// Whether the controller currently owns a saved IF session.
    pub phase: IfDspRadioPhase,
    /// Current Band-B center frequency while active.
    pub band_b_frequency_hz: Option<u32>,
    /// Fixed center of the physical real low-IF stream.
    pub if_center_hz: u32,
}

impl IfDspRadioStatus {
    const fn inactive() -> Self {
        Self {
            phase: IfDspRadioPhase::Inactive,
            band_b_frequency_hz: None,
            if_center_hz: IF_CENTER_HZ,
        }
    }

    const fn active(frequency_hz: u32) -> Self {
        Self {
            phase: IfDspRadioPhase::Active,
            band_b_frequency_hz: Some(frequency_hz),
            if_center_hz: IF_CENTER_HZ,
        }
    }

    const fn needs_restoration() -> Self {
        Self {
            phase: IfDspRadioPhase::NeedsRestoration,
            band_b_frequency_hz: None,
            if_center_hz: IF_CENTER_HZ,
        }
    }
}

/// Clear errors surfaced across the Swift boundary.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error, uniffi::Error)]
pub enum AutomationError {
    /// USB transport setup or I/O failed.
    #[error("USB transport failed: {detail}")]
    UsbTransport {
        /// Underlying transport detail.
        detail: String,
    },
    /// Exact automation identity, hook, runtime, ABI, or aperture proof failed.
    #[error("automation qualification failed: {detail}")]
    AutomationQualification {
        /// Qualification detail.
        detail: String,
    },
    /// A setting operation completed, but automation control could not be restored.
    #[error("{operation} completed, but automation restoration failed: {detail}")]
    AutomationRestoration {
        /// Operation whose result could not be returned as a healthy controller.
        operation: String,
        /// Restoration detail.
        detail: String,
    },
    /// The controller task is no longer running.
    #[error("radio controller is closed")]
    ControllerClosed,
    /// An internal response channel ended unexpectedly.
    #[error("internal controller response failed: {detail}")]
    Internal {
        /// Internal diagnostic detail.
        detail: String,
    },
    /// Stable authenticated screen capture failed.
    #[error("screen capture failed: {detail}")]
    ScreenCapture {
        /// Capture detail.
        detail: String,
    },
    /// No unused authenticated screen is available for guarded input.
    #[error("guarded input requires a fresh unused screen")]
    ScreenLeaseUnavailable,
    /// The UI attempted input from an older screen than the controller holds.
    #[error("stale screen lease {received}; current lease is {expected}")]
    ScreenLeaseStale {
        /// Current one-use lease.
        expected: u64,
        /// Lease supplied by Swift.
        received: u64,
    },
    /// Firmware-guarded input failed before a complete semantic result.
    #[error("guarded input failed: {detail}")]
    GuardedInput {
        /// Guarded-input detail.
        detail: String,
    },
    /// Input disposition was known, but its mandatory recapture failed.
    #[error("post-tap screen capture failed after {disposition}: {detail}")]
    PostTapCapture {
        /// Authenticated input disposition.
        disposition: String,
        /// Capture detail.
        detail: String,
    },
    /// Live setting resolution failed.
    #[error("setting read failed: {detail}")]
    SettingsRead {
        /// Read or decoding detail.
        detail: String,
    },
    /// A proposed setting batch failed complete preflight validation.
    #[error("invalid setting plan: {detail}")]
    InvalidSettingsPlan {
        /// Plan rejection detail.
        detail: String,
    },
    /// The reviewed page snapshot is absent, already consumed, or superseded.
    #[error("setting snapshot {snapshot_id} is unavailable; read the values again")]
    SettingsSnapshotUnavailable {
        /// Requested snapshot identifier.
        snapshot_id: u64,
    },
    /// A typed expected value did not match the reviewed snapshot.
    #[error("setting precondition failed for {setting_id}: expected {expected}, reviewed {actual}")]
    SettingPreconditionFailed {
        /// Setting whose typed precondition failed.
        setting_id: String,
        /// Caller-supplied expected value.
        expected: String,
        /// Value decoded from the retained review snapshot.
        actual: String,
    },
    /// Live bytes changed after review; the compare phase performed zero writes.
    #[error("approved setting snapshot is stale; no settings were written: {detail}")]
    SettingsSnapshotStale {
        /// First byte mismatch reported by the radio layer.
        detail: String,
    },
    /// Setting execution failed; the detail states whether writes may have started.
    #[error("setting apply failed: {detail}")]
    SettingsApply {
        /// Apply, verification, or cleanup detail.
        detail: String,
    },
    /// Closing the radio transport failed.
    #[error("radio shutdown failed: {detail}")]
    Shutdown {
        /// Shutdown detail.
        detail: String,
    },
    /// A live KISS session prevents CAT automation operations on the same serial link.
    #[error(
        "APRS KISS mode is active; stop APRS monitoring before using screen, settings, or front-panel control"
    )]
    AprsModeActive,
    /// An APRS operation requires an active KISS session.
    #[error("APRS KISS mode is not active")]
    AprsModeInactive,
    /// APRS session configuration failed validation before any mode change.
    #[error("invalid APRS configuration: {detail}")]
    InvalidAprsConfiguration {
        /// Validation detail.
        detail: String,
    },
    /// An APRS session or explicit packet operation failed.
    #[error("APRS operation failed: {detail}")]
    AprsOperation {
        /// Operation detail.
        detail: String,
    },
    /// IF-DSP owns a saved radio state and blocks conflicting CAT operations.
    #[error(
        "IF-DSP mode is active; stop IF-DSP before using APRS, screen, settings, or front-panel control"
    )]
    IfDspModeActive,
    /// An IF-DSP operation requires a prepared radio session.
    #[error("IF-DSP radio mode is not active")]
    IfDspModeInactive,
    /// IF-DSP radio setup, verification, or retuning failed.
    #[error("IF-DSP radio operation failed: {detail}")]
    IfDspOperation {
        /// Exact failed step and radio detail.
        detail: String,
    },
    /// Best-effort IF-DSP restoration did not reproduce every saved value.
    #[error("IF-DSP radio restoration was incomplete: {detail}")]
    IfDspRestoration {
        /// Saved fields that failed write or readback verification.
        detail: String,
    },
}

/// Rust-owned qualified automation controller.
///
/// All commands are serialized by one actor that owns `Radio` and its
/// borrowing `AutomationSession`. Swift cannot bypass qualification, reuse a
/// screen lease, or interleave MCP writes with guarded input.
#[derive(uniffi::Object)]
pub struct AutomationController {
    commands: mpsc::Sender<ControllerCommand>,
    abi: AutomationAbiRecord,
    aprs: Arc<Mutex<AprsActivityStore>>,
}

impl std::fmt::Debug for AutomationController {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let aprs_status = lock_aprs_store(&self.aprs).status();
        formatter
            .debug_struct("AutomationController")
            .field("abi", &self.abi)
            .field("aprs", &aprs_status)
            .finish_non_exhaustive()
    }
}

impl Drop for AutomationController {
    fn drop(&mut self) {
        drop(
            self.commands
                .try_send(ControllerCommand::Shutdown { reply: None }),
        );
    }
}

#[uniffi::export(async_runtime = "tokio")]
impl AutomationController {
    /// Return the exact ABI proved at connection time.
    #[must_use]
    pub fn abi(self: Arc<Self>) -> AutomationAbiRecord {
        self.abi
    }

    /// Return current APRS status, incremental activity rows, and heard stations.
    ///
    /// Pass the last observed sequence to receive only newer rows. A `None`
    /// sequence returns the complete retained journal. This is synchronous
    /// because the journal is updated by the controller actor independently of
    /// Swift polling.
    #[must_use]
    pub fn aprs_snapshot(self: Arc<Self>, after_sequence: Option<u64>) -> AprsOperationalSnapshot {
        lock_aprs_store(&self.aprs).snapshot(after_sequence)
    }

    /// Leave qualified automation CAT control and begin continuously draining KISS packets.
    ///
    /// Screen capture, guarded taps, and persistent setting operations are
    /// unavailable until [`Self::stop_aprs`] completes. Starting is RX-only
    /// when `station_callsign` is blank.
    ///
    /// # Errors
    ///
    /// Returns a configuration, radio mode, or transport error. This method
    /// does not transmit an RF data packet.
    pub async fn start_aprs(
        self: Arc<Self>,
        config: AprsSessionConfig,
    ) -> Result<AprsSessionStatus, AutomationError> {
        drop(config.validate()?);
        let (reply, response) = oneshot::channel();
        self.commands
            .send(ControllerCommand::StartAprs { config, reply })
            .await
            .map_err(|_| AutomationError::ControllerClosed)?;
        response.await.map_err(response_lost)?
    }

    /// Send KISS Return and restore qualified automation CAT control.
    ///
    /// This future resolves successfully only after automation has been requalified;
    /// callers never need to reconnect merely to leave APRS mode.
    ///
    /// # Errors
    ///
    /// Returns an APRS-mode or automation-restoration error.
    pub async fn stop_aprs(self: Arc<Self>) -> Result<AprsSessionStatus, AutomationError> {
        let (reply, response) = oneshot::channel();
        self.commands
            .send(ControllerCommand::StopAprs { reply })
            .await
            .map_err(|_| AutomationError::ControllerClosed)?;
        response.await.map_err(response_lost)?
    }

    /// Send one unacknowledged APRS message packet and journal its exact bytes.
    ///
    /// This is a one-shot packet operation: it does not retry or correlate an
    /// APRS acknowledgement. The UI must describe that distinction and obtain
    /// deliberate RF-transmit confirmation.
    ///
    /// # Errors
    ///
    /// Returns an error when KISS is inactive, source identity is absent, the
    /// message is invalid, or the transport write fails.
    pub async fn send_aprs_message(
        self: Arc<Self>,
        addressee: String,
        text: String,
        message_id: Option<String>,
    ) -> Result<AprsActivityRecord, AutomationError> {
        let (reply, response) = oneshot::channel();
        self.commands
            .send(ControllerCommand::SendAprsMessage {
                addressee,
                text,
                message_id,
                reply,
            })
            .await
            .map_err(|_| AutomationError::ControllerClosed)?;
        response.await.map_err(response_lost)?
    }

    /// Send one APRS position packet and journal its exact bytes.
    ///
    /// The UI must obtain deliberate RF-transmit confirmation. No periodic or
    /// `SmartBeaconing` timer is enabled by this operation.
    ///
    /// # Errors
    ///
    /// Returns an error when KISS is inactive, source identity is absent, the
    /// coordinates are invalid, or the transport write fails.
    pub async fn send_aprs_position(
        self: Arc<Self>,
        latitude: f64,
        longitude: f64,
        comment: String,
    ) -> Result<AprsActivityRecord, AutomationError> {
        let (reply, response) = oneshot::channel();
        self.commands
            .send(ControllerCommand::SendAprsPosition {
                latitude,
                longitude,
                comment,
                reply,
            })
            .await
            .map_err(|_| AutomationError::ControllerClosed)?;
        response.await.map_err(response_lost)?
    }

    /// Save every affected radio value, configure Band B for USB IF output,
    /// verify all readbacks, and restore qualified automation control.
    ///
    /// Audio capture must not start until this future succeeds. APRS and all
    /// screen/settings/front-panel operations are rejected while the returned
    /// session remains active.
    ///
    /// # Errors
    ///
    /// Returns an IF setup/restoration error, or an explicit APRS/IF ownership
    /// conflict. A partial setup is restored immediately before an error is
    /// returned.
    pub async fn prepare_if_dsp(self: Arc<Self>) -> Result<IfDspRadioStatus, AutomationError> {
        let (reply, response) = oneshot::channel();
        self.commands
            .send(ControllerCommand::PrepareIfDsp { reply })
            .await
            .map_err(|_| AutomationError::ControllerClosed)?;
        response.await.map_err(response_lost)?
    }

    /// Return the actor's current IF-DSP reservation without changing the radio.
    ///
    /// This lets a caller reconcile ownership after cancellation or a failed
    /// setup response. `NeedsRestoration` means the saved snapshot is retained
    /// and conflicting radio operations remain blocked.
    ///
    /// # Errors
    ///
    /// Returns [`AutomationError::ControllerClosed`] when the owning actor has
    /// ended, or [`AutomationError::Internal`] if its response channel closes.
    pub async fn if_dsp_status(self: Arc<Self>) -> Result<IfDspRadioStatus, AutomationError> {
        let (reply, response) = oneshot::channel();
        self.commands
            .send(ControllerCommand::IfDspStatus { reply })
            .await
            .map_err(|_| AutomationError::ControllerClosed)?;
        response.await.map_err(response_lost)
    }

    /// Attempt to retune Band B using the hardware-required AF → tune → IF sequence.
    ///
    /// The original pre-session frequency remains in the saved restoration
    /// snapshot. The current `kenwood-thd75` direct-frequency writer fails
    /// closed before I/O, so this operation currently resumes IF, attempts the
    /// pre-session restore, and returns an error. If a writer is qualified
    /// later, success must still mean that the new frequency and IF-output
    /// re-engagement were both readback verified. Any retune failure ends the
    /// IF session; an incomplete restore is retained as `NeedsRestoration`.
    ///
    /// # Errors
    ///
    /// Returns an inactive-session, radio tuning, IF re-engagement, or exact
    /// automation-restoration error.
    pub async fn retune_if_dsp(
        self: Arc<Self>,
        frequency_hz: u32,
    ) -> Result<IfDspRadioStatus, AutomationError> {
        let (reply, response) = oneshot::channel();
        self.commands
            .send(ControllerCommand::RetuneIfDsp {
                frequency_hz,
                reply,
            })
            .await
            .map_err(|_| AutomationError::ControllerClosed)?;
        response.await.map_err(response_lost)?
    }

    /// Stop owning IF mode and attempt to restore every saved radio value.
    ///
    /// A failed field remains represented as `NeedsRestoration` so callers can
    /// retry restoration without mistaking the physical IF output for active.
    /// The current direct-frequency quarantine means the frequency step cannot
    /// succeed even when it was never changed. Success is returned only after
    /// every step passes readback and automation control is requalified.
    ///
    /// # Errors
    ///
    /// Returns an inactive-session, partial-restoration, or automation
    /// restoration error.
    pub async fn restore_if_dsp(self: Arc<Self>) -> Result<IfDspRadioStatus, AutomationError> {
        let (reply, response) = oneshot::channel();
        self.commands
            .send(ControllerCommand::RestoreIfDsp { reply })
            .await
            .map_err(|_| AutomationError::ControllerClosed)?;
        response.await.map_err(response_lost)?
    }

    /// Capture a stable authenticated 240x180 LCD frame.
    ///
    /// A successful call invalidates any older screen lease, including one
    /// whose frame bytes remain visible in the app.
    ///
    /// # Errors
    ///
    /// Returns [`AutomationError::ScreenCapture`] for firmware instability or
    /// strict-protocol failure, or [`AutomationError::ControllerClosed`] when
    /// the owning actor ended.
    pub async fn capture_screen(self: Arc<Self>) -> Result<RemoteScreenFrame, AutomationError> {
        let (reply, response) = oneshot::channel();
        self.commands
            .send(ControllerCommand::Capture { reply })
            .await
            .map_err(|_| AutomationError::ControllerClosed)?;
        response.await.map_err(response_lost)?
    }

    /// Consume one screen lease for one firmware-guarded key tap.
    ///
    /// The returned result always includes a post-tap recapture. That new
    /// screen has its own one-use lease for a subsequent action.
    ///
    /// # Errors
    ///
    /// Stale or reused leases are rejected before I/O. Transport or protocol
    /// errors poison the exact automation session and close the controller.
    pub async fn guarded_tap(
        self: Arc<Self>,
        lease_id: u64,
        key: FrontPanelKey,
    ) -> Result<GuardedTapResult, AutomationError> {
        let (reply, response) = oneshot::channel();
        self.commands
            .send(ControllerCommand::GuardedTap {
                lease_id,
                key,
                reply,
            })
            .await
            .map_err(|_| AutomationError::ControllerClosed)?;
        response.await.map_err(response_lost)?
    }

    /// Resolve authoritative live setting values in one sparse MCP session.
    ///
    /// `None` reads all non-blob settings. `Some(ids)` reads only those
    /// fields, including a blob when explicitly requested. The returned
    /// snapshot token and typed values must be copied into any later
    /// [`SettingChange`].
    ///
    /// # Errors
    ///
    /// Returns an error for unknown identifiers, a non-V1.03 target, MCP I/O,
    /// decoding failure, or failed automation restoration.
    pub async fn read_setting_values(
        self: Arc<Self>,
        setting_ids: Option<Vec<String>>,
    ) -> Result<SettingReadResult, AutomationError> {
        let (reply, response) = oneshot::channel();
        self.commands
            .send(ControllerCommand::ReadSettings { setting_ids, reply })
            .await
            .map_err(|_| AutomationError::ControllerClosed)?;
        response.await.map_err(response_lost)?
    }

    /// Apply one fully approved setting batch automatically.
    ///
    /// Rust reruns complete schema validation, verifies every typed expected
    /// value against its retained snapshot, then enters one MCP session. The
    /// radio layer reads all affected live pages and compares every byte with
    /// the reviewed pages before starting any write. A mismatch returns
    /// [`AutomationError::SettingsSnapshotStale`] with zero writes. Matching
    /// pages are changed and immediately read-back verified.
    ///
    /// # Errors
    ///
    /// Returns a typed preflight, precondition, stale snapshot, MCP apply, or
    /// automation-restoration error. Multi-page radio writes are not physically
    /// atomic; an I/O failure can occur after an earlier page was verified,
    /// and [`AutomationError::SettingsApply`] reports that condition.
    pub async fn apply_setting_changes(
        self: Arc<Self>,
        changes: Vec<SettingChange>,
    ) -> Result<SettingApplyReport, AutomationError> {
        let (reply, response) = oneshot::channel();
        self.commands
            .send(ControllerCommand::ApplySettings { changes, reply })
            .await
            .map_err(|_| AutomationError::ControllerClosed)?;
        response.await.map_err(response_lost)?
    }

    /// Close the USB connection and stop the controller actor.
    ///
    /// # Errors
    ///
    /// Returns [`AutomationError::Shutdown`] if the foreign transport cannot
    /// close, or [`AutomationError::ControllerClosed`] if it already ended.
    pub async fn close(self: Arc<Self>) -> Result<(), AutomationError> {
        let (reply, response) = oneshot::channel();
        self.commands
            .send(ControllerCommand::Shutdown { reply: Some(reply) })
            .await
            .map_err(|_| AutomationError::ControllerClosed)?;
        response.await.map_err(response_lost)?
    }
}

/// Connect through Swift's USB byte stream and prove the automation extension.
///
/// Qualification attests the exact TH-D75 model, CAT firmware identity
/// `1.03.AZM`, patched hooks, complete V1.03.AZM runtime, ABI, aperture bounds,
/// and stable metadata. It then runs the missing-snapshot refusal canary before
/// returning the controller.
///
/// # Errors
///
/// Returns [`AutomationError::AutomationQualification`] or
/// [`AutomationError::UsbTransport`] without a controller if any proof or I/O
/// step fails.
#[uniffi::export(async_runtime = "tokio")]
pub async fn connect_automation(
    transport: Arc<dyn ByteTransport>,
) -> Result<Arc<AutomationController>, AutomationError> {
    let (commands, receiver) = mpsc::channel(8);
    let (ready, readiness) = oneshot::channel();
    let aprs = Arc::new(Mutex::new(AprsActivityStore::default()));
    let adapter = SwiftByteTransport::new(transport);
    std::mem::drop(tokio::spawn(run_controller(
        adapter,
        receiver,
        ready,
        aprs.clone(),
    )));

    let abi = readiness.await.map_err(response_lost)??;
    Ok(Arc::new(AutomationController {
        commands,
        abi,
        aprs,
    }))
}

#[derive(Debug)]
enum ControllerCommand {
    Capture {
        reply: oneshot::Sender<Result<RemoteScreenFrame, AutomationError>>,
    },
    GuardedTap {
        lease_id: u64,
        key: FrontPanelKey,
        reply: oneshot::Sender<Result<GuardedTapResult, AutomationError>>,
    },
    ReadSettings {
        setting_ids: Option<Vec<String>>,
        reply: oneshot::Sender<Result<SettingReadResult, AutomationError>>,
    },
    ApplySettings {
        changes: Vec<SettingChange>,
        reply: oneshot::Sender<Result<SettingApplyReport, AutomationError>>,
    },
    StartAprs {
        config: AprsSessionConfig,
        reply: oneshot::Sender<Result<AprsSessionStatus, AutomationError>>,
    },
    StopAprs {
        reply: oneshot::Sender<Result<AprsSessionStatus, AutomationError>>,
    },
    SendAprsMessage {
        addressee: String,
        text: String,
        message_id: Option<String>,
        reply: oneshot::Sender<Result<AprsActivityRecord, AutomationError>>,
    },
    SendAprsPosition {
        latitude: f64,
        longitude: f64,
        comment: String,
        reply: oneshot::Sender<Result<AprsActivityRecord, AutomationError>>,
    },
    PrepareIfDsp {
        reply: oneshot::Sender<Result<IfDspRadioStatus, AutomationError>>,
    },
    IfDspStatus {
        reply: oneshot::Sender<IfDspRadioStatus>,
    },
    RetuneIfDsp {
        frequency_hz: u32,
        reply: oneshot::Sender<Result<IfDspRadioStatus, AutomationError>>,
    },
    RestoreIfDsp {
        reply: oneshot::Sender<Result<IfDspRadioStatus, AutomationError>>,
    },
    Shutdown {
        reply: Option<oneshot::Sender<Result<(), AutomationError>>>,
    },
}

#[derive(Debug)]
enum ActorBreak {
    ReadSettings {
        setting_ids: Option<Vec<String>>,
        reply: oneshot::Sender<Result<SettingReadResult, AutomationError>>,
    },
    ApplySettings {
        changes: Vec<SettingChange>,
        reply: oneshot::Sender<Result<SettingApplyReport, AutomationError>>,
    },
    StartAprs {
        config: AprsSessionConfig,
        reply: oneshot::Sender<Result<AprsSessionStatus, AutomationError>>,
    },
    PrepareIfDsp {
        reply: oneshot::Sender<Result<IfDspRadioStatus, AutomationError>>,
    },
    RetuneIfDsp {
        frequency_hz: u32,
        reply: oneshot::Sender<Result<IfDspRadioStatus, AutomationError>>,
    },
    RestoreIfDsp {
        reply: oneshot::Sender<Result<IfDspRadioStatus, AutomationError>>,
    },
    Shutdown {
        reply: Option<oneshot::Sender<Result<(), AutomationError>>>,
    },
    Fatal,
    SenderClosed,
}

#[derive(Debug)]
enum DeferredReply {
    Read {
        reply: oneshot::Sender<Result<SettingReadResult, AutomationError>>,
        result: SettingReadResult,
    },
    Apply {
        reply: oneshot::Sender<Result<SettingApplyReport, AutomationError>>,
        result: SettingApplyReport,
    },
    StopAprs {
        reply: oneshot::Sender<Result<AprsSessionStatus, AutomationError>>,
    },
    FailedAprsStart {
        reply: oneshot::Sender<Result<AprsSessionStatus, AutomationError>>,
        detail: String,
    },
    IfDsp {
        operation: &'static str,
        rollback_if_undelivered: bool,
        reply: oneshot::Sender<Result<IfDspRadioStatus, AutomationError>>,
        result: Result<IfDspRadioStatus, AutomationError>,
    },
}

impl DeferredReply {
    /// Complete a deferred response. `true` means an unobserved IF prepare
    /// still owns radio state and must be rolled back by the actor.
    fn complete(self, aprs: &Mutex<AprsActivityStore>) -> bool {
        match self {
            Self::Read { reply, result } => {
                drop(reply.send(Ok(result)));
                false
            }
            Self::Apply { reply, result } => {
                drop(reply.send(Ok(result)));
                false
            }
            Self::StopAprs { reply } => {
                let status = {
                    let mut store = lock_aprs_store(aprs);
                    store.mark_inactive();
                    store.status()
                };
                drop(reply.send(Ok(status)));
                false
            }
            Self::FailedAprsStart { reply, detail } => {
                lock_aprs_store(aprs).mark_start_failed_after_restoration(&detail);
                drop(reply.send(Err(AutomationError::AprsOperation { detail })));
                false
            }
            Self::IfDsp {
                rollback_if_undelivered,
                reply,
                result,
                ..
            } => {
                let undelivered = reply.send(result).is_err();
                rollback_if_undelivered && undelivered
            }
        }
    }

    fn fail(self, detail: String, aprs: &Mutex<AprsActivityStore>) {
        match self {
            Self::Read { reply, .. } => {
                drop(reply.send(Err(AutomationError::AutomationRestoration {
                    operation: "setting read".to_owned(),
                    detail,
                })));
            }
            Self::Apply { reply, .. } => {
                drop(reply.send(Err(AutomationError::AutomationRestoration {
                    operation: "setting apply".to_owned(),
                    detail,
                })));
            }
            Self::StopAprs { reply } => {
                lock_aprs_store(aprs).mark_failed(format!(
                    "KISS ended, but automation restoration failed: {detail}"
                ));
                drop(reply.send(Err(AutomationError::AutomationRestoration {
                    operation: "APRS stop".to_owned(),
                    detail,
                })));
            }
            Self::FailedAprsStart {
                reply,
                detail: start_detail,
            } => {
                let combined = format!(
                    "{start_detail}; automation cleanup qualification also failed: {detail}"
                );
                lock_aprs_store(aprs).mark_failed(combined.clone());
                drop(reply.send(Err(AutomationError::AutomationRestoration {
                    operation: "APRS start cleanup".to_owned(),
                    detail: combined,
                })));
            }
            Self::IfDsp {
                operation, reply, ..
            } => drop(reply.send(Err(AutomationError::AutomationRestoration {
                operation: operation.to_owned(),
                detail,
            }))),
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct ActiveIfDspSession {
    saved: SavedIfDspRadioState,
    current_frequency_hz: u32,
    output_verified: bool,
}

impl ActiveIfDspSession {
    const fn status(self) -> IfDspRadioStatus {
        if self.output_verified {
            IfDspRadioStatus::active(self.current_frequency_hz)
        } else {
            IfDspRadioStatus::needs_restoration()
        }
    }
}

#[derive(Debug, Clone)]
struct SettingsSnapshot {
    id: u64,
    pages: BTreeMap<u16, [u8; PAGE_SIZE]>,
    field_ids: Vec<String>,
}

#[derive(Debug)]
struct PendingScreen {
    lease_id: u64,
    snapshot: AutomationSnapshot,
}

#[expect(
    clippy::too_many_lines,
    reason = "the single owner actor keeps CAT, KISS, and IF session transitions visibly exhaustive"
)]
async fn run_controller(
    transport: SwiftByteTransport,
    mut receiver: mpsc::Receiver<ControllerCommand>,
    ready: oneshot::Sender<Result<AutomationAbiRecord, AutomationError>>,
    aprs: Arc<Mutex<AprsActivityStore>>,
) {
    let mut radio = Radio::new(transport);
    let mut initial_ready = Some(ready);
    let mut deferred_reply: Option<DeferredReply> = None;
    let mut settings_snapshot: Option<SettingsSnapshot> = None;
    let mut if_dsp_session: Option<ActiveIfDspSession> = None;
    let mut restore_cat_after_kiss_return = false;
    let mut next_screen_lease = 1_u64;
    let mut next_settings_snapshot = 1_u64;

    loop {
        let cat_synchronization = if restore_cat_after_kiss_return {
            restore_cat_after_kiss_return = false;
            radio
                .restore_cat_after_mode_exit()
                .await
                .map_err(|error| error.to_string())
        } else {
            Ok(())
        };
        let qualification = match cat_synchronization {
            Ok(()) => qualify_automation(&mut radio).await,
            Err(detail) => Err(detail),
        };
        let mut session = match qualification {
            Ok(session) => session,
            Err(detail) => {
                let mut failure_detail = detail;
                if let Some(active) = if_dsp_session.take() {
                    let report = restore_if_dsp_radio(&mut radio, active.saved).await;
                    if !report.is_exact() {
                        failure_detail = format!(
                            "{failure_detail}; emergency IF-DSP restoration also failed: {}",
                            report.summary()
                        );
                    }
                }
                if let Some(reply) = initial_ready.take() {
                    drop(reply.send(Err(AutomationError::AutomationQualification {
                        detail: failure_detail.clone(),
                    })));
                }
                if let Some(reply) = deferred_reply.take() {
                    reply.fail(failure_detail, &aprs);
                }
                drop(radio.disconnect().await);
                return;
            }
        };

        if let Some(reply) = initial_ready.take() {
            drop(reply.send(Ok(session.abi().into())));
        }
        if let Some(reply) = deferred_reply.take() {
            let rollback_if_dsp = reply.complete(&aprs);
            if rollback_if_dsp {
                if let Some(active) = if_dsp_session.take() {
                    let report = restore_if_dsp_radio(&mut radio, active.saved).await;
                    if !report.is_exact() {
                        if_dsp_session = Some(ActiveIfDspSession {
                            output_verified: false,
                            ..active
                        });
                    }
                }
                continue;
            }
        }

        let reason = automation_loop(
            &mut session,
            &mut receiver,
            &mut next_screen_lease,
            if_dsp_session.map_or_else(IfDspRadioStatus::inactive, ActiveIfDspSession::status),
        )
        .await;

        match reason {
            ActorBreak::ReadSettings { setting_ids, reply } => {
                match read_settings(
                    &mut radio,
                    setting_ids.as_deref(),
                    &mut next_settings_snapshot,
                )
                .await
                {
                    Ok((result, snapshot)) => {
                        settings_snapshot = Some(snapshot);
                        deferred_reply = Some(DeferredReply::Read { reply, result });
                    }
                    Err(error) => drop(reply.send(Err(error))),
                }
            }
            ActorBreak::ApplySettings { changes, reply } => {
                match apply_settings(
                    &mut radio,
                    &mut settings_snapshot,
                    &mut next_settings_snapshot,
                    &changes,
                )
                .await
                {
                    Ok(result) => {
                        deferred_reply = Some(DeferredReply::Apply { reply, result });
                    }
                    Err(error) => drop(reply.send(Err(error))),
                }
            }
            ActorBreak::StartAprs { config, reply } => {
                settings_snapshot = None;
                lock_aprs_store(&aprs).begin_start(config.clone());
                let mut kiss = match radio.enter_kiss(config.data_rate.into()).await {
                    Ok(kiss) => kiss,
                    Err((returned_radio, error)) => {
                        radio = returned_radio;
                        restore_cat_after_kiss_return = true;
                        let detail = format!("could not enter KISS mode: {error}");
                        deferred_reply = Some(DeferredReply::FailedAprsStart { reply, detail });
                        continue;
                    }
                };

                if let Err(error) = apply_aprs_kiss_config(&mut kiss, &config).await {
                    let detail = format!("could not apply KISS parameters: {error}");
                    match kiss.exit().await {
                        Ok(returned_radio) => {
                            // The deferred-restore flag below owns the
                            // CAT re-proof on the next loop turn.
                            radio = returned_radio.into_radio_unproven();
                            restore_cat_after_kiss_return = true;
                            deferred_reply = Some(DeferredReply::FailedAprsStart { reply, detail });
                            continue;
                        }
                        Err((_kiss, exit_error)) => {
                            let combined =
                                format!("{detail}; KISS Return also failed: {exit_error}");
                            lock_aprs_store(&aprs).mark_failed(combined.clone());
                            drop(
                                reply
                                    .send(Err(AutomationError::AprsOperation { detail: combined })),
                            );
                            return;
                        }
                    }
                }

                let status = {
                    let mut store = lock_aprs_store(&aprs);
                    store.push_session_note("KISS runtime parameters were applied without transmitting an RF data packet.");
                    store.mark_active();
                    store.status()
                };
                drop(reply.send(Ok(status)));

                match aprs_loop(&mut kiss, &mut receiver, &aprs).await {
                    AprsBreak::Stop { reply } => {
                        lock_aprs_store(&aprs).mark_restoring();
                        match kiss.exit().await {
                            Ok(returned_radio) => {
                                // Deferred restore re-proves CAT next turn.
                                radio = returned_radio.into_radio_unproven();
                                restore_cat_after_kiss_return = true;
                                deferred_reply = Some(DeferredReply::StopAprs { reply });
                            }
                            Err((_kiss, error)) => {
                                let detail = format!("KISS Return failed: {error}");
                                lock_aprs_store(&aprs).mark_failed(detail.clone());
                                drop(reply.send(Err(AutomationError::AprsOperation { detail })));
                                return;
                            }
                        }
                    }
                    AprsBreak::Shutdown { reply } => {
                        lock_aprs_store(&aprs).mark_restoring();
                        let result = match kiss.exit().await {
                            Ok(returned_radio) => {
                                // Shutting down: disconnect, no re-proof.
                                returned_radio
                                    .into_radio_unproven()
                                    .disconnect()
                                    .await
                                    .map_err(|error| AutomationError::Shutdown {
                                        detail: error.to_string(),
                                    })
                            }
                            Err((_kiss, error)) => Err(AutomationError::Shutdown {
                                detail: format!("KISS Return failed during shutdown: {error}"),
                            }),
                        };
                        if let Some(reply) = reply {
                            drop(reply.send(result));
                        }
                        return;
                    }
                    AprsBreak::Fatal | AprsBreak::SenderClosed => {
                        drop(kiss.exit().await);
                        return;
                    }
                }
            }
            ActorBreak::PrepareIfDsp { reply } => {
                if reply.is_closed() {
                    continue;
                }
                settings_snapshot = None;
                // The library session performs the snapshot, configuration,
                // engagement proof, and failure rollback atomically; only
                // the incomplete-rollback case leaves a dirty session for a
                // later restore retry.
                match engage_if_dsp_radio(&mut radio).await {
                    Ok(saved) => {
                        let active = ActiveIfDspSession {
                            saved,
                            current_frequency_hz: saved.band_b_frequency_hz(),
                            output_verified: true,
                        };
                        if_dsp_session = Some(active);
                        deferred_reply = Some(DeferredReply::IfDsp {
                            operation: "IF-DSP prepare",
                            rollback_if_undelivered: true,
                            reply,
                            result: Ok(active.status()),
                        });
                    }
                    Err(EngageIfDspError::Clean(detail)) => {
                        deferred_reply = Some(DeferredReply::IfDsp {
                            operation: "IF-DSP prepare",
                            rollback_if_undelivered: true,
                            reply,
                            result: Err(AutomationError::IfDspOperation { detail }),
                        });
                    }
                    Err(EngageIfDspError::Dirty { detail, saved }) => {
                        if_dsp_session = Some(ActiveIfDspSession {
                            saved,
                            current_frequency_hz: saved.band_b_frequency_hz(),
                            output_verified: false,
                        });
                        deferred_reply = Some(DeferredReply::IfDsp {
                            operation: "failed IF-DSP prepare cleanup",
                            rollback_if_undelivered: true,
                            reply,
                            result: Err(AutomationError::IfDspRestoration { detail }),
                        });
                    }
                }
            }
            ActorBreak::RetuneIfDsp {
                frequency_hz,
                reply,
            } => {
                let result = match if_dsp_session {
                    Some(active) => match retune_if_dsp_radio(&mut radio, frequency_hz).await {
                        Ok(()) => {
                            let retuned = ActiveIfDspSession {
                                current_frequency_hz: frequency_hz,
                                output_verified: true,
                                ..active
                            };
                            if_dsp_session = Some(retuned);
                            Ok(retuned.status())
                        }
                        Err(retune_detail) => {
                            let report = restore_if_dsp_radio(&mut radio, active.saved).await;
                            if report.is_exact() {
                                if_dsp_session = None;
                                Err(AutomationError::IfDspOperation {
                                    detail: format!(
                                        "retune failed ({retune_detail}); the original radio state was restored and the IF-DSP session was stopped"
                                    ),
                                })
                            } else {
                                if_dsp_session = Some(ActiveIfDspSession {
                                    output_verified: false,
                                    ..active
                                });
                                Err(AutomationError::IfDspRestoration {
                                    detail: format!(
                                        "retune failed ({retune_detail}); {}",
                                        report.summary()
                                    ),
                                })
                            }
                        }
                    },
                    None => Err(AutomationError::IfDspModeInactive),
                };
                deferred_reply = Some(DeferredReply::IfDsp {
                    operation: "IF-DSP retune",
                    rollback_if_undelivered: false,
                    reply,
                    result,
                });
            }
            ActorBreak::RestoreIfDsp { reply } => {
                let result = match if_dsp_session {
                    Some(active) => {
                        let report = restore_if_dsp_radio(&mut radio, active.saved).await;
                        if report.is_exact() {
                            if_dsp_session = None;
                            Ok(IfDspRadioStatus::inactive())
                        } else {
                            if_dsp_session = Some(ActiveIfDspSession {
                                output_verified: false,
                                ..active
                            });
                            Err(AutomationError::IfDspRestoration {
                                detail: report.summary(),
                            })
                        }
                    }
                    None => Err(AutomationError::IfDspModeInactive),
                };
                deferred_reply = Some(DeferredReply::IfDsp {
                    operation: "IF-DSP restore",
                    rollback_if_undelivered: false,
                    reply,
                    result,
                });
            }
            ActorBreak::Shutdown { reply } => {
                let restoration_failure = if let Some(active) = if_dsp_session.take() {
                    let report = restore_if_dsp_radio(&mut radio, active.saved).await;
                    (!report.is_exact()).then(|| report.summary())
                } else {
                    None
                };
                let disconnect_result =
                    radio
                        .disconnect()
                        .await
                        .map_err(|error| AutomationError::Shutdown {
                            detail: error.to_string(),
                        });
                let result = match (restoration_failure, disconnect_result) {
                    (None, result) => result,
                    (Some(detail), Ok(())) => Err(AutomationError::Shutdown {
                        detail: format!("IF-DSP restoration was incomplete: {detail}"),
                    }),
                    (Some(restore), Err(AutomationError::Shutdown { detail })) => {
                        Err(AutomationError::Shutdown {
                            detail: format!(
                                "IF-DSP restoration was incomplete ({restore}); disconnect also failed: {detail}"
                            ),
                        })
                    }
                    (Some(_), Err(error)) => Err(error),
                };
                if let Some(reply) = reply {
                    drop(reply.send(result));
                }
                return;
            }
            ActorBreak::Fatal | ActorBreak::SenderClosed => {
                if let Some(active) = if_dsp_session.take() {
                    drop(restore_if_dsp_radio(&mut radio, active.saved).await);
                }
                drop(radio.disconnect().await);
                return;
            }
        }
    }
}

async fn qualify_automation(
    radio: &mut Radio<SwiftByteTransport>,
) -> Result<AutomationSession<'_, SwiftByteTransport>, String> {
    let mut session = radio
        .qualify_automation()
        .await
        .map_err(|error| error.to_string())?;
    let canary = session
        .verify_missing_snapshot_refusal(RadioFrontPanelKey::Menu)
        .await
        .map_err(|error| error.to_string())?;
    drop(canary);
    Ok(session)
}

#[expect(
    clippy::too_many_lines,
    reason = "the exhaustive command dispatcher makes mode-conflict replies auditable"
)]
async fn automation_loop(
    session: &mut AutomationSession<'_, SwiftByteTransport>,
    receiver: &mut mpsc::Receiver<ControllerCommand>,
    next_screen_lease: &mut u64,
    if_dsp_status: IfDspRadioStatus,
) -> ActorBreak {
    let mut pending_screen: Option<PendingScreen> = None;
    let if_dsp_active = if_dsp_status.phase != IfDspRadioPhase::Inactive;
    while let Some(command) = receiver.recv().await {
        match command {
            ControllerCommand::Capture { reply } => {
                if if_dsp_active {
                    drop(reply.send(Err(AutomationError::IfDspModeActive)));
                    continue;
                }
                pending_screen = None;
                let result = capture_screen(session, next_screen_lease)
                    .await
                    .map(|pending| {
                        let record = remote_screen(&pending.snapshot, pending.lease_id);
                        pending_screen = Some(pending);
                        record
                    });
                let valid = session.is_valid();
                drop(reply.send(result));
                if !valid {
                    return ActorBreak::Fatal;
                }
            }
            ControllerCommand::GuardedTap {
                lease_id,
                key,
                reply,
            } => {
                if if_dsp_active {
                    drop(reply.send(Err(AutomationError::IfDspModeActive)));
                    continue;
                }
                let result = guarded_tap(
                    session,
                    &mut pending_screen,
                    next_screen_lease,
                    lease_id,
                    key,
                )
                .await;
                let valid = session.is_valid();
                drop(reply.send(result));
                if !valid {
                    return ActorBreak::Fatal;
                }
            }
            ControllerCommand::ReadSettings { setting_ids, reply } => {
                if if_dsp_active {
                    drop(reply.send(Err(AutomationError::IfDspModeActive)));
                    continue;
                }
                return ActorBreak::ReadSettings { setting_ids, reply };
            }
            ControllerCommand::ApplySettings { changes, reply } => {
                if if_dsp_active {
                    drop(reply.send(Err(AutomationError::IfDspModeActive)));
                    continue;
                }
                return ActorBreak::ApplySettings { changes, reply };
            }
            ControllerCommand::StartAprs { config, reply } => {
                if if_dsp_active {
                    drop(reply.send(Err(AutomationError::IfDspModeActive)));
                    continue;
                }
                return ActorBreak::StartAprs { config, reply };
            }
            ControllerCommand::StopAprs { reply } => {
                drop(reply.send(Err(AutomationError::AprsModeInactive)));
            }
            ControllerCommand::SendAprsMessage { reply, .. }
            | ControllerCommand::SendAprsPosition { reply, .. } => {
                drop(reply.send(Err(AutomationError::AprsModeInactive)));
            }
            ControllerCommand::PrepareIfDsp { reply } => {
                if if_dsp_active {
                    drop(reply.send(Err(AutomationError::IfDspModeActive)));
                } else {
                    return ActorBreak::PrepareIfDsp { reply };
                }
            }
            ControllerCommand::IfDspStatus { reply } => {
                let _send_result = reply.send(if_dsp_status);
            }
            ControllerCommand::RetuneIfDsp {
                frequency_hz,
                reply,
            } => {
                if if_dsp_active {
                    return ActorBreak::RetuneIfDsp {
                        frequency_hz,
                        reply,
                    };
                }
                drop(reply.send(Err(AutomationError::IfDspModeInactive)));
            }
            ControllerCommand::RestoreIfDsp { reply } => {
                if if_dsp_active {
                    return ActorBreak::RestoreIfDsp { reply };
                }
                drop(reply.send(Err(AutomationError::IfDspModeInactive)));
            }
            ControllerCommand::Shutdown { reply } => return ActorBreak::Shutdown { reply },
        }
    }
    ActorBreak::SenderClosed
}

#[derive(Debug)]
enum AprsBreak {
    Stop {
        reply: oneshot::Sender<Result<AprsSessionStatus, AutomationError>>,
    },
    Shutdown {
        reply: Option<oneshot::Sender<Result<(), AutomationError>>>,
    },
    Fatal,
    SenderClosed,
}

async fn apply_aprs_kiss_config(
    kiss: &mut KissSession<SwiftByteTransport>,
    config: &AprsSessionConfig,
) -> Result<(), RadioError> {
    kiss.set_receive_timeout(Duration::from_millis(250));
    kiss.set_tx_delay(KissTxDelay::from_milliseconds(
        u16::from(config.tx_delay_10ms) * 10,
    )?)
    .await?;
    kiss.set_persistence(KissPersistence::new(config.persistence))
        .await?;
    kiss.set_slot_time(KissSlotTime::from_milliseconds(
        u16::from(config.slot_time_10ms) * 10,
    )?)
    .await?;
    kiss.set_tx_tail(KissTxTail::from_milliseconds(
        u16::from(config.tx_tail_10ms) * 10,
    )?)
    .await?;
    kiss.set_duplex(if config.full_duplex {
        KissDuplex::Full
    } else {
        KissDuplex::Half
    })
    .await?;
    kiss.set_hardware_data_rate(PacketDataRate::from(config.data_rate))
        .await?;
    Ok(())
}

#[expect(
    clippy::too_many_lines,
    reason = "the KISS owner loop keeps every controller command response explicit"
)]
async fn aprs_loop(
    kiss: &mut KissSession<SwiftByteTransport>,
    receiver: &mut mpsc::Receiver<ControllerCommand>,
    activity_store: &Mutex<AprsActivityStore>,
) -> AprsBreak {
    loop {
        tokio::select! {
            biased;
            command = receiver.recv() => {
                let Some(command) = command else {
                    return AprsBreak::SenderClosed;
                };
                match command {
                    ControllerCommand::StopAprs { reply } => return AprsBreak::Stop { reply },
                    ControllerCommand::Shutdown { reply } => {
                        return AprsBreak::Shutdown { reply };
                    }
                    ControllerCommand::StartAprs { reply, .. } => {
                        drop(reply.send(Err(AutomationError::AprsModeActive)));
                    }
                    ControllerCommand::Capture { reply } => {
                        drop(reply.send(Err(AutomationError::AprsModeActive)));
                    }
                    ControllerCommand::GuardedTap { reply, .. } => {
                        drop(reply.send(Err(AutomationError::AprsModeActive)));
                    }
                    ControllerCommand::ReadSettings { reply, .. } => {
                        drop(reply.send(Err(AutomationError::AprsModeActive)));
                    }
                    ControllerCommand::ApplySettings { reply, .. } => {
                        drop(reply.send(Err(AutomationError::AprsModeActive)));
                    }
                    ControllerCommand::PrepareIfDsp { reply }
                    | ControllerCommand::RetuneIfDsp { reply, .. }
                    | ControllerCommand::RestoreIfDsp { reply } => {
                        drop(reply.send(Err(AutomationError::AprsModeActive)));
                    }
                    ControllerCommand::IfDspStatus { reply } => {
                        let _send_result = reply.send(IfDspRadioStatus::inactive());
                    }
                    ControllerCommand::SendAprsMessage {
                        addressee,
                        text,
                        message_id,
                        reply,
                    } => {
                        let packet = lock_aprs_store(activity_store).build_message(
                            &addressee,
                            &text,
                            message_id.as_deref(),
                        );
                        match packet {
                            Ok(packet) => {
                                let raw_ax25 = build_ax25(&packet);
                                match kiss.send_data(&raw_ax25).await {
                                    Ok(()) => {
                                        let record = lock_aprs_store(activity_store)
                                            .push_transmitted(&packet);
                                        drop(reply.send(Ok(record)));
                                    }
                                    Err(error) => {
                                        let detail = format!("message transmit failed: {error}");
                                        lock_aprs_store(activity_store)
                                            .push_operation_error(detail.clone());
                                        drop(reply.send(Err(AutomationError::AprsOperation {
                                            detail,
                                        })));
                                    }
                                }
                            }
                            Err(error) => drop(reply.send(Err(error))),
                        }
                    }
                    ControllerCommand::SendAprsPosition {
                        latitude,
                        longitude,
                        comment,
                        reply,
                    } => {
                        let packet = lock_aprs_store(activity_store)
                            .build_position(latitude, longitude, &comment);
                        match packet {
                            Ok(packet) => {
                                let raw_ax25 = build_ax25(&packet);
                                match kiss.send_data(&raw_ax25).await {
                                    Ok(()) => {
                                        let record = lock_aprs_store(activity_store)
                                            .push_transmitted(&packet);
                                        drop(reply.send(Ok(record)));
                                    }
                                    Err(error) => {
                                        let detail = format!("position transmit failed: {error}");
                                        lock_aprs_store(activity_store)
                                            .push_operation_error(detail.clone());
                                        drop(reply.send(Err(AutomationError::AprsOperation {
                                            detail,
                                        })));
                                    }
                                }
                            }
                            Err(error) => drop(reply.send(Err(error))),
                        }
                    }
                }
            }
            incoming_frame = kiss.receive_frame() => {
                match incoming_frame {
                    Ok(frame) if frame.command == KissCommand::Data => {
                        drop(lock_aprs_store(activity_store).push_received_ax25(frame.data));
                    }
                    Ok(frame) => {
                        lock_aprs_store(activity_store).push_kiss_control(
                            &format!("{:?}", frame.command),
                            &frame.data,
                        );
                    }
                    Err(RadioError::Timeout(_)) => {}
                    Err(RadioError::Transport(kenwood_thd75::error::TransportError::Read(error)))
                        if matches!(
                            error.kind(),
                            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                        ) => {}
                    Err(error) => {
                        lock_aprs_store(activity_store).mark_failed(format!(
                            "KISS receive failed: {error}"
                        ));
                        return AprsBreak::Fatal;
                    }
                }
            }
        }
    }
}

fn lock_aprs_store(store: &Mutex<AprsActivityStore>) -> MutexGuard<'_, AprsActivityStore> {
    match store.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

async fn capture_screen(
    session: &mut AutomationSession<'_, SwiftByteTransport>,
    next_screen_lease: &mut u64,
) -> Result<PendingScreen, AutomationError> {
    let snapshot =
        session
            .capture_screen()
            .await
            .map_err(|error| AutomationError::ScreenCapture {
                detail: error.to_string(),
            })?;
    let lease_id = take_identifier(next_screen_lease);
    Ok(PendingScreen { lease_id, snapshot })
}

async fn guarded_tap(
    session: &mut AutomationSession<'_, SwiftByteTransport>,
    pending_screen: &mut Option<PendingScreen>,
    next_screen_lease: &mut u64,
    lease_id: u64,
    key: FrontPanelKey,
) -> Result<GuardedTapResult, AutomationError> {
    let Some(pending) = pending_screen.take() else {
        return Err(AutomationError::ScreenLeaseUnavailable);
    };
    if pending.lease_id != lease_id {
        let expected = pending.lease_id;
        *pending_screen = Some(pending);
        return Err(AutomationError::ScreenLeaseStale {
            expected,
            received: lease_id,
        });
    }

    let outcome = session
        .guarded_tap_key(&pending.snapshot, key.into())
        .await
        .map_err(map_guarded_error)?;
    let disposition = match outcome {
        GuardedKeyOutcome::Dispatched { .. } => GuardedTapDisposition::Dispatched,
        GuardedKeyOutcome::ContextChanged { .. } => GuardedTapDisposition::ContextChanged,
        GuardedKeyOutcome::DeadlineExpired { .. } => GuardedTapDisposition::DispatchedAfterDeadline,
        _ => {
            return Err(AutomationError::GuardedInput {
                detail: "unrecognized guarded-input outcome".to_owned(),
            });
        }
    };

    let post = capture_screen(session, next_screen_lease)
        .await
        .map_err(|error| AutomationError::PostTapCapture {
            disposition: disposition.label().to_owned(),
            detail: error.to_string(),
        })?;
    let screen = remote_screen(&post.snapshot, post.lease_id);
    *pending_screen = Some(post);
    Ok(GuardedTapResult {
        disposition,
        screen,
    })
}

fn remote_screen(snapshot: &AutomationSnapshot, lease_id: u64) -> RemoteScreenFrame {
    let rgb888 = snapshot.frame.to_rgb888();
    let mut rgba8888 = Vec::with_capacity(SCREEN_WIDTH * SCREEN_HEIGHT * 4);
    for pixel in rgb888.chunks_exact(3) {
        rgba8888.extend_from_slice(pixel);
        rgba8888.push(u8::MAX);
    }

    RemoteScreenFrame {
        lease_id,
        width: u32::try_from(SCREEN_WIDTH).unwrap_or(240),
        height: u32::try_from(SCREEN_HEIGHT).unwrap_or(180),
        row_bytes: u32::try_from(SCREEN_WIDTH * 4).unwrap_or(960),
        rgb565_le: snapshot.frame.rgb565_le().to_vec(),
        rgba8888,
        generation: snapshot.metadata.generation,
        crc32: snapshot.metadata.crc32,
        command_count: snapshot.metadata.command_count,
        seqlock: snapshot.metadata.seqlock,
    }
}

async fn read_settings(
    radio: &mut Radio<SwiftByteTransport>,
    requested_ids: Option<&[String]>,
    next_snapshot_id: &mut u64,
) -> Result<(SettingReadResult, SettingsSnapshot), AutomationError> {
    ensure_settings_target(radio, false).await?;
    let fields = select_fields(requested_ids)?;
    let pages = pages_for_fields(&fields)?;
    let live_pages = radio
        .read_sparse_memory_pages(&pages)
        .await
        .map_err(|error| AutomationError::SettingsRead {
            detail: error.to_string(),
        })?;
    let page_map: BTreeMap<u16, [u8; PAGE_SIZE]> = live_pages
        .into_iter()
        .map(|(page, data)| (page.as_raw(), data))
        .collect();
    let field_ids: Vec<String> = fields
        .iter()
        .map(|field| field.descriptor.name.to_owned())
        .collect();
    let snapshot_id = take_identifier(next_snapshot_id);
    let values = decode_values(&field_ids, &page_map)?;
    let result = SettingReadResult {
        snapshot_id,
        values,
    };
    let snapshot = SettingsSnapshot {
        id: snapshot_id,
        pages: page_map,
        field_ids,
    };
    Ok((result, snapshot))
}

async fn apply_settings(
    radio: &mut Radio<SwiftByteTransport>,
    cached_snapshot: &mut Option<SettingsSnapshot>,
    next_snapshot_id: &mut u64,
    changes: &[SettingChange],
) -> Result<SettingApplyReport, AutomationError> {
    let validation = validate_changes(changes);
    if !validation.accepted {
        return Err(AutomationError::InvalidSettingsPlan {
            detail: validation
                .batch_error
                .unwrap_or_else(|| "setting plan validation failed".to_owned()),
        });
    }
    let (patches, fields) = build_patch_plan(changes)
        .map_err(|detail| AutomationError::InvalidSettingsPlan { detail })?;
    let snapshot_id = changes
        .first()
        .map(|change| change.snapshot_id)
        .ok_or_else(|| AutomationError::InvalidSettingsPlan {
            detail: "a setting plan must contain at least one change".to_owned(),
        })?;
    let snapshot = cached_snapshot
        .as_ref()
        .ok_or(AutomationError::SettingsSnapshotUnavailable { snapshot_id })?;
    if snapshot.id != snapshot_id {
        return Err(AutomationError::SettingsSnapshotUnavailable { snapshot_id });
    }

    verify_setting_preconditions(changes, &fields, snapshot, &patches)?;
    ensure_settings_target(radio, true).await?;

    let consumed = cached_snapshot
        .take()
        .ok_or(AutomationError::SettingsSnapshotUnavailable { snapshot_id })?;
    let replacements = replacement_pages(&consumed.pages, &patches)?;
    let exchanges: Vec<McpPageExchange> =
        patches
            .pages()
            .map(|page| {
                let raw_page = page.as_raw();
                let expected = consumed
                    .pages
                    .get(&raw_page)
                    .copied()
                    .ok_or(AutomationError::SettingsSnapshotUnavailable { snapshot_id })?;
                let replacement = replacements.get(&raw_page).copied().ok_or_else(|| {
                    AutomationError::Internal {
                        detail: format!("replacement for MCP page 0x{raw_page:04X} is missing"),
                    }
                })?;
                Ok(McpPageExchange::new(page, expected, replacement))
            })
            .collect::<Result<_, AutomationError>>()?;

    let pages_written = radio
        .compare_exchange_memory_pages(&exchanges)
        .await
        .map_err(|error| map_exchange_error(&error))?
        .into_iter()
        .map(WritableMcpPage::as_raw)
        .collect();

    let mut refreshed_pages = consumed.pages;
    for (page, replacement) in replacements {
        let _old = refreshed_pages.insert(page, replacement);
    }
    let refreshed_snapshot_id = take_identifier(next_snapshot_id);
    let refreshed_records = decode_values(&consumed.field_ids, &refreshed_pages)?;
    let refreshed_values = SettingReadResult {
        snapshot_id: refreshed_snapshot_id,
        values: refreshed_records,
    };
    *cached_snapshot = Some(SettingsSnapshot {
        id: refreshed_snapshot_id,
        pages: refreshed_pages,
        field_ids: consumed.field_ids,
    });

    let change_results = changes
        .iter()
        .map(|change| SettingChangeResult {
            setting_id: change.setting_id.clone(),
            outcome: if change.expected_value == change.desired_value {
                SettingChangeOutcome::AlreadyCurrent
            } else {
                SettingChangeOutcome::Applied
            },
            value: change.desired_value.clone(),
        })
        .collect();

    Ok(SettingApplyReport {
        previous_snapshot_id: snapshot_id,
        pages_written,
        changes: change_results,
        refreshed_values,
    })
}

fn verify_setting_preconditions(
    changes: &[SettingChange],
    fields: &[&MenuField],
    snapshot: &SettingsSnapshot,
    patches: &PatchSet,
) -> Result<(), AutomationError> {
    let field_ids = fields
        .iter()
        .map(|field| field.descriptor.name.to_owned())
        .collect::<Vec<_>>();
    let reviewed_values = decode_values(&field_ids, &snapshot.pages)?;
    for (change, reviewed) in changes.iter().zip(&reviewed_values) {
        if change.expected_value != reviewed.value {
            return Err(AutomationError::SettingPreconditionFailed {
                setting_id: change.setting_id.clone(),
                expected: format!("{:?}", change.expected_value),
                actual: format!("{:?}", reviewed.value),
            });
        }
    }
    if patches
        .pages()
        .any(|page| !snapshot.pages.contains_key(&page.as_raw()))
    {
        return Err(AutomationError::SettingsSnapshotUnavailable {
            snapshot_id: snapshot.id,
        });
    }
    Ok(())
}

async fn ensure_settings_target(
    radio: &mut Radio<SwiftByteTransport>,
    applying: bool,
) -> Result<(), AutomationError> {
    radio
        .verify_mcp_schema_target()
        .await
        .map_err(|error| settings_operation_error(applying, error.to_string()))
}

fn settings_operation_error(applying: bool, detail: String) -> AutomationError {
    if applying {
        AutomationError::SettingsApply { detail }
    } else {
        AutomationError::SettingsRead { detail }
    }
}

fn select_fields(
    requested_ids: Option<&[String]>,
) -> Result<Vec<&'static MenuField>, AutomationError> {
    let fields: Vec<&'static MenuField> = match requested_ids {
        None => MCP_D75_MENU_FIELDS
            .iter()
            .filter(|field| !field.is_blob)
            .collect(),
        Some(ids) => ids
            .iter()
            .map(|identifier| {
                menu_field(identifier).ok_or_else(|| AutomationError::SettingsRead {
                    detail: format!("unknown setting identifier {identifier}"),
                })
            })
            .collect::<Result<_, _>>()?,
    };
    let mut unique = BTreeSet::new();
    for field in &fields {
        if !unique.insert(field.descriptor.name) {
            return Err(AutomationError::SettingsRead {
                detail: format!(
                    "setting identifier {} was requested more than once",
                    field.descriptor.name
                ),
            });
        }
    }
    Ok(fields)
}

fn pages_for_fields(fields: &[&MenuField]) -> Result<Vec<McpPage>, AutomationError> {
    let mut pages = BTreeSet::new();
    for field in fields {
        pages.extend(
            field
                .descriptor
                .pages()
                .map_err(|error| AutomationError::SettingsRead {
                    detail: error.to_string(),
                })?,
        );
    }
    Ok(pages.into_iter().collect())
}

fn decode_values(
    field_ids: &[String],
    pages: &BTreeMap<u16, [u8; PAGE_SIZE]>,
) -> Result<Vec<SettingValueRecord>, AutomationError> {
    let snapshot_pages = pages
        .iter()
        .map(|(&page, data)| {
            McpPage::new(page)
                .map(|page| (page, *data))
                .map_err(|error| AutomationError::SettingsRead {
                    detail: error.to_string(),
                })
        })
        .collect::<Result<Vec<_>, _>>()?;
    let snapshot = MenuFieldSnapshot::from_pages(snapshot_pages).map_err(|error| {
        AutomationError::SettingsRead {
            detail: error.to_string(),
        }
    })?;
    field_ids
        .iter()
        .map(|identifier| {
            let field = menu_field(identifier).ok_or_else(|| AutomationError::SettingsRead {
                detail: format!("unknown cached setting identifier {identifier}"),
            })?;
            let value = snapshot
                .value(field)
                .map_err(|error| AutomationError::SettingsRead {
                    detail: error.to_string(),
                })?;
            Ok(SettingValueRecord {
                setting_id: field.descriptor.name.to_owned(),
                value: setting_value(value),
            })
        })
        .collect()
}

fn setting_value(value: DecodedFieldValue) -> SettingValue {
    match value {
        DecodedFieldValue::Unsigned(value) => SettingValue::Unsigned { value },
        DecodedFieldValue::Signed(value) => SettingValue::Signed { value },
        DecodedFieldValue::Bool(value) => SettingValue::Boolean { value },
        DecodedFieldValue::Text(value) => SettingValue::Text { value },
        DecodedFieldValue::Bytes(value) => SettingValue::Bytes { value },
    }
}

fn replacement_pages(
    expected: &BTreeMap<u16, [u8; PAGE_SIZE]>,
    patches: &PatchSet,
) -> Result<BTreeMap<u16, [u8; PAGE_SIZE]>, AutomationError> {
    let mut replacements = BTreeMap::new();
    for page in patches.pages() {
        let raw_page = page.as_raw();
        let mut replacement = expected
            .get(&raw_page)
            .copied()
            .ok_or(AutomationError::SettingsSnapshotUnavailable { snapshot_id: 0 })?;
        let patch = patches
            .page(page)
            .ok_or_else(|| AutomationError::Internal {
                detail: format!("patch for MCP page 0x{raw_page:04X} is missing"),
            })?;
        patch.apply_to_page(&mut replacement);
        let _previous = replacements.insert(raw_page, replacement);
    }
    Ok(replacements)
}

fn map_exchange_error(error: &McpPageExchangeError) -> AutomationError {
    if matches!(
        &error,
        McpPageExchangeError::Operation { operation, .. }
            if matches!(operation.as_ref(), McpPageExchangeOperationError::CompareMismatch { .. })
    ) {
        return AutomationError::SettingsSnapshotStale {
            detail: error.to_string(),
        };
    }
    let possibly_written = error.possibly_written_pages();
    let detail = if possibly_written.is_empty() {
        error.to_string()
    } else {
        format!("{error}; writes may have started for pages {possibly_written:?}")
    };
    AutomationError::SettingsApply { detail }
}

fn map_guarded_error(error: GuardedKeyError) -> AutomationError {
    match error {
        GuardedKeyError::SnapshotUnavailable | GuardedKeyError::SnapshotExpired { .. } => {
            AutomationError::ScreenLeaseUnavailable
        }
        other => AutomationError::GuardedInput {
            detail: other.to_string(),
        },
    }
}

fn response_lost(_error: oneshot::error::RecvError) -> AutomationError {
    AutomationError::Internal {
        detail: "controller ended before returning the requested result".to_owned(),
    }
}

const fn take_identifier(next: &mut u64) -> u64 {
    let current = *next;
    *next = if current == u64::MAX { 1 } else { current + 1 };
    current
}

#[cfg(test)]
mod tests {
    use super::*;
    use kenwood_thd75::screen::{SCREEN_BYTES, ScreenFrame};
    use kenwood_thd75::transport::MockTransport;
    use kenwood_thd75::types::{PacketDataRate, RadioModel};
    use kiss_tnc::FEND;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn expect_cat_recovery_preamble(transport: &mut MockTransport) {
        transport.expect(b"\r", b"");
        transport.expect(b"\r", b"");
        transport.expect(&[0x03], b"");
        transport.expect(&[FEND, 0xFF, FEND], b"");
        transport.expect(b"\rTC 1\r", b"");
        transport.expect(b"TN 0,0\r", b"");
    }

    #[test]
    fn screen_record_has_exact_dimensions_and_rendering() -> TestResult {
        let raw = vec![0_u8; SCREEN_BYTES];
        let snapshot = AutomationSnapshot {
            frame: ScreenFrame::from_rgb565_le(raw)?,
            metadata: kenwood_thd75::radio::automation::AutomationMetadata {
                seqlock: 2,
                features: 0x7F,
                generation: 1,
                capture_result: 0,
                crc32: 0,
                capture_attempts: 1,
                command_count: 1,
                last_command: 1,
                last_host_sequence: 0,
                last_key: 0,
                last_phase: 0,
                last_key_result: 0,
                rle_encoded_length: 0,
                route_ascii: 0,
                route_guard_count: 0,
                route_completed_taps: 0,
                route_event_mask: 0,
            },
        };
        let record = remote_screen(&snapshot, 9);

        assert_eq!(record.width, 240);
        assert_eq!(record.height, 180);
        assert_eq!(record.row_bytes, 960);
        assert_eq!(record.rgb565_le.len(), SCREEN_BYTES);
        assert_eq!(record.rgba8888.len(), SCREEN_WIDTH * SCREEN_HEIGHT * 4);
        assert_eq!(record.lease_id, 9);
        Ok(())
    }

    #[test]
    fn setting_pages_include_cross_page_fields() -> TestResult {
        let field = menu_field("radio.PoweronBitmap").ok_or("bitmap field missing")?;
        let pages = pages_for_fields(&[field])?;

        assert_eq!(pages.len(), 338, "86,400 bytes span 338 MCP pages");
        Ok(())
    }

    #[test]
    fn identifiers_never_return_zero() {
        let mut next = u64::MAX;
        assert_eq!(take_identifier(&mut next), u64::MAX);
        assert_eq!(take_identifier(&mut next), 1);
        assert_eq!(next, 2);
    }

    #[tokio::test]
    async fn kiss_return_is_synchronized_before_strict_automation_qualification() -> TestResult {
        let mut transport = MockTransport::new();
        transport.expect(b"TN 2,0\r", b"TN 2,0\r");
        transport.expect(&[FEND, 0xFF, FEND], &[]);
        expect_cat_recovery_preamble(&mut transport);
        transport.expect(b"ID\r", b"ID TH-D75\r");
        transport.expect(b"ID\r", b"ID TH-D75\r");
        transport.pend_when_empty();

        let radio = Radio::new(transport);
        let kiss = radio
            .enter_kiss(PacketDataRate::Bps1200)
            .await
            .map_err(|(_, error)| error)?;
        // Take the unproven hatch deliberately: the point of this test
        // is that the radio itself stays fail-closed until restored.
        let mut radio = kiss
            .exit()
            .await
            .map_err(|(_, error)| error)?
            .into_radio_unproven();

        let direct_qualification = radio.qualify_automation().await;
        assert!(
            matches!(direct_qualification, Err(RadioError::CatRecoveryRequired)),
            "KISS exit must keep the strict qualifier fail-closed: {direct_qualification:?}"
        );

        radio.restore_cat_after_mode_exit().await?;
        assert_eq!(radio.identify().await?.model, RadioModel::ThD75);
        Ok(())
    }

    #[tokio::test]
    async fn kiss_return_cat_synchronization_reopens_after_bad_identity() -> TestResult {
        let mut transport = MockTransport::new();
        transport.expect(b"TN 2,0\r", b"TN 2,0\r");
        transport.expect(&[FEND, 0xFF, FEND], &[]);
        expect_cat_recovery_preamble(&mut transport);
        transport.expect(b"ID\r", b"ID OTHER\r");
        transport.expect_reopen(Ok(()));
        expect_cat_recovery_preamble(&mut transport);
        transport.expect(b"ID\r", b"ID TH-D75\r");
        transport.expect(b"ID\r", b"ID TH-D75\r");
        transport.pend_when_empty();

        let radio = Radio::new(transport);
        let kiss = radio
            .enter_kiss(PacketDataRate::Bps1200)
            .await
            .map_err(|(_, error)| error)?;
        let desynced = kiss.exit().await.map_err(|(_, error)| error)?;
        let mut radio = match desynced.restore().await {
            Ok(radio) => radio,
            Err((_desynced, error)) => return Err(error.into()),
        };
        assert_eq!(radio.identify().await?.model, RadioModel::ThD75);
        Ok(())
    }

    #[test]
    fn undelivered_prepare_requests_actor_owned_radio_rollback() {
        let store = Mutex::new(AprsActivityStore::default());
        let (reply, response) = oneshot::channel();
        drop(response);
        let deferred = DeferredReply::IfDsp {
            operation: "IF-DSP prepare",
            rollback_if_undelivered: true,
            reply,
            result: Ok(IfDspRadioStatus::active(145_500_000)),
        };

        assert!(
            deferred.complete(&store),
            "a dropped prepare receiver must not silently commit radio ownership"
        );
    }

    #[test]
    fn undelivered_retune_does_not_discard_preexisting_ownership() {
        let store = Mutex::new(AprsActivityStore::default());
        let (reply, response) = oneshot::channel();
        drop(response);
        let deferred = DeferredReply::IfDsp {
            operation: "IF-DSP retune",
            rollback_if_undelivered: false,
            reply,
            result: Ok(IfDspRadioStatus::active(145_525_000)),
        };

        assert!(
            !deferred.complete(&store),
            "retune cancellation must leave the caller's existing reservation observable"
        );
    }

    #[test]
    fn aprs_stop_reply_is_released_only_after_automation_restoration_completes() -> TestResult {
        let store = Mutex::new(AprsActivityStore::default());
        {
            let mut journal = lock_aprs_store(&store);
            journal.begin_start(AprsSessionConfig::default());
            journal.mark_active();
            journal.mark_restoring();
        }
        let (reply, mut response) = oneshot::channel();
        let deferred = DeferredReply::StopAprs { reply };

        assert!(matches!(
            response.try_recv(),
            Err(oneshot::error::TryRecvError::Empty)
        ));
        assert_eq!(
            lock_aprs_store(&store).status().phase,
            crate::aprs::AprsSessionPhase::Restoring
        );

        // `run_controller` invokes this only after `qualify_automation` returns a
        // proven automation session.
        assert!(
            !deferred.complete(&store),
            "an APRS stop reply never requests IF rollback"
        );
        let status = response.try_recv()??;
        assert_eq!(status.phase, crate::aprs::AprsSessionPhase::Inactive);
        assert_eq!(lock_aprs_store(&store).status(), status);
        Ok(())
    }

    #[test]
    fn failed_automation_restoration_never_reports_aprs_as_stopped_cleanly() -> TestResult {
        let store = Mutex::new(AprsActivityStore::default());
        {
            let mut journal = lock_aprs_store(&store);
            journal.begin_start(AprsSessionConfig::default());
            journal.mark_active();
            journal.mark_restoring();
        }
        let (reply, mut response) = oneshot::channel();
        DeferredReply::StopAprs { reply }.fail("canary rejected".to_owned(), &store);

        let Err(error) = response.try_recv()? else {
            return Err("restoration unexpectedly succeeded".into());
        };
        assert_eq!(
            error,
            AutomationError::AutomationRestoration {
                operation: "APRS stop".to_owned(),
                detail: "canary rejected".to_owned(),
            }
        );
        let status = lock_aprs_store(&store).status();
        assert_eq!(status.phase, crate::aprs::AprsSessionPhase::Failed);
        assert!(
            status
                .last_error
                .is_some_and(|detail| { detail.contains("automation restoration failed") })
        );
        Ok(())
    }

    #[test]
    fn failed_aprs_start_reports_inactive_only_after_automation_cleanup() -> TestResult {
        let store = Mutex::new(AprsActivityStore::default());
        lock_aprs_store(&store).begin_start(AprsSessionConfig::default());
        let (reply, mut response) = oneshot::channel();
        let deferred = DeferredReply::FailedAprsStart {
            reply,
            detail: "KISS parameters were rejected".to_owned(),
        };

        assert!(matches!(
            response.try_recv(),
            Err(oneshot::error::TryRecvError::Empty)
        ));
        assert!(
            !deferred.complete(&store),
            "a failed APRS start reply never requests IF rollback"
        );
        let Err(error) = response.try_recv()? else {
            return Err("failed APRS start unexpectedly succeeded".into());
        };
        assert_eq!(
            error,
            AutomationError::AprsOperation {
                detail: "KISS parameters were rejected".to_owned(),
            }
        );
        let status = lock_aprs_store(&store).status();
        assert_eq!(status.phase, crate::aprs::AprsSessionPhase::Inactive);
        assert_eq!(
            status.last_error.as_deref(),
            Some("KISS parameters were rejected")
        );
        Ok(())
    }

    #[test]
    fn all_front_panel_keys_map_to_exact_distinct_raw_ids() {
        let keys = [
            FrontPanelKey::Mode,
            FrontPanelKey::Menu,
            FrontPanelKey::Ab,
            FrontPanelKey::Function,
            FrontPanelKey::Monitor,
            FrontPanelKey::Up,
            FrontPanelKey::Down,
            FrontPanelKey::Left,
            FrontPanelKey::Right,
            FrontPanelKey::Enter,
            FrontPanelKey::Mark0,
            FrontPanelKey::Vfo1,
            FrontPanelKey::Mr2,
            FrontPanelKey::Call3,
            FrontPanelKey::Msg4,
            FrontPanelKey::List5,
            FrontPanelKey::Beacon6,
            FrontPanelKey::Reverse7,
            FrontPanelKey::Tone8,
            FrontPanelKey::Pf1_9,
            FrontPanelKey::MhzStar,
            FrontPanelKey::Pf2Hash,
            FrontPanelKey::MicPf1,
            FrontPanelKey::MicPf2,
            FrontPanelKey::MicPf3,
        ];
        let raw: BTreeSet<u8> = keys
            .into_iter()
            .map(|key| RadioFrontPanelKey::from(key).as_raw())
            .collect();

        assert_eq!(raw.len(), 25, "all dispatcher keys must remain distinct");
        assert_eq!(raw.first(), Some(&0));
        assert_eq!(raw.last(), Some(&24));
    }
}
