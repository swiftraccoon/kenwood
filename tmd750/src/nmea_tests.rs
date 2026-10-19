use super::*;

type TestResult = Result<(), Box<dyn std::error::Error>>;

/// The NMEA 0183 reference GGA and RMC sentences.
const REFERENCE_GGA: &str = "$GPGGA,123519,4807.038,N,01131.000,E,1,08,0.9,545.4,M,46.9,M,,*47";
const REFERENCE_RMC: &str = "$GPRMC,123519,A,4807.038,N,01131.000,E,022.4,084.4,230394,003.1,W*6A";

fn close(actual: f64, expected: f64) -> bool {
    (actual - expected).abs() < 1e-9
}

#[test]
fn a_reference_gga_decodes_every_field() -> TestResult {
    let Sentence::Gga(gga) = Sentence::parse(REFERENCE_GGA)? else {
        return Err("a GGA sentence".into());
    };
    assert_eq!(gga.talker, "GP");
    assert_eq!(
        gga.time,
        Some(UtcTime {
            hour: 12,
            minute: 35,
            second: 19,
            millisecond: 0
        })
    );
    let position = gga.position.ok_or("a position")?;
    assert!(
        close(position.latitude, 48.0 + 7.038 / 60.0),
        "{position:?}"
    );
    assert!(
        close(position.longitude, 11.0 + 31.0 / 60.0),
        "{position:?}"
    );
    assert_eq!(gga.quality, FixQuality::Gps);
    assert_eq!(gga.satellites, Some(8));
    assert_eq!(gga.hdop, Some(0.9));
    assert_eq!(gga.altitude_m, Some(545.4));
    assert_eq!(gga.geoid_separation_m, Some(46.9));
    Ok(())
}

#[test]
fn a_reference_rmc_decodes_every_field() -> TestResult {
    let Sentence::Rmc(rmc) = Sentence::parse(REFERENCE_RMC)? else {
        return Err("an RMC sentence".into());
    };
    assert_eq!(rmc.status, RmcStatus::Active);
    assert_eq!(rmc.speed_knots, Some(22.4));
    assert_eq!(rmc.course_degrees, Some(84.4));
    assert_eq!(
        rmc.date,
        Some(UtcDate {
            year: 1994,
            month: 3,
            day: 23
        })
    );
    assert_eq!(rmc.mode, None, "the NMEA 2.0 form has no mode field");
    Ok(())
}

#[test]
fn receiver_talkers_south_west_and_fractional_seconds_decode() -> TestResult {
    let Sentence::Rmc(rmc) =
        Sentence::parse("$GNRMC,001220.00,A,3540.1234,N,08154.5678,W,0.021,,181026,,,A*70")?
    else {
        return Err("an RMC sentence".into());
    };
    assert_eq!(rmc.talker, "GN");
    let position = rmc.position.ok_or("a position")?;
    assert!(
        close(position.longitude, -(81.0 + 54.5678 / 60.0)),
        "{position:?}"
    );
    assert_eq!(rmc.course_degrees, None);
    assert_eq!(
        rmc.date,
        Some(UtcDate {
            year: 2026,
            month: 10,
            day: 18
        })
    );
    assert_eq!(rmc.mode, Some('A'));
    let Sentence::Gga(gga) = Sentence::parse(
        "$GNGGA,001220.00,3540.1234,N,08154.5678,W,1,07,1.23,350.2,M,-31.4,M,,*7B",
    )?
    else {
        return Err("a GGA sentence".into());
    };
    assert_eq!(gga.geoid_separation_m, Some(-31.4));
    assert_eq!(gga.time.map(|time| time.millisecond), Some(0));
    Ok(())
}

#[test]
fn sentences_without_a_fix_decode_their_empty_fields_as_absent() -> TestResult {
    let Sentence::Rmc(rmc) = Sentence::parse("$GNRMC,,V,,,,,,,,,,N*4D")? else {
        return Err("an RMC sentence".into());
    };
    assert_eq!(
        (rmc.time, rmc.status, rmc.position, rmc.date, rmc.mode),
        (None, RmcStatus::Void, None, None, Some('N'))
    );
    let Sentence::Gga(gga) = Sentence::parse("$GNGGA,,,,,,0,00,99.99,,,,,,*56")? else {
        return Err("a GGA sentence".into());
    };
    assert_eq!(
        (gga.quality, gga.satellites, gga.hdop, gga.altitude_m),
        (FixQuality::Invalid, Some(0), Some(99.99), None)
    );
    Ok(())
}

/// `body`, the text between `$` and `*`, framed with its checksum.
fn framed(body: &str) -> String {
    let checksum = body.bytes().fold(0, |sum, byte| sum ^ byte);
    format!("${body}*{checksum:02X}")
}

