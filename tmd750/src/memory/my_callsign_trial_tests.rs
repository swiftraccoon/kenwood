use super::*;
use crate::memory::{
    MenuOption, PmNameTrialWrite, StorageTransform, Term, is_supported_schema_target,
};
use crate::types::{FirmwareIdentity, RadioType};

type TestResult = Result<(), Box<dyn std::error::Error>>;

struct Fixture {
    trial: MyCallsignTrial,
    identity: Identity,
    original: [u8; PAGE_SIZE],
    expected: [u8; PAGE_SIZE],
    control: [u8; PAGE_SIZE],
}

fn connection(value: u64) -> Result<NonZeroU64, Box<dyn std::error::Error>> {
    NonZeroU64::new(value).ok_or_else(|| "test connection ID must be nonzero".into())
}

fn make_fixture() -> Result<Fixture, Box<dyn std::error::Error>> {
    let identity = Identity {
        model: RadioModel::TmD750,
        firmware: FirmwareIdentity::new("1.02")?,
        radio_type: RadioType::new("K,2,1")?,
    };
    let mut original = [0xA5; PAGE_SIZE];
    original.get_mut(0..2).ok_or("target selectors")?.fill(0);
    original.get_mut(8..16).ok_or("MY1 field")?.fill(0);
    let mut control = [0x5A; PAGE_SIZE];
    *control.get_mut(9).ok_or("PM selector")? = 0;
    let trial = MyCallsignTrial::prepare_unqualified_offline(&identity, &original, &control)?;
    let expected = *trial.expected_page();
    Ok(Fixture {
        trial,
        identity,
        original,
        expected,
        control,
    })
}

fn fresh(fixture: &mut Fixture, session: u64, changed: bool) -> TestResult {
    fixture.trial.fresh_session(FixedTextTrialObservation {
        id: connection(session)?,
        identity: &fixture.identity,
        memory_format: 0,
        whole_page: if changed {
            &fixture.expected
        } else {
            &fixture.original
        },
        control_page: Some(&fixture.control),
        gateway_mode: Some(DvGatewayMode::Off),
    })?;
    Ok(())
}

fn intent(fixture: &mut Fixture, write: PmNameTrialWrite) -> TestResult {
    fixture.trial.record(PmNameTrialEvent::DurableWriteIntent {
        id: connection(match write {
            PmNameTrialWrite::Rename => 11,
            PmNameTrialWrite::Restore => 12,
        })?,
        write,
    })?;
    Ok(())
}

fn readback(fixture: &mut Fixture, changed: bool) -> TestResult {
    fixture.trial.record(PmNameTrialEvent::ImmediateReadback {
        whole_page: if changed {
            &fixture.expected
        } else {
            &fixture.original
        },
    })?;
    Ok(())
}

fn renamed(fixture: &mut Fixture) -> TestResult {
    fresh(fixture, 1, false)?;
    intent(fixture, PmNameTrialWrite::Rename)?;
    readback(fixture, true)?;
    fixture.trial.finalize_session(connection(1)?)?;
    Ok(())
}

