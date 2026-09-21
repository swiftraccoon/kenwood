//! Reflector setup: DNS on a joined worker, then a bounded authentication and
//! handshake.
//!
//! Host-file and DNS lookups run off the radio's current-thread runtime, and
//! their worker is always joined, including after cancellation or deadline
//! expiry. No session task is spawned unless setup finished inside its original
//! deadline and the caller is still running.

use std::future::Future;
use std::net::{SocketAddr, ToSocketAddrs};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use dstar_gateway::auth::AuthClient;
use dstar_gateway::tokio_shell::{AnyAsyncSession, AsyncSession, ShellError};
use dstar_gateway_core::session::client::{
    Connected, Connecting, DExtra, DPlus, Dcs, Protocol, Session,
};
use dstar_gateway_core::{Callsign, ProtocolKind};
use tokio::net::UdpSocket;
use tokio::time::Instant as Deadline;

use crate::{hosts, output};

use super::LinkArg;

#[cfg(test)]
mod tests;

const SETUP_BUDGET: Duration = Duration::from_secs(30);
const REFLECTOR_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const CANCELLATION_INTERVAL: Duration = Duration::from_millis(10);

struct Resolved {
    protocol: ProtocolKind,
    address: SocketAddr,
}

/// A completed wire handshake whose background session tasks are not spawned.
struct Prepared<P: Protocol> {
    session: Session<P, Connected>,
    socket: Arc<UdpSocket>,
}

impl<P: Protocol + Send + 'static> Prepared<P> {
    fn activate(self) -> AsyncSession<P> {
        AsyncSession::spawn(self.session, self.socket)
    }
}

/// Resolve the reflector address, authenticate, and complete the handshake.
///
/// One absolute `SETUP_BUDGET` covers the resolver, authentication and the UDP
/// handshake. Joining an outstanding system resolver can delay the return past
/// that budget; cancellation or expiry then stops the handshake from starting.
/// The authentication and handshake timeouts apply in addition.
pub(super) async fn connect_reflector(
    callsign: Callsign,
    link: &LinkArg,
    cancelled: &AtomicBool,
) -> Result<AnyAsyncSession, String> {
    let deadline = Deadline::now() + SETUP_BUDGET;
    let name = link.reflector_name;
    let resolved = resolve_joined(move || resolve(name), deadline, cancelled).await?;
    output::line(format_args!(
        "Connecting to {} module {} at {} using {:?}.",
        link.reflector_name, link.reflector_module, resolved.address, resolved.protocol
    ));
    let session = match resolved.protocol {
        ProtocolKind::DPlus => AnyAsyncSession::DPlus(
            bounded(
                connect_dplus(callsign, resolved.address, link, deadline, cancelled),
                deadline,
                cancelled,
            )
            .await?
            .activate(),
        ),
        ProtocolKind::DExtra => AnyAsyncSession::DExtra(
            bounded(
                connect_dextra(callsign, resolved.address, link, deadline, cancelled),
                deadline,
                cancelled,
            )
            .await?
            .activate(),
        ),
        ProtocolKind::Dcs => AnyAsyncSession::Dcs(
            bounded(
                connect_dcs(callsign, resolved.address, link, deadline, cancelled),
                deadline,
                cancelled,
            )
            .await?
            .activate(),
        ),
        protocol => return Err(format!("unsupported reflector protocol {protocol:?}")),
    };
    output::line(format_args!(
        "Connected to {} module {}.",
        link.reflector_name, link.reflector_module
    ));
    Ok(session)
}

fn resolve(name: Callsign) -> Result<Resolved, String> {
    let entry = hosts::resolve(name).map_err(|error| error.to_string())?;
    let protocol = ProtocolKind::from_reflector_prefix(&name.as_str())
        .or_else(|| ProtocolKind::from_port(entry.port))
        .unwrap_or(ProtocolKind::DExtra);
    let address = format!("{}:{}", entry.address, entry.port)
        .to_socket_addrs()
        .map_err(|error| format!("address resolution failed for {}: {error}", entry.address))?
        .next()
        .ok_or_else(|| format!("no address resolved for {}", entry.address))?;
    Ok(Resolved { protocol, address })
}

async fn resolve_joined<T: Send + 'static>(
    resolver: impl FnOnce() -> Result<T, String> + Send + 'static,
    deadline: Deadline,
    cancelled: &AtomicBool,
) -> Result<T, String> {
    check_active(deadline, cancelled)?;
    // Do not select away from this join: system DNS cannot be cancelled safely.
    let result = tokio::task::spawn_blocking(resolver)
        .await
        .map_err(|error| format!("reflector resolver worker failed: {error}"))
        .and_then(std::convert::identity);
    if let Err(stopped) = check_active(deadline, cancelled) {
        return Err(match result {
            Ok(_) => stopped,
            Err(error) => format!("{stopped}; {error}"),
        });
    }
    result
}

/// Run `operation` until `deadline`, failing early once `cancelled` is set.
async fn bounded<T>(
    operation: impl Future<Output = Result<T, String>>,
    deadline: Deadline,
    cancelled: &AtomicBool,
) -> Result<T, String> {
    check_active(deadline, cancelled)?;
    let value = tokio::select! {
        biased;
        () = cancellation(cancelled) => return Err("reflector setup cancelled".to_owned()),
        result = tokio::time::timeout_at(deadline, operation) => {
            result.map_err(|_| "reflector setup deadline expired".to_owned())??
        }
    };
    check_active(deadline, cancelled)?;
    Ok(value)
}

