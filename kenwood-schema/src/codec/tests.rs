use super::*;

type TestResult = Result<(), Box<dyn std::error::Error>>;

const STORED: DecodeOptions = DecodeOptions {
    domain: ValueDomain::Stored,
    boolean: BooleanDecoding::Canonical,
    text: TextPolicy::ExactPadding,
};

const WRITABLE: DecodeOptions = DecodeOptions {
    domain: ValueDomain::Writable,
    ..STORED
};

const PERMISSIVE: DecodeOptions = DecodeOptions {
    boolean: BooleanDecoding::NonZero,
    text: TextPolicy::FirstTerminator,
    ..STORED
};

fn bytes(encoded: &[MaskedByte]) -> Vec<u8> {
    encoded.iter().map(|byte| byte.value()).collect()
}

fn encode(codec: FieldCodec, value: FieldValue<'_>) -> Result<Vec<MaskedByte>, CodecError> {
    codec.encode(value, ValueDomain::Stored, TextPolicy::ExactPadding)
}

fn assert_invalid_metadata(spec: CodecSpec, expected: CodecError) {
    assert_eq!(FieldCodec::new(spec), Err(expected.clone()), "{spec:?}");
    assert_eq!(FieldCodec::try_from(spec), Err(expected), "{spec:?}");
}

#[test]
fn malformed_metadata_is_rejected_at_construction() {
    let invalid = [
        (
            CodecSpec::Byte { min: 2, max: 1 },
            CodecError::InvalidUnsignedRange { min: 2, max: 1 },
        ),
        (
            CodecSpec::BitBool { mask: 0 },
            CodecError::InvalidBitField { mask: 0, shift: 0 },
        ),
        (
            CodecSpec::BitBool { mask: 3 },
            CodecError::InvalidBitField { mask: 3, shift: 0 },
        ),
        (
            CodecSpec::Unsigned {
                width: 0,
                endian: Endian::Little,
                min: 0,
                max: 1,
            },
            CodecError::InvalidIntegerWidth { width: 0 },
        ),
        (
            CodecSpec::Signed {
                width: 9,
                endian: Endian::Big,
                min: 0,
                max: 1,
            },
            CodecError::InvalidIntegerWidth { width: 9 },
        ),
        (
            CodecSpec::Unsigned {
                width: 8,
                endian: Endian::Little,
                min: 2,
                max: 1,
            },
            CodecError::InvalidUnsignedRange { min: 2, max: 1 },
        ),
        (
            CodecSpec::Signed {
                width: 8,
                endian: Endian::Big,
                min: 2,
                max: 1,
            },
            CodecError::InvalidSignedRange { min: 2, max: 1 },
        ),
        (
            CodecSpec::Unsigned {
                width: 1,
                endian: Endian::Little,
                min: 0,
                max: 256,
            },
            CodecError::DomainExceedsWidth { width: 1 },
        ),
        (
            CodecSpec::Signed {
                width: 1,
                endian: Endian::Big,
                min: -129,
                max: 127,
            },
            CodecError::DomainExceedsWidth { width: 1 },
        ),
        (
            CodecSpec::Signed {
                width: 1,
                endian: Endian::Big,
                min: -128,
                max: 128,
            },
            CodecError::DomainExceedsWidth { width: 1 },
        ),
        (
            CodecSpec::Bytes { len: 0 },
            CodecError::InvalidEncodedLength { len: 0 },
        ),
        (
            CodecSpec::FixedString {
                len: 0,
                encoding: StringEncoding::Utf8,
                padding: 0,
            },
            CodecError::InvalidEncodedLength { len: 0 },
        ),
    ];
    for (spec, expected) in invalid {
        assert_invalid_metadata(spec, expected);
    }
}

#[test]
fn bit_metadata_rejects_gaps_misalignment_and_unrepresentable_domains() {
    for (mask, shift, min, max) in [
        (0, 0, 0, 0),
        (0x55, 0, 0, 1),
        (0x0F, 1, 0, 1),
        (0xF0, 3, 0, 1),
        (0xFF, 8, 0, 1),
        (0xFF, 255, 0, 1),
        (0x70, 4, 2, 1),
        (0x70, 4, 0, 8),
    ] {
        let spec = CodecSpec::BitField {
            mask,
            shift,
            min,
            max,
        };
        assert_invalid_metadata(spec, CodecError::InvalidBitField { mask, shift });
    }
}

