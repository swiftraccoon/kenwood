//! Bounded recipient sends shared by same-protocol and transcoded voice.

use std::future::Future;
use std::net::SocketAddr;
use std::time::{Duration, Instant};

use dstar_gateway_core::session::client::Protocol;
use dstar_gateway_core::types::Module;
use tokio::net::UdpSocket;

use crate::client_pool::{ClientPool, UnhealthyOutcome};

/// Maximum wait for one UDP datagram to enter the kernel's send queue.
const SEND_TIMEOUT: Duration = Duration::from_secs(2);

/// Failure to enqueue a UDP datagram within the local send deadline.
#[derive(Debug, thiserror::Error)]
pub(crate) enum SendError {
    /// The kernel rejected the datagram.
    #[error("UDP send failed: {0}")]
    Io(#[source] std::io::Error),
    /// The send remained pending for two seconds.
    #[error("UDP send exceeded two seconds")]
    TimedOut,
}

/// One recipient whose datagram was not enqueued.
#[derive(Debug)]
pub(crate) struct SendFailure {
    /// Datagram destination.
    pub peer: SocketAddr,
    /// Kernel failure or local deadline expiration.
    pub error: SendError,
}

/// Completed recipient pass, retaining partial delivery and every send failure.
///
/// Successful enqueueing describes the local UDP operation, not remote receipt.
#[derive(Debug, Default)]
pub(crate) struct FanOutReport {
    /// Number of successfully enqueued datagrams.
    pub sent: usize,
    /// Recipients skipped because their shared voice budget was exhausted.
    pub rate_limited: usize,
    /// Failures in recipient iteration order.
    pub failures: Vec<SendFailure>,
    /// Peers reaching five consecutive send failures during this pass.
    pub evicted: Vec<SocketAddr>,
}

/// Enqueue one complete datagram, bounded by the local send deadline.
pub(crate) async fn send_datagram(
    socket: &UdpSocket,
    bytes: &[u8],
    peer: SocketAddr,
) -> Result<(), SendError> {
    send_with_deadline(socket.send_to(bytes, peer)).await
}

async fn send_with_deadline(
    send: impl Future<Output = std::io::Result<usize>>,
) -> Result<(), SendError> {
    match tokio::time::timeout(SEND_TIMEOUT, send).await {
        Ok(Ok(_)) => Ok(()),
        Ok(Err(error)) => Err(SendError::Io(error)),
        Err(_) => Err(SendError::TimedOut),
    }
}

/// Send to other module members, charging the same recipient bucket on every path.
///
/// Failures do not stop other recipients. Cancellation can leave a prefix sent;
/// the driver awaits a complete pass before processing its next datagram.
pub(crate) async fn fan_out_voice<P: Protocol>(
    socket: &UdpSocket,
    clients: &ClientPool<P>,
    from: SocketAddr,
    module: Module,
    bytes: &[u8],
) -> FanOutReport {
    fan_out_voice_at(socket, clients, from, module, bytes, Instant::now()).await
}

/// Recipient pass with injected refill time for deterministic budget tests.
pub(crate) async fn fan_out_voice_at<P: Protocol>(
    socket: &UdpSocket,
    clients: &ClientPool<P>,
    from: SocketAddr,
    module: Module,
    bytes: &[u8],
    now: Instant,
) -> FanOutReport {
    let mut report = FanOutReport::default();
    for peer in clients.members_of_module(module).await {
        if peer == from {
            continue;
        }
        if !clients.try_consume_tx_token(&peer, now).await {
            report.rate_limited += 1;
            continue;
        }
        match send_datagram(socket, bytes, peer).await {
            Ok(()) => {
                clients.record_send_success(&peer).await;
                report.sent += 1;
            }
            Err(error) => {
                if matches!(
                    clients.mark_unhealthy(&peer).await,
                    UnhealthyOutcome::ShouldEvict { .. }
                ) {
                    report.evicted.push(peer);
                }
                report.failures.push(SendFailure { peer, error });
            }
        }
    }
    report
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client_pool::{ClientHandle, FrameRate, TokenBucket};
    use crate::reflector::AccessPolicy;
    use dstar_gateway_core::session::client::DExtra;
    use std::net::{IpAddr, Ipv6Addr};

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    async fn add(pool: &ClientPool<DExtra>, peer: SocketAddr, now: Instant, capacity: u32) {
        const RATE: FrameRate = match FrameRate::new(1.0) {
            Ok(rate) => rate,
            Err(_) => unreachable!(),
        };
        let mut handle = ClientHandle::new(peer, Module::C, AccessPolicy::ReadWrite, now)
            .with_tx_budget(TokenBucket::new(capacity, RATE, now));
        handle.module = Some(Module::C);
        let _previous = pool.insert(handle).await;
    }

    #[tokio::test]
    async fn shared_budget_limits_delivery_and_refills() -> TestResult {
        let pool = ClientPool::<DExtra>::new();
        let sender = UdpSocket::bind("127.0.0.1:0").await?;
        let receiver = UdpSocket::bind("127.0.0.1:0").await?;
        let now = Instant::now();
        let peer = receiver.local_addr()?;
        add(&pool, peer, now, 1).await;
        let report =
            fan_out_voice_at(&sender, &pool, sender.local_addr()?, Module::C, b"one", now).await;
        assert_eq!(report.sent, 1);
        let mut bytes = [0; 16];
        let length =
            tokio::time::timeout(Duration::from_secs(1), receiver.recv(&mut bytes)).await??;
        assert_eq!(bytes.get(..length), Some(b"one".as_slice()));
        let report =
            fan_out_voice_at(&sender, &pool, sender.local_addr()?, Module::C, b"two", now).await;
        assert_eq!(report.rate_limited, 1);
        assert_eq!(report.sent, 0);
        assert!(report.failures.is_empty());
        assert!(
            tokio::time::timeout(Duration::from_millis(20), receiver.recv(&mut bytes))
                .await
                .is_err()
        );
        let report = fan_out_voice_at(
            &sender,
            &pool,
            sender.local_addr()?,
            Module::C,
            b"three",
            now + Duration::from_secs(1),
        )
        .await;
        assert_eq!(report.sent, 1);
        let length =
            tokio::time::timeout(Duration::from_secs(1), receiver.recv(&mut bytes)).await??;
        assert_eq!(bytes.get(..length), Some(b"three".as_slice()));
        Ok(())
    }

    #[tokio::test]
    async fn failed_recipient_preserves_delivery_to_others_and_reports_eviction() -> TestResult {
        let pool = ClientPool::<DExtra>::new();
        let sender = UdpSocket::bind("127.0.0.1:0").await?;
        let receiver = UdpSocket::bind("127.0.0.1:0").await?;
        let good = receiver.local_addr()?;
        let bad = SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), good.port());
        let now = Instant::now();
        add(&pool, good, now, 10).await;
        add(&pool, bad, now, 10).await;
        for pass in 1..=5 {
            let report = fan_out_voice_at(
                &sender,
                &pool,
                sender.local_addr()?,
                Module::C,
                b"voice",
                now,
            )
            .await;
            assert_eq!(
                report.sent, 1,
                "healthy recipient still receives pass {pass}"
            );
            assert_eq!(report.failures.len(), 1);
            assert_eq!(
                report.failures.first().map(|failure| failure.peer),
                Some(bad)
            );
            assert_eq!(report.evicted.contains(&bad), pass == 5);
            let mut bytes = [0; 16];
            let length =
                tokio::time::timeout(Duration::from_secs(1), receiver.recv(&mut bytes)).await??;
            assert_eq!(bytes.get(..length), Some(b"voice".as_slice()));
        }
        Ok(())
    }

    #[tokio::test]
    async fn success_resets_consecutive_failure_streak() -> TestResult {
        let pool = ClientPool::<DExtra>::new();
        let sender = UdpSocket::bind("127.0.0.1:0").await?;
        let receiver = UdpSocket::bind("127.0.0.1:0").await?;
        let peer = receiver.local_addr()?;
        let now = Instant::now();
        add(&pool, peer, now, 2).await;
        for _ in 0..4 {
            let _outcome = pool.mark_unhealthy(&peer).await;
        }
        let report = fan_out_voice_at(
            &sender,
            &pool,
            sender.local_addr()?,
            Module::C,
            b"voice",
            now,
        )
        .await;
        assert_eq!(report.sent, 1);
        assert_eq!(
            pool.mark_unhealthy(&peer).await,
            UnhealthyOutcome::StillHealthy { failure_count: 1 }
        );
        Ok(())
    }

    #[tokio::test(start_paused = true)]
    async fn a_pending_udp_send_expires_at_the_local_deadline() {
        let start = tokio::time::Instant::now();
        let result = tokio::time::timeout(
            Duration::from_secs(3),
            send_with_deadline(std::future::pending()),
        )
        .await;
        assert!(matches!(result, Ok(Err(SendError::TimedOut))), "{result:?}");
        assert_eq!(
            start.elapsed(),
            SEND_TIMEOUT,
            "send timeout is total rather than renewed"
        );
    }
}
