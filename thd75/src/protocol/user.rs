//! User/extra commands: US, TY, 0E.
//!
//! Provides parsing of responses for user settings and extra commands.
//! Serialization is handled inline by the main dispatcher.

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

/// Parse a user/extra command response from mnemonic and payload.
///
/// Handles US (user settings), TY (radio type/region), and 0E (MCP status).
/// Returns `None` if the mnemonic is not handled by this module.
pub(crate) fn parse_user(mnemonic: &str, payload: &str) -> Option<Result<Response, ProtocolError>> {
    match mnemonic {
        "US" => Some(
            parse_u8_field(payload, "US", "value").map(|value| Response::UserSettings { value }),
        ),
        "TY" => Some(parse_ty(payload)),
        "0E" => Some(Ok(Response::McpStatus {
            value: payload.to_owned(),
        })),
        _ => None,
    }
}

/// Parse a TY (radio type/region) response.
///
/// Format: `region,variant` (e.g., `K,2`).
fn parse_ty(payload: &str) -> Result<Response, ProtocolError> {
    let (region_str, variant_str) =
        payload
            .split_once(',')
            .ok_or_else(|| ProtocolError::FieldParse {
                command: "TY".to_owned(),
                field: "all".to_owned(),
                detail: format!("expected region,variant, got {payload:?}"),
            })?;

    let variant = parse_u8_field(variant_str, "TY", "variant")?;

    Ok(Response::RadioType {
        region: region_str.to_owned(),
        variant,
    })
}
