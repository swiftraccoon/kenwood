//! Pure scalar input tests, with no radio access.

use super::*;
use crate::memory::{Endian, FieldDescriptor, MenuOption, StringEncoding, menu_field};
use kenwood_schema::CodecError;

type TestError = Box<dyn std::error::Error>;
type TestResult = Result<(), TestError>;

fn field(name: &str) -> Result<&'static MenuField, TestError> {
    menu_field(name).ok_or_else(|| format!("missing registry field {name}").into())
}

const fn scalar(codec: FieldCodec) -> MenuField {
    MenuField {
        menu: "test",
        enum_type: None,
        descriptor: FieldDescriptor::new("test.Scalar", 8, codec),
        options: &[],
        allowed_values: &[],
        storage_transform: None,
        is_blob: false,
    }
}

#[test]
fn owned_values_borrow_without_conversion_or_domain_validation() -> TestResult {
    let values = [
        DecodedFieldValue::Unsigned(u64::MAX),
        DecodedFieldValue::Signed(i64::MIN),
        DecodedFieldValue::Bool(true),
        DecodedFieldValue::Text(" Exact Text ".to_owned()),
        DecodedFieldValue::Bytes(vec![0, 255, 17]),
    ];
    for value in &values {
        match (value, value.as_field_value()) {
            (DecodedFieldValue::Unsigned(expected), FieldValue::Unsigned(actual)) => {
                assert_eq!(
                    actual, *expected,
                    "unsigned stored values must not be range-normalized"
                );
            }
            (DecodedFieldValue::Signed(expected), FieldValue::Signed(actual)) => {
                assert_eq!(
                    actual, *expected,
                    "signed stored values must not be coerced"
                );
            }
            (DecodedFieldValue::Bool(expected), FieldValue::Bool(actual)) => {
                assert_eq!(
                    actual, *expected,
                    "boolean values must retain their typed meaning"
                );
            }
            (DecodedFieldValue::Text(expected), FieldValue::Text(actual)) => {
                assert_eq!(
                    actual, expected,
                    "text borrowing must preserve exact spacing and case"
                );
                assert_eq!(
                    actual.as_ptr(),
                    expected.as_ptr(),
                    "text conversion must borrow the existing allocation"
                );
            }
            (DecodedFieldValue::Bytes(expected), FieldValue::Bytes(actual)) => {
                assert_eq!(
                    actual, expected,
                    "byte values must remain lossless even though scalar parsing refuses them"
                );
                assert_eq!(
                    actual.as_ptr(),
                    expected.as_ptr(),
                    "byte conversion must borrow the existing allocation"
                );
            }
            _ => return Err("borrowed field value must retain its original variant".into()),
        }
    }
    Ok(())
}

#[test]
fn real_boolean_and_masked_boolean_fields_accept_only_documented_aliases() -> TestResult {
    for name in ["radio.RepeaterTxHold", "radio.TxEqualizerFmNfm"] {
        let field = field(name)?;
        for (input, expected) in [
            ("true", true),
            ("TRUE", true),
            ("On", true),
            ("YeS", true),
            ("1", true),
            ("false", false),
            ("FALSE", false),
            ("Off", false),
            ("nO", false),
            ("0", false),
        ] {
            assert_eq!(
                field.parse_value(input)?,
                DecodedFieldValue::Bool(expected),
                "{name} must parse documented boolean alias {input:?}"
            );
        }
        for input in ["", "2", "-1", "0x1", "truthy", " on", "yes ", "false\n"] {
            assert!(
                matches!(
                    field.parse_value(input),
                    Err(MenuValueError::InvalidBoolean { .. })
                ),
                "{name} must reject undocumented boolean syntax {input:?}"
            );
        }
    }
    Ok(())
}

