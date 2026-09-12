use super::*;
use crate::memory::{MenuOption, StorageTransform, Term, is_supported_schema_target};
use crate::types::{FirmwareIdentity, RadioType};

type TestResult = Result<(), Box<dyn std::error::Error>>;

struct Captured {
    identity: Identity,
    off: [u8; PAGE_SIZE],
    active: [u8; PAGE_SIZE],
    control: [u8; PAGE_SIZE],
    routing: [u8; PAGE_SIZE],
}

struct Fixture {
    trial: TerminalExitTrial,
    captured: Captured,
}

fn identifier(value: u64) -> Result<NonZeroU64, Box<dyn std::error::Error>> {
    NonZeroU64::new(value).ok_or_else(|| "test evidence identifier must be nonzero".into())
}

fn fixture() -> Result<Fixture, Box<dyn std::error::Error>> {
    let identity = Identity {
        model: RadioModel::TmD750,
        firmware: FirmwareIdentity::new("1.02")?,
        radio_type: RadioType::new("K,2,1")?,
    };
    let mut off = [0xA5; PAGE_SIZE];
    *off.first_mut().ok_or("Gateway selector")? = 0;
    *off.get_mut(2).ok_or("Terminal subtype")? = 0;
    off.get_mut(8..16)
        .ok_or("existing MY1")?
        .copy_from_slice(b"N0CALL A");
    let mut control = [0x5A; PAGE_SIZE];
    *control.get_mut(9).ok_or("PM selector")? = 0;
    let mut routing = [0xC3; PAGE_SIZE];
    *routing.get_mut(71).ok_or("USB function")? = 0;
    *routing.get_mut(77).ok_or("Gateway route")? = 1;
    let trial =
        TerminalExitTrial::prepare_unqualified_offline(&identity, &off, &control, &routing)?;
    let active = *trial.expected_active_page();
    Ok(Fixture {
        trial,
        captured: Captured {
            identity,
            off,
            active,
            control,
            routing,
        },
    })
}

impl Captured {
    fn fresh(&self, session: Session, id: NonZeroU64) -> TerminalExitTrialEvent<'_> {
        TerminalExitTrialEvent::FreshSession {
            id,
            identity: &self.identity,
            memory_format: 0,
            gateway_mode: match session {
                Session::Apply => DvGatewayMode::Terminal,
                Session::Verify => DvGatewayMode::Off,
            },
            whole_page: match session {
                Session::Apply => &self.active,
                Session::Verify => &self.off,
            },
            control_page: &self.control,
            routing_page: &self.routing,
        }
    }

    fn finalized(&self, id: NonZeroU64) -> TerminalExitTrialEvent<'_> {
        TerminalExitTrialEvent::SessionFinalized {
            id,
            identity: &self.identity,
            gateway_mode: DvGatewayMode::Off,
        }
    }
}

fn apply(fixture: &mut Fixture) -> TestResult {
    fixture
        .trial
        .record(fixture.captured.fresh(Session::Apply, identifier(1)?))?;
    fixture
        .trial
        .record(TerminalExitTrialEvent::DurableWriteIntent {
            id: identifier(11)?,
        })?;
    fixture
        .trial
        .record(TerminalExitTrialEvent::ImmediateReadback {
            whole_page: &fixture.captured.off,
        })?;
    fixture
        .trial
        .record(fixture.captured.finalized(identifier(1)?))?;
    Ok(())
}

fn waiting_for(session: Session) -> Result<Fixture, Box<dyn std::error::Error>> {
    let mut fixture = fixture()?;
    if session == Session::Verify {
        apply(&mut fixture)?;
    }
    Ok(fixture)
}

fn assert_halted(fixture: &Fixture, status: TerminalExitTrialStatus) {
    assert!(
        fixture.trial.is_halted(),
        "invalid evidence must permanently halt the instance"
    );
    assert_eq!(
        fixture.trial.status(),
        status,
        "halt must preserve conservative write status"
    );
    assert_eq!(
        fixture.trial.next_session(),
        Err(TerminalExitTrialError::TerminalState)
    );
}

