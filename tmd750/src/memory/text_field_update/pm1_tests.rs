use std::num::NonZeroU64;

use super::super::{
    Pm1NameUpdate, TextFieldUpdateError, TextFieldUpdateEvent, TextFieldUpdateSession as Session,
    TextFieldUpdateStatus,
};
use super::*;
use crate::memory::{FieldDescriptor, SLOT_TERM, StorageTransform, is_supported_schema_target};
use crate::types::{FirmwareIdentity, RadioModel, RadioType};

type TestResult = Result<(), Box<dyn std::error::Error>>;

struct Fixture {
    update: Pm1NameUpdate,
    identity: Identity,
    original: [u8; PAGE_SIZE],
    desired: [u8; PAGE_SIZE],
}

fn id(value: u64) -> Result<NonZeroU64, Box<dyn std::error::Error>> {
    NonZeroU64::new(value).ok_or_else(|| "test ID must be nonzero".into())
}

fn fixture() -> Result<Fixture, Box<dyn std::error::Error>> {
    let identity = Identity {
        model: RadioModel::TmD750,
        firmware: FirmwareIdentity::new("1.02")?,
        radio_type: RadioType::new("K,2,1")?,
    };
    let mut original = [0xA5; PAGE_SIZE];
    original.get_mut(10..26).ok_or("PM1 range")?.fill(0);
    original
        .get_mut(10..13)
        .ok_or("PM1 name")?
        .copy_from_slice(b"PM1");
    let update = Pm1NameUpdate::prepare(
        &identity,
        &original,
        &Pm1Name::new("PM1")?,
        &Pm1Name::new("My station")?,
    )?;
    let desired = *update.desired_page();
    Ok(Fixture {
        update,
        identity,
        original,
        desired,
    })
}

fn fresh(fixture: &mut Fixture, session: u64) -> TestResult {
    fixture.update.record(TextFieldUpdateEvent::FreshSession {
        id: id(session)?,
        identity: &fixture.identity,
        memory_format: 0,
        whole_page: if session == 1 {
            &fixture.original
        } else {
            &fixture.desired
        },
        guards: (),
    })?;
    Ok(())
}

fn intent(fixture: &mut Fixture) -> TestResult {
    fixture
        .update
        .record(TextFieldUpdateEvent::DurableWriteIntent { id: id(1)? })?;
    Ok(())
}

fn readback(fixture: &mut Fixture) -> TestResult {
    fixture
        .update
        .record(TextFieldUpdateEvent::ImmediateReadback {
            whole_page: &fixture.desired,
        })?;
    Ok(())
}

fn finalize(fixture: &mut Fixture, session: u64) -> TestResult {
    fixture
        .update
        .record(TextFieldUpdateEvent::SessionFinalized {
            id: id(session)?,
            guards: (),
        })?;
    Ok(())
}

fn applied(fixture: &mut Fixture) -> TestResult {
    fresh(fixture, 1)?;
    intent(fixture)?;
    readback(fixture)?;
    finalize(fixture, 1)
}

fn current(fixture: &Fixture) -> Result<&Pm1Name, Box<dyn std::error::Error>> {
    fixture
        .update
        .current()
        .ok_or_else(|| "PM1 updates always carry the current name".into())
}

fn requested(fixture: &Fixture) -> Result<&Pm1Name, Box<dyn std::error::Error>> {
    fixture
        .update
        .requested()
        .ok_or_else(|| "PM1 updates always carry the requested name".into())
}

#[test]
fn names_reject_every_nonprintable_byte_without_trimming_or_truncation() -> TestResult {
    for name in ["", "12345678901234567", "PM\n1", "PM\0", "P\u{7f}M", "PMé"] {
        assert!(
            matches!(
                Pm1Name::new(name),
                Err(TextFieldUpdateError::InvalidText {
                    field: "PM1 name",
                    ..
                })
            ),
            "{name:?}"
        );
    }
    for byte in 0..=u8::MAX {
        let bytes = [byte];
        if let Ok(name) = std::str::from_utf8(&bytes) {
            assert_eq!(Pm1Name::new(name).is_ok(), (32..=126).contains(&byte));
        }
    }
    for name in [" ", " PM1 ", "1234567890123456", "!@#$%^&*()_+[]{}"] {
        assert_eq!(Pm1Name::new(name)?.as_str(), name);
    }
    Ok(())
}