#[test]
fn real_unsigned_codecs_parse_decimal_and_hex_with_storage_bounds() -> TestResult {
    for (name, input, expected) in [
        ("gps.MyPositionList[0].LatitudeDegree", "89", 89),
        ("gps.MyPositionList[0].LatitudeDegree", "0x59", 89),
        ("gps.MyPositionList[0].LatitudeDegree", "0X0A", 10),
        ("gps.MyPositionList[0].NorthSouth", "1", 1),
        ("gps.MyPositionList[0].NorthSouth", "0x0", 0),
        ("gps.MyPositionList[0].LatitudeSecondEncoded", "9999", 9999),
    ] {
        assert_eq!(
            field(name)?.parse_value(input)?,
            DecodedFieldValue::Unsigned(expected),
            "{name} must retain the parsed raw integer"
        );
    }
    for (name, value) in [
        ("gps.MyPositionList[0].LatitudeDegree", "90"),
        ("gps.MyPositionList[0].NorthSouth", "2"),
        ("gps.MyPositionList[0].LatitudeSecondEncoded", "10000"),
    ] {
        assert!(
            matches!(
                field(name)?.parse_value(value),
                Err(MenuValueError::Schema(SchemaError::Codec {
                    source: CodecError::UnsignedOutOfRange { .. },
                    ..
                }))
            ),
            "{name} must retain codec bounds after numeric parsing"
        );
    }
    Ok(())
}

#[test]
fn unsigned_syntax_overflow_and_signed_decimal_are_not_guessed() -> TestResult {
    let unsigned = scalar(FieldCodec::Unsigned {
        width: 8,
        endian: Endian::Big,
        min: 0,
        max: u64::MAX,
    });
    for input in ["18446744073709551615", "0xFFFFFFFFFFFFFFFF"] {
        assert_eq!(
            unsigned.parse_value(input)?,
            DecodedFieldValue::Unsigned(u64::MAX),
            "full-width unsigned values must not narrow"
        );
    }
    for input in [
        "",
        "-1",
        "1.0",
        " 1",
        "1 ",
        "1_000",
        "0x",
        "0xGG",
        "18446744073709551616",
        "0x10000000000000000",
    ] {
        assert!(
            matches!(
                unsigned.parse_value(input),
                Err(MenuValueError::InvalidUnsigned { .. })
            ),
            "invalid unsigned syntax {input:?} must be refused without guessing"
        );
    }
    let signed = field("gps.MyPositionList[0].Altitude")?;
    for (input, expected) in [("-500", -500), ("15000", 15000), ("+12", 12), ("0", 0)] {
        assert_eq!(
            signed.parse_value(input)?,
            DecodedFieldValue::Signed(expected),
            "signed decimal {input:?} must preserve its exact value"
        );
    }
    for input in ["-501", "15001"] {
        assert!(
            matches!(
                signed.parse_value(input),
                Err(MenuValueError::Schema(SchemaError::Codec {
                    source: CodecError::SignedOutOfRange { .. },
                    ..
                }))
            ),
            "signed storage bounds must reject {input}"
        );
    }
    for input in [
        "",
        "0x10",
        "-0x10",
        "1.5",
        " 1",
        "9223372036854775808",
        "-9223372036854775809",
    ] {
        assert!(
            matches!(
                signed.parse_value(input),
                Err(MenuValueError::InvalidSigned { .. })
            ),
            "invalid signed input {input:?} must not be interpreted as another representation"
        );
    }
    Ok(())
}

#[test]
fn real_enum_labels_match_public_text_only_without_trimming() -> TestResult {
    let field = field("radio.RepeaterMode")?;
    for (input, expected) in [
        ("Cross Band", 0),
        ("cross band", 0),
        ("LOCKED TX:A BAND", 1),
        ("Locked TX:B Band", 2),
        ("0x2", 2),
    ] {
        assert_eq!(
            field.parse_value(input)?,
            DecodedFieldValue::Unsigned(expected),
            "a public enum label or admitted raw value must resolve exactly"
        );
    }
    for input in [
        "a",
        "b",
        "c",
        " Cross Band",
        "Cross Band ",
        "Edit_Menu_TXRX_RepeaterRepeaterMode_CrossBand",
    ] {
        assert!(
            matches!(
                field.parse_value(input),
                Err(MenuValueError::UnknownOption { .. })
            ),
            "private aliases or modified labels must be rejected: {input:?}"
        );
    }
    assert!(
        matches!(
            field.parse_value("3"),
            Err(MenuValueError::Schema(SchemaError::DisallowedValue {
                value: 3,
                ..
            }))
        ),
        "numeric enum input must still be one of the published options"
    );
    Ok(())
}

