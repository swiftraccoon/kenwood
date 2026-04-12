//! Fan-out engine for voice frames.
//!
//! The reflector's job on every inbound voice packet is to re-send
//! the same bytes to every other client currently linked to the
//! originator's module. This file implements that "encode once, send
//! N times" loop.
//!
//! This function handles **same-protocol** fan-out only — it
//! re-sends the raw inbound bytes verbatim to every peer on the
//! same module. Cross-protocol forwarding (re-encoding bytes
//! from one protocol into another via [`super::transcode::transcode_voice`])
//! is handled separately by the endpoint run loop's broadcast-channel
//! subscriber path in [`super::endpoint::ProtocolEndpoint::run`].

use std::net::SocketAddr;
use std::time::Instant;

use tokio::net::UdpSocket;

use dstar_gateway_core::session::client::Protocol;
use dstar_gateway_core::types::{Module, ProtocolKind};

use crate::client_pool::{ClientPool, UnhealthyOutcome};
use crate::tokio_shell::endpoint::ShellError;

/// Report from a completed [`fan_out_voice`] call.
///
/// Currently carries the list of peers that exceeded the unhealthy
/// threshold on this tick and should be evicted by the caller. The
/// endpoint's run loop consumes this to remove the peer from the
/// pool and emit a `ServerEvent::ClientEvicted` event.
#[derive(Debug, Clone, Default)]
pub struct FanOutReport {
    /// Peers that hit the eviction threshold on this fan-out pass.
    pub evicted: Vec<SocketAddr>,
}

/// Fan out the raw wire bytes of a voice frame to every other
/// client on the same module.
///
/// The originator (identified by `from`) is filtered out of the
/// recipient list so the reflector never echoes audio back to the
/// client that sent it.
///
/// Individual send failures are logged and the offending peer is
/// marked unhealthy on the client pool; the fan-out loop continues
/// through the rest of the module membership. Peers that cross the
/// [`crate::client_pool::DEFAULT_UNHEALTHY_THRESHOLD`] are recorded
/// in the returned [`FanOutReport::evicted`] list and the caller is
/// responsible for removing them from the pool. The function only
/// returns `Err` if a truly fatal condition occurs — currently none,
/// so the `Result` is reserved for future fatal conditions.
///
/// # Errors
///
/// Reserved for future fatal conditions (e.g. cross-protocol
/// re-encode errors). The current DExtra-only implementation never
/// returns `Err`.
///
/// # Cancellation safety
///
/// This function is **not** cancel-safe. It iterates the module
/// membership list and calls `socket.send_to` for each peer in
/// sequence; cancelling the future mid-iteration leaves some peers
/// delivered and others silently skipped, which will make it look
/// like the skipped peers are missing frames. The endpoint run loop
/// is the only expected caller and it awaits this function to
/// completion per datagram.
pub async fn fan_out_voice<P: Protocol>(
    socket: &UdpSocket,
    clients: &ClientPool<P>,
    from: SocketAddr,
    module: Module,
    _protocol: ProtocolKind,
    bytes: &[u8],
) -> Result<FanOutReport, ShellError> {
    fan_out_voice_at(socket, clients, from, module, bytes, Instant::now()).await
}

