//! Events emitted by the modem loop to consumers.

use mmdvm_core::{Command, ModemStatus, NakReason, VersionResponse};

/// Events emitted by the modem loop to the consumer via
/// [`super::AsyncModem::next_event`].
/// See the [runtime example](crate#drive-and-reclaim-a-modem).
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Event {
    /// A firmware version response to an initial or explicit query.
    Version(VersionResponse),
    /// A modem status update parsed using a confirmed supported version.
    ///
    /// Status received before the first valid version response is rejected as
    /// [`Self::ProtocolViolation`] and cannot grant transmission capacity.
    Status(ModemStatus),
    /// Positive acknowledgement for a sent command.
    Ack {
        /// The command byte the modem ACK'd.
        command: Command,
    },
    /// Negative acknowledgement for a sent command.
    Nak {
        /// The command byte the modem rejected.
        command: Command,
        /// The reason the modem gave.
        reason: NakReason,
    },
    /// D-STAR header received from the modem (41 bytes).
    DstarHeaderRx {
        /// The 41 raw header bytes.
        bytes: [u8; 41],
    },
    /// D-STAR voice data received from the radio (9 AMBE + 3 slow
    /// data = 12 bytes).
    DstarDataRx {
        /// The 12 raw voice-frame bytes.
        bytes: [u8; 12],
    },
    /// D-STAR signal lost from the radio side.
    DstarLost,
    /// D-STAR end-of-transmission from the radio side.
    DstarEot,
    /// Firmware debug response, including text and any binary integer suffix.
    ///
    /// `DEBUG1` through `DEBUG5` carry zero through four appended integers;
    /// `DEBUG_DUMP` carries diagnostic bytes. The command and payload are
    /// retained without imposing a severity level or lossy text conversion.
    Debug {
        /// The debug response identifier.
        command: Command,
        /// Complete payload, unchanged from the wire.
        payload: Vec<u8>,
    },
    /// Serial passthrough data from the modem.
    SerialData(Vec<u8>),
    /// Transparent data from the modem.
    TransparentData(Vec<u8>),
    /// The modem sent a mode without a typed event
    /// (DMR/YSF/P25/NXDN/POCSAG/FM), but the raw frame was accepted
    /// with the payload preserved verbatim.
    UnhandledResponse {
        /// The command byte.
        command: Command,
        /// The raw payload bytes.
        payload: Vec<u8>,
    },
    /// The underlying stream returned EOF.
    TransportClosed,
    /// The modem loop hit a fatal error and is about to exit. After
    /// the event channel drains, [`super::AsyncModem::next_event`]
    /// returns `None`. Distinguishes "the link died" from a clean
    /// close.
    Fatal {
        /// Human-readable description of the error that killed the
        /// loop.
        message: String,
    },
    /// Queued TX frames were discarded when the loop exited.
    ///
    /// The count covers frames still queued. A frame already removed for a
    /// failed write can have made partial progress and is reported by the
    /// terminal error rather than included in this count.
    TxDropped {
        /// Number of frames discarded.
        frames: usize,
    },
    /// The bounded event ring overwrote events before the consumer
    /// could receive them.
    ///
    /// This is an explicit stream discontinuity. The modem loop never
    /// blocks event delivery because doing so could deadlock a caller
    /// awaiting a command reply, but a slow consumer is told exactly
    /// how many events it missed instead of observing a silent gap.
    EventsDropped {
        /// Exact number of overwritten events.
        count: u64,
    },
    /// The modem sent a frame that violates the protocol layout for
    /// its command byte (wrong payload length or unparseable
    /// response). The frame was dropped; counting these lets a
    /// consumer detect a degrading link.
    ProtocolViolation {
        /// The command byte of the offending frame.
        command: Command,
        /// What was wrong with it.
        detail: String,
    },
}
