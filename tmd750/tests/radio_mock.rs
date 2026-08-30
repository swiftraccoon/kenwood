//! Identity proof and MCP exchanges over a scripted mock transport.

use kenwood_thd75 as _;
use mcp_d75_extract as _;
use thiserror as _;
use tokio_serial as _;
use tracing as _;

use kenwood_tmd750::protocol::mcp::{
    ACK, BytePatch, ENTER, EXIT, PagePatch, read_request, write_request,
};
use kenwood_tmd750::radio::Radio;
use kenwood_tmd750::transport::MockTransport;
use kenwood_tmd750::{Address, Error, McpError, Page, ProtocolError, Region};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn scripted_identity(mock: &mut MockTransport) {
    mock.expect(b"ID\r", b"ID TM-D750\r");
    mock.expect(b"FV\r", b"FV 1.00\r");
    mock.expect(b"TY\r", b"TY J\r");
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
    assert_eq!(identity.market.as_byte(), b'J');
    assert_eq!(radio.identity(), Some(&identity));
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

fn patch_for(page: Page) -> PagePatch {
    PagePatch {
        page,
        bytes: vec![BytePatch {
            offset: 2,
            mask: 0xFF,
            value: 0x42,
        }],
    }
}

#[tokio::test]
async fn verified_write_reads_patches_writes_and_reads_back() -> TestResult {
    let page = Page::new(Address::new(8)?, 40)?;
    let patch = patch_for(page);
    let mut written = vec![0x00; 40];
    patch.apply(&mut written);
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
    let patch = patch_for(page);
    let mut written = vec![0x00; 40];
    patch.apply(&mut written);
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
        .write_pages_verified(&[patch_for(outside)], |_| {})
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
    let patch = patch_for(page);
    let mut applied = vec![0x00; 40];
    patch.apply(&mut applied);
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
