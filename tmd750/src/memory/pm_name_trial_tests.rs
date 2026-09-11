use super::*;
use crate::memory::{FieldDescriptor, SLOT_TERM, StorageTransform, is_supported_schema_target};
use crate::types::{FirmwareIdentity, RadioType};

type TestResult = Result<(), Box<dyn std::error::Error>>;

struct Fixture {
    trial: PmNameTrial,
    identity: Identity,
    original: [u8; PAGE_SIZE],
    expected: [u8; PAGE_SIZE],
}

fn connection(value: u64) -> Result<NonZeroU64, Box<dyn std::error::Error>> {
    NonZeroU64::new(value).ok_or_else(|| "test connection ID must be nonzero".into())
}

fn fixture() -> Result<Fixture, Box<dyn std::error::Error>> {
    let identity = Identity {
        model: RadioModel::TmD750,
        firmware: FirmwareIdentity::new("1.02")?,
        radio_type: RadioType::new("K,2,1")?,
    };
    let mut original = [0xA5; PAGE_SIZE];
    original.get_mut(10..26).ok_or("name range")?.fill(0);
    original
        .get_mut(10..13)
        .ok_or("name bytes")?
        .copy_from_slice(b"PM1");
    let trial = PmNameTrial::prepare_unqualified_offline(&identity, &original, "PM1")?;
    let expected = *trial.expected_page();
    Ok(Fixture {
        trial,
        identity,
        original,
        expected,
    })
}

