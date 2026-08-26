//! Error types for the kenwood-thd75 library.
//!
//! This module defines an error hierarchy that mirrors the library's
//! architecture:
//!
//! 1. **[`enum@Error`]**: the top-level enum returned by all public API
//!    methods. It wraps the lower-level categories below, plus
//!    radio-specific conditions like [`Error::CommandRejected`] (`?` response),
//!    [`Error::NotAvailableInCurrentMode`] (`N` response), [`Error::Timeout`], and
//!    MCP memory-related errors.
//!
//! 2. **[`TransportError`]**: failures in the serial/Bluetooth I/O
//!    layer. These occur when opening, reading from, or writing to the
//!    serial port. A `TransportError` generally means the physical link
//!    is broken or was never established. Wrapped by
//!    [`Error::Transport`].
//!
//! 3. **[`ProtocolError`]**: failures in CAT command framing and
//!    parsing. These occur when the radio sends a response that cannot
//!    be decoded: wrong field count, unparseable field value, unknown
//!    command prefix, or a malformed frame (e.g., missing `\r`
//!    terminator). Wrapped by [`Error::Protocol`].
//!
//! 4. **[`ValidationError`]**: failures when a caller-supplied value
//!    is outside the valid range for its type (e.g., band index > 1,
//!    tone code > 49, power level > 3). These are raised **before** any
//!    I/O occurs, during construction of typed wrappers. Wrapped by
//!    [`Error::Validation`].
//!
//! 5. **[`kiss_tnc::KissError`]**: failures while decoding the binary KISS
//!    stream. Malformed and oversized frames retain their precise decoder
//!    error and are wrapped by [`Error::Kiss`].
//!
//! Wrapped lower-level error types implement `From` conversion into
//! [`enum@Error`], so the `?` operator propagates them naturally.

use std::time::Duration;

use crate::protocol::programming::{McpPage, WritableMcpPage};

use thiserror::Error;

