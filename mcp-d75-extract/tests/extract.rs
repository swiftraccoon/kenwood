//! Integration tests for the MCP-D75 menu-schema extractor.
//!
//! These mirror the reference extractor's unittest suite: fixture-driven
//! whole-schema assertions plus inline C# sources that pin the
//! repeated-record layout formulas.

// Each integration test is a separate compilation unit that sees every
// package dependency; only the ones below are exercised directly.
use clap as _;
use fancy_regex as _;
use regex as _;
use sha2 as _;
use thiserror as _;

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use mcp_d75_extract::{
    BuildOptions, RecordSpec, build_schema, extract_repeated_record, json_text, main_with_args,
    rust_text, write_or_check,
};
use serde_json::{Value, json};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn fixture_schema() -> Result<Value, Box<dyn std::error::Error>> {
    Ok(build_schema(&fixtures_dir(), &BuildOptions::default())?)
}

fn get<'a>(value: &'a Value, key: &str) -> Result<&'a Value, Box<dyn std::error::Error>> {
    value
        .get(key)
        .ok_or_else(|| format!("missing key {key}").into())
}

fn get_str<'a>(value: &'a Value, key: &str) -> Result<&'a str, Box<dyn std::error::Error>> {
    get(value, key)?
        .as_str()
        .ok_or_else(|| format!("key {key} is not a string").into())
}

fn menus(schema: &Value) -> Result<&Vec<Value>, Box<dyn std::error::Error>> {
    get(schema, "menus")?
        .as_array()
        .ok_or_else(|| "menus is not an array".into())
}

fn named_operations(schema: &Value) -> Result<HashMap<String, Value>, Box<dyn std::error::Error>> {
    let mut operations = HashMap::new();
    for menu in menus(schema)? {
        for operation in get(menu, "operations")?
            .as_array()
            .ok_or("operations is not an array")?
        {
            if let Some(name) = operation.get("name").and_then(Value::as_str) {
                let _previous = operations.insert(name.to_owned(), operation.clone());
            }
        }
    }
    Ok(operations)
}

fn radio_enum_options(schema: &Value) -> Result<Vec<Value>, Box<dyn std::error::Error>> {
    let radio = menus(schema)?.first().ok_or("no menus extracted")?;
    let catalog = get(radio, "enum_types")?
        .as_array()
        .ok_or("enum_types is not an array")?
        .iter()
        .find(|item| item.get("name").and_then(Value::as_str) == Some("m9.a"))
        .ok_or("enum catalog m9.a not found")?;
    Ok(get(catalog, "options")?
        .as_array()
        .ok_or("options is not an array")?
        .clone())
}

#[test]
fn discovers_public_menu_class_mapping() -> TestResult {
    let schema = fixture_schema()?;
    assert_eq!(
        get(get(&schema, "source")?, "serializer_classes")?,
        &json!({"radio": "m9", "gps": "m1", "aprs": "l4", "dv": "mu"}),
        "serializer class discovery diverged"
    );
    Ok(())
}

