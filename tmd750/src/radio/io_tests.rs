//! Deterministic tests of reply buffering, write deadlines, and cancellation.

use super::*;

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[derive(Debug, Clone, Copy)]
enum WriteBehavior {
    Complete,
    Pending,
}

#[derive(Debug)]
struct TestTransport {
    chunks: VecDeque<Vec<u8>>,
    writes: Vec<Vec<u8>>,
    reads: usize,
    write_behavior: WriteBehavior,
}

impl TestTransport {
    fn new(chunks: impl IntoIterator<Item = Vec<u8>>) -> Self {
        Self {
            chunks: chunks.into_iter().collect(),
            writes: Vec::new(),
            reads: 0,
            write_behavior: WriteBehavior::Complete,
        }
    }
}

impl Transport for TestTransport {
    async fn write(&mut self, bytes: &[u8]) -> Result<(), TransportError> {
        self.writes.push(bytes.to_vec());
        match self.write_behavior {
            WriteBehavior::Complete => Ok(()),
            WriteBehavior::Pending => std::future::pending().await,
        }
    }

    async fn read(&mut self, buffer: &mut [u8]) -> Result<usize, TransportError> {
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
        }
        Ok(count)
    }

    async fn close(&mut self) -> Result<(), TransportError> {
        Ok(())
    }
}

#[tokio::test]
async fn coalesced_cat_lines_survive_subsequent_commands() -> TestResult {
    let transport = TestTransport::new([b"ID TM-D750\rFV 1.02\rTY K,2,1\r".to_vec()]);
    let mut radio = Radio::new(transport);
    let identity = radio.identify().await?;
    assert_eq!(identity.firmware.as_str(), "1.02");
    assert_eq!(identity.radio_type.as_str(), "K,2,1");
    let transport = radio.into_transport();
    assert_eq!(
        transport.reads, 1,
        "later lines must come from buffered input"
    );
    assert_eq!(
        transport.writes,
        [b"ID\r".to_vec(), b"FV\r".to_vec(), b"TY\r".to_vec()]
    );
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
        tokio::time::timeout(Duration::from_millis(1), radio.get_operating_mode(Band::A)).await;
    assert!(result.is_err());
    let retry = radio.get_dv_gateway_mode().await;
    assert!(matches!(retry, Err(Error::Mcp(McpError::RecoveryRequired))));
    assert_eq!(radio.transport.writes, [b"MD 0\r".to_vec()]);
}

#[tokio::test]
async fn complete_rejection_replies_leave_the_cat_boundary_ready() -> TestResult {
    for rejection in [b'N', b'?'] {
        let transport = TestTransport::new([vec![rejection, b'\r', b'G', b'W', b' ', b'0', b'\r']]);
        let mut radio = Radio::new(transport);
        let rejected = radio.get_operating_mode(Band::A).await;
        assert!(matches!(
            rejected,
            Err(Error::Protocol(ProtocolError::UnexpectedResponse { .. }))
        ));
        assert_eq!(radio.get_dv_gateway_mode().await?, DvGatewayMode::Off);
        assert_eq!(radio.transport.writes.len(), 2);
    }
    Ok(())
}