/// Top-level error type for all radio operations.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum Error {
    /// A transport-layer (serial/Bluetooth) error occurred.
    #[error(transparent)]
    Transport(#[from] TransportError),

    /// A protocol-layer error occurred while parsing or encoding a command.
    #[error(transparent)]
    Protocol(#[from] ProtocolError),

    /// A validation error occurred on a user-supplied value.
    #[error(transparent)]
    Validation(#[from] ValidationError),

    /// A received KISS frame was malformed or exceeded the framing limit.
    #[cfg(feature = "aprs")]
    #[error(transparent)]
    Kiss(#[from] kiss_tnc::KissError),

    /// An APRS-IS packet line was malformed or used noncanonical AX.25
    /// identities.
    #[cfg(feature = "aprs")]
    #[error(transparent)]
    AprsIsLine(#[from] aprs_is::AprsIsLineError),

    /// An APRS information field was malformed for its declared data type.
    #[error(transparent)]
    AprsPacket(#[from] aprs::AprsError),

    /// The APRS client was constructed receive-only and cannot transmit.
    #[cfg(feature = "aprs")]
    #[error(
        "APRS client is receive-only; construct its configuration with a \
         station identity to transmit"
    )]
    ReceiveOnly,

    /// Wrapping an APRS-IS packet as a third-party RF packet would exceed the
    /// AX.25 information-field limit.
    #[error(
        "APRS-IS third-party packet is too large for RF: {actual} information bytes (max {maximum})"
    )]
    AprsThirdPartyInformationTooLong {
        /// Length after adding the third-party `TCPIP,IGATE*` wrapper.
        actual: usize,
        /// AX.25 information-field maximum (`256`).
        maximum: usize,
    },

    /// Comparing two state snapshots failed.
    #[error(transparent)]
    Verify(#[from] crate::verify::VerifyError),

    /// The radio rejected the command with `?\r`.
    #[error("radio rejected the {mnemonic} command (`?` response)")]
    CommandRejected {
        /// CAT mnemonic of the rejected command (e.g. `FQ`, `VM`).
        mnemonic: String,
    },

    /// Recalling a memory channel was refused because the channel is
    /// empty (0 Hz receive frequency); recalling it would leave the
    /// radio in an unusable state.
    #[error("memory channel {channel} is empty")]
    EmptyMemoryChannel {
        /// The empty channel.
        channel: crate::types::RegularChannel,
    },

    /// The radio returned "not available" (`N\r`): command not supported in current mode.
    #[error("{mnemonic} command not available in current radio mode")]
    NotAvailableInCurrentMode {
        /// CAT mnemonic of the refused command (e.g. `ID`, `UP`).
        mnemonic: String,
    },

    /// A same-session MCP precondition proved that Menu 983 routes KISS to a
    /// different host interface. The guarded operation performed zero writes.
    #[error(
        "Menu 983 routes KISS to raw interface {actual}, not the approved {expected} interface; no setting was changed"
    )]
    KissInterfaceMismatch {
        /// Host interface approved by the caller.
        expected: crate::types::PcOutputInterface,
        /// Live raw Menu 983 value read in the guarded MCP transaction.
        actual: u8,
    },

    /// A same-session MCP read proved that Menu 506 is outside the strict A/B
    /// domain. The guarded operation performed zero writes.
    #[error("Menu 506 has invalid raw TNC data band {actual}; no setting was changed")]
    InvalidTncDataBand {
        /// Live raw Menu 506 value read in the guarded MCP transaction.
        actual: u8,
    },

    /// A mode write returned a semantic rejection and its required readback
    /// also failed, so the resulting radio state could not be proved.
    #[error(
        "operating-mode write for band {band} requested {requested} and returned {rejection}; \
         immediate readback also failed: {readback}"
    )]
    OperatingModeWriteUnconfirmed {
        /// Band targeted by the write.
        band: crate::types::Band,
        /// Operating mode requested by the caller.
        requested: crate::types::OperatingMode,
        /// Semantic response returned for the write (`?` or `N`).
        rejection: Box<Self>,
        /// Failure that prevented immediate state readback.
        #[source]
        readback: Box<Self>,
    },

    /// A high-level CAT operation is unavailable because this firmware
    /// repurposes the command mnemonic for a different protocol.
    #[error("CAT command {command} is unavailable on firmware {firmware}")]
    CommandUnavailableOnFirmware {
        /// The colliding CAT command mnemonic.
        command: &'static str,
        /// Exact firmware identity returned by `FV`.
        firmware: crate::types::FirmwareIdentity,
    },

    /// A command timed out waiting for a response.
    #[error("command timed out after {0:?}")]
    Timeout(Duration),

    /// A CAT exchange or binary-mode transition ended at an uncertain boundary.
    ///
    /// A command future may have been cancelled after its write began, or a
    /// binary-mode entry/exit command may have reached the radio without a
    /// correlated reply. Ordinary CAT traffic is blocked until the transport
    /// is recovered and an isolated TH-D75 identity exchange succeeds.
    #[error(
        "the CAT stream has an unresolved in-flight exchange or binary-mode transition; \
         call Radio::recover_cat before sending another command"
    )]
    CatRecoveryRequired,

    /// A caller attempted to wrap a CAT-ready transport as a binary session
    /// without first proving or completing the corresponding mode transition.
    #[error(
        "binary mode has not been proved on this link; complete an owned mode transition or a \
         successful binary link diagnosis first"
    )]
    BinaryModeNotProven,

    /// The radio has not been identified yet; call `identify()` first.
    #[error("radio not identified; call identify() first")]
    NotIdentified,

    /// A write was attempted to a protected memory region (factory calibration).
    #[error("write to protected page 0x{page:04X} denied (factory calibration region)")]
    McpWriteProtected {
        /// The page address that was denied.
        page: McpPage,
    },

    /// The radio did not ACK a write command.
    #[error("write to page 0x{page:04X} not acknowledged (expected ACK 0x06, got 0x{got:02X})")]
    McpWriteNotAcknowledged {
        /// The page address that was being written.
        page: WritableMcpPage,
        /// The byte received instead of ACK.
        got: u8,
    },

    /// An MCP write's read-back verification found a differing byte.
    ///
    /// The radio acknowledged the write, but reading the page back
    /// shows the byte did not land. The cached memory image is left
    /// unpatched.
    #[error(
        "MCP verify mismatch on page 0x{page:04X} at offset 0x{offset:02X}: \
         wrote 0x{expected:02X}, read back 0x{actual:02X}"
    )]
    McpVerifyMismatch {
        /// The page address that was written.
        page: WritableMcpPage,
        /// The first differing byte offset within the page.
        offset: usize,
        /// The byte that was written.
        expected: u8,
        /// The byte the read-back returned.
        actual: u8,
    },

    /// The supplied memory image has an invalid size.
    #[error("invalid memory image size: {actual} bytes (expected {expected})")]
    McpInvalidImageSize {
        /// The actual size in bytes.
        actual: usize,
        /// The expected size in bytes.
        expected: usize,
    },

    /// An MCP page read returned data for a different page than the one
    /// requested (a stale duplicate response from an earlier retry).
    /// Accepting it would silently shift the rest of the dump.
    #[error("MCP page mismatch: requested 0x{requested:04X}, radio answered 0x{answered:04X}")]
    McpPageMismatch {
        /// The page that was requested.
        requested: McpPage,
        /// The page the radio's response was for.
        answered: McpPage,
    },

    /// The radio did not complete the ACK handshake for an MCP page read.
    #[error(
        "read of page 0x{page:04X} not acknowledged by radio \
         (expected ACK 0x06, got 0x{got:02X})"
    )]
    McpPageReadNotAcknowledged {
        /// The page contained in the complete `W` response.
        page: McpPage,
        /// The byte received instead of ACK.
        got: u8,
    },

    /// A requested MCP page lies outside the radio's memory image.
    #[error("MCP page 0x{page:04X} out of range (page count: {total_pages})")]
    McpPageOutOfRange {
        /// The invalid page number.
        page: u16,
        /// The number of pages in the radio's memory image.
        total_pages: u16,
    },

    /// A schema-generated patch was aimed at an unqualified live target.
    #[error(
        "MCP-D75 schema patches support only {expected_model} vendor firmware \
         {expected_firmware} (accepted exact CAT FV identities: \
         {accepted_firmware_identities:?}); connected target is model {actual_model} \
         firmware {actual_firmware}"
    )]
    McpUnsupportedSchemaTarget {
        /// Model required by the generated schema.
        expected_model: &'static str,
        /// Canonical vendor firmware release required by the schema.
        expected_firmware: &'static str,
        /// Exact CAT `FV` strings accepted for that vendor release.
        accepted_firmware_identities: &'static [&'static str],
        /// Model reported by the connected radio.
        actual_model: crate::types::RadioModel,
        /// Firmware reported by the connected radio.
        actual_firmware: crate::types::FirmwareIdentity,
    },

    /// The radio answered an MCP exit command with a byte other than ACK.
    ///
    /// The exit was not confirmed, so the programming session remains in
    /// an unknown state until CAT operation is independently proved or the
    /// radio is recovered.
    #[error("MCP exit not acknowledged (expected ACK 0x06, got 0x{got:02X})")]
    McpExitNotAcknowledged {
        /// The byte received instead of ACK.
        got: u8,
    },

    /// MCP cleanup failed and normal CAT operation was not proved.
    ///
    /// The radio may still be in programming mode, or its USB reset may not
    /// have completed. Retrying MCP or sending CAT commands is unsafe until
    /// the radio has been fully power-cycled.
    #[error(
        "MCP cleanup failed: {cleanup}; normal CAT restoration was not proved; \
         fully power-cycle the radio before retrying"
    )]
    McpCleanupNotProved {
        /// The cleanup or reconnect failure.
        #[source]
        cleanup: Box<Self>,
    },

    /// Both an MCP operation and its cleanup failed.
    ///
    /// Both errors are retained for diagnosis. When `cleanup` contains
    /// [`Error::McpCleanupNotProved`], the radio's current mode is unknown
    /// and that nested error carries the required power-cycle guidance.
    #[error("MCP operation failed: {operation}; cleanup also failed: {cleanup}")]
    McpOperationAndCleanupFailed {
        /// The original transfer, entry, or write failure.
        operation: Box<Self>,
        /// The subsequent cleanup or reconnect failure.
        #[source]
        cleanup: Box<Self>,
    },

    /// CAT framing could not be restored after leaving a binary radio mode.
    ///
    /// Both attempts are retained: the first tried to prove CAT on the
    /// existing transport, and the second closed and reopened that transport
    /// before trying again. Callers must treat the radio as not restored and
    /// establish a new connection or explicitly retry recovery.
    #[error(
        "CAT restoration failed in place: {in_place}; the reconnect attempt also failed: {reconnect}"
    )]
    CatRestorationFailed {
        /// Failure from the in-place CAT identity exchange.
        in_place: Box<Self>,
        /// Failure from closing, reopening, sending the universal mode-exit
        /// preamble, or proving the isolated CAT identity exchange.
        #[source]
        reconnect: Box<Self>,
    },

    /// An MCP exit byte may already have reached the radio.
    ///
    /// Sending `E` twice has undefined framing semantics, so recovery must
    /// settle and prove CAT operation without retransmitting the exit byte.
    #[error("MCP exit was already attempted; refusing to send a second exit byte")]
    McpExitAlreadySent,

    /// An MCP programming session was interrupted (its future was
    /// cancelled mid-transfer). The radio may still be in PROG MCP mode
    /// where CAT commands do not work, so call
    /// `Radio::recover_from_interrupted_mcp` first.
    #[error(
        "MCP session interrupted; radio may be in programming mode; call \
         Radio::recover_from_interrupted_mcp first"
    )]
    McpInterrupted,

    /// An MCP command or ACK handshake ended without a proved byte boundary.
    ///
    /// Sending the raw exit byte could complete a partial frame or consume a
    /// stale acknowledgement. The transport is closed and the radio must be
    /// power-cycled before any further protocol traffic.
    #[error(
        "MCP wire boundary is ambiguous; the connection was closed without sending an exit byte; \
         fully power-cycle the radio before reconnecting"
    )]
    McpWireBoundaryUnproved,

    /// A GM memory read was requested before the installed patched target was
    /// attested on this connection.
    #[error(
        "GM memory reads require a live MemoryReader; call \
         Radio::qualify_mem_read_for with the expected patched target first"
    )]
    MemoryReadNotQualified,

    /// An automation operation was attempted without a live qualified
    /// [`AutomationSession`](crate::radio::automation::AutomationSession), or
    /// after that session was poisoned by an incomplete operation.
    #[error(
        "radio automation requires a live qualified AutomationSession; \
         reconnect and call Radio::qualify_automation"
    )]
    AutomationNotQualified,

    /// A strict GM exchange failed or was cancelled after it may have put bytes
    /// in flight. Only reopening the transport can exclude a delayed tail.
    #[error(
        "the GM memory-read stream is poisoned by an incomplete strict exchange; \
         call Radio::recover_cat before sending any more commands"
    )]
    MemoryReadStreamPoisoned,

    /// A GM read would leave the window qualified for the installed patch.
    #[error(
        "GM read 0x{offset:06X}+{length} exceeds the qualified {target} window \
         with exclusive bound 0x{bound:06X}"
    )]
    MemoryReadOutOfRange {
        /// Stable target name, such as `low NOR V1.03`.
        target: &'static str,
        /// Requested offset.
        offset: u32,
        /// Requested byte count.
        length: u16,
        /// One past the last qualified byte.
        bound: u32,
    },

    /// Every bounded automation snapshot attempt observed a valid unstable frame.
    ///
    /// This is a clean semantic result rather than a malformed strict-GM
    /// exchange; the qualified automation session remains usable.
    #[error("radio screen remained unstable across {attempts} bounded capture attempts")]
    AutomationScreenUnstable {
        /// Number of host snapshot commands attempted.
        attempts: u8,
    },

    /// A stepped-tuning operation requires the band in VFO tuning mode.
    #[error(
        "stepped tuning requires band {band} in VFO tuning mode so the selected memory, call, \
         or weather channel is never changed; current tuning mode is {current:?}"
    )]
    VfoTuningRequired {
        /// Band whose tuning mode was checked.
        band: crate::types::Band,
        /// Tuning mode that band reported.
        current: crate::types::TuningMode,
    },

    /// The USB audio output did not engage as requested.
    #[error(
        "USB audio output {requested:?} did not engage (read back {actual:?}); IF and Detect \
         output require Single Band mode on Band B"
    )]
    IfTapNotEngaged {
        /// Output selection that was written.
        requested: crate::types::UsbAudioOutput,
        /// Output selection the radio reported afterwards.
        actual: crate::types::UsbAudioOutput,
    },

    /// A stepped retune target is not a whole number of tuning steps away.
    #[error(
        "retune target {target} is not a whole number of {step} steps from {current}; \
         change the tuning step or pick an on-step target"
    )]
    RetuneOffStep {
        /// Frequency before the retune.
        current: crate::types::Frequency,
        /// Requested target frequency.
        target: crate::types::Frequency,
        /// Tuning step the radio reported.
        step: crate::types::StepSize,
    },

    /// A stepped retune would need more UP/DW steps than the bound allows.
    #[error("retune needs {steps_required} steps (maximum {maximum}); tune the radio closer first")]
    RetuneSpanTooLarge {
        /// Steps the walk would require.
        steps_required: u32,
        /// Upper bound on steps per retune call.
        maximum: u32,
    },

    /// A stepped retune finished on a different frequency than requested.
    #[error(
        "retune landed on {actual} instead of {requested}; the USB audio output remains on \
         the audio path"
    )]
    RetuneNotVerified {
        /// Frequency the caller requested.
        requested: crate::types::Frequency,
        /// Frequency the radio reported after stepping.
        actual: crate::types::Frequency,
    },

    /// A menu-field schema operation failed before or after MCP I/O.
    #[error(transparent)]
    Schema(#[from] crate::memory::SchemaError),

    /// A compare-exchange MCP batch failed; the nested error retains which
    /// pages may already have changed.
    #[error(transparent)]
    McpPageExchange(#[from] crate::radio::programming::McpPageExchangeError),

    /// A sparse detached MCP update failed; the nested error retains which
    /// page writes may have started and which were read-back verified.
    #[error(transparent)]
    DetachedMcpPageUpdate(#[from] crate::radio::programming::DetachedMcpPageUpdateError),

    /// A menu-patch compare-exchange referenced a page absent from the
    /// caller's snapshot, so no expected bytes exist to compare against.
    #[error(
        "menu patch touches page 0x{page:04X}, which the supplied snapshot never read; \
         re-read the snapshot with every patched field included"
    )]
    McpSnapshotPageMissing {
        /// The patched page missing from the snapshot.
        page: WritableMcpPage,
    },

    /// The link never proved MMDVM after a verified route-and-mode update.
    #[error(
        "the radio did not answer MMDVM probes within {window:?} after Menu 985 and Menu 650 \
         were read-back verified for this connection; do not repeat the memory update, because \
         another attempt only reboots the radio again"
    )]
    TerminalModeNotEngaged {
        /// Transition window that elapsed without MMDVM proof.
        window: Duration,
    },
}

