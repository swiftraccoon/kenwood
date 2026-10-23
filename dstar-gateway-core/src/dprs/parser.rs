//! Bounded parsing of uncompressed DPRS position sentences.

use std::str::FromStr;

use super::crc::compute_sentence_crc;
use super::{
    ChecksumPolicy, DprsComment, DprsError, DprsPosition, DprsReport, DprsSource, DprsSymbol,
    Latitude, Longitude, SymbolCode, SymbolTable,
};

/// Maximum supported DPRS sentence length including its terminating CR.
///
/// This is an implementation resource policy following the 300-byte collector
/// in `ircDDBGateway/Common/APRSCollector.cpp::addCRCData`, not a universal
/// DPRS wire maximum. An optional final LF is not counted.
pub const MAX_SENTENCE_BYTES: usize = 300;

impl DprsReport {
    /// Parse an uncompressed position with an explicit checksum policy.
    ///
    /// The `$$CRC` prefix requires exactly four hexadecimal digits and a comma.
    /// Source identifiers end at `>`; route tokens end at `:`. The `!` position
    /// marker can appear within the first 40 information bytes, as APRS 1.0.1
    /// permits for X1J headers; `=` is supported at the first information byte.
    /// Coordinates require their exact decimal points, hemispheres, minute
    /// ranges and geographic endpoints. The table selector is between latitude
    /// and longitude, the code follows longitude, and remaining bytes are the
    /// plain position comment.
    ///
    /// CRC verification includes the final CR, matching the pinned
    /// `ircDDBGateway` collector. Input may include CR, CRLF, or have its line
    /// terminator removed by a caller; the same canonical CR is used in each
    /// case. `FromStr` selects [`ChecksumPolicy::Verify`].
    ///
    /// # Errors
    ///
    /// Returns typed [`DprsError`] variants for size, checksum syntax/integrity,
    /// header/source grammar, coordinate syntax/range, symbol, or comment
    /// failures. Ignoring checksum integrity never bypasses field validation.
    pub fn parse(sentence: &str, checksum: ChecksumPolicy) -> Result<Self, DprsError> {
        if !sentence.starts_with("$$CRC") {
            return Err(DprsError::MissingCrcPrefix);
        }
        let sentence = sentence.strip_suffix('\n').unwrap_or(sentence);
        let sentence = sentence.strip_suffix('\r').unwrap_or(sentence);
        if sentence.len() >= MAX_SENTENCE_BYTES {
            return Err(DprsError::SentenceTooLong {
                got: sentence.len().saturating_add(1),
                maximum: MAX_SENTENCE_BYTES,
            });
        }
        if sentence.len() < 10 {
            return Err(DprsError::TooShort {
                got: sentence.len(),
            });
        }
        let digits = sentence.get(5..9).ok_or(DprsError::MalformedCrcField)?;
        if sentence.as_bytes().get(9) != Some(&b',')
            || !digits.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            return Err(DprsError::MalformedCrcField);
        }
        let on_wire = u16::from_str_radix(digits, 16).map_err(|_| DprsError::MalformedCrcField)?;
        let body = sentence.get(10..).ok_or(DprsError::MalformedHeader)?;
        let computed = compute_sentence_crc(body.as_bytes());
        if checksum == ChecksumPolicy::Verify && computed != on_wire {
            return Err(DprsError::CrcMismatch { computed, on_wire });
        }
        let (header, information) = body.split_once(':').ok_or(DprsError::MalformedHeader)?;
        let (source, route) = header.split_once('>').ok_or(DprsError::MalformedHeader)?;
        let source = DprsSource::new(source.trim_end_matches(' '))?;
        if route.split(',').any(|token| {
            token.is_empty()
                || !token
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'*'))
        }) {
            return Err(DprsError::MalformedHeader);
        }
        let position = if let Some(position) = information.strip_prefix('=') {
            position
        } else {
            let marker = information
                .find('!')
                .filter(|index| *index < 40)
                .ok_or(DprsError::MalformedHeader)?;
            information
                .get(marker + 1..)
                .ok_or(DprsError::MalformedHeader)?
        };
        let bytes = position.as_bytes();
        let latitude = coordinate(
            bytes.get(..8).ok_or(DprsError::MalformedCoordinates)?,
            2,
            b'N',
            b'S',
        )?;
        let longitude = coordinate(
            bytes.get(9..18).ok_or(DprsError::MalformedCoordinates)?,
            3,
            b'E',
            b'W',
        )?;
        let table = char::from(*bytes.get(8).ok_or(DprsError::MalformedCoordinates)?);
        let code = char::from(*bytes.get(18).ok_or(DprsError::MalformedCoordinates)?);
        let comment = position.get(19..).ok_or(DprsError::MalformedCoordinates)?;
        Ok(Self::new(
            source,
            DprsPosition {
                latitude: Latitude::new(latitude)?,
                longitude: Longitude::new(longitude)?,
            },
            DprsSymbol::new(SymbolTable::try_from(table)?, SymbolCode::new(code)?),
            DprsComment::new(comment)?,
        ))
    }
}