const AMBIGUOUS_OPTIONS: &[MenuOption] = &[
    MenuOption {
        raw: 1,
        member: "private_a",
        label: Some("Mode"),
        resource_key: Some("private_resource"),
    },
    MenuOption {
        raw: 2,
        member: "private_b",
        label: Some("MODE"),
        resource_key: None,
    },
    MenuOption {
        raw: 3,
        member: "private_c",
        label: Some("Other"),
        resource_key: None,
    },
    MenuOption {
        raw: 7,
        member: "private_d",
        label: None,
        resource_key: Some("unlabeled_resource"),
    },
];

#[test]
fn ambiguous_labels_are_refused_but_explicit_raw_options_remain_available() -> TestResult {
    let field = MenuField {
        options: AMBIGUOUS_OPTIONS,
        enum_type: Some("test.Options"),
        ..scalar(FieldCodec::Byte { min: 0, max: 7 })
    };
    for input in ["Mode", "mode", "mOdE"] {
        assert!(
            matches!(
                field.parse_value(input),
                Err(MenuValueError::AmbiguousOption { .. })
            ),
            "case-insensitive duplicate label {input:?} must not choose an arbitrary option"
        );
    }
    for raw in [1, 2, 3, 7] {
        assert_eq!(
            field.parse_value(&raw.to_string())?,
            DecodedFieldValue::Unsigned(raw),
            "an explicit admitted raw value resolves label ambiguity"
        );
    }
    assert_eq!(
        field.parse_value("other")?,
        DecodedFieldValue::Unsigned(3),
        "unambiguous public labels remain available"
    );
    for input in [
        "private_a",
        "private_d",
        "private_resource",
        "unlabeled_resource",
    ] {
        assert!(
            matches!(
                field.parse_value(input),
                Err(MenuValueError::UnknownOption { .. })
            ),
            "missing public labels must not expose private member or resource aliases"
        );
    }
    Ok(())
}

#[test]
fn choice_domains_and_codec_bounds_are_both_required_after_parsing() -> TestResult {
    let choices = MenuField {
        allowed_values: &[2, 4],
        ..scalar(FieldCodec::Byte { min: 0, max: 9 })
    };
    for value in [2, 4] {
        assert_eq!(
            choices.parse_value(&value.to_string())?,
            DecodedFieldValue::Unsigned(value),
            "allowed numeric choices must remain writable"
        );
    }
    for value in [0, 3, 9] {
        assert!(
            matches!(
                choices.parse_value(&value.to_string()),
                Err(MenuValueError::Schema(SchemaError::DisallowedValue { .. }))
            ),
            "a value inside byte bounds but outside the allowed choices must fail"
        );
    }
    let intersected = MenuField {
        options: AMBIGUOUS_OPTIONS,
        allowed_values: &[1],
        ..scalar(FieldCodec::Byte { min: 0, max: 7 })
    };
    assert_eq!(
        intersected.parse_value("1")?,
        DecodedFieldValue::Unsigned(1),
        "a value in both option and choice domains must pass"
    );
    assert!(
        matches!(
            intersected.parse_value("Other"),
            Err(MenuValueError::Schema(SchemaError::DisallowedValue {
                value: 3,
                ..
            }))
        ),
        "label resolution must not bypass the separate allowed-choice domain"
    );
    let codec_bound = MenuField {
        options: AMBIGUOUS_OPTIONS,
        ..scalar(FieldCodec::Byte { min: 0, max: 2 })
    };
    assert!(
        matches!(
            codec_bound.parse_value("Other"),
            Err(MenuValueError::Schema(SchemaError::Codec {
                source: CodecError::UnsignedOutOfRange { value: 3, .. },
                ..
            }))
        ),
        "a known option must still fit the field codec"
    );
    let incompatible = MenuField {
        allowed_values: &[0, 1],
        ..scalar(FieldCodec::Bool)
    };
    assert!(
        matches!(
            incompatible.parse_value("true"),
            Err(MenuValueError::Schema(SchemaError::TypeMismatch { .. }))
        ),
        "a boolean parser must not silently coerce a numeric choice-domain descriptor"
    );
    Ok(())
}

