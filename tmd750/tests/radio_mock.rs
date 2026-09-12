//! Identity proof and MCP exchanges over a scripted mock transport.

use mcp_d75_extract as _;
use thiserror as _;
use tokio_serial as _;
use tracing as _;

use kenwood_tmd750::protocol::mcp::{
    ACK, BytePatch, ENTER, EXIT, PagePatch, read_request, write_request,
};
use kenwood_tmd750::radio::Radio;
use kenwood_tmd750::{
    Address, Band, DvGatewayMode, Error, McpError, OperatingMode, Page, ProtocolError, Region,
    SelectableMode,
};
use kenwood_transport::MockTransport;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn scripted_identity(mock: &mut MockTransport) {
    mock.expect(b"ID\r", b"ID TM-D750\r");
    mock.expect(b"FV\r", b"FV 1.00\r");
    mock.expect(b"TY\r", b"TY K,2,1\r");
}

fn scripted_live_identity(mock: &mut MockTransport) {
    mock.expect(b"ID\r", b"ID TM-D750\r");
    mock.expect(b"FV\r", b"FV 1.02\r");
    mock.expect(b"TY\r", b"TY K,2,1\r");
}

fn read_reply(page: Page, data: &[u8]) -> Vec<u8> {
    let mut reply = write_request(page).to_vec();
    reply.extend_from_slice(data);
    reply
}

fn fill_reply(page: Page, fill: u8) -> Vec<u8> {
    let mut reply = write_request(page).to_vec();
    if let Some(first) = reply.first_mut() {
        *first = b'Z';
    }
    reply.push(fill);
    reply
}

#[tokio::test]
async fn identify_proves_the_model_and_caches_it() -> TestResult {
    let mut mock = MockTransport::new();
    scripted_identity(&mut mock);
    let mut radio = Radio::new(mock);
    let identity = radio.identify().await?;
    assert_eq!(identity.firmware.as_str(), "1.00");
    assert_eq!(identity.radio_type.as_str(), "K,2,1");
    assert_eq!(radio.identity(), Some(&identity));
    radio.into_transport().assert_complete();
    Ok(())
}

#[tokio::test]
async fn operating_modes_and_gateway_state_are_typed_reads() -> TestResult {
    let mut mock = MockTransport::new();
    mock.expect(b"MD 0\r", b"MD 0,0\r");
    mock.expect(b"MD 1\r", b"MD 1,7\r");
    mock.expect(b"GW\r", b"GW 2\r");
    let mut radio = Radio::new(mock);
    assert_eq!(radio.get_operating_mode(Band::A).await?, OperatingMode::Fm);
    assert_eq!(
        radio.get_operating_mode(Band::B).await?,
        OperatingMode::Unqualified(7)
    );
    assert_eq!(
        radio.get_dv_gateway_mode().await?,
        DvGatewayMode::Terminal,
        "the read-only GW query must return the observed Terminal state"
    );
    radio.into_transport().assert_complete();
    Ok(())
}

#[tokio::test]
async fn dstar_selection_requires_matching_echo_and_readback() -> TestResult {
    let mut mock = MockTransport::new();
    scripted_live_identity(&mut mock);
    mock.expect(b"MD 0,1\r", b"MD 0,1\r");
    mock.expect(b"MD 0\r", b"MD 0,1\r");
    let mut radio = Radio::new(mock);
    radio.enter_dstar(Band::A).await?;
    radio.into_transport().assert_complete();
    Ok(())
}

#[tokio::test]
async fn mode_selection_refuses_a_mismatched_readback() -> TestResult {
    let mut mock = MockTransport::new();
    scripted_live_identity(&mut mock);
    mock.expect(b"MD 1,1\r", b"MD 1,1\r");
    mock.expect(b"MD 1\r", b"MD 1,0\r");
    let mut radio = Radio::new(mock);
    let result = radio.set_operating_mode(Band::B, SelectableMode::Dv).await;
    assert!(
        matches!(
            result,
            Err(Error::Protocol(ProtocolError::UnexpectedResponse { .. }))
        ),
        "{result:?}"
    );
    radio.into_transport().assert_complete();
    Ok(())
}

#[tokio::test]
async fn mode_selection_refuses_an_unqualified_target_before_writing() -> TestResult {
    let mut mock = MockTransport::new();
    scripted_identity(&mut mock);
    let mut radio = Radio::new(mock);
    let result = radio.set_operating_mode(Band::A, SelectableMode::Dv).await;
    assert!(
        matches!(result, Err(Error::UnsupportedCatWriteTarget { .. })),
        "{result:?}"
    );
    radio.into_transport().assert_complete();
    Ok(())
}

#[tokio::test]
async fn mode_selection_refuses_an_unqualified_radio_type_before_writing() -> TestResult {
    let mut mock = MockTransport::new();
    mock.expect(b"ID\r", b"ID TM-D750\r");
    mock.expect(b"FV\r", b"FV 1.02\r");
    mock.expect(b"TY\r", b"TY K,2,2\r");
    let mut radio = Radio::new(mock);
    let result = radio.set_operating_mode(Band::A, SelectableMode::Dv).await;
    assert!(
        matches!(
            result,
            Err(Error::UnsupportedCatWriteTarget {
                ref actual_firmware,
                ref actual_radio_type,
                ..
            }) if actual_firmware == "1.02" && actual_radio_type == "K,2,2"
        ),
        "{result:?}"
    );
    radio.into_transport().assert_complete();
    Ok(())
}

