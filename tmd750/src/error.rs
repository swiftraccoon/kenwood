//! Error hierarchy: one top-level [`Error`] with typed sub-errors.

pub use crate::transport::TransportError;

/// Any failure of this crate.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// The transport failed or disconnected.
    #[error(transparent)]
    Transport(#[from] TransportError),
    /// The radio's bytes did not follow the protocol.
    #[error(transparent)]
    Protocol(#[from] ProtocolError),
    /// A value failed validation before reaching the wire.
    #[error(transparent)]
    Validation(#[from] ValidationError),
    /// An MCP session rule was violated or an exchange was interrupted.
    #[error(transparent)]
    Mcp(#[from] McpError),
    /// A schema field or patch was invalid.
    #[error(transparent)]
    Schema(#[from] SchemaError),
    /// A `.d750` file was malformed.
    #[error(transparent)]
    File(#[from] FileError),
    /// The radio did not answer in time.
    #[error("{operation} timed out after {millis} ms")]
    Timeout {
        /// What was waited for.
        operation: &'static str,
        /// The timeout that elapsed.
        millis: u64,
    },
    /// The connected radio does not match the conservative schema-target gate.
    #[error(
        "MCP-D750 schema patches support only {expected_model} firmware {expected_firmware} \
         (accepted exact FV identities: {accepted:?}); connected target is model \
         {actual_model} firmware {actual_firmware}"
    )]
    UnsupportedSchemaTarget {
        /// Model the registry was generated for.
        expected_model: &'static str,
        /// Declared firmware provenance label of the generated registry.
        expected_firmware: &'static str,
        /// Exact `FV` strings accepted by the schema-target gate.
        accepted: &'static [&'static str],
        /// Model the radio reported.
        actual_model: String,
        /// Firmware the radio reported.
        actual_firmware: String,
    },
    /// The connected target has not been qualified for CAT operating-mode writes.
    #[error(
        "CAT mode writes support only firmware {expected_firmware} with TY {expected_radio_type}; \
         connected target reports firmware {actual_firmware} with TY {actual_radio_type}"
    )]
    UnsupportedCatWriteTarget {
        /// Exact qualified `FV` identity.
        expected_firmware: &'static str,
        /// Exact qualified `TY` payload.
        expected_radio_type: &'static str,
        /// Connected radio's `FV` identity.
        actual_firmware: String,
        /// Connected radio's `TY` payload.
        actual_radio_type: String,
    },
}

/// A value rejected before it reached the wire.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum ValidationError {
    /// The `ID` payload is not `TM-D750`.
    #[error("unsupported radio model {model:?}; this crate speaks only TM-D750")]
    UnsupportedRadioModel {
        /// The rejected payload.
        model: String,
    },
    /// The `FV` payload is empty or too long.
    #[error("firmware identity length {len} outside 1..={max}")]
    FirmwareIdentityLength {
        /// Rejected length.
        len: usize,
        /// Maximum accepted length.
        max: usize,
    },
    /// The `FV` payload has a non-printable byte.
    #[error("firmware identity byte 0x{value:02X} at offset {offset} is not printable ASCII")]
    InvalidFirmwareIdentityByte {
        /// Offset of the byte.
        offset: usize,
        /// The byte.
        value: u8,
    },
    /// The `TY` payload is empty or contains a non-printable byte.
    #[error("invalid TY radio-type payload {payload:?}; expected non-empty printable ASCII")]
    InvalidRadioTypePayload {
        /// The rejected payload.
        payload: String,
    },
    /// A CAT band index is not A or B.
    #[error("band index {value} is not 0 (A) or 1 (B)")]
    InvalidBand {
        /// The rejected wire value.
        value: u8,
    },
    /// An address lies outside the image.
    #[error("address {address} is outside the {image_length}-byte image")]
    AddressOutOfRange {
        /// Rejected address.
        address: u64,
        /// Image length.
        image_length: usize,
    },
    /// A region is empty, reversed, or outside the image.
    #[error("region {start}..{end} is not a non-empty range inside the {image_length}-byte image")]
    InvalidRegion {
        /// Region start.
        start: u32,
        /// Region end (exclusive).
        end: u32,
        /// Image length.
        image_length: usize,
    },
    /// A page length is not 1..=256.
    #[error("page length {len} outside 1..=256")]
    InvalidPageLength {
        /// Rejected length.
        len: usize,
    },
    /// A slot index is not below the slot count.
    #[error("slot {slot} outside 0..{count}")]
    SlotOutOfRange {
        /// Rejected slot.
        slot: u8,
        /// Slot count.
        count: u8,
    },
    /// A memory image has the wrong length.
    #[error("memory image is {actual} bytes, expected {expected}")]
    ImageLength {
        /// Actual length.
        actual: usize,
        /// Expected length.
        expected: usize,
    },
}

