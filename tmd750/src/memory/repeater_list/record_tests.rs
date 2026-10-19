use std::ops::Range;

use super::*;
use crate::memory::repeater_list::fixtures::{
    AKIHABARA, BUENOS_AIRES, OZONDJISSE, REYKJAVIK, UNUSED, bytes,
};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn record(hex: &str) -> Result<RepeaterRecord, Box<dyn std::error::Error>> {
    RepeaterRecord::from_bytes(bytes(hex)?)?.ok_or_else(|| "a populated record".into())
}

fn set(raw: &mut [u8; REPEATER_RECORD_SIZE], offset: usize, value: &[u8]) -> TestResult {
    raw.get_mut(offset..offset + value.len())
        .ok_or("inside the record")?
        .copy_from_slice(value);
    Ok(())
}

/// Assert that `after` differs from `before` and only inside `field`.
fn assert_confined(before: &RepeaterRecord, after: &RepeaterRecord, field: Range<usize>) {
    let changed: Vec<usize> = before
        .to_bytes()
        .iter()
        .zip(after.to_bytes())
        .enumerate()
        .filter(|(_, (old, new))| **old != *new)
        .map(|(offset, _)| offset)
        .collect();
    assert!(!changed.is_empty(), "nothing changed");
    assert!(
        changed.iter().all(|offset| field.contains(offset)),
        "{changed:?} outside {field:?}"
    );
}

#[test]
fn every_field_of_a_stored_repeater_decodes() -> TestResult {
    let akihabara = record(AKIHABARA)?;
    assert_eq!(akihabara.name().to_string(), "Akihabara");
    assert_eq!(akihabara.sub_name().to_string(), "Tokyo");
    assert_eq!(akihabara.callsign().text(), "JP1YLA A");
    assert_eq!(akihabara.gateway().text(), "JP1YLA G");
    assert_eq!(akihabara.frequency().hertz(), 434_320_000);
    assert_eq!(akihabara.frequency().to_string(), "434.320000 MHz");
    assert_eq!(akihabara.offset().hertz(), 5_000_000);
    assert_eq!(akihabara.shift(), RepeaterShift::Plus);
    let position = akihabara.position();
    assert_eq!(position.accuracy, PositionAccuracy::Approximate);
    assert_eq!(position.latitude.hemisphere(), LatitudeHemisphere::North);
    assert_eq!(
        position.latitude.coordinate(),
        Coordinate::new(35, 41, 9700)?
    );
    assert_eq!(position.longitude.hemisphere(), LongitudeHemisphere::East);
    assert_eq!(
        position.longitude.coordinate(),
        Coordinate::new(139, 46, 2400)?
    );
    assert_eq!(akihabara.utc_offset().minutes(), 540);
    assert_eq!(akihabara.utc_offset().to_string(), "+09:00");
    assert_eq!(
        (akihabara.region(), akihabara.country(), akihabara.group()),
        (
            Some(RegionIndex::new(0)?),
            Some(CountryIndex::new(0)?),
            Some(GroupIndex::new(0)?)
        )
    );
    assert_eq!((akihabara.previous(), akihabara.next()), (None, Some(1)));

    let buenos_aires = record(BUENOS_AIRES)?;
    assert_eq!(buenos_aires.frequency().hertz(), 434_950_000);
    assert_eq!(buenos_aires.shift(), RepeaterShift::Plus);
    assert_eq!(buenos_aires.utc_offset().to_string(), "-03:00");
    assert_eq!(
        (
            buenos_aires.region(),
            buenos_aires.country(),
            buenos_aires.group()
        ),
        (
            Some(RegionIndex::new(4)?),
            Some(CountryIndex::new(60)?),
            Some(GroupIndex::new(199)?)
        )
    );
    assert_eq!(
        (buenos_aires.previous(), buenos_aires.next()),
        (Some(1409), Some(1411))
    );

    let ozondjisse = record(OZONDJISSE)?;
    assert_eq!(ozondjisse.name().to_string(), "Farm Ozondjisse");
    assert_eq!(ozondjisse.callsign().text(), "V53DV  C");
    assert_eq!(ozondjisse.frequency().hertz(), 145_600_000);
    assert_eq!(ozondjisse.offset().hertz(), 600_000);
    assert_eq!(ozondjisse.shift(), RepeaterShift::Minus);
    Ok(())
}

