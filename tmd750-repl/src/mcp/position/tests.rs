//! Position memory edits planned from backup reports: the pages they change,
//! decimal-degree parsing, the refusals and the endpoint gate.

use std::collections::BTreeMap;
use std::fs::File;
use std::path::Path;

use kenwood_tmd750::memory::position::POSITION_RECORD_SIZE;

use super::*;

type TestError = Box<dyn std::error::Error + Send + Sync>;
type TestResult = Result<(), TestError>;

/// First byte of the position memory regions.
const START: u32 = 0x4_C000;

fn position(name: &str) -> Result<StoredPosition, TestError> {
    Ok(StoredPosition::new(
        StoredText::new(name)?,
        PositionTime::new(2026, 10, 19, 12, 34, 56)?,
        Latitude::new(LatitudeHemisphere::North, Coordinate::new(35, 41, 9700)?)?,
        Longitude::new(LongitudeHemisphere::East, Coordinate::new(139, 46, 2400)?)?,
        40,
        PositionSymbol::new(b'/', b'-')?,
    ))
}

/// A backup report holding the standard fixture and a position memory
/// listing `records` from slot 0, every other slot empty as firmware 1.02
/// stores it.
fn backup(directory: &Path, records: &[StoredPosition]) -> Result<PathBuf, TestError> {
    let mut bytes = vec![0xFF; 0x1000];
    for slot in 0..POSITION_MEMORY_ENTRIES {
        let record = records
            .get(slot)
            .map_or([0xFF; POSITION_RECORD_SIZE], StoredPosition::to_bytes);
        let target = bytes
            .get_mut(POSITION_RECORD_SIZE * slot..POSITION_RECORD_SIZE * (slot + 1))
            .ok_or("record inside the span")?;
        target.copy_from_slice(&record);
        if records.get(slot).is_none()
            && let Some(name) = target.get_mut(..9)
        {
            name.fill(0);
        }
    }
    for slot in 0..records.len() {
        *bytes.get_mut(0xF00 + slot).ok_or("index inside the span")? = u8::try_from(slot)?;
    }
    let mut segments = Vec::new();
    for page in regions::POSITION_MEMORY
        .into_iter()
        .flat_map(kenwood_tmd750::Region::pages)
    {
        let offset = usize::try_from(page.address().as_u32() - START)?;
        let data = bytes
            .get(offset..offset + page.len())
            .ok_or("page inside the span")?;
        segments.push(json!({
            "address": page.address().as_u32(),
            "length": page.len(),
            "data": data,
        }));
    }
    let mut document = crate::mcp::snapshot::tests::fixture();
    let _previous = document
        .pointer_mut("/backup")
        .and_then(Value::as_object_mut)
        .ok_or("fixture backup")?
        .insert("position_memory".to_owned(), Value::Array(segments));
    let path = directory.join("report.json");
    serde_json::to_writer(File::create_new(&path)?, &document)?;
    Ok(path)
}

fn edit(backup: PathBuf, remove: &[usize], add: &[[&str; 3]]) -> EditRequest {
    EditRequest {
        backup,
        remove: remove.to_vec(),
        add: add
            .iter()
            .flat_map(|fields| fields.map(str::to_owned))
            .collect(),
        altitude: 0,
        symbol: "\\K".to_owned(),
    }
}

fn pages(prepared: &Prepared) -> Vec<u32> {
    prepared
        .replacements
        .iter()
        .map(|replacement| replacement.page().address().as_u32())
        .collect()
}

#[test]
fn an_add_writes_the_first_record_and_the_index() -> TestResult {
    let directory = tempfile::tempdir()?;
    let report = backup(directory.path(), &[])?;
    let snapshot = Snapshot::load(&report)?;
    let prepared = edit(report, &[], &[["HOME", "35.6995", "-139.770667"]]).prepare(&snapshot)?;
    assert_eq!(pages(&prepared), [0x4_C000, 0x4_CF00]);
    let [added] = prepared.after.entries() else {
        return Err("one position".into());
    };
    assert_eq!(added.name.to_string(), "HOME");
    assert_eq!(added.latitude.coordinate(), Coordinate::new(35, 41, 9700)?);
    assert_eq!(added.longitude.hemisphere(), LongitudeHemisphere::West);
    assert_eq!(
        added.longitude.coordinate(),
        Coordinate::new(139, 46, 2400)?
    );
    assert_eq!(
        (added.altitude, added.symbol.to_string()),
        (0, "\\K".to_owned())
    );
    Ok(())
}

#[test]
fn a_removal_moves_the_later_positions_down() -> TestResult {
    let directory = tempfile::tempdir()?;
    let report = backup(directory.path(), &[position("HOME")?, position("WORK")?])?;
    let snapshot = Snapshot::load(&report)?;
    let prepared = edit(report, &[1], &[]).prepare(&snapshot)?;
    assert_eq!(pages(&prepared), [0x4_C000, 0x4_CF00]);
    assert_eq!(prepared.after.entries(), [position("WORK")?]);
    assert_eq!(
        description(&prepared)
            .get("position_memory")
            .and_then(|memory| memory.pointer("/after/0/name")),
        Some(&json!("WORK"))
    );
    Ok(())
}