/// Bytes from the radio that did not follow the protocol.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum ProtocolError {
    /// A CAT reply exceeded the bounded line buffer before its terminator.
    #[error("CAT line exceeds the {limit}-byte limit")]
    CatLineTooLong {
        /// Maximum line length, excluding the carriage-return terminator.
        limit: usize,
    },
    /// A CAT line held non-ASCII bytes.
    #[error("CAT line is not ASCII: {line:?}")]
    NonAsciiLine {
        /// The raw line.
        line: Vec<u8>,
    },
    /// A CAT line had no mnemonic.
    #[error("CAT line has no mnemonic: {line:?}")]
    EmptyLine {
        /// The line.
        line: String,
    },
    /// A CAT reply field failed to parse.
    #[error("{command} reply field {field} could not be parsed: {detail}")]
    FieldParse {
        /// Command mnemonic.
        command: &'static str,
        /// Field name.
        field: &'static str,
        /// What was wrong.
        detail: String,
    },
    /// A reply was not the one the command expects.
    #[error("expected {expected} reply, got {actual}")]
    UnexpectedResponse {
        /// Expected reply kind.
        expected: &'static str,
        /// What arrived.
        actual: String,
    },
    /// The `ID` reply named another radio.
    #[error("connected radio identified as {reply:?}, not TM-D750")]
    UnexpectedIdentity {
        /// The `ID` payload.
        reply: String,
    },
    /// An MCP header command byte was not one of the known commands.
    #[error("MCP header command byte 0x{command:02X} is not R, W, or Z")]
    UnknownHeaderCommand {
        /// The byte.
        command: u8,
    },
    /// The reply header did not echo the request.
    #[error("MCP reply header {actual:?} does not echo request {expected:?}")]
    HeaderEcho {
        /// Header sent.
        expected: [u8; 5],
        /// Header received.
        actual: [u8; 5],
    },
    /// The expected ACK byte did not arrive.
    #[error("expected ACK 0x06 after {stage}, got 0x{byte:02X}")]
    MissingAck {
        /// Exchange stage.
        stage: &'static str,
        /// The byte received instead.
        byte: u8,
    },
    /// Programming-mode entry was not acknowledged.
    #[error("programming mode entry reply {reply:?} is not {expected:?}")]
    EntryReply {
        /// Expected reply.
        expected: String,
        /// Actual reply.
        reply: String,
    },
}

/// An MCP session rule violation or interrupted exchange.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum McpError {
    /// A CAT operation was requested while the radio is in programming mode.
    #[error("programming mode is active; exit the MCP session before using CAT")]
    SessionActive,
    /// An MCP operation was requested without an active programming session.
    #[error("programming mode is not active")]
    SessionNotActive,
    /// An interrupted exchange left the radio's protocol state uncertain.
    #[error(
        "radio protocol state is uncertain; restore normal mode and establish a fresh connection"
    )]
    RecoveryRequired,
    /// A page lies outside the writable regions.
    #[error("page at {address} (len {len}) lies outside the writable regions")]
    PageNotWritable {
        /// Page address.
        address: u32,
        /// Page length.
        len: u16,
    },
    /// A read-back differed from the bytes written.
    #[error("read-back of page at {address} differs from the written bytes at offset {offset}")]
    VerifyMismatch {
        /// Page address.
        address: u32,
        /// First differing offset within the page.
        offset: usize,
    },
    /// A region requested for an image was not fully read.
    #[error("region {start}..{end} was not fully read")]
    RegionNotCovered {
        /// Region start.
        start: u32,
        /// Region end (exclusive).
        end: u32,
    },
    /// An exchange failed after pages may have changed.
    #[error(
        "{operation} failed after possibly writing {possibly_written} page(s) \
         ({verified} verified): {source}"
    )]
    Interrupted {
        /// The operation.
        operation: &'static str,
        /// Pages the radio may have changed.
        possibly_written: usize,
        /// Pages read back and confirmed.
        verified: usize,
        /// The underlying failure.
        #[source]
        source: Box<Error>,
    },
}