#[test]
fn preparation_pins_my1_and_preserves_every_unrelated_and_control_byte() -> TestResult {
    let fixture = make_fixture()?;
    assert_eq!(fixture.trial.page(), MyCallsignTrial::required_page()?);
    assert_eq!(fixture.trial.page().address().as_u32(), 331_776);
    assert_eq!(fixture.trial.page().len(), PAGE_SIZE);
    assert_eq!(
        fixture.trial.control_page_spec(),
        MyCallsignTrial::required_control_page()?
    );
    assert_eq!(
        fixture.trial.control_page_spec().address().as_u32(),
        323_584
    );
    assert_eq!(fixture.trial.control_page_spec().len(), PAGE_SIZE);
    assert_eq!(fixture.trial.control_page(), &fixture.control);
    assert_eq!(fixture.trial.original_page(), &fixture.original);
    assert_eq!(fixture.trial.identity(), &fixture.identity);
    assert_eq!(fixture.expected.get(8..16), Some(b"KQ4NIT\0\0".as_slice()));
    for (index, (original, expected)) in fixture.original.iter().zip(&fixture.expected).enumerate()
    {
        if !(8..16).contains(&index) {
            assert_eq!(original, expected, "unrelated target byte {index}");
        }
    }
    assert_eq!(
        fixture.expected.get(2),
        Some(&0xA5),
        "terminal subtype is preserved, not admitted as a separate setting"
    );
    assert_eq!(fixture.trial.status(), PmNameTrialStatus::NotWritten);
    assert_eq!(fixture.trial.next_session()?, PmNameTrialSession::Rename);
    assert_eq!(fixture.trial.scope(), FixedTextTrialScope::My1);
    assert_eq!(
        fixture.trial.guard_page(),
        Some(fixture.trial.control_page_spec())
    );
    assert!(
        !is_supported_schema_target(fixture.identity.model, &fixture.identity.firmware),
        "fixed preparation must not relax the generic firmware gate"
    );
    Ok(())
}

#[test]
fn preparation_requires_exact_identity_and_both_complete_pages() -> TestResult {
    let fixture = make_fixture()?;
    for length in [0, 8, 255, 257, 512] {
        let result = MyCallsignTrial::prepare_unqualified_offline(
            &fixture.identity,
            &vec![0; length],
            &fixture.control,
        );
        assert!(
            matches!(result, Err(PmNameTrialError::PageLength { actual }) if actual == length),
            "target length {length}: {result:?}"
        );
        let result = MyCallsignTrial::prepare_unqualified_offline(
            &fixture.identity,
            &fixture.original,
            &vec![0; length],
        );
        assert!(
            matches!(result, Err(PmNameTrialError::ControlPageLength { actual }) if actual == length),
            "control length {length}: {result:?}"
        );
    }
    for firmware in ["1.00", "1.01", "1.03", "01.02", "1.02E"] {
        let mut identity = fixture.identity.clone();
        identity.firmware = FirmwareIdentity::new(firmware)?;
        let result = MyCallsignTrial::prepare_unqualified_offline(
            &identity,
            &fixture.original,
            &fixture.control,
        );
        assert!(
            matches!(result, Err(PmNameTrialError::IdentityMismatch)),
            "firmware {firmware}: {result:?}"
        );
    }
    for radio_type in ["J,2,1", "K,1,1", "K,2,0"] {
        let mut identity = fixture.identity.clone();
        identity.radio_type = RadioType::new(radio_type)?;
        let result = MyCallsignTrial::prepare_unqualified_offline(
            &identity,
            &fixture.original,
            &fixture.control,
        );
        assert!(
            matches!(result, Err(PmNameTrialError::IdentityMismatch)),
            "type {radio_type}: {result:?}"
        );
    }
    Ok(())
}

#[test]
fn every_stored_selector_requires_the_fixed_zero_value() -> TestResult {
    let fixture = make_fixture()?;
    for value in [1, 2, 5, 6, 255] {
        let expected_gateway = if value == 2 {
            DvGatewayMode::Terminal
        } else {
            DvGatewayMode::Unqualified(value)
        };
        let mut control = fixture.control;
        *control.get_mut(9).ok_or("PM selector")? = value;
        let result = MyCallsignTrial::prepare_unqualified_offline(
            &fixture.identity,
            &fixture.original,
            &control,
        );
        assert!(
            matches!(result, Err(PmNameTrialError::PmSelection { actual }) if actual == value),
            "PM selector {value}: {result:?}"
        );
        let mut target = fixture.original;
        *target.first_mut().ok_or("Gateway selector")? = value;
        let result = MyCallsignTrial::prepare_unqualified_offline(
            &fixture.identity,
            &target,
            &fixture.control,
        );
        assert!(
            matches!(result, Err(PmNameTrialError::GatewayMode { actual }) if actual == expected_gateway),
            "Gateway selector {value}: {result:?}"
        );
        let mut target = fixture.original;
        *target.get_mut(1).ok_or("MY selector")? = value;
        let result = MyCallsignTrial::prepare_unqualified_offline(
            &fixture.identity,
            &target,
            &fixture.control,
        );
        assert!(
            matches!(result, Err(PmNameTrialError::MySelection { actual }) if actual == value),
            "MY selector {value}: {result:?}"
        );
    }
    Ok(())
}

