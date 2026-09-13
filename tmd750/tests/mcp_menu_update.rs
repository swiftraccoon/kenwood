//! Public format-checked menu updates on simulated firmware 1.02.

use kenwood_schema as _;
use mcp_d75_extract as _;
use thiserror as _;
use tokio_serial as _;
use tracing as _;

use kenwood_tmd750::protocol::mcp::{ACK, read_request, regions, write_request};
use kenwood_tmd750::{
    Error, FirmwareIdentity, Identity, MenuAssignment, MenuFieldSnapshot, MenuUpdatePlan, Page,
    PageReplacement, Radio, RadioModel, RadioType, Region, SlotIndex,
};
use kenwood_transport::MockTransport;

type TestError = Box<dyn std::error::Error>;
type TestResult = Result<(), TestError>;

fn identity(firmware: &str) -> Result<Identity, TestError> {
    Ok(Identity {
        model: RadioModel::TmD750,
        firmware: FirmwareIdentity::new(firmware)?,
        radio_type: RadioType::new("K,2,1")?,
    })
}

fn plan() -> Result<MenuUpdatePlan, TestError> {
    let snapshot = MenuFieldSnapshot::from_pages(
        regions::menu_regions()
            .into_iter()
            .flat_map(Region::pages)
            .map(|page| (page, vec![0; page.len()]))
            .collect(),
    )?;
    Ok(MenuUpdatePlan::new(
        &identity("1.02")?,
        &snapshot,
        vec![
            MenuAssignment::new("pm.PmName2", None, "BASE")?,
            MenuAssignment::new(
                "dv.MyCallsignDvGatewayList[0].MyCallsignDvGateway",
                Some(SlotIndex::new(0)?),
                "KQ4NIT",
            )?,
            MenuAssignment::new("radio.TxEqualizerFmNfm", Some(SlotIndex::new(5)?), "on")?,
        ],
    )?)
}

fn entry(firmware: &str) -> MockTransport {
    let mut mock = MockTransport::new();
    mock.expect(b"ID\r", b"ID TM-D750\r");
    mock.expect(b"FV\r", format!("FV {firmware}\r").as_bytes());
    mock.expect(b"TY\r", b"TY K,2,1\r");
    mock.expect(b"0M PROGRAM\r", b"0M\r");
    mock
}

fn read(mock: &mut MockTransport, page: Page, bytes: &[u8]) {
    let mut reply = write_request(page).to_vec();
    reply.extend_from_slice(bytes);
    mock.expect(&read_request(page), &reply);
    mock.expect(&[ACK], &[ACK]);
}

fn write(mock: &mut MockTransport, replacement: &PageReplacement) {
    let mut frame = write_request(replacement.page()).to_vec();
    frame.extend_from_slice(replacement.replacement());
    mock.expect(&frame, &[ACK]);
    read(mock, replacement.page(), replacement.replacement());
}

#[tokio::test]
async fn registered_batch_on_102_compares_guards_then_writes_complete_pages() -> TestResult {
    let plan = plan()?;
    let mut mock = entry("1.02");
    for replacement in plan.replacements() {
        read(&mut mock, replacement.page(), replacement.expected());
    }
    for replacement in plan.replacements().iter().filter(|item| !item.is_noop()) {
        write(&mut mock, replacement);
    }
    mock.expect(b"E", &[ACK]);
    let mut radio = Radio::new(mock);
    let mut session = radio.enter_mcp().await?;
    let mut intents = Vec::new();
    let report = session
        .compare_exchange_menu_update(
            &plan,
            |replacement| {
                intents.push(replacement.clone());
                Ok(())
            },
            |_| {},
        )
        .await?;
    assert_eq!(
        report.compared_pages,
        plan.replacements()
            .iter()
            .map(PageReplacement::page)
            .collect::<Vec<_>>(),
        "every control and target page must match before any write"
    );
    assert_eq!(
        intents,
        plan.replacements()
            .iter()
            .filter(|item| !item.is_noop())
            .cloned()
            .collect::<Vec<_>>(),
        "each changed page must have one exact complete durable intent"
    );
    assert_eq!(
        report.verified_pages.len(),
        3,
        "global text, PM-Off callsign, and PM5 bit share one general batch engine"
    );
    assert_eq!(
        session.journal().verified,
        report.verified_pages,
        "all acknowledged pages require complete immediate readback"
    );
    session.exit().await?;
    radio.into_transport().assert_complete();
    Ok(())
}

#[tokio::test]
async fn new_menu_admission_does_not_enable_legacy_raw_page_writes() -> TestResult {
    let plan = plan()?;
    let mut mock = entry("1.02");
    mock.expect(b"E", &[ACK]);
    let mut radio = Radio::new(mock);
    let mut session = radio.enter_mcp().await?;
    let result = session
        .compare_exchange_pages(plan.replacements(), |_| Ok(()), |_| {})
        .await;
    assert!(
        matches!(result, Err(Error::UnsupportedSchemaTarget { .. })),
        "the evidence-bearing menu API must not widen legacy arbitrary-page admission"
    );
    assert!(
        session.journal().possibly_written.is_empty(),
        "refusal must precede dispatch"
    );
    session.exit().await?;
    radio.into_transport().assert_complete();
    Ok(())
}

#[tokio::test]
async fn session_identity_disagreement_refuses_menu_pages_before_any_traffic() -> TestResult {
    let plan = plan()?;
    let mut mock = entry("1.00");
    mock.expect(b"E", &[ACK]);
    let mut radio = Radio::new(mock);
    let mut session = radio.enter_mcp().await?;
    assert!(
        session
            .compare_exchange_menu_update(&plan, |_| Ok(()), |_| {})
            .await
            .is_err(),
        "a source plan cannot be applied to a different exact firmware identity"
    );
    assert!(
        session.journal().possibly_written.is_empty(),
        "identity refusal must not imply a write"
    );
    session.exit().await?;
    radio.into_transport().assert_complete();
    Ok(())
}

#[tokio::test]
async fn changed_format_pm_or_gateway_guard_prevents_every_menu_write() -> TestResult {
    let plan = plan()?;
    for (address, offset) in [(8, 2), (323_584, 9), (331_776, 0)] {
        let mut mock = entry("1.02");
        let mut guarded = false;
        for replacement in plan.replacements() {
            let mut actual = replacement.expected().to_vec();
            if replacement.page().address().as_u32() == address {
                *actual.get_mut(offset).ok_or("guard offset must exist")? = 1;
                guarded = true;
            }
            read(&mut mock, replacement.page(), &actual);
            if guarded {
                break;
            }
        }
        assert!(guarded, "plan must retain the complete guard at {address}");
        mock.expect(b"E", &[ACK]);
        let mut radio = Radio::new(mock);
        let mut session = radio.enter_mcp().await?;
        let mut intents = 0;
        let result = session
            .compare_exchange_menu_update(
                &plan,
                |_| {
                    intents += 1;
                    Ok(())
                },
                |_| {},
            )
            .await;
        assert!(
            result.is_err(),
            "changed guard {address} must refuse the batch"
        );
        assert_eq!(
            intents, 0,
            "a guard mismatch must precede every durable write intent"
        );
        assert!(
            session.journal().possibly_written.is_empty(),
            "no earlier page may escape global preflight"
        );
        session.exit().await?;
        radio.into_transport().assert_complete();
    }
    Ok(())
}