impl FromStr for DprsReport {
    type Err = DprsError;

    fn from_str(sentence: &str) -> Result<Self, Self::Err> {
        Self::parse(sentence, ChecksumPolicy::Verify)
    }
}

/// Parse fixed degree/minute bytes without accepting signs or float syntax.
fn coordinate(
    bytes: &[u8],
    degree_digits: usize,
    positive: u8,
    negative: u8,
) -> Result<f64, DprsError> {
    let dot = degree_digits + 2;
    let hemisphere = bytes
        .last()
        .copied()
        .ok_or(DprsError::MalformedCoordinates)?;
    if bytes.len() != degree_digits + 6
        || bytes.get(dot) != Some(&b'.')
        || !matches!(hemisphere, byte if byte == positive || byte == negative)
    {
        return Err(DprsError::MalformedCoordinates);
    }
    let number = |start: usize, end: usize| -> Result<u16, DprsError> {
        let mut value = 0_u16;
        for byte in bytes
            .get(start..end)
            .ok_or(DprsError::MalformedCoordinates)?
        {
            if !byte.is_ascii_digit() {
                return Err(DprsError::MalformedCoordinates);
            }
            value = value * 10 + u16::from(byte - b'0');
        }
        Ok(value)
    };
    let degrees = number(0, degree_digits)?;
    let minutes = number(degree_digits, dot)?;
    let hundredths = number(dot + 1, dot + 3)?;
    if minutes >= 60 {
        return Err(DprsError::MalformedCoordinates);
    }
    let magnitude =
        f64::from(degrees) + (f64::from(minutes) + f64::from(hundredths) / 100.0) / 60.0;
    Ok(if hemisphere == negative {
        -magnitude
    } else {
        magnitude
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn sentence(body: &str) -> String {
        format!(
            "$$CRC{:04X},{body}\r",
            compute_sentence_crc(body.as_bytes())
        )
    }

    #[test]
    fn variable_source_symbol_offsets_and_crc_terminator_have_independent_contracts() -> TestResult
    {
        let wire = sentence("W1AW>APDPRS,DSTAR*:!3530.00N/08233.00W>Asheville test");
        let report: DprsReport = wire.parse()?;
        assert_eq!(report.source().as_str(), "W1AW");
        assert_eq!(report.symbol().table(), SymbolTable::Primary);
        assert_eq!(report.symbol().code().as_char(), '>');
        assert_eq!(report.comment().as_str(), "Asheville test");
        assert!((report.latitude().degrees() - 35.5).abs() < f64::EPSILON);
        assert!((report.longitude().degrees() + 82.55).abs() < f64::EPSILON);
        assert_eq!(wire.trim_end_matches('\r').parse::<DprsReport>()?, report);
        assert_eq!(format!("{wire}\n").parse::<DprsReport>()?, report);
        let south: DprsReport =
            sentence("VK2ABC-15>APDPRS,DSTAR*:!3351.00S\\15112.00EK").parse()?;
        assert_eq!(south.source().as_str(), "VK2ABC-15");
        assert!(south.latitude().degrees() < 0.0 && south.longitude().degrees() > 0.0);
        assert_eq!(south.symbol().table(), SymbolTable::Alternate);
        assert_eq!(south.symbol().code().as_char(), 'K');
        assert_eq!(south.comment().as_str(), "");
        Ok(())
    }

    #[test]
    fn checksum_policy_never_bypasses_field_grammar() -> TestResult {
        assert!(matches!(
            "HELLO".parse::<DprsReport>(),
            Err(DprsError::MissingCrcPrefix)
        ));
        assert!(matches!(
            "$$CRC12".parse::<DprsReport>(),
            Err(DprsError::TooShort { got: 7 })
        ));
        let placeholder = "$$CRC0000,W1AW>APDPRS,DSTAR*:!3530.00N/08233.00W>";
        assert!(
            matches!(
                placeholder.parse::<DprsReport>(),
                Err(DprsError::CrcMismatch { .. })
            ),
            "ordinary parsing verifies integrity over the complete CR-terminated body"
        );
        let _diagnostic = DprsReport::parse(placeholder, ChecksumPolicy::Ignore)?;
        for invalid in [
            "$$CRCG000,W1AW>APDPRS,DSTAR*:!3530.00N/08233.00W>",
            "$$CRC0000;W1AW>APDPRS,DSTAR*:!3530.00N/08233.00W>",
            "$$CRC0000,W1AW>APDPRS,DSTAR*:!3530.00X/08233.00W>",
            "$$CRC0000,W1AW>APDPRS,DSTAR*:!3560.00N/08233.00W>",
            "$$CRC0000,W1AW>APDPRS,DSTAR*:!35e0.00N/08233.00W>",
            "$$CRC0000,W1AW>APDPRS,DSTAR*:!3530,00N/08233.00W>",
            "$$CRC0000,W1AW>APDPRS,DSTAR*:!9000.01N/08233.00W>",
            "$$CRC0000,W1AW>APDPRS,DSTAR*:!3530.00N/18000.01W>",
            "$$CRC0000,W1AW>APDPRS,DSTAR*:!3530.00N?08233.00W>",
            "$$CRC0000,W1AW>APDPRS,DSTAR*:!3530.00N/08233.00W ",
            "$$CRC0000,W1AW-16>APDPRS,DSTAR*:!3530.00N/08233.00W>",
        ] {
            assert!(
                DprsReport::parse(invalid, ChecksumPolicy::Ignore).is_err(),
                "checksum diagnostics cannot admit malformed fields: {invalid:?}"
            );
        }
        let padded: DprsReport = sentence("W1AW    >APDPRS,DSTAR*:!3530.00N/08233.00W>").parse()?;
        assert_eq!(
            padded.source().as_str(),
            "W1AW",
            "legacy space padding ends before the source delimiter"
        );
        let position = "0000.00N/00000.00E>";
        let last_permitted = sentence(&format!("W1AW>APDPRS,DSTAR*:{}!{position}", "X".repeat(39)));
        let _last_permitted: DprsReport = last_permitted.parse()?;
        let too_late = sentence(&format!("W1AW>APDPRS,DSTAR*:{}!{position}", "X".repeat(40)));
        assert!(
            matches!(
                too_late.parse::<DprsReport>(),
                Err(DprsError::MalformedHeader)
            ),
            "a position marker beyond the protocol's fortieth information character is not a position report"
        );
        let oversized = format!("$$CRC0000,{}", "a".repeat(MAX_SENTENCE_BYTES));
        assert!(
            matches!(
                DprsReport::parse(&oversized, ChecksumPolicy::Ignore),
                Err(DprsError::SentenceTooLong { .. })
            ),
            "diagnostic parsing retains the input resource bound"
        );
        Ok(())
    }
}