fn fresh(fixture: &mut Fixture, session: u64, changed: bool) -> TestResult {
    fixture.trial.record(PmNameTrialEvent::FreshSession {
        id: connection(session)?,
        identity: &fixture.identity,
        memory_format: 0,
        whole_page: if changed {
            &fixture.expected
        } else {
            &fixture.original
        },
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

fn finalize(fixture: &mut Fixture, session: u64) -> TestResult {
    fixture.trial.record(PmNameTrialEvent::SessionFinalized {
        id: connection(session)?,
    })?;
    Ok(())
}

fn renamed(fixture: &mut Fixture) -> TestResult {
    fresh(fixture, 1, false)?;
    intent(fixture, PmNameTrialWrite::Rename)?;
    readback(fixture, true)?;
    finalize(fixture, 1)
}

fn restored(fixture: &mut Fixture) -> TestResult {
    renamed(fixture)?;
    fresh(fixture, 2, true)?;
    intent(fixture, PmNameTrialWrite::Restore)?;
    readback(fixture, false)?;
    finalize(fixture, 2)
}

#[test]
fn preparation_preserves_whole_page_and_changes_only_fixed_pm1_bytes() -> TestResult {
    let fixture = fixture()?;
    assert_eq!(PmNameTrial::required_page()?, fixture.trial.page());
    assert_eq!(fixture.trial.page().address().as_u32(), 323_584);
    assert_eq!(fixture.trial.page().len(), 256);
    assert_eq!(fixture.trial.original_page(), &fixture.original);
    assert_eq!(fixture.trial.identity(), &fixture.identity);
    assert_eq!(
        fixture.expected.get(10..26),
        Some(b"PC TEXT TEST\0\0\0\0".as_slice())
    );
    for (index, (original, expected)) in fixture.original.iter().zip(&fixture.expected).enumerate()
    {
        if !(10..26).contains(&index) {
            assert_eq!(original, expected, "unrelated byte {index}");
        }
    }
    assert_eq!(fixture.trial.status(), PmNameTrialStatus::NotWritten);
    assert_eq!(fixture.trial.next_session()?, PmNameTrialSession::Rename);
    assert!(!is_supported_schema_target(
        fixture.identity.model,
        &fixture.identity.firmware
    ));
    Ok(())
}

#[test]
fn independent_display_text_must_be_safe_exact_and_not_a_noop() -> TestResult {
    let fixture = fixture()?;
    for name in ["", "12345678901234567", "PM\n1", "PM\0", "P\u{7f}M", "PMé"] {
        let result =
            PmNameTrial::prepare_unqualified_offline(&fixture.identity, &fixture.original, name);
        assert!(
            matches!(result, Err(PmNameTrialError::InvalidName)),
            "{name:?}: {result:?}"
        );
    }
    for name in ["PM2", "pm1", "PM1 "] {
        let result =
            PmNameTrial::prepare_unqualified_offline(&fixture.identity, &fixture.original, name);
        assert!(
            matches!(result, Err(PmNameTrialError::DisplayNameMismatch)),
            "{name:?}: {result:?}"
        );
    }
    let noop = PmNameTrial::prepare_unqualified_offline(
        &fixture.identity,
        &fixture.expected,
        PmNameTrial::TEMPORARY_NAME,
    );
    assert!(
        matches!(noop, Err(PmNameTrialError::AlreadyTemporaryName)),
        "{noop:?}"
    );
    let mut malformed = fixture.original;
    *malformed.get_mut(25).ok_or("padding byte")? = 0xFF;
    assert!(matches!(
        PmNameTrial::prepare_unqualified_offline(&fixture.identity, &malformed, "PM1"),
        Err(PmNameTrialError::DisplayNameMismatch)
    ));
    Ok(())
}

#[test]
fn preparation_rejects_every_wrong_page_length_and_exact_target_mismatch() -> TestResult {
    let fixture = fixture()?;
    for len in [0, 16, 255, 257, 512] {
        let result =
            PmNameTrial::prepare_unqualified_offline(&fixture.identity, &vec![0; len], "PM1");
        assert!(matches!(result, Err(PmNameTrialError::PageLength { actual }) if actual == len));
    }
    for firmware in ["1.00", "1.01", "1.03", "1.02E"] {
        let mut identity = fixture.identity.clone();
        identity.firmware = FirmwareIdentity::new(firmware)?;
        assert!(matches!(
            PmNameTrial::prepare_unqualified_offline(&identity, &fixture.original, "PM1"),
            Err(PmNameTrialError::IdentityMismatch)
        ));
    }
    let mut identity = fixture.identity.clone();
    identity.radio_type = RadioType::new("K,2,0")?;
    assert!(matches!(
        PmNameTrial::prepare_unqualified_offline(&identity, &fixture.original, "PM1"),
        Err(PmNameTrialError::IdentityMismatch)
    ));
    Ok(())
}

#[test]
fn changed_generated_descriptor_is_never_accepted_by_the_fixed_trial() -> TestResult {
    let fixture = fixture()?;
    let field = *menu_field(FIELD_NAME).ok_or("generated PM1 field")?;
    let invalid = [
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
    ];
    for field in &invalid {
        assert!(
            matches!(
                PmNameTrial::prepare_with_field(&fixture.identity, &fixture.original, "PM1", field),
                Err(PmNameTrialError::UnsupportedDescriptor)
            ),
            "{field:?}"
        );
    }
    Ok(())
}

#[test]
fn restoration_is_verified_only_after_all_three_independent_finalized_sessions() -> TestResult {
    let mut fixture = fixture()?;
    fresh(&mut fixture, 1, false)?;
    assert_eq!(fixture.trial.status(), PmNameTrialStatus::NotWritten);
    assert_eq!(
        fixture.trial.next_session(),
        Err(PmNameTrialError::UnexpectedEvent)
    );
    intent(&mut fixture, PmNameTrialWrite::Rename)?;
    assert_eq!(fixture.trial.status(), PmNameTrialStatus::PossiblyChanged);
    readback(&mut fixture, true)?;
    assert_eq!(fixture.trial.status(), PmNameTrialStatus::PossiblyChanged);
    finalize(&mut fixture, 1)?;
    assert_eq!(fixture.trial.next_session()?, PmNameTrialSession::Restore);
    fresh(&mut fixture, 2, true)?;
    intent(&mut fixture, PmNameTrialWrite::Restore)?;
    readback(&mut fixture, false)?;
    finalize(&mut fixture, 2)?;
    assert_eq!(fixture.trial.status(), PmNameTrialStatus::PossiblyChanged);
    assert_eq!(
        fixture.trial.next_session()?,
        PmNameTrialSession::VerifyRestoration
    );
    fresh(&mut fixture, 3, false)?;
    assert_eq!(fixture.trial.status(), PmNameTrialStatus::PossiblyChanged);
    finalize(&mut fixture, 3)?;
    assert_eq!(
        fixture.trial.status(),
        PmNameTrialStatus::RestorationVerified
    );
    assert_eq!(
        fixture.trial.next_session(),
        Err(PmNameTrialError::TerminalState)
    );
    assert_eq!(
        fixture.trial.record(PmNameTrialEvent::DurableWriteIntent {
            id: connection(13)?,
            write: PmNameTrialWrite::Rename,
        }),
        Err(PmNameTrialError::TerminalState)
    );
    fixture.trial.halt();
    assert_eq!(
        fixture.trial.status(),
        PmNameTrialStatus::RestorationVerified
    );
    Ok(())
}

#[test]
fn every_original_page_byte_is_compared_before_any_write_intent() -> TestResult {
    for offset in 0..PAGE_SIZE {
        let mut fixture = fixture()?;
        let mut stale = fixture.original;
        *stale.get_mut(offset).ok_or("page byte")? ^= 1;
        assert_eq!(
            fixture.trial.record(PmNameTrialEvent::FreshSession {
                id: connection(1)?,
                identity: &fixture.identity,
                memory_format: 0,
                whole_page: &stale,
            }),
            Err(PmNameTrialError::PageMismatch),
            "offset {offset}"
        );
        assert_eq!(fixture.trial.status(), PmNameTrialStatus::NotWritten);
        assert_eq!(
            fixture.trial.next_session(),
            Err(PmNameTrialError::TerminalState)
        );
    }
    Ok(())
}

#[test]
fn immediate_readbacks_compare_every_byte_and_never_erase_the_obligation() -> TestResult {
    for write in [PmNameTrialWrite::Rename, PmNameTrialWrite::Restore] {
        for offset in 0..PAGE_SIZE {
            let mut fixture = fixture()?;
            if write == PmNameTrialWrite::Restore {
                renamed(&mut fixture)?;
                fresh(&mut fixture, 2, true)?;
            } else {
                fresh(&mut fixture, 1, false)?;
            }
            intent(&mut fixture, write)?;
            let mut wrong = match write {
                PmNameTrialWrite::Rename => fixture.expected,
                PmNameTrialWrite::Restore => fixture.original,
            };
            *wrong.get_mut(offset).ok_or("page byte")? ^= 1;
            assert_eq!(
                fixture
                    .trial
                    .record(PmNameTrialEvent::ImmediateReadback { whole_page: &wrong }),
                Err(PmNameTrialError::PageMismatch),
                "{write:?} offset {offset}"
            );
            assert_eq!(fixture.trial.status(), PmNameTrialStatus::PossiblyChanged);
            assert_eq!(
                fixture.trial.next_session(),
                Err(PmNameTrialError::TerminalState)
            );
        }
    }
    Ok(())
}

#[test]
fn both_persistence_sessions_compare_every_byte_before_advancing() -> TestResult {
    for session in [2, 3] {
        for offset in 0..PAGE_SIZE {
            let mut fixture = fixture()?;
            if session == 2 {
                renamed(&mut fixture)?;
            } else {
                restored(&mut fixture)?;
            }
            let mut wrong = if session == 2 {
                fixture.expected
            } else {
                fixture.original
            };
            *wrong.get_mut(offset).ok_or("page byte")? ^= 1;
            assert_eq!(
                fixture.trial.record(PmNameTrialEvent::FreshSession {
                    id: connection(session)?,
                    identity: &fixture.identity,
                    memory_format: 0,
                    whole_page: &wrong,
                }),
                Err(PmNameTrialError::PageMismatch),
                "session {session} offset {offset}"
            );
            assert_eq!(fixture.trial.status(), PmNameTrialStatus::PossiblyChanged);
            assert_eq!(
                fixture.trial.record(PmNameTrialEvent::DurableWriteIntent {
                    id: connection(12)?,
                    write: PmNameTrialWrite::Restore,
                }),
                Err(PmNameTrialError::TerminalState)
            );
        }
    }
    Ok(())
}

#[test]
fn every_session_requires_new_matching_identity_format_and_full_page() -> TestResult {
    for session in [1, 2, 3] {
        for failure in ["firmware", "type", "format", "length"] {
            let mut fixture = fixture()?;
            if session == 2 {
                renamed(&mut fixture)?;
            }
            if session == 3 {
                restored(&mut fixture)?;
            }
            let mut identity = fixture.identity.clone();
            if failure == "firmware" {
                identity.firmware = FirmwareIdentity::new("1.00")?;
            }
            if failure == "type" {
                identity.radio_type = RadioType::new("K,2,0")?;
            }
            let page = if session == 2 {
                &fixture.expected
            } else {
                &fixture.original
            };
            let result = fixture.trial.record(PmNameTrialEvent::FreshSession {
                id: connection(session)?,
                identity: &identity,
                memory_format: u8::from(failure == "format"),
                whole_page: if failure == "length" {
                    page.get(..255).ok_or("short page")?
                } else {
                    page
                },
            });
            let expected = match failure {
                "format" => PmNameTrialError::MemoryFormat { actual: 1 },
                "length" => PmNameTrialError::PageLength { actual: 255 },
                _ => PmNameTrialError::IdentityMismatch,
            };
            assert_eq!(result, Err(expected), "session {session}, {failure}");
            assert_eq!(
                fixture.trial.status(),
                if session == 1 {
                    PmNameTrialStatus::NotWritten
                } else {
                    PmNameTrialStatus::PossiblyChanged
                }
            );
        }
    }
    Ok(())
}

#[test]
fn readback_requires_a_prior_durable_intent_and_intents_cannot_be_reordered() -> TestResult {
    let mut missing_intent = fixture()?;
    fresh(&mut missing_intent, 1, false)?;
    assert_eq!(
        missing_intent
            .trial
            .record(PmNameTrialEvent::ImmediateReadback {
                whole_page: &missing_intent.expected,
            }),
        Err(PmNameTrialError::UnexpectedEvent)
    );
    assert_eq!(missing_intent.trial.status(), PmNameTrialStatus::NotWritten);
    let mut early_intent = fixture()?;
    assert_eq!(
        early_intent
            .trial
            .record(PmNameTrialEvent::DurableWriteIntent {
                id: connection(11)?,
                write: PmNameTrialWrite::Rename,
            }),
        Err(PmNameTrialError::UnexpectedEvent)
    );
    let mut wrong_intent = fixture()?;
    fresh(&mut wrong_intent, 1, false)?;
    assert_eq!(
        wrong_intent
            .trial
            .record(PmNameTrialEvent::DurableWriteIntent {
                id: connection(11)?,
                write: PmNameTrialWrite::Restore,
            }),
        Err(PmNameTrialError::UnexpectedEvent)
    );
    assert_eq!(wrong_intent.trial.status(), PmNameTrialStatus::NotWritten);
    Ok(())
}

#[test]
fn repeated_sessions_and_durable_intents_are_rejected_without_stale_restore() -> TestResult {
    for session in [2, 3] {
        let mut fixture = fixture()?;
        if session == 2 {
            renamed(&mut fixture)?;
        } else {
            restored(&mut fixture)?;
        }
        assert_eq!(
            fixture.trial.record(PmNameTrialEvent::FreshSession {
                id: connection(1)?,
                identity: &fixture.identity,
                memory_format: 0,
                whole_page: if session == 2 {
                    &fixture.expected
                } else {
                    &fixture.original
                },
            }),
            Err(PmNameTrialError::ReusedSession)
        );
        assert_eq!(fixture.trial.status(), PmNameTrialStatus::PossiblyChanged);
    }
    let mut fixture = fixture()?;
    renamed(&mut fixture)?;
    fresh(&mut fixture, 2, true)?;
    assert_eq!(
        fixture.trial.record(PmNameTrialEvent::DurableWriteIntent {
            id: connection(11)?,
            write: PmNameTrialWrite::Restore,
        }),
        Err(PmNameTrialError::ReusedWriteIntent)
    );
    assert_eq!(fixture.trial.status(), PmNameTrialStatus::PossiblyChanged);
    Ok(())
}

#[test]
fn finalization_cannot_skip_readback_or_refer_to_another_session() -> TestResult {
    let mut missing_readback = fixture()?;
    fresh(&mut missing_readback, 1, false)?;
    intent(&mut missing_readback, PmNameTrialWrite::Rename)?;
    assert_eq!(
        missing_readback
            .trial
            .record(PmNameTrialEvent::SessionFinalized { id: connection(1)? }),
        Err(PmNameTrialError::UnexpectedEvent)
    );
    assert_eq!(
        missing_readback.trial.status(),
        PmNameTrialStatus::PossiblyChanged
    );
    let mut wrong_session = fixture()?;
    fresh(&mut wrong_session, 1, false)?;
    intent(&mut wrong_session, PmNameTrialWrite::Rename)?;
    readback(&mut wrong_session, true)?;
    assert_eq!(
        wrong_session
            .trial
            .record(PmNameTrialEvent::SessionFinalized { id: connection(2)? }),
        Err(PmNameTrialError::SessionMismatch)
    );
    assert_eq!(
        wrong_session.trial.status(),
        PmNameTrialStatus::PossiblyChanged
    );
    Ok(())
}

#[test]
fn halt_preserves_risk_before_and_after_possible_dispatch_and_rejects_all_events() -> TestResult {
    for recorded_intent in [false, true] {
        let mut fixture = fixture()?;
        fresh(&mut fixture, 1, false)?;
        if recorded_intent {
            intent(&mut fixture, PmNameTrialWrite::Rename)?;
        }
        fixture.trial.halt();
        assert_eq!(
            fixture.trial.status(),
            if recorded_intent {
                PmNameTrialStatus::PossiblyChanged
            } else {
                PmNameTrialStatus::NotWritten
            }
        );
        assert_eq!(
            fixture.trial.record(PmNameTrialEvent::ImmediateReadback {
                whole_page: &fixture.original,
            }),
            Err(PmNameTrialError::TerminalState)
        );
        assert_eq!(
            fixture.trial.next_session(),
            Err(PmNameTrialError::TerminalState)
        );
    }
    Ok(())
}
