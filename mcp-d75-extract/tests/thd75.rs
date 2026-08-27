//! Fixture-driven tests for the TH-D75 model.

use clap as _;
use fancy_regex as _;
use regex as _;
use serde as _;
use serde_json as _;
use sha2 as _;
use thiserror as _;

use std::path::{Path, PathBuf};

use mcp_d75_extract::{
    BuildOptions, Codec, Manifest, Menu, Operation, RecordEntry, Role, SCHEMA_VERSION, THD75,
    build_manifest, json_text, main_with_args, parse_manifest, write_or_check,
};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/thd75")
}

fn options() -> BuildOptions {
    BuildOptions {
        model: &THD75,
        mcp_version: "1.03".to_owned(),
        firmware_target: "1.03".to_owned(),
        language_file: None,
        strict_known_layout: false,
    }
}

fn fixture_manifest() -> Result<Manifest, Box<dyn std::error::Error>> {
    Ok(build_manifest(&fixtures_dir(), &options())?)
}

/// Rewrite text with CRLF line endings, whatever endings it has now (a
/// checkout that already converted to CRLF must not become CR CR LF).
fn to_crlf(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\n', "\r\n")
}

/// Copy a fixture tree, rewriting every C# file with CRLF line endings.
fn copy_with_crlf(from: &Path, to: &Path) -> TestResult {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_with_crlf(&entry.path(), &target)?;
        } else {
            let text = std::fs::read_to_string(entry.path())?;
            std::fs::write(&target, to_crlf(&text))?;
        }
    }
    Ok(())
}

#[test]
fn crlf_sources_extract_identically() -> TestResult {
    assert_eq!(to_crlf("a\r\nb\nc"), "a\r\nb\r\nc");
    let temporary = tempfile::tempdir()?;
    copy_with_crlf(&fixtures_dir(), temporary.path())?;
    let sample = std::fs::read(temporary.path().join("m9.cs"))?;
    assert!(
        sample.windows(2).any(|pair| pair == b"\r\n"),
        "the CRLF copy must actually contain CRLF"
    );
    let crlf = build_manifest(temporary.path(), &options())?;
    assert_eq!(crlf, fixture_manifest()?);
    Ok(())
}

fn menu<'a>(manifest: &'a Manifest, key: &str) -> Result<&'a Menu, Box<dyn std::error::Error>> {
    manifest
        .menus
        .iter()
        .find(|menu| menu.menu == key)
        .ok_or_else(|| format!("menu {key} missing").into())
}

fn operation<'a>(
    manifest: &'a Manifest,
    name: &str,
) -> Result<&'a Operation, Box<dyn std::error::Error>> {
    manifest
        .menus
        .iter()
        .flat_map(|menu| menu.operations.iter())
        .find(|operation| operation.name.as_deref() == Some(name))
        .ok_or_else(|| format!("operation {name} missing").into())
}

#[test]
fn discovers_anchors_without_pinned_names() -> TestResult {
    let manifest = fixture_manifest()?;
    assert_eq!(
        manifest.source.serializer_classes,
        vec![
            ("radio".to_owned(), "m9".to_owned()),
            ("gps".to_owned(), "m1".to_owned()),
            ("aprs".to_owned(), "l4".to_owned()),
            ("dv".to_owned(), "mu".to_owned()),
        ]
    );
    assert_eq!(manifest.source.writer_class, "m6");
    assert_eq!(manifest.source.resource_class, "kb");
    assert_eq!(
        manifest
            .source
            .write_methods
            .first()
            .map(|(_, method)| method.method.as_str()),
        Some("a0")
    );
    assert!(manifest.source.detail_classes.is_empty());
    Ok(())
}

