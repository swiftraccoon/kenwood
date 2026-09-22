//! Public complete-page compare-and-exchange behavior with simulated transports.

use kenwood_schema as _;
use mcp_d75_extract as _;
use mmdvm as _;
use thiserror as _;
use tokio_serial as _;
use tracing as _;

use std::future::{Future, poll_fn};
use std::io;
use std::pin::pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::task::Poll;
use std::time::Duration;

use kenwood_tmd750::protocol::mcp::{ACK, read_request, write_request};
use kenwood_tmd750::radio::programming::{McpJournal, PageReplacement};
use kenwood_tmd750::{Address, Error, McpError, Page, Progress, Radio};
use kenwood_transport::{MockTransport, Transport, TransportError};

type TestError = Box<dyn std::error::Error>;
type TestResult = Result<(), TestError>;

fn page(address: u32, length: usize) -> Result<Page, TestError> {
    Ok(Page::new(Address::new(address)?, length)?)
}

fn ready(firmware: &str) -> MockTransport {
    let mut mock = MockTransport::new();
    mock.expect(b"ID\r", b"ID TM-D750\r");
    mock.expect(b"FV\r", format!("FV {firmware}\r").as_bytes());
    mock.expect(b"TY\r", b"TY K,2,1\r");
    mock.expect(b"0M PROGRAM\r", b"0M\r");
    mock
}

fn frame(page: Page, data: &[u8]) -> Vec<u8> {
    let mut bytes = write_request(page).to_vec();
    bytes.extend_from_slice(data);
    bytes
}

fn read(mock: &mut MockTransport, page: Page, data: &[u8]) {
    mock.expect(&read_request(page), &frame(page, data));
    mock.expect(&[ACK], &[ACK]);
}

fn write_and_verify(mock: &mut MockTransport, replacement: &PageReplacement) {
    mock.expect(
        &frame(replacement.page(), replacement.replacement()),
        &[ACK],
    );
    read(mock, replacement.page(), replacement.replacement());
}

fn interrupted(error: &Error, possible: usize, verified: usize) -> Result<&Error, TestError> {
    let Error::Mcp(McpError::Interrupted {
        possibly_written,
        verified: actual_verified,
        source,
        ..
    }) = error
    else {
        return Err(format!("missing conservative interruption context: {error:?}").into());
    };
    assert_eq!(
        *possibly_written, possible,
        "retain the entire session's possible-write count"
    );
    assert_eq!(
        *actual_verified, verified,
        "retain only current full-page verifications"
    );
    Ok(source)
}

async fn assert_retired<T: Transport>(radio: &mut Radio<T>) {
    assert!(
        matches!(
            radio.identify().await,
            Err(Error::Mcp(
                McpError::RecoveryRequired | McpError::ConnectionRetired
            ))
        ),
        "retired or uncertain handles must prohibit CAT without I/O"
    );
    assert!(
        matches!(
            radio.mcp_session(),
            Err(Error::Mcp(
                McpError::RecoveryRequired | McpError::ConnectionRetired
            ))
        ),
        "retired or uncertain handles must prohibit another MCP borrow"
    );
}

#[derive(Debug)]
struct AuditedTransport {
    mock: MockTransport,
    wire: Arc<Mutex<Vec<Vec<u8>>>>,
    intents: Arc<AtomicUsize>,
    writes: usize,
    baud_changes: Vec<u32>,
}

impl Transport for AuditedTransport {
    async fn write(&mut self, bytes: &[u8]) -> Result<(), TransportError> {
        if bytes.first() == Some(&b'W') {
            self.writes += 1;
            assert_eq!(
                self.intents.load(Ordering::SeqCst),
                self.writes,
                "each W requires its own completed durable callback"
            );
        }
        self.wire
            .lock()
            .map_err(|_| TransportError::Write(io::Error::other("wire log poisoned")))?
            .push(bytes.to_vec());
        self.mock.write(bytes).await
    }

    async fn read(&mut self, bytes: &mut [u8]) -> Result<usize, TransportError> {
        self.mock.read(bytes).await
    }

    async fn close(&mut self) -> Result<(), TransportError> {
        self.mock.close().await
    }

    fn set_baud_rate(&mut self, baud: u32) -> Result<(), TransportError> {
        self.baud_changes.push(baud);
        self.mock.set_baud_rate(baud)
    }
}

