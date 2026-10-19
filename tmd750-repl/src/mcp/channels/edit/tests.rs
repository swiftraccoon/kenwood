//! Memory channel stores and clears planned from backup reports: the pages
//! and journal description they produce, the refusals and the endpoint gate.

use std::path::Path;

use super::*;
use crate::mcp::channels::tests::{TestError, stored_patches};
use crate::mcp::snapshot::tests::patched_backup;
use crate::mcp::vfo::tests::vfo_patches;

type TestResult = Result<(), TestError>;

/// A backup holding the channel listing fixture's tables (regular channel 5
/// stored at 146.520 MHz FM) and the bench VFO memory (band A's current
/// record at 145.190 MHz FM).
fn backup(directory: &Path) -> Result<PathBuf, TestError> {
    let mut patches = stored_patches()?;
    patches.extend(vfo_patches());
    patched_backup(directory, &patches)
}

fn selection(backup: PathBuf, store: &[(&str, &str)], clear: &[&str]) -> EditSelection {
    EditSelection {
        backup,
        store: store
            .iter()
            .flat_map(|(channel, source)| [(*channel).to_owned(), (*source).to_owned()])
            .collect(),
        clear: clear.iter().map(|channel| (*channel).to_owned()).collect(),
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
fn a_store_from_a_band_writes_the_priority_flag_and_record() -> TestResult {
    let directory = tempfile::tempdir()?;
    let report = backup(directory.path())?;
    let snapshot = Snapshot::load(&report)?;
    let prepared = selection(report, &[("pri", "a")], &[]).prepare(&snapshot)?;
    assert_eq!(pages(&prepared), [0x3100, 0xF700]);
    let description = description(&prepared);
    let change = description
        .get("memory_channels")
        .and_then(|changes| changes.get(0))
        .ok_or("one change")?;
    assert_eq!(change.get("channel"), Some(&json!("Pri")));
    assert_eq!(change.get("before"), Some(&json!("empty")));
    assert_eq!(change.get("after"), Some(&json!("145.190000 MHz FM")));
    let record = change
        .get("record")
        .and_then(Value::as_str)
        .ok_or("record fields")?;
    assert!(record.starts_with("0145190000,0000600000,"), "{record}");
    assert!(record.ends_with(",0"), "no lockout: {record}");
    Ok(())
}

#[test]
fn a_store_copies_a_stored_channel_and_a_clear_empties_one() -> TestResult {
    let directory = tempfile::tempdir()?;
    let report = backup(directory.path())?;
    let snapshot = Snapshot::load(&report)?;
    let prepared = selection(report, &[("990", "005")], &["005"]).prepare(&snapshot)?;
    assert_eq!(pages(&prepared), [0x2000, 0x2F00, 0x4000, 0xE500]);
    let changes: Vec<[&str; 3]> = prepared
        .changes
        .iter()
        .map(|change| {
            [
                change.before.as_str(),
                change.after.as_str(),
                if change.record.is_some() {
                    "record"
                } else {
                    ""
                },
            ]
        })
        .collect();
    assert_eq!(
        changes,
        [
            ["146.520000 MHz FM", "empty", ""],
            ["empty", "146.520000 MHz FM", "record"]
        ]
    );
    Ok(())
}

#[test]
fn an_edit_needs_valid_channels_and_stored_sources() -> TestResult {
    let directory = tempfile::tempdir()?;
    let report = backup(directory.path())?;
    let snapshot = Snapshot::load(&report)?;
    let cases = [
        (selection(report.clone(), &[], &[]), "give at least one"),
        (
            selection(report.clone(), &[("1000", "a")], &[]),
            "memory address \"1000\"",
        ),
        (
            selection(report.clone(), &[("990", "c")], &[]),
            "memory address \"c\"",
        ),
        (
            selection(report.clone(), &[("990", "006")], &[]),
            "source channel 006 is empty",
        ),
        (
            selection(report, &[("990", "a")], &["990"]),
            "planned more than once",
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
    Ok(())
}

#[test]
fn apply_requires_apply_and_an_explicit_port() -> TestResult {
    let arguments = |extra: &[&str]| {
        [
            "mcp",
            "channels",
            "apply",
            "--backup",
            "report.json",
            "--clear",
            "990",
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
