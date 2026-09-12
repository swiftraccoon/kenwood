//! Pure storage and lifecycle invariants; these fixtures are not radio captures.

use super::*;
use crate::memory::{MenuOption, StorageTransform, is_supported_schema_target};
use crate::types::{FirmwareIdentity, RadioType};

type TestResult = Result<(), Box<dyn std::error::Error>>;

struct Fixture {
    update: My1CallsignUpdate,
    identity: Identity,
    original: [u8; PAGE_SIZE],
    desired: [u8; PAGE_SIZE],
    control: [u8; PAGE_SIZE],
}

fn id(value: u64) -> Result<NonZeroU64, Box<dyn std::error::Error>> {
    NonZeroU64::new(value).ok_or_else(|| "fixture IDs must be nonzero".into())
}

fn fixture() -> Result<Fixture, Box<dyn std::error::Error>> {
    let identity = Identity {
        model: RadioModel::TmD750,
        firmware: FirmwareIdentity::new("1.02")?,
        radio_type: RadioType::new("K,2,1")?,
    };
    let mut original = [0xA5; PAGE_SIZE];
    original
        .get_mut(..2)
        .ok_or("stored Gateway and MY selectors")?
        .fill(0);
    original.get_mut(8..16).ok_or("MY1 range")?.fill(0);
    let mut control = [0x5A; PAGE_SIZE];
    *control.get_mut(9).ok_or("PM selector")? = 0;
    let update = My1CallsignUpdate::prepare(
        &identity,
        &original,
        &control,
        None,
        &My1Callsign::new("N0CALL")?,
    )?;
    let desired = *update.desired_page();
    Ok(Fixture {
        update,
        identity,
        original,
        desired,
        control,
    })
}

fn fresh(fixture: &mut Fixture, verify: bool) -> TestResult {
    fixture
        .update
        .record(My1CallsignUpdateEvent::FreshSession {
            id: id(if verify { 2 } else { 1 })?,
            identity: &fixture.identity,
            memory_format: 0,
            gateway_mode: DvGatewayMode::Off,
            control_page: &fixture.control,
            whole_page: if verify {
                &fixture.desired
            } else {
                &fixture.original
            },
        })?;
    Ok(())
}

fn finalize(fixture: &mut Fixture, verify: bool) -> TestResult {
    fixture
        .update
        .record(My1CallsignUpdateEvent::SessionFinalized {
            id: id(if verify { 2 } else { 1 })?,
            identity: &fixture.identity,
            gateway_mode: DvGatewayMode::Off,
        })?;
    Ok(())
}

fn advance(fixture: &mut Fixture, count: usize) -> TestResult {
    for step in 0..count {
        match step {
            0 => fresh(fixture, false)?,
            1 => fixture
                .update
                .record(My1CallsignUpdateEvent::DurableWriteIntent { id: id(7)? })?,
            2 => fixture
                .update
                .record(My1CallsignUpdateEvent::ImmediateReadback {
                    whole_page: &fixture.desired,
                })?,
            3 => finalize(fixture, false)?,
            4 => fresh(fixture, true)?,
            5 => finalize(fixture, true)?,
            _ => return Err("fixture cannot advance beyond its two sessions".into()),
        }
    }
    Ok(())
}

const fn status_at(phase: usize) -> My1CallsignUpdateStatus {
    match phase {
        0 | 1 => My1CallsignUpdateStatus::NotWritten,
        6 => My1CallsignUpdateStatus::VerifiedAcrossSessions,
        _ => My1CallsignUpdateStatus::PossiblyChanged,
    }
}