#[test]
fn const_construction_preserves_storage_bounds_and_rejects_invalid_domains() -> TestResult {
    const SIGNED: FieldCodec = match FieldCodec::new(CodecSpec::Signed {
        width: 8,
        endian: Endian::Big,
        min: i64::MIN,
        max: i64::MAX,
    }) {
        Ok(codec) => codec,
        Err(_) => unreachable!(),
    };
    const SPEC: CodecSpec = SIGNED.spec();
    const INVALID: Result<FieldCodec, CodecError> = FieldCodec::new(CodecSpec::Unsigned {
        width: 1,
        endian: Endian::Little,
        min: 0,
        max: 256,
    });

    assert_eq!(FieldCodec::try_from(SPEC)?, SIGNED);
    assert_eq!(
        SIGNED.decode(&i64::MIN.to_be_bytes(), WRITABLE)?,
        DecodedFieldValue::Signed(i64::MIN)
    );
    assert_eq!(
        bytes(&encode(SIGNED, FieldValue::Signed(-1))?),
        [u8::MAX; 8]
    );
    assert_eq!(INVALID, Err(CodecError::DomainExceedsWidth { width: 1 }));
    assert_eq!(FieldCodec::new(CodecSpec::Bool)?, FieldCodec::BOOL);
    assert_eq!(FieldCodec::BOOL.spec(), CodecSpec::Bool);
    Ok(())
}

#[test]
fn changing_a_specification_copy_cannot_change_the_validated_codec() -> TestResult {
    let original = CodecSpec::BitField {
        mask: 0x70,
        shift: 4,
        min: 0,
        max: 7,
    };
    let codec = FieldCodec::try_from(original)?;
    let mut projected = CodecSpec::from(codec);
    let CodecSpec::BitField { shift, .. } = &mut projected else {
        return Err("bit-field codec projected another storage kind".into());
    };
    *shift = 8;
    assert_invalid_metadata(
        projected,
        CodecError::InvalidBitField {
            mask: 0x70,
            shift: 8,
        },
    );
    assert_eq!(codec.spec(), original);
    assert_eq!(
        codec.decode(&[0xF0], WRITABLE)?,
        DecodedFieldValue::Unsigned(7)
    );
    let encoded = codec.encode(
        FieldValue::Unsigned(3),
        ValueDomain::Writable,
        TextPolicy::ExactPadding,
    )?;
    assert_eq!(
        encoded.first().map(|byte| (byte.mask(), byte.value())),
        Some((0x70, 0x30))
    );
    Ok(())
}

#[test]
fn positive_lengths_are_not_capped_to_any_radio_image() -> TestResult {
    let bytes = FieldCodec::new(CodecSpec::Bytes { len: usize::MAX })?;
    let text = FieldCodec::new(CodecSpec::FixedString {
        len: usize::MAX,
        encoding: StringEncoding::Utf8,
        padding: 0,
    })?;
    assert_eq!(bytes.encoded_len(), usize::MAX);
    assert_eq!(text.encoded_len(), usize::MAX);
    Ok(())
}

#[test]
fn every_codec_requires_an_exact_supplied_field_slice() -> TestResult {
    let codecs = [
        FieldCodec::new(CodecSpec::Byte { min: 0, max: 255 })?,
        FieldCodec::BOOL,
        FieldCodec::new(CodecSpec::BitBool { mask: 0x80 })?,
        FieldCodec::new(CodecSpec::BitField {
            mask: 0x70,
            shift: 4,
            min: 0,
            max: 7,
        })?,
        FieldCodec::new(CodecSpec::Unsigned {
            width: 3,
            endian: Endian::Little,
            min: 0,
            max: 255,
        })?,
        FieldCodec::new(CodecSpec::Signed {
            width: 2,
            endian: Endian::Big,
            min: -1,
            max: 1,
        })?,
        FieldCodec::new(CodecSpec::FixedString {
            len: 3,
            encoding: StringEncoding::Utf8,
            padding: 0,
        })?,
        FieldCodec::new(CodecSpec::Bytes { len: 3 })?,
    ];
    for codec in codecs {
        let expected = codec.encoded_len();
        for actual in [expected - 1, expected + 1] {
            assert_eq!(
                codec.decode(&vec![0; actual], STORED),
                Err(CodecError::ByteLength { actual, expected }),
                "no missing or extra bytes: {codec:?}"
            );
        }
    }
    Ok(())
}

