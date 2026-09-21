//! Tests for the comparison command over synthetic backup fixtures: argument
//! grammar, changed-byte reporting and field labelling.

use super::*;
use crate::mcp::terminal::tests::{blank_fixture, set, write_fixture};
use crate::mcp::terminal::{TerminalCommand, TerminalRequest};
use clap::Parser;
use kenwood_tmd750::MemoryImage;
use kenwood_tmd750::memory::FieldValue;
use std::path::Path;

type TestResult = AppResult<()>;

fn pair(directory: &Path, before: &MemoryImage, after: &MemoryImage) -> AppResult<CompareRequest> {
    let before_path = directory.join("before capture.json");
    let after_path = directory.join("after capture.json");
    write_fixture(&before_path, before)?;
    write_fixture(&after_path, after)?;
    Ok(CompareRequest {
        before: before_path,
        after: after_path,
        slot: SlotIndex::new(0)?,
        interface: UsbInterface::MainUsb,
        interpret_unqualified: true,
        output: None,
    })
}

fn changed_image(image: MemoryImage, changes: &[(usize, u8)]) -> AppResult<MemoryImage> {
    let mut bytes = image.into_bytes();
    for &(address, value) in changes {
        *bytes
            .get_mut(address)
            .ok_or("fixture address outside image")? = value;
    }
    Ok(MemoryImage::from_bytes(bytes)?)
}

fn replace_json(path: &Path, pointer: &str, value: serde_json::Value) -> TestResult {
    let mut document: serde_json::Value = serde_json::from_slice(&std::fs::read(path)?)?;
    *document
        .pointer_mut(pointer)
        .ok_or("fixture JSON path missing")? = value;
    std::fs::write(path, serde_json::to_vec(&document)?)?;
    Ok(())
}

fn identity_component(path: &Path, component: &str, value: &str) -> TestResult {
    for base in [
        "/backup/identity",
        "/post_exit_verification/attempts/0/connection/identity",
    ] {
        replace_json(
            path,
            &format!("{base}/{component}"),
            serde_json::json!(value),
        )?;
    }
    Ok(())
}

fn raw_changes(report: &ComparisonReport) -> Vec<(u32, u8, u8)> {
    report
        .pages
        .iter()
        .flat_map(|page| &page.changes)
        .map(|change| (change.address, change.before, change.after))
        .collect()
}

#[test]
fn identical_captures_compare_every_standard_page_without_synthetic_bytes() -> TestResult {
    let directory = tempfile::tempdir()?;
    let image = blank_fixture()?;
    let request = pair(directory.path(), &image, &image)?;
    let report = build_report(&request)?;
    assert_eq!(
        report.compared_pages, 1_138,
        "all standard pages must be compared"
    );
    assert_eq!(report.compared_bytes, 289_962, "only captured bytes count");
    assert_eq!(
        report.changed_bytes, 0,
        "identical captures have no byte changes"
    );
    assert_eq!(report.outside_terminal_catalog_bytes, 0, "nothing changed");
    assert!(
        report.pages.is_empty(),
        "unchanged pages must not be reported as changed"
    );
    assert!(
        report.fields.is_empty(),
        "unchanged fields must not be reported as changed"
    );
    let json = serde_json::to_value(&report)?;
    for side in ["before", "after"] {
        assert_eq!(
            json.pointer(&format!("/{side}/assessment/state")),
            Some(&serde_json::json!("decoded")),
            "the supported fixture must decode on both sides"
        );
        assert_eq!(
            json.pointer(&format!("/{side}/identity/firmware")),
            Some(&serde_json::json!("1.02")),
            "comparison must preserve firmware provenance"
        );
    }
    assert_eq!(
        json.get("radio_accessed"),
        Some(&serde_json::json!(false)),
        "offline reports must not claim radio access"
    );
    assert_eq!(
        json.get("radio_applied"),
        Some(&serde_json::json!(false)),
        "offline reports must not claim application"
    );
    Ok(())
}