const fn prior_status(session: Session) -> TerminalExitTrialStatus {
    match session {
        Session::Apply => TerminalExitTrialStatus::NotWritten,
        Session::Verify => TerminalExitTrialStatus::PossiblyChanged,
    }
}

#[test]
fn preparation_changes_only_the_gateway_byte_and_keeps_arbitrary_callsigns() -> TestResult {
    let fixture = fixture()?;
    let trial = &fixture.trial;
    assert_eq!(trial.identity(), &fixture.captured.identity);
    assert_eq!(trial.page(), TerminalExitTrial::required_page()?);
    assert_eq!(trial.page().address().as_u32(), TARGET_PAGE_ADDRESS);
    assert_eq!(trial.page().len(), PAGE_SIZE);
    assert_eq!(
        trial.control_page_spec(),
        TerminalExitTrial::required_control_page()?
    );
    assert_eq!(
        trial.control_page_spec().address().as_u32(),
        CONTROL_PAGE_ADDRESS
    );
    assert_eq!(trial.control_page_spec().len(), PAGE_SIZE);
    assert_eq!(
        trial.routing_page_spec(),
        TerminalExitTrial::required_routing_page()?
    );
    assert_eq!(
        trial.routing_page_spec().address().as_u32(),
        ROUTING_PAGE_ADDRESS
    );
    assert_eq!(trial.routing_page_spec().len(), PAGE_SIZE);
    assert_eq!(trial.off_page(), &fixture.captured.off);
    assert_eq!(trial.control_page(), &fixture.captured.control);
    assert_eq!(trial.routing_page(), &fixture.captured.routing);
    assert_eq!(trial.expected_active_page().first(), Some(&2));
    for (offset, (off, active)) in trial
        .off_page()
        .iter()
        .zip(trial.expected_active_page())
        .enumerate()
    {
        if offset != 0 {
            assert_eq!(off, active, "only Gateway may differ; byte {offset}");
        }
    }
    assert_eq!(
        trial.expected_active_page().get(8..16),
        Some(b"N0CALL A".as_slice())
    );
    assert_eq!(trial.status(), TerminalExitTrialStatus::NotWritten);
    assert_eq!(trial.next_session()?, Session::Apply);
    assert!(
        !trial.is_halted(),
        "offline preparation is an untouched sequence"
    );
    assert!(
        !is_supported_schema_target(
            fixture.captured.identity.model,
            &fixture.captured.identity.firmware
        ),
        "experimental preparation must not relax the generic 1.02 schema refusal"
    );
    Ok(())
}

#[test]
fn preparation_requires_the_exact_identity_and_three_complete_pages() -> TestResult {
    let fixture = fixture()?;
    let captured = &fixture.captured;
    for length in [0, 1, 255, 257, 512] {
        let invalid = vec![0; length];
        for (target, control, routing, error) in [
            (
                invalid.as_slice(),
                captured.control.as_slice(),
                captured.routing.as_slice(),
                TerminalExitTrialError::PageLength { actual: length },
            ),
            (
                captured.off.as_slice(),
                invalid.as_slice(),
                captured.routing.as_slice(),
                TerminalExitTrialError::ControlPageLength { actual: length },
            ),
            (
                captured.off.as_slice(),
                captured.control.as_slice(),
                invalid.as_slice(),
                TerminalExitTrialError::RoutingPageLength { actual: length },
            ),
        ] {
            let result = TerminalExitTrial::prepare_unqualified_offline(
                &captured.identity,
                target,
                control,
                routing,
            );
            assert!(
                matches!(result, Err(actual) if actual == error),
                "all pages must be complete: {result:?}"
            );
        }
    }
    for firmware in ["1.00", "1.01", "1.03", "01.02", "1.02E"] {
        let mut identity = captured.identity.clone();
        identity.firmware = FirmwareIdentity::new(firmware)?;
        let result = TerminalExitTrial::prepare_unqualified_offline(
            &identity,
            &captured.off,
            &captured.control,
            &captured.routing,
        );
        assert!(
            matches!(result, Err(TerminalExitTrialError::IdentityMismatch)),
            "firmware {firmware}: {result:?}"
        );
    }
    for radio_type in ["J,2,1", "K,1,1", "K,2,0"] {
        let mut identity = captured.identity.clone();
        identity.radio_type = RadioType::new(radio_type)?;
        let result = TerminalExitTrial::prepare_unqualified_offline(
            &identity,
            &captured.off,
            &captured.control,
            &captured.routing,
        );
        assert!(
            matches!(result, Err(TerminalExitTrialError::IdentityMismatch)),
            "type {radio_type}: {result:?}"
        );
    }
    Ok(())
}