impl Error {
    /// Whether this failure means the physical link should be treated as
    /// down.
    ///
    /// True for transport-layer failures and command timeouts; the remedy is
    /// [`Radio::reconnect`](crate::radio::Radio::reconnect) (or rebuilding
    /// the transport). Semantic radio replies such as
    /// [`Error::CommandRejected`] leave the link healthy and return false.
    ///
    /// # Examples
    ///
    /// ```
    /// use std::time::Duration;
    /// use kenwood_thd75::error::Error;
    ///
    /// assert!(Error::Timeout(Duration::from_secs(5)).is_link_lost());
    /// let rejected = Error::CommandRejected { mnemonic: "FQ".to_string() };
    /// assert!(!rejected.is_link_lost());
    /// assert!(Error::CatRecoveryRequired.requires_recovery());
    /// ```
    #[must_use]
    pub fn is_link_lost(&self) -> bool {
        match self {
            Self::Transport(_) | Self::Timeout(_) => true,
            Self::McpCleanupNotProved { cleanup } => cleanup.is_link_lost(),
            Self::McpOperationAndCleanupFailed { operation, cleanup } => {
                operation.is_link_lost() || cleanup.is_link_lost()
            }
            Self::CatRestorationFailed {
                in_place,
                reconnect,
            } => in_place.is_link_lost() || reconnect.is_link_lost(),
            Self::McpPageExchange(error) => error.is_link_lost(),
            Self::DetachedMcpPageUpdate(error) => error.is_link_lost(),
            _ => false,
        }
    }

