//! Golden diff report between the thd75 fixture manifest and an edited copy.

use clap as _;
use fancy_regex as _;
use regex as _;
use serde as _;
use serde_json as _;
use sha2 as _;
use thiserror as _;

use std::path::{Path, PathBuf};

use mcp_d75_extract::{
    BuildOptions, Manifest, Operation, RecordEntry, THD75, build_manifest, diff_manifests,
    json_text, main_with_args,
};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn fixture_manifest() -> Result<Manifest, Box<dyn std::error::Error>> {
    Ok(build_manifest(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/thd75"),
        &BuildOptions {
            model: &THD75,
            mcp_version: "1.03".to_owned(),
            firmware_target: "1.03".to_owned(),
            language_file: None,
            strict_known_layout: false,
        },
    )?)
}

/// A later release: one field moved, one removed, one added, one record grown.
fn edited(base: &Manifest) -> Result<Manifest, Box<dyn std::error::Error>> {
    let mut edited = base.clone();
    "1.10".clone_into(&mut edited.release.mcp_version);
    "1.0.9000.1".clone_into(&mut edited.release.assembly_version);
    "1.10".clone_into(&mut edited.release.firmware_target);
    let radio = edited
        .menus
        .iter_mut()
        .find(|menu| menu.menu == "radio")
        .ok_or("radio missing")?;
    let beat_shift = radio
        .operations
        .iter_mut()
        .find(|op| op.name.as_deref() == Some("BeatShift"))
        .ok_or("BeatShift missing")?;
    beat_shift.offset += 2;
    beat_shift.address.base += 2;
    let template: Operation = radio
        .operations
        .iter()
        .find(|op| op.name.as_deref() == Some("TxInhibit"))
        .cloned()
        .ok_or("TxInhibit missing")?;
    radio
        .operations
        .retain(|op| op.name.as_deref() != Some("TxInhibit"));
    let mut added = template;
    added.name = Some("NewField".to_owned());
    added.offset = 0x1234;
    "0x1234".clone_into(&mut added.offset_hex);
    added.address.base = 0x1234;
    radio.operations.push(added);
    let gps = edited
        .menus
        .iter_mut()
        .find(|menu| menu.menu == "gps")
        .ok_or("gps missing")?;
    if let Some(RecordEntry::Extracted(record)) = gps.repeated_records.first_mut() {
        record.count = 6;
    }
    Ok(edited)
}

#[test]
fn reports_added_removed_changed_fields_and_records() -> TestResult {
    let old = fixture_manifest()?;
    let new = edited(&old)?;
    let report = diff_manifests(&old, &new)?;
    let digest: String = old
        .source
        .normalized_source_sha256
        .chars()
        .take(12)
        .collect();
    let expected = [
        "mcp-d75-extract diff".to_owned(),
        "radio: thd75".to_owned(),
        format!("old: MCP 1.03, assembly 1.0.8717.20367, firmware 1.03, source {digest}"),
        format!("new: MCP 1.10, assembly 1.0.9000.1, firmware 1.10, source {digest}"),
        "image_length: 500480 -> 500480".to_owned(),
        "dimensions: unchanged (none)".to_owned(),
        String::new(),
        "menu radio: 3 changes".to_owned(),
        "  + radio.NewField 0x1234 bool".to_owned(),
        "  - radio.TxInhibit 0x1001 bool".to_owned(),
        "  ~ radio.BeatShift address 0x1000 -> 0x1002".to_owned(),
        String::new(),
        "menu gps: 1 change".to_owned(),
        "  ~ record MyPositionList count 5 -> 6".to_owned(),
        String::new(),
        format!(
            "summary: writable fields {} -> {}, enum options {} -> {}, combo mappings {} -> {}",
            old.summary.writable_registry_field_count,
            new.summary.writable_registry_field_count,
            old.summary.enum_option_count,
            new.summary.enum_option_count,
            old.summary.combo_option_mapping_count,
            new.summary.combo_option_mapping_count
        ),
    ];
    assert_eq!(report.lines, expected);
    assert_eq!(report.differences, 4);
    assert!(report.to_string().ends_with('\n'));
    let same = diff_manifests(&old, &old)?;
    assert_eq!(same.differences, 0);
    assert!(same.lines.contains(&"no differences".to_owned()));
    Ok(())
}

#[test]
fn cli_exit_codes_follow_the_report() -> TestResult {
    let old = fixture_manifest()?;
    let new = edited(&old)?;
    let temporary = tempfile::tempdir()?;
    let old_path: PathBuf = temporary.path().join("old.json");
    let new_path: PathBuf = temporary.path().join("new.json");
    std::fs::write(&old_path, json_text(&old)?)?;
    std::fs::write(&new_path, json_text(&new)?)?;
    let old_text = old_path.to_str().ok_or("path is not UTF-8")?;
    let new_text = new_path.to_str().ok_or("path is not UTF-8")?;
    assert_eq!(main_with_args(["diff", old_text, old_text]), 0);
    assert_eq!(main_with_args(["diff", old_text, new_text]), 1);
    let mut other_radio = old;
    other_radio.model.radio = "tmd750".to_owned();
    let other_path = temporary.path().join("other.json");
    std::fs::write(&other_path, json_text(&other_radio)?)?;
    assert_eq!(
        main_with_args([
            "diff",
            old_text,
            other_path.to_str().ok_or("path is not UTF-8")?
        ]),
        2
    );
    assert_eq!(
        main_with_args(["diff", old_text, "/nonexistent/manifest.json"]),
        2
    );
    Ok(())
}
