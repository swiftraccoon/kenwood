//! Tests for argument parsing and the pre-open checks; no enumeration and no
//! connection.

use super::*;
use clap::Parser;

type TestError = Box<dyn std::error::Error + Send + Sync>;
type TestResult = Result<(), TestError>;

#[derive(Parser)]
struct Arguments {
    #[command(flatten)]
    request: SetRequest,
}

fn arguments() -> Vec<String> {
    [
        "mcp",
        "text",
        "set",
        "--backup",
        "backup report.json",
        "--expect",
        "PM1",
        "--apply",
        "pm-name-1",
        "Home",
    ]
    .map(str::to_owned)
    .to_vec()
}

fn request(backup: PathBuf) -> Result<SetRequest, TestError> {
    Ok(SetRequest {
        backup,
        expected: parse_value("PM1")?,
        apply: true,
        output: None,
        channel: None,
        clear: false,
        setting: SetTarget::PmName1,
        value: Some(parse_value("Home")?),
    })
}

fn endpoint() -> SerialCandidate {
    SerialCandidate {
        path: "/dev/never-open".to_owned(),
        vid: Some(0x2166),
        pid: Some(TMD750_MAIN_PID),
    }
}

#[test]
fn live_text_dispatch_requires_explicit_port_and_never_runs_offline() -> TestResult {
    let command = super::super::parse(&arguments())?;
    assert!(
        command.validate_endpoint_selection(false).is_err(),
        "set requires an explicitly pinned endpoint"
    );
    command.validate_endpoint_selection(true)?;
    assert!(
        super::super::run_offline(&command).is_none(),
        "live set must not be dispatched as an offline command"
    );
    Ok(())
}

#[test]
fn approval_expected_name_and_supported_scope_are_mandatory() -> TestResult {
    let _baseline = super::super::parse(&arguments())?;
    let mut without_approval = arguments();
    without_approval.retain(|word| word != "--apply");
    assert!(
        super::super::parse(&without_approval).is_err(),
        "CLI approval cannot default to true"
    );
    assert!(
        super::super::parse(
            &[
                "mcp",
                "text",
                "set",
                "--backup",
                "capture",
                "--apply",
                "pm-name-1",
                "Home"
            ]
            .map(str::to_owned)
        )
        .is_err(),
        "expected current name is required"
    );
    for setting in ["pm-name-2", "dstar-message-1", "dstar-my-callsign-2"] {
        let mut candidate = arguments();
        if let Some(word) = candidate.iter_mut().find(|word| *word == "pm-name-1") {
            *word = setting.to_owned();
        }
        assert!(
            super::super::parse(&candidate).is_err(),
            "{setting} must remain outside this dedicated text setter"
        );
    }
    for flag in ["--slot", "--address", "--force", "--interpret-unqualified"] {
        let mut candidate = arguments();
        candidate.extend([flag.to_owned(), "1".to_owned()]);
        assert!(
            super::super::parse(&candidate).is_err(),
            "{flag} cannot expand the live scope"
        );
    }
    Ok(())
}

#[test]
fn parser_preserves_case_spaces_and_paths_without_normalization() -> TestResult {
    let parsed = Arguments::try_parse_from([
        "text-set",
        "--backup",
        "Backup Dir/report.json",
        "--expect",
        " PM1 ",
        "--apply",
        "--output",
        "New Evidence",
        "pm-name-1",
        " Home /a ",
    ])?;
    assert_eq!(parsed.request.expected.as_str(), " PM1 ");
    assert_eq!(parsed.request.value.as_deref(), Some(" Home /a "));
    assert_eq!(
        parsed.request.backup,
        PathBuf::from("Backup Dir/report.json")
    );
    assert_eq!(parsed.request.output, Some(PathBuf::from("New Evidence")));
    Ok(())
}

#[test]
fn invalid_labels_and_no_change_are_rejected_before_any_io() -> TestResult {
    for value in ["", "ABCDEFGHIJKLMNOPQ", "home\n", "caf\u{e9}", "tab\tname"] {
        assert!(
            Arguments::try_parse_from([
                "text-set",
                "--backup",
                "missing.json",
                "--expect",
                "PM1",
                "--apply",
                "pm-name-1",
                value
            ])
            .is_err(),
            "invalid label {value:?} must fail during parsing"
        );
    }
    let mut candidate = request(PathBuf::from("missing.json"))?;
    candidate.value = Some(candidate.expected.clone());
    assert!(
        candidate
            .prepare(&endpoint(), DEFAULT_BAUD)
            .is_err_and(|error| error.to_string().contains("no name change")),
        "no-op must not touch the backup or radio"
    );
    candidate.value = Some("Home".to_owned());
    candidate.apply = false;
    assert!(
        candidate
            .prepare(&endpoint(), DEFAULT_BAUD)
            .is_err_and(|error| error.to_string().contains("explicit --apply")),
        "constructed requests also require approval"
    );
    Ok(())
}

