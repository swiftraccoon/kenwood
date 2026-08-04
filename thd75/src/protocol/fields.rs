//! Exact parsers for the lexical building blocks used by CAT responses.
//!
//! Rust's general-purpose numeric parsers accept forms such as a leading `+`.
//! The TH-D75 wire protocol does not. Keeping the ASCII grammar here prevents
//! individual command parsers from accidentally broadening it.

use crate::error::ProtocolError;

fn field_error(command: &str, field: &str, detail: String) -> ProtocolError {
    ProtocolError::FieldParse {
        command: command.to_owned(),
        field: field.to_owned(),
        detail,
    }
}

/// Parse a nonempty, unsigned decimal `u8` using ASCII digits only.
pub(super) fn decimal_u8(value: &str, command: &str, field: &str) -> Result<u8, ProtocolError> {
    if value.is_empty() || !value.as_bytes().iter().all(u8::is_ascii_digit) {
        return Err(field_error(
            command,
            field,
            format!("expected one or more decimal digits, got {value:?}"),
        ));
    }

    value.bytes().try_fold(0_u8, |parsed, byte| {
        parsed
            .checked_mul(10)
            .and_then(|value| value.checked_add(byte - b'0'))
            .ok_or_else(|| {
                field_error(
                    command,
                    field,
                    format!("decimal value does not fit in one byte: {value:?}"),
                )
            })
    })
}

/// Parse exactly `WIDTH` unsigned decimal digits into a `u8`.
pub(super) fn fixed_decimal_u8<const WIDTH: usize>(
    value: &str,
    command: &str,
    field: &str,
) -> Result<u8, ProtocolError> {
    if value.len() != WIDTH || !value.as_bytes().iter().all(u8::is_ascii_digit) {
        return Err(field_error(
            command,
            field,
            format!("expected exactly {WIDTH} decimal digit(s), got {value:?}"),
        ));
    }
    decimal_u8(value, command, field)
}

/// Parse the firmware's zero-padded decimal format for one byte.
///
/// The formatter pads to at least `MINIMUM_WIDTH` characters but does not
/// truncate larger values. For example, a minimum width of two emits `08` and
/// `128`, while rejecting noncanonical forms such as `8` and `0128`.
pub(super) fn zero_padded_decimal_u8<const MINIMUM_WIDTH: usize>(
    value: &str,
    command: &str,
    field: &str,
) -> Result<u8, ProtocolError> {
    let parsed = decimal_u8(value, command, field)?;
    let digits = if parsed >= 100 {
        3
    } else if parsed >= 10 {
        2
    } else {
        1
    };
    let expected_width = MINIMUM_WIDTH.max(digits);
    if value.len() != expected_width {
        return Err(field_error(
            command,
            field,
            format!(
                "expected decimal {parsed} zero-padded to exactly {expected_width} digit(s), got {value:?}"
            ),
        ));
    }
    Ok(parsed)
}

/// Parse the exact CAT Boolean digits `0` and `1`.
pub(super) fn boolean(value: &str, command: &str, field: &str) -> Result<bool, ProtocolError> {
    match value {
        "0" => Ok(false),
        "1" => Ok(true),
        _ => Err(field_error(
            command,
            field,
            format!("expected 0 or 1, got {value:?}"),
        )),
    }
}

/// Require the empty payload returned for a bare action command.
pub(super) fn empty_payload(payload: &str, command: &str) -> Result<(), ProtocolError> {
    if payload.is_empty() {
        Ok(())
    } else {
        Err(field_error(
            command,
            "payload",
            format!("expected an empty payload, got {payload:?}"),
        ))
    }
}

/// Parse one uppercase hexadecimal digit.
pub(super) fn upper_hex_nibble(
    value: &str,
    command: &str,
    field: &str,
) -> Result<u8, ProtocolError> {
    let parsed = match value.as_bytes() {
        [byte @ b'0'..=b'9'] => byte - b'0',
        [byte @ b'A'..=b'F'] => byte - b'A' + 10,
        _ => {
            return Err(field_error(
                command,
                field,
                format!("expected one uppercase hexadecimal digit, got {value:?}"),
            ));
        }
    };
    Ok(parsed)
}

