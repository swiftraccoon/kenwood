//! Public schema invariants, including rejected-operation atomicity.

use super::*;
use crate::memory::menu_field;

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[test]
fn model_decoding_keeps_stored_domains_nonzero_booleans_and_first_terminators() -> TestResult {
    let number = FieldDescriptor::new("test.Number", 8, FieldCodec::Byte { min: 0, max: 2 });
    let flag = FieldDescriptor::new("test.Flag", 9, FieldCodec::Bool);
    let text = FieldDescriptor::new(
        "test.Text",
        10,
        FieldCodec::FixedString {
            len: 5,
            encoding: StringEncoding::Utf8,
            padding: b' ',
        },
    );
    let mut image = [0; 16];
    *image.get_mut(8).ok_or("number byte")? = 255;
    *image.get_mut(9).ok_or("boolean byte")? = 7;
    image
        .get_mut(10..15)
        .ok_or("text span")?
        .copy_from_slice(b"A \xFFBC");
    assert_eq!(number.read(&image, None)?, DecodedFieldValue::Unsigned(255));
    assert_eq!(flag.read(&image, None)?, DecodedFieldValue::Bool(true));
    assert_eq!(
        text.read(&image, None)?,
        DecodedFieldValue::Text("A".to_owned())
    );
    image
        .get_mut(10..15)
        .ok_or("text span")?
        .copy_from_slice(b"B\0\xFFCD");
    assert_eq!(
        text.read(&image, None)?,
        DecodedFieldValue::Text("B".to_owned())
    );
    assert!(number.encode(FieldValue::Unsigned(255)).is_err());
    assert!(text.encode(FieldValue::Text("A B")).is_err());
    assert!(text.encode(FieldValue::Text("A\0B")).is_err());
    Ok(())
}

#[test]
fn scalar_types_are_shared_without_converting_or_weakening_model_metadata() -> TestResult {
    use kenwood_schema::{
        FieldCodec as SharedCodec, FieldValue as SharedValue, MaskedByte as SharedMaskedByte,
    };

    let codec: SharedCodec = FieldCodec::Byte { min: 0, max: 2 };
    let field = FieldDescriptor::new("test.Shared", 8, codec);
    let value: SharedValue<'_> = FieldValue::Unsigned(2);
    let encoded: Vec<SharedMaskedByte> = field.encode(value)?;
    assert_eq!(encoded, vec![MaskedByte::new(0, 0xFF, 2)?]);
    Ok(())
}

#[test]
fn registered_bitmap_round_trips_offline_in_the_last_pm_slot() -> TestResult {
    let field = menu_field("radio.PoweronBitmap").ok_or("registered bitmap")?;
    let slot = Some(SlotIndex::new(5)?);
    let bytes: Vec<u8> = (0..256_000)
        .map(|index| u8::try_from(index % 251))
        .collect::<Result<_, _>>()?;
    let mut image = crate::memory::MemoryImage::blank();
    image.set(&field.descriptor, slot, FieldValue::Bytes(&bytes))?;
    let start = field.descriptor.address(slot)?.as_usize();
    let end = start + field.descriptor.codec.encoded_len();
    assert_eq!(
        end, 1_929_216,
        "last bitmap slot must retain its exact stride and length"
    );
    assert_eq!(
        field.descriptor.read(image.as_bytes(), slot)?,
        DecodedFieldValue::Bytes(bytes.clone()),
        "registered binary storage must remain available for offline image editing"
    );
    assert_eq!(
        image.as_bytes().get(start..end),
        Some(bytes.as_slice()),
        "offline encoding must preserve every bitmap byte"
    );
    assert!(
        image
            .as_bytes()
            .get(..start)
            .ok_or("bitmap prefix")?
            .iter()
            .all(|byte| *byte == 0xFF),
        "offline bitmap encoding must not modify the preceding image"
    );
    assert!(
        image
            .as_bytes()
            .get(end..)
            .ok_or("bitmap suffix")?
            .iter()
            .all(|byte| *byte == 0xFF),
        "offline bitmap encoding must not modify the trailing image"
    );
    Ok(())
}

