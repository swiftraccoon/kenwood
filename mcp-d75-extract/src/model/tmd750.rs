//! TM-D750 spec: the reviewed MCP-D750 layout facts.

use super::{
    AnchorSpec, BaseOverride, BlobSpec, DimensionSpec, DirectDomains, MenuSpec, ModelSpec,
    PaddingOverride, PrivateWriterSpec, RecordDomains, RecordSpec, ReviewedLayout,
    StorageTransformSpec, SymbolOverride, ValueHelperSpec, choices_domain, range_domain,
};

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

/// MCP-D750's position writer names its storage fields differently from
/// MCP-D75's (`aw` altitude, `c` record marker, `g` hemisphere bits, `ax`
/// channel, `e` name).
const MY_POSITION_SYMBOLS: [SymbolOverride; 13] = [
    SymbolOverride {
        symbol: "c",
        name: "RecordType",
        csharp_type: "byte",
        role: Some("internal"),
        aliases: &[],
        storage_transform: None,
        not_writable_reason: None,
    },
    plain("aw", "Altitude", "int"),
    plain("g", "NorthSouth", "byte"),
    plain("j", "LatitudeDegree", "byte"),
    plain("m", "LatitudeMinute", "byte"),
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
    SymbolOverride {
        symbol: "ax",
        name: "MyPositionChannel",
        csharp_type: "byte",
        role: None,
        aliases: &[],
        storage_transform: None,
        not_writable_reason: Some(
            "public storage-width byte has no verified MCP-D750 UI/domain semantics",
        ),
    },
    plain("e", "Name", "string"),
    plain("base.e", "Name", "string"),
];

/// `ObjectData` inherits the position fields of `BasePositionData` and
/// writes them with the same storage symbols as the position writer; its own
/// object fields are public properties.
const OBJECT_SYMBOLS: [SymbolOverride; 9] = [
    plain("g", "NorthSouth", "byte"),
    plain("j", "LatitudeDegree", "byte"),
    plain("m", "LatitudeMinute", "byte"),
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
    plain("e", "Name", "string"),
];

fn direct_domains() -> DirectDomains {
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
    domains.push((
        "gps.MyPositionSelect".to_owned(),
        range_domain(0, 5, "ui_choices"),
    ));
    domains.push((
        "gps.Interval".to_owned(),
        range_domain(2, 1800, "ui_numeric"),
    ));
    domains.push((
        "aprs.QsyLimit".to_owned(),
        range_domain(0, 250, "ui_numeric_scaled_with_off"),
    ));
    domains.push((
        "aprs.LowSpeedSpeed".to_owned(),
        range_domain(2, 30, "ui_numeric"),
    ));
    domains.push((
        "aprs.HiSpeedSpeed".to_owned(),
        range_domain(2, 90, "ui_numeric"),
    ));
    domains
}

fn record_domains() -> RecordDomains {
    let mut domains = vec![(
        ("MyPositionData", "Altitude"),
        range_domain(-500, 15000, "model_validation"),
    )];
    for class in ["MyPositionData", "ObjectData"] {
        domains.push((
            (class, "LatitudeDegree"),
            range_domain(0, 89, "model_validation"),
        ));
        domains.push((
            (class, "LatitudeMinute"),
            range_domain(0, 59, "model_validation"),
        ));
        domains.push((
            (class, "LatitudeSecondEncoded"),
            range_domain(0, 9999, "model_validation"),
        ));
        domains.push((
            (class, "LongitudeDegree"),
            range_domain(0, 179, "model_validation"),
        ));
        domains.push((
            (class, "LongitudeMinute"),
            range_domain(0, 59, "model_validation"),
        ));
        domains.push((
            (class, "LongitudeSecondEncoded"),
            range_domain(0, 9999, "model_validation"),
        ));
    }
    domains.push((
        ("ObjectData", "ObjectTable"),
        choices_domain((47..58).chain(65..91).collect(), "ui_choices"),
    ));
    domains.push((
        ("ObjectData", "ObjectSymbol"),
        range_domain(33, 126, "model_validation"),
    ));
    domains
}