    /// Whether this failure leaves the CAT stream refusing ordinary commands
    /// until an explicit recovery API runs.
    ///
    /// True for every error that reports a poisoned or unresolved stream
    /// boundary, and for everything [`Error::is_link_lost`] covers (a failed
    /// or timed-out exchange leaves the request/response boundary
    /// unresolved). The remedy is
    /// [`Radio::recover_cat`](crate::radio::Radio::recover_cat) or
    /// [`Radio::reconnect`](crate::radio::Radio::reconnect).
    ///
    /// This classifies the error value itself. A [`Error::Protocol`] frame
    /// error can also poison the handle without being classified here;
    /// [`Radio::cat_recovery_required`](crate::radio::Radio::cat_recovery_required)
    /// remains the authoritative live-state check.
    #[must_use]
    pub fn requires_recovery(&self) -> bool {
        if self.is_link_lost() {
            return true;
        }
        match self {
            Self::CatRecoveryRequired
            | Self::MemoryReadStreamPoisoned
            | Self::McpInterrupted
            | Self::McpWireBoundaryUnproved
            | Self::McpExitAlreadySent
            | Self::McpCleanupNotProved { .. }
            | Self::McpOperationAndCleanupFailed { .. }
            | Self::CatRestorationFailed { .. } => true,
            Self::McpPageExchange(error) => error.requires_recovery(),
            Self::DetachedMcpPageUpdate(error) => error.requires_recovery(),
            _ => false,
        }
    }
}

/// Errors originating from the transport layer (serial port / Bluetooth).
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum TransportError {
    /// Failed to open the serial port at the given path.
    #[error("failed to open serial port at {path}")]
    Open {
        /// The filesystem path that could not be opened.
        path: String,
        /// The underlying I/O error.
        source: std::io::Error,
    },

    /// The isolated macOS Bluetooth helper could not select one paired radio,
    /// be prepared or launched, or complete its readiness handshake.
    #[error("Bluetooth helper failed during {context}")]
    BluetoothHelper {
        /// Operation or resource that failed.
        context: String,
        /// The underlying process or pipe error.
        source: std::io::Error,
    },

    /// More than one paired Bluetooth device has the requested display name.
    #[error(
        "multiple paired Bluetooth devices have the requested name; pass an exact Bluetooth address instead"
    )]
    BluetoothDeviceNameAmbiguous,

    /// A caller cancelled a bounded macOS Bluetooth discovery or open.
    #[error("Bluetooth helper open was interrupted")]
    BluetoothOpenInterrupted,

    /// No matching serial device was found.
    #[error("no matching serial device found")]
    NotFound,

    /// The serial connection was lost.
    #[error("serial connection lost")]
    Disconnected(
        /// The underlying I/O error.
        #[source]
        std::io::Error,
    ),

    /// A write to the serial port failed.
    #[error("serial write failed")]
    Write(
        /// The underlying I/O error.
        #[source]
        std::io::Error,
    ),

    /// A read from the serial port failed.
    #[error("serial read failed")]
    Read(
        /// The underlying I/O error.
        #[source]
        std::io::Error,
    ),

    /// The transport cannot re-establish its own connection.
    ///
    /// Returned by the default [`Transport::reopen`] implementation.
    /// Callers must build a fresh transport instead.
    ///
    /// [`Transport::reopen`]: crate::transport::Transport::reopen
    #[error("this transport cannot reopen its connection")]
    ReopenUnsupported,

    /// A thread-affine third-party transport refused this reopen call.
    ///
    /// The built-in macOS Bluetooth transport is process-isolated and does
    /// not have this restriction. This variant remains available to custom
    /// transports whose platform API requires its original opening thread.
    #[error("reopen must run on the thread that opened the transport")]
    WrongThread,

    /// The main-thread broker has been dropped and cannot execute more jobs.
    #[error("the main-thread transport broker is no longer available")]
    BrokerUnavailable,
}

