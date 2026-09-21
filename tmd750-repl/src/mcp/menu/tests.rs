//! Tests for the offline menu commands: argument grammar, which backups the
//! loader accepts, and the exact bytes a preview produces.

use std::fs::File;

use super::*;
use crate::mcp::{McpCommand, parse};

type TestError = Box<dyn std::error::Error + Send + Sync>;
type TestResult = Result<(), TestError>;

fn command(words: &[&str]) -> Result<McpCommand, clap::Error> {
    parse(
        &words
            .iter()
            .map(|word| (*word).to_owned())
            .collect::<Vec<_>>(),
    )
}

fn page_mut(
    document: &mut serde_json::Value,
    address: u32,
) -> Result<&mut Vec<serde_json::Value>, TestError> {
    document
        .pointer_mut("/backup/segments")
        .and_then(serde_json::Value::as_array_mut)
        .and_then(|segments| {
            segments
                .iter_mut()
                .find(|segment| segment.get("address") == Some(&address.into()))
        })
        .and_then(|segment| segment.get_mut("data"))
        .and_then(serde_json::Value::as_array_mut)
        .ok_or_else(|| "fixture page missing".into())
}

fn fixture() -> Result<serde_json::Value, TestError> {
    let mut document = crate::mcp::snapshot::tests::fixture();
    *page_mut(&mut document, 8)?
        .get_mut(2)
        .ok_or("format missing")? = 0.into();
    *page_mut(&mut document, 323_584)?
        .get_mut(9)
        .ok_or("PM selection missing")? = 0.into();
    *page_mut(&mut document, 331_776)?
        .first_mut()
        .ok_or("Gateway missing")? = 0.into();
    Ok(document)
}

fn save(path: &Path, document: &serde_json::Value) -> TestResult {
    serde_json::to_writer(File::create_new(path)?, document)?;
    Ok(())
}

fn selection(path: &Path, field: &str, slot: Option<u8>) -> Result<Selection, TestError> {
    Ok(Selection {
        backup: path.to_owned(),
        field: field.to_owned(),
        slot: slot.map(SlotIndex::new).transpose()?,
    })
}

#[test]
fn offline_commands_dispatch_without_an_endpoint() -> TestResult {
    for words in [
        vec!["mcp", "menu", "list", "--group", "PM"],
        vec!["mcp", "menu", "describe", "pm.PmName2"],
        vec![
            "mcp",
            "menu",
            "show",
            "--backup",
            "missing.json",
            "pm.PmName2",
        ],
        vec![
            "mcp",
            "menu",
            "preview",
            "--backup",
            "missing.json",
            "pm.PmName2",
            "BASE",
        ],
    ] {
        let command = command(&words)?;
        command.validate_endpoint_selection(false)?;
        assert!(
            crate::mcp::run_offline(&command).is_some(),
            "offline command must be handled before enumeration: {words:?}"
        );
    }
    Ok(())
}

#[test]
fn apply_requires_approval_and_an_explicit_endpoint() -> TestResult {
    let words = [
        "mcp",
        "menu",
        "apply",
        "--backup",
        "missing.json",
        "--apply",
        "pm.PmName2",
        "BASE",
    ];
    let request = command(&words)?;
    assert!(
        request.validate_endpoint_selection(false).is_err(),
        "live apply must reject implicit endpoint selection"
    );
    request.validate_endpoint_selection(true)?;
    assert!(
        crate::mcp::run_offline(&request).is_none(),
        "apply must use the live workflow"
    );
    let missing = words
        .into_iter()
        .filter(|word| *word != "--apply")
        .collect::<Vec<_>>();
    assert!(
        command(&missing).is_err(),
        "omitting approval must fail argument parsing"
    );
    Ok(())
}

