//! Deterministic tests of reply buffering, stale-line handling, write
//! deadlines, and cancellation.

use super::*;

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[derive(Debug, Clone, Copy)]
enum WriteBehavior {
    Complete,
    Pending,
}

/// A transport whose chunks either are readable immediately (`released`
/// unset) or become readable one per write, as a radio's replies do.
#[derive(Debug)]
struct TestTransport {
    chunks: VecDeque<Vec<u8>>,
    writes: Vec<Vec<u8>>,
    reads: usize,
    write_behavior: WriteBehavior,
    release_per_write: bool,
    released: usize,
}

impl TestTransport {
    fn new(chunks: impl IntoIterator<Item = Vec<u8>>) -> Self {
        Self {
            chunks: chunks.into_iter().collect(),
            writes: Vec::new(),
            reads: 0,
            write_behavior: WriteBehavior::Complete,
            release_per_write: false,
            released: 0,
        }
    }

    /// One chunk becomes readable per completed write.
    fn replying(chunks: impl IntoIterator<Item = Vec<u8>>) -> Self {
        let mut transport = Self::new(chunks);
        transport.release_per_write = true;
        transport
    }
}

impl Transport for TestTransport {
    async fn write(&mut self, bytes: &[u8]) -> Result<(), TransportError> {
        self.writes.push(bytes.to_vec());
        match self.write_behavior {
            WriteBehavior::Complete => {
                self.released += 1;
                Ok(())
            }
            WriteBehavior::Pending => std::future::pending().await,
        }
    }

    async fn read(&mut self, buffer: &mut [u8]) -> Result<usize, TransportError> {
        if self.release_per_write && self.released == 0 {
            return std::future::pending().await;
        }
        let Some(mut chunk) = self.chunks.pop_front() else {
            return std::future::pending().await;
        };
        self.reads += 1;
        let count = chunk.len().min(buffer.len());
        for (target, source) in buffer.iter_mut().zip(&chunk) {
            *target = *source;
        }
        if chunk.len() > count {
            self.chunks.push_front(chunk.split_off(count));
        } else if self.release_per_write {
            self.released -= 1;
        }
        Ok(count)
    }

    async fn close(&mut self) -> Result<(), TransportError> {
        Ok(())
    }
}

#[tokio::test]
async fn a_repeated_reply_line_never_stands_in_for_the_next_command() -> TestResult {
    let transport = TestTransport::replying([
        b"ID TM-D750\rID TM-D750\r".to_vec(),
        b"FV 1.02\r".to_vec(),
        b"TY K,2,1\r".to_vec(),
    ]);
    let mut radio = Radio::new(transport);
    let identity = radio.identify().await?;
    assert_eq!(identity.firmware.as_str(), "1.02");
    assert_eq!(identity.radio_type.as_str(), "K,2,1");
    let transport = radio.into_transport();
    assert_eq!(
        transport.reads, 3,
        "the repeated ID line must not stand in for the FV reply"
    );
    assert_eq!(
        transport.writes,
        [b"ID\r".to_vec(), b"FV\r".to_vec(), b"TY\r".to_vec()]
    );
    Ok(())
}

#[tokio::test]
async fn commands_after_a_rejection_discard_the_repeated_reply() -> TestResult {
    let transport = TestTransport::replying([
        b"?\r".to_vec(),
        b"FQ 0,0145190000\rFQ 0,0145190000\r".to_vec(),
        b"FQ 0,0145195000\rFQ 0,0145195000\r".to_vec(),
        b"FQ 0,0145200000\r".to_vec(),
    ]);
    let mut radio = Radio::new(transport);
    let rejected = radio.get_frequency(Band::A).await;
    assert!(
        matches!(
            rejected,
            Err(Error::Protocol(ProtocolError::Rejected { command: "FQ" }))
        ),
        "{rejected:?}"
    );
    assert_eq!(radio.get_frequency(Band::A).await?.as_hz(), 145_190_000);
    assert_eq!(radio.get_frequency(Band::A).await?.as_hz(), 145_195_000);
    assert_eq!(radio.get_frequency(Band::A).await?.as_hz(), 145_200_000);
    assert_eq!(radio.transport.writes.len(), 4);
    assert_eq!(
        radio.settle_commands, 0,
        "settling ends after REJECTION_SETTLE_COMMANDS commands"
    );
    Ok(())
}