#[test]
fn every_planner_entry_retains_registered_blob_rejection() -> TestResult {
    let field = menu_field("radio.PoweronBitmap").ok_or("registered bitmap")?;
    let weakened = crate::memory::MenuField {
        is_blob: false,
        ..*field
    };
    let bytes = vec![0xA5; field.descriptor.codec.encoded_len()];
    let slot = Some(SlotIndex::new(5)?);
    for metadata in [None, Some(field), Some(&weakened)] {
        let mut planner = PatchPlanner::new();
        let result = if let Some(selected) = metadata {
            planner.set_menu(selected, slot, FieldValue::Bytes(&bytes))
        } else {
            planner.set(&field.descriptor, slot, FieldValue::Bytes(&bytes))
        };
        assert!(
            matches!(
                result,
                Err(SchemaError::BlobNotPatchable {
                    field: "radio.PoweronBitmap"
                })
            ),
            "offline blob encoding must not grant scalar-planner admission"
        );
        assert!(
            planner.finish()?.is_empty(),
            "rejected bitmap must not leave any byte claims"
        );
    }
    Ok(())
}

#[test]
fn malformed_codecs_are_rejected_before_reading_or_encoding() {
    for codec in [
        FieldCodec::Unsigned {
            width: 0,
            endian: Endian::Little,
            min: 0,
            max: 0,
        },
        FieldCodec::Unsigned {
            width: 9,
            endian: Endian::Little,
            min: 0,
            max: 1,
        },
        FieldCodec::Unsigned {
            width: 1,
            endian: Endian::Little,
            min: 0,
            max: 1000,
        },
        FieldCodec::Signed {
            width: 0,
            endian: Endian::Little,
            min: 0,
            max: 0,
        },
        FieldCodec::Signed {
            width: 9,
            endian: Endian::Little,
            min: 0,
            max: 1,
        },
        FieldCodec::Signed {
            width: 1,
            endian: Endian::Little,
            min: -129,
            max: 127,
        },
        FieldCodec::Byte { min: 5, max: 1 },
        FieldCodec::BitField {
            mask: 0xFF,
            shift: 8,
            min: 0,
            max: 1,
        },
        FieldCodec::BitField {
            mask: 0x05,
            shift: 0,
            min: 0,
            max: 5,
        },
        FieldCodec::BitField {
            mask: 0x03,
            shift: 0,
            min: 0,
            max: 4,
        },
        FieldCodec::BitBool { mask: 0 },
        FieldCodec::BitBool { mask: 3 },
        FieldCodec::FixedString {
            len: 0,
            encoding: StringEncoding::Utf8,
            padding: 0,
        },
        FieldCodec::Bytes { len: 0 },
    ] {
        let field = FieldDescriptor::new("test.Invalid", 8, codec);
        assert!(
            field.address(None).is_err(),
            "malformed codec must not resolve: {codec:?}"
        );
        assert!(
            field.read(&[0; 32], None).is_err(),
            "malformed codec must not decode: {codec:?}"
        );
        let value = match codec {
            FieldCodec::Signed { .. } => FieldValue::Signed(0),
            FieldCodec::Bool | FieldCodec::BitBool { .. } => FieldValue::Bool(false),
            FieldCodec::FixedString { .. } => FieldValue::Text(""),
            FieldCodec::Bytes { .. } => FieldValue::Bytes(&[]),
            _ => FieldValue::Unsigned(0),
        };
        assert!(
            field.encode(value).is_err(),
            "malformed codec must not encode: {codec:?}"
        );
    }
}

#[test]
fn oversized_field_lengths_return_an_error_without_overflow() {
    let field = FieldDescriptor::new("test.Huge", 8, FieldCodec::Bytes { len: usize::MAX });
    assert!(
        field.address(None).is_err(),
        "huge byte length must fail before address addition"
    );
    assert!(
        field.read(&[], None).is_err(),
        "huge byte length must not panic while reading"
    );
}