#[test]
fn byte_storage_domain_is_distinct_from_writable_range() -> TestResult {
    let codec = FieldCodec::new(CodecSpec::Byte { min: 10, max: 20 })?;
    for value in 0_u8..=255 {
        let raw = u64::from(value);
        assert_eq!(
            codec.decode(&[value], STORED)?,
            DecodedFieldValue::Unsigned(raw)
        );
        assert_eq!(bytes(&encode(codec, FieldValue::Unsigned(raw))?), [value]);
        let writable = (10..=20).contains(&value);
        assert_eq!(codec.decode(&[value], WRITABLE).is_ok(), writable);
        assert_eq!(
            codec
                .encode(
                    FieldValue::Unsigned(raw),
                    ValueDomain::Writable,
                    TextPolicy::ExactPadding
                )
                .is_ok(),
            writable
        );
    }
    assert_eq!(
        encode(codec, FieldValue::Unsigned(256)),
        Err(CodecError::UnsignedOutOfRange {
            value: 256,
            min: 0,
            max: 255
        })
    );
    Ok(())
}

#[test]
fn all_contiguous_masks_preserve_only_the_owned_bits() -> TestResult {
    for shift in 0_u8..8 {
        for width in 1_u8..=8 - shift {
            let capacity = u8::try_from((1_u16 << width) - 1)?;
            let mask = capacity << shift;
            let codec = FieldCodec::new(CodecSpec::BitField {
                mask,
                shift,
                min: 0,
                max: capacity,
            })?;
            for value in 0..=capacity {
                let encoded = encode(codec, FieldValue::Unsigned(u64::from(value)))?;
                assert_eq!(encoded.len(), 1);
                let byte = encoded.first().ok_or("missing masked byte")?;
                assert_eq!(byte.offset(), 0);
                assert_eq!(byte.mask(), mask);
                assert_eq!(byte.value(), value << shift);
                assert_eq!(byte.value() & !mask, 0);
                let applied = (0xA5 & !mask) | byte.value();
                assert_eq!(applied & !mask, 0xA5 & !mask);
                assert_eq!(
                    codec.decode(&[applied], STORED)?,
                    DecodedFieldValue::Unsigned(u64::from(value))
                );
            }
            assert!(matches!(
                encode(codec, FieldValue::Unsigned(u64::from(capacity) + 1)),
                Err(CodecError::UnsignedOutOfRange { .. })
            ));
        }
    }
    Ok(())
}

#[test]
fn bit_storage_domain_does_not_erase_the_writable_range() -> TestResult {
    let codec = FieldCodec::new(CodecSpec::BitField {
        mask: 0x70,
        shift: 4,
        min: 1,
        max: 3,
    })?;
    assert_eq!(
        codec.decode(&[0xFF], STORED)?,
        DecodedFieldValue::Unsigned(7)
    );
    assert_eq!(bytes(&encode(codec, FieldValue::Unsigned(7))?), [0x70]);
    assert_eq!(
        codec.decode(&[0xFF], WRITABLE),
        Err(CodecError::UnsignedOutOfRange {
            value: 7,
            min: 1,
            max: 3
        })
    );
    assert_eq!(
        codec.encode(
            FieldValue::Unsigned(0),
            ValueDomain::Writable,
            TextPolicy::ExactPadding
        ),
        Err(CodecError::UnsignedOutOfRange {
            value: 0,
            min: 1,
            max: 3
        })
    );
    Ok(())
}

#[test]
fn boolean_read_policy_is_explicit_and_encoding_is_always_canonical() -> TestResult {
    for value in 0_u8..=255 {
        assert_eq!(
            FieldCodec::BOOL.decode(&[value], PERMISSIVE)?,
            DecodedFieldValue::Bool(value != 0)
        );
        if value <= 1 {
            assert_eq!(
                FieldCodec::BOOL.decode(&[value], STORED)?,
                DecodedFieldValue::Bool(value != 0)
            );
        } else {
            assert_eq!(
                FieldCodec::BOOL.decode(&[value], STORED),
                Err(CodecError::NonCanonicalBoolean { value })
            );
        }
        let decoded = FieldCodec::BOOL.decode(&[value], PERMISSIVE)?;
        assert_eq!(
            bytes(&encode(FieldCodec::BOOL, decoded.as_field_value())?),
            [u8::from(value != 0)]
        );
    }
    for shift in 0..8 {
        let mask = 1_u8 << shift;
        let codec = FieldCodec::new(CodecSpec::BitBool { mask })?;
        assert_eq!(
            codec.decode(&[!mask], STORED)?,
            DecodedFieldValue::Bool(false)
        );
        assert_eq!(
            codec.decode(&[mask], STORED)?,
            DecodedFieldValue::Bool(true)
        );
        let encoded = encode(codec, FieldValue::Bool(true))?;
        assert_eq!(
            encoded.first().map(|byte| (byte.mask(), byte.value())),
            Some((mask, mask))
        );
        let cleared = encode(codec, FieldValue::Bool(false))?;
        assert_eq!(
            cleared.first().map(|byte| (byte.mask(), byte.value())),
            Some((mask, 0))
        );
    }
    Ok(())
}