/// Split a comma-separated payload into exactly `COUNT` borrowed fields.
///
/// This performs no allocation and rejects both missing and additional
/// fields before any command-specific parsing begins.
pub(super) fn split_exact<'a, const COUNT: usize>(
    payload: &'a str,
    command: &str,
) -> Result<[&'a str; COUNT], ProtocolError> {
    let actual = if payload.is_empty() {
        0
    } else {
        payload.split(',').count()
    };
    if actual != COUNT {
        return Err(ProtocolError::FieldCount {
            command: command.to_owned(),
            expected: COUNT,
            actual,
        });
    }

    let mut fields = [""; COUNT];
    let mut values = payload.split(',');
    for field in &mut fields {
        let Some(value) = values.next() else {
            return Err(ProtocolError::FieldCount {
                command: command.to_owned(),
                expected: COUNT,
                actual,
            });
        };
        *field = value;
    }
    if values.next().is_some() {
        return Err(ProtocolError::FieldCount {
            command: command.to_owned(),
            expected: COUNT,
            actual,
        });
    }
    Ok(fields)
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn decimal_parser_accepts_only_unsigned_ascii_digits() -> TestResult {
        assert_eq!(decimal_u8("0", "TS", "value")?, 0);
        assert_eq!(decimal_u8("255", "TS", "value")?, 255);

        for malformed in ["", "+1", "-1", " 1", "1 ", "1_0", "\u{0661}"] {
            assert!(
                decimal_u8(malformed, "TS", "value").is_err(),
                "non-wire decimal form was accepted: {malformed:?}"
            );
        }
        assert!(decimal_u8("256", "TS", "value").is_err());
        Ok(())
    }

    #[test]
    fn fixed_decimal_parser_enforces_width() -> TestResult {
        assert_eq!(fixed_decimal_u8::<3>("091", "TS", "value")?, 91);
        for malformed in ["91", "0091", "+91", "09A"] {
            assert!(fixed_decimal_u8::<3>(malformed, "TS", "value").is_err());
        }
        Ok(())
    }

    #[test]
    fn empty_payload_parser_rejects_every_nonempty_shape() -> TestResult {
        empty_payload("", "TS")?;
        for malformed in ["0", " ", ","] {
            assert!(empty_payload(malformed, "TS").is_err());
        }
        Ok(())
    }

    #[test]
    fn zero_padded_decimal_parser_matches_firmware_minimum_width() -> TestResult {
        assert_eq!(zero_padded_decimal_u8::<2>("08", "TS", "value")?, 8);
        assert_eq!(zero_padded_decimal_u8::<2>("99", "TS", "value")?, 99);
        assert_eq!(zero_padded_decimal_u8::<2>("128", "TS", "value")?, 128);
        assert_eq!(zero_padded_decimal_u8::<3>("008", "TS", "value")?, 8);
        assert_eq!(zero_padded_decimal_u8::<3>("128", "TS", "value")?, 128);

        for malformed in ["8", "008", "0128", "+08", "0A"] {
            assert!(zero_padded_decimal_u8::<2>(malformed, "TS", "value").is_err());
        }
        Ok(())
    }

    #[test]
    fn hexadecimal_parser_accepts_one_uppercase_nibble() -> TestResult {
        assert_eq!(upper_hex_nibble("0", "TS", "value")?, 0);
        assert_eq!(upper_hex_nibble("A", "TS", "value")?, 10);
        assert_eq!(upper_hex_nibble("F", "TS", "value")?, 15);
        for malformed in ["", "a", "00", "+A", "G"] {
            assert!(upper_hex_nibble(malformed, "TS", "value").is_err());
        }
        Ok(())
    }

    #[test]
    fn exact_splitter_preserves_empty_fields_without_allocating() -> TestResult {
        assert_eq!(split_exact::<3>("A,,C", "TS")?, ["A", "", "C"]);
        assert!(matches!(
            split_exact::<3>("A,B", "TS"),
            Err(ProtocolError::FieldCount {
                expected: 3,
                actual: 2,
                ..
            })
        ));
        assert!(matches!(
            split_exact::<3>("A,B,C,D", "TS"),
            Err(ProtocolError::FieldCount {
                expected: 3,
                actual: 4,
                ..
            })
        ));
        Ok(())
    }
}
