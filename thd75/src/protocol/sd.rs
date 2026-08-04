//! SD card commands: SD.
//!
//! Provides parsing of responses for the SD card CAT protocol command.
//! Serialization is handled inline by the main dispatcher.

use crate::error::ProtocolError;

use super::Response;
use super::fields::boolean;

/// Parse an SD card command response from mnemonic and payload.
///
/// Returns `None` if the mnemonic is not an SD command.
pub(crate) fn parse_sd(mnemonic: &str, payload: &str) -> Option<Result<Response, ProtocolError>> {
    if mnemonic != "SD" {
        return None;
    }
    Some(boolean(payload, "SD", "value").map(|present| Response::SdCard { present }))
}