#[test]
fn endpoint_and_baud_policy_precede_backup_access() -> TestResult {
    let candidate = request(PathBuf::from("missing.json"))?;
    for selected in [
        SerialCandidate {
            vid: None,
            pid: None,
            ..endpoint()
        },
        SerialCandidate {
            vid: Some(0xFFFF),
            ..endpoint()
        },
        SerialCandidate {
            pid: Some(0x0001),
            ..endpoint()
        },
    ] {
        assert!(
            candidate
                .prepare(&selected, DEFAULT_BAUD)
                .is_err_and(|error| error.to_string().contains("TM-D750 USB")),
            "an unidentified or foreign USB interface must be refused before file access"
        );
    }
    assert!(
        candidate
            .prepare(&endpoint(), 115_200)
            .is_err_and(|error| error.to_string().contains("9600 baud")),
        "wrong baud must be refused before file access"
    );
    Ok(())
}

fn backup_fixture(path: &Path) -> AppResult<()> {
    let mut fixture = super::super::snapshot::tests::fixture();
    let segments = fixture
        .get_mut("backup")
        .and_then(|value| value.get_mut("segments"))
        .and_then(serde_json::Value::as_array_mut)
        .ok_or("fixture lacks segments")?;
    let segment = segments
        .iter_mut()
        .find(|segment| segment.get("address") == Some(&serde_json::json!(323_584)))
        .ok_or("fixture lacks PM1 page")?;
    let mut page = vec![0x42_u8; 256];
    page.get_mut(10..26).ok_or("name range")?.fill(0);
    page.get_mut(10..13)
        .ok_or("name prefix")?
        .copy_from_slice(b"PM1");
    *segment.get_mut("data").ok_or("fixture lacks page data")? = serde_json::to_value(page)?;
    serde_json::to_writer(File::create_new(path)?, &fixture)?;
    Ok(())
}

#[test]
fn complete_backup_retains_all_unrelated_bytes_and_binds_expected_name() -> TestResult {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("report.json");
    backup_fixture(&path)?;
    let mut candidate = request(path)?;
    for pid in [TMD750_MAIN_PID, TMD750_PANEL_PID] {
        let selected = SerialCandidate {
            pid: Some(pid),
            ..endpoint()
        };
        let PreparedUpdate::Pm1(update) = candidate.prepare(&selected, DEFAULT_BAUD)? else {
            return Err("PM1 request selected a different target".into());
        };
        assert_eq!(update.original_page().first(), Some(&0x42));
        assert_eq!(update.current().map(Pm1Name::as_str), Some("PM1"));
        assert_eq!(update.requested().map(Pm1Name::as_str), Some("Home"));
        for (index, (before, after)) in update
            .original_page()
            .iter()
            .zip(update.desired_page())
            .enumerate()
        {
            assert!(
                before == after || (10..26).contains(&index),
                "unrelated byte {index} must not change"
            );
        }
    }
    candidate.expected = "Other".to_owned();
    assert!(
        candidate.prepare(&endpoint(), DEFAULT_BAUD).is_err(),
        "expected current name must match actual captured bytes"
    );
    Ok(())
}

fn channel_request(words: &[&str]) -> Result<SetRequest, TestError> {
    let mut arguments = vec!["text-set", "--backup", "missing.json", "--expect", ""];
    arguments.extend_from_slice(words);
    Ok(Arguments::try_parse_from(arguments)?.request)
}

