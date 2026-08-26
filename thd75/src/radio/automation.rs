//! Fail-closed host protocol for the TH-D75 V1.03.AZM automation target.
//!
//! The firmware extends the already-guarded `GM` command with an exact ABI
//! query, bounded front-panel events, and stable LCD snapshots.  None of those
//! operations are exposed until [`Radio::qualify_automation`] proves the exact
//! V1.03.AZM identity, every patched hook, the complete linked runtime, the ABI
//! reply, and the reader's upper-bound refusal. Firmware-side framebuffer
//! guards protect individual keys and one-command three-digit routes. The route
//! guard is atomic with respect to the intentional redraws caused by its three
//! synchronous digit taps.
//!
//! A qualified [`AutomationSession`] exclusively borrows its [`Radio`].  Every
//! operation poisons both the session and the underlying strict `GM` stream
//! before its first await; only a completely parsed and independently
//! validated result clears that poison.
//! Cancelling or failing an operation therefore requires a transport reconnect
//! before any further CAT traffic.

use std::time::Duration;

use crate::error::{Error, ValidationError};
use crate::screen::{SCREEN_BYTES, ScreenFrame};
use crate::transport::Transport;
use crate::types::{Frequency, MemoryReadOffset, ReadLen, UsbAudioOutput};

use super::{
    McpPhase, Radio,
    if_tap::{IfTapConfig, IfTapEnterError, IfTapRestoreReport, IfTapSavedState},
};
#[cfg(feature = "aprs")]
use crate::types::{SerialNumber, TncDataBand, TncState};

#[path = "automation_runtime.rs"]
mod automation_runtime;

const EXPECTED_MODEL_FRAME: &[u8] = b"ID TH-D75\r";
const EXPECTED_FIRMWARE_FRAME: &[u8] = b"FV 1.03.AZM\r";
const ABI_QUERY: &[u8] = b"GM A000000\r";
const ABI_REPLY: &[u8] = b"GM A00000044373541037F1802\r";
const CROSSING_READ: &[u8] = b"GM FFFFFF,02\r";
const CROSSING_REFUSAL: &[u8] = b"N\r";

const AUTOMATION_MAGIC: u32 = 0x4135_3744;
const AUTOMATION_MAX_KEY: u8 = 0x18;
const AUTOMATION_MAX_PHASE: u8 = 2;
const PIXEL_FORMAT_RGB565LE: u32 = 0x3536_3552;
const FRAME_WIDTH: u32 = 240;
const FRAME_HEIGHT: u32 = 180;
const FRAME_STRIDE: u32 = 480;

const METADATA_OFFSET: u32 = 0xF0_0000;
const METADATA_LENGTH: u32 = 0x100;
const PIXEL_OFFSET: u32 = 0xF0_0100;
const PIXEL_LENGTH: u32 = 0x1_5180;
const RAW_APERTURE_END: u32 = 0xF1_5280;
const RLE_OFFSET: u32 = 0xF1_5300;
const RLE_APERTURE_END: u32 = 0xF2_A480;
const RLE_MAGIC: u32 = 0x3345_4C52;
const RLE_RELATIVE_OFFSET: u32 = 0x1_5300;

const FRAMEBUFFER_ADDRESS: u32 = 0xC234_9A40;
const SNAPSHOT_ADDRESS: u32 = 0xC01A_0100;
const MAX_CAPTURE_ATTEMPTS: u32 = 3;
const MAX_HOST_CAPTURE_ATTEMPTS: u8 = 3;
const TAP_HOLD: Duration = Duration::from_millis(40);
const CAPTURE_RETRY_DELAY: Duration = Duration::from_millis(40);
const CHANGED_CONTEXT_CANARY_SETTLE: Duration = Duration::from_millis(140);
/// Maximum post-validation interval a host may hold a guarded snapshot lease.
///
/// The clock starts only after raw/RLE transfer, metadata continuity, and CRC
/// validation complete. It bounds subsequent semantic screen validation and
/// other host work; transfer time is deliberately excluded because the live
/// firmware comparison, not this host clock, is the actual context guard.
pub const GUARDED_SNAPSHOT_MAX_AGE: Duration = Duration::from_secs(5);
/// Maximum interval allowed for one guarded route dispatch exchange.
///
/// 1.5 seconds bounds the single `GM R` exchange or three legacy Bluetooth
/// `GM G`/`GM K` taps without assuming any undocumented numeric-entry timeout.
/// Every `GM G` exchange is bounded by one absolute deadline; `GM R` applies
/// the same bound once around its sole request/reply. Cancelling transport I/O
/// cannot prove when or whether
/// already-submitted bytes reached the radio, so an overdue exchange poisons
/// the session rather than claiming a physical-delivery bound. On the legacy
/// path, once a press is acknowledged as dispatched, its `GM K` release is
/// attempted even if cleanup crosses the deadline; no subsequent press is sent.
pub const GUARDED_ROUTE_MAX_DURATION: Duration = Duration::from_millis(1_500);
/// Maximum number of keys authorized by one guarded input transaction.
pub const GUARDED_INPUT_MAX_TAPS: usize = 3;

const COMMAND_SNAPSHOT: u32 = 1;
const COMMAND_KEY: u32 = 2;
const COMMAND_GUARDED_KEY: u32 = 3;
const COMMAND_GUARDED_DECIMAL_ROUTE: u32 = 4;
const RESULT_OK: u32 = 0;
const RESULT_UNSTABLE: u32 = 1;
const RESULT_CONTEXT_CHANGED: u32 = 2;
const GUARDED_STATUS_OK: u8 = 0;

const DISPATCH_ATTESTATION_OFFSET: u32 = 0x02_E2C8;
const DISPATCH_ATTESTATION: &[u8] = &[0x01, 0xEC, 0x02, 0xC0, 0x47, 0x4D, 0x00, 0x00];
const ADAPTER_ATTESTATION_OFFSET: u32 = 0x02_EC00;
const ADAPTER_ATTESTATION: &[u8] = &[
    0x10, 0xB5, 0x14, 0x00, 0x6E, 0xF1, 0x3C, 0xFB, 0x02, 0x20, 0x20, 0x70, 0x10, 0xBD,
];
const BOUND_ATTESTATION_OFFSET: u32 = 0x06_F85C;
const BOUND_ATTESTATION: &[u8] = &[0x80, 0x26, 0x76, 0x04];
const READ_HOOK_ATTESTATION_OFFSET: u32 = 0x06_F8A0;
const READ_HOOK_ATTESTATION: &[u8] = &[
    0xC0, 0x26, 0x36, 0x06, 0x01, 0x99, 0x89, 0x19, 0x02, 0xA8, 0x00, 0x9A, 0x2D, 0xF1, 0x1F, 0xFF,
];
const AUTOMATION_RUNTIME_OFFSET: u32 = 0x19_D280;
const AUTOMATION_RUNTIME: &[u8] = automation_runtime::BYTES;

/// Exact automation ABI proved during V1.03.AZM qualification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AutomationAbi {
    /// ABI revision returned by the firmware.
    pub version: u8,
    /// Feature bitmap returned by the firmware.
    pub features: u8,
    /// Largest accepted raw front-panel key identifier.
    pub max_key: u8,
    /// Largest accepted event phase.
    pub max_phase: u8,
}

const EXPECTED_ABI: AutomationAbi = AutomationAbi {
    version: 3,
    features: 0x7F,
    max_key: AUTOMATION_MAX_KEY,
    max_phase: AUTOMATION_MAX_PHASE,
};

/// One of the 25 ordinary input-dispatch identifiers accepted by automation.
///
/// Live key-to-screen qualification established the named functions and the
/// four directional orientations from exact selected-menu-label transitions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum FrontPanelKey {
    /// `[MODE]` (`0x00`).
    Mode = 0x00,
    /// `[MENU]` (`0x01`).
    Menu = 0x01,
    /// `[A/B]` (`0x02`).
    Ab = 0x02,
    /// `[F]` function key (`0x03`).
    Function = 0x03,
    /// `[MONI]` monitor key (`0x04`).
    Monitor = 0x04,
    /// Direction pad up (`0x05`).
    Up = 0x05,
    /// Direction pad down (`0x06`).
    Down = 0x06,
    /// Direction pad left (`0x07`).
    Left = 0x07,
    /// Direction pad right (`0x08`).
    Right = 0x08,
    /// Enter/confirm (`0x09`).
    Enter = 0x09,
    /// `[MARK]` / keypad `0` (`0x0A`).
    Mark0 = 0x0A,
    /// `[VFO]` / keypad `1` (`0x0B`).
    Vfo1 = 0x0B,
    /// `[MR]` / keypad `2` (`0x0C`).
    Mr2 = 0x0C,
    /// `[CALL]` / keypad `3` (`0x0D`).
    Call3 = 0x0D,
    /// `[MSG]` / keypad `4` (`0x0E`).
    Msg4 = 0x0E,
    /// `[LIST]` / keypad `5` (`0x0F`).
    List5 = 0x0F,
    /// `[BCN]` / keypad `6` (`0x10`).
    Beacon6 = 0x10,
    /// `[REV]` / keypad `7` (`0x11`).
    Reverse7 = 0x11,
    /// `[TONE]` / keypad `8` (`0x12`).
    Tone8 = 0x12,
    /// Front `[PF1]` / keypad `9` (`0x13`).
    Pf1_9 = 0x13,
    /// `[MHz]` / keypad `*` (`0x14`).
    MhzStar = 0x14,
    /// Front `[PF2]` / keypad `#` (`0x15`).
    Pf2Hash = 0x15,
    /// Microphone PF1 (`0x16`).
    MicPf1 = 0x16,
    /// Microphone PF2 (`0x17`).
    MicPf2 = 0x17,
    /// Microphone PF3 (`0x18`).
    MicPf3 = 0x18,
}

impl FrontPanelKey {
    /// Return the exact raw dispatcher identifier.
    #[must_use]
    pub const fn as_raw(self) -> u8 {
        self as u8
    }
}

impl TryFrom<u8> for FrontPanelKey {
    type Error = ValidationError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0x00 => Ok(Self::Mode),
            0x01 => Ok(Self::Menu),
            0x02 => Ok(Self::Ab),
            0x03 => Ok(Self::Function),
            0x04 => Ok(Self::Monitor),
            0x05 => Ok(Self::Up),
            0x06 => Ok(Self::Down),
            0x07 => Ok(Self::Left),
            0x08 => Ok(Self::Right),
            0x09 => Ok(Self::Enter),
            0x0A => Ok(Self::Mark0),
            0x0B => Ok(Self::Vfo1),
            0x0C => Ok(Self::Mr2),
            0x0D => Ok(Self::Call3),
            0x0E => Ok(Self::Msg4),
            0x0F => Ok(Self::List5),
            0x10 => Ok(Self::Beacon6),
            0x11 => Ok(Self::Reverse7),
            0x12 => Ok(Self::Tone8),
            0x13 => Ok(Self::Pf1_9),
            0x14 => Ok(Self::MhzStar),
            0x15 => Ok(Self::Pf2Hash),
            0x16 => Ok(Self::MicPf1),
            0x17 => Ok(Self::MicPf2),
            0x18 => Ok(Self::MicPf3),
            _ => Err(ValidationError::SettingOutOfRange {
                name: "automation front-panel key",
                value,
                detail: "must be 0x00-0x18",
            }),
        }
    }
}

impl From<FrontPanelKey> for u8 {
    fn from(key: FrontPanelKey) -> Self {
        key.as_raw()
    }
}

/// Exactly three decimal digits accepted by the guarded route command.
///
/// Keeping this as a constructed type prevents a malformed or non-decimal
/// direct-menu route from reaching the radio. Digits are numeric (`0..=9`),
/// not ASCII; leading zeroes are preserved.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct GuardedDecimalRoute {
    digits: [u8; 3],
}

impl GuardedDecimalRoute {
    /// Construct one exact three-digit route.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::SettingOutOfRange`] when any element is not
    /// a decimal digit.
    pub fn new(digits: [u8; 3]) -> Result<Self, ValidationError> {
        if let Some(&value) = digits.iter().find(|&&digit| digit > 9) {
            return Err(ValidationError::SettingOutOfRange {
                name: "guarded decimal route digit",
                value,
                detail: "must be 0-9",
            });
        }
        Ok(Self { digits })
    }

    /// Return the three numeric digits in dispatch order.
    #[must_use]
    pub const fn digits(self) -> [u8; 3] {
        self.digits
    }

    const fn ascii_digits(self) -> [u8; 3] {
        let [first, second, third] = self.digits;
        [first + b'0', second + b'0', third + b'0']
    }

    fn packed_ascii(self) -> u32 {
        let [first, second, third] = self.ascii_digits();
        u32::from(first) | (u32::from(second) << 8) | (u32::from(third) << 16)
    }

    fn key_at(self, index: usize) -> Option<FrontPanelKey> {
        self.digits
            .get(index)
            .copied()
            .and_then(Self::key_for_digit)
    }

    const fn key_for_digit(digit: u8) -> Option<FrontPanelKey> {
        match digit {
            0 => Some(FrontPanelKey::Mark0),
            1 => Some(FrontPanelKey::Vfo1),
            2 => Some(FrontPanelKey::Mr2),
            3 => Some(FrontPanelKey::Call3),
            4 => Some(FrontPanelKey::Msg4),
            5 => Some(FrontPanelKey::List5),
            6 => Some(FrontPanelKey::Beacon6),
            7 => Some(FrontPanelKey::Reverse7),
            8 => Some(FrontPanelKey::Tone8),
            9 => Some(FrontPanelKey::Pf1_9),
            _ => None,
        }
    }
}

impl std::fmt::Display for GuardedDecimalRoute {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let ascii = self.ascii_digits();
        let text = std::str::from_utf8(&ascii).map_err(|_| std::fmt::Error)?;
        formatter.write_str(text)
    }
}

/// Verified input-dispatch phase used by the automation key command.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum KeyPhase {
    /// Initial key-down event.
    Press = 0,
    /// Key-up event.
    Release = 1,
    /// Held-key repeat event.
    Repeat = 2,
}

impl KeyPhase {
    /// Return the exact raw phase value.
    #[must_use]
    pub const fn as_raw(self) -> u8 {
        self as u8
    }
}

impl TryFrom<u8> for KeyPhase {
    type Error = ValidationError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Press),
            1 => Ok(Self::Release),
            2 => Ok(Self::Repeat),
            _ => Err(ValidationError::SettingOutOfRange {
                name: "automation key phase",
                value,
                detail: "must be 0 (press), 1 (release), or 2 (repeat)",
            }),
        }
    }
}

impl From<KeyPhase> for u8 {
    fn from(phase: KeyPhase) -> Self {
        phase.as_raw()
    }
}

/// One validated, stable automation metadata record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutomationMetadata {
    /// Even seqlock value observed before and after the associated data read.
    pub seqlock: u32,
    /// Firmware feature bitmap.
    pub features: u32,
    /// Published frame generation.
    pub generation: u32,
    /// Result of the most recent snapshot command.
    pub capture_result: u32,
    /// Standard reflected IEEE CRC-32 of the raw RGB565LE frame.
    pub crc32: u32,
    /// Number of double-copy attempts used by the most recent capture.
    pub capture_attempts: u32,
    /// Number of key and snapshot commands handled since metadata init.
    pub command_count: u32,
    /// Most recent command identifier (`1` snapshot, `2` key, `3` guarded key,
    /// `4` guarded decimal route).
    pub last_command: u32,
    /// Host sequence echoed by the most recent input or snapshot command.
    pub last_host_sequence: u32,
    /// Raw key identifier recorded by the most recent input command.
    pub last_key: u32,
    /// Raw phase recorded by the most recent input command.
    pub last_phase: u32,
    /// Result recorded by the most recent input command.
    pub last_key_result: u32,
    /// Encoded RLE byte count, or zero when the raw aperture is authoritative.
    pub rle_encoded_length: u32,
    /// Three route ASCII digits packed little-endian, or zero for other commands.
    pub route_ascii: u32,
    /// Number of framebuffer guard comparisons attempted by command 4.
    pub route_guard_count: u32,
    /// Number of complete press/release pairs dispatched by command 4.
    pub route_completed_taps: u32,
    /// Command 4 event mask: bits `2*i`/`2*i+1` are press/release for digit `i`.
    pub route_event_mask: u32,
}

/// A stable LCD frame and the exact firmware metadata that authenticated it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutomationSnapshot {
    /// Canonical top-down 240x180 RGB565LE LCD frame.
    pub frame: ScreenFrame,
    /// Stable metadata whose generation and CRC authenticate `frame`.
    pub metadata: AutomationMetadata,
}

/// Result of one firmware-guarded key press.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum GuardedKeyResult {
    /// The frozen framebuffer still matched and the press/release pair ran.
    Dispatched,
    /// No valid snapshot existed or the live framebuffer differed; firmware
    /// refused the press.
    ContextChanged,
}

/// Receipt for one key in a guarded-input transaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GuardedKeyReceipt {
    /// Key submitted to the firmware guard.
    pub key: FrontPanelKey,
    /// Eight-bit host sequence used by the guarded press.
    pub press_sequence: u8,
    /// Host sequence used by the unconditional release, when a press ran.
    pub release_sequence: Option<u8>,
    /// Firmware decision for the guarded press.
    pub result: GuardedKeyResult,
    /// Exact cumulative command count implied by the ordered acknowledged
    /// exchanges.
    pub command_count: u32,
    /// Exact cumulative even seqlock implied by the ordered acknowledged
    /// exchanges.
    pub seqlock: u32,
}

/// Fully authenticated result of one guarded-input transaction.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum GuardedKeyOutcome {
    /// Every guarded press matched and was paired with a release.
    Dispatched {
        /// Final stable metadata authenticating the aggregate transaction.
        metadata: AutomationMetadata,
        /// One ordered receipt per dispatched key.
        receipts: Vec<GuardedKeyReceipt>,
    },
    /// Firmware refused one press because no valid snapshot existed or its
    /// full-frame comparison observed a changed LCD.
    ContextChanged {
        /// Stable metadata proving command 3, result 2, and exact continuity.
        metadata: AutomationMetadata,
        /// Successful receipts followed by the refused guarded press.
        receipts: Vec<GuardedKeyReceipt>,
    },
    /// The host deadline expired before another press or during a completed tap.
    DeadlineExpired {
        /// Final stable metadata authenticating every completed pair.
        metadata: AutomationMetadata,
        /// Complete press/release pairs sent before the deadline expired.
        receipts: Vec<GuardedKeyReceipt>,
    },
}

impl GuardedKeyOutcome {
    /// Return the final authenticated firmware metadata.
    #[must_use]
    pub const fn metadata(&self) -> &AutomationMetadata {
        match self {
            Self::Dispatched { metadata, .. }
            | Self::ContextChanged { metadata, .. }
            | Self::DeadlineExpired { metadata, .. } => metadata,
        }
    }

    /// Return the ordered per-key receipts.
    #[must_use]
    pub fn receipts(&self) -> &[GuardedKeyReceipt] {
        match self {
            Self::Dispatched { receipts, .. }
            | Self::ContextChanged { receipts, .. }
            | Self::DeadlineExpired { receipts, .. } => receipts,
        }
    }
}

/// Authenticated receipt for one guarded `GM RDDD,SS` transaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuardedDecimalRouteReceipt {
    /// Exact three-digit route requested by the host and echoed by metadata.
    pub route: GuardedDecimalRoute,
    /// Eight-bit host sequence echoed by the wire reply and metadata.
    pub sequence: u8,
    /// Number of framebuffer comparisons attempted, including a failed one.
    pub guard_count: u8,
    /// Number of complete zero-hold press/release pairs dispatched.
    pub completed_taps: u8,
    /// Exact six-bit press/release event mask for the completed prefix.
    pub event_mask: u8,
    /// Final stable metadata authenticating this receipt.
    pub metadata: AutomationMetadata,
}

/// Fully authenticated result of one guarded decimal route command.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum GuardedDecimalRouteOutcome {
    /// All three guarded zero-hold taps were dispatched synchronously.
    Dispatched(GuardedDecimalRouteReceipt),
    /// The framebuffer comparison failed before any digit was dispatched.
    ContextChanged(GuardedDecimalRouteReceipt),
}

impl GuardedDecimalRouteOutcome {
    /// Return the exact command-4 receipt for either semantic outcome.
    #[must_use]
    pub const fn receipt(&self) -> &GuardedDecimalRouteReceipt {
        match self {
            Self::Dispatched(receipt) | Self::ContextChanged(receipt) => receipt,
        }
    }

    /// Return the final authenticated firmware metadata.
    #[must_use]
    pub const fn metadata(&self) -> &AutomationMetadata {
        &self.receipt().metadata
    }

    /// Report whether a completed numeric prefix requires UI recovery.
    ///
    /// ABI 3 only accepts all-or-nothing receipts, so a valid context refusal
    /// never requires recovery.
    #[must_use]
    pub const fn requires_recovery(&self) -> bool {
        matches!(
            self,
            Self::ContextChanged(receipt) if receipt.completed_taps > 0
        )
    }
}