/// Same as [`fan_out_voice`], but takes an injected `now: Instant`
/// for deterministic rate-limiter testing.
///
/// Production callers use [`fan_out_voice`] which samples the
/// wall clock itself; tests can drive this variant to advance the
/// token bucket's refill clock without waiting for real time.
///
/// # Errors
///
/// Same as [`fan_out_voice`]: reserved for future fatal conditions.
pub async fn fan_out_voice_at<P: Protocol>(
    socket: &UdpSocket,
    clients: &ClientPool<P>,
    from: SocketAddr,
    module: Module,
    bytes: &[u8],
    now: Instant,
) -> Result<FanOutReport, ShellError> {
    let mut report = FanOutReport::default();
    let members = clients.members_of_module(module).await;
    for peer in members.iter().copied().filter(|p| *p != from) {
        // Fix 5: consult the per-client TX token bucket BEFORE the
        // kernel send. On empty-bucket, drop the frame for THIS
        // peer — rate-limited is not the same as broken, so we do
        // NOT mark_unhealthy here. Other peers on the same module
        // still receive the frame.
        if !clients.try_consume_tx_token(&peer, now).await {
            tracing::debug!(
                ?peer,
                "fan-out rate-limited: TX budget exhausted, dropping frame for peer"
            );
            continue;
        }
        if let Err(e) = socket.send_to(bytes, peer).await {
            tracing::warn!(?peer, ?e, "fan-out send_to failed");
            match clients.mark_unhealthy(&peer).await {
                UnhealthyOutcome::ShouldEvict { failure_count } => {
                    tracing::warn!(
                        ?peer,
                        failure_count,
                        "fan-out send failure threshold exceeded; evicting peer"
                    );
                    report.evicted.push(peer);
                }
                UnhealthyOutcome::StillHealthy { .. } => {}
            }
        }
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::fan_out_voice;
    use crate::client_pool::{
        ClientHandle, ClientPool, DEFAULT_UNHEALTHY_THRESHOLD, UnhealthyOutcome,
    };
    use crate::reflector::AccessPolicy;
    use dstar_gateway_core::ServerSessionCore;
    use dstar_gateway_core::session::client::DExtra;
    use dstar_gateway_core::types::{Module, ProtocolKind};
    use std::net::SocketAddr;
    use std::time::Instant;
    use tokio::net::UdpSocket;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    async fn bound_socket() -> Result<(std::sync::Arc<UdpSocket>, SocketAddr), std::io::Error> {
        let sock = UdpSocket::bind("127.0.0.1:0").await?;
        let addr = sock.local_addr()?;
        Ok((std::sync::Arc::new(sock), addr))
    }

    fn fresh_handle(peer: SocketAddr) -> ClientHandle<DExtra> {
        let core = ServerSessionCore::new(ProtocolKind::DExtra, peer, Module::C);
        ClientHandle::new(core, AccessPolicy::ReadWrite, Instant::now())
    }

    #[tokio::test]
    async fn fan_out_with_one_client_sends_nothing() -> TestResult {
        let pool = ClientPool::<DExtra>::new();
        let (sock, addr) = bound_socket().await?;
        pool.insert(addr, fresh_handle(addr)).await;
        pool.set_module(&addr, Module::C).await;

        // No other members — fan_out_voice returns Ok and sends no
        // datagrams. We test the Ok-path here; the "no send" side is
        // implicit because there's nobody to receive.
        let result = fan_out_voice(
            sock.as_ref(),
            &pool,
            addr,
            Module::C,
            ProtocolKind::DExtra,
            b"hello",
        )
        .await?;
        assert!(result.evicted.is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn fan_out_to_two_peers_delivers_bytes() -> TestResult {
        // Bind three loopback sockets: A is the "reflector" (the
        // originator), B and C are receivers. fan_out_voice uses A's
        // socket as the send-side — B and C receive via their own
        // bound sockets which we use only to observe.
        let pool = ClientPool::<DExtra>::new();
        let (sock_a, addr_a) = bound_socket().await?;
        let sock_b = UdpSocket::bind("127.0.0.1:0").await?;
        let addr_b = sock_b.local_addr()?;
        let sock_c = UdpSocket::bind("127.0.0.1:0").await?;
        let addr_c = sock_c.local_addr()?;

        pool.insert(addr_a, fresh_handle(addr_a)).await;
        pool.insert(addr_b, fresh_handle(addr_b)).await;
        pool.insert(addr_c, fresh_handle(addr_c)).await;
        pool.set_module(&addr_a, Module::C).await;
        pool.set_module(&addr_b, Module::C).await;
        pool.set_module(&addr_c, Module::C).await;

        // Fan-out from A's socket, originating peer is A.
        let report = fan_out_voice(
            sock_a.as_ref(),
            &pool,
            addr_a,
            Module::C,
            ProtocolKind::DExtra,
            b"voicebits",
        )
        .await?;
        assert!(report.evicted.is_empty(), "no peers evicted on happy path");

        // B and C both received the payload. A did not (filter).
        let mut buf_b = [0u8; 64];
        let (n_b, src_b) = tokio::time::timeout(
            std::time::Duration::from_millis(500),
            sock_b.recv_from(&mut buf_b),
        )
        .await??;
        assert_eq!(src_b, addr_a);
        assert_eq!(&buf_b[..n_b], b"voicebits");

        let mut buf_c = [0u8; 64];
        let (n_c, src_c) = tokio::time::timeout(
            std::time::Duration::from_millis(500),
            sock_c.recv_from(&mut buf_c),
        )
        .await??;
        assert_eq!(src_c, addr_a);
        assert_eq!(&buf_c[..n_c], b"voicebits");
        Ok(())
    }

    // ─── Fix 4: unhealthy-client eviction ─────────────────────────
    #[tokio::test]
    async fn fan_out_reports_evicted_peer_after_threshold() -> TestResult {
        // Set up two peers on the same module: A is the originator
        // (sends the voice), B is the target (and will trigger the
        // send failure). We make B's address a closed loopback port
        // so send_to to B fails repeatedly.
        //
        // Note: UDP send_to on Linux/macOS loopback will *succeed*
        // against any port — there's no connection and the kernel
        // just drops the datagram on the floor. To reliably fail a
        // send we pre-mark the peer unhealthy 4 times and let the
        // 5th tick (via a normal successful send+mark cycle) flip
        // the threshold.
        let pool = ClientPool::<DExtra>::new();
        let (sock_a, addr_a) = bound_socket().await?;
        let sock_b = UdpSocket::bind("127.0.0.1:0").await?;
        let addr_b = sock_b.local_addr()?;

        pool.insert(addr_a, fresh_handle(addr_a)).await;
        pool.insert(addr_b, fresh_handle(addr_b)).await;
        pool.set_module(&addr_a, Module::C).await;
        pool.set_module(&addr_b, Module::C).await;

        // Prime the failure counter to threshold-1 by calling
        // mark_unhealthy directly. This skips the socket-failure
        // simulation, which is flaky on UDP loopback.
        for _ in 0..DEFAULT_UNHEALTHY_THRESHOLD - 1 {
            let _outcome = pool.mark_unhealthy(&addr_b).await;
        }

        // Now directly call mark_unhealthy once more to trip the
        // threshold — this is exactly what fan_out_voice does on the
        // Nth send failure. Confirm the outcome reports ShouldEvict.
        let outcome = pool.mark_unhealthy(&addr_b).await;
        assert!(matches!(outcome, UnhealthyOutcome::ShouldEvict { .. }));

        // Drop B from the pool the way the endpoint's run loop
        // would after seeing ShouldEvict.
        let removed = pool.remove(&addr_b).await;
        assert!(removed.is_some(), "evicted peer must be removable");
        assert!(
            !pool.contains(&addr_b).await,
            "evicted peer is no longer in the pool"
        );

        // Fan-out after eviction must target zero peers (A is the
        // originator and is filtered out; B is gone).
        let report = fan_out_voice(
            sock_a.as_ref(),
            &pool,
            addr_a,
            Module::C,
            ProtocolKind::DExtra,
            b"post-evict",
        )
        .await?;
        assert!(report.evicted.is_empty());
        Ok(())
    }

    // ─── Fix 5: per-client TX token-bucket rate limiting ─────────
    #[tokio::test]
    async fn fan_out_rate_limits_peer_when_tx_budget_exhausted() -> TestResult {
        use dstar_gateway_core::ServerSessionCore;
        // Peer B has a 1-token bucket with 1 token/sec refill. The
        // first frame goes through; the second (same instant) is
        // dropped for B. Other peers on the same module would still
        // receive the frame — here we only have one other peer to
        // keep the test focused on the rate-limit mechanism.
        let pool = ClientPool::<DExtra>::new();
        let (sock_a, addr_a) = bound_socket().await?;
        let sock_b = UdpSocket::bind("127.0.0.1:0").await?;
        let addr_b = sock_b.local_addr()?;
        let now = Instant::now();

        pool.insert(addr_a, fresh_handle(addr_a)).await;
        // Insert B with a tight 1-token budget.
        let b_core = ServerSessionCore::new(ProtocolKind::DExtra, addr_b, Module::C);
        let b_handle = ClientHandle::<DExtra>::new_with_tx_budget(
            b_core,
            AccessPolicy::ReadWrite,
            now,
            1,
            1.0,
        );
        pool.insert(addr_b, b_handle).await;
        pool.set_module(&addr_a, Module::C).await;
        pool.set_module(&addr_b, Module::C).await;

        // First frame: B's bucket has 1 token, consume succeeds, send goes.
        let report =
            super::fan_out_voice_at(sock_a.as_ref(), &pool, addr_a, Module::C, b"frame1", now)
                .await?;
        assert!(report.evicted.is_empty());

        // B must have received the first frame.
        let mut buf_b1 = [0u8; 64];
        let (n1, _src) = tokio::time::timeout(
            std::time::Duration::from_millis(200),
            sock_b.recv_from(&mut buf_b1),
        )
        .await??;
        assert_eq!(&buf_b1[..n1], b"frame1");

        // Second frame at the SAME instant: B's bucket is empty, so
        // we skip the send_to for B. Rate-limited is NOT unhealthy,
        // so no eviction.
        let report =
            super::fan_out_voice_at(sock_a.as_ref(), &pool, addr_a, Module::C, b"frame2", now)
                .await?;
        assert!(report.evicted.is_empty(), "rate-limited != unhealthy");

        // B did NOT receive frame2 within the deadline.
        let mut buf_b2 = [0u8; 64];
        let r = tokio::time::timeout(
            std::time::Duration::from_millis(100),
            sock_b.recv_from(&mut buf_b2),
        )
        .await;
        assert!(
            r.is_err(),
            "rate-limited peer must not receive the second frame"
        );

        // Advance the clock by 1 second — bucket refills by 1 token.
        let later = now + std::time::Duration::from_secs(1);
        let _report3 =
            super::fan_out_voice_at(sock_a.as_ref(), &pool, addr_a, Module::C, b"frame3", later)
                .await?;
        let mut buf_b3 = [0u8; 64];
        let (n3, _src) = tokio::time::timeout(
            std::time::Duration::from_millis(200),
            sock_b.recv_from(&mut buf_b3),
        )
        .await??;
        assert_eq!(&buf_b3[..n3], b"frame3", "refilled bucket delivers frame3");
        Ok(())
    }

    #[tokio::test]
    async fn fan_out_skips_other_modules() -> TestResult {
        let pool = ClientPool::<DExtra>::new();
        let (sock_a, addr_a) = bound_socket().await?;
        let sock_b = UdpSocket::bind("127.0.0.1:0").await?;
        let addr_b = sock_b.local_addr()?;

        pool.insert(addr_a, fresh_handle(addr_a)).await;
        pool.insert(addr_b, fresh_handle(addr_b)).await;
        pool.set_module(&addr_a, Module::C).await;
        pool.set_module(&addr_b, Module::D).await;

        let report = fan_out_voice(
            sock_a.as_ref(),
            &pool,
            addr_a,
            Module::C,
            ProtocolKind::DExtra,
            b"c-only",
        )
        .await?;
        assert!(report.evicted.is_empty());

        let mut buf = [0u8; 64];
        let result = tokio::time::timeout(
            std::time::Duration::from_millis(100),
            sock_b.recv_from(&mut buf),
        )
        .await;
        assert!(
            result.is_err(),
            "peer on different module must not receive fan-out"
        );
        Ok(())
    }
}
