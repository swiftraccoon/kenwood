//! Parsers for the comma-separated fields of CAT replies.
//!
//! Every helper names the command and field in its error so a malformed
//! reply is attributable without the raw line.

use crate::error::ProtocolError;
use crate::types::Band;

pub(crate) const fn field_error(
    command: &'static str,
    field: &'static str,
    detail: String,
) -> ProtocolError {
    ProtocolError::FieldParse {
        command,
        field,
        detail,
    }
}

/// Split a payload into exactly `COUNT` comma-separated fields, keeping
/// empty fields.
pub(crate) fn split_exact<'a, const COUNT: usize>(
    payload: &'a str,
    command: &'static str,
) -> Result<[&'a str; COUNT], ProtocolError> {
    let mut fields = [""; COUNT];
    let mut count = 0;
    for field in payload.split(',') {
        if let Some(slot) = fields.get_mut(count) {
            *slot = field;
        }
        count += 1;
    }
    if count == COUNT {
        Ok(fields)
    } else {
        Err(ProtocolError::FieldCount {
            command,
            expected: COUNT,
            actual: count,
        })
    }
}

/// Parse a canonical unsigned decimal (no sign, no leading zeros except `0`).
pub(crate) fn decimal_u8(
    value: &str,
    command: &'static str,
    field: &'static str,
) -> Result<u8, ProtocolError> {
    let parsed = value
        .parse::<u8>()
        .map_err(|error| field_error(command, field, error.to_string()))?;
    if value == parsed.to_string() {
        Ok(parsed)
    } else {
        Err(field_error(
            command,
            field,
            format!("noncanonical decimal value {value:?}"),
        ))
    }
}

/// Parse an unsigned decimal of exactly `WIDTH` digits, leading zeros
/// included.
pub(crate) fn fixed_decimal_u8<const WIDTH: usize>(
    value: &str,
    command: &'static str,
    field: &'static str,
) -> Result<u8, ProtocolError> {
    if value.len() != WIDTH || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(field_error(
            command,
            field,
            format!("expected exactly {WIDTH} decimal digits, got {value:?}"),
        ));
    }
    value
        .parse::<u8>()
        .map_err(|error| field_error(command, field, error.to_string()))
}

/// Parse a one-digit `0` or `1` flag.
pub(crate) fn boolean(
    value: &str,
    command: &'static str,
    field: &'static str,
) -> Result<bool, ProtocolError> {
    match value {
        "0" => Ok(false),
        "1" => Ok(true),
        other => Err(field_error(
            command,
            field,
            format!("expected 0 or 1, got {other:?}"),
        )),
    }
}

/// Parse a one-digit band index.
pub(crate) fn band(value: &str, command: &'static str) -> Result<Band, ProtocolError> {
    let raw = decimal_u8(value, command, "band")?;
    Band::try_from(raw).map_err(|error| field_error(command, "band", error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn exact_splitter_keeps_empty_fields_and_counts_strictly() -> TestResult {
        assert_eq!(split_exact::<3>("1,,", "DC")?, ["1", "", ""]);
        let short = split_exact::<3>("1,2", "DC");
        assert!(
            matches!(
                short,
                Err(ProtocolError::FieldCount {
                    command: "DC",
                    expected: 3,
                    actual: 2
                })
            ),
            "{short:?}"
        );
        let long = split_exact::<2>("0,1,2", "MD");
        assert!(
            matches!(long, Err(ProtocolError::FieldCount { actual: 3, .. })),
            "{long:?}"
        );
        Ok(())
    }

    #[test]
    fn decimal_parsers_distinguish_canonical_and_fixed_width_forms() -> TestResult {
        assert_eq!(decimal_u8("31", "SQ", "level")?, 31);
        assert!(decimal_u8("031", "SQ", "level").is_err());
        assert!(decimal_u8("+3", "SQ", "level").is_err());
        assert_eq!(fixed_decimal_u8::<2>("08", "FO", "tone_code")?, 8);
        assert!(fixed_decimal_u8::<2>("8", "FO", "tone_code").is_err());
        assert!(fixed_decimal_u8::<3>("1000", "FO", "dcs_code").is_err());
        assert!(boolean("1", "BY", "busy")?);
        assert!(boolean("2", "BY", "busy").is_err());
        assert_eq!(band("1", "FQ")?, Band::B);
        assert!(band("2", "FQ").is_err());
        Ok(())
    }
}