#[test]
fn preparation_preserves_all_unrelated_bytes_and_keeps_generic_gate_closed() -> TestResult {
    let fixture = fixture()?;
    assert_eq!(Pm1NameUpdate::required_page()?, fixture.update.page());
    assert_eq!(fixture.update.field(), TextField::Pm1Name);
    assert_eq!(fixture.update.page().address().as_u32(), PAGE_ADDRESS);
    assert_eq!(fixture.update.page().len(), PAGE_SIZE);
    assert_eq!(fixture.update.identity(), &fixture.identity);
    assert_eq!(fixture.update.original_page(), &fixture.original);
    assert_eq!(current(&fixture)?.as_str(), "PM1");
    assert_eq!(requested(&fixture)?.as_str(), "My station");
    assert_eq!(
        fixture.desired.get(10..26),
        Some(b"My station\0\0\0\0\0\0".as_slice())
    );
    for (index, (original, desired)) in fixture.original.iter().zip(&fixture.desired).enumerate() {
        if !(10..26).contains(&index) {
            assert_eq!(original, desired, "unrelated byte {index}");
        }
    }
    assert_eq!(fixture.update.status(), TextFieldUpdateStatus::NotWritten);
    assert_eq!(fixture.update.next_session()?, Session::Apply);
    assert!(
        !is_supported_schema_target(fixture.identity.model, &fixture.identity.firmware),
        "the field-specific update must not relax the generic schema gate"
    );
    Ok(())
}

#[test]
fn current_name_requires_exact_padding_and_noop_is_explicitly_rejected() -> TestResult {
    let fixture = fixture()?;
    let current = Pm1Name::new("PM1")?;
    let desired = Pm1Name::new("Other")?;
    assert!(
        matches!(
            Pm1NameUpdate::prepare(&fixture.identity, &fixture.original, &current, &current),
            Err(TextFieldUpdateError::NoChange)
        ),
        "equal expected and requested names must refuse a write"
    );
    for name in ["PM2", "pm1", "PM1 "] {
        assert!(
            matches!(
                Pm1NameUpdate::prepare(
                    &fixture.identity,
                    &fixture.original,
                    &Pm1Name::new(name)?,
                    &desired
                ),
                Err(TextFieldUpdateError::CurrentValueMismatch)
            ),
            "expected name {name:?} must match the captured field exactly"
        );
    }
    for index in 13..26 {
        let mut changed = fixture.original;
        *changed.get_mut(index).ok_or("padding byte")? = 0xFF;
        assert!(
            matches!(
                Pm1NameUpdate::prepare(&fixture.identity, &changed, &current, &desired),
                Err(TextFieldUpdateError::CurrentValueMismatch)
            ),
            "nonzero padding at page byte {index} must be refused"
        );
    }
    let mut full = fixture.original;
    full.get_mut(10..26)
        .ok_or("full name")?
        .copy_from_slice(b"1234567890123456");
    let update = Pm1NameUpdate::prepare(
        &fixture.identity,
        &full,
        &Pm1Name::new("1234567890123456")?,
        &Pm1Name::new(" ")?,
    )?;
    assert_eq!(
        update.desired_page().get(10..26),
        Some(b" \0\0\0\0\0\0\0\0\0\0\0\0\0\0\0".as_slice())
    );
    Ok(())
}

#[test]
fn preparation_rejects_partial_pages_and_every_other_exact_target() -> TestResult {
    let fixture = fixture()?;
    let current = current(&fixture)?;
    let desired = requested(&fixture)?;
    for len in [0, 16, 255, 257, 512] {
        assert!(
            matches!(
                Pm1NameUpdate::prepare(&fixture.identity, &vec![0; len], current, desired),
                Err(TextFieldUpdateError::PageLength { actual }) if actual == len
            ),
            "a {len}-byte input is not a complete canonical page"
        );
    }
    for firmware in ["1.00", "1.01", "1.03", "1.02E"] {
        let mut identity = fixture.identity.clone();
        identity.firmware = FirmwareIdentity::new(firmware)?;
        assert!(
            matches!(
                Pm1NameUpdate::prepare(&identity, &fixture.original, current, desired),
                Err(TextFieldUpdateError::IdentityMismatch)
            ),
            "firmware {firmware} must not inherit the PM1 exception"
        );
    }
    for radio_type in ["K,2,0", "E,2,1", "K,1,1", "K21"] {
        let mut identity = fixture.identity.clone();
        identity.radio_type = RadioType::new(radio_type)?;
        assert!(
            matches!(
                Pm1NameUpdate::prepare(&identity, &fixture.original, current, desired),
                Err(TextFieldUpdateError::IdentityMismatch)
            ),
            "radio type {radio_type} must not inherit the PM1 exception"
        );
    }
    Ok(())
}