#[tokio::test]
async fn a_stale_line_of_another_command_is_skipped_within_one_reply() -> TestResult {
    let transport = TestTransport::replying([b"GP 1,0\rFQ 0,0145190000\r".to_vec()]);
    let mut radio = Radio::new(transport);
    let frequency = radio.get_frequency(Band::A).await?;
    assert_eq!(frequency.as_hz(), 145_190_000);
    assert_eq!(radio.transport.writes, [b"FQ 0\r".to_vec()]);
    Ok(())
}

#[tokio::test]
async fn leftover_bytes_from_an_earlier_connection_user_are_skipped() -> TestResult {
    let transport = TestTransport::replying([b"\r0145190000\rFQ 0,0145190000\r".to_vec()]);
    let mut radio = Radio::new(transport);
    let frequency = radio.get_frequency(Band::A).await?;
    assert_eq!(frequency.as_hz(), 145_190_000);
    let transport = TestTransport::replying([b"\r\r\r\r".to_vec()]);
    let mut radio = Radio::new(transport);
    let result = radio.get_frequency(Band::A).await;
    assert!(
        matches!(
            result,
            Err(Error::Protocol(ProtocolError::EmptyLine { .. }))
        ),
        "{result:?}"
    );
    Ok(())
}

#[tokio::test]
async fn a_reply_for_the_other_band_is_stale_and_the_command_times_out() -> TestResult {
    let transport = TestTransport::replying([b"MD 1,1\r".to_vec()]);
    let mut radio = Radio::new(transport);
    radio.set_timeout(Duration::from_millis(20));
    let result = radio.get_operating_mode(Band::A).await;
    assert!(
        matches!(
            result,
            Err(Error::Timeout {
                operation: "MD",
                millis: 20
            })
        ),
        "{result:?}"
    );
    Ok(())
}

#[tokio::test]
async fn too_many_stale_lines_fail_the_command() -> TestResult {
    let transport =
        TestTransport::replying([b"XX 1\rXX 2\rXX 3\rXX 4\rFQ 0,0145190000\r".to_vec()]);
    let mut radio = Radio::new(transport);
    let result = radio.get_frequency(Band::A).await;
    assert!(
        matches!(
            result,
            Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: "FQ",
                ..
            }))
        ),
        "{result:?}"
    );
    let retry = radio.get_frequency(Band::A).await;
    assert!(matches!(retry, Err(Error::Mcp(McpError::RecoveryRequired))));
    Ok(())
}

#[tokio::test]
async fn cat_and_binary_reads_share_one_lossless_receive_buffer() -> TestResult {
    let transport =
        TestTransport::new([b"0M\rW\x00\x00\x38\x04\xAA\xBB\xCC\xDD\x06GW 0\r".to_vec()]);
    let mut radio = Radio::new(transport);
    assert_eq!(radio.read_line("entry").await?, b"0M");
    assert_eq!(radio.read_exact(5, "header").await?, b"W\x00\x00\x38\x04");
    assert_eq!(radio.read_exact(4, "page").await?, b"\xAA\xBB\xCC\xDD");
    assert_eq!(radio.read_exact(1, "ACK").await?, b"\x06");
    assert_eq!(radio.read_line("CAT").await?, b"GW 0");
    assert_eq!(radio.into_transport().reads, 1);
    Ok(())
}

#[tokio::test]
async fn binary_read_retains_the_remainder_of_a_larger_transport_chunk() -> TestResult {
    let mut radio = Radio::new(TestTransport::new([b"\x01\x02\x03\x04\x05".to_vec()]));
    assert_eq!(radio.read_exact(2, "first").await?, [1, 2]);
    assert_eq!(radio.read_exact(3, "second").await?, [3, 4, 5]);
    assert_eq!(radio.into_transport().reads, 1);
    Ok(())
}

#[tokio::test]
async fn cancellation_retains_partial_cat_and_binary_input() -> TestResult {
    let mut radio = Radio::new(TestTransport::new([b"ID ".to_vec()]));
    let interrupted =
        tokio::time::timeout(Duration::from_millis(1), radio.read_line("partial CAT")).await;
    assert!(interrupted.is_err());
    radio.transport.chunks.push_back(b"TM-D750\r".to_vec());
    assert_eq!(radio.read_line("completed CAT").await?, b"ID TM-D750");

    radio.transport.chunks.push_back(vec![1, 2]);
    let interrupted = tokio::time::timeout(
        Duration::from_millis(1),
        radio.read_exact(4, "partial binary"),
    )
    .await;
    assert!(interrupted.is_err());
    radio.transport.chunks.push_back(vec![3, 4]);
    assert_eq!(radio.read_exact(4, "completed binary").await?, [1, 2, 3, 4]);
    Ok(())
}