#[test]
fn callsign_syntax_is_exact_uppercase_ascii_with_alphanumeric_content() -> TestResult {
    for text in [
        "",
        " ",
        "        ",
        "N0CALL123",
        "n0call",
        "N0/CALL",
        "N0CALL\0",
        "N0\r",
        "N0\t",
        "N0é",
    ] {
        assert_eq!(
            My1Callsign::new(text),
            Err(My1CallsignUpdateError::InvalidCallsign),
            "reject {text:?} without normalization"
        );
    }
    for byte in 0..=u8::MAX {
        let bytes = [b'A', byte];
        if let Ok(text) = std::str::from_utf8(&bytes) {
            assert_eq!(
                My1Callsign::new(text).is_ok(),
                byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b' ',
                "exact admission for byte {byte}"
            );
        }
    }
    for text in ["A", "0", " N0CALL ", "W1AW   A", "A B C D ", "       1"] {
        assert_eq!(
            My1Callsign::new(text)?.as_str(),
            text,
            "preserve every supplied space"
        );
    }
    Ok(())
}

#[test]
fn preparation_changes_only_my1_and_never_relaxes_the_generic_firmware_gate() -> TestResult {
    let fixture = fixture()?;
    assert_eq!(
        My1CallsignUpdate::required_page()?,
        fixture.update.page(),
        "prepared target must be the canonical required page"
    );
    assert_eq!(
        My1CallsignUpdate::required_control_page()?,
        fixture.update.control_page_spec(),
        "prepared control page must retain the canonical address and length"
    );
    assert_eq!(
        (
            fixture.update.page().address().as_u32(),
            fixture.update.page().len()
        ),
        (TARGET_ADDRESS, PAGE_SIZE),
        "the only write target is the complete fixed MY1 page"
    );
    assert_eq!(
        (
            fixture.update.control_page_spec().address().as_u32(),
            fixture.update.control_page_spec().len()
        ),
        (CONTROL_ADDRESS, PAGE_SIZE),
        "the immutable control guard covers its entire fixed page"
    );
    assert_eq!(
        fixture.update.identity(),
        &fixture.identity,
        "retain the exact captured identity"
    );
    assert_eq!(
        fixture.update.original_page(),
        &fixture.original,
        "preparation must retain every original byte"
    );
    assert_eq!(
        fixture.update.control_page(),
        &fixture.control,
        "control-page preparation must not modify any byte"
    );
    assert_eq!(
        fixture.update.current_callsign(),
        None,
        "an eight-NUL field is represented as empty"
    );
    assert_eq!(
        fixture.update.desired_callsign().as_str(),
        "N0CALL",
        "retain the caller's exact requested text"
    );
    assert_eq!(
        fixture.desired.get(8..16),
        Some(b"N0CALL\0\0".as_slice()),
        "a six-byte callsign receives exactly two NUL bytes"
    );
    for (offset, (before, after)) in fixture.original.iter().zip(&fixture.desired).enumerate() {
        if !(8..16).contains(&offset) {
            assert_eq!(before, after, "unrelated target byte {offset}");
        }
    }
    assert_eq!(
        fixture.update.status(),
        My1CallsignUpdateStatus::NotWritten,
        "offline preparation creates no write obligation"
    );
    assert_eq!(
        fixture.update.next_session()?,
        Session::Apply,
        "fresh original-page comparison is the first required session"
    );
    assert!(
        !is_supported_schema_target(fixture.identity.model, &fixture.identity.firmware),
        "bounded MY1 support is not generic firmware admission"
    );
    Ok(())
}

#[test]
fn empty_current_requires_eight_nuls_not_spaces_erased_bytes_or_partial_text() -> TestResult {
    let fixture = fixture()?;
    for fill in [b' ', 0xFF, b'A'] {
        for offset in 8..16 {
            let mut original = fixture.original;
            *original.get_mut(offset).ok_or("MY1 byte")? = fill;
            assert!(
                matches!(
                    My1CallsignUpdate::prepare(
                        &fixture.identity,
                        &original,
                        &fixture.control,
                        None,
                        fixture.update.desired_callsign()
                    ),
                    Err(My1CallsignUpdateError::CurrentCallsignMismatch)
                ),
                "None must reject changed byte {offset}={fill}"
            );
        }
    }
    Ok(())
}

