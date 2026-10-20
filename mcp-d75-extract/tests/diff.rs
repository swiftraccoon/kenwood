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
    main_with_args,
};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn fixture_manifest() -> Result<Manifest, Box<dyn std::error::Error>> {
    Ok(build_manifest(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/thd75"),
        &BuildOptions::new(&THD75, "1.03", "1.03")?,
    )?)
}

/// A later release with one field moved, one removed and one added.
fn edited(base: &Manifest) -> Result<Manifest, Box<dyn std::error::Error>> {
    let mut edited = base.to_candidate();
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
    beat_shift.offset_hex = mcp_d75_extract::offset_hex(beat_shift.offset);
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
    for (sequence, operation) in radio.operations.iter_mut().enumerate() {
        operation.sequence = u64::try_from(sequence)?;
    }
    Ok(edited.validate()?)
}

#[test]
fn reports_added_removed_changed_fields_and_records() -> TestResult {
    let old = fixture_manifest()?;
    let new = edited(&old)?;
    let report = diff_manifests(&old, &new)?;
    let digest: String = old
        .source()
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
        format!(
            "summary: writable fields {} -> {}, enum options {} -> {}, combo mappings {} -> {}",
            old.summary().writable_registry_field_count,
            new.summary().writable_registry_field_count,
            old.summary().enum_option_count,
            new.summary().enum_option_count,
            old.summary().combo_option_mapping_count,
            new.summary().combo_option_mapping_count
        ),
    ];
    assert_eq!(report.lines, expected);
    assert_eq!(report.differences, 3);
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
    std::fs::write(&old_path, old.to_json()?)?;
    std::fs::write(&new_path, new.to_json()?)?;
    let old_text = old_path.to_str().ok_or("path is not UTF-8")?;
    let new_text = new_path.to_str().ok_or("path is not UTF-8")?;
    assert_eq!(main_with_args(["diff", old_text, old_text]), 0);
    assert_eq!(main_with_args(["diff", old_text, new_text]), 1);
    let other_radio =
        Manifest::from_json(include_str!("../../tmd750/data/mcp_d750_menu_schema.json"))?;
    let other_path = temporary.path().join("other.json");
    std::fs::write(&other_path, other_radio.to_json()?)?;
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

#[test]
fn reports_a_consistent_record_layout_change() -> TestResult {
    let old = fixture_manifest()?;
    let mut candidate = old.to_candidate();
    let record = candidate
        .menus
        .iter_mut()
        .flat_map(|menu| &mut menu.repeated_records)
        .find_map(|entry| match entry {
            RecordEntry::Extracted(record)
                if record.count > 1 && record.offset_layout.overrides.is_empty() =>
            {
                Some(record)
            }
            RecordEntry::Extracted(_) | RecordEntry::Unsupported(_) => None,
        })
        .ok_or("no linear repeated record")?;
    let old_count = record.count;
    record.count -= 1;
    let _last_base = record
        .record_base_offsets
        .pop()
        .ok_or("no last record base")?;
    let removed: Vec<_> = record
        .expanded_fields
        .iter()
        .filter(|field| field.record_index == record.count)
        .collect();
    let removed_count = u64::try_from(removed.len())?;
    let removed_writable = u64::try_from(
        removed
            .iter()
            .filter(|field| field.writable.unwrap_or(true))
            .count(),
    )?;
    record
        .expanded_fields
        .retain(|field| field.record_index < record.count);
    candidate.summary.expanded_record_field_count -= removed_count;
    candidate.summary.total_public_field_count -= removed_count;
    candidate.summary.writable_registry_field_count -= removed_writable;
    let new = candidate.validate()?;
    let report = diff_manifests(&old, &new)?;
    assert!(
        report
            .lines
            .iter()
            .any(|line| line.contains(&format!("count {old_count} -> {}", old_count - 1))),
        "{report}"
    );
    assert!(report.differences > usize::try_from(removed_count)?);
    Ok(())
}
