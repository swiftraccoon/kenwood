//! Fixture-driven tests for the TM-D750 model: per-slot addressing.

use clap as _;
use fancy_regex as _;
use regex as _;
use serde as _;
use serde_json as _;
use sha2 as _;
use tempfile as _;
use thiserror as _;

use std::path::{Path, PathBuf};

use mcp_d75_extract::{
    BuildOptions, Codec, Domain, Manifest, Menu, Operation, RecordEntry, TMD750, Term,
    build_manifest, json_text, parse_manifest, rust_text,
};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/tmd750")
}

fn options(strict: bool) -> BuildOptions {
    BuildOptions {
        model: &TMD750,
        mcp_version: "1.00".to_owned(),
        firmware_target: "1.00".to_owned(),
        language_file: None,
        strict_known_layout: strict,
    }
}

fn manifest() -> Result<Manifest, Box<dyn std::error::Error>> {
    Ok(build_manifest(&fixtures_dir(), &options(false))?)
}

fn menu<'a>(manifest: &'a Manifest, key: &str) -> Result<&'a Menu, Box<dyn std::error::Error>> {
    manifest
        .menus
        .iter()
        .find(|menu| menu.menu == key)
        .ok_or_else(|| format!("menu {key} missing").into())
}

fn operation<'a>(menu: &'a Menu, name: &str) -> Result<&'a Operation, Box<dyn std::error::Error>> {
    menu.operations
        .iter()
        .find(|op| op.name.as_deref() == Some(name))
        .ok_or_else(|| format!("{name} missing").into())
}

fn slot(stride: u64) -> Term {
    Term {
        dimension: "pm_slot".to_owned(),
        stride,
    }
}

fn record_names(menu: &Menu) -> Vec<&str> {
    menu.repeated_records
        .iter()
        .map(|entry| match entry {
            RecordEntry::Extracted(record) => record.name.as_str(),
            RecordEntry::Unsupported(record) => record.name.as_str(),
        })
        .collect()
}

#[test]
fn declares_the_slot_dimension_and_discovers_details() -> TestResult {
    let manifest = manifest()?;
    assert_eq!(manifest.model.image_length, 1_929_472);
    assert_eq!(manifest.release.assembly_version, "1.0.9699.22639");
    assert_eq!(manifest.dimensions.len(), 1);
    assert_eq!(
        manifest
            .dimensions
            .first()
            .map(|dimension| dimension.anchors.len()),
        Some(2)
    );
    assert_eq!(manifest.source.writer_class, "n7");
    assert_eq!(manifest.source.resource_class, "ky");
    let keys: Vec<&str> = manifest
        .source
        .serializer_classes
        .iter()
        .map(|(key, _)| key.as_str())
        .collect();
    assert_eq!(keys, vec!["radio", "gps", "aprs", "dv", "ipnet", "pm"]);
    assert_eq!(
        manifest.source.detail_classes,
        vec![
            ("radio".to_owned(), "nl".to_owned()),
            ("gps".to_owned(), "m7".to_owned()),
            ("aprs".to_owned(), "ms".to_owned()),
            ("dv".to_owned(), "m5".to_owned()),
            ("ipnet".to_owned(), "nb".to_owned()),
            ("pm".to_owned(), "nj".to_owned()),
        ]
    );
    let radio = menu(&manifest, "radio")?;
    assert_eq!(radio.write_method, "a6(n7 A_0)");
    assert_eq!(radio.detail_write_method.as_deref(), Some("a6(n7 A_0)"));
    Ok(())
}

#[test]
fn addresses_carry_slot_terms_with_their_anchor_stride() -> TestResult {
    let manifest = manifest()?;
    let radio = menu(&manifest, "radio")?;
    let repeater = operation(radio, "RepeaterMode")?;
    assert!(repeater.address.is_absolute());
    assert_eq!(repeater.writer_class, "oa");
    let meter = operation(radio, "MeterType")?;
    assert_eq!(meter.offset, 328_995);
    assert_eq!(meter.address.terms, vec![slot(8192)]);
    assert_eq!(meter.writer_class, "nl");
    let bitmap = operation(radio, "PoweronBitmap")?;
    assert_eq!(bitmap.address.terms, vec![slot(256_000)]);
    assert!(
        matches!(
            bitmap.codec,
            Codec::RawBytes {
                length: Some(86_400),
                ..
            }
        ),
        "{bitmap:?}"
    );
    assert_eq!(bitmap.category.as_deref(), Some("blob"));
    let eq = operation(radio, "TxEqLevel04")?;
    assert!(eq.domain.is_some());
    assert!(manifest.summary.slot_relative_field_count > 0);
    assert_eq!(manifest.summary.dimension_count, 1);
    Ok(())
}