#[test]
fn all_integer_widths_round_trip_boundaries_in_both_orders() -> TestResult {
    for width in 1_u8..=8 {
        let maximum = u64::MAX >> (64 - u32::from(width) * 8);
        let signed_maximum = i64::try_from(maximum >> 1)?;
        let signed_minimum = -signed_maximum - 1;
        for endian in [Endian::Little, Endian::Big] {
            let unsigned = FieldCodec::new(CodecSpec::Unsigned {
                width,
                endian,
                min: 0,
                max: maximum,
            })?;
            for value in [0, 1, maximum] {
                let encoded = encode(unsigned, FieldValue::Unsigned(value))?;
                assert_eq!(encoded.len(), usize::from(width));
                assert_eq!(
                    unsigned.decode(&bytes(&encoded), WRITABLE)?,
                    DecodedFieldValue::Unsigned(value)
                );
                for (index, byte) in encoded.iter().enumerate() {
                    assert_eq!(byte.offset(), index);
                    assert_eq!(byte.mask(), 255);
                }
            }
            let signed = FieldCodec::new(CodecSpec::Signed {
                width,
                endian,
                min: signed_minimum,
                max: signed_maximum,
            })?;
            for value in [signed_minimum, -1, 0, 1, signed_maximum] {
                let encoded = encode(signed, FieldValue::Signed(value))?;
                assert_eq!(
                    signed.decode(&bytes(&encoded), WRITABLE)?,
                    DecodedFieldValue::Signed(value)
                );
            }
        }
    }
    Ok(())
}

#[test]
fn integer_wire_order_and_sign_extension_have_independent_vectors() -> TestResult {
    for (endian, unsigned_bytes, signed_bytes) in [
        (Endian::Little, [0x56, 0x34, 0x12], [0xFE, 0xFF, 0xFF]),
        (Endian::Big, [0x12, 0x34, 0x56], [0xFF, 0xFF, 0xFE]),
    ] {
        let unsigned = FieldCodec::new(CodecSpec::Unsigned {
            width: 3,
            endian,
            min: 0,
            max: 0xFF_FFFF,
        })?;
        assert_eq!(
            unsigned.decode(&unsigned_bytes, STORED)?,
            DecodedFieldValue::Unsigned(0x12_3456)
        );
        assert_eq!(
            bytes(&encode(unsigned, FieldValue::Unsigned(0x12_3456))?),
            unsigned_bytes
        );
        let signed = FieldCodec::new(CodecSpec::Signed {
            width: 3,
            endian,
            min: -0x80_0000,
            max: 0x7F_FFFF,
        })?;
        assert_eq!(
            signed.decode(&signed_bytes, STORED)?,
            DecodedFieldValue::Signed(-2)
        );
        assert_eq!(
            bytes(&encode(signed, FieldValue::Signed(-2))?),
            signed_bytes
        );
    }
    Ok(())
}

