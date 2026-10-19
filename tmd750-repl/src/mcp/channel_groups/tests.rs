//! Channel group assignments planned from backup reports: the groups and
//! pages they change, the refusals, the journal description and the endpoint
//! gate.

use super::*;
use crate::mcp::channels::tests::stored_backup;

type TestError = Box<dyn std::error::Error + Send + Sync>;
type TestResult = Result<(), TestError>;

fn selection(backup: PathBuf, pairs: &[(&str, &str)]) -> GroupSelection {
    GroupSelection {
        backup,
        set: pairs
            .iter()
            .flat_map(|(channel, group)| [(*channel).to_owned(), (*group).to_owned()])
            .collect(),
    }
}

#[test]
fn an_assignment_rewrites_the_group_of_a_stored_channel() -> TestResult {
    let directory = tempfile::tempdir()?;
    let report = stored_backup(directory.path())?;
    let snapshot = Snapshot::load(&report)?;
    let prepared = selection(report, &[("005", "12")]).prepare(&snapshot)?;
    let pages: Vec<u32> = prepared
        .replacements
        .iter()
        .map(|replacement| replacement.page().address().as_u32())
        .collect();
    assert_eq!(pages, [0x2000]);
    assert_eq!(
        description(&prepared).get("channel_groups"),
        Some(&json!([{"channel": "005", "before": 0, "after": 12}]))
    );
    Ok(())
}

#[test]
fn an_assignment_needs_a_stored_regular_channel_and_a_group() -> TestResult {
    let directory = tempfile::tempdir()?;
    let report = stored_backup(directory.path())?;
    let snapshot = Snapshot::load(&report)?;
    let cases = [
        (selection(report.clone(), &[]), "give at least one"),
        (selection(report.clone(), &[("005", "x")]), "not 0 to 29"),
        (selection(report.clone(), &[("005", "30")]), "memory group"),
        (
            selection(report.clone(), &[("L00", "1")]),
            "not a regular channel",
        ),
        (
            selection(report.clone(), &[("006", "1")]),
            "holds no stored record",
        ),
        (
            selection(report, &[("005", "1"), ("005", "2")]),
            "assigned a group more than once",
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
            "channel-groups",
            "apply",
            "--backup",
            "report.json",
            "--set",
            "005",
            "12",
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
