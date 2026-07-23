//! Reviewed-layout tables: serializer roster, record specs, symbol overrides,
//! and audited value domains.
//!
//! These tables pin stable public names and audited domains that the
//! decompilation alone obscures; the extractor still derives and validates
//! every offset formula and write call against the sources.

use std::collections::HashMap;
use std::sync::LazyLock;

use serde_json::{Value, json};

/// Manifest format version emitted in the schema header.
pub(crate) const SCHEMA_VERSION: u64 = 3;

/// Generator identifier recorded in the manifest and the `@generated` header.
pub(crate) const GENERATOR: &str = "mcp-d75-extract";

/// The four top-level menu serializers: `(menu, public property name)`.
pub(crate) const SERIALIZERS: [(&str, &str); 4] = [
    ("radio", "RadioMenuData"),
    ("gps", "GpsMenuData"),
    ("aprs", "AprsMenuData"),
    ("dv", "DvMenuData"),
];

/// Reviewed direct-operation counts checked by `--strict-known-layout`.
pub(crate) const KNOWN_OPERATION_COUNTS: [(&str, u64); 4] =
    [("radio", 134), ("gps", 17), ("aprs", 85), ("dv", 31)];

/// Reviewed combo-backed enum type count checked by `--strict-known-layout`.
pub(crate) const KNOWN_COMBO_ENUM_TYPE_COUNT: usize = 87;

/// Reviewed combo option mapping count checked by `--strict-known-layout`.
pub(crate) const KNOWN_COMBO_OPTION_COUNT: usize = 655;

/// One public, statically sized repeated-record serializer to extract.
///
/// These are the public list records called by the four top-level
/// serializers. The extractor still derives and validates each child
/// method's offset formula and write calls; this spec supplies the stable
/// public collection name and the parent loop bound, which are otherwise
/// obscured by control-flow decompilation. A changed child shape fails
/// extraction rather than silently dropping fields.
#[derive(Debug, Clone)]
pub struct RecordSpec {
    /// Stable public collection name (for example `MyPositionList`).
    pub name: &'static str,
    /// Decompiled record class name.
    pub source_class: &'static str,
    /// Child writer method name.
    pub method: &'static str,
    /// Parent loop bound: number of records.
    pub count: usize,
}

/// Repeated-record specs per menu.
pub(crate) const REPEATED_RECORD_SPECS: [(&str, &[RecordSpec]); 3] = [
    (
        "gps",
        &[RecordSpec {
            name: "MyPositionList",
            source_class: "MyPositionData",
            method: "ax",
            count: 5,
        }],
    ),
    (
        "aprs",
        &[
            RecordSpec {
                name: "NavitraMessageList",
                source_class: "NavitraMessageData",
                method: "b",
                count: 5,
            },
            RecordSpec {
                name: "StatusTextList",
                source_class: "StatusTextData",
                method: "b",
                count: 5,
            },
            RecordSpec {
                name: "ObjectList",
                source_class: "ObjectData",
                method: "ax",
                count: 3,
            },
            RecordSpec {
                name: "UserPhraseList",
                source_class: "UserPhraseData",
                method: "b",
                count: 20,
            },
        ],
    ),
    (
        "dv",
        &[
            RecordSpec {
                name: "MyDvMessageList",
                source_class: "MyDvMessageData",
                method: "b",
                count: 5,
            },
            RecordSpec {
                name: "MyCallsignDvGatewayList",
                source_class: "MyCallsignDvGatewayData",
                method: "b",
                count: 6,
            },
        ],
    ),
];

/// Fill bytes for fixed strings whose stored form is not NUL-padded.
///
/// The string writers themselves NUL-fill the reserved span, but the
/// application normalizes some property values before serializing them. The
/// D-STAR MY callsign gateway list is stored space-padded on hardware: a
/// real TH-D75 dump holds an 8-character space-filled callsign there while
/// every other populated fixed string in the same dump is NUL-filled. Keyed
/// by `(source_class, field name)`; applies to repeated-record fields.
pub(crate) const FIXED_STRING_PADDING_OVERRIDES: [((&str, &str), u64); 1] =
    [(("MyCallsignDvGatewayData", "MyCallsignDvGateway"), 32)];

/// Look up a fixed-string padding override.
pub(crate) fn fixed_string_padding_override(source_class: &str, field: &str) -> Option<u64> {
    FIXED_STRING_PADDING_OVERRIDES
        .iter()
        .find(|((class_name, field_name), _)| *class_name == source_class && *field_name == field)
        .map(|(_, padding)| *padding)
}

/// The three radio nested records that expose no public menu properties.
pub(crate) fn unsupported_radio_records() -> Vec<Value> {
    vec![
        json!({
            "name": "private_a4_pair",
            "source_class": "m9.a4",
            "call_count": 2,
            "count": 2,
            "offset_layout": {"kind": "linear", "base": 848, "stride": 16},
            "unsupported_public_reason":
                "the serializer writes only private fields and exposes no public menu properties",
        }),
        json!({
            "name": "private_a5_blob",
            "source_class": "m9.a5",
            "call_count": 1,
            "count": 1,
            "offset_layout": {"kind": "fixed", "base": 880},
            "unsupported_public_reason":
                "the serializer writes private arrays (13 two-byte values and 42 raw bytes) with no public menu properties",
        }),
    ]
}