/// Host-side refusal or protocol failure for guarded input.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum GuardedKeyError {
    /// The qualified firmware does not expose the guarded-input ABI.
    #[error("firmware-guarded input requires the exact qualified automation ABI")]
    RequiresGuardedInput,
    /// No unused screen-capture lease exists in this session.
    #[error("guarded input requires a fresh authenticated screen capture")]
    SnapshotUnavailable,
    /// The unavailable-snapshot canary must precede the first successful capture.
    #[error(
        "missing-snapshot canary requires a qualified automation session before its first capture"
    )]
    MissingSnapshotCanaryUnavailable,
    /// The supplied snapshot is not the session's immediately preceding capture.
    #[error("guarded input snapshot does not match the current session receipt")]
    SnapshotReceiptMismatch,
    /// The host held the validated lease too long before guarded input.
    #[error("guarded input snapshot lease was held longer than {max_age:?} after validation")]
    SnapshotExpired {
        /// Maximum accepted age.
        max_age: Duration,
    },
    /// A complete guarded transaction must contain at least one key.
    #[error("guarded input requires at least one key")]
    EmptySequence,
    /// A transaction exceeded the bounded three-key direct-menu route.
    #[error("guarded input requested {actual} keys; maximum is {maximum}")]
    TooManyKeys {
        /// Submitted key count.
        actual: usize,
        /// Maximum accepted key count.
        maximum: usize,
    },
    /// An I/O or strict-protocol failure poisoned the automation session.
    #[error(transparent)]
    Automation(#[from] Error),
}

#[derive(Debug, Clone)]
struct GuardedInputLease {
    metadata: AutomationMetadata,
    validated_at: tokio::time::Instant,
}

enum CaptureAttempt {
    Stable(AutomationSnapshot),
    Unstable(AutomationMetadata),
}

/// Exclusive capability for the exact qualified automation firmware.
pub struct AutomationSession<'a, T: Transport> {
    radio: &'a mut Radio<T>,
    abi: AutomationAbi,
    valid: bool,
    next_key_sequence: u8,
    next_snapshot_sequence: u32,
    last_generation: u32,
    last_command_count: u32,
    last_seqlock: u32,
    guarded_input_lease: Option<GuardedInputLease>,
}

impl<T: Transport> std::fmt::Debug for AutomationSession<'_, T> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AutomationSession")
            .field("abi", &self.abi)
            .field("valid", &self.valid)
            .field("next_key_sequence", &self.next_key_sequence)
            .field("next_snapshot_sequence", &self.next_snapshot_sequence)
            .field("last_generation", &self.last_generation)
            .field("last_command_count", &self.last_command_count)
            .field("last_seqlock", &self.last_seqlock)
            .field(
                "has_guarded_input_lease",
                &self.guarded_input_lease.is_some(),
            )
            .finish_non_exhaustive()
    }
}

