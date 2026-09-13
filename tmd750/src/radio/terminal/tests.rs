//! Exact scope, reversible images, and shared MCP boundary regressions.

use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use kenwood_transport::{MockTransport, Transport, TransportError};

use super::*;
use crate::error::McpError;
use crate::protocol::mcp::{ACK, read_request, write_request};
use crate::radio::{Progress, Radio};
use crate::types::{FirmwareIdentity, RadioType};

type TestError = Box<dyn std::error::Error>;
type TestResult = Result<(), TestError>;

fn identity() -> Result<Identity, TestError> {
    Ok(Identity {
        model: RadioModel::TmD750,
        firmware: FirmwareIdentity::new("1.02")?,
        radio_type: RadioType::new("K,2,1")?,
    })
}

fn fixture(slot: u8, route: u8, before: u8, subtype: u8) -> Result<MenuFieldSnapshot, TestError> {
    let offset = SlotIndex::new(slot)?.offset();
    let mut pages = vec![
        (Page::new(Address::new(8)?, 40)?, vec![0xA5; 40]),
        (Page::new(Address::new(323_584)?, 256)?, vec![0xA5; 256]),
        (
            Page::new(Address::new(328_960 + offset)?, 256)?,
            vec![0xA5; 256],
        ),
        (
            Page::new(Address::new(331_776 + offset)?, 256)?,
            vec![0xA5; 256],
        ),
    ];
    for (address, value) in [
        (10, 0),
        (323_593, slot),
        (329_031 + offset, 0),
        (329_037 + offset, route),
        (331_776 + offset, before),
        (331_778 + offset, subtype),
    ] {
        change(&mut pages, address, value)?;
    }
    Ok(MenuFieldSnapshot::from_pages(pages)?)
}

fn change(pages: &mut [(Page, Vec<u8>)], address: u32, value: u8) -> TestResult {
    let (page, bytes) = pages
        .iter_mut()
        .find(|(page, _)| (page.address().as_u32()..page.end()).contains(&address))
        .ok_or("fixture change must fall within one complete page")?;
    let offset = usize::try_from(address - page.address().as_u32())?;
    *bytes.get_mut(offset).ok_or("fixture byte missing")? = value;
    Ok(())
}

fn entry_plan() -> Result<TerminalPlan, TestError> {
    Ok(TerminalPlan::new(
        &identity()?,
        &fixture(0, 1, 0, 1)?,
        TerminalTarget::ReflectorTerminal,
    )?)
}

#[test]
fn every_slot_and_route_preserve_full_guards_and_change_only_gateway_and_subtype() -> TestResult {
    for slot in 0..6 {
        for (raw_route, route) in [
            (0, TerminalGatewayRoute::MainUnit),
            (1, TerminalGatewayRoute::ControlPanel),
            (2, TerminalGatewayRoute::Bluetooth),
        ] {
            let snapshot = fixture(slot, raw_route, 0, 1)?;
            let original = snapshot.clone();
            let plan =
                TerminalPlan::new(&identity()?, &snapshot, TerminalTarget::ReflectorTerminal)?;
            assert_eq!(plan.identity(), &identity()?);
            assert_eq!(plan.slot(), SlotIndex::new(slot)?);
            assert_eq!(plan.route(), route);
            assert_eq!(plan.before(), DvGatewayMode::Off);
            assert_eq!(plan.target(), TerminalTarget::ReflectorTerminal);
            assert_eq!(plan.replacements().len(), 4);
            assert_eq!(
                snapshot, original,
                "planning must not mutate captured pages"
            );
            let mut changed = Vec::new();
            for replacement in plan.replacements() {
                assert_eq!(
                    snapshot.page(replacement.page()),
                    Some(replacement.expected())
                );
                assert_eq!(replacement.expected().len(), replacement.page().len());
                assert_eq!(replacement.replacement().len(), replacement.page().len());
                for (offset, (before, after)) in replacement
                    .expected()
                    .iter()
                    .zip(replacement.replacement())
                    .enumerate()
                {
                    if before != after {
                        changed.push((
                            replacement.page().address().as_u32() + u32::try_from(offset)?,
                            *before,
                            *after,
                        ));
                    }
                }
            }
            let base = 331_776 + SlotIndex::new(slot)?.offset();
            assert_eq!(changed, [(base, 0, 2), (base + 2, 1, 0)]);
        }
    }
    Ok(())
}