fn expected_success_wire() -> Vec<Vec<u8>> {
    let mut first = b"W\0\x03\0\0".to_vec();
    first.extend_from_slice(&[0x5A; 256]);
    let mut second = b"W\0\x02\0\0".to_vec();
    second.extend_from_slice(&[0x33; 256]);
    [
        b"ID\r".as_slice(),
        b"FV\r",
        b"TY\r",
        b"0M PROGRAM\r",
        b"R\0\x03\0\0",
        &[ACK],
        b"R\0\0\x08\x28",
        &[ACK],
        b"R\0\x02\0\0",
        &[ACK],
        &first,
        b"R\0\x03\0\0",
        &[ACK],
        &second,
        b"R\0\x02\0\0",
        &[ACK],
        b"E",
    ]
    .into_iter()
    .map(<[u8]>::to_vec)
    .collect()
}

#[tokio::test]
async fn every_page_is_compared_before_the_first_write_with_exact_wire_and_noop_scope() -> TestResult
{
    let replacements = [
        PageReplacement::new(page(768, 256)?, &[0xA5; 256], &[0x5A; 256])?,
        PageReplacement::new(page(8, 40)?, &[0x11; 40], &[0x11; 40])?,
        PageReplacement::new(page(512, 256)?, &[0x22; 256], &[0x33; 256])?,
    ];
    let mut mock = ready("1.00");
    for replacement in &replacements {
        read(&mut mock, replacement.page(), replacement.expected());
    }
    for replacement in &replacements {
        if !replacement.is_noop() {
            write_and_verify(&mut mock, replacement);
        }
    }
    mock.expect(b"E", &[ACK]);
    let wire = Arc::new(Mutex::new(Vec::new()));
    let intents = Arc::new(AtomicUsize::new(0));
    let mut radio = Radio::new(AuditedTransport {
        mock,
        wire: Arc::clone(&wire),
        intents: Arc::clone(&intents),
        writes: 0,
        baud_changes: Vec::new(),
    });
    let mut session = radio.enter_mcp().await?;
    let mut progress = Vec::new();
    let report = session.compare_exchange_pages(&replacements, |_| {
        if intents.load(Ordering::SeqCst) == 0 {
            let wire = wire.lock().map_err(|_| io::Error::other("wire log poisoned"))?;
            let reads = wire.iter().filter(|bytes| bytes.first() == Some(&b'R')).count();
            drop(wire);
            assert_eq!(reads, 3, "all expected pages, including no-ops, must be freshly compared before intent");
        }
        let _previous = intents.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }, |value| progress.push(value)).await?;
    assert_eq!(
        report.compared_pages,
        replacements
            .iter()
            .map(PageReplacement::page)
            .collect::<Vec<_>>(),
        "validation sorting must not reorder caller operations"
    );
    assert_eq!(
        report.verified_pages,
        [page(768, 256)?, page(512, 256)?],
        "only changed pages require W and readback"
    );
    assert_eq!(
        report.unchanged_pages,
        [page(8, 40)?],
        "no-op remains read-only"
    );
    assert_eq!(
        progress,
        [
            Progress { done: 1, total: 3 },
            Progress { done: 2, total: 3 },
            Progress { done: 3, total: 3 }
        ],
        "progress counts fully resolved replacements"
    );
    assert_eq!(
        session.journal(),
        &McpJournal {
            possibly_written: report.verified_pages.clone(),
            verified: report.verified_pages
        },
        "only actual dispatches belong in the session write journal"
    );
    session.exit().await?;
    assert_retired(&mut radio).await;
    let transport = radio.into_transport();
    assert_eq!(
        transport.baud_changes,
        [9600],
        "detached exit cannot perform an old-handle baud operation"
    );
    assert_eq!(
        transport.writes, 2,
        "each changed page is exactly one write"
    );
    assert_eq!(
        transport.mock.writes(),
        expected_success_wire(),
        "literal framing pins every read, ACK, complete W, and detached exit"
    );
    transport.mock.assert_complete();
    Ok(())
}

