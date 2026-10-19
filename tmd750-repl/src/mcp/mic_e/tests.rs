//! Mic-E type code edits planned from backup reports: the entry and page
//! they change, the refusals, the journal description and the endpoint gate.

use std::fs::File;
use std::path::Path;

use super::*;

type TestError = Box<dyn std::error::Error + Send + Sync>;
type TestResult = Result<(), TestError>;

/// A backup whose tables are empty except Kenwood handheld entry 1, which
/// holds `*` named `TM-D750`.
fn backup(directory: &Path) -> Result<PathBuf, TestError> {
    let mut tables = vec![0_u8; 0xF0];
    for (destination, source) in tables.iter_mut().zip(b"*TM-D750") {
        *destination = *source;
    }
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
            if let Some(stored) = offset
                .checked_sub(TABLES.0)
                .and_then(|index| tables.get(usize::try_from(index).ok()?))
            {
                *byte = Value::from(*stored);
            }
        }
    }
    let path = directory.join("report.json");
    serde_json::to_writer(File::create_new(&path)?, &document)?;
    Ok(path)
}

fn edit(
    backup: PathBuf,
    family: Family,
    position: usize,
    entry: Option<(char, &str)>,
) -> EditRequest {
    EditRequest {
        backup,
        family,
        position,
        code: entry.map(|(code, _)| code),
        model: entry.map(|(_, model)| model.to_owned()),
        clear: entry.is_none(),
    }
}

#[test]
fn an_edit_stores_one_entry_and_plans_the_tables_page() -> TestResult {
    let directory = tempfile::tempdir()?;
    let report = backup(directory.path())?;
    let snapshot = Snapshot::load(&report)?;
    let prepared = edit(report, Family::Yaesu, 3, Some(('_', "FT-99"))).prepare(&snapshot)?;
    assert_eq!(prepared.before, None);
    assert_eq!(prepared.after, Some(MicETypeCode::new('_', "FT-99")?));
    let replacement = prepared.replacement.as_ref().ok_or("one changed page")?;
    assert_eq!(replacement.page().address().as_u32(), 0x800);
    assert_eq!(
        description(&prepared).get("mic_e"),
        Some(&json!({
            "family": "yaesu",
            "position": 3,
            "before": null,
            "after": {"code": "_", "model": "FT-99"},
        }))
    );
    Ok(())
}

#[test]
fn an_edit_needs_an_entry_or_clear_and_a_listed_position() -> TestResult {
    let directory = tempfile::tempdir()?;
    let report = backup(directory.path())?;
    let snapshot = Snapshot::load(&report)?;
    let mut incomplete = edit(report.clone(), Family::Yaesu, 1, None);
    incomplete.clear = false;
    let cases = [
        (incomplete, "give --code"),
        (edit(report.clone(), Family::Yaesu, 0, None), "position 0"),
        (edit(report.clone(), Family::Yaesu, 6, None), "position 6"),
        (
            edit(report.clone(), Family::Yaesu, 1, Some((' ', "X"))),
            "printable ASCII",
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
    let cleared = edit(report.clone(), Family::KenwoodHandheld, 1, None).prepare(&snapshot)?;
    assert_eq!(cleared.before, Some(MicETypeCode::new('*', "TM-D750")?));
    assert!(cleared.replacement.is_some());
    let unchanged = edit(report, Family::KenwoodMobile, 2, None).prepare(&snapshot)?;
    assert!(unchanged.replacement.is_none());
    Ok(())
}

#[test]
fn apply_requires_apply_an_explicit_port_and_one_kind_of_edit() -> TestResult {
    let arguments = |extra: &[&str]| {
        [
            "mcp",
            "mic-e",
            "apply",
            "--backup",
            "report.json",
            "--family",
            "kenwood-mobile",
            "--position",
            "2",
            "--code",
            "*",
            "--model",
            "TM-D750",
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
    assert!(
        crate::mcp::parse(&arguments(&["--apply", "--clear"])).is_err(),
        "--clear conflicts with --code"
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
