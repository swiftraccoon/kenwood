//! Deterministic text-layout and offline patch tests; no radio access.

use super::*;

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[test]
fn strict_constructor_rejects_unmatched_firmware() -> TestResult {
    let image = MemoryImage::blank();
    for identity in ["1.02", "1.0", "1.01", "V1.00"] {
        let firmware = FirmwareIdentity::new(identity)?;
        assert!(matches!(
            TextImage::new(&image, &firmware),
            Err(TextError::UnsupportedFirmware { actual, .. }) if actual == firmware
        ));
    }
    let firmware = FirmwareIdentity::new("1.00")?;
    let view = TextImage::new(&image, &firmware)?;
    assert_eq!(view.firmware(), &firmware);
    assert_eq!(
        view.qualification(),
        TextLayoutQualification::RegistryTargetMatched
    );
    Ok(())
}

#[test]
fn supported_keys_are_finite_unique_and_registry_backed() -> TestResult {
    let mut keys = std::collections::HashSet::new();
    let mut fields = std::collections::HashSet::new();
    assert_eq!(TextSetting::all().len(), 23);
    for &setting in TextSetting::all() {
        assert!(keys.insert(setting.key()));
        assert_eq!(setting.key().parse::<TextSetting>()?, setting);
        assert_eq!(setting.to_string(), setting.key());
        let metadata = setting.metadata()?;
        assert!(fields.insert(metadata.field_name));
        let field = menu_field(metadata.field_name).ok_or("generated field missing")?;
        assert!(!field.is_blob);
        assert_eq!(
            field.descriptor.codec,
            FieldCodec::FixedString {
                len: metadata.max_bytes,
                encoding: metadata.encoding,
                padding: metadata.padding,
            }
        );
        assert_eq!(
            field.descriptor.is_per_slot(),
            metadata.scope == TextScopeKind::PerSlot
        );
    }
    for rejected in [
        "pm-name-0",
        "pm-name-6",
        "dstar-my-callsign-7",
        "dstar-message-6",
        "PM-NAME-1",
        "pm.PmName1",
        "radio.BluetoothDeviceName",
    ] {
        assert!(matches!(
            rejected.parse::<TextSetting>(),
            Err(TextError::UnknownSetting { .. })
        ));
    }
    Ok(())
}

#[test]
fn generated_storage_constraints_are_exposed_without_ui_assumptions() -> TestResult {
    for (setting, scope, max_bytes, encoding, padding) in [
        (
            TextSetting::PmName1,
            TextScopeKind::Global,
            16,
            StringEncoding::Utf8,
            0,
        ),
        (
            TextSetting::PmName5,
            TextScopeKind::Global,
            16,
            StringEncoding::Utf8,
            0,
        ),
        (
            TextSetting::DstarMyCallsign1,
            TextScopeKind::PerSlot,
            8,
            StringEncoding::Utf8,
            0,
        ),
        (
            TextSetting::DstarMyCallsign6,
            TextScopeKind::PerSlot,
            8,
            StringEncoding::Utf8,
            0,
        ),
        (
            TextSetting::DstarMemo6,
            TextScopeKind::PerSlot,
            4,
            StringEncoding::Utf8,
            0,
        ),
        (
            TextSetting::DstarMessage5,
            TextScopeKind::PerSlot,
            20,
            StringEncoding::MemoryMap,
            0,
        ),
        (
            TextSetting::PowerOnMessage,
            TextScopeKind::PerSlot,
            16,
            StringEncoding::MemoryMap,
            0,
        ),
    ] {
        let metadata = setting.metadata()?;
        assert_eq!(metadata.scope, scope);
        assert_eq!(metadata.max_bytes, max_bytes);
        assert_eq!(metadata.encoding, encoding);
        assert_eq!(metadata.padding, padding);
    }
    Ok(())
}