#[test]
fn manifest_is_a_v4_superset_with_absolute_addresses() -> TestResult {
    let manifest = fixture_manifest()?;
    assert_eq!(manifest.schema_version, SCHEMA_VERSION);
    assert_eq!(manifest.generator, "mcp-d75-extract");
    assert_eq!(manifest.model.radio, "thd75");
    assert_eq!(manifest.model.image_length, 500_480);
    assert_eq!(manifest.release.assembly_version, "1.0.8717.20367");
    assert_eq!(manifest.release.mcp_version, "1.03");
    assert!(manifest.dimensions.is_empty());
    assert_eq!(manifest.summary.dimension_count, 0);
    assert_eq!(manifest.summary.slot_relative_field_count, 0);
    for operation in manifest
        .menus
        .iter()
        .flat_map(|menu| menu.operations.iter())
    {
        assert!(operation.address.is_absolute(), "{operation:?}");
        assert_eq!(operation.offset, operation.address.base);
        assert_eq!(operation.offset_hex, format!("0x{:04X}", operation.offset));
    }
    let radio = menu(&manifest, "radio")?;
    assert_eq!(radio.write_method, "a0(m6 A_0)");
    assert!(radio.detail_class.is_none());
    let json = json_text(&manifest)?;
    assert_eq!(parse_manifest(&json)?, manifest, "manifest must round-trip");
    Ok(())
}

#[test]
fn extracts_supported_codecs_and_resolves_lengths() -> TestResult {
    let manifest = fixture_manifest()?;
    let beat_shift = operation(&manifest, "BeatShift")?;
    assert_eq!(beat_shift.offset_hex, "0x1000");
    assert_eq!(beat_shift.codec.value_type(), Some("enum"));
    assert_eq!(beat_shift.codec.enum_type(), Some("m9.a"));
    assert_eq!(operation(&manifest, "TxInhibit")?.codec.kind(), "bool");
    assert_eq!(
        operation(&manifest, "LedControl_Receive")?.codec,
        Codec::BitField {
            bit: 0,
            width: 1,
            csharp_type: Some("bool".to_owned()),
            value_type: "bool".to_owned(),
            value_expression: None,
            enum_type: None,
        }
    );
    assert!(matches!(
        operation(&manifest, "PowerOnMessage")?.codec,
        Codec::FixedString { length: 16, ref encoding, .. } if encoding == "memory_map"
    ));
    let poweron = operation(&manifest, "PoweronBitmap")?;
    assert!(
        matches!(
            poweron.codec,
            Codec::RawBytes {
                length: Some(86_400),
                ..
            }
        ),
        "{poweron:?}"
    );
    assert_eq!(poweron.category.as_deref(), Some("blob"));
    assert_eq!(
        operation(&manifest, "Interval")?.codec.kind(),
        "unsigned_le"
    );
    assert!(matches!(
        operation(&manifest, "MyCallsign")?.codec,
        Codec::FixedString { ref encoding, .. } if encoding == "utf8"
    ));
    assert_eq!(
        operation(&manifest, "Sequence")?.codec.kind(),
        "unsigned_le"
    );
    Ok(())
}

#[test]
fn attaches_raw_enum_domain_and_resource_keys() -> TestResult {
    let manifest = fixture_manifest()?;
    let radio = menu(&manifest, "radio")?;
    let catalog = radio
        .enum_types
        .iter()
        .find(|catalog| catalog.name == "m9.a")
        .ok_or("m9.a missing")?;
    let members: Vec<(i64, &str, Option<&str>)> = catalog
        .options
        .iter()
        .map(|option| {
            (
                option.value,
                option.member.as_str(),
                option.resource_key.as_deref(),
            )
        })
        .collect();
    assert_eq!(
        members,
        vec![
            (0, "a", Some("Edit_Menu_TestBeatShift_Off")),
            (4, "b", Some("Edit_Menu_TestBeatShift_Numbered"))
        ]
    );
    Ok(())
}