#[test]
fn every_original_my1_byte_must_be_nul_not_erased_or_space_padded() -> TestResult {
    let fixture = make_fixture()?;
    for offset in 8..16 {
        for value in [b' ', b'K', 255] {
            let mut target = fixture.original;
            *target.get_mut(offset).ok_or("MY1 byte")? = value;
            let result = MyCallsignTrial::prepare_unqualified_offline(
                &fixture.identity,
                &target,
                &fixture.control,
            );
            assert!(
                matches!(result, Err(PmNameTrialError::MyCallsignNotEmpty)),
                "MY1 offset {offset}, value {value}: {result:?}"
            );
        }
    }
    let result = MyCallsignTrial::prepare_unqualified_offline(
        &fixture.identity,
        &fixture.expected,
        &fixture.control,
    );
    assert!(
        matches!(result, Err(PmNameTrialError::MyCallsignNotEmpty)),
        "already-temporary target is not an approved empty baseline: {result:?}"
    );
    Ok(())
}

fn reject_changed_descriptor(
    field_index: usize,
    change: impl FnOnce(&mut MenuField),
) -> TestResult {
    let fixture = make_fixture()?;
    let loaded = Descriptors::load()?;
    let mut fields = [
        *loaded.callsign,
        *loaded.gateway,
        *loaded.my_selection,
        *loaded.pm_selection,
    ];
    change(fields.get_mut(field_index).ok_or("descriptor index")?);
    let [callsign, gateway, my_selection, pm_selection] = &fields;
    let descriptors = Descriptors {
        callsign,
        gateway,
        my_selection,
        pm_selection,
    };
    let result = MyCallsignTrial::prepare_with_fields(
        &fixture.identity,
        &fixture.original,
        &fixture.control,
        &descriptors,
    );
    assert!(
        matches!(result, Err(PmNameTrialError::UnsupportedDescriptor)),
        "changed descriptor {field_index}: {result:?}"
    );
    Ok(())
}

#[test]
fn every_target_and_guard_descriptor_shape_is_load_bearing() -> TestResult {
    const REPEATED_OPTION: MenuOption = MenuOption {
        raw: 0,
        member: "repeated",
        label: None,
        resource_key: None,
    };
    for index in 0..4 {
        reject_changed_descriptor(index, |field| field.descriptor.name = "other.Field")?;
        reject_changed_descriptor(index, |field| field.descriptor.base += 1)?;
        reject_changed_descriptor(index, |field| field.descriptor.codec = FieldCodec::Bool)?;
        reject_changed_descriptor(index, |field| field.menu = "other")?;
        reject_changed_descriptor(index, |field| field.is_blob = true)?;
        reject_changed_descriptor(index, |field| field.allowed_values = &[0])?;
        reject_changed_descriptor(index, |field| {
            field.storage_transform = Some(StorageTransform {
                input_unit: "other",
                numerator: 1,
                denominator: 1,
            });
        })?;
        reject_changed_descriptor(index, |field| {
            field.enum_type = if field.enum_type.is_some() {
                None
            } else {
                Some("unexpected")
            }
        })?;
    }
    for index in 0..3 {
        reject_changed_descriptor(index, |field| field.descriptor.terms = &[])?;
        reject_changed_descriptor(index, |field| {
            field.descriptor.terms = &[Term {
                dimension: "pm_slot",
                stride: 8193,
            }];
        })?;
        reject_changed_descriptor(index, |field| {
            field.descriptor.terms = &[Term {
                dimension: "other_slot",
                stride: 8192,
            }];
        })?;
    }
    reject_changed_descriptor(3, |field| field.descriptor.terms = &[SLOT_TERM])?;
    for index in [1, 3] {
        reject_changed_descriptor(index, |field| field.options = &[])?;
    }
    reject_changed_descriptor(1, |field| field.options = &[REPEATED_OPTION; 3])?;
    reject_changed_descriptor(3, |field| field.options = &[REPEATED_OPTION; 7])?;
    for index in [0, 2] {
        reject_changed_descriptor(index, |field| field.options = &[REPEATED_OPTION])?;
    }
    Ok(())
}