/// Errors in the CAT protocol layer (framing, field parsing, etc.).
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum ProtocolError {
    /// The radio returned an unknown command identifier.
    #[error("unknown command: {0}")]
    UnknownCommand(
        /// The unrecognised command string.
        String,
    ),

    /// A command response had the wrong number of fields.
    #[error("command {command}: expected {expected} fields, got {actual}")]
    FieldCount {
        /// The two-letter command identifier.
        command: String,
        /// The expected number of fields.
        expected: usize,
        /// The actual number of fields received.
        actual: usize,
    },

    /// A single field in a command response could not be parsed.
    #[error("command {command}: failed to parse field {field}: {detail}")]
    FieldParse {
        /// The two-letter command identifier.
        command: String,
        /// The name or index of the problematic field.
        field: String,
        /// A human-readable description of the parse failure.
        detail: String,
    },

    /// The response did not match the expected command.
    #[error("unexpected response: expected {expected}, got {actual:?}")]
    UnexpectedResponse {
        /// The expected command prefix.
        expected: String,
        /// The raw bytes actually received.
        actual: Vec<u8>,
    },

    /// A received frame was not valid (e.g. missing terminator).
    #[error("malformed frame: {0:?}")]
    MalformedFrame(
        /// The raw bytes of the malformed frame.
        Vec<u8>,
    ),

    /// Unconsumed CAT input exceeded the codec's bounded framing buffer.
    ///
    /// The codec becomes poisoned when this occurs and the transport stream
    /// must be reopened or otherwise brought to a proven frame boundary before
    /// the codec is cleared. This prevents a suffix of an oversized frame from
    /// being mistaken for a fresh response.
    #[error(
        "CAT framing buffer would exceed {maximum} bytes: {buffered} buffered + {incoming} incoming"
    )]
    FrameTooLong {
        /// Maximum unconsumed byte count accepted by the codec.
        maximum: usize,
        /// Bytes buffered before the rejected feed.
        buffered: usize,
        /// Bytes in the rejected feed.
        incoming: usize,
    },

    /// An MCP `W` response did not have the exact required frame size.
    #[error("W response has {actual} bytes, expected exactly {expected}")]
    WriteResponseSize {
        /// The actual byte count received.
        actual: usize,
        /// The exact byte count required (marker + 4-byte address + page).
        expected: usize,
    },

    /// An MCP write response carried a marker byte other than `'W'`.
    #[error("expected W response marker, got 0x{got:02X}")]
    WriteResponseBadMarker {
        /// The byte received in place of the `W` marker.
        got: u8,
    },

    /// An MCP `W` response carried a nonzero byte offset.
    #[error("expected zero W response offset, got 0x{got:04X}")]
    WriteResponseNonzeroOffset {
        /// The unexpected big-endian offset from address bytes 3-4.
        got: u16,
    },
}