#[test]
fn grammar_preserves_spaces_empty_text_and_hyphens() -> TestResult {
    for value in [" Base Camp ", "", "-12", "--literal"] {
        let McpCommand::Menu(request) = command(&[
            "mcp",
            "menu",
            "preview",
            "--backup",
            "My Backup/report.json",
            "--output",
            "New Preview.json",
            "pm.PmName2",
            value,
        ])?
        else {
            return Err("wrong top-level command".into());
        };
        let MenuCommand::Preview(request) = request.command else {
            return Err("wrong menu command".into());
        };
        assert_eq!(
            request.value, value,
            "scalar arguments must not be split, trimmed, or lowercased"
        );
        assert_eq!(
            request.selection.backup,
            PathBuf::from("My Backup/report.json"),
            "backup path must retain its exact spelling"
        );
        assert_eq!(
            request.output,
            Some(PathBuf::from("New Preview.json")),
            "output path must retain spaces"
        );
    }
    assert!(
        command(&[
            "mcp",
            "menu",
            "show",
            "--backup",
            "missing",
            "--slot",
            "6",
            "radio.TxEqualizerFmNfm"
        ])
        .is_err(),
        "slot six must fail parsing"
    );
    Ok(())
}

#[test]
fn ordinary_admission_rejects_scope_lifecycle_and_unknown_fields_before_backup_io() -> TestResult {
    for (field, slot, value) in [
        ("pm.PmName2", Some(0), "BASE"),
        ("radio.TxEqualizerFmNfm", None, "on"),
        ("dv.DvGatewayModeDvGateway", Some(0), "0"),
        ("pm.PmSelect", None, "0"),
        ("missing.field", None, "secret-value"),
    ] {
        let request = ApplyRequest {
            selection: selection(Path::new("missing.json"), field, slot)?,
            apply: true,
            value: value.to_owned(),
            output: None,
        };
        let error = request
            .validate_options()
            .err()
            .ok_or("invalid assignment admitted")?;
        assert!(
            !error.to_string().contains("secret-value"),
            "errors must not echo unrecognized input values"
        );
    }
    Ok(())
}

#[test]
fn preview_and_apply_share_exact_text_and_complete_page_preservation() -> TestResult {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("backup.json");
    save(&path, &fixture()?)?;
    let original = std::fs::read(&path)?;
    let request = PreviewRequest {
        selection: selection(&path, "pm.PmName2", None)?,
        value: " Base Camp ".to_owned(),
        output: Some(directory.path().join("preview.json")),
    };
    let prepared = request.prepare()?;
    assert_eq!(
        prepared.changed_bytes, 15,
        "padding changes count, but the already-matching B byte does not"
    );
    assert_eq!(
        prepared.pages.len(),
        1,
        "the selected string lives in one complete page"
    );
    assert_eq!(
        prepared
            .pages
            .first()
            .ok_or("preview page missing")?
            .address,
        323_584,
        "global PM2 name must use the control page"
    );
    preview(&request)?;
    let saved = std::fs::read(request.output.as_ref().ok_or("preview output missing")?)?;
    let json: serde_json::Value = serde_json::from_slice(&saved)?;
    assert_eq!(
        json.get("radio_applied"),
        Some(&false.into()),
        "offline evidence must never claim application"
    );
    assert_eq!(
        json.pointer("/after/value"),
        Some(&" Base Camp ".into()),
        "preview must retain desired spaces"
    );
    assert!(
        preview(&request).is_err(),
        "a second preview must not overwrite existing evidence"
    );
    assert_eq!(
        std::fs::read(request.output.as_ref().ok_or("preview output missing")?)?,
        saved,
        "exclusive output must remain unchanged"
    );
    let apply = ApplyRequest {
        selection: selection(&path, "pm.PmName2", None)?,
        apply: true,
        value: request.value,
        output: None,
    };
    let plan = apply.prepare()?;
    assert_eq!(
        plan.replacements().len(),
        3,
        "the format and active Gateway guard pages must accompany the changed control page"
    );
    let target = plan
        .replacements()
        .iter()
        .find(|page| page.page().address().as_u32() == 323_584)
        .ok_or("target page missing")?;
    for (offset, (before, after)) in target
        .expected()
        .iter()
        .zip(target.replacement())
        .enumerate()
    {
        if !(26..42).contains(&offset) {
            assert_eq!(
                before, after,
                "unrelated byte {offset} must remain untouched"
            );
        }
    }
    assert_eq!(
        std::fs::read(&path)?,
        original,
        "neither preview nor planning may edit the backup"
    );
    Ok(())
}

