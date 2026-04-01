//! D-STAR (Digital Smart Technologies for Amateur Radio) commands: DS, CS, GW.
//!
//! Provides parsing of responses for the 3 D-STAR-related CAT protocol
//! commands. Serialization is handled inline by the main dispatcher.

use crate::error::ProtocolError;

use super::Response;

/// Parse a `u8` from a string field.
fn parse_u8_field(s: &str, cmd: &str, field: &str) -> Result<u8, ProtocolError> {
    s.parse::<u8>().map_err(|_| ProtocolError::FieldParse {
        command: cmd.to_owned(),
        field: field.to_owned(),
        detail: format!("invalid u8: {s:?}"),
    })
}

/// Parse a D-STAR command response from mnemonic and payload.
///
/// Returns `None` if the mnemonic is not a D-STAR command.
pub(crate) fn parse_dstar(
    mnemonic: &str,
    payload: &str,
) -> Option<Result<Response, ProtocolError>> {
    match mnemonic {
        "DS" => Some(parse_u8_field(payload, "DS", "slot").map(|slot| Response::DstarSlot { slot })),
        "CS" => Some(parse_u8_field(payload, "CS", "slot").map(|slot| Response::ActiveCallsignSlot { slot })),
        "GW" => Some(parse_u8_field(payload, "GW", "value").map(|value| Response::Gateway { value })),
        _ => None,
    }
}