#[tokio::test]
async fn stale_final_byte_of_a_later_page_prevents_every_write_and_intent() -> TestResult {
    let replacements = [
        PageReplacement::new(page(512, 256)?, &[0; 256], &[1; 256])?,
        PageReplacement::new(page(768, 256)?, &[2; 256], &[3; 256])?,
    ];
    let mut stale = [2; 256];
    *stale.last_mut().ok_or("tail missing")? = 4;
    let mut mock = ready("1.00");
    read(
        &mut mock,
        replacements[0].page(),
        replacements[0].expected(),
    );
    read(&mut mock, replacements[1].page(), &stale);
    mock.expect(b"E", &[ACK]);
    let mut radio = Radio::new(mock);
    let mut session = radio.enter_mcp().await?;
    let mut intents = 0;
    let mut progress = Vec::new();
    let error = session
        .compare_exchange_pages(
            &replacements,
            |_| {
                intents += 1;
                Ok(())
            },
            |value| progress.push(value),
        )
        .await
        .err()
        .ok_or("stale page unexpectedly passed")?;
    assert!(
        matches!(
            interrupted(&error, 0, 0)?,
            Error::Mcp(McpError::CompareMismatch {
                address: 768,
                offset: 255
            })
        ),
        "all bytes of every expected page participate in preflight"
    );
    assert_eq!(
        intents, 0,
        "global comparison must precede the first intent"
    );
    assert!(
        progress.is_empty(),
        "failed global preflight must not claim completed replacements"
    );
    assert!(
        session.is_ready(),
        "complete mismatch remains a safe exit boundary"
    );
    session.exit().await?;
    radio.into_transport().assert_complete();
    Ok(())
}

#[tokio::test]
async fn duplicates_and_unsupported_firmware_fail_before_page_traffic() -> TestResult {
    for firmware in ["1.00", "1.02"] {
        let replacement = PageReplacement::new(page(512, 256)?, &[0; 256], &[1; 256])?;
        let mut mock = ready(firmware);
        mock.expect(b"E", &[ACK]);
        let mut radio = Radio::new(mock);
        let mut session = radio.enter_mcp().await?;
        let mut called = false;
        let result = session
            .compare_exchange_pages(
                &[replacement.clone(), replacement],
                |_| {
                    called = true;
                    Ok(())
                },
                |_| {},
            )
            .await;
        if firmware == "1.00" {
            assert!(
                matches!(
                    result,
                    Err(Error::Mcp(McpError::DuplicateReplacement { address: 512 }))
                ),
                "duplicate batch entries must fail up front"
            );
        } else {
            assert!(
                matches!(result, Err(Error::UnsupportedSchemaTarget { .. })),
                "new compare-and-exchange must preserve the exact firmware gate"
            );
        }
        assert!(
            !called,
            "up-front rejection must never reach durable intent"
        );
        assert_eq!(
            session.journal(),
            &McpJournal::default(),
            "admission failure must not change the journal"
        );
        session.exit().await?;
        radio.into_transport().assert_complete();
    }
    Ok(())
}

#[tokio::test]
async fn noops_and_empty_batches_never_dispatch_but_still_enforce_scope() -> TestResult {
    for firmware in ["1.00", "1.02"] {
        let replacement = PageReplacement::new(page(8, 40)?, &[0xA5; 40], &[0xA5; 40])?;
        let mut mock = ready(firmware);
        if firmware == "1.00" {
            read(&mut mock, replacement.page(), replacement.expected());
        }
        mock.expect(b"E", &[ACK]);
        let mut radio = Radio::new(mock);
        let mut session = radio.enter_mcp().await?;
        let called = std::cell::Cell::new(false);
        let empty = session
            .compare_exchange_pages(
                &[],
                |_| {
                    called.set(true);
                    Ok(())
                },
                |_| called.set(true),
            )
            .await;
        let noop = session
            .compare_exchange_pages(
                &[replacement],
                |_| {
                    called.set(true);
                    Ok(())
                },
                |_| {},
            )
            .await;
        if firmware == "1.00" {
            assert!(
                empty?.compared_pages.is_empty(),
                "empty batches return empty results without progress"
            );
            let report = noop?;
            assert_eq!(
                report.unchanged_pages,
                [page(8, 40)?],
                "canonical short no-op fragments require comparison only"
            );
            assert!(
                report.verified_pages.is_empty(),
                "read-only comparison cannot manufacture a verified write"
            );
        } else {
            assert!(
                matches!(empty, Err(Error::UnsupportedSchemaTarget { .. })),
                "empty batches do not bypass firmware admission"
            );
            assert!(
                matches!(noop, Err(Error::UnsupportedSchemaTarget { .. })),
                "no-op batches do not bypass firmware admission"
            );
        }
        assert!(
            !called.get(),
            "empty and no-op batches must not request durable intent"
        );
        assert_eq!(
            session.journal(),
            &McpJournal::default(),
            "read-only comparisons leave prior write state untouched"
        );
        session.exit().await?;
        radio.into_transport().assert_complete();
    }
    Ok(())
}