#[test]
fn unqualified_interpretation_preserves_firmware_and_preview_status() -> TestResult {
    let image = image_with(TextSetting::PmName1, TextScope::Global, "HOME")?;
    for identity in ["1.02", "1.00"] {
        let firmware = FirmwareIdentity::new(identity)?;
        let view = TextImage::interpret_unqualified(&image, &firmware);
        assert_eq!(view.firmware(), &firmware);
        assert_eq!(
            view.qualification(),
            TextLayoutQualification::UnqualifiedInterpretation
        );
        assert_eq!(view.read(TextSetting::PmName1, TextScope::Global)?, "HOME");
        let preview = view.preview(TextSetting::PmName1, TextScope::Global, "FIELD")?;
        assert_eq!(preview.before(), "HOME");
        assert_eq!(preview.after(), "FIELD");
        assert_eq!(preview.firmware(), &firmware);
        assert_eq!(
            preview.qualification(),
            TextLayoutQualification::UnqualifiedInterpretation
        );
        assert_eq!(preview.setting(), TextSetting::PmName1);
        assert_eq!(preview.scope(), TextScope::Global);
        assert!(!preview.patches().is_empty());
    }
    Ok(())
}

#[test]
fn scope_confusion_is_rejected_for_reads_and_previews() -> TestResult {
    let image = MemoryImage::blank();
    let firmware = FirmwareIdentity::new("1.00")?;
    let view = TextImage::new(&image, &firmware)?;
    for (setting, scope, expected, actual) in [
        (
            TextSetting::PmName1,
            TextScope::Slot(SlotIndex::new(0)?),
            TextScopeKind::Global,
            TextScopeKind::PerSlot,
        ),
        (
            TextSetting::DstarMyCallsign1,
            TextScope::Global,
            TextScopeKind::PerSlot,
            TextScopeKind::Global,
        ),
    ] {
        let error = TextError::ScopeMismatch {
            setting,
            expected,
            actual,
        };
        assert_eq!(view.read(setting, scope), Err(error.clone()));
        assert_eq!(view.preview(setting, scope, "TEST"), Err(error));
    }
    Ok(())
}

#[test]
fn unicode_capacity_is_checked_in_bytes_without_truncation() -> TestResult {
    let image = image_with(TextSetting::PmName1, TextScope::Global, "OLD")?;
    let firmware = FirmwareIdentity::new("1.00")?;
    let view = TextImage::new(&image, &firmware)?;
    let exact = "é".repeat(8);
    let preview = view.preview(TextSetting::PmName1, TextScope::Global, &exact)?;
    assert_eq!(preview.after(), exact);
    assert_eq!(
        preview.qualification(),
        TextLayoutQualification::RegistryTargetMatched
    );
    let mut patched = image.as_bytes().to_vec();
    preview.patches().apply_to_image(&mut patched);
    assert_eq!(patched.get(0x4F00A..0x4F01A), Some(exact.as_bytes()));
    assert!(matches!(
        view.preview(TextSetting::PmName1, TextScope::Global, &"é".repeat(9)),
        Err(TextError::Schema(SchemaError::TextTooLong {
            len: 18,
            max: 16,
            ..
        }))
    ));
    assert_eq!(view.read(TextSetting::PmName1, TextScope::Global)?, "OLD");
    Ok(())
}

#[test]
fn memory_map_strings_reject_non_ascii_and_keep_exact_capacity() -> TestResult {
    let scope = TextScope::Slot(SlotIndex::new(5)?);
    let firmware = FirmwareIdentity::new("1.00")?;
    for setting in [TextSetting::DstarMessage1, TextSetting::PowerOnMessage] {
        let image = image_with(setting, scope, "OLD")?;
        let view = TextImage::new(&image, &firmware)?;
        let exact = "X".repeat(setting.metadata()?.max_bytes);
        assert_eq!(view.preview(setting, scope, &exact)?.after(), exact);
        assert!(matches!(
            view.preview(setting, scope, "café"),
            Err(TextError::Schema(SchemaError::TextByte { .. }))
        ));
        assert!(matches!(
            view.preview(setting, scope, &(exact + "X")),
            Err(TextError::Schema(SchemaError::TextTooLong { .. }))
        ));
    }
    Ok(())
}

