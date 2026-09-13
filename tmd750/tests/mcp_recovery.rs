//! Recovery never attributes observed bytes to missing or ambiguous intent.

use kenwood_schema as _;
use mcp_d75_extract as _;
use thiserror as _;
use tokio_serial as _;
use tracing as _;

use kenwood_tmd750::protocol::mcp::{
    ACK, BytePatch, ENTER, EXIT, PagePatch, read_request, write_request,
};
use kenwood_tmd750::radio::McpJournal;
use kenwood_tmd750::{Address, Error, McpError, Page, Radio};
use kenwood_transport::MockTransport;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn patch(page: Page) -> Result<PagePatch, Box<dyn std::error::Error>> {
    Ok(PagePatch::new(page, vec![BytePatch::new(2, 0x0F, 5)?])?)
}

const fn journal(pages: Vec<Page>) -> McpJournal {
    McpJournal {
        possibly_written: pages,
        verified: Vec::new(),
    }
}

#[tokio::test]
async fn missing_wrong_length_and_ambiguous_intents_refuse_all_io() -> TestResult {
    let page = Page::new(Address::new(8)?, 40)?;
    let wrong_length = Page::new(page.address(), 39)?;
    for (intended, count) in [
        (Vec::new(), 0),
        (vec![patch(wrong_length)?], 0),
        (vec![patch(page)?, patch(page)?], 2),
    ] {
        let mut radio = Radio::new(MockTransport::new());
        let result = radio.recover(&journal(vec![page]), &intended).await;
        assert!(
            matches!(result, Err(Error::Mcp(McpError::RecoveryIntentCount {
                address: 8, len: 40, count: actual,
            })) if actual == count),
            "recovery must know exactly one intent before identity or entry: {result:?}"
        );
        radio.into_transport().assert_complete();
    }
    Ok(())
}

#[tokio::test]
async fn all_journal_pages_are_validated_before_any_radio_io() -> TestResult {
    let first = Page::new(Address::new(8)?, 40)?;
    let later = Page::new(Address::new(56)?, 200)?;
    let mut radio = Radio::new(MockTransport::new());
    let result = radio
        .recover(&journal(vec![first, later]), &[patch(first)?])
        .await;
    assert!(
        matches!(
            result,
            Err(Error::Mcp(McpError::RecoveryIntentCount {
                address: 56,
                len: 200,
                count: 0,
            }))
        ),
        "a later missing intent must prevent even the first page read: {result:?}"
    );
    radio.into_transport().assert_complete();
    Ok(())
}

#[tokio::test]
async fn duplicate_journal_pages_are_rejected_before_radio_io() -> TestResult {
    let page = Page::new(Address::new(8)?, 40)?;
    let mut radio = Radio::new(MockTransport::new());
    let result = radio
        .recover(&journal(vec![page, page]), &[patch(page)?])
        .await;
    assert!(
        matches!(
            result,
            Err(Error::Mcp(McpError::DuplicateRecoveryPage {
                address: 8,
                len: 40,
            }))
        ),
        "duplicate recovery attribution must be rejected: {result:?}"
    );
    radio.into_transport().assert_complete();
    Ok(())
}

#[tokio::test]
async fn an_empty_journal_does_not_enter_programming_mode() -> TestResult {
    let mut radio = Radio::new(MockTransport::new());
    let result = radio.recover(&McpJournal::default(), &[]).await?;
    assert!(result.applied.is_empty(), "no page has recovery evidence");
    assert!(result.pending.is_empty(), "no page needs a recovery read");
    radio.into_transport().assert_complete();
    Ok(())
}

#[tokio::test]
async fn recovery_reads_only_journaled_pages_and_retires_the_original_handle() -> TestResult {
    let page = Page::new(Address::new(8)?, 40)?;
    let unattempted = Page::new(Address::new(56)?, 200)?;
    for stored in [0xF5, 0xF6] {
        let mut mock = MockTransport::new();
        mock.expect(b"ID\r", b"ID TM-D750\r");
        mock.expect(b"FV\r", b"FV 1.00\r");
        mock.expect(b"TY\r", b"TY K,2,1\r");
        mock.expect(ENTER, b"0M\r");
        let mut reply = write_request(page).to_vec();
        reply.extend(vec![stored; page.len()]);
        mock.expect(&read_request(page), &reply);
        mock.expect(&[ACK], &[ACK]);
        mock.expect(&[EXIT], &[ACK]);
        let mut radio = Radio::new(mock);
        let result = radio
            .recover(&journal(vec![page]), &[patch(page)?, patch(unattempted)?])
            .await?;
        assert_eq!(
            result.applied,
            if stored == 0xF5 {
                vec![page]
            } else {
                Vec::new()
            },
            "applied requires the exact intended masked bits"
        );
        assert_eq!(
            result.pending,
            if stored == 0xF6 {
                vec![page]
            } else {
                Vec::new()
            },
            "mismatching intent must remain pending"
        );
        let further = radio.get_dv_gateway_mode().await;
        assert!(
            matches!(further, Err(Error::Mcp(McpError::ConnectionRetired))),
            "successful recovery is not permission for old-handle CAT: {further:?}"
        );
        radio.into_transport().assert_complete();
    }
    Ok(())
}
