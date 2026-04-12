#![cfg(all(feature = "examples-network", feature = "blocking"))]
//! Blocking-shell CLI: no tokio runtime, just `std::net::UdpSocket`.
//!
//! Demonstrates the [`dstar_gateway::blocking_shell::BlockingSession`]
//! entry point for consumers that don't want to drag in the tokio
//! runtime. The main loop is a regular `fn main()` — no
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
//! REFLECTOR_HOST=xrf030.example.com:30001 \
//!     cargo run -p dstar-gateway --example 09_blocking_cli \
//!     --features "examples-network blocking"
//! ```

use std::env;
use std::net::UdpSocket;
use std::time::{Duration, Instant};

use dstar_gateway::blocking_shell::BlockingSession;
use dstar_gateway_core::session::Driver;
use dstar_gateway_core::session::client::{
    ClientStateKind, Configured, Connected, DExtra, Session,
};
use dstar_gateway_core::types::{Callsign, Module};

// Acknowledged workspace dev-deps.
use pcap_parser as _;
use trybuild as _;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();

    let callsign = Callsign::try_from_str("W1AW")?;
    let reflector_host =
        env::var("REFLECTOR_HOST").unwrap_or_else(|_| "xrf030.example.com:30001".to_string());

    // 1. Resolve + bind a std UdpSocket. No tokio.
    let peer = reflector_host
        .to_socket_addrs_std()?
        .next()
        .ok_or("resolve failed")?;
    let sock = UdpSocket::bind("0.0.0.0:0")?;
    sock.set_read_timeout(Some(Duration::from_secs(2)))?;
    sock.connect(peer)?; // optional — lets send() drop the address

    // 2. Typestate handshake, synchronously.
    let session: Session<DExtra, Configured> = Session::<DExtra, Configured>::builder()
        .callsign(callsign)
        .local_module(Module::try_from_char('B')?)
        .reflector_module(Module::try_from_char('C')?)
        .peer(peer)
        .build();

    let now = Instant::now();
    let mut connecting = session
        .connect(now)
        .map_err(|f| format!("connect: {}", f.error))?;

    // Send LINK.
    if let Some(tx) = connecting.poll_transmit(now) {
        let _ = sock.send_to(tx.payload, tx.dst)?;
    }

    // Recv ACK.
    let mut buf = [0u8; 64];
    let (n, src) = sock.recv_from(&mut buf)?;
    connecting.handle_input(Instant::now(), src, &buf[..n])?;
    if connecting.state_kind() != ClientStateKind::Connected {
        eprintln!("handshake did not complete");
        return Ok(());
    }

    let connected: Session<DExtra, Connected> = connecting
        .promote()
        .map_err(|f| format!("promote: {}", f.error))?;

    // 3. Hand off to the blocking shell. The shell owns the socket
    //    from here on and drives the driver loop one step per
    //    `run_until_event` call.
    let mut shell = BlockingSession::new(connected, sock);

    // 4. Print events for 30 seconds, one step at a time. A real
    //    CLI would install a SIGINT handler here and break out of
    //    the loop when it fires. No-event steps are a quiet path —
    //    the shell's internal 100 ms idle wait already throttles
    //    CPU, so we simply loop back to the next call.
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline {
        if let Some(event) = shell.run_until_event()? {
            println!("event: {event:?}");
        }
    }
    Ok(())
}

/// Little helper so the example doesn't need to pull in `ToSocketAddrs`
/// via a trait import at the top of the file.
trait HostResolve {
    fn to_socket_addrs_std(&self) -> std::io::Result<std::vec::IntoIter<std::net::SocketAddr>>;
}

impl HostResolve for String {
    fn to_socket_addrs_std(&self) -> std::io::Result<std::vec::IntoIter<std::net::SocketAddr>> {
        use std::net::ToSocketAddrs;
        self.as_str().to_socket_addrs()
    }
}
