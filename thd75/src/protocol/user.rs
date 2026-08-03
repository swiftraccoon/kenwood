//! Radio-type command: TY.
//!
//! Provides parsing of the radio region and hardware variant response.
//! Serialization is handled inline by the main dispatcher.

use crate::error::ProtocolError;

use super::Response;

/// Parse the TY command response from mnemonic and payload.
///
/// Returns `None` if the mnemonic is not TY. `US` is a write-only unresolved
/// handler and `0E` is a service-state transition; neither belongs in the
/// ordinary CAT response model.
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
    let (region_str, variant_str) =
        payload
            .split_once(',')
            .ok_or_else(|| ProtocolError::FieldParse {
                command: "TY".to_owned(),
                field: "all".to_owned(),
                detail: format!("expected region,variant, got {payload:?}"),
            })?;

    if !matches!(region_str, "E" | "J" | "K" | "0") {
        return Err(ProtocolError::FieldParse {
            command: "TY".to_owned(),
            field: "region".to_owned(),
            detail: format!("expected E, J, K, or 0, got {region_str:?}"),
        });
    }

    let variant_bytes = variant_str.as_bytes();
    let &[variant_byte] = variant_bytes else {
        return Err(ProtocolError::FieldParse {
            command: "TY".to_owned(),
            field: "variant".to_owned(),
            detail: format!("expected one hexadecimal digit, got {variant_str:?}"),
        });
    };
    if !variant_byte.is_ascii_digit() && !(b'A'..=b'F').contains(&variant_byte) {
        return Err(ProtocolError::FieldParse {
            command: "TY".to_owned(),
            field: "variant".to_owned(),
            detail: format!("expected one uppercase hexadecimal digit, got {variant_str:?}"),
        });
    }
    let variant =
        u8::from_str_radix(variant_str, 16).map_err(|error| ProtocolError::FieldParse {
            command: "TY".to_owned(),
            field: "variant".to_owned(),
            detail: error.to_string(),
        })?;

    Ok(Response::RadioType {
        region: region_str.to_owned(),
        variant,
    })
}