#[test]
fn every_stored_selector_is_fixed_and_not_an_editable_input() -> TestResult {
    for actual in 0..=u8::MAX {
        for field in 0..5 {
            let mut fixture = fixture()?;
            let captured = &mut fixture.captured;
            let (byte, expected, error) = match field {
                0 => (
                    captured.control.get_mut(9),
                    0,
                    TerminalExitTrialError::PmSelection { actual },
                ),
                1 => (
                    captured.off.first_mut(),
                    0,
                    TerminalExitTrialError::StoredGatewayMode { actual },
                ),
                2 => (
                    captured.off.get_mut(2),
                    0,
                    TerminalExitTrialError::TerminalSubtype { actual },
                ),
                3 => (
                    captured.routing.get_mut(71),
                    0,
                    TerminalExitTrialError::UsbFunction { actual },
                ),
                _ => (
                    captured.routing.get_mut(77),
                    1,
                    TerminalExitTrialError::GatewayRoute { actual },
                ),
            };
            if actual == expected {
                continue;
            }
            *byte.ok_or("fixed guard field")? = actual;
            let result = TerminalExitTrial::prepare_unqualified_offline(
                &captured.identity,
                &captured.off,
                &captured.control,
                &captured.routing,
            );
            assert!(
                matches!(result, Err(found) if found == error),
                "guard {field}, raw {actual}: {result:?}"
            );
        }
    }
    Ok(())
}

fn reject_descriptor_change(index: usize, change: impl FnOnce(&mut MenuField)) -> TestResult {
    let fixture = fixture()?;
    let loaded = Descriptors::load()?;
    let mut fields = [
        *loaded.gateway,
        *loaded.subtype,
        *loaded.pm_selection,
        *loaded.usb_function,
        *loaded.gateway_route,
    ];
    change(fields.get_mut(index).ok_or("descriptor index")?);
    let [gateway, subtype, pm_selection, usb_function, gateway_route] = &fields;
    let descriptors = Descriptors {
        gateway,
        subtype,
        pm_selection,
        usb_function,
        gateway_route,
    };
    let captured = &fixture.captured;
    let result = TerminalExitTrial::prepare_with_fields(
        &captured.identity,
        &captured.off,
        &captured.control,
        &captured.routing,
        &descriptors,
    );
    assert!(
        matches!(result, Err(TerminalExitTrialError::UnsupportedDescriptor)),
        "descriptor {index} drift: {result:?}"
    );
    Ok(())
}

#[test]
fn every_descriptor_shape_and_option_domain_is_load_bearing() -> TestResult {
    const WRONG_OPTIONS: &[MenuOption] = &[MenuOption {
        raw: 1,
        member: "changed",
        label: None,
        resource_key: None,
    }; 3];
    for index in 0..5 {
        reject_descriptor_change(index, |field| field.descriptor.name = "different.Field")?;
        reject_descriptor_change(index, |field| field.descriptor.base += 1)?;
        reject_descriptor_change(index, |field| field.descriptor.codec = FieldCodec::Bool)?;
        reject_descriptor_change(index, |field| field.menu = "other")?;
        reject_descriptor_change(index, |field| field.enum_type = None)?;
        reject_descriptor_change(index, |field| field.options = WRONG_OPTIONS)?;
        reject_descriptor_change(index, |field| field.is_blob = true)?;
        reject_descriptor_change(index, |field| field.allowed_values = &[0])?;
        reject_descriptor_change(index, |field| {
            field.storage_transform = Some(StorageTransform {
                input_unit: "other",
                numerator: 1,
                denominator: 1,
            });
        })?;
        reject_descriptor_change(index, |field| {
            field.descriptor.terms = &[Term {
                dimension: "pm_slot",
                stride: 4096,
            }];
        })?;
    }
    for index in [0, 1, 3, 4] {
        reject_descriptor_change(index, |field| field.descriptor.terms = &[])?;
    }
    reject_descriptor_change(2, |field| field.descriptor.terms = &[SLOT_TERM])?;
    Ok(())
}

