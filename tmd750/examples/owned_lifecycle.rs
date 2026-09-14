//! Execute read-only CAT/MCP ownership examples using strict mocks only.
//!
//! No device, file, network, or settings write is used. Fixture bytes and
//! fresh connections are scripted observations, not hardware qualification.

use kenwood_schema as _;
use mcp_d75_extract as _;
use thiserror as _;
use tokio_serial as _;
use tracing as _;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use kenwood_tmd750::protocol::mcp::{ACK, ENTER, EXIT, read_request, write_request};
use kenwood_tmd750::radio::{McpJournal, RegionImage};
use kenwood_tmd750::{DvGatewayMode, Error, McpError, ProtocolError, Radio, Region};
use kenwood_transport::{MockTransport, Transport, TransportError};

type ExampleResult = Result<(), Box<dyn std::error::Error>>;

// Example-only fault injection: keep the normal mock's strict wire ordering,
// while independently observing close/drop and optionally failing close.
#[derive(Debug, Clone, Copy)]
enum CloseResult {
    Success,
    Failure,
}

#[derive(Debug)]
struct Fixture {
    mock: MockTransport,
    close_result: CloseResult,
    close_calls: usize,
    dropped: Arc<AtomicBool>,
}

impl Fixture {
    fn new(mock: MockTransport, close_result: CloseResult) -> Self {
        Self {
            mock,
            close_result,
            close_calls: 0,
            dropped: Arc::new(AtomicBool::new(false)),
        }
    }
}

impl Transport for Fixture {
    async fn write(&mut self, bytes: &[u8]) -> Result<(), TransportError> {
        self.mock.write(bytes).await
    }

    async fn read(&mut self, bytes: &mut [u8]) -> Result<usize, TransportError> {
        self.mock.read(bytes).await
    }

