//! Parsers for the comma-separated fields of CAT replies.
//!
//! Every helper names the command and field in its error so a malformed
//! reply is attributable without the raw line.

use crate::error::{CommandName, ProtocolError, ReplyField, ReplyFieldError};
use crate::types::Band;

pub(crate) fn field_error(
    command: CommandName,
    field: ReplyField,
    source: impl Into<ReplyFieldError>,
) -> ProtocolError {
    ProtocolError::FieldParse {
        command,
        field,
        source: source.into(),
    }
}

/// Split a payload into exactly `COUNT` comma-separated fields, keeping
/// empty fields.
pub(crate) fn split_exact<const COUNT: usize>(
    payload: &str,
    command: CommandName,
) -> Result<[&str; COUNT], ProtocolError> {
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
    command: CommandName,
    field: ReplyField,
) -> Result<u8, ProtocolError> {
    let parsed = value
        .parse::<u8>()
        .map_err(|error| field_error(command, field, error))?;
    if value == parsed.to_string() {
        Ok(parsed)
    } else {
        Err(field_error(
            command,
            field,
            ReplyFieldError::NoncanonicalDecimal {
                value: value.to_owned(),
            },
        ))
    }
}

/// Parse an unsigned decimal of exactly `WIDTH` digits, leading zeros
/// included.
pub(crate) fn fixed_decimal_u8<const WIDTH: usize>(
    value: &str,
    command: CommandName,
    field: ReplyField,
) -> Result<u8, ProtocolError> {
    if value.len() != WIDTH || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(field_error(
            command,
            field,
            ReplyFieldError::DecimalWidth {
                width: WIDTH,
                value: value.to_owned(),
            },
        ));
    }
    value
        .parse::<u8>()
        .map_err(|error| field_error(command, field, error))
}

/// Parse a one-digit `0` or `1` flag.
pub(crate) fn boolean(
    value: &str,
    command: CommandName,
    field: ReplyField,
) -> Result<bool, ProtocolError> {
    match value {
        "0" => Ok(false),
        "1" => Ok(true),
        other => Err(field_error(
            command,
            field,
            ReplyFieldError::Boolean {
                value: other.to_owned(),
            },
        )),
    }
}

/// Parse a one-digit band index.
pub(crate) fn band(value: &str, command: CommandName) -> Result<Band, ProtocolError> {
    let raw = decimal_u8(value, command, ReplyField::Band)?;
    Band::try_from(raw).map_err(|error| field_error(command, ReplyField::Band, error))
}

#[cfg(test)]
mod tests {
    use std::error::Error as _;

    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn exact_splitter_keeps_empty_fields_and_counts_strictly() -> TestResult {
        assert_eq!(split_exact::<3>("1,,", CommandName::Dc)?, ["1", "", ""]);
        let short = split_exact::<3>("1,2", CommandName::Dc);
        assert!(
            matches!(
                short,
                Err(ProtocolError::FieldCount {
                    command: CommandName::Dc,
                    expected: 3,
                    actual: 2
                })
            ),
            "{short:?}"
        );
        let long = split_exact::<2>("0,1,2", CommandName::Md);
        assert!(
            matches!(long, Err(ProtocolError::FieldCount { actual: 3, .. })),
            "{long:?}"
        );
        Ok(())
    }

    #[test]
    fn decimal_parsers_distinguish_canonical_and_fixed_width_forms() -> TestResult {
        assert_eq!(decimal_u8("31", CommandName::Sq, ReplyField::Level)?, 31);
        assert!(decimal_u8("031", CommandName::Sq, ReplyField::Level).is_err());
        assert!(decimal_u8("+3", CommandName::Sq, ReplyField::Level).is_err());
        assert_eq!(
            fixed_decimal_u8::<2>("08", CommandName::Fo, ReplyField::ToneCode)?,
            8
        );
        assert!(fixed_decimal_u8::<2>("8", CommandName::Fo, ReplyField::ToneCode).is_err());
        assert!(fixed_decimal_u8::<3>("1000", CommandName::Fo, ReplyField::DcsCode).is_err());
        assert!(boolean("1", CommandName::By, ReplyField::Busy)?);
        assert!(boolean("2", CommandName::By, ReplyField::Busy).is_err());
        assert_eq!(band("1", CommandName::Fq)?, Band::B);
        assert!(band("2", CommandName::Fq).is_err());
        Ok(())
    }

    #[test]
    fn typed_field_failures_retain_integer_and_validation_sources() -> TestResult {
        let Err(integer) = decimal_u8("999", CommandName::Sq, ReplyField::Level) else {
            return Err("out-of-range decimal was accepted".into());
        };
        assert!(
            matches!(
                integer,
                ProtocolError::FieldParse {
                    command: CommandName::Sq,
                    field: ReplyField::Level,
                    source: ReplyFieldError::Integer(_),
                }
            ),
            "{integer:?}"
        );
        let integer_source = integer.source().and_then(std::error::Error::source);
        assert!(
            integer_source.is_some_and(<dyn std::error::Error>::is::<std::num::ParseIntError>),
            "the original integer parser failure must remain in the source chain"
        );

        let Err(validation) = band("2", CommandName::Md) else {
            return Err("an unsupported band was accepted".into());
        };
        assert!(
            matches!(
                validation,
                ProtocolError::FieldParse {
                    command: CommandName::Md,
                    field: ReplyField::Band,
                    source: ReplyFieldError::Validation(crate::ValidationError::InvalidBand {
                        value: 2
                    }),
                }
            ),
            "{validation:?}"
        );
        let validation_source = validation.source().and_then(std::error::Error::source);
        assert!(
            validation_source.is_some_and(<dyn std::error::Error>::is::<crate::ValidationError>),
            "the original checked-domain error must remain in the source chain"
        );
        Ok(())
    }
}