#[test]
fn every_supported_integer_width_round_trips_its_extreme_values() -> TestResult {
    for width in 1..=8 {
        let magnitude = 1_i128 << (u32::from(width) * 8 - 1);
        let minimum = i64::try_from(-magnitude)?;
        let maximum = i64::try_from(magnitude - 1)?;
        let unsigned_maximum = u64::try_from((1_u128 << (u32::from(width) * 8)) - 1)?;
        for endian in [Endian::Little, Endian::Big] {
            let unsigned = FieldDescriptor::new(
                "test.Unsigned",
                8,
                FieldCodec::Unsigned {
                    width,
                    endian,
                    min: 0,
                    max: unsigned_maximum,
                },
            );
            let signed = FieldDescriptor::new(
                "test.Signed",
                8,
                FieldCodec::Signed {
                    width,
                    endian,
                    min: minimum,
                    max: maximum,
                },
            );
            for (field, value) in [
                (unsigned, FieldValue::Unsigned(0)),
                (unsigned, FieldValue::Unsigned(unsigned_maximum)),
                (signed, FieldValue::Signed(minimum)),
                (signed, FieldValue::Signed(maximum)),
            ] {
                let mut image = crate::memory::MemoryImage::blank();
                image.set(&field, None, value)?;
                assert_eq!(
                    field.read(image.as_bytes(), None)?.as_field_value(),
                    value,
                    "{width}-byte {endian:?} storage must not truncate extreme values"
                );
            }
        }
    }
    Ok(())
}

#[test]
fn every_contiguous_bitfield_shape_preserves_neighbor_bits() -> TestResult {
    for shift in 0..8 {
        for width in 1..=8 - shift {
            let maximum = u8::try_from((1_u16 << width) - 1)?;
            let mask = maximum << shift;
            let field = FieldDescriptor::new(
                "test.Bits",
                8,
                FieldCodec::BitField {
                    mask,
                    shift,
                    min: 0,
                    max: maximum,
                },
            );
            for value in [0, u64::from(maximum)] {
                let mut image = [0xA5; 16];
                for patch in field.encode(FieldValue::Unsigned(value))? {
                    let byte = image.get_mut(8 + patch.offset()).ok_or("bitfield byte")?;
                    *byte = patch.apply(*byte);
                }
                assert_eq!(
                    field.read(&image, None)?,
                    DecodedFieldValue::Unsigned(value),
                    "contiguous mask {mask:#04X}, shift {shift} must round-trip both extrema"
                );
                assert_eq!(
                    image.get(8).copied().ok_or("bitfield byte")? & !mask,
                    0xA5 & !mask,
                    "mask {mask:#04X} must preserve every neighboring bit"
                );
            }
            assert!(
                field
                    .encode(FieldValue::Unsigned(u64::from(maximum) + 1))
                    .is_err(),
                "mask {mask:#04X} must reject a value above its capacity"
            );
        }
        let mask = 1 << shift;
        let flag = FieldDescriptor::new("test.Flag", 8, FieldCodec::BitBool { mask });
        for enabled in [false, true] {
            let encoded = flag.encode(FieldValue::Bool(enabled))?;
            assert_eq!(
                encoded,
                vec![MaskedByte::new(0, mask, if enabled { mask } else { 0 })?],
                "every single-bit boolean mask must encode without rejecting valid shapes"
            );
        }
    }
    Ok(())
}

#[test]
fn direct_text_encoding_rejects_storage_terminators() {
    for (padding, text) in [(0, "A\0B"), (b' ', "A B")] {
        let field = FieldDescriptor::new(
            "test.Text",
            8,
            FieldCodec::FixedString {
                len: 8,
                encoding: StringEncoding::Utf8,
                padding,
            },
        );
        assert!(
            field.encode(FieldValue::Text(text)).is_err(),
            "descriptor encoding must not accept text that its decoder would shorten"
        );
    }
}

