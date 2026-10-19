//! Repeater-list editing planned from backup reports: option checks, the one
//! page an edit writes, its journal description and the endpoint gate.

use std::fs::File;
use std::path::Path;

use serde_json::Value;

use super::*;

type TestError = Box<dyn std::error::Error + Send + Sync>;
type TestResult = Result<(), TestError>;

/// Akihabara, JP1YLA A: the first record of a captured list, linked to slot 1.
const AKIHABARA: &str = "416b6968616261726100000000000000546f6b796f00000000000000000000004a5031594c4120414a5031594c4120478032e319404b4c002329e4258b2e6009f3015c02ffff0000000000eeffff0100";
/// A slot that holds no repeater.
const UNUSED: &str = "000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000ffffffffffffffff0000000000000000fbffffffffffffffffff00f0ffffffff";

fn bytes(hex: &str) -> Result<Vec<u8>, TestError> {
    (0..hex.len())
        .step_by(2)
        .map(|index| {
            let digits = hex.get(index..index + 2).ok_or("an even digit count")?;
            Ok(u8::from_str_radix(digits, 16)?)
        })
        .collect()
}

fn page_mut(document: &mut Value, address: u32) -> Result<&mut Vec<Value>, TestError> {
    document
        .get_mut("backup")
        .and_then(|backup| backup.get_mut("segments"))
        .and_then(Value::as_array_mut)
        .and_then(|segments| {
            segments
                .iter_mut()
                .find(|segment| segment.get("address") == Some(&Value::from(address)))
        })
        .and_then(|segment| segment.get_mut("data"))
        .and_then(Value::as_array_mut)
        .ok_or_else(|| format!("fixture lacks page {address}").into())
}

/// A backup whose repeater list holds Akihabara alone in slot 0 and whose
/// hotspot list is empty.
fn backup(directory: &Path) -> Result<PathBuf, TestError> {
    let mut document = crate::mcp::snapshot::tests::fixture();
    let unused = bytes(UNUSED)?;
    for kind in [RepeaterListKind::Repeater, RepeaterListKind::Hotspot] {
        for index in (0..kind.capacity()).step_by(3) {
            let page = page_mut(&mut document, RepeaterSlot::new(kind, index)?.address())?;
            for (offset, byte) in page.iter_mut().enumerate() {
                *byte = Value::from(unused.get(offset % 80).copied().unwrap_or(0xFF));
            }
            for byte in page.iter_mut().skip(240) {
                *byte = Value::from(0xFF);
            }
        }
    }
    let first = page_mut(
        &mut document,
        RepeaterSlot::new(RepeaterListKind::Repeater, 0)?.address(),
    )?;
    let mut record = bytes(AKIHABARA)?;
    record
        .get_mut(78..80)
        .ok_or("next link")?
        .copy_from_slice(&[0xFF, 0xFF]);
    for (byte, value) in first.iter_mut().zip(record) {
        *byte = Value::from(value);
    }
    let ends = page_mut(
        &mut document,
        RepeaterListKind::Repeater.ends_address() - 240,
    )?;
    for (byte, value) in ends.iter_mut().skip(240).zip([0, 0, 0, 0]) {
        *byte = Value::from(value);
    }
    let path = directory.join("report.json");
    serde_json::to_writer(File::create_new(&path)?, &document)?;
    Ok(path)
}

fn edit(backup: PathBuf) -> EditRequest {
    EditRequest {
        backup,
        hotspots: false,
        slot: 0,
        frequency: None,
        offset: None,
        shift: None,
        name: None,
        sub_name: None,
        callsign: None,
        gateway: None,
    }
}

#[test]
fn an_edit_plans_one_page_changing_only_its_fields() -> TestResult {
    let directory = tempfile::tempdir()?;
    let snapshot = Snapshot::load(&backup(directory.path())?)?;
    let mut request = edit(directory.path().join("report.json"));
    request.frequency = Some("434.330".to_owned());
    request.name = Some("Akiba".to_owned());
    let prepared = request.prepare(&snapshot)?;
    assert_eq!(prepared.replacement.page().address().as_u32(), 0x2_A000);
    let changed: Vec<usize> = prepared
        .replacement
        .expected()
        .iter()
        .zip(prepared.replacement.replacement())
        .enumerate()
        .filter(|(_, (old, new))| old != new)
        .map(|(offset, _)| offset)
        .collect();
    assert_eq!(changed, [3, 5, 6, 7, 8, 48, 49]);
    assert_eq!(prepared.after.name().to_string(), "Akiba");
    assert_eq!(prepared.before.frequency().hertz(), 434_320_000);

    let journal = description(&prepared);
    let repeater = journal.get("repeater").ok_or("repeater description")?;
    assert_eq!(repeater.get("slot"), Some(&Value::from(0)));
    assert_eq!(repeater.get("list"), Some(&Value::from("repeater")));
    assert_eq!(
        repeater.pointer("/after/frequency_hz"),
        Some(&Value::from(434_330_000))
    );
    assert_eq!(
        repeater.pointer("/before/name"),
        Some(&Value::from("Akihabara"))
    );
    Ok(())
}

#[test]
fn an_edit_needs_a_field_a_stored_record_and_storable_values() -> TestResult {
    let directory = tempfile::tempdir()?;
    let snapshot = Snapshot::load(&backup(directory.path())?)?;
    let report = directory.path().join("report.json");
    let refused = |request: &EditRequest, expected: &str| -> TestResult {
        let result = request.prepare(&snapshot);
        assert!(
            matches!(&result, Err(error) if error.to_string().contains(expected)),
            "{expected}: {:?}",
            result.err()
        );
        Ok(())
    };
    refused(&edit(report.clone()), "give at least one")?;
    let mut empty_slot = edit(report.clone());
    empty_slot.slot = 1;
    empty_slot.frequency = Some("434.330".to_owned());
    refused(&empty_slot, "holds no record")?;
    let mut hotspot = edit(report.clone());
    hotspot.hotspots = true;
    hotspot.frequency = Some("434.330".to_owned());
    refused(&hotspot, "holds no record")?;
    let mut outside = edit(report.clone());
    outside.slot = 1500;
    outside.frequency = Some("434.330".to_owned());
    refused(&outside, "outside")?;
    let mut name = edit(report.clone());
    name.name = Some("\u{6771}\u{4eac}".to_owned());
    refused(&name, "no stored form")?;
    let mut frequency = edit(report);
    frequency.frequency = Some("434.33 MHz".to_owned());
    refused(&frequency, "megahertz")?;
    Ok(())
}

#[test]
fn set_requires_apply_and_an_explicit_port() -> TestResult {
    let arguments = |extra: &[&str]| {
        [
            "mcp",
            "repeater",
            "set",
            "--backup",
            "report.json",
            "--slot",
            "0",
            "--frequency",
            "434.330",
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
    let list = crate::mcp::parse(
        &["mcp", "repeater", "list", "--backup", "report.json"].map(str::to_owned),
    )?;
    list.validate_endpoint_selection(false)?;
    Ok(())
}