    async fn close(&mut self) -> Result<(), TransportError> {
        self.close_calls += 1;
        self.mock.close().await?;
        match self.close_result {
            CloseResult::Success => Ok(()),
            CloseResult::Failure => Err(TransportError::Disconnected(std::io::Error::other(
                "injected close failure",
            ))),
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.dropped.store(true, Ordering::Relaxed);
    }
}

#[derive(Debug)]
struct Closed<T> {
    operation: T,
    close: Result<(), TransportError>,
}

// No `?` between operation completion and retirement: retain both results.
async fn retire<T: Send>(radio: Radio<Fixture>, operation: T) -> Closed<T> {
    let mut transport = radio.into_transport();
    let close = transport.close().await;
    let close_calls = transport.close_calls;
    transport.mock.assert_complete();
    let dropped = Arc::clone(&transport.dropped);
    drop(transport);
    assert_eq!(close_calls, 1, "explicit close must precede drop");
    assert!(dropped.load(Ordering::Relaxed), "owner must be dropped");
    Closed { operation, close }
}

fn identity_script() -> MockTransport {
    let mut mock = MockTransport::new();
    mock.expect(b"ID\r", b"ID TM-D750\r");
    mock.expect(b"FV\r", b"FV 1.02\r");
    mock.expect(b"TY\r", b"TY K,2,1\r");
    mock
}

fn page_script(mock: &mut MockTransport, region: Region) -> ExampleResult {
    let page = region
        .pages()
        .first()
        .copied()
        .ok_or("region has no page")?;
    let mut reply = write_request(page).to_vec();
    reply.extend(std::iter::repeat_n(0x2A, page.len()));
    mock.expect(&read_request(page), &reply);
    mock.expect(&[ACK], &[ACK]);
    Ok(())
}

#[derive(Debug)]
enum ExitOutcome {
    EntryFailed,
    BlockedByIncompleteExchange,
    Attempted(Result<(), Error>),
}

#[derive(Debug)]
struct McpRead {
    operation: Result<RegionImage, Error>,
    exit: ExitOutcome,
    journal: McpJournal,
}

async fn read_fragment<T: Transport>(
    radio: &mut Radio<T>,
    region: Region,
    expected: u8,
) -> McpRead {
    let mut session = match radio.enter_mcp().await {
        Ok(session) => session,
        Err(error) => {
            return McpRead {
                operation: Err(error),
                exit: ExitOutcome::EntryFailed,
                journal: McpJournal::default(),
            };
        }
    };
    let operation = session
        .read_regions(&[region], |_| {})
        .await
        .and_then(|image| {
            if image
                .bytes(region)
                .is_some_and(|bytes| bytes.iter().all(|byte| *byte == expected))
            {
                Ok(image)
            } else {
                // A host comparison can fail after a fully acknowledged read.
                Err(ProtocolError::UnexpectedResponse {
                    expected: "fixture page containing only the requested byte",
                    actual: "complete page had different fixture content".to_owned(),
                }
                .into())
            }
        });
    // Retain the journal before consuming the session, including on error.
    let journal = session.journal().clone();
    let exit = if session.is_ready() {
        ExitOutcome::Attempted(session.exit().await)
    } else {
        // Do not send E into an incomplete binary exchange.
        ExitOutcome::BlockedByIncompleteExchange
    };
    McpRead {
        operation,
        exit,
        journal,
    }
}

async fn cat_failure_retains_close_failure() {
    let mut mock = MockTransport::new();
    mock.expect(b"ID\r", b"ID NOT-THE-SELECTED-MODEL\r");
    let mut radio = Radio::new(Fixture::new(mock, CloseResult::Failure));
    let operation = radio.identify().await;
    // This call must be refused locally, without a second ID on the wire.
    let refused = radio.identify().await;
    let closed = retire(radio, operation).await;
    assert!(
        matches!(
            closed.operation,
            Err(Error::Protocol(ProtocolError::UnexpectedIdentity { .. }))
        ),
        "the original identity failure must survive cleanup"
    );
    assert!(
        matches!(refused, Err(Error::Mcp(McpError::RecoveryRequired))),
        "uncertain identity exchange must block further CAT locally"
    );
    assert!(
        matches!(closed.close, Err(TransportError::Disconnected(_))),
        "independent close failure must remain observable"
    );
}

async fn complete_mcp_lifecycle() -> ExampleResult {
    let region = Region::new(8, 48)?;
    let mut mock = identity_script();
    mock.expect(ENTER, b"0M\r");
    page_script(&mut mock, region)?;
    mock.expect(&[EXIT], &[ACK]);
    let mut radio = Radio::new(Fixture::new(mock, CloseResult::Success));
    let read = read_fragment(&mut radio, region, 0x2A).await;
    let identity = radio.identity().cloned();
    let closed = retire(radio, read).await;
    closed.close?;
    assert!(
        matches!(closed.operation.exit, ExitOutcome::Attempted(Ok(()))),
        "successful read must retain the acknowledged MCP exit"
    );
    assert!(
        closed.operation.journal.possibly_written.is_empty(),
        "a read-only session must not report possibly written pages"
    );
    let image = closed.operation.operation?;
    assert!(image.covers(region), "the completed page must be covered");
    assert_eq!(
        image.bytes(region),
        Some([0x2A; 40].as_slice()),
        "covered bytes must equal the received payload"
    );
    let gap = Region::new(48, 56)?;
    assert!(
        image.bytes(gap).is_none(),
        "unread gaps are not observations"
    );
    let coverage = image.covered().to_vec();
    let storage = image.into_memory_image(&[region])?;
    assert_eq!(coverage, [region], "coverage must exclude unread gaps");
    assert_eq!(
        storage.as_bytes().get(48..56),
        Some([0; 8].as_slice()),
        "unread storage contains synthetic zeroes, not observations"
    );
    // Full-sized storage retained synthetic zeroes, not a complete backup.

    // Only now create a separate fixture owner. A live caller instead uses its
    // explicitly selected endpoint and qualified readiness policy, never a
    // guessed delay or reopening an uncertain protocol stream.
    let mut fresh = identity_script();
    fresh.expect(b"GW\r", b"GW 0\r");
    let mut radio = Radio::new(Fixture::new(fresh, CloseResult::Success));
    let verification = async {
        let observed = radio.identify().await?;
        let gateway = radio.get_dv_gateway_mode().await?;
        Ok::<_, Error>((observed, gateway))
    }
    .await;
    let closed = retire(radio, verification).await;
    closed.close?;
    let (observed, gateway) = closed.operation?;
    assert_eq!(Some(observed), identity, "fresh identity must match");
    assert_eq!(gateway, DvGatewayMode::Off, "fresh GW must confirm Off");
    Ok(())
}

async fn mcp_failure_retains_exit_and_close_failures() -> ExampleResult {
    let region = Region::new(8, 48)?;
    let mut mock = identity_script();
    mock.expect(ENTER, b"0M\r");
    page_script(&mut mock, region)?;
    mock.expect(&[EXIT], &[0x15]); // Complete but invalid exit acknowledgement.
    let mut radio = Radio::new(Fixture::new(mock, CloseResult::Failure));
    let operation = read_fragment(&mut radio, region, 0).await;
    let closed = retire(radio, operation).await;
    assert!(
        matches!(
            closed.operation.operation,
            Err(Error::Protocol(ProtocolError::UnexpectedResponse { .. }))
        ),
        "the completed-page comparison failure must survive cleanup"
    );
    assert!(
        matches!(
            closed.operation.exit,
            ExitOutcome::Attempted(Err(Error::Protocol(ProtocolError::MissingAck { .. })))
        ),
        "the independent exit acknowledgement failure must survive close"
    );
    assert!(
        matches!(closed.close, Err(TransportError::Disconnected(_))),
        "the independent close failure must survive operation and exit errors"
    );
    assert!(
        closed.operation.journal.possibly_written.is_empty(),
        "a failed read-only session must not invent possibly written pages"
    );
    // Failed exit/close grants no fresh-connection or retry authority.
    Ok(())
}

async fn incomplete_read_sends_no_speculative_exit() -> ExampleResult {
    let region = Region::new(8, 48)?;
    let page = region
        .pages()
        .first()
        .copied()
        .ok_or("region has no page")?;
    let mut mock = identity_script();
    mock.expect(ENTER, b"0M\r");
    mock.expect_eof(&read_request(page));
    let mut radio = Radio::new(Fixture::new(mock, CloseResult::Success));
    let operation = read_fragment(&mut radio, region, 0x2A).await;
    let closed = retire(radio, operation).await;
    closed.close?;
    assert!(
        matches!(
            closed.operation.operation,
            Err(Error::Transport(TransportError::Disconnected(_)))
        ),
        "the incomplete read must retain its disconnection error"
    );
    assert!(
        matches!(
            closed.operation.exit,
            ExitOutcome::BlockedByIncompleteExchange
        ),
        "an incomplete binary exchange must not send a speculative exit"
    );
    assert!(
        closed.operation.journal.possibly_written.is_empty(),
        "an incomplete read must not invent possibly written pages"
    );
    Ok(())
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExampleResult {
    cat_failure_retains_close_failure().await;
    complete_mcp_lifecycle().await?;
    mcp_failure_retains_exit_and_close_failures().await?;
    incomplete_read_sends_no_speculative_exit().await?;
    Ok(())
}
