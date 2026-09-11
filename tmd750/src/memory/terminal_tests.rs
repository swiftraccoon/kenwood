//! Deterministic historical-image preflight tests, with no radio access.

use super::*;
use crate::memory::FieldValue;

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[test]
fn exact_firmware_gate_and_explicit_unqualified_provenance_are_distinct() -> TestResult {
    let slot = SlotIndex::new(0)?;
    let image = fixture(slot)?;
    for identity in ["1.02", "1.0", "V1.00"] {
        let firmware = FirmwareIdentity::new(identity)?;
        assert!(matches!(
            ReflectorTerminalPreflight::read(&image, &firmware, slot, TerminalUsbRoute::MainUnit),
            Err(TerminalPreflightError::UnsupportedFirmware { actual }) if actual == firmware
        ));
    }
    for identity in ["1.00", "1.02"] {
        let firmware = FirmwareIdentity::new(identity)?;
        let report = ReflectorTerminalPreflight::interpret_unqualified(
            &image,
            &firmware,
            slot,
            TerminalUsbRoute::MainUnit,
        )?;
        assert_eq!(report.firmware, firmware);
        assert_eq!(
            report.qualification,
            TextLayoutQualification::UnqualifiedInterpretation
        );
    }
    let report = read(&image, slot)?;
    assert_eq!(
        report.qualification,
        TextLayoutQualification::RegistryTargetMatched
    );
    assert!(report.findings.is_empty());
    Ok(())
}

#[test]
fn required_fields_cover_every_read_and_reuse_generated_menu_descriptors() -> TestResult {
    let fields = ReflectorTerminalPreflight::required_fields()?;
    assert_eq!(fields.len(), 15);
    let mut names = std::collections::HashSet::new();
    let slot = SlotIndex::new(5)?;
    for descriptor in fields {
        assert!(names.insert(descriptor.name));
        if descriptor.name == "format.Version" {
            assert_eq!(descriptor.base, 10);
            assert!(!descriptor.is_per_slot());
        } else {
            let generated = menu_field(descriptor.name).ok_or("missing generated descriptor")?;
            assert!(std::ptr::eq(
                descriptor,
                std::ptr::from_ref(&generated.descriptor)
            ));
        }
        if !descriptor.is_per_slot() {
            assert_eq!(descriptor.address(Some(slot))?, descriptor.address(None)?);
        }
    }
    for field in Field::SCALARS.into_iter().chain([Field::Rpt1, Field::Rpt2]) {
        assert!(names.contains(field.descriptor()?.name));
    }
    for index in 0..6 {
        assert!(names.contains(Field::MyCallsign(index).descriptor()?.name));
    }
    Ok(())
}

#[test]
fn unknown_format_and_every_scalar_are_rejected_without_normalization() -> TestResult {
    let slot = SlotIndex::new(0)?;
    for raw in [1, 2, 0xFF] {
        let mut image = fixture(slot)?;
        raw_byte(&mut image, &FORMAT, slot, raw)?;
        assert_eq!(
            read(&image, slot),
            Err(TerminalPreflightError::UnsupportedFormatVersion { actual: raw })
        );
    }
    for (field, values) in [
        (Field::ActivePm, &[6, 7, 0xFF][..]),
        (Field::UsbFunction, &[2, 0xFF][..]),
        (Field::GatewayRoute, &[3, 0xFF][..]),
        (Field::GatewayMode, &[3, 0xFF][..]),
        (Field::MySelection, &[6, 0xFF][..]),
        (Field::TerminalMode, &[2, 0xFF][..]),
    ] {
        for &value in values {
            let mut image = fixture(slot)?;
            raw_byte(&mut image, field.descriptor()?, slot, value)?;
            assert_eq!(read(&image, slot), Err(field.unknown(value)));
        }
    }
    Ok(())
}

