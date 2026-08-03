//! Memory commands: ME, MR, 0M.
//!
//! Provides serialization of ME reads and MR actions, plus parsing of ME/MR
//! responses. Lossy ME writes are intentionally not exposed.

use crate::error::ProtocolError;
use crate::types::{Band, MemoryChannelRecord, MemorySelector};

use super::Response;
use super::core::{CHANNEL_FIELD_COUNT, parse_channel_fields};

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
    let actual = fields.len();

    // ME wire: `channel,f1..=f13,me14,f15..=f21,me22` (23 fields exactly).
    let &[ch_str, ref body @ ..] = fields.as_slice() else {
        return Err(ProtocolError::FieldCount {
            command: "ME".to_owned(),
            expected: ME_FIELD_COUNT,
            actual,
        });
    };
    if body.len() != ME_FIELD_COUNT - 1 {
        return Err(ProtocolError::FieldCount {
            command: "ME".to_owned(),
            expected: ME_FIELD_COUNT,
            actual,
        });
    }

    let selector = parse_selector(ch_str, "ME")?;

    // Remap ME fields to the 20-field FO layout, skipping the two ME-specific
    // fields at body indices 13 (ME field 14) and 21 (ME field 22).
    //   body[0..=12]  -> FO fields 0..=12  (freq through x5, 13 items)
    //   body[14..=20] -> FO fields 13..=19 (tt through dm, 7 items)
    let Some(head) = body.get(..13) else {
        return Err(ProtocolError::FieldCount {
            command: "ME".to_owned(),
            expected: ME_FIELD_COUNT,
            actual,
        });
    };
    let Some(tail) = body.get(14..21) else {
        return Err(ProtocolError::FieldCount {
            command: "ME".to_owned(),
            expected: ME_FIELD_COUNT,
            actual,
        });
    };
    let fo_fields: Vec<&str> = head.iter().chain(tail.iter()).copied().collect();

    debug_assert_eq!(
        fo_fields.len(),
        CHANNEL_FIELD_COUNT,
        "ME → FO reconstruction must yield exactly {CHANNEL_FIELD_COUNT} fields for the \
         shared `parse_channel_fields` path to accept the input",
    );

    let channel = parse_channel_fields(&fo_fields, "ME")?;
    let me_field_14_raw = body
        .get(13)
        .ok_or_else(|| ProtocolError::FieldCount {
            command: "ME".to_owned(),
            expected: ME_FIELD_COUNT,
            actual,
        })?
        .to_string();
    let me_field_22_raw = body
        .get(21)
        .ok_or_else(|| ProtocolError::FieldCount {
            command: "ME".to_owned(),
            expected: ME_FIELD_COUNT,
            actual,
        })?
        .to_string();

    Ok(Response::MemoryChannel {
        selector,
        record: MemoryChannelRecord {
            channel,
            me_field_14_raw,
            me_field_22_raw,
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

        let selector = parse_selector(ch_str, "MR")?;

        Ok(Response::MemoryRecall { band, selector })
    } else {
        let selector = parse_selector(payload, "MR")?;
        Ok(Response::CurrentChannel { selector })
    }
}

fn parse_selector(payload: &str, command: &str) -> Result<MemorySelector, ProtocolError> {
    MemorySelector::try_from(payload).map_err(|error| ProtocolError::FieldParse {
        command: command.to_owned(),
        field: "selector".to_owned(),
        detail: error.to_string(),
    })
}
