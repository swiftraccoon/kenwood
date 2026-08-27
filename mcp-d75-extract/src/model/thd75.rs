//! TH-D75 spec: the reviewed MCP-D75 layout facts.

use super::{
    BlobSpec, MenuSpec, ModelSpec, PaddingOverride, PrivateWriterSpec, RecordSpec, ReviewedLayout,
    StorageTransformSpec, SymbolOverride, choices_domain, range_domain,
};
use crate::manifest::Domain;

const POSITION_TRANSFORM: StorageTransformSpec = StorageTransformSpec {
    input_unit: "seconds",
    encode: "round(value * 10000 / 60)",
    decode: "round(stored * 60 / 10000, 1)",
    numerator: 10000,
    denominator: 60,
};

const fn plain(
    symbol: &'static str,
    name: &'static str,
    csharp_type: &'static str,
) -> SymbolOverride {
    SymbolOverride {
        symbol,
        name,
        csharp_type,
        role: None,
        aliases: &[],
        storage_transform: None,
        not_writable_reason: None,
    }
}

/// Symbols shared by `MyPositionData` and `ObjectData`, minus the record marker.
const POSITION_SYMBOLS: [SymbolOverride; 11] = [
    plain("e", "Altitude", "int"),
    plain("base.g", "NorthSouth", "byte"),
    plain("j", "LatitudeDegree", "byte"),
    plain("base.j", "LatitudeDegree", "byte"),
    plain("m", "LatitudeMinute", "byte"),
    plain("base.m", "LatitudeMinute", "byte"),
    SymbolOverride {
        symbol: "p",
        name: "LatitudeSecondEncoded",
        csharp_type: "int",
        role: None,
        aliases: &["LatitudeSecond", "LatitudeMin"],
        storage_transform: Some(POSITION_TRANSFORM),
        not_writable_reason: None,
    },
    plain("s", "EastWest", "byte"),
    plain("v", "LongitudeDegree", "byte"),
    plain("y", "LongitudeMinute", "byte"),
    SymbolOverride {
        symbol: "ab",
        name: "LongitudeSecondEncoded",
        csharp_type: "int",
        role: None,
        aliases: &["LongitudeSecond", "LongitudeMin"],
        storage_transform: Some(POSITION_TRANSFORM),
        not_writable_reason: None,
    },
];

const MY_POSITION_SYMBOLS: [SymbolOverride; 14] = [
    SymbolOverride {
        symbol: "base.c",
        name: "RecordType",
        csharp_type: "byte",
        role: Some("internal"),
        aliases: &[],
        storage_transform: None,
        not_writable_reason: None,
    },
    POSITION_SYMBOLS[0],
    POSITION_SYMBOLS[1],
    POSITION_SYMBOLS[2],
    POSITION_SYMBOLS[3],
    POSITION_SYMBOLS[4],
    POSITION_SYMBOLS[5],
    POSITION_SYMBOLS[6],
    POSITION_SYMBOLS[7],
    POSITION_SYMBOLS[8],
    POSITION_SYMBOLS[9],
    POSITION_SYMBOLS[10],
    plain("base.e", "Name", "string"),
    SymbolOverride {
        symbol: "f",
        name: "MyPositionChannel",
        csharp_type: "byte",
        role: None,
        aliases: &[],
        storage_transform: None,
        not_writable_reason: Some(
            "public storage-width byte has no verified MCP-D75 UI/domain semantics",
        ),
    },
];

const OBJECT_SYMBOLS: [SymbolOverride; 12] = [
    POSITION_SYMBOLS[0],
    POSITION_SYMBOLS[1],
    POSITION_SYMBOLS[2],
    POSITION_SYMBOLS[3],
    POSITION_SYMBOLS[4],
    POSITION_SYMBOLS[5],
    POSITION_SYMBOLS[6],
    POSITION_SYMBOLS[7],
    POSITION_SYMBOLS[8],
    POSITION_SYMBOLS[9],
    POSITION_SYMBOLS[10],
    plain("base.e", "Name", "string"),
];

fn icon_table() -> Vec<i64> {
    (47..58).chain(65..91).collect()
}

fn direct_domains() -> Vec<(String, Domain)> {
    let mut domains = Vec::new();
    for band in ["04", "08", "16", "32"] {
        domains.push((
            format!("radio.TxEqLevel{band}"),
            range_domain(0, 12, "ui_numeric"),
        ));
    }
    for band in ["04", "08", "16", "32", "64"] {
        domains.push((
            format!("radio.RxEqLevel{band}"),
            range_domain(0, 18, "ui_numeric"),
        ));
    }
    for index in 0..30 {
        let mut values: Vec<i64> = (0..30).collect();
        values.push(255);
        domains.push((
            format!("radio.GroupLink{index}"),
            choices_domain(values, "ui_choices"),
        ));
    }
    let time_zone: Vec<i64> = (0..53).chain(56..113).chain(201..204).collect();
    let entries: [(&str, Domain); 23] = [
        ("radio.CwPitchFreq", range_domain(0, 6, "ui_choices")),
        ("radio.AutoMuteRetTime", range_domain(1, 10, "ui_numeric")),
        ("radio.TimeZone", choices_domain(time_zone, "ui_choices")),
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
        (
            "aprs.IconTableOverlay",
            choices_domain(icon_table(), "ui_choices"),
        ),
        (
            "dv.MyCallsignSelectDvGateway",
            range_domain(0, 5, "ui_choices"),
        ),
    ];
    for (key, domain) in entries {
        domains.push((key.to_owned(), domain));
    }
    domains.push((
        "dv.MyDvMessageSelect".to_owned(),
        range_domain(0, 5, "ui_choices"),
    ));
    domains
}