fn changed_bytes(plan: &TerminalPlan) -> Result<Vec<(u32, u8, u8)>, TestError> {
    let mut changed = Vec::new();
    for replacement in plan.replacements() {
        for (offset, (before, after)) in replacement
            .expected()
            .iter()
            .zip(replacement.replacement())
            .enumerate()
        {
            if before != after {
                changed.push((
                    replacement.page().address().as_u32() + u32::try_from(offset)?,
                    *before,
                    *after,
                ));
            }
        }
    }
    Ok(changed)
}

#[test]
fn selected_route_entry_changes_only_route_gateway_and_subtype_in_every_pm() -> TestResult {
    let routes = [
        (0, TerminalGatewayRoute::MainUnit),
        (1, TerminalGatewayRoute::ControlPanel),
        (2, TerminalGatewayRoute::Bluetooth),
    ];
    for slot in 0..6 {
        let offset = SlotIndex::new(slot)?.offset();
        for (before_raw, before) in routes {
            for (target_raw, target) in routes {
                let snapshot = fixture(slot, before_raw, 0, 1)?;
                let original = snapshot.clone();
                let plan = TerminalPlan::for_route(&identity()?, &snapshot, target)?;
                assert_eq!(plan.route(), before);
                assert_eq!(plan.target_route(), target);
                assert_eq!(plan.replacements().len(), 4);
                assert_eq!(snapshot, original, "planning cannot alter its source");
                for page in plan.replacements() {
                    assert_eq!(snapshot.page(page.page()), Some(page.expected()));
                    assert_eq!(page.expected().len(), page.page().len());
                    assert_eq!(page.replacement().len(), page.page().len());
                }
                let mut expected = Vec::new();
                if before_raw != target_raw {
                    expected.push((329_037 + offset, before_raw, target_raw));
                }
                expected.extend([(331_776 + offset, 0, 2), (331_778 + offset, 1, 0)]);
                assert_eq!(changed_bytes(&plan)?, expected);
            }
        }
    }
    Ok(())
}

#[test]
fn selected_route_exact_restoration_roundtrips_all_modes_subtypes_and_pms() -> TestResult {
    for slot in 0..6 {
        for before_route in 0..3 {
            for destination in [
                TerminalGatewayRoute::MainUnit,
                TerminalGatewayRoute::ControlPanel,
                TerminalGatewayRoute::Bluetooth,
            ] {
                for (gateway, subtype) in [(0, 0), (0, 1), (2, 0)] {
                    let snapshot = fixture(slot, before_route, gateway, subtype)?;
                    let forward = TerminalPlan::for_route(&identity()?, &snapshot, destination)?;
                    let reverse = forward.restoration()?;
                    assert_eq!(reverse.route(), forward.target_route());
                    assert_eq!(reverse.target_route(), forward.route());
                    assert_eq!(reverse.slot(), forward.slot());
                    assert_eq!(reverse.replacements().len(), forward.replacements().len());
                    for (original, restored) in
                        forward.replacements().iter().zip(reverse.replacements())
                    {
                        assert_eq!(original.page(), restored.page());
                        assert_eq!(original.expected(), restored.replacement());
                        assert_eq!(original.replacement(), restored.expected());
                    }
                    assert_eq!(reverse.restoration()?, forward);
                }
            }
        }
    }
    Ok(())
}