impl<T: Transport> AutomationSession<'_, T> {
    const fn abi_supports_guarded_input(abi: AutomationAbi) -> bool {
        abi.version == EXPECTED_ABI.version
            && abi.features == EXPECTED_ABI.features
            && abi.max_key == EXPECTED_ABI.max_key
            && abi.max_phase == EXPECTED_ABI.max_phase
    }

    /// Return the exact ABI proved during qualification.
    #[must_use]
    pub const fn abi(&self) -> AutomationAbi {
        self.abi
    }

    /// Report whether no prior operation failed, was cancelled, or left a
    /// refused nonempty numeric prefix requiring recovery.
    #[must_use]
    pub const fn is_valid(&self) -> bool {
        self.valid
    }

    /// Read the exact CAT serial identity and complete live TN state while
    /// retaining this already-attested automation session.
    ///
    /// This is the post-reboot proof used before one approved APRS retry. It
    /// performs no setting write and no mode transition. An unresolved CAT
    /// boundary invalidates the session but does not invent strict-GM poison.
    ///
    /// # Errors
    ///
    /// Returns the exact CAT identity or TN read failure. The caller must
    /// validate both mode and band before authorizing a transition.
    #[cfg(feature = "aprs")]
    pub async fn aprs_recovery_state(&mut self) -> Result<(SerialNumber, TncState), Error> {
        self.require_valid()?;
        let result = async {
            let serial = self.radio.get_serial_information().await?.into_parts().0;
            let state = self.radio.get_tnc_mode().await?;
            Ok((serial, state))
        }
        .await;
        if result.as_ref().is_err_and(Error::requires_recovery)
            || self.radio.cat_recovery_required()
        {
            self.valid = false;
        }
        result
    }

    /// Perform the single `TN 2,x` transition while retaining this attested
    /// session on a correlated semantic refusal.
    ///
    /// Success means the transport has already changed to KISS and invalidates
    /// this CAT automation capability. Drop the session and consume the same
    /// radio through [`Radio::into_kiss_session`](crate::radio::Radio::into_kiss_session);
    /// the protocol-specific proof is stored on that radio and the ownership
    /// conversion sends no second `TN` command. A correlated `N` or `?` leaves
    /// both CAT and this attested session ready, so the caller can report the
    /// refusal without packet recovery or firmware requalification.
    ///
    /// # Errors
    ///
    /// Returns the exact CAT transition error. An unresolved boundary poisons
    /// this session, while an aligned semantic refusal keeps it valid.
    #[cfg(feature = "aprs")]
    pub async fn transition_to_kiss(&mut self, data_band: TncDataBand) -> Result<(), Error> {
        self.require_valid()?;
        self.guarded_input_lease = None;
        let result = self.radio.transition_to_kiss(data_band).await;
        match &result {
            Ok(()) => {
                // CAT no longer exists on this transport. Do not set the GM
                // poison: the already-proved binary boundary must remain
                // consumable by `into_kiss_session`.
                self.valid = false;
            }
            Err(error) if error.requires_recovery() || self.radio.cat_recovery_required() => {
                // `Radio::execute` already recorded the exact CAT recovery
                // obligation. This is not a failed strict-GM exchange, so do
                // not invent GM poison that would make packet-mode recovery
                // unreachable.
                self.valid = false;
                self.guarded_input_lease = None;
            }
            Err(_) => {
                // `execute` proved an aligned `N`/`?` terminal and restored
                // CAT readiness. The firmware attestation remains current.
            }
        }
        result
    }

    /// Configure the typed USB IF tap without discarding this already-attested
    /// automation session.
    ///
    /// Every affected radio value is snapshotted and independently verified by
    /// the IF-tap implementation. Any authenticated screen lease is revoked
    /// before ordinary CAT changes the visible radio state. A semantic refusal
    /// with exact rollback keeps the session usable and does not repeat the
    /// multi-kilobyte firmware attestation. Any timeout, malformed boundary,
    /// or link failure which requires CAT recovery poisons the session.
    ///
    /// # Errors
    ///
    /// Returns [`IfTapEnterError`] with the exact setup failure and verified
    /// rollback report. Recovery-required failures invalidate this automation
    /// session before returning.
    pub async fn enter_if_tap(
        &mut self,
        config: IfTapConfig,
    ) -> Result<IfTapSavedState, IfTapEnterError> {
        self.guarded_input_lease = None;
        let result = self.radio.enter_if_tap(config).await;
        match result {
            Ok(session) => Ok(session.into_saved_state()),
            Err(error) => {
                let recovery_required = error.source.requires_recovery()
                    || error
                        .rollback
                        .failures()
                        .iter()
                        .any(|(_, failure)| failure.requires_recovery())
                    || self.radio.cat_recovery_required();
                if recovery_required {
                    self.poison_after_external_recovery_requirement();
                }
                Err(error)
            }
        }
    }

    /// Retune a live IF tap through the verified bounded step walk while
    /// retaining the existing automation attestation.
    ///
    /// # Errors
    ///
    /// Returns the typed CAT, tuning-bound, or readback failure. A failure
    /// which leaves CAT recovery required invalidates this automation session.
    pub async fn retune_if_tap(
        &mut self,
        saved: &IfTapSavedState,
        target: Frequency,
        output: UsbAudioOutput,
    ) -> Result<Frequency, Error> {
        self.guarded_input_lease = None;
        let result = self.radio.retune_if_tap(saved, target, output).await;
        if result.as_ref().is_err_and(Error::requires_recovery)
            || self.radio.cat_recovery_required()
        {
            self.poison_after_external_recovery_requirement();
        }
        result
    }

    /// Restore a saved IF-tap state with per-field readback verification while
    /// retaining the existing automation attestation when the CAT link stays
    /// synchronized.
    pub async fn restore_if_tap(&mut self, saved: IfTapSavedState) -> IfTapRestoreReport {
        self.guarded_input_lease = None;
        let report = self.radio.restore_if_tap(saved).await;
        if report
            .failures()
            .iter()
            .any(|(_, failure)| failure.requires_recovery())
            || self.radio.cat_recovery_required()
        {
            self.poison_after_external_recovery_requirement();
        }
        report
    }

    const fn poison_after_external_recovery_requirement(&mut self) {
        self.valid = false;
        self.radio.gm_poisoned = true;
        self.radio.desynced = true;
    }

    /// Dispatch one verified key phase.
    ///
    /// A standalone [`KeyPhase::Press`] must eventually be paired with a
    /// [`KeyPhase::Release`] for the same key.  Prefer [`Self::tap_key`] for
    /// ordinary button activation because it keeps press, hold, and release
    /// inside one fail-closed poisoned operation.
    ///
    /// # Errors
    ///
    /// Returns an error and permanently poisons this session if the command,
    /// exact echoed ACK, or post-command metadata cannot be fully validated.
    pub async fn key_event(
        &mut self,
        key: FrontPanelKey,
        phase: KeyPhase,
    ) -> Result<AutomationMetadata, Error> {
        self.require_valid()?;
        let sequence = self.next_key_sequence;
        let expected_command_count = self.last_command_count.wrapping_add(1);
        let expected_seqlock = self.last_seqlock.wrapping_add(2);
        self.begin_operation();
        let result = self
            .key_event_inner(
                key,
                phase,
                sequence,
                expected_command_count,
                expected_seqlock,
            )
            .await;
        if let Ok(metadata) = &result {
            self.next_key_sequence = sequence.wrapping_add(1);
            self.accept_metadata(metadata);
            self.finish_operation();
        }
        result
    }

    /// Press a key, hold it for 40 ms after the exact press ACK, and release it
    /// as one operation.
    ///
    /// No metadata transfer occurs during the hold.  The exact release ACK and
    /// one stable post-release record must prove an aggregate command-count
    /// advance of two and seqlock advance of four, so metadata-read latency
    /// cannot lengthen the requested hold and the wrapped eight-bit phase
    /// sequences remain correlated to one unique firmware transaction pair.
    ///
    /// Cancellation at any point leaves the strict stream poisoned.  It can
    /// also leave the physical key logically pressed; reconnect and explicitly
    /// recover the UI instead of assuming cancellation sent the release.
    ///
    /// # Errors
    ///
    /// Returns an error and permanently poisons this session unless both exact
    /// phase ACKs and their aggregate post-release metadata are fully validated.
    pub async fn tap_key(&mut self, key: FrontPanelKey) -> Result<AutomationMetadata, Error> {
        self.require_valid()?;
        let press_sequence = self.next_key_sequence;
        let release_sequence = press_sequence.wrapping_add(1);
        let expected_command_count = self.last_command_count.wrapping_add(2);
        let expected_seqlock = self.last_seqlock.wrapping_add(4);
        self.begin_operation();
        let result = async {
            self.send_key_command(key, KeyPhase::Press, press_sequence)
                .await?;
            tokio::time::sleep(TAP_HOLD).await;
            self.send_key_command(key, KeyPhase::Release, release_sequence)
                .await?;

            let (raw, metadata) = self.read_stable_metadata().await?;
            Self::validate_key_metadata(
                &raw,
                &metadata,
                key,
                KeyPhase::Release,
                release_sequence,
                expected_command_count,
                expected_seqlock,
            )?;
            if metadata.generation != self.last_generation {
                return Err(Radio::<T>::strict_protocol_error(
                    "a key tap that does not alter the published frame generation",
                    metadata.generation.to_le_bytes().to_vec(),
                ));
            }
            Ok(metadata)
        }
        .await;
        if let Ok(metadata) = &result {
            self.next_key_sequence = release_sequence.wrapping_add(1);
            self.accept_metadata(metadata);
            self.finish_operation();
        }
        result
    }

    /// Prove that guarded input is refused when firmware has no valid snapshot.
    ///
    /// This qualification canary is deliberately separate from
    /// [`Self::guarded_tap_key`]: ordinary callers must never bypass its fresh
    /// host-side snapshot lease. The canary first proves stable metadata with
    /// `capture_result == 1`, then submits exactly one guarded press. Status
    /// `02`, command 3/result 2 metadata, exact count/seqlock continuity, and
    /// the absence of a release receipt jointly authenticate the refusal. The
    /// exact runtime proved when this session was created establishes that
    /// status `02` returned without calling the stock input dispatcher.
    ///
    /// Call this immediately after [`Radio::qualify_automation`], before the
    /// first screen capture. The ABI query revokes any snapshot
    /// retained from an earlier session, so this proof remains repeatable
    /// without a radio reboot. If the guard unexpectedly dispatches, the
    /// method still releases the key before returning a poisoned-session
    /// protocol error.
    ///
    /// # Errors
    ///
    /// Requires an exact qualified session with no host snapshot lease and
    /// stable firmware metadata proving that no valid snapshot exists. Any I/O
    /// or protocol failure permanently poisons the session.
    pub async fn verify_missing_snapshot_refusal(
        &mut self,
        key: FrontPanelKey,
    ) -> Result<GuardedKeyOutcome, GuardedKeyError> {
        self.require_valid()?;
        if !Self::abi_supports_guarded_input(self.abi) {
            return Err(GuardedKeyError::RequiresGuardedInput);
        }
        if self.guarded_input_lease.is_some() {
            return Err(GuardedKeyError::MissingSnapshotCanaryUnavailable);
        }

        let base_command_count = self.last_command_count;
        let base_seqlock = self.last_seqlock;
        let press_sequence = self.next_key_sequence;
        self.begin_operation();
        let result = async {
            let (before_raw, before) = self.read_stable_metadata().await?;
            if before.command_count != base_command_count
                || before.seqlock != base_seqlock
                || before.generation != self.last_generation
                || before.capture_result != RESULT_UNSTABLE
                || before.rle_encoded_length != 0
            {
                return Err(Radio::<T>::strict_protocol_error(
                    "stable guarded-automation metadata proving no valid firmware snapshot",
                    before_raw,
                ));
            }

            let status = self.send_guarded_press(key, press_sequence).await?;
            if status == GUARDED_STATUS_OK {
                // A successful guarded press is logically down. Always release
                // it before reporting that the fail-closed canary failed.
                tokio::time::sleep(TAP_HOLD).await;
                let release_sequence = press_sequence.wrapping_add(1);
                self.send_key_command(key, KeyPhase::Release, release_sequence)
                    .await?;
                let (raw, metadata) = self.read_stable_metadata().await?;
                Self::validate_key_metadata(
                    &raw,
                    &metadata,
                    key,
                    KeyPhase::Release,
                    release_sequence,
                    base_command_count.wrapping_add(2),
                    base_seqlock.wrapping_add(4),
                )?;
                return Err(Radio::<T>::strict_protocol_error(
                    "a missing-snapshot guarded refusal (status 02), not a dispatch",
                    raw,
                ));
            }

            let expected_command_count = base_command_count.wrapping_add(1);
            let expected_seqlock = base_seqlock.wrapping_add(2);
            let (raw, metadata) = self.read_stable_metadata().await?;
            Self::validate_guarded_context_changed_metadata(
                &raw,
                &metadata,
                key,
                press_sequence,
                expected_command_count,
                expected_seqlock,
            )?;
            Self::validate_guarded_snapshot_state_unchanged(&raw, &metadata, &before)?;
            self.next_key_sequence = press_sequence.wrapping_add(1);
            Ok(GuardedKeyOutcome::ContextChanged {
                metadata,
                receipts: vec![GuardedKeyReceipt {
                    key,
                    press_sequence,
                    release_sequence: None,
                    result: GuardedKeyResult::ContextChanged,
                    command_count: expected_command_count,
                    seqlock: expected_seqlock,
                }],
            })
        }
        .await;
        if let Ok(outcome) = &result {
            self.accept_metadata(outcome.metadata());
            self.finish_operation();
        }
        result.map_err(GuardedKeyError::from)
    }

    /// Prove that guarded input is refused after a deliberate UI change.
    ///
    /// The supplied snapshot must be the session's fresh, immediately
    /// preceding capture. This canary sends one unconditional press/release
    /// pair with `context_change_key`, then sends one `GM G` press for
    /// `guarded_probe_key` after a fixed 140 ms redraw interval and without
    /// taking another snapshot. An authenticated status `02` and command
    /// 3/result 2 receipt prove that the frozen and live framebuffers differed
    /// and that the guarded probe was not dispatched.
    /// The caller must capture the resulting screen and independently prove the
    /// intended context change and restoration.
    ///
    /// If the guarded probe unexpectedly dispatches, its release is still sent
    /// before a poisoned-session protocol error is returned.
    ///
    /// # Errors
    ///
    /// Host-side snapshot failures perform no I/O. Any I/O, malformed receipt,
    /// unexpected dispatch, or metadata discontinuity permanently poisons the
    /// session.
    pub async fn verify_changed_context_refusal(
        &mut self,
        snapshot: &AutomationSnapshot,
        context_change_key: FrontPanelKey,
        guarded_probe_key: FrontPanelKey,
    ) -> Result<GuardedKeyOutcome, GuardedKeyError> {
        self.require_valid()?;
        if !Self::abi_supports_guarded_input(self.abi) {
            return Err(GuardedKeyError::RequiresGuardedInput);
        }
        let lease = self.validate_guarded_snapshot(snapshot)?;
        let base_command_count = self.last_command_count;
        let base_seqlock = self.last_seqlock;
        let change_press_sequence = self.next_key_sequence;
        let change_release_sequence = change_press_sequence.wrapping_add(1);
        let guarded_press_sequence = change_release_sequence.wrapping_add(1);
        self.begin_operation();
        let result = async {
            self.send_key_command(context_change_key, KeyPhase::Press, change_press_sequence)
                .await?;
            tokio::time::sleep(TAP_HOLD).await;
            self.send_key_command(
                context_change_key,
                KeyPhase::Release,
                change_release_sequence,
            )
            .await?;
            tokio::time::sleep(CHANGED_CONTEXT_CANARY_SETTLE).await;

            let guarded_deadline = tokio::time::Instant::now() + GUARDED_ROUTE_MAX_DURATION;
            let status = tokio::time::timeout_at(
                guarded_deadline,
                self.send_guarded_press(guarded_probe_key, guarded_press_sequence),
            )
            .await
            .map_err(|_elapsed| Error::Timeout(GUARDED_ROUTE_MAX_DURATION))??;
            if status == GUARDED_STATUS_OK {
                // A successful guarded press is logically down. Always release
                // it before reporting that the changed-context canary failed.
                tokio::time::sleep(TAP_HOLD).await;
                let guarded_release_sequence = guarded_press_sequence.wrapping_add(1);
                self.send_key_command(
                    guarded_probe_key,
                    KeyPhase::Release,
                    guarded_release_sequence,
                )
                .await?;
                let (raw, metadata) = self.read_stable_metadata().await?;
                Self::validate_key_metadata(
                    &raw,
                    &metadata,
                    guarded_probe_key,
                    KeyPhase::Release,
                    guarded_release_sequence,
                    base_command_count.wrapping_add(4),
                    base_seqlock.wrapping_add(8),
                )?;
                Self::validate_guarded_frame_receipt(&raw, &metadata, &lease)?;
                return Err(Radio::<T>::strict_protocol_error(
                    "a changed-context guarded refusal (status 02), not a dispatch",
                    raw,
                ));
            }

            let expected_command_count = base_command_count.wrapping_add(3);
            let expected_seqlock = base_seqlock.wrapping_add(6);
            let (raw, metadata) = self.read_stable_metadata().await?;
            Self::validate_guarded_context_changed_metadata(
                &raw,
                &metadata,
                guarded_probe_key,
                guarded_press_sequence,
                expected_command_count,
                expected_seqlock,
            )?;
            Self::validate_guarded_frame_receipt(&raw, &metadata, &lease)?;
            self.next_key_sequence = guarded_press_sequence.wrapping_add(1);
            Ok(GuardedKeyOutcome::ContextChanged {
                metadata,
                receipts: vec![GuardedKeyReceipt {
                    key: guarded_probe_key,
                    press_sequence: guarded_press_sequence,
                    release_sequence: None,
                    result: GuardedKeyResult::ContextChanged,
                    command_count: expected_command_count,
                    seqlock: expected_seqlock,
                }],
            })
        }
        .await;
        if let Ok(outcome) = &result {
            self.accept_metadata(outcome.metadata());
            self.finish_operation();
        }
        result.map_err(GuardedKeyError::from)
    }

    /// Prove that a complete decimal route is refused after a deliberate UI change.
    ///
    /// The supplied snapshot must be the session's fresh, immediately preceding
    /// capture. This canary sends one unconditional press/release pair with
    /// `context_change_key`, waits for the fixed redraw interval, and then sends
    /// one `GM RDDD,SS` transaction against the now-stale snapshot. It succeeds
    /// only for an authenticated command-4 status `02` receipt with one failed
    /// guard, zero completed taps, and no dispatched events. A zero-prefix
    /// refusal leaves no hidden numeric input in the radio and keeps the session
    /// usable.
    ///
    /// A status `00` or an authenticated nonempty prefix is treated as a canary
    /// failure. The typed receipt is validated first, but the session and strict
    /// GM stream remain poisoned because the radio's resulting UI/parser state
    /// is no longer known.
    ///
    /// # Errors
    ///
    /// Host-side snapshot failures perform no I/O. Any I/O, malformed receipt,
    /// unexpected dispatch, nonempty prefix, or metadata discontinuity
    /// permanently poisons the session.
    ///
    /// ABI 3 accepts only the all-or-nothing route receipt: one failed guard,
    /// no completed tap, and a zero event mask. A partial-prefix receipt is a
    /// protocol failure.
    ///
    /// # Errors
    ///
    /// Requires an exact session and its fresh immediately preceding
    /// snapshot. Any I/O, partial receipt, or protocol failure permanently
    /// poisons the session.
    pub async fn verify_decimal_route_changed_context_refusal(
        &mut self,
        snapshot: &AutomationSnapshot,
        context_change_key: FrontPanelKey,
        route: GuardedDecimalRoute,
    ) -> Result<GuardedDecimalRouteOutcome, GuardedKeyError> {
        self.require_valid()?;
        if !Self::abi_supports_guarded_input(self.abi) {
            return Err(GuardedKeyError::RequiresGuardedInput);
        }
        let lease = self.validate_guarded_snapshot(snapshot)?;
        let base_command_count = self.last_command_count;
        let base_seqlock = self.last_seqlock;
        let change_press_sequence = self.next_key_sequence;
        let change_release_sequence = change_press_sequence.wrapping_add(1);
        let route_sequence = change_release_sequence.wrapping_add(1);
        self.begin_operation();
        let result = async {
            self.send_key_command(context_change_key, KeyPhase::Press, change_press_sequence)
                .await?;
            tokio::time::sleep(TAP_HOLD).await;
            self.send_key_command(
                context_change_key,
                KeyPhase::Release,
                change_release_sequence,
            )
            .await?;
            tokio::time::sleep(CHANGED_CONTEXT_CANARY_SETTLE).await;

            let status = tokio::time::timeout(
                GUARDED_ROUTE_MAX_DURATION,
                self.send_guarded_decimal_route(route, route_sequence),
            )
            .await
            .map_err(|_elapsed| Error::Timeout(GUARDED_ROUTE_MAX_DURATION))??;
            let expected_command_count = base_command_count.wrapping_add(3);
            let expected_seqlock = base_seqlock.wrapping_add(6);
            let (raw, metadata) = self.read_stable_metadata().await?;
            let receipt = Self::validate_guarded_decimal_route_metadata(
                &raw,
                metadata,
                &lease,
                route,
                route_sequence,
                status,
                expected_command_count,
                expected_seqlock,
            )?;
            if status != 2
                || receipt.guard_count != 1
                || receipt.completed_taps != 0
                || receipt.event_mask != 0
            {
                return Err(Radio::<T>::strict_protocol_error(
                    "a changed-context decimal-route refusal before its first digit",
                    raw,
                ));
            }
            self.next_key_sequence = route_sequence.wrapping_add(1);
            Ok(GuardedDecimalRouteOutcome::ContextChanged(receipt))
        }
        .await;
        if let Ok(outcome) = &result {
            self.accept_metadata(outcome.metadata());
            self.finish_operation();
        }
        result.map_err(GuardedKeyError::from)
    }

    /// Dispatch one firmware-guarded press/release pair.
    ///
    /// This is the one-key convenience form of [`Self::guarded_tap_keys`]. The
    /// supplied snapshot must be the session's fresh, immediately preceding
    /// authenticated capture and is consumed by this call.
    ///
    /// # Errors
    ///
    /// Host-side capability failures perform no I/O. Any I/O or malformed
    /// receipt permanently poisons the session.
    pub async fn guarded_tap_key(
        &mut self,
        snapshot: &AutomationSnapshot,
        key: FrontPanelKey,
    ) -> Result<GuardedKeyOutcome, GuardedKeyError> {
        self.guarded_tap_keys(snapshot, &[key]).await
    }

    /// Dispatch one complete three-digit route in one firmware call.
    ///
    /// `GM RDDD,SS` consumes the session's immediately preceding fresh screen
    /// snapshot. The firmware compares all 21,600 live 32-bit framebuffer
    /// words (43,200 RGB565 pixels / 86,400 bytes) once before any input, then
    /// synchronously emits all three zero-hold
    /// press/release pairs in the same transaction so the stock numeric-entry
    /// redraw after digit one cannot invalidate the original Menu guard. The
    /// single wire reply and one double-read stable metadata record authenticate
    /// either all six events or a zero-input refusal. Command count
    /// advances by one and seqlock by two for the whole route.
    ///
    /// Zero-hold input is a separate physical behavior from the established
    /// 40 ms host-driven tap path. A deployment must not treat it as qualified
    /// until a live harmless-route canary has proved the expected screen and
    /// exact restoration on that hardware/firmware combination. The firmware
    /// removes host OCR, filesystem, and transport gaps between digits, but a
    /// concurrent framebuffer writer can still change a word after it was
    /// compared and before synchronous dispatch; that residual TOCTOU remains.
    /// An authenticated context refusal before the first digit leaves the
    /// session usable. ABI 3 admits no partial-refusal receipt. No timeout is
    /// assumed, and the command is not retried.
    ///
    /// # Errors
    ///
    /// Host-side capability failures perform no I/O. Once the single route
    /// exchange begins, timeout, cancellation, malformed echo/status, or any
    /// metadata discontinuity permanently poisons the session because physical
    /// dispatch may already have occurred. The command is never retried.
    pub async fn guarded_decimal_route(
        &mut self,
        snapshot: &AutomationSnapshot,
        route: GuardedDecimalRoute,
    ) -> Result<GuardedDecimalRouteOutcome, GuardedKeyError> {
        self.require_valid()?;
        if !Self::abi_supports_guarded_input(self.abi) {
            return Err(GuardedKeyError::RequiresGuardedInput);
        }
        let lease = self.validate_guarded_snapshot(snapshot)?;
        let sequence = self.next_key_sequence;
        let expected_command_count = self.last_command_count.wrapping_add(1);
        let expected_seqlock = self.last_seqlock.wrapping_add(2);
        self.begin_operation();
        let result = async {
            let status = tokio::time::timeout(
                GUARDED_ROUTE_MAX_DURATION,
                self.send_guarded_decimal_route(route, sequence),
            )
            .await
            .map_err(|_elapsed| Error::Timeout(GUARDED_ROUTE_MAX_DURATION))??;
            let (raw, metadata) = self.read_stable_metadata().await?;
            let receipt = Self::validate_guarded_decimal_route_metadata(
                &raw,
                metadata,
                &lease,
                route,
                sequence,
                status,
                expected_command_count,
                expected_seqlock,
            )?;
            self.next_key_sequence = sequence.wrapping_add(1);
            match status {
                GUARDED_STATUS_OK => Ok(GuardedDecimalRouteOutcome::Dispatched(receipt)),
                2 => Ok(GuardedDecimalRouteOutcome::ContextChanged(receipt)),
                _ => Err(Radio::<T>::strict_protocol_error(
                    "a parsed guarded-route status",
                    [status].to_vec(),
                )),
            }
        }
        .await;
        if let Ok(outcome) = &result {
            self.accept_metadata(outcome.metadata());
            if !outcome.requires_recovery() {
                self.finish_operation();
            }
        }
        result.map_err(GuardedKeyError::from)
    }

    /// Dispatch one complete guarded-input transaction of up to three keys.
    ///
    /// The transaction consumes exactly one fresh [`AutomationSnapshot`]. Each
    /// press uses `GM G`, which compares the live framebuffer byte-for-byte
    /// with that frozen snapshot before synchronous dispatch. A
    /// matching press is always followed by an unconditional `GM K` release.
    /// No metadata transfer, capture, OCR, or filesystem work occurs between
    /// keys. One final stable metadata record authenticates the aggregate
    /// command-count and seqlock advance.
    ///
    /// The snapshot may be cloned, but session receipt continuity makes every
    /// clone unusable after this call starts. One absolute 1.5-second deadline
    /// bounds every guarded-press exchange, including an only or final key. If
    /// an unacknowledged `GM G` reaches it, cancellation makes the dispatch
    /// state unknowable and poisons the session. Once `GM G` acknowledges a
    /// dispatched press, its `GM K` release is always attempted; if that tap
    /// completes after the deadline, no next press is sent and
    /// [`GuardedKeyOutcome::DeadlineExpired`] authenticates the completed
    /// prefix. A firmware context mismatch sends no release because firmware
    /// sent no press; command 3/result 2 metadata proves that refusal. Both
    /// authenticated semantic outcomes leave the session usable but require a
    /// new capture before any further guarded input. This removes the host-side
    /// check-to-command gap, but a concurrent framebuffer writer can still race
    /// the comparison itself and change an already-compared word before input
    /// dispatch.
    ///
    /// Cancellation or a protocol/I/O failure poisons the session. Cancellation
    /// after a successful press can prevent its release; reconnect and recover
    /// the UI rather than assuming the key is no longer logically held.
    ///
    /// # Errors
    ///
    /// Host-side validation errors are returned before any I/O. Once I/O begins,
    /// any error permanently poisons this session and its strict GM stream.
    pub async fn guarded_tap_keys(
        &mut self,
        snapshot: &AutomationSnapshot,
        keys: &[FrontPanelKey],
    ) -> Result<GuardedKeyOutcome, GuardedKeyError> {
        self.require_valid()?;
        if !Self::abi_supports_guarded_input(self.abi) {
            return Err(GuardedKeyError::RequiresGuardedInput);
        }
        if keys.is_empty() {
            return Err(GuardedKeyError::EmptySequence);
        }
        if keys.len() > GUARDED_INPUT_MAX_TAPS {
            return Err(GuardedKeyError::TooManyKeys {
                actual: keys.len(),
                maximum: GUARDED_INPUT_MAX_TAPS,
            });
        }

        let lease = self.validate_guarded_snapshot(snapshot)?;
        self.begin_operation();
        let route_deadline = tokio::time::Instant::now() + GUARDED_ROUTE_MAX_DURATION;
        let result = self
            .guarded_tap_keys_inner(&lease, route_deadline, keys)
            .await;
        if let Ok(outcome) = &result {
            self.accept_metadata(outcome.metadata());
            self.finish_operation();
        }
        result.map_err(GuardedKeyError::from)
    }

    async fn guarded_tap_keys_inner(
        &mut self,
        lease: &GuardedInputLease,
        route_deadline: tokio::time::Instant,
        keys: &[FrontPanelKey],
    ) -> Result<GuardedKeyOutcome, Error> {
        let base_command_count = self.last_command_count;
        let base_seqlock = self.last_seqlock;
        let mut sequence = self.next_key_sequence;
        let mut receipts: Vec<GuardedKeyReceipt> = Vec::with_capacity(keys.len());

        for (index, &key) in keys.iter().enumerate() {
            let press_sequence = sequence;
            let status = tokio::time::timeout_at(
                route_deadline,
                self.send_guarded_press(key, press_sequence),
            )
            .await
            .map_err(|_elapsed| Error::Timeout(GUARDED_ROUTE_MAX_DURATION))??;
            if status == 2 {
                let completed = u32::try_from(receipts.len()).map_err(|_| {
                    Radio::<T>::strict_protocol_error(
                        "a host-addressable guarded receipt count",
                        receipts.len().to_string().into_bytes(),
                    )
                })?;
                let command_delta = completed.wrapping_mul(2).wrapping_add(1);
                let expected_command_count = base_command_count.wrapping_add(command_delta);
                let expected_seqlock = base_seqlock.wrapping_add(command_delta.wrapping_mul(2));
                let (raw, metadata) = self.read_stable_metadata().await?;
                Self::validate_guarded_context_changed_metadata(
                    &raw,
                    &metadata,
                    key,
                    press_sequence,
                    expected_command_count,
                    expected_seqlock,
                )?;
                Self::validate_guarded_frame_receipt(&raw, &metadata, lease)?;
                receipts.push(GuardedKeyReceipt {
                    key,
                    press_sequence,
                    release_sequence: None,
                    result: GuardedKeyResult::ContextChanged,
                    command_count: expected_command_count,
                    seqlock: expected_seqlock,
                });
                self.next_key_sequence = press_sequence.wrapping_add(1);
                return Ok(GuardedKeyOutcome::ContextChanged { metadata, receipts });
            }

            tokio::time::sleep(TAP_HOLD).await;
            let release_sequence = press_sequence.wrapping_add(1);
            self.send_key_command(key, KeyPhase::Release, release_sequence)
                .await?;
            sequence = release_sequence.wrapping_add(1);
            let completed = u32::try_from(index + 1).map_err(|_| {
                Radio::<T>::strict_protocol_error(
                    "a host-addressable guarded receipt count",
                    (index + 1).to_string().into_bytes(),
                )
            })?;
            receipts.push(GuardedKeyReceipt {
                key,
                press_sequence,
                release_sequence: Some(release_sequence),
                result: GuardedKeyResult::Dispatched,
                command_count: base_command_count.wrapping_add(completed.wrapping_mul(2)),
                seqlock: base_seqlock.wrapping_add(completed.wrapping_mul(4)),
            });

            if tokio::time::Instant::now() >= route_deadline {
                return self
                    .authenticate_guarded_deadline(lease, sequence, receipts)
                    .await;
            }
        }

        let last = receipts.last().ok_or_else(|| {
            Radio::<T>::strict_protocol_error("one guarded key receipt", Vec::new())
        })?;
        let release_sequence = last.release_sequence.ok_or_else(|| {
            Radio::<T>::strict_protocol_error("one guarded release receipt", Vec::new())
        })?;
        let (raw, metadata) = self.read_stable_metadata().await?;
        Self::validate_key_metadata(
            &raw,
            &metadata,
            last.key,
            KeyPhase::Release,
            release_sequence,
            last.command_count,
            last.seqlock,
        )?;
        Self::validate_guarded_frame_receipt(&raw, &metadata, lease)?;
        self.next_key_sequence = sequence;
        Ok(GuardedKeyOutcome::Dispatched { metadata, receipts })
    }

    async fn authenticate_guarded_deadline(
        &mut self,
        lease: &GuardedInputLease,
        next_sequence: u8,
        receipts: Vec<GuardedKeyReceipt>,
    ) -> Result<GuardedKeyOutcome, Error> {
        let last = receipts.last().ok_or_else(|| {
            Radio::<T>::strict_protocol_error(
                "at least one complete guarded tap before route expiry",
                Vec::new(),
            )
        })?;
        let release_sequence = last.release_sequence.ok_or_else(|| {
            Radio::<T>::strict_protocol_error("a release receipt before route expiry", Vec::new())
        })?;
        let (raw, metadata) = self.read_stable_metadata().await?;
        Self::validate_key_metadata(
            &raw,
            &metadata,
            last.key,
            KeyPhase::Release,
            release_sequence,
            last.command_count,
            last.seqlock,
        )?;
        Self::validate_guarded_frame_receipt(&raw, &metadata, lease)?;
        self.next_key_sequence = next_sequence;
        Ok(GuardedKeyOutcome::DeadlineExpired { metadata, receipts })
    }

    fn validate_guarded_snapshot(
        &mut self,
        snapshot: &AutomationSnapshot,
    ) -> Result<GuardedInputLease, GuardedKeyError> {
        let lease = self
            .guarded_input_lease
            .as_ref()
            .ok_or(GuardedKeyError::SnapshotUnavailable)?;
        if snapshot.metadata != lease.metadata
            || snapshot.metadata.generation != self.last_generation
            || snapshot.metadata.command_count != self.last_command_count
            || snapshot.metadata.seqlock != self.last_seqlock
            || snapshot.frame.crc32() != snapshot.metadata.crc32
        {
            return Err(GuardedKeyError::SnapshotReceiptMismatch);
        }
        if lease.validated_at.elapsed() > GUARDED_SNAPSHOT_MAX_AGE {
            self.guarded_input_lease = None;
            return Err(GuardedKeyError::SnapshotExpired {
                max_age: GUARDED_SNAPSHOT_MAX_AGE,
            });
        }
        Ok(lease.clone())
    }

    async fn send_guarded_press(&mut self, key: FrontPanelKey, sequence: u8) -> Result<u8, Error> {
        let request = format!("GM G{:02X},0{sequence:02X}\r", key.as_raw());
        let reply = self
            .radio
            .strict_cat_exchange(request.as_bytes(), 13)
            .await?;
        Self::parse_guarded_status(request.as_bytes(), &reply)
    }

    async fn send_guarded_decimal_route(
        &mut self,
        route: GuardedDecimalRoute,
        sequence: u8,
    ) -> Result<u8, Error> {
        let request = format!("GM R{route},{sequence:02X}\r");
        let reply = self
            .radio
            .strict_cat_exchange(request.as_bytes(), 13)
            .await?;
        Self::parse_guarded_status(request.as_bytes(), &reply)
    }

    /// Capture and independently validate one stable LCD frame.
    ///
    /// The firmware command must succeed, metadata must describe the exact
    /// aperture, advance command count and seqlock exactly once, and advance by
    /// one generation. Metadata reads bracketing the pixel transfer must be
    /// byte-identical, and the host-computed raw-frame CRC must match the
    /// published CRC.
    ///
    /// # Errors
    ///
    /// Three fully validated firmware `unstable` results return
    /// [`Error::AutomationScreenUnstable`] without poisoning, so a caller
    /// may try again. Malformed replies and all metadata, aperture, frame, or
    /// CRC failures permanently poison the session.
    pub async fn capture_screen(&mut self) -> Result<AutomationSnapshot, Error> {
        self.require_valid()?;
        let mut sequence = self.next_snapshot_sequence & 0x00FF_FFFF;
        self.begin_operation();
        for host_attempt in 1..=MAX_HOST_CAPTURE_ATTEMPTS {
            let expected_command_count = self.last_command_count.wrapping_add(1);
            let expected_seqlock = self.last_seqlock.wrapping_add(2);
            match self
                .capture_screen_once(sequence, expected_command_count, expected_seqlock)
                .await
            {
                Ok(CaptureAttempt::Stable(snapshot)) => {
                    self.next_snapshot_sequence = sequence.wrapping_add(1) & 0x00FF_FFFF;
                    self.last_generation = snapshot.metadata.generation;
                    self.accept_metadata(&snapshot.metadata);
                    self.finish_operation();
                    if Self::abi_supports_guarded_input(self.abi) {
                        self.guarded_input_lease = Some(GuardedInputLease {
                            metadata: snapshot.metadata.clone(),
                            validated_at: tokio::time::Instant::now(),
                        });
                    }
                    return Ok(snapshot);
                }
                Ok(CaptureAttempt::Unstable(metadata)) => {
                    self.accept_metadata(&metadata);
                    sequence = sequence.wrapping_add(1) & 0x00FF_FFFF;
                    if host_attempt < MAX_HOST_CAPTURE_ATTEMPTS {
                        tokio::time::sleep(CAPTURE_RETRY_DELAY).await;
                    }
                }
                Err(error) => return Err(error),
            }
        }
        self.next_snapshot_sequence = sequence;
        self.finish_operation();
        Err(Error::AutomationScreenUnstable {
            attempts: MAX_HOST_CAPTURE_ATTEMPTS,
        })
    }

    async fn key_event_inner(
        &mut self,
        key: FrontPanelKey,
        phase: KeyPhase,
        sequence: u8,
        expected_command_count: u32,
        expected_seqlock: u32,
    ) -> Result<AutomationMetadata, Error> {
        self.send_key_command(key, phase, sequence).await?;

        let (raw, metadata) = self.read_stable_metadata().await?;
        Self::validate_key_metadata(
            &raw,
            &metadata,
            key,
            phase,
            sequence,
            expected_command_count,
            expected_seqlock,
        )?;
        if metadata.generation != self.last_generation {
            return Err(Radio::<T>::strict_protocol_error(
                "a key event that does not alter the published frame generation",
                metadata.generation.to_le_bytes().to_vec(),
            ));
        }
        Ok(metadata)
    }

    async fn send_key_command(
        &mut self,
        key: FrontPanelKey,
        phase: KeyPhase,
        sequence: u8,
    ) -> Result<(), Error> {
        let request = format!(
            "GM K{:02X},{}{:02X}\r",
            key.as_raw(),
            phase.as_raw(),
            sequence
        );
        let expected = format!(
            "GM K{:02X},{}{:02X}00\r",
            key.as_raw(),
            phase.as_raw(),
            sequence
        );
        self.radio
            .strict_expect(request.as_bytes(), expected.as_bytes())
            .await
    }

    async fn capture_screen_once(
        &mut self,
        sequence: u32,
        expected_command_count: u32,
        expected_seqlock: u32,
    ) -> Result<CaptureAttempt, Error> {
        let request = format!("GM S{sequence:06X}\r");
        let reply = self
            .radio
            .strict_cat_exchange(request.as_bytes(), 13)
            .await?;
        let status = Self::parse_command_status(request.as_bytes(), &reply)?;

        let metadata_before_raw = self.read_range(METADATA_OFFSET, METADATA_LENGTH).await?;
        let metadata = Self::parse_metadata(&metadata_before_raw, self.abi)?;
        if status == 1 {
            Self::validate_unstable_snapshot_metadata(
                &metadata_before_raw,
                &metadata,
                sequence,
                self.last_generation,
                expected_command_count,
                expected_seqlock,
            )?;
            let metadata_after_raw = self.read_range(METADATA_OFFSET, METADATA_LENGTH).await?;
            if metadata_after_raw != metadata_before_raw {
                return Err(Radio::<T>::strict_protocol_error(
                    "byte-identical metadata for an unstable snapshot result",
                    metadata_after_raw,
                ));
            }
            return Ok(CaptureAttempt::Unstable(metadata));
        }
        Self::validate_snapshot_metadata(
            &metadata_before_raw,
            &metadata,
            sequence,
            self.last_generation.wrapping_add(1),
            expected_command_count,
            expected_seqlock,
        )?;

        let pixels = if metadata.rle_encoded_length == 0 {
            self.read_range(PIXEL_OFFSET, PIXEL_LENGTH).await?
        } else {
            let encoded = self
                .read_range(RLE_OFFSET, metadata.rle_encoded_length)
                .await?;
            Self::decode_rle(&encoded)?
        };
        let metadata_after_raw = self.read_range(METADATA_OFFSET, METADATA_LENGTH).await?;
        if metadata_after_raw != metadata_before_raw {
            return Err(Radio::<T>::strict_protocol_error(
                "byte-identical metadata bracketing the snapshot transfer",
                metadata_after_raw,
            ));
        }
        let metadata_after = Self::parse_metadata(&metadata_after_raw, self.abi)?;
        if metadata_after.seqlock & 1 != 0 {
            return Err(Radio::<T>::strict_protocol_error(
                "an even snapshot metadata seqlock",
                metadata_after.seqlock.to_le_bytes().to_vec(),
            ));
        }

        let frame = ScreenFrame::from_rgb565_le(pixels).map_err(|error| {
            Radio::<T>::strict_protocol_error(
                "one canonical 240x180 RGB565LE screen frame",
                error.to_string().into_bytes(),
            )
        })?;
        let actual_crc = frame.crc32();
        if actual_crc != metadata.crc32 {
            return Err(Radio::<T>::strict_protocol_error(
                "a screen frame matching the metadata IEEE CRC-32",
                actual_crc.to_le_bytes().to_vec(),
            ));
        }

        Ok(CaptureAttempt::Stable(AutomationSnapshot {
            frame,
            metadata,
        }))
    }

    fn parse_command_status(request: &[u8], reply: &[u8]) -> Result<u8, Error> {
        let echo = request.get(..10).ok_or_else(|| {
            Radio::<T>::strict_protocol_error("an eleven-byte automation command", request.to_vec())
        })?;
        let reply_echo = reply.get(..10).ok_or_else(|| {
            Radio::<T>::strict_protocol_error("a complete echoed automation reply", reply.to_vec())
        })?;
        let status = reply.get(10..12).ok_or_else(|| {
            Radio::<T>::strict_protocol_error(
                "one hexadecimal automation status byte",
                reply.to_vec(),
            )
        })?;
        if reply.len() != 13 || reply.last() != Some(&b'\r') || reply_echo != echo {
            return Err(Radio::<T>::strict_protocol_error(
                "an exact automation command echo, one status byte, and CR",
                reply.to_vec(),
            ));
        }
        match status {
            b"00" => Ok(0),
            b"01" => Ok(1),
            _ => Err(Radio::<T>::strict_protocol_error(
                "automation status 00 or 01 in uppercase hexadecimal",
                reply.to_vec(),
            )),
        }
    }

    fn parse_guarded_status(request: &[u8], reply: &[u8]) -> Result<u8, Error> {
        let echo = request.get(..10).ok_or_else(|| {
            Radio::<T>::strict_protocol_error("an eleven-byte guarded command", request.to_vec())
        })?;
        let reply_echo = reply.get(..10).ok_or_else(|| {
            Radio::<T>::strict_protocol_error(
                "a complete echoed guarded-command reply",
                reply.to_vec(),
            )
        })?;
        let status = reply.get(10..12).ok_or_else(|| {
            Radio::<T>::strict_protocol_error(
                "one hexadecimal guarded-command status byte",
                reply.to_vec(),
            )
        })?;
        if reply.len() != 13 || reply.last() != Some(&b'\r') || reply_echo != echo {
            return Err(Radio::<T>::strict_protocol_error(
                "an exact guarded-command echo, one status byte, and CR",
                reply.to_vec(),
            ));
        }
        match status {
            b"00" => Ok(0),
            b"02" => Ok(2),
            _ => Err(Radio::<T>::strict_protocol_error(
                "guarded-command status 00 or 02 in uppercase hexadecimal",
                reply.to_vec(),
            )),
        }
    }

    async fn read_stable_metadata(&mut self) -> Result<(Vec<u8>, AutomationMetadata), Error> {
        let first = self.read_range(METADATA_OFFSET, METADATA_LENGTH).await?;
        let metadata = Self::parse_metadata(&first, self.abi)?;
        let second = self.read_range(METADATA_OFFSET, METADATA_LENGTH).await?;
        if first != second {
            return Err(Radio::<T>::strict_protocol_error(
                "two byte-identical automation metadata reads",
                second,
            ));
        }
        if metadata.seqlock & 1 != 0 {
            return Err(Radio::<T>::strict_protocol_error(
                "an even automation metadata seqlock",
                metadata.seqlock.to_le_bytes().to_vec(),
            ));
        }
        Ok((first, metadata))
    }

    async fn read_range(&mut self, start: u32, total: u32) -> Result<Vec<u8>, Error> {
        let end = start.checked_add(total).ok_or_else(|| {
            Radio::<T>::strict_protocol_error(
                "a non-overflowing automation aperture range",
                format!("0x{start:06X}+0x{total:X}").into_bytes(),
            )
        })?;
        if total == 0 || !Self::aperture_range_allowed(start, end) {
            return Err(Radio::<T>::strict_protocol_error(
                "a nonempty range wholly inside the qualified automation aperture",
                format!("0x{start:06X}..0x{end:06X}").into_bytes(),
            ));
        }
        let capacity = usize::try_from(total).map_err(|_| {
            Radio::<T>::strict_protocol_error(
                "a host-addressable automation range length",
                total.to_le_bytes().to_vec(),
            )
        })?;
        let mut output = Vec::with_capacity(capacity);
        let mut cursor = start;
        while cursor < end {
            let remaining = end - cursor;
            let count = remaining.min(256);
            let length = ReadLen::new(u16::try_from(count).map_err(|_| {
                Radio::<T>::strict_protocol_error(
                    "an automation GM chunk no larger than 256 bytes",
                    count.to_le_bytes().to_vec(),
                )
            })?)?;
            let bytes = self
                .radio
                .strict_gm_read(MemoryReadOffset::new(cursor)?, length)
                .await?;
            output.extend_from_slice(&bytes);
            cursor = cursor.checked_add(count).ok_or_else(|| {
                Radio::<T>::strict_protocol_error(
                    "a non-overflowing automation GM cursor",
                    cursor.to_le_bytes().to_vec(),
                )
            })?;
        }
        Ok(output)
    }

    const fn aperture_range_allowed(start: u32, end: u32) -> bool {
        (start >= METADATA_OFFSET && end <= RAW_APERTURE_END)
            || (start >= RLE_OFFSET && end <= RLE_APERTURE_END)
    }

    fn decode_rle(encoded: &[u8]) -> Result<Vec<u8>, Error> {
        if encoded.is_empty() || !encoded.len().is_multiple_of(3) {
            return Err(Radio::<T>::strict_protocol_error(
                "nonempty RLE3 records with a length divisible by three",
                encoded.len().to_string().into_bytes(),
            ));
        }

        let mut decoded = Vec::with_capacity(SCREEN_BYTES);
        for record in encoded.chunks_exact(3) {
            let [count, low, high] = record else {
                return Err(Radio::<T>::strict_protocol_error(
                    "one complete RLE3 record",
                    record.to_vec(),
                ));
            };
            if *count == 0 {
                return Err(Radio::<T>::strict_protocol_error(
                    "a nonzero RLE3 run length",
                    record.to_vec(),
                ));
            }
            let run_bytes = usize::from(*count).checked_mul(2).ok_or_else(|| {
                Radio::<T>::strict_protocol_error("a non-overflowing RLE3 run", record.to_vec())
            })?;
            let new_length = decoded.len().checked_add(run_bytes).ok_or_else(|| {
                Radio::<T>::strict_protocol_error(
                    "a non-overflowing decoded frame",
                    decoded.len().to_string().into_bytes(),
                )
            })?;
            if new_length > SCREEN_BYTES {
                return Err(Radio::<T>::strict_protocol_error(
                    "RLE3 data bounded by one complete screen",
                    new_length.to_string().into_bytes(),
                ));
            }
            for _ in 0..*count {
                decoded.extend_from_slice(&[*low, *high]);
            }
        }
        if decoded.len() != SCREEN_BYTES {
            return Err(Radio::<T>::strict_protocol_error(
                "RLE3 data expanding to exactly one complete screen",
                decoded.len().to_string().into_bytes(),
            ));
        }
        Ok(decoded)
    }

    #[expect(
        clippy::too_many_lines,
        reason = "keeping the complete fixed-offset ABI record validation linear makes omissions and offset drift auditable"
    )]
    fn parse_metadata(
        raw: &[u8],
        expected_abi: AutomationAbi,
    ) -> Result<AutomationMetadata, Error> {
        if raw.len() != 256 {
            return Err(Radio::<T>::strict_protocol_error(
                "one exact 0x100-byte automation metadata record",
                raw.len().to_string().into_bytes(),
            ));
        }

        let magic = Self::metadata_word(raw, 0x00)?;
        let abi_version = Self::metadata_word(raw, 0x04)?;
        let seqlock = Self::metadata_word(raw, 0x08)?;
        let features = Self::metadata_word(raw, 0x0C)?;
        let width = Self::metadata_word(raw, 0x10)?;
        let height = Self::metadata_word(raw, 0x14)?;
        let stride = Self::metadata_word(raw, 0x18)?;
        let pixel_format = Self::metadata_word(raw, 0x1C)?;
        let pixel_length = Self::metadata_word(raw, 0x20)?;
        let pixel_offset = Self::metadata_word(raw, 0x24)?;
        let generation = Self::metadata_word(raw, 0x28)?;
        let capture_result = Self::metadata_word(raw, 0x2C)?;
        let crc32 = Self::metadata_word(raw, 0x30)?;
        let capture_attempts = Self::metadata_word(raw, 0x34)?;
        let command_count = Self::metadata_word(raw, 0x38)?;
        let last_command = Self::metadata_word(raw, 0x3C)?;
        let last_host_sequence = Self::metadata_word(raw, 0x40)?;
        let last_key = Self::metadata_word(raw, 0x44)?;
        let last_phase = Self::metadata_word(raw, 0x48)?;
        let last_key_result = Self::metadata_word(raw, 0x4C)?;
        let framebuffer_address = Self::metadata_word(raw, 0x50)?;
        let snapshot_address = Self::metadata_word(raw, 0x54)?;
        let limits = Self::metadata_word(raw, 0x58)?;
        let rle_magic = Self::metadata_word(raw, 0x5C)?;
        let rle_relative_offset = Self::metadata_word(raw, 0x60)?;
        let rle_encoded_length = Self::metadata_word(raw, 0x64)?;
        let route_ascii = Self::metadata_word(raw, 0x68)?;
        let route_guard_count = Self::metadata_word(raw, 0x6C)?;
        let route_completed_taps = Self::metadata_word(raw, 0x70)?;
        let route_event_mask = Self::metadata_word(raw, 0x74)?;
        let trailing_magic = Self::metadata_word(raw, 0xFC)?;

        let expected_limits =
            u32::from(AUTOMATION_MAX_KEY) | (u32::from(AUTOMATION_MAX_PHASE) << 8);
        let static_valid = magic == AUTOMATION_MAGIC
            && abi_version == u32::from(expected_abi.version)
            && features == u32::from(expected_abi.features)
            && width == FRAME_WIDTH
            && height == FRAME_HEIGHT
            && stride == FRAME_STRIDE
            && pixel_format == PIXEL_FORMAT_RGB565LE
            && pixel_length == PIXEL_LENGTH
            && pixel_offset == METADATA_LENGTH
            && framebuffer_address == FRAMEBUFFER_ADDRESS
            && snapshot_address == SNAPSHOT_ADDRESS
            && limits == expected_limits
            && rle_magic == RLE_MAGIC
            && rle_relative_offset == RLE_RELATIVE_OFFSET
            && rle_encoded_length <= PIXEL_LENGTH
            && (rle_encoded_length == 0 || rle_encoded_length % 3 == 0)
            && trailing_magic == AUTOMATION_MAGIC;
        if !static_valid {
            return Err(Radio::<T>::strict_protocol_error(
                "exact qualified automation metadata constants",
                raw.to_vec(),
            ));
        }

        let reserved = raw.get(0x78..0xFC).ok_or_else(|| {
            Radio::<T>::strict_protocol_error(
                "the complete automation metadata reserved window",
                raw.to_vec(),
            )
        })?;
        if reserved.iter().any(|byte| *byte != 0) {
            return Err(Radio::<T>::strict_protocol_error(
                "zeroed automation metadata reserved bytes",
                reserved.to_vec(),
            ));
        }
        let route_fields = [
            route_ascii,
            route_guard_count,
            route_completed_taps,
            route_event_mask,
        ];
        let route_fields_valid = match last_command {
            COMMAND_GUARDED_DECIMAL_ROUTE => {
                let [first, second, third, high] = route_ascii.to_le_bytes();
                let ascii_digits = [first, second, third];
                let digits_valid = high == 0 && ascii_digits.iter().all(u8::is_ascii_digit);
                let last_digit_index = if last_key_result == RESULT_OK {
                    Some(2_usize)
                } else if last_key_result == RESULT_CONTEXT_CHANGED && route_completed_taps == 0 {
                    Some(0_usize)
                } else {
                    None
                };
                let last_digit_matches = last_digit_index.is_some_and(|index| {
                    ascii_digits
                        .get(index)
                        .and_then(|ascii| ascii.checked_sub(b'0'))
                        .is_some_and(|digit| {
                            digit <= 9 && last_key == u32::from(0x0A_u8.wrapping_add(digit))
                        })
                });
                let outcome_valid = if last_key_result == RESULT_OK {
                    route_guard_count == 1
                        && route_completed_taps == 3
                        && route_event_mask == 0x3F
                        && last_phase == u32::from(KeyPhase::Release.as_raw())
                } else {
                    last_key_result == RESULT_CONTEXT_CHANGED
                        && route_completed_taps == 0
                        && route_guard_count == 1
                        && route_event_mask == 0
                        && last_phase == u32::from(KeyPhase::Press.as_raw())
                };
                digits_valid
                    && last_host_sequence <= 0xFF
                    && rle_encoded_length == 0
                    && last_digit_matches
                    && outcome_valid
            }
            _ => route_fields.iter().all(|&value| value == 0),
        };
        if !route_fields_valid {
            return Err(Radio::<T>::strict_protocol_error(
                "exact qualified route fields or zeroed non-route metadata fields",
                raw.get(0x68..0x78).unwrap_or(raw).to_vec(),
            ));
        }

        Ok(AutomationMetadata {
            seqlock,
            features,
            generation,
            capture_result,
            crc32,
            capture_attempts,
            command_count,
            last_command,
            last_host_sequence,
            last_key,
            last_phase,
            last_key_result,
            rle_encoded_length,
            route_ascii,
            route_guard_count,
            route_completed_taps,
            route_event_mask,
        })
    }

    fn metadata_word(raw: &[u8], offset: usize) -> Result<u32, Error> {
        let end = offset.checked_add(4).ok_or_else(|| {
            Radio::<T>::strict_protocol_error(
                "a non-overflowing metadata field offset",
                offset.to_string().into_bytes(),
            )
        })?;
        let field = raw.get(offset..end).ok_or_else(|| {
            Radio::<T>::strict_protocol_error(
                "a complete four-byte metadata field",
                offset.to_string().into_bytes(),
            )
        })?;
        let bytes = <[u8; 4]>::try_from(field).map_err(|_| {
            Radio::<T>::strict_protocol_error("four metadata field bytes", field.to_vec())
        })?;
        Ok(u32::from_le_bytes(bytes))
    }

    fn validate_key_metadata(
        raw: &[u8],
        metadata: &AutomationMetadata,
        key: FrontPanelKey,
        phase: KeyPhase,
        sequence: u8,
        expected_command_count: u32,
        expected_seqlock: u32,
    ) -> Result<(), Error> {
        if metadata.seqlock == expected_seqlock
            && metadata.command_count == expected_command_count
            && metadata.last_command == COMMAND_KEY
            && metadata.last_host_sequence == u32::from(sequence)
            && metadata.last_key == u32::from(key.as_raw())
            && metadata.last_phase == u32::from(phase.as_raw())
            && metadata.last_key_result == RESULT_OK
        {
            Ok(())
        } else {
            Err(Radio::<T>::strict_protocol_error(
                "metadata proving the exact acknowledged key event",
                raw.to_vec(),
            ))
        }
    }

    fn validate_guarded_context_changed_metadata(
        raw: &[u8],
        metadata: &AutomationMetadata,
        key: FrontPanelKey,
        sequence: u8,
        expected_command_count: u32,
        expected_seqlock: u32,
    ) -> Result<(), Error> {
        if metadata.seqlock == expected_seqlock
            && metadata.command_count == expected_command_count
            && metadata.last_command == COMMAND_GUARDED_KEY
            && metadata.last_host_sequence == u32::from(sequence)
            && metadata.last_key == u32::from(key.as_raw())
            && metadata.last_phase == u32::from(KeyPhase::Press.as_raw())
            && metadata.last_key_result == RESULT_CONTEXT_CHANGED
        {
            Ok(())
        } else {
            Err(Radio::<T>::strict_protocol_error(
                "metadata proving an exact guarded context refusal",
                raw.to_vec(),
            ))
        }
    }

    fn validate_guarded_decimal_route_metadata(
        raw: &[u8],
        metadata: AutomationMetadata,
        lease: &GuardedInputLease,
        route: GuardedDecimalRoute,
        sequence: u8,
        status: u8,
        expected_command_count: u32,
        expected_seqlock: u32,
    ) -> Result<GuardedDecimalRouteReceipt, Error> {
        let semantic = match status {
            GUARDED_STATUS_OK => {
                metadata.route_guard_count == 1
                    && metadata.route_completed_taps == 3
                    && metadata.route_event_mask == 0x3F
                    && route
                        .key_at(2)
                        .is_some_and(|key| metadata.last_key == u32::from(key.as_raw()))
                    && metadata.last_phase == u32::from(KeyPhase::Release.as_raw())
                    && metadata.last_key_result == RESULT_OK
            }
            2 => {
                let refused_key_matches = route
                    .key_at(0)
                    .is_some_and(|key| metadata.last_key == u32::from(key.as_raw()));
                metadata.route_completed_taps == 0
                    && metadata.route_guard_count == 1
                    && metadata.route_event_mask == 0
                    && refused_key_matches
                    && metadata.last_phase == u32::from(KeyPhase::Press.as_raw())
                    && metadata.last_key_result == RESULT_CONTEXT_CHANGED
            }
            _ => false,
        };
        if metadata.seqlock != expected_seqlock
            || metadata.command_count != expected_command_count
            || metadata.last_command != COMMAND_GUARDED_DECIMAL_ROUTE
            || metadata.last_host_sequence != u32::from(sequence)
            || metadata.route_ascii != route.packed_ascii()
            || metadata.rle_encoded_length != 0
            || !semantic
        {
            return Err(Radio::<T>::strict_protocol_error(
                "metadata proving the exact qualified guarded decimal route receipt",
                raw.to_vec(),
            ));
        }
        Self::validate_guarded_frame_receipt(raw, &metadata, lease)?;
        let guard_count = u8::try_from(metadata.route_guard_count).map_err(|_| {
            Radio::<T>::strict_protocol_error("a three-step guarded route", raw.to_vec())
        })?;
        let completed_taps = u8::try_from(metadata.route_completed_taps).map_err(|_| {
            Radio::<T>::strict_protocol_error("a three-tap guarded route", raw.to_vec())
        })?;
        let event_mask = u8::try_from(metadata.route_event_mask).map_err(|_| {
            Radio::<T>::strict_protocol_error("a six-bit guarded route event mask", raw.to_vec())
        })?;
        Ok(GuardedDecimalRouteReceipt {
            route,
            sequence,
            guard_count,
            completed_taps,
            event_mask,
            metadata,
        })
    }

    fn validate_guarded_frame_receipt(
        raw: &[u8],
        metadata: &AutomationMetadata,
        lease: &GuardedInputLease,
    ) -> Result<(), Error> {
        if metadata.generation == lease.metadata.generation
            && metadata.crc32 == lease.metadata.crc32
            && metadata.capture_result == lease.metadata.capture_result
        {
            Ok(())
        } else {
            Err(Radio::<T>::strict_protocol_error(
                "metadata retaining the guarded snapshot generation and CRC",
                raw.to_vec(),
            ))
        }
    }

    fn validate_guarded_snapshot_state_unchanged(
        raw: &[u8],
        metadata: &AutomationMetadata,
        before: &AutomationMetadata,
    ) -> Result<(), Error> {
        if metadata.generation == before.generation
            && metadata.capture_result == before.capture_result
            && metadata.crc32 == before.crc32
            && metadata.capture_attempts == before.capture_attempts
            && metadata.rle_encoded_length == 0
        {
            Ok(())
        } else {
            Err(Radio::<T>::strict_protocol_error(
                "metadata retaining the unavailable guarded snapshot state",
                raw.to_vec(),
            ))
        }
    }

    fn validate_snapshot_metadata(
        raw: &[u8],
        metadata: &AutomationMetadata,
        sequence: u32,
        expected_generation: u32,
        expected_command_count: u32,
        expected_seqlock: u32,
    ) -> Result<(), Error> {
        if metadata.seqlock == expected_seqlock
            && metadata.command_count == expected_command_count
            && metadata.last_command == COMMAND_SNAPSHOT
            && metadata.last_host_sequence == sequence
            && metadata.capture_result == RESULT_OK
            && (1..=MAX_CAPTURE_ATTEMPTS).contains(&metadata.capture_attempts)
            && metadata.generation == expected_generation
        {
            Ok(())
        } else {
            Err(Radio::<T>::strict_protocol_error(
                "metadata proving one newly published stable snapshot",
                raw.to_vec(),
            ))
        }
    }

    fn validate_unstable_snapshot_metadata(
        raw: &[u8],
        metadata: &AutomationMetadata,
        sequence: u32,
        expected_generation: u32,
        expected_command_count: u32,
        expected_seqlock: u32,
    ) -> Result<(), Error> {
        if metadata.seqlock == expected_seqlock
            && metadata.command_count == expected_command_count
            && metadata.last_command == COMMAND_SNAPSHOT
            && metadata.last_host_sequence == sequence
            && metadata.capture_result == RESULT_UNSTABLE
            && metadata.capture_attempts == MAX_CAPTURE_ATTEMPTS
            && metadata.generation == expected_generation
            && metadata.crc32 == 0
            && metadata.rle_encoded_length == 0
        {
            Ok(())
        } else {
            Err(Radio::<T>::strict_protocol_error(
                "metadata proving one bounded unstable snapshot result",
                raw.to_vec(),
            ))
        }
    }

    const fn require_valid(&self) -> Result<(), Error> {
        if self.valid {
            Ok(())
        } else {
            Err(Error::AutomationNotQualified)
        }
    }

    const fn begin_operation(&mut self) {
        self.valid = false;
        self.guarded_input_lease = None;
        self.radio.gm_poisoned = true;
        self.radio.desynced = true;
    }

    const fn finish_operation(&mut self) {
        self.radio.desynced = false;
        self.radio.gm_poisoned = false;
        self.valid = true;
    }

    const fn accept_metadata(&mut self, metadata: &AutomationMetadata) {
        self.last_command_count = metadata.command_count;
        self.last_seqlock = metadata.seqlock;
    }
}