#[test]
fn requested_pm_and_selected_my_index_control_all_captured_settings() -> TestResult {
    let slot = SlotIndex::new(5)?;
    let mut image = fixture(slot)?;
    raw_byte(&mut image, Field::MySelection.descriptor()?, slot, 5)?;
    set_text(&mut image, Field::MyCallsign(5), slot, "N0ABC  B")?;
    set_text(&mut image, Field::Rpt1, slot, "N0RPT  A")?;
    set_text(&mut image, Field::Rpt2, slot, "N0RPT  G")?;
    raw_byte(&mut image, Field::UsbFunction.descriptor()?, slot, 1)?;
    raw_byte(&mut image, Field::GatewayRoute.descriptor()?, slot, 1)?;
    raw_byte(&mut image, Field::GatewayMode.descriptor()?, slot, 2)?;
    raw_byte(&mut image, Field::TerminalMode.descriptor()?, slot, 1)?;
    let original = image.clone();
    let report = read(&image, slot)?;
    assert_eq!(report.slot, slot);
    assert_eq!(report.captured_active_slot, slot);
    assert_eq!(report.selected_my_callsign.index(), 5);
    assert_eq!(report.selected_my_callsign.to_string(), "6");
    assert_eq!(report.my_callsign, "N0ABC  B");
    assert_eq!(report.rpt1, "N0RPT  A");
    assert_eq!(report.rpt2, "N0RPT  G");
    assert_eq!(report.usb_function, TerminalUsbFunction::MassStorage);
    assert_eq!(report.gateway_route, TerminalGatewayRoute::ControlPanel);
    assert_eq!(report.gateway_mode, TerminalGatewayMode::Terminal);
    assert_eq!(report.terminal_mode, TerminalMode::Repeater);
    assert_eq!(
        report.findings,
        vec![
            TerminalFinding::RouteMismatch,
            TerminalFinding::MassStorage,
            TerminalFinding::WrongTerminalMode,
            TerminalFinding::GatewayAlreadyActive,
        ]
    );
    assert_eq!(image, original);
    Ok(())
}

#[test]
fn missing_callsign_is_distinct_from_a_conservative_suffix_warning() -> TestResult {
    let slot = SlotIndex::new(0)?;
    for text in ["", " ", "        "] {
        let mut image = fixture(slot)?;
        set_text(&mut image, Field::MyCallsign(0), slot, text)?;
        let report = read(&image, slot)?;
        assert_eq!(report.my_callsign, text);
        assert_eq!(report.findings, vec![TerminalFinding::MissingMyCallsign]);
    }
    for text in [
        "N0ABC", "N0ABC   ", "N0ABC  G", "N0ABC  I", "N0ABC  S", "N0ABC  1",
    ] {
        let mut image = fixture(slot)?;
        set_text(&mut image, Field::MyCallsign(0), slot, text)?;
        let report = read(&image, slot)?;
        assert_eq!(report.my_callsign, text);
        assert_eq!(
            report.findings,
            vec![TerminalFinding::MyCallsignSuffixNeedsReview]
        );
    }
    Ok(())
}

#[test]
fn unsafe_text_and_invalid_encoding_are_rejected_in_every_read_string() -> TestResult {
    let slot = SlotIndex::new(0)?;
    for field in [Field::MyCallsign(0), Field::Rpt1, Field::Rpt2] {
        for (value, character) in [
            ("AB\0CD", '\0'),
            ("ABC\n", '\n'),
            ("AbC", 'b'),
            ("AB/C", '/'),
            ("ABé", 'é'),
        ] {
            let mut image = fixture(slot)?;
            set_text(&mut image, field, slot, value)?;
            assert_eq!(
                read(&image, slot),
                Err(TerminalPreflightError::UnsafeText {
                    field: field.label(),
                    character
                })
            );
        }
        let mut image = fixture(slot)?;
        raw_byte(&mut image, field.descriptor()?, slot, 0xFF)?;
        assert!(
            matches!(read(&image, slot), Err(TerminalPreflightError::InvalidUtf8 { field: label, .. }) if label == field.label())
        );
    }
    Ok(())
}

#[test]
fn selected_entry_only_is_decoded_and_spaces_are_preserved() -> TestResult {
    let slot = SlotIndex::new(3)?;
    let mut image = fixture(slot)?;
    raw_byte(&mut image, Field::MyCallsign(1).descriptor()?, slot, 0xFF)?;
    set_text(&mut image, Field::Rpt1, slot, "DIRECT  ")?;
    set_text(&mut image, Field::Rpt2, slot, "AB CD")?;
    let report = read(&image, slot)?;
    assert_eq!(report.selected_my_callsign.index(), 0);
    assert_eq!(report.rpt1, "DIRECT  ");
    assert_eq!(report.rpt2, "AB CD");
    Ok(())
}

