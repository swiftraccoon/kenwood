//! Radio-type command: TY.
//!
//! Provides parsing of the radio region and hardware variant response.
//! Serialization is handled inline by the main dispatcher.

use crate::error::ProtocolError;
use crate::types::{HardwareVariant, RadioRegion, RadioType};

use super::Response;
use super::fields::{split_exact, upper_hex_nibble};

/// Parse the TY command response from mnemonic and payload.
///
/// Returns `None` if the mnemonic is not TY. `US` is write-only and unresolved;
/// `0E` changes service state. Neither belongs in the ordinary CAT response
/// model.
pub(crate) fn parse_user(mnemonic: &str, payload: &str) -> Option<Result<Response, ProtocolError>> {
    match mnemonic {
        "TY" => Some(parse_ty(payload)),
        _ => None,
    }
}

/// Parse a TY (radio type/region) response.
///
/// Format: `region,variant` (e.g., `K,2`).
fn parse_ty(payload: &str) -> Result<Response, ProtocolError> {
    let [region_str, variant_str] = split_exact::<2>(payload, "TY")?;

    let region = RadioRegion::try_from(region_str).map_err(|error| ProtocolError::FieldParse {
        command: "TY".to_owned(),
        field: "region".to_owned(),
        detail: error.to_string(),
    })?;

    let variant_raw = upper_hex_nibble(variant_str, "TY", "hardware_variant")?;
    let hardware_variant =
        HardwareVariant::new(variant_raw).map_err(|error| ProtocolError::FieldParse {
            command: "TY".to_owned(),
            field: "hardware_variant".to_owned(),
            detail: error.to_string(),
        })?;

    Ok(Response::RadioType(RadioType::new(
        region,
        hardware_variant,
    )))
}
