//! Voice code listing and edits planned from backup reports: the lines, the
//! page an edit writes, its journal description, the refusals and the
//! endpoint gate.

use std::fs::File;
use std::path::Path;

use kenwood_tmd750::memory::repeater_list::{
    REPEATER_COUNTRY_NAMES_ADDRESS, REPEATER_GROUP_NAMES_ADDRESS,
};

use super::*;

type TestError = Box<dyn std::error::Error + Send + Sync>;
type TestResult = Result<(), TestError>;

/// A backup whose name and voice tables are empty except Japan (`JPN`),
/// India (no code) and group 0, `JA1 : Kanto` (`JA1`).
fn backup(directory: &Path) -> Result<PathBuf, TestError> {
    let entries: [(u32, &[u8]); 5] = [
        (REPEATER_COUNTRY_NAMES_ADDRESS, b"Japan"),
        (REPEATER_COUNTRY_NAMES_ADDRESS + 16, b"India"),
        (REPEATER_GROUP_NAMES_ADDRESS, b"JA1 : Kanto"),
        (REPEATER_COUNTRY_VOICE_ADDRESS, b"JPN"),
        (REPEATER_GROUP_VOICE_ADDRESS, b"JA1"),
    ];
    let stored = |address: u32| {
        for (start, text) in entries {
            if let Some(byte) = address
                .checked_sub(start)
                .and_then(|offset| text.get(usize::try_from(offset).ok()?))
            {
                return Some(*byte);
            }
        }
        ((0x3600..0x3F00).contains(&address) || (0x4_A000..0x4_C000).contains(&address))
            .then_some(0)
    };
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
        for (offset, byte) in (u32::try_from(address)?..).zip(data.iter_mut()) {
            if let Some(value) = stored(offset) {
                *byte = Value::from(value);
            }
        }
    }
    let path = directory.join("report.json");
    serde_json::to_writer(File::create_new(&path)?, &document)?;
    Ok(path)
}

fn edit(backup: PathBuf, country: Option<u8>, group: Option<u16>, code: Option<&str>) -> VoiceEdit {
    VoiceEdit {
        backup,
        country,
        group,
        code: code.map(str::to_owned),
        clear: code.is_none(),
    }
}

#[test]
fn the_listing_shows_each_named_or_coded_entry() -> TestResult {
    let directory = tempfile::tempdir()?;
    let snapshot = Snapshot::load(&backup(directory.path())?)?;
    let (names, _image) = captured(&snapshot)?;
    assert_eq!(
        name_lines(&names),
        [
            "Countries:",
            "    0 Japan: voice code JPN",
            "    1 India: no voice code",
            "Groups:",
            "    0 JA1 : Kanto: voice code JA1",
        ]
    );
    Ok(())
}

#[test]
fn an_edit_writes_the_page_of_its_code() -> TestResult {
    let directory = tempfile::tempdir()?;
    let report = backup(directory.path())?;
    let snapshot = Snapshot::load(&report)?;
    let country = edit(report.clone(), Some(1), None, Some("IND")).prepare(&snapshot)?;
    let pages: Vec<u32> = country
        .replacements
        .iter()
        .map(|replacement| replacement.page().address().as_u32())
        .collect();
    assert_eq!(pages, [0x3600]);
    assert_eq!(
        description(&country).get("voice"),
        Some(&json!({
            "table": "country",
            "index": 1,
            "name": "India",
            "before": "",
            "after": "IND",
        }))
    );
    let group = edit(report, None, Some(0), None).prepare(&snapshot)?;
    assert_eq!(group.before.as_str(), "JA1");
    assert!(group.after.is_empty());
    let pages: Vec<u32> = group
        .replacements
        .iter()
        .map(|replacement| replacement.page().address().as_u32())
        .collect();
    assert_eq!(pages, [0x3A00]);
    Ok(())
}

#[test]
fn an_edit_needs_one_entry_and_a_valid_code() -> TestResult {
    let directory = tempfile::tempdir()?;
    let report = backup(directory.path())?;
    let snapshot = Snapshot::load(&report)?;
    let mut both = edit(report.clone(), Some(1), None, Some("IND"));
    both.clear = true;
    let cases = [
        (
            edit(report.clone(), None, None, Some("IND")),
            "give --country",
        ),
        (both, "give --code"),
        (
            edit(report.clone(), Some(1), None, Some("ind")),
            "uppercase letters and digits",
        ),
        (
            edit(report, Some(150), None, Some("IND")),
            "country index 150",
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
fn set_requires_apply_and_an_explicit_port() -> TestResult {
    let arguments = |extra: &[&str]| {
        [
            "mcp",
            "repeater",
            "voice",
            "set",
            "--backup",
            "report.json",
            "--country",
            "1",
            "--code",
            "IND",
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
        matches!(&refused, Err(error) if error.to_string().contains("voice set requires an explicit --port")),
        "{refused:?}"
    );
    request.validate_endpoint_selection(true)?;
    Ok(())
}
