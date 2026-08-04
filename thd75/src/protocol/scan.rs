//! Identified scan commands: SF plus BS antenna selection.
//!
//! Provides parsing of responses for scan-related CAT protocol commands.
//!
//! Firmware-verified:
//! - SF = Step Size, band-indexed (`SF band\r` returns `SF band,step`).
//! - BS controls the MW/SW antenna (`BS\r` reads, `BS 0|1\r` writes).
//!
//! Firmware analysis identifies `SR 0/1/2` as the scan-resume operation
//! (Time/Carrier/Seek), not a reset. This module currently implements only the
//! independently qualified `SF` and `BS` CAT surfaces; the public analog and
//! digital scan-resume setters target their separate MCP menu cells.

use crate::error::ProtocolError;
use crate::types::mode::StepSize;
use crate::types::{AntennaInput, Band};

use super::Response;
use super::fields::{boolean, fixed_decimal_u8, split_exact, upper_hex_nibble};

/// Parse a scan command response from mnemonic and payload.
///
/// Returns `None` if the mnemonic is not a scan command.
///
pub(crate) fn parse_scan(mnemonic: &str, payload: &str) -> Option<Result<Response, ProtocolError>> {
    match mnemonic {
        "SF" => Some(parse_sf(payload)),
        "BS" => Some(parse_bs(payload)),
        _ => None,
    }
}

/// Parse SF (step size): `band,step`.
///
/// Firmware-verified: SF = Step Size. `SF band\r` returns `SF band,step`.
fn parse_sf(payload: &str) -> Result<Response, ProtocolError> {
    let [band_str, step_str] = split_exact::<2>(payload, "SF")?;
    let band_val = fixed_decimal_u8::<1>(band_str, "SF", "band")?;
    let band = Band::try_from(band_val).map_err(|e| ProtocolError::FieldParse {
        command: "SF".to_owned(),
        field: "band".to_owned(),
        detail: e.to_string(),
    })?;
    // Step value is one hexadecimal index; A and B select 50 and 100 kHz.
    let step_val = upper_hex_nibble(step_str, "SF", "step")?;
    let step = StepSize::try_from(step_val).map_err(|e| ProtocolError::FieldParse {
        command: "SF".to_owned(),
        field: "step".to_owned(),
        detail: e.to_string(),
    })?;
    Ok(Response::StepSize { band, step })
}

/// Parse BS (MW/SW antenna): 0 = ANT Connector, 1 = internal bar antenna.
fn parse_bs(payload: &str) -> Result<Response, ProtocolError> {
    boolean(payload, "BS", "internal bar selection").map(|internal_bar| Response::AntennaInput {
        input: if internal_bar {
            AntennaInput::InternalBar
        } else {
            AntennaInput::Connector
        },
    })
}