/// Errors raised when a user-supplied value fails validation.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum ValidationError {
    /// An Internet-to-RF `IGate` eligibility period is zero or exceeds a
    /// protocol-defined maximum.
    #[error("invalid Internet-to-RF IGate {field} period {value:?}: {detail}")]
    IGatePeriodOutOfRange {
        /// Eligibility period being configured.
        field: &'static str,
        /// Rejected duration.
        value: Duration,
        /// Exact valid domain.
        detail: &'static str,
    },

    /// A memory channel display name exceeds its 16-byte storage field.
    #[error("channel display name is {len} bytes long (maximum is 16)")]
    ChannelDisplayNameTooLong {
        /// The actual encoded length in bytes.
        len: usize,
    },

    /// A memory channel display name contains a byte outside printable ASCII.
    #[error(
        "channel display name byte at offset {offset} is 0x{value:02X} \
         (expected printable ASCII 0x20-0x7E)"
    )]
    InvalidChannelDisplayNameByte {
        /// Zero-based byte offset of the invalid value.
        offset: usize,
        /// The invalid byte.
        value: u8,
    },

    /// A power-on message exceeds its 16-byte display and storage field.
    #[error("power-on message is {len} bytes long (maximum is 16)")]
    PowerOnMessageTooLong {
        /// Actual encoded length in bytes.
        len: usize,
    },

    /// A power-on message contains a byte the TH-D75 cannot display as text.
    #[error(
        "power-on message byte at offset {offset} is 0x{value:02X} \
         (expected printable ASCII 0x20-0x7E)"
    )]
    InvalidPowerOnMessageByte {
        /// Zero-based byte offset of the invalid value.
        offset: usize,
        /// Invalid byte.
        value: u8,
    },

    /// A NUL-padded power-on-message field contains data after its terminator.
    #[error(
        "power-on message has nonzero byte 0x{value:02X} at offset {offset} \
         after its NUL terminator at offset {terminator_offset}"
    )]
    PowerOnMessageDataAfterNul {
        /// Zero-based byte offset of the first NUL terminator.
        terminator_offset: usize,
        /// Zero-based byte offset of the unexpected value.
        offset: usize,
        /// Unexpected nonzero byte.
        value: u8,
    },

    /// A CAT `ID` response names a radio other than the exact TH-D75 model
    /// this crate controls.
    #[error("unsupported radio model identity {model:?} (expected exact TH-D75)")]
    UnsupportedRadioModel {
        /// Rejected CAT model identity.
        model: String,
    },

    /// A CAT firmware identity does not fit its fixed one-to-eight-byte field.
    #[error("firmware identity is {len} bytes long (expected 1-{max})")]
    FirmwareIdentityLength {
        /// Actual encoded length.
        len: usize,
        /// Maximum encoded length.
        max: usize,
    },

    /// A CAT firmware identity contains whitespace, non-ASCII, or a control
    /// byte instead of one visible ASCII token.
    #[error(
        "firmware identity byte at offset {offset} is 0x{value:02X} \
         (expected visible ASCII 0x21-0x7E)"
    )]
    InvalidFirmwareIdentityByte {
        /// Zero-based byte offset of the invalid value.
        offset: usize,
        /// Invalid byte.
        value: u8,
    },

    /// A fixed-width CAT identity field has the wrong encoded length.
    #[error("{field} is {actual} bytes long (expected exactly {expected})")]
    IdentityFieldLength {
        /// Stable English field name used in diagnostics.
        field: &'static str,
        /// Actual encoded length.
        actual: usize,
        /// Required encoded length.
        expected: usize,
    },

    /// A fixed-width CAT identity contains a byte that cannot occur in its
    /// comma-separated printable-ASCII field.
    #[error(
        "{field} byte at offset {offset} is 0x{value:02X} \
         (expected printable ASCII 0x20-0x7E other than comma)"
    )]
    InvalidIdentityFieldByte {
        /// Stable English field name used in diagnostics.
        field: &'static str,
        /// Zero-based byte offset of the invalid value.
        offset: usize,
        /// Invalid byte.
        value: u8,
    },

    /// A CAT `TY` response contains a market/region code the firmware cannot
    /// emit.
    #[error("unknown radio region code {code:?} (expected 0, E, J, or K)")]
    UnknownRadioRegion {
        /// Rejected exact field value.
        code: String,
    },

    /// A hardware-variant value cannot fit the single hexadecimal digit used
    /// by the CAT `TY` response.
    #[error("hardware variant {value} is out of range (expected 0-15)")]
    HardwareVariantOutOfRange {
        /// Rejected numeric value.
        value: u8,
    },

    /// A memory channel display-name field contains data after its NUL terminator.
    #[error(
        "channel display name has nonzero byte 0x{value:02X} at offset {offset} \
         after its NUL terminator"
    )]
    ChannelDisplayNameDataAfterNul {
        /// Zero-based byte offset of the unexpected value.
        offset: usize,
        /// The unexpected nonzero byte.
        value: u8,
    },

    /// A callsign or callsign suffix exceeds its fixed-width byte field.
    #[error("callsign field too long ({len} bytes, max {max})")]
    CallsignTooLong {
        /// The actual encoded length.
        len: usize,
        /// The maximum allowed length.
        max: usize,
    },

    /// A D-STAR callsign or suffix contains a byte unsafe for its CAT field.
    #[error(
        "D-STAR {field} byte at offset {offset} is 0x{value:02X} \
         (expected printable ASCII other than comma)"
    )]
    InvalidDstarCallsignByte {
        /// Field containing the invalid byte (`callsign` or `suffix`).
        field: &'static str,
        /// Zero-based byte offset of the invalid value.
        offset: usize,
        /// The invalid byte.
        value: u8,
    },

    /// A NUL-padded D-STAR identity field contains data in its padding.
    #[error(
        "D-STAR {field} padding byte at offset {offset} is 0x{value:02X} \
         (expected NUL padding)"
    )]
    InvalidDstarCallsignPadding {
        /// Field containing the invalid padding.
        field: &'static str,
        /// Zero-based byte offset of the invalid padding byte.
        offset: usize,
        /// Invalid byte found after the first NUL.
        value: u8,
    },

    /// The channel number is outside the valid range.
    #[error("channel {channel} out of range (max {max})")]
    ChannelOutOfRange {
        /// The invalid channel number.
        channel: u16,
        /// The maximum valid channel number.
        max: u16,
    },

    /// A memory-group index is outside the radio's 30 groups.
    #[error("memory group {group} out of range (max 29)")]
    MemoryGroupOutOfRange {
        /// The invalid group index.
        group: u8,
    },

    /// A memory-channel band code is not one of the values verified in radio images.
    #[error(
        "memory channel band code 0x{marker:02X} is invalid (expected 0x00, 0x01, 0x02, or 0x05)"
    )]
    MemoryChannelBandOutOfRange {
        /// The invalid three-bit band code.
        marker: u8,
    },

    /// A stored-channel flag record does not have its required four-byte width.
    #[error("stored channel flag must be exactly 4 bytes, got {actual}")]
    StoredChannelFlagLength {
        /// Number of bytes supplied by the caller.
        actual: usize,
    },

    /// A KISS timing value is outside its radio-supported ten-millisecond domain.
    #[error(
        "{parameter} {milliseconds} ms is invalid (expected 0-{maximum_milliseconds} ms in 10 ms steps)"
    )]
    InvalidKissTiming {
        /// Name of the KISS control being validated.
        parameter: &'static str,
        /// Requested duration in milliseconds.
        milliseconds: u16,
        /// Largest duration accepted for this control.
        maximum_milliseconds: u16,
    },

    /// A three-character ME/MR memory selector is not in the firmware's
    /// accepted selector domain.
    #[error("invalid memory selector {selector:?}: {detail}")]
    InvalidMemorySelector {
        /// Rejected selector text.
        selector: String,
        /// Human-readable accepted-domain description.
        detail: &'static str,
    },

    /// A real-time-clock payload is not a valid `YYMMDDHHmmss` value.
    #[error("invalid radio date/time {value:?}: {detail}")]
    InvalidRadioDateTime {
        /// Rejected wire value.
        value: String,
        /// Human-readable validation failure.
        detail: &'static str,
    },

    /// An FM broadcast-memory channel index is outside FM0-FM9.
    #[error("FM radio channel {channel} out of range (must be 0-9)")]
    FmRadioChannelOutOfRange {
        /// Rejected zero-based channel index.
        channel: u8,
    },

    /// An FM broadcast-memory frequency is outside 76-108 MHz inclusive.
    #[error(
        "FM radio frequency {frequency_hz} Hz out of range \
         (must be 76000000-108000000 Hz)"
    )]
    FmRadioFrequencyOutOfRange {
        /// Rejected frequency in hertz.
        frequency_hz: u32,
    },

    /// An FM broadcast-memory station name exceeds its eight-byte field.
    #[error("FM radio station name is {len} bytes long (maximum is 8)")]
    FmRadioNameTooLong {
        /// Actual UTF-8 encoded length.
        len: usize,
    },

    /// A voice-message name exceeds its eight-byte field.
    #[error("voice message name is {len} bytes long (maximum is 8)")]
    VoiceMessageNameTooLong {
        /// Actual UTF-8 encoded length.
        len: usize,
    },

    /// A voice recording duration exceeds its selected channel's capacity.
    #[error(
        "voice message channel {channel} duration is {seconds} seconds \
         (maximum is {maximum})"
    )]
    VoiceMessageDurationOutOfRange {
        /// One-based voice-message channel number.
        channel: u8,
        /// Rejected duration in seconds.
        seconds: u8,
        /// Maximum duration accepted by that channel.
        maximum: u8,
    },

    /// A voice-message repeat interval is above 60 seconds.
    #[error("voice repeat interval {seconds} seconds out of range (must be 0-60)")]
    VoiceRepeatIntervalOutOfRange {
        /// Rejected interval in seconds.
        seconds: u8,
    },

    /// A CW sidetone pitch is outside the stepped menu domain.
    #[error("CW pitch {hz} Hz is invalid (must be 400-1000 Hz in 100 Hz steps)")]
    CwPitchOutOfRange {
        /// Rejected pitch in hertz.
        hz: u16,
    },

    /// A DTMF memory-slot index is outside 0-9.
    #[error("DTMF memory slot {index} out of range (must be 0-9)")]
    DtmfSlotOutOfRange {
        /// Rejected zero-based slot index.
        index: u8,
    },

    /// A DTMF memory name exceeds its sixteen-byte field.
    #[error("DTMF memory name is {len} bytes long (maximum is 16)")]
    DtmfNameTooLong {
        /// Actual UTF-8 encoded length.
        len: usize,
    },

    /// A DTMF auto-dial sequence exceeds sixteen digits.
    #[error("DTMF digit sequence is {len} bytes long (maximum is 16)")]
    DtmfDigitsTooLong {
        /// Actual encoded length.
        len: usize,
    },

    /// A DTMF auto-dial sequence contains a character outside the keypad.
    #[error(
        "DTMF digit at byte offset {offset} is {value:?} \
         (expected 0-9, A-D, *, or #)"
    )]
    InvalidDtmfDigit {
        /// Byte offset of the rejected character.
        offset: usize,
        /// Rejected character.
        value: char,
    },

    /// An `EchoLink` memory-slot index is outside 0-9.
    #[error("EchoLink memory slot {index} out of range (must be 0-9)")]
    EchoLinkSlotOutOfRange {
        /// Rejected zero-based slot index.
        index: u8,
    },

    /// An `EchoLink` station name exceeds its eight-byte field.
    #[error("EchoLink station name is {len} bytes long (maximum is 8)")]
    EchoLinkNameTooLong {
        /// Actual UTF-8 encoded length.
        len: usize,
    },

    /// An `EchoLink` DTMF code exceeds eight digits.
    #[error("EchoLink DTMF code is {len} bytes long (maximum is 8)")]
    EchoLinkCodeTooLong {
        /// Actual encoded length.
        len: usize,
    },

    /// An `EchoLink` code contains a character outside the DTMF keypad.
    #[error(
        "EchoLink DTMF digit at byte offset {offset} is {value:?} \
         (expected 0-9, A-D, *, or #)"
    )]
    InvalidEchoLinkCodeDigit {
        /// Byte offset of the rejected character.
        offset: usize,
        /// Rejected character.
        value: char,
    },

    /// A wireless remote-control code is not exactly three encoded bytes.
    #[error("wireless remote-control code is {len} bytes long (expected exactly 3)")]
    RemoteControlCodeLength {
        /// Actual UTF-8 encoded length.
        len: usize,
    },

    /// A validated integer-backed type received a value outside its domain.
    ///
    /// This complements [`ValidationError::SettingOutOfRange`] for domains
    /// whose values do not fit in `u8`.
    #[error("{name} value {value} out of range ({detail})")]
    IntegerOutOfRange {
        /// Stable English name of the validated value.
        name: &'static str,
        /// Rejected integer value.
        value: i64,
        /// Human-readable accepted-domain description.
        detail: &'static str,
    },

    /// A validated text field has an encoded length outside its domain.
    #[error("{name} is {len} bytes long ({detail})")]
    TextLengthOutOfRange {
        /// Stable English name of the validated text field.
        name: &'static str,
        /// Rejected encoded length in bytes.
        len: usize,
        /// Human-readable accepted-length description.
        detail: &'static str,
    },

    /// A validated text field contains a byte outside its character domain.
    #[error("{name} byte at offset {offset} is 0x{value:02X} ({detail})")]
    InvalidTextByte {
        /// Stable English name of the validated text field.
        name: &'static str,
        /// Zero-based byte offset of the rejected value.
        offset: usize,
        /// Rejected byte.
        value: u8,
        /// Human-readable accepted-character description.
        detail: &'static str,
    },

    /// A validated character is outside its accepted character set.
    #[error("{name} character {value:?} is invalid ({detail})")]
    InvalidCharacter {
        /// Stable English name of the validated character.
        name: &'static str,
        /// Rejected character.
        value: char,
        /// Human-readable accepted-character description.
        detail: &'static str,
    },

    /// A validated textual value has invalid structure beyond one byte.
    #[error("{name} value {value:?} is invalid ({detail}; {reason})")]
    InvalidTextValue {
        /// Stable English name of the validated value.
        name: &'static str,
        /// Rejected text.
        value: String,
        /// Human-readable accepted-domain description.
        detail: &'static str,
        /// Specific reason reported by the underlying parser.
        reason: String,
    },

    /// A validated collection contains too many entries.
    #[error("{name} contains {len} entries ({detail})")]
    CollectionLengthOutOfRange {
        /// Stable English name of the validated collection.
        name: &'static str,
        /// Rejected entry count.
        len: usize,
        /// Human-readable accepted-count description.
        detail: &'static str,
    },

    /// A wireless remote-control code contains a non-decimal character.
    #[error(
        "wireless remote-control code digit at byte offset {offset} is {value:?} \
         (expected 0-9)"
    )]
    InvalidRemoteControlCodeDigit {
        /// Byte offset of the rejected character.
        offset: usize,
        /// Rejected character.
        value: char,
    },

    /// A settings/configuration enum value is outside its valid range.
    ///
    /// Used for MCP binary settings types (backlight, EQ, language, etc.)
    /// where adding a dedicated variant per type would be excessive.
    #[error("{name} value {value} out of range ({detail})")]
    SettingOutOfRange {
        /// The setting type name (e.g., "backlight control").
        name: &'static str,
        /// The invalid raw value.
        value: u8,
        /// Human-readable valid range description (e.g., "must be 0-2").
        detail: &'static str,
    },

    /// A memory-read request parameter is outside the range the radio accepts.
    ///
    /// Used by `MemoryReadOffset` and `ReadLen`, whose valid ranges are wider
    /// than the `u8` that [`ValidationError::SettingOutOfRange`] carries.
    #[error("{name} value {value:#X} out of range ({detail})")]
    MemoryParamOutOfRange {
        /// The parameter name, e.g. "memory-read offset", "read length".
        name: &'static str,
        /// The invalid raw value.
        value: u32,
        /// Human-readable valid range description, e.g. "must be 0-0xFFFFFF".
        detail: &'static str,
    },

    /// A runtime APRS wire-format value failed validation.
    ///
    /// Used by the `aprs` and `ax25-codec` crates for typed primitives
    /// such as `Callsign`, `Latitude`, `Longitude`, `Course`, and
    /// `MessageId` where the failing value may be too wide to fit in a
    /// `u8`.
    #[error("{field} out of range: {detail}")]
    AprsWireOutOfRange {
        /// Field name, e.g. `"Latitude"`, `"Callsign byte"`.
        field: &'static str,
        /// Human-readable explanation (e.g. `"length 7 exceeds max 6"`).
        detail: &'static str,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn validation_error_display() {
        // 50 (the 1750 Hz tone burst) is VALID, so the message must
        // state the real accepted range.
        let err = ValidationError::SettingOutOfRange {
            name: "tone code",
            value: 51,
            detail: "must be 0-50 (0-49 CTCSS tones, 50 = 1750 Hz tone burst)",
        };
        assert_eq!(
            err.to_string(),
            "tone code value 51 out of range (must be 0-50 (0-49 CTCSS tones, 50 = 1750 Hz tone burst))"
        );
    }

    #[test]
    fn generic_validated_type_errors_preserve_rejected_context() {
        let integer = ValidationError::IntegerOutOfRange {
            name: "track-log interval",
            value: 1,
            detail: "must be 2-1800 seconds",
        };
        assert_eq!(
            integer.to_string(),
            "track-log interval value 1 out of range (must be 2-1800 seconds)"
        );

        let length = ValidationError::TextLengthOutOfRange {
            name: "position name",
            len: 9,
            detail: "must be at most 8 encoded bytes",
        };
        assert_eq!(
            length.to_string(),
            "position name is 9 bytes long (must be at most 8 encoded bytes)"
        );

        let byte = ValidationError::InvalidTextByte {
            name: "stored APRS status text",
            offset: 4,
            value: b'\n',
            detail: "must contain only printable ASCII bytes 0x20-0x7E",
        };
        assert_eq!(
            byte.to_string(),
            "stored APRS status text byte at offset 4 is 0x0A (must contain only printable ASCII bytes 0x20-0x7E)"
        );
    }

    #[test]
    fn operating_mode_error_message_covers_full_range() {
        // OperatingMode accepts 0-9 (including WFM=8 and CW-R=9); the message
        // must not claim 0-7. The construction lives in `types::mode`; this
        // pins the shared variant's rendering of that domain text.
        let err = ValidationError::SettingOutOfRange {
            name: "operating mode",
            value: 10,
            detail: "must be 0-9: FM/DV/AM/LSB/USB/CW/NFM/DR/WFM/CW-R",
        };
        let msg = err.to_string();
        assert!(
            msg.contains("0-9"),
            "message must state the real range: {msg}"
        );
    }

    #[test]
    fn protocol_error_display() {
        let err = ProtocolError::FieldCount {
            command: "FO".to_owned(),
            expected: 21,
            actual: 19,
        };
        assert!(err.to_string().contains("21"));
        assert!(err.to_string().contains("19"));
    }

    #[test]
    fn error_from_validation() {
        let val_err = ValidationError::SettingOutOfRange {
            name: "band index",
            value: 2,
            detail: "must be 0-1",
        };
        let err: Error = val_err.into();
        assert!(matches!(err, Error::Validation(_)));
    }

    #[test]
    fn channel_out_of_range_display() {
        let err = ValidationError::ChannelOutOfRange {
            channel: 1200,
            max: 1199,
        };
        assert!(err.to_string().contains("1200"));
        assert!(err.to_string().contains("1199"));
    }

    #[test]
    fn setting_out_of_range_display() {
        let err = ValidationError::SettingOutOfRange {
            name: "backlight control",
            value: 5,
            detail: "must be 0-2",
        };
        let msg = err.to_string();
        assert!(msg.contains("backlight control"));
        assert!(msg.contains('5'));
        assert!(msg.contains("must be 0-2"));
    }

    #[test]
    fn error_from_transport() {
        let t_err = TransportError::NotFound;
        let err: Error = t_err.into();
        assert!(matches!(err, Error::Transport(_)));
    }

    #[test]
    fn error_from_protocol() {
        let p_err = ProtocolError::MalformedFrame(vec![0xFF]);
        let err: Error = p_err.into();
        assert!(matches!(err, Error::Protocol(_)));
    }

    #[cfg(feature = "aprs")]
    #[test]
    fn error_from_kiss() {
        let err: Error = kiss_tnc::KissError::FrameTooLong.into();
        assert!(matches!(
            err,
            Error::Kiss(kiss_tnc::KissError::FrameTooLong)
        ));
    }

    #[test]
    fn timeout_error_display() {
        let err = Error::Timeout(Duration::from_secs(5));
        assert!(err.to_string().contains("5s"));
    }

    /// Sample semantic rejection for classification tests.
    fn rejected() -> Error {
        Error::CommandRejected {
            mnemonic: "FQ".to_string(),
        }
    }

    #[test]
    fn link_lost_classification_covers_transport_and_timeout() {
        assert!(Error::Timeout(Duration::from_secs(5)).is_link_lost());
        assert!(Error::Transport(TransportError::NotFound).is_link_lost());
        assert!(!rejected().is_link_lost());
        assert!(
            !Error::NotAvailableInCurrentMode {
                mnemonic: "ID".to_string()
            }
            .is_link_lost()
        );
        assert!(!Error::CatRecoveryRequired.is_link_lost());
    }

    #[test]
    fn recovery_classification_covers_poisoned_stream_states() {
        assert!(Error::CatRecoveryRequired.requires_recovery());
        assert!(Error::MemoryReadStreamPoisoned.requires_recovery());
        assert!(Error::McpInterrupted.requires_recovery());
        assert!(
            Error::McpCleanupNotProved {
                cleanup: Box::new(rejected())
            }
            .requires_recovery()
        );
        assert!(
            Error::CatRestorationFailed {
                in_place: Box::new(rejected()),
                reconnect: Box::new(rejected())
            }
            .requires_recovery()
        );
        // A command-phase timeout leaves the exchange boundary unresolved,
        // so it is both link loss and a recovery-required state.
        assert!(Error::Timeout(Duration::from_secs(5)).requires_recovery());
        assert!(!rejected().requires_recovery());
        assert!(!Error::NotIdentified.requires_recovery());
    }
}