#[test]
fn every_byte_of_target_control_and_routing_is_compared_in_each_session() -> TestResult {
    for session in [Session::Apply, Session::Verify] {
        for offset in 0..PAGE_SIZE {
            for role in [PageRole::Target, PageRole::Control, PageRole::Routing] {
                let mut fixture = waiting_for(session)?;
                let captured = &mut fixture.captured;
                let (page, error) = match role {
                    PageRole::Target => (
                        match session {
                            Session::Apply => &mut captured.active,
                            Session::Verify => &mut captured.off,
                        },
                        TerminalExitTrialError::PageMismatch,
                    ),
                    PageRole::Control => (
                        &mut captured.control,
                        TerminalExitTrialError::ControlPageMismatch,
                    ),
                    PageRole::Routing => (
                        &mut captured.routing,
                        TerminalExitTrialError::RoutingPageMismatch,
                    ),
                };
                *page.get_mut(offset).ok_or("complete page byte")? ^= 1;
                let result = fixture
                    .trial
                    .record(captured.fresh(session, identifier(2)?));
                assert_eq!(result, Err(error), "fresh {session:?} guard byte {offset}");
                assert_halted(&fixture, prior_status(session));
            }
        }
    }
    Ok(())
}

#[test]
fn fresh_observations_require_all_complete_page_lengths() -> TestResult {
    for session in [Session::Apply, Session::Verify] {
        for length in [0, 255, 257] {
            for role in [PageRole::Target, PageRole::Control, PageRole::Routing] {
                let mut fixture = waiting_for(session)?;
                let invalid = vec![0; length];
                let mut event = fixture.captured.fresh(session, identifier(2)?);
                let TerminalExitTrialEvent::FreshSession {
                    whole_page,
                    control_page,
                    routing_page,
                    ..
                } = &mut event
                else {
                    return Err("fixture must produce a fresh event".into());
                };
                let error = match role {
                    PageRole::Target => {
                        *whole_page = &invalid;
                        TerminalExitTrialError::PageLength { actual: length }
                    }
                    PageRole::Control => {
                        *control_page = &invalid;
                        TerminalExitTrialError::ControlPageLength { actual: length }
                    }
                    PageRole::Routing => {
                        *routing_page = &invalid;
                        TerminalExitTrialError::RoutingPageLength { actual: length }
                    }
                };
                assert_eq!(
                    fixture.trial.record(event),
                    Err(error),
                    "all fresh pages must be complete"
                );
                assert_halted(&fixture, prior_status(session));
            }
        }
    }
    Ok(())
}