#[test]
fn comparison_retains_other_slots_unselected_my_memos_and_unrelated_changes() -> TestResult {
    let directory = tempfile::tempdir()?;
    let before = blank_fixture()?;
    let changes = [
        (323_593, 1),
        (323_594, b'X'),
        (331_776, 2),
        (331_792, b'N'),
        (331_844, b'A'),
        (356_352, 1),
    ];
    let after = changed_image(before.clone(), &changes)?;
    let request = pair(directory.path(), &before, &after)?;
    let report = build_report(&request)?;
    let expected: Vec<_> = changes
        .into_iter()
        .map(|(address, value)| Ok((u32::try_from(address)?, 0, value)))
        .collect::<Result<_, std::num::TryFromIntError>>()?;
    assert_eq!(
        raw_changes(&report),
        expected,
        "every observed change must remain in the exact comparison"
    );
    assert_eq!(report.changed_bytes, 6, "all six changed bytes must count");
    assert_eq!(
        report.outside_terminal_catalog_bytes, 1,
        "the PM name change remains outside the Terminal catalog"
    );
    for (start, slot, length) in [
        (323_593, None, 1),
        (331_776, Some(0), 1),
        (331_792, Some(0), 4),
        (331_844, Some(0), 8),
        (356_352, Some(3), 1),
    ] {
        assert!(
            report.fields.iter().any(|change| {
                change.field.start == start
                    && change.field.slot == slot
                    && change.field.length == length
                    && change.changed_bytes == 1
            }),
            "missing exact field attribution for address {start}, slot {slot:?}"
        );
    }
    assert_eq!(
        report.fields.len(),
        5,
        "unrelated bytes must not receive Terminal labels"
    );
    Ok(())
}

#[test]
fn unread_dense_image_gaps_never_become_observed_differences() -> TestResult {
    let directory = tempfile::tempdir()?;
    let before = blank_fixture()?;
    let after = changed_image(before.clone(), &[(0, 1), (48, 2), (393_216, 3)])?;
    let request = pair(directory.path(), &before, &after)?;
    let report = build_report(&request)?;
    assert_eq!(
        report.changed_bytes, 0,
        "uncaptured image bytes cannot enter a sparse comparison"
    );
    assert_eq!(
        report.compared_bytes, 289_962,
        "synthetic gaps must not increase coverage"
    );
    assert!(report.pages.is_empty(), "no captured page changed");
    Ok(())
}

#[test]
fn failed_enum_format_and_utf8_interpretations_preserve_the_raw_difference() -> TestResult {
    for (address, value) in [(331_776, 255), (10, 1), (331_784, 128)] {
        let directory = tempfile::tempdir()?;
        let before = blank_fixture()?;
        let after = changed_image(before.clone(), &[(address, value)])?;
        let request = pair(directory.path(), &before, &after)?;
        let report = build_report(&request)?;
        assert_eq!(
            raw_changes(&report),
            vec![(u32::try_from(address)?, 0, value)],
            "decode failure must not discard changed bytes at {address}"
        );
        let json = serde_json::to_value(&report)?;
        assert_eq!(
            json.pointer("/before/assessment/state"),
            Some(&serde_json::json!("decoded")),
            "the original fixture must remain independently decoded"
        );
        assert_eq!(
            json.pointer("/after/assessment/state"),
            Some(&serde_json::json!("failed")),
            "invalid captured values must not receive a default interpretation"
        );
        let error = json
            .pointer("/after/assessment/error")
            .and_then(serde_json::Value::as_str)
            .ok_or("failed assessment lacks its error")?;
        assert!(
            !error.is_empty(),
            "private evidence must explain the decode failure"
        );
        let console = describe(&report).join("\n");
        assert!(
            console.contains("interpretation failed"),
            "the console must expose assessment failure: {console}"
        );
        assert!(
            !console.contains(error),
            "detailed decoder errors must remain in private output"
        );
    }
    Ok(())
}