/// Build an inclusive integer range domain with provenance.
pub(crate) fn range_domain(minimum: i64, maximum: i64, provenance: &str) -> Value {
    json!({
        "kind": "range",
        "min": minimum,
        "max": maximum,
        "step": 1,
        "provenance": provenance,
    })
}

/// Build an explicit allowed-values domain with provenance.
pub(crate) fn choices_domain(values: &[i64], provenance: &str) -> Value {
    json!({
        "kind": "choices",
        "allowed_values": values,
        "provenance": provenance,
    })
}

fn position_storage_transform() -> Value {
    json!({
        "kind": "scaled_integer",
        "input_unit": "seconds",
        "encode": "round(value * 10000 / 60)",
        "decode": "round(stored * 60 / 10000, 1)",
        "numerator": 10000,
        "denominator": 60,
    })
}

/// Decompiled storage symbols used by the position record writers.
///
/// The two encoded coordinate fields are not degrees: the model maps seconds
/// to the signed integer with `round(seconds * 10000 / 60)`. Keep that
/// transform explicit so generated clients cannot mistake the stored value
/// for a display unit. `base.c` is an internal record marker, not a public
/// property.
fn position_symbols() -> Vec<(&'static str, Value)> {
    vec![
        (
            "base.c",
            json!({"name": "RecordType", "csharp_type": "byte", "role": "internal"}),
        ),
        ("e", json!({"name": "Altitude", "csharp_type": "int"})),
        (
            "base.g",
            json!({"name": "NorthSouth", "csharp_type": "byte"}),
        ),
        (
            "j",
            json!({"name": "LatitudeDegree", "csharp_type": "byte"}),
        ),
        (
            "base.j",
            json!({"name": "LatitudeDegree", "csharp_type": "byte"}),
        ),
        (
            "m",
            json!({"name": "LatitudeMinute", "csharp_type": "byte"}),
        ),
        (
            "base.m",
            json!({"name": "LatitudeMinute", "csharp_type": "byte"}),
        ),
        (
            "p",
            json!({
                "name": "LatitudeSecondEncoded",
                "csharp_type": "int",
                "aliases": ["LatitudeSecond", "LatitudeMin"],
                "storage_transform": position_storage_transform(),
            }),
        ),
        ("s", json!({"name": "EastWest", "csharp_type": "byte"})),
        (
            "v",
            json!({"name": "LongitudeDegree", "csharp_type": "byte"}),
        ),
        (
            "y",
            json!({"name": "LongitudeMinute", "csharp_type": "byte"}),
        ),
        (
            "ab",
            json!({
                "name": "LongitudeSecondEncoded",
                "csharp_type": "int",
                "aliases": ["LongitudeSecond", "LongitudeMin"],
                "storage_transform": position_storage_transform(),
            }),
        ),
        ("base.e", json!({"name": "Name", "csharp_type": "string"})),
    ]
}

/// Per-record-class symbol overrides: `source_class -> symbol -> override`.
pub(crate) static RECORD_SYMBOLS: LazyLock<HashMap<&'static str, HashMap<&'static str, Value>>> =
    LazyLock::new(|| {
        let mut my_position: HashMap<&'static str, Value> =
            position_symbols().into_iter().collect();
        drop(my_position.insert(
            "f",
            json!({"name": "MyPositionChannel", "csharp_type": "byte"}),
        ));
        let object_data: HashMap<&'static str, Value> = position_symbols()
            .into_iter()
            .filter(|(key, _)| *key != "base.c")
            .collect();
        HashMap::from([("MyPositionData", my_position), ("ObjectData", object_data)])
    });

