//! Memory commands: ME and MR.
//!
//! Provides serialization of ME reads and MR actions, plus parsing of ME/MR
//! responses. ME writes remain unavailable until hardware qualification.

use crate::error::ProtocolError;
use crate::types::{Band, CatMemoryChannelRecord, CurrentMemorySelector, MemoryChannelAddress};

use super::Response;
use super::core::{CHANNEL_FIELD_COUNT, parse_channel_fields};
use super::fields::{boolean, decimal_u8, split_exact};

/// Parse a memory command response from mnemonic and payload.
///
/// Returns `None` if the mnemonic is not a memory command.
pub(crate) fn parse_memory(
    mnemonic: &str,
    payload: &str,
) -> Option<Result<Response, ProtocolError>> {
    match mnemonic {
        "ME" => Some(parse_me(payload)),
        "MR" => Some(parse_mr(payload)),
        _ => None,
    }
}

/// Number of comma-separated fields in an ME response (channel + 22 data).
const ME_FIELD_COUNT: usize = 23;

/// Parse an ME (memory channel) response.
///
/// ME responses contain 23 comma-separated fields: 1 channel number followed by
/// 22 data fields. ME inserts its split flag before the shift field and appends
/// the scan-lockout flag after the shared channel data:
///
/// ```text
/// ME layout (22 data fields after channel):
///   [ 1..=12] frequency through reverse
///   [13]      split
///   [14]      shift
///   [15..=21] tone code through digital-squelch code
///   [22]      scan lockout
/// ```
///
/// We remap these into the 20-field FO order and delegate to
/// [`parse_channel_fields`].
fn parse_me(payload: &str) -> Result<Response, ProtocolError> {
    let fields = split_exact::<ME_FIELD_COUNT>(payload, "ME")?;
    let selector = parse_memory_address(fields[0], "ME")?;

    // Remap ME's shared fields to FO order. ME field 13 is the split flag;
    // field 14 is the actual shift field used as FO field 12.
    let fo_fields: [&str; CHANNEL_FIELD_COUNT] = [
        fields[1], fields[2], fields[3], fields[4], fields[5], fields[6], fields[7], fields[8],
        fields[9], fields[10], fields[11], fields[12], fields[14], fields[15], fields[16],
        fields[17], fields[18], fields[19], fields[20], fields[21],
    ];

    let channel = parse_channel_fields(&fo_fields, "ME")?;
    let split = boolean(fields[13], "ME", "split")?;
    let scan_lockout = boolean(fields[22], "ME", "scan_lockout")?;

    Ok(Response::MemoryChannel {
        selector,
        record: CatMemoryChannelRecord {
            channel,
            split,
            scan_lockout,
        },
    })
}

/// Parse an MR response.
///
/// Two formats are supported:
/// - Write acknowledgment: `band,selector` (comma-separated, e.g., `0,021`)
/// - Read response: `selector` alone (e.g., `021`, `L00`, or `Pri`)
///
/// Hardware-verified: `MR 0\r` returns `MR 021` (read, no band in the frame).
/// `MR 0,021\r` returns `MR 0,021` (write acknowledgment, with comma).
fn parse_mr(payload: &str) -> Result<Response, ProtocolError> {
    if payload.contains(',') {
        // Write acknowledgment format: "band,channel"
        let [band_str, ch_str] = split_exact::<2>(payload, "MR")?;
        let band_val = decimal_u8(band_str, "MR", "band")?;

        let band = Band::try_from(band_val).map_err(|e| ProtocolError::FieldParse {
            command: "MR".to_owned(),
            field: "band".to_owned(),
            detail: e.to_string(),
        })?;

        let selector = parse_memory_address(ch_str, "MR")?;

        Ok(Response::MemoryRecallAck { band, selector })
    } else {
        let selector = parse_current_selector(payload)?;
        Ok(Response::CurrentChannel { selector })
    }
}

fn parse_memory_address(
    payload: &str,
    command: &str,
) -> Result<MemoryChannelAddress, ProtocolError> {
    MemoryChannelAddress::try_from(payload).map_err(|error| ProtocolError::FieldParse {
        command: command.to_owned(),
        field: "memory_address".to_owned(),
        detail: error.to_string(),
    })
}

fn parse_current_selector(payload: &str) -> Result<CurrentMemorySelector, ProtocolError> {
    CurrentMemorySelector::try_from(payload).map_err(|error| ProtocolError::FieldParse {
        command: "MR".to_owned(),
        field: "current_selector".to_owned(),
        detail: error.to_string(),
    })
}