#[test]
fn the_hemisphere_bits_name_south_and_west() -> TestResult {
    for (hex, latitude, longitude) in [
        (
            AKIHABARA,
            LatitudeHemisphere::North,
            LongitudeHemisphere::East,
        ),
        (
            REYKJAVIK,
            LatitudeHemisphere::North,
            LongitudeHemisphere::West,
        ),
        (
            OZONDJISSE,
            LatitudeHemisphere::South,
            LongitudeHemisphere::East,
        ),
        (
            BUENOS_AIRES,
            LatitudeHemisphere::South,
            LongitudeHemisphere::West,
        ),
    ] {
        let position = record(hex)?.position();
        assert_eq!(
            (
                position.latitude.hemisphere(),
                position.longitude.hemisphere()
            ),
            (latitude, longitude)
        );
    }
    let position = record(BUENOS_AIRES)?.position();
    assert!((position.latitude.degrees() + 34.603_666).abs() < 1e-5);
    assert!((position.longitude.degrees() + 58.381).abs() < 1e-5);
    Ok(())
}

#[test]
fn records_re_encode_byte_for_byte_and_the_empty_flag_decides_population() -> TestResult {
    for hex in [AKIHABARA, REYKJAVIK, OZONDJISSE, BUENOS_AIRES] {
        assert_eq!(record(hex)?.to_bytes(), bytes(hex)?);
    }
    assert_eq!(RepeaterRecord::from_bytes(bytes(UNUSED)?)?, None);
    let mut flagged = bytes(AKIHABARA)?;
    set(&mut flagged, 75, &[0xFE])?;
    assert_eq!(
        RepeaterRecord::from_bytes(flagged)?,
        None,
        "bit 4 of byte 75 marks the slot empty whatever else it holds"
    );
    Ok(())
}

#[test]
fn a_populated_record_with_a_field_outside_its_domain_is_refused() -> TestResult {
    for (offset, value) in [
        (33, &[0x07][..]),
        (12, b"X"),
        (48, &[0xFF; 4]),
        (56, &[91]),
        (57, &[60]),
        (58, &[0x10, 0x27]),
        (60, &[181]),
        (66, &[113]),
        (70, &[6]),
        (71, &[150]),
        (72, &[0x2C, 0x01]),
    ] {
        let mut raw = bytes(AKIHABARA)?;
        set(&mut raw, offset, value)?;
        let result = RepeaterRecord::from_bytes(raw);
        assert!(result.is_err(), "byte {offset} = {value:02X?}: {result:?}");
    }
    let mut pole = bytes(AKIHABARA)?;
    set(&mut pole, 56, &[90, 0, 0, 0])?;
    assert!(
        RepeaterRecord::from_bytes(pole)?.is_some(),
        "exactly 90 degrees"
    );
    set(&mut pole, 57, &[1])?;
    assert!(RepeaterRecord::from_bytes(pole).is_err(), "beyond the pole");
    Ok(())
}

#[test]
fn all_ones_indices_name_no_table_entry() -> TestResult {
    let mut raw = bytes(AKIHABARA)?;
    set(&mut raw, 70, &[0xFF; 4])?;
    let record = RepeaterRecord::from_bytes(raw)?.ok_or("populated")?;
    assert_eq!(
        (record.region(), record.country(), record.group()),
        (None, None, None)
    );
    Ok(())
}