#[test]
fn current_and_desired_text_use_exact_nul_padding_without_trimming() -> TestResult {
    let fixture = fixture()?;
    let current = My1Callsign::new(" N0CALL ")?;
    let desired = My1Callsign::new("W1AW   A")?;
    let mut original = fixture.original;
    original
        .get_mut(8..16)
        .ok_or("MY1 field")?
        .copy_from_slice(b" N0CALL ");
    let update = My1CallsignUpdate::prepare(
        &fixture.identity,
        &original,
        &fixture.control,
        Some(&current),
        &desired,
    )?;
    assert_eq!(
        update.current_callsign(),
        Some(&current),
        "matched current text retains both edge spaces"
    );
    assert_eq!(
        update.desired_page().get(8..16),
        Some(b"W1AW   A".as_slice()),
        "a full eight-byte desired value must preserve its interior spaces"
    );
    assert!(
        matches!(
            My1CallsignUpdate::prepare(
                &fixture.identity,
                &original,
                &fixture.control,
                Some(&current),
                &current
            ),
            Err(My1CallsignUpdateError::NoChange)
        ),
        "a no-op never authorizes a write"
    );
    let shortened = My1Callsign::new("N0CALL")?;
    assert!(
        matches!(
            My1CallsignUpdate::prepare(
                &fixture.identity,
                &original,
                &fixture.control,
                Some(&shortened),
                &desired
            ),
            Err(My1CallsignUpdateError::CurrentCallsignMismatch)
        ),
        "spaces are part of the expected value"
    );
    original
        .get_mut(8..16)
        .ok_or("MY1 field")?
        .copy_from_slice(b"N0CALL\0\0");
    assert!(
        My1CallsignUpdate::prepare(
            &fixture.identity,
            &original,
            &fixture.control,
            Some(&shortened),
            &desired
        )
        .is_ok(),
        "canonical shorter text is NUL padded"
    );
    *original.get_mut(15).ok_or("padding byte")? = b' ';
    assert!(
        matches!(
            My1CallsignUpdate::prepare(
                &fixture.identity,
                &original,
                &fixture.control,
                Some(&shortened),
                &desired
            ),
            Err(My1CallsignUpdateError::CurrentCallsignMismatch)
        ),
        "space padding is not NUL padding"
    );
    Ok(())
}

#[test]
fn preparation_rejects_incomplete_pages_and_every_changed_identity_component() -> TestResult {
    let fixture = fixture()?;
    for length in [0, 8, 255, 257, 512] {
        let bytes = vec![0; length];
        assert!(
            matches!(My1CallsignUpdate::prepare(&fixture.identity, &bytes, &fixture.control, None, fixture.update.desired_callsign()), Err(My1CallsignUpdateError::PageLength { actual }) if actual == length),
            "target length {length}"
        );
        assert!(
            matches!(My1CallsignUpdate::prepare(&fixture.identity, &fixture.original, &bytes, None, fixture.update.desired_callsign()), Err(My1CallsignUpdateError::ControlPageLength { actual }) if actual == length),
            "control length {length}"
        );
    }
    for firmware in ["1.00", "1.01", "1.03", "1.020", "1.02E"] {
        let mut identity = fixture.identity.clone();
        identity.firmware = FirmwareIdentity::new(firmware)?;
        assert!(
            matches!(
                My1CallsignUpdate::prepare(
                    &identity,
                    &fixture.original,
                    &fixture.control,
                    None,
                    fixture.update.desired_callsign()
                ),
                Err(My1CallsignUpdateError::IdentityMismatch)
            ),
            "exact firmware {firmware} is not admitted"
        );
    }
    for radio_type in ["E,2,1", "K,2,2", "K,1,1", "K21"] {
        let mut identity = fixture.identity.clone();
        identity.radio_type = RadioType::new(radio_type)?;
        assert!(
            matches!(
                My1CallsignUpdate::prepare(
                    &identity,
                    &fixture.original,
                    &fixture.control,
                    None,
                    fixture.update.desired_callsign()
                ),
                Err(My1CallsignUpdateError::IdentityMismatch)
            ),
            "exact type {radio_type} is not admitted"
        );
    }
    Ok(())
}

