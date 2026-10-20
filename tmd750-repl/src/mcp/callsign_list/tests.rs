//! Callsign-list edits planned from backup reports: positions, appended
//! entries, the pages they change, the journal description and the endpoint
//! gate.

use std::fs::File;
use std::path::Path;

use serde_json::Value;

use super::*;

type TestError = Box<dyn std::error::Error + Send + Sync>;
type TestResult = Result<(), TestError>;

fn entry(name: &str, callsign: &str, memo: &str) -> Result<CallsignEntry, TestError> {
    Ok(CallsignEntry::new(
        StoredText::new(name)?,
        StoredCallsign::new(callsign)?,
        StoredText::new(memo)?,
    )?)
}

/// A backup whose callsign list holds `entries` compact from slot 0, with
/// every other slot in the empty-slot form.
fn backup(directory: &Path, entries: &[CallsignEntry]) -> Result<PathBuf, TestError> {
    let slot_size = u32::try_from(CALLSIGN_ENTRY_SIZE)?;
    let mut table: Vec<u8> = (0..CALLSIGN_LIST_CAPACITY)
        .flat_map(|_| [vec![0; 56], vec![0xFF; 8]].concat())
        .collect();
    let list = CallsignList::from_entries(entries.to_vec())?;
    for (slot, stored) in &list {
        let start = usize::from(slot.index()) * CALLSIGN_ENTRY_SIZE;
        table
            .get_mut(start..start + CALLSIGN_ENTRY_SIZE)
            .ok_or("a slot of the table")?
            .copy_from_slice(&stored.to_bytes());
    }
    let ends: Vec<u8> = [list.iter().next(), list.iter().next_back()]
        .into_iter()
        .flat_map(|end| end.map_or(0xFFFF, |(slot, _)| slot.index()).to_le_bytes())
        .collect();
    let table_end = CALLSIGN_LIST_ADDRESS + u32::from(CALLSIGN_LIST_CAPACITY) * slot_size;
    let mut document = crate::mcp::snapshot::tests::fixture();
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
            let offset = u32::try_from(offset)?;
            if (CALLSIGN_LIST_ADDRESS..table_end).contains(&offset) {
                let index = usize::try_from(offset - CALLSIGN_LIST_ADDRESS)?;
                *byte = Value::from(*table.get(index).ok_or("a table byte")?);
            } else if (CALLSIGN_LIST_ENDS_ADDRESS..CALLSIGN_LIST_ENDS_ADDRESS + 4).contains(&offset)
            {
                let index = usize::try_from(offset - CALLSIGN_LIST_ENDS_ADDRESS)?;
                *byte = Value::from(*ends.get(index).ok_or("an ends byte")?);
            }
        }
    }
    let path = directory.join("report.json");
    serde_json::to_writer(File::create_new(&path)?, &document)?;
    Ok(path)
}

fn edit(backup: PathBuf, remove: &[usize], add: &[&str]) -> EditRequest {
    EditRequest {
        backup,
        remove: remove.to_vec(),
        add: add.iter().map(|word| (*word).to_owned()).collect(),
    }
}

#[test]
fn an_edit_removes_by_position_appends_and_stores_compact() -> TestResult {
    let directory = tempfile::tempdir()?;
    let stored = [
        entry("Alpha", "W1AW", "first")?,
        entry("Bravo", "K1ABC", "")?,
        entry("Charlie", "N0CALL", "third")?,
    ];
    let report = backup(directory.path(), &stored)?;
    let snapshot = Snapshot::load(&report)?;
    let prepared = edit(
        report,
        &[2],
        &["Delta", "DL1ABC", "Jürgen", "REF001 C", "REF001CL", ""],
    )
    .prepare(&snapshot)?;
    let names: Vec<String> = prepared
        .after
        .iter()
        .map(|(_, entry)| entry.name().to_string())
        .collect();
    assert_eq!(names, ["Alpha", "Charlie", "Delta", "REF001 C"]);
    let slots: Vec<u16> = prepared
        .after
        .iter()
        .map(|(slot, _)| slot.index())
        .collect();
    assert_eq!(slots, [0, 1, 2, 3], "the edited list is compact");
    let pages: Vec<u32> = prepared
        .replacements
        .iter()
        .map(|replacement| replacement.page().address().as_u32())
        .collect();
    assert_eq!(pages, [CALLSIGN_LIST_ADDRESS, 0x4_9300]);
    let journal = description(&prepared);
    assert_eq!(
        journal
            .get("callsign_list")
            .and_then(|list| list.pointer("/before/1/name")),
        Some(&Value::from("Bravo"))
    );
    assert_eq!(
        journal
            .get("callsign_list")
            .and_then(|list| list.pointer("/after/2/memo")),
        Some(&Value::from("Jürgen"))
    );
    Ok(())
}

#[test]
fn an_edit_needs_listed_positions_valid_entries_and_room() -> TestResult {
    let directory = tempfile::tempdir()?;
    let stored = [entry("Alpha", "W1AW", "")?, entry("Bravo", "K1ABC", "")?];
    let report = backup(directory.path(), &stored)?;
    let snapshot = Snapshot::load(&report)?;
    for (remove, add, expected) in [
        (&[][..], &[][..], "give at least one"),
        (&[0][..], &[][..], "position 0"),
        (&[3][..], &[][..], "position 3"),
        (&[1, 1][..], &[][..], "more than once"),
        (&[][..], &["Seventeen bytes!!", "W1AW", ""][..], "16 bytes"),
        (&[][..], &["Name", "TOOLONGCALL", ""][..], "TOOLONGCALL"),
    ] {
        let result = edit(report.clone(), remove, add).prepare(&snapshot);
        assert!(
            matches!(&result, Err(error) if error.to_string().contains(expected)),
            "{expected}: {:?}",
            result.err().map(|error| error.to_string())
        );
    }
    let unchanged = edit(report, &[2], &["Bravo", "K1ABC", ""]).prepare(&snapshot)?;
    assert!(
        unchanged.replacements.is_empty(),
        "re-adding the removed last entry stores the same list"
    );
    Ok(())
}

#[test]
fn apply_requires_apply_and_an_explicit_port() -> TestResult {
    let arguments = |extra: &[&str]| {
        [
            "mcp",
            "callsign-list",
            "apply",
            "--backup",
            "report.json",
            "--add",
            "CQ",
            "CQCQCQ",
            "",
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