#[tokio::test]
async fn mode_selection_refuses_a_mismatched_write_echo_before_readback() -> TestResult {
    for reply in [b"MD 1,1\r".as_slice(), b"MD 0,0\r", b"N\r", b"?\r"] {
        let mut mock = MockTransport::new();
        scripted_live_identity(&mut mock);
        mock.expect(b"MD 0,1\r", reply);
        let mut radio = Radio::new(mock);
        let result = radio.set_operating_mode(Band::A, SelectableMode::Dv).await;
        assert!(
            matches!(
                result,
                Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                    expected: "matching OperatingMode write echo",
                    ..
                }))
            ),
            "reply {reply:?}: {result:?}"
        );
        radio.into_transport().assert_complete();
    }
    Ok(())
}

#[tokio::test]
async fn operating_mode_reads_refuse_the_other_bands_reply() -> TestResult {
    let mut mock = MockTransport::new();
    mock.expect(b"MD 0\r", b"MD 1,1\r");
    let mut radio = Radio::new(mock);
    let result = radio.get_operating_mode(Band::A).await;
    assert!(
        matches!(
            result,
            Err(Error::Protocol(ProtocolError::UnexpectedResponse { .. }))
        ),
        "{result:?}"
    );
    radio.into_transport().assert_complete();
    Ok(())
}

#[tokio::test]
async fn mode_selection_refuses_a_readback_from_the_other_band() -> TestResult {
    let mut mock = MockTransport::new();
    scripted_live_identity(&mut mock);
    mock.expect(b"MD 0,1\r", b"MD 0,1\r");
    mock.expect(b"MD 0\r", b"MD 1,1\r");
    let mut radio = Radio::new(mock);
    let result = radio.set_operating_mode(Band::A, SelectableMode::Dv).await;
    assert!(
        matches!(
            result,
            Err(Error::Protocol(ProtocolError::UnexpectedResponse { .. }))
        ),
        "{result:?}"
    );
    radio.into_transport().assert_complete();
    Ok(())
}

#[tokio::test]
async fn a_different_radio_is_refused() -> TestResult {
    let mut mock = MockTransport::new();
    mock.expect(b"ID\r", b"ID TH-D75\r");
    let mut radio = Radio::new(mock);
    let refused = radio.identify().await;
    assert!(
        matches!(
            refused,
            Err(Error::Protocol(ProtocolError::UnexpectedIdentity { ref reply })) if reply == "TH-D75"
        ),
        "{refused:?}"
    );
    Ok(())
}

#[tokio::test]
async fn silence_is_a_timeout() -> TestResult {
    let mut mock = MockTransport::new();
    mock.expect_hang(b"ID\r");
    let mut radio = Radio::new(mock);
    radio.set_timeout(std::time::Duration::from_millis(50));
    let result = radio.identify().await;
    assert!(matches!(result, Err(Error::Timeout { .. })), "{result:?}");
    Ok(())
}

#[tokio::test]
async fn read_regions_handles_data_and_fill_replies() -> TestResult {
    let region = Region::new(8, 300)?;
    let pages = region.pages();
    let first = pages.first().copied().ok_or("no first page")?;
    let second = pages.get(1).copied().ok_or("no second page")?;
    let mut mock = MockTransport::new();
    scripted_identity(&mut mock);
    mock.expect(ENTER, b"0M\r");
    mock.expect(&read_request(first), &read_reply(first, &[0x11; 256]));
    mock.expect(&[ACK], &[ACK]);
    mock.expect(&read_request(second), &fill_reply(second, 0xEE));
    mock.expect(&[ACK], &[ACK]);
    mock.expect(&[EXIT], &[ACK]);
    let mut radio = Radio::new(mock);
    let mut session = radio.enter_mcp().await?;
    let mut seen = Vec::new();
    let image = session
        .read_regions(&[region], |progress| seen.push(progress.done))
        .await?;
    assert_eq!(seen, vec![1, 2]);
    assert!(image.covers(region));
    let bytes = image.bytes(region).ok_or("region not readable")?;
    assert_eq!(bytes.len(), 292);
    assert_eq!(bytes.first().copied(), Some(0x11));
    assert_eq!(bytes.last().copied(), Some(0xEE));
    session.exit().await?;
    radio.into_transport().assert_complete();
    Ok(())
}

#[tokio::test]
async fn echo_mismatch_is_a_protocol_error() -> TestResult {
    let page = Page::new(Address::new(8)?, 40)?;
    let other = Page::new(Address::new(56)?, 40)?;
    let mut mock = MockTransport::new();
    scripted_identity(&mut mock);
    mock.expect(ENTER, b"0M\r");
    mock.expect(&read_request(page), &read_reply(other, &[0; 40]));
    let mut radio = Radio::new(mock);
    let mut session = radio.enter_mcp().await?;
    let result = session.read_regions(&[page.region()], |_| {}).await;
    assert!(
        matches!(
            result,
            Err(Error::Protocol(ProtocolError::HeaderEcho { .. }))
        ),
        "{result:?}"
    );
    Ok(())
}