#[test]
fn every_nonzero_stored_selector_or_gateway_is_refused_without_changing_capture() -> TestResult {
    let fixture = fixture()?;
    for value in 1..=u8::MAX {
        let mut target = fixture.original;
        let mut control = fixture.control;
        *control.get_mut(9).ok_or("PM selector")? = value;
        assert!(
            matches!(My1CallsignUpdate::prepare(&fixture.identity, &target, &control, None, fixture.update.desired_callsign()), Err(My1CallsignUpdateError::PmSelection { actual }) if actual == value),
            "PM selector {value}"
        );
        *target.get_mut(0).ok_or("Gateway selector")? = value;
        assert!(
            matches!(My1CallsignUpdate::prepare(&fixture.identity, &target, &fixture.control, None, fixture.update.desired_callsign()), Err(My1CallsignUpdateError::GatewayMode { actual }) if actual == DvGatewayMode::from(value)),
            "Gateway {value}"
        );
        target = fixture.original;
        *target.get_mut(1).ok_or("MY selector")? = value;
        assert!(
            matches!(My1CallsignUpdate::prepare(&fixture.identity, &target, &fixture.control, None, fixture.update.desired_callsign()), Err(My1CallsignUpdateError::MySelection { actual }) if actual == value),
            "MY selector {value}"
        );
    }
    assert_eq!(
        fixture.update.original_page(),
        &fixture.original,
        "refused preparation cannot alter an existing immutable plan"
    );
    Ok(())
}

fn descriptor_mutations(field: MenuField) -> Vec<MenuField> {
    vec![
        MenuField {
            descriptor: FieldDescriptor {
                name: "changed.field",
                ..field.descriptor
            },
            ..field
        },
        MenuField {
            descriptor: FieldDescriptor {
                base: field.descriptor.base + 1,
                ..field.descriptor
            },
            ..field
        },
        MenuField {
            descriptor: FieldDescriptor {
                terms: if field.descriptor.is_per_slot() {
                    &[]
                } else {
                    &[SLOT_TERM]
                },
                ..field.descriptor
            },
            ..field
        },
        MenuField {
            descriptor: FieldDescriptor {
                codec: FieldCodec::Bytes { len: 9 },
                ..field.descriptor
            },
            ..field
        },
        MenuField {
            menu: "changed",
            ..field
        },
        MenuField {
            is_blob: true,
            ..field
        },
        MenuField {
            enum_type: if field.enum_type.is_some() {
                None
            } else {
                Some("changed")
            },
            ..field
        },
        MenuField {
            allowed_values: &[0],
            ..field
        },
        MenuField {
            options: &[MenuOption {
                raw: 99,
                member: "changed",
                label: None,
                resource_key: None,
            }],
            ..field
        },
        MenuField {
            storage_transform: Some(StorageTransform {
                input_unit: "changed",
                numerator: 1,
                denominator: 1,
            }),
            ..field
        },
    ]
}

#[test]
fn every_descriptor_and_guard_shape_is_pinned_before_preparation() -> TestResult {
    let fixture = fixture()?;
    for selected in 0..4 {
        let original = Descriptors::load()?;
        let field = match selected {
            0 => original.callsign,
            1 => original.gateway,
            2 => original.my_selection,
            _ => original.pm_selection,
        };
        for changed in descriptor_mutations(*field) {
            let mut fields = Descriptors::load()?;
            match selected {
                0 => fields.callsign = &changed,
                1 => fields.gateway = &changed,
                2 => fields.my_selection = &changed,
                _ => fields.pm_selection = &changed,
            }
            assert!(
                matches!(
                    My1CallsignUpdate::prepare_with_fields(
                        &fixture.identity,
                        &fixture.original,
                        &fixture.control,
                        None,
                        fixture.update.desired_callsign(),
                        &fields
                    ),
                    Err(My1CallsignUpdateError::UnsupportedDescriptor)
                ),
                "descriptor {selected} mutation must refuse preparation"
            );
        }
    }
    for codec in [
        FieldCodec::FixedString {
            len: 7,
            encoding: StringEncoding::Utf8,
            padding: 0,
        },
        FieldCodec::FixedString {
            len: 8,
            encoding: StringEncoding::Utf8,
            padding: b' ',
        },
        FieldCodec::FixedString {
            len: 8,
            encoding: StringEncoding::MemoryMap,
            padding: 0,
        },
    ] {
        let fields = Descriptors::load()?;
        let changed = MenuField {
            descriptor: FieldDescriptor {
                codec,
                ..fields.callsign.descriptor
            },
            ..*fields.callsign
        };
        assert_eq!(
            supported_page(&changed, CALLSIGN),
            Err(My1CallsignUpdateError::UnsupportedDescriptor),
            "exact width, codec, and padding are required"
        );
    }
    Ok(())
}