/// The reference RMC sentence with `ddmmyy` as its date.
fn rmc_dated(ddmmyy: &str) -> String {
    framed(&format!(
        "GPRMC,123519,A,4807.038,N,01131.000,E,022.4,084.4,{ddmmyy},003.1,W"
    ))
}

#[test]
fn a_date_names_a_day_of_its_month() -> TestResult {
    for (ddmmyy, year, month, day) in [
        ("290224", 2024, 2, 29),
        ("290200", 2000, 2, 29),
        ("300426", 2026, 4, 30),
        ("311226", 2026, 12, 31),
    ] {
        let Sentence::Rmc(rmc) = Sentence::parse(&rmc_dated(ddmmyy))? else {
            return Err("an RMC sentence".into());
        };
        assert_eq!(rmc.date, Some(UtcDate { year, month, day }), "{ddmmyy}");
    }
    for ddmmyy in ["310224", "290223", "290299", "310426", "300226"] {
        let result = Sentence::parse(&rmc_dated(ddmmyy));
        assert!(
            matches!(result, Err(NmeaError::Field { .. })),
            "{ddmmyy}: {result:?}"
        );
    }
    Ok(())
}

#[test]
fn other_sentences_keep_their_fields() -> TestResult {
    let Sentence::Other(raw) = Sentence::parse("$GPGSV,1,1,01,05,45,180,32*44")? else {
        return Err("an unmodeled sentence".into());
    };
    assert_eq!((raw.talker(), raw.formatter()), (Some("GP"), "GSV"));
    assert_eq!(raw.fields(), ["1", "1", "01", "05", "45", "180", "32"]);
    let raw = RawSentence::parse("$GPVTG,,T,,M,0.021,N,0.039,K,A*2A")?;
    assert_eq!(raw.fields().len(), 9);
    Ok(())
}

#[test]
fn framing_checksum_and_field_errors_are_typed() {
    for (line, expected) in [
        ("GPGGA,123519*47", "framing"),
        ("$GPGGA,123519", "framing"),
        ("$GPGGA,123519*4", "framing"),
        ("$GPGGA,123519*G7", "framing"),
        ("$GPGGA,12\u{e9}519*47", "framing"),
        ("$GP,1*0A", "address"),
        (
            "$GPGGA,123519,4807.038,N,01131.000,E,1,08,0.9,545.4,M,46.9,M,,*48",
            "checksum",
        ),
    ] {
        let result = RawSentence::parse(line);
        let matched = match &result {
            Err(NmeaError::Framing { .. }) => expected == "framing",
            Err(NmeaError::Address { .. }) => expected == "address",
            Err(NmeaError::Checksum { .. }) => expected == "checksum",
            _ => false,
        };
        assert!(matched, "{line:?}: {result:?}");
    }
    for line in [
        "$GPGGA,123519,4867.038,N,01131.000,E,1,08,0.9,545.4,M,46.9,M,,*41",
        "$GPGGA,253519,4807.038,N,01131.000,E,1,08,0.9,545.4,M,46.9,M,,*43",
        "$GPGGA,123519,4807.038,X,01131.000,E,1,08,0.9,545.4,M,46.9,M,,*51",
        "$GPGGA,123519,4807.038,N,01131.000,E,1,08,inf,545.4,M,46.9,M,,*01",
        "$GPRMC,123519,A,4807.038,N,01131.000,E,022.4,084.4,320394,003.1,W*6A",
        "$GPRMC,123519,A,4807.038*35",
    ] {
        let result = Sentence::parse(line);
        assert!(
            matches!(
                result,
                Err(NmeaError::Field { .. } | NmeaError::FieldCount { .. })
            ),
            "{line:?}: {result:?}"
        );
    }
}

#[test]
fn the_line_reader_yields_sentences_and_skips_cat_replies() -> TestResult {
    let mut reader = LineReader::new();
    let mut lines = reader.push(format!("ID TM-D750\r{REFERENCE_GGA}\r\n$GNRMC,,V,").as_bytes());
    lines.extend(reader.push(b",,,,,,,,,N*4D\r\nFV 1.02\r"));
    let sentences = lines.into_iter().collect::<Result<Vec<_>, _>>()?;
    assert_eq!(sentences.len(), 2);
    assert!(matches!(sentences.first(), Some(Sentence::Gga(_))));
    assert!(matches!(sentences.get(1), Some(Sentence::Rmc(_))));

    let overflow = format!(
        "${}\r\n{REFERENCE_RMC}\r\n",
        "A".repeat(MAX_SENTENCE_LENGTH)
    );
    let results = reader.push(overflow.as_bytes());
    assert!(matches!(
        results.first(),
        Some(Err(NmeaError::TooLong { .. }))
    ));
    assert!(
        matches!(results.get(1), Some(Ok(Sentence::Rmc(_)))),
        "the reader resynchronizes at the next terminator: {results:?}"
    );
    Ok(())
}