#[test]
fn strings_preserve_exact_text_and_use_encoded_byte_limits() -> TestResult {
    let utf8 = field("pm.PmName1")?;
    for text in [
        "",
        " Home /a ",
        "caf\u{e9}",
        "\u{e9}\u{e9}\u{e9}\u{e9}\u{e9}\u{e9}\u{e9}\u{e9}",
    ] {
        assert_eq!(
            utf8.parse_value(text)?,
            DecodedFieldValue::Text(text.to_owned()),
            "UTF-8 strings must preserve all supplied text within the encoded width"
        );
    }
    for text in [
        "ABCDEFGHIJKLMNOPQ",
        "\u{e9}\u{e9}\u{e9}\u{e9}\u{e9}\u{e9}\u{e9}\u{e9}\u{e9}",
    ] {
        assert!(
            matches!(
                utf8.parse_value(text),
                Err(MenuValueError::Schema(SchemaError::Codec {
                    source: CodecError::TextTooLong { .. },
                    ..
                }))
            ),
            "UTF-8 byte length, not character count or truncation, must enforce storage limits"
        );
    }
    let ascii = field("radio.PowerOnMessage")?;
    assert_eq!(
        ascii.parse_value(" Ready /1 ")?,
        DecodedFieldValue::Text(" Ready /1 ".to_owned()),
        "memory-map text must preserve admitted ASCII punctuation and spacing"
    );
    for text in ["caf\u{e9}", "line\n", "tab\t"] {
        assert!(
            matches!(
                ascii.parse_value(text),
                Err(MenuValueError::Schema(SchemaError::Codec {
                    source: CodecError::InvalidMemoryMapTextByte { .. },
                    ..
                }))
            ),
            "the actual memory-map encoding must reject unsupported text bytes"
        );
    }
    Ok(())
}

#[test]
fn string_terminators_are_rejected_instead_of_silently_shortening_the_value() -> TestResult {
    let utf8 = field("pm.PmName1")?;
    for text in ["\0", "A\0B", "\0START", "END\0"] {
        assert!(
            matches!(
                utf8.parse_value(text),
                Err(MenuValueError::EmbeddedTerminator { .. })
            ),
            "NUL-containing input must not decode to a different shorter string"
        );
    }
    let padded = scalar(FieldCodec::FixedString {
        len: 8,
        encoding: StringEncoding::Utf8,
        padding: b' ',
    });
    assert_eq!(
        padded.parse_value("TEXT")?,
        DecodedFieldValue::Text("TEXT".to_owned()),
        "text without the configured padding byte remains representable"
    );
    for text in [" TEXT", "TEXT ", "A B", "A\0B"] {
        assert!(
            matches!(
                padded.parse_value(text),
                Err(MenuValueError::EmbeddedTerminator { .. })
            ),
            "embedded configured padding must not be trimmed or accepted as exact text"
        );
    }
    Ok(())
}

#[test]
fn raw_numeric_storage_never_applies_display_unit_transforms() -> TestResult {
    let encoded = field("gps.MyPositionList[0].LatitudeSecondEncoded")?;
    assert!(
        encoded.storage_transform.is_some(),
        "fixture must expose actual scaling metadata"
    );
    assert_eq!(
        encoded.parse_value("30")?,
        DecodedFieldValue::Unsigned(30),
        "raw input 30 must not be silently converted from seconds into encoded storage"
    );
    for text in ["30 seconds", "30s", "0.5", "00:30"] {
        assert!(
            matches!(
                encoded.parse_value(text),
                Err(MenuValueError::InvalidUnsigned { .. })
            ),
            "unit or display syntax must require a separately specified conversion API"
        );
    }
    Ok(())
}