#[test]
fn all_six_events_are_required_before_verified_status() -> TestResult {
    let mut fixture = fixture()?;
    for phase in 0..=6 {
        assert_eq!(
            fixture.update.status(),
            status_at(phase),
            "status before event {phase}"
        );
        match phase {
            0 => {
                assert_eq!(
                    fixture.update.next_session()?,
                    Session::Apply,
                    "the first session must compare the original image"
                );
                fresh(&mut fixture, false)?;
            }
            1 => fixture
                .update
                .record(My1CallsignUpdateEvent::DurableWriteIntent { id: id(7)? })?,
            2 => fixture
                .update
                .record(My1CallsignUpdateEvent::ImmediateReadback {
                    whole_page: &fixture.desired,
                })?,
            3 => finalize(&mut fixture, false)?,
            4 => {
                assert_eq!(
                    fixture.update.next_session()?,
                    Session::Verify,
                    "only finalized Apply admits independent Verify"
                );
                fresh(&mut fixture, true)?;
            }
            5 => finalize(&mut fixture, true)?,
            _ => assert_eq!(
                fixture.update.next_session(),
                Err(My1CallsignUpdateError::TerminalState),
                "completed verification cannot request a third session"
            ),
        }
    }
    assert_eq!(
        fixture.update.original_page(),
        &fixture.original,
        "all accepted events preserve the original image"
    );
    assert_eq!(
        fixture.update.desired_page(),
        &fixture.desired,
        "accepted evidence cannot change the requested image"
    );
    assert_eq!(
        fixture.update.control_page(),
        &fixture.control,
        "no lifecycle event modifies the immutable guard image"
    );
    Ok(())
}

#[test]
fn each_byte_of_both_fresh_pages_and_immediate_readback_is_compared() -> TestResult {
    for phase in [0, 2, 4] {
        for control_changed in [false, true] {
            if phase == 2 && control_changed {
                continue;
            }
            for offset in 0..PAGE_SIZE {
                let mut fixture = fixture()?;
                advance(&mut fixture, phase)?;
                let mut target = if phase == 0 {
                    fixture.original
                } else {
                    fixture.desired
                };
                let mut control = fixture.control;
                let changed = if control_changed {
                    &mut control
                } else {
                    &mut target
                };
                *changed.get_mut(offset).ok_or("comparison byte")? ^= 1;
                let event = if phase == 2 {
                    My1CallsignUpdateEvent::ImmediateReadback {
                        whole_page: &target,
                    }
                } else {
                    My1CallsignUpdateEvent::FreshSession {
                        id: id(if phase == 0 { 1 } else { 2 })?,
                        identity: &fixture.identity,
                        memory_format: 0,
                        gateway_mode: DvGatewayMode::Off,
                        control_page: &control,
                        whole_page: &target,
                    }
                };
                assert_eq!(
                    fixture.update.record(event),
                    Err(if control_changed {
                        My1CallsignUpdateError::ControlPageMismatch
                    } else {
                        My1CallsignUpdateError::PageMismatch
                    }),
                    "phase {phase}, control={control_changed}, byte {offset}"
                );
                assert_eq!(
                    fixture.update.status(),
                    status_at(phase),
                    "failed comparison cannot erase accepted intent"
                );
                assert_eq!(
                    fixture.update.next_session(),
                    Err(My1CallsignUpdateError::TerminalState),
                    "any byte mismatch permanently halts the transaction"
                );
                assert_eq!(
                    fixture.update.original_page(),
                    &fixture.original,
                    "a differing read cannot rebase the original image"
                );
                assert_eq!(
                    fixture.update.desired_page(),
                    &fixture.desired,
                    "a differing read cannot replace the desired image"
                );
            }
        }
    }
    Ok(())
}