#[test]
fn unqualified_firmware_requires_explicit_opt_in_before_output_creation() -> TestResult {
    let directory = tempfile::tempdir()?;
    let image = blank_fixture()?;
    let mut request = pair(directory.path(), &image, &image)?;
    let output = directory.path().join("comparison.json");
    request.output = Some(output.clone());
    request.interpret_unqualified = false;
    let original_before = std::fs::read(&request.before)?;
    let original_after = std::fs::read(&request.after)?;
    assert!(
        run(&request).is_err(),
        "firmware 1.02 requires explicit unqualified interpretation"
    );
    assert!(
        !output.exists(),
        "refused comparisons must not create an output report"
    );
    assert_eq!(
        std::fs::read(&request.before)?,
        original_before,
        "refusal must preserve the earlier source"
    );
    assert_eq!(
        std::fs::read(&request.after)?,
        original_after,
        "refusal must preserve the later source"
    );
    request.interpret_unqualified = true;
    assert_eq!(
        build_report(&request)?.qualification,
        "unqualified_interpretation",
        "opt-in must retain the unqualified label"
    );
    Ok(())
}

#[test]
fn registry_label_match_is_preserved_without_claiming_live_qualification() -> TestResult {
    let directory = tempfile::tempdir()?;
    let image = blank_fixture()?;
    let mut request = pair(directory.path(), &image, &image)?;
    identity_component(&request.before, "firmware", "1.00")?;
    identity_component(&request.after, "firmware", "1.00")?;
    request.interpret_unqualified = false;
    let report = build_report(&request)?;
    assert_eq!(
        report.qualification, "registry_target_matched",
        "exact registry identity must be distinguished from an override"
    );
    let console = describe(&report).join("\n");
    assert!(
        console.contains("Layout interpretation: registry_target_matched"),
        "the console must name the layout the values were decoded with: {console}"
    );
    assert!(
        console.contains("names the model and firmware, not the physical unit"),
        "matching identities must retain their stated limit: {console}"
    );
    Ok(())
}

#[test]
fn mismatched_firmware_or_full_type_identity_cannot_be_compared() -> TestResult {
    for (component, value) in [("firmware", "1.01"), ("radio_type", "K,2,2")] {
        let directory = tempfile::tempdir()?;
        let image = blank_fixture()?;
        let request = pair(directory.path(), &image, &image)?;
        identity_component(&request.after, component, value)?;
        assert!(
            build_report(&request).is_err(),
            "comparison must reject differing {component}, even with layout opt-in"
        );
    }
    Ok(())
}

#[test]
fn incomplete_failed_or_wrong_format_reports_are_not_promoted_to_comparisons() -> TestResult {
    for (pointer, value) in [
        ("/backup/segments", serde_json::json!([])),
        ("/backup/complete_configuration", serde_json::json!(false)),
        ("/transcript/complete", serde_json::json!(false)),
        ("/format_version", serde_json::json!(5)),
    ] {
        let directory = tempfile::tempdir()?;
        let image = blank_fixture()?;
        let request = pair(directory.path(), &image, &image)?;
        replace_json(&request.after, pointer, value)?;
        assert!(
            build_report(&request).is_err(),
            "invalid report state {pointer} must fail structural validation"
        );
    }
    Ok(())
}

#[test]
fn missing_or_malformed_sources_do_not_create_output() -> TestResult {
    let directory = tempfile::tempdir()?;
    let image = blank_fixture()?;
    let mut request = pair(directory.path(), &image, &image)?;
    let output = directory.path().join("comparison.json");
    request.output = Some(output.clone());
    request.before = directory.path().join("missing.json");
    assert!(
        run(&request).is_err(),
        "missing sources must fail without endpoint fallback"
    );
    assert!(
        !output.exists(),
        "a missing source must not leave an output file"
    );
    std::fs::write(&request.before, b"not a JSON report")?;
    assert!(run(&request).is_err(), "malformed source JSON must fail");
    assert!(
        !output.exists(),
        "malformed input must not leave an output file"
    );
    Ok(())
}

#[test]
fn rejected_source_payloads_never_escape_through_comparison_errors() -> TestResult {
    let secret = "PRIVATE_SENTINEL";
    for malformed in 0..3 {
        let directory = tempfile::tempdir()?;
        let image = blank_fixture()?;
        let request = pair(directory.path(), &image, &image)?;
        match malformed {
            0 => replace_json(
                &request.after,
                "/backup/segments/0/data/0",
                serde_json::json!(secret),
            )?,
            1 => identity_component(&request.after, "model", secret)?,
            _ => identity_component(&request.after, "radio_type", secret)?,
        }
        let error = build_report(&request)
            .err()
            .ok_or("private malformed source was unexpectedly accepted")?;
        let mut chain: Option<&dyn std::error::Error> = Some(error.as_ref());
        while let Some(error) = chain {
            assert!(
                !error.to_string().contains(secret),
                "captured input strings must not appear anywhere in the error chain"
            );
            chain = error.source();
        }
    }
    Ok(())
}

