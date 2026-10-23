//! Channel names planned from backup reports: argument checks, the pages a
//! set of names changes, the journal description and the endpoint gate.

use std::fs::File;
use std::path::Path;

use serde_json::Value;

use super::*;

type TestError = Box<dyn std::error::Error + Send + Sync>;
type TestResult = Result<(), TestError>;

/// A backup whose name table holds sixteen NUL bytes for every channel.
fn backup(directory: &Path) -> Result<PathBuf, TestError> {
    let mut document = crate::mcp::snapshot::tests::fixture();
    let table = name_table()?;
    let segments = document
        .get_mut("backup")
        .and_then(|backup| backup.get_mut("segments"))
        .and_then(Value::as_array_mut)
        .ok_or("fixture segments")?;
    for segment in segments {
        let address = segment
            .get("address")
            .and_then(Value::as_u64)
            .ok_or("segment address")?;
        let data = segment
            .get_mut("data")
            .and_then(Value::as_array_mut)
            .ok_or("segment data")?;
        for (offset, byte) in (address..).zip(data.iter_mut()) {
            if (u64::from(table.start())..u64::from(table.end())).contains(&offset) {
                *byte = Value::from(0);
            }
        }
    }
    let path = directory.join("report.json");
    serde_json::to_writer(File::create_new(&path)?, &document)?;
    Ok(path)
}

fn selection(backup: PathBuf, set: &[&str], clear: &[&str]) -> NamesSelection {
    NamesSelection {
        backup,
        set: set.iter().map(|word| (*word).to_owned()).collect(),
        clear: clear.iter().map(|word| (*word).to_owned()).collect(),
    }
}

#[test]
fn names_plan_each_changed_page_once() -> TestResult {
    let directory = tempfile::tempdir()?;
    let report = backup(directory.path())?;
    let snapshot = Snapshot::load(&report)?;
    let prepared = selection(
        report,
        &["995", "Batch 995", "996", "Batch 996", "l05", "Lower 5"],
        &["000"],
    )
    .prepare(&snapshot)?;
    let pages: Vec<u32> = prepared
        .replacements
        .iter()
        .map(|replacement| replacement.page().address().as_u32())
        .collect();
    assert_eq!(
        pages,
        [0x1_3E00, 0x1_3F00],
        "995 and 996 share a page; channel 0 is already unnamed"
    );
    assert_eq!(
        prepared.entries,
        [
            ("995".to_owned(), Some("Batch 995".to_owned())),
            ("996".to_owned(), Some("Batch 996".to_owned())),
            ("L05".to_owned(), Some("Lower 5".to_owned())),
            ("000".to_owned(), None),
        ]
    );
    let journal = description(&prepared);
    assert_eq!(
        journal
            .get("channel_names")
            .and_then(|names| names.pointer("/3/name")),
        Some(&Value::Null)
    );
    Ok(())
}

#[test]
fn a_plan_needs_channels_valid_names_and_no_repeats() -> TestResult {
    let directory = tempfile::tempdir()?;
    let report = backup(directory.path())?;
    let snapshot = Snapshot::load(&report)?;
    for (set, clear, expected) in [
        (&[][..], &[][..], "give at least one"),
        (&["1000", "Name"][..], &[][..], "1000"),
        (&["995", "Seventeen bytes!!"][..], &[][..], "channel name"),
        (&["995", "One"][..], &["995"][..], "more than once"),
    ] {
        let result = selection(report.clone(), set, clear).prepare(&snapshot);
        assert!(
            matches!(&result, Err(error) if error.to_string().contains(expected)),
            "{expected}: {:?}",
            result.err().map(|error| error.to_string())
        );
    }
    let unchanged = selection(report, &[], &["995"]).prepare(&snapshot)?;
    assert_eq!(unchanged.replacements, Vec::<PageReplacement>::new());
    Ok(())
}

#[test]
fn apply_requires_apply_and_an_explicit_port() -> TestResult {
    let arguments = |extra: &[&str]| {
        [
            "mcp",
            "channel-names",
            "apply",
            "--backup",
            "report.json",
            "--set",
            "995",
            "Batch 995",
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