#[test]
fn selected_satisfied_route_keeps_all_guards_without_writes() -> TestResult {
    for slot in 0..6 {
        for (raw, route) in [
            (0, TerminalGatewayRoute::MainUnit),
            (1, TerminalGatewayRoute::ControlPanel),
            (2, TerminalGatewayRoute::Bluetooth),
        ] {
            let plan = TerminalPlan::for_route(&identity()?, &fixture(slot, raw, 2, 0)?, route)?;
            assert_eq!(plan.replacements().len(), 4);
            assert!(plan.replacements().iter().all(PageReplacement::is_noop));
            assert_eq!(plan.restoration()?, plan);
        }
    }
    Ok(())
}

#[test]
fn selected_route_never_replaces_an_unknown_captured_route_with_a_default() -> TestResult {
    for slot in 0..6 {
        for raw in [3, 255] {
            let result = TerminalPlan::for_route(
                &identity()?,
                &fixture(slot, raw, 0, 0)?,
                TerminalGatewayRoute::Bluetooth,
            );
            assert!(
                matches!(result, Err(TerminalPlanError::GuardValue { field: ROUTE_FIELD, actual }) if actual == u64::from(raw))
            );
        }
    }
    Ok(())
}

#[test]
fn required_capture_pages_are_exact_canonical_unique_and_cover_every_pm() -> TestResult {
    let pages = TerminalPlan::required_pages()?;
    let observed: Vec<_> = pages
        .iter()
        .map(|page| (page.address().as_u32(), page.len()))
        .collect();
    let mut expected = vec![(8, 40), (323_584, 256)];
    for slot in 0..6 {
        let offset = SlotIndex::new(slot)?.offset();
        expected.extend([(328_960 + offset, 256), (331_776 + offset, 256)]);
        let plan = TerminalPlan::for_route(
            &identity()?,
            &fixture(slot, 0, 0, 1)?,
            TerminalGatewayRoute::Bluetooth,
        )?;
        assert!(
            plan.replacements()
                .iter()
                .all(|page| pages.contains(&page.page()))
        );
    }
    assert_eq!(observed, expected);
    let captured = pages
        .into_iter()
        .map(|page| (page, vec![0; page.len()]))
        .collect();
    let _canonical = MenuFieldSnapshot::from_pages(captured)?;
    Ok(())
}

#[test]
fn direct_off_preserves_subtype_and_satisfied_targets_keep_all_noop_guards() -> TestResult {
    for (before, subtype, target) in [
        (0, 0, TerminalTarget::Off),
        (0, 1, TerminalTarget::Off),
        (2, 0, TerminalTarget::ReflectorTerminal),
    ] {
        let plan = TerminalPlan::new(&identity()?, &fixture(0, 1, before, subtype)?, target)?;
        assert_eq!(plan.replacements().len(), 4);
        assert!(plan.replacements().iter().all(PageReplacement::is_noop));
    }
    let plan = TerminalPlan::new(&identity()?, &fixture(5, 2, 2, 0)?, TerminalTarget::Off)?;
    let changed = plan
        .replacements()
        .iter()
        .filter(|page| !page.is_noop())
        .collect::<Vec<_>>();
    assert_eq!(changed.len(), 1);
    let page = changed.first().ok_or("one changed Gateway page required")?;
    assert_eq!(page.expected().first(), Some(&2));
    assert_eq!(page.replacement().first(), Some(&0));
    assert_eq!(page.expected().get(1..), page.replacement().get(1..));
    Ok(())
}

