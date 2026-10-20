//! Identify, read the slot menus, decode a PM name, patch it, compare and
//! exchange the page, exit: the whole slice over the mock transport.

use mcp_d75_extract as _;
use mmdvm as _;
use proptest as _;
use thiserror as _;
use tokio_serial as _;
use tracing as _;

use kenwood_schema::{DecodedFieldValue, FieldValue};
use kenwood_tmd750::memory::{MemoryImage, PatchPlanner, menu_field};
use kenwood_tmd750::protocol::mcp::regions::{GLOBAL_SETTINGS, slot_menu};
use kenwood_tmd750::protocol::mcp::{ACK, ENTER, EXIT, Header};
use kenwood_tmd750::radio::Radio;
use kenwood_tmd750::{Page, PageReplacement, Region, SlotIndex};
use kenwood_transport::MockTransport;

type TestError = Box<dyn std::error::Error>;
type TestResult = Result<(), TestError>;

fn script_read(mock: &mut MockTransport, page: Page, image: &[u8]) -> TestResult {
    let start = page.address().as_usize();
    let data = image
        .get(start..start + page.len())
        .ok_or("page outside image")?;
    let mut reply = Header::write_request(page).to_vec();
    reply.extend_from_slice(data);
    mock.expect(&Header::read_request(page), &reply);
    mock.expect(&[ACK], &[ACK]);
    Ok(())
}

#[tokio::test]
async fn patch_a_pm_name_end_to_end() -> TestResult {
    let pm_name = menu_field("pm.PmName1").ok_or("pm.PmName1 missing")?;
    let meter = menu_field("radio.MeterType").ok_or("radio.MeterType missing")?;
    let slot = SlotIndex::new(1)?;
    let mut radio_image = MemoryImage::blank();
    radio_image.set(&pm_name.descriptor, None, FieldValue::Text("HOME"))?;
    radio_image.set(&meter.descriptor, Some(slot), FieldValue::Unsigned(1))?;

    let mut regions: Vec<Region> = GLOBAL_SETTINGS.to_vec();
    regions.extend(slot_menu(slot));
    let mut mock = MockTransport::new();
    mock.expect(b"ID\r", b"ID TM-D750\r");
    mock.expect(b"FV\r", b"FV 1.02\r");
    mock.expect(b"TY\r", b"TY K,2,1\r");
    mock.expect(ENTER, b"0M\r");
    for region in &regions {
        for page in region.pages() {
            script_read(&mut mock, page, radio_image.as_bytes())?;
        }
    }
    let mut planner = PatchPlanner::new();
    let _planner = planner.set_menu(pm_name, None, FieldValue::Text("MOBILE"))?;
    let patches = planner.finish()?;
    let mut expected_bytes = radio_image.clone().into_bytes();
    patches.apply_to_image(&mut expected_bytes)?;
    let expected_image = MemoryImage::from_bytes(expected_bytes)?;
    for patch in patches.pages() {
        script_read(&mut mock, patch.page(), radio_image.as_bytes())?;
        let start = patch.page().address().as_usize();
        let written = expected_image
            .as_bytes()
            .get(start..start + patch.page().len())
            .ok_or("patch page outside image")?;
        let mut frame = Header::write_request(patch.page()).to_vec();
        frame.extend_from_slice(written);
        mock.expect(&frame, &[ACK]);
        script_read(&mut mock, patch.page(), expected_image.as_bytes())?;
    }
    mock.expect(&[EXIT], &[ACK]);

    let mut radio = Radio::new(mock);
    let identity = radio.identify().await?;
    assert!(
        identity.is_write_target(),
        "the scripted radio is a write target"
    );
    let mut session = radio.enter_mcp().await?;
    let read = session.read_regions(&regions, |_| {}).await?;
    let before = read.into_memory_image(&regions)?;
    assert_eq!(
        before.global().read(&pm_name.descriptor)?,
        DecodedFieldValue::Text("HOME".to_owned())
    );
    assert_eq!(
        before.slot(slot).read(&meter.descriptor)?,
        DecodedFieldValue::Unsigned(1)
    );
    let replacements = patches
        .pages()
        .iter()
        .map(|patch| {
            let page = patch.page();
            let start = page.address().as_usize();
            let expected = before
                .as_bytes()
                .get(start..start + page.len())
                .ok_or("patch page outside image")?;
            let mut replacement = expected.to_vec();
            patch.apply(&mut replacement)?;
            Ok(PageReplacement::new(page, expected, &replacement)?)
        })
        .collect::<Result<Vec<_>, TestError>>()?;
    let report = session
        .compare_exchange_pages(&replacements, |_| Ok(()), |_| {})
        .await?;
    assert_eq!(report.verified_pages.len(), patches.len());
    session.exit().await?;
    radio.into_transport().assert_complete();
    Ok(())
}