#[test]
fn resolves_utf16_english_labels_and_formats_arguments() -> TestResult {
    let language_text =
        "[Edit_Menu_Test]\r\nBeatShift_Off = Off\r\nBeatShift_Numbered = Choice {0}\r\n";
    let temporary = tempfile::tempdir()?;
    let language_file = temporary.path().join("English.lng");
    let mut encoded: Vec<u8> = vec![0xFF, 0xFE];
    for unit in language_text.encode_utf16() {
        encoded.extend_from_slice(&unit.to_le_bytes());
    }
    std::fs::write(&language_file, encoded)?;
    let manifest = build_manifest(
        &fixtures_dir(),
        &BuildOptions {
            language_file: Some(language_file),
            ..options()
        },
    )?;
    let radio = menu(&manifest, "radio")?;
    let catalog = radio
        .enum_types
        .iter()
        .find(|catalog| catalog.name == "m9.a")
        .ok_or("m9.a missing")?;
    let labels: Vec<Option<&str>> = catalog
        .options
        .iter()
        .map(|option| option.label.as_deref())
        .collect();
    assert_eq!(labels, vec![Some("Off"), Some("Choice 1")]);
    assert_eq!(
        manifest
            .source
            .language_file
            .as_ref()
            .map(|info| info.file_name.as_str()),
        Some("English.lng")
    );
    Ok(())
}

#[test]
fn records_constants_clears_nested_calls_and_private_catalog() -> TestResult {
    let manifest = fixture_manifest()?;
    let radio = menu(&manifest, "radio")?;
    let roles: Vec<Role> = radio
        .operations
        .iter()
        .map(|operation| operation.role)
        .collect();
    assert!(roles.contains(&Role::Constant), "{roles:?}");
    assert!(roles.contains(&Role::Clear), "{roles:?}");
    let nested: Vec<(&str, &str, Option<&str>)> = radio
        .nested_serializers
        .iter()
        .map(|call| {
            (
                call.target.as_str(),
                call.method.as_str(),
                call.index_expression.as_deref(),
            )
        })
        .collect();
    assert_eq!(
        nested,
        vec![
            ("this.m_a", "b", Some("0")),
            ("this.m_b", "b", Some("1")),
            ("this.m_c", "b", None)
        ]
    );
    let names: Vec<&str> = radio
        .repeated_records
        .iter()
        .map(|entry| match entry {
            RecordEntry::Unsupported(record) => record.name.as_str(),
            RecordEntry::Extracted(record) => record.name.as_str(),
        })
        .collect();
    assert_eq!(names, vec!["private_pair_848", "private_blob_880"]);
    if let Some(RecordEntry::Unsupported(pair)) = radio.repeated_records.first() {
        assert_eq!(pair.source_class, "m9.a4");
        assert_eq!(pair.offset_layout.stride, Some(16));
        assert_eq!(pair.call_count, 2);
    }
    Ok(())
}

#[test]
fn extracts_the_position_record_list() -> TestResult {
    let manifest = fixture_manifest()?;
    let gps = menu(&manifest, "gps")?;
    let Some(RecordEntry::Extracted(record)) = gps.repeated_records.first() else {
        return Err("MyPositionList missing".into());
    };
    assert_eq!(record.name, "MyPositionList");
    assert_eq!(record.write_method, "ax(m6 A_0, int A_1)");
    assert_eq!(
        record.record_base_offsets,
        vec![4384, 4416, 4448, 4480, 4512]
    );
    assert_eq!(record.expanded_fields.len(), 55);
    let encoded = record
        .expanded_fields
        .iter()
        .find(|field| field.name == "MyPositionList[0].LatitudeSecondEncoded")
        .ok_or("encoded latitude field missing")?;
    assert_eq!(
        encoded
            .storage_transform
            .as_ref()
            .map(|transform| transform.denominator),
        Some(60)
    );
    assert!(encoded.address.is_absolute());
    assert_eq!(manifest.summary.expanded_record_field_count, 55);
    assert_eq!(manifest.summary.repeated_record_type_count, 1);
    assert_eq!(manifest.summary.unsupported_public_record_type_count, 2);
    assert_eq!(manifest.summary.nested_serializer_call_count, 4);
    Ok(())
}

#[test]
fn output_is_deterministic_and_checkable() -> TestResult {
    let first = json_text(&fixture_manifest()?)?;
    let second = json_text(&fixture_manifest()?)?;
    assert_eq!(first, second);
    let temporary = tempfile::tempdir()?;
    let output = temporary.path().join("schema.json");
    write_or_check(&output, &first, false)?;
    write_or_check(&output, &second, true)?;
    Ok(())
}