#[test]
fn blobs_and_byte_arrays_have_no_scalar_parser_even_if_the_codec_looks_simple() {
    for field in [
        MenuField {
            is_blob: true,
            ..scalar(FieldCodec::Byte { min: 0, max: 255 })
        },
        scalar(FieldCodec::Bytes { len: 4 }),
        MenuField {
            is_blob: true,
            ..scalar(FieldCodec::FixedString {
                len: 8,
                encoding: StringEncoding::Utf8,
                padding: 0,
            })
        },
    ] {
        for text in ["0", "0x00", "DEADBEEF", "", "abcd"] {
            assert!(
                matches!(
                    field.parse_value(text),
                    Err(MenuValueError::UnsupportedBinaryField { .. })
                ),
                "binary storage must not gain a raw write representation through scalar parsing"
            );
        }
    }
}

#[test]
fn parser_errors_do_not_echo_input_that_could_be_a_credential() -> TestResult {
    let secret = "PrivateCredential!";
    let ambiguous = MenuField {
        options: &[
            MenuOption {
                raw: 0,
                member: "a",
                label: Some("PrivateCredential!"),
                resource_key: None,
            },
            MenuOption {
                raw: 1,
                member: "b",
                label: Some("PRIVATECREDENTIAL!"),
                resource_key: None,
            },
        ],
        ..scalar(FieldCodec::Byte { min: 0, max: 1 })
    };
    for field in [
        scalar(FieldCodec::Bool),
        scalar(FieldCodec::Unsigned {
            width: 8,
            endian: Endian::Little,
            min: 0,
            max: u64::MAX,
        }),
        scalar(FieldCodec::Signed {
            width: 8,
            endian: Endian::Little,
            min: i64::MIN,
            max: i64::MAX,
        }),
        MenuField {
            options: AMBIGUOUS_OPTIONS,
            ..scalar(FieldCodec::Byte { min: 0, max: 7 })
        },
        ambiguous,
        scalar(FieldCodec::FixedString {
            len: 4,
            encoding: StringEncoding::Utf8,
            padding: 0,
        }),
    ] {
        let error = field
            .parse_value(secret)
            .err()
            .ok_or("private input unexpectedly parsed")?;
        assert!(
            !error.to_string().contains(secret),
            "display diagnostics must not repeat potentially private input"
        );
        assert!(
            !format!("{error:?}").contains(secret),
            "typed debug diagnostics must not retain potentially private input"
        );
        assert!(
            error.to_string().contains(field.descriptor.name),
            "diagnostics must identify the field without exposing its input"
        );
    }
    let terminated = format!("{secret}\0");
    let field = scalar(FieldCodec::FixedString {
        len: 64,
        encoding: StringEncoding::Utf8,
        padding: 0,
    });
    let error = field
        .parse_value(&terminated)
        .err()
        .ok_or("embedded NUL accepted")?;
    assert!(
        !error.to_string().contains(secret),
        "terminator diagnostics must not repeat private string content"
    );
    assert!(
        !format!("{error:?}").contains(secret),
        "terminator errors must not store the private string"
    );
    Ok(())
}

#[test]
fn parsed_masked_value_composes_with_existing_patch_planning_without_neighbor_changes() -> TestResult
{
    let field = field("radio.TxEqualizerFmNfm")?;
    let parsed = field.parse_value("on")?;
    let slot = SlotIndex::new(0)?;
    let mut planner = PatchPlanner::new();
    let _accepted = planner.set_menu(field, Some(slot), parsed.as_field_value())?;
    let patches = planner.finish()?;
    assert_eq!(
        patches.len(),
        1,
        "one bit field must affect one canonical page"
    );
    let patch = patches.pages().first().ok_or("planned page")?;
    let mut page = vec![0xA5; patch.page().len()];
    patch.apply(&mut page)?;
    let offset =
        field.descriptor.address(Some(slot))?.as_usize() - patch.page().address().as_usize();
    for (index, byte) in page.into_iter().enumerate() {
        assert_eq!(
            byte,
            if index == offset { 0xA7 } else { 0xA5 },
            "parsed bit value must preserve every unrelated bit and page byte at offset {index}"
        );
    }
    Ok(())
}