#[tokio::test]
async fn callback_failure_prevents_its_write_without_rolling_back_earlier_pages() -> TestResult {
    for failed in 0..2 {
        let replacements = [
            PageReplacement::new(page(512, 256)?, &[0; 256], &[1; 256])?,
            PageReplacement::new(page(768, 256)?, &[2; 256], &[3; 256])?,
        ];
        let mut mock = ready("1.00");
        for replacement in &replacements {
            read(&mut mock, replacement.page(), replacement.expected());
        }
        if failed == 1 {
            write_and_verify(&mut mock, &replacements[0]);
        }
        mock.expect(b"E", &[ACK]);
        let mut radio = Radio::new(mock);
        let mut session = radio.enter_mcp().await?;
        let mut intents = 0;
        let error = session
            .compare_exchange_pages(
                &replacements,
                |_| {
                    let reject = intents == failed;
                    intents += 1;
                    if reject {
                        Err(io::Error::other("durable synchronization failed"))
                    } else {
                        Ok(())
                    }
                },
                |_| {},
            )
            .await
            .err()
            .ok_or("callback failure unexpectedly passed")?;
        let cause = interrupted(&error, failed, failed)?;
        assert!(
            matches!(cause, Error::Mcp(McpError::DurableIntent { source, .. }) if source.to_string() == "durable synchronization failed"),
            "retain the original durable-intent error"
        );
        assert_eq!(
            intents,
            failed + 1,
            "failed callback must prohibit later intents"
        );
        assert_eq!(
            session.journal().possibly_written.len(),
            failed,
            "only completed prior dispatches belong in the journal"
        );
        assert!(
            session.is_ready(),
            "callback failure occurs between complete exchanges"
        );
        session.exit().await?;
        radio.into_transport().assert_complete();
    }
    Ok(())
}

#[tokio::test]
async fn uncertain_write_ack_preserves_possible_change_and_forbids_speculative_exit() -> TestResult
{
    for timeout in [false, true] {
        let replacement = PageReplacement::new(page(512, 256)?, &[0; 256], &[1; 256])?;
        let mut mock = ready("1.00");
        read(&mut mock, replacement.page(), replacement.expected());
        let write = frame(replacement.page(), replacement.replacement());
        if timeout {
            mock.expect_hang(&write);
        } else {
            mock.expect(&write, &[0x15]);
        }
        let mut radio = Radio::new(mock);
        radio.set_timeout(Duration::from_millis(1));
        let mut session = radio.enter_mcp().await?;
        let error = session
            .compare_exchange_pages(&[replacement], |_| Ok(()), |_| {})
            .await
            .err()
            .ok_or("uncertain ACK unexpectedly passed")?;
        let _cause = interrupted(&error, 1, 0)?;
        assert!(
            !session.is_ready(),
            "unacknowledged W leaves framing uncertain"
        );
        assert!(
            matches!(
                session.exit().await,
                Err(Error::Mcp(McpError::RecoveryRequired))
            ),
            "an uncertain write prohibits speculative E"
        );
        assert_retired(&mut radio).await;
        let mock = radio.into_transport();
        assert_eq!(
            mock.writes().last(),
            Some(&write),
            "no readback, retry, rollback, or exit may follow uncertain W"
        );
        mock.assert_complete();
    }
    Ok(())
}

#[tokio::test]
async fn complete_last_byte_readback_mismatch_retains_actual_possible_write() -> TestResult {
    let replacement = PageReplacement::new(page(512, 256)?, &[0; 256], &[1; 256])?;
    let mut wrong = [1; 256];
    *wrong.last_mut().ok_or("tail missing")? = 2;
    let mut mock = ready("1.00");
    read(&mut mock, replacement.page(), replacement.expected());
    mock.expect(
        &frame(replacement.page(), replacement.replacement()),
        &[ACK],
    );
    read(&mut mock, replacement.page(), &wrong);
    mock.expect(b"E", &[ACK]);
    let mut radio = Radio::new(mock);
    let mut session = radio.enter_mcp().await?;
    let error = session
        .compare_exchange_pages(&[replacement], |_| Ok(()), |_| {})
        .await
        .err()
        .ok_or("wrong readback unexpectedly passed")?;
    assert!(
        matches!(
            interrupted(&error, 1, 0)?,
            Error::Mcp(McpError::VerifyMismatch {
                address: 512,
                offset: 255
            })
        ),
        "immediate readback must compare the final unrelated byte too"
    );
    assert!(
        session.is_ready(),
        "a complete mismatching readback permits explicit detached exit"
    );
    session.exit().await?;
    radio.into_transport().assert_complete();
    Ok(())
}