#[test]
fn cli_extracts_checks_and_rejects_stale_output() -> TestResult {
    let temporary = tempfile::tempdir()?;
    let output = temporary.path().join("schema.json");
    let fixtures = fixtures_dir();
    let arguments = [
        "extract",
        "--model",
        "thd75",
        "--source-dir",
        fixtures.to_str().ok_or("fixtures path is not UTF-8")?,
        "--mcp-version",
        "1.03",
        "--firmware",
        "1.03",
        "--output",
        output.to_str().ok_or("output path is not UTF-8")?,
    ];
    assert_eq!(main_with_args(arguments), 0, "CLI generation run failed");
    let check_arguments: Vec<&str> = arguments.iter().copied().chain(["--check"]).collect();
    assert_eq!(
        main_with_args(check_arguments.clone()),
        0,
        "clean --check failed"
    );
    std::fs::write(&output, "{}\n")?;
    assert_eq!(
        main_with_args(check_arguments),
        1,
        "stale --check must fail"
    );
    assert_ne!(
        main_with_args([
            "extract",
            "--model",
            "tm-d710",
            "--source-dir",
            ".",
            "--mcp-version",
            "1",
            "--firmware",
            "1",
            "--output",
            "x"
        ]),
        0
    );
    Ok(())
}

#[test]
fn strict_known_layout_rejects_fixture() -> TestResult {
    let result = build_manifest(
        &fixtures_dir(),
        &BuildOptions {
            strict_known_layout: true,
            ..options()
        },
    );
    let Err(error) = result else {
        return Err("strict known layout unexpectedly accepted the fixture".into());
    };
    let message = error.to_string();
    assert!(
        message.contains("was not found in menu") || message.contains("operation counts changed"),
        "unexpected strict-layout error: {message}"
    );
    Ok(())
}

#[test]
fn rust_registry_is_deterministic_and_checked_by_cli() -> TestResult {
    let manifest = fixture_manifest()?;
    let first = mcp_d75_extract::rust_text(&manifest)?;
    assert_eq!(first, mcp_d75_extract::rust_text(&fixture_manifest()?)?);
    let rendered = u64::try_from(first.matches("    MenuField {").count())?;
    assert_eq!(rendered, manifest.summary.writable_registry_field_count);
    assert!(first.contains("pub const MCP_D75_SCHEMA_VERSION: u32 = 4;"));
    assert!(first.contains("FieldDescriptor::new(\n            \"radio.BeatShift\","));
    assert!(first.contains("FieldCodec::BitBool {"));
    assert!(first.contains("resource_key: Some(\"Edit_Menu_TestBeatShift_Off\")"));
    assert!(first.contains("MyPositionList[4].LongitudeSecondEncoded"));
    let temporary = tempfile::tempdir()?;
    let output = temporary.path().join("schema.json");
    let rust_output = temporary.path().join("menu_fields.rs");
    let fixtures = fixtures_dir();
    let arguments = [
        "extract",
        "--model",
        "thd75",
        "--source-dir",
        fixtures.to_str().ok_or("fixtures path is not UTF-8")?,
        "--mcp-version",
        "1.03",
        "--firmware",
        "1.03",
        "--output",
        output.to_str().ok_or("output path is not UTF-8")?,
        "--rust-output",
        rust_output
            .to_str()
            .ok_or("rust output path is not UTF-8")?,
    ];
    assert_eq!(main_with_args(arguments), 0);
    assert_eq!(std::fs::read_to_string(&rust_output)?, first);
    let check_arguments: Vec<&str> = arguments.iter().copied().chain(["--check"]).collect();
    assert_eq!(main_with_args(check_arguments.clone()), 0);
    std::fs::write(&rust_output, format!("{first}// stale\n"))?;
    assert_eq!(main_with_args(check_arguments), 1);
    Ok(())
}