impl<T: Transport> Radio<T> {
    /// Prove and exclusively borrow the exact guarded-automation firmware.
    ///
    /// Qualification independently attests the stock identity, every patched
    /// hook, the complete 1,300-byte linked runtime, the exact ABI 3/feature
    /// `0x7F` response, the upper-bound refusal, and stable metadata. The
    /// ABI query invalidates any inherited firmware snapshot lease before it
    /// replies. ABI 3 route receipts are all-or-nothing: one guard precedes
    /// either all three complete taps or a zero-prefix refusal.
    ///
    /// # Errors
    ///
    /// Returns an error without a session if the CAT stream is not pristine or
    /// any identity, patch, runtime, ABI, aperture, or bound proof fails. Once
    /// I/O begins, cancellation or failure poisons the strict GM stream until
    /// [`Radio::reconnect`].
    pub async fn qualify_automation(&mut self) -> Result<AutomationSession<'_, T>, Error> {
        // Qualification is a strict CAT conversation, not a recovery path.
        // Require an already trusted frame boundary before any attestation
        // traffic is sent.
        self.require_cat_ready()?;

        if self.mcp_phase != McpPhase::Inactive {
            return Err(Error::McpInterrupted);
        }
        if self.gm_poisoned {
            return Err(Error::MemoryReadStreamPoisoned);
        }
        if self.desynced {
            return Err(Self::strict_protocol_error(
                "a synchronized CAT stream before automation qualification",
                b"stream is marked desynchronized".to_vec(),
            ));
        }
        if !self.codec.is_empty() {
            return Err(Self::strict_protocol_error(
                "an empty CAT codec before automation qualification",
                b"buffered CAT bytes".to_vec(),
            ));
        }