/// The TM-D750 spec.
pub static TMD750: ModelSpec = ModelSpec {
    id: "tmd750",
    product: "TM-D750",
    mcp_product: "MCP-D750 Memory Control Program",
    image_length: 1_929_472,
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
        MenuSpec {
            key: "ipnet",
            property: "IpNetworkMenuData",
        },
        MenuSpec {
            key: "pm",
            property: "ProgrammableMemoryData",
        },
    ],
    dimensions: &[DimensionSpec {
        name: "pm_slot",
        count: 6,
        anchors: &[
            AnchorSpec {
                property: "OffsetProgrammableMemoryAddress",
                stride: 8192,
            },
            AnchorSpec {
                property: "OffsetProgrammableMemoryBitmapAddress",
                stride: 256_000,
            },
        ],
    }],
    records: &[
        RecordSpec {
            menu: "gps",
            list: "MyPositionList",
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
        RecordSpec {
            menu: "dv",
            list: "ReflectorHostsUrlList",
            count: 1,
            base_override: Some(BaseOverride {
                property: "StartAddress",
                value: 334_081,
            }),
        },
        RecordSpec {
            menu: "dv",
            list: "AutoUpdateUrlList",
            count: 1,
            base_override: Some(BaseOverride {
                property: "StartAddress",
                value: 335_106,
            }),
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
            reason: "the serializer writes private arrays (13 two-byte values and 53 raw bytes) with no public menu properties",
        },
        PrivateWriterSpec {
            menu: "radio",
            name: "private_slot_pair_328016",
            base: 328_016,
            stride: Some(16),
            calls: 2,
            count: 2,
            reason: "the per-slot serializer writes only private fields and exposes no public menu properties",
        },
        PrivateWriterSpec {
            menu: "radio",
            name: "private_slot_blob_328048",
            base: 328_048,
            stride: None,
            calls: 1,
            count: 1,
            reason: "the per-slot serializer writes private arrays (20, 4, 6, 6, and 12 raw bytes), four two-byte values, and 17 bytes with no public menu properties",
        },
        PrivateWriterSpec {
            menu: "radio",
            name: "private_slot_blob_332810",
            base: 332_810,
            stride: None,
            calls: 1,
            count: 1,
            reason: "the per-slot serializer writes a private byte and a private byte table with no public menu properties",
        },
    ],
    blobs: &[BlobSpec {
        field: "PoweronBitmap",
        writable: true,
        reason: None,
    }],
    padding_overrides: &[PaddingOverride {
        class: "MyCallsignDvGatewayData",
        field: "MyCallsignDvGateway",
        padding: 32,
    }],
    value_helpers: &[
        ValueHelperSpec {
            property: "IpAddress",
            length: 4,
            encoding: "ipv4_dotted_quad",
        },
        ValueHelperSpec {
            property: "SubnetMask",
            length: 4,
            encoding: "ipv4_dotted_quad",
        },
        ValueHelperSpec {
            property: "DefaultGateway",
            length: 4,
            encoding: "ipv4_dotted_quad",
        },
        ValueHelperSpec {
            property: "PrimaryDnsServer",
            length: 4,
            encoding: "ipv4_dotted_quad",
        },
        ValueHelperSpec {
            property: "SecondaryDnsServer",
            length: 4,
            encoding: "ipv4_dotted_quad",
        },
    ],
    reviewed: ReviewedLayout {
        // Reviewed against MCP-D750 V1.00 (assembly 1.0.9699.22639): direct
        // writes per menu, serializer and per-slot detail class together.
        operation_counts: &[
            ("radio", 141),
            ("gps", 20),
            ("aprs", 82),
            ("dv", 41),
            ("ipnet", 7),
            ("pm", 7),
        ],
        combo_enum_types: 106,
        combo_options: 669,
    },
    record_symbols: &[
        ("MyPositionData", &MY_POSITION_SYMBOLS),
        ("ObjectData", &OBJECT_SYMBOLS),
    ],
    direct_domains,
    record_domains,
};