fn patch_for(page: Page) -> Result<PagePatch, Box<dyn std::error::Error>> {
    Ok(PagePatch::new(page, vec![BytePatch::new(2, 0xFF, 0x42)?])?)
}

#[tokio::test]
async fn verified_write_reads_patches_writes_and_reads_back() -> TestResult {
    let page = Page::new(Address::new(8)?, 40)?;
    let patch = patch_for(page)?;
    let mut written = vec![0x00; 40];
    patch.apply(&mut written)?;
    let mut expected_write = write_request(page).to_vec();
    expected_write.extend_from_slice(&written);
    let mut mock = MockTransport::new();
    scripted_identity(&mut mock);
    mock.expect(ENTER, b"0M\r");
    mock.expect(&read_request(page), &read_reply(page, &[0x00; 40]));
    mock.expect(&[ACK], &[ACK]);
    mock.expect(&expected_write, &[ACK]);
    mock.expect(&read_request(page), &read_reply(page, &written));
    mock.expect(&[ACK], &[ACK]);
    mock.expect(&[EXIT], &[ACK]);
    let mut radio = Radio::new(mock);
    let mut session = radio.enter_mcp().await?;
    let report = session.write_pages_verified(&[patch], |_| {}).await?;
    assert_eq!(report.verified_pages, vec![page]);
    assert!(report.possibly_written_pages.is_empty());
    session.exit().await?;
    radio.into_transport().assert_complete();
    Ok(())
}

#[tokio::test]
async fn a_read_back_mismatch_keeps_the_page_in_the_journal() -> TestResult {
    let page = Page::new(Address::new(8)?, 40)?;
    let patch = patch_for(page)?;
    let mut written = vec![0x00; 40];
    patch.apply(&mut written)?;
    let mut expected_write = write_request(page).to_vec();
    expected_write.extend_from_slice(&written);
    let mut mock = MockTransport::new();
    scripted_identity(&mut mock);
    mock.expect(ENTER, b"0M\r");
    mock.expect(&read_request(page), &read_reply(page, &[0x00; 40]));
    mock.expect(&[ACK], &[ACK]);
    mock.expect(&expected_write, &[ACK]);
    mock.expect(&read_request(page), &read_reply(page, &[0x00; 40]));
    mock.expect(&[ACK], &[ACK]);
    let mut radio = Radio::new(mock);
    let mut session = radio.enter_mcp().await?;
    let result = session.write_pages_verified(&[patch], |_| {}).await;
    match result {
        Err(Error::Mcp(McpError::Interrupted {
            possibly_written,
            verified,
            source,
            ..
        })) => {
            assert_eq!((possibly_written, verified), (1, 0));
            assert!(
                matches!(
                    *source,
                    Error::Mcp(McpError::VerifyMismatch { offset: 2, .. })
                ),
                "{source:?}"
            );
        }
        other => return Err(format!("expected an interrupted write, got {other:?}").into()),
    }
    assert_eq!(session.journal().possibly_written, vec![page]);
    Ok(())
}

#[tokio::test]
async fn pages_outside_the_writable_regions_are_refused_before_any_write() -> TestResult {
    let outside = Page::new(Address::new(400_000)?, 256)?;
    let mut mock = MockTransport::new();
    scripted_identity(&mut mock);
    mock.expect(ENTER, b"0M\r");
    let mut radio = Radio::new(mock);
    let mut session = radio.enter_mcp().await?;
    let result = session
        .write_pages_verified(&[patch_for(outside)?], |_| {})
        .await;
    assert!(
        matches!(
            result,
            Err(Error::Mcp(McpError::PageNotWritable {
                address: 400_000,
                ..
            }))
        ),
        "{result:?}"
    );
    assert!(session.journal().possibly_written.is_empty());
    Ok(())
}

#[tokio::test]
async fn recovery_reports_which_journaled_pages_carry_the_patch() -> TestResult {
    let page = Page::new(Address::new(8)?, 40)?;
    let patch = patch_for(page)?;
    let mut applied = vec![0x00; 40];
    patch.apply(&mut applied)?;
    let mut mock = MockTransport::new();
    scripted_identity(&mut mock);
    mock.expect(ENTER, b"0M\r");
    mock.expect(&read_request(page), &read_reply(page, &applied));
    mock.expect(&[ACK], &[ACK]);
    mock.expect(&[EXIT], &[ACK]);
    let mut radio = Radio::new(mock);
    let journal = kenwood_tmd750::radio::programming::McpJournal {
        possibly_written: vec![page],
        verified: Vec::new(),
    };
    let report = radio.recover(&journal, &[patch]).await?;
    assert_eq!(report.applied, vec![page]);
    assert!(report.pending.is_empty());
    radio.into_transport().assert_complete();
    Ok(())
}
