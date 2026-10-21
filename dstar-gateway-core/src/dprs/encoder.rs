//! Canonical CR-terminated DPRS position encoding.

use super::DprsReport;
use super::crc::compute_sentence_crc;

impl DprsReport {
    /// Encode a canonical source, route, uncompressed position and checksum.
    ///
    /// The result ends with one CR. Its CRC covers every body byte including
    /// that CR, following `ircDDBGateway/Common/APRSCollector.cpp::checkCRC`.
    /// The route is `APDPRS,DSTAR*`. Coordinates round to hundredths of a
    /// minute, carrying a rounded 60 minutes into the next degree. Table and
    /// code occupy their distinct APRS positions; comment bytes are unchanged.
    /// All field construction has already validated width and syntax.
    #[must_use]
    pub fn to_sentence(&self) -> String {
        let latitude = coordinate(self.latitude().degrees(), 2, 'N', 'S');
        let longitude = coordinate(self.longitude().degrees(), 3, 'E', 'W');
        let symbol = self.symbol();
        let body = format!(
            "{}>APDPRS,DSTAR*:!{latitude}{}{longitude}{}{}",
            self.source(),
            symbol.table(),
            symbol.code(),
            self.comment()
        );
        let crc = compute_sentence_crc(body.as_bytes());
        format!("$$CRC{crc:04X},{body}\r")
    }
}

/// Format a finite, range-checked coordinate without narrowing float casts.
fn coordinate(value: f64, width: usize, positive: char, negative: char) -> String {
    let magnitude = value.abs();
    let mut degrees = magnitude.trunc();
    let hundredths = ((magnitude - degrees) * 6000.0).round();
    let minutes = if hundredths >= 6000.0 {
        degrees += 1.0;
        0.0
    } else {
        hundredths / 100.0
    };
    let hemisphere = if value < 0.0 { negative } else { positive };
    format!("{degrees:0width$.0}{minutes:05.2}{hemisphere}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dprs::{
        DprsComment, DprsPosition, DprsSource, DprsSymbol, Latitude, Longitude, SymbolCode,
        SymbolOverlay, SymbolTable,
    };
    use crate::types::Callsign;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn report(latitude: f64, longitude: f64) -> Result<DprsReport, super::super::DprsError> {
        Ok(DprsReport::new(
            DprsSource::new("W1AW")?,
            DprsPosition {
                latitude: Latitude::new(latitude)?,
                longitude: Longitude::new(longitude)?,
            },
            DprsSymbol::new(SymbolTable::Primary, SymbolCode::new('>')?),
            DprsComment::new("Asheville test")?,
        ))
    }

    #[test]
    fn independent_wire_vector_includes_the_symbol_and_crc_terminator() -> TestResult {
        let report = report(35.5, -82.55)?;
        let wire = report.to_sentence();
        assert_eq!(
            wire, "$$CRC8FF2,W1AW>APDPRS,DSTAR*:!3530.00N/08233.00W>Asheville test\r",
            "fixed wire bytes distinguish source width, symbol placement and inclusion of CR in the checksum"
        );
        assert_eq!(wire.parse::<DprsReport>()?, report);
        Ok(())
    }

    #[test]
    fn rounded_minutes_carry_without_creating_out_of_domain_coordinates() -> TestResult {
        let north = report(12.999_999_9, -179.999_999_9)?.to_sentence();
        assert!(
            north.contains("1300.00N/18000.00W>"),
            "rounded minutes must carry before formatting: {north:?}"
        );
        let north: DprsReport = north.parse()?;
        assert_eq!(north.latitude(), Latitude::new(13.0)?);
        assert_eq!(north.longitude(), Longitude::new(-180.0)?);
        let south = report(-89.999_999_9, 179.999_999_9)?.to_sentence();
        assert!(
            south.contains("9000.00S/18000.00E>"),
            "both endpoint hemispheres remain valid: {south:?}"
        );
        let south: DprsReport = south.parse()?;
        assert_eq!(south.latitude(), Latitude::new(-90.0)?);
        assert_eq!(south.longitude(), Longitude::new(180.0)?);
        Ok(())
    }

    #[test]
    fn source_symbol_and_plain_comment_domains_reject_invalid_construction() -> TestResult {
        assert_eq!(
            DprsSource::try_from(Callsign::from_wire_bytes(*b"N0CALL C"))?.as_str(),
            "N0CALL-C"
        );
        assert_eq!(DprsSource::new("n0call-15")?.as_str(), "N0CALL-15");
        assert!(
            DprsSource::try_from(Callsign::from_wire_bytes([
                b'W', b'1', 0x80, b'W', b' ', b' ', b' ', b' '
            ],))
            .is_err(),
            "invalid receive bytes must fail before text encoding or allocation into the report"
        );
        for invalid in [
            "",
            "N0CALL-16",
            "N0CALL-",
            "N0CALL-A-B",
            " N0CALL",
            "N0CALL\n",
            "N0,CALL",
        ] {
            assert!(
                DprsSource::new(invalid).is_err(),
                "invalid source {invalid:?}"
            );
        }
        let comment = "a".repeat(43);
        assert_eq!(DprsComment::new(&comment)?.as_str(), comment);
        for invalid in [
            "a".repeat(44),
            "line\rbreak".to_owned(),
            "x|y".to_owned(),
            "x~y".to_owned(),
            "é".to_owned(),
        ] {
            assert!(
                DprsComment::new(&invalid).is_err(),
                "invalid plain comment {invalid:?}"
            );
        }
        for invalid in [' ', '\n', 'é'] {
            assert!(
                SymbolCode::new(invalid).is_err(),
                "invalid symbol code {invalid:?}"
            );
        }
        for invalid in ['#', 'a', 'é', ' '] {
            assert!(
                SymbolTable::try_from(invalid).is_err(),
                "invalid uncompressed table selector {invalid:?}"
            );
        }
        assert!(
            SymbolOverlay::new('a').is_err(),
            "lowercase overlay selectors belong to compressed formats"
        );
        let overlay = SymbolOverlay::new('3')?;
        let symbol = DprsSymbol::new(SymbolTable::Overlay(overlay), SymbolCode::new('>')?);
        let report = DprsReport::new(
            DprsSource::new("W1AW")?,
            DprsPosition {
                latitude: Latitude::new(0.0)?,
                longitude: Longitude::new(0.0)?,
            },
            symbol,
            DprsComment::EMPTY,
        );
        let parsed: DprsReport = report.to_sentence().parse()?;
        assert_eq!(
            parsed.symbol(),
            symbol,
            "the overlay belongs between the coordinates"
        );
        Ok(())
    }
}