#[test]
fn channel_name_selector_clear_and_scope_are_enforced_before_any_io() -> TestResult {
    let named = channel_request(&["--apply", "--channel", "999", "channel-name", "Repeater 1"])?;
    assert_eq!(named.setting, SetTarget::ChannelName);
    assert_eq!(named.channel.map(PhysicalChannel::index), Some(999));
    assert_eq!(named.value.as_deref(), Some("Repeater 1"));
    named.validate_options()?;

    let cleared = channel_request(&["--apply", "--channel", "l05", "--clear", "channel-name"])?;
    assert!(cleared.clear);
    assert_eq!(cleared.value, None);
    assert_eq!(cleared.channel.map(PhysicalChannel::index), Some(1010));
    assert!(
        cleared
            .validate_options()
            .is_err_and(|error| error.to_string().contains("no channel name change")),
        "clearing an unnamed channel is a no-op"
    );

    assert!(
        channel_request(&["--apply", "--clear", "channel-name"])?
            .validate_options()
            .is_err_and(|error| error.to_string().contains("requires --channel")),
        "clearing a channel name requires --channel"
    );
    assert!(
        channel_request(&["--apply", "--clear", "pm-name-1"])?
            .validate_options()
            .is_err_and(|error| error
                .to_string()
                .contains("dstar-my-callsign-1 and channel-name only")),
        "PM1 has no empty form"
    );
    assert!(
        channel_request(&[
            "--apply",
            "--channel",
            "999",
            "--clear",
            "channel-name",
            "Home"
        ])
        .is_err(),
        "--clear and NEW_TEXT conflict"
    );
    assert!(
        channel_request(&["--apply", "--channel", "1102", "channel-name", "Home"]).is_err(),
        "the selector must name a memory channel"
    );
    let missing = channel_request(&["--apply", "channel-name", "Home"])?;
    assert!(
        missing
            .validate_options()
            .is_err_and(|error| error.to_string().contains("requires --channel"))
    );
    let misuse = channel_request(&["--apply", "--channel", "999", "pm-name-1", "Home"])?;
    assert!(
        misuse
            .validate_options()
            .is_err_and(|error| error.to_string().contains("channel-name only"))
    );
    Ok(())
}

fn name_page_fixture(path: &Path) -> AppResult<()> {
    let mut fixture = super::super::snapshot::tests::fixture();
    let segments = fixture
        .get_mut("backup")
        .and_then(|value| value.get_mut("segments"))
        .and_then(serde_json::Value::as_array_mut)
        .ok_or("fixture lacks segments")?;
    let segment = segments
        .iter_mut()
        .find(|segment| segment.get("address") == Some(&serde_json::json!(81_408)))
        .ok_or("fixture lacks the channel 999 name page")?;
    let mut page = vec![0x42_u8; 256];
    page.get_mut(112..128).ok_or("name range")?.fill(0);
    *segment.get_mut("data").ok_or("fixture lacks page data")? = serde_json::to_value(page)?;
    let format = segments
        .iter_mut()
        .find(|segment| segment.get("address") == Some(&serde_json::json!(8)))
        .and_then(|segment| segment.get_mut("data"))
        .and_then(serde_json::Value::as_array_mut)
        .ok_or("fixture lacks the format fragment")?;
    *format
        .get_mut(2)
        .ok_or("fixture lacks the memory-format byte")? = serde_json::json!(0);
    serde_json::to_writer(File::create_new(path)?, &fixture)?;
    Ok(())
}

#[test]
fn channel_name_prepare_binds_the_captured_name_page_on_either_usb_role() -> TestResult {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("report.json");
    name_page_fixture(&path)?;
    let mut candidate =
        channel_request(&["--apply", "--channel", "999", "channel-name", "Repeater 1"])?;
    candidate.backup = path;
    for pid in [TMD750_MAIN_PID, TMD750_PANEL_PID] {
        let selected = SerialCandidate {
            pid: Some(pid),
            ..endpoint()
        };
        let PreparedUpdate::ChannelName(update) = candidate.prepare(&selected, DEFAULT_BAUD)?
        else {
            return Err("channel-name request selected a different target".into());
        };
        assert_eq!(update.channel().index(), 999);
        assert_eq!(update.page().address().as_u32(), 81_408);
        assert_eq!(update.original_page().first(), Some(&0x42));
        assert_eq!(update.current(), None);
        assert_eq!(
            update.desired_page().get(112..128),
            Some(b"Repeater 1\0\0\0\0\0\0".as_slice())
        );
    }
    assert!(
        candidate
            .prepare(
                &SerialCandidate {
                    vid: None,
                    pid: None,
                    ..endpoint()
                },
                DEFAULT_BAUD
            )
            .is_err_and(|error| error.to_string().contains("TM-D750 USB")),
        "an unidentified endpoint is refused before file access"
    );
    candidate.expected = "Other".to_owned();
    assert!(
        candidate.prepare(&endpoint(), DEFAULT_BAUD).is_err(),
        "the expected name must match the captured field"
    );
    Ok(())
}