#[test]
fn callsign_codec_length_padding_encoding_and_scalar_domains_are_exact() -> TestResult {
    for codec in [
        FieldCodec::FixedString {
            len: 9,
            encoding: StringEncoding::Utf8,
            padding: 0,
        },
        FieldCodec::FixedString {
            len: 8,
            encoding: StringEncoding::Utf8,
            padding: 32,
        },
        FieldCodec::FixedString {
            len: 8,
            encoding: StringEncoding::MemoryMap,
            padding: 0,
        },
        FieldCodec::Bytes { len: 8 },
    ] {
        reject_changed_descriptor(0, |field| field.descriptor.codec = codec)?;
    }
    for index in 1..4 {
        reject_changed_descriptor(index, |field| {
            field.descriptor.codec = FieldCodec::Byte { min: 0, max: 1 }
        })?;
    }
    for index in 0..4 {
        reject_changed_descriptor(index, |field| field.descriptor.base += 8192)?;
    }
    Ok(())
}

#[test]
fn every_fresh_session_requires_both_guards_and_exact_typed_gateway_off() -> TestResult {
    for gateway in [
        None,
        Some(DvGatewayMode::Unqualified(0)),
        Some(DvGatewayMode::Unqualified(1)),
        Some(DvGatewayMode::Terminal),
        Some(DvGatewayMode::Unqualified(2)),
        Some(DvGatewayMode::Unqualified(255)),
    ] {
        let mut fixture = make_fixture()?;
        let result = fixture.trial.fresh_session(FixedTextTrialObservation {
            id: connection(1)?,
            identity: &fixture.identity,
            memory_format: 0,
            whole_page: &fixture.original,
            control_page: Some(&fixture.control),
            gateway_mode: gateway,
        });
        assert!(
            result.is_err(),
            "missing or non-Off typed GW cannot advance: {gateway:?}: {result:?}"
        );
        assert_eq!(fixture.trial.status(), PmNameTrialStatus::NotWritten);
        assert_eq!(
            fixture.trial.next_session(),
            Err(PmNameTrialError::TerminalState)
        );
    }
    let mut fixture = make_fixture()?;
    let result = fixture.trial.fresh_session(FixedTextTrialObservation {
        id: connection(1)?,
        identity: &fixture.identity,
        memory_format: 0,
        whole_page: &fixture.original,
        control_page: None,
        gateway_mode: Some(DvGatewayMode::Off),
    });
    assert_eq!(result, Err(PmNameTrialError::FreshGuardsMissing));
    assert_eq!(
        fixture.trial.next_session(),
        Err(PmNameTrialError::TerminalState)
    );
    Ok(())
}

