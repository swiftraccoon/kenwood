//! Memory commands: ME, MR, 0M.
//!
//! Provides serialization of ME write commands and parsing of ME responses.
//! MR (recall) and 0M (programming mode) are action commands whose responses
//! are either echoes or absent.

use crate::error::ProtocolError;
use crate::types::Band;

use super::core::{CHANNEL_FIELD_COUNT, parse_channel_fields, serialize_channel_fields};
use super::{Command, Response};

/// Serialize a memory write command into its wire-format body (without trailing `\r`).
///
/// The ME wire format has 23 comma-separated fields: 1 channel number followed
/// by 22 data fields. Two ME-specific fields (at positions 14 and 22) are
/// inserted relative to the 20-field FO layout and serialized as `0`.
///
/// Returns `None` if the command is not a memory write command.
pub(crate) fn serialize_memory_write(cmd: &Command) -> Option<String> {
    match cmd {
        Command::SetMemoryChannel { channel, data } => {
            let fo = serialize_channel_fields(data);
            // FO serializes 20 comma-separated fields. Split them so we can
            // insert the two ME-specific extras.
            let parts: Vec<&str> = fo.split(',').collect();
            debug_assert_eq!(parts.len(), CHANNEL_FIELD_COUNT);

            // Reconstruct ME layout:
            //   parts[0..=12]  -> ME fields 1..=13  (freq through x5)
            //   "0"            -> ME field 14        (ME-specific)
            //   parts[13..=19] -> ME fields 15..=21  (tt through dm)
            //   "0"            -> ME field 22        (ME-specific)
            let me_body: String = parts[..13]
                .iter()
                .copied()
                .chain(std::iter::once("0"))
                .chain(parts[13..].iter().copied())
                .chain(std::iter::once("0"))
                .collect::<Vec<&str>>()
                .join(",");

            Some(format!("ME {channel:03},{me_body}"))
        }
        _ => None,
    }
}

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
        "0M" => Some(Ok(Response::ProgrammingMode)),
        _ => None,
    }
}

/// Number of comma-separated fields in an ME response (channel + 22 data).
const ME_FIELD_COUNT: usize = 23;

/// Parse an ME (memory channel) response.
///
/// ME responses contain 23 comma-separated fields: 1 channel number followed by
/// 22 data fields. The ME layout differs from FO by inserting one extra field
/// at position 14 (between x5 and tone-code) and one extra field at position 22
/// (after data-mode):
///
/// ```text
/// ME layout (22 data fields after channel):
///   [ 1.. 13] freq, offset, step, shift, reverse, tone, ctcss, dcs, x1-x5
///   [14]      ME-specific field (unknown purpose)
///   [15..=21] tt, cc, ddd, ds, urcall, lo, dm
///   [22]      ME-specific field (unknown purpose)
/// ```
///
/// We remap these into the 20-field FO order and delegate to
/// [`parse_channel_fields`].
fn parse_me(payload: &str) -> Result<Response, ProtocolError> {
    let fields: Vec<&str> = payload.split(',').collect();

    if fields.len() != ME_FIELD_COUNT {
        return Err(ProtocolError::FieldCount {
            command: "ME".to_owned(),
            expected: ME_FIELD_COUNT,
            actual: fields.len(),
        });
    }

    let channel = fields[0]
        .parse::<u16>()
        .map_err(|_| ProtocolError::FieldParse {
            command: "ME".to_owned(),
            field: "channel".to_owned(),
            detail: format!("invalid channel number: {:?}", fields[0]),
        })?;

    // Remap ME fields to the 20-field FO layout, skipping the two ME-specific
    // fields at indices 14 and 22.
    //   fields[1..=13]  -> FO fields 0..=12  (freq through x5, 13 items)
    //   fields[15..=21] -> FO fields 13..=19 (tt through dm, 7 items)
    let fo_fields: Vec<&str> = fields[1..=13]
        .iter()
        .chain(fields[15..=21].iter())
        .copied()
        .collect();

    debug_assert_eq!(fo_fields.len(), CHANNEL_FIELD_COUNT);

    let data = parse_channel_fields(&fo_fields, "ME")?;

    Ok(Response::MemoryChannel { channel, data })
}

/// Parse an MR response.
///
/// Two formats are supported:
/// - Write acknowledgment: `band,channel` (comma-separated, e.g., `0,021`)
/// - Read response: `bandCCC` (no comma, e.g., `021` meaning band 0 channel 21)
///
/// Hardware-verified: `MR 0\r` returns `MR 021` (read, no comma).
/// `MR 0,021\r` returns `MR 0,021` (write acknowledgment, with comma).
fn parse_mr(payload: &str) -> Result<Response, ProtocolError> {
    if let Some((band_str, ch_str)) = payload.split_once(',') {
        // Write acknowledgment format: "band,channel"
        let band_val = band_str
            .parse::<u8>()
            .map_err(|_| ProtocolError::FieldParse {
                command: "MR".to_owned(),
                field: "band".to_owned(),
                detail: format!("invalid band: {band_str:?}"),
            })?;

        let band = Band::try_from(band_val).map_err(|e| ProtocolError::FieldParse {
            command: "MR".to_owned(),
            field: "band".to_owned(),
            detail: e.to_string(),
        })?;

        let channel = ch_str
            .parse::<u16>()
            .map_err(|_| ProtocolError::FieldParse {
                command: "MR".to_owned(),
                field: "channel".to_owned(),
                detail: format!("invalid channel number: {ch_str:?}"),
            })?;

        Ok(Response::MemoryRecall { band, channel })
    } else {
        // Read response format: "bandCCC" (no comma)
        // First character is the band digit, rest is the channel number.
        if payload.is_empty() {
            return Err(ProtocolError::FieldParse {
                command: "MR".to_owned(),
                field: "all".to_owned(),
                detail: "empty MR read payload".to_owned(),
            });
        }

        let band_str = &payload[..1];
        let ch_str = &payload[1..];

        let band_val = band_str
            .parse::<u8>()
            .map_err(|_| ProtocolError::FieldParse {
                command: "MR".to_owned(),
                field: "band".to_owned(),
                detail: format!("invalid band: {band_str:?}"),
            })?;

        let band = Band::try_from(band_val).map_err(|e| ProtocolError::FieldParse {
            command: "MR".to_owned(),
            field: "band".to_owned(),
            detail: e.to_string(),
        })?;

        let channel = ch_str
            .parse::<u16>()
            .map_err(|_| ProtocolError::FieldParse {
                command: "MR".to_owned(),
                field: "channel".to_owned(),
                detail: format!("invalid channel number: {ch_str:?}"),
            })?;

        Ok(Response::CurrentChannel { band, channel })
    }
}
