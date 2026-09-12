//! Offline CLI contract tests; fixtures never represent live radio evidence.

use super::*;
use kenwood_tmd750::MemoryImage;
use kenwood_tmd750::memory::{FieldValue, TerminalFinding, menu_field};
use std::path::Path;

type TestResult = AppResult<()>;

fn request(path: &Path) -> AppResult<PreflightRequest> {
    Ok(PreflightRequest {
        backup: path.to_owned(),
        slot: SlotIndex::new(0)?,
        interface: UsbInterface::MainUsb,
        interpret_unqualified: false,
    })
}

pub(super) fn write_fixture(path: &Path, image: &MemoryImage) -> TestResult {
    let mut document = super::super::snapshot::tests::fixture();
    let segments = document
        .pointer_mut("/backup/segments")
        .and_then(serde_json::Value::as_array_mut)
        .ok_or("fixture lacks segments")?;
    for segment in segments {
        let start = usize::try_from(
            segment
                .get("address")
                .and_then(serde_json::Value::as_u64)
                .ok_or("fixture lacks address")?,
        )?;
        let length = usize::try_from(
            segment
                .get("length")
                .and_then(serde_json::Value::as_u64)
                .ok_or("fixture lacks length")?,
        )?;
        let bytes = image
            .as_bytes()
            .get(start..start + length)
            .ok_or("fixture segment outside image")?;
        *segment.get_mut("data").ok_or("fixture lacks data")? = serde_json::to_value(bytes)?;
    }
    let mut writer = std::io::BufWriter::new(super::super::capture::create_private_file(path)?);
    super::super::write_report(&mut writer, &document)?;
    Ok(())
}

pub(super) fn blank_fixture() -> AppResult<MemoryImage> {
    Ok(MemoryImage::from_bytes(
        vec![0; kenwood_tmd750::types::IMAGE_LENGTH],
    )?)
}

pub(super) fn set(
    image: &mut MemoryImage,
    name: &str,
    slot: SlotIndex,
    value: FieldValue<'_>,
) -> TestResult {
    let field = menu_field(name).ok_or("fixture field missing")?;
    image.set(&field.descriptor, Some(slot), value)?;
    Ok(())
}

#[test]
fn parser_requires_explicit_slot_and_interface_and_exposes_no_write_surface() -> TestResult {
    let arguments = [
        "terminal",
        "preflight",
        "--backup",
        "Capture Case/report.json",
        "--slot",
        "5",
        "--interface",
        "panel-usb",
        "--interpret-unqualified",
    ];
    let parsed = TerminalRequest::try_parse_from(arguments)?;
    let TerminalCommand::Preflight(parsed) = parsed.command else {
        return Err("expected preflight command".into());
    };
    assert_eq!(parsed.backup, PathBuf::from("Capture Case/report.json"));
    assert_eq!(parsed.slot.index(), 5);
    assert_eq!(parsed.interface, UsbInterface::PanelUsb);
    assert!(parsed.interpret_unqualified);
    for additional in [
        &["--output", "patch.json"][..],
        &["--write"][..],
        &["--activate"][..],
    ] {
        let args: Vec<_> = arguments.iter().chain(additional).copied().collect();
        assert!(TerminalRequest::try_parse_from(args).is_err());
    }
    for invalid in [
        vec![
            "terminal",
            "preflight",
            "--backup",
            "x",
            "--interface",
            "main-usb",
        ],
        vec!["terminal", "preflight", "--backup", "x", "--slot", "0"],
        vec![
            "terminal",
            "preflight",
            "--backup",
            "x",
            "--slot",
            "6",
            "--interface",
            "main-usb",
        ],
        vec![
            "terminal",
            "preflight",
            "--backup",
            "x",
            "--slot",
            "0",
            "--interface",
            "bluetooth",
        ],
    ] {
        assert!(TerminalRequest::try_parse_from(invalid).is_err());
    }
    Ok(())
}