/// A schema field or patch problem.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum SchemaError {
    /// A per-slot field was addressed without a slot.
    #[error("field {field} needs a slot index: it has a {dimension} term")]
    SlotRequired {
        /// Field name.
        field: &'static str,
        /// Dimension name.
        dimension: &'static str,
    },
    /// A field term names a dimension this crate does not know.
    #[error("field {field} has an unknown dimension {dimension}")]
    UnknownDimension {
        /// Field name.
        field: &'static str,
        /// Dimension name.
        dimension: &'static str,
    },
    /// The value kind does not match the codec.
    #[error("field {field} expects {expected}, got {actual}")]
    TypeMismatch {
        /// Field name.
        field: &'static str,
        /// Expected kind.
        expected: &'static str,
        /// Actual kind.
        actual: &'static str,
    },
    /// An unsigned value is outside its bounds.
    #[error("field {field} value {value} outside {min}..={max}")]
    UnsignedOutOfRange {
        /// Field name.
        field: &'static str,
        /// Rejected value.
        value: u64,
        /// Minimum.
        min: u64,
        /// Maximum.
        max: u64,
    },
    /// A signed value is outside its bounds.
    #[error("field {field} value {value} outside {min}..={max}")]
    SignedOutOfRange {
        /// Field name.
        field: &'static str,
        /// Rejected value.
        value: i64,
        /// Minimum.
        min: i64,
        /// Maximum.
        max: i64,
    },
    /// A value is not one of the field's allowed choices or enum members.
    #[error("field {field} value {value} is not an allowed choice")]
    DisallowedValue {
        /// Field name.
        field: &'static str,
        /// Rejected value.
        value: u64,
    },
    /// Text does not fit the fixed string.
    #[error("field {field} text of {len} bytes exceeds {max}")]
    TextTooLong {
        /// Field name.
        field: &'static str,
        /// Text length.
        len: usize,
        /// Capacity.
        max: usize,
    },
    /// Text holds a byte the encoding cannot store.
    #[error(
        "field {field} text contains a byte the {encoding} encoding cannot store: 0x{value:02X}"
    )]
    TextByte {
        /// Field name.
        field: &'static str,
        /// Encoding name.
        encoding: &'static str,
        /// The byte.
        value: u8,
    },
    /// A byte value has the wrong length.
    #[error("field {field} bytes of {len} do not match codec length {expected}")]
    BytesLength {
        /// Field name.
        field: &'static str,
        /// Given length.
        len: usize,
        /// Codec length.
        expected: usize,
    },
    /// A field's storage exceeds the image.
    #[error("field {field} at {address} (len {len}) exceeds the image ({image_length})")]
    OutOfBounds {
        /// Field name.
        field: &'static str,
        /// Resolved address.
        address: u64,
        /// Encoded length.
        len: usize,
        /// Image length.
        image_length: usize,
    },
    /// Two patches claim the same bits.
    #[error("fields {first} and {second} both claim bits of byte {address}")]
    ByteConflict {
        /// First claimant.
        first: &'static str,
        /// Second claimant.
        second: &'static str,
        /// Byte address.
        address: u32,
    },
    /// A patch lands outside every writable region.
    #[error("field {field} byte {address} lies outside the writable regions")]
    NotWritable {
        /// Field name.
        field: &'static str,
        /// Byte address.
        address: u32,
    },
    /// Blobs are not patched through the planner.
    #[error("field {field} is a blob and cannot be patched through the planner")]
    BlobNotPatchable {
        /// Field name.
        field: &'static str,
    },
}

/// A malformed `.d750` file.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum FileError {
    /// The file length is not header plus image.
    #[error("file is {actual} bytes; a .d750 file is exactly {expected} bytes")]
    Length {
        /// Actual length.
        actual: usize,
        /// Expected length.
        expected: usize,
    },
}