#[test]
fn extracts_supported_codecs_and_resolves_lengths() -> TestResult {
    let schema = fixture_schema()?;
    let operations = named_operations(&schema)?;
    let operation = |name: &str| -> Result<&Value, Box<dyn std::error::Error>> {
        operations
            .get(name)
            .ok_or_else(|| format!("operation {name} missing").into())
    };
    let beat_shift = operation("BeatShift")?;
    assert_eq!(
        get_str(beat_shift, "offset_hex")?,
        "0x1000",
        "BeatShift offset"
    );
    assert_eq!(
        get_str(get(beat_shift, "codec")?, "value_type")?,
        "enum",
        "BeatShift value type"
    );
    assert_eq!(
        get_str(get(beat_shift, "codec")?, "enum_type")?,
        "m9.a",
        "BeatShift enum type"
    );
    assert_eq!(
        get_str(get(operation("TxInhibit")?, "codec")?, "kind")?,
        "bool",
        "TxInhibit codec kind"
    );
    assert_eq!(
        get(operation("LedControl_Receive")?, "codec")?,
        &json!({
            "kind": "bit_field",
            "bit": 0,
            "width": 1,
            "csharp_type": "bool",
            "value_type": "bool",
        }),
        "LedControl_Receive codec"
    );
    let power_on_message = get(operation("PowerOnMessage")?, "codec")?;
    assert_eq!(
        get(power_on_message, "length")?,
        &json!(16),
        "PowerOnMessage length"
    );
    assert_eq!(
        get_str(power_on_message, "encoding")?,
        "memory_map",
        "PowerOnMessage encoding"
    );
    let poweron_bitmap = operation("PoweronBitmap")?;
    assert_eq!(
        get(get(poweron_bitmap, "codec")?, "length")?,
        &json!(86400),
        "PoweronBitmap inferred length"
    );
    assert_eq!(
        get_str(poweron_bitmap, "category")?,
        "blob",
        "PoweronBitmap category"
    );
    assert_eq!(
        get_str(get(operation("Interval")?, "codec")?, "kind")?,
        "unsigned_le",
        "Interval codec kind"
    );
    assert_eq!(
        get_str(get(operation("MyCallsign")?, "codec")?, "encoding")?,
        "utf8",
        "MyCallsign encoding"
    );
    assert_eq!(
        get_str(get(operation("Sequence")?, "codec")?, "kind")?,
        "unsigned_le",
        "Sequence codec kind"
    );
    Ok(())
}