#[test]
fn enums_declared_in_the_serializer_resolve_from_the_detail_class() -> TestResult {
    let manifest = manifest()?;
    let radio = menu(&manifest, "radio")?;
    let backlight = operation(radio, "InfoBacklight")?;
    assert_eq!(backlight.writer_class, "nl");
    assert_eq!(backlight.codec.csharp_type(), Some("oa.ad"));
    assert_eq!(backlight.codec.value_type(), Some("enum"));
    assert_eq!(backlight.codec.enum_type(), Some("oa.ad"));
    let catalog = radio
        .enum_types
        .iter()
        .find(|catalog| catalog.name == "oa.ad")
        .ok_or("oa.ad catalog missing")?;
    assert_eq!(catalog.csharp_name, "ad");
    assert_eq!(catalog.underlying_type, "byte");
    assert_eq!(catalog.options.len(), 2);
    Ok(())
}

#[test]
fn records_inherit_the_slot_term_and_fold_pinned_bases() -> TestResult {
    let manifest = manifest()?;
    let gps = menu(&manifest, "gps")?;
    let Some(RecordEntry::Extracted(position)) = gps.repeated_records.first() else {
        return Err("MyPositionList missing".into());
    };
    assert_eq!(position.write_method, "a3(n7 A_0, int A_1)");
    assert_eq!(position.offset_layout.base, 329_232);
    assert_eq!(position.offset_layout.terms, vec![slot(8192)]);
    assert_eq!(
        position.record_base_offsets,
        vec![329_232, 329_264, 329_296, 329_328, 329_360]
    );
    let channel = position
        .expanded_fields
        .iter()
        .find(|field| field.name == "MyPositionList[4].MyPositionChannel")
        .ok_or("channel missing")?;
    assert_eq!(channel.writable, Some(false));
    assert_eq!(channel.address.terms, vec![slot(8192)]);
    let aprs = menu(&manifest, "aprs")?;
    assert_eq!(
        record_names(aprs),
        vec!["StatusTextList", "ObjectList", "UserPhraseList"]
    );
    let Some(RecordEntry::Extracted(status)) = aprs.repeated_records.first() else {
        return Err("StatusTextList missing".into());
    };
    assert_eq!(status.offset_layout.kind, "linear_with_override");
    assert_eq!(
        status.record_base_offsets,
        vec![329_504, 329_552, 329_600, 329_648, 329_728]
    );
    let dv = menu(&manifest, "dv")?;
    assert_eq!(
        record_names(dv),
        vec![
            "MyDvMessageList",
            "MyCallsignDvGatewayList",
            "ReflectorHostsUrlList",
            "AutoUpdateUrlList"
        ]
    );
    let Some(RecordEntry::Extracted(reflector)) = dv.repeated_records.get(2) else {
        return Err("ReflectorHostsUrlList missing".into());
    };
    assert_eq!(reflector.offset_layout.base, 334_081);
    assert_eq!(reflector.offset_layout.stride, Some(96));
    assert_eq!(reflector.offset_layout.terms, vec![slot(8192)]);
    let Some(RecordEntry::Extracted(gateway)) = dv.repeated_records.get(1) else {
        return Err("gateway list missing".into());
    };
    assert!(matches!(
        gateway.fields.first().map(|field| &field.codec),
        Some(Codec::FixedString { padding: 32, .. })
    ));
    Ok(())
}