#[test]
fn route_and_mode_findings_report_captured_differences_without_changing_them() -> TestResult {
    let slot = SlotIndex::new(4)?;
    let mut image = fixture(slot)?;
    raw_byte(&mut image, Field::ActivePm.descriptor()?, slot, 0)?;
    raw_byte(&mut image, Field::GatewayMode.descriptor()?, slot, 1)?;
    raw_byte(&mut image, Field::GatewayRoute.descriptor()?, slot, 2)?;
    let report = read(&image, slot)?;
    assert_eq!(report.gateway_route, TerminalGatewayRoute::Bluetooth);
    assert_eq!(report.gateway_mode, TerminalGatewayMode::Direct);
    assert_eq!(report.captured_active_slot.index(), 0);
    assert_eq!(
        report.findings,
        vec![
            TerminalFinding::RouteMismatch,
            TerminalFinding::DifferentCapturedActivePm,
            TerminalFinding::GatewayAlreadyActive
        ]
    );
    raw_byte(&mut image, Field::GatewayRoute.descriptor()?, slot, 1)?;
    let report = ReflectorTerminalPreflight::read(
        &image,
        &FirmwareIdentity::new("1.00")?,
        slot,
        TerminalUsbRoute::ControlPanel,
    )?;
    assert!(!report.findings.contains(&TerminalFinding::RouteMismatch));
    Ok(())
}

#[test]
fn display_labels_never_leak_generated_member_names() -> TestResult {
    assert_eq!(TerminalUsbRoute::MainUnit.to_string(), "USB (Main Unit)");
    assert_eq!(TerminalUsbRoute::ControlPanel.to_string(), "USB (Panel)");
    assert_eq!(TerminalGatewayRoute::Bluetooth.to_string(), "Bluetooth");
    assert_eq!(
        TerminalUsbFunction::ComAndAudio.to_string(),
        "COM+AF In/Out"
    );
    assert_eq!(TerminalGatewayMode::Terminal.to_string(), "Terminal Mode");
    assert_eq!(TerminalMode::Reflector.to_string(), "Reflector TERM Mode");
    for field in Field::SCALARS {
        let error = field.unknown(0xFF).to_string();
        assert!(error.contains(field.label()));
        assert!(!error.contains(field.descriptor()?.name));
    }
    Ok(())
}

fn fixture(slot: SlotIndex) -> Result<MemoryImage, Box<dyn std::error::Error>> {
    let mut image = MemoryImage::from_bytes(vec![0; crate::types::IMAGE_LENGTH])?;
    raw_byte(
        &mut image,
        Field::ActivePm.descriptor()?,
        slot,
        slot.index(),
    )?;
    set_text(&mut image, Field::MyCallsign(0), slot, "N0CALL A")?;
    set_text(&mut image, Field::Rpt1, slot, "DIRECT")?;
    set_text(&mut image, Field::Rpt2, slot, "DIRECT")?;
    Ok(image)
}

fn set_text(image: &mut MemoryImage, field: Field, slot: SlotIndex, value: &str) -> TestResult {
    image.set(field.descriptor()?, Some(slot), FieldValue::Text(value))?;
    Ok(())
}

fn raw_byte(
    image: &mut MemoryImage,
    field: &FieldDescriptor,
    slot: SlotIndex,
    value: u8,
) -> TestResult {
    let start = field.address(Some(slot))?.as_usize();
    *image
        .bytes
        .get_mut(start)
        .ok_or("test field outside image")? = value;
    Ok(())
}

fn read(
    image: &MemoryImage,
    slot: SlotIndex,
) -> Result<ReflectorTerminalPreflight, TerminalPreflightError> {
    let firmware =
        FirmwareIdentity::new("1.00").map_err(|_| TerminalPreflightError::RegistryField {
            field: "test firmware",
        })?;
    ReflectorTerminalPreflight::read(image, &firmware, slot, TerminalUsbRoute::MainUnit)
}
