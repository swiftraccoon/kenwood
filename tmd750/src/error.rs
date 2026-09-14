//! Error hierarchy: one top-level [`Error`] with typed sub-errors.

use kenwood_transport::TransportError;

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
    /// A transport write or reply-reading step exceeded its configured deadline.
    ///
    /// `operation` names the timed-out step. This does not establish radio
    /// silence, zero delivered bytes, or that a requested change did not occur.
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
    /// The `FV` payload contains a byte outside graphic ASCII (`0x21..=0x7E`).
    #[error("firmware identity byte 0x{value:02X} at offset {offset} is not graphic ASCII")]
    InvalidFirmwareIdentityByte {
        /// Offset of the byte.
        offset: usize,
        /// The byte.
        value: u8,
    },
    /// The `TY` payload is empty or contains a byte outside graphic ASCII.
    #[error("invalid TY radio-type payload {payload:?}; expected non-empty graphic ASCII")]
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
    /// A supported MCP command appeared where a data or fill response was required.
    ///
    /// A read-request echo is valid header framing but not a completed page
    /// response. The session remains recovery-required; no payload ACK or exit
    /// is authorized by this error.
    #[error("MCP page response command {command:?} is not Write or Fill")]
    UnexpectedPageResponse {
        /// The supported command found in the wrong response role.
        command: crate::protocol::mcp::HeaderCommand,
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
    #[error(
        "programming mode is active; exit MCP, release this connection, and identify a fresh connection before using CAT"
    )]
    SessionActive,
    /// An MCP operation was requested without an active programming session.
    #[error("programming mode is not active")]
    SessionNotActive,
    /// An interrupted exchange left the radio's protocol state uncertain.
    #[error(
        "radio protocol state is uncertain; restore normal mode and establish a fresh connection"
    )]
    RecoveryRequired,
    /// MCP exit was acknowledged; the original connection must be released.
    #[error(
        "MCP exit was acknowledged; close this connection and prove identity on a fresh connection"
    )]
    ConnectionRetired,
    /// A journaled page does not have exactly one intended patch.
    #[error(
        "recovery page at {address} (len {len}) requires exactly one intended patch; found {count}"
    )]
    RecoveryIntentCount {
        /// Journaled page address.
        address: u32,
        /// Journaled page length.
        len: usize,
        /// Matching intended patches; zero means no intended value is known.
        count: usize,
    },
    /// A recovery journal repeats the same page.
    #[error("recovery journal repeats page at {address} (len {len})")]
    DuplicateRecoveryPage {
        /// Repeated page address.
        address: u32,
        /// Repeated page length.
        len: usize,
    },
    /// A page lies outside the writable regions.
    #[error("page at {address} (len {len}) lies outside the writable regions")]
    PageNotWritable {
        /// Page address.
        address: u32,
        /// Page length.
        len: u16,
    },
    /// A replacement is not one complete page of the writable-region walk.
    #[error("page at {address} (len {len}) is not a canonical writable transfer page")]
    NonCanonicalPage {
        /// Supplied page address.
        address: u32,
        /// Supplied page length.
        len: usize,
    },
    /// Expected and replacement bytes must each cover the complete page.
    #[error(
        "replacement at {address} needs {page_len} expected and replacement bytes; \
         received {expected_len} and {replacement_len}"
    )]
    ReplacementLength {
        /// Supplied page address.
        address: u32,
        /// Canonical page length.
        page_len: usize,
        /// Supplied expected-byte count.
        expected_len: usize,
        /// Supplied replacement-byte count.
        replacement_len: usize,
    },
    /// A compare-and-exchange batch names the same page more than once.
    #[error("compare-and-exchange batch repeats the page at {address}")]
    DuplicateReplacement {
        /// Repeated page address.
        address: u32,
    },
    /// Two replacements cover overlapping byte ranges.
    #[error("compare-and-exchange pages at {first} and {second} overlap")]
    OverlappingReplacements {
        /// Earlier page address in ascending address order.
        first: u32,
        /// Later page address in ascending address order.
        second: u32,
    },
    /// A fresh complete page differs from its immutable expected before-image.
    #[error("fresh page at {address} differs from its expected bytes at offset {offset}")]
    CompareMismatch {
        /// Compared page address.
        address: u32,
        /// First differing offset within the page.
        offset: usize,
    },
    /// The caller could not durably record a page's intent before dispatch.
    #[error("durable intent for page at {address} failed: {source}")]
    DurableIntent {
        /// Page whose write was not dispatched.
        address: u32,
        /// Original caller error; earlier batch writes may still have completed.
        #[source]
        source: std::io::Error,
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
    /// A shared scalar codec rejected this model-bound field.
    #[error("field {field}: {source}")]
    Codec {
        /// Model descriptor whose storage or value was rejected.
        field: &'static str,
        /// Scalar failure with offsets relative to the field.
        #[source]
        source: kenwood_schema::CodecError,
    },
    /// Masked assignments conflict or contain an invalid bit claim.
    #[error(transparent)]
    Patch(#[from] kenwood_schema::PatchError),
    /// A named registry descriptor differs from the immutable compiled entry.
    #[error("field {field} descriptor does not match the compiled registry")]
    CatalogDescriptorMismatch {
        /// Registered field name whose metadata was changed.
        field: &'static str,
    },
    /// A byte patch is empty or contains bits outside its declared mask.
    #[error("byte patch at offset {offset} has invalid mask 0x{mask:02X} or unmasked value")]
    InvalidBytePatch {
        /// Offset within the containing page.
        offset: u8,
        /// Declared owned bits.
        mask: u8,
    },
    /// A page patch must contain at least one effective bit claim.
    #[error("page patch at {address} contains no bit claims")]
    EmptyPagePatch {
        /// Address of the page.
        address: u32,
    },
    /// A patch offset lies outside its declared page.
    #[error("page patch at {address} has offset {offset} outside its {len}-byte page")]
    PatchOffsetOutOfBounds {
        /// Address of the page.
        address: u32,
        /// Rejected in-page offset.
        offset: u8,
        /// Declared page length.
        len: usize,
    },
    /// Two byte patches claim the same bits within a page.
    #[error("page patch at {address} repeats bits at offset {offset}")]
    OverlappingPatchBits {
        /// Address of the page.
        address: u32,
        /// Offset of the overlapping claims.
        offset: u8,
    },
    /// A page operation requires its entire exact-length byte buffer.
    #[error("page at {address} requires {expected} bytes, got {actual}")]
    PatchBufferLength {
        /// Address of the page.
        address: u32,
        /// Required page length.
        expected: usize,
        /// Supplied buffer length.
        actual: usize,
    },
    /// A global field was given an unrelated Programmable-Memory slot.
    #[error("field {field} is global; omit the PM slot")]
    UnexpectedSlot {
        /// Global field name.
        field: &'static str,
    },
    /// A sparse menu snapshot lacks a complete required page.
    #[error("snapshot for {field} lacks page {address} (len {len})")]
    SnapshotPageMissing {
        /// Field or operation requiring the page.
        field: &'static str,
        /// Required page address.
        address: u32,
        /// Required page length.
        len: usize,
    },
    /// A snapshot supplied bytes that do not fill its declared page.
    #[error("snapshot page {address} requires {expected} bytes, got {actual}")]
    SnapshotPageLength {
        /// Page address.
        address: u32,
        /// Declared page length.
        expected: usize,
        /// Supplied byte count.
        actual: usize,
    },
    /// A snapshot page is not part of the known configuration transfer walk.
    #[error("snapshot page {address} (len {len}) is not a canonical configuration page")]
    SnapshotPageNotCanonical {
        /// Rejected page address.
        address: u32,
        /// Rejected page length.
        len: usize,
    },
    /// Two snapshot entries claim the same canonical page.
    #[error("snapshot repeats page {address}")]
    DuplicateSnapshotPage {
        /// Repeated page address.
        address: u32,
    },
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
    /// A value is not one of the field's allowed choices or enum members.
    #[error("field {field} value {value} is not an allowed choice")]
    DisallowedValue {
        /// Field name.
        field: &'static str,
        /// Rejected value.
        value: u64,
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
    /// The input does not contain the complete configuration header.
    #[error("file is {actual} bytes; a .d750 header requires {minimum} bytes")]
    HeaderTooShort {
        /// Actual input length.
        actual: usize,
        /// Required header length.
        minimum: usize,
    },
    /// The header does not declare a supported full or short file signature.
    #[error("invalid .d750 signature: {found:02X?}")]
    InvalidSignature {
        /// The first eight header bytes; a short signature uses the first seven.
        found: [u8; 8],
    },
    /// The model marker at header offset 16 is not `TM-D750`.
    #[error("invalid .d750 model marker: {found:02X?}")]
    InvalidModel {
        /// The exact seven marker bytes.
        found: [u8; 7],
    },
    /// The reserved header byte at offset 32 is nonzero.
    #[error("unsupported .d750 reserved byte at offset 32: {value}")]
    NonzeroReservedByte {
        /// The actual byte.
        value: u8,
    },
    /// The file length disagrees with the validated header's full or short layout.
    #[error("file is {actual} bytes; its .d750 signature requires exactly {expected} bytes")]
    Length {
        /// Actual length.
        actual: usize,
        /// Total header-plus-payload length selected by the header signature.
        expected: usize,
    },
    /// A supplied image payload has the wrong length for the validated header.
    #[error("image is {actual} bytes; its .d750 header requires exactly {expected} image bytes")]
    ImageLength {
        /// Actual payload length, excluding the header.
        actual: usize,
        /// Payload length selected by the header signature.
        expected: usize,
    },
    /// An opaque radio type cannot be encoded in a newly constructed header.
    #[error(
        "cannot construct a configuration header from radio type {payload:?}: expected three single-byte components separated by commas"
    )]
    UnsupportedRadioType {
        /// The unchanged radio-type payload that could not be represented.
        payload: String,
    },
}
