//! DTMF and `EchoLink` memory edits planned from backup reports: the channel
//! and pages they change, the refusals, the journal description and the
//! endpoint gate.

use std::fs::File;
use std::path::Path;

use super::*;

type TestError = Box<dyn std::error::Error + Send + Sync>;
type TestResult = Result<(), TestError>;

/// A backup whose memories are empty except DTMF channel 1, which dials
/// `123` and is named `Home`.
fn backup(directory: &Path) -> Result<PathBuf, TestError> {
    let text = |field: &[u8], offset: u32| field.get(usize::try_from(offset).ok()?).copied();
    let stored = |address: u32| match address {
        0x50..0x53 => text(b"123", address - 0x50),
        0x110..0x114 => text(b"Home", address - 0x110),
        0x40..0xE0 | 0x100..0x1A0 | 0x200..0x2A0 => Some(0),
        _ => None,
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

fn edit(
    backup: PathBuf,
    memory: Memory,
    channel: u8,
    entry: Option<(Option<&str>, Option<&str>)>,
) -> EditRequest {
    EditRequest {
        backup,
        memory,
        channel,
        name: entry.and_then(|(name, _)| name.map(str::to_owned)),
        code: entry.and_then(|(_, code)| code.map(str::to_owned)),
        clear: entry.is_none(),
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
fn an_edit_stores_one_channel_and_plans_the_pages_it_changes() -> TestResult {
    let directory = tempfile::tempdir()?;
    let report = backup(directory.path())?;
    let snapshot = Snapshot::load(&report)?;
    let prepared = edit(
        report.clone(),
        Memory::Dtmf,
        4,
        Some((Some("Office"), Some("555 1212"))),
    )
    .prepare(&snapshot)?;
    assert_eq!(prepared.before, None);
    assert_eq!(pages(&prepared), [0x38, 0x100]);
    assert_eq!(
        description(&prepared).get("dtmf"),
        Some(&json!({
            "memory": "dtmf",
            "channel": 4,
            "before": null,
            "after": {"name": "Office", "code": "555 1212"},
        }))
    );
    let node =
        edit(report, Memory::EchoLink, 0, Some((None, Some("12345678")))).prepare(&snapshot)?;
    assert_eq!(
        node.after,
        Some(Stored {
            name: String::new(),
            code: "12345678".to_owned()
        })
    );
    assert_eq!(pages(&node), [0x200]);
    Ok(())
}

#[test]
fn an_edit_needs_a_name_or_code_or_clear_and_a_radio_channel() -> TestResult {
    let directory = tempfile::tempdir()?;
    let report = backup(directory.path())?;
    let snapshot = Snapshot::load(&report)?;
    let mut incomplete = edit(report.clone(), Memory::Dtmf, 1, None);
    incomplete.clear = false;
    let cases = [
        (incomplete, "give --name"),
        (edit(report.clone(), Memory::Dtmf, 10, None), "CH0 to CH9"),
        (
            edit(report.clone(), Memory::Dtmf, 1, Some((None, Some("12a")))),
            "0-9, A-D",
        ),
        (
            edit(
                report.clone(),
                Memory::EchoLink,
                1,
                Some((Some("Nine char"), None)),
            ),
            "longer than 8 bytes",
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
    let cleared = edit(report.clone(), Memory::Dtmf, 1, None).prepare(&snapshot)?;
    assert_eq!(
        cleared.before,
        Some(Stored {
            name: "Home".to_owned(),
            code: "123".to_owned()
        })
    );
    assert_eq!(pages(&cleared), [0x38, 0x100]);
    let unchanged = edit(report, Memory::EchoLink, 1, None).prepare(&snapshot)?;
    assert!(unchanged.replacements.is_empty());
    Ok(())
}

#[test]
fn apply_requires_apply_and_an_explicit_port() -> TestResult {
    let arguments = |extra: &[&str]| {
        [
            "mcp",
            "dtmf",
            "apply",
            "--backup",
            "report.json",
            "--memory",
            "echolink",
            "--channel",
            "2",
            "--code",
            "*123#",
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