#[test]
fn object_records_name_inherited_position_symbols() -> TestResult {
    let manifest = manifest()?;
    let aprs = menu(&manifest, "aprs")?;
    let Some(RecordEntry::Extracted(objects)) = aprs.repeated_records.get(1) else {
        return Err("ObjectList missing".into());
    };
    assert_eq!(objects.source_class, "ObjectData");
    assert_eq!(objects.record_base_offsets, vec![331_264, 331_328, 331_392]);
    let names: Vec<Option<&str>> = objects
        .fields
        .iter()
        .map(|field| field.name.as_deref())
        .collect();
    assert_eq!(
        names,
        vec![
            None,
            Some("NorthSouth"),
            Some("LatitudeDegree"),
            Some("LatitudeMinute"),
            Some("LatitudeSecondEncoded"),
            Some("EastWest"),
            Some("LongitudeDegree"),
            Some("LongitudeMinute"),
            Some("LongitudeSecondEncoded"),
            Some("Name"),
            Some("ObjectTxFormat"),
            Some("ObjectTxInterval"),
            Some("ObjectTable"),
            Some("ObjectSymbol"),
            Some("ObjectComment"),
        ]
    );
    let latitude = objects.fields.get(4).ok_or("latitude field missing")?;
    assert!(latitude.storage_transform.is_some());
    assert!(
        matches!(latitude.domain, Some(Domain::Range { .. })),
        "{latitude:?}"
    );
    let format = objects.fields.get(10).ok_or("format field missing")?;
    assert_eq!(format.codec.enum_type(), Some("ObjectData.a"));
    let name = objects.fields.get(9).ok_or("name field missing")?;
    assert!(
        matches!(name.codec, Codec::FixedString { length: 9, .. }),
        "{name:?}"
    );
    assert!(
        aprs.enum_types
            .iter()
            .any(|catalog| catalog.name == "ObjectData.b" && catalog.options.len() == 3)
    );
    Ok(())
}

#[test]
fn private_writers_are_cataloged_with_slot_terms() -> TestResult {
    let manifest = manifest()?;
    let radio = menu(&manifest, "radio")?;
    let privates: Vec<(&str, &str, u64, Vec<Term>)> = radio
        .repeated_records
        .iter()
        .filter_map(|entry| match entry {
            RecordEntry::Unsupported(record) => Some((
                record.name.as_str(),
                record.source_class.as_str(),
                record.offset_layout.base,
                record.offset_layout.terms.clone(),
            )),
            RecordEntry::Extracted(_) => None,
        })
        .collect();
    assert_eq!(
        privates,
        vec![
            ("private_pair_848", "oa.bd", 848, Vec::new()),
            ("private_blob_880", "oa.be", 880, Vec::new()),
            (
                "private_slot_pair_328016",
                "nl.a",
                328_016,
                vec![slot(8192)]
            ),
            (
                "private_slot_blob_328048",
                "nl.b",
                328_048,
                vec![slot(8192)]
            ),
            (
                "private_slot_blob_332810",
                "nl.c",
                332_810,
                vec![slot(8192)]
            ),
        ]
    );
    Ok(())
}

#[test]
fn ipv4_helper_values_use_the_pinned_codec() -> TestResult {
    let manifest = manifest()?;
    let ipnet = menu(&manifest, "ipnet")?;
    let ip = operation(ipnet, "IpAddress")?;
    assert!(
        matches!(&ip.codec, Codec::RawBytes { length: Some(4), encoding: Some(encoding), .. } if encoding == "ipv4_dotted_quad"),
        "{ip:?}"
    );
    assert_eq!(ip.address.terms, vec![slot(8192)]);
    Ok(())
}

#[test]
fn manifest_round_trips_and_rustgen_refuses_slot_terms() -> TestResult {
    let manifest = manifest()?;
    let json = json_text(&manifest)?;
    assert_eq!(parse_manifest(&json)?, manifest);
    let refused = rust_text(&manifest);
    assert!(
        refused
            .as_ref()
            .is_err_and(|error| error.to_string().contains("thd75")),
        "{refused:?}"
    );
    Ok(())
}

#[test]
fn strict_known_layout_rejects_fixture() -> TestResult {
    let result = build_manifest(&fixtures_dir(), &options(true));
    let Err(error) = result else {
        return Err("strict known layout unexpectedly accepted the fixture".into());
    };
    let message = error.to_string();
    assert!(
        message.contains("operation counts changed") || message.contains("was not found in menu"),
        "unexpected strict-layout error: {message}"
    );
    Ok(())
}