#[test]
fn fresh_identity_format_and_gateway_state_are_mandatory_in_both_sessions() -> TestResult {
    for session in [Session::Apply, Session::Verify] {
        let mut fixture = waiting_for(session)?;
        fixture.captured.identity.radio_type = RadioType::new("J,2,1")?;
        assert_eq!(
            fixture
                .trial
                .record(fixture.captured.fresh(session, identifier(2)?)),
            Err(TerminalExitTrialError::IdentityMismatch)
        );
        assert_halted(&fixture, prior_status(session));
        for raw in 1..=u8::MAX {
            let mut fixture = waiting_for(session)?;
            let mut event = fixture.captured.fresh(session, identifier(2)?);
            let TerminalExitTrialEvent::FreshSession { memory_format, .. } = &mut event else {
                return Err("fixture must produce fresh format evidence".into());
            };
            *memory_format = raw;
            assert_eq!(
                fixture.trial.record(event),
                Err(TerminalExitTrialError::MemoryFormat { actual: raw })
            );
            assert_halted(&fixture, prior_status(session));
        }
        for raw in 0..=u8::MAX {
            let expected = match session {
                Session::Apply => DvGatewayMode::Terminal,
                Session::Verify => DvGatewayMode::Off,
            };
            let actual = DvGatewayMode::from(raw);
            if actual == expected {
                continue;
            }
            let mut fixture = waiting_for(session)?;
            let mut event = fixture.captured.fresh(session, identifier(2)?);
            let TerminalExitTrialEvent::FreshSession { gateway_mode, .. } = &mut event else {
                return Err("fixture must produce fresh Gateway evidence".into());
            };
            *gateway_mode = actual;
            assert_eq!(
                fixture.trial.record(event),
                Err(TerminalExitTrialError::GatewayModeMismatch { expected, actual })
            );
            assert_halted(&fixture, prior_status(session));
        }
    }
    Ok(())
}

#[test]
fn active_expectation_does_not_accept_the_off_baseline_as_a_fresh_active_page() -> TestResult {
    let mut fixture = fixture()?;
    fixture.captured.active = fixture.captured.off;
    assert_eq!(
        fixture
            .trial
            .record(fixture.captured.fresh(Session::Apply, identifier(1)?)),
        Err(TerminalExitTrialError::PageMismatch)
    );
    assert_halted(&fixture, TerminalExitTrialStatus::NotWritten);
    assert_eq!(
        fixture
            .trial
            .record(TerminalExitTrialEvent::DurableWriteIntent {
                id: identifier(11)?
            }),
        Err(TerminalExitTrialError::TerminalState)
    );
    Ok(())
}

#[test]
fn completion_requires_one_intent_and_two_finalized_off_verified_sessions() -> TestResult {
    let mut fixture = fixture()?;
    fixture
        .trial
        .record(fixture.captured.fresh(Session::Apply, identifier(1)?))?;
    assert_eq!(fixture.trial.status(), TerminalExitTrialStatus::NotWritten);
    assert_eq!(
        fixture.trial.next_session(),
        Err(TerminalExitTrialError::UnexpectedEvent)
    );
    fixture
        .trial
        .record(TerminalExitTrialEvent::DurableWriteIntent {
            id: identifier(11)?,
        })?;
    assert_eq!(
        fixture.trial.status(),
        TerminalExitTrialStatus::PossiblyChanged
    );
    fixture
        .trial
        .record(TerminalExitTrialEvent::ImmediateReadback {
            whole_page: &fixture.captured.off,
        })?;
    assert_eq!(
        fixture.trial.status(),
        TerminalExitTrialStatus::PossiblyChanged
    );
    fixture
        .trial
        .record(fixture.captured.finalized(identifier(1)?))?;
    assert_eq!(fixture.trial.next_session()?, Session::Verify);
    fixture
        .trial
        .record(fixture.captured.fresh(Session::Verify, identifier(2)?))?;
    assert_eq!(
        fixture.trial.status(),
        TerminalExitTrialStatus::PossiblyChanged,
        "fresh Off pages alone cannot complete the second lifecycle"
    );
    fixture
        .trial
        .record(fixture.captured.finalized(identifier(2)?))?;
    assert_eq!(
        fixture.trial.status(),
        TerminalExitTrialStatus::OffVerifiedAcrossSessions
    );
    fixture.trial.halt();
    assert_eq!(
        fixture.trial.status(),
        TerminalExitTrialStatus::OffVerifiedAcrossSessions
    );
    assert_eq!(
        fixture.trial.next_session(),
        Err(TerminalExitTrialError::TerminalState)
    );
    assert_eq!(
        fixture
            .trial
            .record(fixture.captured.finalized(identifier(2)?)),
        Err(TerminalExitTrialError::TerminalState)
    );
    Ok(())
}