#[test]
fn firmware_policy_is_explicit_and_output_never_claims_live_readiness() -> TestResult {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("backup.json");
    write_fixture(&path, &blank_fixture()?)?;
    let original = std::fs::read(&path)?;
    let snapshot = Snapshot::load(&path)?;
    let mut request = request(&path)?;
    assert!(assess(&request, &snapshot).is_err());
    request.interpret_unqualified = true;
    let report = assess(&request, &snapshot)?;
    assert_eq!(report.firmware.as_str(), "1.02");
    assert_eq!(
        report.qualification,
        TextLayoutQualification::UnqualifiedInterpretation
    );
    assert_eq!(report.findings, vec![TerminalFinding::MissingMyCallsign]);
    let output = describe(&snapshot, &report)?.join("\n");
    for expected in [
        "Historical backup only",
        "current radio state is unknown",
        "firmware 1.02",
        "Unqualified software-layout interpretation",
        "No radio endpoints enumerated or opened",
        "Live activation and restoration remain unqualified",
        "Main-unit ID/FV/TY/GW queries were observed on firmware 1.02",
        "with Terminal selected and the Gateway routed to panel USB",
        "does not establish current routing, general CAT access",
        "MCP readiness, or qualified automatic exit",
    ] {
        assert!(output.contains(expected), "missing {expected}: {output}");
    }
    assert!(
        !output.contains("Another port's CAT access and automatic exit are unqualified"),
        "the bounded alternate-port CAT observation must not be discarded"
    );
    assert_eq!(std::fs::read(&path)?, original);
    Ok(())
}

#[test]
fn selected_pm_and_my_entry_are_never_substituted_with_populated_defaults() -> TestResult {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("backup.json");
    let slot = SlotIndex::new(5)?;
    let mut image = blank_fixture()?;
    set(
        &mut image,
        "dv.MyCallsignSelectDvGateway",
        slot,
        FieldValue::Unsigned(5),
    )?;
    set(
        &mut image,
        "dv.MyCallsignDvGatewayList[0].MyCallsignDvGateway",
        slot,
        FieldValue::Text("N0ABC  A"),
    )?;
    set(
        &mut image,
        "radio.DvGatewayInterface",
        slot,
        FieldValue::Unsigned(1),
    )?;
    write_fixture(&path, &image)?;
    let snapshot = Snapshot::load(&path)?;
    let mut request = request(&path)?;
    request.interpret_unqualified = true;
    request.slot = slot;
    let report = assess(&request, &snapshot)?;
    assert_eq!(report.slot.index(), 5);
    assert_eq!(report.captured_active_slot.index(), 0);
    assert_eq!(report.selected_my_callsign.index(), 5);
    assert!(report.my_callsign.is_empty());
    assert_eq!(
        report.findings,
        vec![
            TerminalFinding::MissingMyCallsign,
            TerminalFinding::RouteMismatch,
            TerminalFinding::DifferentCapturedActivePm,
        ]
    );
    request.interface = UsbInterface::PanelUsb;
    assert!(
        !assess(&request, &snapshot)?
            .findings
            .contains(&TerminalFinding::RouteMismatch)
    );
    Ok(())
}

#[test]
fn offline_dispatch_handles_both_success_and_missing_backup_without_endpoint_selection()
-> TestResult {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("backup.json");
    let command = super::super::McpCommand::Terminal(TerminalRequest {
        command: TerminalCommand::Preflight(PreflightRequest {
            interpret_unqualified: true,
            ..request(&path)?
        }),
    });
    command.validate_endpoint_selection(false)?;
    let missing =
        super::super::run_offline(&command).ok_or("offline command dispatched as live")?;
    assert!(missing.is_err());
    assert!(!path.exists());
    write_fixture(&path, &blank_fixture()?)?;
    let original = std::fs::read(&path)?;
    super::super::run_offline(&command).ok_or("offline command dispatched as live")??;
    assert_eq!(std::fs::read(&path)?, original);
    assert_eq!(std::fs::read_dir(directory.path())?.count(), 1);
    Ok(())
}

#[test]
fn even_conflict_free_output_does_not_claim_a_radio_is_ready() -> TestResult {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("backup.json");
    let mut image = blank_fixture()?;
    set(
        &mut image,
        "dv.MyCallsignDvGatewayList[0].MyCallsignDvGateway",
        SlotIndex::new(0)?,
        FieldValue::Text("N0ABC  A"),
    )?;
    write_fixture(&path, &image)?;
    let snapshot = Snapshot::load(&path)?;
    let mut request = request(&path)?;
    request.interpret_unqualified = true;
    let report = assess(&request, &snapshot)?;
    assert!(report.findings.is_empty());
    let output = describe(&snapshot, &report)?.join("\n");
    assert!(output.contains("No conflicts found by these offline checks"));
    assert!(output.contains("This is not a live-readiness result"));
    assert!(
        output.contains("Main-unit ID/FV/TY/GW queries were observed on firmware 1.02"),
        "the bounded alternate-port CAT finding must remain explicit"
    );
    assert!(
        output.contains("MCP readiness, or qualified automatic exit"),
        "historical CAT success does not establish automatic exit readiness"
    );
    Ok(())
}