#[test]
fn fresh_comparisons_cover_every_target_and_control_byte() -> TestResult {
    for offset in 0..PAGE_SIZE {
        let mut fixture = make_fixture()?;
        let mut target = fixture.original;
        *target.get_mut(offset).ok_or("target offset")? ^= 1;
        let result = fixture.trial.fresh_session(FixedTextTrialObservation {
            id: connection(1)?,
            identity: &fixture.identity,
            memory_format: 0,
            whole_page: &target,
            control_page: Some(&fixture.control),
            gateway_mode: Some(DvGatewayMode::Off),
        });
        assert_eq!(
            result,
            Err(PmNameTrialError::PageMismatch),
            "target offset {offset}"
        );
        assert_eq!(fixture.trial.status(), PmNameTrialStatus::NotWritten);
        let mut fixture = make_fixture()?;
        let mut control = fixture.control;
        *control.get_mut(offset).ok_or("control offset")? ^= 1;
        let result = fixture.trial.fresh_session(FixedTextTrialObservation {
            id: connection(1)?,
            identity: &fixture.identity,
            memory_format: 0,
            whole_page: &fixture.original,
            control_page: Some(&control),
            gateway_mode: Some(DvGatewayMode::Off),
        });
        assert_eq!(
            result,
            Err(PmNameTrialError::ControlPageMismatch),
            "control offset {offset}"
        );
        assert_eq!(fixture.trial.status(), PmNameTrialStatus::NotWritten);
    }
    Ok(())
}

#[test]
fn fresh_identity_format_and_both_lengths_remain_mandatory() -> TestResult {
    for length in [0, 255, 257] {
        let mut fixture = make_fixture()?;
        let result = fixture.trial.fresh_session(FixedTextTrialObservation {
            id: connection(1)?,
            identity: &fixture.identity,
            memory_format: 0,
            whole_page: &fixture.original,
            control_page: Some(&vec![0; length]),
            gateway_mode: Some(DvGatewayMode::Off),
        });
        assert_eq!(
            result,
            Err(PmNameTrialError::ControlPageLength { actual: length })
        );
        let mut fixture = make_fixture()?;
        let result = fixture.trial.fresh_session(FixedTextTrialObservation {
            id: connection(1)?,
            identity: &fixture.identity,
            memory_format: 0,
            whole_page: &vec![0; length],
            control_page: Some(&fixture.control),
            gateway_mode: Some(DvGatewayMode::Off),
        });
        assert_eq!(result, Err(PmNameTrialError::PageLength { actual: length }));
    }
    let mut fixture = make_fixture()?;
    let result = fixture.trial.fresh_session(FixedTextTrialObservation {
        id: connection(1)?,
        identity: &fixture.identity,
        memory_format: 1,
        whole_page: &fixture.original,
        control_page: Some(&fixture.control),
        gateway_mode: Some(DvGatewayMode::Off),
    });
    assert_eq!(result, Err(PmNameTrialError::MemoryFormat { actual: 1 }));
    let mut fixture = make_fixture()?;
    let mut identity = fixture.identity.clone();
    identity.radio_type = RadioType::new("J,2,1")?;
    let result = fixture.trial.fresh_session(FixedTextTrialObservation {
        id: connection(1)?,
        identity: &identity,
        memory_format: 0,
        whole_page: &fixture.original,
        control_page: Some(&fixture.control),
        gateway_mode: Some(DvGatewayMode::Off),
    });
    assert_eq!(result, Err(PmNameTrialError::IdentityMismatch));
    Ok(())
}

#[test]
fn unguarded_common_fresh_event_cannot_bypass_my1_policy() -> TestResult {
    let mut fixture = make_fixture()?;
    let result = fixture.trial.record(PmNameTrialEvent::FreshSession {
        id: connection(1)?,
        identity: &fixture.identity,
        memory_format: 0,
        whole_page: &fixture.original,
    });
    assert_eq!(result, Err(PmNameTrialError::FreshGuardsMissing));
    assert_eq!(fixture.trial.status(), PmNameTrialStatus::NotWritten);
    assert_eq!(
        fixture.trial.next_session(),
        Err(PmNameTrialError::TerminalState)
    );
    Ok(())
}

