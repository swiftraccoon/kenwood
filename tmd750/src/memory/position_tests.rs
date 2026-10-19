use super::*;
use crate::types::{
    Coordinate, IMAGE_LENGTH, Latitude, LatitudeHemisphere, Longitude, LongitudeHemisphere,
};

type TestResult = Result<(), Box<dyn std::error::Error>>;

/// Slot 0 of a stored position: `HOME`, 2026-10-19 12:34:56 UTC, byte 15
/// with the program's upper nibble and fix field 1, 35°41.9700' N,
/// 139°46.2400' E, altitude 40, symbol `/-`, bytes 30 and 31 `FF`.
const HOME: [u8; POSITION_RECORD_SIZE] = [
    b'H', b'O', b'M', b'E', 0, 0, 0, 0, 0, 26, 10, 19, 12, 34, 56, 0xF1, 35, 41, 0xE4, 0x25, 139,
    46, 0x60, 0x09, 40, 0, 0, 0, b'/', b'-', 0xFF, 0xFF,
];

fn put(image: &mut [u8], offset: usize, bytes: &[u8]) -> TestResult {
    image
        .get_mut(offset..offset + bytes.len())
        .ok_or("offset inside the image")?
        .copy_from_slice(bytes);
    Ok(())
}

/// A full image of `0x42` holding an empty position memory as firmware 1.02
/// stores it, with `records` in their slots and `index` from the index
/// page's start.
fn image(
    records: &[(usize, [u8; POSITION_RECORD_SIZE])],
    index: &[u8],
) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let mut image = vec![0x42; IMAGE_LENGTH];
    for slot in 0..POSITION_MEMORY_ENTRIES {
        put(&mut image, record_address(slot), &EMPTY_RECORD)?;
    }
    put(
        &mut image,
        record_address(POSITION_MEMORY_ENTRIES),
        &[0xFF; 128],
    )?;
    put(&mut image, INDEX, &[0xFF; 256])?;
    for (slot, record) in records {
        put(&mut image, record_address(*slot), record)?;
    }
    put(&mut image, INDEX, index)?;
    Ok(image)
}

fn home() -> Result<StoredPosition, Box<dyn std::error::Error>> {
    Ok(StoredPosition::new(
        StoredText::new("HOME")?,
        PositionTime::new(2026, 10, 19, 12, 34, 56)?,
        Latitude::new(LatitudeHemisphere::North, Coordinate::new(35, 41, 9700)?)?,
        Longitude::new(LongitudeHemisphere::East, Coordinate::new(139, 46, 2400)?)?,
        40,
        PositionSymbol::new(b'/', b'-')?,
    ))
}

#[test]
fn an_empty_memory_lists_no_positions() -> TestResult {
    let memory = PositionMemory::read(&MemoryImage::from_bytes(image(&[], &[])?)?)?;
    assert!(memory.entries().is_empty());
    Ok(())
}

#[test]
fn a_stored_position_decodes_and_reencodes_its_bytes() -> TestResult {
    let memory = PositionMemory::read(&MemoryImage::from_bytes(image(&[(0, HOME)], &[0])?)?)?;
    let [entry] = memory.entries() else {
        return Err("one entry".into());
    };
    assert_eq!(entry.name.to_string(), "HOME");
    assert_eq!(entry.time.to_string(), "2026-10-19 12:34:56 UTC");
    assert_eq!(entry.gps_fix, 1);
    assert!((entry.latitude.degrees() - 35.6995).abs() < 1e-9);
    assert!((entry.longitude.degrees() - 139.770_666_666_666_7).abs() < 1e-9);
    assert_eq!(entry.altitude, 40);
    assert_eq!(entry.symbol.to_string(), "/-");
    assert_eq!(entry.to_bytes(), HOME);
    let mut fix_zero = HOME;
    fix_zero[15] = 0xF0;
    assert_eq!(home()?.to_bytes(), fix_zero);

    let mut south_west = HOME;
    south_west[15] = 0xFC;
    let decoded = StoredPosition::from_bytes(south_west)?;
    assert_eq!(decoded.latitude.hemisphere(), LatitudeHemisphere::South);
    assert_eq!(decoded.longitude.hemisphere(), LongitudeHemisphere::West);
    assert_eq!((decoded.gps_fix, decoded.flag_bits), (0, 0xF));
    assert_eq!(decoded.to_bytes(), south_west);
    Ok(())
}

