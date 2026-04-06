//! Service mode commands: 0G, 0Y, 0S, 0R, 0W, 1A, 1D, 1E, 1I, 1N, 1U, 1V, 1W, 1F, 9E, 9R, 2V, 1G, 1C.
//!
//! Provides parsing of responses for the 20 factory service mode commands
//! discovered via Ghidra firmware reverse engineering of the TH-D75 V1.03.
//! Serialization is handled inline by the main dispatcher.
//!
//! # Safety
//!
//! These commands are intended for factory use only. Service mode is entered
//! by sending `0G KENWOOD` and exited with bare `0G`. While in service mode,
//! the standard CAT command table is replaced with the service mode table.

use crate::error::ProtocolError;

use super::Response;

/// Parse a service mode command response from mnemonic and payload.
///
/// Returns `None` if the mnemonic is not a service mode command.
pub(crate) fn parse_service(
    mnemonic: &str,
    payload: &str,
) -> Option<Result<Response, ProtocolError>> {
    match mnemonic {
        "0G" => Some(Ok(Response::ServiceMode {
            data: payload.to_owned(),
        })),
        "0S" => Some(Ok(Response::ServiceCalibrationData {
            data: payload.to_owned(),
        })),
        "1A" | "1D" | "1N" | "1E" | "1V" | "1W" | "1C" | "1U" => {
            Some(Ok(Response::ServiceCalibrationParam {
                mnemonic: mnemonic.to_owned(),
                data: payload.to_owned(),
            }))
        }
        "0R" => Some(Ok(Response::ServiceCalibrationWrite {
            data: payload.to_owned(),
        })),
        "0W" => Some(Ok(Response::ServiceWriteConfig {
            data: payload.to_owned(),
        })),
        "0Y" => Some(Ok(Response::ServiceBandSelect {
            data: payload.to_owned(),
        })),
        "1I" => Some(Ok(Response::ServiceWriteId {
            data: payload.to_owned(),
        })),
        "1F" => Some(Ok(Response::ServiceFlash {
            data: payload.to_owned(),
        })),
        "9E" => Some(Ok(Response::ServiceEepromData {
            data: payload.to_owned(),
        })),
        "9R" => Some(Ok(Response::ServiceEepromAddr {
            data: payload.to_owned(),
        })),
        "2V" => Some(Ok(Response::ServiceVersion {
            data: payload.to_owned(),
        })),
        "1G" => Some(Ok(Response::ServiceHardware {
            data: payload.to_owned(),
        })),
        _ => None,
    }
}