#[test]
fn restoration_requires_three_guarded_and_finalized_sessions() -> TestResult {
    let mut fixture = make_fixture()?;
    fresh(&mut fixture, 1, false)?;
    assert_eq!(fixture.trial.status(), PmNameTrialStatus::NotWritten);
    intent(&mut fixture, PmNameTrialWrite::Rename)?;
    assert_eq!(fixture.trial.status(), PmNameTrialStatus::PossiblyChanged);
    readback(&mut fixture, true)?;
    fixture.trial.finalize_session(connection(1)?)?;
    assert_eq!(fixture.trial.next_session()?, PmNameTrialSession::Restore);
    fresh(&mut fixture, 2, true)?;
    intent(&mut fixture, PmNameTrialWrite::Restore)?;
    readback(&mut fixture, false)?;
    fixture.trial.finalize_session(connection(2)?)?;
    assert_eq!(fixture.trial.status(), PmNameTrialStatus::PossiblyChanged);
    assert_eq!(
        fixture.trial.next_session()?,
        PmNameTrialSession::VerifyRestoration
    );
    fresh(&mut fixture, 3, false)?;
    assert_eq!(fixture.trial.status(), PmNameTrialStatus::PossiblyChanged);
    fixture.trial.finalize_session(connection(3)?)?;
    assert_eq!(
        fixture.trial.status(),
        PmNameTrialStatus::RestorationVerified
    );
    fixture.trial.halt();
    assert_eq!(
        fixture.trial.status(),
        PmNameTrialStatus::RestorationVerified
    );
    assert_eq!(
        fixture.trial.next_session(),
        Err(PmNameTrialError::TerminalState)
    );
    assert_eq!(
        fixture.trial.finalize_session(connection(3)?),
        Err(PmNameTrialError::TerminalState)
    );
    Ok(())
}

#[test]
fn guard_drift_after_rename_never_authorizes_stale_restoration() -> TestResult {
    for changed_control in [false, true] {
        let mut fixture = make_fixture()?;
        renamed(&mut fixture)?;
        let mut control = fixture.control;
        if changed_control {
            *control.last_mut().ok_or("control last byte")? ^= 1;
        }
        let result = fixture.trial.fresh_session(FixedTextTrialObservation {
            id: connection(2)?,
            identity: &fixture.identity,
            memory_format: 0,
            whole_page: &fixture.expected,
            control_page: Some(&control),
            gateway_mode: Some(if changed_control {
                DvGatewayMode::Off
            } else {
                DvGatewayMode::Terminal
            }),
        });
        assert!(
            result.is_err(),
            "fresh guard drift cannot permit restoration: {result:?}"
        );
        assert_eq!(fixture.trial.status(), PmNameTrialStatus::PossiblyChanged);
        assert_eq!(
            fixture.trial.next_session(),
            Err(PmNameTrialError::TerminalState)
        );
        assert_eq!(
            fixture.trial.record(PmNameTrialEvent::DurableWriteIntent {
                id: connection(12)?,
                write: PmNameTrialWrite::Restore
            }),
            Err(PmNameTrialError::TerminalState)
        );
    }
    Ok(())
}

#[test]
fn premature_finalization_and_unguarded_freshness_preserve_write_obligation() -> TestResult {
    let mut fixture = make_fixture()?;
    fresh(&mut fixture, 1, false)?;
    intent(&mut fixture, PmNameTrialWrite::Rename)?;
    assert_eq!(
        fixture.trial.finalize_session(connection(1)?),
        Err(PmNameTrialError::UnexpectedEvent)
    );
    assert_eq!(fixture.trial.status(), PmNameTrialStatus::PossiblyChanged);
    let mut fixture = make_fixture()?;
    renamed(&mut fixture)?;
    let result = fixture.trial.record(PmNameTrialEvent::FreshSession {
        id: connection(2)?,
        identity: &fixture.identity,
        memory_format: 0,
        whole_page: &fixture.expected,
    });
    assert_eq!(result, Err(PmNameTrialError::FreshGuardsMissing));
    assert_eq!(fixture.trial.status(), PmNameTrialStatus::PossiblyChanged);
    assert_eq!(
        fixture.trial.next_session(),
        Err(PmNameTrialError::TerminalState)
    );
    Ok(())
}