#[test]
fn each_setter_changes_only_its_own_bytes() -> TestResult {
    let original = record(AKIHABARA)?;

    let mut edited = original.clone();
    edited.set_name(StoredText::new("Akiba")?);
    assert_confined(&original, &edited, 0..16);
    assert_eq!(edited.name().to_string(), "Akiba");

    let mut edited = original.clone();
    edited.set_sub_name(StoredText::new("Chiyoda")?);
    assert_confined(&original, &edited, 16..32);

    let mut edited = original.clone();
    edited.set_callsign(StoredCallsign::new("JP1YLA B")?)?;
    assert_confined(&original, &edited, 32..40);
    assert_eq!(edited.callsign().text(), "JP1YLA B");

    let mut edited = original.clone();
    edited.set_gateway(StoredCallsign::new("")?)?;
    assert_confined(&original, &edited, 40..48);
    assert_eq!(edited.gateway().text(), "");

    let mut edited = original.clone();
    edited.set_frequency(RepeaterFrequency::new(434_330_000)?);
    assert_confined(&original, &edited, 48..52);
    assert_eq!(edited.frequency().hertz(), 434_330_000);

    let mut edited = original.clone();
    edited.set_offset(RepeaterFrequency::new(0)?);
    assert_confined(&original, &edited, 52..56);

    let mut edited = original.clone();
    let position = RepeaterPosition {
        accuracy: PositionAccuracy::Exact,
        latitude: Latitude::new(LatitudeHemisphere::South, Coordinate::new(10, 5, 100)?)?,
        longitude: Longitude::new(LongitudeHemisphere::West, Coordinate::new(20, 6, 200)?)?,
    };
    edited.set_position(position);
    assert_confined(&original, &edited, 56..66);
    assert_eq!(edited.position(), position);
    let unnamed_bits = |record: &RepeaterRecord| record.to_bytes().get(64).map(|byte| byte & !0x0C);
    assert_eq!(unnamed_bits(&edited), unnamed_bits(&original));

    let mut edited = original.clone();
    edited.set_utc_offset(UtcOffset::from_minutes(-570)?);
    assert_confined(&original, &edited, 66..67);
    assert_eq!(edited.utc_offset().to_string(), "-09:30");

    let mut edited = original.clone();
    edited.set_shift(RepeaterShift::Minus);
    assert_confined(&original, &edited, 67..68);

    let mut edited = original.clone();
    edited.set_region(Some(RegionIndex::new(5)?));
    edited.set_country(None);
    edited.set_group(Some(GroupIndex::new(299)?));
    assert_confined(&original, &edited, 70..74);
    assert_eq!(
        (edited.region(), edited.country(), edited.group()),
        (
            Some(RegionIndex::new(5)?),
            None,
            Some(GroupIndex::new(299)?)
        )
    );
    assert_eq!(
        RepeaterRecord::from_bytes(edited.to_bytes())?,
        Some(edited),
        "an edited record decodes to itself"
    );
    Ok(())
}

#[test]
fn a_callsign_outside_the_stored_form_is_refused() -> TestResult {
    let mut edited = record(AKIHABARA)?;
    let result = edited.set_callsign(StoredCallsign::from_bytes([0x07; 8]));
    assert!(
        matches!(result, Err(RepeaterListError::MalformedCallsign { .. })),
        "{result:?}"
    );
    assert_eq!(
        edited,
        record(AKIHABARA)?,
        "a refused value changes nothing"
    );
    Ok(())
}

#[test]
fn field_values_are_validated_at_construction() -> TestResult {
    assert!(RepeaterFrequency::new(u32::MAX).is_err());
    assert_eq!(
        RepeaterFrequency::from_mhz_str("448.380")?.hertz(),
        448_380_000
    );
    assert_eq!(
        RepeaterFrequency::from_mhz_str("1291")?.hertz(),
        1_291_000_000
    );
    for (text, empty_slot) in [
        ("4294.967295", true),
        ("448.38 MHz", false),
        ("5000", false),
        ("-1", false),
        ("448.", false),
        ("448.1234567", false),
    ] {
        let result = RepeaterFrequency::from_mhz_str(text);
        if empty_slot {
            assert!(
                matches!(result, Err(RepeaterListError::FieldOutOfRange { .. })),
                "{text}: {result:?}"
            );
        } else {
            assert!(
                matches!(result, Err(RepeaterListError::InvalidMegahertzText { .. })),
                "{text}: {result:?}"
            );
        }
    }
    assert!(Coordinate::new(0, 60, 0).is_err());
    assert!(Coordinate::new(0, 59, 10_000).is_err());
    assert!(Latitude::new(LatitudeHemisphere::North, Coordinate::new(90, 0, 1)?).is_err());
    assert!(Latitude::new(LatitudeHemisphere::North, Coordinate::new(90, 0, 0)?).is_ok());
    assert!(Longitude::new(LongitudeHemisphere::East, Coordinate::new(180, 0, 1)?).is_err());
    assert!(Longitude::new(LongitudeHemisphere::East, Coordinate::new(180, 0, 0)?).is_ok());
    for minutes in [-840, 0, 345, 840] {
        assert_eq!(UtcOffset::from_minutes(minutes)?.minutes(), minutes);
    }
    assert_eq!(UtcOffset::from_minutes(345)?.to_string(), "+05:45");
    assert_eq!(UtcOffset::from_minutes(0)?.to_string(), "+00:00");
    for minutes in [-855, 7, 845] {
        assert!(UtcOffset::from_minutes(minutes).is_err(), "{minutes}");
    }
    assert!(RegionIndex::new(6).is_err());
    assert!(CountryIndex::new(150).is_err());
    assert!(GroupIndex::new(300).is_err());
    assert_eq!(u8::from(RepeaterShift::from(7)), 7);
    assert_eq!(PositionAccuracy::from(2), PositionAccuracy::Exact);
    Ok(())
}