#[test]
fn an_edit_needs_a_position_memory_backup_and_valid_fields() -> TestResult {
    let directory = tempfile::tempdir()?;
    std::fs::create_dir(directory.path().join("standard"))?;
    let standard = crate::mcp::snapshot::tests::patched_backup(
        &directory.path().join("standard"),
        &BTreeMap::new(),
    )?;
    let without = Snapshot::load(&standard)?;
    let refused = edit(standard, &[1], &[]).prepare(&without);
    assert!(
        matches!(&refused, Err(error) if error.to_string().contains("--position-memory")),
        "{:?}",
        refused.err().map(|error| error.to_string())
    );

    let report = backup(directory.path(), &[position("HOME")?, position("WORK")?])?;
    let snapshot = Snapshot::load(&report)?;
    let cases = [
        (edit(report.clone(), &[], &[]), "give at least one"),
        (
            edit(report.clone(), &[3], &[]),
            "not in the backup's 2 positions",
        ),
        (edit(report.clone(), &[1, 1], &[]), "removed more than once"),
        (
            edit(report.clone(), &[], &[["HOME", "abc", "0"]]),
            "not decimal degrees",
        ),
        (
            edit(report.clone(), &[], &[["HOME", "91", "0"]]),
            "latitude degrees",
        ),
        (
            edit(report.clone(), &[], &[["ABCDEFGHIJ", "0", "0"]]),
            "no stored form",
        ),
    ];
    for (request, expected) in cases {
        let result = request.prepare(&snapshot);
        assert!(
            matches!(&result, Err(error) if error.to_string().contains(expected)),
            "{expected}: {:?}",
            result.err().map(|error| error.to_string())
        );
    }
    let mut symbol = edit(report, &[], &[["HOME", "0", "0"]]);
    symbol.symbol = "/".to_owned();
    let result = symbol.prepare(&snapshot);
    assert!(
        matches!(&result, Err(error) if error.to_string().contains("table byte then a code byte")),
        "{:?}",
        result.err().map(|error| error.to_string())
    );
    Ok(())
}

#[test]
fn a_report_missing_a_position_page_is_refused() -> TestResult {
    let directory = tempfile::tempdir()?;
    let report = backup(directory.path(), &[])?;
    let mut document: Value = serde_json::from_reader(File::open(&report)?)?;
    let _last = document
        .pointer_mut("/backup/position_memory")
        .and_then(Value::as_array_mut)
        .ok_or("position memory segments")?
        .pop();
    let truncated = directory.path().join("truncated.json");
    serde_json::to_writer(File::create_new(&truncated)?, &document)?;
    let result = Snapshot::load(&truncated);
    assert!(
        matches!(&result, Err(error) if error.to_string().contains("position memory pages")),
        "{:?}",
        result.err().map(|error| error.to_string())
    );
    Ok(())
}

#[test]
fn decimal_degrees_round_to_ten_thousandths_of_a_minute() -> TestResult {
    for (text, negative, [degrees, minutes, fraction]) in [
        ("35.6995", false, [35, 41, 9700]),
        ("-139.770667", true, [139, 46, 2400]),
        ("0.000001", false, [0, 0, 1]),
        ("35.99999999", false, [36, 0, 0]),
        ("180", false, [180, 0, 0]),
    ] {
        let (parsed_negative, coordinate) = parse_degrees(text, CoordinateKind::Longitude)?;
        assert_eq!(parsed_negative, negative, "{text}");
        assert_eq!(
            coordinate,
            Coordinate::new(
                u8::try_from(degrees)?,
                u8::try_from(minutes)?,
                u16::try_from(fraction)?
            )?,
            "{text}"
        );
    }
    for text in ["", "-", "1.2.3", "1e5", "1000", "0.0000000001", " 1"] {
        assert!(
            parse_degrees(text, CoordinateKind::Longitude).is_err(),
            "{text:?}"
        );
    }
    Ok(())
}

#[test]
fn coordinate_domain_is_checked_before_quantization() -> TestResult {
    for (kind, endpoint, outside) in [
        (CoordinateKind::Latitude, "90", "90.000000001"),
        (CoordinateKind::Latitude, "-90", "-90.000000001"),
        (CoordinateKind::Longitude, "180", "180.000000001"),
        (CoordinateKind::Longitude, "-180", "-180.000000001"),
    ] {
        let _endpoint = parse_degrees(endpoint, kind)?;
        assert!(
            parse_degrees(outside, kind).is_err(),
            "{outside} must fail before rounding"
        );
    }
    Ok(())
}

#[test]
fn apply_requires_apply_and_an_explicit_port() -> TestResult {
    let arguments = |extra: &[&str]| {
        [
            "mcp",
            "position",
            "apply",
            "--backup",
            "report.json",
            "--add",
            "HOME",
            "-33.8688",
            "151.2093",
        ]
        .iter()
        .chain(extra)
        .map(|word| (*word).to_owned())
        .collect::<Vec<_>>()
    };
    assert!(
        crate::mcp::parse(&arguments(&[])).is_err(),
        "--apply is required"
    );
    let request = crate::mcp::parse(&arguments(&["--apply"]))?;
    let refused = request.validate_endpoint_selection(false);
    assert!(
        matches!(&refused, Err(error) if error.to_string().contains("--port")),
        "{refused:?}"
    );
    request.validate_endpoint_selection(true)?;
    Ok(())
}