#[test]
fn exact_reverse_restores_original_off_subtype_and_inverts_every_full_image() -> TestResult {
    let forward = entry_plan()?;
    let reverse = forward.restoration()?;
    assert_eq!(reverse.identity(), forward.identity());
    assert_eq!(reverse.slot(), forward.slot());
    assert_eq!(reverse.route(), forward.route());
    assert_eq!(reverse.before(), DvGatewayMode::Terminal);
    assert_eq!(reverse.target(), TerminalTarget::Off);
    for (original, restoration) in forward.replacements().iter().zip(reverse.replacements()) {
        assert_eq!(original.page(), restoration.page());
        assert_eq!(original.expected(), restoration.replacement());
        assert_eq!(original.replacement(), restoration.expected());
    }
    assert_eq!(
        reverse.restoration()?,
        forward,
        "reversing twice retains exact bytes and intent"
    );
    Ok(())
}

#[test]
fn restoration_revalidates_complete_unique_expected_guards() -> TestResult {
    let original = entry_plan()?;
    for omitted in 0..original.replacements().len() {
        let mut incomplete = original.clone();
        let removed = incomplete.replacements.remove(omitted);
        assert!(
            matches!(incomplete.restoration(), Err(TerminalPlanError::Schema(SchemaError::SnapshotPageMissing { address, .. })) if address == removed.page().address().as_u32())
        );
    }
    let mut duplicate = original.clone();
    let repeated = original
        .replacements()
        .first()
        .ok_or("format guard required")?;
    duplicate.replacements.push(repeated.clone());
    assert!(matches!(
        duplicate.restoration(),
        Err(TerminalPlanError::Schema(
            SchemaError::DuplicateSnapshotPage { address: 8 }
        ))
    ));
    Ok(())
}

#[test]
fn identities_domains_and_all_missing_guard_pages_are_typed_refusals() -> TestResult {
    let snapshot = fixture(0, 1, 0, 1)?;
    for (firmware, radio_type) in [("1.00", "K,2,1"), ("1.03", "K,2,1"), ("1.02", "J,2,1")] {
        let mut supplied = identity()?;
        supplied.firmware = FirmwareIdentity::new(firmware)?;
        supplied.radio_type = RadioType::new(radio_type)?;
        assert!(
            matches!(TerminalPlan::new(&supplied, &snapshot, TerminalTarget::Off), Err(TerminalPlanError::UnsupportedIdentity { actual }) if actual == supplied)
        );
    }
    for (address, values, name) in [
        (323_593, &[6, 255][..], PM_FIELD),
        (329_031, &[1, 255][..], USB_FIELD),
        (329_037, &[3, 255][..], ROUTE_FIELD),
        (331_776, &[1, 3, 255][..], GATEWAY_FIELD),
        (331_778, &[2, 255][..], SUBTYPE_FIELD),
    ] {
        for &value in values {
            let mut pages = snapshot.pages().to_vec();
            change(&mut pages, address, value)?;
            let altered = MenuFieldSnapshot::from_pages(pages)?;
            assert!(
                matches!(TerminalPlan::new(&identity()?, &altered, TerminalTarget::Off), Err(TerminalPlanError::GuardValue { field, actual }) if field == name && actual == u64::from(value))
            );
        }
    }
    assert!(matches!(
        TerminalPlan::new(&identity()?, &fixture(0, 1, 2, 1)?, TerminalTarget::Off),
        Err(TerminalPlanError::GuardValue {
            field: SUBTYPE_FIELD,
            actual: 1
        })
    ));
    let mut pages = snapshot.pages().to_vec();
    change(&mut pages, 10, 1)?;
    assert!(matches!(
        TerminalPlan::new(
            &identity()?,
            &MenuFieldSnapshot::from_pages(pages)?,
            TerminalTarget::Off
        ),
        Err(TerminalPlanError::MemoryFormat { actual: 1 })
    ));
    for omitted in 0..snapshot.pages().len() {
        let mut pages = snapshot.pages().to_vec();
        let (page, _) = pages.remove(omitted);
        let incomplete = MenuFieldSnapshot::from_pages(pages)?;
        assert!(
            matches!(TerminalPlan::new(&identity()?, &incomplete, TerminalTarget::Off), Err(TerminalPlanError::Schema(SchemaError::SnapshotPageMissing { address, .. })) if address == page.address().as_u32())
        );
    }
    Ok(())
}