fn altered_descriptors(field: MenuField) -> [MenuField; 12] {
    [
        MenuField {
            descriptor: FieldDescriptor {
                name: "pm.PmName2",
                ..field.descriptor
            },
            ..field
        },
        MenuField {
            descriptor: FieldDescriptor {
                base: FIELD_ADDRESS + 1,
                ..field.descriptor
            },
            ..field
        },
        MenuField {
            descriptor: FieldDescriptor {
                terms: &[SLOT_TERM],
                ..field.descriptor
            },
            ..field
        },
        MenuField {
            descriptor: FieldDescriptor {
                codec: FieldCodec::Bytes { len: 16 },
                ..field.descriptor
            },
            ..field
        },
        MenuField {
            descriptor: FieldDescriptor {
                codec: FieldCodec::FixedString {
                    len: 17,
                    encoding: StringEncoding::Utf8,
                    padding: 0,
                },
                ..field.descriptor
            },
            ..field
        },
        MenuField {
            descriptor: FieldDescriptor {
                codec: FieldCodec::FixedString {
                    len: 16,
                    encoding: StringEncoding::MemoryMap,
                    padding: 0,
                },
                ..field.descriptor
            },
            ..field
        },
        MenuField {
            descriptor: FieldDescriptor {
                codec: FieldCodec::FixedString {
                    len: 16,
                    encoding: StringEncoding::Utf8,
                    padding: 32,
                },
                ..field.descriptor
            },
            ..field
        },
        MenuField {
            is_blob: true,
            ..field
        },
        MenuField {
            menu: "dv",
            ..field
        },
        MenuField {
            enum_type: Some("changed"),
            ..field
        },
        MenuField {
            allowed_values: &[0],
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
fn changed_generated_descriptor_cannot_expand_the_single_field_scope() -> TestResult {
    let fixture = fixture()?;
    let field = *menu_field(FIELD_NAME).ok_or("PM1 field")?;
    for field in &altered_descriptors(field) {
        assert!(
            matches!(
                Pm1NameUpdate::prepare_with_field(
                    &fixture.identity,
                    &fixture.original,
                    current(&fixture)?,
                    requested(&fixture)?,
                    field
                ),
                Err(TextFieldUpdateError::UnsupportedDescriptor)
            ),
            "changed descriptor must not alter the allowed field: {field:?}"
        );
    }
    Ok(())
}

#[test]
fn verification_requires_intent_readback_two_distinct_sessions_and_both_finalizations() -> TestResult
{
    let mut fixture = fixture()?;
    fresh(&mut fixture, 1)?;
    assert_eq!(fixture.update.status(), TextFieldUpdateStatus::NotWritten);
    assert_eq!(
        fixture.update.next_session(),
        Err(TextFieldUpdateError::UnexpectedEvent)
    );
    intent(&mut fixture)?;
    assert_eq!(
        fixture.update.status(),
        TextFieldUpdateStatus::PossiblyChanged
    );
    readback(&mut fixture)?;
    assert_eq!(
        fixture.update.status(),
        TextFieldUpdateStatus::PossiblyChanged
    );
    finalize(&mut fixture, 1)?;
    assert_eq!(fixture.update.next_session()?, Session::Verify);
    assert_eq!(
        fixture.update.status(),
        TextFieldUpdateStatus::PossiblyChanged
    );
    fresh(&mut fixture, 2)?;
    assert_eq!(
        fixture.update.status(),
        TextFieldUpdateStatus::PossiblyChanged
    );
    finalize(&mut fixture, 2)?;
    assert_eq!(
        fixture.update.status(),
        TextFieldUpdateStatus::VerifiedAcrossSessions
    );
    assert_eq!(
        fixture.update.next_session(),
        Err(TextFieldUpdateError::TerminalState)
    );
    assert_eq!(
        fixture
            .update
            .record(TextFieldUpdateEvent::DurableWriteIntent { id: id(2)? }),
        Err(TextFieldUpdateError::TerminalState)
    );
    fixture.update.halt();
    assert_eq!(
        fixture.update.status(),
        TextFieldUpdateStatus::VerifiedAcrossSessions
    );
    Ok(())
}

#[test]
fn every_byte_of_fresh_immediate_and_verification_pages_must_match() -> TestResult {
    for stage in 0..3 {
        for offset in 0..PAGE_SIZE {
            let mut fixture = fixture()?;
            let mut page = if stage == 0 {
                fixture.original
            } else {
                fixture.desired
            };
            *page.get_mut(offset).ok_or("mutation offset")? ^= 1;
            let event = if stage == 1 {
                fresh(&mut fixture, 1)?;
                intent(&mut fixture)?;
                TextFieldUpdateEvent::ImmediateReadback { whole_page: &page }
            } else {
                if stage == 2 {
                    applied(&mut fixture)?;
                }
                TextFieldUpdateEvent::FreshSession {
                    id: id(if stage == 0 { 1 } else { 2 })?,
                    identity: &fixture.identity,
                    memory_format: 0,
                    whole_page: &page,
                    guards: (),
                }
            };
            assert_eq!(
                fixture.update.record(event),
                Err(TextFieldUpdateError::PageMismatch),
                "stage {stage}, offset {offset}"
            );
            assert_eq!(
                fixture.update.status(),
                if stage == 0 {
                    TextFieldUpdateStatus::NotWritten
                } else {
                    TextFieldUpdateStatus::PossiblyChanged
                }
            );
            assert_eq!(
                fixture.update.next_session(),
                Err(TextFieldUpdateError::TerminalState)
            );
        }
    }
    Ok(())
}

#[test]
fn partial_evidence_is_rejected_at_every_comparison() -> TestResult {
    for stage in 0..3 {
        for len in [0, 16, 255, 257] {
            let mut fixture = fixture()?;
            let bytes = vec![0; len];
            let event = if stage == 1 {
                fresh(&mut fixture, 1)?;
                intent(&mut fixture)?;
                TextFieldUpdateEvent::ImmediateReadback { whole_page: &bytes }
            } else {
                if stage == 2 {
                    applied(&mut fixture)?;
                }
                TextFieldUpdateEvent::FreshSession {
                    id: id(if stage == 0 { 1 } else { 2 })?,
                    identity: &fixture.identity,
                    memory_format: 0,
                    whole_page: &bytes,
                    guards: (),
                }
            };
            assert_eq!(
                fixture.update.record(event),
                Err(TextFieldUpdateError::PageLength { actual: len })
            );
        }
    }
    Ok(())
}

#[test]
fn fresh_identity_and_format_are_checked_again_in_the_verification_session() -> TestResult {
    for session in 1..=2 {
        for changed in 0..3 {
            let mut fixture = fixture()?;
            if session == 2 {
                applied(&mut fixture)?;
            }
            let mut identity = fixture.identity.clone();
            match changed {
                0 => identity.firmware = FirmwareIdentity::new("1.03")?,
                1 => identity.radio_type = RadioType::new("K,2,0")?,
                _ => {}
            }
            assert_eq!(
                fixture.update.record(TextFieldUpdateEvent::FreshSession {
                    id: id(session)?,
                    identity: &identity,
                    memory_format: u8::from(changed == 2),
                    whole_page: if session == 1 {
                        &fixture.original
                    } else {
                        &fixture.desired
                    },
                    guards: (),
                }),
                Err(if changed == 2 {
                    TextFieldUpdateError::MemoryFormat { actual: 1 }
                } else {
                    TextFieldUpdateError::IdentityMismatch
                })
            );
            assert_eq!(
                fixture.update.status(),
                if session == 1 {
                    TextFieldUpdateStatus::NotWritten
                } else {
                    TextFieldUpdateStatus::PossiblyChanged
                }
            );
        }
    }
    Ok(())
}

#[test]
fn reused_sessions_and_wrong_finalization_ids_halt_without_clearing_write_risk() -> TestResult {
    let mut reused = fixture()?;
    applied(&mut reused)?;
    assert_eq!(
        reused.update.record(TextFieldUpdateEvent::FreshSession {
            id: id(1)?,
            identity: &reused.identity,
            memory_format: 0,
            whole_page: &reused.desired,
            guards: (),
        }),
        Err(TextFieldUpdateError::ReusedSession)
    );
    assert_eq!(
        reused.update.status(),
        TextFieldUpdateStatus::PossiblyChanged
    );
    for session in 1..=2 {
        let mut fixture = fixture()?;
        if session == 2 {
            applied(&mut fixture)?;
            fresh(&mut fixture, 2)?;
        } else {
            fresh(&mut fixture, 1)?;
            intent(&mut fixture)?;
            readback(&mut fixture)?;
        }
        assert_eq!(
            fixture
                .update
                .record(TextFieldUpdateEvent::SessionFinalized {
                    id: id(99)?,
                    guards: (),
                }),
            Err(TextFieldUpdateError::SessionMismatch)
        );
        assert_eq!(
            fixture.update.status(),
            TextFieldUpdateStatus::PossiblyChanged
        );
    }
    Ok(())
}

#[test]
fn skipped_intent_and_repeated_intent_never_advance_the_update() -> TestResult {
    let mut early = fixture()?;
    assert_eq!(
        early
            .update
            .record(TextFieldUpdateEvent::DurableWriteIntent { id: id(1)? }),
        Err(TextFieldUpdateError::UnexpectedEvent)
    );
    assert_eq!(early.update.status(), TextFieldUpdateStatus::NotWritten);
    let mut skipped = fixture()?;
    fresh(&mut skipped, 1)?;
    assert_eq!(
        skipped
            .update
            .record(TextFieldUpdateEvent::ImmediateReadback {
                whole_page: &skipped.desired,
            }),
        Err(TextFieldUpdateError::UnexpectedEvent)
    );
    assert_eq!(skipped.update.status(), TextFieldUpdateStatus::NotWritten);
    for second_id in [1, 2] {
        let mut repeated = fixture()?;
        fresh(&mut repeated, 1)?;
        intent(&mut repeated)?;
        assert_eq!(
            repeated
                .update
                .record(TextFieldUpdateEvent::DurableWriteIntent { id: id(second_id)? }),
            Err(TextFieldUpdateError::UnexpectedEvent)
        );
        assert_eq!(
            repeated.update.status(),
            TextFieldUpdateStatus::PossiblyChanged
        );
    }
    Ok(())
}

#[test]
fn neither_readback_nor_fresh_verification_can_skip_a_finalization() -> TestResult {
    let mut missing_readback = fixture()?;
    fresh(&mut missing_readback, 1)?;
    intent(&mut missing_readback)?;
    assert_eq!(
        missing_readback
            .update
            .record(TextFieldUpdateEvent::SessionFinalized {
                id: id(1)?,
                guards: (),
            }),
        Err(TextFieldUpdateError::UnexpectedEvent)
    );
    let mut missing_finalize = fixture()?;
    fresh(&mut missing_finalize, 1)?;
    intent(&mut missing_finalize)?;
    readback(&mut missing_finalize)?;
    assert_eq!(
        missing_finalize
            .update
            .record(TextFieldUpdateEvent::FreshSession {
                id: id(2)?,
                identity: &missing_finalize.identity,
                memory_format: 0,
                whole_page: &missing_finalize.desired,
                guards: (),
            }),
        Err(TextFieldUpdateError::UnexpectedEvent)
    );
    let mut missing_verification = fixture()?;
    applied(&mut missing_verification)?;
    assert_eq!(
        missing_verification
            .update
            .record(TextFieldUpdateEvent::SessionFinalized {
                id: id(2)?,
                guards: (),
            }),
        Err(TextFieldUpdateError::UnexpectedEvent)
    );
    Ok(())
}

#[test]
fn explicit_halt_and_errors_are_irreversible_at_each_nonterminal_stage() -> TestResult {
    for stage in 0..6 {
        let mut fixture = fixture()?;
        if stage >= 1 {
            fresh(&mut fixture, 1)?;
        }
        if stage >= 2 {
            intent(&mut fixture)?;
        }
        if stage >= 3 {
            readback(&mut fixture)?;
        }
        if stage >= 4 {
            finalize(&mut fixture, 1)?;
        }
        if stage >= 5 {
            fresh(&mut fixture, 2)?;
        }
        fixture.update.halt();
        assert_eq!(
            fixture.update.status(),
            if stage < 2 {
                TextFieldUpdateStatus::NotWritten
            } else {
                TextFieldUpdateStatus::PossiblyChanged
            }
        );
        assert_eq!(
            fixture.update.next_session(),
            Err(TextFieldUpdateError::TerminalState)
        );
        assert_eq!(
            fixture
                .update
                .record(TextFieldUpdateEvent::SessionFinalized {
                    id: id(1)?,
                    guards: (),
                }),
            Err(TextFieldUpdateError::TerminalState)
        );
    }
    Ok(())
}
