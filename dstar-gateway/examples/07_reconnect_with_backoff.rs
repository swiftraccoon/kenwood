#![cfg(feature = "examples-network")]
//! Reconnect-on-failure pattern with capped exponential backoff.
//!
//! Wraps the `DExtra` connect flow in a retry loop that doubles the
//! wait on each failure (1 s, 2 s, 4 s, 8 s, ..., capped at 60 s).
//! After a successful connect the session runs for 30 seconds, then
//! disconnects cleanly and starts the cycle over. Intended to
//! illustrate the idiomatic way to recover from a dropped session
//! using the `Failed<S, E>` return type surfaced by the typestate
//! transitions.
//!
//! Gated behind the `examples-network` feature.
//!
//! ```text
//! REFLECTOR_HOST=xrf030.example.com:30001 \
//!     cargo run -p dstar-gateway --example 07_reconnect_with_backoff \
//!     --features examples-network
//! ```

use std::env;
use std::sync::Arc;
use std::time::{Duration, Instant};

use dstar_gateway::tokio_shell::AsyncSession;
use dstar_gateway_core::session::Driver;
use dstar_gateway_core::session::client::{
    ClientStateKind, Configured, Connected, DExtra, Session,
};
use dstar_gateway_core::types::{Callsign, Module};
use tokio::net::UdpSocket;
use tokio::time::timeout;

// Acknowledged workspace dev-deps.
use pcap_parser as _;
use trybuild as _;

/// Max session lifetime before this example voluntarily disconnects
/// and reconnects. In a real deployment this would be unbounded and
/// reconnect would only fire on failure.
const SESSION_LIFETIME: Duration = Duration::from_secs(30);

/// Starting wait on failure.
const INITIAL_BACKOFF: Duration = Duration::from_secs(1);

/// Maximum wait between reconnect attempts.
const MAX_BACKOFF: Duration = Duration::from_secs(60);

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();

    let callsign = Callsign::try_from_str("W1AW")?;
    let reflector_host =
        env::var("REFLECTOR_HOST").unwrap_or_else(|_| "xrf030.example.com:30001".to_string());

    let mut backoff = INITIAL_BACKOFF;

    // Outer loop runs forever (Ctrl-C to stop).
    loop {
        match connect_once(callsign, &reflector_host).await {
            Ok(mut async_session) => {
                tracing::info!("connected, running for {:?}", SESSION_LIFETIME);
                backoff = INITIAL_BACKOFF; // reset on success

                let deadline = tokio::time::Instant::now() + SESSION_LIFETIME;
                loop {
                    tokio::select! {
                        () = tokio::time::sleep_until(deadline) => break,
                        ev = async_session.next_event() => {
                            let Some(event) = ev else {
                                tracing::warn!("event stream closed — will reconnect");
                                break;
                            };
                            tracing::debug!(?event, "event");
                        }
                    }
                }

                // Graceful disconnect — shell sends UNLINK and waits
                // for the ACK. On error we fall through to backoff.
                if let Err(e) = async_session.disconnect().await {
                    tracing::warn!(?e, "disconnect failed");
                }
            }
            Err(e) => {
                tracing::warn!(?e, "connect attempt failed; backing off {:?}", backoff);
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(MAX_BACKOFF);
            }
        }
    }
}

/// Single connect attempt. Returns the spawned `AsyncSession` on
/// success, a boxed error on any failure (resolve, bind, handshake,
/// typestate promote).
async fn connect_once(
    callsign: Callsign,
    reflector_host: &str,
) -> Result<AsyncSession<DExtra>, Box<dyn std::error::Error>> {
    let peer = tokio::net::lookup_host(reflector_host)
        .await?
        .next()
        .ok_or("resolve failed")?;
    let sock = Arc::new(UdpSocket::bind("0.0.0.0:0").await?);

    let session: Session<DExtra, Configured> = Session::<DExtra, Configured>::builder()
        .callsign(callsign)
        .local_module(Module::try_from_char('B')?)
        .reflector_module(Module::try_from_char('C')?)
        .peer(peer)
        .build();

    // `Failed<Session, Error>` lets the caller retry with the
    // original session; for this example we just rebuild from
    // scratch on failure, so we flatten `Failed` into the boxed
    // error string.
    let now = Instant::now();
    let mut connecting = session
        .connect(now)
        .map_err(|f| format!("connect: {}", f.error))?;

    if let Some(tx) = connecting.poll_transmit(now) {
        let _ = sock.send_to(tx.payload, tx.dst).await?;
    }
    let mut buf = [0u8; 64];
    let (n, src) = timeout(Duration::from_secs(5), sock.recv_from(&mut buf))
        .await
        .map_err(|_| "handshake timeout")??;
    connecting.handle_input(Instant::now(), src, &buf[..n])?;

    if connecting.state_kind() != ClientStateKind::Connected {
        return Err("handshake did not complete".into());
    }

    let connected: Session<DExtra, Connected> = connecting
        .promote()
        .map_err(|f| format!("promote: {}", f.error))?;
    Ok(AsyncSession::spawn(connected, sock))
}