#[tokio::test]
async fn incomplete_preflight_or_readback_preserves_distinct_journal_states() -> TestResult {
    for after_write in [false, true] {
        let replacement = PageReplacement::new(page(512, 256)?, &[0; 256], &[1; 256])?;
        let mut mock = ready("1.00");
        if after_write {
            read(&mut mock, replacement.page(), replacement.expected());
            mock.expect(
                &frame(replacement.page(), replacement.replacement()),
                &[ACK],
            );
        }
        mock.expect_partial_then_hang(&read_request(replacement.page()), b"W\0");
        let mut radio = Radio::new(mock);
        radio.set_timeout(Duration::from_millis(1));
        let mut session = radio.enter_mcp().await?;
        let error = session
            .compare_exchange_pages(&[replacement], |_| Ok(()), |_| {})
            .await
            .err()
            .ok_or("incomplete read unexpectedly passed")?;
        let _cause = interrupted(&error, usize::from(after_write), 0)?;
        assert!(
            !session.is_ready(),
            "partial page framing prohibits more protocol traffic"
        );
        assert!(
            matches!(
                session.exit().await,
                Err(Error::Mcp(McpError::RecoveryRequired))
            ),
            "partial read must not send speculative E"
        );
        assert_retired(&mut radio).await;
        radio.into_transport().assert_complete();
    }
    Ok(())
}

#[tokio::test]
async fn failed_rewrite_cannot_inherit_an_earlier_verification_of_the_same_page() -> TestResult {
    let first = PageReplacement::new(page(512, 256)?, &[0; 256], &[1; 256])?;
    let second = PageReplacement::new(first.page(), first.replacement(), &[2; 256])?;
    let mut mock = ready("1.00");
    read(&mut mock, first.page(), first.expected());
    write_and_verify(&mut mock, &first);
    read(&mut mock, second.page(), second.expected());
    mock.expect(&frame(second.page(), second.replacement()), &[0x15]);
    let mut radio = Radio::new(mock);
    let mut session = radio.enter_mcp().await?;
    let report = session
        .compare_exchange_pages(&[first], |_| Ok(()), |_| {})
        .await?;
    assert_eq!(
        report.verified_pages,
        [page(512, 256)?],
        "the earlier write must actually have verified"
    );
    let error = session
        .compare_exchange_pages(&[second], |_| Ok(()), |_| {})
        .await
        .err()
        .ok_or("failed rewrite unexpectedly passed")?;
    let _cause = interrupted(&error, 1, 0)?;
    assert_eq!(
        session.journal().possibly_written,
        [page(512, 256)?],
        "one page has an unresolved latest write"
    );
    assert!(
        session.journal().verified.is_empty(),
        "a new dispatch must invalidate stale verification"
    );
    assert!(
        matches!(
            session.exit().await,
            Err(Error::Mcp(McpError::RecoveryRequired))
        ),
        "failed rewrite cannot permit exit"
    );
    radio.into_transport().assert_complete();
    Ok(())
}

async fn drop_pending(future: impl Future) {
    let mut future = pin!(future);
    let result = poll_fn(|context| Poll::Ready(future.as_mut().poll(context))).await;
    assert!(
        result.is_pending(),
        "scripted incomplete exchange must still be pending"
    );
}

#[tokio::test]
async fn dropped_write_future_retains_possible_dispatch_and_blocks_protocol_reuse() -> TestResult {
    let replacement = PageReplacement::new(page(512, 256)?, &[0; 256], &[1; 256])?;
    let mut mock = ready("1.00");
    read(&mut mock, replacement.page(), replacement.expected());
    mock.expect_hang(&frame(replacement.page(), replacement.replacement()));
    let mut radio = Radio::new(mock);
    let mut session = radio.enter_mcp().await?;
    drop_pending(session.compare_exchange_pages(&[replacement], |_| Ok(()), |_| {})).await;
    assert_eq!(
        session.journal().possibly_written,
        [page(512, 256)?],
        "possible dispatch must be marked before awaiting the exchange"
    );
    assert!(
        session.journal().verified.is_empty(),
        "an abandoned ACK cannot count as readback verification"
    );
    assert!(
        matches!(
            session.exit().await,
            Err(Error::Mcp(McpError::RecoveryRequired))
        ),
        "dropped write future cannot permit speculative E"
    );
    assert_retired(&mut radio).await;
    radio.into_transport().assert_complete();
    Ok(())
}