        self.gm_poisoned = true;
        self.desynced = true;
        let result = async {
            self.require_strict_quiet().await?;
            self.strict_expect(b"ID\r", EXPECTED_MODEL_FRAME).await?;
            self.strict_expect(b"FV\r", EXPECTED_FIRMWARE_FRAME).await?;
            self.require_strict_quiet().await?;

            self.attest_automation_bytes("V1.03.AZM", READ_HOOK_ATTESTATION_OFFSET, &[0xC0])
                .await?;
            self.attest_automation_bytes(
                "V1.03.AZM",
                DISPATCH_ATTESTATION_OFFSET,
                DISPATCH_ATTESTATION,
            )
            .await?;
            self.attest_automation_bytes(
                "V1.03.AZM",
                ADAPTER_ATTESTATION_OFFSET,
                ADAPTER_ATTESTATION,
            )
            .await?;
            self.attest_automation_bytes("V1.03.AZM", BOUND_ATTESTATION_OFFSET, BOUND_ATTESTATION)
                .await?;
            self.attest_automation_bytes(
                "V1.03.AZM",
                READ_HOOK_ATTESTATION_OFFSET,
                READ_HOOK_ATTESTATION,
            )
            .await?;
            self.attest_automation_bytes(
                "V1.03.AZM",
                AUTOMATION_RUNTIME_OFFSET,
                AUTOMATION_RUNTIME,
            )
            .await?;

            self.strict_expect(ABI_QUERY, ABI_REPLY).await?;
            self.strict_checkpoint().await?;
            self.strict_expect(CROSSING_READ, CROSSING_REFUSAL).await?;
            self.strict_checkpoint().await?;

            let first = self
                .strict_automation_range(METADATA_OFFSET, METADATA_LENGTH)
                .await?;
            let metadata = AutomationSession::<T>::parse_metadata(&first, EXPECTED_ABI)?;
            let second = self
                .strict_automation_range(METADATA_OFFSET, METADATA_LENGTH)
                .await?;
            if first != second || metadata.seqlock & 1 != 0 {
                return Err(Self::strict_protocol_error(
                    "stable even-seqlock automation metadata through the virtual aperture",
                    second,
                ));
            }
            Ok(metadata)
        }
        .await;