#[test]
fn intent_and_readback_cannot_be_recorded_before_a_full_fresh_match() -> TestResult {
    for intent in [false, true] {
        let mut fixture = fixture()?;
        let event = if intent {
            TerminalExitTrialEvent::DurableWriteIntent {
                id: identifier(11)?,
            }
        } else {
            TerminalExitTrialEvent::ImmediateReadback {
                whole_page: &fixture.captured.off,
            }
        };
        assert_eq!(
            fixture.trial.record(event),
            Err(TerminalExitTrialError::UnexpectedEvent)
        );
        assert_halted(&fixture, TerminalExitTrialStatus::NotWritten);
    }
    let mut fixture = fixture()?;
    fixture
        .trial
        .record(fixture.captured.fresh(Session::Apply, identifier(1)?))?;
    assert_eq!(
        fixture
            .trial
            .record(TerminalExitTrialEvent::ImmediateReadback {
                whole_page: &fixture.captured.off
            }),
        Err(TerminalExitTrialError::UnexpectedEvent)
    );
    assert_halted(&fixture, TerminalExitTrialStatus::NotWritten);
    Ok(())
}

#[test]
fn immediate_readback_requires_every_off_byte_and_exact_length() -> TestResult {
    for offset in 0..PAGE_SIZE {
        let mut fixture = fixture()?;
        fixture
            .trial
            .record(fixture.captured.fresh(Session::Apply, identifier(1)?))?;
        fixture
            .trial
            .record(TerminalExitTrialEvent::DurableWriteIntent {
                id: identifier(11)?,
            })?;
        *fixture
            .captured
            .off
            .get_mut(offset)
            .ok_or("readback byte")? ^= 1;
        assert_eq!(
            fixture
                .trial
                .record(TerminalExitTrialEvent::ImmediateReadback {
                    whole_page: &fixture.captured.off
                }),
            Err(TerminalExitTrialError::PageMismatch),
            "readback byte {offset}"
        );
        assert_halted(&fixture, TerminalExitTrialStatus::PossiblyChanged);
    }
    for length in [0, 255, 257] {
        let mut fixture = fixture()?;
        fixture
            .trial
            .record(fixture.captured.fresh(Session::Apply, identifier(1)?))?;
        fixture
            .trial
            .record(TerminalExitTrialEvent::DurableWriteIntent {
                id: identifier(11)?,
            })?;
        assert_eq!(
            fixture
                .trial
                .record(TerminalExitTrialEvent::ImmediateReadback {
                    whole_page: &vec![0; length]
                }),
            Err(TerminalExitTrialError::PageLength { actual: length })
        );
        assert_halted(&fixture, TerminalExitTrialStatus::PossiblyChanged);
    }
    Ok(())
}

#[test]
fn finalization_requires_matching_session_new_identity_and_gateway_off() -> TestResult {
    for session in [Session::Apply, Session::Verify] {
        for wrong in 0..3 {
            let mut fixture = waiting_for(session)?;
            let id = identifier(match session {
                Session::Apply => 1,
                Session::Verify => 2,
            })?;
            fixture.trial.record(fixture.captured.fresh(session, id))?;
            if session == Session::Apply {
                fixture
                    .trial
                    .record(TerminalExitTrialEvent::DurableWriteIntent {
                        id: identifier(11)?,
                    })?;
                fixture
                    .trial
                    .record(TerminalExitTrialEvent::ImmediateReadback {
                        whole_page: &fixture.captured.off,
                    })?;
            }
            let mut identity = fixture.captured.identity.clone();
            if wrong == 1 {
                identity.firmware = FirmwareIdentity::new("1.03")?;
            }
            let gateway_mode = if wrong == 2 {
                DvGatewayMode::Terminal
            } else {
                DvGatewayMode::Off
            };
            let result = fixture
                .trial
                .record(TerminalExitTrialEvent::SessionFinalized {
                    id: if wrong == 0 { identifier(99)? } else { id },
                    identity: &identity,
                    gateway_mode,
                });
            let error = match wrong {
                0 => TerminalExitTrialError::SessionMismatch,
                1 => TerminalExitTrialError::IdentityMismatch,
                _ => TerminalExitTrialError::GatewayModeMismatch {
                    expected: DvGatewayMode::Off,
                    actual: DvGatewayMode::Terminal,
                },
            };
            assert_eq!(result, Err(error), "{session:?} finalization guard {wrong}");
            assert_halted(&fixture, TerminalExitTrialStatus::PossiblyChanged);
        }
    }
    Ok(())
}