async fn cancellation(cancelled: &AtomicBool) {
    while !cancelled.load(Ordering::Acquire) {
        tokio::time::sleep(CANCELLATION_INTERVAL).await;
    }
}

fn check_active(deadline: Deadline, cancelled: &AtomicBool) -> Result<(), String> {
    if cancelled.load(Ordering::Acquire) {
        Err("reflector setup cancelled".to_owned())
    } else if Deadline::now() >= deadline {
        Err("reflector setup deadline expired".to_owned())
    } else {
        Ok(())
    }
}

async fn bind_socket() -> Result<Arc<UdpSocket>, String> {
    UdpSocket::bind("0.0.0.0:0")
        .await
        .map(Arc::new)
        .map_err(|error| format!("UDP bind failed: {error}"))
}

async fn drive_handshake<P: Protocol>(
    session: Session<P, Connecting>,
    socket: &UdpSocket,
    deadline: Deadline,
    cancelled: &AtomicBool,
) -> Result<Session<P, Connected>, String> {
    check_active(deadline, cancelled)?;
    dstar_gateway::tokio_shell::drive_connecting(session, socket, REFLECTOR_CONNECT_TIMEOUT)
        .await
        .map_err(|error| error.to_string())
}

async fn connect_dextra(
    callsign: Callsign,
    peer: SocketAddr,
    link: &LinkArg,
    deadline: Deadline,
    cancelled: &AtomicBool,
) -> Result<Prepared<DExtra>, String> {
    let socket = bind_socket().await?;
    let connecting = Session::<DExtra, _>::builder()
        .callsign(callsign)
        .local_module(link.local_module)
        .reflector_module(link.reflector_module)
        .reflector_callsign(link.reflector_name)
        .peer(peer)
        .build()
        .connect(Instant::now())
        .map_err(|failure| format!("DExtra link request failed: {}", failure.error))?;
    let session = drive_handshake(connecting, &socket, deadline, cancelled).await?;
    Ok(Prepared { session, socket })
}

async fn connect_dplus(
    callsign: Callsign,
    peer: SocketAddr,
    link: &LinkArg,
    deadline: Deadline,
    cancelled: &AtomicBool,
) -> Result<Prepared<DPlus>, String> {
    check_active(deadline, cancelled)?;
    output::line(format_args!(
        "Authenticating with the DPlus gateway server."
    ));
    let hosts = match AuthClient::new().authenticate(callsign).await {
        Ok(hosts) => hosts,
        Err(error) => {
            check_active(deadline, cancelled)?;
            output::error(format_args!(
                "Warning: DPlus authentication failed: {error}; trying the UDP link anyway."
            ));
            dstar_gateway_core::codec::dplus::HostList::new()
        }
    };
    check_active(deadline, cancelled)?;
    let socket = bind_socket().await?;
    let authenticated = Session::<DPlus, _>::builder()
        .callsign(callsign)
        .local_module(link.local_module)
        .reflector_module(link.reflector_module)
        .reflector_callsign(link.reflector_name)
        .peer(peer)
        .build()
        .authenticate(hosts)
        .map_err(|failure| format!("DPlus host-list setup failed: {}", failure.error))?;
    let connecting = authenticated
        .connect(Instant::now())
        .map_err(|failure| format!("DPlus link request failed: {}", failure.error))?;
    let session = drive_handshake(connecting, &socket, deadline, cancelled).await?;
    Ok(Prepared { session, socket })
}

async fn connect_dcs(
    callsign: Callsign,
    peer: SocketAddr,
    link: &LinkArg,
    deadline: Deadline,
    cancelled: &AtomicBool,
) -> Result<Prepared<Dcs>, String> {
    let socket = bind_socket().await?;
    let connecting = Session::<Dcs, _>::builder()
        .callsign(callsign)
        .local_module(link.local_module)
        .reflector_module(link.reflector_module)
        .reflector_callsign(link.reflector_name)
        .peer(peer)
        .build()
        .connect(Instant::now())
        .map_err(|failure| format!("DCS link request failed: {}", failure.error))?;
    let session = drive_handshake(connecting, &socket, deadline, cancelled).await?;
    Ok(Prepared { session, socket })
}

/// Send the protocol unlink and report its outcome, before radio shutdown.
pub(super) async fn disconnect_reflector(reflector: &mut AnyAsyncSession) {
    match reflector.disconnect().await {
        Ok(()) => output::line(format_args!("Disconnected from reflector.")),
        Err(ShellError::DisconnectUnacknowledged) => output::error(format_args!(
            "Warning: reflector did not acknowledge unlink; its protocol timeout closed the local session."
        )),
        Err(ShellError::DisconnectedBeforeUnlink { reason }) => output::line(format_args!(
            "Reflector session had already closed: {reason:?}."
        )),
        Err(error) => output::error(format_args!(
            "Warning: reflector disconnect did not complete: {error}; continuing radio shutdown."
        )),
    }
}