/// Audited domains for direct top-level fields, keyed by `menu.Name`.
pub(crate) static DIRECT_FIELD_DOMAINS: LazyLock<HashMap<String, Value>> = LazyLock::new(|| {
    let mut domains = HashMap::new();
    for band in ["04", "08", "16", "32"] {
        drop(domains.insert(
            format!("radio.TxEqLevel{band}"),
            range_domain(0, 12, "ui_numeric"),
        ));
    }
    for band in ["04", "08", "16", "32", "64"] {
        drop(domains.insert(
            format!("radio.RxEqLevel{band}"),
            range_domain(0, 18, "ui_numeric"),
        ));
    }
    for index in 0..30 {
        let mut values: Vec<i64> = (0..30).collect();
        values.push(255);
        drop(domains.insert(
            format!("radio.GroupLink{index}"),
            choices_domain(&values, "ui_choices"),
        ));
    }
    let mut time_zone: Vec<i64> = (0..53).collect();
    time_zone.extend(56..113);
    time_zone.extend(201..204);
    let mut icon_table: Vec<i64> = (47..58).collect();
    icon_table.extend(65..91);
    let entries: [(&str, Value); 21] = [
        ("radio.CwPitchFreq", range_domain(0, 6, "ui_choices")),
        ("radio.AutoMuteRetTime", range_domain(1, 10, "ui_numeric")),
        ("radio.TimeZone", choices_domain(&time_zone, "ui_choices")),
        ("gps.MyPositionSelect", range_domain(0, 5, "ui_choices")),
        ("gps.Interval", range_domain(2, 1800, "ui_numeric")),
        ("gps.Distance", range_domain(1, 999, "ui_numeric_scaled")),
        // Both distance controls expose raw zero as the official "Off"
        // display value; raw 1..=250 represents 10..=2500 distance units.
        (
            "aprs.QsyLimit",
            range_domain(0, 250, "ui_numeric_scaled_with_off"),
        ),
        (
            "aprs.FilterPositionLimit",
            range_domain(0, 250, "ui_numeric_scaled_with_off"),
        ),
        ("aprs.LowSpeedSpeed", range_domain(2, 30, "ui_numeric")),
        ("aprs.HiSpeedSpeed", range_domain(2, 90, "ui_numeric")),
        ("aprs.SlowRateTime", range_domain(1, 100, "ui_numeric")),
        ("aprs.FastRateTime", range_domain(10, 180, "ui_numeric")),
        ("aprs.TurnAngleDeg", range_domain(5, 90, "ui_numeric")),
        ("aprs.TurnSlopeDegSpeed", range_domain(1, 255, "ui_numeric")),
        ("aprs.TurnTimeTime", range_domain(5, 180, "ui_numeric")),
        ("aprs.UIcheck", range_domain(0, 250, "ui_numeric")),
        ("aprs.StatusTextSelect", range_domain(0, 4, "ui_choices")),
        (
            "aprs.NavitraMessageSelect",
            range_domain(0, 4, "ui_choices"),
        ),
        ("aprs.ObjectUsedNo", range_domain(0, 2, "ui_choices")),
        ("aprs.IconSymbol", range_domain(33, 126, "model_validation")),
        ("aprs.IconNavitra", range_domain(0, 14, "ui_choices")),
    ];
    for (key, value) in entries {
        drop(domains.insert(key.to_owned(), value));
    }
    drop(domains.insert(
        "aprs.IconTableOverlay".to_owned(),
        choices_domain(&icon_table, "ui_choices"),
    ));
    drop(domains.insert(
        "dv.MyCallsignSelectDvGateway".to_owned(),
        range_domain(0, 5, "ui_choices"),
    ));
    drop(domains.insert(
        "dv.MyDvMessageSelect".to_owned(),
        range_domain(0, 5, "ui_choices"),
    ));
    domains
});

/// Audited domains for repeated-record fields, keyed by `(class, field)`.
pub(crate) static RECORD_FIELD_DOMAINS: LazyLock<HashMap<(&'static str, &'static str), Value>> =
    LazyLock::new(|| {
        let mut icon_table: Vec<i64> = (47..58).collect();
        icon_table.extend(65..91);
        HashMap::from([
            (
                ("MyPositionData", "Altitude"),
                range_domain(-500, 15000, "model_validation"),
            ),
            (
                ("MyPositionData", "LatitudeDegree"),
                range_domain(0, 89, "model_validation"),
            ),
            (
                ("MyPositionData", "LatitudeMinute"),
                range_domain(0, 59, "model_validation"),
            ),
            (
                ("MyPositionData", "LatitudeSecondEncoded"),
                range_domain(0, 9999, "model_validation"),
            ),
            (
                ("MyPositionData", "LongitudeDegree"),
                range_domain(0, 179, "model_validation"),
            ),
            (
                ("MyPositionData", "LongitudeMinute"),
                range_domain(0, 59, "model_validation"),
            ),
            (
                ("MyPositionData", "LongitudeSecondEncoded"),
                range_domain(0, 9999, "model_validation"),
            ),
            (
                ("ObjectData", "LatitudeDegree"),
                range_domain(0, 89, "model_validation"),
            ),
            (
                ("ObjectData", "LatitudeMinute"),
                range_domain(0, 59, "model_validation"),
            ),
            (
                ("ObjectData", "LatitudeSecondEncoded"),
                range_domain(0, 9999, "model_validation"),
            ),
            (
                ("ObjectData", "LongitudeDegree"),
                range_domain(0, 179, "model_validation"),
            ),
            (
                ("ObjectData", "LongitudeMinute"),
                range_domain(0, 59, "model_validation"),
            ),
            (
                ("ObjectData", "LongitudeSecondEncoded"),
                range_domain(0, 9999, "model_validation"),
            ),
            (
                ("ObjectData", "ObjectTable"),
                choices_domain(&icon_table, "ui_choices"),
            ),
            (
                ("ObjectData", "ObjectSymbol"),
                range_domain(33, 126, "model_validation"),
            ),
        ])
    });
