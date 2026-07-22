//! Events emitted by the modem loop to consumers.

use mmdvm_core::{ModemStatus, NakReason, VersionResponse};

/// Events emitted by the modem loop to the consumer via
/// [`super::AsyncModem::next_event`].
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum Event {
    /// Modem firmware version + capabilities (one-shot, typically at
    /// init).
    Version(VersionResponse),
    /// Modem status update: periodic (from internal 250 ms poll) OR
    /// unsolicited (from the modem itself after TX transitions etc.).
    Status(ModemStatus),
    /// Positive acknowledgement for a sent command.
    Ack {
        /// The command byte the modem ACK'd.
        command: u8,
    },
    /// Negative acknowledgement for a sent command.
    Nak {
        /// The command byte the modem rejected.
        command: u8,
        /// The reason the modem gave.
        reason: NakReason,
    },
    /// D-STAR header received from the radio's MMDVM TX (41 bytes).
    DStarHeaderRx {
        /// The 41 raw header bytes.
        bytes: [u8; 41],
    },
    /// D-STAR voice data received from the radio (9 AMBE + 3 slow
    /// data = 12 bytes).
    DStarDataRx {
        /// The 12 raw voice-frame bytes.
        bytes: [u8; 12],
    },
    /// D-STAR signal lost from the radio side.
    DStarLost,
    /// D-STAR end-of-transmission from the radio side.
    DStarEot,
    /// Debug text from the modem firmware
    /// (`MMDVM_DEBUG1..DEBUG5`).
    Debug {
        /// Debug level (1..=5).
        level: u8,
        /// Decoded UTF-8 text (NUL-trimmed, lossy where needed).
        text: String,
    },
    /// Serial passthrough data from the modem.
    SerialData(Vec<u8>),
    /// Transparent data from the modem.
    TransparentData(Vec<u8>),
    /// The modem sent a mode we don't fully model yet
    /// (DMR/YSF/P25/NXDN/POCSAG/FM), but the raw frame was accepted
    /// with the payload preserved verbatim.
    UnhandledResponse {
        /// The command byte.
        command: u8,
        /// The raw payload bytes.
        payload: Vec<u8>,
    },
    /// The transport closed gracefully (e.g. serial device unplug).
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
    /// Queued TX frames were discarded because the loop exited before
    /// the modem granted enough FIFO space to send them. Without
    /// this, a transmission truncates silently even though every
    /// `send_dstar_*` call reported success.
    TxDropped {
        /// Number of frames discarded.
        frames: usize,
    },
    /// The modem sent a frame that violates the protocol layout for
    /// its command byte (wrong payload length or unparseable
    /// response). The frame was dropped; counting these lets a
    /// consumer detect a degrading link.
    ProtocolViolation {
        /// The command byte of the offending frame.
        command: u8,
        /// What was wrong with it.
        detail: String,
    },
}