#[test]
fn input_control_characters_are_refused_before_patch_creation() -> TestResult {
    let setting = TextSetting::PmName1;
    let image = image_with(setting, TextScope::Global, "OLD")?;
    let firmware = FirmwareIdentity::new("1.00")?;
    let view = TextImage::new(&image, &firmware)?;
    for character in ['\0', '\n', '\r', '\t', '\u{1b}', '\u{7f}', '\u{85}'] {
        assert_eq!(
            view.preview(setting, TextScope::Global, &format!("A{character}B")),
            Err(TextError::ControlCharacter { setting, character })
        );
    }
    Ok(())
}

#[test]
fn decoding_keeps_interior_spaces_and_trims_only_trailing_padding() -> TestResult {
    let setting = TextSetting::DstarMyCallsign1;
    let scope = TextScope::Slot(SlotIndex::new(0)?);
    let image = image_with(setting, scope, "AB CD")?;
    let firmware = FirmwareIdentity::new("1.00")?;
    let view = TextImage::new(&image, &firmware)?;
    assert_eq!(
        image.as_bytes().get(0x51008..0x51010),
        Some(b"AB CD\0\0\0".as_slice())
    );
    assert_eq!(view.read(setting, scope)?, "AB CD");
    let preview = view.preview(setting, scope, "EF G  ")?;
    assert_eq!(preview.after(), "EF G  ");
    let mut patched = image.as_bytes().to_vec();
    preview.patches().apply_to_image(&mut patched);
    assert_eq!(
        patched.get(0x51008..0x51010),
        Some(b"EF G  \0\0".as_slice())
    );
    let image = MemoryImage::from_bytes(patched)?;
    assert_eq!(
        TextImage::new(&image, &firmware)?.read(setting, scope)?,
        "EF G  "
    );
    Ok(())
}

#[test]
fn nul_filled_callsign_is_empty_under_both_explicit_layout_modes() -> TestResult {
    let image = MemoryImage::from_bytes(vec![0; crate::types::IMAGE_LENGTH])?;
    let setting = TextSetting::DstarMyCallsign1;
    let scope = TextScope::Slot(SlotIndex::new(0)?);
    let registry_firmware = FirmwareIdentity::new("1.00")?;
    let unqualified_firmware = FirmwareIdentity::new("1.02")?;
    assert_eq!(
        image.as_bytes().get(0x51008..0x51010),
        Some([0; 8].as_slice())
    );
    for view in [
        TextImage::new(&image, &registry_firmware)?,
        TextImage::interpret_unqualified(&image, &unqualified_firmware),
    ] {
        assert_eq!(view.read(setting, scope)?, "");
        let preview = view.preview(setting, scope, "KQ4NIT")?;
        assert_eq!(preview.before(), "");
        assert_eq!(preview.after(), "KQ4NIT");
        assert_eq!(preview.firmware(), view.firmware());
        assert_eq!(preview.qualification(), view.qualification());
        let mut bytes = image.as_bytes().to_vec();
        preview.patches().apply_to_image(&mut bytes);
        assert_eq!(bytes.get(0x51008..0x51010), Some(b"KQ4NIT\0\0".as_slice()));
    }
    Ok(())
}