fn coordinate_domains(class: &'static str) -> Vec<((&'static str, &'static str), Domain)> {
    vec![
        (
            (class, "LatitudeDegree"),
            range_domain(0, 89, "model_validation"),
        ),
        (
            (class, "LatitudeMinute"),
            range_domain(0, 59, "model_validation"),
        ),
        (
            (class, "LatitudeSecondEncoded"),
            range_domain(0, 9999, "model_validation"),
        ),
        (
            (class, "LongitudeDegree"),
            range_domain(0, 179, "model_validation"),
        ),
        (
            (class, "LongitudeMinute"),
            range_domain(0, 59, "model_validation"),
        ),
        (
            (class, "LongitudeSecondEncoded"),
            range_domain(0, 9999, "model_validation"),
        ),
    ]
}

fn record_domains() -> Vec<((&'static str, &'static str), Domain)> {
    let mut domains = vec![(
        ("MyPositionData", "Altitude"),
        range_domain(-500, 15000, "model_validation"),
    )];
    domains.extend(coordinate_domains("MyPositionData"));
    domains.extend(coordinate_domains("ObjectData"));
    domains.push((
        ("ObjectData", "ObjectTable"),
        choices_domain(icon_table(), "ui_choices"),
    ));
    domains.push((
        ("ObjectData", "ObjectSymbol"),
        range_domain(33, 126, "model_validation"),
    ));
    domains
}

/// The TH-D75 spec.
pub static THD75: ModelSpec = ModelSpec {
    id: "thd75",
    product: "TH-D75",
    mcp_product: "MCP-D75 Memory Control Program",
    image_length: 500_480,
    menus: &[
        MenuSpec {
            key: "radio",
            property: "RadioMenuData",
        },
        MenuSpec {
            key: "gps",
            property: "GpsMenuData",
        },
        MenuSpec {
            key: "aprs",
            property: "AprsMenuData",
        },
        MenuSpec {
            key: "dv",
            property: "DvMenuData",
        },
    ],
    dimensions: &[],
    records: &[
        RecordSpec {
            menu: "gps",
            list: "MyPositionList",
            count: 5,
            base_override: None,
        },
        RecordSpec {
            menu: "aprs",
            list: "NavitraMessageList",
            count: 5,
            base_override: None,
        },
        RecordSpec {
            menu: "aprs",
            list: "StatusTextList",
            count: 5,
            base_override: None,
        },
        RecordSpec {
            menu: "aprs",
            list: "ObjectList",
            count: 3,
            base_override: None,
        },
        RecordSpec {
            menu: "aprs",
            list: "UserPhraseList",
            count: 20,
            base_override: None,
        },
        RecordSpec {
            menu: "dv",
            list: "MyDvMessageList",
            count: 5,
            base_override: None,
        },
        RecordSpec {
            menu: "dv",
            list: "MyCallsignDvGatewayList",
            count: 6,
            base_override: None,
        },
    ],
    private_writers: &[
        PrivateWriterSpec {
            menu: "radio",
            name: "private_pair_848",
            base: 848,
            stride: Some(16),
            calls: 2,
            count: 2,
            reason: "the serializer writes only private fields and exposes no public menu properties",
        },
        PrivateWriterSpec {
            menu: "radio",
            name: "private_blob_880",
            base: 880,
            stride: None,
            calls: 1,
            count: 1,
            reason: "the serializer writes private arrays (13 two-byte values and 42 raw bytes) with no public menu properties",
        },
    ],
    blobs: &[
        BlobSpec {
            field: "PoweronBitmap",
            writable: true,
            reason: None,
        },
        BlobSpec {
            field: "GpsLogBitmap",
            writable: false,
            reason: Some(
                "the blob reaches protected factory-calibration pages; sparse radio writes must reject it before I/O",
            ),
        },
    ],
    padding_overrides: &[PaddingOverride {
        class: "MyCallsignDvGatewayData",
        field: "MyCallsignDvGateway",
        padding: 32,
    }],
    value_helpers: &[],
    reviewed: ReviewedLayout {
        operation_counts: &[("radio", 134), ("gps", 17), ("aprs", 85), ("dv", 31)],
        combo_enum_types: 87,
        combo_options: 655,
    },
    record_symbols: &[
        ("MyPositionData", &MY_POSITION_SYMBOLS),
        ("ObjectData", &OBJECT_SYMBOLS),
    ],
    direct_domains,
    record_domains,
};