#[test]
fn output_never_overwrites_existing_reports_or_either_source() -> TestResult {
    let directory = tempfile::tempdir()?;
    let image = blank_fixture()?;
    let mut request = pair(directory.path(), &image, &image)?;
    let existing = directory.path().join("existing.json");
    std::fs::write(&existing, b"existing report must survive")?;
    let before_bytes = std::fs::read(&request.before)?;
    let after_bytes = std::fs::read(&request.after)?;
    for destination in [&request.before, &request.after, &existing].map(Clone::clone) {
        request.output = Some(destination.clone());
        assert!(
            run(&request).is_err(),
            "existing destination {destination:?} must be refused"
        );
        assert_eq!(
            std::fs::read(&request.before)?,
            before_bytes,
            "the earlier source must remain byte-identical"
        );
        assert_eq!(
            std::fs::read(&request.after)?,
            after_bytes,
            "the later source must remain byte-identical"
        );
        assert_eq!(
            std::fs::read(&existing)?,
            b"existing report must survive",
            "unrelated existing output must remain byte-identical"
        );
    }
    Ok(())
}

#[cfg(unix)]
#[test]
fn private_output_has_owner_only_permissions_and_retains_exact_evidence() -> TestResult {
    use std::os::unix::fs::PermissionsExt;

    let directory = tempfile::tempdir()?;
    let before = blank_fixture()?;
    let after = changed_image(before.clone(), &[(323_594, b'X')])?;
    let mut request = pair(directory.path(), &before, &after)?;
    let output = directory.path().join("private comparison.json");
    request.output = Some(output.clone());
    let expected = serde_json::to_value(build_report(&request)?)?;
    let before_bytes = std::fs::read(&request.before)?;
    let after_bytes = std::fs::read(&request.after)?;
    run(&request)?;
    assert_eq!(
        std::fs::metadata(&output)?.permissions().mode() & 0o777,
        0o600,
        "captured byte values must remain owner-only"
    );
    let saved: serde_json::Value = serde_json::from_slice(&std::fs::read(&output)?)?;
    assert_eq!(
        saved, expected,
        "saved private evidence must match the complete in-memory comparison"
    );
    assert_eq!(
        std::fs::read(&request.before)?,
        before_bytes,
        "saving evidence must not modify the earlier source"
    );
    assert_eq!(
        std::fs::read(&request.after)?,
        after_bytes,
        "saving evidence must not modify the later source"
    );
    Ok(())
}

#[test]
fn console_omits_private_callsigns_text_and_byte_values() -> TestResult {
    let directory = tempfile::tempdir()?;
    let before = blank_fixture()?;
    let mut after = before.clone();
    let slot = SlotIndex::new(0)?;
    for (name, text) in [
        (
            "dv.MyCallsignDvGatewayList[0].MyCallsignDvGateway",
            "N0ABC  A",
        ),
        ("dv.RPT1DvGateway", "N0CALL A"),
    ] {
        set(&mut after, name, slot, FieldValue::Text(text))?;
    }
    let request = pair(directory.path(), &before, &after)?;
    let report = build_report(&request)?;
    let json = serde_json::to_string(&report)?;
    let console = describe(&report).join("\n");
    for text in ["N0ABC  A", "N0CALL A"] {
        assert!(
            json.contains(text),
            "private JSON must retain captured text {text}"
        );
        assert!(
            !console.contains(text),
            "console output must not expose captured text {text}"
        );
    }
    for raw_value in [
        "0x4e",
        "0x4E",
        "[78,48,65,66,67,32,32,65]",
        "\"before\":0",
        "\"after\":78",
    ] {
        assert!(
            !console.contains(raw_value),
            "console output must not expose raw captured bytes: {raw_value}"
        );
    }
    assert!(
        console.contains("Console omits captured text and byte values"),
        "console must explain where private details are retained: {console}"
    );
    Ok(())
}