#[test]
fn corrupt_stored_text_is_not_silently_truncated_or_replacement_decoded() -> TestResult {
    let setting = TextSetting::PmName1;
    let firmware = FirmwareIdentity::new("1.00")?;
    let image = image_with(setting, TextScope::Global, "AB\0CD")?;
    let view = TextImage::new(&image, &firmware)?;
    assert_eq!(
        view.read(setting, TextScope::Global),
        Err(TextError::ControlCharacter {
            setting,
            character: '\0'
        })
    );
    assert!(matches!(
        view.preview(setting, TextScope::Global, "NEW"),
        Err(TextError::ControlCharacter { .. })
    ));
    let mut bytes = image.into_bytes();
    *bytes.get_mut(0x4F00A).ok_or("PM name start missing")? = 0xFF;
    let invalid = MemoryImage::from_bytes(bytes)?;
    assert!(matches!(
        TextImage::new(&invalid, &firmware)?.read(setting, TextScope::Global),
        Err(TextError::InvalidUtf8 { .. })
    ));
    let setting = TextSetting::PowerOnMessage;
    let scope = TextScope::Slot(SlotIndex::new(0)?);
    let mut bytes = image_with(setting, scope, "ABC")?.into_bytes();
    *bytes
        .get_mut(0x50571)
        .ok_or("power-on message byte missing")? = 0;
    let image = MemoryImage::from_bytes(bytes)?;
    assert!(matches!(
        TextImage::new(&image, &firmware)?.read(setting, scope),
        Err(TextError::Schema(SchemaError::TextByte { value: 0, .. }))
    ));
    Ok(())
}

#[test]
fn every_setting_round_trips_without_touching_other_image_bytes() -> TestResult {
    let firmware = FirmwareIdentity::new("1.00")?;
    for &setting in TextSetting::all() {
        let scopes: Vec<_> = match setting.metadata()?.scope {
            TextScopeKind::Global => vec![TextScope::Global],
            TextScopeKind::PerSlot => SlotIndex::all().into_iter().map(TextScope::Slot).collect(),
        };
        for scope in scopes {
            let image = image_with(setting, scope, "OLD")?;
            let original = image.clone();
            let view = TextImage::new(&image, &firmware)?;
            let preview = view.preview(setting, scope, "NEW")?;
            assert_eq!(preview.before(), "OLD");
            assert_eq!(preview.after(), "NEW");
            let mut patched = image.as_bytes().to_vec();
            preview.patches().apply_to_image(&mut patched);
            let mut expected = image.clone();
            expected.set(
                &setting.field()?.descriptor,
                scope.slot(),
                FieldValue::Text("NEW"),
            )?;
            assert_eq!(patched, expected.as_bytes());
            assert_eq!(image, original);
            assert_eq!(
                TextImage::new(&expected, &firmware)?.read(setting, scope)?,
                "NEW"
            );
        }
    }
    Ok(())
}

#[test]
fn per_slot_preview_uses_the_selected_slot_and_clears_old_string_tail() -> TestResult {
    let setting = TextSetting::DstarMyCallsign1;
    let scope = TextScope::Slot(SlotIndex::new(5)?);
    let image = image_with(setting, scope, "ABCDEFGH")?;
    let firmware = FirmwareIdentity::new("1.00")?;
    let preview = TextImage::new(&image, &firmware)?.preview(setting, scope, "A")?;
    let mut bytes = image.as_bytes().to_vec();
    preview.patches().apply_to_image(&mut bytes);
    assert_eq!(
        bytes.get(0x5B008..0x5B010),
        Some(b"A\0\0\0\0\0\0\0".as_slice())
    );
    assert_eq!(bytes.get(0x51008..0x51010), Some([0xFF; 8].as_slice()));
    let image = MemoryImage::from_bytes(bytes)?;
    let preview = TextImage::new(&image, &firmware)?.preview(setting, scope, "")?;
    assert_eq!(preview.before(), "A");
    assert_eq!(preview.after(), "");
    let mut bytes = image.into_bytes();
    preview.patches().apply_to_image(&mut bytes);
    assert_eq!(bytes.get(0x5B008..0x5B010), Some([0; 8].as_slice()));
    Ok(())
}

fn image_with(
    setting: TextSetting,
    scope: TextScope,
    value: &str,
) -> Result<MemoryImage, Box<dyn std::error::Error>> {
    let mut image = MemoryImage::blank();
    image.set(
        &setting.field()?.descriptor,
        scope.slot(),
        FieldValue::Text(value),
    )?;
    Ok(image)
}