#[test]
fn fresh_guards_are_mandatory_in_both_apply_and_verify() -> TestResult {
    for phase in [0, 4] {
        for fault in 0..7 {
            let mut fixture = fixture()?;
            advance(&mut fixture, phase)?;
            let mut identity = fixture.identity.clone();
            if fault == 0 {
                identity.radio_type = RadioType::new("E,2,1")?;
            }
            let gateway_mode = match fault {
                1 => DvGatewayMode::Terminal,
                2 => DvGatewayMode::Unqualified(255),
                6 => DvGatewayMode::Unqualified(0),
                _ => DvGatewayMode::Off,
            };
            let page = if phase == 0 {
                &fixture.original
            } else {
                &fixture.desired
            };
            let result = fixture.update.record(My1CallsignUpdateEvent::FreshSession {
                id: id(if phase == 0 { 1 } else { 2 })?,
                identity: &identity,
                memory_format: u8::from(fault == 3),
                gateway_mode,
                control_page: if fault == 4 { &[] } else { &fixture.control },
                whole_page: if fault == 5 { &[] } else { page },
            });
            assert_eq!(
                result,
                Err(match fault {
                    0 => My1CallsignUpdateError::IdentityMismatch,
                    1 | 2 | 6 => My1CallsignUpdateError::GatewayMode {
                        actual: gateway_mode
                    },
                    3 => My1CallsignUpdateError::MemoryFormat { actual: 1 },
                    4 => My1CallsignUpdateError::ControlPageLength { actual: 0 },
                    _ => My1CallsignUpdateError::PageLength { actual: 0 },
                }),
                "fresh guard fault {fault} must be retained at phase {phase}"
            );
            assert_eq!(
                fixture.update.status(),
                status_at(phase),
                "guard failure cannot clear a previously accepted intent"
            );
            assert_eq!(
                fixture.update.next_session(),
                Err(My1CallsignUpdateError::TerminalState),
                "a failed fresh guard forbids subsequent evidence"
            );
        }
    }
    Ok(())
}

#[test]
fn both_finalizations_require_the_current_id_exact_fresh_identity_and_gateway_off() -> TestResult {
    for phase in [3, 5] {
        for fault in 0..5 {
            let mut fixture = fixture()?;
            advance(&mut fixture, phase)?;
            let mut identity = fixture.identity.clone();
            if fault == 1 {
                identity.firmware = FirmwareIdentity::new("1.03")?;
            }
            let gateway_mode = match fault {
                2 => DvGatewayMode::Terminal,
                3 => DvGatewayMode::Unqualified(1),
                4 => DvGatewayMode::Unqualified(0),
                _ => DvGatewayMode::Off,
            };
            let session = if fault == 0 {
                9
            } else if phase == 3 {
                1
            } else {
                2
            };
            let result = fixture
                .update
                .record(My1CallsignUpdateEvent::SessionFinalized {
                    id: id(session)?,
                    identity: &identity,
                    gateway_mode,
                });
            assert_eq!(
                result,
                Err(match fault {
                    0 => My1CallsignUpdateError::SessionMismatch,
                    1 => My1CallsignUpdateError::IdentityMismatch,
                    _ => My1CallsignUpdateError::GatewayMode {
                        actual: gateway_mode
                    },
                }),
                "finalization fault {fault} must remain explicit at phase {phase}"
            );
            assert_eq!(
                fixture.update.status(),
                My1CallsignUpdateStatus::PossiblyChanged,
                "failed cleanup evidence cannot clear the intent"
            );
            assert_eq!(
                fixture.update.next_session(),
                Err(My1CallsignUpdateError::TerminalState),
                "failed fresh cleanup evidence must permanently halt completion"
            );
        }
    }
    Ok(())
}