#[test]
fn attaches_raw_enum_domain_and_resource_keys() -> TestResult {
    let schema = fixture_schema()?;
    let options = radio_enum_options(&schema)?;
    assert_eq!(
        Value::Array(options),
        json!([
            {
                "value": 0,
                "member": "a",
                "resource_key": "Edit_Menu_TestBeatShift_Off",
            },
            {
                "value": 4,
                "member": "b",
                "resource_key": "Edit_Menu_TestBeatShift_Numbered",
            },
        ]),
        "m9.a enum options"
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
    let schema = build_schema(
        &fixtures_dir(),
        &BuildOptions {
            strict_known_layout: false,
            language_file: Some(language_file),
        },
    )?;
    let options = radio_enum_options(&schema)?;
    let labels: Vec<Option<&str>> = options
        .iter()
        .map(|option| option.get("label").and_then(Value::as_str))
        .collect();
    assert_eq!(
        labels,
        vec![Some("Off"), Some("Choice 1")],
        "resolved labels"
    );
    assert_eq!(
        get_str(get(get(&schema, "source")?, "language_file")?, "file_name")?,
        "English.lng",
        "language file provenance"
    );
    Ok(())
}

#[test]
fn records_constants_clears_and_nested_serializers() -> TestResult {
    let schema = fixture_schema()?;
    let radio = menus(&schema)?.first().ok_or("no menus extracted")?;
    let roles: Vec<&str> = get(radio, "operations")?
        .as_array()
        .ok_or("operations is not an array")?
        .iter()
        .filter_map(|operation| operation.get("role").and_then(Value::as_str))
        .collect();
    assert!(
        roles.contains(&"constant"),
        "expected a constant role, got {roles:?}"
    );
    assert!(
        roles.contains(&"clear"),
        "expected a clear role, got {roles:?}"
    );
    assert_eq!(
        get(radio, "nested_serializers")?,
        &json!([{"target": "this.child", "method": "b", "index_expression": "0"}]),
        "nested serializer calls"
    );
    Ok(())
}

#[test]
fn output_is_deterministic_and_checkable() -> TestResult {
    let first = json_text(&fixture_schema()?)?;
    let second = json_text(&fixture_schema()?)?;
    assert_eq!(first, second, "JSON output must be deterministic");
    let temporary = tempfile::tempdir()?;
    let output = temporary.path().join("schema.json");
    write_or_check(&output, &first, false)?;
    write_or_check(&output, &second, true)?;
    Ok(())
}

#[test]
fn rust_output_is_deterministic_and_checked_by_cli() -> TestResult {
    let schema = fixture_schema()?;
    let first = rust_text(&schema)?;
    let second = rust_text(&fixture_schema()?)?;
    assert_eq!(first, second, "Rust output must be deterministic");
    let expected_fields = get(get(&schema, "summary")?, "writable_registry_field_count")?
        .as_u64()
        .ok_or("writable_registry_field_count is not an integer")?;
    let rendered = u64::try_from(first.matches("    MenuField {").count())?;
    assert_eq!(rendered, expected_fields, "rendered field count");
    assert!(
        first.contains("FieldDescriptor::new(\n            \"radio.BeatShift\","),
        "missing BeatShift descriptor"
    );
    assert!(
        first.contains("FieldCodec::BitBool {"),
        "missing BitBool codec"
    );
    assert!(
        first.contains("resource_key: Some(\"Edit_Menu_TestBeatShift_Off\")"),
        "missing resource key"
    );
    assert!(first.contains("member: \"b\","), "missing enum member");

    let temporary = tempfile::tempdir()?;
    let output = temporary.path().join("schema.json");
    let rust_output = temporary.path().join("menu_fields.rs");
    let fixtures = fixtures_dir();
    let arguments = [
        "--source-dir",
        fixtures.to_str().ok_or("fixtures path is not UTF-8")?,
        "--output",
        output.to_str().ok_or("output path is not UTF-8")?,
        "--rust-output",
        rust_output
            .to_str()
            .ok_or("rust output path is not UTF-8")?,
    ];
    assert_eq!(main_with_args(arguments), 0, "CLI generation run failed");
    assert_eq!(
        std::fs::read_to_string(&rust_output)?,
        first,
        "CLI wrote a different registry"
    );
    let check_arguments: Vec<&str> = arguments.iter().copied().chain(["--check"]).collect();
    assert_eq!(
        main_with_args(check_arguments.clone()),
        0,
        "clean --check failed"
    );
    std::fs::write(&rust_output, format!("{first}// stale\n"))?;
    assert_eq!(
        main_with_args(check_arguments),
        1,
        "stale --check must fail"
    );
    Ok(())
}

#[test]
fn strict_known_layout_rejects_fixture() -> TestResult {
    let result = build_schema(
        &fixtures_dir(),
        &BuildOptions {
            strict_known_layout: true,
            language_file: None,
        },
    );
    let error = match result {
        Err(error) => error.to_string(),
        Ok(_) => return Err("strict known layout unexpectedly accepted the fixture".into()),
    };
    assert!(
        error.contains("operation counts changed"),
        "unexpected strict-layout error: {error}"
    );
    Ok(())
}

#[test]
fn extracts_piecewise_repeated_record_and_expands_indices() -> TestResult {
    let source = r"
public class StatusTextData
{
    public enum a : byte { a, b, c, d, e, f, g, h, i }
    public string StatusText
    {
        get { return string.Empty; }
    }
    public a TxRate
    {
        get { return a.a; }
    }
    public void b(m6 A_0, int A_1)
    {
        int num = ((A_1 == 4) ? 4864 : (4656 + 48 * A_1));
        A_0.d(StatusText, num, nb.u);
        A_0.a((byte)TxRate, num + 42);
    }
}
";
    let record = extract_repeated_record(
        &RecordSpec {
            name: "StatusTextList",
            source_class: "StatusTextData",
            method: "b",
            count: 5,
        },
        Path::new("StatusTextData.cs"),
        source,
        Path::new(""),
        &HashMap::from([("nb.u".to_owned(), 42_i64)]),
    )?;
    assert_eq!(
        get(&record, "record_base_offsets")?,
        &json!([4656, 4704, 4752, 4800, 4864]),
        "piecewise base offsets"
    );
    assert_eq!(
        get(get(&record, "offset_layout")?, "overrides")?,
        &json!({"4": 4864}),
        "offset layout override"
    );
    let expanded = get(&record, "expanded_fields")?
        .as_array()
        .ok_or("expanded_fields is not an array")?;
    assert_eq!(expanded.len(), 10, "expanded field count");
    let last = expanded.last().ok_or("expanded_fields is empty")?;
    assert_eq!(
        get_str(last, "name")?,
        "StatusTextList[4].TxRate",
        "last expanded name"
    );
    assert_eq!(get(last, "offset")?, &json!(4906), "last expanded offset");
    Ok(())
}

#[test]
fn coordinate_record_keeps_encoded_storage_transform() -> TestResult {
    let source = r"
public class MyPositionData
{
    public int Altitude { get { return e; } }
    public byte MyPositionChannel { get { return f; } }
    public override void ax(m6 A_0, int A_1)
    {
        int num = 4384 + 32 * A_1;
        A_0.a(base.c, num + 12);
        A_0.b(e, 4, num);
        A_0.a(base.g, 2, num + 12);
        A_0.a(j, num + 4);
        A_0.a(m, num + 5);
        A_0.b(p, 2, num + 6);
        A_0.a(s, 3, num + 12);
        A_0.a(v, num + 8);
        A_0.a(y, num + 9);
        A_0.b(ab, 2, num + 10);
        A_0.a(f, num + 13);
        A_0.c(base.e, num + 14, nb.aa);
    }
}
";
    let record = extract_repeated_record(
        &RecordSpec {
            name: "MyPositionList",
            source_class: "MyPositionData",
            method: "ax",
            count: 5,
        },
        Path::new("MyPositionData.cs"),
        source,
        Path::new(""),
        &HashMap::from([("nb.aa".to_owned(), 8_i64)]),
    )?;
    assert_eq!(
        get(&record, "operation_count_per_record")?,
        &json!(12),
        "operation count"
    );
    assert_eq!(
        get(&record, "field_count_per_record")?,
        &json!(11),
        "field count"
    );
    let expanded = get(&record, "expanded_fields")?
        .as_array()
        .ok_or("expanded_fields is not an array")?;
    assert_eq!(expanded.len(), 55, "expanded field count");
    let encoded = expanded
        .iter()
        .find(|field| {
            field.get("name").and_then(Value::as_str)
                == Some("MyPositionList[0].LatitudeSecondEncoded")
        })
        .ok_or("encoded latitude field missing")?;
    assert_eq!(
        get_str(get(encoded, "codec")?, "kind")?,
        "unsigned_le",
        "encoded codec kind"
    );
    let transform = get(encoded, "storage_transform")?;
    assert_eq!(
        get(transform, "numerator")?,
        &json!(10000),
        "transform numerator"
    );
    assert_eq!(
        get(transform, "denominator")?,
        &json!(60),
        "transform denominator"
    );
    Ok(())
}

#[test]
fn gateway_callsign_list_uses_space_padding() -> TestResult {
    let source = r"
public class MyCallsignDvGatewayData
{
    public string MyCallsignDvGateway
    {
        get { return string.Empty; }
    }
    public string MemoDvGateway
    {
        get { return string.Empty; }
    }
    public void b(m6 A_0, int A_1)
    {
        int num = 7336 + 12 * A_1;
        A_0.d(MyCallsignDvGateway, num, 8);
        A_0.d(MemoDvGateway, num + 8, 4);
    }
}
";
    let record = extract_repeated_record(
        &RecordSpec {
            name: "MyCallsignDvGatewayList",
            source_class: "MyCallsignDvGatewayData",
            method: "b",
            count: 6,
        },
        Path::new("MyCallsignDvGatewayData.cs"),
        source,
        Path::new(""),
        &HashMap::new(),
    )?;
    let mut paddings: HashMap<String, i64> = HashMap::new();
    for field in get(&record, "expanded_fields")?
        .as_array()
        .ok_or("expanded_fields is not an array")?
    {
        let name = get_str(field, "name")?.to_owned();
        let padding = get(get(field, "codec")?, "padding")?
            .as_i64()
            .ok_or("padding is not an integer")?;
        let _previous = paddings.insert(name, padding);
    }
    // The MY-callsign list is stored space-padded on hardware; the memo
    // beside it keeps the writers' default NUL fill.
    assert_eq!(
        paddings.get("MyCallsignDvGatewayList[0].MyCallsignDvGateway"),
        Some(&32),
        "first gateway callsign padding"
    );
    assert_eq!(
        paddings.get("MyCallsignDvGatewayList[5].MyCallsignDvGateway"),
        Some(&32),
        "last gateway callsign padding"
    );
    assert_eq!(
        paddings.get("MyCallsignDvGatewayList[0].MemoDvGateway"),
        Some(&0),
        "memo padding"
    );
    Ok(())
}