fn ready(firmware: &str) -> MockTransport {
    let mut mock = MockTransport::new();
    mock.expect(b"ID\r", b"ID TM-D750\r");
    mock.expect(b"FV\r", format!("FV {firmware}\r").as_bytes());
    mock.expect(b"TY\r", b"TY K,2,1\r");
    mock.expect(b"0M PROGRAM\r", b"0M\r");
    mock
}

fn frame(page: Page, bytes: &[u8]) -> Vec<u8> {
    let mut frame = write_request(page).to_vec();
    frame.extend_from_slice(bytes);
    frame
}

fn read(mock: &mut MockTransport, page: Page, bytes: &[u8]) {
    mock.expect(&read_request(page), &frame(page, bytes));
    mock.expect(&[ACK], &[ACK]);
}

fn preflight(mock: &mut MockTransport, plan: &TerminalPlan) {
    for replacement in plan.replacements() {
        read(mock, replacement.page(), replacement.expected());
    }
}

struct IntentTransport {
    mock: MockTransport,
    intents: Arc<AtomicUsize>,
    writes: usize,
}

impl Transport for IntentTransport {
    async fn write(&mut self, bytes: &[u8]) -> Result<(), TransportError> {
        if bytes.first() == Some(&b'W') {
            self.writes += 1;
            assert_eq!(
                self.intents.load(Ordering::Relaxed),
                self.writes,
                "durable intent must precede each W"
            );
        }
        self.mock.write(bytes).await
    }
    async fn read(&mut self, bytes: &mut [u8]) -> Result<usize, TransportError> {
        self.mock.read(bytes).await
    }
    async fn close(&mut self) -> Result<(), TransportError> {
        self.mock.close().await
    }
}

#[tokio::test]
async fn both_directions_use_all_guard_comparisons_intent_readback_and_retired_exit() -> TestResult
{
    let selected = TerminalPlan::for_route(
        &identity()?,
        &fixture(0, 1, 0, 1)?,
        TerminalGatewayRoute::Bluetooth,
    )?;
    exercise_exchange_directions(&entry_plan()?, 1).await?;
    exercise_exchange_directions(&selected, 2).await
}

async fn exercise_exchange_directions(forward: &TerminalPlan, changed_pages: usize) -> TestResult {
    for plan in [forward, &forward.restoration()?] {
        let mut mock = ready("1.02");
        preflight(&mut mock, plan);
        for replacement in plan.replacements().iter().filter(|page| !page.is_noop()) {
            mock.expect(
                &frame(replacement.page(), replacement.replacement()),
                &[ACK],
            );
            read(&mut mock, replacement.page(), replacement.replacement());
        }
        mock.expect(b"E", &[ACK]);
        let intents = Arc::new(AtomicUsize::new(0));
        let mut radio = Radio::new(IntentTransport {
            mock,
            intents: Arc::clone(&intents),
            writes: 0,
        });
        let mut session = radio.enter_mcp().await?;
        let mut progress = Vec::new();
        let report = session
            .compare_exchange_terminal(
                plan,
                |_| {
                    let _previous = intents.fetch_add(1, Ordering::Relaxed);
                    Ok(())
                },
                |value| progress.push(value),
            )
            .await?;
        assert_eq!(
            report.compared_pages,
            plan.replacements()
                .iter()
                .map(PageReplacement::page)
                .collect::<Vec<_>>()
        );
        assert_eq!(report.verified_pages.len(), changed_pages);
        assert_eq!(report.unchanged_pages.len(), 4 - changed_pages);
        assert_eq!(
            progress,
            (1..=4)
                .map(|done| Progress { done, total: 4 })
                .collect::<Vec<_>>()
        );
        assert_eq!(session.journal().possibly_written, report.verified_pages);
        assert_eq!(session.journal().verified, report.verified_pages);
        session.exit().await?;
        assert!(matches!(
            radio.identify().await,
            Err(Error::Mcp(McpError::ConnectionRetired))
        ));
        radio.into_transport().mock.assert_complete();
    }
    Ok(())
}