#[test]
fn parser_preserves_explicit_comparison_scope_and_whitespace_paths() -> TestResult {
    let parsed = TerminalRequest::try_parse_from([
        "terminal",
        "compare",
        "--before",
        "Before Capture/report.json",
        "--after",
        "After Capture/report.json",
        "--slot",
        "5",
        "--interface",
        "panel-usb",
        "--interpret-unqualified",
        "--output",
        "Private Report.json",
    ])?;
    let TerminalCommand::Compare(parsed) = parsed.command else {
        return Err("compare was parsed as a different Terminal command".into());
    };
    assert_eq!(
        parsed.before,
        PathBuf::from("Before Capture/report.json"),
        "the earlier path must preserve spaces"
    );
    assert_eq!(
        parsed.after,
        PathBuf::from("After Capture/report.json"),
        "the later path must preserve spaces"
    );
    assert_eq!(
        parsed.slot.index(),
        5,
        "the requested PM slot must not default to PM Off"
    );
    assert_eq!(
        parsed.interface,
        UsbInterface::PanelUsb,
        "the requested connector must be retained"
    );
    assert!(
        parsed.interpret_unqualified,
        "explicit layout opt-in must survive parsing"
    );
    assert_eq!(
        parsed.output,
        Some(PathBuf::from("Private Report.json")),
        "private output path must preserve spaces"
    );
    Ok(())
}

#[test]
fn parser_requires_complete_scope_and_rejects_live_or_arbitrary_write_options() {
    let valid = [
        "terminal",
        "compare",
        "--before",
        "a",
        "--after",
        "b",
        "--slot",
        "0",
        "--interface",
        "main-usb",
    ];
    for extra in [
        &["--write"][..],
        &["--activate"][..],
        &["--restore"][..],
        &["--address", "10"][..],
        &["--apply"][..],
    ] {
        let arguments: Vec<_> = valid.iter().chain(extra).copied().collect();
        assert!(
            TerminalRequest::try_parse_from(arguments).is_err(),
            "compare must not expose {extra:?}"
        );
    }
    for missing in ["--before", "--after", "--slot", "--interface"] {
        let mut arguments = vec!["terminal", "compare"];
        for pair in [
            ["--before", "a"],
            ["--after", "b"],
            ["--slot", "0"],
            ["--interface", "main-usb"],
        ] {
            if pair.first().copied() != Some(missing) {
                arguments.extend_from_slice(&pair);
            }
        }
        assert!(
            TerminalRequest::try_parse_from(arguments).is_err(),
            "{missing} must be explicit"
        );
    }
    for (slot, interface) in [("6", "main-usb"), ("-1", "main-usb"), ("0", "bluetooth")] {
        assert!(
            TerminalRequest::try_parse_from([
                "terminal",
                "compare",
                "--before",
                "a",
                "--after",
                "b",
                "--slot",
                slot,
                "--interface",
                interface,
            ])
            .is_err(),
            "invalid slot/interface {slot}/{interface} must fail"
        );
    }
}

#[test]
fn offline_dispatch_accepts_comparison_without_a_selected_endpoint() -> TestResult {
    let directory = tempfile::tempdir()?;
    let image = blank_fixture()?;
    let request = pair(directory.path(), &image, &image)?;
    let before_path = request.before.clone();
    let after_path = request.after.clone();
    let before_bytes = std::fs::read(&before_path)?;
    let after_bytes = std::fs::read(&after_path)?;
    let command = crate::mcp::McpCommand::Terminal(TerminalRequest {
        command: TerminalCommand::Compare(request),
    });
    command.validate_endpoint_selection(false)?;
    crate::mcp::run_offline(&command)
        .ok_or("comparison incorrectly dispatched as a live command")??;
    assert_eq!(
        std::fs::read(&before_path)?,
        before_bytes,
        "offline dispatch must preserve the earlier source"
    );
    assert_eq!(
        std::fs::read(&after_path)?,
        after_bytes,
        "offline dispatch must preserve the later source"
    );
    assert_eq!(
        std::fs::read_dir(directory.path())?.count(),
        2,
        "console-only comparison must not create artifacts"
    );
    Ok(())
}