#[tokio::test]
async fn cat_line_limit_accepts_the_boundary_and_rejects_longer_input() -> TestResult {
    let mut boundary = vec![b'A'; MAX_CAT_LINE_BYTES];
    boundary.push(b'\r');
    let mut radio = Radio::new(TestTransport::new([boundary]));
    assert_eq!(radio.read_line("boundary").await?.len(), MAX_CAT_LINE_BYTES);

    for terminated in [false, true] {
        let mut oversized = vec![b'A'; MAX_CAT_LINE_BYTES + 1];
        if terminated {
            oversized.push(b'\r');
        }
        let mut radio = Radio::new(TestTransport::new([oversized]));
        let result = radio.read_line("oversized").await;
        assert!(
            matches!(
                result,
                Err(Error::Protocol(ProtocolError::CatLineTooLong { limit }))
                    if limit == MAX_CAT_LINE_BYTES
            ),
            "oversized reply must fail by length, not timeout: {result:?}"
        );
    }
    Ok(())
}

#[tokio::test]
async fn hanging_write_times_out_and_blocks_further_cat_commands() -> TestResult {
    let mut transport = TestTransport::new([]);
    transport.write_behavior = WriteBehavior::Pending;
    let mut radio = Radio::new(transport);
    radio.set_timeout(Duration::from_millis(5));
    let result =
        tokio::time::timeout(Duration::from_secs(1), radio.get_operating_mode(Band::A)).await?;
    assert!(matches!(
        result,
        Err(Error::Timeout {
            operation: "serial write",
            millis: 5,
        })
    ));
    let retry = radio.get_operating_mode(Band::B).await;
    assert!(matches!(retry, Err(Error::Mcp(McpError::RecoveryRequired))));
    assert_eq!(radio.transport.writes, [b"MD 0\r".to_vec()]);
    assert_eq!(radio.transport.reads, 0);
    Ok(())
}

#[tokio::test]
async fn cancelled_write_retains_the_uncertain_protocol_state() {
    let mut transport = TestTransport::new([]);
    transport.write_behavior = WriteBehavior::Pending;
    let mut radio = Radio::new(transport);
    let result =
        tokio::time::timeout(Duration::from_millis(50), radio.get_operating_mode(Band::A)).await;
    assert!(result.is_err());
    let retry = radio.get_dv_gateway_mode().await;
    assert!(matches!(retry, Err(Error::Mcp(McpError::RecoveryRequired))));
    assert_eq!(radio.transport.writes, [b"MD 0\r".to_vec()]);
}

#[tokio::test]
async fn complete_rejection_replies_leave_the_cat_boundary_ready() -> TestResult {
    let transport = TestTransport::replying([b"N\r".to_vec(), b"GW 0\r".to_vec()]);
    let mut radio = Radio::new(transport);
    let rejected = radio.get_operating_mode(Band::A).await;
    assert!(
        matches!(
            rejected,
            Err(Error::Protocol(ProtocolError::NotAvailable {
                command: "MD"
            }))
        ),
        "{rejected:?}"
    );
    assert_eq!(radio.get_dv_gateway_mode().await?, DvGatewayMode::Off);
    assert_eq!(radio.transport.writes.len(), 2);

    let transport = TestTransport::replying([b"?\r".to_vec(), b"GW 0\r".to_vec()]);
    let mut radio = Radio::new(transport);
    let rejected = radio.get_operating_mode(Band::A).await;
    assert!(
        matches!(
            rejected,
            Err(Error::Protocol(ProtocolError::Rejected { command: "MD" }))
        ),
        "{rejected:?}"
    );
    assert_eq!(radio.get_dv_gateway_mode().await?, DvGatewayMode::Off);
    assert_eq!(radio.transport.writes.len(), 2);
    Ok(())
}

#[tokio::test]
async fn bluetooth_writes_wait_longer_than_the_ordinary_deadline() -> TestResult {
    let mut radio = Radio::new(TestTransport::new([]));
    radio.set_timeout(Duration::from_millis(10));
    assert_eq!(
        radio.reply_timeout(&Command::SetBluetooth { enabled: true }),
        BLUETOOTH_WRITE_TIMEOUT
    );
    assert_eq!(
        radio.reply_timeout(&Command::GetBluetooth),
        Duration::from_millis(10)
    );
    Ok(())
}
