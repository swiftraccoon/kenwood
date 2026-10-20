//! Adversarial construction and decoding tests for the checked manifest boundary.

use clap as _;
use fancy_regex as _;
use regex as _;
use serde as _;
use sha2 as _;
use tempfile as _;
use thiserror as _;

use mcp_d75_extract::{Codec, Domain, Manifest, ManifestCandidate, Record, RecordEntry, Role};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn baseline() -> Result<ManifestCandidate, Box<dyn std::error::Error>> {
    Ok(
        Manifest::from_json(include_str!("../../thd75/data/mcp_d75_menu_schema.json"))?
            .into_candidate(),
    )
}

fn rejected(candidate: ManifestCandidate) -> TestResult {
    let json = serde_json::to_string(&candidate)?;
    assert!(
        candidate.validate().is_err(),
        "candidate validation accepted invalid input"
    );
    assert!(
        Manifest::from_json(&json).is_err(),
        "JSON entry point bypassed validation"
    );
    assert!(
        serde_json::from_str::<Manifest>(&json).is_err(),
        "Serde bypassed validation"
    );
    Ok(())
}

fn first_record(
    candidate: &mut ManifestCandidate,
) -> Result<&mut Record, Box<dyn std::error::Error>> {
    candidate
        .menus
        .iter_mut()
        .flat_map(|menu| &mut menu.repeated_records)
        .find_map(|entry| match entry {
            RecordEntry::Extracted(record) => Some(record),
            RecordEntry::Unsupported(_) => None,
        })
        .ok_or_else(|| "no extracted record in baseline".into())
}

#[test]
fn both_real_manifests_round_trip_canonical_bytes_with_lf_and_crlf() -> TestResult {
    for json in [
        include_str!("../../thd75/data/mcp_d75_menu_schema.json"),
        include_str!("../../tmd750/data/mcp_d750_menu_schema.json"),
    ] {
        // Git may check text out as CRLF; generated artifacts use canonical LF.
        let canonical_json = json.replace("\r\n", "\n");
        let crlf_json = canonical_json.replace('\n', "\r\n");
        for input in [&canonical_json, &crlf_json] {
            let manifest = Manifest::from_json(input)?;
            assert_eq!(manifest.to_json()?, canonical_json);
            assert_eq!(serde_json::from_str::<Manifest>(input)?, manifest);
            assert_eq!(manifest.to_candidate().validate()?, manifest);
        }
    }
    Ok(())
}

#[test]
fn model_source_maps_and_derived_counts_cannot_disagree() -> TestResult {
    let base = baseline()?;
    let mut candidate = base.clone();
    candidate.model.image_length += 1;
    rejected(candidate)?;
    let mut candidate = base.clone();
    candidate.model.radio = mcp_d75_extract::RadioModel::Tmd750;
    rejected(candidate)?;
    let mut candidate = base.clone();
    let entry = candidate
        .source
        .serializer_classes
        .first()
        .cloned()
        .ok_or("no source serializer")?;
    candidate.source.serializer_classes.push(entry);
    rejected(candidate)?;
    let mut candidate = base.clone();
    candidate
        .source
        .write_methods
        .first_mut()
        .ok_or("no source method")?
        .1
        .line += 1;
    rejected(candidate)?;
    let mut candidate = base.clone();
    candidate.source.normalized_source_sha256.clear();
    rejected(candidate)?;
    let mut candidate = base;
    candidate.summary.total_public_field_count += 1;
    rejected(candidate)?;
    let mut candidate = baseline()?;
    candidate.release.firmware_target.push('\n');
    rejected(candidate)?;
    Ok(())
}

#[test]
fn codec_geometry_domains_and_roles_are_checked_after_decoding() -> TestResult {
    let base = baseline()?;
    let mut candidate = base.clone();
    let menu = candidate.menus.first_mut().ok_or("no menu")?;
    let operation = menu
        .operations
        .iter_mut()
        .find(|operation| operation.role == Role::Field)
        .ok_or("no public field")?;
    operation.codec = Codec::BitField {
        bit: 7,
        width: 2,
        csharp_type: Some("byte".to_owned()),
        value_type: mcp_d75_extract::ValueType::Byte,
        value_expression: None,
        enum_type: None,
    };
    rejected(candidate)?;
    let mut candidate = base.clone();
    let menu = candidate.menus.first_mut().ok_or("no menu")?;
    let operation = menu
        .operations
        .iter_mut()
        .find(|operation| matches!(operation.codec, Codec::Bool { .. }))
        .ok_or("no boolean field")?;
    operation.domain = Some(Domain::Range {
        min: 0,
        max: 2,
        step: 1,
        provenance: "test".to_owned(),
    });
    rejected(candidate)?;
    let mut candidate = base.clone();
    let operation = candidate
        .menus
        .iter_mut()
        .flat_map(|menu| &mut menu.operations)
        .find(|operation| matches!(operation.codec, Codec::Bool { .. }))
        .ok_or("no boolean field")?;
    if let Codec::Bool { value_type, .. } = &mut operation.codec {
        *value_type = mcp_d75_extract::ValueType::String;
    }
    rejected(candidate)?;
    let mut candidate = base.clone();
    let operation = candidate
        .menus
        .first_mut()
        .and_then(|menu| menu.operations.first_mut())
        .ok_or("no operation")?;
    operation.role = Role::Field;
    rejected(candidate)?;
    let mut candidate = base.clone();
    let operation = candidate
        .menus
        .first_mut()
        .and_then(|menu| menu.operations.first_mut())
        .ok_or("no operation")?;
    operation.address.base = u64::MAX;
    operation.offset = u64::MAX;
    operation.offset_hex = mcp_d75_extract::offset_hex(u64::MAX);
    rejected(candidate)?;
    let mut candidate = base;
    let option = candidate
        .menus
        .iter_mut()
        .flat_map(|menu| &mut menu.enum_types)
        .flat_map(|catalog| &mut catalog.options)
        .next()
        .ok_or("no enum option")?;
    option.value = 256;
    rejected(candidate)?;
    Ok(())
}

#[test]
fn partial_record_growth_and_write_restrictions_cannot_be_forged() -> TestResult {
    let base = baseline()?;
    let mut candidate = base.clone();
    first_record(&mut candidate)?.count += 1;
    rejected(candidate)?;
    let mut candidate = base.clone();
    first_record(&mut candidate)?
        .expanded_fields
        .first_mut()
        .ok_or("no expanded field")?
        .offset += 1;
    rejected(candidate)?;
    let mut candidate = base;
    let field = first_record(&mut candidate)?
        .fields
        .iter_mut()
        .find(|field| field.role == Role::Field)
        .ok_or("no public record field")?;
    field.writable = Some(false);
    field.not_writable_reason = None;
    rejected(candidate)?;
    Ok(())
}