#[tokio::test]
async fn late_guard_conflict_prevents_writes_including_exact_restoration() -> TestResult {
    let forward = entry_plan()?;
    for plan in [&forward, &forward.restoration()?] {
        let mut mock = ready("1.02");
        for (index, replacement) in plan.replacements().iter().enumerate() {
            let mut bytes = replacement.expected().to_vec();
            if index == 3 {
                *bytes.last_mut().ok_or("complete target page required")? ^= 1;
            }
            read(&mut mock, replacement.page(), &bytes);
        }
        mock.expect(b"E", &[ACK]);
        let mut radio = Radio::new(mock);
        let mut session = radio.enter_mcp().await?;
        let mut callbacks = 0;
        let result = session
            .compare_exchange_terminal(
                plan,
                |_| {
                    callbacks += 1;
                    Ok(())
                },
                |_| {},
            )
            .await;
        assert!(
            matches!(result, Err(TerminalPlanError::Operation(Error::Mcp(McpError::Interrupted { source, .. }))) if matches!(*source, Error::Mcp(McpError::CompareMismatch { offset: 255, .. })))
        );
        assert_eq!(callbacks, 0);
        assert!(session.journal().possibly_written.is_empty());
        assert!(session.is_ready());
        session.exit().await?;
        radio.into_transport().assert_complete();
    }
    Ok(())
}

#[tokio::test]
async fn identity_and_legacy_gate_fail_before_any_page_traffic() -> TestResult {
    let plan = entry_plan()?;
    for firmware in ["1.00", "1.03"] {
        let mut mock = ready(firmware);
        mock.expect(b"E", &[ACK]);
        let mut radio = Radio::new(mock);
        let mut session = radio.enter_mcp().await?;
        assert!(matches!(
            session
                .compare_exchange_terminal(&plan, |_| Ok(()), |_| {})
                .await,
            Err(TerminalPlanError::IdentityMismatch { .. })
        ));
        session.exit().await?;
        radio.into_transport().assert_complete();
    }
    let mut mock = ready("1.02");
    mock.expect(b"E", &[ACK]);
    let mut radio = Radio::new(mock);
    let mut session = radio.enter_mcp().await?;
    assert!(matches!(
        session
            .compare_exchange_pages(plan.replacements(), |_| Ok(()), |_| {})
            .await,
        Err(Error::UnsupportedSchemaTarget { .. })
    ));
    session.exit().await?;
    radio.into_transport().assert_complete();
    Ok(())
}

#[tokio::test]
async fn failed_durable_callback_sends_no_write_and_preserves_ready_exit() -> TestResult {
    let plan = entry_plan()?;
    let mut mock = ready("1.02");
    preflight(&mut mock, &plan);
    mock.expect(b"E", &[ACK]);
    let mut radio = Radio::new(mock);
    let mut session = radio.enter_mcp().await?;
    let result = session
        .compare_exchange_terminal(
            &plan,
            |_| Err(io::Error::other("intent sync failed")),
            |_| {},
        )
        .await;
    assert!(
        matches!(result, Err(TerminalPlanError::Operation(Error::Mcp(McpError::Interrupted { source, .. }))) if matches!(*source, Error::Mcp(McpError::DurableIntent { .. })))
    );
    assert!(session.journal().possibly_written.is_empty());
    assert!(session.is_ready());
    session.exit().await?;
    radio.into_transport().assert_complete();
    Ok(())
}