#[test]
fn premature_finalization_and_verification_write_are_permanently_refused() -> TestResult {
    for after_intent in [false, true] {
        let mut fixture = fixture()?;
        fixture
            .trial
            .record(fixture.captured.fresh(Session::Apply, identifier(1)?))?;
        if after_intent {
            fixture
                .trial
                .record(TerminalExitTrialEvent::DurableWriteIntent {
                    id: identifier(11)?,
                })?;
        }
        assert_eq!(
            fixture
                .trial
                .record(fixture.captured.finalized(identifier(1)?)),
            Err(TerminalExitTrialError::UnexpectedEvent)
        );
        assert_halted(
            &fixture,
            if after_intent {
                TerminalExitTrialStatus::PossiblyChanged
            } else {
                TerminalExitTrialStatus::NotWritten
            },
        );
    }
    let mut fixture = waiting_for(Session::Verify)?;
    fixture
        .trial
        .record(fixture.captured.fresh(Session::Verify, identifier(2)?))?;
    assert_eq!(
        fixture
            .trial
            .record(TerminalExitTrialEvent::DurableWriteIntent {
                id: identifier(12)?
            }),
        Err(TerminalExitTrialError::UnexpectedEvent)
    );
    assert_halted(&fixture, TerminalExitTrialStatus::PossiblyChanged);
    Ok(())
}

#[test]
fn duplicate_session_and_intent_identifiers_are_refused() -> TestResult {
    let mut fixture = waiting_for(Session::Verify)?;
    assert_eq!(
        fixture
            .trial
            .record(fixture.captured.fresh(Session::Verify, identifier(1)?)),
        Err(TerminalExitTrialError::ReusedSession)
    );
    assert_halted(&fixture, TerminalExitTrialStatus::PossiblyChanged);
    let mut fixture = waiting_for(Session::Apply)?;
    fixture
        .trial
        .record(fixture.captured.fresh(Session::Apply, identifier(1)?))?;
    fixture
        .trial
        .record(TerminalExitTrialEvent::DurableWriteIntent {
            id: identifier(11)?,
        })?;
    assert_eq!(
        fixture
            .trial
            .record(TerminalExitTrialEvent::DurableWriteIntent {
                id: identifier(11)?
            }),
        Err(TerminalExitTrialError::ReusedWriteIntent)
    );
    assert_halted(&fixture, TerminalExitTrialStatus::PossiblyChanged);
    Ok(())
}

#[test]
fn explicit_halt_preserves_conservative_status_and_never_restarts() -> TestResult {
    for after_intent in [false, true] {
        let mut fixture = fixture()?;
        if after_intent {
            fixture
                .trial
                .record(fixture.captured.fresh(Session::Apply, identifier(1)?))?;
            fixture
                .trial
                .record(TerminalExitTrialEvent::DurableWriteIntent {
                    id: identifier(11)?,
                })?;
        }
        fixture.trial.halt();
        let status = if after_intent {
            TerminalExitTrialStatus::PossiblyChanged
        } else {
            TerminalExitTrialStatus::NotWritten
        };
        assert_halted(&fixture, status);
        assert_eq!(
            fixture
                .trial
                .record(fixture.captured.fresh(Session::Apply, identifier(2)?)),
            Err(TerminalExitTrialError::TerminalState)
        );
        assert_halted(&fixture, status);
    }
    Ok(())
}
