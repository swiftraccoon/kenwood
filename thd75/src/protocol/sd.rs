//! SD card commands: SD.
//!
//! Provides parsing of responses for the SD card CAT protocol command.
//! Serialization is handled inline by the main dispatcher.

use crate::error::ProtocolError;

use super::Response;

/// Parse a boolean field ("0" or "1").
fn parse_bool(payload: &str, cmd: &str) -> Result<bool, ProtocolError> {
    match payload {
        "0" => Ok(false),
        "1" => Ok(true),
        _ => Err(ProtocolError::FieldParse {
            command: cmd.to_owned(),
            field: "value".to_owned(),
            detail: format!("expected 0 or 1, got {payload:?}"),
        }),
    }
}

/// Parse an SD card command response from mnemonic and payload.
///
/// Returns `None` if the mnemonic is not an SD command.
pub(crate) fn parse_sd(mnemonic: &str, payload: &str) -> Option<Result<Response, ProtocolError>> {
    if mnemonic != "SD" {
        return None;
    }
    Some(parse_bool(payload, "SD").map(|present| Response::SdCard { present }))
}