#[test]
fn the_index_page_orders_the_positions() -> TestResult {
    let mut second = HOME;
    second[0] = b'W';
    let memory = PositionMemory::read(&MemoryImage::from_bytes(image(
        &[(3, HOME), (7, second)],
        &[7, 0xFF, 3],
    )?)?)?;
    let names: Vec<String> = memory
        .entries()
        .iter()
        .map(|entry| entry.name.to_string())
        .collect();
    assert_eq!(names, ["WOME", "HOME"]);
    Ok(())
}

#[test]
fn a_write_stores_the_positions_compactly_in_order() -> TestResult {
    let mut second = HOME;
    second[0] = b'W';
    let before = image(&[(3, HOME), (7, second)], &[7, 3])?;
    let current = MemoryImage::from_bytes(before.clone())?;
    let mut memory = PositionMemory::read(&current)?;
    memory.push(home()?)?;
    let mut after = before.clone();
    for replacement in memory.page_replacements(&current)? {
        let start = replacement.page().address().as_usize();
        assert_eq!(
            before.get(start..start + replacement.expected().len()),
            Some(replacement.expected())
        );
        put(&mut after, start, replacement.replacement())?;
    }
    let mut fresh_home = HOME;
    fresh_home[15] = 0xF0;
    let expected = image(&[(0, second), (1, HOME), (2, fresh_home)], &[0, 1, 2])?;
    assert_eq!(after, expected);
    let reread = PositionMemory::read(&MemoryImage::from_bytes(after)?)?;
    assert_eq!(reread, memory);

    assert!(
        PositionMemory::read(&MemoryImage::from_bytes(expected.clone())?)?
            .page_replacements(&MemoryImage::from_bytes(expected)?)?
            .is_empty()
    );
    Ok(())
}

#[test]
fn the_memory_holds_one_hundred_positions() -> TestResult {
    let mut memory = PositionMemory::from_entries(vec![home()?; POSITION_MEMORY_ENTRIES])?;
    assert_eq!(memory.push(home()?), Err(PositionMemoryError::Full));
    assert!(matches!(
        PositionMemory::from_entries(vec![home()?; POSITION_MEMORY_ENTRIES + 1]),
        Err(PositionMemoryError::Full)
    ));
    assert_eq!(memory.remove(0)?, home()?);
    assert_eq!(
        memory.remove(POSITION_MEMORY_ENTRIES),
        Err(PositionMemoryError::NoEntry {
            index: POSITION_MEMORY_ENTRIES
        })
    );
    Ok(())
}

#[test]
fn malformed_indexes_and_records_are_refused() -> TestResult {
    let read = |records: &[(usize, [u8; POSITION_RECORD_SIZE])], index: &[u8]| {
        PositionMemory::read(&MemoryImage::from_bytes(image(records, index)?)?)
            .map_err(|error| -> Box<dyn std::error::Error> { error.into() })
    };
    let refused = |result: Result<PositionMemory, Box<dyn std::error::Error>>, expected: &str| {
        assert!(
            matches!(&result, Err(error) if error.to_string().contains(expected)),
            "{expected}: {result:?}"
        );
    };
    refused(read(&[], &[100]), "slot 100");
    refused(read(&[(0, HOME)], &[0, 0]), "listed twice");
    refused(read(&[], &[5]), "slot 5 is empty");
    let mut month = HOME;
    month[10] = 13;
    refused(read(&[(0, month)], &[0]), "time");
    let mut minutes = HOME;
    minutes[17] = 60;
    refused(read(&[(0, minutes)], &[0]), "position minutes");
    let mut symbol = HOME;
    symbol[29] = 0x7F;
    refused(read(&[(0, symbol)], &[0]), "symbol");
    assert!(PositionTime::new(2026, 2, 29, 0, 0, 0).is_err());
    let leap_day = PositionTime::new(2028, 2, 29, 0, 0, 0)?;
    assert_eq!(leap_day.to_string(), "2028-02-29 00:00:00 UTC");
    assert!(PositionTime::new(2100, 1, 1, 0, 0, 0).is_err());
    Ok(())
}