#[test]
fn a_reused_verification_session_id_permanently_halts_with_possible_change() -> TestResult {
    let mut fixture = fixture()?;
    advance(&mut fixture, 4)?;
    assert_eq!(
        fixture.update.record(My1CallsignUpdateEvent::FreshSession {
            id: id(1)?,
            identity: &fixture.identity,
            memory_format: 0,
            gateway_mode: DvGatewayMode::Off,
            control_page: &fixture.control,
            whole_page: &fixture.desired,
        }),
        Err(My1CallsignUpdateError::ReusedSession),
        "Verify must not reuse Apply's connection identifier"
    );
    assert_eq!(
        fixture.update.status(),
        My1CallsignUpdateStatus::PossiblyChanged,
        "a reused session cannot discharge the accepted write"
    );
    assert_eq!(
        fixture.update.next_session(),
        Err(My1CallsignUpdateError::TerminalState),
        "session reuse irreversibly halts this instance"
    );
    Ok(())
}

#[test]
fn every_out_of_order_event_halts_without_resetting_status_or_images() -> TestResult {
    for phase in 0..=6 {
        for event_kind in 0..4 {
            let expected = match phase {
                0 | 4 => Some(0),
                1 => Some(1),
                2 => Some(2),
                3 | 5 => Some(3),
                _ => None,
            };
            if expected == Some(event_kind) {
                continue;
            }
            let mut fixture = fixture()?;
            advance(&mut fixture, phase)?;
            let session = id(if phase < 4 { 1 } else { 2 })?;
            let event = match event_kind {
                0 => My1CallsignUpdateEvent::FreshSession {
                    id: session,
                    identity: &fixture.identity,
                    memory_format: 0,
                    gateway_mode: DvGatewayMode::Off,
                    control_page: &fixture.control,
                    whole_page: &fixture.desired,
                },
                1 => My1CallsignUpdateEvent::DurableWriteIntent { id: id(7)? },
                2 => My1CallsignUpdateEvent::ImmediateReadback {
                    whole_page: &fixture.desired,
                },
                _ => My1CallsignUpdateEvent::SessionFinalized {
                    id: session,
                    identity: &fixture.identity,
                    gateway_mode: DvGatewayMode::Off,
                },
            };
            assert_eq!(
                fixture.update.record(event),
                Err(if phase == 6 {
                    My1CallsignUpdateError::TerminalState
                } else {
                    My1CallsignUpdateError::UnexpectedEvent
                }),
                "phase {phase}, event {event_kind}"
            );
            assert_eq!(
                fixture.update.status(),
                status_at(phase),
                "out-of-order events cannot rewrite the previous risk status"
            );
            assert_eq!(
                fixture.update.next_session(),
                Err(My1CallsignUpdateError::TerminalState),
                "out-of-order evidence prevents further sessions"
            );
            assert_eq!(
                fixture.update.original_page(),
                &fixture.original,
                "invalid evidence cannot replace the original image"
            );
            assert_eq!(
                fixture.update.desired_page(),
                &fixture.desired,
                "invalid evidence cannot replace the desired image"
            );
        }
    }
    Ok(())
}

#[test]
fn halt_is_sticky_at_every_phase_and_cannot_erase_completed_verification() -> TestResult {
    for phase in 0..=6 {
        let mut fixture = fixture()?;
        advance(&mut fixture, phase)?;
        fixture.update.halt();
        fixture.update.halt();
        assert_eq!(
            fixture.update.status(),
            status_at(phase),
            "halt preserves the last conservative outcome"
        );
        assert_eq!(
            fixture.update.next_session(),
            Err(My1CallsignUpdateError::TerminalState),
            "halt is terminal at every lifecycle phase"
        );
        assert_eq!(
            fixture
                .update
                .record(My1CallsignUpdateEvent::DurableWriteIntent { id: id(8)? }),
            Err(My1CallsignUpdateError::TerminalState),
            "a new intent cannot revive a halted or completed update"
        );
        assert_eq!(
            fixture.update.status(),
            status_at(phase),
            "late misuse cannot rewrite historical success or obligation"
        );
    }
    Ok(())
}