        match result {
            Ok(metadata) => {
                self.desynced = false;
                self.gm_poisoned = false;
                self.firmware_version = Some(crate::types::FirmwareIdentity::new(
                    super::AZIMUTH_AUTOMATION_FIRMWARE,
                )?);
                Ok(AutomationSession {
                    radio: self,
                    abi: EXPECTED_ABI,
                    valid: true,
                    next_key_sequence: 0,
                    next_snapshot_sequence: 0,
                    last_generation: metadata.generation,
                    last_command_count: metadata.command_count,
                    last_seqlock: metadata.seqlock,
                    guarded_input_lease: None,
                })
            }
            Err(error) => Err(error),
        }
    }

    async fn attest_automation_bytes(
        &mut self,
        revision: &'static str,
        start: u32,
        expected: &[u8],
    ) -> Result<(), Error> {
        let mut cursor = start;
        for chunk in expected.chunks(256) {
            let length = u16::try_from(chunk.len()).map_err(|_| {
                Self::strict_protocol_error(
                    "an automation attestation chunk no larger than 256 bytes",
                    chunk.len().to_string().into_bytes(),
                )
            })?;
            let actual = self
                .strict_gm_read(MemoryReadOffset::new(cursor)?, ReadLen::new(length)?)
                .await?;
            if actual != chunk {
                return Err(Self::strict_protocol_error(
                    &format!("exact {revision} bytes at DDR offset 0x{cursor:06X}"),
                    actual,
                ));
            }
            self.strict_checkpoint().await?;
            cursor = cursor.checked_add(u32::from(length)).ok_or_else(|| {
                Self::strict_protocol_error(
                    "a non-overflowing automation attestation cursor",
                    cursor.to_le_bytes().to_vec(),
                )
            })?;
        }
        Ok(())
    }

    async fn strict_automation_range(&mut self, start: u32, total: u32) -> Result<Vec<u8>, Error> {
        let end = start.checked_add(total).ok_or_else(|| {
            Self::strict_protocol_error(
                "a non-overflowing automation qualification range",
                format!("0x{start:06X}+0x{total:X}").into_bytes(),
            )
        })?;
        if total == 0 || !AutomationSession::<T>::aperture_range_allowed(start, end) {
            return Err(Self::strict_protocol_error(
                "an automation qualification read inside the automation aperture",
                format!("0x{start:06X}..0x{end:06X}").into_bytes(),
            ));
        }
        let capacity = usize::try_from(total).map_err(|_| {
            Self::strict_protocol_error(
                "a host-addressable automation qualification length",
                total.to_le_bytes().to_vec(),
            )
        })?;
        let mut output = Vec::with_capacity(capacity);
        let mut cursor = start;
        while cursor < end {
            let count = (end - cursor).min(256);
            let length = ReadLen::new(u16::try_from(count).map_err(|_| {
                Self::strict_protocol_error(
                    "an automation qualification chunk no larger than 256 bytes",
                    count.to_le_bytes().to_vec(),
                )
            })?)?;
            let bytes = self
                .strict_gm_read(MemoryReadOffset::new(cursor)?, length)
                .await?;
            output.extend_from_slice(&bytes);
            cursor = cursor.checked_add(count).ok_or_else(|| {
                Self::strict_protocol_error(
                    "a non-overflowing automation qualification cursor",
                    cursor.to_le_bytes().to_vec(),
                )
            })?;
        }
        Ok(output)
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{
        ABI_QUERY, ABI_REPLY, ADAPTER_ATTESTATION, ADAPTER_ATTESTATION_OFFSET, AUTOMATION_MAGIC,
        AUTOMATION_MAX_KEY, AUTOMATION_MAX_PHASE, AUTOMATION_RUNTIME, AUTOMATION_RUNTIME_OFFSET,
        AutomationAbi, AutomationSession, AutomationSnapshot, BOUND_ATTESTATION,
        BOUND_ATTESTATION_OFFSET, COMMAND_GUARDED_DECIMAL_ROUTE, COMMAND_GUARDED_KEY, COMMAND_KEY,
        COMMAND_SNAPSHOT, CROSSING_READ, CROSSING_REFUSAL, DISPATCH_ATTESTATION,
        DISPATCH_ATTESTATION_OFFSET, EXPECTED_ABI, FRAME_HEIGHT, FRAME_STRIDE, FRAME_WIDTH,
        FRAMEBUFFER_ADDRESS, FrontPanelKey, GUARDED_ROUTE_MAX_DURATION, GUARDED_SNAPSHOT_MAX_AGE,
        GuardedDecimalRoute, GuardedDecimalRouteOutcome, GuardedInputLease, GuardedKeyError,
        GuardedKeyOutcome, GuardedKeyResult, IfTapConfig, KeyPhase, MAX_CAPTURE_ATTEMPTS,
        METADATA_LENGTH, METADATA_OFFSET, PIXEL_FORMAT_RGB565LE, PIXEL_LENGTH, PIXEL_OFFSET,
        READ_HOOK_ATTESTATION, READ_HOOK_ATTESTATION_OFFSET, RESULT_CONTEXT_CHANGED, RESULT_OK,
        RLE_MAGIC, RLE_OFFSET, RLE_RELATIVE_OFFSET, SNAPSHOT_ADDRESS,
    };
    use crate::error::Error;
    use crate::protocol::memread::encode_hex_upper;
    use crate::radio::{BinaryProtocolProof, CatState, Radio};
    use crate::screen::{SCREEN_BYTES, ScreenFrame};
    use crate::transport::MockTransport;
    use crate::types::{Frequency, OperatingMode, StepSize, UsbAudioOutput};

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[tokio::test]
    async fn qualification_rejects_untrusted_cat_boundaries_before_io() -> TestResult {
        for cat_state in [
            CatState::RecoveryRequired,
            CatState::BinaryProven(BinaryProtocolProof::Mmdvm { data_band: None }),
        ] {
            let mut radio = Radio::new(MockTransport::new());
            radio.cat_state = cat_state;

            let result = radio.qualify_automation().await;

            assert!(matches!(result, Err(Error::CatRecoveryRequired)));
            assert!(!radio.gm_poisoned);
            assert!(!radio.desynced);
        }
        Ok(())
    }

    #[derive(Debug, Clone, Copy)]
    struct MetadataFixture {
        seqlock: u32,
        generation: u32,
        capture_result: u32,
        crc32: u32,
        capture_attempts: u32,
        command_count: u32,
        last_command: u32,
        last_host_sequence: u32,
        last_key: u32,
        last_phase: u32,
        last_key_result: u32,
        rle_encoded_length: u32,
        route_ascii: u32,
        route_guard_count: u32,
        route_completed_taps: u32,
        route_event_mask: u32,
    }

    impl Default for MetadataFixture {
        fn default() -> Self {
            Self {
                seqlock: 0,
                generation: 0,
                capture_result: 1,
                crc32: 0,
                capture_attempts: 0,
                command_count: 0,
                last_command: 0,
                last_host_sequence: 0,
                last_key: 0,
                last_phase: 0,
                last_key_result: 0,
                rle_encoded_length: 0,
                route_ascii: 0,
                route_guard_count: 0,
                route_completed_taps: 0,
                route_event_mask: 0,
            }
        }
    }

    fn put_word(raw: &mut [u8], offset: usize, value: u32) -> TestResult {
        let end = offset.checked_add(4).ok_or("metadata offset overflow")?;
        raw.get_mut(offset..end)
            .ok_or("metadata fixture field is out of range")?
            .copy_from_slice(&value.to_le_bytes());
        Ok(())
    }

    fn metadata_bytes(fixture: MetadataFixture) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
        metadata_bytes_for(EXPECTED_ABI, fixture)
    }

    fn metadata_bytes_for(
        abi: AutomationAbi,
        fixture: MetadataFixture,
    ) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
        let mut raw = vec![0_u8; 256];
        put_word(&mut raw, 0x00, AUTOMATION_MAGIC)?;
        put_word(&mut raw, 0x04, u32::from(abi.version))?;
        put_word(&mut raw, 0x08, fixture.seqlock)?;
        put_word(&mut raw, 0x0C, u32::from(abi.features))?;
        put_word(&mut raw, 0x10, FRAME_WIDTH)?;
        put_word(&mut raw, 0x14, FRAME_HEIGHT)?;
        put_word(&mut raw, 0x18, FRAME_STRIDE)?;
        put_word(&mut raw, 0x1C, PIXEL_FORMAT_RGB565LE)?;
        put_word(&mut raw, 0x20, PIXEL_LENGTH)?;
        put_word(&mut raw, 0x24, METADATA_LENGTH)?;
        put_word(&mut raw, 0x28, fixture.generation)?;
        put_word(&mut raw, 0x2C, fixture.capture_result)?;
        put_word(&mut raw, 0x30, fixture.crc32)?;
        put_word(&mut raw, 0x34, fixture.capture_attempts)?;
        put_word(&mut raw, 0x38, fixture.command_count)?;
        put_word(&mut raw, 0x3C, fixture.last_command)?;
        put_word(&mut raw, 0x40, fixture.last_host_sequence)?;
        put_word(&mut raw, 0x44, fixture.last_key)?;
        put_word(&mut raw, 0x48, fixture.last_phase)?;
        put_word(&mut raw, 0x4C, fixture.last_key_result)?;
        put_word(&mut raw, 0x50, FRAMEBUFFER_ADDRESS)?;
        put_word(&mut raw, 0x54, SNAPSHOT_ADDRESS)?;
        put_word(
            &mut raw,
            0x58,
            u32::from(AUTOMATION_MAX_KEY) | (u32::from(AUTOMATION_MAX_PHASE) << 8),
        )?;
        put_word(&mut raw, 0x5C, RLE_MAGIC)?;
        put_word(&mut raw, 0x60, RLE_RELATIVE_OFFSET)?;
        put_word(&mut raw, 0x64, fixture.rle_encoded_length)?;
        put_word(&mut raw, 0x68, fixture.route_ascii)?;
        put_word(&mut raw, 0x6C, fixture.route_guard_count)?;
        put_word(&mut raw, 0x70, fixture.route_completed_taps)?;
        put_word(&mut raw, 0x74, fixture.route_event_mask)?;
        put_word(&mut raw, 0xFC, AUTOMATION_MAGIC)?;
        Ok(raw)
    }

    fn reply(offset: u32, data: &[u8]) -> Vec<u8> {
        format!("GM {offset:06X},{}\r", encode_hex_upper(data)).into_bytes()
    }

    fn queue_range(
        mock: &mut MockTransport,
        start: u32,
        data: &[u8],
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut cursor = start;
        for chunk in data.chunks(256) {
            let wire_length = if chunk.len() == 256 { 0 } else { chunk.len() };
            let request = format!("GM {cursor:06X},{wire_length:02X}\r");
            mock.expect(request.as_bytes(), &reply(cursor, chunk));
            cursor = cursor
                .checked_add(u32::try_from(chunk.len())?)
                .ok_or("mock range cursor overflow")?;
        }
        Ok(())
    }

    fn queue_attestation(
        mock: &mut MockTransport,
        start: u32,
        expected: &[u8],
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut cursor = start;
        for chunk in expected.chunks(256) {
            queue_range(mock, cursor, chunk)?;
            mock.expect(b"ID\r", b"ID TH-D75\r");
            cursor = cursor
                .checked_add(u32::try_from(chunk.len())?)
                .ok_or("mock attestation cursor overflow")?;
        }
        Ok(())
    }

    fn queue_qualification(
        mock: &mut MockTransport,
        metadata: &[u8],
    ) -> Result<(), Box<dyn std::error::Error>> {
        mock.pend_when_empty();
        mock.expect(b"ID\r", b"ID TH-D75\r");
        mock.expect(b"FV\r", b"FV 1.03.AZM\r");
        queue_attestation(mock, READ_HOOK_ATTESTATION_OFFSET, &[0xC0])?;
        queue_attestation(mock, DISPATCH_ATTESTATION_OFFSET, DISPATCH_ATTESTATION)?;
        queue_attestation(mock, ADAPTER_ATTESTATION_OFFSET, ADAPTER_ATTESTATION)?;
        queue_attestation(mock, BOUND_ATTESTATION_OFFSET, BOUND_ATTESTATION)?;
        queue_attestation(mock, READ_HOOK_ATTESTATION_OFFSET, READ_HOOK_ATTESTATION)?;
        queue_attestation(mock, AUTOMATION_RUNTIME_OFFSET, AUTOMATION_RUNTIME)?;
        mock.expect(ABI_QUERY, ABI_REPLY);
        mock.expect(b"ID\r", b"ID TH-D75\r");
        mock.expect(CROSSING_READ, CROSSING_REFUSAL);
        mock.expect(b"ID\r", b"ID TH-D75\r");
        queue_range(mock, METADATA_OFFSET, metadata)?;
        queue_range(mock, METADATA_OFFSET, metadata)?;
        Ok(())
    }

    fn direct_session(
        radio: &mut Radio<MockTransport>,
        generation: u32,
    ) -> AutomationSession<'_, MockTransport> {
        direct_session_with_receipt(radio, generation, 0, 0)
    }

    fn direct_session_with_receipt(
        radio: &mut Radio<MockTransport>,
        generation: u32,
        command_count: u32,
        seqlock: u32,
    ) -> AutomationSession<'_, MockTransport> {
        AutomationSession {
            radio,
            abi: EXPECTED_ABI,
            valid: true,
            next_key_sequence: 0,
            next_snapshot_sequence: 0,
            last_generation: generation,
            last_command_count: command_count,
            last_seqlock: seqlock,
            guarded_input_lease: None,
        }
    }

    fn automation_snapshot(
        generation: u32,
        command_count: u32,
        seqlock: u32,
    ) -> Result<AutomationSnapshot, Box<dyn std::error::Error>> {
        let (pixels, crc32) = zero_frame()?;
        let raw = metadata_bytes_for(
            EXPECTED_ABI,
            MetadataFixture {
                seqlock,
                generation,
                capture_result: RESULT_OK,
                crc32,
                capture_attempts: 1,
                command_count,
                last_command: COMMAND_SNAPSHOT,
                ..MetadataFixture::default()
            },
        )?;
        Ok(AutomationSnapshot {
            frame: ScreenFrame::from_rgb565_le(pixels)?,
            metadata: AutomationSession::<MockTransport>::parse_metadata(&raw, EXPECTED_ABI)?,
        })
    }

    fn direct_automation_session<'a>(
        radio: &'a mut Radio<MockTransport>,
        snapshot: &AutomationSnapshot,
    ) -> AutomationSession<'a, MockTransport> {
        AutomationSession {
            radio,
            abi: EXPECTED_ABI,
            valid: true,
            next_key_sequence: 0,
            next_snapshot_sequence: 0,
            last_generation: snapshot.metadata.generation,
            last_command_count: snapshot.metadata.command_count,
            last_seqlock: snapshot.metadata.seqlock,
            guarded_input_lease: Some(GuardedInputLease {
                metadata: snapshot.metadata.clone(),
                validated_at: tokio::time::Instant::now(),
            }),
        }
    }

    fn direct_automation_session_without_snapshot(
        radio: &mut Radio<MockTransport>,
    ) -> AutomationSession<'_, MockTransport> {
        AutomationSession {
            radio,
            abi: EXPECTED_ABI,
            valid: true,
            next_key_sequence: 0,
            next_snapshot_sequence: 0,
            last_generation: 0,
            last_command_count: 0,
            last_seqlock: 0,
            guarded_input_lease: None,
        }
    }

    #[tokio::test]
    #[cfg(feature = "aprs")]
    async fn aligned_kiss_refusal_retains_attestation_for_one_later_transition() -> TestResult {
        let mut mock = MockTransport::new();
        mock.expect(b"TN 2,0\r", b"N\r");
        mock.expect(b"TN 2,0\r", b"TN 2,0\r");
        let mut radio = Radio::new(mock);

        {
            let mut session = direct_automation_session_without_snapshot(&mut radio);
            let refused = session
                .transition_to_kiss(crate::types::TncDataBand::A)
                .await;
            assert!(matches!(
                refused,
                Err(Error::NotAvailableInCurrentMode { mnemonic }) if mnemonic == "TN"
            ));
            assert!(
                session.is_valid(),
                "an aligned TN=N must retain the already-proved automation session"
            );

            session
                .transition_to_kiss(crate::types::TncDataBand::A)
                .await?;
            assert!(!session.is_valid(), "accepted TN ends CAT automation");
        }

        let kiss = radio.into_kiss_session().map_err(|(_, error)| error)?;
        assert_eq!(
            kiss.transport.writes(),
            &[b"TN 2,0\r".to_vec(), b"TN 2,0\r".to_vec()],
            "one refused request and one accepted request must send TN exactly once each"
        );
        kiss.transport.assert_complete();
        Ok(())
    }

    fn queue_if_tap_entry(mock: &mut MockTransport) {
        mock.expect(b"VM 1\r", b"VM 1,0\r");
        mock.expect(b"BC\r", b"BC 0\r");
        mock.expect(b"DL\r", b"DL 0\r");
        mock.expect(b"IO\r", b"IO 0\r");
        mock.expect(b"SQ 1\r", b"SQ 1,2\r");
        mock.expect(b"MD 1\r", b"MD 1,0\r");
        mock.expect(b"FQ 1\r", b"FQ 1,0145000000\r");
        mock.expect(b"SF 1\r", b"SF 1,5\r");
        mock.expect(b"BC 1\r", b"BC 1\r");
        mock.expect(b"BC\r", b"BC 1\r");
        mock.expect(b"DL 1\r", b"DL 1\r");
        mock.expect(b"DL\r", b"DL 1\r");
        mock.expect(b"SF 1,0\r", b"SF 1,0\r");
        mock.expect(b"SF 1\r", b"SF 1,0\r");
        mock.expect(b"MD 1,4\r", b"MD 1,4\r");
        mock.expect(b"MD 1\r", b"MD 1,4\r");
        mock.expect(b"SQ 1,0\r", b"SQ 1,0\r");
        mock.expect(b"SQ 1\r", b"SQ 1,0\r");
        mock.expect(b"IO 1\r", b"IO 1\r");
        mock.expect(b"IO\r", b"IO 1\r");
    }

    fn queue_if_tap_retune_to_145_025(mock: &mut MockTransport) {
        mock.expect(b"FQ 1\r", b"FQ 1,0145000000\r");
        mock.expect(b"SF 1\r", b"SF 1,0\r");
        mock.expect(b"IO 0\r", b"IO 0\r");
        mock.expect(b"IO\r", b"IO 0\r");
        for frequency in [
            145_005_000,
            145_010_000,
            145_015_000,
            145_020_000,
            145_025_000,
        ] {
            mock.expect(b"UP\r", b"UP\r");
            mock.expect(b"FQ 1\r", format!("FQ 1,{frequency:010}\r").as_bytes());
        }
        mock.expect(b"IO 1\r", b"IO 1\r");
        mock.expect(b"IO\r", b"IO 1\r");
    }

    fn queue_if_tap_restore_from_145_025(mock: &mut MockTransport) {
        mock.expect(b"IO 0\r", b"IO 0\r");
        mock.expect(b"IO\r", b"IO 0\r");
        mock.expect(b"SF 1,5\r", b"SF 1,5\r");
        mock.expect(b"SF 1\r", b"SF 1,5\r");
        mock.expect(b"FQ 1\r", b"FQ 1,0145025000\r");
        mock.expect(b"BC\r", b"BC 1\r");
        mock.expect(b"SF 1\r", b"SF 1,5\r");
        mock.expect(b"DW\r", b"DW\r");
        mock.expect(b"FQ 1\r", b"FQ 1,0145012500\r");
        mock.expect(b"DW\r", b"DW\r");
        mock.expect(b"FQ 1\r", b"FQ 1,0145000000\r");
        mock.expect(b"SQ 1,2\r", b"SQ 1,2\r");
        mock.expect(b"SQ 1\r", b"SQ 1,2\r");
        mock.expect(b"MD 1,0\r", b"MD 1,0\r");
        mock.expect(b"MD 1\r", b"MD 1,0\r");
        mock.expect(b"DL 0\r", b"DL 0\r");
        mock.expect(b"DL\r", b"DL 0\r");
        mock.expect(b"BC 0\r", b"BC 0\r");
        mock.expect(b"BC\r", b"BC 0\r");
    }

    fn queue_if_tap_clean_rejection_and_exact_rollback(mock: &mut MockTransport) {
        mock.expect(b"VM 1\r", b"VM 1,0\r");
        mock.expect(b"BC\r", b"BC 0\r");
        mock.expect(b"DL\r", b"DL 0\r");
        mock.expect(b"IO\r", b"IO 0\r");
        mock.expect(b"SQ 1\r", b"SQ 1,2\r");
        mock.expect(b"MD 1\r", b"MD 1,0\r");
        mock.expect(b"FQ 1\r", b"FQ 1,0145000000\r");
        mock.expect(b"SF 1\r", b"SF 1,5\r");
        mock.expect(b"BC 1\r", b"N\r");
        mock.expect(b"BC\r", b"BC 0\r");
        mock.expect(b"IO 0\r", b"IO 0\r");
        mock.expect(b"IO\r", b"IO 0\r");
        mock.expect(b"SF 1,5\r", b"N\r");
        mock.expect(b"SF 1\r", b"SF 1,5\r");
        mock.expect(b"FQ 1\r", b"FQ 1,0145000000\r");
        mock.expect(b"SQ 1,2\r", b"N\r");
        mock.expect(b"SQ 1\r", b"SQ 1,2\r");
        mock.expect(b"MD 1,0\r", b"MD 1,0\r");
        mock.expect(b"MD 1\r", b"MD 1,0\r");
        mock.expect(b"DL 0\r", b"N\r");
        mock.expect(b"DL\r", b"DL 0\r");
        mock.expect(b"BC 0\r", b"N\r");
        mock.expect(b"BC\r", b"BC 0\r");
    }

    #[tokio::test]
    async fn if_tap_lifecycle_retains_one_attested_session_without_requalification() -> TestResult {
        let mut mock = MockTransport::new();
        queue_if_tap_entry(&mut mock);
        queue_if_tap_retune_to_145_025(&mut mock);
        queue_if_tap_restore_from_145_025(&mut mock);
        let snapshot = automation_snapshot(9, 14, 28)?;
        let mut radio = Radio::new(mock);

        let mut session = direct_automation_session(&mut radio, &snapshot);
        let saved = session
            .enter_if_tap(IfTapConfig::new(OperatingMode::Usb).with_step(StepSize::Hz5000))
            .await?;
        assert!(session.guarded_input_lease.is_none());
        assert!(session.is_valid());

        let landed = session
            .retune_if_tap(
                &saved,
                Frequency::new(145_025_000),
                UsbAudioOutput::IntermediateFrequency,
            )
            .await?;
        assert_eq!(landed.as_hz(), 145_025_000);
        assert!(session.is_valid());

        let report = session.restore_if_tap(saved).await;
        assert!(report.is_complete(), "exact restore failed: {report:?}");
        assert!(session.is_valid());

        radio.transport.assert_complete();
        let qualification_commands = radio
            .transport
            .writes()
            .iter()
            .filter(|command| {
                command.as_slice() == b"ID\r"
                    || command.as_slice() == b"FV\r"
                    || command.starts_with(b"GM ")
            })
            .count();
        assert_eq!(
            qualification_commands, 0,
            "IF prepare/retune/restore must not repeat firmware qualification"
        );
        Ok(())
    }

    #[tokio::test]
    async fn malformed_if_entry_poisons_the_attested_session_and_refuses_followup() -> TestResult {
        let mut mock = MockTransport::new();
        mock.expect(b"VM 1\r", b"VM malformed\r");
        let mut radio = Radio::new(mock);
        let mut session = direct_automation_session_without_snapshot(&mut radio);

        let result = session
            .enter_if_tap(IfTapConfig::new(OperatingMode::Usb))
            .await;
        let Err(error) = result else {
            return Err("malformed IF entry unexpectedly succeeded".into());
        };
        assert!(matches!(*error.source, Error::Protocol(_)));
        assert!(!session.is_valid());
        let writes_before_followup = session.radio.transport.writes().len();
        assert!(matches!(
            session.capture_screen().await,
            Err(Error::AutomationNotQualified)
        ));
        assert_eq!(
            session.radio.transport.writes().len(),
            writes_before_followup,
            "a poisoned session must refuse follow-up automation before I/O"
        );
        session.radio.transport.assert_complete();
        Ok(())
    }

    #[tokio::test]
    async fn malformed_if_restore_poisons_the_attested_session_and_refuses_followup() -> TestResult
    {
        let mut mock = MockTransport::new();
        queue_if_tap_entry(&mut mock);
        mock.expect(b"IO 0\r", b"IO malformed\r");
        let mut radio = Radio::new(mock);
        let mut session = direct_automation_session_without_snapshot(&mut radio);
        let saved = session
            .enter_if_tap(IfTapConfig::new(OperatingMode::Usb).with_step(StepSize::Hz5000))
            .await?;

        let report = session.restore_if_tap(saved).await;
        assert!(!report.is_complete());
        assert!(!session.is_valid());
        let writes_before_followup = session.radio.transport.writes().len();
        assert!(matches!(
            session.capture_screen().await,
            Err(Error::AutomationNotQualified)
        ));
        assert_eq!(
            session.radio.transport.writes().len(),
            writes_before_followup,
            "a poisoned session must refuse follow-up automation before I/O"
        );
        session.radio.transport.assert_complete();
        Ok(())
    }

    #[tokio::test]
    async fn clean_if_rejection_with_exact_rollback_keeps_attested_session_valid() -> TestResult {
        let mut mock = MockTransport::new();
        queue_if_tap_clean_rejection_and_exact_rollback(&mut mock);
        let snapshot = automation_snapshot(4, 8, 16)?;
        let mut radio = Radio::new(mock);
        let mut session = direct_automation_session(&mut radio, &snapshot);

        let result = session
            .enter_if_tap(IfTapConfig::new(OperatingMode::Usb).with_step(StepSize::Hz5000))
            .await;
        let Err(error) = result else {
            return Err("unavailable IF entry unexpectedly succeeded".into());
        };
        assert!(matches!(
            *error.source,
            Error::NotAvailableInCurrentMode { .. }
        ));
        assert!(error.rollback.is_complete());
        assert!(error.snapshot.is_none());
        assert!(session.is_valid());
        assert!(session.guarded_input_lease.is_none());
        session.radio.transport.assert_complete();
        Ok(())
    }

    fn zero_frame() -> Result<(Vec<u8>, u32), Box<dyn std::error::Error>> {
        let bytes = vec![0_u8; SCREEN_BYTES];
        let crc = ScreenFrame::from_rgb565_le(bytes.clone())?.crc32();
        Ok((bytes, crc))
    }

    fn encode_solid_frame(low: u8, high: u8) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
        let mut pixels_remaining = SCREEN_BYTES / 2;
        let mut encoded = Vec::new();
        while pixels_remaining > 0 {
            let run = pixels_remaining.min(255);
            encoded.extend_from_slice(&[u8::try_from(run)?, low, high]);
            pixels_remaining -= run;
        }
        Ok(encoded)
    }

    fn key_metadata(
        generation: u32,
        key: FrontPanelKey,
        phase: KeyPhase,
        sequence: u8,
        command_count: u32,
    ) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
        key_metadata_with_seqlock(
            generation,
            key,
            phase,
            sequence,
            command_count,
            command_count.wrapping_mul(2),
        )
    }

    fn key_metadata_with_seqlock(
        generation: u32,
        key: FrontPanelKey,
        phase: KeyPhase,
        sequence: u8,
        command_count: u32,
        seqlock: u32,
    ) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
        metadata_bytes(MetadataFixture {
            seqlock,
            generation,
            command_count,
            last_command: COMMAND_KEY,
            last_host_sequence: u32::from(sequence),
            last_key: u32::from(key.as_raw()),
            last_phase: u32::from(phase.as_raw()),
            last_key_result: RESULT_OK,
            ..MetadataFixture::default()
        })
    }

    fn command_metadata(
        snapshot: &AutomationSnapshot,
        key: FrontPanelKey,
        phase: KeyPhase,
        sequence: u8,
        command_count: u32,
        seqlock: u32,
        command: u32,
        result: u32,
    ) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
        metadata_bytes_for(
            EXPECTED_ABI,
            MetadataFixture {
                seqlock,
                generation: snapshot.metadata.generation,
                capture_result: snapshot.metadata.capture_result,
                crc32: snapshot.metadata.crc32,
                capture_attempts: snapshot.metadata.capture_attempts,
                command_count,
                last_command: command,
                last_host_sequence: u32::from(sequence),
                last_key: u32::from(key.as_raw()),
                last_phase: u32::from(phase.as_raw()),
                last_key_result: result,
                ..MetadataFixture::default()
            },
        )
    }

    fn route_metadata(
        snapshot: &AutomationSnapshot,
        route: GuardedDecimalRoute,
        sequence: u8,
        command_count: u32,
        seqlock: u32,
        completed_taps: u32,
        result: u32,
    ) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
        let success = result == RESULT_OK;
        if !success && completed_taps != 0 {
            return Err("ABI 3 refusals cannot contain a completed prefix".into());
        }
        let key_index = if success { 2 } else { 0 };
        let event_mask = if success { 0x3F } else { 0 };
        let last_key = route
            .key_at(key_index)
            .ok_or("route fixture key index must be in range")?;
        metadata_bytes_for(
            EXPECTED_ABI,
            MetadataFixture {
                seqlock,
                generation: snapshot.metadata.generation,
                capture_result: snapshot.metadata.capture_result,
                crc32: snapshot.metadata.crc32,
                capture_attempts: snapshot.metadata.capture_attempts,
                command_count,
                last_command: COMMAND_GUARDED_DECIMAL_ROUTE,
                last_host_sequence: u32::from(sequence),
                last_key: u32::from(last_key.as_raw()),
                last_phase: u32::from(if success {
                    KeyPhase::Release.as_raw()
                } else {
                    KeyPhase::Press.as_raw()
                }),
                last_key_result: result,
                route_ascii: route.packed_ascii(),
                route_guard_count: 1,
                route_completed_taps: completed_taps,
                route_event_mask: event_mask,
                ..MetadataFixture::default()
            },
        )
    }

    fn snapshot_metadata(
        generation: u32,
        sequence: u32,
        crc32: u32,
        rle_encoded_length: u32,
    ) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
        metadata_bytes(MetadataFixture {
            seqlock: 2,
            generation,
            capture_result: RESULT_OK,
            crc32,
            capture_attempts: 1,
            command_count: 1,
            last_command: COMMAND_SNAPSHOT,
            last_host_sequence: sequence,
            rle_encoded_length,
            ..MetadataFixture::default()
        })
    }

    fn unstable_metadata(
        generation: u32,
        sequence: u32,
        command_count: u32,
    ) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
        metadata_bytes(MetadataFixture {
            seqlock: command_count.wrapping_mul(2),
            generation,
            capture_result: 1,
            crc32: 0,
            capture_attempts: MAX_CAPTURE_ATTEMPTS,
            command_count,
            last_command: COMMAND_SNAPSHOT,
            last_host_sequence: sequence,
            ..MetadataFixture::default()
        })
    }

    #[test]
    fn raw_key_conversions_cover_only_the_verified_domain() -> TestResult {
        for raw in 0_u8..=AUTOMATION_MAX_KEY {
            let key = FrontPanelKey::try_from(raw)?;
            assert_eq!(key.as_raw(), raw, "front-panel key must round-trip");
        }
        assert!(FrontPanelKey::try_from(0x19).is_err());
        assert!(KeyPhase::try_from(0).is_ok());
        assert!(KeyPhase::try_from(1).is_ok());
        assert!(KeyPhase::try_from(2).is_ok());
        assert!(KeyPhase::try_from(3).is_err());
        Ok(())
    }

    #[test]
    fn frozen_runtime_fixture_has_the_audited_shape() {
        assert_eq!(AUTOMATION_RUNTIME.len(), 1_300);
        assert_eq!(
            READ_HOOK_ATTESTATION,
            [
                0xC0, 0x26, 0x36, 0x06, 0x01, 0x99, 0x89, 0x19, 0x02, 0xA8, 0x00, 0x9A, 0x2D, 0xF1,
                0x1F, 0xFF,
            ]
        );
        assert_eq!(
            AUTOMATION_RUNTIME.get(..4),
            Some([0xF8, 0xB5, 0x84, 0xB0].as_slice())
        );
        assert_eq!(
            AUTOMATION_RUNTIME.get(0x46E..0x472),
            Some([0xF8, 0xB5, 0x04, 0x00].as_slice())
        );
        assert_eq!(
            AUTOMATION_RUNTIME.get(1_292..),
            Some([0x00, 0x53, 0xF1, 0xC0, 0x80, 0xA4, 0xF2, 0xC0].as_slice())
        );
    }

    #[tokio::test(start_paused = true)]
    async fn qualification_attests_exact_runtime_abi_and_metadata() -> TestResult {
        let initial = metadata_bytes_for(
            EXPECTED_ABI,
            MetadataFixture {
                seqlock: 84,
                generation: 8,
                command_count: 42,
                ..MetadataFixture::default()
            },
        )?;
        let mut mock = MockTransport::new();
        queue_qualification(&mut mock, &initial)?;
        let mut radio = Radio::new(mock);
        let session = radio.qualify_automation().await?;
        assert_eq!(session.abi(), EXPECTED_ABI);
        assert_eq!(session.last_generation, 8);
        assert_eq!(session.last_command_count, 42);
        assert_eq!(session.last_seqlock, 84);
        assert!(session.is_valid());
        Ok(())
    }

    #[tokio::test]
    async fn guarded_input_without_a_capture_is_refused_before_io() -> TestResult {
        let snapshot = automation_snapshot(7, 10, 20)?;
        let mock = MockTransport::new();
        let mut radio = Radio::new(mock);
        {
            let mut session = direct_automation_session(&mut radio, &snapshot);
            session.guarded_input_lease = None;
            let result = session
                .guarded_tap_key(&snapshot, FrontPanelKey::Menu)
                .await;
            assert!(matches!(result, Err(GuardedKeyError::SnapshotUnavailable)));
            assert!(session.is_valid());
        }
        radio.transport.assert_complete();
        Ok(())
    }

    #[tokio::test(start_paused = true)]
    async fn missing_snapshot_canary_authenticates_exact_abi3_refusal() -> TestResult {
        let initial = metadata_bytes_for(EXPECTED_ABI, MetadataFixture::default())?;
        let refused = metadata_bytes_for(
            EXPECTED_ABI,
            MetadataFixture {
                seqlock: 2,
                command_count: 1,
                last_command: COMMAND_GUARDED_KEY,
                last_key: u32::from(FrontPanelKey::Menu.as_raw()),
                last_phase: u32::from(KeyPhase::Press.as_raw()),
                last_key_result: RESULT_CONTEXT_CHANGED,
                ..MetadataFixture::default()
            },
        )?;
        let mut mock = MockTransport::new();
        queue_range(&mut mock, METADATA_OFFSET, &initial)?;
        queue_range(&mut mock, METADATA_OFFSET, &initial)?;
        mock.expect(b"GM G01,000\r", b"GM G01,00002\r");
        queue_range(&mut mock, METADATA_OFFSET, &refused)?;
        queue_range(&mut mock, METADATA_OFFSET, &refused)?;
        let mut radio = Radio::new(mock);
        {
            let mut session = direct_automation_session_without_snapshot(&mut radio);
            let outcome = session
                .verify_missing_snapshot_refusal(FrontPanelKey::Menu)
                .await?;
            assert!(matches!(outcome, GuardedKeyOutcome::ContextChanged { .. }));
            assert!(session.is_valid());
        }
        radio.transport.assert_complete();
        Ok(())
    }

    #[tokio::test(start_paused = true)]
    async fn changed_context_canary_authenticates_refusal_after_safe_tap() -> TestResult {
        let snapshot = automation_snapshot(7, 10, 20)?;
        let refused = command_metadata(
            &snapshot,
            FrontPanelKey::Menu,
            KeyPhase::Press,
            2,
            13,
            26,
            COMMAND_GUARDED_KEY,
            RESULT_CONTEXT_CHANGED,
        )?;
        let mut mock = MockTransport::new();
        mock.expect(b"GM K01,000\r", b"GM K01,00000\r");
        mock.expect(b"GM K01,101\r", b"GM K01,10100\r");
        mock.expect(b"GM G01,002\r", b"GM G01,00202\r");
        queue_range(&mut mock, METADATA_OFFSET, &refused)?;
        queue_range(&mut mock, METADATA_OFFSET, &refused)?;
        let mut radio = Radio::new(mock);
        {
            let mut session = direct_automation_session(&mut radio, &snapshot);
            let outcome = session
                .verify_changed_context_refusal(&snapshot, FrontPanelKey::Menu, FrontPanelKey::Menu)
                .await?;
            let GuardedKeyOutcome::ContextChanged { metadata, receipts } = outcome else {
                return Err("changed context did not return a typed refusal".into());
            };
            assert_eq!(metadata.last_command, COMMAND_GUARDED_KEY);
            assert_eq!(metadata.last_key_result, RESULT_CONTEXT_CHANGED);
            assert_eq!(metadata.command_count, 13);
            assert_eq!(metadata.seqlock, 26);
            assert_eq!(receipts.len(), 1);
            let receipt = receipts.first().ok_or("one changed-context receipt")?;
            assert_eq!(receipt.press_sequence, 2);
            assert_eq!(receipt.release_sequence, None);
            assert!(session.is_valid());
        }
        radio.transport.assert_complete();
        Ok(())
    }

    #[tokio::test(start_paused = true)]
    async fn decimal_route_canary_authenticates_zero_prefix_refusal() -> TestResult {
        let snapshot = automation_snapshot(7, 10, 20)?;
        let route = GuardedDecimalRoute::new([9, 9, 1])?;
        let refused = route_metadata(&snapshot, route, 2, 13, 26, 0, RESULT_CONTEXT_CHANGED)?;
        let mut mock = MockTransport::new();
        mock.expect(b"GM K01,000\r", b"GM K01,00000\r");
        mock.expect(b"GM K01,101\r", b"GM K01,10100\r");
        mock.expect(b"GM R991,02\r", b"GM R991,0202\r");
        queue_range(&mut mock, METADATA_OFFSET, &refused)?;
        queue_range(&mut mock, METADATA_OFFSET, &refused)?;
        let mut radio = Radio::new(mock);
        {
            let mut session = direct_automation_session(&mut radio, &snapshot);
            let outcome = session
                .verify_decimal_route_changed_context_refusal(&snapshot, FrontPanelKey::Menu, route)
                .await?;
            let GuardedDecimalRouteOutcome::ContextChanged(receipt) = outcome else {
                return Err("changed context did not return a decimal-route refusal".into());
            };
            assert_eq!(receipt.route, route);
            assert_eq!(receipt.sequence, 2);
            assert_eq!(receipt.guard_count, 1);
            assert_eq!(receipt.completed_taps, 0);
            assert_eq!(receipt.event_mask, 0);
            assert_eq!(receipt.metadata.command_count, 13);
            assert_eq!(receipt.metadata.seqlock, 26);
            assert_eq!(session.next_key_sequence, 3);
            assert!(session.is_valid());
        }
        radio.transport.assert_complete();
        Ok(())
    }

    #[tokio::test(start_paused = true)]
    async fn decimal_route_canary_unexpected_dispatch_poisons() -> TestResult {
        let snapshot = automation_snapshot(7, 10, 20)?;
        let route = GuardedDecimalRoute::new([9, 9, 1])?;
        let dispatched = route_metadata(&snapshot, route, 2, 13, 26, 3, RESULT_OK)?;
        let mut mock = MockTransport::new();
        mock.expect(b"GM K01,000\r", b"GM K01,00000\r");
        mock.expect(b"GM K01,101\r", b"GM K01,10100\r");
        mock.expect(b"GM R991,02\r", b"GM R991,0200\r");
        queue_range(&mut mock, METADATA_OFFSET, &dispatched)?;
        queue_range(&mut mock, METADATA_OFFSET, &dispatched)?;
        let mut radio = Radio::new(mock);
        {
            let mut session = direct_automation_session(&mut radio, &snapshot);
            let result = session
                .verify_decimal_route_changed_context_refusal(&snapshot, FrontPanelKey::Menu, route)
                .await;
            assert!(result.is_err());
            assert!(!session.is_valid());
        }
        assert!(radio.gm_poisoned);
        radio.transport.assert_complete();
        Ok(())
    }

    #[tokio::test(start_paused = true)]
    async fn guarded_three_key_route_dispatches_and_consumes_lease() -> TestResult {
        let snapshot = automation_snapshot(7, 10, 20)?;
        let keys = [
            FrontPanelKey::Vfo1,
            FrontPanelKey::Mr2,
            FrontPanelKey::Call3,
        ];
        let final_metadata = command_metadata(
            &snapshot,
            FrontPanelKey::Call3,
            KeyPhase::Release,
            5,
            16,
            32,
            COMMAND_KEY,
            RESULT_OK,
        )?;
        let mut mock = MockTransport::new();
        mock.expect(b"GM G0B,000\r", b"GM G0B,00000\r");
        mock.expect(b"GM K0B,101\r", b"GM K0B,10100\r");
        mock.expect(b"GM G0C,002\r", b"GM G0C,00200\r");
        mock.expect(b"GM K0C,103\r", b"GM K0C,10300\r");
        mock.expect(b"GM G0D,004\r", b"GM G0D,00400\r");
        mock.expect(b"GM K0D,105\r", b"GM K0D,10500\r");
        queue_range(&mut mock, METADATA_OFFSET, &final_metadata)?;
        queue_range(&mut mock, METADATA_OFFSET, &final_metadata)?;
        let mut radio = Radio::new(mock);
        {
            let mut session = direct_automation_session(&mut radio, &snapshot);
            let outcome = session.guarded_tap_keys(&snapshot, &keys).await?;
            let GuardedKeyOutcome::Dispatched { metadata, receipts } = outcome else {
                return Err("matching guarded route was not dispatched".into());
            };
            assert_eq!(metadata.command_count, 16);
            assert_eq!(metadata.seqlock, 32);
            assert_eq!(receipts.len(), 3);
            assert!(receipts.iter().all(|receipt| {
                receipt.result == GuardedKeyResult::Dispatched && receipt.release_sequence.is_some()
            }));
            assert!(session.is_valid());

            let reused = session
                .guarded_tap_key(&snapshot, FrontPanelKey::Menu)
                .await;
            assert!(matches!(reused, Err(GuardedKeyError::SnapshotUnavailable)));
            assert!(session.is_valid());
        }
        radio.transport.assert_complete();
        Ok(())
    }

    #[test]
    fn guarded_decimal_route_is_exactly_three_typed_digits() -> TestResult {
        let route = GuardedDecimalRoute::new([0, 4, 9])?;
        assert_eq!(route.digits(), [0, 4, 9]);
        assert_eq!(route.to_string(), "049");
        assert_eq!(route.packed_ascii(), 0x0039_3430);
        assert!(GuardedDecimalRoute::new([0, 10, 9]).is_err());
        Ok(())
    }

    #[test]
    fn route_metadata_accepts_only_atomic_success_or_zero_prefix_refusal() -> TestResult {
        let route = GuardedDecimalRoute::new([9, 8, 0])?;
        let common = MetadataFixture {
            seqlock: 2,
            generation: 7,
            capture_result: RESULT_OK,
            command_count: 1,
            last_command: COMMAND_GUARDED_DECIMAL_ROUTE,
            last_host_sequence: 0xA1,
            route_ascii: route.packed_ascii(),
            route_guard_count: 1,
            ..MetadataFixture::default()
        };

        let success = metadata_bytes_for(
            EXPECTED_ABI,
            MetadataFixture {
                last_key: u32::from(FrontPanelKey::Mark0.as_raw()),
                last_phase: u32::from(KeyPhase::Release.as_raw()),
                last_key_result: RESULT_OK,
                route_completed_taps: 3,
                route_event_mask: 0x3F,
                ..common
            },
        )?;
        let _success_metadata =
            AutomationSession::<MockTransport>::parse_metadata(&success, EXPECTED_ABI)?;

        let refused = metadata_bytes_for(
            EXPECTED_ABI,
            MetadataFixture {
                last_key: u32::from(FrontPanelKey::Pf1_9.as_raw()),
                last_phase: u32::from(KeyPhase::Press.as_raw()),
                last_key_result: RESULT_CONTEXT_CHANGED,
                ..common
            },
        )?;
        let _refused_metadata =
            AutomationSession::<MockTransport>::parse_metadata(&refused, EXPECTED_ABI)?;

        let partial = metadata_bytes_for(
            EXPECTED_ABI,
            MetadataFixture {
                last_key: u32::from(FrontPanelKey::Tone8.as_raw()),
                last_phase: u32::from(KeyPhase::Press.as_raw()),
                last_key_result: RESULT_CONTEXT_CHANGED,
                route_completed_taps: 1,
                route_event_mask: 0x03,
                ..common
            },
        )?;
        assert!(
            AutomationSession::<MockTransport>::parse_metadata(&partial, EXPECTED_ABI).is_err(),
            "ABI 3 must reject every partial-prefix route receipt"
        );
        Ok(())
    }

    #[tokio::test]
    async fn guarded_decimal_route_authenticates_one_complete_command() -> TestResult {
        let snapshot = automation_snapshot(7, 10, 20)?;
        let route = GuardedDecimalRoute::new([9, 9, 1])?;
        let final_metadata = route_metadata(&snapshot, route, 0, 11, 22, 3, RESULT_OK)?;
        let mut mock = MockTransport::new();
        mock.expect(b"GM R991,00\r", b"GM R991,0000\r");
        queue_range(&mut mock, METADATA_OFFSET, &final_metadata)?;
        queue_range(&mut mock, METADATA_OFFSET, &final_metadata)?;
        let mut radio = Radio::new(mock);
        {
            let mut session = direct_automation_session(&mut radio, &snapshot);
            let outcome = session.guarded_decimal_route(&snapshot, route).await?;
            let GuardedDecimalRouteOutcome::Dispatched(receipt) = outcome else {
                return Err("matching batch route was not dispatched".into());
            };
            assert_eq!(receipt.route, route);
            assert_eq!(receipt.sequence, 0);
            assert_eq!(receipt.guard_count, 1);
            assert_eq!(receipt.completed_taps, 3);
            assert_eq!(receipt.event_mask, 0x3F);
            assert_eq!(receipt.metadata.command_count, 11);
            assert_eq!(receipt.metadata.seqlock, 22);
            assert_eq!(session.last_command_count, 11);
            assert_eq!(session.last_seqlock, 22);
            assert!(session.guarded_input_lease.is_none());
            assert!(session.is_valid());
        }
        radio.transport.assert_complete();
        Ok(())
    }

    #[tokio::test]
    async fn guarded_decimal_route_zero_digit_refusal_remains_usable() -> TestResult {
        let snapshot = automation_snapshot(7, 10, 20)?;
        let route = GuardedDecimalRoute::new([9, 9, 1])?;
        let refused = route_metadata(&snapshot, route, 0, 11, 22, 0, RESULT_CONTEXT_CHANGED)?;
        let mut mock = MockTransport::new();
        mock.expect(b"GM R991,00\r", b"GM R991,0002\r");
        queue_range(&mut mock, METADATA_OFFSET, &refused)?;
        queue_range(&mut mock, METADATA_OFFSET, &refused)?;
        let mut radio = Radio::new(mock);
        {
            let mut session = direct_automation_session(&mut radio, &snapshot);
            let outcome = session.guarded_decimal_route(&snapshot, route).await?;
            let GuardedDecimalRouteOutcome::ContextChanged(receipt) = &outcome else {
                return Err("changed framebuffer did not return a zero-digit refusal".into());
            };
            assert_eq!(receipt.guard_count, 1);
            assert_eq!(receipt.completed_taps, 0);
            assert_eq!(receipt.event_mask, 0);
            assert!(!outcome.requires_recovery());
            assert!(session.is_valid());
            assert!(matches!(
                session.guarded_decimal_route(&snapshot, route).await,
                Err(GuardedKeyError::SnapshotUnavailable)
            ));
            assert!(session.is_valid());
        }
        assert!(!radio.gm_poisoned);
        radio.transport.assert_complete();
        Ok(())
    }

    #[tokio::test(start_paused = true)]
    async fn guarded_decimal_route_timeout_is_ambiguous_and_poisons() -> TestResult {
        let snapshot = automation_snapshot(7, 10, 20)?;
        let route = GuardedDecimalRoute::new([9, 9, 1])?;
        let mut mock = MockTransport::new();
        mock.queue_read_delayed(
            b"GM R991,0000\r",
            u64::try_from(GUARDED_ROUTE_MAX_DURATION.as_millis())? + 1,
        );
        mock.expect(b"GM R991,00\r", b"");
        let mut radio = Radio::new(mock);
        {
            let mut session = direct_automation_session(&mut radio, &snapshot);
            let result = session.guarded_decimal_route(&snapshot, route).await;
            assert!(matches!(
                result,
                Err(GuardedKeyError::Automation(Error::Timeout(timeout)))
                    if timeout == GUARDED_ROUTE_MAX_DURATION
            ));
            assert!(!session.is_valid());
        }
        assert!(matches!(
            radio.identify().await,
            Err(Error::MemoryReadStreamPoisoned)
        ));
        radio.transport.assert_complete();
        Ok(())
    }

    #[tokio::test]
    async fn guarded_context_change_is_authenticated_without_a_release() -> TestResult {
        let snapshot = automation_snapshot(7, 10, 20)?;
        let changed = command_metadata(
            &snapshot,
            FrontPanelKey::Menu,
            KeyPhase::Press,
            0,
            11,
            22,
            COMMAND_GUARDED_KEY,
            RESULT_CONTEXT_CHANGED,
        )?;
        let mut mock = MockTransport::new();
        mock.expect(b"GM G01,000\r", b"GM G01,00002\r");
        queue_range(&mut mock, METADATA_OFFSET, &changed)?;
        queue_range(&mut mock, METADATA_OFFSET, &changed)?;
        let mut radio = Radio::new(mock);
        {
            let mut session = direct_automation_session(&mut radio, &snapshot);
            let outcome = session
                .guarded_tap_key(&snapshot, FrontPanelKey::Menu)
                .await?;
            let GuardedKeyOutcome::ContextChanged { metadata, receipts } = outcome else {
                return Err("changed framebuffer did not return a typed refusal".into());
            };
            assert_eq!(metadata.last_command, COMMAND_GUARDED_KEY);
            assert_eq!(metadata.last_key_result, RESULT_CONTEXT_CHANGED);
            assert_eq!(metadata.command_count, 11);
            assert_eq!(metadata.seqlock, 22);
            assert_eq!(receipts.len(), 1);
            let receipt = receipts.first().ok_or("one guarded refusal receipt")?;
            assert_eq!(receipt.release_sequence, None);
            assert_eq!(receipt.result, GuardedKeyResult::ContextChanged);
            assert!(session.is_valid());
            assert_eq!(session.last_command_count, 11);
            assert_eq!(session.last_seqlock, 22);
            assert!(session.guarded_input_lease.is_none());
        }
        radio.transport.assert_complete();
        Ok(())
    }

    #[tokio::test(start_paused = true)]
    async fn guarded_press_deadline_cancellation_poisons_session_and_stream() -> TestResult {
        let snapshot = automation_snapshot(7, 10, 20)?;
        let mut mock = MockTransport::new();
        mock.queue_read_delayed(
            b"GM G0B,00000\r",
            u64::try_from(GUARDED_ROUTE_MAX_DURATION.as_millis())? + 1,
        );
        mock.expect(b"GM G0B,000\r", b"");
        let mut radio = Radio::new(mock);
        {
            let mut session = direct_automation_session(&mut radio, &snapshot);
            let result = session
                .guarded_tap_keys(&snapshot, &[FrontPanelKey::Vfo1, FrontPanelKey::Mr2])
                .await;
            assert!(matches!(
                result,
                Err(GuardedKeyError::Automation(Error::Timeout(timeout)))
                    if timeout == GUARDED_ROUTE_MAX_DURATION
            ));
            assert!(
                !session.is_valid(),
                "cancelling an in-flight guarded press leaves dispatch unknowable"
            );
        }
        assert!(matches!(
            radio.identify().await,
            Err(Error::MemoryReadStreamPoisoned)
        ));
        radio.transport.assert_complete();
        Ok(())
    }

    #[tokio::test(start_paused = true)]
    async fn guarded_only_tap_crossing_deadline_is_released_and_authenticated() -> TestResult {
        let snapshot = automation_snapshot(7, 10, 20)?;
        let final_release = command_metadata(
            &snapshot,
            FrontPanelKey::Menu,
            KeyPhase::Release,
            1,
            12,
            24,
            COMMAND_KEY,
            RESULT_OK,
        )?;
        let press_delay = u64::try_from(
            GUARDED_ROUTE_MAX_DURATION
                .saturating_sub(Duration::from_millis(20))
                .as_millis(),
        )?;
        let mut mock = MockTransport::new();
        mock.queue_read_delayed(b"GM G01,00000\r", press_delay);
        mock.expect(b"GM G01,000\r", b"");
        mock.expect(b"GM K01,101\r", b"GM K01,10100\r");
        queue_range(&mut mock, METADATA_OFFSET, &final_release)?;
        queue_range(&mut mock, METADATA_OFFSET, &final_release)?;
        let mut radio = Radio::new(mock);
        {
            let mut session = direct_automation_session(&mut radio, &snapshot);
            let outcome = session
                .guarded_tap_key(&snapshot, FrontPanelKey::Menu)
                .await?;
            let GuardedKeyOutcome::DeadlineExpired { metadata, receipts } = outcome else {
                return Err("overdue final tap did not return its authenticated prefix".into());
            };
            assert_eq!(metadata.command_count, 12);
            assert_eq!(receipts.len(), 1);
            let receipt = receipts.first().ok_or("one overdue tap receipt")?;
            assert_eq!(receipt.release_sequence, Some(1));
            assert!(session.is_valid());
        }
        radio.transport.assert_complete();
        Ok(())
    }

    #[tokio::test(start_paused = true)]
    async fn slow_guarded_release_prevents_the_next_press() -> TestResult {
        let snapshot = automation_snapshot(7, 10, 20)?;
        let first_release = command_metadata(
            &snapshot,
            FrontPanelKey::Vfo1,
            KeyPhase::Release,
            1,
            12,
            24,
            COMMAND_KEY,
            RESULT_OK,
        )?;
        let mut mock = MockTransport::new();
        mock.queue_read(b"GM G0B,00000\r");
        mock.queue_read_delayed(
            b"GM K0B,10100\r",
            u64::try_from(GUARDED_ROUTE_MAX_DURATION.as_millis())? + 1,
        );
        mock.expect(b"GM G0B,000\r", b"");
        mock.expect(b"GM K0B,101\r", b"");
        queue_range(&mut mock, METADATA_OFFSET, &first_release)?;
        queue_range(&mut mock, METADATA_OFFSET, &first_release)?;
        let mut radio = Radio::new(mock);
        {
            let mut session = direct_automation_session(&mut radio, &snapshot);
            let outcome = session
                .guarded_tap_keys(&snapshot, &[FrontPanelKey::Vfo1, FrontPanelKey::Mr2])
                .await?;
            let GuardedKeyOutcome::DeadlineExpired { metadata, receipts } = outcome else {
                return Err("slow release did not stop the remaining guarded route".into());
            };
            assert_eq!(metadata.command_count, 12);
            assert_eq!(receipts.len(), 1);
            let receipt = receipts.first().ok_or("one slow-release receipt")?;
            assert_eq!(receipt.key, FrontPanelKey::Vfo1);
            assert!(session.is_valid());
        }
        radio.transport.assert_complete();
        Ok(())
    }

    #[tokio::test(start_paused = true)]
    async fn stale_or_wrong_guarded_snapshot_is_refused_before_io() -> TestResult {
        let snapshot = automation_snapshot(7, 10, 20)?;
        let mut wrong = snapshot.clone();
        wrong.metadata.command_count = wrong.metadata.command_count.wrapping_add(1);
        let mock = MockTransport::new();
        let mut radio = Radio::new(mock);
        {
            let mut session = direct_automation_session(&mut radio, &snapshot);
            let wrong_result = session.guarded_tap_key(&wrong, FrontPanelKey::Menu).await;
            assert!(matches!(
                wrong_result,
                Err(GuardedKeyError::SnapshotReceiptMismatch)
            ));
            assert!(session.is_valid());

            tokio::time::advance(GUARDED_SNAPSHOT_MAX_AGE + Duration::from_millis(1)).await;
            let stale = session
                .guarded_tap_key(&snapshot, FrontPanelKey::Menu)
                .await;
            assert!(matches!(
                stale,
                Err(GuardedKeyError::SnapshotExpired { .. })
            ));
            assert!(session.is_valid());
            assert!(session.guarded_input_lease.is_none());
        }
        radio.transport.assert_complete();
        Ok(())
    }

    #[tokio::test(start_paused = true)]
    async fn cancelled_guarded_release_poisons_session_and_stream() -> TestResult {
        let snapshot = automation_snapshot(7, 10, 20)?;
        let mut mock = MockTransport::new();
        mock.expect(b"GM G01,000\r", b"GM G01,00000\r");
        mock.expect_hang(b"GM K01,101\r");
        let mut radio = Radio::new(mock);
        {
            let mut session = direct_automation_session(&mut radio, &snapshot);
            let cancelled = tokio::time::timeout(
                Duration::from_millis(41),
                session.guarded_tap_key(&snapshot, FrontPanelKey::Menu),
            )
            .await;
            assert!(cancelled.is_err(), "outer cancellation must win");
            assert!(!session.is_valid());
        }
        assert!(matches!(
            radio.identify().await,
            Err(Error::MemoryReadStreamPoisoned)
        ));
        Ok(())
    }

    #[tokio::test]
    async fn key_event_validates_exact_ack_and_stable_metadata() -> TestResult {
        let key = FrontPanelKey::Menu;
        let metadata = key_metadata(7, key, KeyPhase::Press, 0, 1)?;
        let mut mock = MockTransport::new();
        mock.expect(b"GM K01,000\r", b"GM K01,00000\r");
        queue_range(&mut mock, METADATA_OFFSET, &metadata)?;
        queue_range(&mut mock, METADATA_OFFSET, &metadata)?;
        let mut radio = Radio::new(mock);
        let mut session = direct_session(&mut radio, 7);
        let observed = session.key_event(key, KeyPhase::Press).await?;
        assert_eq!(observed.last_key, 1);
        assert_eq!(observed.last_phase, 0);
        assert!(session.is_valid());
        Ok(())
    }

    #[tokio::test]
    async fn key_event_rejects_command_count_or_seqlock_discontinuity() -> TestResult {
        let key = FrontPanelKey::Menu;
        for (command_count, seqlock) in [(2, 2), (1, 4)] {
            let metadata =
                key_metadata_with_seqlock(7, key, KeyPhase::Press, 0, command_count, seqlock)?;
            let mut mock = MockTransport::new();
            mock.expect(b"GM K01,000\r", b"GM K01,00000\r");
            queue_range(&mut mock, METADATA_OFFSET, &metadata)?;
            queue_range(&mut mock, METADATA_OFFSET, &metadata)?;
            let mut radio = Radio::new(mock);
            let mut session = direct_session(&mut radio, 7);
            let result = session.key_event(key, KeyPhase::Press).await;
            assert!(
                result.is_err(),
                "receipt {command_count}/{seqlock} skipped exact continuity"
            );
            assert!(!session.is_valid());
        }
        Ok(())
    }

    #[tokio::test]
    async fn key_event_accepts_exact_wrapping_receipt_continuity() -> TestResult {
        let key = FrontPanelKey::Menu;
        let metadata = key_metadata_with_seqlock(7, key, KeyPhase::Press, 0, 0, 0)?;
        let mut mock = MockTransport::new();
        mock.expect(b"GM K01,000\r", b"GM K01,00000\r");
        queue_range(&mut mock, METADATA_OFFSET, &metadata)?;
        queue_range(&mut mock, METADATA_OFFSET, &metadata)?;
        let mut radio = Radio::new(mock);
        let mut session = direct_session_with_receipt(&mut radio, 7, u32::MAX, u32::MAX - 1);
        let observed = session.key_event(key, KeyPhase::Press).await?;
        assert_eq!(observed.command_count, 0);
        assert_eq!(observed.seqlock, 0);
        assert!(session.is_valid());
        Ok(())
    }

    #[tokio::test]
    async fn wrong_key_echo_poisons_session_and_stream() -> TestResult {
        let mut mock = MockTransport::new();
        mock.expect(b"GM K01,000\r", b"GM K01,00100\r");
        let mut radio = Radio::new(mock);
        {
            let mut session = direct_session(&mut radio, 7);
            let result = session
                .key_event(FrontPanelKey::Menu, KeyPhase::Press)
                .await;
            assert!(result.is_err(), "wrong key echo must fail");
            assert!(!session.is_valid(), "wrong key echo must poison capability");
        }
        assert!(matches!(
            radio.identify().await,
            Err(Error::MemoryReadStreamPoisoned)
        ));
        Ok(())
    }

    #[tokio::test(start_paused = true)]
    async fn tap_key_owns_press_hold_release_sequences() -> TestResult {
        let key = FrontPanelKey::Menu;
        let release = key_metadata(7, key, KeyPhase::Release, 1, 2)?;
        let mut mock = MockTransport::new();
        mock.expect(b"GM K01,000\r", b"GM K01,00000\r");
        mock.expect(b"GM K01,101\r", b"GM K01,10100\r");
        queue_range(&mut mock, METADATA_OFFSET, &release)?;
        queue_range(&mut mock, METADATA_OFFSET, &release)?;
        let mut radio = Radio::new(mock);
        let mut session = direct_session(&mut radio, 7);
        let observed = session.tap_key(key).await?;
        assert_eq!(observed.last_phase, 1);
        assert_eq!(observed.last_host_sequence, 1);
        assert_eq!(observed.command_count, 2);
        assert_eq!(observed.seqlock, 4);
        assert!(session.is_valid());
        Ok(())
    }

    #[tokio::test(start_paused = true)]
    async fn tap_key_requires_two_command_aggregate_receipt() -> TestResult {
        let key = FrontPanelKey::Menu;
        let release = key_metadata(7, key, KeyPhase::Release, 1, 1)?;
        let mut mock = MockTransport::new();
        mock.expect(b"GM K01,000\r", b"GM K01,00000\r");
        mock.expect(b"GM K01,101\r", b"GM K01,10100\r");
        queue_range(&mut mock, METADATA_OFFSET, &release)?;
        queue_range(&mut mock, METADATA_OFFSET, &release)?;
        let mut radio = Radio::new(mock);
        let mut session = direct_session(&mut radio, 7);
        let result = session.tap_key(key).await;
        assert!(result.is_err());
        assert!(!session.is_valid());
        Ok(())
    }

    #[tokio::test(start_paused = true)]
    async fn tap_key_continuity_survives_more_than_three_hundred_taps() -> TestResult {
        const TAP_COUNT: u32 = 301;

        let key = FrontPanelKey::Menu;
        let mut mock = MockTransport::new();
        for tap_index in 0..TAP_COUNT {
            let press_sequence = u8::try_from(tap_index.wrapping_mul(2) & 0xFF)?;
            let release_sequence = press_sequence.wrapping_add(1);
            let command_count = tap_index.wrapping_add(1).wrapping_mul(2);
            let seqlock = tap_index.wrapping_add(1).wrapping_mul(4);
            let press_request = format!("GM K01,0{press_sequence:02X}\r");
            let press_reply = format!("GM K01,0{press_sequence:02X}00\r");
            let release_request = format!("GM K01,1{release_sequence:02X}\r");
            let release_reply = format!("GM K01,1{release_sequence:02X}00\r");
            let release = key_metadata_with_seqlock(
                7,
                key,
                KeyPhase::Release,
                release_sequence,
                command_count,
                seqlock,
            )?;

            mock.expect(press_request.as_bytes(), press_reply.as_bytes());
            mock.expect(release_request.as_bytes(), release_reply.as_bytes());
            queue_range(&mut mock, METADATA_OFFSET, &release)?;
            queue_range(&mut mock, METADATA_OFFSET, &release)?;
        }

        let mut radio = Radio::new(mock);
        {
            let mut session = direct_session(&mut radio, 7);
            for tap_index in 0..TAP_COUNT {
                let observed = session.tap_key(key).await?;
                assert_eq!(observed.command_count, tap_index.wrapping_add(1) * 2);
                assert_eq!(observed.seqlock, tap_index.wrapping_add(1) * 4);
            }
            assert_eq!(session.next_key_sequence, 0x5A);
            assert_eq!(session.last_command_count, 602);
            assert_eq!(session.last_seqlock, 1_204);
            assert!(session.is_valid());
        }
        radio.transport.assert_complete();
        Ok(())
    }

    #[tokio::test(start_paused = true)]
    async fn cancelled_tap_poisons_session_and_underlying_stream() -> TestResult {
        let key = FrontPanelKey::Menu;
        let mut mock = MockTransport::new();
        mock.expect(b"GM K01,000\r", b"GM K01,00000\r");
        mock.expect_hang(b"GM K01,101\r");
        let mut radio = Radio::new(mock);
        {
            let mut session = direct_session(&mut radio, 7);
            let cancelled =
                tokio::time::timeout(Duration::from_millis(41), session.tap_key(key)).await;
            assert!(cancelled.is_err(), "outer cancellation must win");
            assert!(!session.is_valid(), "cancelled tap must poison capability");
        }
        let ordinary = radio.identify().await;
        assert!(matches!(ordinary, Err(Error::MemoryReadStreamPoisoned)));
        Ok(())
    }

    #[tokio::test]
    async fn raw_snapshot_builds_screen_and_validates_crc() -> TestResult {
        let (pixels, crc) = zero_frame()?;
        let metadata = snapshot_metadata(8, 0, crc, 0)?;
        let mut mock = MockTransport::new();
        mock.expect(b"GM S000000\r", b"GM S00000000\r");
        queue_range(&mut mock, METADATA_OFFSET, &metadata)?;
        queue_range(&mut mock, PIXEL_OFFSET, &pixels)?;
        queue_range(&mut mock, METADATA_OFFSET, &metadata)?;
        let mut radio = Radio::new(mock);
        let mut session = direct_session(&mut radio, 7);
        let snapshot = session.capture_screen().await?;
        assert_eq!(snapshot.frame.rgb565_le(), pixels);
        assert_eq!(snapshot.metadata.generation, 8);
        assert_eq!(snapshot.metadata.crc32, crc);
        assert!(session.is_valid());
        Ok(())
    }

    #[tokio::test]
    async fn rle_snapshot_decodes_exact_frame_and_validates_raw_crc() -> TestResult {
        let (pixels, crc) = zero_frame()?;
        let encoded = encode_solid_frame(0, 0)?;
        assert_eq!(encoded.len(), 510);
        let encoded_length = u32::try_from(encoded.len())?;
        let metadata = snapshot_metadata(8, 0, crc, encoded_length)?;
        let mut mock = MockTransport::new();
        mock.expect(b"GM S000000\r", b"GM S00000000\r");
        queue_range(&mut mock, METADATA_OFFSET, &metadata)?;
        queue_range(&mut mock, RLE_OFFSET, &encoded)?;
        queue_range(&mut mock, METADATA_OFFSET, &metadata)?;
        let mut radio = Radio::new(mock);
        let mut session = direct_session(&mut radio, 7);
        let snapshot = session.capture_screen().await?;
        assert_eq!(snapshot.frame.rgb565_le(), pixels);
        assert_eq!(snapshot.metadata.rle_encoded_length, encoded_length);
        assert!(session.is_valid());
        Ok(())
    }

    #[test]
    fn rle_decoder_rejects_zero_run_overflow_and_underflow() -> TestResult {
        let zero_run = AutomationSession::<MockTransport>::decode_rle(&[0, 0, 0]);
        assert!(zero_run.is_err());

        let mut overflow = encode_solid_frame(0, 0)?;
        overflow.extend_from_slice(&[1, 0, 0]);
        assert!(AutomationSession::<MockTransport>::decode_rle(&overflow).is_err());

        let underflow = AutomationSession::<MockTransport>::decode_rle(&[1, 0, 0]);
        assert!(underflow.is_err());
        assert!(AutomationSession::<MockTransport>::decode_rle(&[1, 0]).is_err());
        Ok(())
    }

    #[tokio::test(start_paused = true)]
    async fn validated_unstable_results_retry_and_leave_session_usable() -> TestResult {
        let mut mock = MockTransport::new();
        for sequence in 0_u32..3 {
            let request = format!("GM S{sequence:06X}\r");
            let response = format!("GM S{sequence:06X}01\r");
            mock.expect(request.as_bytes(), response.as_bytes());
            let metadata = unstable_metadata(7, sequence, sequence + 1)?;
            queue_range(&mut mock, METADATA_OFFSET, &metadata)?;
            queue_range(&mut mock, METADATA_OFFSET, &metadata)?;
        }
        let mut radio = Radio::new(mock);
        let mut session = direct_session(&mut radio, 7);
        let result = session.capture_screen().await;
        assert!(matches!(
            result,
            Err(Error::AutomationScreenUnstable { attempts: 3 })
        ));
        assert!(
            session.is_valid(),
            "validated unstable status is not corruption"
        );
        assert_eq!(session.next_snapshot_sequence, 3);
        assert_eq!(session.last_command_count, 3);
        assert_eq!(session.last_seqlock, 6);
        Ok(())
    }

    #[tokio::test]
    async fn unstable_snapshot_requires_zero_rle_length() -> TestResult {
        let metadata = metadata_bytes(MetadataFixture {
            seqlock: 2,
            generation: 7,
            capture_result: 1,
            crc32: 0,
            capture_attempts: MAX_CAPTURE_ATTEMPTS,
            command_count: 1,
            last_command: COMMAND_SNAPSHOT,
            last_host_sequence: 0,
            rle_encoded_length: 3,
            ..MetadataFixture::default()
        })?;
        let mut mock = MockTransport::new();
        mock.expect(b"GM S000000\r", b"GM S00000001\r");
        queue_range(&mut mock, METADATA_OFFSET, &metadata)?;
        let mut radio = Radio::new(mock);
        let mut session = direct_session(&mut radio, 7);
        let result = session.capture_screen().await;
        assert!(result.is_err());
        assert!(!session.is_valid());
        Ok(())
    }

    #[tokio::test]
    async fn malformed_snapshot_status_poisons() -> TestResult {
        let mut mock = MockTransport::new();
        mock.expect(b"GM S000000\r", b"GM S00000002\r");
        let mut radio = Radio::new(mock);
        let mut session = direct_session(&mut radio, 7);
        let result = session.capture_screen().await;
        assert!(result.is_err());
        assert!(!session.is_valid());
        Ok(())
    }

    #[tokio::test]
    async fn wrong_generation_poisons_before_pixel_reads() -> TestResult {
        let metadata = snapshot_metadata(9, 0, 0, 0)?;
        let mut mock = MockTransport::new();
        mock.expect(b"GM S000000\r", b"GM S00000000\r");
        queue_range(&mut mock, METADATA_OFFSET, &metadata)?;
        let mut radio = Radio::new(mock);
        let mut session = direct_session(&mut radio, 7);
        let result = session.capture_screen().await;
        assert!(result.is_err());
        assert!(!session.is_valid());
        Ok(())
    }

    #[tokio::test]
    async fn snapshot_rejects_command_count_or_seqlock_discontinuity() -> TestResult {
        for (command_count, seqlock) in [(2, 2), (1, 4)] {
            let metadata = metadata_bytes(MetadataFixture {
                seqlock,
                generation: 8,
                capture_result: RESULT_OK,
                crc32: 0,
                capture_attempts: 1,
                command_count,
                last_command: COMMAND_SNAPSHOT,
                last_host_sequence: 0,
                ..MetadataFixture::default()
            })?;
            let mut mock = MockTransport::new();
            mock.expect(b"GM S000000\r", b"GM S00000000\r");
            queue_range(&mut mock, METADATA_OFFSET, &metadata)?;
            let mut radio = Radio::new(mock);
            let mut session = direct_session(&mut radio, 7);
            let result = session.capture_screen().await;
            assert!(
                result.is_err(),
                "receipt {command_count}/{seqlock} skipped exact continuity"
            );
            assert!(!session.is_valid());
        }
        Ok(())
    }

    #[tokio::test]
    async fn metadata_change_during_rle_transfer_poisons() -> TestResult {
        let (_pixels, crc) = zero_frame()?;
        let encoded = encode_solid_frame(0, 0)?;
        let encoded_length = u32::try_from(encoded.len())?;
        let before = snapshot_metadata(8, 0, crc, encoded_length)?;
        let after = metadata_bytes(MetadataFixture {
            seqlock: 4,
            generation: 8,
            capture_result: RESULT_OK,
            crc32: crc,
            capture_attempts: 1,
            command_count: 1,
            last_command: COMMAND_SNAPSHOT,
            rle_encoded_length: encoded_length,
            ..MetadataFixture::default()
        })?;
        let mut mock = MockTransport::new();
        mock.expect(b"GM S000000\r", b"GM S00000000\r");
        queue_range(&mut mock, METADATA_OFFSET, &before)?;
        queue_range(&mut mock, RLE_OFFSET, &encoded)?;
        queue_range(&mut mock, METADATA_OFFSET, &after)?;
        let mut radio = Radio::new(mock);
        let mut session = direct_session(&mut radio, 7);
        let result = session.capture_screen().await;
        assert!(result.is_err());
        assert!(!session.is_valid());
        Ok(())
    }

    #[tokio::test]
    async fn crc_mismatch_poisons_after_complete_rle_transfer() -> TestResult {
        let encoded = encode_solid_frame(0, 0)?;
        let encoded_length = u32::try_from(encoded.len())?;
        let metadata = snapshot_metadata(8, 0, 0xDEAD_BEEF, encoded_length)?;
        let mut mock = MockTransport::new();
        mock.expect(b"GM S000000\r", b"GM S00000000\r");
        queue_range(&mut mock, METADATA_OFFSET, &metadata)?;
        queue_range(&mut mock, RLE_OFFSET, &encoded)?;
        queue_range(&mut mock, METADATA_OFFSET, &metadata)?;
        let mut radio = Radio::new(mock);
        let mut session = direct_session(&mut radio, 7);
        let result = session.capture_screen().await;
        assert!(result.is_err());
        assert!(!session.is_valid());
        Ok(())
    }

    #[tokio::test(start_paused = true)]
    async fn cancelled_snapshot_poisons_session_and_stream() -> TestResult {
        let mut mock = MockTransport::new();
        mock.expect_hang(b"GM S000000\r");
        let mut radio = Radio::new(mock);
        {
            let mut session = direct_session(&mut radio, 7);
            let cancelled =
                tokio::time::timeout(Duration::from_millis(1), session.capture_screen()).await;
            assert!(cancelled.is_err());
            assert!(!session.is_valid());
        }
        assert!(matches!(
            radio.identify().await,
            Err(Error::MemoryReadStreamPoisoned)
        ));
        Ok(())
    }
}