#[test]
fn per_slot_preview_uses_the_selected_pm_stride_and_mask() -> TestResult {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("backup.json");
    save(&path, &fixture()?)?;
    let request = PreviewRequest {
        selection: selection(&path, "radio.TxEqualizerFmNfm", Some(5))?,
        value: "off".to_owned(),
        output: None,
    };
    let prepared = request.prepare()?;
    let page = prepared.pages.first().ok_or("preview page missing")?;
    let byte = page.bytes.first().ok_or("preview byte missing")?;
    assert_eq!(
        (
            page.address,
            byte.offset,
            byte.mask,
            byte.before,
            byte.after
        ),
        (0x05_A500, 0x25, 2, 0x42, 0x40),
        "slot stride and masked neighbor preservation must both be exact"
    );
    Ok(())
}

#[test]
fn apply_rejects_invalid_format_gateway_and_identity_without_output_creation() -> TestResult {
    for (address, offset, value) in [(8, 2, 1), (323_584, 9, 6), (331_776, 0, 2)] {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("backup.json");
        let mut document = fixture()?;
        *page_mut(&mut document, address)?
            .get_mut(offset)
            .ok_or("guard byte missing")? = value.into();
        save(&path, &document)?;
        let output = directory.path().join("never-created");
        let request = ApplyRequest {
            selection: selection(&path, "pm.PmName2", None)?,
            apply: true,
            value: "BASE".to_owned(),
            output: Some(output.clone()),
        };
        assert!(
            request.prepare().is_err(),
            "invalid captured guard {address}:{offset} must fail locally"
        );
        assert!(
            !output.exists(),
            "planning must not reserve any live evidence directory"
        );
    }
    Ok(())
}

#[test]
fn description_distinguishes_policy_storage_and_public_choices() -> TestResult {
    let ordinary = Description::new(resolve("PM.PMNAME2")?);
    assert_eq!(
        ordinary.field, "pm.PmName2",
        "lookup must return the canonical registry key"
    );
    assert_eq!(
        ordinary.write_policy, "ordinary",
        "known text storage is admitted to ordinary planning"
    );
    let lifecycle = Description::new(resolve("dv.DvGatewayModeDvGateway")?);
    assert!(
        lifecycle.write_policy.starts_with("lifecycle_required"),
        "Gateway editing requires a separate lifecycle"
    );
    let json = serde_json::to_value(lifecycle)?;
    assert!(
        json.get("enum_type").is_none(),
        "private enum aliases are not public keyboard choices"
    );
    assert!(
        resolve("invented.field").is_err(),
        "unknown fields must remain unknown"
    );
    Ok(())
}

#[test]
fn description_output_preserves_valid_json_without_prose_wrapping() -> TestResult {
    let mut bytes = Vec::new();
    describe("pm.PmName2", &mut bytes)?;
    let text = String::from_utf8(bytes)?;
    let actual: serde_json::Value = serde_json::from_str(&text)?;
    let expected = serde_json::to_value(Description::new(resolve("pm.PmName2")?))?;
    assert_eq!(
        actual, expected,
        "description output must round-trip every field without adding timestamps or embedded line breaks"
    );
    assert!(
        text.ends_with('\n'),
        "structured output must finish with a newline"
    );
    assert!(
        text.lines().any(|line| line.len() > 80),
        "long JSON strings must not be wrapped as prose"
    );
    Ok(())
}

#[cfg(unix)]
#[test]
fn preview_files_are_owner_only() -> TestResult {
    use std::os::unix::fs::PermissionsExt;
    let directory = tempfile::tempdir()?;
    let backup = directory.path().join("backup.json");
    save(&backup, &fixture()?)?;
    let path = directory.path().join("preview.json");
    preview(&PreviewRequest {
        selection: selection(&backup, "pm.PmName2", None)?,
        value: "BASE".to_owned(),
        output: Some(path.clone()),
    })?;
    assert_eq!(
        std::fs::metadata(path)?.permissions().mode() & 0o777,
        0o600,
        "preview values must not become group- or world-readable"
    );
    Ok(())
}
