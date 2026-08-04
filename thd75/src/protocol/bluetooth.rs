//! Bluetooth commands: BT.
//!
//! Provides parsing of responses for the Bluetooth CAT protocol command.
//! Serialization is handled inline by the main dispatcher.

use crate::error::ProtocolError;

use super::Response;
use super::fields::boolean;

/// Parse a Bluetooth command response from mnemonic and payload.
///
/// Returns `None` if the mnemonic is not a Bluetooth command.
pub(crate) fn parse_bluetooth(
    mnemonic: &str,
    payload: &str,
) -> Option<Result<Response, ProtocolError>> {
    if mnemonic != "BT" {
        return None;
    }
    Some(parse_bt(payload))
}

/// Parse BT (Bluetooth): "0" or "1".
fn parse_bt(payload: &str) -> Result<Response, ProtocolError> {
    boolean(payload, "BT", "enabled").map(|enabled| Response::Bluetooth { enabled })
}