#[test]
fn stored_integer_values_must_fit_width_but_not_writable_range() -> TestResult {
    let unsigned = FieldCodec::new(CodecSpec::Unsigned {
        width: 1,
        endian: Endian::Little,
        min: 10,
        max: 20,
    })?;
    assert_eq!(
        unsigned.decode(&[255], STORED)?,
        DecodedFieldValue::Unsigned(255)
    );
    assert_eq!(bytes(&encode(unsigned, FieldValue::Unsigned(255))?), [255]);
    assert!(matches!(
        unsigned.decode(&[255], WRITABLE),
        Err(CodecError::UnsignedOutOfRange { .. })
    ));
    assert_eq!(
        unsigned.encode(
            FieldValue::Unsigned(255),
            ValueDomain::Writable,
            TextPolicy::ExactPadding
        ),
        Err(CodecError::UnsignedOutOfRange {
            value: 255,
            min: 10,
            max: 20,
        })
    );
    assert!(matches!(
        encode(unsigned, FieldValue::Unsigned(256)),
        Err(CodecError::UnsignedOutOfRange { .. })
    ));
    let signed = FieldCodec::new(CodecSpec::Signed {
        width: 1,
        endian: Endian::Big,
        min: -10,
        max: 10,
    })?;
    assert_eq!(
        signed.decode(&[128], STORED)?,
        DecodedFieldValue::Signed(-128)
    );
    assert_eq!(bytes(&encode(signed, FieldValue::Signed(-128))?), [128]);
    assert!(matches!(
        signed.decode(&[128], WRITABLE),
        Err(CodecError::SignedOutOfRange { .. })
    ));
    assert_eq!(
        signed.encode(
            FieldValue::Signed(-128),
            ValueDomain::Writable,
            TextPolicy::ExactPadding
        ),
        Err(CodecError::SignedOutOfRange {
            value: -128,
            min: -10,
            max: 10,
        })
    );
    assert!(matches!(
        encode(signed, FieldValue::Signed(-129)),
        Err(CodecError::SignedOutOfRange { .. })
    ));
    assert!(matches!(
        encode(signed, FieldValue::Signed(128)),
        Err(CodecError::SignedOutOfRange { .. })
    ));
    Ok(())
}

#[test]
fn exact_nul_padding_rejects_hidden_tail_while_first_terminator_ignores_it() -> TestResult {
    let codec = FieldCodec::new(CodecSpec::FixedString {
        len: 5,
        encoding: StringEncoding::Utf8,
        padding: 0,
    })?;
    assert_eq!(
        codec.decode(b"AB\0\0\0", STORED)?,
        DecodedFieldValue::Text("AB".to_owned())
    );
    assert_eq!(
        bytes(&codec.encode(
            FieldValue::Text("AB"),
            ValueDomain::Writable,
            TextPolicy::FirstTerminator
        )?),
        b"AB\0\0\0"
    );
    assert_eq!(
        codec.decode(b"AB\0\0\xFF", STORED),
        Err(CodecError::FixedStringDataAfterNul {
            terminator_offset: 2,
            offset: 4,
            value: 255
        })
    );
    assert_eq!(
        codec.decode(b"AB\0\0\xFF", PERMISSIVE)?,
        DecodedFieldValue::Text("AB".to_owned())
    );
    assert_eq!(
        encode(codec, FieldValue::Text("A\0B")),
        Err(CodecError::TextContainsNul { offset: 1 })
    );
    assert_eq!(
        codec.encode(
            FieldValue::Text("A\0B"),
            ValueDomain::Writable,
            TextPolicy::FirstTerminator
        ),
        Err(CodecError::TextTerminator {
            offset: 1,
            value: 0,
            padding: 0
        })
    );
    Ok(())
}

#[test]
fn non_nul_padding_policy_preserves_interior_bytes_only_when_requested() -> TestResult {
    let codec = FieldCodec::new(CodecSpec::FixedString {
        len: 5,
        encoding: StringEncoding::Utf8,
        padding: b' ',
    })?;
    assert_eq!(
        codec.decode(b"A B  ", STORED)?,
        DecodedFieldValue::Text("A B".to_owned())
    );
    assert_eq!(
        codec.decode(b"A B  ", PERMISSIVE)?,
        DecodedFieldValue::Text("A".to_owned())
    );
    assert_eq!(bytes(&encode(codec, FieldValue::Text("A B"))?), b"A B  ");
    assert_eq!(
        encode(codec, FieldValue::Text("AB ")),
        Err(CodecError::TextEndsWithPadding {
            offset: 2,
            padding: b' '
        })
    );
    assert_eq!(
        codec.encode(
            FieldValue::Text("A B"),
            ValueDomain::Writable,
            TextPolicy::FirstTerminator
        ),
        Err(CodecError::TextTerminator {
            offset: 1,
            value: b' ',
            padding: b' '
        })
    );
    assert_eq!(
        codec.decode(b"A\0B  ", STORED)?,
        DecodedFieldValue::Text("A\0B".to_owned())
    );
    assert_eq!(bytes(&encode(codec, FieldValue::Text("A\0B"))?), b"A\0B  ");
    assert_eq!(
        codec.decode(b"A\0B  ", PERMISSIVE)?,
        DecodedFieldValue::Text("A".to_owned())
    );
    Ok(())
}

