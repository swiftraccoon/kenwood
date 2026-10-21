//! Loopback authentication resource and deadline regressions.

use super::*;
use tokio::net::TcpListener;

type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;
const CALLSIGN: Callsign = Callsign::from_wire_bytes(*b"W1AW    ");

#[test]
fn defaults_share_nonzero_phase_budgets_and_reference_packet() {
    assert_eq!(AuthClient::default(), AuthClient::new());
    assert_eq!(AuthClient::new().connect_timeout, Duration::from_secs(10));
    assert_eq!(AuthClient::new().read_timeout, Duration::from_secs(5));
    let packet = build_auth_packet(CALLSIGN);
    assert_eq!(packet.len(), 56);
    assert_eq!(packet.get(..4), Some([0x38, 0xC0, 0x01, 0].as_slice()));
    assert_eq!(packet.get(4..12), Some(b"W1AW    ".as_slice()));
    assert_eq!(packet.get(12..20), Some(b"DV019999".as_slice()));
    assert_eq!(packet.get(28..33), Some(b"W7IB2".as_slice()));
    assert_eq!(packet.get(40..47), Some(b"DHS0257".as_slice()));
    assert!(packet.get(20..28).is_some_and(|bytes| bytes == [b' '; 8]));
}

#[tokio::test]
async fn default_client_accepts_a_complete_loopback_response() -> TestResult {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await?;
        let mut request = [0_u8; 56];
        let _count = socket.read_exact(&mut request).await?;
        assert_eq!(request, build_auth_packet(CALLSIGN));
        socket.write_all(&[8, 0xC0, 1, 0, 0, 0, 0, 0]).await?;
        Ok::<_, std::io::Error>(())
    });
    let hosts = AuthClient::default()
        .with_endpoint(address)
        .authenticate(CALLSIGN)
        .await?;
    assert!(hosts.is_empty());
    server.await??;
    Ok(())
}

#[tokio::test]
async fn continuous_partial_data_cannot_renew_the_response_deadline() -> TestResult {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await?;
        let mut request = [0_u8; 56];
        let _count = socket.read_exact(&mut request).await?;
        for _ in 0..12 {
            if socket.write_all(&[0]).await.is_err() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        Ok::<_, std::io::Error>(())
    });
    let result = AuthClient::new()
        .with_endpoint(address)
        .with_read_timeout(Duration::from_millis(100))
        .with_idle_timeout(Duration::from_secs(1))
        .authenticate(CALLSIGN)
        .await;
    assert!(
        matches!(
            result,
            Err(AuthError::Timeout {
                phase: AuthPhase::Read,
                ..
            })
        ),
        "continuous data must hit the total response deadline: {result:?}"
    );
    server.await??;
    Ok(())
}

#[tokio::test]
async fn oversized_response_is_rejected_before_parsing_or_retaining_excess() -> TestResult {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await?;
        let mut request = [0_u8; 56];
        let _count = socket.read_exact(&mut request).await?;
        socket.write_all(&[0; 65]).await?;
        Ok::<_, std::io::Error>(())
    });
    let result = AuthClient::new()
        .with_endpoint(address)
        .with_response_limit(NonZeroUsize::new(64).ok_or("zero response limit")?)
        .authenticate(CALLSIGN)
        .await;
    assert!(
        matches!(result, Err(AuthError::ResponseTooLarge { limit, observed_at_least })
            if limit.get() == 64 && observed_at_least >= 65),
        "oversized input must not reach the parser: {result:?}"
    );
    server.await??;
    Ok(())
}

#[tokio::test]
async fn unrepresentable_configuration_fails_before_any_connection() -> TestResult {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let result = AuthClient::new()
        .with_endpoint(address)
        .with_read_timeout(Duration::MAX)
        .authenticate(CALLSIGN)
        .await;
    assert!(
        matches!(
            result,
            Err(AuthError::DeadlineOverflow {
                phase: AuthPhase::Read,
                ..
            })
        ),
        "invalid configuration must be rejected before connecting: {result:?}"
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(20), listener.accept())
            .await
            .is_err(),
        "invalid configuration must not open a TCP connection"
    );
    Ok(())
}
