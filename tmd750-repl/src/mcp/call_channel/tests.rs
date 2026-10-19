//! CALL channel edits planned from backup reports: the record and page they
//! change, the band check, the journal description and the endpoint gate.

use clap::ValueEnum;

use super::*;
use crate::mcp::channels::tests::{TestError, stored_backup};

type TestResult = Result<(), TestError>;

fn edit(backup: PathBuf, channel: Channel, frequency: &str) -> EditRequest {
    EditRequest {
        backup,
        channel,
        frequency: frequency.to_owned(),
    }
}

#[test]
fn an_edit_replaces_only_the_receive_frequency() -> TestResult {
    let directory = tempfile::tempdir()?;
    let report = stored_backup(directory.path())?;
    let snapshot = Snapshot::load(&report)?;
    let prepared = edit(report, Channel::UhfFm, "446.100").prepare(&snapshot)?;
    assert_eq!(prepared.before.receive_frequency.as_hz(), 446_000_000);
    let mut expected = prepared.before.clone();
    expected.receive_frequency = Frequency::new(446_100_000)?;
    assert_eq!(prepared.after, expected);
    let replacement = prepared.replacement.as_ref().ok_or("one changed page")?;
    assert_eq!(replacement.page().address().as_u32(), 0xFD00);
    assert_eq!(
        description(&prepared).get("call_channel"),
        Some(&json!({
            "channel": "uhf-fm",
            "before_hz": 446_000_000,
            "after_hz": 446_100_000,
        }))
    );
    Ok(())
}

#[test]
fn an_edit_stays_inside_the_channel_band() -> TestResult {
    let directory = tempfile::tempdir()?;
    let report = stored_backup(directory.path())?;
    let snapshot = Snapshot::load(&report)?;
    for (channel, frequency, expected) in [
        (Channel::VhfFm, "223.500", "outside 136000000..174000000 Hz"),
        (
            Channel::Band220Dv,
            "174.000",
            "outside 216000000..260000000 Hz",
        ),
        (Channel::UhfDv, "four", "decimal megahertz"),
    ] {
        let result = edit(report.clone(), channel, frequency).prepare(&snapshot);
        assert!(
            matches!(&result, Err(error) if error.to_string().contains(expected)),
            "{frequency}: {:?}",
            result.err().map(|error| error.to_string())
        );
    }
    let unchanged = edit(report, Channel::Band220Dv, "223.000").prepare(&snapshot)?;
    assert!(unchanged.replacement.is_none());
    Ok(())
}

#[test]
fn every_channel_name_maps_to_its_call_channel_and_journal_label() {
    for value in Channel::value_variants() {
        let name = value
            .to_possible_value()
            .map(|possible| possible.get_name().to_owned());
        assert_eq!(name.as_deref(), Some(label(value.call_channel())));
    }
    let channels: Vec<CallChannel> = Channel::value_variants()
        .iter()
        .map(|value| value.call_channel())
        .collect();
    assert_eq!(channels, CallChannel::ALL);
}

#[test]
fn set_requires_apply_and_an_explicit_port() -> TestResult {
    let arguments = |extra: &[&str]| {
        [
            "mcp",
            "call-channel",
            "set",
            "--backup",
            "report.json",
            "--channel",
            "uhf-fm",
            "--frequency",
            "446.1",
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