#[test]
fn text_lengths_count_bytes_and_invalid_utf8_is_not_replaced() -> TestResult {
    let codec = FieldCodec::new(CodecSpec::FixedString {
        len: 2,
        encoding: StringEncoding::Utf8,
        padding: 0,
    })?;
    assert_eq!(bytes(&encode(codec, FieldValue::Text("é"))?), [0xC3, 0xA9]);
    assert_eq!(
        codec.decode(&[0xC3, 0xA9], STORED)?,
        DecodedFieldValue::Text("é".to_owned())
    );
    assert_eq!(
        encode(codec, FieldValue::Text("éA")),
        Err(CodecError::TextTooLong { actual: 3, max: 2 })
    );
    assert_eq!(
        codec.decode(&[b'A', 0xC3], STORED),
        Err(CodecError::InvalidUtf8 {
            valid_up_to: 1,
            error_len: None
        })
    );
    assert_eq!(
        codec.decode(&[b'A', 0xFF], STORED),
        Err(CodecError::InvalidUtf8 {
            valid_up_to: 1,
            error_len: Some(1)
        })
    );
    assert_eq!(bytes(&encode(codec, FieldValue::Text(""))?), [0, 0]);
    Ok(())
}

#[test]
fn memory_map_encoding_accepts_only_printable_ascii_semantic_bytes() -> TestResult {
    let codec = FieldCodec::new(CodecSpec::FixedString {
        len: 1,
        encoding: StringEncoding::MemoryMap,
        padding: 255,
    })?;
    for value in b' '..=b'~' {
        let source = [value];
        let text = std::str::from_utf8(&source)?;
        assert_eq!(
            codec.decode(&source, STORED)?,
            DecodedFieldValue::Text(text.to_owned())
        );
        assert_eq!(bytes(&encode(codec, FieldValue::Text(text))?), source);
    }
    for value in [0, 0x1F, 0x7F, 0x80, 0xFE] {
        assert_eq!(
            codec.decode(&[value], STORED),
            Err(CodecError::InvalidMemoryMapTextByte { offset: 0, value })
        );
    }
    for text in ["\n", "\u{7F}"] {
        assert!(matches!(
            encode(codec, FieldValue::Text(text)),
            Err(CodecError::InvalidMemoryMapTextByte { offset: 0, .. })
        ));
    }
    assert_eq!(
        codec.decode(&[255], STORED)?,
        DecodedFieldValue::Text(String::new())
    );
    Ok(())
}

#[test]
fn raw_bytes_and_value_kinds_remain_exact() -> TestResult {
    let codec = FieldCodec::new(CodecSpec::Bytes { len: 3 })?;
    let decoded = codec.decode(&[0, 0x80, 255], STORED)?;
    assert_eq!(decoded.as_field_value(), FieldValue::Bytes(&[0, 0x80, 255]));
    assert_eq!(decoded.kind(), ValueKind::Bytes);
    assert_eq!(codec.value_kind(), ValueKind::Bytes);
    assert_eq!(
        bytes(&encode(codec, decoded.as_field_value())?),
        [0, 0x80, 255]
    );
    assert_eq!(
        encode(codec, FieldValue::Bytes(&[0, 255])),
        Err(CodecError::ByteLength {
            actual: 2,
            expected: 3
        })
    );
    for value in [
        FieldValue::Unsigned(0),
        FieldValue::Signed(0),
        FieldValue::Bool(false),
        FieldValue::Text(""),
    ] {
        assert_eq!(
            encode(codec, value),
            Err(CodecError::TypeMismatch {
                expected: ValueKind::Bytes,
                actual: value.kind()
            })
        );
    }
    for (decoded, kind, name) in [
        (
            DecodedFieldValue::Unsigned(1),
            ValueKind::Unsigned,
            "unsigned",
        ),
        (DecodedFieldValue::Signed(-1), ValueKind::Signed, "signed"),
        (DecodedFieldValue::Bool(true), ValueKind::Boolean, "boolean"),
        (
            DecodedFieldValue::Text("ok".to_owned()),
            ValueKind::Text,
            "text",
        ),
        (DecodedFieldValue::Bytes(vec![0]), ValueKind::Bytes, "bytes"),
    ] {
        assert_eq!(decoded.kind(), kind);
        assert_eq!(decoded.as_field_value().kind(), kind);
        assert_eq!(kind.as_str(), name);
        assert_eq!(kind.to_string(), name);
    }
    Ok(())
}