#[tokio::test]
async fn satisfied_terminal_plan_compares_all_guards_without_intent_or_writes() -> TestResult {
    let plan = TerminalPlan::new(
        &identity()?,
        &fixture(4, 2, 2, 0)?,
        TerminalTarget::ReflectorTerminal,
    )?;
    let mut mock = ready("1.02");
    preflight(&mut mock, &plan);
    mock.expect(b"E", &[ACK]);
    let mut radio = Radio::new(mock);
    let mut session = radio.enter_mcp().await?;
    let mut callbacks = 0;
    let report = session
        .compare_exchange_terminal(
            &plan,
            |_| {
                callbacks += 1;
                Ok(())
            },
            |_| {},
        )
        .await?;
    assert_eq!(callbacks, 0);
    assert_eq!(report.compared_pages.len(), 4);
    assert_eq!(report.unchanged_pages, report.compared_pages);
    assert!(report.verified_pages.is_empty());
    assert!(session.journal().possibly_written.is_empty());
    assert!(session.journal().verified.is_empty());
    session.exit().await?;
    radio.into_transport().assert_complete();
    Ok(())
}

#[tokio::test]
async fn missing_write_ack_retains_debt_and_forbids_readback_exit_and_retry() -> TestResult {
    let plan = entry_plan()?;
    let changed = plan
        .replacements()
        .iter()
        .find(|page| !page.is_noop())
        .ok_or("one changed page required")?;
    let write = frame(changed.page(), changed.replacement());
    let mut mock = ready("1.02");
    preflight(&mut mock, &plan);
    mock.expect_hang(&write);
    let mut radio = Radio::new(mock);
    radio.set_timeout(Duration::from_millis(1));
    let mut session = radio.enter_mcp().await?;
    let result = session
        .compare_exchange_terminal(&plan, |_| Ok(()), |_| {})
        .await;
    assert!(matches!(
        result,
        Err(TerminalPlanError::Operation(Error::Mcp(
            McpError::Interrupted {
                possibly_written: 1,
                verified: 0,
                ..
            }
        )))
    ));
    assert_eq!(session.journal().possibly_written, [changed.page()]);
    assert!(session.journal().verified.is_empty());
    assert!(!session.is_ready());
    assert!(matches!(
        session
            .compare_exchange_terminal(&plan, |_| Ok(()), |_| {})
            .await,
        Err(TerminalPlanError::Operation(Error::Mcp(
            McpError::RecoveryRequired
        )))
    ));
    assert!(matches!(
        session.exit().await,
        Err(Error::Mcp(McpError::RecoveryRequired))
    ));
    let mock = radio.into_transport();
    assert_eq!(mock.writes().last(), Some(&write));
    mock.assert_complete();
    Ok(())
}

#[tokio::test]
async fn complete_readback_mismatch_retains_debt_but_permits_clean_exit() -> TestResult {
    let plan = entry_plan()?;
    let changed = plan
        .replacements()
        .iter()
        .find(|page| !page.is_noop())
        .ok_or("one changed page required")?;
    let mut actual = changed.replacement().to_vec();
    *actual.last_mut().ok_or("complete destination required")? ^= 1;
    let mut mock = ready("1.02");
    preflight(&mut mock, &plan);
    mock.expect(&frame(changed.page(), changed.replacement()), &[ACK]);
    read(&mut mock, changed.page(), &actual);
    mock.expect(b"E", &[ACK]);
    let mut radio = Radio::new(mock);
    let mut session = radio.enter_mcp().await?;
    let result = session
        .compare_exchange_terminal(&plan, |_| Ok(()), |_| {})
        .await;
    assert!(
        matches!(result, Err(TerminalPlanError::Operation(Error::Mcp(McpError::Interrupted { possibly_written: 1, verified: 0, source, .. }))) if matches!(*source, Error::Mcp(McpError::VerifyMismatch { offset: 255, .. })))
    );
    assert_eq!(session.journal().possibly_written, [changed.page()]);
    assert!(session.journal().verified.is_empty());
    assert!(session.is_ready());
    session.exit().await?;
    radio.into_transport().assert_complete();
    Ok(())
}