#[test]
fn direct_descriptor_planning_cannot_bypass_registered_domains() -> TestResult {
    let field = menu_field("aprs.ObjectList[0].ObjectTable").ok_or("object table field")?;
    let slot = Some(SlotIndex::new(0)?);
    let mut planner = PatchPlanner::new();
    assert!(
        planner
            .set(&field.descriptor, slot, FieldValue::Unsigned(58))
            .is_err(),
        "raw descriptor planning must enforce the registered finite domain"
    );
    assert!(
        planner.finish()?.is_empty(),
        "a rejected domain value must leave no claims"
    );
    let mut altered = field.descriptor;
    altered.base += 1;
    assert!(
        altered.encode(FieldValue::Unsigned(47)).is_err(),
        "a registered name cannot authorize altered descriptor metadata"
    );
    assert!(
        altered.read(&vec![0; IMAGE_LENGTH], slot).is_err(),
        "a registered name cannot decode using altered descriptor metadata"
    );
    let weakened = crate::memory::MenuField {
        allowed_values: &[],
        ..*field
    };
    let mut planner = PatchPlanner::new();
    assert!(
        planner
            .set_menu(&weakened, slot, FieldValue::Unsigned(58))
            .is_err(),
        "mutable menu metadata must not weaken the compiled descriptor domain"
    );
    assert!(
        planner.finish()?.is_empty(),
        "weakened metadata must not leave accepted claims"
    );
    Ok(())
}

#[test]
fn rejected_assignment_never_retains_a_writable_prefix() -> TestResult {
    let crossing = FieldDescriptor::new("test.Crossing", 47, FieldCodec::Bytes { len: 2 });
    let mut planner = PatchPlanner::new();
    assert!(
        planner
            .set(&crossing, None, FieldValue::Bytes(&[1, 2]))
            .is_err(),
        "the second byte is outside the writable region"
    );
    assert!(
        planner.finish()?.is_empty(),
        "rejected field prefix must not remain planned"
    );
    Ok(())
}

#[test]
fn identical_repeated_assignments_are_idempotent() -> TestResult {
    let flag = FieldDescriptor::new("test.Flag", 8, FieldCodec::BitBool { mask: 0x01 });
    let alias = FieldDescriptor::new("test.Alias", 8, FieldCodec::BitBool { mask: 0x01 });
    let neighboring = FieldDescriptor::new("test.Neighbor", 8, FieldCodec::BitBool { mask: 0x02 });
    let mut planner = PatchPlanner::new();
    let _planned = planner.set(&flag, None, FieldValue::Bool(true))?;
    let _planned = planner.set(&flag, None, FieldValue::Bool(true))?;
    let _planned = planner.set(&alias, None, FieldValue::Bool(true))?;
    let _planned = planner.set(&neighboring, None, FieldValue::Bool(false))?;
    let patches = planner.finish()?;
    let mut image = vec![0xFE; 48];
    patches.apply_to_image(&mut image)?;
    assert_eq!(image.get(8), Some(&0xFD));
    assert_eq!(patches.len(), 1);
    assert_eq!(
        patches.pages().first().ok_or("planned page")?.bytes().len(),
        1
    );
    Ok(())
}

#[test]
fn a_late_conflict_leaves_exactly_the_prior_plan() -> TestResult {
    let existing = FieldDescriptor::new("test.Existing", 9, FieldCodec::Byte { min: 0, max: 255 });
    let conflicting = FieldDescriptor::new("test.Conflict", 8, FieldCodec::Bytes { len: 2 });
    let mut planner = PatchPlanner::new();
    let _planned = planner.set(&existing, None, FieldValue::Unsigned(7))?;
    assert!(
        planner
            .set(&conflicting, None, FieldValue::Bytes(&[1, 2]))
            .is_err(),
        "the second byte must conflict with the prior assignment"
    );
    let mut expected = PatchPlanner::new();
    let _planned = expected.set(&existing, None, FieldValue::Unsigned(7))?;
    assert_eq!(
        planner.finish()?,
        expected.finish()?,
        "failed assignment must preserve the exact prior plan"
    );
    Ok(())
}

#[test]
fn image_application_rejects_missing_pages_before_any_mutation() -> TestResult {
    let mut planner = PatchPlanner::new();
    for address in [8, 56] {
        let field =
            FieldDescriptor::new("test.Byte", address, FieldCodec::Byte { min: 0, max: 255 });
        let _planned = planner.set(&field, None, FieldValue::Unsigned(7))?;
    }
    let patches = planner.finish()?;
    let mut image = vec![0; 48];
    assert!(
        patches.apply_to_image(&mut image).is_err(),
        "absent second page must be reported"
    );
    assert_eq!(
        image,
        vec![0; 48],
        "failed image application must not modify earlier pages"
    );
    Ok(())
}
