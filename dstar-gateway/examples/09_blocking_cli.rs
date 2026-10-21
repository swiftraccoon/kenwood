#![cfg(all(feature = "examples-network", feature = "blocking"))]
//! Blocking-shell CLI: no tokio runtime, just `std::net::UdpSocket`.
//!
//! Demonstrates the [`dstar_gateway::blocking_shell::BlockingSession`]
//! entry point for consumers that don't want to run a tokio runtime.
//! Tokio remains a dependency of the parent async crate. The main
//! loop is a regular `fn main()`: no
//! `#[tokio::main]`, no `async fn`, no channels.
//!
//! The blocking shell is caller-driven: each call to
//! [`BlockingSession::run_until_event`] drives one step of the sans-io
//! driver loop (drain outbound, arm read timeout, try to recv,
//! drain one event). The caller loops on that method and prints
//! each event until Ctrl-C.
//!
//! Gated behind `examples-network` AND the `blocking` feature:
//!
//! ```text
//! DSTAR_CALLSIGN=N0CALL REFLECTOR_HOST=xrf030.example.com:30001 \
//!     cargo run -p dstar-gateway --example 09_blocking_cli \
//!     --features "examples-network blocking"
//! ```

#[cfg(feature = "insecure-plaintext-xlx-directory")]
use reqwest as _;

use std::env;
use std::net::{ToSocketAddrs as _, UdpSocket};
use std::time::{Duration, Instant};

use dstar_gateway::blocking_shell::{BlockingSession, drive_connecting};
use dstar_gateway_core::session::client::{Configured, DExtra, Session};
use dstar_gateway_core::types::{Module, RoutingCallsign};

// Acknowledged workspace dev-deps. This example is deliberately
// not running tokio, so the async runtime and `tracing` facade are
// acknowledged rather than used.
use pcap_parser as _;
use thiserror as _;
use tokio as _;
use tracing as _;
use trybuild as _;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();

    let callsign = RoutingCallsign::new(&env::var("DSTAR_CALLSIGN")?)?;
    let reflector_host =
        env::var("REFLECTOR_HOST").unwrap_or_else(|_| "xrf030.example.com:30001".to_string());

    // 1. Resolve + bind a std UdpSocket. No tokio.
    let peer = reflector_host
        .to_socket_addrs()?
        .next()
        .ok_or("resolve failed")?;
    let sock = UdpSocket::bind("0.0.0.0:0")?;
    sock.set_read_timeout(Some(Duration::from_secs(2)))?;
    sock.connect(peer)?; // optional: lets send() drop the address

    // 2. Typestate handshake, synchronously.
    let session: Session<DExtra, Configured> = Session::<DExtra, Configured>::builder()
        .callsign(callsign)
        .local_module(Module::try_from_char('B')?)
        .reflector_module(Module::try_from_char('C')?)
        .peer(peer)
        .build();

    let connecting = session.connect(Instant::now())?;
    let connected = drive_connecting(connecting, &sock, Duration::from_secs(5))?;

    // 3. Hand off to the blocking shell. The shell owns the socket
    //    from here on and drives the driver loop one step per
    //    `run_until_event` call.
    let mut shell = BlockingSession::new(connected, sock)?;

    // 4. Print events for 30 seconds, one step at a time. A real
    //    CLI would install a SIGINT handler here and break out of
    //    the loop when it fires. No-event steps are a quiet path:
    //    the shell's internal 100 ms idle wait already throttles
    //    CPU, so we simply loop back to the next call.
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline && !shell.is_closed() {
        if let Some(event) = shell.run_until_event()? {
            println!("event: {event:?}");
        }
    }
    shell.disconnect()?;
    Ok(())
}
