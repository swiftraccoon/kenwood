//! Error types for the kenwood-thd75 library.

use std::time::Duration;

use thiserror::Error;

/// Top-level error type for all radio operations.
#[derive(Debug, Error)]
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

    /// The radio returned an error response (`?\r`).
    #[error("radio returned error response")]
    RadioError,

    /// The radio returned "not available" (`N\r`) — command not supported in current mode.
    #[error("command not available in current radio mode")]
    NotAvailable,

    /// A command timed out waiting for a response.
    #[error("command timed out after {0:?}")]
    Timeout(Duration),

    /// The radio has not been identified yet; call `identify()` first.
    #[error("radio not identified \u{2014} call identify() first")]
    NotIdentified,

    /// A write was attempted to a protected memory region (factory calibration).
    #[error("write to protected page 0x{page:04X} denied (factory calibration region)")]
    MemoryWriteProtected {
        /// The page address that was denied.
        page: u16,
    },

    /// The radio did not ACK a write command.
    #[error("write to page 0x{page:04X} not acknowledged (expected ACK 0x06, got 0x{got:02X})")]
    WriteNotAcknowledged {
        /// The page address that was being written.
        page: u16,
        /// The byte received instead of ACK.
        got: u8,
    },

    /// The supplied memory image has an invalid size.
    #[error("invalid memory image size: {actual} bytes (expected {expected})")]
    InvalidImageSize {
        /// The actual size in bytes.
        actual: usize,
        /// The expected size in bytes.
        expected: usize,
    },
}

/// Errors originating from the transport layer (serial port / Bluetooth).
#[derive(Debug, Error)]
pub enum TransportError {
    /// Failed to open the serial port at the given path.
    #[error("failed to open serial port at {path}")]
    Open {
        /// The filesystem path that could not be opened.
        path: String,
        /// The underlying I/O error.
        source: std::io::Error,
    },

    /// No matching serial device was found.
    #[error("no matching serial device found")]
    NotFound,

    /// The serial connection was lost.
    #[error("serial connection lost")]
    Disconnected(
        /// The underlying I/O error.
        std::io::Error,
    ),

    /// A write to the serial port failed.
    #[error("serial write failed")]
    Write(
        /// The underlying I/O error.
        std::io::Error,
    ),

    /// A read from the serial port failed.
    #[error("serial read failed")]
    Read(
        /// The underlying I/O error.
        std::io::Error,
    ),
}

/// Errors in the CAT protocol layer (framing, field parsing, etc.).
#[derive(Debug, Error)]
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
}

/// Errors raised when a user-supplied value fails validation.
#[derive(Debug, Error)]
pub enum ValidationError {
    /// The CTCSS tone code is outside the valid range 0-49.
    #[error("tone code {0} out of range (must be 0-49)")]
    ToneCodeOutOfRange(
        /// The invalid tone code.
        u8,
    ),

    /// The band index is outside the valid range 0-13.
    #[error("band index {0} out of range (must be 0-13)")]
    BandOutOfRange(
        /// The invalid band index.
        u8,
    ),

    /// The operating mode is outside the valid range 0-8.
    #[error("mode {0} out of range (must be 0-8: FM/DV/AM/LSB/USB/CW/NFM/WFM/DR)")]
    ModeOutOfRange(
        /// The invalid mode value.
        u8,
    ),

    /// The memory (flash) mode is outside the valid range 0-7.
    #[error("memory mode {0} out of range (must be 0-7: FM/DV/AM/LSB/USB/CW/NFM/DR)")]
    MemoryModeOutOfRange(
        /// The invalid memory mode value.
        u8,
    ),

    /// The power level is outside the valid range 0-3.
    #[error("power level {0} out of range (must be 0-3: High/Medium/Low/ExtraLow)")]
    PowerLevelOutOfRange(
        /// The invalid power level.
        u8,
    ),

    /// The tone mode is outside the valid range 0-2.
    #[error("tone mode {0} out of range (must be 0-2: Off/CTCSS/DCS)")]
    ToneModeOutOfRange(
        /// The invalid tone mode.
        u8,
    ),

    /// The shift direction is outside the valid 4-bit range 0-15.
    #[error("shift direction {0} out of range (must be 0-15)")]
    ShiftOutOfRange(
        /// The invalid shift direction.
        u8,
    ),

    /// The step size index is outside the valid range 0-11.
    #[error("step size {0} out of range (must be 0-11)")]
    StepSizeOutOfRange(
        /// The invalid step size.
        u8,
    ),

    /// The data speed is outside the valid range 0-1.
    #[error("data speed {0} out of range (must be 0-1)")]
    DataSpeedOutOfRange(
        /// The invalid data speed.
        u8,
    ),

    /// The lockout mode is outside the valid range 0-2.
    #[error("lockout mode {0} out of range (must be 0-2)")]
    LockoutOutOfRange(
        /// The invalid lockout mode.
        u8,
    ),

    /// The DCS code index is not in the valid code table.
    #[error("DCS code index {0} not in valid code table")]
    DcsCodeInvalid(
        /// The invalid DCS code index.
        u8,
    ),

    /// The channel name exceeds the maximum length of 8 characters.
    #[error("channel name too long ({len} chars, max 8)")]
    ChannelNameTooLong {
        /// The actual length of the channel name.
        len: usize,
    },

    /// The frequency is outside the valid range for the band.
    #[error("frequency {0} Hz out of range for band")]
    FrequencyOutOfRange(
        /// The invalid frequency in Hz.
        u32,
    ),

    /// The digital squelch code is outside the valid range 0-99.
    #[error("digital squelch code {0} out of range (must be 0-99)")]
    DigitalSquelchCodeOutOfRange(
        /// The invalid digital squelch code.
        u8,
    ),

    /// The cross-tone type is outside the valid range 0-3.
    #[error("cross-tone type {0} out of range (must be 0-3)")]
    CrossToneTypeOutOfRange(
        /// The invalid cross-tone type value.
        u8,
    ),

    /// The flash digital squelch mode is outside the valid range 0-2.
    #[error("flash digital squelch mode {0} out of range (must be 0-2)")]
    FlashDigitalSquelchOutOfRange(
        /// The invalid flash digital squelch value.
        u8,
    ),

    /// The channel number is outside the valid range.
    #[error("channel {channel} out of range (max {max})")]
    ChannelOutOfRange {
        /// The invalid channel number.
        channel: u16,
        /// The maximum valid channel number.
        max: u16,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn validation_error_display() {
        let err = ValidationError::ToneCodeOutOfRange(50);
        assert_eq!(err.to_string(), "tone code 50 out of range (must be 0-49)");
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
        let val_err = ValidationError::BandOutOfRange(14);
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

    #[test]
    fn timeout_error_display() {
        let err = Error::Timeout(Duration::from_secs(5));
        assert!(err.to_string().contains("5s"));
    }
}
