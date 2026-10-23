//! Group name edits planned from backup reports: the names and pages they
//! change, the refusals, the journal description and the endpoint gate.

use std::fs::File;
use std::path::Path;

use super::*;

type TestError = Box<dyn std::error::Error + Send + Sync>;
type TestResult = Result<(), TestError>;

/// A backup holding the default group names `GRP-0` to `GRP-29` and
/// `Weather`.
fn backup(directory: &Path) -> Result<PathBuf, TestError> {
    let mut names = vec![0_u8; 31 * 16];
    for (index, field) in names.as_chunks_mut::<16>().0.iter_mut().enumerate() {
        let text = if index == 30 {
            "Weather".to_owned()
        } else {
            format!("GRP-{index}")
        };
        for (destination, source) in field.iter_mut().zip(text.bytes()) {
            *destination = source;
        }
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
                .checked_sub(0x1_4800)
                .and_then(|index| names.get(usize::try_from(index).ok()?))
            {
                *byte = Value::from(*stored);
            }
        }
    }
    let path = directory.join("report.json");
    serde_json::to_writer(File::create_new(&path)?, &document)?;
    Ok(path)
}

fn selection(backup: PathBuf, pairs: &[(&str, &str)]) -> NamesSelection {
    NamesSelection {
        backup,
        set: pairs
            .iter()
            .flat_map(|(group, name)| [(*group).to_owned(), (*name).to_owned()])
            .collect(),
    }
}

#[test]
fn an_edit_names_its_groups_and_plans_their_pages() -> TestResult {
    let directory = tempfile::tempdir()?;
    let report = backup(directory.path())?;
    let snapshot = Snapshot::load(&report)?;
    let prepared =
        selection(report, &[("3", "Repeaters"), ("20", "Simplex 2m")]).prepare(&snapshot)?;
    let pages: Vec<u32> = prepared
        .replacements
        .iter()
        .map(|replacement| replacement.page().address().as_u32())
        .collect();
    assert_eq!(pages, [0x1_4800, 0x1_4900]);
    assert_eq!(
        description(&prepared).get("group_names"),
        Some(&json!([
            {"group": 3, "before": "GRP-3", "after": "Repeaters"},
            {"group": 20, "before": "GRP-20", "after": "Simplex 2m"},
        ]))
    );
    Ok(())
}

#[test]
fn an_edit_needs_valid_groups_and_names() -> TestResult {
    let directory = tempfile::tempdir()?;
    let report = backup(directory.path())?;
    let snapshot = Snapshot::load(&report)?;
    let cases = [
        (selection(report.clone(), &[]), "give at least one"),
        (selection(report.clone(), &[("x", "A")]), "not 0 to 29"),
        (selection(report.clone(), &[("30", "A")]), "memory group"),
        (
            selection(report.clone(), &[("1", "Seventeen bytes!!")]),
            "printable ASCII",
        ),
        (
            selection(report.clone(), &[("4", "A"), ("4", "B")]),
            "named more than once",
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
    let unchanged = selection(report, &[("29", "GRP-29")]).prepare(&snapshot)?;
    assert_eq!(unchanged.replacements, Vec::<PageReplacement>::new());
    Ok(())
}

#[test]
fn apply_requires_apply_and_an_explicit_port() -> TestResult {
    let arguments = |extra: &[&str]| {
        [
            "mcp",
            "group-names",
            "apply",
            "--backup",
            "report.json",
            "--set",
            "0",
            "Local",
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
